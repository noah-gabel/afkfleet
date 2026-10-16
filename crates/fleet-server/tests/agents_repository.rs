//! Tests for the agents' repository (Plan.md P10.2, P10.3, Appendix D,
//! ADR-0019): an enrolled agent reads back enabled and not yet heard from;
//! two enabled agents never share a name, whatever its case, while a disabled
//! one frees its name; an agent holds any number of certificates, each stored
//! once by its fingerprint; and the tables refuse what their CHECKs and
//! foreign keys forbid.
//!
//! Each test gets a fresh database from `#[sqlx::test]` and opens it with
//! `Database::connect`, the production settings. Time isn't paused: sqlx's
//! worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget};
use fleet_core::id::AgentId;
use fleet_core::pki::CertFingerprint;
use fleet_core::value::{AgentCapacity, AgentName, AgentVersion};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::agents::{
    Agent, AgentCert, AgentHello, InsertAgentError, ListedAgent, NewAgent, NewAgentCert,
};
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::page::{Page, PageLimit};
use fleet_server::ports::store::{Store, StoreError};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// The time everything starts at, in milliseconds.
const T0: i64 = 1_800_000_000_000;
/// A day in milliseconds.
const DAY: i64 = 86_400_000;

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

async fn database(options: SqliteConnectOptions) -> Database {
    Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap()
}

/// The entry a test commits its setup with. A test-only name, never listed
/// in `audit_actions!`.
fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from("test.agents_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// Agent `n`, named `name`, enrolled `n` milliseconds after `T0`.
fn new_agent(n: u8, name: &str) -> NewAgent {
    NewAgent {
        id: AgentId::new_v7(at(T0 + i64::from(n)), [n; 10]).unwrap(),
        name: AgentName::try_from(name).unwrap(),
        created_at: at(T0 + i64::from(n)),
    }
}

/// A certificate with fingerprint `byte`… for `agent`, stored at `T0` and
/// valid 60 days.
fn new_cert(byte: u8, agent: AgentId) -> NewAgentCert {
    NewAgentCert {
        fingerprint: CertFingerprint::from_digest([byte; 32]),
        agent,
        not_after: at(T0 + 60 * DAY),
        created_at: at(T0),
    }
}

/// Inserts `agent` in its own audited transaction.
async fn insert(db: &Database, agent: &NewAgent) -> Result<(), InsertAgentError> {
    let mut tx = db.write().await?;
    tx.agents().insert(agent).await?;
    tx.commit(setup_entry()).await?;
    Ok(())
}

/// Stores `cert` in its own audited transaction.
async fn insert_cert(db: &Database, cert: &NewAgentCert) -> Result<(), StoreError> {
    let mut tx = db.write().await?;
    tx.agents().insert_cert(cert).await?;
    tx.commit(setup_entry()).await?;
    Ok(())
}

/// Disables agent `id`, as P10.10 will, through the write pool.
async fn disable(db: &Database, id: AgentId) {
    let id = id.as_uuid().hyphenated().to_string();
    sqlx::query!("UPDATE agents SET enabled = 0 WHERE id = ?", id)
        .execute(db.write_pool())
        .await
        .unwrap();
}

#[sqlx::test(migrations = false)]
async fn an_enrolled_agent_reads_back_enabled_and_not_yet_heard_from(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let agent = new_agent(1, "agent-1");

    insert(&db, &agent).await.unwrap();

    assert_eq!(
        db.agents().find(agent.id).await.unwrap(),
        Some(Agent {
            id: agent.id,
            name: agent.name.clone(),
            enabled: true,
            capacity: None,
            version: None,
            last_seen_at: None,
            created_at: agent.created_at,
        })
    );
}

#[sqlx::test(migrations = false)]
async fn an_unknown_agent_is_none(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    insert(&db, &new_agent(1, "agent-1")).await.unwrap();

    let unknown = new_agent(2, "agent-2").id;

    assert_eq!(db.agents().find(unknown).await.unwrap(), None);
    assert_eq!(db.agents().certs(unknown).await.unwrap(), []);
}

#[sqlx::test(migrations = false)]
async fn two_enabled_agents_never_share_a_name_in_any_case(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    insert(&db, &new_agent(1, "agent-1")).await.unwrap();

    let same = insert(&db, &new_agent(2, "agent-1")).await;
    let other_case = insert(&db, &new_agent(3, "AGENT-1")).await;

    assert!(matches!(same, Err(InsertAgentError::NameTaken)), "{same:?}");
    assert!(
        matches!(other_case, Err(InsertAgentError::NameTaken)),
        "{other_case:?}"
    );
    assert_eq!(db.agents().find(new_agent(2, "x").id).await.unwrap(), None);
    assert!(
        db.agents()
            .name_taken(&AgentName::try_from("Agent-1").unwrap())
            .await
            .unwrap()
    );
    assert!(
        !db.agents()
            .name_taken(&AgentName::try_from("agent-2").unwrap())
            .await
            .unwrap()
    );
}

#[sqlx::test(migrations = false)]
async fn a_disabled_agent_frees_its_name(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let old = new_agent(1, "agent-1");
    insert(&db, &old).await.unwrap();

    disable(&db, old.id).await;
    let name = AgentName::try_from("agent-1").unwrap();
    let taken = db.agents().name_taken(&name).await.unwrap();
    let replacement = insert(&db, &new_agent(2, "Agent-1")).await;

    assert!(!taken);
    replacement.unwrap();
    assert!(!db.agents().find(old.id).await.unwrap().unwrap().enabled);
}

#[sqlx::test(migrations = false)]
async fn an_agent_holds_any_number_of_certificates_oldest_first(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let agent = new_agent(1, "agent-1");
    insert(&db, &agent).await.unwrap();
    let first = new_cert(7, agent.id);
    let second = NewAgentCert {
        created_at: at(T0 + DAY),
        not_after: at(T0 + 61 * DAY),
        ..new_cert(8, agent.id)
    };

    insert_cert(&db, &second).await.unwrap();
    insert_cert(&db, &first).await.unwrap();

    let stored = |cert: &NewAgentCert| AgentCert {
        fingerprint: cert.fingerprint,
        agent: cert.agent,
        not_after: cert.not_after,
        revoked_at: None,
        created_at: cert.created_at,
    };
    assert_eq!(
        db.agents().certs(agent.id).await.unwrap(),
        [stored(&first), stored(&second)]
    );
}

#[sqlx::test(migrations = false)]
async fn a_fingerprint_is_stored_once(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let one = new_agent(1, "agent-1");
    let two = new_agent(2, "agent-2");
    insert(&db, &one).await.unwrap();
    insert(&db, &two).await.unwrap();
    insert_cert(&db, &new_cert(7, one.id)).await.unwrap();

    let again = insert_cert(&db, &new_cert(7, two.id)).await;

    assert!(matches!(again, Err(StoreError::Backend(_))), "{again:?}");
    assert_eq!(db.agents().certs(two.id).await.unwrap(), []);
}

#[sqlx::test(migrations = false)]
async fn a_certificate_of_an_unknown_agent_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let unknown = new_agent(1, "agent-1").id;

    let refused = insert_cert(&db, &new_cert(7, unknown)).await;

    assert!(
        matches!(refused, Err(StoreError::Backend(_))),
        "{refused:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_certificate_expires_after_it_is_stored(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let agent = new_agent(1, "agent-1");
    insert(&db, &agent).await.unwrap();
    let expired = NewAgentCert {
        not_after: at(T0),
        ..new_cert(7, agent.id)
    };

    let refused = insert_cert(&db, &expired).await;

    assert!(
        matches!(refused, Err(StoreError::Backend(_))),
        "{refused:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_time_past_year_9999_is_unstorable(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let agent = new_agent(1, "agent-1");
    insert(&db, &agent).await.unwrap();
    let beyond = NewAgentCert {
        not_after: "+10000-01-01T00:00:00Z".parse().unwrap(),
        ..new_cert(7, agent.id)
    };

    let refused = insert_cert(&db, &beyond).await;

    assert!(
        matches!(
            refused,
            Err(StoreError::Unstorable {
                table: "agent_certs",
                column: "not_after"
            })
        ),
        "{refused:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_stored_name_that_isnt_an_agent_name_is_corrupt(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let agent = new_agent(1, "agent-1");
    insert(&db, &agent).await.unwrap();
    let id = agent.id.as_uuid().hyphenated().to_string();
    sqlx::query!("UPDATE agents SET name = '.hidden' WHERE id = ?", id)
        .execute(db.write_pool())
        .await
        .unwrap();

    let read = db.agents().find(agent.id).await;

    assert!(
        matches!(
            read,
            Err(StoreError::Corrupt {
                table: "agents",
                column: "name",
                ..
            })
        ),
        "{read:?}"
    );
}

// --- Group C: admission and presence (P10.4; ADR-0019, S29, S30) ---------------------

/// The hello agent `agent` sends at `at_millis`.
fn hello(agent: AgentId, capacity: u32, version: &str, at_millis: i64) -> AgentHello {
    AgentHello {
        agent,
        capacity: AgentCapacity::try_from(capacity).unwrap(),
        version: AgentVersion::try_from(version).unwrap(),
        at: at(at_millis),
    }
}

/// Records `hello` in its own audited transaction; whether it changed a row.
async fn record_hello(db: &Database, hello: &AgentHello) -> bool {
    let mut tx = db.write().await.unwrap();
    let recorded = tx.agents().record_hello(hello).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    recorded
}

/// Records that agent `agent` was last heard from at `at_millis`.
async fn record_seen(db: &Database, agent: AgentId, at_millis: i64) {
    let mut tx = db.write().await.unwrap();
    tx.agents().record_seen(agent, at(at_millis)).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
}

#[sqlx::test(migrations = false)]
async fn a_certificate_is_found_by_its_fingerprint(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let agent = new_agent(1, "agent-1");
    insert(&db, &agent).await.unwrap();
    insert_cert(&db, &new_cert(7, agent.id)).await.unwrap();

    let found = db
        .agents()
        .cert(CertFingerprint::from_digest([7; 32]))
        .await
        .unwrap();
    let unknown = db
        .agents()
        .cert(CertFingerprint::from_digest([8; 32]))
        .await
        .unwrap();

    assert_eq!(
        found,
        Some(AgentCert {
            fingerprint: CertFingerprint::from_digest([7; 32]),
            agent: agent.id,
            not_after: at(T0 + 60 * DAY),
            revoked_at: None,
            created_at: at(T0),
        })
    );
    assert_eq!(unknown, None);
}

#[sqlx::test(migrations = false)]
async fn a_transaction_reads_an_agent_and_a_certificate_again(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let agent = new_agent(1, "agent-1");
    insert(&db, &agent).await.unwrap();
    insert_cert(&db, &new_cert(7, agent.id)).await.unwrap();

    let mut tx = db.write().await.unwrap();
    let read = tx.agents().find(agent.id).await.unwrap().unwrap();
    let cert = tx
        .agents()
        .cert(CertFingerprint::from_digest([7; 32]))
        .await
        .unwrap()
        .unwrap();
    let missing = tx
        .agents()
        .cert(CertFingerprint::from_digest([8; 32]))
        .await
        .unwrap();
    drop(tx);

    assert_eq!(read.id, agent.id);
    assert!(read.enabled);
    assert_eq!(cert.agent, agent.id);
    assert_eq!(missing, None);
}

#[sqlx::test(migrations = false)]
async fn a_hello_stores_the_capacity_the_version_and_when_it_was_seen(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let agent = new_agent(1, "agent-1");
    insert(&db, &agent).await.unwrap();

    let recorded = record_hello(&db, &hello(agent.id, 50, "0.1.0", T0 + DAY)).await;

    let read = db.agents().find(agent.id).await.unwrap().unwrap();
    assert!(recorded);
    assert_eq!(
        read.capacity,
        Some(AgentCapacity::try_from(50_u32).unwrap())
    );
    assert_eq!(read.version, Some(AgentVersion::try_from("0.1.0").unwrap()));
    assert_eq!(read.last_seen_at, Some(at(T0 + DAY)));
}

#[sqlx::test(migrations = false)]
async fn a_hello_of_a_disabled_agent_changes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let agent = new_agent(1, "agent-1");
    insert(&db, &agent).await.unwrap();
    disable(&db, agent.id).await;

    let recorded = record_hello(&db, &hello(agent.id, 50, "0.1.0", T0 + DAY)).await;

    let read = db.agents().find(agent.id).await.unwrap().unwrap();
    assert!(!recorded);
    assert_eq!(
        (read.capacity, read.version, read.last_seen_at),
        (None, None, None)
    );
}

#[sqlx::test(migrations = false)]
async fn the_end_of_a_stream_moves_last_seen_forward(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let agent = new_agent(1, "agent-1");
    insert(&db, &agent).await.unwrap();
    record_hello(&db, &hello(agent.id, 50, "0.1.0", T0 + DAY)).await;

    record_seen(&db, agent.id, T0 + 2 * DAY).await;

    let read = db.agents().find(agent.id).await.unwrap().unwrap();
    assert_eq!(read.last_seen_at, Some(at(T0 + 2 * DAY)));
    assert_eq!(
        read.capacity,
        Some(AgentCapacity::try_from(50_u32).unwrap())
    );
}

#[sqlx::test(migrations = false)]
async fn last_seen_never_moves_backwards(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let agent = new_agent(1, "agent-1");
    insert(&db, &agent).await.unwrap();
    record_seen(&db, agent.id, T0 + 2 * DAY).await;

    record_seen(&db, agent.id, T0 + DAY).await;
    record_hello(&db, &hello(agent.id, 60, "0.2.0", T0 + DAY)).await;

    let read = db.agents().find(agent.id).await.unwrap().unwrap();
    assert_eq!(read.last_seen_at, Some(at(T0 + 2 * DAY)));
    assert_eq!(
        read.capacity,
        Some(AgentCapacity::try_from(60_u32).unwrap())
    );
    assert_eq!(read.version, Some(AgentVersion::try_from("0.2.0").unwrap()));
}

#[sqlx::test(migrations = false)]
async fn failures_since_count_only_failures_of_the_action_on_the_target_after_the_time(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let agent = new_agent(1, "agent-1");
    let other = new_agent(2, "agent-2");
    let target = AuditTarget::Agent(agent.id);
    let entry = |millis: i64, action: AuditAction, outcome: AuditOutcome, target: AuditTarget| {
        NewAuditEntry::new(at(millis), action, outcome).target(target)
    };
    let mut tx = db.write().await.unwrap();
    for record in [
        entry(
            T0,
            AuditAction::AGENT_CONNECT,
            AuditOutcome::Failure,
            target,
        ),
        entry(
            T0 + DAY,
            AuditAction::AGENT_CONNECT,
            AuditOutcome::Failure,
            target,
        ),
        entry(
            T0 + DAY + 1,
            AuditAction::AGENT_CONNECT,
            AuditOutcome::Failure,
            target,
        ),
        entry(
            T0 + DAY,
            AuditAction::AGENT_CONNECT,
            AuditOutcome::Success,
            target,
        ),
        entry(
            T0 + DAY,
            AuditAction::AGENT_ENROLL,
            AuditOutcome::Failure,
            target,
        ),
        entry(
            T0 + DAY,
            AuditAction::AGENT_CONNECT,
            AuditOutcome::Failure,
            AuditTarget::Agent(other.id),
        ),
    ] {
        tx.audit().record(&record).await.unwrap();
    }
    tx.commit(setup_entry()).await.unwrap();

    let since = |millis: i64| {
        db.audit()
            .failures_on_since(target, &AuditAction::AGENT_CONNECT, at(millis))
    };

    assert_eq!(since(T0 - 1).await.unwrap(), 3);
    assert_eq!(since(T0).await.unwrap(), 2, "after, not at");
    assert_eq!(since(T0 + DAY).await.unwrap(), 1);
    assert_eq!(since(T0 + DAY + 1).await.unwrap(), 0);
    let mut tx = db.write().await.unwrap();
    assert_eq!(
        tx.audit()
            .failures_on_since(target, &AuditAction::AGENT_CONNECT, at(T0))
            .await
            .unwrap(),
        2
    );
}

/// Revokes every certificate of `agent`, through the write pool.
async fn revoke_all(db: &Database, agent: AgentId) {
    let id = agent.as_uuid().hyphenated().to_string();
    sqlx::query!(
        "UPDATE agent_certs SET revoked_at = '2027-01-15T08:00:00.000Z' WHERE agent_id = ?",
        id
    )
    .execute(db.write_pool())
    .await
    .unwrap();
}

#[sqlx::test(migrations = false)]
async fn the_admissible_certificates_are_unrevoked_unexpired_and_of_enabled_agents(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let revoked = new_agent(1, "agent-1");
    insert(&db, &revoked).await.unwrap();
    insert_cert(&db, &new_cert(1, revoked.id)).await.unwrap();
    revoke_all(&db, revoked.id).await;
    // Issued after the revocation: it admits its agent.
    insert_cert(&db, &new_cert(5, revoked.id)).await.unwrap();
    let fine = new_agent(2, "agent-2");
    insert(&db, &fine).await.unwrap();
    insert_cert(&db, &new_cert(2, fine.id)).await.unwrap();
    let disabled = new_agent(3, "agent-3");
    insert(&db, &disabled).await.unwrap();
    insert_cert(&db, &new_cert(3, disabled.id)).await.unwrap();
    disable(&db, disabled.id).await;
    let expiring = new_agent(4, "agent-4");
    insert(&db, &expiring).await.unwrap();
    insert_cert(
        &db,
        &NewAgentCert {
            not_after: at(T0 + DAY),
            ..new_cert(4, expiring.id)
        },
    )
    .await
    .unwrap();
    let sorted = |mut fingerprints: Vec<CertFingerprint>| {
        fingerprints.sort_by_key(CertFingerprint::to_string);
        fingerprints
    };
    let fingerprints = |bytes: &[u8]| {
        sorted(
            bytes
                .iter()
                .map(|&byte| CertFingerprint::from_digest([byte; 32]))
                .collect(),
        )
    };

    let before = db.agents().admissible(at(T0 + DAY - 1)).await.unwrap();
    let at_expiry = db.agents().admissible(at(T0 + DAY)).await.unwrap();

    assert_eq!(sorted(before), fingerprints(&[2, 4, 5]));
    assert_eq!(sorted(at_expiry), fingerprints(&[2, 5]));
}

// --- The agents' list and disabling (P10.10) ---------------------------------------------

/// Page `before` of at most `limit` agents.
fn page(before: Option<AgentId>, limit: u32) -> Page<AgentId> {
    Page {
        before,
        limit: PageLimit::new(limit).unwrap(),
    }
}

#[sqlx::test(migrations = false)]
async fn the_list_pages_agents_newest_first_with_their_latest_unrevoked_not_after(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let first = new_agent(1, "agent-1");
    let second = new_agent(2, "agent-2");
    let third = new_agent(3, "agent-3");
    for agent in [&first, &second, &third] {
        insert(&db, agent).await.unwrap();
    }
    // agent-1: a revoked certificate valid longer, and an unrevoked one.
    insert_cert(
        &db,
        &NewAgentCert {
            not_after: at(T0 + 90 * DAY),
            ..new_cert(1, first.id)
        },
    )
    .await
    .unwrap();
    revoke_all(&db, first.id).await;
    insert_cert(&db, &new_cert(4, first.id)).await.unwrap();
    // agent-2: none left unrevoked.
    insert_cert(&db, &new_cert(2, second.id)).await.unwrap();
    revoke_all(&db, second.id).await;
    // agent-3: two unrevoked; the later one counts.
    insert_cert(&db, &new_cert(3, third.id)).await.unwrap();
    insert_cert(
        &db,
        &NewAgentCert {
            not_after: at(T0 + 70 * DAY),
            ..new_cert(5, third.id)
        },
    )
    .await
    .unwrap();

    let first_page = db.agents().list(page(None, 2)).await.unwrap();
    let rest = db.agents().list(page(Some(second.id), 2)).await.unwrap();

    let shown = |listed: &[ListedAgent]| {
        listed
            .iter()
            .map(|listed| (listed.agent.name.as_str().to_owned(), listed.not_after))
            .collect::<Vec<_>>()
    };
    // One past the limit: another page follows.
    assert_eq!(
        shown(&first_page),
        [
            ("agent-3".to_owned(), Some(at(T0 + 70 * DAY))),
            ("agent-2".to_owned(), None),
            ("agent-1".to_owned(), Some(at(T0 + 60 * DAY))),
        ]
    );
    assert_eq!(
        shown(&rest),
        [("agent-1".to_owned(), Some(at(T0 + 60 * DAY)))]
    );
    let listed = db.agents().listed(first.id).await.unwrap().unwrap();
    assert_eq!(listed, rest[0].clone());
    assert_eq!(listed.agent.id, first.id);
    assert!(listed.agent.enabled);
    assert_eq!(
        db.agents()
            .listed(AgentId::new_v7(at(T0), [9; 10]).unwrap())
            .await
            .unwrap(),
        None
    );
}

#[sqlx::test(migrations = false)]
async fn disabling_turns_the_agent_off_once_and_revokes_each_unrevoked_certificate(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let agent = new_agent(1, "agent-1");
    insert(&db, &agent).await.unwrap();
    insert_cert(&db, &new_cert(1, agent.id)).await.unwrap();
    revoke_all(&db, agent.id).await;
    insert_cert(&db, &new_cert(2, agent.id)).await.unwrap();
    insert_cert(&db, &new_cert(3, agent.id)).await.unwrap();

    let mut tx = db.write().await.unwrap();
    let disabled = tx.agents().disable(agent.id).await.unwrap();
    let revoked = tx
        .agents()
        .revoke_certs(agent.id, at(T0 + DAY))
        .await
        .unwrap();
    let again = tx.agents().disable(agent.id).await.unwrap();
    let revoked_again = tx
        .agents()
        .revoke_certs(agent.id, at(T0 + 2 * DAY))
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert!(disabled);
    assert_eq!(revoked, 2);
    assert!(!again);
    assert_eq!(revoked_again, 0);
    assert!(!db.agents().find(agent.id).await.unwrap().unwrap().enabled);
    let certs = db.agents().certs(agent.id).await.unwrap();
    let revoked_at: Vec<_> = certs.iter().map(|cert| cert.revoked_at).collect();
    assert_eq!(
        revoked_at,
        [
            Some("2027-01-15T08:00:00Z".parse().unwrap()),
            Some(at(T0 + DAY)),
            Some(at(T0 + DAY)),
        ]
    );
}
