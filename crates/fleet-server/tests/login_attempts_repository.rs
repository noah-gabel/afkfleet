//! Tests for the `login_attempts` repository (Plan.md P7.1, P7.10, §7.4,
//! ADR-0016): a streak is stored under its key and found again, through
//! the read connections and inside a transaction; storing again replaces
//! it; deleting removes it; the purge deletes only expired streaks without a
//! running lock, at most its limit at a time; and the table refuses what
//! its CHECKs forbid.
//!
//! Each test gets a fresh database from `#[sqlx::test]` and opens it with
//! `Database::connect`, the production settings. Time isn't paused: sqlx's
//! worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use chrono::{DateTime, TimeDelta, TimeZone as _, Utc};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::audit::{AuditLimit, AuditPage};
use fleet_server::ports::login_attempts::{AttemptKey, LoginAttempt};
use fleet_server::ports::store::{Store, StoreError, UnauditedWrite};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// The time everything starts at, in milliseconds.
const T0: i64 = 1_800_000_000_000;
/// A stored time.
const STORED_AT: &str = "2027-01-15T08:00:00.000Z";

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

fn hours(n: i64) -> TimeDelta {
    TimeDelta::hours(n)
}

fn key(byte: u8) -> AttemptKey {
    AttemptKey::from_bytes([byte; 32])
}

async fn database(options: SqliteConnectOptions) -> Database {
    Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap()
}

/// A streak of `failures` whose last failure was at `last`, locked until
/// `locked_until`.
fn streak(failures: u32, last: DateTime<Utc>, locked_until: Option<DateTime<Utc>>) -> LoginAttempt {
    LoginAttempt {
        failures,
        last_failure_at: last,
        locked_until,
    }
}

/// Stores `attempt` under `key` and commits.
async fn store(db: &Database, key: &AttemptKey, attempt: &LoginAttempt) {
    let mut tx = db.write().await.unwrap();
    tx.login_attempts().upsert(key, attempt).await.unwrap();
    tx.commit_unaudited(UnauditedWrite::LoginAttemptsPurge)
        .await
        .unwrap();
}

/// What the read connections find under `key`.
async fn found(db: &Database, key: &AttemptKey) -> Option<LoginAttempt> {
    db.login_attempts().find(key).await.unwrap()
}

/// Runs one purge batch and commits it.
async fn purge(db: &Database, failed_by: DateTime<Utc>, now: DateTime<Utc>, limit: u32) -> u64 {
    let mut tx = db.write().await.unwrap();
    let deleted = tx
        .login_attempts()
        .purge_stale(failed_by, now, limit)
        .await
        .unwrap();
    tx.commit_unaudited(UnauditedWrite::LoginAttemptsPurge)
        .await
        .unwrap();
    deleted
}

async fn row_count(db: &Database) -> i64 {
    sqlx::query_scalar!(r#"SELECT count(*) AS "count!: i64" FROM login_attempts"#)
        .fetch_one(db.read_pool())
        .await
        .unwrap()
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

// --- Storing, finding and deleting ---------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_stored_streak_is_found_by_its_key(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let unlocked = streak(3, at(T0), None);
    let locked = streak(6, at(T0), Some(at(T0) + TimeDelta::minutes(2)));

    store(&db, &key(1), &unlocked).await;
    store(&db, &key(2), &locked).await;

    assert_eq!(found(&db, &key(1)).await, Some(unlocked));
    assert_eq!(found(&db, &key(2)).await, Some(locked));
}

#[sqlx::test(migrations = false)]
async fn a_streak_is_found_inside_a_transaction_too(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let stored = streak(2, at(T0), None);
    store(&db, &key(1), &stored).await;

    let mut tx = db.write().await.unwrap();
    let inside = tx.login_attempts().find(&key(1)).await.unwrap();
    let unknown = tx.login_attempts().find(&key(9)).await.unwrap();

    assert_eq!(inside, Some(stored));
    assert_eq!(unknown, None);
}

#[sqlx::test(migrations = false)]
async fn an_unknown_key_finds_nothing(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    store(&db, &key(1), &streak(1, at(T0), None)).await;

    assert_eq!(found(&db, &key(2)).await, None);
}

#[sqlx::test(migrations = false)]
async fn storing_again_replaces_the_streak(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    store(&db, &key(1), &streak(4, at(T0), None)).await;
    let later = streak(5, at(T0) + hours(1), Some(at(T0) + hours(1)));

    store(&db, &key(1), &later).await;

    assert_eq!(found(&db, &key(1)).await, Some(later));
    assert_eq!(row_count(&db).await, 1);
}

#[sqlx::test(migrations = false)]
async fn deleting_removes_the_streak_and_says_whether_there_was_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    store(&db, &key(1), &streak(4, at(T0), None)).await;
    store(&db, &key(2), &streak(1, at(T0), None)).await;

    let mut tx = db.write().await.unwrap();
    let deleted = tx.login_attempts().delete(&key(1)).await.unwrap();
    let again = tx.login_attempts().delete(&key(1)).await.unwrap();
    tx.commit_unaudited(UnauditedWrite::LoginAttemptsPurge)
        .await
        .unwrap();

    assert!(deleted);
    assert!(!again);
    assert_eq!(found(&db, &key(1)).await, None);
    assert!(found(&db, &key(2)).await.is_some(), "other keys stay");
}

#[sqlx::test(migrations = false)]
async fn a_dropped_transaction_stores_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;

    let mut tx = db.write().await.unwrap();
    tx.login_attempts()
        .upsert(&key(1), &streak(1, at(T0), None))
        .await
        .unwrap();
    drop(tx);

    assert_eq!(found(&db, &key(1)).await, None);
}

#[sqlx::test(migrations = false)]
async fn a_time_that_cant_be_stored_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let far = Utc.with_ymd_and_hms(10_000, 1, 1, 0, 0, 0).unwrap();

    let mut tx = db.write().await.unwrap();
    let last = tx
        .login_attempts()
        .upsert(&key(1), &streak(1, far, None))
        .await;
    let until = tx
        .login_attempts()
        .upsert(&key(1), &streak(5, at(T0), Some(far)))
        .await;

    assert!(
        matches!(
            last,
            Err(StoreError::Unstorable {
                table: "login_attempts",
                column: "last_failure_at"
            })
        ),
        "{last:?}"
    );
    assert!(
        matches!(
            until,
            Err(StoreError::Unstorable {
                table: "login_attempts",
                column: "locked_until"
            })
        ),
        "{until:?}"
    );
}

// --- The purge -------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn the_purge_deletes_only_expired_streaks_without_a_running_lock(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let now = at(T0) + hours(30);
    let failed_by = now - hours(24);
    // Expired, never locked: deleted.
    store(&db, &key(1), &streak(2, failed_by - hours(1), None)).await;
    // Expired exactly at the limit: deleted.
    store(&db, &key(2), &streak(2, failed_by, None)).await;
    // Expired, its lock long over: deleted.
    store(
        &db,
        &key(3),
        &streak(6, failed_by - hours(1), Some(failed_by)),
    )
    .await;
    // Expired, its lock ending exactly now: deleted.
    store(&db, &key(4), &streak(6, failed_by - hours(1), Some(now))).await;
    // A millisecond short of expired: kept.
    store(
        &db,
        &key(5),
        &streak(2, failed_by + TimeDelta::milliseconds(1), None),
    )
    .await;
    // Expired by its last failure, but a lock still runs: kept.
    store(
        &db,
        &key(6),
        &streak(9, failed_by - hours(1), Some(now + hours(1))),
    )
    .await;

    let deleted = purge(&db, failed_by, now, 1000).await;

    assert_eq!(deleted, 4);
    for gone in [1, 2, 3, 4] {
        assert_eq!(found(&db, &key(gone)).await, None, "key {gone}");
    }
    for kept in [5, 6] {
        assert!(found(&db, &key(kept)).await.is_some(), "key {kept}");
    }
}

#[sqlx::test(migrations = false)]
async fn the_purge_deletes_at_most_its_limit_and_counts_what_it_deleted(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let now = at(T0) + hours(30);
    for n in 1..=5 {
        store(&db, &key(n), &streak(1, at(T0), None)).await;
    }

    let first = purge(&db, now - hours(24), now, 3).await;
    let second = purge(&db, now - hours(24), now, 3).await;
    let third = purge(&db, now - hours(24), now, 3).await;

    assert_eq!((first, second, third), (3, 2, 0));
    assert_eq!(row_count(&db).await, 0);
}

#[sqlx::test(migrations = false)]
async fn the_purge_commits_without_an_audit_entry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let now = at(T0) + hours(30);
    store(&db, &key(1), &streak(1, at(T0), None)).await;
    let entries = audit_count(&db).await;

    assert_eq!(purge(&db, now - hours(24), now, 1000).await, 1);

    assert_eq!(row_count(&db).await, 0, "committed");
    assert_eq!(audit_count(&db).await, entries, "no entry");
}

// --- What the table refuses, and rows that don't read back ------------------------------

/// A row as raw column values, for inserts that bypass the repository.
#[derive(Clone)]
struct RawAttempt {
    username_key: Vec<u8>,
    failures: i64,
    last_failure_at: String,
    locked_until: Option<String>,
}

impl RawAttempt {
    fn valid() -> Self {
        Self {
            username_key: vec![7; 32],
            failures: 1,
            last_failure_at: STORED_AT.to_owned(),
            locked_until: None,
        }
    }

    async fn insert(&self, pool: &SqlitePool) -> Result<(), sqlx::Error> {
        let key = self.username_key.as_slice();
        sqlx::query!(
            "INSERT INTO login_attempts (username_key, failures, last_failure_at, locked_until)
             VALUES (?, ?, ?, ?)",
            key,
            self.failures,
            self.last_failure_at,
            self.locked_until
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
async fn a_valid_raw_row_is_accepted(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let row = RawAttempt {
        locked_until: Some(STORED_AT.to_owned()),
        ..RawAttempt::valid()
    };

    row.insert(db.write_pool()).await.unwrap();

    let stored_at = Utc.with_ymd_and_hms(2027, 1, 15, 8, 0, 0).unwrap();
    assert_eq!(
        found(&db, &key(7)).await,
        Some(streak(1, stored_at, Some(stored_at)))
    );
}

#[sqlx::test(migrations = false)]
async fn the_table_refuses_malformed_values(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let valid = RawAttempt::valid();
    let cases: Vec<(&str, RawAttempt)> = vec![
        (
            "a key of 31 bytes",
            RawAttempt {
                username_key: vec![7; 31],
                ..valid.clone()
            },
        ),
        (
            "a key of 33 bytes",
            RawAttempt {
                username_key: vec![7; 33],
                ..valid.clone()
            },
        ),
        (
            "no failures",
            RawAttempt {
                failures: 0,
                ..valid.clone()
            },
        ),
        (
            "negative failures",
            RawAttempt {
                failures: -1,
                ..valid.clone()
            },
        ),
        (
            "last_failure_at without milliseconds",
            RawAttempt {
                last_failure_at: "2027-01-15T08:00:00Z".to_owned(),
                ..valid.clone()
            },
        ),
        (
            "last_failure_at with an offset",
            RawAttempt {
                last_failure_at: "2027-01-15T08:00:00.000+00:00".to_owned(),
                ..valid.clone()
            },
        ),
        (
            "an empty locked_until",
            RawAttempt {
                locked_until: Some(String::new()),
                ..valid.clone()
            },
        ),
        (
            "a date-only locked_until",
            RawAttempt {
                locked_until: Some("2027-01-15".to_owned()),
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
async fn a_row_that_doesnt_read_back_is_corrupt_naming_its_column(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let cases = [
        (
            "failures",
            RawAttempt {
                username_key: vec![1; 32],
                failures: i64::from(u32::MAX) + 1,
                ..RawAttempt::valid()
            },
        ),
        (
            "last_failure_at",
            RawAttempt {
                username_key: vec![2; 32],
                last_failure_at: "2027-13-45T99:99:99.999Z".to_owned(),
                ..RawAttempt::valid()
            },
        ),
        (
            "locked_until",
            RawAttempt {
                username_key: vec![3; 32],
                locked_until: Some("2027-02-30T08:00:00.000Z".to_owned()),
                ..RawAttempt::valid()
            },
        ),
    ];

    for (column, row) in cases {
        row.insert(db.write_pool()).await.unwrap();
        let key = AttemptKey::try_from(row.username_key.as_slice()).unwrap();

        let read = db.login_attempts().find(&key).await;

        assert!(
            matches!(
                read,
                Err(StoreError::Corrupt { table: "login_attempts", column: found, .. })
                    if found == column
            ),
            "{column}: {read:?}"
        );
    }
}
