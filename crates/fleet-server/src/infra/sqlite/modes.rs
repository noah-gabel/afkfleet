//! The `modes` table's SQL.
//!
//! A mode's JSON is read back only through fleet-core's validated
//! deserializer; `serde_json`'s error, which can quote the stored text, is
//! dropped for [`StoreError::Corrupt`], which never holds it. Its name reads
//! back through [`ModeName`]'s constructor, and one the constructor refuses
//! or changes is corrupt too.

use core::num::NonZeroU32;

use fleet_core::authz::{Actor, ModeVisibility, Role, UserRef};
use fleet_core::id::{ModeId, UserId};
use fleet_core::mode::{ModeDefinition, ModeName};
use fleet_core::value::Username;
use sqlx::SqliteConnection;

use super::convert::{id_text, parse_id, parse_time, time_text};
use super::error::{store_error, unique_violation};
use crate::ports::modes::{ListedMode, ModeChange, ModeOwner, NewMode, StoredMode, WriteModeError};
use crate::ports::page::Page;
use crate::ports::store::StoreError;

/// The table, as errors name it.
const MODES: &str = "modes";

/// A `modes` row, before its values are checked.
struct ModeRow {
    rowid: i64,
    id: String,
    owner_user_id: Option<String>,
    name: String,
    visibility: String,
    definition_json: String,
    version: i64,
    created_at: String,
    updated_at: String,
}

/// The mode `id`.
pub(super) async fn find<'c, E>(executor: E, id: ModeId) -> Result<Option<StoredMode>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let id = id_text(id);
    sqlx::query_as!(
        ModeRow,
        r#"SELECT rowid AS "rowid!: i64", id AS "id!", owner_user_id AS "owner_user_id?",
                  name AS "name!", visibility AS "visibility!", definition_json AS "definition_json!",
                  version AS "version!: i64", created_at AS "created_at!",
                  updated_at AS "updated_at!"
           FROM modes WHERE id = ?"#,
        id
    )
    .fetch_optional(executor)
    .await
    .map_err(store_error)?
    .map(|row| mode_from_row(&row))
    .transpose()
}

/// How many modes `owner` owns.
pub(super) async fn count_owned_by(
    conn: &mut SqliteConnection,
    owner: UserId,
) -> Result<u32, StoreError> {
    let owner = id_text(owner);
    let count = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!: i64" FROM modes WHERE owner_user_id = ?"#,
        owner
    )
    .fetch_one(conn)
    .await
    .map_err(store_error)?;
    // A count past u32 can't be stored by anyone the cap allows.
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

/// Whether `name` is taken for a mode of `owner`; see
/// [`ModeWrites::name_taken`].
///
/// [`ModeWrites::name_taken`]: crate::ports::modes::ModeWrites::name_taken
pub(super) async fn name_taken(
    conn: &mut SqliteConnection,
    owner: UserId,
    name: &ModeName,
    shared: bool,
    except: Option<ModeId>,
) -> Result<bool, StoreError> {
    let owner = id_text(owner);
    let name = name.as_str();
    let except = except.map(|id| id_text(id).to_string());
    // SQLite's lower() folds ASCII only, as ModeName::same_name and the
    // unique indexes do.
    sqlx::query_scalar!(
        r#"SELECT EXISTS (
               SELECT 1 FROM modes
               WHERE lower(name) = lower(?)
                 AND (owner_user_id = ? OR (? AND visibility = 'shared'))
                 AND (? IS NULL OR id <> ?)
           ) AS "taken!: bool""#,
        name,
        owner,
        shared,
        except,
        except
    )
    .fetch_one(conn)
    .await
    .map_err(store_error)
}

/// Stores `mode` at version 1, its definition as fleet-core writes it.
pub(super) async fn insert(
    conn: &mut SqliteConnection,
    mode: &NewMode,
) -> Result<(), WriteModeError> {
    let id = id_text(mode.id);
    let owner = id_text(mode.owner);
    let name = mode.name.as_str();
    let visibility = mode.visibility.as_str();
    let definition = serde_json::to_string(&mode.definition)
        .map_err(|error| StoreError::Backend(Box::new(error)))?;
    let created_at = time_text(mode.created_at).ok_or(StoreError::Unstorable {
        table: MODES,
        column: "created_at",
    })?;
    sqlx::query!(
        "INSERT INTO modes (id, owner_user_id, name, visibility, definition_json, version, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, 1, ?, ?)",
        id,
        owner,
        name,
        visibility,
        definition,
        created_at,
        created_at
    )
    .execute(conn)
    .await
    .map_err(|error| match unique_violation(&error).as_deref() {
        Some(TAKEN_OWNERS_NAME | TAKEN_SHARED_NAME) => WriteModeError::NameTaken,
        _ => WriteModeError::Store(store_error(error)),
    })?;
    Ok(())
}

/// Stores `change` over a user's mode still at its expected version; see
/// [`ModeWrites::update`].
///
/// [`ModeWrites::update`]: crate::ports::modes::ModeWrites::update
pub(super) async fn update(
    conn: &mut SqliteConnection,
    change: &ModeChange,
) -> Result<bool, WriteModeError> {
    let id = id_text(change.id);
    let expected = i64::from(change.expected.get());
    let version = i64::from(change.version.get());
    let name = change.name.as_str();
    let visibility = change.visibility.as_str();
    let definition = serde_json::to_string(&change.definition)
        .map_err(|error| StoreError::Backend(Box::new(error)))?;
    let updated_at = time_text(change.updated_at).ok_or(StoreError::Unstorable {
        table: MODES,
        column: "updated_at",
    })?;
    let done = sqlx::query!(
        "UPDATE modes
         SET name = ?, visibility = ?, definition_json = ?, version = ?, updated_at = ?
         WHERE id = ? AND version = ? AND owner_user_id IS NOT NULL",
        name,
        visibility,
        definition,
        version,
        updated_at,
        id,
        expected
    )
    .execute(conn)
    .await
    .map_err(|error| match unique_violation(&error).as_deref() {
        Some(TAKEN_OWNERS_NAME | TAKEN_SHARED_NAME) => WriteModeError::NameTaken,
        _ => WriteModeError::Store(store_error(error)),
    })?;
    Ok(done.rows_affected() == 1)
}

/// Deletes a user's mode; see [`ModeWrites::delete`].
///
/// [`ModeWrites::delete`]: crate::ports::modes::ModeWrites::delete
pub(super) async fn delete(conn: &mut SqliteConnection, id: ModeId) -> Result<bool, StoreError> {
    let id = id_text(id);
    let done = sqlx::query!(
        "DELETE FROM modes WHERE id = ? AND owner_user_id IS NOT NULL",
        id
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// The mode `id` with its owner; see [`ModeReads::listed`].
///
/// [`ModeReads::listed`]: crate::ports::modes::ModeReads::listed
pub(super) async fn listed<'c, E>(executor: E, id: ModeId) -> Result<Option<ListedMode>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let id = id_text(id);
    sqlx::query_as!(
        ListedModeRow,
        r#"SELECT m.rowid AS "rowid!: i64", m.id AS "id!", m.owner_user_id AS "owner_user_id?",
                  m.name AS "name!", m.visibility AS "visibility!",
                  m.definition_json AS "definition_json!", m.version AS "version!: i64",
                  m.created_at AS "created_at!", m.updated_at AS "updated_at!",
                  u.role AS "owner_role?", u.username AS "owner_name?"
           FROM modes m
           LEFT JOIN users u ON u.id = m.owner_user_id
           WHERE m.id = ?"#,
        id
    )
    .fetch_optional(executor)
    .await
    .map_err(store_error)?
    .map(listed_from_row)
    .transpose()
}

/// What SQLite names in a violation of the unique index on an owner's
/// lowercased names: an index on an expression is named as the index.
const TAKEN_OWNERS_NAME: &str = "index 'modes_owner_name'";
/// The same for the unique index on the shared modes' lowercased names.
const TAKEN_SHARED_NAME: &str = "index 'modes_shared_name'";

/// The modes `viewer` may see on `page`; see [`ModeReads::visible`].
///
/// [`ModeReads::visible`]: crate::ports::modes::ModeReads::visible
pub(super) async fn visible<'c, E>(
    executor: E,
    viewer: Actor,
    page: Page<ModeId>,
) -> Result<Vec<ListedMode>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let role = viewer.role.as_str();
    let viewer = id_text(viewer.id);
    let before = page.before.map(|id| id_text(id).to_string());
    let fetch = page.limit.fetch();
    sqlx::query_as!(
        ListedModeRow,
        r#"SELECT m.rowid AS "rowid!: i64", m.id AS "id!", m.owner_user_id AS "owner_user_id?",
                  m.name AS "name!", m.visibility AS "visibility!",
                  m.definition_json AS "definition_json!", m.version AS "version!: i64",
                  m.created_at AS "created_at!", m.updated_at AS "updated_at!",
                  u.role AS "owner_role?", u.username AS "owner_name?"
           FROM modes m
           LEFT JOIN users u ON u.id = m.owner_user_id
           WHERE (? IS NULL OR m.id < ?)
             AND (m.owner_user_id IS NULL
                  OR m.visibility = 'shared'
                  OR m.owner_user_id = ?
                  OR ? = 'owner'
                  OR (? = 'admin' AND u.role = 'member'))
           ORDER BY m.id DESC
           LIMIT ?"#,
        before,
        before,
        viewer,
        role,
        role,
        fetch
    )
    .fetch_all(executor)
    .await
    .map_err(store_error)?
    .into_iter()
    .map(listed_from_row)
    .collect()
}

/// A `modes` row with its owner's role and name, before its values are
/// checked.
struct ListedModeRow {
    rowid: i64,
    id: String,
    owner_user_id: Option<String>,
    name: String,
    visibility: String,
    definition_json: String,
    version: i64,
    created_at: String,
    updated_at: String,
    owner_role: Option<String>,
    owner_name: Option<String>,
}

/// Reads a listed row back: the mode as [`mode_from_row`] does, and its
/// owner, whose role and name must read back too.
fn listed_from_row(row: ListedModeRow) -> Result<ListedMode, StoreError> {
    let ListedModeRow {
        rowid,
        id,
        owner_user_id,
        name,
        visibility,
        definition_json,
        version,
        created_at,
        updated_at,
        owner_role,
        owner_name,
    } = row;
    let mode = mode_from_row(&ModeRow {
        rowid,
        id,
        owner_user_id,
        name,
        visibility,
        definition_json,
        version,
        created_at,
        updated_at,
    })?;
    let corrupt = || StoreError::Corrupt {
        table: MODES,
        column: "owner_user_id",
        rowid,
    };
    let owner = match (mode.owner, owner_role, owner_name) {
        (None, _, _) => None,
        (Some(id), Some(role), Some(username)) => Some(ModeOwner {
            user: UserRef {
                id,
                role: role.parse::<Role>().map_err(|_| corrupt())?,
            },
            username: Username::try_from(username.as_str()).map_err(|_| corrupt())?,
        }),
        // The foreign key keeps an owner from going missing.
        (Some(_), _, _) => return Err(corrupt()),
    };
    Ok(ListedMode { mode, owner })
}

/// Reads a `modes` row back, or [`StoreError::Corrupt`] naming the first
/// column that doesn't.
fn mode_from_row(row: &ModeRow) -> Result<StoredMode, StoreError> {
    let rowid = row.rowid;
    let corrupt = |column| StoreError::Corrupt {
        table: MODES,
        column,
        rowid,
    };
    let owner = row
        .owner_user_id
        .as_deref()
        .map(|owner| parse_id(owner).ok_or_else(|| corrupt("owner_user_id")))
        .transpose()?;
    // serde_json's error can quote the stored text: it's dropped here.
    let definition: ModeDefinition =
        serde_json::from_str(&row.definition_json).map_err(|_| corrupt("definition_json"))?;
    Ok(StoredMode {
        id: parse_id(&row.id).ok_or_else(|| corrupt("id"))?,
        owner,
        // Stored exactly as the constructor made it: a name it would change
        // (untrimmed, say) didn't come from here.
        name: ModeName::try_from(row.name.as_str())
            .ok()
            .filter(|name| name.as_str() == row.name)
            .ok_or_else(|| corrupt("name"))?,
        visibility: row
            .visibility
            .parse::<ModeVisibility>()
            .map_err(|_| corrupt("visibility"))?,
        definition,
        version: u32::try_from(row.version)
            .ok()
            .and_then(NonZeroU32::new)
            .ok_or_else(|| corrupt("version"))?,
        created_at: parse_time(&row.created_at).ok_or_else(|| corrupt("created_at"))?,
        updated_at: parse_time(&row.updated_at).ok_or_else(|| corrupt("updated_at"))?,
    })
}
