//! The long run (Plan.md P16.18; ADR-0036, M3, M4, R1, R2; ADR-0054): a
//! buyer script on the real managed agent clicks thousands of times against
//! the Paper harness, in seven parts of [`CLICKS`] clicks, each a test of its own
//! under the profile's 10 minutes. `just long-run` runs them, alone.
//!
//! Each part makes it hard, at fixed counts of the buyer's clicks: the
//! other player buys the listing the buyer picked, between its scan and its
//! click and on its confirm screen; the listings change, and every open
//! auction house is redrawn at once; a screen opens unasked, in the middle
//! of a purchase and anywhere; the server freezes until a click's fate
//! comes back `unanswered`, past its 5 s; and the bot is kicked. Then it
//! stops in an orderly way, which names any buy still open.
//!
//! [`trades::count`] counts from the harness's own records: double buys,
//! silent misses and clicks onto another screen must all be none, and
//! Paper's log holds no kick or warning about either player but the one
//! the part caused. Each part prints its counts as `LONG` lines.

use core::time::Duration;
use std::rc::Rc;
use std::time::Instant;

use fleet_core::script::{LeashRadius, MessageDraft, ScriptDraft, ToolGuard};

use crate::harness::agent::Agent;
use crate::harness::logs::PATIENCE;
use crate::harness::paper::{KICKED_BY_AN_OPERATOR, Paper, unasked};
use crate::harness::trades::{self, Trades};
use crate::harness::world::World;
use crate::slow_scripts::{entries, request};
use crate::slow_trading::{
    NAME, OTHER, agent, fits, harness, kick, on_paper, rcon, scripted_bot, the_other_player, world,
};

/// The buyer's script; see the file.
const BUYER: &str = include_str!("buyer.rhai");
/// The clicks a part runs to.
const CLICKS: usize = 500;
/// A part's steps, past which they're stopped: the clicks, a freeze or two,
/// a rejoin and the setup.
const STEPS: Duration = Duration::from_secs(300);
const _: () = assert!(fits(STEPS));
/// The most the buyer pays, as its script has it.
const BUDGET: u32 = 1_000;
/// Diamonds within the budget kept listed, at unique prices: one, topped
/// up at every change of the market. Each buy needs a command to open the
/// auction house again, and the bot sends one every 3 s at most.
const SUPPLY: usize = 1;
/// How long a freeze may last at most when no click waits for its fate:
/// below Paper's 15 s keep-alive check, which a longer pause can trip.
const FREEZE: Duration = Duration::from_secs(12);

/// What a part does at a count of the buyer's clicks.
#[derive(Debug, Clone, Copy)]
enum Chaos {
    /// Listings change: one above the budget listed and an old one expired,
    /// every open auction house redrawn; and the supply kept.
    Market,
    /// The other player buys the cheapest diamond within the budget the
    /// tick after the buyer's auction house next opens.
    SnipeTheHouse,
    /// The other player buys the listing on the buyer's next Confirm
    /// Purchase, the tick after it opens.
    SnipeTheConfirm,
    /// The plugin opens the sell menu the tick after the buyer's next
    /// Confirm Purchase opens: in the middle of a purchase.
    Intrude,
    /// The plugin opens the sell menu now, unasked.
    OpenUnasked,
    /// The server freezes until a click's fate is `unanswered`.
    Freeze,
    /// An operator kicks the buyer; it joins again.
    Kick,
}

/// The chaos of a part, by the count of the buyer's clicks it comes at.
fn schedule() -> Vec<(usize, Chaos)> {
    let mut at: Vec<(usize, Chaos)> = (1..CLICKS / 40).map(|n| (n * 40, Chaos::Market)).collect();
    at.extend([
        (50, Chaos::SnipeTheHouse),
        (100, Chaos::Intrude),
        (150, Chaos::SnipeTheConfirm),
        (200, Chaos::Freeze),
        (250, Chaos::OpenUnasked),
        (300, Chaos::SnipeTheHouse),
        (350, Chaos::Kick),
        (400, Chaos::SnipeTheConfirm),
        (440, Chaos::Freeze),
        (480, Chaos::Intrude),
    ]);
    at.sort_by_key(|(clicks, _)| *clicks);
    at
}

/// The buyer's mode: its script, `/ah` declared.
fn buyer() -> fleet_api_types::script::ModeScriptRequest {
    request(ScriptDraft {
        api: 2,
        source: BUYER.to_owned(),
        leash: LeashRadius::DEFAULT.blocks(),
        tool_guard: ToolGuard::DEFAULT.uses_left(),
        block_guard: Vec::new(),
        messages: vec![MessageDraft {
            key: "auction".to_owned(),
            text: "/ah".to_owned(),
            values: Vec::new(),
        }],
        patterns: Vec::new(),
        signs: Vec::new(),
    })
}

/// How many clicks the runner has handed the session so far: its screen
/// commands' lines that act, counted in the log's text.
fn clicks(world: &World) -> usize {
    world
        .logs
        .buffer()
        .text()
        .matches("\"command\":\"Act(Click(")
        .count()
}

/// How many clicks came back `unanswered` so far.
fn unanswered(world: &World) -> usize {
    entries(world, "Menu", "Unanswered")
}

/// What a part did besides the clicks: its chaos, as it came.
#[derive(Debug, Default)]
struct Faults {
    market: usize,
    snipes: usize,
    intrusions: usize,
    unasked: usize,
    kicks: usize,
    /// Each freeze's length, and whether a click's fate came `unanswered`
    /// in it.
    freezes: Vec<(Duration, bool)>,
}

/// The diamonds within the budget the run lists, each at a price of its own.
#[derive(Debug, Default)]
struct Supply {
    listed: u32,
    expired: u32,
    dear: u32,
}

impl Supply {
    /// Lists diamonds within the budget until [`SUPPLY`] of them are listed.
    async fn keep(&mut self, paper: &Paper) {
        let said = rcon(paper, &format!("harness count minecraft:diamond {BUDGET}")).await;
        let within: usize = said
            .strip_prefix("count ")
            .and_then(|count| count.parse().ok())
            .unwrap_or_else(|| panic!("{said}"));
        for _ in within..SUPPLY {
            // 500.00, 501.37, 502.74, …: never one price twice.
            let n = self.listed;
            self.listed += 1;
            let price = format!("{}.{:02}", 500 + n, (n * 37) % 100);
            let seller = format!("Seller{}", n % 3 + 1);
            rcon(
                paper,
                &format!("harness list {seller} {price} minecraft:diamond"),
            )
            .await;
        }
    }

    /// One listing above the budget listed, and one of the filler expired.
    async fn change(&mut self, paper: &Paper) {
        self.dear += 1;
        self.expired += 1;
        rcon(
            paper,
            &format!(
                "harness list Seller{} {} minecraft:diamond",
                self.dear % 3 + 1,
                2_000 + self.dear
            ),
        )
        .await;
        rcon(paper, &format!("harness expire {}", self.expired)).await;
    }
}

/// Brings `chaos` about.
async fn happen(
    chaos: Chaos,
    world: &World,
    paper: &Paper,
    supply: &mut Supply,
    faults: &mut Faults,
) {
    match chaos {
        Chaos::Market => {
            supply.change(paper).await;
            supply.keep(paper).await;
            faults.market += 1;
        }
        Chaos::SnipeTheHouse => {
            rcon(
                paper,
                &format!("harness snipe {OTHER} house minecraft:diamond {BUDGET}"),
            )
            .await;
            faults.snipes += 1;
        }
        Chaos::SnipeTheConfirm => {
            rcon(paper, &format!("harness snipe {OTHER} confirm")).await;
            faults.snipes += 1;
        }
        Chaos::Intrude => {
            rcon(
                paper,
                &format!("harness intrude {NAME} sell Confirm Purchase"),
            )
            .await;
            faults.intrusions += 1;
        }
        Chaos::OpenUnasked => {
            rcon(paper, &format!("harness open {NAME} sell")).await;
            faults.unasked += 1;
        }
        Chaos::Freeze => {
            let before = unanswered(world);
            let from = Instant::now();
            paper.frozen(true).await.unwrap();
            let came = within_reading(world, FREEZE, || unanswered(world) > before).await;
            paper.frozen(false).await.unwrap();
            faults.freezes.push((from.elapsed(), came));
        }
        Chaos::Kick => {
            kick(paper, NAME).await;
            faults.kicks += 1;
        }
    }
}

/// How often a wait reads the whole log at most: it grows to megabytes.
const READ_EVERY: Duration = Duration::from_millis(200);

/// Waits until `done`, looked at the log's wakes at most every
/// [`READ_EVERY`], within `bound`; says whether it came.
async fn within_reading(world: &World, bound: Duration, mut done: impl FnMut() -> bool) -> bool {
    let mut last: Option<Instant> = None;
    world
        .logs
        .within(bound, async || {
            if last.is_some_and(|at| at.elapsed() < READ_EVERY) {
                return false;
            }
            last = Some(Instant::now());
            done()
        })
        .await
}

/// Waits until `done`, as [`within_reading`] does, within the log's
/// patience; fails the part with `what` otherwise.
async fn until_reading(world: &World, what: &str, done: impl FnMut() -> bool) {
    let came = within_reading(world, PATIENCE, done).await;
    if !came {
        let said: Vec<String> = crate::slow_scripts::logged(world)
            .into_iter()
            .rev()
            .take(40)
            .map(|logged| logged.entry)
            .collect();
        panic!(
            "{what}: not within {PATIENCE:?}; {} clicks; the script's last entries, newest first: {said:#?}",
            clicks(world)
        );
    }
}

/// Waits until the buyer has handed the session `count` clicks.
async fn clicks_reached(world: &World, count: usize) {
    until_reading(world, &format!("the buyer's {count} clicks"), || {
        clicks(world) >= count
    })
    .await;
}

/// The part's counts and what it did, as `LONG` lines.
fn report(part: usize, trades: &Trades, faults: &Faults, took: Duration) -> Vec<String> {
    let list = |counts: &std::collections::BTreeMap<String, usize>| {
        counts
            .iter()
            .map(|(name, count)| format!("{name} {count}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let freezes: Vec<String> = faults
        .freezes
        .iter()
        .map(|(length, came)| {
            format!(
                "{:.1} s{}",
                length.as_secs_f64(),
                if *came { "" } else { " (no fate came in it)" }
            )
        })
        .collect();
    let mut lines = vec![
        format!(
            "LONG part {part}: {} clicks asked, {} sent, {} received by the harness, in {:.0} s; the script heard {:?} fates",
            trades.asked,
            trades.sent,
            trades.received,
            took.as_secs_f64(),
            trades.answered
        ),
        format!(
            "LONG part {part}: fates: {}; no fate, its session ended first: {}",
            list(&trades.fates),
            trades.unheard
        ),
        format!("LONG part {part}: not sent: {}", list(&trades.not_sent)),
        format!(
            "LONG part {part}: {} picks, {} bought, {} misses",
            trades.picks,
            trades.bought,
            trades.misses.len()
        ),
        format!(
            "LONG part {part}: double buys {}, silent misses {}, clicks onto another screen {}; sent onto the state seen but met a newer one {}, met another listing {}, packets no handler got {}, misses of a listing bought {}",
            trades.double_buys.len(),
            trades.silent_misses.len(),
            trades.other_screens.len(),
            trades.changed_in_flight.len(),
            trades.other_listings.len(),
            trades.ignored,
            trades.missed_but_bought.len()
        ),
        format!(
            "LONG part {part}: chaos: {} market changes, {} snipes, {} intrusions, {} screens opened unasked, {} kicks, freezes {freezes:?}",
            faults.market, faults.snipes, faults.intrusions, faults.unasked, faults.kicks
        ),
    ];
    lines.extend(
        trades
            .misses
            .iter()
            .map(|miss| format!("LONG part {part}: miss: {miss}")),
    );
    lines.extend(
        trades
            .changed_in_flight
            .iter()
            .chain(&trades.other_listings)
            .map(|click| format!("LONG part {part}: in flight: {click}")),
    );
    lines
}

/// One part of the long run; see the [module docs](self).
async fn part(world: Rc<World>, paper: Rc<Paper>, number: usize) -> (Agent, ()) {
    let agent = agent(&world).await;
    the_other_player(&world, &paper, &agent).await;
    rcon(&paper, &format!("harness money {NAME} 100000000")).await;
    // Five pages it can't afford: potions, swords and books.
    rcon(&paper, "harness fill 220").await;
    let mut supply = Supply::default();
    supply.keep(&paper).await;
    rcon(&paper, "harness live on").await;
    let mode = world
        .owner
        .scripted_mode("long_run_buyer", buyer())
        .await
        .id;
    let bot = scripted_bot(&world, &paper, &agent, mode, 1).await;

    let from = Instant::now();
    let mut faults = Faults::default();
    for (at, chaos) in schedule() {
        clicks_reached(&world, at).await;
        happen(chaos, &world, &paper, &mut supply, &mut faults).await;
    }
    clicks_reached(&world, CLICKS).await;
    let took = from.elapsed();
    // An orderly end, whose on_stop names any buy still open.
    world.owner.stop(bot).await;
    until_reading(&world, "the buyer's orderly end", || {
        world.logs.count("the bot's script ended") >= 1
    })
    .await;

    let records = harness(&paper).await;
    let agent_lines = world.logs.buffer().json_lines().unwrap();
    let trades = trades::count(&records, &agent_lines, NAME);
    for line in report(number, &trades, &faults, took) {
        eprintln!("{line}");
    }
    let log = paper.log(Duration::from_secs(30)).await.unwrap();
    assert_eq!(trades.double_buys, Vec::<String>::new(), "double buys");
    assert_eq!(trades.silent_misses, Vec::<String>::new(), "silent misses");
    assert_eq!(
        trades.other_screens,
        Vec::<String>::new(),
        "clicks onto another screen"
    );
    assert_eq!(
        unasked(&log, &[NAME, OTHER], &[KICKED_BY_AN_OPERATOR]),
        Vec::<String>::new()
    );
    assert!(trades.asked >= CLICKS, "{}", trades.asked);
    assert!(trades.picks > 0 && trades.bought > 0, "it bought");
    (agent, ())
}

#[rstest::rstest]
#[case::first(1)]
#[case::second(2)]
#[case::third(3)]
#[case::fourth(4)]
#[case::fifth(5)]
#[case::sixth(6)]
#[case::seventh(7)]
#[tokio::test]
async fn long_run_part(#[case] number: usize) {
    on_paper(world().await, STEPS, move |world, paper| {
        part(world, paper, number)
    })
    .await;
}
