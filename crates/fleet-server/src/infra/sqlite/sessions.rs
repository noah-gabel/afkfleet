//! The `sessions` and `rotated_refresh_tokens` tables' SQL.
//!
//! Every lookup joins `users`, so a session comes with its user's current
//! role and status; a session whose user row is gone isn't found. Hashes are
//! bound as their 32 bytes.

use chrono::{DateTime, Utc};
use fleet_core::id::{SessionId, UserId};
use sqlx::{SqliteConnection, SqlitePool};

use super::convert::{id_text, parse_id, parse_time, time_text};
use super::error::store_error;
use crate::ports::page::Page;
use crate::ports::sessions::{
    LiveSince, NewSession, RefreshLookup, Rotation, Session, SessionWithUser,
};
use crate::ports::store::StoreError;
use crate::ports::tokens::TokenHash;

/// The sessions table, as errors name it.
const SESSIONS: &str = "sessions";
/// The history table, as errors name it.
const HISTORY: &str = "rotated_refresh_tokens";
/// The users table, as errors name it.
const USERS: &str = "users";

/// A session row with its user's role and status, before its values are
/// checked.
struct SessionRow {
    rowid: i64,
    id: String,
    user_id: String,
    access_hash: Vec<u8>,
    refresh_hash: Vec<u8>,
    created_at: String,
    refreshed_at: String,
    last_used_at: String,
    stepped_up_at: Option<String>,
    revoked_at: Option<String>,
    user_rowid: i64,
    role: String,
    disabled: i64,
}

/// A history row, before its values are checked.
struct HistoryRow {
    rowid: i64,
    hash: Vec<u8>,
    session_id: String,
    rotated_at: String,
}

/// The stored form of `at` in `column`, or [`StoreError::Unstorable`].
fn stored_time(
    at: DateTime<Utc>,
    table: &'static str,
    column: &'static str,
) -> Result<String, StoreError> {
    time_text(at).ok_or(StoreError::Unstorable { table, column })
}

/// Inserts `session`, with `refreshed_at` and `last_used_at` = `created_at`.
pub(super) async fn insert(
    conn: &mut SqliteConnection,
    session: &NewSession,
) -> Result<(), StoreError> {
    let id = id_text(session.id);
    let user_id = id_text(session.user_id);
    let access_hash = session.access_hash.as_bytes().as_slice();
    let refresh_hash = session.refresh_hash.as_bytes().as_slice();
    let created_at = stored_time(session.created_at, SESSIONS, "created_at")?;
    sqlx::query!(
        "INSERT INTO sessions (id, user_id, access_hash, refresh_hash, created_at, refreshed_at,
                               last_used_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        id,
        user_id,
        access_hash,
        refresh_hash,
        created_at,
        created_at,
        created_at
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(())
}

/// The session whose current access token hashes to `hash`, with its user.
pub(super) async fn find_by_access_hash(
    pool: &SqlitePool,
    hash: &TokenHash,
) -> Result<Option<SessionWithUser>, StoreError> {
    let hash = hash.as_bytes().as_slice();
    sqlx::query_as!(
        SessionRow,
        r#"SELECT s.rowid AS "rowid!: i64", s.id AS "id!", s.user_id AS "user_id!",
                  s.access_hash AS "access_hash!: Vec<u8>",
                  s.refresh_hash AS "refresh_hash!: Vec<u8>",
                  s.created_at AS "created_at!", s.refreshed_at AS "refreshed_at!",
                  s.last_used_at AS "last_used_at!", s.stepped_up_at AS "stepped_up_at?",
                  s.revoked_at AS "revoked_at?", u.rowid AS "user_rowid!: i64",
                  u.role AS "role!", u.disabled AS "disabled!: i64"
           FROM sessions s JOIN users u ON u.id = s.user_id
           WHERE s.access_hash = ?"#,
        hash
    )
    .fetch_optional(pool)
    .await
    .map_err(store_error)?
    .map(|row| session_from_row(&row))
    .transpose()
}

/// The session a refresh token's `hash` belongs to: as its current token,
/// or as one in its history.
pub(super) async fn find_by_refresh_hash(
    conn: &mut SqliteConnection,
    hash: &TokenHash,
) -> Result<Option<RefreshLookup>, StoreError> {
    let bytes = hash.as_bytes().as_slice();
    let current = sqlx::query_as!(
        SessionRow,
        r#"SELECT s.rowid AS "rowid!: i64", s.id AS "id!", s.user_id AS "user_id!",
                  s.access_hash AS "access_hash!: Vec<u8>",
                  s.refresh_hash AS "refresh_hash!: Vec<u8>",
                  s.created_at AS "created_at!", s.refreshed_at AS "refreshed_at!",
                  s.last_used_at AS "last_used_at!", s.stepped_up_at AS "stepped_up_at?",
                  s.revoked_at AS "revoked_at?", u.rowid AS "user_rowid!: i64",
                  u.role AS "role!", u.disabled AS "disabled!: i64"
           FROM sessions s JOIN users u ON u.id = s.user_id
           WHERE s.refresh_hash = ?"#,
        bytes
    )
    .fetch_optional(&mut *conn)
    .await
    .map_err(store_error)?;
    if let Some(row) = current {
        return session_from_row(&row).map(|session| Some(RefreshLookup::Current(session)));
    }
    let Some(history) = sqlx::query_as!(
        HistoryRow,
        r#"SELECT rowid AS "rowid!: i64", hash AS "hash!: Vec<u8>", session_id AS "session_id!",
                  rotated_at AS "rotated_at!"
           FROM rotated_refresh_tokens WHERE hash = ?"#,
        bytes
    )
    .fetch_optional(&mut *conn)
    .await
    .map_err(store_error)?
    else {
        return Ok(None);
    };
    let corrupt = |column| StoreError::Corrupt {
        table: HISTORY,
        column,
        rowid: history.rowid,
    };
    let stored = TokenHash::try_from(history.hash.as_slice()).map_err(|_| corrupt("hash"))?;
    let rotated_at = parse_time(&history.rotated_at).ok_or_else(|| corrupt("rotated_at"))?;
    let session_id: SessionId =
        parse_id(&history.session_id).ok_or_else(|| corrupt("session_id"))?;
    let session = find_by_id(conn, session_id).await?;
    Ok(session.map(|session| RefreshLookup::Rotated {
        session,
        hash: stored,
        rotated_at,
    }))
}

/// Whether `hash` is a current or replaced refresh token.
pub(super) async fn refresh_hash_exists(
    pool: &SqlitePool,
    hash: &TokenHash,
) -> Result<bool, StoreError> {
    let hash = hash.as_bytes().as_slice();
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM sessions WHERE refresh_hash = ?)
               OR EXISTS (SELECT 1 FROM rotated_refresh_tokens WHERE hash = ?) AS "exists!: bool""#,
        hash,
        hash
    )
    .fetch_one(pool)
    .await
    .map_err(store_error)
}

/// Revokes every live session of `user` but `except`.
pub(super) async fn revoke_all_for_user(
    conn: &mut SqliteConnection,
    user: UserId,
    except: Option<SessionId>,
    at: DateTime<Utc>,
) -> Result<u64, StoreError> {
    let user = id_text(user);
    let except = except.map(id_text);
    let at = stored_time(at, SESSIONS, "revoked_at")?;
    let done = sqlx::query!(
        "UPDATE sessions SET revoked_at = ?
         WHERE user_id = ? AND revoked_at IS NULL AND (? IS NULL OR id <> ?)",
        at,
        user,
        except,
        except
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected())
}

/// Stamps `stepped_up_at` on a session that isn't revoked.
pub(super) async fn stamp_step_up(
    conn: &mut SqliteConnection,
    id: SessionId,
    at: DateTime<Utc>,
) -> Result<bool, StoreError> {
    let id = id_text(id);
    let at = stored_time(at, SESSIONS, "stepped_up_at")?;
    let done = sqlx::query!(
        "UPDATE sessions SET stepped_up_at = ? WHERE id = ? AND revoked_at IS NULL",
        at,
        id
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Ends a session's step-up.
pub(super) async fn clear_step_up(
    conn: &mut SqliteConnection,
    id: SessionId,
) -> Result<bool, StoreError> {
    let id = id_text(id);
    let done = sqlx::query!("UPDATE sessions SET stepped_up_at = NULL WHERE id = ?", id)
        .execute(conn)
        .await
        .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// The user's live sessions of `page`, newest first: one more than the page
/// holds. Times compare as text, in their one stored form.
pub(super) async fn list_live(
    pool: &SqlitePool,
    user: UserId,
    live: LiveSince,
    page: Page<SessionId>,
) -> Result<Vec<Session>, StoreError> {
    let user = id_text(user);
    let refreshed_after = stored_time(live.refreshed_after, SESSIONS, "refreshed_at")?;
    let created_after = stored_time(live.created_after, SESSIONS, "created_at")?;
    let before = page.before.map(|id| id_text(id).to_string());
    let fetch = page.limit.fetch();
    sqlx::query_as!(
        SessionRow,
        r#"SELECT s.rowid AS "rowid!: i64", s.id AS "id!", s.user_id AS "user_id!",
                  s.access_hash AS "access_hash!: Vec<u8>",
                  s.refresh_hash AS "refresh_hash!: Vec<u8>",
                  s.created_at AS "created_at!", s.refreshed_at AS "refreshed_at!",
                  s.last_used_at AS "last_used_at!", s.stepped_up_at AS "stepped_up_at?",
                  s.revoked_at AS "revoked_at?", u.rowid AS "user_rowid!: i64",
                  u.role AS "role!", u.disabled AS "disabled!: i64"
           FROM sessions s JOIN users u ON u.id = s.user_id
           WHERE s.user_id = ? AND s.revoked_at IS NULL
                 AND s.refreshed_at > ? AND s.created_at > ?
                 AND (? IS NULL OR s.id < ?)
           ORDER BY s.id DESC
           LIMIT ?"#,
        user,
        refreshed_after,
        created_after,
        before,
        before,
        fetch
    )
    .fetch_all(pool)
    .await
    .map_err(store_error)?
    .iter()
    .map(|row| session_from_row(row).map(|found| found.session))
    .collect()
}

/// The session with this ID, with its user: in a write transaction, or on
/// the read pool.
pub(super) async fn find_by_id<'c, E>(
    executor: E,
    id: SessionId,
) -> Result<Option<SessionWithUser>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let id = id_text(id);
    sqlx::query_as!(
        SessionRow,
        r#"SELECT s.rowid AS "rowid!: i64", s.id AS "id!", s.user_id AS "user_id!",
                  s.access_hash AS "access_hash!: Vec<u8>",
                  s.refresh_hash AS "refresh_hash!: Vec<u8>",
                  s.created_at AS "created_at!", s.refreshed_at AS "refreshed_at!",
                  s.last_used_at AS "last_used_at!", s.stepped_up_at AS "stepped_up_at?",
                  s.revoked_at AS "revoked_at?", u.rowid AS "user_rowid!: i64",
                  u.role AS "role!", u.disabled AS "disabled!: i64"
           FROM sessions s JOIN users u ON u.id = s.user_id
           WHERE s.id = ?"#,
        id
    )
    .fetch_optional(executor)
    .await
    .map_err(store_error)?
    .map(|row| session_from_row(&row))
    .transpose()
}

/// Replaces the session's tokens if its current refresh token is the
/// replaced one, then keeps the replaced hash in the history.
pub(super) async fn rotate(
    conn: &mut SqliteConnection,
    rotation: &Rotation,
) -> Result<bool, StoreError> {
    let id = id_text(rotation.session_id);
    let replaced = rotation.replaced.as_bytes().as_slice();
    let access_hash = rotation.access_hash.as_bytes().as_slice();
    let refresh_hash = rotation.refresh_hash.as_bytes().as_slice();
    let at = stored_time(rotation.at, SESSIONS, "refreshed_at")?;
    let done = sqlx::query!(
        "UPDATE sessions SET access_hash = ?, refresh_hash = ?, refreshed_at = ?
         WHERE id = ? AND refresh_hash = ?",
        access_hash,
        refresh_hash,
        at,
        id,
        replaced
    )
    .execute(&mut *conn)
    .await
    .map_err(store_error)?;
    if done.rows_affected() == 0 {
        return Ok(false);
    }
    sqlx::query!(
        "INSERT INTO rotated_refresh_tokens (hash, session_id, rotated_at) VALUES (?, ?, ?)",
        replaced,
        id,
        at
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(true)
}

/// Stamps `revoked_at` on a session that isn't revoked yet.
pub(super) async fn revoke(
    conn: &mut SqliteConnection,
    id: SessionId,
    at: DateTime<Utc>,
) -> Result<bool, StoreError> {
    let id = id_text(id);
    let at = stored_time(at, SESSIONS, "revoked_at")?;
    let done = sqlx::query!(
        "UPDATE sessions SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL",
        at,
        id
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Sets `last_used_at` to `at` when the stored value is at or before
/// `stale_since`. Stored times compare as text in time order.
pub(super) async fn touch(
    conn: &mut SqliteConnection,
    id: SessionId,
    at: DateTime<Utc>,
    stale_since: DateTime<Utc>,
) -> Result<bool, StoreError> {
    let id = id_text(id);
    let at = stored_time(at, SESSIONS, "last_used_at")?;
    let stale_since = stored_time(stale_since, SESSIONS, "last_used_at")?;
    let done = sqlx::query!(
        "UPDATE sessions SET last_used_at = ? WHERE id = ? AND last_used_at <= ?",
        at,
        id,
        stale_since
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Checks every value of a joined row; one that doesn't read back is
/// [`StoreError::Corrupt`], naming its table, column and rowid, never the
/// value.
fn session_from_row(row: &SessionRow) -> Result<SessionWithUser, StoreError> {
    let corrupt = |column| StoreError::Corrupt {
        table: SESSIONS,
        column,
        rowid: row.rowid,
    };
    let user_corrupt = |column| StoreError::Corrupt {
        table: USERS,
        column,
        rowid: row.user_rowid,
    };
    let time = |text: &str, column| parse_time(text).ok_or_else(|| corrupt(column));
    let optional_time =
        |text: Option<&str>, column| text.map(|text| time(text, column)).transpose();
    let session = Session {
        id: parse_id(&row.id).ok_or_else(|| corrupt("id"))?,
        user_id: parse_id(&row.user_id).ok_or_else(|| corrupt("user_id"))?,
        access_hash: TokenHash::try_from(row.access_hash.as_slice())
            .map_err(|_| corrupt("access_hash"))?,
        refresh_hash: TokenHash::try_from(row.refresh_hash.as_slice())
            .map_err(|_| corrupt("refresh_hash"))?,
        created_at: time(&row.created_at, "created_at")?,
        refreshed_at: time(&row.refreshed_at, "refreshed_at")?,
        last_used_at: time(&row.last_used_at, "last_used_at")?,
        stepped_up_at: optional_time(row.stepped_up_at.as_deref(), "stepped_up_at")?,
        revoked_at: optional_time(row.revoked_at.as_deref(), "revoked_at")?,
    };
    Ok(SessionWithUser {
        session,
        role: row.role.parse().map_err(|_| user_corrupt("role"))?,
        disabled: match row.disabled {
            0 => false,
            1 => true,
            _ => return Err(user_corrupt("disabled")),
        },
    })
}
