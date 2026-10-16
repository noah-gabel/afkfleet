//! Who may open a stream: only a verified client certificate whose
//! fingerprint is enrolled, unrevoked and unexpired, of an enabled agent
//! its SAN names (S24, S30); P10.9's four rejections among them.

use core::time::Duration;
use std::net::IpAddr;

use chrono::TimeDelta;
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::system::Clock as _;
use fleet_server::grpc::control::{CERTIFICATE_REFUSED, CERTIFICATE_REQUIRED, REQUEST_ID};
use fleet_server::infra::pki::{CertificateAuthority, Ed25519Key};
use fleet_testkit::system::SeededRandom;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tonic::{Code, Status};

use super::support::{Agent, Call, Settings, identity, on, request_id, start, text};

/// The refusal before the stream opened: one answer for every identity
/// problem, with the call's request ID.
fn assert_refused(status: &Status) {
    assert_eq!(status.code(), Code::Unauthenticated, "{status:?}");
    assert_eq!(status.message(), CERTIFICATE_REFUSED);
    let _ = request_id(status.metadata());
}

/// A refusal TLS made: no call ever reached the service, so neither the
/// gate's answers nor a request ID.
fn assert_handshake_failed(status: &Status) {
    assert_ne!(status.code(), Code::Ok);
    assert_ne!(status.code(), Code::Unauthenticated, "{status:?}");
    assert_ne!(status.message(), CERTIFICATE_REQUIRED);
    assert_ne!(status.message(), CERTIFICATE_REFUSED);
    assert!(status.metadata().get(REQUEST_ID).is_none(), "{status:?}");
}

#[sqlx::test(migrations = false)]
async fn a_call_without_a_client_certificate_is_refused_before_anything_is_read(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    // Anything that reached the database would now fail another way.
    port.db.close().await;
    let client = port.client(None).await.unwrap();

    let status = Call::open(client).await.unwrap_err();

    assert_eq!(status.code(), Code::Unauthenticated, "{status:?}");
    assert_eq!(status.message(), CERTIFICATE_REQUIRED);
    assert!(
        status.metadata().get(REQUEST_ID).is_none(),
        "the gate checks the certificate before it mints an ID: {status:?}"
    );
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_revoked_certificate_is_refused_like_an_unknown_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    port.revoke(&agent).await;
    let stranger = port.stranger(2);

    let revoked = port.open(&agent).await.unwrap_err();
    let unknown = port.open(&stranger).await.unwrap_err();

    assert_refused(&revoked);
    assert_refused(&unknown);
    // Only the certificate the server knows is audited.
    let entries = port.entries(&AuditAction::AGENT_CONNECT).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(on(&entries[0], &agent));
    assert_eq!(entries[0].outcome, AuditOutcome::Failure);
    assert_eq!(text(&entries[0], "reason").as_deref(), Some("revoked"));
    assert_eq!(port.row(&agent).await.last_seen_at, None);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_client_certificate_from_another_ca_fails_the_handshake(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let random = SeededRandom::new(77);
    let now = port.clock.now();
    let (other, _) = CertificateAuthority::create(&random, now).unwrap();
    let key = Ed25519Key::generate(&random).unwrap();
    let named = port.agent_id(1);
    let cert = other
        .issue_agent(&key, named, TimeDelta::days(60), &random, now)
        .unwrap();
    // Its fingerprint is even stored as the agent's: TLS refuses it first.
    let agent = port
        .store(
            1,
            "agent-1",
            Agent {
                id: named,
                name: String::new(),
                identity: identity(&cert.pem, &key),
                fingerprint: cert.fingerprint,
                der: cert.der.to_vec(),
                pem: cert.pem.clone(),
                issued: now,
                not_after: cert.validity.not_after,
            },
        )
        .await;

    let status = port.open(&agent).await.unwrap_err();

    assert_handshake_failed(&status);
    assert_eq!(port.audit().await, Vec::new());
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn an_agent_that_never_enrolled_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let stranger = port.stranger(3);

    let status = port.open(&stranger).await.unwrap_err();

    assert_refused(&status);
    // Only someone with the CA's key makes such a certificate: a log line
    // says so, and nothing is audited (S30).
    assert_eq!(port.audit().await, Vec::new());
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn an_expired_certificate_fails_the_handshake(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let now = port.clock.now();
    let expired = port.certificate(
        port.agent_id(4),
        now - TimeDelta::days(3),
        TimeDelta::days(1),
    );
    let agent = port.store(4, "agent-4", expired).await;

    let status = port.open(&agent).await.unwrap_err();

    assert_handshake_failed(&status);
    assert_eq!(port.audit().await, Vec::new());
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn every_identity_problem_is_one_answer_and_audited_with_its_reason(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let now = port.clock.now();
    let revoked = port.enroll(1, "agent-1").await;
    port.revoke(&revoked).await;
    // Valid for TLS, which reads the real time; expired by the server's
    // clock, which decides (S24).
    let short = port.certificate(port.agent_id(2), now, TimeDelta::days(1));
    let expired = port.store(2, "agent-2", short).await;
    let disabled = port.enroll(3, "agent-3").await;
    port.disable(&disabled).await;
    let elsewhere = port.certificate(port.agent_id(9), now, TimeDelta::days(60));
    let mismatched = port.store(4, "agent-4", elsewhere).await;
    port.clock.advance(Duration::from_hours(48));

    for agent in [&revoked, &expired, &disabled, &mismatched] {
        let status = port.open(agent).await.unwrap_err();
        assert_refused(&status);
    }

    let entries = port.entries(&AuditAction::AGENT_CONNECT).await;
    assert_eq!(entries.len(), 4, "{entries:?}");
    for (agent, reason) in [
        (&revoked, "revoked"),
        (&expired, "expired"),
        (&disabled, "disabled"),
        (&mismatched, "san_mismatch"),
    ] {
        let entry = entries
            .iter()
            .find(|entry| on(entry, agent))
            .unwrap_or_else(|| panic!("no entry for {reason}: {entries:?}"));
        assert_eq!(entry.outcome, AuditOutcome::Failure);
        assert_eq!(entry.actor, None);
        assert_eq!(entry.ip, Some(IpAddr::from([127, 0, 0, 1])));
        assert_eq!(text(entry, "reason").as_deref(), Some(reason));
        assert_eq!(
            text(entry, "fingerprint"),
            Some(agent.fingerprint.to_string())
        );
    }
    port.stop().await;
}
