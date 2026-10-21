//! The slow on-respawn case (Plan.md P11.4; ADR-0020, S26): the real
//! server and the real managed agent, its bot on fleet-mc's azalea
//! connector, against an offline-mode Minecraft server in Docker, pinned as
//! `deploy/compose.dev.yaml` pins it. `just test-slow` runs it.
//!
//! An offline account only, on the local container: no Microsoft account,
//! no token, and no vault key but the loopback server's fixed test key,
//! held in memory. The agent's `private_targets` are `127.0.0.1/32`, and its
//! bot dials only `127.0.0.1`, the container's published game port.
//!
//! A mode whose on-respawn steps look east and say `back` is created and set
//! through the API, so it crosses the stored JSON and the proto. The bot is
//! killed twice through RCON, `rcon-cli` inside the container, so no
//! password is ever on a command line. Each death is respawned, the
//! server's confirmation comes (`Respawned`, the actor's `debug` line), and
//! the steps run:
//! - the look after each respawn: a respawned player faces the spawn's
//!   direction, so the server sees the look again only if the step ran
//!   again; and never at start, where the bot keeps its join's direction;
//! - the chat once: the second confirmation comes less than 30 s after the
//!   first, by their lines' timestamps, so its chat is skipped (S24).
//!
//! A jump isn't used: right after the load the bot isn't standing yet, so
//! the server counts no jump (seen while building C3).
//!
//! Every wait is the log's (`Logs::within` and `until`): RCON is read again
//! at each wake until the server shows the look, within [`LOOK_WAIT`] (the
//! user's choice, ADR-0020, U9). Once each respawn's chat went out or was
//! skipped, the history is read after each ingest batch until it holds
//! every mode line the agent sent, while the bot still runs: the ingest
//! drops the lines of a bot the server no longer places (U10).
//!
//! The steps run in a task of their own, bounded by [`STEPS`], so a panic
//! or the bound comes back here and the container is always removed first,
//! before the agent's and the server's stops, which may fail too; the
//! steps' panic goes on after that. [`WORST`], every bound added up, stays
//! inside the slow profile's 600 s.

use core::time::Duration;
use std::rc::Rc;

use chrono::DateTime;
use fleet_api_types::chat::ChatLineKind;
use fleet_api_types::modes::{
    ModeActionKind, ModeActionRequest, ModeScheduleKind, ModeScheduleRequest, ModeStepRequest,
    ModeSteps,
};
use fleet_core::id::BotId;
use tokio::task::LocalSet;

use crate::harness::agent::{Agent, RunOptions};
use crate::harness::client::on_respawn;
use crate::harness::logs::PATIENCE;
use crate::harness::server::ServerOptions;
use crate::harness::world::World;
use crate::managed_check::minecraft::{Minecraft, STARTUP};

/// The bot's offline account.
const NAME: &str = "AfkBot1";
/// The actor's line for a respawn the server confirmed.
const CONFIRMED: &str = "the server confirmed the respawn";
/// The retry's lines for a call that failed or that the server didn't
/// confirm in time: each death's first at `warn`, the later ones at `debug`.
const RETRIES: [&str; 4] = [
    "a respawn failed; retrying",
    "a respawn failed again",
    "the server didn't confirm a respawn; retrying",
    "the server didn't confirm a respawn again",
];
/// The agent's line for a mode's chat it sent.
const SENT: &str = "the bot's mode sent chat";
/// The runner's line for an on-respawn chat it skipped.
const SKIPPED: &str =
    "the on-respawn chat was skipped: the last one went out less than 30 s before";
/// The ingest's line for a batch it stored.
const BATCH: &str = "the ingest wrote a batch";
/// What the mode says on respawn.
const BACK: &str = "back";
/// Where the mode looks on respawn: east, a little down.
const LOOK: [f32; 2] = [-90.0, 30.0];

/// The steps' budget, past which they're stopped and teardown runs: the
/// agent's enrollment, the bot online, two deaths and the reads.
const STEPS: Duration = Duration::from_secs(75);
/// One RCON command.
const RCON: Duration = Duration::from_secs(10);
/// How long one kill waits for its confirmed respawn, a few retries'
/// worth: a respawned player takes no damage until the server has its
/// `PlayerLoaded`, so a kill right after a respawn may be lost (P3.6).
const KILL_WAIT: Duration = Duration::from_secs(10);
/// How many kills one death may take.
const KILLS: usize = 3;
/// How long the server may take to show the look after a respawn.
const LOOK_WAIT: Duration = Duration::from_secs(10);
/// How far inside the 30 s rule the second confirmation must come, so the
/// runner's clock can't see 30 s where the log's timestamps saw less.
const SKIP_MARGIN: Duration = Duration::from_secs(5);
/// The container's removal.
const REMOVAL: Duration = Duration::from_secs(20);
/// The case's worst case: the container's start (the world starts beside
/// it), the steps, the removal, then the agent's and the server's stops,
/// each bounded by the harness's [`PATIENCE`].
const WORST: Duration = STARTUP
    .saturating_add(STEPS)
    .saturating_add(REMOVAL)
    .saturating_add(PATIENCE)
    .saturating_add(PATIENCE);
// The slow profile ends a test at 600 s (`.config/nextest.toml`: 60 s, ten
// times); 20 s are left for the rest, as the managed check leaves them.
const _: () = assert!(WORST.as_secs() + 20 <= 600);

/// What the case saw, checked after the teardown.
#[derive(Debug)]
struct Seen {
    looked_at_start: bool,
    looked_after: [bool; 2],
    confirmed: usize,
    retried: usize,
    between: Option<Duration>,
    said_back: usize,
}

#[tokio::test]
async fn the_on_respawn_steps_run_after_each_confirmed_respawn_and_chat_once() {
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
        Err(elapsed) => panic!("the steps didn't end within {STEPS:?}: {elapsed}"),
    };
    agent.stop().await;
    let world = Rc::into_inner(world).expect("the steps' task let the world go");
    world.end(vec![agent]).await;

    assert_eq!(removed, Ok(()));
    assert!(!seen.looked_at_start, "no on-respawn step at start");
    assert_eq!(
        seen.looked_after,
        [true, true],
        "the look after each respawn"
    );
    assert_eq!(seen.confirmed, 2, "two respawns were confirmed");
    assert_eq!(seen.retried, 0, "every respawn was confirmed at once");
    assert!(
        seen.between
            .is_some_and(|between| between + SKIP_MARGIN < Duration::from_secs(30)),
        "the second confirmation came {:?} after the first: too late to show the 30 s skip",
        seen.between
    );
    assert_eq!(seen.said_back, 1, "the second respawn's chat was skipped");
}

/// The case's steps; see the [module docs](self). Returns the running agent
/// for the teardown, with what they saw.
async fn steps(world: Rc<World>, minecraft: Rc<Minecraft>) -> (Agent, Seen) {
    let mut agent = world.agent("agent-a").await;
    agent.start_on_minecraft(RunOptions::capacity(1));
    let bot = world.owner.bot(NAME).await;
    world.owner.point(bot, &minecraft.address()).await;
    let mode = world
        .owner
        .mode_of(
            "Back again",
            ModeSteps(vec![
                looking(),
                on_respawn(ModeActionKind::SendChat, Some(BACK)),
            ]),
        )
        .await;
    world.owner.set_mode(bot, mode.id).await;
    world.owner.start(bot).await;
    world.online(&agent, bot, 0).await;
    let looked_at_start = facing_the_look(rotation(&minecraft).await);

    kill(&world, &minecraft, 1).await;
    let after_the_first = looks(&world, &minecraft).await;
    kill(&world, &minecraft, 2).await;
    let after_the_second = looks(&world, &minecraft).await;

    let said_back = stored(&world, bot).await;
    let seen = Seen {
        looked_at_start,
        looked_after: [after_the_first, after_the_second],
        confirmed: world.logs.count(CONFIRMED),
        retried: RETRIES.iter().map(|line| world.logs.count(line)).sum(),
        between: between_confirmations(&world),
        said_back,
    };
    (agent, seen)
}

/// How many `back` lines `bot`'s history holds once each respawn's chat
/// went out or was skipped and the history holds every one the agent sent:
/// the ingest stores them in batches, so the history is read again after
/// each batch, never between two.
async fn stored(world: &World, bot: BotId) -> usize {
    world
        .logs
        .until("each respawn's chat went out or was skipped", async || {
            world.logs.count(SENT) + world.logs.count(SKIPPED) >= 2
        })
        .await;
    let mut batches = None;
    let mut said_back = 0;
    world
        .logs
        .until("the mode's chat lines are stored", async || {
            let now = world.logs.count(BATCH);
            if batches == Some(now) {
                return false;
            }
            batches = Some(now);
            said_back = world
                .owner
                .history(bot)
                .await
                .items
                .iter()
                .filter(|line| {
                    line.kind.known() == Some(ChatLineKind::Mode) && line.text.as_str() == BACK
                })
                .count();
            said_back > 0 && said_back == world.logs.count(SENT)
        })
        .await;
    said_back
}

/// The time from the first confirmation's line to the second's, by their
/// timestamps; `None` without two.
fn between_confirmations(world: &World) -> Option<Duration> {
    let lines = world.logs.buffer().lines_with(CONFIRMED).unwrap();
    let times: Vec<_> = lines
        .iter()
        .filter_map(|line| line["timestamp"].as_str())
        .filter_map(|at| DateTime::parse_from_rfc3339(at).ok())
        .collect();
    let [first, second, ..] = times.as_slice() else {
        return None;
    };
    (*second - *first).to_std().ok()
}

/// The mode's on-respawn look.
fn looking() -> ModeStepRequest {
    let [yaw, pitch] = LOOK;
    ModeStepRequest {
        action: ModeActionRequest {
            kind: ModeActionKind::Look,
            yaw: Some(yaw),
            pitch: Some(pitch),
            max_yaw: None,
            max_pitch: None,
            on: None,
            slot: None,
            message: None,
        },
        schedule: ModeScheduleRequest {
            kind: ModeScheduleKind::OnRespawn,
            interval_ms: None,
            jitter_ms: None,
        },
        probability: 100,
    }
}

/// Kills the bot until the server has confirmed `confirmed` respawns.
async fn kill(world: &World, minecraft: &Minecraft, confirmed: usize) {
    for _ in 0..KILLS {
        rcon(minecraft, &format!("kill {NAME}")).await;
        let came = world
            .logs
            .within(KILL_WAIT, async || world.logs.count(CONFIRMED) >= confirmed)
            .await;
        if came {
            return;
        }
    }
    panic!(
        "respawn {confirmed} wasn't confirmed after {KILLS} kills\n{}",
        world.logs.buffer().text()
    );
}

/// Whether the server shows the bot facing the mode's look within
/// [`LOOK_WAIT`], the rotation read again at each of the log's wakes.
async fn looks(world: &World, minecraft: &Minecraft) -> bool {
    world
        .logs
        .within(LOOK_WAIT, async || {
            facing_the_look(rotation(minecraft).await)
        })
        .await
}

/// Whether `rotation` is the mode's look, within half a degree: azalea
/// rounds a look to the mouse's steps, as fleet-mc's own check allows.
fn facing_the_look([yaw, pitch]: [f32; 2]) -> bool {
    let [look_yaw, look_pitch] = LOOK;
    let yaw_off = (yaw - look_yaw).rem_euclid(360.0);
    yaw_off.min(360.0 - yaw_off) < 0.5 && (pitch - look_pitch).abs() < 0.5
}

/// The bot's rotation as the server has it, `[yaw, pitch]`.
async fn rotation(minecraft: &Minecraft) -> [f32; 2] {
    let output = rcon(minecraft, &format!("data get entity {NAME} Rotation")).await;
    let numbers: Vec<f32> = output
        .rsplit_once('[')
        .and_then(|(_, list)| list.split_once(']'))
        .map(|(list, _)| {
            list.split(',')
                .filter_map(|number| number.trim().trim_end_matches('f').parse().ok())
                .collect()
        })
        .unwrap_or_default();
    match numbers.as_slice() {
        [yaw, pitch] => [*yaw, *pitch],
        _ => [f32::NAN; 2],
    }
}

/// `command`'s output through RCON.
async fn rcon(minecraft: &Minecraft, command: &str) -> String {
    minecraft
        .rcon(command, RCON)
        .await
        .unwrap_or_else(|why| panic!("{why}"))
}
