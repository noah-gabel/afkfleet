//! Tests for the agents' connections (Plan.md P10.4; ADR-0019, S24, S29,
//! S30): which presented certificate admits its agent, which refusals are
//! audited and how many, what an accepted hello records, and what a
//! stream's end records.
//!
//! Each test gets a fresh database from `#[sqlx::test]` and opens it with
//! `Database::connect`. Time isn't paused: sqlx's worker threads would make
//! paused timeouts fire early (CLAUDE.md). The service's clock is a
//! `ManualClock`.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::net::IpAddr;
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget, RecordedValue};
use fleet_core::id::{AgentId, RequestId};
use fleet_core::pki::CertFingerprint;
use fleet_core::value::{AgentCapacity, AgentName, AgentVersion};
use fleet_server::app::agents::AgentConnectionService;
use fleet_server::infra::pki::{CertificateAuthority, Ed25519Key, X509AgentNames, fingerprint};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::agent_connections::{
    AdmitError, Admitted, AgentConnections as _, Ended, Presented, StreamEnd,
};
use fleet_server::ports::agents::{NewAgent, NewAgentCert};
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditRecord, NewAuditEntry};
use fleet_server::ports::store::Store as _;
use fleet_testkit::system::{ManualClock, SeededRandom};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

const T0: &str = "2027-01-15T08:00:00Z";
const AGENT_IP: [u8; 4] = [192, 0, 2, 20];
/// The setup's own audit entries, which the checks leave out.
const SETUP: &str = "test.connections_setup";

fn at(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

fn peer() -> IpAddr {
    IpAddr::from(AGENT_IP)
}

struct Setup {
    db: Database,
    clock: ManualClock,
    connections: AgentConnectionService,
    ca: CertificateAuthority,
    random: SeededRandom,
}

/// An enrolled agent and the certificate it connects with.
struct Enrolled {
    id: AgentId,
    name: AgentName,
    der: Vec<u8>,
    fingerprint: CertFingerprint,
    not_after: DateTime<Utc>,
}

async fn setup_with(options: SqliteConnectOptions, db_options: DatabaseOptions) -> Setup {
    let db = Database::connect(options, db_options).await.unwrap();
    let clock = ManualClock::new(at(T0));
    let random = SeededRandom::new(21);
    let (ca, _) = CertificateAuthority::create(&random, at(T0)).unwrap();
    let connections = AgentConnectionService::new(
        Arc::new(clock.clone()),
        Arc::new(db.clone()),
        Arc::new(X509AgentNames),
    );
    Setup {
        db,
        clock,
        connections,
        ca,
        random,
    }
}

async fn setup(options: SqliteConnectOptions) -> Setup {
    setup_with(options, DatabaseOptions::default()).await
}

fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from(SETUP).unwrap(),
        AuditOutcome::Success,
    )
}

impl Setup {
    /// Enrolls agent `n` named `name`: its row and a 60-day certificate
    /// whose SAN names `named`, stored under the agent's row.
    async fn enroll_named(&self, n: u8, name: &str, named: AgentId) -> Enrolled {
        let id = AgentId::new_v7(at(T0), [n; 10]).unwrap();
        let key = Ed25519Key::generate(&self.random).unwrap();
        let issued = self
            .ca
            .issue_agent(&key, named, TimeDelta::days(60), &self.random, at(T0))
            .unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.agents()
            .insert(&NewAgent {
                id,
                name: AgentName::try_from(name).unwrap(),
                created_at: at(T0),
            })
            .await
            .unwrap();
        tx.agents()
            .insert_cert(&NewAgentCert {
                fingerprint: issued.fingerprint,
                agent: id,
                not_after: issued.validity.not_after,
                created_at: at(T0),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        Enrolled {
            id,
            name: AgentName::try_from(name).unwrap(),
            der: issued.der.to_vec(),
            fingerprint: issued.fingerprint,
            not_after: issued.validity.not_after,
        }
    }

    /// Enrolls agent `n` named `name`, its certificate naming it.
    async fn enroll(&self, n: u8, name: &str) -> Enrolled {
        let id = AgentId::new_v7(at(T0), [n; 10]).unwrap();
        self.enroll_named(n, name, id).await
    }

    /// Runs `sql` with `id` through the write pool.
    async fn revoke(&self, agent: AgentId) {
        let id = agent.as_uuid().hyphenated().to_string();
        sqlx::query!(
            "UPDATE agent_certs SET revoked_at = '2027-01-15T08:00:00.000Z' WHERE agent_id = ?",
            id
        )
        .execute(self.db.write_pool())
        .await
        .unwrap();
    }

    async fn disable(&self, agent: AgentId) {
        let id = agent.as_uuid().hyphenated().to_string();
        sqlx::query!("UPDATE agents SET enabled = 0 WHERE id = ?", id)
            .execute(self.db.write_pool())
            .await
            .unwrap();
    }

    async fn admit(&self, enrolled: &Enrolled) -> Result<Admitted, AdmitError> {
        self.connections
            .admit(Presented {
                request: RequestId::new_v7(at(T0), [1; 10]).unwrap(),
                fingerprint: enrolled.fingerprint,
                certificate: enrolled.der.clone(),
                peer: Some(peer()),
            })
            .await
    }

    async fn entries(&self) -> Vec<AuditRecord> {
        self.db
            .audit()
            .list(AuditPage {
                before: None,
                limit: AuditLimit::new(100).unwrap(),
            })
            .await
            .unwrap()
            .into_iter()
            .filter(|record| record.action.as_str() != SETUP)
            .collect()
    }
}

fn text(record: &AuditRecord, key: &str) -> Option<String> {
    record
        .metadata
        .entries()
        .iter()
        .find(|(found, _)| found == key)
        .map(|(_, value)| match value {
            RecordedValue::Text(text) => text.clone(),
            RecordedValue::Int(n) => n.to_string(),
            other => format!("{other:?}"),
        })
}

/// The one entry of `entries`, which must be `action` with `outcome` on
/// `agent`, from the agent's address and with no actor.
fn the_entry(
    entries: &[AuditRecord],
    action: &AuditAction,
    outcome: AuditOutcome,
    agent: AgentId,
) -> AuditRecord {
    assert_eq!(entries.len(), 1, "{entries:?}");
    let entry = entries[0].clone();
    assert_eq!(&entry.action, action);
    assert_eq!(entry.outcome, outcome);
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Agent(agent)))
    );
    assert_eq!(entry.actor, None);
    assert_eq!(entry.ip, Some(peer()));
    entry
}

fn refused<T: core::fmt::Debug>(result: &Result<T, AdmitError>) {
    assert!(matches!(result, Err(AdmitError::Refused)), "{result:?}");
}

// --- Admission ---------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn an_enrolled_agents_certificate_admits_it_with_its_name_and_expiry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let agent = setup.enroll(1, "agent-1").await;

    let admitted = setup.admit(&agent).await.unwrap();

    assert_eq!(admitted.agent, agent.id);
    assert_eq!(admitted.name, agent.name);
    assert_eq!(admitted.fingerprint, agent.fingerprint);
    assert_eq!(admitted.not_after, agent.not_after);
    assert_eq!(admitted.peer, Some(peer()));
    assert_eq!(
        setup.entries().await,
        Vec::new(),
        "admitting writes nothing"
    );
}

#[sqlx::test(migrations = false)]
async fn a_certificate_no_agent_holds_is_refused_unrecorded(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let key = Ed25519Key::generate(&setup.random).unwrap();
    let stranger = AgentId::new_v7(at(T0), [9; 10]).unwrap();
    let issued = setup
        .ca
        .issue_agent(&key, stranger, TimeDelta::days(60), &setup.random, at(T0))
        .unwrap();

    let result = setup
        .admit(&Enrolled {
            id: stranger,
            name: AgentName::try_from("stranger").unwrap(),
            der: issued.der.to_vec(),
            fingerprint: fingerprint(&issued.der),
            not_after: issued.validity.not_after,
        })
        .await;

    refused(&result);
    assert_eq!(setup.entries().await, Vec::new());
}

#[sqlx::test(migrations = false)]
async fn a_revoked_certificate_is_refused_and_recorded_with_its_reason_agent_and_address(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let agent = setup.enroll(1, "agent-1").await;
    setup.revoke(agent.id).await;

    let result = setup.admit(&agent).await;

    refused(&result);
    let entry = the_entry(
        &setup.entries().await,
        &AuditAction::AGENT_CONNECT,
        AuditOutcome::Failure,
        agent.id,
    );
    assert_eq!(text(&entry, "reason").as_deref(), Some("revoked"));
    assert_eq!(
        text(&entry, "fingerprint"),
        Some(agent.fingerprint.to_string())
    );
}

#[sqlx::test(migrations = false)]
async fn a_certificate_expired_by_the_servers_clock_is_refused_and_recorded(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let agent = setup.enroll(1, "agent-1").await;
    setup.clock.set(agent.not_after);

    let result = setup.admit(&agent).await;

    refused(&result);
    let entry = the_entry(
        &setup.entries().await,
        &AuditAction::AGENT_CONNECT,
        AuditOutcome::Failure,
        agent.id,
    );
    assert_eq!(text(&entry, "reason").as_deref(), Some("expired"));
}

#[sqlx::test(migrations = false)]
async fn a_disabled_agents_unrevoked_certificate_is_refused_as_disabled(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let agent = setup.enroll(1, "agent-1").await;
    setup.disable(agent.id).await;

    let result = setup.admit(&agent).await;

    refused(&result);
    let entry = the_entry(
        &setup.entries().await,
        &AuditAction::AGENT_CONNECT,
        AuditOutcome::Failure,
        agent.id,
    );
    assert_eq!(text(&entry, "reason").as_deref(), Some("disabled"));
}

#[sqlx::test(migrations = false)]
async fn a_revoked_certificate_of_a_disabled_agent_is_recorded_as_revoked(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let agent = setup.enroll(1, "agent-1").await;
    setup.disable(agent.id).await;
    setup.revoke(agent.id).await;

    refused(&setup.admit(&agent).await);

    let entries = setup.entries().await;
    assert_eq!(text(&entries[0], "reason").as_deref(), Some("revoked"));
}

#[sqlx::test(migrations = false)]
async fn a_certificate_whose_san_names_another_agent_is_refused_as_a_san_mismatch(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let other = AgentId::new_v7(at(T0), [2; 10]).unwrap();
    let agent = setup.enroll_named(1, "agent-1", other).await;

    let result = setup.admit(&agent).await;

    refused(&result);
    let entry = the_entry(
        &setup.entries().await,
        &AuditAction::AGENT_CONNECT,
        AuditOutcome::Failure,
        agent.id,
    );
    assert_eq!(text(&entry, "reason").as_deref(), Some("san_mismatch"));
}

#[sqlx::test(migrations = false)]
async fn an_agents_refusals_are_recorded_ten_times_a_day_and_then_only_logged(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let agent = setup.enroll(1, "agent-1").await;
    setup.revoke(agent.id).await;

    for _ in 0..11 {
        refused(&setup.admit(&agent).await);
        setup.clock.advance(Duration::from_secs(60));
    }
    let within = setup.entries().await.len();
    setup.clock.set(at(T0) + TimeDelta::hours(24));
    refused(&setup.admit(&agent).await);

    assert_eq!(within, 10);
    assert_eq!(
        setup.entries().await.len(),
        11,
        "the first one is a day old"
    );
}

#[sqlx::test(migrations = false)]
async fn two_refusals_racing_for_the_last_place_record_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let agent = setup.enroll(1, "agent-1").await;
    setup.revoke(agent.id).await;
    for _ in 0..9 {
        refused(&setup.admit(&agent).await);
    }

    let (first, second) = tokio::join!(setup.admit(&agent), setup.admit(&agent));

    refused(&first);
    refused(&second);
    assert_eq!(setup.entries().await.len(), 10);
}

// --- The hello ---------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn an_accepted_hello_commits_agent_connect_with_the_certificate_the_version_and_the_capacity(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let agent = setup.enroll(1, "agent-1").await;
    let admitted = setup.admit(&agent).await.unwrap();
    setup.clock.advance(Duration::from_secs(2));

    setup
        .connections
        .hello(
            &admitted,
            AgentCapacity::try_from(50_u32).unwrap(),
            &AgentVersion::try_from("0.1.0").unwrap(),
        )
        .await
        .unwrap();

    let entry = the_entry(
        &setup.entries().await,
        &AuditAction::AGENT_CONNECT,
        AuditOutcome::Success,
        agent.id,
    );
    assert_eq!(
        text(&entry, "fingerprint"),
        Some(agent.fingerprint.to_string())
    );
    assert_eq!(text(&entry, "version").as_deref(), Some("0.1.0"));
    assert_eq!(text(&entry, "capacity").as_deref(), Some("50"));
    let row = setup.db.agents().find(agent.id).await.unwrap().unwrap();
    assert_eq!(row.capacity, Some(AgentCapacity::try_from(50_u32).unwrap()));
    assert_eq!(row.version, Some(AgentVersion::try_from("0.1.0").unwrap()));
    assert_eq!(row.last_seen_at, Some(at(T0) + TimeDelta::seconds(2)));
}

#[sqlx::test(migrations = false)]
async fn a_hello_after_the_certificate_was_revoked_is_refused_and_recorded(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let agent = setup.enroll(1, "agent-1").await;
    let admitted = setup.admit(&agent).await.unwrap();
    setup.revoke(agent.id).await;

    let result = setup
        .connections
        .hello(
            &admitted,
            AgentCapacity::try_from(50_u32).unwrap(),
            &AgentVersion::try_from("0.1.0").unwrap(),
        )
        .await;

    refused(&result);
    let entry = the_entry(
        &setup.entries().await,
        &AuditAction::AGENT_CONNECT,
        AuditOutcome::Failure,
        agent.id,
    );
    assert_eq!(text(&entry, "reason").as_deref(), Some("revoked"));
    let row = setup.db.agents().find(agent.id).await.unwrap().unwrap();
    assert_eq!(row.capacity, None, "nothing recorded");
}

#[sqlx::test(migrations = false)]
async fn a_hello_after_the_agent_was_disabled_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let agent = setup.enroll(1, "agent-1").await;
    let admitted = setup.admit(&agent).await.unwrap();
    setup.disable(agent.id).await;

    let result = setup
        .connections
        .hello(
            &admitted,
            AgentCapacity::try_from(50_u32).unwrap(),
            &AgentVersion::try_from("0.1.0").unwrap(),
        )
        .await;

    refused(&result);
    let entries = setup.entries().await;
    assert_eq!(text(&entries[0], "reason").as_deref(), Some("disabled"));
}

#[sqlx::test(migrations = false)]
async fn a_hello_while_the_writer_is_held_is_busy_and_records_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        DatabaseOptions::default().with_acquire_timeout(Duration::from_millis(300)),
    )
    .await;
    let agent = setup.enroll(1, "agent-1").await;
    let admitted = setup.admit(&agent).await.unwrap();
    let held = setup.db.write().await.unwrap();

    let result = setup
        .connections
        .hello(
            &admitted,
            AgentCapacity::try_from(50_u32).unwrap(),
            &AgentVersion::try_from("0.1.0").unwrap(),
        )
        .await;
    drop(held);

    assert!(matches!(result, Err(AdmitError::Busy)), "{result:?}");
    assert_eq!(setup.entries().await, Vec::new());
}

#[sqlx::test(migrations = false)]
async fn a_hello_naming_another_agent_is_recorded_as_a_name_mismatch(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let agent = setup.enroll(1, "agent-1").await;
    let admitted = setup.admit(&agent).await.unwrap();

    setup.connections.name_refused(&admitted).await;

    let entry = the_entry(
        &setup.entries().await,
        &AuditAction::AGENT_CONNECT,
        AuditOutcome::Failure,
        agent.id,
    );
    assert_eq!(text(&entry, "reason").as_deref(), Some("name_mismatch"));
}

// --- The end ------------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn the_end_of_a_stream_records_its_reason_and_when_it_was_last_seen(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let agent = setup.enroll(1, "agent-1").await;
    let admitted = setup.admit(&agent).await.unwrap();
    let connected_at = at(T0) + TimeDelta::seconds(1);
    let last_seen = at(T0) + TimeDelta::minutes(5);

    setup
        .connections
        .ended(
            &admitted,
            Ended {
                reason: StreamEnd::HeartbeatTimeout,
                connected_at,
                last_seen,
            },
        )
        .await;

    let entry = the_entry(
        &setup.entries().await,
        &AuditAction::AGENT_DISCONNECT,
        AuditOutcome::Success,
        agent.id,
    );
    assert_eq!(text(&entry, "reason").as_deref(), Some("heartbeat_timeout"));
    assert_eq!(
        text(&entry, "connected_at").as_deref(),
        Some("2027-01-15T08:00:01.000Z")
    );
    let row = setup.db.agents().find(agent.id).await.unwrap().unwrap();
    assert_eq!(row.last_seen_at, Some(last_seen));
}

#[test]
fn every_end_has_its_stable_name() {
    let names = [
        StreamEnd::Closed,
        StreamEnd::HeartbeatTimeout,
        StreamEnd::StreamError,
        StreamEnd::ProtocolError,
        StreamEnd::TooSlow,
        StreamEnd::Disabled,
        StreamEnd::CertificateRevoked,
        StreamEnd::CertificateExpired,
        StreamEnd::ServerShutdown,
    ]
    .map(|end| end.to_string());

    assert_eq!(
        names,
        [
            "closed",
            "heartbeat_timeout",
            "stream_error",
            "protocol_error",
            "too_slow",
            "disabled",
            "certificate_revoked",
            "certificate_expired",
            "server_shutdown",
        ]
    );
}
