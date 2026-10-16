//! Liveness (S23): every message that converts keeps a stream alive until
//! the heartbeat timeout, and HTTP/2 keepalive closes a connection that
//! stops answering. Heartbeats of 1 s and 2 s; everything else at its
//! production value.

use core::time::Duration;

use fleet_proto::generated as wire;
use fleet_server::grpc::control::{HEARTBEAT_MISSED, ONE_HELLO};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::time::Instant;
use tonic::Code;

use super::proxy::Proxy;
use super::support::{BOUND, Call, Settings, heartbeat, hello, start, text};

/// The heartbeat timeout of these tests.
const TIMEOUT: Duration = Duration::from_secs(2);

fn settings() -> Settings {
    Settings::heartbeats(1, 2)
}

#[sqlx::test(migrations = false)]
async fn a_silent_stream_ends_at_the_heartbeat_timeout(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (mut call, _) = port.welcomed(&agent).await;
    let welcomed = Instant::now();

    let status = call.ended().await;

    assert!(welcomed.elapsed() >= TIMEOUT.saturating_sub(Duration::from_millis(100)));
    assert_eq!(status.code(), Code::DeadlineExceeded, "{status:?}");
    assert_eq!(status.message(), HEARTBEAT_MISSED);
    let ended = port.disconnect(&agent).await;
    assert_eq!(text(&ended, "reason").as_deref(), Some("heartbeat_timeout"));
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn messages_that_dont_convert_are_no_sign_of_life(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (mut call, _) = port.welcomed(&agent).await;
    let unreadable = wire::AgentMessage {
        msg: Some(wire::agent_message::Msg::Event(wire::BotEvent::default())),
    };
    let mut every = tokio::time::interval(Duration::from_millis(250));

    let status = tokio::time::timeout(BOUND, async {
        loop {
            // `message()` is cancel-safe: a lost race read nothing.
            tokio::select! {
                answer = call.answers.message() => break answer,
                _ = every.tick() => call.send(unreadable.clone()).await,
            }
        }
    })
    .await
    .unwrap()
    .unwrap_err();

    assert_eq!(status.message(), HEARTBEAT_MISSED, "{status:?}");
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_beating_agent_outlives_three_heartbeat_timeouts(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (mut call, welcome) = port.welcomed(&agent).await;
    let mut beats = tokio::time::interval_at(
        Instant::now() + welcome.heartbeat_interval,
        welcome.heartbeat_interval,
    );

    // Six beats: past three timeouts, with no answer and no end meanwhile.
    for _ in 0..6 {
        tokio::select! {
            answer = call.answers.message() => panic!("the stream answered: {answer:?}"),
            _ = beats.tick() => call.send(heartbeat()).await,
        }
    }

    call.send(hello("agent-1")).await;
    let status = call.ended().await;
    assert_eq!(status.message(), ONE_HELLO, "{status:?}");
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_silent_connection_is_closed_at_the_heartbeat_timeout(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let mut proxy = Proxy::start(port.address).await;
    let agent = port.enroll(1, "agent-1").await;
    let client = port
        .client_at(proxy.address, Some(&agent.identity))
        .await
        .unwrap();
    // A finished call: the server has the connection's handshake behind it,
    // and the connection is idle.
    let _kept = client.clone();
    let mut call = Call::open(client).await.unwrap();
    call.send(hello("agent-1")).await;
    let _ = call.welcome().await;
    call.close();
    assert!(matches!(call.next().await, Ok(None)));

    proxy.go_silent();
    let silent = Instant::now();

    proxy.server_closed(1).await;
    // Not earlier than a ping's interval: it's keepalive that closed it.
    assert!(
        silent.elapsed() >= Duration::from_secs(1),
        "{:?}",
        silent.elapsed()
    );
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_silent_connection_with_a_stream_is_closed_and_the_stream_recorded(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let mut proxy = Proxy::start(port.address).await;
    let agent = port.enroll(1, "agent-1").await;
    let client = port
        .client_at(proxy.address, Some(&agent.identity))
        .await
        .unwrap();
    let mut call = Call::open(client).await.unwrap();
    call.send(hello("agent-1")).await;
    let _ = call.welcome().await;

    proxy.go_silent();

    proxy.server_closed(1).await;
    let ended = port.disconnect(&agent).await;
    // The keepalive and the stream's own deadline run out together; either
    // may end it.
    let reason = text(&ended, "reason");
    assert!(
        matches!(
            reason.as_deref(),
            Some("heartbeat_timeout" | "stream_error")
        ),
        "{reason:?}"
    );
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn an_idle_connection_that_answers_pings_stays_open(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, settings()).await;
    let mut proxy = Proxy::start(port.address).await;
    let agent = port.enroll(1, "agent-1").await;
    let _client = port
        .client_at(proxy.address, Some(&agent.identity))
        .await
        .unwrap();

    let closed = proxy
        .server_closed_within(1, 2 * TIMEOUT + TIMEOUT / 2)
        .await;

    assert!(
        !closed,
        "the server closed a connection that answered its pings"
    );
    assert_eq!(proxy.accepted(), 1);
    port.stop().await;
}
