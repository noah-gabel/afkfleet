//! Tests for the certificates' renewal (Plan.md P13.6; ADR-0027): a
//! renewal signs the presented certificate's key again for its agent and
//! stores the new fingerprint beside the old one, which stays valid until
//! the agent's first hello with the new one; a certificate that stopped
//! admitting its agent renews nothing; and nothing is stored when the
//! signing is too slow, fails, or the database is busy.
//!
//! Each test gets a fresh database from `#[sqlx::test]` and opens it with
//! `Database::connect`. Time isn't paused: sqlx's worker threads would make
//! paused timeouts fire early (CLAUDE.md). The services' clock is a
//! `ManualClock`.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::net::IpAddr;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeDelta, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget, RecordedValue};
use fleet_core::id::{AgentId, RequestId};
use fleet_core::pki::CertFingerprint;
use fleet_core::system::RandomError;
use fleet_core::value::{AgentCapacity, AgentName, AgentVersion};
use fleet_server::app::agents::{AgentConnectionService, CertificateRenewalService};
use fleet_server::infra::pki::{
    AgentIssuer, CertificateAuthority, Ed25519Key, X509AgentNames, fingerprint,
};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::metrics::Metrics;
use fleet_server::ports::agent_connections::{
    AdmitError, Admitted, AgentConnections as _, Presented,
};
use fleet_server::ports::agents::{AgentCert, NewAgent, NewAgentCert};
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditRecord, NewAuditEntry};
use fleet_server::ports::pki::{AgentCertificates, IssueError, IssuedCertificate};
use fleet_server::ports::renewals::{CertificateRenewals as _, RenewError, Renewed};
use fleet_server::ports::store::Store as _;
use fleet_testkit::system::{ManualClock, SeededRandom};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject as _;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

const T0: &str = "2027-01-15T08:00:00Z";
const AGENT_IP: [u8; 4] = [192, 0, 2, 20];
/// The setup's own audit entries, which the checks leave out.
const SETUP: &str = "test.renewal_setup";
/// A deadline no test reaches.
const DEADLINE: Duration = Duration::from_secs(15);
/// A deadline the slow signer outlasts.
const SHORT_DEADLINE: Duration = Duration::from_millis(200);
/// What a signer held back waits at most.
const BOUND: Duration = Duration::from_secs(5);

fn at(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

fn peer() -> IpAddr {
    IpAddr::from(AGENT_IP)
}

/// The real CA, which waits for `release` before a renewal when it has one.
struct Signer {
    issuer: AgentIssuer,
    release: Option<Mutex<mpsc::Receiver<()>>>,
}

impl core::fmt::Debug for Signer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Signer")
    }
}

impl AgentCertificates for Signer {
    fn issue(
        &self,
        csr_der: &[u8],
        agent: AgentId,
        now: DateTime<Utc>,
    ) -> Result<IssuedCertificate, IssueError> {
        self.issuer.issue(csr_der, agent, now)
    }

    fn renew(
        &self,
        presented_der: &[u8],
        agent: AgentId,
        now: DateTime<Utc>,
    ) -> Result<IssuedCertificate, IssueError> {
        if let Some(release) = &self.release {
            let _ = release.lock().unwrap().recv_timeout(BOUND);
        }
        self.issuer.renew(presented_der, agent, now)
    }

    fn ca_fingerprint(&self) -> CertFingerprint {
        self.issuer.ca_fingerprint()
    }
}

struct Setup {
    db: Database,
    clock: ManualClock,
    connections: AgentConnectionService,
    renewals: CertificateRenewalService,
    metrics: Metrics,
    /// A second handle on the CA, for the first certificates.
    ca: CertificateAuthority,
    random: SeededRandom,
    /// The signer's random source.
    signing: Arc<SeededRandom>,
    /// Lets a held signer go on.
    release: Option<mpsc::Sender<()>>,
}

/// An agent's certificate as it connects with it.
#[derive(Clone)]
struct Held {
    agent: AgentId,
    der: Vec<u8>,
    fingerprint: CertFingerprint,
}

async fn setup_with(
    options: SqliteConnectOptions,
    db_options: DatabaseOptions,
    held: bool,
) -> Setup {
    let db = Database::connect(options, db_options).await.unwrap();
    let clock = ManualClock::new(at(T0));
    let random = SeededRandom::new(21);
    let (ca, _) = CertificateAuthority::create(&random, at(T0)).unwrap();
    let again = CertificateAuthority::from_parts(
        ca.cert_der().clone(),
        Ed25519Key::from_pem(ca.key().to_pem().as_bytes()).unwrap(),
    )
    .unwrap();
    let signing = Arc::new(SeededRandom::new(22));
    let (release, waiting) = mpsc::channel();
    let signer = Signer {
        issuer: AgentIssuer::new(again, signing.clone(), TimeDelta::days(60)),
        release: held.then(|| Mutex::new(waiting)),
    };
    let metrics = Metrics::prometheus();
    let connections = AgentConnectionService::new(
        Arc::new(clock.clone()),
        Arc::new(db.clone()),
        Arc::new(X509AgentNames),
    )
    .with_metrics(metrics.recording().clone());
    let renewals = CertificateRenewalService::new(
        Arc::new(clock.clone()),
        Arc::new(db.clone()),
        Arc::new(signer),
        connections.clone(),
    )
    .with_metrics(metrics.recording().clone());
    Setup {
        db,
        clock,
        connections,
        renewals,
        metrics,
        ca,
        random,
        signing,
        release: held.then_some(release),
    }
}

async fn setup(options: SqliteConnectOptions) -> Setup {
    setup_with(options, DatabaseOptions::default(), false).await
}

fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from(SETUP).unwrap(),
        AuditOutcome::Success,
    )
}

impl Setup {
    /// Enrolls agent `n` named `name` with a 60-day certificate from `T0`.
    async fn enroll(&self, n: u8, name: &str) -> (Held, Ed25519Key) {
        let id = AgentId::new_v7(at(T0), [n; 10]).unwrap();
        let key = Ed25519Key::generate(&self.random).unwrap();
        let issued = self
            .ca
            .issue_agent(&key, id, TimeDelta::days(60), &self.random, at(T0))
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
        (
            Held {
                agent: id,
                der: issued.der.to_vec(),
                fingerprint: issued.fingerprint,
            },
            key,
        )
    }

    async fn admit(&self, held: &Held) -> Result<Admitted, AdmitError> {
        self.connections
            .admit(Presented {
                request: RequestId::new_v7(at(T0), [1; 10]).unwrap(),
                fingerprint: held.fingerprint,
                certificate: held.der.clone(),
                peer: Some(peer()),
            })
            .await
    }

    /// Admits `held` and renews it.
    async fn renew(&self, held: &Held) -> Result<Renewed, RenewError> {
        let admitted = self.admit(held).await.unwrap();
        self.renewals
            .renew(&admitted, held.der.clone(), DEADLINE)
            .await
    }

    /// Admits `held` and says hello with it.
    async fn hello(&self, held: &Held) -> Result<(), AdmitError> {
        let admitted = self.admit(held).await?;
        self.connections
            .hello(
                &admitted,
                AgentCapacity::try_from(50_u32).unwrap(),
                &AgentVersion::try_from("0.1.2").unwrap(),
                &[],
            )
            .await
    }

    async fn certs(&self, agent: AgentId) -> Vec<AgentCert> {
        self.db.agents().certs(agent).await.unwrap()
    }

    /// The entries of `action`, oldest first.
    async fn entries_of(&self, action: &AuditAction) -> Vec<AuditRecord> {
        let mut entries: Vec<_> = self
            .db
            .audit()
            .list(AuditPage {
                before: None,
                limit: AuditLimit::new(100).unwrap(),
            })
            .await
            .unwrap()
            .into_iter()
            .filter(|record| &record.action == action)
            .collect();
        entries.reverse();
        entries
    }

    fn renewals_counted(&self) -> String {
        self.metrics
            .render()
            .text()
            .lines()
            .find(|line| line.starts_with("afkfleet_agent_certificate_renewals_total"))
            .unwrap_or_default()
            .to_owned()
    }
}

/// `renewed` as the agent connects with it next.
fn held(agent: AgentId, renewed: &Renewed) -> Held {
    let der = CertificateDer::from_pem_slice(renewed.certificate_pem.as_bytes())
        .unwrap()
        .to_vec();
    assert_eq!(fingerprint(&der), renewed.fingerprint);
    Held {
        agent,
        der,
        fingerprint: renewed.fingerprint,
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

/// What a certificate's row says: whether it's revoked, and its times.
fn rows(certs: &[AgentCert]) -> Vec<(CertFingerprint, Option<DateTime<Utc>>)> {
    certs
        .iter()
        .map(|cert| (cert.fingerprint, cert.revoked_at))
        .collect()
}

#[sqlx::test(migrations = false)]
async fn a_renewal_stores_the_new_certificate_beside_the_one_that_asked_and_audits_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (first, key) = setup.enroll(1, "agent-1").await;
    setup.clock.advance(Duration::from_hours(40 * 24));
    let now = at(T0) + TimeDelta::days(40);

    let renewed = setup.renew(&first).await.unwrap();

    assert_eq!(renewed.not_after, now + TimeDelta::days(60));
    assert_ne!(renewed.fingerprint, first.fingerprint);
    // The same key, the same agent: the agent's own check passes on it.
    let next = held(first.agent, &renewed);
    let admitted = setup.admit(&next).await.unwrap();
    assert_eq!(admitted.agent, first.agent);
    assert_eq!(admitted.not_after, renewed.not_after);
    let (_, parsed) = x509_parser::parse_x509_certificate(&next.der).unwrap();
    assert_eq!(
        parsed.public_key().subject_public_key.data.as_ref(),
        key.public_key_raw()
    );
    // Both rows, neither revoked; the new one stored now.
    let certs = setup.certs(first.agent).await;
    assert_eq!(
        rows(&certs),
        [(first.fingerprint, None), (renewed.fingerprint, None)]
    );
    assert_eq!(certs[1].created_at, now);
    assert_eq!(certs[1].not_after, renewed.not_after);
    let entries = setup.entries_of(&AuditAction::AGENT_CERT_RENEW).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    let entry = &entries[0];
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Agent(first.agent)))
    );
    assert_eq!(entry.actor, None);
    assert_eq!(entry.ip, Some(peer()));
    assert_eq!(
        text(entry, "fingerprint"),
        Some(renewed.fingerprint.to_string())
    );
    assert_eq!(text(entry, "replaces"), Some(first.fingerprint.to_string()));
    assert!(text(entry, "not_after").is_some());
    assert_eq!(text(entry, "certificates_revoked").as_deref(), Some("0"));
    assert_eq!(
        setup.renewals_counted(),
        "afkfleet_agent_certificate_renewals_total 1"
    );
}

#[sqlx::test(migrations = false)]
async fn the_first_hello_with_the_renewed_certificate_revokes_the_old_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (first, _) = setup.enroll(1, "agent-1").await;
    setup.clock.advance(Duration::from_hours(40 * 24));
    let renewed = setup.renew(&first).await.unwrap();
    let next = held(first.agent, &renewed);
    // Until then the old one still connects: a lost answer locks nobody out.
    setup.hello(&first).await.unwrap();
    assert_eq!(
        rows(&setup.certs(first.agent).await),
        [(first.fingerprint, None), (renewed.fingerprint, None)],
        "a hello with the old certificate leaves the newer one"
    );
    setup.clock.advance(Duration::from_secs(5));
    let switched = at(T0) + TimeDelta::days(40) + TimeDelta::seconds(5);

    setup.hello(&next).await.unwrap();

    assert_eq!(
        rows(&setup.certs(first.agent).await),
        [
            (first.fingerprint, Some(switched)),
            (renewed.fingerprint, None)
        ]
    );
    let connects = setup.entries_of(&AuditAction::AGENT_CONNECT).await;
    assert_eq!(connects.len(), 2, "{connects:?}");
    assert_eq!(text(&connects[0], "certificates_revoked"), None);
    assert_eq!(
        text(&connects[1], "certificates_revoked").as_deref(),
        Some("1")
    );
    // A process still holding the old certificate is refused as revoked.
    let stale = setup.admit(&first).await;
    assert!(matches!(stale, Err(AdmitError::Refused)), "{stale:?}");
    let refusals: Vec<_> = setup
        .entries_of(&AuditAction::AGENT_CONNECT)
        .await
        .into_iter()
        .filter(|entry| entry.outcome == AuditOutcome::Failure)
        .collect();
    assert_eq!(refusals.len(), 1);
    assert_eq!(text(&refusals[0], "reason").as_deref(), Some("revoked"));
    // And a later hello with the new one has nothing left to revoke.
    setup.hello(&next).await.unwrap();
    let connects = setup.entries_of(&AuditAction::AGENT_CONNECT).await;
    assert_eq!(text(connects.last().unwrap(), "certificates_revoked"), None);
}

#[sqlx::test(migrations = false)]
async fn a_hello_with_an_agents_only_certificate_revokes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (first, _) = setup.enroll(1, "agent-1").await;
    let (other, _) = setup.enroll(2, "agent-2").await;

    setup.hello(&first).await.unwrap();

    assert_eq!(
        rows(&setup.certs(first.agent).await),
        [(first.fingerprint, None)]
    );
    assert_eq!(
        rows(&setup.certs(other.agent).await),
        [(other.fingerprint, None)],
        "another agent's certificate is never touched"
    );
    let connects = setup.entries_of(&AuditAction::AGENT_CONNECT).await;
    assert_eq!(text(&connects[0], "certificates_revoked"), None);
}

#[sqlx::test(migrations = false)]
async fn a_renewal_asked_again_revokes_the_answer_the_agent_never_used(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (first, _) = setup.enroll(1, "agent-1").await;
    setup.clock.advance(Duration::from_hours(40 * 24));
    let lost = setup.renew(&first).await.unwrap();
    setup.clock.advance(Duration::from_hours(1));
    let again = at(T0) + TimeDelta::days(40) + TimeDelta::hours(1);

    let renewed = setup.renew(&first).await.unwrap();

    assert_eq!(
        rows(&setup.certs(first.agent).await),
        [
            (first.fingerprint, None),
            (lost.fingerprint, Some(again)),
            (renewed.fingerprint, None)
        ]
    );
    let entries = setup.entries_of(&AuditAction::AGENT_CERT_RENEW).await;
    assert_eq!(
        text(&entries[1], "certificates_revoked").as_deref(),
        Some("1")
    );
    assert_eq!(
        setup.renewals_counted(),
        "afkfleet_agent_certificate_renewals_total 2"
    );
}

#[sqlx::test(migrations = false)]
async fn a_renewed_certificate_renews_again(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (first, _) = setup.enroll(1, "agent-1").await;
    setup.clock.advance(Duration::from_hours(40 * 24));
    let second = held(first.agent, &setup.renew(&first).await.unwrap());
    setup.hello(&second).await.unwrap();
    setup.clock.advance(Duration::from_hours(40 * 24));

    let third = setup.renew(&second).await.unwrap();

    assert_eq!(
        third.not_after,
        at(T0) + TimeDelta::days(80) + TimeDelta::days(60)
    );
    let certs = setup.certs(first.agent).await;
    assert_eq!(certs.len(), 3);
    assert!(certs[0].revoked_at.is_some());
    assert_eq!(certs[1].revoked_at, None);
    assert_eq!(certs[2].revoked_at, None);
}

#[sqlx::test(migrations = false)]
async fn a_certificate_that_stopped_admitting_its_agent_renews_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    // Each agent is admitted first, as the gRPC port admits its call, and
    // loses its permission before the renewal's transaction.
    let setup = setup(options).await;
    let (revoked, _) = setup.enroll(1, "revoked").await;
    let (disabled, _) = setup.enroll(2, "disabled").await;
    let (expired, _) = setup.enroll(3, "expired").await;
    let admitted = [
        setup.admit(&revoked).await.unwrap(),
        setup.admit(&disabled).await.unwrap(),
        setup.admit(&expired).await.unwrap(),
    ];
    let mut tx = setup.db.write().await.unwrap();
    tx.agents()
        .revoke_certs(revoked.agent, at(T0))
        .await
        .unwrap();
    tx.agents().disable(disabled.agent).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();

    let first = setup
        .renewals
        .renew(&admitted[0], revoked.der.clone(), DEADLINE)
        .await;
    let second = setup
        .renewals
        .renew(&admitted[1], disabled.der.clone(), DEADLINE)
        .await;
    setup.clock.advance(Duration::from_hours(60 * 24));
    let third = setup
        .renewals
        .renew(&admitted[2], expired.der.clone(), DEADLINE)
        .await;

    for (result, held, reason) in [
        (first, &revoked, "revoked"),
        (second, &disabled, "disabled"),
        (third, &expired, "expired"),
    ] {
        assert!(
            matches!(result, Err(RenewError::Refused)),
            "{reason}: {result:?}"
        );
        assert_eq!(setup.certs(held.agent).await.len(), 1, "{reason}");
        let refusals: Vec<_> = setup
            .entries_of(&AuditAction::AGENT_CONNECT)
            .await
            .into_iter()
            .filter(|entry| {
                entry.target == Some(RecordedTarget::Known(AuditTarget::Agent(held.agent)))
            })
            .collect();
        assert_eq!(refusals.len(), 1, "{reason}: {refusals:?}");
        assert_eq!(refusals[0].outcome, AuditOutcome::Failure);
        assert_eq!(text(&refusals[0], "reason").as_deref(), Some(reason));
        assert_eq!(
            text(&refusals[0], "fingerprint"),
            Some(held.fingerprint.to_string())
        );
    }
    assert_eq!(
        setup.entries_of(&AuditAction::AGENT_CERT_RENEW).await,
        Vec::new()
    );
    assert_eq!(
        setup.renewals_counted(),
        "afkfleet_agent_certificate_renewals_total 0"
    );
}

#[sqlx::test(migrations = false)]
async fn a_renewal_signed_too_slowly_stores_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(options, DatabaseOptions::default(), true).await;
    let (first, _) = setup.enroll(1, "agent-1").await;
    let admitted = setup.admit(&first).await.unwrap();

    let result = setup
        .renewals
        .renew(&admitted, first.der.clone(), SHORT_DEADLINE)
        .await;
    // The signer goes on and ends; what it signed is dropped.
    setup.release.as_ref().unwrap().send(()).unwrap();

    assert!(matches!(result, Err(RenewError::TooSlow)), "{result:?}");
    assert_eq!(
        rows(&setup.certs(first.agent).await),
        [(first.fingerprint, None)]
    );
    assert_eq!(
        setup.entries_of(&AuditAction::AGENT_CERT_RENEW).await,
        Vec::new()
    );
}

#[sqlx::test(migrations = false)]
async fn a_renewal_the_ca_cant_sign_is_a_failure_and_stores_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (first, _) = setup.enroll(1, "agent-1").await;
    let admitted = setup.admit(&first).await.unwrap();
    setup.signing.fail_next(RandomError::Unavailable);

    let failed = setup
        .renewals
        .renew(&admitted, first.der.clone(), DEADLINE)
        .await;
    let unreadable = setup
        .renewals
        .renew(&admitted, b"not a certificate".to_vec(), DEADLINE)
        .await;

    assert!(matches!(failed, Err(RenewError::Internal(_))), "{failed:?}");
    assert!(
        matches!(unreadable, Err(RenewError::Internal(_))),
        "{unreadable:?}"
    );
    assert_eq!(setup.certs(first.agent).await.len(), 1);
    assert_eq!(
        setup.entries_of(&AuditAction::AGENT_CERT_RENEW).await,
        Vec::new()
    );
}

#[sqlx::test(migrations = false)]
async fn a_renewal_while_the_writer_is_held_is_busy_and_stores_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        DatabaseOptions::default().with_acquire_timeout(Duration::from_millis(300)),
        false,
    )
    .await;
    let (first, _) = setup.enroll(1, "agent-1").await;
    let admitted = setup.admit(&first).await.unwrap();
    let writer = setup.db.write().await.unwrap();

    let result = setup
        .renewals
        .renew(&admitted, first.der.clone(), DEADLINE)
        .await;
    drop(writer);

    assert!(matches!(result, Err(RenewError::Busy)), "{result:?}");
    assert_eq!(setup.certs(first.agent).await.len(), 1);
}

#[test]
fn a_renewed_certificates_debug_never_prints_its_text() {
    let renewed = Renewed {
        fingerprint: CertFingerprint::from_digest([0xCD; 32]),
        certificate_pem: "-----BEGIN CERTIFICATE-----\nMIIB\n".to_owned(),
        not_after: at(T0),
    };

    let shown = format!("{renewed:?}");

    assert!(shown.contains("CD:CD"), "{shown}");
    assert!(!shown.contains("MIIB"), "{shown}");
}
