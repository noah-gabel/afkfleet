//! Tests for the sessions repository and the unaudited commit (Plan.md P7.1,
//! P7.3, ADR-0016): a session is found by its access hash and by its current
//! or replaced refresh hash, rotation keeps the replaced hash, revocation
//! stamps once, the touch is conditional, and the tables refuse what their
//! CHECKs and foreign keys forbid.
//!
//! Each test gets a fresh database from `#[sqlx::test]` and opens it with
//! `Database::connect`, the production settings. Time isn't paused: sqlx's
//! worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use chrono::{DateTime, TimeDelta, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::authz::Role;
use fleet_core::id::{SessionId, UserId};
use fleet_core::value::Username;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::audit::{AuditLimit, AuditPage, NewAuditEntry};
use fleet_server::ports::sessions::{NewSession, RefreshLookup, Rotation, SessionWithUser};
use fleet_server::ports::store::{Store, StoreError, UnauditedWrite};
use fleet_server::ports::tokens::TokenHash;
use fleet_server::ports::users::{NewUser, PasswordHash};
use secrecy::SecretString;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// The time everything starts at, in milliseconds.
const T0: i64 = 1_800_000_000_000;
/// A stored time.
const STORED_AT: &str = "2027-01-15T08:00:00.000Z";
/// A well-formed version 4 UUID: the right shape for the CHECKs, but no
/// session ID reads it back.
const V4: &str = "550e8400-e29b-41d4-a716-446655440000";

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

fn secs(n: i64) -> TimeDelta {
    TimeDelta::seconds(n)
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
        AuditAction::try_from("test.session_create").unwrap(),
        AuditOutcome::Success,
    )
}

/// Adds user `n` with `role`, and returns its ID.
async fn add_user(db: &Database, n: u8, role: Role) -> UserId {
    let id = UserId::new_v7(at(T0), [n; 10]).unwrap();
    let user = NewUser {
        id,
        username: Username::try_from(format!("user{n}").as_str()).unwrap(),
        password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
        role,
        created_at: at(T0),
    };
    let mut tx = db.write().await.unwrap();
    tx.users().insert(&user).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    id
}

/// Session `n` of `user`, created at `created`, with access hash `10 + n` and
/// refresh hash `20 + n`.
fn new_session(n: u8, user: UserId, created: DateTime<Utc>) -> NewSession {
    NewSession {
        id: SessionId::new_v7(created, [n; 10]).unwrap(),
        user_id: user,
        access_hash: hash(10 + n),
        refresh_hash: hash(20 + n),
        created_at: created,
    }
}

/// Inserts `session` in its own audited transaction.
async fn add_session(db: &Database, session: &NewSession) -> Result<(), StoreError> {
    let mut tx = db.write().await?;
    tx.sessions().insert(session).await?;
    tx.commit(setup_entry()).await?;
    Ok(())
}

/// A user and one session of theirs, created at `T0`.
async fn one_session(db: &Database) -> (UserId, NewSession) {
    let user = add_user(db, 1, Role::Member).await;
    let session = new_session(1, user, at(T0));
    add_session(db, &session).await.unwrap();
    (user, session)
}

async fn by_access(db: &Database, hash: &TokenHash) -> Option<SessionWithUser> {
    db.sessions().find_by_access_hash(hash).await.unwrap()
}

/// Looks `hash` up as a refresh token in a transaction that's dropped.
async fn by_refresh(db: &Database, hash: &TokenHash) -> Option<RefreshLookup> {
    let mut tx = db.write().await.unwrap();
    tx.sessions().find_by_refresh_hash(hash).await.unwrap()
}

async fn audit_count(db: &Database) -> usize {
    db.audit()
        .list(AuditPage {
            before: None,
            limit: AuditLimit::new(100).unwrap(),
        })
        .await
        .unwrap()
        .len()
}

/// Rotates `session`'s tokens to hashes `30 + n` and `40 + n` at `when`.
fn rotation(session: &NewSession, n: u8, replaced: TokenHash, when: DateTime<Utc>) -> Rotation {
    Rotation {
        session_id: session.id,
        replaced,
        access_hash: hash(30 + n),
        refresh_hash: hash(40 + n),
        at: when,
    }
}

async fn rotate(db: &Database, rotation: &Rotation) -> bool {
    let mut tx = db.write().await.unwrap();
    let done = tx.sessions().rotate(rotation).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    done
}

// --- Insert and the access lookup ----------------------------------------------

#[sqlx::test(migrations = false)]
async fn an_inserted_session_is_found_by_its_access_hash_with_its_user(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (user, session) = one_session(&db).await;

    let found = by_access(&db, &hash(11)).await.unwrap();

    assert_eq!(found.session.id, session.id);
    assert_eq!(found.session.user_id, user);
    assert!(found.session.access_hash.ct_eq(&hash(11)));
    assert!(found.session.refresh_hash.ct_eq(&hash(21)));
    assert_eq!(found.session.created_at, at(T0));
    assert_eq!(found.session.refreshed_at, at(T0), "the first pair's time");
    assert_eq!(found.session.last_used_at, at(T0), "created counts as used");
    assert_eq!(found.session.stepped_up_at, None);
    assert_eq!(found.session.revoked_at, None);
    assert_eq!(found.role, Role::Member);
    assert!(!found.disabled);
}

#[sqlx::test(migrations = false)]
async fn the_users_current_role_and_status_come_with_the_session(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (user, _) = one_session(&db).await;
    let user_text = user.to_string();
    sqlx::query!(
        "UPDATE users SET role = 'admin', disabled = 1 WHERE id = ?",
        user_text
    )
    .execute(db.write_pool())
    .await
    .unwrap();

    let found = by_access(&db, &hash(11)).await.unwrap();

    assert_eq!(found.role, Role::Admin);
    assert!(found.disabled);
}

#[sqlx::test(migrations = false)]
async fn an_unknown_access_hash_finds_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    one_session(&db).await;

    assert!(by_access(&db, &hash(99)).await.is_none());
    assert!(
        by_access(&db, &hash(21)).await.is_none(),
        "a refresh hash isn't an access hash"
    );
}

#[sqlx::test(migrations = false)]
async fn a_hash_already_in_use_is_refused(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let (user, session) = one_session(&db).await;
    let mut twin = new_session(2, user, at(T0));
    twin.access_hash = session.access_hash;

    let refused = add_session(&db, &twin).await;

    assert!(
        matches!(refused, Err(StoreError::Backend(_))),
        "{refused:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_session_for_an_unknown_user_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let stranger = UserId::new_v7(at(T0), [9; 10]).unwrap();

    let refused = add_session(&db, &new_session(1, stranger, at(T0))).await;

    assert!(
        matches!(refused, Err(StoreError::Backend(_))),
        "{refused:?}"
    );
}

// --- The refresh lookup and rotation ------------------------------------------------

#[sqlx::test(migrations = false)]
async fn the_current_refresh_hash_finds_the_session(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (_, session) = one_session(&db).await;

    let found = by_refresh(&db, &hash(21)).await;

    let Some(RefreshLookup::Current(found)) = found else {
        panic!("not the current token: {found:?}");
    };
    assert_eq!(found.session.id, session.id);
    assert_eq!(found.role, Role::Member);
}

#[sqlx::test(migrations = false)]
async fn an_unknown_refresh_hash_finds_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    one_session(&db).await;

    assert!(by_refresh(&db, &hash(99)).await.is_none());
    assert!(
        by_refresh(&db, &hash(11)).await.is_none(),
        "an access hash isn't a refresh hash"
    );
}

#[sqlx::test(migrations = false)]
async fn rotation_replaces_both_tokens_and_keeps_the_replaced_refresh_hash(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (_, session) = one_session(&db).await;
    let later = at(T0) + secs(900);

    let done = rotate(&db, &rotation(&session, 1, hash(21), later)).await;

    assert!(done);
    assert!(
        by_access(&db, &hash(11)).await.is_none(),
        "the old access hash is gone"
    );
    let current = by_access(&db, &hash(31)).await.unwrap();
    assert_eq!(current.session.id, session.id, "the same session ID");
    assert_eq!(current.session.refreshed_at, later);
    assert_eq!(
        current.session.created_at,
        at(T0),
        "the absolute anchor stays"
    );
    assert!(matches!(
        by_refresh(&db, &hash(41)).await,
        Some(RefreshLookup::Current(_))
    ));
    let replaced = by_refresh(&db, &hash(21)).await;
    let Some(RefreshLookup::Rotated {
        session: owner,
        hash: stored,
        rotated_at,
    }) = replaced
    else {
        panic!("the replaced token isn't in the history: {replaced:?}");
    };
    assert_eq!(owner.session.id, session.id);
    assert!(
        owner.session.refresh_hash.ct_eq(&hash(41)),
        "with the session as it is now"
    );
    assert!(stored.ct_eq(&hash(21)));
    assert_eq!(rotated_at, later);
}

#[sqlx::test(migrations = false)]
async fn every_replaced_refresh_hash_stays_in_the_history(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (_, session) = one_session(&db).await;
    let first = at(T0) + secs(900);
    let second = at(T0) + secs(1800);
    assert!(rotate(&db, &rotation(&session, 1, hash(21), first)).await);
    assert!(rotate(&db, &rotation(&session, 2, hash(41), second)).await);

    let oldest = by_refresh(&db, &hash(21)).await;

    let Some(RefreshLookup::Rotated { rotated_at, .. }) = oldest else {
        panic!("the first token is gone from the history: {oldest:?}");
    };
    assert_eq!(rotated_at, first);
}

#[sqlx::test(migrations = false)]
async fn rotating_from_a_token_that_isnt_current_changes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (_, session) = one_session(&db).await;

    let done = rotate(&db, &rotation(&session, 1, hash(99), at(T0) + secs(900))).await;

    assert!(!done);
    assert!(by_access(&db, &hash(11)).await.is_some(), "the tokens stay");
    assert!(by_refresh(&db, &hash(99)).await.is_none(), "no history row");
}

// --- Revocation ------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn revocation_stamps_the_session_once(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (_, session) = one_session(&db).await;
    let first = at(T0) + secs(60);

    let mut tx = db.write().await.unwrap();
    let revoked = tx.sessions().revoke(session.id, first).await.unwrap();
    let again = tx
        .sessions()
        .revoke(session.id, first + secs(60))
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert!(revoked);
    assert!(!again, "already revoked");
    let found = by_access(&db, &hash(11)).await.unwrap();
    assert_eq!(
        found.session.revoked_at,
        Some(first),
        "the first stamp stays"
    );
}

#[sqlx::test(migrations = false)]
async fn revoking_an_unknown_session_changes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    one_session(&db).await;
    let unknown = SessionId::new_v7(at(T0), [9; 10]).unwrap();

    let mut tx = db.write().await.unwrap();
    let revoked = tx.sessions().revoke(unknown, at(T0)).await.unwrap();

    assert!(!revoked);
}

// --- The touch and the unaudited commit -------------------------------------------

/// Touches `session` at `when` if its `last_used_at` is at or before
/// `stale_since`, committed without an entry.
async fn touch(
    db: &Database,
    session: SessionId,
    when: DateTime<Utc>,
    stale_since: DateTime<Utc>,
) -> bool {
    let mut tx = db.write().await.unwrap();
    let wrote = tx
        .sessions()
        .touch(session, when, stale_since)
        .await
        .unwrap();
    tx.commit_unaudited(UnauditedWrite::SessionTouch)
        .await
        .unwrap();
    wrote
}

async fn last_used(db: &Database) -> DateTime<Utc> {
    by_access(db, &hash(11)).await.unwrap().session.last_used_at
}

#[sqlx::test(migrations = false)]
async fn a_stale_last_used_time_is_written(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (_, session) = one_session(&db).await;
    let now = at(T0) + secs(61);

    let wrote = touch(&db, session.id, now, now - secs(60)).await;

    assert!(wrote);
    assert_eq!(last_used(&db).await, now);
}

#[sqlx::test(migrations = false)]
async fn a_last_used_time_exactly_at_the_limit_counts_as_stale(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (_, session) = one_session(&db).await;
    let now = at(T0) + secs(60);

    let wrote = touch(&db, session.id, now, at(T0)).await;

    assert!(wrote);
    assert_eq!(last_used(&db).await, now);
}

#[sqlx::test(migrations = false)]
async fn a_fresh_last_used_time_is_left_alone(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (_, session) = one_session(&db).await;
    let now = at(T0) + secs(30);

    let wrote = touch(&db, session.id, now, now - secs(60)).await;

    assert!(!wrote);
    assert_eq!(last_used(&db).await, at(T0));
}

#[sqlx::test(migrations = false)]
async fn a_clock_that_went_back_never_moves_last_used_back(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (_, session) = one_session(&db).await;
    let behind = at(T0) - secs(600);

    let wrote = touch(&db, session.id, behind, behind - secs(60)).await;

    assert!(!wrote);
    assert_eq!(last_used(&db).await, at(T0));
}

#[sqlx::test(migrations = false)]
async fn an_unaudited_commit_commits_and_records_no_entry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (_, session) = one_session(&db).await;
    let entries = audit_count(&db).await;
    let now = at(T0) + secs(120);

    assert!(touch(&db, session.id, now, now - secs(60)).await);

    assert_eq!(last_used(&db).await, now, "committed");
    assert_eq!(audit_count(&db).await, entries, "no entry");
}

#[sqlx::test(migrations = false)]
async fn a_dropped_transaction_rolls_the_touch_back(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (_, session) = one_session(&db).await;
    let now = at(T0) + secs(120);

    let mut tx = db.write().await.unwrap();
    assert!(
        tx.sessions()
            .touch(session.id, now, now - secs(60))
            .await
            .unwrap()
    );
    drop(tx);

    assert_eq!(last_used(&db).await, at(T0));
}

// --- What the tables refuse ----------------------------------------------------------

/// A sessions row as raw column values, for inserts that bypass the
/// repository.
#[derive(Clone)]
struct RawSession {
    id: String,
    user_id: String,
    access_hash: Vec<u8>,
    refresh_hash: Vec<u8>,
    created_at: String,
    refreshed_at: String,
    last_used_at: String,
    stepped_up_at: Option<String>,
    revoked_at: Option<String>,
}

impl RawSession {
    /// A valid row of `user`.
    fn valid(user: UserId) -> Self {
        Self {
            id: SessionId::new_v7(at(T0), [5; 10]).unwrap().to_string(),
            user_id: user.to_string(),
            access_hash: vec![50; 32],
            refresh_hash: vec![60; 32],
            created_at: STORED_AT.to_owned(),
            refreshed_at: STORED_AT.to_owned(),
            last_used_at: STORED_AT.to_owned(),
            stepped_up_at: None,
            revoked_at: None,
        }
    }

    async fn insert(&self, pool: &SqlitePool) -> Result<(), sqlx::Error> {
        let access = self.access_hash.as_slice();
        let refresh = self.refresh_hash.as_slice();
        sqlx::query!(
            "INSERT INTO sessions (id, user_id, access_hash, refresh_hash, created_at, refreshed_at,
                                   last_used_at, stepped_up_at, revoked_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            self.id,
            self.user_id,
            access,
            refresh,
            self.created_at,
            self.refreshed_at,
            self.last_used_at,
            self.stepped_up_at,
            self.revoked_at
        )
        .execute(pool)
        .await
        .map(drop)
    }
}

/// Whether `result` is a CHECK constraint failure.
fn check_failed(result: &Result<(), sqlx::Error>) -> bool {
    result
        .as_ref()
        .err()
        .and_then(sqlx::Error::as_database_error)
        .is_some_and(|error| error.message().starts_with("CHECK constraint failed"))
}

#[sqlx::test(migrations = false)]
async fn a_valid_raw_session_row_is_accepted(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1, Role::Member).await;

    let mut row = RawSession::valid(user);
    row.stepped_up_at = Some(STORED_AT.to_owned());
    row.revoked_at = Some(STORED_AT.to_owned());

    row.insert(db.write_pool()).await.unwrap();
}

#[sqlx::test(migrations = false)]
async fn the_sessions_table_refuses_malformed_values(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1, Role::Member).await;
    let valid = RawSession::valid(user);
    let cases: Vec<(&str, RawSession)> = vec![
        (
            "an uppercase ID",
            RawSession {
                id: valid.id.to_uppercase(),
                ..valid.clone()
            },
        ),
        (
            "an ID without hyphens",
            RawSession {
                id: valid.id.replace('-', ""),
                ..valid.clone()
            },
        ),
        (
            "an access hash of 31 bytes",
            RawSession {
                access_hash: vec![50; 31],
                ..valid.clone()
            },
        ),
        (
            "a refresh hash of 33 bytes",
            RawSession {
                refresh_hash: vec![60; 33],
                ..valid.clone()
            },
        ),
        (
            "created_at without milliseconds",
            RawSession {
                created_at: "2027-01-15T08:00:00Z".to_owned(),
                ..valid.clone()
            },
        ),
        (
            "refreshed_at with an offset",
            RawSession {
                refreshed_at: "2027-01-15T08:00:00.000+00:00".to_owned(),
                ..valid.clone()
            },
        ),
        (
            "last_used_at with a space",
            RawSession {
                last_used_at: "2027-01-15 08:00:00.000Z".to_owned(),
                ..valid.clone()
            },
        ),
        (
            "an empty stepped_up_at",
            RawSession {
                stepped_up_at: Some(String::new()),
                ..valid.clone()
            },
        ),
        (
            "a date-only revoked_at",
            RawSession {
                revoked_at: Some("2027-01-15".to_owned()),
                ..valid.clone()
            },
        ),
    ];

    for (case, row) in cases {
        let result = row.insert(db.write_pool()).await;

        assert!(check_failed(&result), "{case}: {result:?}");
    }
}

/// Inserts a history row directly, bypassing the repository.
async fn raw_rotated(
    pool: &SqlitePool,
    hash: &[u8],
    session_id: &str,
    rotated_at: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO rotated_refresh_tokens (hash, session_id, rotated_at) VALUES (?, ?, ?)",
        hash,
        session_id,
        rotated_at
    )
    .execute(pool)
    .await
    .map(drop)
}

#[sqlx::test(migrations = false)]
async fn the_history_table_refuses_malformed_values(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (_, session) = one_session(&db).await;
    let id = session.id.to_string();
    let pool = db.write_pool();

    let short = raw_rotated(pool, &[1; 31], &id, STORED_AT).await;
    let bad_time = raw_rotated(pool, &[2; 32], &id, "2027-01-15T08:00:00Z").await;
    let bad_id = raw_rotated(pool, &[3; 32], &id.to_uppercase(), STORED_AT).await;

    assert!(check_failed(&short), "{short:?}");
    assert!(check_failed(&bad_time), "{bad_time:?}");
    assert!(
        bad_id.is_err(),
        "an uppercase ID is refused, by its CHECK or the foreign key: {bad_id:?}"
    );
    raw_rotated(pool, &[4; 32], &id, STORED_AT).await.unwrap();
}

#[sqlx::test(migrations = false)]
async fn a_history_row_for_an_unknown_session_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let unknown = SessionId::new_v7(at(T0), [9; 10]).unwrap().to_string();

    let refused = raw_rotated(db.write_pool(), &[1; 32], &unknown, STORED_AT).await;

    let error = refused.unwrap_err();
    assert!(
        error
            .as_database_error()
            .is_some_and(|error| error.message().contains("FOREIGN KEY")),
        "{error:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn deleting_a_session_deletes_its_history(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (_, session) = one_session(&db).await;
    assert!(rotate(&db, &rotation(&session, 1, hash(21), at(T0) + secs(900))).await);
    let id = session.id.to_string();

    sqlx::query!("DELETE FROM sessions WHERE id = ?", id)
        .execute(db.write_pool())
        .await
        .unwrap();

    let left = sqlx::query_scalar!(r#"SELECT COUNT(*) AS "n!: i64" FROM rotated_refresh_tokens"#)
        .fetch_one(db.read_pool())
        .await
        .unwrap();
    assert_eq!(left, 0);
}

// --- Rows that don't read back -------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_stored_id_that_isnt_a_session_id_is_corrupt(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1, Role::Member).await;
    let row = RawSession {
        id: V4.to_owned(),
        ..RawSession::valid(user)
    };
    row.insert(db.write_pool()).await.unwrap();

    let found = db
        .sessions()
        .find_by_access_hash(&TokenHash::from_bytes([50; 32]))
        .await;

    assert!(
        matches!(
            found,
            Err(StoreError::Corrupt {
                table: "sessions",
                column: "id",
                ..
            })
        ),
        "{found:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_stored_time_that_isnt_a_date_is_corrupt(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1, Role::Member).await;
    let row = RawSession {
        refreshed_at: "2027-02-30T08:00:00.000Z".to_owned(),
        ..RawSession::valid(user)
    };
    row.insert(db.write_pool()).await.unwrap();

    let mut tx = db.write().await.unwrap();
    let found = tx
        .sessions()
        .find_by_refresh_hash(&TokenHash::from_bytes([60; 32]))
        .await;

    assert!(
        matches!(
            found,
            Err(StoreError::Corrupt {
                table: "sessions",
                column: "refreshed_at",
                ..
            })
        ),
        "{found:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_stored_rotation_time_that_isnt_a_date_is_corrupt(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (_, session) = one_session(&db).await;
    let id = session.id.to_string();
    raw_rotated(db.write_pool(), &[7; 32], &id, "2027-13-01T08:00:00.000Z")
        .await
        .unwrap();

    let mut tx = db.write().await.unwrap();
    let found = tx
        .sessions()
        .find_by_refresh_hash(&TokenHash::from_bytes([7; 32]))
        .await;

    assert!(
        matches!(
            found,
            Err(StoreError::Corrupt {
                table: "rotated_refresh_tokens",
                column: "rotated_at",
                ..
            })
        ),
        "{found:?}"
    );
}
