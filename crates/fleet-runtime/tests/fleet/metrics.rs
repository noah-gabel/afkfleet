//! The runtime's metrics (Plan.md P4.9; ADR-0013), read from a recorder
//! each test installs on its own thread: the bots per state, reconnects,
//! watchdog trips and actor restarts.
//!
//! Three cases the `Fleet` can't reach are unit tests next to the code: a
//! crash of the crash-loop actor and the supervisor's own `Failed(CrashLoop)`
//! (supervisor), and a trip the drained session events overtake (actor).

use std::collections::BTreeSet;

use fleet_core::bot::BotState;
use fleet_core::mc::{ConnectError, SessionEvent};
use fleet_core::mode::{Action, Schedule, Step};
use fleet_testkit::mc::SessionController;
use rstest::rstest;

use crate::harness::{
    BOT, Harness, OTHER, Setup, advance, backoff, connecting, crash_loop, emit, id, kick, mode, n,
    policy, running, secs, settle, step_seconds, stopped,
};
use crate::panicky::PanicOn;
use crate::recorder::{ACTOR_RESTARTS, BOTS, RECONNECTS, Recorder, WATCHDOG_TRIPS, bots, series};

const TRANSIENT: &str = "multiplayer.disconnect.server_shutdown";

/// A mode that jumps when the bot joins, so a session that panics on a
/// jump crashes the mode runner.
fn jumping(bot: &str, name: &str) -> fleet_core::bot::BotSpec {
    let mut spec = running(bot, name);
    spec.mode = mode(vec![Step {
        action: Action::Jump,
        schedule: Schedule::AtStart,
        probability: 100,
    }]);
    spec
}

/// Waits out the longest first backoff, so the bot connects again.
async fn back_off_once() {
    advance(policy().bounds(n(1)).1).await;
}

// --- Registration ---

#[tokio::test(start_paused = true)]
async fn a_new_fleet_describes_its_metrics_and_starts_every_series_at_zero() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();

    let _fleet = Setup::new().start().await;

    let states = [
        "awaiting_session",
        "backoff",
        "connecting",
        "failed",
        "online",
        "paused",
        "stopped",
        "stopping",
    ];
    let mut expected: BTreeSet<String> = states
        .into_iter()
        .map(|state| series(BOTS, [("state", state)].into_iter()))
        .collect();
    expected.insert(series(RECONNECTS, core::iter::empty()));
    expected.insert(series(WATCHDOG_TRIPS, [("kind", "tick")].into_iter()));
    expected.insert(series(WATCHDOG_TRIPS, [("kind", "packet")].into_iter()));
    expected.insert(series(ACTOR_RESTARTS, core::iter::empty()));
    assert_eq!(recorder.series(), expected);
    assert_eq!(recorder.bots(), bots([]));
    for name in [BOTS, RECONNECTS, WATCHDOG_TRIPS, ACTOR_RESTARTS] {
        assert!(recorder.is_described(name), "{name}");
    }
}

// --- Bots per state ---

#[tokio::test(start_paused = true)]
async fn a_new_bot_counts_as_stopped_and_moves_with_its_state() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let fleet = Setup::new().start().await;

    fleet.apply(stopped(BOT, "AfkBot1")).await;
    assert_eq!(recorder.bots(), bots([("stopped", 1.0)]));

    let _controller = fleet.online(BOT, "AfkBot1", 0).await;
    fleet.apply(stopped(OTHER, "AfkBot2")).await;

    assert_eq!(recorder.bots(), bots([("online", 1.0), ("stopped", 1.0)]));
}

#[tokio::test(start_paused = true)]
async fn a_change_within_one_state_leaves_the_gauge_alone() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let fleet = Setup::new().start().await;
    let controller = fleet.online(BOT, "AfkBot1", 0).await;
    controller.delay_disconnect(secs(20));
    fleet.apply(stopped(BOT, "AfkBot1")).await;
    assert_eq!(
        fleet.state(BOT).await,
        BotState::Stopping { restart: false }
    );

    fleet.apply(running(BOT, "AfkBot1")).await;

    assert_eq!(fleet.state(BOT).await, BotState::Stopping { restart: true });
    assert_eq!(recorder.bots(), bots([("stopping", 1.0)]));
}

#[tokio::test(start_paused = true)]
async fn a_removed_bot_leaves_the_gauge_whatever_its_state() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let fleet = Setup::new().start().await;
    let _first = fleet.online(BOT, "AfkBot1", 0).await;
    let second = fleet.online(OTHER, "AfkBot2", 1).await;
    emit(&second, kick("multiplayer.disconnect.duplicate_login"));
    settle().await;
    assert_eq!(recorder.bots(), bots([("online", 1.0), ("paused", 1.0)]));

    fleet.remove(BOT).await;
    fleet.remove(OTHER).await;

    assert_eq!(recorder.bots(), bots([]));
}

#[tokio::test(start_paused = true)]
async fn after_a_shutdown_every_state_is_zero_once_the_supervisor_has_ended() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let fleet = Setup::new().start().await;
    let _controller = fleet.online(BOT, "AfkBot1", 0).await;
    fleet.apply(stopped(OTHER, "AfkBot2")).await;

    fleet.fleet.shutdown(secs(10)).await.unwrap();
    fleet.ended().await;

    assert_eq!(recorder.bots(), bots([]));
}

#[tokio::test(start_paused = true)]
async fn a_restarted_actor_still_counts_the_bot_once() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let fleet = Setup::new().start().await;
    fleet.connector.panic_on_connect(1);

    fleet.apply(running(BOT, "AfkBot1")).await;

    assert_eq!(fleet.state(BOT).await, backoff(n(1)));
    assert_eq!(recorder.bots(), bots([("backoff", 1.0)]));
}

// --- Reconnects ---

#[tokio::test(start_paused = true)]
async fn the_first_connect_is_no_reconnect() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let fleet = Setup::new().start().await;

    let _controller = fleet.online(BOT, "AfkBot1", 0).await;

    assert_eq!(recorder.reconnects(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_retry_after_a_kick_is_a_reconnect() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let fleet = Setup::new().start().await;
    let controller = fleet.online(BOT, "AfkBot1", 0).await;

    emit(&controller, kick(TRANSIENT));
    settle().await;
    back_off_once().await;

    assert_eq!(fleet.state(BOT).await, connecting(n(2)));
    assert_eq!(recorder.reconnects(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_retry_after_a_failed_connect_is_a_reconnect() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let setup = Setup::new();
    setup
        .fake
        .push_connect_result(Err(ConnectError::HostUnavailable));
    let fleet = setup.start().await;

    fleet.apply(running(BOT, "AfkBot1")).await;
    assert_eq!(recorder.reconnects(), 0, "the failed first connect");
    back_off_once().await;

    assert_eq!(fleet.fake.connects().len(), 2);
    assert_eq!(recorder.reconnects(), 1);
}

#[tokio::test(start_paused = true)]
async fn the_fresh_token_retry_is_a_reconnect() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let fleet = Setup::new().start().await;
    fleet.apply(running(BOT, "AfkBot1")).await;
    let controller = fleet.session(0).await;

    emit(
        &controller,
        kick("multiplayer.disconnect.unverified_username"),
    );
    settle().await;

    assert_eq!(
        fleet.state(BOT).await,
        BotState::Connecting {
            attempt: n(1),
            auth_retried: true
        }
    );
    assert_eq!(recorder.reconnects(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_retry_after_a_crash_restart_is_a_reconnect() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let fleet = Setup::new().start().await;
    fleet.connector.panic_on_connect(1);
    fleet.apply(running(BOT, "AfkBot1")).await;

    back_off_once().await;

    assert_eq!(fleet.connector.attempts(), 2);
    assert_eq!(recorder.reconnects(), 1);
}

/// A deliberate act that starts a new run.
#[derive(Debug, Clone, Copy)]
enum Deliberate {
    Restart,
    ServerChange,
    Reset,
    Resume,
}

#[rstest]
#[case::restart(Deliberate::Restart)]
#[case::server_change(Deliberate::ServerChange)]
#[case::reset(Deliberate::Reset)]
#[case::resume(Deliberate::Resume)]
#[tokio::test(start_paused = true)]
async fn a_deliberate_new_run_is_no_reconnect(#[case] deliberate: Deliberate) {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let fleet = Setup::new().start().await;
    let controller = fleet.online(BOT, "AfkBot1", 0).await;

    act(&fleet, &controller, deliberate).await;

    assert_eq!(fleet.fake.connects().len(), 2);
    assert_eq!(recorder.reconnects(), 0);
}

async fn act(fleet: &Harness, controller: &SessionController, deliberate: Deliberate) {
    match deliberate {
        Deliberate::Restart => fleet.fleet.restart(id(BOT)).await.unwrap(),
        Deliberate::ServerChange => {
            let mut moved = running(BOT, "AfkBot1");
            moved.server = "example.com".try_into().unwrap();
            fleet.fleet.apply(moved, None).await.unwrap();
        }
        Deliberate::Reset => {
            emit(controller, kick("multiplayer.disconnect.banned"));
            settle().await;
            fleet.fleet.reset(id(BOT)).await.unwrap();
        }
        Deliberate::Resume => {
            emit(controller, kick("multiplayer.disconnect.duplicate_login"));
            settle().await;
            fleet.fleet.resume(id(BOT)).await.unwrap();
        }
    }
    settle().await;
}

// --- Watchdog trips ---

#[rstest]
#[case::tick("tick", "packet")]
#[case::packet("packet", "tick")]
#[tokio::test(start_paused = true)]
async fn a_watchdog_trip_counts_under_its_kind(#[case] kind: &str, #[case] other: &str) {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let fleet = Setup::new().start().await;
    let controller = fleet.online(BOT, "AfkBot1", 0).await;

    if kind == "tick" {
        controller.freeze_ticks();
    } else {
        controller.freeze_packets();
    }
    step_seconds(31).await;

    assert_eq!(fleet.state(BOT).await, backoff(n(1)));
    assert_eq!(recorder.trips(kind), 1);
    assert_eq!(recorder.trips(other), 0);
}

#[tokio::test(start_paused = true)]
async fn two_trips_in_a_row_count_two() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let fleet = Setup::new().start().await;
    let first = fleet.online(BOT, "AfkBot1", 0).await;
    first.freeze_ticks();
    step_seconds(31).await;
    back_off_once().await;
    let second = fleet.session(1).await;
    emit(&second, SessionEvent::Joined);
    settle().await;

    second.freeze_ticks();
    step_seconds(31).await;

    assert_eq!(recorder.trips("tick"), 2);
}

#[tokio::test(start_paused = true)]
async fn a_restored_backoff_after_a_crash_is_no_trip() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let fleet = Setup::new().start().await;
    let controller = fleet.online(BOT, "AfkBot1", 0).await;
    controller.freeze_ticks();
    step_seconds(31).await;
    assert_eq!(recorder.trips("tick"), 1);
    fleet.connector.panic_on_connect(2);

    back_off_once().await;

    assert_eq!(recorder.restarts(), 1, "the second connect panicked");
    assert_eq!(fleet.state(BOT).await, backoff(n(2)));
    assert_eq!(recorder.trips("tick"), 1);
}

// --- Actor restarts ---

#[tokio::test(start_paused = true)]
async fn a_panic_counts_one_restart() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let fleet = Setup::new().start().await;
    fleet.connector.panic_on_connect(1);

    fleet.apply(running(BOT, "AfkBot1")).await;

    assert_eq!(recorder.restarts(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_crashed_task_counts_one_restart() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let fleet = Setup::new().start().await;
    fleet.connector.sessions_panic_on(PanicOn::Jump);
    fleet.apply(jumping(BOT, "AfkBot1")).await;
    let controller = fleet.session(0).await;

    emit(&controller, SessionEvent::Joined);
    settle().await;

    assert_eq!(fleet.state(BOT).await, backoff(n(1)));
    assert_eq!(recorder.restarts(), 1);
}

#[tokio::test(start_paused = true)]
async fn an_actor_whose_runner_and_teardown_both_panic_counts_one_restart() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let fleet = Setup::new().start().await;
    fleet.connector.sessions_panic_on(PanicOn::Jump);
    fleet.connector.sessions_panic_on(PanicOn::Disconnect);
    fleet.apply(jumping(BOT, "AfkBot1")).await;
    let controller = fleet.session(0).await;

    emit(&controller, SessionEvent::Joined);
    settle().await;

    assert!(controller.is_torn_down());
    assert_eq!(recorder.restarts(), 1);
}

#[tokio::test(start_paused = true)]
async fn the_crash_loop_start_counts_as_a_restart() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let mut fleet = Setup::new().start().await;

    crash_loop(&mut fleet).await;

    assert_eq!(
        recorder.restarts(),
        6,
        "5 restarts and the crash-loop start"
    );
}

#[tokio::test(start_paused = true)]
async fn a_new_bot_and_a_reset_after_a_crash_loop_are_no_restarts() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let mut fleet = Setup::new().start().await;
    fleet.apply(stopped(OTHER, "AfkBot2")).await;
    assert_eq!(recorder.restarts(), 0, "a new bot");
    crash_loop(&mut fleet).await;
    fleet.connector.panic_always(false);

    fleet.fleet.reset(id(BOT)).await.unwrap();
    settle().await;

    assert_eq!(recorder.restarts(), 6);
}

#[tokio::test(start_paused = true)]
async fn a_crash_during_removal_is_no_restart() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let fleet = Setup::new().start().await;
    fleet.connector.sessions_panic_on(PanicOn::Disconnect);
    let _controller = fleet.online(BOT, "AfkBot1", 0).await;

    fleet.remove(BOT).await;

    assert_eq!(fleet.connector.attempts(), 1);
    assert_eq!(recorder.restarts(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_crash_during_a_shutdown_is_no_restart() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let fleet = Setup::new().start().await;
    fleet.connector.sessions_panic_on(PanicOn::Disconnect);
    let _controller = fleet.online(BOT, "AfkBot1", 0).await;

    let report = fleet.fleet.shutdown(secs(10)).await.unwrap();

    assert_eq!(report.crashed, 1);
    assert_eq!(recorder.restarts(), 0);
}
