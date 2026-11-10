//! `AgentControl.RenewCertificate` over the real port (Plan.md P13.6;
//! ADR-0027): an admitted certificate gets its successor for the same key;
//! the old one stays valid until the first hello with the new one; every
//! identity problem is `Connect`'s one answer, recorded as a refused
//! connection; and the call spends the certificate's `Connect` limit.

use core::num::NonZeroU32;
use core::time::Duration;
use std::net::IpAddr;

use chrono::TimeDelta;
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::system::Clock as _;
use fleet_proto::generated as wire;
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::grpc::control::{
    CERTIFICATE_REFUSED, CERTIFICATE_REQUIRED, RATE_LIMITED, REQUEST_ID, RETRY_AFTER,
};
use fleet_server::ports::store::Store as _;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tonic::Code;

use super::support::{BOUND, Settings, on, request_id, start, text};

#[sqlx::test(migrations = false)]
async fn an_agent_renews_its_certificate_and_connects_with_the_new_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    fleet_testkit::log_capture::install().unwrap();
    let port = start(options, Settings::default()).await;
    // TLS reads the real time, so the server's clock stays there: a
    // certificate renewed in its future wouldn't be valid yet.
    let agent = port.enroll(1, "agent-1").await;

    let (renewed, metadata) = port.renew(&agent).await.unwrap();

    let _ = request_id(&metadata);
    assert_ne!(renewed.fingerprint, agent.fingerprint);
    assert_eq!(renewed.not_after, {
        // Whole seconds from the server's clock, 60 days on.
        let now = port.clock.now();
        chrono::DateTime::from_timestamp(now.timestamp(), 0).unwrap() + TimeDelta::days(60)
    });
    let entries = port.entries(&AuditAction::AGENT_CERT_RENEW).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(on(&entries[0], &agent));
    assert_eq!(entries[0].outcome, AuditOutcome::Success);
    assert_eq!(entries[0].actor, None);
    assert_eq!(entries[0].ip, Some(IpAddr::from([127, 0, 0, 1])));
    assert_eq!(
        text(&entries[0], "fingerprint"),
        Some(renewed.fingerprint.to_string())
    );
    assert_eq!(
        text(&entries[0], "replaces"),
        Some(agent.fingerprint.to_string())
    );
    assert_eq!(
        port.metric("afkfleet_agent_certificate_renewals_total")
            .as_deref(),
        Some("1")
    );
    assert_eq!(
        fleet_testkit::log_capture::count(
            "fleet_server::grpc::control::renew",
            "a certificate renewal finished"
        ),
        1
    );
    // The new certificate's first hello ends the old one.
    let (mut call, welcome) = port.welcomed(&renewed).await;
    assert_eq!(welcome.agent_id, agent.id);
    call.close();
    let _ = call.next().await;
    let _ = port.disconnect(&agent).await;
    let stale = port.open(&agent).await.unwrap_err();
    assert_eq!(stale.code(), Code::Unauthenticated, "{stale:?}");
    assert_eq!(stale.message(), CERTIFICATE_REFUSED);
    let again = port.renew(&agent).await.map(drop).unwrap_err();
    assert_eq!(again.code(), Code::Unauthenticated, "{again:?}");
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_renewal_leaves_the_agents_live_stream_alone_and_the_old_certificate_valid(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (mut call, _) = port.welcomed(&agent).await;

    let (renewed, _) = port.renew(&agent).await.unwrap();

    // The stream goes on: it isn't ended, and nothing was sent on it.
    assert_eq!(port.registry.live_certificates().len(), 1);
    assert_eq!(
        port.registry.live_certificates()[0].fingerprint,
        agent.fingerprint
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), call.next())
            .await
            .is_err(),
        "the stream said nothing"
    );
    // The answer may be lost: the old certificate still connects, and
    // doing so leaves the new one valid.
    call.close();
    let _ = call.next().await;
    let _ = port.disconnect(&agent).await;
    let (mut again, _) = port.welcomed(&agent).await;
    again.close();
    let _ = again.next().await;
    let certs = port.db.agents().certs(agent.id).await.unwrap();
    assert_eq!(certs.len(), 2);
    assert!(certs.iter().all(|cert| cert.revoked_at.is_none()));
    assert_eq!(certs[1].fingerprint, renewed.fingerprint);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_call_without_a_client_certificate_renews_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    // Anything that reached the database would now fail another way.
    port.db.close().await;
    let mut client = port.client(None).await.unwrap();

    let status = tokio::time::timeout(BOUND, client.renew_certificate(wire::RenewRequest {}))
        .await
        .unwrap()
        .unwrap_err();

    assert_eq!(status.code(), Code::Unauthenticated, "{status:?}");
    assert_eq!(status.message(), CERTIFICATE_REQUIRED);
    assert!(status.metadata().get(REQUEST_ID).is_none(), "{status:?}");
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn every_identity_problem_is_connects_one_answer_and_renews_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let now = port.clock.now();
    let revoked = port.enroll(1, "agent-1").await;
    port.revoke(&revoked).await;
    // Valid for TLS, which reads the real time; expired by the server's
    // clock, which decides (S24): an expired certificate enrolls again.
    let short = port.certificate(port.agent_id(2), now, TimeDelta::days(1));
    let expired = port.store(2, "agent-2", short).await;
    let disabled = port.enroll(3, "agent-3").await;
    port.disable(&disabled).await;
    let elsewhere = port.certificate(port.agent_id(9), now, TimeDelta::days(60));
    let mismatched = port.store(4, "agent-4", elsewhere).await;
    let stranger = port.stranger(5);
    port.clock.advance(Duration::from_hours(48));

    for agent in [&revoked, &expired, &disabled, &mismatched, &stranger] {
        let status = port.renew(agent).await.map(drop).unwrap_err();
        assert_eq!(status.code(), Code::Unauthenticated, "{status:?}");
        assert_eq!(status.message(), CERTIFICATE_REFUSED);
        let _ = request_id(status.metadata());
    }

    // Recorded as refused connections are, each with its reason; the
    // certificate no agent holds isn't.
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
        assert_eq!(text(entry, "reason").as_deref(), Some(reason));
        assert_eq!(port.db.agents().certs(agent.id).await.unwrap().len(), 1);
    }
    assert_eq!(
        port.entries(&AuditAction::AGENT_CERT_RENEW).await,
        Vec::new()
    );
    assert_eq!(
        port.metric("afkfleet_agent_certificate_renewals_total")
            .as_deref(),
        Some("0")
    );
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_renewal_spends_the_limit_its_certificates_connects_spend(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(
        options,
        Settings {
            limits: RateLimitConfig {
                connect: Rate::per_minute(NonZeroU32::new(3).unwrap()),
                ..RateLimitConfig::default()
            },
            ..Settings::default()
        },
    )
    .await;
    let agent = port.enroll(1, "agent-1").await;
    let other = port.enroll(2, "agent-2").await;
    // Two renewals and a connect: the certificate's three.
    port.renew(&agent).await.unwrap();
    port.renew(&agent).await.unwrap();
    let (mut call, _) = port.welcomed(&agent).await;
    call.close();
    let _ = call.next().await;

    let renewal = port.renew(&agent).await.map(drop).unwrap_err();
    let connect = port.open(&agent).await.unwrap_err();

    for status in [&renewal, &connect] {
        assert_eq!(status.code(), Code::ResourceExhausted, "{status:?}");
        assert_eq!(status.message(), RATE_LIMITED);
        assert!(status.metadata().get(RETRY_AFTER).is_some(), "{status:?}");
        let _ = request_id(status.metadata());
    }
    // Another certificate keeps its own budget.
    port.renew(&other).await.unwrap();
    assert_eq!(port.entries(&AuditAction::AGENT_CERT_RENEW).await.len(), 3);
    port.stop().await;
}
