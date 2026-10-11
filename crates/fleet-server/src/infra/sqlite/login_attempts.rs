//! The `login_attempts` table's SQL: one row per username with a running
//! failure streak, keyed by its [`AttemptKey`], bound as its 32 bytes.
//! Stored times compare as text in time order.

use chrono::{DateTime, Utc};
use sqlx::{Executor, Sqlite, SqliteConnection};

use super::convert::{parse_time, time_text};
use super::error::store_error;
use crate::ports::login_attempts::{AttemptKey, LoginAttempt};
use crate::ports::store::StoreError;

/// The table, as errors name it.
const TABLE: &str = "login_attempts";

/// A row, before its values are checked.
struct AttemptRow {
    rowid: i64,
    failures: i64,
    last_failure_at: String,
    locked_until: Option<String>,
}

/// The stored form of `at` in `column`, or [`StoreError::Unstorable`].
fn stored_time(at: DateTime<Utc>, column: &'static str) -> Result<String, StoreError> {
    time_text(at).ok_or(StoreError::Unstorable {
        table: TABLE,
        column,
    })
}

/// The streak stored under `key`, through the read pool or inside a
/// transaction.
pub(super) async fn find<'e, E>(
    executor: E,
    key: &AttemptKey,
) -> Result<Option<LoginAttempt>, StoreError>
where
    E: Executor<'e, Database = Sqlite>,
{
    let key = key.as_bytes().as_slice();
    sqlx::query_as!(
        AttemptRow,
        r#"SELECT rowid AS "rowid!: i64", failures AS "failures!: i64",
                  last_failure_at AS "last_failure_at!", locked_until AS "locked_until?"
           FROM login_attempts WHERE username_key = ?"#,
        key
    )
    .fetch_optional(executor)
    .await
    .map_err(store_error)?
    .map(|row| attempt_from_row(&row))
    .transpose()
}

/// Stores `attempt` under `key`, replacing any streak stored there.
pub(super) async fn upsert(
    conn: &mut SqliteConnection,
    key: &AttemptKey,
    attempt: &LoginAttempt,
) -> Result<(), StoreError> {
    let key = key.as_bytes().as_slice();
    let failures = i64::from(attempt.failures);
    let last_failure_at = stored_time(attempt.last_failure_at, "last_failure_at")?;
    let locked_until = attempt
        .locked_until
        .map(|until| stored_time(until, "locked_until"))
        .transpose()?;
    sqlx::query!(
        "INSERT INTO login_attempts (username_key, failures, last_failure_at, locked_until)
         VALUES (?, ?, ?, ?)
         ON CONFLICT (username_key) DO UPDATE SET
             failures = excluded.failures,
             last_failure_at = excluded.last_failure_at,
             locked_until = excluded.locked_until",
        key,
        failures,
        last_failure_at,
        locked_until
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(())
}

/// Deletes the streak stored under `key`; whether there was one.
pub(super) async fn delete(
    conn: &mut SqliteConnection,
    key: &AttemptKey,
) -> Result<bool, StoreError> {
    let key = key.as_bytes().as_slice();
    let done = sqlx::query!("DELETE FROM login_attempts WHERE username_key = ?", key)
        .execute(conn)
        .await
        .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Deletes at most `limit` streaks whose last failure is at or before
/// `failed_by` and whose lock, if any, ended at or before `now`. SQLite's
/// `DELETE … LIMIT` needs a compile option the bundled library lacks, so
/// the rows are chosen by a subquery.
pub(super) async fn purge_stale(
    conn: &mut SqliteConnection,
    failed_by: DateTime<Utc>,
    now: DateTime<Utc>,
    limit: u32,
) -> Result<u64, StoreError> {
    let failed_by = stored_time(failed_by, "last_failure_at")?;
    let now = stored_time(now, "locked_until")?;
    let limit = i64::from(limit);
    let done = sqlx::query!(
        "DELETE FROM login_attempts WHERE rowid IN (
             SELECT rowid FROM login_attempts
             WHERE last_failure_at <= ? AND (locked_until IS NULL OR locked_until <= ?)
             LIMIT ?
         )",
        failed_by,
        now,
        limit
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected())
}

/// Checks a row's values; one that doesn't read back is
/// [`StoreError::Corrupt`], naming its column and rowid, never the value.
fn attempt_from_row(row: &AttemptRow) -> Result<LoginAttempt, StoreError> {
    let corrupt = |column| StoreError::Corrupt {
        table: TABLE,
        column,
        rowid: row.rowid,
    };
    Ok(LoginAttempt {
        failures: u32::try_from(row.failures).map_err(|_| corrupt("failures"))?,
        last_failure_at: parse_time(&row.last_failure_at)
            .ok_or_else(|| corrupt("last_failure_at"))?,
        locked_until: row
            .locked_until
            .as_deref()
            .map(|text| parse_time(text).ok_or_else(|| corrupt("locked_until")))
            .transpose()?,
    })
}
