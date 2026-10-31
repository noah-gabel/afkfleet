//! Tests for the enrollment tokens' repository (Plan.md P10.3, ADR-0019): a
//! token is found by its hash, on the read pool and in a transaction; it's
//! used once, never from its expiry on, and only by one agent; pending
//! tokens are counted; the table refuses what its CHECKs and foreign keys
//! forbid; and the audit log counts a token's refused enrollments.
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
use fleet_core::authz::Role;
use fleet_core::id::{AgentId, EnrollmentTokenId, UserId};
use fleet_core::value::{AgentName, Username};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::agents::NewAgent;
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::enrollment_tokens::{EnrollmentToken, NewEnrollmentToken};
use fleet_server::ports::store::{Store, StoreError};
use fleet_server::ports::tokens::TokenHash;
use fleet_server::ports::users::{NewUser, PasswordHash};
use secrecy::SecretString;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// The time everything starts at, in milliseconds.
const T0: i64 = 1_800_000_000_000;
/// A token's lifetime, in milliseconds.
const TTL: i64 = 15 * 60_000;

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

fn hash(byte: u8) -> TokenHash {
    TokenHash::from_bytes([byte; 32])
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
        AuditAction::try_from("test.enrollment_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// Adds the Owner and returns its ID.
async fn add_owner(db: &Database) -> UserId {
    let id = UserId::new_v7(at(T0), [1; 10]).unwrap();
    let user = NewUser {
        id,
        username: Username::try_from("owner").unwrap(),
        password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
        role: Role::Owner,
        created_at: at(T0),
    };
    let mut tx = db.write().await.unwrap();
    tx.users().insert(&user).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    id
}

/// Adds agent `n` and returns its ID.
async fn add_agent(db: &Database, n: u8) -> AgentId {
    let agent = NewAgent {
        id: AgentId::new_v7(at(T0), [n; 10]).unwrap(),
        name: AgentName::try_from(format!("agent-{n}")).unwrap(),
        created_at: at(T0),
    };
    let mut tx = db.write().await.unwrap();
    tx.agents().insert(&agent).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    agent.id
}

/// Token `n`, created `millis` after `T0`, living [`TTL`].
fn new_token(n: u8, millis: i64) -> NewEnrollmentToken {
    NewEnrollmentToken {
        id: EnrollmentTokenId::new_v7(at(T0 + millis), [n; 10]).unwrap(),
        token_hash: hash(n),
        created_at: at(T0 + millis),
        expires_at: at(T0 + millis + TTL),
    }
}

/// Inserts `token` in its own audited transaction.
async fn insert(db: &Database, token: &NewEnrollmentToken) -> Result<(), StoreError> {
    let mut tx = db.write().await?;
    tx.enrollment_tokens().insert(token).await?;
    tx.commit(setup_entry()).await?;
    Ok(())
}

/// Uses token `id` for `agent` at `millis` after `T0`, in its own
/// transaction.
async fn use_token(
    db: &Database,
    id: EnrollmentTokenId,
    agent: AgentId,
    millis: i64,
) -> Result<bool, StoreError> {
    let mut tx = db.write().await?;
    let used = tx
        .enrollment_tokens()
        .use_token(id, agent, at(T0 + millis))
        .await?;
    tx.commit(setup_entry()).await?;
    Ok(used)
}

/// What a stored token is, for comparisons.
type Summary = (
    EnrollmentTokenId,
    [u8; 32],
    DateTime<Utc>,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    Option<AgentId>,
);

fn summary(token: &EnrollmentToken) -> Summary {
    (
        token.id,
        *token.token_hash.as_bytes(),
        token.created_at,
        token.expires_at,
        token.used_at,
        token.agent,
    )
}

fn pending(token: &NewEnrollmentToken) -> Summary {
    (
        token.id,
        *token.token_hash.as_bytes(),
        token.created_at,
        token.expires_at,
        None,
        None,
    )
}

async fn found(db: &Database, byte: u8) -> Option<Summary> {
    db.enrollment_tokens()
        .find_by_hash(&hash(byte))
        .await
        .unwrap()
        .as_ref()
        .map(summary)
}

#[sqlx::test(migrations = false)]
async fn a_token_is_found_by_its_hash_on_the_read_pool_and_in_a_transaction(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let token = new_token(7, 0);

    insert(&db, &token).await.unwrap();

    assert_eq!(found(&db, 7).await, Some(pending(&token)));
    let mut tx = db.write().await.unwrap();
    let in_tx = tx.enrollment_tokens().find_by_hash(&hash(7)).await.unwrap();
    assert_eq!(in_tx.as_ref().map(summary), Some(pending(&token)));
}

#[sqlx::test(migrations = false)]
async fn an_unknown_hash_finds_nothing(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    insert(&db, &new_token(7, 0)).await.unwrap();

    assert_eq!(found(&db, 8).await, None);
}

#[sqlx::test(migrations = false)]
async fn a_host_operators_token_has_no_creator(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let token = new_token(7, 0);

    insert(&db, &token).await.unwrap();

    let id = token.id.as_uuid().hyphenated().to_string();
    let row = sqlx::query!(
        r#"SELECT created_by AS "created_by?: String" FROM enrollment_tokens WHERE id = ?"#,
        id
    )
    .fetch_one(db.write_pool())
    .await
    .unwrap();
    assert_eq!(row.created_by, None);
}

/// A token the removed route stored names its creator, which nothing reads
/// any more (ADR-0020, S88).
#[sqlx::test(migrations = false)]
async fn a_token_stored_with_a_creator_still_reads_back(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_owner(&db).await.as_uuid().hyphenated().to_string();
    let token = new_token(7, 0);
    insert(&db, &token).await.unwrap();
    let id = token.id.as_uuid().hyphenated().to_string();
    sqlx::query!(
        "UPDATE enrollment_tokens SET created_by = ? WHERE id = ?",
        owner,
        id
    )
    .execute(db.write_pool())
    .await
    .unwrap();

    assert_eq!(found(&db, 7).await, Some(pending(&token)));
}

#[sqlx::test(migrations = false)]
async fn a_token_enrolls_once_and_names_its_agent(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let token = new_token(7, 0);
    insert(&db, &token).await.unwrap();
    let first = add_agent(&db, 1).await;
    let second = add_agent(&db, 2).await;

    let once = use_token(&db, token.id, first, 1000).await.unwrap();
    let twice = use_token(&db, token.id, second, 2000).await.unwrap();

    assert!(once);
    assert!(!twice);
    let stored = found(&db, 7).await.unwrap();
    assert_eq!(stored.4, Some(at(T0 + 1000)));
    assert_eq!(stored.5, Some(first));
}

#[sqlx::test(migrations = false)]
async fn a_token_cant_be_used_from_its_expiry_on(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let token = new_token(7, 0);
    let other = new_token(8, 0);
    insert(&db, &token).await.unwrap();
    insert(&db, &other).await.unwrap();
    let agent = add_agent(&db, 1).await;
    let other_agent = add_agent(&db, 2).await;

    let at_expiry = use_token(&db, token.id, agent, TTL).await.unwrap();
    let just_before = use_token(&db, other.id, other_agent, TTL - 1)
        .await
        .unwrap();

    assert!(!at_expiry);
    assert!(just_before);
    assert_eq!(found(&db, 7).await.unwrap().5, None);
}

#[sqlx::test(migrations = false)]
async fn one_agent_is_enrolled_by_one_token(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let token = new_token(7, 0);
    let other = new_token(8, 0);
    insert(&db, &token).await.unwrap();
    insert(&db, &other).await.unwrap();
    let agent = add_agent(&db, 1).await;
    use_token(&db, token.id, agent, 1000).await.unwrap();

    let again = use_token(&db, other.id, agent, 2000).await;

    assert!(matches!(again, Err(StoreError::Backend(_))), "{again:?}");
}

#[sqlx::test(migrations = false)]
async fn only_unused_unexpired_tokens_are_pending(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    for n in 1..=4 {
        insert(&db, &new_token(n, 0)).await.unwrap();
    }
    insert(&db, &new_token(5, TTL)).await.unwrap();
    let agent = add_agent(&db, 1).await;
    let used = new_token(1, 0).id;
    use_token(&db, used, agent, 1000).await.unwrap();

    let mut tx = db.write().await.unwrap();
    let before_expiry = tx
        .enrollment_tokens()
        .count_pending(at(T0 + TTL - 1))
        .await
        .unwrap();
    let at_expiry = tx
        .enrollment_tokens()
        .count_pending(at(T0 + TTL))
        .await
        .unwrap();

    assert_eq!(before_expiry, 4);
    assert_eq!(at_expiry, 1);
}

#[sqlx::test(migrations = false)]
async fn a_hash_stored_twice_is_refused(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    insert(&db, &new_token(7, 0)).await.unwrap();
    let same_hash = NewEnrollmentToken {
        id: EnrollmentTokenId::new_v7(at(T0), [8; 10]).unwrap(),
        ..new_token(7, 0)
    };

    let refused = insert(&db, &same_hash).await;

    assert!(
        matches!(refused, Err(StoreError::Backend(_))),
        "{refused:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_token_expires_after_it_is_created(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let backwards = NewEnrollmentToken {
        expires_at: at(T0),
        ..new_token(7, 0)
    };

    let refused = insert(&db, &backwards).await;

    assert!(
        matches!(refused, Err(StoreError::Backend(_))),
        "{refused:?}"
    );
    assert_eq!(found(&db, 7).await, None);
}

#[sqlx::test(migrations = false)]
async fn a_tokens_refusals_are_counted_from_the_audit_log(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let token = AuditTarget::EnrollmentToken(new_token(7, 0).id);
    let other = AuditTarget::EnrollmentToken(new_token(8, 0).id);
    let entry = |target, outcome| {
        NewAuditEntry::new(at(T0), AuditAction::AGENT_ENROLL, outcome).target(target)
    };
    let mut tx = db.write().await.unwrap();
    for record in [
        entry(token, AuditOutcome::Failure),
        entry(token, AuditOutcome::Failure),
        entry(token, AuditOutcome::Success),
        entry(other, AuditOutcome::Failure),
        NewAuditEntry::new(
            at(T0),
            AuditAction::ENROLLMENT_TOKEN_USE,
            AuditOutcome::Failure,
        )
        .target(token),
    ] {
        tx.audit().record(&record).await.unwrap();
    }
    tx.commit(setup_entry()).await.unwrap();

    let read = db
        .audit()
        .failures_on(token, &AuditAction::AGENT_ENROLL)
        .await
        .unwrap();
    let mut tx = db.write().await.unwrap();
    let in_tx = tx
        .audit()
        .failures_on(token, &AuditAction::AGENT_ENROLL)
        .await
        .unwrap();

    assert_eq!(read, 2);
    assert_eq!(in_tx, 2);
}
