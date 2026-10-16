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
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::id::AgentId;
use fleet_core::pki::CertFingerprint;
use fleet_core::value::AgentName;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::agents::{Agent, AgentCert, InsertAgentError, NewAgent, NewAgentCert};
use fleet_server::ports::audit::NewAuditEntry;
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
