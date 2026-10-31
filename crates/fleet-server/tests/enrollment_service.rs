//! Tests for enrolling agents (Plan.md P10.3; ADR-0019, S15–S17, S21;
//! ADR-0020, S88): the tokens' creation on the server's host, and the
//! enrollment that trades one token for one agent's certificate, in S21's
//! order, single-use even when two enrollments race, with every refusal's
//! answer and audit entry.
//!
//! Each test gets a fresh database from `#[sqlx::test]`. The CA is a real
//! one made from a seed, behind a signer that counts its calls and can run a
//! change "meanwhile": between the checks on the read pool and the write
//! transaction, on the blocking thread the service signs on. Time isn't
//! paused: sqlx's worker threads would make paused timeouts fire early
//! (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeDelta, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget, RecordedValue};
use fleet_core::enrollment::{EnrollmentBundle, MAX_PENDING_TOKENS};
use fleet_core::id::AgentId;
use fleet_core::pki::{CertFingerprint, ed25519_pkcs8};
use fleet_core::system::Clock as _;
use fleet_core::value::AgentName;
use fleet_proto::enrollment::{EnrollRequest, SERVER_DEADLINE};
use fleet_server::app::enrollment::{
    CreateTokenError, EnrollError, EnrollmentService, EnrollmentTokenService, TokenProblem,
};
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::infra::pki::{AgentIssuer, CertificateAuthority, fingerprint};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::agents::NewAgent;
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditRecord, NewAuditEntry};
use fleet_server::ports::pki::{AgentCertificates, CsrRefusal, IssueError, IssuedCertificate};
use fleet_server::ports::store::Store as _;
use fleet_server::ports::tokens::{TokenKind, Tokens as _};
use fleet_testkit::system::{ManualClock, SeededRandom};
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
use rustls::pki_types::PrivatePkcs8KeyDer;
use secrecy::{ExposeSecret as _, SecretString};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// When everything starts.
const T0: &str = "2027-01-15T08:00:00Z";
/// The agent's address.
const AGENT_IP: [u8; 4] = [192, 0, 2, 20];
/// How long a test waits to see that an enrollment is still running.
const STILL_RUNNING: Duration = Duration::from_secs(1);
/// The longest a blocked signer waits to be released.
const BOUND: Duration = Duration::from_secs(5);
/// A deadline the tests outlast.
const SHORT_DEADLINE: Duration = Duration::from_millis(200);

fn at(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

/// What the signer runs "meanwhile", on the service's blocking thread.
type Hook = Box<dyn FnOnce(&Database) + Send>;

/// The real CA behind a counter and an optional hook.
struct Signer {
    issuer: AgentIssuer,
    calls: AtomicUsize,
    meanwhile: Mutex<Option<Hook>>,
    db: Database,
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(hook) = self.meanwhile.lock().unwrap().take() {
            hook(&self.db);
        }
        self.issuer.issue(csr_der, agent, now)
    }

    fn ca_fingerprint(&self) -> CertFingerprint {
        self.issuer.ca_fingerprint()
    }
}

struct Setup {
    db: Database,
    clock: ManualClock,
    tokens: EnrollmentTokenService,
    enrollment: EnrollmentService,
    signer: Arc<Signer>,
    ca: CertFingerprint,
}

async fn setup(options: SqliteConnectOptions) -> Setup {
    let db = Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap();
    let clock = ManualClock::new(at(T0));
    let random = SeededRandom::new(11);
    let (ca, _) = CertificateAuthority::create(&SeededRandom::new(3), at(T0)).unwrap();
    let ca_fingerprint = ca.fingerprint();
    let signer = Arc::new(Signer {
        issuer: AgentIssuer::new(ca, Arc::new(random.clone()), TimeDelta::days(60)),
        calls: AtomicUsize::new(0),
        meanwhile: Mutex::new(None),
        db: db.clone(),
    });
    let token_port = Arc::new(Sha256Tokens::new(Arc::new(random.clone())));
    let tokens = EnrollmentTokenService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(db.clone()),
        token_port.clone(),
        ca_fingerprint,
    );
    let enrollment = EnrollmentService::new(
        Arc::new(clock.clone()),
        Arc::new(random),
        Arc::new(db.clone()),
        token_port,
        Arc::clone(&signer) as _,
    );
    Setup {
        db,
        clock,
        tokens,
        enrollment,
        signer,
        ca: ca_fingerprint,
    }
}

fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from("test.enrollment_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// An agent's key from `seed`, as the agent makes one.
fn agent_key(seed: u8) -> KeyPair {
    let der = ed25519_pkcs8(&[seed; 32]);
    KeyPair::from_pkcs8_der_and_sign_algo(
        &PrivatePkcs8KeyDer::from(der.expose_secret().as_slice()),
        &PKCS_ED25519,
    )
    .unwrap()
}

fn csr(key: &KeyPair) -> Vec<u8> {
    CertificateParams::default()
        .serialize_request(key)
        .unwrap()
        .der()
        .to_vec()
}

fn request(token: &SecretString, name: &str, seed: u8) -> EnrollRequest {
    EnrollRequest {
        token: token.clone(),
        name: AgentName::try_from(name).unwrap(),
        csr_der: csr(&agent_key(seed)),
    }
}

async fn host_token(setup: &Setup) -> SecretString {
    let created = setup.tokens.create().await.unwrap();
    created.bundle.token().clone()
}

async fn enroll(
    setup: &Setup,
    token: &SecretString,
    name: &str,
) -> Result<fleet_server::app::enrollment::Enrolled, EnrollError> {
    setup
        .enrollment
        .enroll(
            request(token, name, 7),
            IpAddr::from(AGENT_IP),
            SERVER_DEADLINE,
        )
        .await
}

async fn entries(db: &Database) -> Vec<AuditRecord> {
    db.audit()
        .list(AuditPage {
            before: None,
            limit: AuditLimit::new(100).unwrap(),
        })
        .await
        .unwrap()
        .into_iter()
        .filter(|record| record.action.as_str() != "test.enrollment_setup")
        .collect()
}

fn of<'a>(entries: &'a [AuditRecord], action: &AuditAction) -> Vec<&'a AuditRecord> {
    entries
        .iter()
        .filter(|record| record.action == *action)
        .collect()
}

fn text(record: &AuditRecord, key: &str) -> Option<String> {
    record
        .metadata
        .entries()
        .iter()
        .find(|(found, _)| found == key)
        .map(|(_, value)| match value {
            RecordedValue::Text(text) => text.clone(),
            other => format!("{other:?}"),
        })
}

fn token_problem(
    result: Result<fleet_server::app::enrollment::Enrolled, EnrollError>,
) -> TokenProblem {
    match result {
        Err(EnrollError::Token(problem)) => problem,
        other => panic!("expected a token refusal, got {other:?}"),
    }
}

// --- Creating a token ----------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_tokens_bundle_pins_the_ca_and_is_stored_only_as_its_hash(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;

    let created = setup.tokens.create().await.unwrap();

    assert_eq!(created.bundle.ca_fingerprint(), setup.ca);
    assert_eq!(created.expires_at, at(T0) + TimeDelta::minutes(15));
    let token = created.bundle.token().expose_secret().to_owned();
    assert!(token.starts_with("afk_et_"), "a token");
    let again = EnrollmentBundle::try_from(created.bundle.to_text().expose_secret()).unwrap();
    assert_eq!(again.ca_fingerprint(), setup.ca);
    let hash = Sha256Tokens::new(Arc::new(SeededRandom::new(1)))
        .hash_presented(TokenKind::Enrollment, &token)
        .unwrap();
    let stored = setup
        .db
        .enrollment_tokens()
        .find_by_hash(&hash)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.id, created.id);
    let all = entries(&setup.db).await;
    let created_entries = of(&all, &AuditAction::ENROLLMENT_TOKEN_CREATE);
    assert_eq!(created_entries.len(), 1);
    let entry = created_entries[0];
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, None);
    assert_eq!(entry.ip, None);
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::EnrollmentToken(
            created.id
        )))
    );
    assert_eq!(text(entry, "via").as_deref(), Some("cli"));
    assert!(text(entry, "expires_at").is_some());
    let shown = format!("{all:?}");
    assert!(
        !shown.contains(&token[7..]),
        "the token reached the audit log"
    );
}

#[sqlx::test(migrations = false)]
async fn the_twenty_first_pending_token_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    for _ in 0..MAX_PENDING_TOKENS {
        setup.tokens.create().await.unwrap();
    }

    let refused = setup.tokens.create().await;
    setup.clock.advance(core::time::Duration::from_mins(15));
    let after_expiry = setup.tokens.create().await;

    assert!(
        matches!(refused, Err(CreateTokenError::CapReached)),
        "{refused:?}"
    );
    after_expiry.unwrap();
}

// --- Enrolling ---------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_token_enrolls_one_agent_recorded_twice(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let created = setup.tokens.create().await.unwrap();
    setup.clock.advance(core::time::Duration::from_secs(60));

    let enrolled = enroll(&setup, created.bundle.token(), "agent-1")
        .await
        .unwrap();

    assert_eq!(enrolled.name.as_str(), "agent-1");
    assert_eq!(
        enrolled.not_after,
        at(T0) + TimeDelta::minutes(1) + TimeDelta::days(60)
    );
    let agent = setup
        .db
        .agents()
        .find(enrolled.agent)
        .await
        .unwrap()
        .unwrap();
    assert!(agent.enabled);
    assert_eq!(agent.name.as_str(), "agent-1");
    let certs = setup.db.agents().certs(enrolled.agent).await.unwrap();
    assert_eq!(certs.len(), 1);
    assert_eq!(certs[0].fingerprint, fingerprint(&enrolled.certificate_der));
    assert_eq!(certs[0].not_after, enrolled.not_after);
    let all = entries(&setup.db).await;
    let enrolls = of(&all, &AuditAction::AGENT_ENROLL);
    assert_eq!(enrolls.len(), 1);
    let enroll_entry = enrolls[0];
    assert_eq!(enroll_entry.outcome, AuditOutcome::Success);
    assert_eq!(enroll_entry.actor, None);
    assert_eq!(enroll_entry.ip, Some(IpAddr::from(AGENT_IP)));
    assert_eq!(
        enroll_entry.target,
        Some(RecordedTarget::Known(AuditTarget::Agent(enrolled.agent)))
    );
    assert_eq!(
        text(enroll_entry, "fingerprint"),
        Some(certs[0].fingerprint.to_string())
    );
    assert_eq!(text(enroll_entry, "name").as_deref(), Some("agent-1"));
    assert_eq!(
        text(enroll_entry, "enrollment"),
        Some(created.id.to_string())
    );
    let uses = of(&all, &AuditAction::ENROLLMENT_TOKEN_USE);
    assert_eq!(uses.len(), 1);
    assert_eq!(
        uses[0].target,
        Some(RecordedTarget::Known(AuditTarget::EnrollmentToken(
            created.id
        )))
    );
    assert_eq!(text(uses[0], "agent"), Some(enrolled.agent.to_string()));
    assert_eq!(setup.signer.calls.load(Ordering::SeqCst), 1);
}

#[sqlx::test(migrations = false)]
async fn two_enrollments_with_one_token_give_one_certificate(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let token = host_token(&setup).await;

    let (one, two) = tokio::join!(
        enroll(&setup, &token, "agent-1"),
        enroll(&setup, &token, "agent-2"),
    );

    let (winner, loser) = match (one, two) {
        (Ok(winner), Err(loser)) | (Err(loser), Ok(winner)) => (winner, loser),
        other => panic!("expected one enrollment, got {other:?}"),
    };
    assert!(matches!(loser, EnrollError::Token(_)), "{loser:?}");
    assert_eq!(
        setup.db.agents().certs(winner.agent).await.unwrap().len(),
        1
    );
    let all = entries(&setup.db).await;
    assert_eq!(
        of(&all, &AuditAction::AGENT_ENROLL)
            .iter()
            .filter(|entry| entry.outcome == AuditOutcome::Success)
            .count(),
        1
    );
    assert_eq!(of(&all, &AuditAction::ENROLLMENT_TOKEN_USE).len(), 1);
}

#[sqlx::test(migrations = false)]
async fn every_token_problem_is_one_answer_and_never_reaches_the_signer(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let used = host_token(&setup).await;
    enroll(&setup, &used, "agent-1").await.unwrap();
    let expired = host_token(&setup).await;
    setup.clock.advance(core::time::Duration::from_mins(15));
    let calls = setup.signer.calls.load(Ordering::SeqCst);

    let malformed = enroll(&setup, &SecretString::from("afk_et_short"), "agent-2").await;
    let unknown = enroll(
        &setup,
        &SecretString::from("afk_et_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopq"),
        "agent-3",
    )
    .await;
    let expired = enroll(&setup, &expired, "agent-4").await;
    let used_again = enroll(&setup, &used, "agent-5").await;

    assert_eq!(token_problem(malformed), TokenProblem::Malformed);
    assert_eq!(token_problem(unknown), TokenProblem::Unknown);
    assert_eq!(token_problem(expired), TokenProblem::Expired);
    assert!(matches!(
        token_problem(used_again),
        TokenProblem::Used { .. }
    ));
    assert_eq!(setup.signer.calls.load(Ordering::SeqCst), calls);
}

#[sqlx::test(migrations = false)]
async fn a_real_unusable_token_is_recorded_ten_times_and_an_unknown_never(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let used = host_token(&setup).await;
    let first = enroll(&setup, &used, "agent-1").await.unwrap();

    for _ in 0..12 {
        enroll(&setup, &used, "agent-2").await.unwrap_err();
    }
    for _ in 0..3 {
        enroll(
            &setup,
            &SecretString::from("afk_et_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopq"),
            "agent-3",
        )
        .await
        .unwrap_err();
    }

    let all = entries(&setup.db).await;
    let failures: Vec<&AuditRecord> = of(&all, &AuditAction::AGENT_ENROLL)
        .into_iter()
        .filter(|entry| entry.outcome == AuditOutcome::Failure)
        .collect();
    assert_eq!(failures.len(), 10);
    for failure in failures {
        assert_eq!(text(failure, "reason").as_deref(), Some("used"));
        assert_eq!(text(failure, "agent"), Some(first.agent.to_string()));
        assert_eq!(failure.actor, None);
        assert_eq!(failure.ip, Some(IpAddr::from(AGENT_IP)));
        assert!(matches!(
            failure.target,
            Some(RecordedTarget::Known(AuditTarget::EnrollmentToken(_)))
        ));
    }
}

#[sqlx::test(migrations = false)]
async fn an_expired_token_is_recorded_with_its_reason(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let token = host_token(&setup).await;
    setup.clock.advance(core::time::Duration::from_mins(15));

    enroll(&setup, &token, "agent-1").await.unwrap_err();

    let all = entries(&setup.db).await;
    let failures = of(&all, &AuditAction::AGENT_ENROLL);
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].outcome, AuditOutcome::Failure);
    assert_eq!(text(failures[0], "reason").as_deref(), Some("expired"));
    assert_eq!(text(failures[0], "agent"), None);
}

#[sqlx::test(migrations = false)]
async fn a_taken_name_is_refused_before_signing_and_the_token_stays_pending(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let first = host_token(&setup).await;
    enroll(&setup, &first, "agent-1").await.unwrap();
    let token = host_token(&setup).await;
    let calls = setup.signer.calls.load(Ordering::SeqCst);

    let taken = enroll(&setup, &token, "AGENT-1").await;
    let other_name = enroll(&setup, &token, "agent-2").await;

    assert!(matches!(taken, Err(EnrollError::NameTaken)), "{taken:?}");
    other_name.unwrap();
    assert_eq!(setup.signer.calls.load(Ordering::SeqCst), calls + 1);
}

#[sqlx::test(migrations = false)]
async fn a_refused_csr_leaves_the_token_pending_and_unrecorded(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let token = host_token(&setup).await;
    let bad = EnrollRequest {
        token: token.clone(),
        name: AgentName::try_from("agent-1").unwrap(),
        csr_der: b"not a csr".to_vec(),
    };

    let refused = setup
        .enrollment
        .enroll(bad, IpAddr::from(AGENT_IP), SERVER_DEADLINE)
        .await;
    let then = enroll(&setup, &token, "agent-1").await;

    assert!(
        matches!(refused, Err(EnrollError::Csr(CsrRefusal::Unparsable))),
        "{refused:?}"
    );
    then.unwrap();
    assert!(
        of(&entries(&setup.db).await, &AuditAction::AGENT_ENROLL)
            .iter()
            .all(|entry| entry.outcome == AuditOutcome::Success)
    );
}

#[sqlx::test(migrations = false)]
async fn a_token_used_meanwhile_is_refused_and_its_certificate_never_stored(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let token = host_token(&setup).await;
    let other = AgentId::new_v7(at(T0), [9; 10]).unwrap();
    let hash = Sha256Tokens::new(Arc::new(SeededRandom::new(1)))
        .hash_presented(TokenKind::Enrollment, token.expose_secret())
        .unwrap();
    let id = setup
        .db
        .enrollment_tokens()
        .find_by_hash(&hash)
        .await
        .unwrap()
        .unwrap()
        .id;
    *setup.signer.meanwhile.lock().unwrap() = Some(Box::new(move |db: &Database| {
        let db = db.clone();
        tokio::runtime::Handle::current().block_on(async move {
            let mut tx = db.write().await.unwrap();
            tx.agents()
                .insert(&NewAgent {
                    id: other,
                    name: AgentName::try_from("someone-else").unwrap(),
                    created_at: at(T0),
                })
                .await
                .unwrap();
            assert!(
                tx.enrollment_tokens()
                    .use_token(id, other, at(T0))
                    .await
                    .unwrap()
            );
            tx.commit(setup_entry()).await.unwrap();
        });
    }));

    let refused = enroll(&setup, &token, "agent-1").await;

    assert_eq!(token_problem(refused), TokenProblem::Raced);
    let all = entries(&setup.db).await;
    assert_eq!(of(&all, &AuditAction::ENROLLMENT_TOKEN_USE).len(), 0);
    assert!(
        of(&all, &AuditAction::AGENT_ENROLL)
            .iter()
            .all(|entry| entry.outcome == AuditOutcome::Failure)
    );
    assert!(
        !setup
            .db
            .agents()
            .name_taken(&AgentName::try_from("agent-1").unwrap())
            .await
            .unwrap()
    );
}

#[sqlx::test(migrations = false)]
async fn a_name_taken_meanwhile_is_refused_and_the_token_stays_pending(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let token = host_token(&setup).await;
    *setup.signer.meanwhile.lock().unwrap() = Some(Box::new(|db: &Database| {
        let db = db.clone();
        tokio::runtime::Handle::current().block_on(async move {
            let mut tx = db.write().await.unwrap();
            tx.agents()
                .insert(&NewAgent {
                    id: AgentId::new_v7(at(T0), [9; 10]).unwrap(),
                    name: AgentName::try_from("agent-1").unwrap(),
                    created_at: at(T0),
                })
                .await
                .unwrap();
            tx.commit(setup_entry()).await.unwrap();
        });
    }));

    let refused = enroll(&setup, &token, "agent-1").await;
    let then = enroll(&setup, &token, "agent-2").await;

    assert!(
        matches!(refused, Err(EnrollError::NameTaken)),
        "{refused:?}"
    );
    then.unwrap();
}

#[sqlx::test(migrations = false)]
async fn the_clock_reads_once_per_enrollment_for_its_records(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let token = host_token(&setup).await;
    let now = setup.clock.now();

    let enrolled = enroll(&setup, &token, "agent-1").await.unwrap();

    let agent = setup
        .db
        .agents()
        .find(enrolled.agent)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(agent.created_at, now);
    assert_eq!(
        setup.db.agents().certs(enrolled.agent).await.unwrap()[0].created_at,
        now
    );
}

// --- The deadline --------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn signing_past_the_deadline_is_too_slow_and_leaves_the_token_pending(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let token = host_token(&setup).await;
    let (release, released) = std::sync::mpsc::channel::<()>();
    *setup.signer.meanwhile.lock().unwrap() = Some(Box::new(move |_: &Database| {
        let _ = released.recv_timeout(BOUND);
    }));

    let refused = setup
        .enrollment
        .enroll(
            request(&token, "agent-1", 7),
            IpAddr::from(AGENT_IP),
            SHORT_DEADLINE,
        )
        .await;
    // A signer that gave up waiting has dropped its end already.
    let _ = release.send(());

    assert!(matches!(refused, Err(EnrollError::TooSlow)), "{refused:?}");
    let all = entries(&setup.db).await;
    assert_eq!(of(&all, &AuditAction::AGENT_ENROLL).len(), 0);
    enroll(&setup, &token, "agent-1").await.unwrap();
}

#[sqlx::test(migrations = false)]
async fn an_enrollment_in_its_transaction_finishes_past_its_deadline(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let token = host_token(&setup).await;
    // The one write connection, held so the enrollment's transaction waits
    // for it (within the pool's 5 s acquire timeout).
    let held = setup.db.write().await.unwrap();
    let mut enrolling = std::pin::pin!(setup.enrollment.enroll(
        request(&token, "agent-1", 7),
        IpAddr::from(AGENT_IP),
        SHORT_DEADLINE,
    ));

    let early = tokio::time::timeout(STILL_RUNNING, &mut enrolling).await;
    assert!(early.is_err(), "it ended at its deadline: {early:?}");
    drop(held);
    let enrolled = enrolling.await;

    let enrolled = enrolled.unwrap();
    assert!(
        setup
            .db
            .agents()
            .find(enrolled.agent)
            .await
            .unwrap()
            .is_some()
    );
    let all = entries(&setup.db).await;
    assert_eq!(of(&all, &AuditAction::ENROLLMENT_TOKEN_USE).len(), 1);
}

#[sqlx::test(migrations = false)]
async fn a_refusal_being_recorded_finishes_past_its_deadline(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let token = host_token(&setup).await;
    setup.clock.advance(Duration::from_mins(15));
    let held = setup.db.write().await.unwrap();
    let mut enrolling = std::pin::pin!(setup.enrollment.enroll(
        request(&token, "agent-1", 7),
        IpAddr::from(AGENT_IP),
        SHORT_DEADLINE,
    ));

    let early = tokio::time::timeout(STILL_RUNNING, &mut enrolling).await;
    assert!(early.is_err(), "it ended at its deadline: {early:?}");
    drop(held);
    let refused = enrolling.await;

    assert_eq!(token_problem(refused), TokenProblem::Expired);
    let all = entries(&setup.db).await;
    assert_eq!(of(&all, &AuditAction::AGENT_ENROLL).len(), 1);
}
