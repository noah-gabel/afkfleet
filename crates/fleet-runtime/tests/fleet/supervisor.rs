//! The supervisor and the `Fleet` handle (Plan.md P4.7; ADR-0013): bots come
//! and go through the handle, a crashed actor restarts from its last state
//! without skipping its backoff, crashes end in `CrashLoop`, and a shutdown
//! stops every bot within its timeout.

use core::num::{NonZeroU32, NonZeroUsize};

use fleet_core::bot::{BotNotification, BotState, FailReason, StickyState};
use fleet_core::id::BotId;
use fleet_core::mc::{ConnectError, SessionEvent};
use fleet_core::mode::{Action, Schedule, Step};
use fleet_runtime::{ChatError, Fleet, FleetError, FleetEventKind, SendChatError, ShutdownReport};
use fleet_testkit::mc::Performed;
use tracing::Level;

use crate::common::Levels;
use crate::harness::{
    BOT, CRASH_LOOP, DUPLICATE_LOGIN, Harness, OTHER, PAUSED, Setup, advance, awaiting, backoff,
    circuit, connecting, crash_loop, emit, id, kick, message, mode, ms, n, policy, running, secs,
    settle, stopped,
};
use crate::panicky::{PanicOn, PanickyConnector};

// --- Bots come and go ---

#[tokio::test(start_paused = true)]
async fn a_new_bot_runs_and_goes_online() {
    let mut fleet = Setup::new().start().await;

    let _controller = fleet.online(BOT, "AfkBot1", 0).await;

    let states = fleet.states_of(BOT);
    assert!(
        matches!(
            &states[..],
            [first, second, BotState::Online { .. }]
                if *first == awaiting(n(1)) && *second == connecting(n(1))
        ),
        "{states:?}"
    );
    assert_eq!(fleet.fake.connects().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_new_bot_that_should_be_stopped_publishes_nothing() {
    let mut fleet = Setup::new().start().await;

    fleet.apply(stopped(BOT, "AfkBot1")).await;
    advance(secs(3_600)).await;

    assert_eq!(fleet.state(BOT).await, BotState::Stopped);
    assert_eq!(fleet.kinds_of(BOT), []);
    assert_eq!(fleet.connector.attempts(), 0);
}

#[tokio::test(start_paused = true)]
async fn snapshot_all_is_sorted_by_bot_id() {
    let fleet = Setup::new().start().await;
    fleet.apply(stopped(OTHER, "AfkBot2")).await;
    fleet.apply(stopped(BOT, "AfkBot1")).await;

    let snapshots = fleet.fleet.snapshot_all().await.unwrap();

    let ids: Vec<BotId> = snapshots.iter().map(|snapshot| snapshot.bot_id).collect();
    assert_eq!(ids, [id(BOT), id(OTHER)]);
}

#[tokio::test(start_paused = true)]
async fn every_subscriber_gets_the_fleets_events() {
    let fleet = Setup::new().start().await;
    let mut subscriber = fleet.fleet.subscribe();

    fleet.apply(running(BOT, "AfkBot1")).await;

    let event = subscriber.try_recv().unwrap();
    assert_eq!(event.bot_id, id(BOT));
    assert!(matches!(event.kind, FleetEventKind::StateChanged(_)));
}

#[tokio::test(start_paused = true)]
async fn a_changed_spec_reaches_the_actor() {
    let fleet = Setup::new().start().await;
    let controller = fleet.online(BOT, "AfkBot1", 0).await;

    fleet.apply(stopped(BOT, "AfkBot1")).await;

    assert_eq!(fleet.state(BOT).await, BotState::Stopped);
    assert!(controller.is_torn_down());
}

#[tokio::test(start_paused = true)]
async fn a_changed_account_is_refused_even_when_only_its_case_changes() {
    let fleet = Setup::new().start().await;
    fleet.apply(stopped(BOT, "AfkBot1")).await;

    let case_only = fleet.fleet.apply(stopped(BOT, "afkbot1"), None).await;
    let other = fleet.fleet.apply(stopped(BOT, "AfkBot2"), None).await;

    assert_eq!(case_only, Err(FleetError::AccountChanged));
    assert_eq!(other, Err(FleetError::AccountChanged));
}

#[tokio::test(start_paused = true)]
async fn an_account_another_bot_uses_is_refused_whatever_its_case() {
    let fleet = Setup::new().start().await;
    fleet.apply(stopped(BOT, "AfkBot1")).await;

    let refused = fleet.fleet.apply(stopped(OTHER, "AFKBOT1"), None).await;

    assert_eq!(refused, Err(FleetError::AccountInUse));
    assert_eq!(fleet.fleet.snapshot_all().await.unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn an_account_held_only_by_a_removing_bot_is_busy_until_removed() {
    let mut fleet = Setup::new().start().await;
    let controller = fleet.online(BOT, "AfkBot1", 0).await;
    controller.delay_disconnect(secs(20));
    fleet.remove(BOT).await;

    assert_eq!(
        fleet.fleet.apply(running(OTHER, "afkBot1"), None).await,
        Err(FleetError::Busy)
    );
    advance(secs(20)).await;

    assert_eq!(fleet.kinds_of(BOT).last(), Some(&FleetEventKind::Removed));
    assert_eq!(
        fleet.fleet.apply(running(OTHER, "afkBot1"), None).await,
        Ok(())
    );
}

#[tokio::test(start_paused = true)]
async fn a_full_fleet_refuses_a_new_bot_but_updates_a_known_one() {
    let mut setup = Setup::new();
    setup.config.max_bots = NonZeroUsize::MIN;
    let fleet = setup.start().await;
    fleet.apply(stopped(BOT, "AfkBot1")).await;

    let new = fleet.fleet.apply(stopped(OTHER, "AfkBot2"), None).await;
    let known = fleet.fleet.apply(running(BOT, "AfkBot1"), None).await;

    assert_eq!(new, Err(FleetError::AtCapacity));
    assert_eq!(known, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn every_call_for_an_unknown_bot_answers_unknown_bot() {
    let fleet = Setup::new().start().await;
    let bot = id(BOT);

    assert_eq!(fleet.fleet.remove(bot).await, Err(FleetError::UnknownBot));
    assert_eq!(fleet.fleet.reset(bot).await, Err(FleetError::UnknownBot));
    assert_eq!(fleet.fleet.resume(bot).await, Err(FleetError::UnknownBot));
    assert_eq!(fleet.fleet.restart(bot).await, Err(FleetError::UnknownBot));
    assert_eq!(fleet.fleet.snapshot(bot).await, Err(FleetError::UnknownBot));
    assert_eq!(
        fleet.fleet.send_chat(bot, message("hi")).await,
        Err(SendChatError::Fleet(FleetError::UnknownBot))
    );
}

#[tokio::test(start_paused = true)]
async fn a_restart_does_nothing_while_the_bot_should_be_stopped() {
    let mut fleet = Setup::new().start().await;
    fleet.apply(stopped(BOT, "AfkBot1")).await;

    assert_eq!(fleet.fleet.restart(id(BOT)).await, Ok(()));
    settle().await;

    assert_eq!(fleet.kinds_of(BOT), []);
    assert_eq!(fleet.connector.attempts(), 0);
}

// --- Chat ---

#[tokio::test(start_paused = true)]
async fn user_chat_gets_a_ticket_once_the_bot_is_online() {
    let mut fleet = Setup::new().start().await;
    let controller = fleet.online(BOT, "AfkBot1", 0).await;

    let ticket = fleet.fleet.send_chat(id(BOT), message("hi")).await.unwrap();
    settle().await;

    assert!(
        fleet
            .kinds_of(BOT)
            .contains(&FleetEventKind::ChatSent { ticket })
    );
    assert_eq!(controller.log(), [Performed::Chat(message("hi"))]);
}

#[tokio::test(start_paused = true)]
async fn user_chat_is_refused_until_the_bot_is_online() {
    let fleet = Setup::new().start().await;
    fleet.apply(running(BOT, "AfkBot1")).await;
    assert_eq!(fleet.state(BOT).await, connecting(n(1)));

    let refused = fleet.fleet.send_chat(id(BOT), message("too early")).await;

    assert_eq!(refused, Err(SendChatError::Chat(ChatError::NotOnline)));
}

// --- Removal ---

#[tokio::test(start_paused = true)]
async fn removing_a_running_bot_stops_it_then_publishes_removed() {
    let mut fleet = Setup::new().start().await;
    let controller = fleet.online(BOT, "AfkBot1", 0).await;
    let _ = fleet.drain();

    fleet.remove(BOT).await;

    let kinds = fleet.kinds_of(BOT);
    assert!(
        matches!(
            &kinds[..],
            [
                FleetEventKind::StateChanged(stopping),
                FleetEventKind::StateChanged(stopped),
                FleetEventKind::Removed,
            ] if stopping.state == BotState::Stopping { restart: false }
                && stopped.state == BotState::Stopped
        ),
        "{kinds:?}"
    );
    assert!(controller.is_torn_down());
    assert_eq!(
        fleet.fleet.snapshot(id(BOT)).await,
        Err(FleetError::UnknownBot)
    );
    assert_eq!(fleet.fleet.snapshot_all().await, Ok(Vec::new()));
}

// P10.5 moves a bot by waiting for `Removed`, then hands the new agent the
// server's stored state: a `Stopped` here would overwrite a stored Paused.
#[tokio::test(start_paused = true)]
async fn removing_a_paused_bot_publishes_only_removed() {
    let mut fleet = Setup::new().start().await;
    let controller = fleet.online(BOT, "AfkBot1", 0).await;
    emit(&controller, kick("multiplayer.disconnect.duplicate_login"));
    settle().await;
    assert_eq!(fleet.state(BOT).await, PAUSED);
    let _ = fleet.drain();

    fleet.remove(BOT).await;

    assert_eq!(fleet.kinds_of(BOT), [FleetEventKind::Removed]);
}

#[tokio::test(start_paused = true)]
async fn while_a_bot_is_removed_only_reads_still_see_it() {
    let fleet = Setup::new().start().await;
    let controller = fleet.online(BOT, "AfkBot1", 0).await;
    controller.delay_disconnect(secs(20));
    let bot = id(BOT);

    fleet.remove(BOT).await;

    assert_eq!(
        fleet.fleet.apply(running(BOT, "AfkBot1"), None).await,
        Err(FleetError::Busy)
    );
    assert_eq!(fleet.fleet.reset(bot).await, Err(FleetError::UnknownBot));
    assert_eq!(fleet.fleet.resume(bot).await, Err(FleetError::UnknownBot));
    assert_eq!(fleet.fleet.restart(bot).await, Err(FleetError::UnknownBot));
    assert_eq!(
        fleet.fleet.send_chat(bot, message("hi")).await,
        Err(SendChatError::Fleet(FleetError::UnknownBot))
    );
    assert_eq!(fleet.fleet.remove(bot).await, Ok(()), "already accepted");
    assert_eq!(
        fleet.state(BOT).await,
        BotState::Stopping { restart: false }
    );
    assert_eq!(fleet.fleet.snapshot_all().await.unwrap().len(), 1);

    advance(secs(20)).await;

    assert_eq!(fleet.fleet.snapshot(bot).await, Err(FleetError::UnknownBot));
}

#[tokio::test(start_paused = true)]
async fn a_removing_bot_counts_toward_max_bots_until_removed() {
    let mut setup = Setup::new();
    setup.config.max_bots = NonZeroUsize::MIN;
    let fleet = setup.start().await;
    let controller = fleet.online(BOT, "AfkBot1", 0).await;
    controller.delay_disconnect(secs(20));
    fleet.remove(BOT).await;

    assert_eq!(
        fleet.fleet.apply(stopped(OTHER, "AfkBot2"), None).await,
        Err(FleetError::AtCapacity)
    );
    advance(secs(20)).await;

    assert_eq!(
        fleet.fleet.apply(stopped(OTHER, "AfkBot2"), None).await,
        Ok(())
    );
}

#[tokio::test(start_paused = true)]
async fn a_removal_whose_actor_crashes_still_publishes_removed() {
    let levels = Levels::default();
    let _guard = tracing::subscriber::set_default(levels.clone());
    let mut fleet = Setup::new().start().await;
    fleet.connector.sessions_panic_on(PanicOn::Disconnect);
    let _controller = fleet.online(BOT, "AfkBot1", 0).await;

    fleet.remove(BOT).await;
    advance(secs(3_600)).await;

    assert_eq!(fleet.kinds_of(BOT).last(), Some(&FleetEventKind::Removed));
    assert_eq!(fleet.connector.attempts(), 1, "not restarted");
    assert_eq!(levels.of("crashed while it was removed"), [Level::WARN]);
}

// --- Crashes and restarts ---

#[tokio::test(start_paused = true)]
async fn an_actor_panic_restarts_the_bot_from_its_backoff() {
    let mut fleet = Setup::new().start().await;
    fleet.connector.panic_on_connect(1);

    fleet.apply(running(BOT, "AfkBot1")).await;

    assert_eq!(fleet.connector.attempts(), 1);
    assert_eq!(
        fleet.states_of(BOT),
        [awaiting(n(1)), connecting(n(1)), backoff(n(1))]
    );
    let (lower, upper) = policy().bounds(n(1));
    advance(lower.checked_sub(ms(1)).unwrap()).await;
    assert_eq!(
        fleet.connector.attempts(),
        1,
        "no connect before the backoff"
    );
    advance(upper.checked_sub(lower).unwrap() + ms(1)).await;
    assert_eq!(fleet.connector.attempts(), 2);
    assert_eq!(fleet.state(BOT).await, connecting(n(2)));
}

#[tokio::test(start_paused = true)]
async fn a_restart_logs_a_warning_in_the_bots_span() {
    let levels = Levels::default();
    let _guard = tracing::subscriber::set_default(levels.clone());
    let fleet = Setup::new().start().await;
    fleet.connector.panic_on_connect(1);

    fleet.apply(running(BOT, "AfkBot1")).await;

    assert_eq!(levels.of("it restarts from its last state"), [Level::WARN]);
    let span = format!("bot bot_id={BOT}");
    let spans = levels.spans_of("it restarts from its last state");
    assert!(
        spans.iter().all(|logged| logged.contains(&span)),
        "{spans:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_crashed_task_restarts_the_bot_like_a_panic() {
    let levels = Levels::default();
    let _guard = tracing::subscriber::set_default(levels.clone());
    let fleet = Setup::new().start().await;
    fleet.connector.sessions_panic_on(PanicOn::Jump);
    let mut spec = running(BOT, "AfkBot1");
    spec.mode = mode(vec![Step {
        action: Action::Jump,
        schedule: Schedule::AtStart,
        probability: 100,
    }]);
    fleet.apply(spec).await;
    let controller = fleet.session(0).await;

    emit(&controller, SessionEvent::Joined);
    settle().await;

    assert_eq!(fleet.state(BOT).await, backoff(n(1)));
    assert!(controller.is_torn_down());
    assert_eq!(levels.of("it restarts from its last state"), [Level::WARN]);
    assert_eq!(fleet.connector.attempts(), 1);
}

#[tokio::test(start_paused = true)]
async fn the_breaker_keeps_its_failures_across_a_restart() {
    let mut setup = Setup::new();
    setup.circuit = circuit(2);
    setup
        .fake
        .push_connect_result(Err(ConnectError::HostUnavailable));
    setup
        .fake
        .push_connect_result(Err(ConnectError::HostUnavailable));
    let fleet = setup.start().await;
    fleet.connector.panic_on_connect(2);

    fleet.apply(running(BOT, "AfkBot1")).await;
    advance(policy().bounds(n(1)).1).await;
    assert_eq!(fleet.connector.attempts(), 2, "the second connect panicked");
    advance(policy().bounds(n(2)).1).await;
    assert_eq!(fleet.connector.attempts(), 3);

    // The third connect is the breaker's second failure, so it opened for
    // 900 s. A breaker the restart had reset would retry within 40 s.
    advance(secs(800)).await;
    assert_eq!(fleet.connector.attempts(), 3);
    advance(secs(200)).await;
    assert_eq!(fleet.connector.attempts(), 4);
}

#[tokio::test(start_paused = true)]
async fn a_restart_does_not_refill_the_chat_bucket() {
    let mut setup = Setup::new();
    setup.config.chat_interval = secs(3_600);
    setup.config.chat_burst = NonZeroU32::MIN;
    let fleet = setup.start().await;
    let first = fleet.online(BOT, "AfkBot1", 0).await;
    fleet.fleet.send_chat(id(BOT), message("hi")).await.unwrap();
    fleet.connector.panic_on_connect(2);

    emit(&first, kick("multiplayer.disconnect.server_shutdown"));
    settle().await;
    advance(policy().bounds(n(1)).1).await;
    assert_eq!(fleet.connector.attempts(), 2, "the second connect panicked");
    advance(policy().bounds(n(2)).1).await;
    let second = fleet.session(1).await;
    emit(&second, SessionEvent::Joined);
    settle().await;
    assert!(matches!(fleet.state(BOT).await, BotState::Online { .. }));

    assert_eq!(
        fleet.fleet.send_chat(id(BOT), message("again")).await,
        Err(SendChatError::Chat(ChatError::RateLimited))
    );
}

#[tokio::test(start_paused = true)]
async fn tickets_stay_unique_across_a_restart() {
    let fleet = Setup::new().start().await;
    let first = fleet.online(BOT, "AfkBot1", 0).await;
    let before = fleet.fleet.send_chat(id(BOT), message("hi")).await.unwrap();
    fleet.connector.panic_on_connect(2);

    emit(&first, kick("multiplayer.disconnect.server_shutdown"));
    settle().await;
    advance(policy().bounds(n(1)).1).await;
    advance(policy().bounds(n(2)).1).await;
    let second = fleet.session(1).await;
    emit(&second, SessionEvent::Joined);
    settle().await;
    let after = fleet
        .fleet
        .send_chat(id(BOT), message("again"))
        .await
        .unwrap();

    assert!(after > before, "{after:?} after {before:?}");
}

// --- Crash loops ---

#[tokio::test(start_paused = true)]
async fn the_sixth_crash_within_ten_minutes_fails_the_bot_with_crash_loop() {
    let levels = Levels::default();
    let _guard = tracing::subscriber::set_default(levels.clone());
    let mut fleet = Setup::new().start().await;

    crash_loop(&mut fleet).await;
    advance(secs(3_600)).await;

    assert_eq!(fleet.connector.attempts(), 6, "no connect after the loop");
    assert_eq!(
        fleet.kinds_of(BOT).last(),
        Some(&FleetEventKind::Alert(BotNotification::Failed {
            reason: FailReason::CrashLoop
        }))
    );
    assert_eq!(
        levels.of("it restarts from its last state"),
        [Level::WARN; 5]
    );
    assert_eq!(levels.of("fails with CrashLoop"), [Level::ERROR]);
}

#[tokio::test(start_paused = true)]
async fn a_crash_looped_bot_acts_like_a_failed_bot() {
    let mut fleet = Setup::new().start().await;
    crash_loop(&mut fleet).await;
    let bot = id(BOT);

    assert_eq!(fleet.fleet.resume(bot).await, Ok(()));
    assert_eq!(fleet.fleet.restart(bot).await, Ok(()));
    settle().await;

    assert_eq!(
        fleet.fleet.send_chat(bot, message("hi")).await,
        Err(SendChatError::Chat(ChatError::NotOnline))
    );
    assert_eq!(fleet.state(BOT).await, CRASH_LOOP);
    assert_eq!(fleet.connector.attempts(), 6);
}

#[tokio::test(start_paused = true)]
async fn a_reset_after_a_crash_loop_clears_the_restart_window() {
    let mut fleet = Setup::new().start().await;
    crash_loop(&mut fleet).await;
    fleet.connector.panic_always(false);
    fleet.connector.panic_on_connect(7);

    fleet.fleet.reset(id(BOT)).await.unwrap();
    settle().await;

    assert_eq!(fleet.connector.attempts(), 7);
    assert_eq!(
        fleet.state(BOT).await,
        backoff(n(1)),
        "a single crash restarts normally"
    );
    advance(policy().bounds(n(1)).1).await;
    assert_eq!(fleet.connector.attempts(), 8);
    assert_eq!(fleet.state(BOT).await, connecting(n(2)));
}

// --- Sticky states ---

#[tokio::test(start_paused = true)]
async fn a_bot_restored_as_paused_never_connects() {
    let mut fleet = Setup::new().start().await;

    fleet
        .fleet
        .apply(
            running(BOT, "AfkBot1"),
            Some(StickyState::Paused(DUPLICATE_LOGIN)),
        )
        .await
        .unwrap();
    advance(secs(3_600)).await;

    let kinds = fleet.kinds_of(BOT);
    assert!(
        matches!(
            &kinds[..],
            [FleetEventKind::StateChanged(snapshot)] if snapshot.state == PAUSED
        ),
        "{kinds:?}"
    );
    assert_eq!(fleet.connector.attempts(), 0);
    assert_eq!(fleet.credentials.requests().len(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_restore_for_a_known_bot_is_ignored() {
    let fleet = Setup::new().start().await;
    let controller = fleet.online(BOT, "AfkBot1", 0).await;

    fleet
        .fleet
        .apply(
            running(BOT, "AfkBot1"),
            Some(StickyState::Paused(DUPLICATE_LOGIN)),
        )
        .await
        .unwrap();
    settle().await;

    assert!(matches!(fleet.state(BOT).await, BotState::Online { .. }));
    assert!(!controller.is_torn_down());
}

// --- Shutdown ---

#[tokio::test(start_paused = true)]
async fn a_shutdown_stops_every_bot_and_reports_it() {
    let levels = Levels::default();
    let _guard = tracing::subscriber::set_default(levels.clone());
    let mut fleet = Setup::new().start().await;
    let first = fleet.online(BOT, "AfkBot1", 0).await;
    let second = fleet.online(OTHER, "AfkBot2", 1).await;
    let _ = fleet.drain();

    let report = fleet.fleet.shutdown(secs(10)).await.unwrap();

    assert_eq!(
        report,
        ShutdownReport {
            stopped: 2,
            aborted: 0,
            crashed: 0
        }
    );
    assert!(first.is_torn_down());
    assert!(second.is_torn_down());
    // The actors stop side by side, so the order of their events isn't fixed.
    let mut stopped: Vec<_> = fleet
        .drain()
        .into_iter()
        .filter_map(|event| match event.kind {
            FleetEventKind::StateChanged(snapshot) if snapshot.state == BotState::Stopped => {
                Some(event.bot_id)
            }
            _ => None,
        })
        .collect();
    stopped.sort();
    assert_eq!(stopped, [id(BOT), id(OTHER)]);
    assert!(!levels.any_warning());
    fleet.ended().await;
}

#[tokio::test(start_paused = true)]
async fn after_a_shutdown_every_call_answers_shutting_down() {
    let fleet = Setup::new().start().await;
    fleet.fleet.shutdown(secs(10)).await.unwrap();

    assert_eq!(
        fleet.fleet.shutdown(secs(10)).await,
        Err(FleetError::ShuttingDown)
    );
    assert_eq!(
        fleet.fleet.apply(stopped(BOT, "AfkBot1"), None).await,
        Err(FleetError::ShuttingDown)
    );
    assert_eq!(
        fleet.fleet.snapshot_all().await,
        Err(FleetError::ShuttingDown)
    );
    assert_eq!(
        fleet.fleet.send_chat(id(BOT), message("hi")).await,
        Err(SendChatError::Fleet(FleetError::ShuttingDown))
    );
}

#[tokio::test(start_paused = true)]
async fn while_the_fleet_shuts_down_every_call_answers_shutting_down() {
    let fleet = Setup::new().start().await;
    let controller = fleet.online(BOT, "AfkBot1", 0).await;
    controller.delay_disconnect(secs(20));
    let shutdown = tokio::spawn({
        let handle = fleet.fleet.clone();
        async move { handle.shutdown(secs(30)).await }
    });
    settle().await;
    let bot = id(BOT);
    let refused = Err(FleetError::ShuttingDown);

    assert_eq!(
        fleet.fleet.apply(running(BOT, "AfkBot1"), None).await,
        refused
    );
    assert_eq!(fleet.fleet.remove(bot).await, refused);
    assert_eq!(fleet.fleet.reset(bot).await, refused);
    assert_eq!(fleet.fleet.snapshot(bot).await.map(|_| ()), refused);
    assert_eq!(fleet.fleet.snapshot_all().await.map(|_| ()), refused);
    assert_eq!(fleet.fleet.shutdown(secs(1)).await.map(|_| ()), refused);
    assert_eq!(
        fleet.fleet.send_chat(bot, message("hi")).await,
        Err(SendChatError::Fleet(FleetError::ShuttingDown))
    );

    advance(secs(20)).await;
    assert_eq!(
        shutdown.await.unwrap(),
        Ok(ShutdownReport {
            stopped: 1,
            aborted: 0,
            crashed: 0
        })
    );
}

#[tokio::test(start_paused = true)]
async fn a_teardown_slower_than_the_timeout_is_aborted() {
    let levels = Levels::default();
    let _guard = tracing::subscriber::set_default(levels.clone());
    let fleet = Setup::new().start().await;
    let controller = fleet.online(BOT, "AfkBot1", 0).await;
    controller.delay_disconnect(secs(60));
    let started = tokio::time::Instant::now();

    let report = fleet.fleet.shutdown(secs(10)).await.unwrap();

    assert_eq!(
        report,
        ShutdownReport {
            stopped: 0,
            aborted: 1,
            crashed: 0
        }
    );
    assert_eq!(started.elapsed(), secs(10));
    assert_eq!(
        levels.of("the remaining actors were aborted"),
        [Level::WARN]
    );
}

#[tokio::test(start_paused = true)]
async fn a_teardown_that_panics_during_a_shutdown_counts_as_crashed() {
    let levels = Levels::default();
    let _guard = tracing::subscriber::set_default(levels.clone());
    let fleet = Setup::new().start().await;
    fleet.connector.sessions_panic_on(PanicOn::Disconnect);
    let _controller = fleet.online(BOT, "AfkBot1", 0).await;

    let report = fleet.fleet.shutdown(secs(10)).await.unwrap();

    assert_eq!(
        report,
        ShutdownReport {
            stopped: 0,
            aborted: 0,
            crashed: 1
        }
    );
    assert_eq!(
        levels.of("crashed while the fleet shut down"),
        [Level::WARN]
    );
}

#[tokio::test(start_paused = true)]
async fn a_removal_in_progress_at_a_shutdown_still_publishes_removed() {
    let mut fleet = Setup::new().start().await;
    let controller = fleet.online(BOT, "AfkBot1", 0).await;
    controller.delay_disconnect(secs(5));
    fleet.remove(BOT).await;

    let report = fleet.fleet.shutdown(secs(10)).await.unwrap();

    assert_eq!(report.stopped, 1);
    assert_eq!(fleet.kinds_of(BOT).last(), Some(&FleetEventKind::Removed));
}

#[tokio::test(start_paused = true)]
async fn cancelling_the_supervisor_shuts_the_fleet_down() {
    let fleet = Setup::new().start().await;
    let controller = fleet.online(BOT, "AfkBot1", 0).await;

    fleet.cancel.cancel();
    settle().await;

    assert!(controller.is_torn_down());
    assert_eq!(
        fleet.fleet.snapshot_all().await,
        Err(FleetError::ShuttingDown)
    );
    fleet.ended().await;
}

#[tokio::test(start_paused = true)]
async fn dropping_every_fleet_handle_shuts_the_fleet_down() {
    let fleet = Setup::new().start().await;
    let controller = fleet.online(BOT, "AfkBot1", 0).await;
    let Harness {
        fleet: handle,
        task,
        ..
    } = fleet;

    drop(handle);
    settle().await;

    assert!(controller.is_torn_down());
    tokio::time::timeout(secs(60), task)
        .await
        .expect("the supervisor should have ended")
        .unwrap();
}

// --- The supervisor's queue ---

#[tokio::test(start_paused = true)]
async fn a_call_times_out_when_the_supervisor_does_not_answer() {
    let setup = Setup::new();
    let connector = PanickyConnector::new(setup.fake.clone());
    let (fleet, supervisor) = Fleet::new(setup.parts(connector)).unwrap();
    let started = tokio::time::Instant::now();

    let answer = fleet.snapshot_all().await;

    assert_eq!(answer, Err(FleetError::TimedOut));
    assert_eq!(started.elapsed(), secs(5));
    drop(supervisor);
}

#[tokio::test(start_paused = true)]
async fn a_full_supervisor_queue_answers_busy() {
    let mut setup = Setup::new();
    setup.config.supervisor_queue = NonZeroUsize::MIN;
    let connector = PanickyConnector::new(setup.fake.clone());
    let (fleet, supervisor) = Fleet::new(setup.parts(connector)).unwrap();
    let waiting = tokio::spawn({
        let fleet = fleet.clone();
        async move { fleet.snapshot_all().await }
    });
    settle().await;

    assert_eq!(fleet.snapshot_all().await, Err(FleetError::Busy));

    drop(supervisor);
    assert_eq!(waiting.await.unwrap(), Err(FleetError::ShuttingDown));
}

#[tokio::test(start_paused = true)]
async fn a_call_to_a_supervisor_that_is_gone_answers_shutting_down() {
    let setup = Setup::new();
    let connector = PanickyConnector::new(setup.fake.clone());
    let (fleet, supervisor) = Fleet::new(setup.parts(connector)).unwrap();
    drop(supervisor);

    assert_eq!(fleet.remove(id(BOT)).await, Err(FleetError::ShuttingDown));
}
