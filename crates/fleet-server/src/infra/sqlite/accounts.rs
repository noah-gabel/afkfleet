//! The `mc_accounts` table's SQL.
//!
//! An account's kind decides which columns it fills: a Microsoft account
//! its UUID, its refresh token's envelope and its status; an offline
//! account none of them, and always the status `active`. A row whose
//! columns disagree with its kind doesn't read back, though the table's
//! CHECKs already refuse one. The status is read from two columns, `status`
//! and `status_reason`, which only a refused account fills (P9.5); a pair
//! that doesn't describe a status doesn't read back either.

use chrono::{DateTime, Utc};
use fleet_core::account::{AccountError, AccountKind, AccountStatus, RefusalReason};
use fleet_core::authz::{Actor, GrantLevel, Role, UserRef};
use fleet_core::id::{AccountId, UserId};
use fleet_core::value::{McUsername, McUuid, Username};
use sqlx::{SqliteConnection, SqlitePool};

use super::convert::{id_text, parse_id, parse_time, time_text};
use super::error::{store_error, unique_violation};
use crate::ports::accounts::{
    AccountCredential, DeletedAccount, InsertAccountError, ListedAccount, McAccount,
};
use crate::ports::page::Page;
use crate::ports::store::StoreError;

/// The table, as errors name it.
const MC_ACCOUNTS: &str = "mc_accounts";
/// What SQLite names in a violation of the UNIQUE index on a Microsoft
/// account's UUID.
const TAKEN_UUID: &str = "mc_accounts.mc_uuid";
/// What SQLite names in a violation of the UNIQUE index on an offline
/// account's lowercased name: an index on an expression is named as the
/// index, not as a column.
const TAKEN_OFFLINE_NAME: &str = "index 'mc_accounts_offline_name'";

/// An `mc_accounts` row, before its values are checked.
struct AccountRow {
    rowid: i64,
    id: String,
    owner_user_id: String,
    kind: String,
    mc_uuid: Option<String>,
    mc_name: String,
    refresh_envelope: Option<Vec<u8>>,
    status: String,
    status_reason: Option<String>,
    created_at: String,
}

/// The account `id`.
pub(super) async fn find<'c, E>(executor: E, id: AccountId) -> Result<Option<McAccount>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let id = id_text(id);
    sqlx::query_as!(
        AccountRow,
        r#"SELECT rowid AS "rowid!: i64", id AS "id!", owner_user_id AS "owner_user_id!",
                  kind AS "kind!", mc_uuid, mc_name AS "mc_name!",
                  refresh_envelope AS "refresh_envelope: Vec<u8>", status AS "status!",
                  status_reason, created_at AS "created_at!"
           FROM mc_accounts WHERE id = ?"#,
        id
    )
    .fetch_optional(executor)
    .await
    .map_err(store_error)?
    .map(account_from_row)
    .transpose()
}

/// How many accounts `owner` owns, of any kind and status.
pub(super) async fn count_owned_by<'c, E>(executor: E, owner: UserId) -> Result<u32, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let owner = id_text(owner);
    let count = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!: i64" FROM mc_accounts WHERE owner_user_id = ?"#,
        owner
    )
    .fetch_one(executor)
    .await
    .map_err(store_error)?;
    // A count is never negative, and nobody owns 4 billion accounts.
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

/// How many offline accounts exist.
pub(super) async fn count_offline(pool: &SqlitePool) -> Result<u64, StoreError> {
    let count = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!: i64" FROM mc_accounts WHERE kind = 'offline'"#
    )
    .fetch_one(pool)
    .await
    .map_err(store_error)?;
    // A count is never negative.
    Ok(u64::try_from(count).unwrap_or_default())
}

/// Inserts `account`. A taken Microsoft account or offline name is
/// [`InsertAccountError::Taken`].
pub(super) async fn insert(
    conn: &mut SqliteConnection,
    account: &McAccount,
) -> Result<(), InsertAccountError> {
    let id = id_text(account.id);
    let owner = id_text(account.owner);
    let kind = account.credential.kind().as_str();
    let name = account.name.as_str();
    let status = account.status().as_str();
    let reason = account.status().reason().map(RefusalReason::as_str);
    let created_at = time_text(account.created_at).ok_or(StoreError::Unstorable {
        table: MC_ACCOUNTS,
        column: "created_at",
    })?;
    let (uuid, envelope) = match &account.credential {
        AccountCredential::Microsoft {
            uuid,
            refresh_envelope,
            ..
        } => (Some(uuid.to_string()), Some(refresh_envelope.as_slice())),
        AccountCredential::Offline => (None, None),
    };
    sqlx::query!(
        "INSERT INTO mc_accounts
             (id, owner_user_id, kind, mc_uuid, mc_name, refresh_envelope, status,
              status_reason, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        id,
        owner,
        kind,
        uuid,
        name,
        envelope,
        status,
        reason,
        created_at
    )
    .execute(conn)
    .await
    .map_err(|error| match unique_violation(&error).as_deref() {
        Some(TAKEN_UUID | TAKEN_OFFLINE_NAME) => InsertAccountError::Taken,
        _ => InsertAccountError::Store(store_error(error)),
    })?;
    Ok(())
}

/// Sets the Microsoft account `id`'s status and reason, which an offline
/// account never changes.
pub(super) async fn set_status(
    conn: &mut SqliteConnection,
    id: AccountId,
    status: AccountStatus,
) -> Result<bool, StoreError> {
    let id = id_text(id);
    let name = status.as_str();
    let reason = status.reason().map(RefusalReason::as_str);
    let done = sqlx::query!(
        "UPDATE mc_accounts SET status = ?, status_reason = ?
         WHERE id = ? AND kind = 'microsoft'",
        name,
        reason,
        id
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Renames the account `id` from exactly `from` to `to`.
pub(super) async fn rename(
    conn: &mut SqliteConnection,
    id: AccountId,
    from: &McUsername,
    to: &McUsername,
) -> Result<bool, StoreError> {
    let id = id_text(id);
    let from = from.as_str();
    let to = to.as_str();
    let done = sqlx::query!(
        "UPDATE mc_accounts SET mc_name = ? WHERE id = ? AND mc_name = ?",
        to,
        id,
        from
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// A row of the views: an account without its envelope, its owner's role
/// and name, and the viewer's grant.
struct ListedRow {
    rowid: i64,
    id: String,
    owner_user_id: String,
    owner_role: String,
    owner_name: String,
    owner_rowid: i64,
    kind: String,
    mc_uuid: Option<String>,
    mc_name: String,
    status: String,
    status_reason: Option<String>,
    created_at: String,
    grant_level: Option<String>,
    grant_rowid: Option<i64>,
}

/// The account `id` as `viewer` sees it.
pub(super) async fn listed<'c, E>(
    executor: E,
    id: AccountId,
    viewer: UserId,
) -> Result<Option<ListedAccount>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let id = id_text(id);
    let viewer = id_text(viewer);
    sqlx::query_as!(
        ListedRow,
        r#"SELECT a.rowid AS "rowid!: i64", a.id AS "id!", a.owner_user_id AS "owner_user_id!",
                  o.role AS "owner_role!", o.username AS "owner_name!",
                  o.rowid AS "owner_rowid!: i64", a.kind AS "kind!", a.mc_uuid AS "mc_uuid?",
                  a.mc_name AS "mc_name!", a.status AS "status!",
                  a.status_reason AS "status_reason?", a.created_at AS "created_at!",
                  g.level AS "grant_level?", g.rowid AS "grant_rowid?: i64"
           FROM mc_accounts AS a
           JOIN users AS o ON o.id = a.owner_user_id
           LEFT JOIN account_grants AS g ON g.account_id = a.id AND g.user_id = ?
           WHERE a.id = ?"#,
        viewer,
        id
    )
    .fetch_optional(executor)
    .await
    .map_err(store_error)?
    .map(|row| listed_from_row(&row))
    .transpose()
}

/// One page of the accounts `viewer` may see: their own, those granted to
/// them, every Member's for an Admin, and all of them for the Owner, by the
/// owners' current roles (`authorize()`'s account rule).
pub(super) async fn list_visible(
    pool: &SqlitePool,
    viewer: Actor,
    page: Page<AccountId>,
) -> Result<Vec<ListedAccount>, StoreError> {
    let role = viewer.role.as_str();
    let viewer = id_text(viewer.id);
    let before = page.before.map(|id| id_text(id).to_string());
    let fetch = page.limit.fetch();
    sqlx::query_as!(
        ListedRow,
        r#"SELECT a.rowid AS "rowid!: i64", a.id AS "id!", a.owner_user_id AS "owner_user_id!",
                  o.role AS "owner_role!", o.username AS "owner_name!",
                  o.rowid AS "owner_rowid!: i64", a.kind AS "kind!", a.mc_uuid AS "mc_uuid?",
                  a.mc_name AS "mc_name!", a.status AS "status!",
                  a.status_reason AS "status_reason?", a.created_at AS "created_at!",
                  g.level AS "grant_level?", g.rowid AS "grant_rowid?: i64"
           FROM mc_accounts AS a
           JOIN users AS o ON o.id = a.owner_user_id
           LEFT JOIN account_grants AS g ON g.account_id = a.id AND g.user_id = ?
           WHERE (? IS NULL OR a.id < ?)
             AND (? = 'owner'
                  OR a.owner_user_id = ?
                  OR g.level IS NOT NULL
                  OR (? = 'admin' AND o.role = 'member'))
           ORDER BY a.id DESC
           LIMIT ?"#,
        viewer,
        before,
        before,
        role,
        viewer,
        role,
        fetch
    )
    .fetch_all(pool)
    .await
    .map_err(store_error)?
    .into_iter()
    .map(|row| listed_from_row(&row))
    .collect()
}

/// One page of the pending accounts `viewer` may review: those whose owner
/// they outrank by current role, the Owner over Admins and Members and an
/// Admin over Members (`authorize()`'s `ReviewLink`).
pub(super) async fn awaiting_review(
    pool: &SqlitePool,
    viewer: Actor,
    page: Page<AccountId>,
) -> Result<Vec<ListedAccount>, StoreError> {
    let role = viewer.role.as_str();
    let viewer = id_text(viewer.id);
    let before = page.before.map(|id| id_text(id).to_string());
    let fetch = page.limit.fetch();
    sqlx::query_as!(
        ListedRow,
        r#"SELECT a.rowid AS "rowid!: i64", a.id AS "id!", a.owner_user_id AS "owner_user_id!",
                  o.role AS "owner_role!", o.username AS "owner_name!",
                  o.rowid AS "owner_rowid!: i64", a.kind AS "kind!", a.mc_uuid AS "mc_uuid?",
                  a.mc_name AS "mc_name!", a.status AS "status!",
                  a.status_reason AS "status_reason?", a.created_at AS "created_at!",
                  g.level AS "grant_level?", g.rowid AS "grant_rowid?: i64"
           FROM mc_accounts AS a
           JOIN users AS o ON o.id = a.owner_user_id
           LEFT JOIN account_grants AS g ON g.account_id = a.id AND g.user_id = ?
           WHERE a.status = 'pending'
             AND a.owner_user_id <> ?
             AND ((? = 'owner' AND o.role <> 'owner') OR (? = 'admin' AND o.role = 'member'))
             AND (? IS NULL OR a.id < ?)
           ORDER BY a.id DESC
           LIMIT ?"#,
        viewer,
        viewer,
        role,
        role,
        before,
        before,
        fetch
    )
    .fetch_all(pool)
    .await
    .map_err(store_error)?
    .into_iter()
    .map(|row| listed_from_row(&row))
    .collect()
}

/// Deletes the account `id`; its grants go with it.
pub(super) async fn delete(
    conn: &mut SqliteConnection,
    id: AccountId,
    at: DateTime<Utc>,
) -> Result<Option<DeletedAccount>, StoreError> {
    let id = id_text(id);
    let at = time_text(at).ok_or(StoreError::Unstorable {
        table: "releasing_bots",
        column: "created_at",
    })?;
    // An agent may still run the bot: its Minecraft account stays fenced
    // until that agent no longer runs it (ADR-0019, S39). Written before the
    // foreign key deletes the bot with the account.
    sqlx::query!(
        "INSERT INTO releasing_bots (bot_id, agent_id, account_id, kind, mc_uuid, mc_name, created_at)
         SELECT b.id, b.agent_id, a.id, a.kind, a.mc_uuid, a.mc_name, ?
         FROM bots b JOIN mc_accounts a ON a.id = b.account_id
         WHERE a.id = ? AND b.agent_id IS NOT NULL
         ON CONFLICT (bot_id, agent_id) DO NOTHING",
        at,
        id
    )
    .execute(&mut *conn)
    .await
    .map_err(store_error)?;
    // Read before the foreign key deletes it with the account.
    let bot = sqlx::query!(
        r#"SELECT rowid AS "rowid!: i64", id AS "id!", agent_id AS "agent_id?"
           FROM bots WHERE account_id = ?"#,
        id
    )
    .fetch_optional(&mut *conn)
    .await
    .map_err(store_error)?;
    let done = sqlx::query!("DELETE FROM mc_accounts WHERE id = ?", id)
        .execute(&mut *conn)
        .await
        .map_err(store_error)?;
    if done.rows_affected() != 1 {
        return Ok(None);
    }
    let Some(bot) = bot else {
        return Ok(Some(DeletedAccount {
            bot: None,
            agent: None,
        }));
    };
    let corrupt = |column| StoreError::Corrupt {
        table: "bots",
        column,
        rowid: bot.rowid,
    };
    Ok(Some(DeletedAccount {
        bot: Some(parse_id(&bot.id).ok_or_else(|| corrupt("id"))?),
        agent: bot
            .agent_id
            .as_deref()
            .map(|agent| parse_id(agent).ok_or_else(|| corrupt("agent_id")))
            .transpose()?,
    }))
}

/// Stores a new link of the Microsoft account `id`, but only over exactly
/// the envelope `from`, and never on a pending account.
pub(super) async fn relink(
    conn: &mut SqliteConnection,
    id: AccountId,
    from: &[u8],
    to: &[u8],
    name: &McUsername,
) -> Result<bool, StoreError> {
    let id = id_text(id);
    let name = name.as_str();
    let done = sqlx::query!(
        "UPDATE mc_accounts
         SET refresh_envelope = ?, status = 'active', status_reason = NULL, mc_name = ?
         WHERE id = ? AND kind = 'microsoft' AND refresh_envelope = ? AND status <> 'pending'",
        to,
        name,
        id,
        from
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Checks every value of a stored row; one that doesn't read back, or a
/// combination its kind doesn't allow, is [`StoreError::Corrupt`], naming
/// the column and the rowid, never the value.
fn account_from_row(row: AccountRow) -> Result<McAccount, StoreError> {
    let corrupt = |column| StoreError::Corrupt {
        table: MC_ACCOUNTS,
        column,
        rowid: row.rowid,
    };
    let kind: AccountKind = row.kind.parse().map_err(|_| corrupt("kind"))?;
    let status = read_status(&row.status, row.status_reason.as_deref(), corrupt)?;
    let credential = match kind {
        AccountKind::Microsoft => AccountCredential::Microsoft {
            uuid: row
                .mc_uuid
                .as_deref()
                .and_then(|text| text.parse::<McUuid>().ok())
                .ok_or_else(|| corrupt("mc_uuid"))?,
            refresh_envelope: row
                .refresh_envelope
                .ok_or_else(|| corrupt("refresh_envelope"))?,
            status,
        },
        AccountKind::Offline => {
            if row.mc_uuid.is_some() {
                return Err(corrupt("mc_uuid"));
            }
            if row.refresh_envelope.is_some() {
                return Err(corrupt("refresh_envelope"));
            }
            if status != AccountStatus::Active {
                return Err(corrupt("status"));
            }
            if row.status_reason.is_some() {
                return Err(corrupt("status_reason"));
            }
            AccountCredential::Offline
        }
    };
    Ok(McAccount {
        id: parse_id(&row.id).ok_or_else(|| corrupt("id"))?,
        owner: parse_id(&row.owner_user_id).ok_or_else(|| corrupt("owner_user_id"))?,
        name: McUsername::try_from(row.mc_name.as_str()).map_err(|_| corrupt("mc_name"))?,
        created_at: parse_time(&row.created_at).ok_or_else(|| corrupt("created_at"))?,
        credential,
    })
}

/// Reads a status from its two columns; a pair that doesn't describe one is
/// [`StoreError::Corrupt`], naming the column that's wrong.
fn read_status(
    status: &str,
    reason: Option<&str>,
    corrupt: impl Fn(&'static str) -> StoreError,
) -> Result<AccountStatus, StoreError> {
    AccountStatus::from_columns(status, reason).map_err(|error| match error {
        AccountError::UnknownStatus | AccountError::UnknownKind => corrupt("status"),
        AccountError::UnknownReason
        | AccountError::MissingReason
        | AccountError::UnexpectedReason => corrupt("status_reason"),
    })
}

/// Checks every value of a view's row as [`account_from_row`] does, and the
/// owner's role and name and the viewer's grant, each naming its own table's
/// row when it doesn't read back.
fn listed_from_row(row: &ListedRow) -> Result<ListedAccount, StoreError> {
    let corrupt = |column| StoreError::Corrupt {
        table: MC_ACCOUNTS,
        column,
        rowid: row.rowid,
    };
    let owner_corrupt = |column| StoreError::Corrupt {
        table: "users",
        column,
        rowid: row.owner_rowid,
    };
    let kind: AccountKind = row.kind.parse().map_err(|_| corrupt("kind"))?;
    let status = read_status(&row.status, row.status_reason.as_deref(), corrupt)?;
    let uuid = match kind {
        AccountKind::Microsoft => Some(
            row.mc_uuid
                .as_deref()
                .and_then(|text| text.parse::<McUuid>().ok())
                .ok_or_else(|| corrupt("mc_uuid"))?,
        ),
        AccountKind::Offline => {
            if row.mc_uuid.is_some() {
                return Err(corrupt("mc_uuid"));
            }
            if status != AccountStatus::Active {
                return Err(corrupt("status"));
            }
            None
        }
    };
    let grant = match (row.grant_level.as_deref(), row.grant_rowid) {
        (Some(level), Some(rowid)) => {
            Some(
                level
                    .parse::<GrantLevel>()
                    .map_err(|_| StoreError::Corrupt {
                        table: "account_grants",
                        column: "level",
                        rowid,
                    })?,
            )
        }
        _ => None,
    };
    Ok(ListedAccount {
        id: parse_id(&row.id).ok_or_else(|| corrupt("id"))?,
        owner: UserRef {
            id: parse_id(&row.owner_user_id).ok_or_else(|| corrupt("owner_user_id"))?,
            role: row
                .owner_role
                .parse::<Role>()
                .map_err(|_| owner_corrupt("role"))?,
        },
        owner_name: Username::try_from(row.owner_name.as_str())
            .map_err(|_| owner_corrupt("username"))?,
        name: McUsername::try_from(row.mc_name.as_str()).map_err(|_| corrupt("mc_name"))?,
        uuid,
        kind,
        status,
        created_at: parse_time(&row.created_at).ok_or_else(|| corrupt("created_at"))?,
        grant,
    })
}
