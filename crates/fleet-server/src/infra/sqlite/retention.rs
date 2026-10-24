//! The retention's deletes and the reads of where it cuts (Plan.md P11.6;
//! ADR-0020, S45, S46, R50).
//!
//! SQLite's `DELETE … LIMIT` needs a compile option the bundled library
//! lacks, so each delete picks its rows by a subquery over an index, the
//! oldest first, as the lockout purge does. Times compare as text: the
//! stored form sorts in time order (`convert`).

use chrono::{DateTime, Utc};
use fleet_core::id::BotId;
use sqlx::{Executor, Sqlite, SqliteConnection};

use super::convert::{id_text, parse_id, time_text};
use super::error::store_error;
use crate::ports::audit::AuditEntryId;
use crate::ports::chat::ChatMessageId;
use crate::ports::store::StoreError;

/// The stored form of `at` in `table`'s `column`, or
/// [`StoreError::Unstorable`].
fn stored_time(
    at: DateTime<Utc>,
    table: &'static str,
    column: &'static str,
) -> Result<String, StoreError> {
    time_text(at).ok_or(StoreError::Unstorable { table, column })
}

/// Every bot's ID; one that doesn't read back is [`StoreError::Corrupt`].
pub(super) async fn bots<'e, E>(executor: E) -> Result<Vec<BotId>, StoreError>
where
    E: Executor<'e, Database = Sqlite>,
{
    let rows = sqlx::query!(r#"SELECT rowid AS "rowid!: i64", id FROM bots ORDER BY id"#)
        .fetch_all(executor)
        .await
        .map_err(store_error)?;
    rows.into_iter()
        .map(|row| {
            parse_id(&row.id).ok_or(StoreError::Corrupt {
                table: "bots",
                column: "id",
                rowid: row.rowid,
            })
        })
        .collect()
}

/// The ID of `bot`'s line just past its newest `keep`: its (`keep` + 1)th
/// newest, found by a walk of the `(bot_id, id)` index.
pub(super) async fn chat_cut<'e, E>(
    executor: E,
    bot: BotId,
    keep: u32,
) -> Result<Option<ChatMessageId>, StoreError>
where
    E: Executor<'e, Database = Sqlite>,
{
    let bot = id_text(bot);
    let keep = i64::from(keep);
    let cut = sqlx::query_scalar!(
        r#"SELECT id AS "id!: i64" FROM chat_messages WHERE bot_id = ?
           ORDER BY id DESC LIMIT 1 OFFSET ?"#,
        bot,
        keep
    )
    .fetch_optional(executor)
    .await
    .map_err(store_error)?;
    Ok(cut.map(ChatMessageId::new))
}

/// The highest audit ID that, with every one below it, is dated at or
/// before `before`: one below the first entry dated after it, found by a
/// walk in ID order that stops there, or the last ID when none is.
pub(super) async fn audit_cut<'e, E>(
    executor: E,
    before: DateTime<Utc>,
) -> Result<Option<AuditEntryId>, StoreError>
where
    E: Executor<'e, Database = Sqlite>,
{
    let before = stored_time(before, "audit_log", "at")?;
    let row = sqlx::query!(
        r#"SELECT
               (SELECT id FROM audit_log WHERE at > ? ORDER BY id LIMIT 1) AS "first_kept: i64",
               (SELECT MAX(id) FROM audit_log) AS "last: i64""#,
        before
    )
    .fetch_one(executor)
    .await
    .map_err(store_error)?;
    let cut = match (row.first_kept, row.last) {
        (Some(first_kept), _) => Some(first_kept.saturating_sub(1)),
        (None, last) => last,
    };
    Ok(cut.map(AuditEntryId::new))
}

/// Deletes at most `limit` replaced refresh tokens of sessions created at or
/// before `created_by`, over `sessions(created_at)` and the tokens'
/// `session_id` index.
pub(super) async fn delete_expired_rotations(
    conn: &mut SqliteConnection,
    created_by: DateTime<Utc>,
    limit: u32,
) -> Result<u64, StoreError> {
    let created_by = stored_time(created_by, "sessions", "created_at")?;
    let limit = i64::from(limit);
    let done = sqlx::query!(
        "DELETE FROM rotated_refresh_tokens WHERE rowid IN (
             SELECT r.rowid FROM sessions s
             JOIN rotated_refresh_tokens r ON r.session_id = s.id
             WHERE s.created_at <= ?
             LIMIT ?
         )",
        created_by,
        limit
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected())
}

/// Deletes at most `limit` sessions created at or before `created_by`, the
/// oldest first, over `sessions(created_at)`.
pub(super) async fn delete_expired_sessions(
    conn: &mut SqliteConnection,
    created_by: DateTime<Utc>,
    limit: u32,
) -> Result<u64, StoreError> {
    let created_by = stored_time(created_by, "sessions", "created_at")?;
    let limit = i64::from(limit);
    let done = sqlx::query!(
        "DELETE FROM sessions WHERE rowid IN (
             SELECT rowid FROM sessions WHERE created_at <= ? ORDER BY created_at LIMIT ?
         )",
        created_by,
        limit
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected())
}

/// Deletes at most `limit` chat lines received at or before `received_by`,
/// the oldest first, over `chat_messages(at)`.
pub(super) async fn delete_chat_received_by(
    conn: &mut SqliteConnection,
    received_by: DateTime<Utc>,
    limit: u32,
) -> Result<u64, StoreError> {
    let received_by = stored_time(received_by, "chat_messages", "at")?;
    let limit = i64::from(limit);
    let done = sqlx::query!(
        "DELETE FROM chat_messages WHERE id IN (
             SELECT id FROM chat_messages WHERE at <= ? ORDER BY at LIMIT ?
         )",
        received_by,
        limit
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected())
}

/// Deletes at most `limit` of `bot`'s chat lines with an ID at or below
/// `cut`, the oldest first, over `chat_messages(bot_id, id)`.
pub(super) async fn delete_chat_through(
    conn: &mut SqliteConnection,
    bot: BotId,
    cut: ChatMessageId,
    limit: u32,
) -> Result<u64, StoreError> {
    let bot = id_text(bot);
    let cut = cut.get();
    let limit = i64::from(limit);
    let done = sqlx::query!(
        "DELETE FROM chat_messages WHERE id IN (
             SELECT id FROM chat_messages WHERE bot_id = ? AND id <= ? ORDER BY id LIMIT ?
         )",
        bot,
        cut,
        limit
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected())
}

/// Deletes at most `limit` audit entries with an ID at or below `cut`, the
/// oldest first.
pub(super) async fn delete_audit_through(
    conn: &mut SqliteConnection,
    cut: AuditEntryId,
    limit: u32,
) -> Result<u64, StoreError> {
    let cut = cut.get();
    let limit = i64::from(limit);
    let done = sqlx::query!(
        "DELETE FROM audit_log WHERE id IN (
             SELECT id FROM audit_log WHERE id <= ? ORDER BY id LIMIT ?
         )",
        cut,
        limit
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected())
}

/// Deletes at most `limit` unused invites that expired at or before
/// `expired_by`, the oldest first, over `invites(expires_at)`.
pub(super) async fn delete_unused_invites(
    conn: &mut SqliteConnection,
    expired_by: DateTime<Utc>,
    limit: u32,
) -> Result<u64, StoreError> {
    let expired_by = stored_time(expired_by, "invites", "expires_at")?;
    let limit = i64::from(limit);
    let done = sqlx::query!(
        "DELETE FROM invites WHERE rowid IN (
             SELECT rowid FROM invites
             WHERE used_at IS NULL AND expires_at <= ?
             ORDER BY expires_at LIMIT ?
         )",
        expired_by,
        limit
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected())
}

/// Deletes at most `limit` unused enrollment tokens that expired at or
/// before `expired_by`, the oldest first, over
/// `enrollment_tokens(expires_at)`.
pub(super) async fn delete_unused_enrollment_tokens(
    conn: &mut SqliteConnection,
    expired_by: DateTime<Utc>,
    limit: u32,
) -> Result<u64, StoreError> {
    let expired_by = stored_time(expired_by, "enrollment_tokens", "expires_at")?;
    let limit = i64::from(limit);
    let done = sqlx::query!(
        "DELETE FROM enrollment_tokens WHERE rowid IN (
             SELECT rowid FROM enrollment_tokens
             WHERE used_at IS NULL AND expires_at <= ?
             ORDER BY expires_at LIMIT ?
         )",
        expired_by,
        limit
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected())
}
