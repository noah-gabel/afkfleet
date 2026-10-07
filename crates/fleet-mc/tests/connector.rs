//! Tests for `AzaleaConnector` and `McSession` without a Minecraft server:
//! connect failures, the connect timeout, teardown and the host limit
//! (Plan.md P3.4; ADR-0008 §5 and §10, ADR-0011).
//!
//! They talk to `127.0.0.1` only. Like the host-pool tests, they wait for real
//! OS threads, so they run in real time: every wait has an upper bound that
//! only runs out when a test fails, and none of them sleeps. The slow suite
//! covers sessions against a real server (P3.7).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::future::Future;
use core::num::NonZeroUsize;
use core::time::Duration;
use std::net::TcpListener;
use std::sync::mpsc as std_mpsc;
use std::time::Instant;

use fleet_core::disconnect::{ConnectFailure, DisconnectReason};
use fleet_core::id::BotId;
use fleet_core::mc::{
    ConnectError, ConnectParams, MinecraftConnector, SessionCredentials, SessionError,
    SessionEvent, SessionEvents, SessionHandle,
};
use fleet_core::mode::GameAction;
use fleet_mc::{AzaleaConnector, McConfig, McEvents, McSession, ShutdownOutcome};

const BOT: &str = "018bcfe5-6800-7bab-abab-abababababab";

/// The upper bound for waits that only run out when a test fails.
const WITHIN: Duration = Duration::from_secs(30);
/// A connect timeout that a test expects to run out.
const SHORT_CONNECT: Duration = Duration::from_secs(1);
/// A connect timeout that must not run out. Refusing a connection to a closed
/// loopback port takes about 2 s on Windows.
const LONG_CONNECT: Duration = Duration::from_secs(15);

fn bot() -> BotId {
    BOT.parse().unwrap()
}

fn connector() -> AzaleaConnector {
    AzaleaConnector::new(&McConfig::default())
}

fn params(port: u16, connect_timeout: Duration) -> ConnectParams {
    ConnectParams {
        bot_id: bot(),
        server: format!("127.0.0.1:{port}").parse().unwrap(),
        credentials: SessionCredentials::Offline {
            username: "AfkBot1".parse().unwrap(),
        },
        connect_timeout,
    }
}

/// A server that completes the TCP handshake (the OS does that) but never
/// accepts the connection, so the login never gets an answer.
fn silent_server() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

/// A loopback port nobody listens on.
fn closed_port() -> u16 {
    let (listener, port) = silent_server();
    drop(listener);
    port
}

/// Awaits `future`, failing the test if it takes longer than [`WITHIN`].
async fn within<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(WITHIN, future)
        .await
        .expect("the session didn't get there within the test's bound")
}

/// Waits, without sleeping, until `done` holds.
async fn until(mut done: impl FnMut() -> bool) {
    within(async {
        while !done() {
            tokio::task::yield_now().await;
        }
    })
    .await;
}

async fn connect(connector: &AzaleaConnector, params: ConnectParams) -> (McSession, McEvents) {
    within(connector.connect(params)).await.unwrap()
}

// --- Connect failures ---

#[tokio::test]
async fn a_server_that_never_answers_fails_at_the_connect_timeout() {
    let connector = connector();
    let (_listener, port) = silent_server();
    let started = Instant::now();
    let (session, mut events) = connect(&connector, params(port, SHORT_CONNECT)).await;

    let before = session.perform(GameAction::Jump).await;
    let event = within(events.next()).await;

    assert_eq!(before, Err(SessionError::NotInWorld));
    assert_eq!(
        event,
        Some(SessionEvent::ConnectionFailed(ConnectFailure::TimedOut))
    );
    assert!(started.elapsed() >= SHORT_CONNECT);
    assert_eq!(within(events.next()).await, None);
    assert_eq!(
        session.perform(GameAction::Jump).await,
        Err(SessionError::Closed)
    );
    within(session.disconnect()).await;
}

#[tokio::test]
async fn a_closed_port_fails_with_refused() {
    let connector = connector();
    let (session, mut events) = connect(&connector, params(closed_port(), LONG_CONNECT)).await;

    let event = within(events.next()).await;

    assert_eq!(
        event,
        Some(SessionEvent::ConnectionFailed(ConnectFailure::Refused))
    );
    within(session.disconnect()).await;
}

// --- Teardown ---

#[tokio::test]
async fn disconnect_is_idempotent_and_frees_the_thread_and_the_world() {
    let connector = connector();
    let (_listener, port) = silent_server();
    let (session, mut events) = connect(&connector, params(port, LONG_CONNECT)).await;
    until(|| connector.live_worlds() == 1).await;
    let clone = session.clone();

    within(async { tokio::join!(session.disconnect(), clone.disconnect()) }).await;
    within(session.disconnect()).await;

    assert_eq!(connector.pool().live_threads(), 0);
    assert_eq!(connector.live_worlds(), 0);
    assert_eq!(connector.pool().abandoned_threads(), 0);
    // A torn-down session ends its events without a crash.
    assert_eq!(within(events.next()).await, None);
    assert_eq!(
        session.perform(GameAction::Jump).await,
        Err(SessionError::Closed)
    );
}

#[tokio::test]
async fn disconnect_after_the_session_ended_on_its_own_is_harmless() {
    let connector = connector();
    let (_listener, port) = silent_server();
    let (session, mut events) = connect(&connector, params(port, SHORT_CONNECT)).await;
    within(events.next()).await;

    within(session.disconnect()).await;

    assert_eq!(connector.pool().live_threads(), 0);
    assert_eq!(connector.live_worlds(), 0);
}

#[tokio::test]
async fn dropping_every_handle_ends_the_session_and_frees_its_thread() {
    let connector = connector();
    let (_listener, port) = silent_server();
    let (session, mut events) = connect(&connector, params(port, LONG_CONNECT)).await;
    until(|| connector.live_worlds() == 1).await;
    let clone = session.clone();

    drop(session);
    drop(clone);

    assert_eq!(
        within(events.next()).await,
        Some(SessionEvent::Disconnected(DisconnectReason::SessionCrashed))
    );
    assert_eq!(within(events.next()).await, None);
    until(|| connector.pool().live_threads() == 0).await;
    until(|| connector.live_worlds() == 0).await;
    assert_eq!(connector.pool().abandoned_threads(), 0);
}

#[tokio::test]
async fn liveness_starts_when_the_session_does() {
    let connector = connector();
    let (_listener, port) = silent_server();
    let before = Instant::now();

    let (session, _events) = connect(&connector, params(port, LONG_CONNECT)).await;
    let liveness = session.liveness();

    assert!(liveness.last_tick >= before);
    assert!(liveness.last_packet >= before);
    within(session.disconnect()).await;
}

// --- The host limit ---

#[tokio::test]
async fn connect_returns_host_unavailable_once_the_abandoned_limit_is_reached() {
    let connector = AzaleaConnector::new(&McConfig {
        max_abandoned_threads: NonZeroUsize::MIN,
        thread_shutdown_timeout: Duration::from_millis(100),
        ..McConfig::default()
    });
    // Hang one host thread and abandon it.
    let host = connector.pool().spawn(bot()).unwrap();
    let (release_tx, release_rx) = std_mpsc::sync_channel::<()>(1);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let job = host.run(move || async move {
        started_tx.send(()).unwrap();
        let _ = release_rx.recv();
    });
    within(started_rx).await.unwrap();
    drop(job);
    assert_eq!(within(host.shutdown()).await, ShutdownOutcome::Abandoned);

    let refused = within(connector.connect(params(closed_port(), LONG_CONNECT))).await;

    assert_eq!(refused.unwrap_err(), ConnectError::HostUnavailable);
    release_tx.send(()).unwrap();
}
