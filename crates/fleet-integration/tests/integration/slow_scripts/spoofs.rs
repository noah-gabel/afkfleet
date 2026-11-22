//! A spoofed line triggers nothing, live (Plan.md P15.36; ADR-0037, C4, C9;
//! ADR-0042, G10): the balance example as written, on a real server, beside
//! a second player.
//!
//! At every join the script sends `/balance` and opens a window of 5 s for
//! the reply `Balance: ${amount}`; a reply would make it pay what's above
//! 10,000 with `/pay AfkMain {amount}`. Each case puts a line before the bot
//! inside such a window, then lets the window run out, and kicks the bot,
//! which joins again and opens the next one:
//! - **A player's lines:** the reply's exact text as the second player's
//!   chat, as a whisper to the bot and with `/me`. None matches, nothing is
//!   paid.
//! - **The server's lines a player causes:** the second player dies to an
//!   armour stand's item renamed to the reply's text, and a player joins
//!   under `Balance_500`, a name as close to it as a name can be. Neither
//!   matches.
//! - **The server's own line:** the server prints the reply itself with
//!   `tellraw`. Outside a window it doesn't count; inside one it matches,
//!   and the script pays exactly what's above 10,000. This is the example
//!   running as written, too.
//!
//! A spoof proves something only inside a window. The first window opens
//! while the bots come online, so the rounds start at the second, after a
//! kick. Each spoof is put before the bot within [`SOON`] of its window's
//! opening, by the runner's own log line's time (a join within [`JOIN`],
//! judged when the player is online), on a server that relays a line at
//! once, of a window of 5 s; the bot's history then holds the line, and the
//! window runs out without a match.

use core::time::Duration;
use std::rc::Rc;

use chrono::{DateTime, Utc};
use fleet_api_types::chat::ChatLine;
use fleet_api_types::modes::ModeSteps;
use fleet_core::id::BotId;

use crate::harness::agent::Agent;
use crate::harness::world::World;
use crate::managed_check::minecraft::Minecraft;

use super::{
    NAME, agent_on_minecraft, bot_on, entries, example, logged, on_a_server, position, rcon,
    started, until_logged,
};

/// The second player.
const PLAYER: &str = "AfkBot2";
/// A player's name as close to the reply as a name can be.
const NEAR: &str = "Balance_500";
/// The reply's exact text, which the balance example's pattern fits.
const REPLY: &str = "Balance: $50,000";
/// What the script pays for it: what's above 10,000, with two places.
const PAID: &str = "/pay AfkMain 40000.00";
/// How soon after its window opens a spoof is put before the bot, at most.
const SOON: Duration = Duration::from_secs(2);
/// How soon after its window opens a player joins, at most: a new session
/// connects first.
const JOIN: Duration = Duration::from_secs(4);

/// The bots of a case: the scripted one, and the second player's.
struct Bots {
    scripted: BotId,
    player: BotId,
}

/// The balance example on `NAME` and an idle step mode on the second
/// player, both online, and the script started: its first window opened
/// meanwhile.
async fn the_balance_beside_a_player(world: &World, minecraft: &Minecraft, agent: &Agent) -> Bots {
    let scripted = bot_on(world, minecraft, NAME).await;
    let player = bot_on(world, minecraft, PLAYER).await;
    let balance = example(world, "balance").await;
    let idle = world.owner.mode_of("Idle", ModeSteps(Vec::new())).await.id;
    world.owner.set_mode(scripted, balance).await;
    world.owner.set_mode(player, idle).await;
    world.owner.start(player).await;
    world.online(agent, player, 0).await;
    world.owner.start(scripted).await;
    world.online(agent, scripted, 0).await;
    started(world, 1).await;
    Bots { scripted, player }
}

/// Waits for the first window to run out and kicks `NAME`, whose next
/// join opens the second, the first that a round can use from its start.
async fn a_fresh_window(world: &World, minecraft: &Minecraft) {
    let _ = ran_out(world, 1).await;
    rcon(minecraft, &format!("kick {NAME}")).await;
}

/// The ingest's line for a batch it stored.
const BATCH: &str = "the ingest wrote a batch";

/// When the server received the first line of `bot`'s history that `seen`
/// picks, waiting for it: the ingest stores lines in batches, so the
/// history is read again after each batch, never between two, which keeps
/// the reads under the API's rate.
async fn received(
    world: &World,
    bot: BotId,
    what: &str,
    seen: impl Fn(&ChatLine) -> bool,
) -> DateTime<Utc> {
    let mut at = None;
    let mut batches = None;
    world
        .logs
        .until(&format!("{NAME} read {what}"), async || {
            let now = world.logs.count(BATCH);
            if batches == Some(now) {
                return false;
            }
            batches = Some(now);
            at = world
                .owner
                .history(bot)
                .await
                .items
                .iter()
                .find(|line| seen(line))
                .map(|line| line.at.get());
            at.is_some()
        })
        .await;
    at.expect("waited for it")
}

/// When the `n`th entry (from 1) of `kind` was logged, waiting for it.
async fn the_nth(world: &World, kind: &str, n: usize) -> DateTime<Utc> {
    until_logged(world, kind, "balance", n).await;
    logged(world)
        .iter()
        .filter(|logged| logged.entry.starts_with(kind))
        .nth(n - 1)
        .expect("waited for it")
        .at
}

/// When the `n`th window (from 1) ran out, waiting for it.
async fn ran_out(world: &World, n: usize) -> DateTime<Utc> {
    the_nth(world, "WindowExpired", n).await
}

/// A round: the window it uses (from 1), what it puts before the bot, and
/// how soon after the window opens that comes at the latest.
struct Round<'a> {
    window: usize,
    what: &'a str,
    soon: Duration,
}

/// `round`'s window: waits for it to open, puts the round's line before the
/// bot with `spoof` within the round's bound, waits until the bot's history
/// holds it (`seen`) and the window has run out, holds that nothing matched
/// or was paid, then kicks the bot, which joins again and opens the next
/// window.
async fn a_round(
    world: &World,
    minecraft: &Minecraft,
    bots: &Bots,
    round: Round<'_>,
    spoof: impl AsyncFnOnce(),
    seen: impl Fn(&ChatLine) -> bool,
) {
    let Round {
        window: n,
        what,
        soon,
    } = round;
    let opened = the_nth(world, "WindowOpened", n).await;
    spoof().await;
    let put = Utc::now();
    assert!(
        (put - opened).to_std().is_ok_and(|took| took < soon),
        "{what} was put {} after window {n} opened; at most {soon:?}",
        put - opened
    );
    received(world, bots.scripted, what, seen).await;
    let _ = ran_out(world, n).await;
    assert_eq!(entries(world, "Matched", ""), 0, "{what} matched");
    assert_eq!(entries(world, "Sent", "Ident(\"pay\")"), 0, "{what} paid");
    rcon(minecraft, &format!("kick {NAME}")).await;
}

/// A line whose text holds the reply's.
fn the_reply(line: &ChatLine) -> bool {
    line.text.as_str().contains(REPLY)
}

#[tokio::test]
async fn slow_a_players_chat_whisper_and_me_never_count_as_the_reply() {
    on_a_server(players_messages).await;
}

async fn players_messages(world: Rc<World>, minecraft: Rc<Minecraft>) -> (Agent, ()) {
    let agent = agent_on_minecraft(&world, 2).await;
    let bots = the_balance_beside_a_player(&world, &minecraft, &agent).await;
    a_fresh_window(&world, &minecraft).await;
    for (n, (what, text)) in [
        ("the player's chat", REPLY.to_owned()),
        ("the player's whisper", format!("/msg {NAME} {REPLY}")),
        ("the player's /me", format!("/me {REPLY}")),
    ]
    .into_iter()
    .enumerate()
    {
        // A newer line than the last round's.
        let newest = world
            .owner
            .history(bots.scripted)
            .await
            .items
            .iter()
            .map(|line| line.id.get())
            .max()
            .unwrap_or(0);
        a_round(
            &world,
            &minecraft,
            &bots,
            Round {
                window: n + 2,
                what,
                soon: SOON,
            },
            async || {
                let sent = if text.starts_with('/') {
                    world.owner.command(bots.player, &text).await
                } else {
                    world.owner.chat(bots.player, &text).await
                };
                assert!(sent.is_ok(), "{what}: {sent:?}");
            },
            |line| the_reply(line) && line.id.get() > newest,
        )
        .await;
    }
    (agent, ())
}

#[tokio::test]
async fn slow_a_death_to_a_renamed_item_and_a_join_under_a_near_name_never_count() {
    on_a_server(deaths_and_joins).await;
}

async fn deaths_and_joins(world: Rc<World>, minecraft: Rc<Minecraft>) -> (Agent, ()) {
    let agent = agent_on_minecraft(&world, 3).await;
    let bots = the_balance_beside_a_player(&world, &minecraft, &agent).await;
    a_fresh_window(&world, &minecraft).await;
    let near = bot_on(&world, &minecraft, NEAR).await;
    let idle = world
        .owner
        .mode_of("Idle too", ModeSteps(Vec::new()))
        .await
        .id;
    world.owner.set_mode(near, idle).await;
    // A peaceful world lets nothing but a player hurt one.
    rcon(&minecraft, "difficulty easy").await;

    // The player dies to an armour stand's stick renamed to the reply.
    a_round(
        &world,
        &minecraft,
        &bots,
        Round {
            window: 2,
            what: "the death line of a renamed item",
            soon: SOON,
        },
        async || {
            let [x, y, z] = position(&minecraft, PLAYER).await;
            rcon(
                &minecraft,
                &format!(
                    "summon minecraft:armor_stand {x} {y} {} {{Tags:[\"renamed\"],equipment:{{mainhand:{{id:\"minecraft:stick\",components:{{\"minecraft:custom_name\":\"{REPLY}\"}}}}}}}}",
                    z + 2.0
                ),
            )
            .await;
            rcon(
                &minecraft,
                &format!("damage {PLAYER} 1000 minecraft:mob_attack by @e[tag=renamed,limit=1]"),
            )
            .await;
        },
        |line| line.text.as_str() == format!("{PLAYER} was slain by Armor Stand using [{REPLY}]"),
    )
    .await;

    // A player joins under the nearest name a name can be.
    a_round(
        &world,
        &minecraft,
        &bots,
        Round {
            window: 3,
            what: "the join line of a name like the reply",
            soon: JOIN,
        },
        async || {
            world.owner.start(near).await;
            world.online(&agent, near, 0).await;
        },
        |line| line.text.as_str() == format!("{NEAR} joined the game"),
    )
    .await;

    (agent, ())
}

#[tokio::test]
async fn slow_the_servers_own_line_counts_inside_a_window_and_not_outside_it() {
    on_a_server(the_servers_own_line).await;
}

async fn the_servers_own_line(world: Rc<World>, minecraft: Rc<Minecraft>) -> (Agent, ()) {
    let agent = agent_on_minecraft(&world, 2).await;
    let bots = the_balance_beside_a_player(&world, &minecraft, &agent).await;

    // While no window is open: the first ran out, and the next opens only
    // at the join after a kick.
    let _ = ran_out(&world, 1).await;
    rcon(&minecraft, &format!("tellraw {NAME} \"{REPLY}\"")).await;
    let _ = received(
        &world,
        bots.scripted,
        "the server's line outside a window",
        |line| line.text.as_str() == REPLY,
    )
    .await;
    assert_eq!(
        entries(&world, "Matched", ""),
        0,
        "a line outside a window matched"
    );
    assert_eq!(entries(&world, "Sent", "Ident(\"pay\")"), 0);

    // Inside the next window the same line is the reply: the script pays
    // what's above 10,000.
    rcon(&minecraft, &format!("kick {NAME}")).await;
    until_logged(&world, "WindowOpened", "balance", 2).await;
    rcon(&minecraft, &format!("tellraw {NAME} \"{REPLY}\"")).await;
    until_logged(&world, "Matched", "Ident(\"balance\")", 1).await;
    until_logged(&world, "Sent", PAID, 1).await;
    assert_eq!(entries(&world, "Matched", ""), 1);
    assert_eq!(entries(&world, "Sent", "Ident(\"pay\")"), 1);
    (agent, ())
}
