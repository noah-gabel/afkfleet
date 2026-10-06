//! Tests for one fake session: its events, its handle, its liveness stamps,
//! and the port contract it keeps (Plan.md P3.1; ADR-0010, ADR-0011).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;

use fleet_core::chat::{ChatMessage, IncomingChat};
use fleet_core::disconnect::{ConnectFailure, DisconnectReason};
use fleet_core::mc::{
    ConnectParams, MinecraftConnector, SessionCredentials, SessionError, SessionEvent,
    SessionEvents, SessionHandle,
};
use fleet_core::mode::GameAction;
use fleet_testkit::mc::{
    EmitOutcome, FakeConnector, FakeEvents, FakeSession, Performed, SessionController,
};

fn params() -> ConnectParams {
    ConnectParams {
        bot_id: "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap(),
        server: "localhost".try_into().unwrap(),
        credentials: SessionCredentials::Offline {
            username: "AfkBot1".try_into().unwrap(),
        },
        connect_timeout: Duration::from_secs(30),
    }
}

/// Starts one fake session on a connector with the given event capacity.
async fn start_with_capacity(capacity: usize) -> (FakeSession, FakeEvents, SessionController) {
    let connector = FakeConnector::with_event_capacity(capacity);
    let (session, events) = connector.connect(params()).await.unwrap();
    (session, events, connector.session(0).await)
}

async fn start() -> (FakeSession, FakeEvents, SessionController) {
    start_with_capacity(FakeConnector::DEFAULT_EVENT_CAPACITY).await
}

/// Starts a session whose bot has joined, with `Joined` already consumed.
async fn start_joined() -> (FakeSession, FakeEvents, SessionController) {
    let (session, mut events, controller) = start().await;
    assert_eq!(controller.emit(SessionEvent::Joined), EmitOutcome::Queued);
    assert_eq!(events.next().await, Some(SessionEvent::Joined));
    (session, events, controller)
}

fn chat(text: &str) -> SessionEvent {
    SessionEvent::Chat(IncomingChat::system(text))
}

fn message(text: &str) -> ChatMessage {
    text.try_into().unwrap()
}

// --- Events ---

#[tokio::test(start_paused = true)]
async fn emitted_events_arrive_in_order() {
    let (_session, mut events, controller) = start().await;

    assert_eq!(controller.emit(SessionEvent::Joined), EmitOutcome::Queued);
    assert_eq!(controller.emit(chat("hello")), EmitOutcome::Queued);
    assert_eq!(controller.emit(SessionEvent::Died), EmitOutcome::Queued);

    assert_eq!(events.next().await, Some(SessionEvent::Joined));
    assert_eq!(events.next().await, Some(chat("hello")));
    assert_eq!(events.next().await, Some(SessionEvent::Died));
}

#[tokio::test(start_paused = true)]
async fn next_waits_for_an_event_emitted_later() {
    let (_session, mut events, controller) = start().await;
    let test = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(controller.emit(SessionEvent::Joined), EmitOutcome::Queued);
    });

    let event = tokio::time::timeout(Duration::from_secs(5), events.next()).await;

    assert_eq!(event, Ok(Some(SessionEvent::Joined)));
    test.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn next_is_cancel_safe() {
    let (_session, mut events, controller) = start().await;

    // `next` is polled first and waits; the second branch then emits and
    // wins, so the woken `next` is dropped without being polled again.
    tokio::select! {
        biased;
        event = events.next() => panic!("nothing was emitted yet, got {event:?}"),
        () = async { assert_eq!(controller.emit(SessionEvent::Joined), EmitOutcome::Queued); } => {}
    }

    assert_eq!(events.next().await, Some(SessionEvent::Joined));
}

#[tokio::test(start_paused = true)]
async fn disconnected_is_the_last_event() {
    let (_session, mut events, controller) = start().await;
    assert_eq!(controller.emit(SessionEvent::Joined), EmitOutcome::Queued);
    assert_eq!(
        controller.emit(SessionEvent::Disconnected(
            DisconnectReason::ConnectionClosed,
        )),
        EmitOutcome::Queued
    );

    let after = controller.emit(chat("too late"));

    assert_eq!(after, EmitOutcome::SessionEnded);
    assert_eq!(events.next().await, Some(SessionEvent::Joined));
    assert_eq!(
        events.next().await,
        Some(SessionEvent::Disconnected(
            DisconnectReason::ConnectionClosed
        ))
    );
    assert_eq!(events.next().await, None);
}

#[tokio::test(start_paused = true)]
async fn connection_failed_is_the_last_event() {
    let (_session, mut events, controller) = start().await;
    assert_eq!(
        controller.emit(SessionEvent::ConnectionFailed(ConnectFailure::Refused)),
        EmitOutcome::Queued
    );

    let after = controller.emit(SessionEvent::Joined);

    assert_eq!(after, EmitOutcome::SessionEnded);
    assert_eq!(
        events.next().await,
        Some(SessionEvent::ConnectionFailed(ConnectFailure::Refused))
    );
    assert_eq!(events.next().await, None);
}

#[tokio::test(start_paused = true)]
async fn a_second_death_before_respawn_is_dropped() {
    let (_session, mut events, controller) = start_joined().await;

    let first = controller.emit(SessionEvent::Died);
    let second = controller.emit(SessionEvent::Died);
    assert_eq!(controller.emit(chat("after")), EmitOutcome::Queued);

    assert_eq!(first, EmitOutcome::Queued);
    assert_eq!(second, EmitOutcome::DuplicateDeath);
    assert_eq!(events.next().await, Some(SessionEvent::Died));
    assert_eq!(events.next().await, Some(chat("after")));
}

#[tokio::test(start_paused = true)]
async fn a_death_after_a_respawn_is_delivered() {
    let (session, mut events, controller) = start_joined().await;
    assert_eq!(controller.emit(SessionEvent::Died), EmitOutcome::Queued);
    assert_eq!(events.next().await, Some(SessionEvent::Died));
    session.respawn().await.unwrap();

    let again = controller.emit(SessionEvent::Died);

    assert_eq!(again, EmitOutcome::Queued);
    assert_eq!(events.next().await, Some(SessionEvent::Died));
}

#[tokio::test(start_paused = true)]
async fn chat_is_dropped_and_counted_when_the_queue_is_full() {
    let (_session, mut events, controller) = start_with_capacity(2).await;

    let outcomes = [
        controller.emit(chat("one")),
        controller.emit(chat("two")),
        controller.emit(chat("three")),
        controller.emit(chat("four")),
    ];

    assert_eq!(
        outcomes,
        [
            EmitOutcome::Queued,
            EmitOutcome::Queued,
            EmitOutcome::ChatDropped,
            EmitOutcome::ChatDropped
        ]
    );
    assert_eq!(controller.dropped_chat(), 2);
    assert_eq!(events.next().await, Some(chat("one")));
    assert_eq!(events.next().await, Some(chat("two")));
}

#[tokio::test(start_paused = true)]
async fn lifecycle_events_are_delivered_even_when_the_queue_is_full() {
    let (_session, mut events, controller) = start_with_capacity(1).await;
    assert_eq!(
        controller.emit(chat("fills the queue")),
        EmitOutcome::Queued
    );

    let outcomes = [
        controller.emit(SessionEvent::Joined),
        controller.emit(SessionEvent::Died),
        controller.emit(SessionEvent::Disconnected(
            DisconnectReason::ConnectionClosed,
        )),
    ];

    assert_eq!(outcomes, [EmitOutcome::Queued; 3]);
    assert_eq!(events.next().await, Some(chat("fills the queue")));
    assert_eq!(events.next().await, Some(SessionEvent::Joined));
    assert_eq!(events.next().await, Some(SessionEvent::Died));
    assert_eq!(
        events.next().await,
        Some(SessionEvent::Disconnected(
            DisconnectReason::ConnectionClosed
        ))
    );
}

// --- The handle ---

#[tokio::test(start_paused = true)]
async fn calls_before_joined_fail_with_not_in_world() {
    let (session, _events, controller) = start().await;

    assert_eq!(
        session.perform(GameAction::Jump).await,
        Err(SessionError::NotInWorld)
    );
    assert_eq!(
        session.send_chat(message("hi")).await,
        Err(SessionError::NotInWorld)
    );
    assert_eq!(session.respawn().await, Err(SessionError::NotInWorld));
    assert_eq!(controller.log(), []);
}

#[tokio::test(start_paused = true)]
async fn calls_after_joined_are_logged_in_order() {
    let (session, _events, controller) = start_joined().await;

    session.perform(GameAction::SwingArm).await.unwrap();
    session.send_chat(message("hello")).await.unwrap();
    session.respawn().await.unwrap();
    session.clone().perform(GameAction::Jump).await.unwrap();

    assert_eq!(
        controller.log(),
        [
            Performed::Action(GameAction::SwingArm),
            Performed::Chat(message("hello")),
            Performed::Respawn,
            Performed::Action(GameAction::Jump),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn failing_actions_fail_perform_only_until_they_succeed_again() {
    let (session, _events, controller) = start_joined().await;
    controller.fail_actions(SessionError::QueueFull);

    let failed = session.perform(GameAction::Jump).await;
    let chat = session.send_chat(message("still works")).await;
    controller.succeed_actions();
    let again = session.perform(GameAction::Jump).await;

    assert_eq!(failed, Err(SessionError::QueueFull));
    assert_eq!(chat, Ok(()));
    assert_eq!(again, Ok(()));
    assert_eq!(
        controller.log(),
        [
            Performed::Chat(message("still works")),
            Performed::Action(GameAction::Jump),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn calls_after_a_terminal_event_fail_with_closed() {
    let (session, _events, controller) = start_joined().await;
    assert_eq!(
        controller.emit(SessionEvent::Disconnected(
            DisconnectReason::ConnectionClosed,
        )),
        EmitOutcome::Queued
    );

    assert_eq!(
        session.perform(GameAction::Jump).await,
        Err(SessionError::Closed)
    );
    assert_eq!(
        session.send_chat(message("hi")).await,
        Err(SessionError::Closed)
    );
    assert_eq!(session.respawn().await, Err(SessionError::Closed));
}

#[tokio::test(start_paused = true)]
async fn hang_makes_every_call_time_out() {
    let (session, _events, controller) = start_joined().await;

    controller.hang();

    assert_eq!(
        session.perform(GameAction::Jump).await,
        Err(SessionError::TimedOut)
    );
    assert_eq!(
        session.send_chat(message("hi")).await,
        Err(SessionError::TimedOut)
    );
    assert_eq!(session.respawn().await, Err(SessionError::TimedOut));
    assert_eq!(controller.log(), []);
}

#[tokio::test(start_paused = true)]
async fn disconnect_tears_the_session_down() {
    let (session, mut events, controller) = start_joined().await;
    assert_eq!(controller.emit(chat("never read")), EmitOutcome::Queued);

    session.disconnect().await;
    session.clone().disconnect().await;

    assert!(controller.is_torn_down());
    assert_eq!(events.next().await, None);
    assert_eq!(
        controller.emit(SessionEvent::Died),
        EmitOutcome::SessionEnded
    );
    assert_eq!(
        session.perform(GameAction::Jump).await,
        Err(SessionError::Closed)
    );
    assert_eq!(controller.log(), [Performed::Disconnect]);
}

#[tokio::test(start_paused = true)]
async fn disconnect_wakes_a_waiting_next() {
    let (session, mut events, _controller) = start_joined().await;
    let waiting = tokio::spawn(async move { events.next().await });
    tokio::task::yield_now().await;

    session.disconnect().await;

    let event = tokio::time::timeout(Duration::from_secs(5), waiting).await;
    assert_eq!(event.unwrap().unwrap(), None);
}

#[tokio::test(start_paused = true)]
async fn disconnect_finishes_on_a_hung_session() {
    let (session, _events, controller) = start_joined().await;
    controller.hang();

    let teardown = tokio::time::timeout(Duration::from_secs(5), session.disconnect()).await;

    assert!(teardown.is_ok());
    assert!(controller.is_torn_down());
}

// --- Liveness ---

#[tokio::test(start_paused = true)]
async fn liveness_stamps_start_at_creation() {
    let created = tokio::time::Instant::now().into_std();
    let (session, _events, _controller) = start().await;

    let liveness = session.liveness();

    assert_eq!(liveness.last_tick, created);
    assert_eq!(liveness.last_packet, created);
}

#[tokio::test(start_paused = true)]
async fn liveness_follows_the_clock() {
    let (session, _events, _controller) = start().await;
    let before = session.liveness();

    tokio::time::advance(Duration::from_secs(5)).await;
    let after = session.liveness();

    assert_eq!(after.last_tick - before.last_tick, Duration::from_secs(5));
    assert_eq!(
        after.last_packet - before.last_packet,
        Duration::from_secs(5)
    );
}

#[tokio::test(start_paused = true)]
async fn freeze_ticks_stops_only_the_tick_stamp() {
    let (session, _events, controller) = start().await;
    let frozen_at = session.liveness();

    controller.freeze_ticks();
    tokio::time::advance(Duration::from_secs(30)).await;
    let liveness = session.liveness();

    assert_eq!(liveness.last_tick, frozen_at.last_tick);
    assert_eq!(
        liveness.last_packet - frozen_at.last_packet,
        Duration::from_secs(30)
    );
}

#[tokio::test(start_paused = true)]
async fn freeze_packets_stops_only_the_packet_stamp() {
    let (session, _events, controller) = start().await;
    let frozen_at = session.liveness();

    controller.freeze_packets();
    tokio::time::advance(Duration::from_secs(30)).await;
    let liveness = session.liveness();

    assert_eq!(liveness.last_packet, frozen_at.last_packet);
    assert_eq!(
        liveness.last_tick - frozen_at.last_tick,
        Duration::from_secs(30)
    );
}

#[tokio::test(start_paused = true)]
async fn a_second_freeze_keeps_the_first_stamp() {
    let (session, _events, controller) = start().await;
    let frozen_at = session.liveness();
    controller.freeze_ticks();
    tokio::time::advance(Duration::from_secs(10)).await;

    controller.freeze_ticks();
    tokio::time::advance(Duration::from_secs(10)).await;

    assert_eq!(session.liveness().last_tick, frozen_at.last_tick);
}

#[tokio::test(start_paused = true)]
async fn unfreeze_lets_both_stamps_follow_the_clock_again() {
    let (session, _events, controller) = start().await;
    controller.freeze_ticks();
    controller.freeze_packets();
    tokio::time::advance(Duration::from_secs(10)).await;

    controller.unfreeze();
    let now = tokio::time::Instant::now().into_std();

    assert_eq!(session.liveness().last_tick, now);
    assert_eq!(session.liveness().last_packet, now);
}

#[tokio::test(start_paused = true)]
async fn hang_freezes_both_stamps_for_good() {
    let (session, _events, controller) = start().await;
    let hung_at = session.liveness();

    controller.hang();
    tokio::time::advance(Duration::from_secs(60)).await;
    controller.unfreeze();

    assert_eq!(session.liveness(), hung_at);
}
