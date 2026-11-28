//! Phase 16's check on the Paper harness (Plan.md P16.17; ADR-0036, M9,
//! M13, M17; ADR-0049; ADR-0054): the reference's five level-2 examples,
//! each saved through the API as the editor's starter template sets it, as
//! written, sent by the real server to the real managed agent and run by
//! its bot on fleet-mc's azalea connector against the harness, beside a
//! second player who buys first; and a script's every way out, after
//! which nothing is held and no screen is open. `just test-slow` runs them.
//!
//! - [`examples`]: the sell macro, the auction scan with the other player
//!   buying the listing it picked, the farm that sells, and both listers.
//! - [`ways_out`]: a stop, a limit, a disconnect and a mode change, each in
//!   the middle of a click.
//!
//! What happened is read from the harness's own records (the plugin's
//! `harness-…` lines and `harness state`), and what the script said from
//! the runner's lines. As Phase 15's `slow_scripts`: offline accounts only,
//! on the local container; the bots dial only `127.0.0.1`; RCON runs
//! inside the container. A case's steps run in a task of their own,
//! bounded, so a panic or the bound comes back here and the container is
//! always removed first.

mod examples;
mod ways_out;

use core::future::Future;
use core::time::Duration;
use std::rc::Rc;
use std::time::Instant;

use fleet_api_types::modes::ModeSteps;
use fleet_core::id::{BotId, ModeId};
use tokio::task::LocalSet;

use crate::harness::agent::{Agent, RunOptions};
use crate::harness::logs::PATIENCE;
use crate::harness::paper::{self, Paper, Record, records};
use crate::harness::server::ServerOptions;
use crate::harness::world::World;

/// The scripted bot's offline account.
pub(crate) const NAME: &str = "AfkBot1";
/// The second player, who buys first.
pub(crate) const OTHER: &str = "AfkBot2";
/// One RCON command.
const RCON: Duration = Duration::from_secs(10);
/// Reading the server's whole log.
const LOG: Duration = Duration::from_secs(20);
/// The container's removal.
const REMOVAL: Duration = Duration::from_secs(20);
/// How often a wait reads the server's log at most: it's read whole.
const READ_EVERY: Duration = Duration::from_millis(250);

/// Whether a case whose steps are bounded by `steps` ends within the slow
/// profile's 600 s, with 10 s to spare: the harness's start, the steps,
/// the removal, then the agent's and the server's stops.
pub(crate) const fn fits(steps: Duration) -> bool {
    paper::STARTUP
        .saturating_add(steps)
        .saturating_add(REMOVAL)
        .saturating_add(PATIENCE)
        .saturating_add(PATIENCE)
        .as_secs()
        + 10
        <= 600
}

/// Runs `steps` against a new Paper harness and the world beside it,
/// bounded by `within`; removes the container, then stops the agent the
/// steps hand back and the world. Returns what the steps saw.
pub(crate) async fn on_paper<T, F, Fut>(world: World, within: Duration, steps: F) -> T
where
    T: 'static,
    F: FnOnce(Rc<World>, Rc<Paper>) -> Fut,
    Fut: Future<Output = (Agent, T)> + 'static,
{
    let paper = Paper::start().await.unwrap_or_else(|why| panic!("{why}"));
    let world = Rc::new(world);
    let paper = Rc::new(paper);

    // A local task: the world holds this thread's log capture.
    let local = LocalSet::new();
    let mut stepping = local.spawn_local(steps(Rc::clone(&world), Rc::clone(&paper)));
    let ended = local
        .run_until(tokio::time::timeout(within, &mut stepping))
        .await;
    if ended.is_err() {
        stepping.abort();
        // Every await in the steps yields, so the aborted task ends at its
        // next one, dropping what it held.
        let _ = local.run_until(&mut stepping).await;
    }
    let paper = Rc::into_inner(paper).expect("the steps' task let the harness go");
    let tail = if ended.is_err() {
        paper.log(LOG).await.unwrap_or_default()
    } else {
        String::new()
    };
    let removed = paper.remove(REMOVAL).await;
    let (mut agent, seen) = match ended {
        Ok(Ok(done)) => done,
        Ok(Err(panicked)) => std::panic::resume_unwind(panicked.into_panic()),
        Err(elapsed) => panic!(
            "the steps didn't end within {within:?}: {elapsed}\n{}\n--- the harness ---\n{}",
            world.logs.buffer().text(),
            harness_lines(&tail)
        ),
    };
    agent.stop().await;
    let world = Rc::into_inner(world).expect("the steps' task let the world go");
    world.end(vec![agent]).await;
    assert_eq!(removed, Ok(()));
    seen
}

/// A world for a case of examples: the server's and the agent's lines on
/// this thread.
pub(crate) async fn world() -> World {
    World::new(ServerOptions::default()).await
}

/// The harness's own lines in `log`, for a failure's message.
fn harness_lines(log: &str) -> String {
    log.lines()
        .filter(|line| line.contains("harness-") || line.contains("lost connection"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `command`'s output through RCON.
pub(crate) async fn rcon(paper: &Paper, command: &str) -> String {
    paper
        .rcon(command, RCON)
        .await
        .unwrap_or_else(|why| panic!("{command}: {why}"))
}

/// Puts `item` (the game's syntax) into `player`'s slot `slot` (`hotbar.0`,
/// `inventory.12`).
pub(crate) async fn give(paper: &Paper, player: &str, slot: &str, item: &str) {
    let said = rcon(
        paper,
        &format!("item replace entity {player} {slot} with {item}"),
    )
    .await;
    assert!(!said.contains("rror") && !said.contains("nknown"), "{said}");
}

/// What the server has of `player`: `harness state`.
pub(crate) async fn state(paper: &Paper, player: &str) -> Record {
    online_state(paper, player)
        .await
        .unwrap_or_else(|| panic!("{player} isn't online"))
}

/// `harness state` for `player`, or `None` while it isn't online.
pub(crate) async fn online_state(paper: &Paper, player: &str) -> Option<Record> {
    let said = rcon(paper, &format!("harness state {player}")).await;
    if said == "not online" {
        return None;
    }
    Some(Record::parse(&said).unwrap_or_else(|| panic!("no state in {said:?}")))
}

/// Every record of the harness so far.
pub(crate) async fn harness(paper: &Paper) -> Vec<Record> {
    records(&paper.log(LOG).await.unwrap_or_else(|why| panic!("{why}")))
}

/// Waits until the harness's records meet `done`, reading the server's log
/// at the log's wakes, at most every [`READ_EVERY`]; returns the records
/// that met it.
pub(crate) async fn until_records(
    world: &World,
    paper: &Paper,
    what: &str,
    mut done: impl FnMut(&[Record]) -> bool,
) -> Vec<Record> {
    let mut last: Option<Instant> = None;
    let mut met = Vec::new();
    world
        .logs
        .until(what, async || {
            if last.is_some_and(|at| at.elapsed() < READ_EVERY) {
                return false;
            }
            last = Some(Instant::now());
            let read = harness(paper).await;
            let done = done(&read);
            if done {
                met = read;
            }
            done
        })
        .await;
    met
}

/// Waits until `harness state` for `player` meets `done`, read at the
/// log's wakes, at most every [`READ_EVERY`]; returns that state.
pub(crate) async fn until_state(
    world: &World,
    paper: &Paper,
    player: &str,
    what: &str,
    mut done: impl FnMut(&Record) -> bool,
) -> Record {
    let mut last: Option<Instant> = None;
    let mut met = None;
    world
        .logs
        .until(what, async || {
            if last.is_some_and(|at| at.elapsed() < READ_EVERY) {
                return false;
            }
            last = Some(Instant::now());
            let Some(now) = online_state(paper, player).await else {
                return false;
            };
            let done = done(&now);
            if done {
                met = Some(now);
            }
            done
        })
        .await;
    met.expect("waited for it")
}

/// A managed agent on the azalea connector with room for both bots.
pub(crate) async fn agent(world: &World) -> Agent {
    let mut agent = world.agent("agent-a").await;
    agent.start_on_minecraft(RunOptions::capacity(2));
    agent
}

/// A new offline account `name`, and its bot pointed at the harness.
pub(crate) async fn bot_on(world: &World, paper: &Paper, name: &str) -> BotId {
    let bot = world.owner.bot(name).await;
    world.owner.point(bot, &paper.address()).await;
    bot
}

/// The second player online on an idle step mode, with money to buy.
pub(crate) async fn the_other_player(world: &World, paper: &Paper, agent: &Agent) -> BotId {
    let other = bot_on(world, paper, OTHER).await;
    let idle = world.owner.mode_of("Idle", ModeSteps(Vec::new())).await.id;
    world.owner.set_mode(other, idle).await;
    world.owner.start(other).await;
    world.online(agent, other, 0).await;
    rcon(paper, &format!("harness money {OTHER} 1000000")).await;
    other
}

/// The scripted bot on `mode`, started and online, its script started:
/// the `count`th script to start in this world.
pub(crate) async fn scripted_bot(
    world: &World,
    paper: &Paper,
    agent: &Agent,
    mode: ModeId,
    count: usize,
) -> BotId {
    let bot = bot_on(world, paper, NAME).await;
    world.owner.set_mode(bot, mode).await;
    world.owner.start(bot).await;
    world.online(agent, bot, 0).await;
    crate::slow_scripts::started(world, count).await;
    bot
}

/// Kicks `player` through RCON, as an operator does: the bot comes back.
pub(crate) async fn kick(paper: &Paper, player: &str) {
    rcon(paper, &format!("kick {player}")).await;
}
