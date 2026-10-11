//! The `user_mfa` and `recovery_codes` tables' SQL.
//!
//! The secret's envelope and the hashes are bound as their bytes; a time
//! step as an INTEGER. A step is a `u64` in Rust and an `i64` in SQLite: one
//! beyond `i64::MAX` can't be stored, and a stored negative one (which the
//! CHECK forbids) doesn't read back.

use chrono::{DateTime, Utc};
use fleet_core::id::UserId;
use sqlx::{SqliteConnection, SqlitePool};

use super::convert::{id_text, parse_id, parse_time, time_text};
use super::error::{store_error, unique_violation};
use crate::ports::mfa::{InsertMfaError, UserMfa};
use crate::ports::recovery_codes::RecoveryCodeHash;
use crate::ports::store::StoreError;

/// The authenticators' table, as errors name it.
const USER_MFA: &str = "user_mfa";
/// The recovery codes' table, as errors name it.
const RECOVERY_CODES: &str = "recovery_codes";

/// A `user_mfa` row, before its values are checked.
struct MfaRow {
    rowid: i64,
    user_id: String,
    secret_envelope: Vec<u8>,
    last_used_step: i64,
    confirmed_at: String,
}

/// A `recovery_codes` row's hash, before its length is checked.
struct CodeRow {
    rowid: i64,
    hash: Vec<u8>,
}

/// The stored form of `at` in `column`, or [`StoreError::Unstorable`].
fn stored_time(
    at: DateTime<Utc>,
    table: &'static str,
    column: &'static str,
) -> Result<String, StoreError> {
    time_text(at).ok_or(StoreError::Unstorable { table, column })
}

/// The stored form of a time step, or [`StoreError::Unstorable`] beyond
/// SQLite's integers.
fn stored_step(step: u64) -> Result<i64, StoreError> {
    i64::try_from(step).map_err(|_| StoreError::Unstorable {
        table: USER_MFA,
        column: "last_used_step",
    })
}

/// The user's authenticator.
pub(super) async fn find<'c, E>(executor: E, user: UserId) -> Result<Option<UserMfa>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let user = id_text(user);
    sqlx::query_as!(
        MfaRow,
        r#"SELECT rowid AS "rowid!: i64", user_id AS "user_id!",
                  secret_envelope AS "secret_envelope!: Vec<u8>",
                  last_used_step AS "last_used_step!: i64", confirmed_at AS "confirmed_at!"
           FROM user_mfa WHERE user_id = ?"#,
        user
    )
    .fetch_optional(executor)
    .await
    .map_err(store_error)?
    .map(mfa_from_row)
    .transpose()
}

/// The stored hash of the user's unused recovery code `hash`.
pub(super) async fn find_unused_code(
    pool: &SqlitePool,
    user: UserId,
    hash: &RecoveryCodeHash,
) -> Result<Option<RecoveryCodeHash>, StoreError> {
    let user = id_text(user);
    let hash = hash.as_bytes().as_slice();
    let Some(row) = sqlx::query_as!(
        CodeRow,
        r#"SELECT rowid AS "rowid!: i64", hash AS "hash!: Vec<u8>"
           FROM recovery_codes WHERE hash = ? AND user_id = ? AND used_at IS NULL"#,
        hash,
        user
    )
    .fetch_optional(pool)
    .await
    .map_err(store_error)?
    else {
        return Ok(None);
    };
    RecoveryCodeHash::try_from(row.hash.as_slice())
        .map(Some)
        .map_err(|_| StoreError::Corrupt {
            table: RECOVERY_CODES,
            column: "hash",
            rowid: row.rowid,
        })
}

/// Inserts a first enrollment.
pub(super) async fn insert(
    conn: &mut SqliteConnection,
    mfa: &UserMfa,
) -> Result<(), InsertMfaError> {
    let user = id_text(mfa.user_id);
    let envelope = mfa.secret_envelope.as_slice();
    let step = stored_step(mfa.last_used_step)?;
    let confirmed_at = stored_time(mfa.confirmed_at, USER_MFA, "confirmed_at")?;
    sqlx::query!(
        "INSERT INTO user_mfa (user_id, secret_envelope, last_used_step, confirmed_at)
         VALUES (?, ?, ?, ?)",
        user,
        envelope,
        step,
        confirmed_at
    )
    .execute(conn)
    .await
    .map_err(|error| match unique_violation(&error).as_deref() {
        Some("user_mfa.user_id") => InsertMfaError::Exists,
        _ => InsertMfaError::Store(store_error(error)),
    })?;
    Ok(())
}

/// Replaces the user's authenticator.
pub(super) async fn replace(
    conn: &mut SqliteConnection,
    mfa: &UserMfa,
) -> Result<bool, StoreError> {
    let user = id_text(mfa.user_id);
    let envelope = mfa.secret_envelope.as_slice();
    let step = stored_step(mfa.last_used_step)?;
    let confirmed_at = stored_time(mfa.confirmed_at, USER_MFA, "confirmed_at")?;
    let done = sqlx::query!(
        "UPDATE user_mfa SET secret_envelope = ?, last_used_step = ?, confirmed_at = ?
         WHERE user_id = ?",
        envelope,
        step,
        confirmed_at,
        user
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Sets `last_used_step` while it's before `step` and the authenticator is
/// the one confirmed at `confirmed_at`. Stored times compare as text.
pub(super) async fn advance_step(
    conn: &mut SqliteConnection,
    user: UserId,
    step: u64,
    confirmed_at: DateTime<Utc>,
) -> Result<bool, StoreError> {
    let user = id_text(user);
    let step = stored_step(step)?;
    let confirmed_at = stored_time(confirmed_at, USER_MFA, "confirmed_at")?;
    let done = sqlx::query!(
        "UPDATE user_mfa SET last_used_step = ?
         WHERE user_id = ? AND last_used_step < ? AND confirmed_at = ?",
        step,
        user,
        step,
        confirmed_at
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Replaces every recovery code of the user.
pub(super) async fn replace_codes(
    conn: &mut SqliteConnection,
    user: UserId,
    hashes: &[RecoveryCodeHash],
) -> Result<(), StoreError> {
    let user = id_text(user);
    sqlx::query!("DELETE FROM recovery_codes WHERE user_id = ?", user)
        .execute(&mut *conn)
        .await
        .map_err(store_error)?;
    for hash in hashes {
        let hash = hash.as_bytes().as_slice();
        sqlx::query!(
            "INSERT INTO recovery_codes (hash, user_id, used_at) VALUES (?, ?, NULL)",
            hash,
            user
        )
        .execute(&mut *conn)
        .await
        .map_err(store_error)?;
    }
    Ok(())
}

/// Marks an unused recovery code used.
pub(super) async fn use_code(
    conn: &mut SqliteConnection,
    user: UserId,
    hash: &RecoveryCodeHash,
    at: DateTime<Utc>,
) -> Result<bool, StoreError> {
    let user = id_text(user);
    let hash = hash.as_bytes().as_slice();
    let at = stored_time(at, RECOVERY_CODES, "used_at")?;
    let done = sqlx::query!(
        "UPDATE recovery_codes SET used_at = ?
         WHERE hash = ? AND user_id = ? AND used_at IS NULL",
        at,
        hash,
        user
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Checks every value of a stored row; one that doesn't read back is
/// [`StoreError::Corrupt`], naming the column and the rowid, never the
/// value.
fn mfa_from_row(row: MfaRow) -> Result<UserMfa, StoreError> {
    let corrupt = |column| StoreError::Corrupt {
        table: USER_MFA,
        column,
        rowid: row.rowid,
    };
    Ok(UserMfa {
        user_id: parse_id(&row.user_id).ok_or_else(|| corrupt("user_id"))?,
        last_used_step: u64::try_from(row.last_used_step).map_err(|_| corrupt("last_used_step"))?,
        confirmed_at: parse_time(&row.confirmed_at).ok_or_else(|| corrupt("confirmed_at"))?,
        secret_envelope: row.secret_envelope,
    })
}
