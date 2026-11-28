//! The reference's five level-2 examples as written, on the Paper harness
//! (Plan.md P16.17; ADR-0036, M13, M17 amended; ADR-0054): each saved
//! through the API as the editor's starter template, sent to the real agent
//! and judged by the harness's records.
//!
//! - **The sell macro** sells every Turtle Master potion it carries and
//!   nothing else.
//! - **The auction scan** buys the cheapest diamond within its budget once;
//!   when the other player buys the listing it picked, on its confirm
//!   screen or between its scan and its click, it buys nothing in its
//!   place and logs the miss.
//! - **The farm that sells** never drinks a potion: the potions it carried
//!   are the ones the harness bought, and it farms on with its own item.
//! - **The auction lister** never lists below its floor for bait, skips its
//!   own listings and collects its expired ones.
//! - **The anvil lister** never clicks the trap renamed "Create", and stops
//!   with a line at a price the plugin refuses, a step that never answers
//!   and a screen opened unasked, before it lists.

use core::time::Duration;
use std::rc::Rc;

use crate::harness::agent::Agent;
use crate::harness::paper::{KICKED_BY_AN_OPERATOR, Paper, Record, items, of_kind, unasked};
use crate::harness::world::World;
use crate::slow_scripts::{entries, example, until_logged};

use super::{
    NAME, OTHER, agent, fits, give, harness, kick, on_paper, rcon, scripted_bot, state,
    the_other_player, until_records, world,
};

/// A Turtle Master potion of each strength, in the game's syntax.
const TURTLE: &str = "minecraft:potion[potion_contents={potion:\"minecraft:turtle_master\"}]";
const LONG_TURTLE: &str =
    "minecraft:potion[potion_contents={potion:\"minecraft:long_turtle_master\"}]";
const STRONG_TURTLE: &str =
    "minecraft:potion[potion_contents={potion:\"minecraft:strong_turtle_master\"}]";
/// The keys the harness gives them.
const POTIONS: [&str; 3] = [
    "minecraft:potion:turtle_master",
    "minecraft:potion:long_turtle_master",
    "minecraft:potion:strong_turtle_master",
];

/// How many Turtle Master potions `state` says the player carries.
fn potions(state: &Record) -> u32 {
    let carried = items(state);
    POTIONS
        .iter()
        .map(|key| carried.get(*key).copied().unwrap_or(0))
        .sum()
}

/// The records after the first one `first` takes, if any.
fn after(records: &[Record], first: impl Fn(&Record) -> bool) -> Option<&[Record]> {
    let at = records.iter().position(first)?;
    records.get(at + 1..)
}

/// The bot's clicks on the screen titled `screen`.
fn clicks_on<'a>(records: &'a [Record], screen: &str) -> Vec<&'a Record> {
    of_kind(records, "click", NAME)
        .into_iter()
        .filter(|click| click.get("screen") == Some(screen))
        .collect()
}

/// The records about either player, one a line: what an assertion shows.
fn told(records: &[Record]) -> String {
    records
        .iter()
        .filter(|record| record.of(NAME) || record.of(OTHER) || record.get("seller") == Some(NAME))
        .map(|record| format!("{record:?}"))
        .collect::<Vec<_>>()
        .join(
            "
",
        )
}

/// Asserts Paper's log holds no disconnect and no warning about either
/// player but what the case caused itself.
async fn quiet(paper: &Paper, caused: &[&str]) {
    let log = paper
        .log(Duration::from_secs(20))
        .await
        .unwrap_or_else(|why| panic!("{why}"));
    assert_eq!(unasked(&log, &[NAME, OTHER], caused), Vec::<String>::new());
}

// --- The sell macro ---

const SELL_STEPS: Duration = Duration::from_secs(150);
const _: () = assert!(fits(SELL_STEPS));

#[tokio::test]
async fn slow_the_sell_macro_sells_every_turtle_master_potion_and_nothing_else() {
    on_paper(world().await, SELL_STEPS, sell_macro).await;
}

async fn sell_macro(world: Rc<World>, paper: Rc<Paper>) -> (Agent, ()) {
    let agent = agent(&world).await;
    let mode = example(&world, "sell_macro").await;
    scripted_bot(&world, &paper, &agent, mode, 1).await;
    // Before the first minute's sell: the three potions, and two things it
    // must leave, wheat among them, which the sell menu would buy.
    for (slot, item) in [
        ("hotbar.1", TURTLE),
        ("inventory.3", LONG_TURTLE),
        ("inventory.20", STRONG_TURTLE),
        ("hotbar.4", "minecraft:diamond_pickaxe"),
        ("inventory.8", "minecraft:wheat 16"),
    ] {
        give(&paper, NAME, slot, item).await;
    }
    let before = state(&paper, NAME).await;

    let seen = until_records(
        &world,
        &paper,
        "the sale and the close after it",
        |records| {
            after(records, |record| record.kind == "sold" && record.of(NAME)).is_some_and(|rest| {
                rest.iter().any(|record| {
                    record.kind == "close"
                        && record.of(NAME)
                        && record.get("screen") == Some("Sell Items")
                })
            })
        },
    )
    .await;
    let after_it = state(&paper, NAME).await;

    assert_eq!(potions(&before), 3);
    let sold = of_kind(&seen, "sold", NAME);
    assert_eq!(sold.len(), 1);
    assert_eq!(sold[0].get("items"), Some("3"));
    assert_eq!(sold[0].get("total"), Some("900"));
    assert_eq!(potions(&after_it), 0);
    let left = items(&after_it);
    assert_eq!(left.get("minecraft:diamond_pickaxe"), Some(&1));
    assert_eq!(left.get("minecraft:wheat"), Some(&16));
    assert_eq!(after_it.get("screen"), Some("none"));
    // A shift-click a potion, then Sell once.
    let clicks: Vec<(Option<&str>, Option<i64>)> = clicks_on(&seen, "Sell Items")
        .iter()
        .map(|click| (click.get("click"), click.number("raw")))
        .collect();
    assert_eq!(clicks.len(), 4, "{clicks:?}");
    assert!(
        clicks
            .iter()
            .take(3)
            .all(|(kind, raw)| *kind == Some("SHIFT_LEFT") && raw.is_some_and(|raw| raw >= 54)),
        "{clicks:?}"
    );
    assert_eq!(clicks[3], (Some("LEFT"), Some(49)));
    quiet(&paper, &[]).await;
    (agent, ())
}

// --- The auction scan ---

const SCAN_STEPS: Duration = Duration::from_secs(240);
const _: () = assert!(fits(SCAN_STEPS));

#[tokio::test]
async fn slow_the_auction_scan_buys_once_and_never_another_listing_in_the_place_of_one_bought_first()
 {
    on_paper(world().await, SCAN_STEPS, auction_scan).await;
}

/// Lists a diamond for `price` by `seller`; returns its ID.
async fn diamond(paper: &Paper, seller: &str, price: u32) -> String {
    let said = rcon(
        paper,
        &format!("harness list {seller} {price} minecraft:diamond"),
    )
    .await;
    said.strip_prefix("listed ")
        .unwrap_or_else(|| panic!("{said}"))
        .to_owned()
}

async fn auction_scan(world: Rc<World>, paper: Rc<Paper>) -> (Agent, ()) {
    let agent = agent(&world).await;
    the_other_player(&world, &paper, &agent).await;
    rcon(&paper, &format!("harness money {NAME} 100000")).await;
    // Oldest first: a page shows them in this order.
    let _dear = diamond(&paper, "Seller1", 2000).await;
    let first_out = diamond(&paper, "Seller2", 1400).await;
    let in_its_place = diamond(&paper, "Seller3", 1450).await;
    let confirmed_away = diamond(&paper, "Seller3", 1200).await;
    let cheapest = diamond(&paper, "Seller1", 900).await;
    let mode = example(&world, "auction_scan").await;
    scripted_bot(&world, &paper, &agent, mode, 1).await;

    // 1. The first scan, 30 s after the join: the cheapest within 1,500.
    until_logged(&world, "Line", "Bought 1 diamonds for 900", 1).await;

    // 2. The other player buys the listing picked while its confirm screen
    // is open: the confirm finds it gone, and nothing is bought.
    rcon(&paper, &format!("harness snipe {OTHER} confirm")).await;
    until_logged(&world, "Line", "The purchase didn't go through.", 1).await;

    // 3. The other player buys the listing it will pick the tick after its
    // auction house opens: the bot's click meets a listing that's gone, and
    // the plugin draws the next one in its slot.
    rcon(
        &paper,
        &format!("harness snipe {OTHER} house minecraft:diamond 1500"),
    )
    .await;
    let seen = until_records(
        &world,
        &paper,
        "the click on a listing that's gone",
        |records| !of_kind(records, "gone", NAME).is_empty(),
    )
    .await;
    until_logged(&world, "Line", "was gone", 1).await;
    let after_it = harness(&paper).await;

    let id = |record: &Record| record.get("id").map(str::to_owned);
    let bought: Vec<Option<String>> = of_kind(&after_it, "bought", NAME)
        .into_iter()
        .map(id)
        .collect();
    assert_eq!(bought, [Some(cheapest)], "only the one it confirmed");
    let theirs: Vec<Option<String>> = of_kind(&after_it, "bought", OTHER)
        .into_iter()
        .map(id)
        .collect();
    assert_eq!(
        theirs,
        [Some(confirmed_away.clone()), Some(first_out.clone())]
    );
    let refused = of_kind(&after_it, "refused", NAME);
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].get("id"), Some(confirmed_away.as_str()));
    assert_eq!(refused[0].get("reason"), Some("GONE"));
    assert_eq!(
        of_kind(&seen, "gone", NAME)[0].get("id"),
        Some(first_out.as_str())
    );
    // The listing drawn in the place of the one bought first was never
    // clicked, and no Confirm Purchase opened for it.
    assert!(
        of_kind(&after_it, "click", NAME)
            .iter()
            .all(|click| click.get("listing") != Some(in_its_place.as_str())),
    );
    assert!(
        of_kind(&after_it, "open", NAME)
            .iter()
            .all(|open| open.get("listing") != Some(in_its_place.as_str())),
    );
    quiet(&paper, &[]).await;
    (agent, ())
}

// --- The farm that sells ---

const FARM_STEPS: Duration = Duration::from_secs(180);
const _: () = assert!(fits(FARM_STEPS));

#[tokio::test]
async fn slow_the_farm_that_sells_never_drinks_a_potion_and_farms_on_with_its_own_item() {
    on_paper(world().await, FARM_STEPS, farm_that_sells).await;
}

async fn farm_that_sells(world: Rc<World>, paper: Rc<Paper>) -> (Agent, ()) {
    let agent = agent(&world).await;
    let mode = example(&world, "farm_that_sells").await;
    scripted_bot(&world, &paper, &agent, mode, 1).await;
    // The farm's item, a shield, in hotbar slot 1; potions in the hotbar
    // beside it and in the inventory, where a slip of the hand would drink
    // one; its junk in two places; two tools it must keep.
    for (slot, item) in [
        ("hotbar.0", "minecraft:shield"),
        ("hotbar.1", TURTLE),
        ("hotbar.2", LONG_TURTLE),
        ("inventory.5", STRONG_TURTLE),
        ("hotbar.3", "minecraft:rotten_flesh 4"),
        ("inventory.10", "minecraft:rotten_flesh 8"),
        ("hotbar.4", "minecraft:diamond_pickaxe"),
        ("inventory.15", "minecraft:diamond_sword"),
    ] {
        give(&paper, NAME, slot, item).await;
    }
    // It joins again with them, as it would after any reconnect: its join
    // takes the shield and holds the right button.
    kick(&paper, NAME).await;
    let farming = |state: &Record| {
        state.get("held") == Some("1")
            && state.get("item") == Some("minecraft:shield")
            && state.get("raised") == Some("true")
    };
    let before =
        super::until_state(&world, &paper, NAME, "farming with the shield up", farming).await;

    let seen = until_records(
        &world,
        &paper,
        "the sale and the close after it",
        |records| {
            after(records, |record| record.kind == "sold" && record.of(NAME)).is_some_and(|rest| {
                rest.iter().any(|record| {
                    record.kind == "close"
                        && record.of(NAME)
                        && record.get("screen") == Some("Sell Items")
                })
            })
        },
    )
    .await;
    let after_it = super::until_state(
        &world,
        &paper,
        NAME,
        "farming on with the shield up",
        farming,
    )
    .await;

    // Every potion it carried, the harness bought; none was drunk.
    assert_eq!(potions(&before), 3);
    let sold = of_kind(&seen, "sold", NAME);
    assert_eq!(sold.len(), 1);
    assert_eq!(sold[0].get("items"), Some("3"));
    assert_eq!(potions(&after_it), 0);
    assert_eq!(of_kind(&seen, "consume", NAME).len(), 0, "nothing drunk");
    assert_eq!(after_it.get("effects"), Some("[]"));
    // Its junk dropped, and nothing else.
    let dropped: Vec<Option<&str>> = of_kind(&seen, "drop", NAME)
        .iter()
        .map(|drop| drop.get("item"))
        .collect();
    assert_ne!(dropped, Vec::<Option<&str>>::new());
    assert!(
        dropped
            .iter()
            .all(|item| *item == Some("minecraft:rotten_flesh")),
        "{dropped:?}"
    );
    let left = items(&after_it);
    assert_eq!(left.get("minecraft:rotten_flesh"), None);
    assert_eq!(left.get("minecraft:diamond_pickaxe"), Some(&1));
    assert_eq!(left.get("minecraft:diamond_sword"), Some(&1));
    assert_eq!(left.get("minecraft:shield"), Some(&1));
    quiet(&paper, &[KICKED_BY_AN_OPERATOR]).await;
    (agent, ())
}

// --- The auction lister ---

const LISTER_STEPS: Duration = Duration::from_secs(240);
const _: () = assert!(fits(LISTER_STEPS));

#[tokio::test]
async fn slow_the_auction_lister_never_lists_below_its_floor_skips_its_own_and_collects() {
    on_paper(world().await, LISTER_STEPS, auction_lister).await;
}

/// Lists a Turtle Master potion for `price` by `seller`; returns its ID.
async fn potion_listed(paper: &Paper, seller: &str, price: u32) -> String {
    let said = rcon(paper, &format!("harness list {seller} {price} {TURTLE}")).await;
    said.strip_prefix("listed ")
        .unwrap_or_else(|| panic!("{said}"))
        .to_owned()
}

/// The bot's own listings the plugin made from its screens, by price.
fn listed_by_the_bot(records: &[Record]) -> Vec<String> {
    records
        .iter()
        .filter(|record| record.kind == "listed" && record.get("seller") == Some(NAME))
        .filter_map(|record| record.get("price").map(str::to_owned))
        .collect()
}

async fn auction_lister(world: Rc<World>, paper: Rc<Paper>) -> (Agent, ()) {
    let agent = agent(&world).await;
    // 1. Bait: the only potions others list are below the floor of 100.
    for (seller, price) in [("Bait1", 10), ("Bait2", 20), ("Bait3", 30)] {
        potion_listed(&paper, seller, price).await;
    }
    let mode = example(&world, "auction_lister").await;
    scripted_bot(&world, &paper, &agent, mode, 1).await;
    give(&paper, NAME, "hotbar.2", TURTLE).await;
    until_logged(&world, "Line", "Not listed at 20.0: below the floor.", 1).await;
    let after_the_bait = harness(&paper).await;

    // 2. One bait among others' listings, the bot's own cheaper one, and
    // an expired one of its own; it joins again and lists.
    rcon(&paper, "harness reset").await;
    for (seller, price) in [
        ("Bait1", 50),
        ("Seller1", 400),
        ("Seller2", 500),
        ("Seller3", 600),
        (NAME, 150),
    ] {
        potion_listed(&paper, seller, price).await;
    }
    let expired = potion_listed(&paper, NAME, 160).await;
    rcon(&paper, &format!("harness expire {expired}")).await;
    kick(&paper, NAME).await;
    let seen = until_records(
        &world,
        &paper,
        "a listing, then the expired one collected",
        |records| !of_kind(records, "collected", NAME).is_empty(),
    )
    .await;
    let after_it = state(&paper, NAME).await;

    // The bait's round sent nothing; the listing is the middle of the
    // three cheapest of others' (50, 400, 500), never its own 150's.
    assert_eq!(listed_by_the_bot(&after_the_bait), Vec::<String>::new());
    assert!(
        of_kind(&after_the_bait, "open", NAME)
            .iter()
            .all(|open| open.get("screen") != Some("Confirm Listing")),
    );
    let listed: Vec<String> = listed_by_the_bot(&seen)
        .into_iter()
        .filter(|price| price != "150" && price != "160")
        .collect();
    assert_eq!(listed, ["400"], "{}", told(&seen));
    assert_eq!(clicks_on(&seen, "Confirm Listing").len(), 1);
    assert_eq!(of_kind(&seen, "collected", NAME).len(), 1);
    // The one it listed went; the expired one came back.
    assert_eq!(potions(&after_it), 1);
    quiet(&paper, &[KICKED_BY_AN_OPERATOR]).await;
    (agent, ())
}

// --- The auction lister with the price in an anvil ---

const ANVIL_STEPS: Duration = Duration::from_secs(300);
/// The watchdog's round: a rejoin, a first try up to 30 s after it, the flow
/// and the watchdog's minute.
const WATCHDOG_ROUND: Duration = Duration::from_secs(150);
const _: () = assert!(fits(ANVIL_STEPS));

#[tokio::test]
async fn slow_the_anvil_lister_never_clicks_the_trap_and_stops_with_a_line_at_every_fault() {
    on_paper(world().await, ANVIL_STEPS, anvil_lister).await;
}

async fn anvil_lister(world: Rc<World>, paper: Rc<Paper>) -> (Agent, ()) {
    let agent = agent(&world).await;
    for (seller, price) in [("Seller1", 400), ("Seller2", 500), ("Seller3", 600)] {
        potion_listed(&paper, seller, price).await;
    }
    // A listing renamed "Create", in the rows above the buttons.
    let said = rcon(
        &paper,
        "harness list Seller2 100 minecraft:emerald[custom_name=\"Create\"]",
    )
    .await;
    let trap = said.strip_prefix("listed ").unwrap().to_owned();
    rcon(&paper, "harness cap 300").await;
    let mode = example(&world, "auction_lister_anvil").await;
    scripted_bot(&world, &paper, &agent, mode, 1).await;
    // In the main inventory: found wherever it is.
    give(&paper, NAME, "inventory.13", TURTLE).await;

    // 1. A price the plugin refuses: the anvil stays open.
    until_logged(&world, "Line", "Listing stopped: the price was refused.", 1).await;
    // 2. A step that never answers: the watchdog ends it.
    rcon(&paper, "harness cap 1000000000").await;
    rcon(&paper, "harness stall on").await;
    kick(&paper, NAME).await;
    // A rejoin, the first try 10 to 30 s after it, and the watchdog's
    // minute: longer than a wait's patience.
    let watched = world
        .logs
        .within(WATCHDOG_ROUND, async || {
            entries(
                &world,
                "Line",
                "Listing stopped: it took longer than a minute.",
            ) >= 1
        })
        .await;
    assert!(watched, "the watchdog's line within {WATCHDOG_ROUND:?}");
    // 3. A screen the plugin opens unasked in the middle of the flow.
    rcon(&paper, "harness stall off").await;
    rcon(
        &paper,
        &format!("harness intrude {NAME} sell Create Listing"),
    )
    .await;
    kick(&paper, NAME).await;
    until_logged(
        &world,
        "Line",
        "Listing stopped: an unexpected screen, Sell Items.",
        1,
    )
    .await;
    // 4. The listing, through the anvil.
    kick(&paper, NAME).await;
    until_logged(&world, "Line", "Listed a potion for 500.0.", 1).await;
    let seen = harness(&paper).await;

    let prices: Vec<(Option<&str>, Option<&str>)> = of_kind(&seen, "anvil", NAME)
        .iter()
        .map(|anvil| (anvil.get("text"), anvil.get("taken")))
        .collect();
    assert_eq!(
        prices,
        [(Some("500"), Some("false")), (Some("500"), Some("true"))]
    );
    assert_eq!(listed_by_the_bot(&seen), ["500"]);
    assert_eq!(of_kind(&seen, "stalled", NAME).len(), 1);
    assert_eq!(of_kind(&seen, "intrude", NAME).len(), 1);
    // Create by its slot every time; the trap never.
    let on_the_house: Vec<Option<i64>> = clicks_on(&seen, "Auction House")
        .iter()
        .map(|click| click.number("raw"))
        .collect();
    assert_eq!(on_the_house, [Some(47); 4]);
    assert!(
        of_kind(&seen, "click", NAME)
            .iter()
            .all(|click| click.get("listing") != Some(trap.as_str())),
    );
    // Each stop closed the screen it stopped on.
    let closed: Vec<Option<&str>> = of_kind(&seen, "close", NAME)
        .iter()
        .filter(|close| close.get("reason") == Some("PLAYER"))
        .map(|close| close.get("screen"))
        .collect();
    for stopped_on in ["Preis eingeben", "Confirm Item", "Sell Items"] {
        assert!(closed.contains(&Some(stopped_on)), "{closed:?}");
    }
    quiet(&paper, &[KICKED_BY_AN_OPERATOR]).await;
    (agent, ())
}
