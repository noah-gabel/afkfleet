//! A welcomed stream until it ends (S24, S27, S29): what it keeps, what
//! ends it, and what its end records.

use core::time::Duration;

use chrono::TimeDelta;
use fleet_core::audit::AuditAction;
use fleet_core::system::Clock as _;
use fleet_proto::control::RejectReason;
use fleet_proto::generated as wire;
use fleet_server::grpc::control::{ONE_HELLO, SHUTTING_DOWN};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tonic::Code;

use super::proxy::Proxy;
use super::support::{BOUND, Call, Settings, heartbeat, hello, request_id, start, text};

/// An event whose bot ID isn't one: it never converts.
fn unreadable_event() -> wire::AgentMessage {
    wire::AgentMessage {
        msg: Some(wire::agent_message::Msg::Event(wire::BotEvent {
            bot_id: vec![1, 2, 3],
            ..wire::BotEvent::default()
        })),
    }
}

#[sqlx::test(migrations = false)]
async fn a_second_hello_ends_the_stream_as_a_protocol_error(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (mut call, _) = port.welcomed(&agent).await;

    call.send(hello("agent-1")).await;
    let status = call.ended().await;

    assert_eq!(status.code(), Code::FailedPrecondition, "{status:?}");
    assert_eq!(status.message(), ONE_HELLO);
    assert_eq!(request_id(status.metadata()), call.request_id());
    let ended = port.disconnect(&agent).await;
    assert_eq!(text(&ended, "reason").as_deref(), Some("protocol_error"));
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_message_that_doesnt_convert_is_dropped_and_the_stream_stays(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (mut call, _) = port.welcomed(&agent).await;

    call.send(unreadable_event()).await;
    call.send(heartbeat()).await;
    // Only a running session answers a second hello.
    call.send(hello("agent-1")).await;

    let status = call.ended().await;
    assert_eq!(status.message(), ONE_HELLO, "{status:?}");
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_stream_the_agent_closes_ends_ok_and_records_when_it_was_last_heard(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    let connected = port.clock.now();
    let (mut call, _) = port.welcomed(&agent).await;
    port.clock.advance(Duration::from_secs(5));
    call.send(heartbeat()).await;

    call.close();
    let after = call.next().await;

    assert!(matches!(after, Ok(None)), "{after:?}");
    let ended = port.disconnect(&agent).await;
    assert_eq!(text(&ended, "reason").as_deref(), Some("closed"));
    assert_eq!(
        text(&ended, "connected_at"),
        Some(connected.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
    );
    assert_eq!(
        port.row(&agent).await.last_seen_at,
        Some(connected + TimeDelta::seconds(5))
    );
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_stream_whose_connection_dies_records_a_stream_error(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let proxy = Proxy::start(port.address).await;
    let agent = port.enroll(1, "agent-1").await;
    let client = port
        .client_at(proxy.address, Some(&agent.identity))
        .await
        .unwrap();
    let mut call = Call::open(client).await.unwrap();
    call.send(hello("agent-1")).await;
    let _ = call.welcome().await;

    proxy.cut();

    let ended = port.disconnect(&agent).await;
    assert_eq!(text(&ended, "reason").as_deref(), Some("stream_error"));
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn the_shutdown_ends_every_stream_unavailable_and_records_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let first = port.enroll(1, "agent-1").await;
    let second = port.enroll(2, "agent-2").await;
    let (mut first_call, _) = port.welcomed(&first).await;
    let (mut second_call, _) = port.welcomed(&second).await;

    tokio::time::timeout(BOUND, port.sessions.shutdown())
        .await
        .unwrap();

    for (call, agent) in [(&mut first_call, &first), (&mut second_call, &second)] {
        let status = call.ended().await;
        assert_eq!(status.code(), Code::Unavailable, "{status:?}");
        assert_eq!(status.message(), SHUTTING_DOWN);
        // Each recorded its end before the shutdown returned.
        let ended: Vec<_> = port
            .entries(&AuditAction::AGENT_DISCONNECT)
            .await
            .into_iter()
            .filter(|entry| super::support::on(entry, agent))
            .collect();
        assert_eq!(ended.len(), 1, "{ended:?}");
        assert_eq!(
            text(&ended[0], "reason").as_deref(),
            Some("server_shutdown")
        );
    }
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_stream_ends_when_its_certificate_expires(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    // The server's clock says the certificate has 300 ms left.
    port.clock
        .set(agent.not_after - TimeDelta::milliseconds(300));
    let (mut call, _) = port.welcomed(&agent).await;

    assert_eq!(call.rejected().await, RejectReason::CertificateExpired);
    let ended = port.disconnect(&agent).await;
    assert_eq!(
        text(&ended, "reason").as_deref(),
        Some("certificate_expired")
    );
    port.stop().await;
}
