//! One stream per agent, the ends of a permission, and what an agent may
//! report (P10.5's registry; ADR-0019, S25–S28, R12, R13).

use core::time::Duration;

use chrono::DateTime;
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::id::BotId;
use fleet_proto::control::{RejectReason, ServerMessage};
use fleet_proto::generated as wire;
use fleet_server::grpc::control::{ONE_HELLO, TOO_SLOW};
use fleet_server::grpc::registry::{RefusedReports, SendError};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tonic::Code;

use super::support::{BOUND, Settings, heartbeat, hello, on, start, text};

fn bot(n: u8) -> BotId {
    BotId::new_v7(
        DateTime::from_timestamp_millis(1_790_000_000_000).unwrap(),
        [n; 10],
    )
    .unwrap()
}

/// Production values, with the sweep every 100 ms.
fn sweeping() -> Settings {
    Settings {
        sweep: Duration::from_millis(100),
        ..Settings::default()
    }
}

#[sqlx::test(migrations = false)]
async fn a_second_connect_is_rejected_already_connected_and_the_live_stream_stays(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (mut live, _) = port.welcomed(&agent).await;
    let mut second = port.open(&agent).await.unwrap();

    second.send(hello("agent-1")).await;

    assert_eq!(second.rejected().await, RejectReason::AlreadyConnected);
    // Only the live stream's session answers its second hello.
    live.send(hello("agent-1")).await;
    assert_eq!(live.ended().await.message(), ONE_HELLO);
    // A refusal for a live stream isn't audited (R12).
    let connects = port.entries(&AuditAction::AGENT_CONNECT).await;
    assert!(
        connects
            .iter()
            .all(|entry| entry.outcome == AuditOutcome::Success),
        "{connects:?}"
    );
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn the_next_connect_is_welcomed_once_the_live_stream_ended(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (mut first, _) = port.welcomed(&agent).await;
    first.close();
    assert!(matches!(first.next().await, Ok(None)));

    let (_second, welcome) = port.welcomed(&agent).await;

    assert_eq!(welcome.agent_id, agent.id);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_message_on_the_live_stream_after_a_refusal_is_audited_once_as_a_conflict(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (mut live, _) = port.welcomed(&agent).await;
    for _ in 0..2 {
        let mut second = port.open(&agent).await.unwrap();
        second.send(hello("agent-1")).await;
        assert_eq!(second.rejected().await, RejectReason::AlreadyConnected);
        live.send(heartbeat()).await;
    }

    // The session handles its messages in order: once it answered this,
    // it handled both heartbeats.
    live.send(hello("agent-1")).await;
    assert_eq!(live.ended().await.message(), ONE_HELLO);

    let conflicts = port
        .entries(&AuditAction::SECURITY_AGENT_IDENTITY_CONFLICT)
        .await;
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    let conflict = &conflicts[0];
    assert!(on(conflict, &agent));
    assert_eq!(conflict.outcome, AuditOutcome::Failure);
    assert_eq!(conflict.actor, None);
    assert_eq!(
        text(conflict, "fingerprint"),
        Some(agent.fingerprint.to_string())
    );
    assert_eq!(text(conflict, "live_ip").as_deref(), Some("127.0.0.1"));
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_restarts_refused_connect_records_no_conflict(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (mut old, _) = port.welcomed(&agent).await;
    let mut early = port.open(&agent).await.unwrap();
    early.send(hello("agent-1")).await;
    assert_eq!(early.rejected().await, RejectReason::AlreadyConnected);

    // The old process is gone: its stream ends without another word.
    old.close();
    assert!(matches!(old.next().await, Ok(None)));
    let (mut new, _) = port.welcomed(&agent).await;
    new.send(heartbeat()).await;
    new.send(hello("agent-1")).await;
    assert_eq!(new.ended().await.message(), ONE_HELLO);

    let conflicts = port
        .entries(&AuditAction::SECURITY_AGENT_IDENTITY_CONFLICT)
        .await;
    assert_eq!(conflicts, Vec::new());
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn reports_about_bots_the_agent_doesnt_hold_are_dropped_and_the_stream_stays(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (mut call, _) = port.welcomed(&agent).await;
    let status = wire::AgentMessage {
        msg: Some(wire::agent_message::Msg::Status(wire::BotSnapshot {
            bot_id: bot(1).as_uuid().as_bytes().to_vec(),
            state: Some(wire::BotState {
                state: Some(wire::bot_state::State::Stopped(wire::Unit {})),
            }),
            since_unix_ms: Some(1_790_000_000_000),
            last_disconnect: None,
        })),
    };
    let removed = wire::AgentMessage {
        msg: Some(wire::agent_message::Msg::Event(wire::BotEvent {
            bot_id: bot(2).as_uuid().as_bytes().to_vec(),
            at_unix_ms: Some(1_790_000_000_000),
            kind: Some(wire::bot_event::Kind::Removed(wire::Unit {})),
        })),
    };

    call.send(status).await;
    call.send(removed).await;
    call.send(hello("agent-1")).await;

    assert_eq!(call.ended().await.message(), ONE_HELLO);
    assert_eq!(
        port.registry.take_refused_reports(),
        vec![RefusedReports {
            agent: agent.id,
            name: "agent-1".try_into().unwrap(),
            count: 2,
            sessions_denied: 0,
            chat_dropped: 0,
        }]
    );
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_session_request_is_denied_retryable(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (mut call, _) = port.welcomed(&agent).await;
    let request = wire::AgentMessage {
        msg: Some(wire::agent_message::Msg::SessionRequest(
            wire::SessionRequest {
                id: 7,
                bot_id: bot(1).as_uuid().as_bytes().to_vec(),
                fresh: true,
            },
        )),
    };

    call.send(request).await;

    match call.next().await {
        Ok(Some(ServerMessage::SessionDenied(denied))) => {
            assert_eq!(denied.id.get().get(), 7);
            assert_eq!(denied.bot_id, bot(1));
            assert!(denied.retryable);
        }
        other => panic!("expected SessionDenied, got {other:?}"),
    }
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_host_disable_ends_the_stream_within_one_sweep(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, sweeping()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (mut call, _) = port.welcomed(&agent).await;

    // As `afkfleet-server agent disable` does, beside the running server:
    // every certificate is revoked too, and the end is still the disable.
    port.host_disable(&agent).await;

    assert_eq!(call.rejected().await, RejectReason::AgentDisabled);
    let ended = port.disconnect(&agent).await;
    assert_eq!(text(&ended, "reason").as_deref(), Some("disabled"));
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_revoked_certificate_ends_its_stream_within_one_sweep(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, sweeping()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (mut call, _) = port.welcomed(&agent).await;

    port.revoke(&agent).await;

    assert_eq!(call.rejected().await, RejectReason::CertificateRevoked);
    let ended = port.disconnect(&agent).await;
    assert_eq!(
        text(&ended, "reason").as_deref(),
        Some("certificate_revoked")
    );
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_full_queue_ends_the_stream_too_slow(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (mut call, _) = port.welcomed(&agent).await;

    // The agent reads nothing meanwhile.
    let full = (0..1_000_000_u32)
        .map(|_| {
            port.registry
                .send(agent.id, ServerMessage::Unassign(bot(1)))
        })
        .find(Result::is_err);

    assert_eq!(full, Some(Err(SendError::QueueFull)));
    let status = tokio::time::timeout(BOUND, async {
        loop {
            match call.next().await {
                Ok(Some(ServerMessage::Unassign(_))) => {}
                other => break other,
            }
        }
    })
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(status.code(), Code::ResourceExhausted, "{status:?}");
    assert_eq!(status.message(), TOO_SLOW);
    let ended = port.disconnect(&agent).await;
    assert_eq!(text(&ended, "reason").as_deref(), Some("too_slow"));
    port.stop().await;
}
