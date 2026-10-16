//! The `enrollment_tokens` table's SQL.
//!
//! The token's hash is bound as its bytes. Expiry is compared as text:
//! stored times have one fixed form, so text order is time order. Using a
//! token checks, in its own `WHERE`, that it's still unused and unexpired, so
//! two transactions that both checked it first can't both use it.

use chrono::{DateTime, Utc};
use fleet_core::id::{AgentId, EnrollmentTokenId};
use sqlx::SqliteConnection;

use super::convert::{id_text, parse_id, parse_time, time_text};
use super::error::store_error;
use crate::ports::enrollment_tokens::{EnrollmentToken, NewEnrollmentToken};
use crate::ports::store::StoreError;
use crate::ports::tokens::TokenHash;

/// The table, as errors name it.
const TABLE: &str = "enrollment_tokens";

/// A row as stored, before its values are checked.
struct TokenRow {
    rowid: i64,
    id: String,
    token_hash: Vec<u8>,
    created_by: Option<String>,
    created_at: String,
    expires_at: String,
    used_at: Option<String>,
    agent_id: Option<String>,
}

/// The stored form of `at` in `column`, or [`StoreError::Unstorable`].
fn stored_time(at: DateTime<Utc>, column: &'static str) -> Result<String, StoreError> {
    time_text(at).ok_or(StoreError::Unstorable {
        table: TABLE,
        column,
    })
}

/// The token whose text hashes to `hash`.
pub(super) async fn find_by_hash<'c, E>(
    executor: E,
    hash: &TokenHash,
) -> Result<Option<EnrollmentToken>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let hash = hash.as_bytes().as_slice();
    sqlx::query_as!(
        TokenRow,
        r#"SELECT rowid AS "rowid!: i64", id AS "id!", token_hash AS "token_hash!: Vec<u8>",
                  created_by AS "created_by?", created_at AS "created_at!",
                  expires_at AS "expires_at!", used_at AS "used_at?", agent_id AS "agent_id?"
           FROM enrollment_tokens WHERE token_hash = ?"#,
        hash
    )
    .fetch_optional(executor)
    .await
    .map_err(store_error)?
    .map(token_from_row)
    .transpose()
}

/// Inserts `token`, unused.
pub(super) async fn insert(
    conn: &mut SqliteConnection,
    token: &NewEnrollmentToken,
) -> Result<(), StoreError> {
    let id = id_text(token.id);
    let hash = token.token_hash.as_bytes().as_slice();
    let created_by = token.created_by.map(id_text);
    let created_at = stored_time(token.created_at, "created_at")?;
    let expires_at = stored_time(token.expires_at, "expires_at")?;
    sqlx::query!(
        "INSERT INTO enrollment_tokens (id, token_hash, created_by, created_at, expires_at)
         VALUES (?, ?, ?, ?, ?)",
        id,
        hash,
        created_by,
        created_at,
        expires_at
    )
    .execute(conn)
    .await
    .map(drop)
    .map_err(store_error)
}

/// How many tokens are unused and unexpired at `now`.
pub(super) async fn count_pending(
    conn: &mut SqliteConnection,
    now: DateTime<Utc>,
) -> Result<u32, StoreError> {
    let now = stored_time(now, "expires_at")?;
    let count = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!: i64" FROM enrollment_tokens
           WHERE used_at IS NULL AND expires_at > ?"#,
        now
    )
    .fetch_one(conn)
    .await
    .map_err(store_error)?;
    // A count is never negative, and never near 2^32 rows.
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

/// Marks token `id` used by `agent` at `now`, while it's unused and
/// unexpired.
pub(super) async fn use_token(
    conn: &mut SqliteConnection,
    id: EnrollmentTokenId,
    agent: AgentId,
    now: DateTime<Utc>,
) -> Result<bool, StoreError> {
    let id = id_text(id);
    let agent = id_text(agent);
    let now = stored_time(now, "used_at")?;
    let done = sqlx::query!(
        "UPDATE enrollment_tokens SET used_at = ?, agent_id = ?
         WHERE id = ? AND used_at IS NULL AND expires_at > ?",
        now,
        agent,
        id,
        now
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Reads a row back, or [`StoreError::Corrupt`] naming the first column that
/// doesn't.
fn token_from_row(row: TokenRow) -> Result<EnrollmentToken, StoreError> {
    let rowid = row.rowid;
    let corrupt = |column| StoreError::Corrupt {
        table: TABLE,
        column,
        rowid,
    };
    let token_hash =
        TokenHash::try_from(row.token_hash.as_slice()).map_err(|_| corrupt("token_hash"))?;
    let created_by = row
        .created_by
        .map(|id| parse_id(&id).ok_or_else(|| corrupt("created_by")))
        .transpose()?;
    let used_at = row
        .used_at
        .map(|at| parse_time(&at).ok_or_else(|| corrupt("used_at")))
        .transpose()?;
    let agent = row
        .agent_id
        .map(|id| parse_id(&id).ok_or_else(|| corrupt("agent_id")))
        .transpose()?;
    Ok(EnrollmentToken {
        id: parse_id(&row.id).ok_or_else(|| corrupt("id"))?,
        token_hash,
        created_by,
        created_at: parse_time(&row.created_at).ok_or_else(|| corrupt("created_at"))?,
        expires_at: parse_time(&row.expires_at).ok_or_else(|| corrupt("expires_at"))?,
        used_at,
        agent,
    })
}
