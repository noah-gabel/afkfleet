//! Nothing is left behind, whichever way a script ends (Plan.md P16.17, the
//! user's point 3; ADR-0054): a mode changed, a limit hit, a disconnect and
//! a stop, each while a click of the script waits for its fate. After each,
//! the harness shows no screen open for the bot, nothing on its cursor, no
//! key held (it isn't sneaking) and no button (no hand raised), though the
//! script held both before its screen opened.
//!
//! A click waits for its fate while the server is frozen (`docker pause`):
//! the bot's packets wait in the socket, and the server handles them,
//! close included, when it's thawed.

use core::time::Duration;
use std::rc::Rc;

use fleet_api_types::modes::ModeSteps;
use fleet_core::id::{BotId, ModeId};
use fleet_core::script::{LeashRadius, MessageDraft, ScriptDraft, ToolGuard};

use crate::harness::agent::Agent;
use crate::harness::paper::{KICKED_MID_CLICK, Paper, Record, of_kind, unasked};
use crate::harness::world::World;
use crate::slow_scripts::{entries, request, started};

use super::{NAME, agent, bot_on, fits, give, on_paper, rcon, until_records, until_state, world};

const STEPS: Duration = Duration::from_secs(240);
const _: () = assert!(fits(STEPS));

/// The runner's line for every screen command it hands the session.
const SCREEN_COMMAND: &str = "the script does something in a screen or a sign editor";
/// The runner's lines for a script's end.
const ENDED: &str = "the bot's script ended";
const STOPPED: &str = "the bot's script stopped; the bot stays online and idle";

/// At its first join it holds sneak and the right button (a shield), then
/// opens the sell menu, where it clicks slot 0 every second. `SPIN` is
/// what runs 300 ms after each click.
const WAYS_OUT: &str = r#"fn on_join(first) {
    if first {
        keys.sneak.hold();
        mouse.right.hold();
        timer.after(2000, "open");
    }
}

fn on_timer(name) {
    if name == "open" {
        chat.send("sell_menu");
    } else if name == "click" {
        menu.click(0);
        timer.after(300, "spin");
    } else if name == "spin" {
        SPIN
    }
}

fn on_menu_open() {
    timer.every(1000, "click");
}

fn on_click(result) {}
"#;

/// The mode whose script never spins, or whose script loops for good
/// while a click still waits for its fate: the operations' limit.
fn ways_out(spins: bool) -> fleet_api_types::script::ModeScriptRequest {
    let spin = if spins {
        "if menu.pending { loop {} }"
    } else {
        "return;"
    };
    request(ScriptDraft {
        api: 2,
        source: WAYS_OUT.replace("SPIN", spin),
        leash: LeashRadius::DEFAULT.blocks(),
        tool_guard: ToolGuard::DEFAULT.uses_left(),
        block_guard: Vec::new(),
        messages: vec![MessageDraft {
            key: "sell_menu".to_owned(),
            text: "/sell".to_owned(),
            values: Vec::new(),
        }],
        patterns: Vec::new(),
        signs: Vec::new(),
    })
}

/// Whether `state` shows nothing left: no screen, nothing on the cursor,
/// no sneak, no hand raised.
fn nothing_left(state: &Record) -> bool {
    state.get("screen") == Some("none")
        && state.get("cursor") == Some("none")
        && state.get("sneaking") == Some("false")
        && state.get("raised") == Some("false")
}

/// How many screen commands the runner handed the session so far.
fn commands(world: &World) -> usize {
    world.logs.count(SCREEN_COMMAND)
}

/// Waits until the bot's sell menu opened for the `nth` time (from 1) and
/// the bot clicked in it.
async fn clicking(world: &World, paper: &Paper, nth: usize) {
    until_records(
        world,
        paper,
        &format!("the bot clicking in its sell menu #{nth}"),
        |records| {
            let opened = records
                .iter()
                .enumerate()
                .filter(|(_, record)| {
                    record.kind == "open"
                        && record.of(NAME)
                        && record.get("screen") == Some("Sell Items")
                })
                .nth(nth - 1)
                .map(|(at, _)| at);
            opened.is_some_and(|at| {
                records.get(at..).unwrap_or_default().iter().any(|record| {
                    record.kind == "click"
                        && record.of(NAME)
                        && record.get("screen") == Some("Sell Items")
                })
            })
        },
    )
    .await;
}

/// How many of the script's clicks were refused because an earlier one
/// still waits for its fate.
fn refused_pending(world: &World) -> usize {
    entries(world, "Refused", "ClickPending")
}

/// Freezes the server and waits until a click of the script waits for its
/// fate: one more handed to the session, or the next one refused in its
/// hook because the last one, caught by the freeze, still waits.
async fn a_click_pending(world: &World, paper: &Paper) {
    let (commands_before, refused_before) = (commands(world), refused_pending(world));
    paper.frozen(true).await.unwrap();
    world
        .logs
        .until(
            "a click waiting for its fate on the frozen server",
            async || commands(world) > commands_before || refused_pending(world) > refused_before,
        )
        .await;
}

/// Thaws the server and waits until it closed the bot's sell menu.
async fn thawed_and_closed(world: &World, paper: &Paper, closes: usize) {
    paper.frozen(false).await.unwrap();
    until_records(world, paper, "the sell menu closed", |records| {
        of_kind(records, "close", NAME)
            .iter()
            .filter(|close| close.get("screen") == Some("Sell Items"))
            .count()
            >= closes
    })
    .await;
}

#[tokio::test]
async fn slow_whichever_way_a_script_ends_mid_click_nothing_is_held_and_no_screen_is_open() {
    on_paper(world().await, STEPS, every_way_out).await;
}

async fn every_way_out(world: Rc<World>, paper: Rc<Paper>) -> (Agent, ()) {
    let agent = agent(&world).await;
    let bot = bot_on(&world, &paper, NAME).await;
    let plain = world
        .owner
        .scripted_mode("ways_out", ways_out(false))
        .await
        .id;
    let spinning = world
        .owner
        .scripted_mode("ways_out_spinning", ways_out(true))
        .await
        .id;
    let idle = world.owner.mode_of("Idle", ModeSteps(Vec::new())).await.id;

    // 1. A mode changed while a click waits.
    let mut scripts = 1;
    on_mode(&world, &agent, bot, plain, scripts).await;
    give(&paper, NAME, "hotbar.0", "minecraft:shield").await;
    clicking(&world, &paper, 1).await;
    a_click_pending(&world, &paper).await;
    world.owner.set_mode(bot, idle).await;
    world
        .logs
        .until("the script's orderly end", async || {
            world.logs.count(ENDED) >= 1
        })
        .await;
    thawed_and_closed(&world, &paper, 1).await;
    let after_a_mode_change = until_state(
        &world,
        &paper,
        NAME,
        "nothing left after a mode change",
        nothing_left,
    )
    .await;

    // 2. A limit hit while a click waits.
    scripts += 1;
    world.owner.set_mode(bot, spinning).await;
    started(&world, scripts).await;
    clicking(&world, &paper, 2).await;
    // It loops only while a click waits: that's the frozen server's.
    paper.frozen(true).await.unwrap();
    world
        .logs
        .until("the script stopped at its limit", async || {
            world.logs.count(STOPPED) >= 1
        })
        .await;
    thawed_and_closed(&world, &paper, 2).await;
    let after_a_limit = until_state(
        &world,
        &paper,
        NAME,
        "nothing left after a limit",
        nothing_left,
    )
    .await;

    // 3. A disconnect inside a click: the server kicks the bot there; it
    // joins again, and a rejoin opens nothing.
    scripts += 1;
    world.owner.set_mode(bot, plain).await;
    started(&world, scripts).await;
    clicking(&world, &paper, 3).await;
    rcon(&paper, &format!("harness kickon {NAME}")).await;
    until_records(&world, &paper, "the kick inside a click", |records| {
        !of_kind(records, "kickon", NAME).is_empty()
    })
    .await;
    // `harness state` waits through the rejoin: it answers once the bot is back.
    let after_a_disconnect = until_state(
        &world,
        &paper,
        NAME,
        "nothing left after a disconnect",
        nothing_left,
    )
    .await;

    // 4. A stop while a click waits: the bot leaves, its screen closed.
    rcon(&paper, &format!("harness open {NAME} sell")).await;
    clicking(&world, &paper, 4).await;
    a_click_pending(&world, &paper).await;
    world.owner.stop(bot).await;
    world
        .logs
        .until("the script's orderly end at the stop", async || {
            world.logs.count(ENDED) >= 2
        })
        .await;
    thawed_and_closed(&world, &paper, 4).await;
    let gone = rcon(&paper, &format!("harness state {NAME}")).await;

    for (way, state) in [
        ("a mode change", &after_a_mode_change),
        ("a limit", &after_a_limit),
        ("a disconnect", &after_a_disconnect),
    ] {
        assert!(nothing_left(state), "{way}: {state:?}");
    }
    assert_eq!(gone, "not online");
    // The script held the right button with the shield in hand before its
    // first screen: had anything pressed it again, the hand would be up.
    assert_eq!(after_a_limit.get("item"), Some("minecraft:shield"));
    let log = paper.log(Duration::from_secs(20)).await.unwrap();
    assert_eq!(
        unasked(&log, &[NAME], &[KICKED_MID_CLICK]),
        Vec::<String>::new()
    );
    (agent, ())
}

/// Sets `mode` on the stopped bot, starts it and waits for its script.
async fn on_mode(world: &World, agent: &Agent, bot: BotId, mode: ModeId, scripts: usize) {
    world.owner.set_mode(bot, mode).await;
    world.owner.start(bot).await;
    world.online(agent, bot, 0).await;
    started(world, scripts).await;
}
