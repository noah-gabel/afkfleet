//! The slow on-respawn case (Plan.md P11.4; ADR-0020, S26): the real
//! server and the real managed agent, its bot on fleet-mc's azalea
//! connector, against an offline-mode Minecraft server in Docker, pinned as
//! `deploy/compose.dev.yaml` pins it. `just test-slow` runs it.
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
//! - the chat once: the second death comes less than 30 s after the first,
//!   so its chat is skipped (S24).
//!
//! A jump isn't used: right after the load the bot isn't standing yet, so
//! the server counts no jump (seen while building C3).
//!
//! Every wait for the agent is the log's (`Logs::within` and `until`); RCON
//! is read until the server shows the look, bounded. Once each respawn's
//! chat went out or was skipped, the history is read after each ingest
//! batch until it holds every mode line the agent sent, while the bot still
//! runs: the ingest drops the lines of a bot the server no longer places.
//! The steps are bounded by [`STEPS`], and the whole case stays inside the
//! slow profile's 600 s.

use core::time::Duration;

use fleet_api_types::chat::ChatLineKind;
use fleet_api_types::modes::{
    ModeActionKind, ModeActionRequest, ModeScheduleKind, ModeScheduleRequest, ModeStepRequest,
    ModeSteps,
};
use fleet_core::id::BotId;
use tokio::time::Instant;

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
/// The retry's line for a respawn the server didn't confirm in time.
const UNCONFIRMED: &str = "the server didn't confirm a respawn";
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

/// The steps after the container is up: the bot online, two deaths and the
/// reads.
const STEPS: Duration = Duration::from_secs(150);
/// One RCON command.
const RCON: Duration = Duration::from_secs(10);
/// How long one kill waits for its confirmed respawn, a few retries'
/// worth: a respawned player takes no damage until the server has its
/// `PlayerLoaded`, so a kill right after a respawn may be lost (P3.6).
const KILL_WAIT: Duration = Duration::from_secs(10);
/// How many kills one death may take.
const KILLS: usize = 4;
/// How many times the rotation is read before the look must show.
const READS: usize = 20;
/// The container's removal.
const REMOVAL: Duration = Duration::from_secs(20);

// The container's start, the steps, the agent's stop and the removal stay
// inside the slow profile's 600 s.
const _: () =
    assert!(STARTUP.as_secs() + STEPS.as_secs() + PATIENCE.as_secs() + REMOVAL.as_secs() < 600);

/// What the case saw, checked after the teardown.
#[derive(Debug)]
struct Seen {
    looked_at_start: bool,
    looked_after: [bool; 2],
    confirmed: usize,
    unconfirmed: usize,
    between: Duration,
    said_back: usize,
}

#[tokio::test]
async fn the_on_respawn_steps_run_after_each_confirmed_respawn_and_chat_once() {
    let (world, minecraft) = tokio::join!(
        World::new(ServerOptions::default()),
        Minecraft::start_offline()
    );
    let minecraft = minecraft.expect("the Minecraft container");
    let mut agent = world.agent("agent-a").await;
    agent.start_on_minecraft(RunOptions::capacity(1));

    let seen = tokio::time::timeout(STEPS, steps(&world, &agent, &minecraft)).await;

    agent.stop().await;
    world.end(vec![agent]).await;
    let removed = minecraft.remove(REMOVAL).await;
    let seen = seen.expect("the steps didn't end within their bound");
    assert_eq!(removed, Ok(()));
    assert!(!seen.looked_at_start, "no on-respawn step at start");
    assert_eq!(
        seen.looked_after,
        [true, true],
        "the look after each respawn"
    );
    assert_eq!(seen.confirmed, 2, "two respawns were confirmed");
    assert_eq!(seen.unconfirmed, 0, "every respawn was confirmed in time");
    assert!(
        seen.between < Duration::from_secs(30),
        "the second death came {:?} after the first: too late to show the 30 s skip",
        seen.between
    );
    assert_eq!(seen.said_back, 1, "the second respawn's chat was skipped");
}

/// The case's steps; see the [module docs](self).
async fn steps(world: &World, agent: &Agent, minecraft: &Minecraft) -> Seen {
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
    world.online(agent, bot, 0).await;
    let looked_at_start = facing_the_look(rotation(minecraft).await);

    kill(world, minecraft, 1).await;
    let first = Instant::now();
    let after_the_first = looks(minecraft).await;
    kill(world, minecraft, 2).await;
    let between = first.elapsed();
    let after_the_second = looks(minecraft).await;

    let said_back = stored(world, bot).await;
    Seen {
        looked_at_start,
        looked_after: [after_the_first, after_the_second],
        confirmed: world.logs.count(CONFIRMED),
        unconfirmed: world.logs.count(UNCONFIRMED),
        between,
        said_back,
    }
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
/// [`READS`] reads.
async fn looks(minecraft: &Minecraft) -> bool {
    for _ in 0..READS {
        if facing_the_look(rotation(minecraft).await) {
            return true;
        }
    }
    false
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
