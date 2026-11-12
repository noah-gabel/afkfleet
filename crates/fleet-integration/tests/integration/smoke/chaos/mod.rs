//! P13.5's chaos tests on the real images (Plan.md P13.5; ADR-0033): the
//! production compose project with a Minecraft server and real bots
//! ([`world`]), and Docker's own faults. `just chaos` builds both images and
//! runs them, one stack at a time; the Stack workflow runs it nightly.
//!
//! - **Kills and stops:** the server's, the agent's and the Minecraft
//!   server's containers, each with `SIGKILL` and with a graceful stop, a
//!   write acknowledged right before and chat flowing meanwhile.
//! - **A partition:** the agent cut off the fleet network. The server frees
//!   its stream within the heartbeat timeout (ADR-0019, S23), its bots stay
//!   its own and read `unknown` (S32), the agent notices within its
//!   keepalive (S67), a bot kicked meanwhile rejoins with what its agent
//!   kept, and once the network heals its requests go to the server again
//!   (S63).
//! - **The placement gate:** after the server restarts while an agent stays
//!   away, nothing new is placed for 90 s (S45).
//! - **A full disk:** the database's volume and the backups', capped and
//!   filled. Reads work, writes fail with the generic error and a log line,
//!   retention and the backups say so in their metrics, and once there's
//!   room again everything goes on with nothing restarted or repaired.
//! - **A lost volume:** without its database the server refuses to start,
//!   and a restored backup brings the fleet back.
//!
//! After every fault ([`Chaos::after_the_fault`]): every bot back at its
//! desired state, no account ever logged in twice, every acknowledged write
//! there with its audit entry, the database sound by SQLite's own check,
//! every secret's key configured, and a new backup, which each test restores
//! at its end with `restore`'s own steps ([`Chaos::end`]).
//!
//! The exact kill points (a renewal, a backup, a retention run, an ingest
//! batch, a refresh) are `crate::crash`'s, which needs no Docker.
//!
//! Every wait is bounded and wakes on a service's next log line.

mod world;

use core::time::Duration;
use std::time::Instant;

use fleet_api_types::agents::AgentStatus;
use fleet_api_types::bots::BotStatusKind;
use fleet_client::ClientError;
use fleet_proto::control::{KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT};

use self::world::{BOTS, Chaos, LOCAL, SECOND, until};
use super::stack::{QUICK, SLOW};

/// The sum of every sample of the series `name` in `render`.
fn total(render: &str, name: &str) -> f64 {
    render
        .lines()
        .filter_map(|line| {
            let (series, value) = line.rsplit_once(' ')?;
            let series = series.split_once('{').map_or(series, |(bare, _)| bare);
            (series == name).then(|| value.parse::<f64>().ok())?
        })
        .sum()
}

/// The sample of `name` whose labels hold `label` in `render`, or 0.
fn labelled(render: &str, name: &str, label: &str) -> f64 {
    render
        .lines()
        .filter(|line| line.starts_with(name) && line.contains(label))
        .filter_map(|line| line.rsplit_once(' ')?.1.parse::<f64>().ok())
        .sum()
}

/// How many bots each of the two agents holds, as the API lists them.
async fn held(chaos: &Chaos) -> (u32, u32) {
    let agents = chaos.agents().await.expect("the server answers");
    let of = |name: &str| {
        agents
            .iter()
            .find(|agent| agent.name.as_str() == name)
            .map_or(0, |agent| agent.bots)
    };
    (of(LOCAL), of(SECOND))
}

/// Has every bot hear `lines` chat lines, through the Minecraft server's
/// console, so the agent's and the server's batches are busy when a fault
/// lands.
fn chatter(chaos: &mut Chaos, lines: usize) {
    for n in 0..lines {
        chaos.rcon(&["say", &format!("chaos line {n}")]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos_the_server_killed_and_stopped_loses_nothing_it_acknowledged() {
    let mut chaos = Chaos::start("afkfleet-chaos-server", 70, false).await;

    // SIGKILL, right after a write the API acknowledged, in the middle of
    // the ingest's batches.
    chaos.write("KilledServer").await;
    chatter(&mut chaos, 20);
    let killed = chaos.kill("server");
    assert!(
        killed.running || killed.exit_code == 137,
        "the server ended with {} after its kill",
        killed.exit_code
    );
    // The file as the kill left it, before anything opens it to write.
    chaos.assert_sound(false);
    chaos.bring_up("server");
    // The Owner's session is still good: no login in between.
    chaos.after_the_fault().await;
    chaos.log_in_again().await;
    for name in BOTS {
        assert_eq!(chaos.joins(name), 1, "{name} left its server");
    }
    assert_eq!(held(&chaos).await, (3, 0));

    // A graceful stop: exit 0, never Docker's kill.
    chaos.write("StoppedServer").await;
    chatter(&mut chaos, 20);
    assert_eq!(chaos.stop("server"), 0);
    chaos.assert_sound(false);
    chaos.bring_up("server");
    chaos.after_the_fault().await;
    chaos.log_in_again().await;
    for name in BOTS {
        assert_eq!(chaos.joins(name), 1, "{name} left its server");
    }
    assert_eq!(held(&chaos).await, (3, 0));
    chaos.end().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos_the_agent_killed_and_stopped_brings_each_bot_back_once() {
    let mut chaos = Chaos::start("afkfleet-chaos-agent", 80, false).await;

    // SIGKILL, with chat on its way to the server.
    chaos.write("KilledAgent").await;
    chatter(&mut chaos, 20);
    let killed = chaos.kill("agent");
    assert!(
        killed.running || killed.exit_code == 137,
        "the agent ended with {} after its kill",
        killed.exit_code
    );
    chaos.bring_up("agent");
    chaos.after_the_fault().await;
    // Its sessions died with it: each bot joined again, once, on the same
    // agent.
    for name in BOTS {
        assert_eq!(chaos.joins(name), 2, "{name}");
    }
    assert_eq!(held(&chaos).await, (3, 0));

    // A graceful stop: every bot leaves its server first, and it exits 0.
    chaos.write("StoppedAgent").await;
    assert_eq!(chaos.stop("agent"), 0);
    // The server shows it offline, and its bots stay its own (S32).
    chaos.agent(LOCAL, AgentStatus::Offline).await;
    assert_eq!(held(&chaos).await, (3, 0));
    chaos.bring_up("agent");
    chaos.after_the_fault().await;
    for name in BOTS {
        assert_eq!(chaos.joins(name), 3, "{name}");
    }
    assert_eq!(held(&chaos).await, (3, 0));
    chaos.end().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos_the_minecraft_server_killed_and_stopped_gets_every_bot_back() {
    let mut chaos = Chaos::start("afkfleet-chaos-minecraft", 90, false).await;

    // SIGKILL: every bot's connection dies without a word.
    chaos.write("KilledMinecraft").await;
    chaos.kill("minecraft");
    chaos.bring_up("minecraft");
    // The bots find it again by themselves.
    chaos.after_the_fault().await;
    for name in BOTS {
        assert!(chaos.joins(name) >= 2, "{name} never joined again");
    }
    let after_the_kill: Vec<usize> = BOTS.iter().map(|name| chaos.joins(name)).collect();

    // A graceful stop kicks every player with "Server closed".
    chaos.write("StoppedMinecraft").await;
    chaos.stop("minecraft");
    chaos.bring_up("minecraft");
    chaos.after_the_fault().await;
    for (name, before) in BOTS.iter().zip(after_the_kill) {
        assert!(chaos.joins(name) > before, "{name} never joined again");
    }
    assert_eq!(held(&chaos).await, (3, 0));
    chaos.end().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos_a_partition_between_the_agent_and_the_server_heals_with_every_bot_where_it_was() {
    let mut chaos = Chaos::start("afkfleet-chaos-partition", 100, false).await;
    let granted = |chaos: &mut Chaos| total(&chaos.metrics(), "afkfleet_session_grants_total");

    chaos.write("Partitioned").await;
    chaos.partition("agent");
    let cut = Instant::now();

    // The server frees the silent stream within its heartbeat timeout, 30 s
    // (S23), and the agent gives its own up within its keepalive (S67).
    chaos.agent(LOCAL, AgentStatus::Offline).await;
    assert!(
        cut.elapsed() < Duration::from_secs(60),
        "the server freed the agent after {:?}",
        cut.elapsed()
    );
    let compose = chaos.stack.compose.clone();
    until(&compose, "agent", "the agent noticed", async || {
        let log = compose
            .run(&["logs", "--no-color", "--no-log-prefix", "agent"], QUICK)
            .ok()?;
        log.stdout
            .contains("the control stream ended; the agent reconnects")
            .then_some(())
    })
    .await;
    assert!(
        cut.elapsed() < (KEEPALIVE_INTERVAL + KEEPALIVE_TIMEOUT) * 3,
        "the agent noticed after {:?}",
        cut.elapsed()
    );
    // Its bots stay its own, read "unknown", and play on (S32).
    for bot in chaos.bots.clone() {
        let shown = chaos.shown(bot).await.expect("the server answers");
        assert_eq!(shown.status.kind.known(), Some(BotStatusKind::Unknown));
    }
    assert_eq!(held(&chaos).await, (3, 0));
    // A bot kicked while its agent is cut off rejoins with what the agent
    // kept, since it can't ask the server (S63).
    chaos.rcon(&["kick", BOTS[0]]);
    chaos.joined(BOTS[0], 2).await;
    assert_eq!(held(&chaos).await, (3, 0), "time moved a bot");

    chaos.heal("agent");

    chaos.agent(LOCAL, AgentStatus::Online).await;
    chaos.after_the_fault().await;
    assert_eq!(chaos.joins(BOTS[0]), 2);
    assert_eq!(chaos.joins(BOTS[1]), 1);
    assert_eq!(chaos.joins(BOTS[2]), 1);
    assert_eq!(held(&chaos).await, (3, 0));
    // Healed, a kicked bot's request goes to the server again (S63).
    let before = granted(&mut chaos);
    chaos.rcon(&["kick", BOTS[1]]);
    chaos.joined(BOTS[1], 2).await;
    assert!(granted(&mut chaos) > before, "the agent never asked");
    chaos.end().await;
}

/// S45's gate: `PLACEMENT_GRACE`, in whole seconds.
const GRACE: Duration = Duration::from_secs(90);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos_after_a_restart_nothing_is_placed_until_every_agent_is_back_or_the_grace_passed() {
    let mut chaos = Chaos::start("afkfleet-chaos-gate", 110, false).await;

    // The local agent goes away, with its three bots, and stays away while
    // the server restarts.
    chaos.partition("agent");
    chaos.agent(LOCAL, AgentStatus::Offline).await;
    assert_eq!(chaos.stop("server"), 0);
    chaos.bring_up("server");
    let started = Instant::now();
    chaos.agent(SECOND, AgentStatus::Online).await;

    // A bot started now could go to the second agent at once. It doesn't:
    // the local one may still run bots the database doesn't show (S45).
    chaos.new_bot("AfkBot4").await;
    let new = *chaos.bots.last().expect("the new bot");
    let placed = until(
        &chaos.stack.compose,
        "server",
        "the new bot is placed",
        async || {
            let shown = chaos.shown(new).await?;
            (shown.status.kind.known() != Some(BotStatusKind::Unplaced)).then_some(())
        },
    );
    placed.await;
    assert!(
        started.elapsed() + Duration::from_secs(15) >= GRACE,
        "the new bot was placed {:?} after the start",
        started.elapsed()
    );
    // Then it's the second agent's, and the three stay the local one's.
    assert_eq!(held(&chaos).await, (3, 1));

    chaos.heal("agent");
    chaos.after_the_fault().await;
    assert_eq!(held(&chaos).await, (3, 1));
    for name in BOTS {
        assert_eq!(chaos.joins(name), 1, "{name} left its server");
    }
    chaos.end().await;
}

/// How long a write may take to fail on a full disk: SQLite's busy timeout
/// and the request's own.
const REFUSAL: Duration = Duration::from_secs(30);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos_a_full_disk_fails_writes_loudly_and_everything_goes_on_once_there_is_room() {
    let mut chaos = Chaos::start("afkfleet-chaos-disk", 120, true).await;
    chaos.write("DiskBefore").await;
    chaos.after_the_fault().await;
    let server = chaos.stack.container("server");
    // Every bot holds more lines than it keeps: the next retention run has
    // rows to delete.
    chatter(&mut chaos, 130);
    let runs = |chaos: &mut Chaos, outcome: &str| {
        labelled(
            &chaos.metrics(),
            "afkfleet_retention_runs_total",
            &format!("outcome=\"{outcome}\""),
        )
    };
    let failed_runs = runs(&mut chaos, "failed");
    let failed_backups = total(&chaos.metrics(), "afkfleet_backup_failures_total");

    // Both volumes, to their last byte; with no finished backup left, the
    // schedule finds one due.
    chaos.drop_backups();
    assert_eq!(chaos.fill("backups"), 0);
    assert_eq!(chaos.fill("data"), 0);

    // Reads keep working.
    assert!(chaos.agents().await.is_some(), "a read failed");
    for bot in chaos.bots.clone() {
        assert!(chaos.shown(bot).await.is_some(), "a read failed");
    }
    // A write fails with the generic error and a request ID, within the
    // request's own bound.
    let asked = Instant::now();
    let refused = chaos.try_write("DiskFull").await;
    assert!(asked.elapsed() < REFUSAL, "{:?}", asked.elapsed());
    let Err(ClientError::Api(failure)) = &refused else {
        panic!("a write on a full disk wasn't refused by the API: {refused:?}");
    };
    assert_eq!(failure.status, 500, "{failure:?}");
    assert_ne!(failure.request_id.as_str(), "");
    // The log names that request at `error`, with SQLite's own words, which
    // never reach the client.
    let log = chaos
        .stack
        .ok(&["logs", "--no-color", "--no-log-prefix", "server"], QUICK);
    assert!(
        log.lines()
            .any(|line| line.contains(failure.request_id.as_str())
                && line.contains("\"level\":\"ERROR\"")),
        "no error line for the refused write"
    );
    // The command says the same of a backup, and publishes nothing.
    let backup = chaos
        .stack
        .compose
        .run(
            &[
                "exec",
                "-T",
                "server",
                "/usr/local/bin/afkfleet-server",
                "backup",
                "--config",
                super::stack::SERVER_CONFIG,
            ],
            SLOW,
        )
        .unwrap();
    assert_eq!(backup.code, Some(1), "{}", backup.stderr);
    // Retention and the schedule's backup fail in their metrics.
    let compose = chaos.stack.compose.clone();
    until(&compose, "server", "a retention run failed", async || {
        let render = compose.run(&["run", "--rm", "probe"], SLOW).ok()?.stdout;
        let failed = labelled(
            &render,
            "afkfleet_retention_runs_total",
            "outcome=\"failed\"",
        ) > failed_runs;
        let backup = total(&render, "afkfleet_backup_failures_total") > failed_backups;
        (failed && backup).then_some(())
    })
    .await;
    // Nothing is corrupted, and the server never stopped.
    chaos.assert_sound(true);
    assert_eq!(chaos.stack.container("server"), server);

    chaos.free("data");
    chaos.free("backups");

    // With room again, everything goes on by itself: the write, the next
    // retention run, the bots' chat, and a backup; the same container,
    // never restarted.
    chaos.write("DiskRoomAgain").await;
    let succeeded = runs(&mut chaos, "success");
    until(
        &compose,
        "server",
        "a retention run succeeded",
        async || {
            let render = compose.run(&["run", "--rm", "probe"], SLOW).ok()?.stdout;
            (labelled(
                &render,
                "afkfleet_retention_runs_total",
                "outcome=\"success\"",
            ) > succeeded)
                .then_some(())
        },
    )
    .await;
    chaos.after_the_fault().await;
    assert_eq!(chaos.stack.container("server"), server);
    let state = chaos.stack.docker(&["inspect", &server]);
    let state = fleet_testkit::docker::ContainerState::from_inspect(&state.stdout).unwrap();
    assert_eq!(state.restart_count, 0, "the server was restarted");
    for name in BOTS {
        assert_eq!(chaos.joins(name), 1, "{name} left its server");
    }
    chaos.log_in_again().await;
    chaos.end().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos_a_server_whose_data_volume_is_gone_refuses_to_start_until_a_backup_is_restored() {
    let mut chaos = Chaos::start("afkfleet-chaos-volume", 130, false).await;
    chaos.write("BeforeTheLoss").await;
    chaos.after_the_fault().await;

    // The volume is lost: everything that mounts it stops, and it goes.
    chaos.stack.ok(&["stop", "agent", "server"], SLOW);
    chaos.stack.ok(&["rm", "--force", "server"], QUICK);
    let volume = format!("{}_data", chaos.stack.compose.project());
    let removed = chaos.stack.docker(&["volume", "rm", &volume]);
    assert!(removed.success, "{}", removed.stderr);

    // Compose makes a new, empty volume of that name. The server refuses to
    // start on it, says which file and where its volume belongs, and
    // creates nothing.
    let up = chaos
        .stack
        .compose
        .run(
            &["up", "--detach", "--wait", "--wait-timeout", "45", "server"],
            SLOW,
        )
        .unwrap();
    assert!(!up.success, "the server started without its database");
    let log = chaos
        .stack
        .ok(&["logs", "--no-color", "--no-log-prefix", "server"], QUICK);
    assert!(
        log.contains(
            "the database /data/afkfleet.db doesn't exist, and this command never creates one"
        ) && log.contains("isn't mounted at /data"),
        "{log}"
    );
    let data = chaos.stack.listing("data");
    assert!(!data.contains("afkfleet.db"), "{data}");

    // The runbook's way back is a restore, which Chaos::end runs.
    chaos.end().await;
}
