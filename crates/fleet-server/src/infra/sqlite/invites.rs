//! The `invites` table's SQL.
//!
//! The code's hash is bound as its bytes. Expiry is compared as text: stored
//! times have one fixed form, so text order is time order. The conditional
//! writes check, in their own `WHERE`, that the invite is still unused,
//! unrevoked and unexpired, so two transactions that both checked it first
//! can't both use or revoke it.

use chrono::{DateTime, Utc};
use fleet_core::authz::Role;
use fleet_core::id::{InviteId, UserId};
use fleet_core::value::Username;
use sqlx::{SqliteConnection, SqlitePool};

use super::convert::{id_text, parse_id, parse_time, time_text};
use super::error::store_error;
use crate::ports::invites::{Invite, ListedInvite, NewInvite, RevokedInvite};
use crate::ports::page::Page;
use crate::ports::store::StoreError;
use crate::ports::tokens::TokenHash;

/// The table, as errors name it.
const TABLE: &str = "invites";

/// A row as stored, before its values are checked.
struct InviteRow {
    rowid: i64,
    id: String,
    code_hash: Vec<u8>,
    role: String,
    created_by: String,
    created_at: String,
    expires_at: String,
    used_by: Option<String>,
    used_at: Option<String>,
    revoked_at: Option<String>,
}

/// A listed row: the invite and its people's names.
struct ListedRow {
    rowid: i64,
    id: String,
    code_hash: Vec<u8>,
    role: String,
    created_by: String,
    created_at: String,
    expires_at: String,
    used_by: Option<String>,
    used_at: Option<String>,
    revoked_at: Option<String>,
    created_by_name: String,
    creator_rowid: i64,
    used_by_name: Option<String>,
    invitee_rowid: Option<i64>,
}

/// A revoked row's ID and role.
struct RevokedRow {
    rowid: i64,
    id: String,
    role: String,
}

/// The stored form of `at` in `column`, or [`StoreError::Unstorable`].
fn stored_time(at: DateTime<Utc>, column: &'static str) -> Result<String, StoreError> {
    time_text(at).ok_or(StoreError::Unstorable {
        table: TABLE,
        column,
    })
}

/// The invite with this ID.
pub(super) async fn find<'c, E>(executor: E, id: InviteId) -> Result<Option<Invite>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let id = id_text(id);
    sqlx::query_as!(
        InviteRow,
        r#"SELECT rowid AS "rowid!: i64", id AS "id!", code_hash AS "code_hash!: Vec<u8>",
                  role AS "role!", created_by AS "created_by!", created_at AS "created_at!",
                  expires_at AS "expires_at!", used_by AS "used_by?", used_at AS "used_at?",
                  revoked_at AS "revoked_at?"
           FROM invites WHERE id = ?"#,
        id
    )
    .fetch_optional(executor)
    .await
    .map_err(store_error)?
    .map(invite_from_row)
    .transpose()
}

/// The invite whose code hashes to `hash`.
pub(super) async fn find_by_code_hash<'c, E>(
    executor: E,
    hash: &TokenHash,
) -> Result<Option<Invite>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let hash = hash.as_bytes().as_slice();
    sqlx::query_as!(
        InviteRow,
        r#"SELECT rowid AS "rowid!: i64", id AS "id!", code_hash AS "code_hash!: Vec<u8>",
                  role AS "role!", created_by AS "created_by!", created_at AS "created_at!",
                  expires_at AS "expires_at!", used_by AS "used_by?", used_at AS "used_at?",
                  revoked_at AS "revoked_at?"
           FROM invites WHERE code_hash = ?"#,
        hash
    )
    .fetch_optional(executor)
    .await
    .map_err(store_error)?
    .map(invite_from_row)
    .transpose()
}

/// The invites of `page`, newest first, with their people's names: one
/// more than the page holds.
pub(super) async fn list(
    pool: &SqlitePool,
    page: Page<InviteId>,
) -> Result<Vec<ListedInvite>, StoreError> {
    let before = page.before.map(|id| id_text(id).to_string());
    let fetch = page.limit.fetch();
    sqlx::query_as!(
        ListedRow,
        r#"SELECT invites.rowid AS "rowid!: i64", invites.id AS "id!",
                  invites.code_hash AS "code_hash!: Vec<u8>", invites.role AS "role!",
                  invites.created_by AS "created_by!", invites.created_at AS "created_at!",
                  invites.expires_at AS "expires_at!", invites.used_by AS "used_by?",
                  invites.used_at AS "used_at?", invites.revoked_at AS "revoked_at?",
                  creator.username AS "created_by_name!", creator.rowid AS "creator_rowid!: i64",
                  invitee.username AS "used_by_name?", invitee.rowid AS "invitee_rowid?: i64"
           FROM invites
           JOIN users AS creator ON creator.id = invites.created_by
           LEFT JOIN users AS invitee ON invitee.id = invites.used_by
           WHERE ? IS NULL OR invites.id < ?
           ORDER BY invites.id DESC
           LIMIT ?"#,
        before,
        before,
        fetch
    )
    .fetch_all(pool)
    .await
    .map_err(store_error)?
    .into_iter()
    .map(listed_from_row)
    .collect()
}

/// Inserts `invite`.
pub(super) async fn insert(
    conn: &mut SqliteConnection,
    invite: &NewInvite,
) -> Result<(), StoreError> {
    let id = id_text(invite.id);
    let code_hash = invite.code_hash.as_bytes().as_slice();
    let role = invite.role.as_str();
    let created_by = id_text(invite.created_by);
    let created_at = stored_time(invite.created_at, "created_at")?;
    let expires_at = stored_time(invite.expires_at, "expires_at")?;
    sqlx::query!(
        "INSERT INTO invites (id, code_hash, role, created_by, created_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?)",
        id,
        code_hash,
        role,
        created_by,
        created_at,
        expires_at
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(())
}

/// How many pending invites `creator` holds at `now`.
pub(super) async fn count_pending_by(
    conn: &mut SqliteConnection,
    creator: UserId,
    now: DateTime<Utc>,
) -> Result<u32, StoreError> {
    let creator = id_text(creator);
    let now = stored_time(now, "expires_at")?;
    let count = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!: i64" FROM invites
           WHERE created_by = ? AND used_at IS NULL AND revoked_at IS NULL AND expires_at > ?"#,
        creator,
        now
    )
    .fetch_one(conn)
    .await
    .map_err(store_error)?;
    // A count is never negative, and a creator never holds 4 billion.
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

/// Marks a usable invite used.
pub(super) async fn use_invite(
    conn: &mut SqliteConnection,
    id: InviteId,
    user: UserId,
    at: DateTime<Utc>,
) -> Result<bool, StoreError> {
    let id = id_text(id);
    let user = id_text(user);
    let at = stored_time(at, "used_at")?;
    let done = sqlx::query!(
        "UPDATE invites SET used_by = ?, used_at = ?
         WHERE id = ? AND used_at IS NULL AND revoked_at IS NULL AND expires_at > ?",
        user,
        at,
        id,
        at
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Revokes a pending invite.
pub(super) async fn revoke(
    conn: &mut SqliteConnection,
    id: InviteId,
    at: DateTime<Utc>,
) -> Result<bool, StoreError> {
    let id = id_text(id);
    let at = stored_time(at, "revoked_at")?;
    let done = sqlx::query!(
        "UPDATE invites SET revoked_at = ?
         WHERE id = ? AND used_at IS NULL AND revoked_at IS NULL AND expires_at > ?",
        at,
        id,
        at
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Revokes every pending invite of `creator`.
pub(super) async fn revoke_pending_by(
    conn: &mut SqliteConnection,
    creator: UserId,
    at: DateTime<Utc>,
) -> Result<Vec<RevokedInvite>, StoreError> {
    let creator = id_text(creator);
    let at = stored_time(at, "revoked_at")?;
    sqlx::query_as!(
        RevokedRow,
        r#"UPDATE invites SET revoked_at = ?
           WHERE created_by = ? AND used_at IS NULL AND revoked_at IS NULL AND expires_at > ?
           RETURNING rowid AS "rowid!: i64", id AS "id!", role AS "role!""#,
        at,
        creator,
        at
    )
    .fetch_all(conn)
    .await
    .map_err(store_error)?
    .into_iter()
    .map(|row| {
        let corrupt = |column| StoreError::Corrupt {
            table: TABLE,
            column,
            rowid: row.rowid,
        };
        Ok(RevokedInvite {
            id: parse_id(&row.id).ok_or_else(|| corrupt("id"))?,
            role: invite_role(&row.role).ok_or_else(|| corrupt("role"))?,
        })
    })
    .collect()
}

/// An invite's stored role: a Member or an Admin. The table's CHECK
/// refuses anything else, so another value is corrupt.
fn invite_role(text: &str) -> Option<Role> {
    text.parse()
        .ok()
        .filter(|role| matches!(role, Role::Member | Role::Admin))
}

/// Checks every value of a stored row; one that doesn't read back is
/// [`StoreError::Corrupt`], naming the column and the rowid, never the
/// value.
fn invite_from_row(row: InviteRow) -> Result<Invite, StoreError> {
    let corrupt = |column| StoreError::Corrupt {
        table: TABLE,
        column,
        rowid: row.rowid,
    };
    let optional_time = |text: Option<String>, column| {
        text.map(|text| parse_time(&text).ok_or_else(|| corrupt(column)))
            .transpose()
    };
    Ok(Invite {
        id: parse_id(&row.id).ok_or_else(|| corrupt("id"))?,
        code_hash: TokenHash::try_from(row.code_hash.as_slice())
            .map_err(|_| corrupt("code_hash"))?,
        role: invite_role(&row.role).ok_or_else(|| corrupt("role"))?,
        created_by: parse_id(&row.created_by).ok_or_else(|| corrupt("created_by"))?,
        created_at: parse_time(&row.created_at).ok_or_else(|| corrupt("created_at"))?,
        expires_at: parse_time(&row.expires_at).ok_or_else(|| corrupt("expires_at"))?,
        used_by: row
            .used_by
            .map(|text| parse_id(&text).ok_or_else(|| corrupt("used_by")))
            .transpose()?,
        used_at: optional_time(row.used_at, "used_at")?,
        revoked_at: optional_time(row.revoked_at, "revoked_at")?,
    })
}

/// Checks a listed row: the invite, then the names.
fn listed_from_row(row: ListedRow) -> Result<ListedInvite, StoreError> {
    // The names are the `users` rows', so a bad one names that row.
    let corrupt_name = |rowid| StoreError::Corrupt {
        table: "users",
        column: "username",
        rowid,
    };
    let created_by_name = Username::try_from(row.created_by_name.as_str())
        .map_err(|_| corrupt_name(row.creator_rowid))?;
    let used_by_name = row
        .used_by_name
        .as_deref()
        .map(|name| {
            Username::try_from(name)
                .map_err(|_| corrupt_name(row.invitee_rowid.unwrap_or(row.rowid)))
        })
        .transpose()?;
    let invite = invite_from_row(InviteRow {
        rowid: row.rowid,
        id: row.id,
        code_hash: row.code_hash,
        role: row.role,
        created_by: row.created_by,
        created_at: row.created_at,
        expires_at: row.expires_at,
        used_by: row.used_by,
        used_at: row.used_at,
        revoked_at: row.revoked_at,
    })?;
    Ok(ListedInvite {
        invite,
        created_by_name,
        used_by_name,
    })
}
