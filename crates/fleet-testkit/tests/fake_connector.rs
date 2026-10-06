//! Tests for `FakeConnector`: scripted connect results, the connect log, and
//! how a test reaches each session (Plan.md P3.1).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;

use fleet_core::chat::IncomingChat;
use fleet_core::mc::{
    ConnectError, ConnectParams, MinecraftConnector, SessionCredentials, SessionEvent,
    SessionEvents,
};
use fleet_testkit::mc::{EmitOutcome, FakeConnector};

fn params(server: &str) -> ConnectParams {
    ConnectParams {
        bot_id: "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap(),
        server: server.try_into().unwrap(),
        credentials: SessionCredentials::Offline {
            username: "AfkBot1".try_into().unwrap(),
        },
        connect_timeout: Duration::from_secs(30),
    }
}

/// The servers of every recorded connect, in order.
fn servers(connector: &FakeConnector) -> Vec<String> {
    connector
        .connects()
        .iter()
        .map(|params| params.server.to_string())
        .collect()
}

#[tokio::test(start_paused = true)]
async fn connect_without_a_script_starts_a_session_and_records_the_params() {
    let connector = FakeConnector::new();

    let result = connector.connect(params("localhost:25565")).await;

    assert!(result.is_ok());
    assert_eq!(connector.session_count(), 1);
    assert_eq!(servers(&connector), ["localhost:25565"]);
}

#[tokio::test(start_paused = true)]
async fn scripted_results_are_used_in_order_then_sessions_start_again() {
    let connector = FakeConnector::new();
    connector.push_connect_result(Err(ConnectError::HostUnavailable));
    connector.push_connect_result(Ok(()));
    connector.push_connect_result(Err(ConnectError::HostUnavailable));

    let first = connector.connect(params("a.example.com")).await;
    let second = connector.connect(params("b.example.com")).await;
    let third = connector.connect(params("c.example.com")).await;
    let fourth = connector.connect(params("d.example.com")).await;

    assert_eq!(first.err(), Some(ConnectError::HostUnavailable));
    assert!(second.is_ok());
    assert_eq!(third.err(), Some(ConnectError::HostUnavailable));
    assert!(fourth.is_ok());
    assert_eq!(connector.session_count(), 2);
    assert_eq!(
        servers(&connector),
        [
            "a.example.com",
            "b.example.com",
            "c.example.com",
            "d.example.com"
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn try_session_is_none_until_the_session_starts() {
    let connector = FakeConnector::new();
    assert!(connector.try_session(0).is_none());

    let _session = connector.connect(params("localhost")).await.unwrap();

    assert!(connector.try_session(0).is_some());
    assert!(connector.try_session(1).is_none());
}

#[tokio::test(start_paused = true)]
async fn session_waits_until_the_code_under_test_connects() {
    let connector = FakeConnector::new();
    let code_under_test = {
        let connector = connector.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            connector.connect(params("localhost")).await.map(|_| ())
        })
    };

    let controller = tokio::time::timeout(Duration::from_secs(10), connector.session(0)).await;

    assert!(controller.is_ok());
    assert_eq!(code_under_test.await.unwrap(), Ok(()));
}

#[tokio::test(start_paused = true)]
async fn sessions_are_numbered_in_start_order() {
    let connector = FakeConnector::new();
    let (_first, mut first_events) = connector.connect(params("a.example.com")).await.unwrap();
    let (_second, mut second_events) = connector.connect(params("b.example.com")).await.unwrap();

    let second = connector.session(1).await.emit(SessionEvent::Joined);
    let first = connector
        .session(0)
        .await
        .emit(SessionEvent::Chat(IncomingChat::system("first")));

    assert_eq!((first, second), (EmitOutcome::Queued, EmitOutcome::Queued));

    assert_eq!(second_events.next().await, Some(SessionEvent::Joined));
    assert_eq!(
        first_events.next().await,
        Some(SessionEvent::Chat(IncomingChat::system("first")))
    );
}

#[tokio::test(start_paused = true)]
async fn clones_share_the_script_the_log_and_the_sessions() {
    let connector = FakeConnector::new();
    let clone = connector.clone();
    connector.push_connect_result(Err(ConnectError::HostUnavailable));

    let failed = clone.connect(params("a.example.com")).await;
    let started = clone.connect(params("b.example.com")).await;

    assert!(failed.is_err());
    assert!(started.is_ok());
    assert_eq!(servers(&connector), ["a.example.com", "b.example.com"]);
    assert!(connector.try_session(0).is_some());
}

#[tokio::test(start_paused = true)]
async fn with_event_capacity_sets_how_many_events_a_session_queues() {
    let connector = FakeConnector::with_event_capacity(1);
    let _session = connector.connect(params("localhost")).await.unwrap();
    let controller = connector.session(0).await;

    let first = controller.emit(SessionEvent::Chat(IncomingChat::system("one")));
    let second = controller.emit(SessionEvent::Chat(IncomingChat::system("two")));

    assert_eq!(first, EmitOutcome::Queued);
    assert_eq!(second, EmitOutcome::ChatDropped);
}

#[tokio::test(start_paused = true)]
async fn default_sessions_queue_the_default_capacity() {
    let connector = FakeConnector::default();
    let _session = connector.connect(params("localhost")).await.unwrap();
    let controller = connector.session(0).await;

    let outcomes: Vec<EmitOutcome> = (0..=FakeConnector::DEFAULT_EVENT_CAPACITY)
        .map(|n| controller.emit(SessionEvent::Chat(IncomingChat::system(&n.to_string()))))
        .collect();

    assert_eq!(
        outcomes
            .iter()
            .filter(|o| **o == EmitOutcome::Queued)
            .count(),
        FakeConnector::DEFAULT_EVENT_CAPACITY
    );
    assert_eq!(outcomes.last(), Some(&EmitOutcome::ChatDropped));
}
