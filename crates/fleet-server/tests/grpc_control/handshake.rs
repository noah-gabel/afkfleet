//! The hello and every answer before `Welcome` (S24, S29): the protocol
//! version first, then the conversion, then the name byte for byte, then
//! the hello's transaction, which checks the certificate and the agent
//! again.

use core::time::Duration;
use std::net::IpAddr;

use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::system::Clock as _;
use fleet_core::value::AgentVersion;
use fleet_proto::PROTOCOL_VERSION;
use fleet_proto::control::RejectReason;
use fleet_proto::generated as wire;
use fleet_server::config::Heartbeats;
use fleet_server::grpc::control::{
    CERTIFICATE_REFUSED, ControlTimings, HELLO_MISSING, HELLO_TOO_SLOW,
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tonic::Code;

use super::support::{
    AGENT_VERSION, CAPACITY, Settings, heartbeat, hello, hello_with, on, request_id, start, text,
};

#[sqlx::test(migrations = false)]
async fn an_agent_is_welcomed_with_its_id_and_its_hello_is_recorded(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    let mut call = port.open(&agent).await.unwrap();
    let _ = call.request_id();

    call.send(hello("agent-1")).await;
    let welcome = call.welcome().await;

    assert_eq!(welcome.agent_id, agent.id);
    assert_eq!(welcome.heartbeat_interval, Heartbeats::default().interval());
    let row = port.row(&agent).await;
    assert_eq!(
        row.capacity.map(|capacity| u32::from(capacity.get())),
        Some(CAPACITY)
    );
    assert_eq!(
        row.version.as_ref().map(AgentVersion::as_str),
        Some(AGENT_VERSION)
    );
    assert_eq!(row.last_seen_at, Some(port.clock.now()));
    let entries = port.entries(&AuditAction::AGENT_CONNECT).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    let entry = &entries[0];
    assert!(on(entry, &agent));
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, None);
    assert_eq!(entry.ip, Some(IpAddr::from([127, 0, 0, 1])));
    assert_eq!(
        text(entry, "fingerprint"),
        Some(agent.fingerprint.to_string())
    );
    assert_eq!(text(entry, "version").as_deref(), Some(AGENT_VERSION));
    assert_eq!(text(entry, "capacity"), Some(CAPACITY.to_string()));
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn another_protocol_version_is_rejected_as_incompatible_and_the_stream_ends_ok(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;

    for version in [0, PROTOCOL_VERSION + 1, u32::MAX] {
        let mut call = port.open(&agent).await.unwrap();
        // The version is read first: a hello that doesn't convert either
        // is still incompatible.
        call.send(hello_with("agent-1", version, 0)).await;
        assert_eq!(
            call.rejected().await,
            RejectReason::IncompatibleProtocol,
            "version {version}"
        );
    }

    assert_eq!(port.row(&agent).await.last_seen_at, None);
    assert_eq!(port.audit().await, Vec::new());
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_hello_naming_another_agent_is_rejected_and_audited(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    let _other = port.enroll(2, "agent-2").await;

    for name in ["agent-2", "Agent-1"] {
        let mut call = port.open(&agent).await.unwrap();
        call.send(hello(name)).await;
        assert_eq!(call.rejected().await, RejectReason::NameMismatch, "{name}");
    }

    let entries = port.entries(&AuditAction::AGENT_CONNECT).await;
    assert_eq!(entries.len(), 2, "{entries:?}");
    for entry in &entries {
        assert!(on(entry, &agent));
        assert_eq!(entry.outcome, AuditOutcome::Failure);
        assert_eq!(text(entry, "reason").as_deref(), Some("name_mismatch"));
    }
    assert_eq!(port.row(&agent).await.last_seen_at, None);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_first_message_that_isnt_a_hello_or_doesnt_convert_is_rejected_as_invalid(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    let firsts = [
        ("capacity 0", hello_with("agent-1", PROTOCOL_VERSION, 0)),
        (
            "capacity 1001",
            hello_with("agent-1", PROTOCOL_VERSION, 1001),
        ),
        ("a heartbeat", heartbeat()),
        ("an empty message", wire::AgentMessage { msg: None }),
    ];

    for (case, first) in firsts {
        let mut call = port.open(&agent).await.unwrap();
        call.send(first).await;
        assert_eq!(call.rejected().await, RejectReason::InvalidHello, "{case}");
    }

    assert_eq!(port.audit().await, Vec::new());
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_stream_without_a_hello_in_time_ends_deadline_exceeded(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(
        options,
        Settings {
            timings: ControlTimings {
                hello_timeout: Duration::from_millis(300),
                ..ControlTimings::new(Heartbeats::default())
            },
            ..Settings::default()
        },
    )
    .await;
    let agent = port.enroll(1, "agent-1").await;
    let mut call = port.open(&agent).await.unwrap();

    let status = call.ended().await;

    assert_eq!(status.code(), Code::DeadlineExceeded, "{status:?}");
    assert_eq!(status.message(), HELLO_TOO_SLOW);
    assert_eq!(request_id(status.metadata()), call.request_id());
    assert_eq!(port.audit().await, Vec::new());
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_stream_closed_before_its_hello_ends_cancelled(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    let mut call = port.open(&agent).await.unwrap();

    call.close();
    let status = call.ended().await;

    assert_eq!(status.code(), Code::Cancelled, "{status:?}");
    assert_eq!(status.message(), HELLO_MISSING);
    assert_eq!(port.audit().await, Vec::new());
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn an_agent_disabled_or_revoked_after_its_admission_is_refused_at_its_hello(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let disabled = port.enroll(1, "agent-1").await;
    let revoked = port.enroll(2, "agent-2").await;
    let mut disabled_call = port.open(&disabled).await.unwrap();
    let mut revoked_call = port.open(&revoked).await.unwrap();
    port.disable(&disabled).await;
    port.revoke(&revoked).await;

    disabled_call.send(hello("agent-1")).await;
    revoked_call.send(hello("agent-2")).await;

    for (call, agent, reason) in [
        (&mut disabled_call, &disabled, "disabled"),
        (&mut revoked_call, &revoked, "revoked"),
    ] {
        let status = call.ended().await;
        assert_eq!(status.code(), Code::Unauthenticated, "{status:?}");
        assert_eq!(status.message(), CERTIFICATE_REFUSED);
        let entries: Vec<_> = port
            .entries(&AuditAction::AGENT_CONNECT)
            .await
            .into_iter()
            .filter(|entry| on(entry, agent))
            .collect();
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].outcome, AuditOutcome::Failure);
        assert_eq!(text(&entries[0], "reason").as_deref(), Some(reason));
        assert_eq!(port.row(agent).await.last_seen_at, None);
    }
    port.stop().await;
}
