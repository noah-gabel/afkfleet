//! Phase 15's check on a real server (Plan.md P15.29, P15.36; ADR-0036,
//! D18; ADR-0037, C9; ADR-0046): the reference's examples, each set as
//! written on a bot through the API, run by the real managed agent on
//! fleet-mc's azalea connector against an offline-mode Minecraft server in
//! Docker, pinned as `deploy/compose.dev.yaml` pins it; and the balance
//! example beside a second player who tries every way a player has to put
//! its reply in front of the bot. `just test-slow` runs them.
//!
//! - [`examples`]: the AFK farm, the minecart farm and the cobblestone
//!   generator, each in a case of its own, each judged by what the server
//!   shows (a look, a score, a seat, a block mined) and by the script's own
//!   log.
//! - [`spoofs`]: the balance example (P15.36). A player's chat, whisper,
//!   `/me`, a death to an item renamed to the reply's text and a join under
//!   a name as close to it as a name can be, each inside a window the
//!   script opened: nothing matches and `/pay` is never sent. The server's
//!   own `tellraw` matches inside a window and not outside one, and the
//!   script then pays what it read.
//!
//! As the slow on-respawn case: offline accounts only, on the local
//! container; the bots dial only `127.0.0.1`; RCON runs inside the
//! container, so no password is on a command line. A case's steps run in
//! a task of their own, bounded by [`STEPS`], so a panic or the bound
//! comes back here and the container is always removed first. What a
//! script did is read from the runner's own log lines, captured at `debug`
//! (`the bot's script logged`, with the entry), and from the server through
//! RCON, read again at each of the log's wakes.

mod examples;
mod spoofs;

use core::future::Future;
use core::time::Duration;
use std::rc::Rc;

use chrono::{DateTime, Utc};
use fleet_api_types::script::ModeScriptRequest;
use fleet_core::id::{BotId, ModeId};
use fleet_core::script::ScriptDraft;
use fleet_script::EXAMPLES;
use tokio::task::LocalSet;

use crate::harness::agent::Agent;
use crate::harness::logs::PATIENCE;
use crate::harness::server::ServerOptions;
use crate::harness::world::World;
use crate::managed_check::minecraft::{Minecraft, STARTUP};

/// The scripted bot's offline account.
pub(crate) const NAME: &str = "AfkBot1";
/// A case's steps' budget, past which they're stopped and teardown runs.
pub(crate) const STEPS: Duration = Duration::from_secs(80);
/// One RCON command.
const RCON: Duration = Duration::from_secs(10);
/// The container's removal.
const REMOVAL: Duration = Duration::from_secs(20);
/// A case's worst: the container's start (the world starts beside it), the
/// steps, the removal, then the agent's and the server's stops.
const WORST: Duration = STARTUP
    .saturating_add(STEPS)
    .saturating_add(REMOVAL)
    .saturating_add(PATIENCE)
    .saturating_add(PATIENCE);
// The slow profile ends a test at 600 s (`.config/nextest.toml`); 10 s are
// left for the rest.
const _: () = assert!(WORST.as_secs() + 10 <= 600);

/// The runner's line for an entry of a script's log.
pub(crate) const LOGGED: &str = "the bot's script logged";
/// The runner's line for a script that started.
pub(crate) const STARTED: &str = "the bot's script started";
/// The actor's line for a respawn the server confirmed.
pub(crate) const CONFIRMED: &str = "the server confirmed the respawn";

/// Runs `steps` against a new offline-mode Minecraft server and the world
/// beside it, bounded by [`STEPS`]; removes the container, then stops the
/// agent the steps hand back and the world. Returns what the steps saw.
pub(crate) async fn on_a_server<T, F, Fut>(steps: F) -> T
where
    T: 'static,
    F: FnOnce(Rc<World>, Rc<Minecraft>) -> Fut,
    Fut: Future<Output = (Agent, T)> + 'static,
{
    let (world, minecraft) = tokio::join!(
        World::new(ServerOptions::default()),
        Minecraft::start_offline()
    );
    let world = Rc::new(world);
    let minecraft = Rc::new(minecraft.expect("the Minecraft container"));

    // A local task: the world holds this thread's log capture.
    let local = LocalSet::new();
    let mut stepping = local.spawn_local(steps(Rc::clone(&world), Rc::clone(&minecraft)));
    let ended = local
        .run_until(tokio::time::timeout(STEPS, &mut stepping))
        .await;
    if ended.is_err() {
        stepping.abort();
        // Every await in the steps yields, so the aborted task ends at its
        // next one, dropping what it held.
        let _ = local.run_until(&mut stepping).await;
    }
    let minecraft = Rc::into_inner(minecraft).expect("the steps' task let the container go");
    let removed = minecraft.remove(REMOVAL).await;
    let (mut agent, seen) = match ended {
        Ok(Ok(done)) => done,
        Ok(Err(panicked)) => std::panic::resume_unwind(panicked.into_panic()),
        Err(elapsed) => panic!(
            "the steps didn't end within {STEPS:?}: {elapsed}\n{}",
            world.logs.buffer().text()
        ),
    };
    agent.stop().await;
    let world = Rc::into_inner(world).expect("the steps' task let the world go");
    world.end(vec![agent]).await;
    assert_eq!(removed, Ok(()));
    seen
}

/// `draft` as the API's body: the server sets the level itself.
pub(crate) fn request(draft: ScriptDraft) -> ModeScriptRequest {
    let mut body = serde_json::to_value(draft).expect("a draft's JSON");
    body.as_object_mut().expect("an object").remove("api");
    serde_json::from_value(body).expect("the API's body")
}

/// The reference's example `id`, as a scripted mode of the Owner's, as
/// written: its script, its settings and what it declares.
pub(crate) async fn example(world: &World, id: &str) -> ModeId {
    let example = EXAMPLES
        .iter()
        .find(|example| example.id == id)
        .expect("an example of that name");
    world
        .owner
        .scripted_mode(example.title, request(example.draft()))
        .await
        .id
}

/// A new offline account `name`, and its bot pointed at `minecraft`.
pub(crate) async fn bot_on(world: &World, minecraft: &Minecraft, name: &str) -> BotId {
    let bot = world.owner.bot(name).await;
    world.owner.point(bot, &minecraft.address()).await;
    bot
}

/// `command`'s output through RCON.
pub(crate) async fn rcon(minecraft: &Minecraft, command: &str) -> String {
    minecraft
        .rcon(command, RCON)
        .await
        .unwrap_or_else(|why| panic!("{why}"))
}

/// Whether the `execute` test `command` passes.
pub(crate) async fn passes(minecraft: &Minecraft, command: &str) -> bool {
    rcon(minecraft, command).await.contains("Test passed")
}

/// The numbers in the list at the end of `output`: `[0.5d, -60.0d, 0.5d]`.
fn numbers(output: &str) -> Vec<f64> {
    output
        .rsplit_once('[')
        .and_then(|(_, list)| list.split_once(']'))
        .map(|(list, _)| {
            list.split(',')
                .filter_map(|number| number.trim().trim_end_matches(['d', 'f']).parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Where `name`'s feet are, as the server has it.
pub(crate) async fn position(minecraft: &Minecraft, name: &str) -> [f64; 3] {
    let output = rcon(minecraft, &format!("data get entity {name} Pos")).await;
    match numbers(&output).as_slice() {
        [x, y, z] => [*x, *y, *z],
        _ => panic!("no position in {output:?}"),
    }
}

/// Where `name` looks, `[yaw, pitch]`, as the server has it.
pub(crate) async fn rotation(minecraft: &Minecraft, name: &str) -> [f64; 2] {
    let output = rcon(minecraft, &format!("data get entity {name} Rotation")).await;
    match numbers(&output).as_slice() {
        [yaw, pitch] => [*yaw, *pitch],
        _ => [f64::NAN; 2],
    }
}

/// Whether `[yaw, pitch]` is `look` within half a degree: azalea rounds a
/// look to the mouse's steps.
pub(crate) fn facing([yaw, pitch]: [f64; 2], look: [f64; 2]) -> bool {
    let off = (yaw - look[0]).rem_euclid(360.0);
    off.min(360.0 - off) < 0.5 && (pitch - look[1]).abs() < 0.5
}

/// `name`'s score in `objective`, 0 before its first.
pub(crate) async fn score(minecraft: &Minecraft, name: &str, objective: &str) -> i64 {
    let output = rcon(
        minecraft,
        &format!("scoreboard players get {name} {objective}"),
    )
    .await;
    // "AfkBot1 has 50 [dealt]"; "Can't get value of dealt for AfkBot1; none is set".
    output
        .split_once(" has ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .and_then(|number| number.parse().ok())
        .unwrap_or(0)
}

/// An entry of a script's log, as the runner logged it: its kind and what
/// it says, in Rust's debug form (`Sent { key: Ident("pay"), … }`), and
/// when.
#[derive(Debug, Clone)]
pub(crate) struct Logged {
    pub(crate) entry: String,
    pub(crate) at: DateTime<Utc>,
}

/// Every entry of the scripts' logs so far, in order.
pub(crate) fn logged(world: &World) -> Vec<Logged> {
    world
        .logs
        .buffer()
        .lines_with(LOGGED)
        .unwrap()
        .iter()
        .filter_map(|line| {
            let entry = line["entry"].as_str()?.to_owned();
            let at = DateTime::parse_from_rfc3339(line["timestamp"].as_str()?)
                .ok()?
                .with_timezone(&Utc);
            Some(Logged { entry, at })
        })
        .collect()
}

/// How many entries of the scripts' logs start with `kind`
/// (`WindowOpened`, `Matched`, …) and hold `holding`.
pub(crate) fn entries(world: &World, kind: &str, holding: &str) -> usize {
    logged(world)
        .iter()
        .filter(|logged| logged.entry.starts_with(kind) && logged.entry.contains(holding))
        .count()
}

/// Waits until the scripts' logs hold `count` entries of `kind` holding
/// `holding`.
pub(crate) async fn until_logged(world: &World, kind: &str, holding: &str, count: usize) {
    world
        .logs
        .until(&format!("{count} {kind} holding {holding:?}"), async || {
            entries(world, kind, holding) >= count
        })
        .await;
}

/// Waits until `count` scripts have started.
pub(crate) async fn started(world: &World, count: usize) {
    world
        .logs
        .until(&format!("{count} scripts started"), async || {
            world.logs.count(STARTED) >= count
        })
        .await;
}

/// Starts `agent` on the azalea connector with room for `capacity` bots.
pub(crate) async fn agent_on_minecraft(world: &World, capacity: usize) -> Agent {
    let mut agent = world.agent("agent-a").await;
    agent.start_on_minecraft(crate::harness::agent::RunOptions::capacity(capacity));
    agent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rcon_lists_and_scores_read_as_numbers() {
        assert_eq!(
            numbers("AfkBot1 has the following entity data: [0.5d, -60.0d, 0.5d]"),
            [0.5, -60.0, 0.5]
        );
        assert_eq!(
            numbers("AfkBot1 has the following entity data: [90.0f, 10.0f]"),
            [90.0, 10.0]
        );
        assert!(facing([-270.0, 10.2], [90.0, 10.0]));
        assert!(!facing([0.0, 10.0], [90.0, 10.0]));
    }
}
