//! Tests for the second factor's repository (Plan.md P7.1, P7.7, ADR-0016):
//! a confirmed authenticator is found on the read pool and in a
//! transaction, a first enrollment is never inserted twice, a replacement
//! swaps everything, a time step and a recovery code are each accepted only
//! once, and the tables refuse what their CHECKs and foreign keys forbid.
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
use fleet_core::authz::Role;
use fleet_core::id::UserId;
use fleet_core::value::Username;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::mfa::{InsertMfaError, UserMfa};
use fleet_server::ports::recovery_codes::RecoveryCodeHash;
use fleet_server::ports::store::{Store, StoreError};
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
/// A version 1 envelope's length for a 20-byte secret.
const ENVELOPE_LEN: usize = 65;

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

fn code_hash(byte: u8) -> RecoveryCodeHash {
    RecoveryCodeHash::from_bytes([byte; 32])
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
        AuditAction::try_from("test.mfa_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// Adds user `n`, and returns its ID.
async fn add_user(db: &Database, n: u8) -> UserId {
    let id = UserId::new_v7(at(T0), [n; 10]).unwrap();
    let user = NewUser {
        id,
        username: Username::try_from(format!("user{n}").as_str()).unwrap(),
        password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
        role: Role::Member,
        created_at: at(T0),
    };
    let mut tx = db.write().await.unwrap();
    tx.users().insert(&user).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    id
}

/// An authenticator of `user` whose envelope is all `byte`, at `step`,
/// confirmed at `confirmed`.
fn mfa(user: UserId, byte: u8, step: u64, confirmed: DateTime<Utc>) -> UserMfa {
    UserMfa {
        user_id: user,
        secret_envelope: vec![byte; ENVELOPE_LEN],
        last_used_step: step,
        confirmed_at: confirmed,
    }
}

/// Inserts `mfa` in its own audited transaction.
async fn insert(db: &Database, mfa: &UserMfa) -> Result<(), InsertMfaError> {
    let mut tx = db.write().await?;
    tx.mfa().insert(mfa).await?;
    tx.commit(setup_entry()).await?;
    Ok(())
}

/// A user with an authenticator at step 100, confirmed at `T0`.
async fn enrolled(db: &Database) -> (UserId, UserMfa) {
    let user = add_user(db, 1).await;
    let stored = mfa(user, 0x11, 100, at(T0));
    insert(db, &stored).await.unwrap();
    (user, stored)
}

async fn found(db: &Database, user: UserId) -> Option<UserMfa> {
    db.mfa().find(user).await.unwrap()
}

/// The fields of an authenticator, for comparisons.
fn summary(mfa: &UserMfa) -> (UserId, Vec<u8>, u64, DateTime<Utc>) {
    (
        mfa.user_id,
        mfa.secret_envelope.clone(),
        mfa.last_used_step,
        mfa.confirmed_at,
    )
}

/// Runs `advance_step` in its own transaction, committed only when it
/// changed a row.
async fn advance(db: &Database, user: UserId, step: u64, confirmed: DateTime<Utc>) -> bool {
    let mut tx = db.write().await.unwrap();
    let advanced = tx.mfa().advance_step(user, step, confirmed).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    advanced
}

/// Replaces `user`'s recovery codes with `hashes` in its own transaction.
async fn replace_codes(db: &Database, user: UserId, hashes: &[RecoveryCodeHash]) {
    let mut tx = db.write().await.unwrap();
    tx.mfa().replace_codes(user, hashes).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
}

/// Runs `use_code` in its own transaction.
async fn use_code(db: &Database, user: UserId, hash: &RecoveryCodeHash) -> bool {
    let mut tx = db.write().await.unwrap();
    let marked = tx.mfa().use_code(user, hash, at(T0 + 5_000)).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    marked
}

/// Whether `user`'s unused code `hash` is found, confirmed in constant time.
async fn unused(db: &Database, user: UserId, hash: &RecoveryCodeHash) -> bool {
    db.mfa()
        .find_unused_code(user, hash)
        .await
        .unwrap()
        .is_some_and(|stored| stored.ct_eq(hash))
}

// --- The authenticator ---------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn an_inserted_authenticator_is_found_on_the_read_pool_and_in_a_transaction(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;

    let (user, stored) = enrolled(&db).await;

    let read = found(&db, user).await.unwrap();
    let mut tx = db.write().await.unwrap();
    let in_tx = tx.mfa().find(user).await.unwrap().unwrap();
    assert_eq!(summary(&read), summary(&stored));
    assert_eq!(summary(&in_tx), summary(&stored));
}

#[sqlx::test(migrations = false)]
async fn a_user_without_2fa_has_no_authenticator(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;

    assert!(found(&db, user).await.is_none());
    let mut tx = db.write().await.unwrap();
    assert!(tx.mfa().find(user).await.unwrap().is_none());
}

#[sqlx::test(migrations = false)]
async fn a_second_first_enrollment_is_refused_and_changes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (user, stored) = enrolled(&db).await;

    let second = insert(&db, &mfa(user, 0x22, 200, at(T0 + 1_000))).await;

    assert!(matches!(second, Err(InsertMfaError::Exists)), "{second:?}");
    assert_eq!(summary(&found(&db, user).await.unwrap()), summary(&stored));
}

#[sqlx::test(migrations = false)]
async fn an_authenticator_for_an_unknown_user_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let unknown = UserId::new_v7(at(T0), [9; 10]).unwrap();

    let refused = insert(&db, &mfa(unknown, 0x11, 1, at(T0))).await;

    assert!(
        matches!(refused, Err(InsertMfaError::Store(StoreError::Backend(_)))),
        "{refused:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_replacement_swaps_the_secret_the_step_and_the_confirmation(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (user, _) = enrolled(&db).await;
    let replacement = mfa(user, 0x33, 300, at(T0 + 60_000));

    let mut tx = db.write().await.unwrap();
    let replaced = tx.mfa().replace(&replacement).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert!(replaced);
    assert_eq!(
        summary(&found(&db, user).await.unwrap()),
        summary(&replacement)
    );
}

#[sqlx::test(migrations = false)]
async fn replacing_without_2fa_changes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;

    let mut tx = db.write().await.unwrap();
    let replaced = tx
        .mfa()
        .replace(&mfa(user, 0x33, 300, at(T0)))
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert!(!replaced);
    assert!(found(&db, user).await.is_none());
}

#[sqlx::test(migrations = false)]
async fn a_later_step_is_accepted_and_stored(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (user, _) = enrolled(&db).await;

    assert!(advance(&db, user, 101, at(T0)).await);

    assert_eq!(found(&db, user).await.unwrap().last_used_step, 101);
}

#[sqlx::test(migrations = false)]
async fn the_same_or_an_earlier_step_is_refused_and_changes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (user, _) = enrolled(&db).await;

    for step in [100, 99, 0] {
        assert!(!advance(&db, user, step, at(T0)).await, "step {step}");

        assert_eq!(found(&db, user).await.unwrap().last_used_step, 100);
    }
}

#[sqlx::test(migrations = false)]
async fn a_step_is_refused_once_the_authenticator_was_replaced(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (user, original) = enrolled(&db).await;
    let replacement = mfa(user, 0x33, 100, at(T0 + 60_000));
    let mut tx = db.write().await.unwrap();
    assert!(tx.mfa().replace(&replacement).await.unwrap());
    tx.commit(setup_entry()).await.unwrap();

    let advanced = advance(&db, user, 101, original.confirmed_at).await;

    assert!(!advanced);
    assert_eq!(found(&db, user).await.unwrap().last_used_step, 100);
}

#[sqlx::test(migrations = false)]
async fn advancing_without_2fa_changes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;

    assert!(!advance(&db, user, 1, at(T0)).await);
}

#[sqlx::test(migrations = false)]
async fn a_step_beyond_sqlites_integers_cant_be_stored(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (user, _) = enrolled(&db).await;
    let mut tx = db.write().await.unwrap();

    let advanced = tx.mfa().advance_step(user, u64::MAX, at(T0)).await;
    let inserted = tx.mfa().replace(&mfa(user, 0x33, u64::MAX, at(T0))).await;

    assert!(
        matches!(advanced, Err(StoreError::Unstorable { .. })),
        "{advanced:?}"
    );
    assert!(
        matches!(inserted, Err(StoreError::Unstorable { .. })),
        "{inserted:?}"
    );
}

// --- Recovery codes ------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn stored_codes_are_found_unused_by_hash_and_user(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (user, _) = enrolled(&db).await;

    replace_codes(&db, user, &[code_hash(1), code_hash(2)]).await;

    assert!(unused(&db, user, &code_hash(1)).await);
    assert!(unused(&db, user, &code_hash(2)).await);
    assert!(!unused(&db, user, &code_hash(3)).await);
}

#[sqlx::test(migrations = false)]
async fn another_users_code_isnt_found_or_used(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (user, _) = enrolled(&db).await;
    let other = add_user(&db, 2).await;
    replace_codes(&db, user, &[code_hash(1)]).await;

    assert!(!unused(&db, other, &code_hash(1)).await);
    assert!(!use_code(&db, other, &code_hash(1)).await);
    assert!(unused(&db, user, &code_hash(1)).await);
}

#[sqlx::test(migrations = false)]
async fn replacing_the_codes_deletes_the_old_ones(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (user, _) = enrolled(&db).await;
    replace_codes(&db, user, &[code_hash(1), code_hash(2)]).await;

    replace_codes(&db, user, &[code_hash(3)]).await;

    assert!(!unused(&db, user, &code_hash(1)).await);
    assert!(!unused(&db, user, &code_hash(2)).await);
    assert!(unused(&db, user, &code_hash(3)).await);
    assert_eq!(code_rows(db.write_pool()).await, 1);
}

#[sqlx::test(migrations = false)]
async fn replacing_one_users_codes_leaves_anothers_alone(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (user, _) = enrolled(&db).await;
    let other = add_user(&db, 2).await;
    replace_codes(&db, other, &[code_hash(9)]).await;

    replace_codes(&db, user, &[code_hash(1)]).await;

    assert!(unused(&db, other, &code_hash(9)).await);
}

#[sqlx::test(migrations = false)]
async fn a_code_is_used_once_and_keeps_its_row(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (user, _) = enrolled(&db).await;
    replace_codes(&db, user, &[code_hash(1), code_hash(2)]).await;

    let first = use_code(&db, user, &code_hash(1)).await;
    let second = use_code(&db, user, &code_hash(1)).await;

    assert!(first);
    assert!(!second);
    assert!(!unused(&db, user, &code_hash(1)).await);
    assert!(unused(&db, user, &code_hash(2)).await);
    assert_eq!(code_rows(db.write_pool()).await, 2);
    assert_eq!(
        used_at(db.write_pool(), &code_hash(1)).await.as_deref(),
        Some("2027-01-15T08:00:05.000Z")
    );
}

#[sqlx::test(migrations = false)]
async fn a_code_used_up_by_a_replacement_cant_be_used(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (user, _) = enrolled(&db).await;
    replace_codes(&db, user, &[code_hash(1)]).await;
    replace_codes(&db, user, &[code_hash(2)]).await;

    assert!(!use_code(&db, user, &code_hash(1)).await);
}

#[sqlx::test(migrations = false)]
async fn a_hash_stored_twice_is_refused(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let (user, _) = enrolled(&db).await;
    let mut tx = db.write().await.unwrap();

    let refused = tx
        .mfa()
        .replace_codes(user, &[code_hash(1), code_hash(1)])
        .await;

    assert!(
        matches!(refused, Err(StoreError::Backend(_))),
        "{refused:?}"
    );
}

// --- What the tables refuse ----------------------------------------------------------

async fn code_rows(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar!(r#"SELECT count(*) AS "count!: i64" FROM recovery_codes"#)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn used_at(pool: &SqlitePool, hash: &RecoveryCodeHash) -> Option<String> {
    let hash = hash.as_bytes().as_slice();
    sqlx::query_scalar!(
        r#"SELECT used_at AS "used_at?" FROM recovery_codes WHERE hash = ?"#,
        hash
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A `user_mfa` row as raw column values, for inserts that bypass the
/// repository.
#[derive(Clone)]
struct RawMfa {
    user_id: String,
    secret_envelope: Vec<u8>,
    last_used_step: i64,
    confirmed_at: String,
}

impl RawMfa {
    fn valid(user: UserId) -> Self {
        Self {
            user_id: user.to_string(),
            secret_envelope: vec![1; ENVELOPE_LEN],
            last_used_step: 0,
            confirmed_at: STORED_AT.to_owned(),
        }
    }

    async fn insert(&self, pool: &SqlitePool) -> Result<(), sqlx::Error> {
        let envelope = self.secret_envelope.as_slice();
        sqlx::query!(
            "INSERT INTO user_mfa (user_id, secret_envelope, last_used_step, confirmed_at)
             VALUES (?, ?, ?, ?)",
            self.user_id,
            envelope,
            self.last_used_step,
            self.confirmed_at
        )
        .execute(pool)
        .await
        .map(drop)
    }
}

/// Inserts a `recovery_codes` row directly, bypassing the repository.
async fn raw_code(
    pool: &SqlitePool,
    hash: &[u8],
    user_id: &str,
    used_at: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO recovery_codes (hash, user_id, used_at) VALUES (?, ?, ?)",
        hash,
        user_id,
        used_at
    )
    .execute(pool)
    .await
    .map(drop)
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
async fn valid_raw_rows_are_accepted(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;

    RawMfa::valid(user).insert(db.write_pool()).await.unwrap();
    raw_code(db.write_pool(), &[1; 32], &user.to_string(), None)
        .await
        .unwrap();
    raw_code(
        db.write_pool(),
        &[2; 32],
        &user.to_string(),
        Some(STORED_AT),
    )
    .await
    .unwrap();
}

#[sqlx::test(migrations = false)]
async fn the_user_mfa_table_refuses_malformed_values(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let valid = RawMfa::valid(user);
    let cases: Vec<(&str, RawMfa)> = vec![
        (
            "an envelope one byte short",
            RawMfa {
                secret_envelope: vec![1; ENVELOPE_LEN - 1],
                ..valid.clone()
            },
        ),
        (
            "an envelope one byte long",
            RawMfa {
                secret_envelope: vec![1; ENVELOPE_LEN + 1],
                ..valid.clone()
            },
        ),
        (
            "a negative step",
            RawMfa {
                last_used_step: -1,
                ..valid.clone()
            },
        ),
        (
            "confirmed_at without milliseconds",
            RawMfa {
                confirmed_at: "2027-01-15T08:00:00Z".to_owned(),
                ..valid.clone()
            },
        ),
        (
            "an uppercase user ID",
            RawMfa {
                user_id: valid.user_id.to_uppercase(),
                ..valid.clone()
            },
        ),
    ];

    for (case, row) in cases {
        let result = row.insert(db.write_pool()).await;

        assert!(check_failed(&result), "{case}: {result:?}");
    }
}

#[sqlx::test(migrations = false)]
async fn the_recovery_codes_table_refuses_malformed_values(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await.to_string();
    let pool = db.write_pool();

    let short = raw_code(pool, &[1; 31], &user, None).await;
    let long = raw_code(pool, &[2; 33], &user, None).await;
    let bad_time = raw_code(pool, &[3; 32], &user, Some("2027-01-15")).await;
    let bad_id = raw_code(pool, &[4; 32], &user.to_uppercase(), None).await;

    assert!(check_failed(&short), "{short:?}");
    assert!(check_failed(&long), "{long:?}");
    assert!(check_failed(&bad_time), "{bad_time:?}");
    assert!(check_failed(&bad_id), "{bad_id:?}");
}

#[sqlx::test(migrations = false)]
async fn rows_for_an_unknown_user_are_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let unknown = UserId::new_v7(at(T0), [9; 10]).unwrap();

    let mfa_row = RawMfa::valid(unknown).insert(db.write_pool()).await;
    let code_row = raw_code(db.write_pool(), &[1; 32], &unknown.to_string(), None).await;

    for result in [mfa_row, code_row] {
        let error = result.unwrap_err();
        assert!(
            error
                .as_database_error()
                .is_some_and(|error| error.message().contains("FOREIGN KEY")),
            "{error:?}"
        );
    }
}
