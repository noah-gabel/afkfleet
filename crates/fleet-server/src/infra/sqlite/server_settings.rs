//! The `server_settings` table's SQL.
//!
//! A row's server reads back only as a [`ServerKey`] already in canonical
//! form, its conflict texts only through [`ConflictTexts`]' constructor,
//! exactly as [`ConflictTexts::iter`] writes them: sorted and without
//! duplicates; and its chat formats only through [`ChatFormats`]'
//! constructor, exactly as written: in order, duplicates kept (ADR-0020,
//! R139). `serde_json`'s error, which can quote the stored text, is
//! dropped for [`StoreError::Corrupt`], which never holds it. The fleet's
//! read never reads the formats: they never reach an agent.

use core::num::NonZeroU32;

use fleet_core::chat::ChatFormats;
use fleet_core::disconnect::ConflictTexts;
use fleet_core::id::ServerSettingsId;
use fleet_core::value::ServerKey;
use sqlx::SqliteConnection;

use super::convert::{id_text, parse_id, parse_time, time_text};
use super::error::{store_error, unique_violation};
use crate::ports::page::Page;
use crate::ports::server_settings::{
    NewServerSettings, ServerSettingsChange, ServerTexts, StoredServerSettings, UnreadableSettings,
    WriteServerSettingsError,
};
use crate::ports::store::StoreError;

/// The table, as errors name it.
pub(super) const SERVER_SETTINGS: &str = "server_settings";

/// What SQLite names in a violation of the unique key on `server`.
const TAKEN_SERVER: &str = "server_settings.server";

/// A `server_settings` row, before its values are checked.
struct SettingsRow {
    rowid: i64,
    id: String,
    server: String,
    conflict_texts: String,
    chat_formats: String,
    version: i64,
    created_at: String,
    updated_at: String,
}

/// The settings `id`.
pub(super) async fn find<'c, E>(
    executor: E,
    id: ServerSettingsId,
) -> Result<Option<StoredServerSettings>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let id = id_text(id);
    sqlx::query_as!(
        SettingsRow,
        r#"SELECT rowid AS "rowid!: i64", id AS "id!", server AS "server!",
                  conflict_texts AS "conflict_texts!", chat_formats AS "chat_formats!",
                  version AS "version!: i64", created_at AS "created_at!",
                  updated_at AS "updated_at!"
           FROM server_settings WHERE id = ?"#,
        id
    )
    .fetch_optional(executor)
    .await
    .map_err(store_error)?
    .map(|row| settings_from_row(&row))
    .transpose()
}

/// The settings on `page`, newest first by ID, one more than the page.
pub(super) async fn page<'c, E>(
    executor: E,
    page: Page<ServerSettingsId>,
) -> Result<Vec<StoredServerSettings>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let before = page.before.map(|id| id_text(id).to_string());
    let fetch = page.limit.fetch();
    sqlx::query_as!(
        SettingsRow,
        r#"SELECT rowid AS "rowid!: i64", id AS "id!", server AS "server!",
                  conflict_texts AS "conflict_texts!", chat_formats AS "chat_formats!",
                  version AS "version!: i64", created_at AS "created_at!",
                  updated_at AS "updated_at!"
           FROM server_settings
           WHERE (? IS NULL OR id < ?)
           ORDER BY id DESC
           LIMIT ?"#,
        before,
        before,
        fetch
    )
    .fetch_all(executor)
    .await
    .map_err(store_error)?
    .iter()
    .map(settings_from_row)
    .collect()
}

/// How many servers have settings.
pub(super) async fn count(conn: &mut SqliteConnection) -> Result<u32, StoreError> {
    let count = sqlx::query_scalar!(r#"SELECT count(*) AS "count!: i64" FROM server_settings"#)
        .fetch_one(conn)
        .await
        .map_err(store_error)?;
    // A count past u32 can't be stored by anyone the cap allows.
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

/// Whether `server` has settings in a row other than `except`.
pub(super) async fn server_taken(
    conn: &mut SqliteConnection,
    server: &ServerKey,
    except: Option<ServerSettingsId>,
) -> Result<bool, StoreError> {
    let server = server.to_string();
    let except = except.map(|id| id_text(id).to_string());
    sqlx::query_scalar!(
        r#"SELECT EXISTS (
               SELECT 1 FROM server_settings
               WHERE server = ? AND (? IS NULL OR id <> ?)
           ) AS "taken!: bool""#,
        server,
        except,
        except
    )
    .fetch_one(conn)
    .await
    .map_err(store_error)
}

/// Stores `settings` at version 1.
pub(super) async fn insert(
    conn: &mut SqliteConnection,
    settings: &NewServerSettings,
) -> Result<(), WriteServerSettingsError> {
    let id = id_text(settings.id);
    let server = settings.server.to_string();
    let texts = texts_json(&settings.conflict_texts)?;
    let formats = formats_json(&settings.chat_formats)?;
    let created_at = time_text(settings.created_at).ok_or(StoreError::Unstorable {
        table: SERVER_SETTINGS,
        column: "created_at",
    })?;
    sqlx::query!(
        "INSERT INTO server_settings
             (id, server, conflict_texts, chat_formats, version, created_at, updated_at)
         VALUES (?, ?, ?, ?, 1, ?, ?)",
        id,
        server,
        texts,
        formats,
        created_at,
        created_at
    )
    .execute(conn)
    .await
    .map_err(written)?;
    Ok(())
}

/// Stores `change` over the settings still at its expected version.
pub(super) async fn update(
    conn: &mut SqliteConnection,
    change: &ServerSettingsChange,
) -> Result<bool, WriteServerSettingsError> {
    let id = id_text(change.id);
    let expected = i64::from(change.expected.get());
    let version = i64::from(change.version.get());
    let server = change.server.to_string();
    let texts = texts_json(&change.conflict_texts)?;
    let formats = formats_json(&change.chat_formats)?;
    let updated_at = time_text(change.updated_at).ok_or(StoreError::Unstorable {
        table: SERVER_SETTINGS,
        column: "updated_at",
    })?;
    let done = sqlx::query!(
        "UPDATE server_settings
         SET server = ?, conflict_texts = ?, chat_formats = ?, version = ?, updated_at = ?
         WHERE id = ? AND version = ?",
        server,
        texts,
        formats,
        version,
        updated_at,
        id,
        expected
    )
    .execute(conn)
    .await
    .map_err(written)?;
    Ok(done.rows_affected() == 1)
}

/// Deletes the settings `id`.
pub(super) async fn delete(
    conn: &mut SqliteConnection,
    id: ServerSettingsId,
) -> Result<bool, StoreError> {
    let id = id_text(id);
    let done = sqlx::query!("DELETE FROM server_settings WHERE id = ?", id)
        .execute(conn)
        .await
        .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Every server's conflict texts, for the fleet's snapshot (ADR-0020, R132):
/// a row whose server doesn't read back fails the read, since its bots
/// can't be told; one whose texts don't is unreadable, by its server, with
/// its rowid and, when it reads, its ID.
pub(super) async fn texts_by_server(
    conn: &mut SqliteConnection,
) -> Result<ServerTexts, StoreError> {
    let rows = sqlx::query!(
        r#"SELECT rowid AS "rowid!: i64", id AS "id!", server AS "server!",
                  conflict_texts AS "conflict_texts!"
           FROM server_settings"#
    )
    .fetch_all(conn)
    .await
    .map_err(store_error)?;
    let mut texts = ServerTexts::default();
    for row in rows {
        let server = ServerKey::try_from(row.server.as_str()).map_err(|_| StoreError::Corrupt {
            table: SERVER_SETTINGS,
            column: "server",
            rowid: row.rowid,
        })?;
        if let Some(read) = stored_texts(&row.conflict_texts) {
            texts.readable.insert(server, read);
        } else {
            let unread = UnreadableSettings {
                rowid: row.rowid,
                id: parse_id(&row.id),
            };
            texts.unreadable.insert(server, unread);
        }
    }
    Ok(texts)
}

/// A write's error: the unique key's refusal as [`WriteServerSettingsError::ServerTaken`].
fn written(error: sqlx::Error) -> WriteServerSettingsError {
    match unique_violation(&error).as_deref() {
        Some(TAKEN_SERVER) => WriteServerSettingsError::ServerTaken,
        _ => WriteServerSettingsError::Store(store_error(error)),
    }
}

/// `texts` as the column stores them: a JSON array, in the set's order.
fn texts_json(texts: &ConflictTexts) -> Result<String, StoreError> {
    serde_json::to_string(&texts.iter().collect::<Vec<_>>())
        .map_err(|error| StoreError::Backend(Box::new(error)))
}

/// `formats` as the column stores them: a JSON array, as written.
fn formats_json(formats: &ChatFormats) -> Result<String, StoreError> {
    serde_json::to_string(&formats.iter().collect::<Vec<_>>())
        .map_err(|error| StoreError::Backend(Box::new(error)))
}

/// Reads a `server_settings` row back, or [`StoreError::Corrupt`] naming the
/// first column that doesn't.
fn settings_from_row(row: &SettingsRow) -> Result<StoredServerSettings, StoreError> {
    let rowid = row.rowid;
    let corrupt = |column| StoreError::Corrupt {
        table: SERVER_SETTINGS,
        column,
        rowid,
    };
    Ok(StoredServerSettings {
        id: parse_id(&row.id).ok_or_else(|| corrupt("id"))?,
        // Only the key's own text: a form it would have to repair didn't
        // come from here.
        server: ServerKey::try_from(row.server.as_str()).map_err(|_| corrupt("server"))?,
        conflict_texts: stored_texts(&row.conflict_texts)
            .ok_or_else(|| corrupt("conflict_texts"))?,
        chat_formats: stored_formats(&row.chat_formats).ok_or_else(|| corrupt("chat_formats"))?,
        version: u32::try_from(row.version)
            .ok()
            .and_then(NonZeroU32::new)
            .ok_or_else(|| corrupt("version"))?,
        created_at: parse_time(&row.created_at).ok_or_else(|| corrupt("created_at"))?,
        updated_at: parse_time(&row.updated_at).ok_or_else(|| corrupt("updated_at"))?,
    })
}

/// The texts a stored array holds, only when it's exactly what the set
/// writes: every entry one the constructor takes, sorted, no duplicates.
fn stored_texts(json: &str) -> Option<ConflictTexts> {
    // serde_json's error can quote the stored text: it's dropped here.
    let entries: Vec<String> = serde_json::from_str(json).ok()?;
    let texts = ConflictTexts::try_new(&entries).ok()?;
    texts
        .iter()
        .eq(entries.iter().map(String::as_str))
        .then_some(texts)
}

/// The formats a stored array holds, only when it's exactly what they were
/// given as: every entry one the constructor takes, in order.
fn stored_formats(json: &str) -> Option<ChatFormats> {
    // serde_json's error can quote the stored text: it's dropped here.
    let entries: Vec<String> = serde_json::from_str(json).ok()?;
    let formats = ChatFormats::try_new(&entries).ok()?;
    formats
        .iter()
        .eq(entries.iter().map(String::as_str))
        .then_some(formats)
}
