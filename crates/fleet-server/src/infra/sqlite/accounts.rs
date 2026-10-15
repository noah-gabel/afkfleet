//! The `mc_accounts` table's SQL.
//!
//! An account's kind decides which columns it fills: a Microsoft account
//! its UUID, its refresh token's envelope and its status; an offline
//! account none of them, and always the status `active`. A row whose
//! columns disagree with its kind doesn't read back, though the table's
//! CHECKs already refuse one. The status is read from two columns, `status`
//! and `status_reason`, which only a refused account fills (P9.5); a pair
//! that doesn't describe a status doesn't read back either.

use fleet_core::account::{AccountError, AccountKind, AccountStatus, RefusalReason};
use fleet_core::id::{AccountId, UserId};
use fleet_core::value::{McUsername, McUuid};
use sqlx::SqliteConnection;

use super::convert::{id_text, parse_id, parse_time, time_text};
use super::error::{store_error, unique_violation};
use crate::ports::accounts::{AccountCredential, InsertAccountError, McAccount};
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
    let status = AccountStatus::from_columns(&row.status, row.status_reason.as_deref()).map_err(
        |error| match error {
            AccountError::UnknownStatus | AccountError::UnknownKind => corrupt("status"),
            AccountError::UnknownReason
            | AccountError::MissingReason
            | AccountError::UnexpectedReason => corrupt("status_reason"),
        },
    )?;
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
