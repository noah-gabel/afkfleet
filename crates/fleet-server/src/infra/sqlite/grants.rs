//! The `account_grants` table's SQL.
//!
//! One row per account and user, so a second grant to the same user is a
//! PRIMARY KEY violation, which [`insert`] maps to
//! [`InsertGrantError::Exists`].

use chrono::{DateTime, Utc};
use fleet_core::authz::{GrantLevel, Role, UserRef};
use fleet_core::id::{AccountId, UserId};
use fleet_core::value::{McUsername, Username};
use sqlx::{SqliteConnection, SqlitePool};

use super::convert::{id_text, parse_id, parse_time, time_text};
use super::error::{store_error, unique_violation};
use crate::ports::grants::{AccountGrant, HeldGrant, InsertGrantError, ListedGrant};
use crate::ports::page::Page;
use crate::ports::store::StoreError;

/// The table, as errors name it.
const ACCOUNT_GRANTS: &str = "account_grants";
/// What SQLite names in a violation of the PRIMARY KEY: one grant per
/// account and user.
const EXISTING_GRANT: &str = "account_grants.account_id, account_grants.user_id";

/// An `account_grants` row, before its values are checked.
struct GrantRow {
    rowid: i64,
    account_id: String,
    user_id: String,
    level: String,
    granted_by: String,
    granted_at: String,
}

/// A row of the list: the grant with both people's names.
struct ListedRow {
    rowid: i64,
    account_id: String,
    user_id: String,
    level: String,
    granted_by: String,
    granted_at: String,
    user_name: String,
    user_rowid: i64,
    granted_by_name: String,
    granter_rowid: i64,
}

/// A row of the grants one user holds: the grant with its account and both
/// people.
struct HeldRow {
    rowid: i64,
    account_id: String,
    user_id: String,
    level: String,
    granted_by: String,
    granted_at: String,
    account_name: String,
    account_rowid: i64,
    owner_id: String,
    owner_role: String,
    owner_name: String,
    owner_rowid: i64,
    granted_by_name: String,
    granter_rowid: i64,
}

/// A held grant as read back; a value that doesn't read is
/// [`StoreError::Corrupt`], naming its table's row.
fn held_from_row(row: HeldRow) -> Result<HeldGrant, StoreError> {
    let grant = grant_from_row(&GrantRow {
        rowid: row.rowid,
        account_id: row.account_id,
        user_id: row.user_id,
        level: row.level,
        granted_by: row.granted_by,
        granted_at: row.granted_at,
    })?;
    let owner_corrupt = |column| StoreError::Corrupt {
        table: "users",
        column,
        rowid: row.owner_rowid,
    };
    let owner = UserRef {
        id: parse_id(&row.owner_id).ok_or_else(|| owner_corrupt("id"))?,
        role: row
            .owner_role
            .parse::<Role>()
            .map_err(|_| owner_corrupt("role"))?,
    };
    Ok(HeldGrant {
        grant,
        account_name: row
            .account_name
            .parse::<McUsername>()
            .map_err(|_| StoreError::Corrupt {
                table: "mc_accounts",
                column: "mc_name",
                rowid: row.account_rowid,
            })?,
        owner,
        owner_name: Username::try_from(row.owner_name.as_str())
            .map_err(|_| owner_corrupt("username"))?,
        granted_by_name: Username::try_from(row.granted_by_name.as_str()).map_err(|_| {
            StoreError::Corrupt {
                table: "users",
                column: "username",
                rowid: row.granter_rowid,
            }
        })?,
    })
}

/// One page of the grants on `account`, newest grantee first.
pub(super) async fn list(
    pool: &SqlitePool,
    account: AccountId,
    page: Page<UserId>,
) -> Result<Vec<ListedGrant>, StoreError> {
    let account = id_text(account);
    let before = page.before.map(|id| id_text(id).to_string());
    let fetch = page.limit.fetch();
    sqlx::query_as!(
        ListedRow,
        r#"SELECT account_grants.rowid AS "rowid!: i64",
                  account_grants.account_id AS "account_id!",
                  account_grants.user_id AS "user_id!", account_grants.level AS "level!",
                  account_grants.granted_by AS "granted_by!",
                  account_grants.granted_at AS "granted_at!",
                  grantee.username AS "user_name!", grantee.rowid AS "user_rowid!: i64",
                  granter.username AS "granted_by_name!",
                  granter.rowid AS "granter_rowid!: i64"
           FROM account_grants
           JOIN users AS grantee ON grantee.id = account_grants.user_id
           JOIN users AS granter ON granter.id = account_grants.granted_by
           WHERE account_grants.account_id = ?
             AND (? IS NULL OR account_grants.user_id < ?)
           ORDER BY account_grants.user_id DESC
           LIMIT ?"#,
        account,
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

/// One page of the grants `user` holds, newest account first.
pub(super) async fn held_by(
    pool: &SqlitePool,
    user: UserId,
    page: Page<AccountId>,
) -> Result<Vec<HeldGrant>, StoreError> {
    let user = id_text(user);
    let before = page.before.map(|id| id_text(id).to_string());
    let fetch = page.limit.fetch();
    sqlx::query_as!(
        HeldRow,
        r#"SELECT account_grants.rowid AS "rowid!: i64",
                  account_grants.account_id AS "account_id!",
                  account_grants.user_id AS "user_id!", account_grants.level AS "level!",
                  account_grants.granted_by AS "granted_by!",
                  account_grants.granted_at AS "granted_at!",
                  account.mc_name AS "account_name!", account.rowid AS "account_rowid!: i64",
                  owner.id AS "owner_id!", owner.role AS "owner_role!",
                  owner.username AS "owner_name!", owner.rowid AS "owner_rowid!: i64",
                  granter.username AS "granted_by_name!",
                  granter.rowid AS "granter_rowid!: i64"
           FROM account_grants
           JOIN mc_accounts AS account ON account.id = account_grants.account_id
           JOIN users AS owner ON owner.id = account.owner_user_id
           JOIN users AS granter ON granter.id = account_grants.granted_by
           WHERE account_grants.user_id = ?
             AND (? IS NULL OR account_grants.account_id < ?)
           ORDER BY account_grants.account_id DESC
           LIMIT ?"#,
        user,
        before,
        before,
        fetch
    )
    .fetch_all(pool)
    .await
    .map_err(store_error)?
    .into_iter()
    .map(held_from_row)
    .collect()
}

/// `user`'s grant on `account`.
pub(super) async fn find(
    conn: &mut SqliteConnection,
    account: AccountId,
    user: UserId,
) -> Result<Option<AccountGrant>, StoreError> {
    let account = id_text(account);
    let user = id_text(user);
    sqlx::query_as!(
        GrantRow,
        r#"SELECT rowid AS "rowid!: i64", account_id AS "account_id!", user_id AS "user_id!",
                  level AS "level!", granted_by AS "granted_by!", granted_at AS "granted_at!"
           FROM account_grants WHERE account_id = ? AND user_id = ?"#,
        account,
        user
    )
    .fetch_optional(conn)
    .await
    .map_err(store_error)?
    .map(|row| grant_from_row(&row))
    .transpose()
}

/// Inserts `grant`. A second grant to the same user on the same account is
/// [`InsertGrantError::Exists`].
pub(super) async fn insert(
    conn: &mut SqliteConnection,
    grant: &AccountGrant,
) -> Result<(), InsertGrantError> {
    let account = id_text(grant.account);
    let user = id_text(grant.user);
    let level = grant.level.as_str();
    let by = id_text(grant.granted_by);
    let granted_at = stored_time(grant.granted_at)?;
    sqlx::query!(
        "INSERT INTO account_grants (account_id, user_id, level, granted_by, granted_at)
         VALUES (?, ?, ?, ?, ?)",
        account,
        user,
        level,
        by,
        granted_at
    )
    .execute(conn)
    .await
    .map_err(|error| match unique_violation(&error).as_deref() {
        Some(EXISTING_GRANT) => InsertGrantError::Exists,
        _ => InsertGrantError::Store(store_error(error)),
    })?;
    Ok(())
}

/// Rewrites `user`'s grant on `account`.
pub(super) async fn set_level(
    conn: &mut SqliteConnection,
    account: AccountId,
    user: UserId,
    level: GrantLevel,
    by: UserId,
    at: DateTime<Utc>,
) -> Result<bool, StoreError> {
    let account = id_text(account);
    let user = id_text(user);
    let level = level.as_str();
    let by = id_text(by);
    let at = stored_time(at)?;
    let done = sqlx::query!(
        "UPDATE account_grants SET level = ?, granted_by = ?, granted_at = ?
         WHERE account_id = ? AND user_id = ?",
        level,
        by,
        at,
        account,
        user
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Deletes `user`'s grant on `account`.
pub(super) async fn delete(
    conn: &mut SqliteConnection,
    account: AccountId,
    user: UserId,
) -> Result<bool, StoreError> {
    let account = id_text(account);
    let user = id_text(user);
    let done = sqlx::query!(
        "DELETE FROM account_grants WHERE account_id = ? AND user_id = ?",
        account,
        user
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// The stored form of `at`, or [`StoreError::Unstorable`].
fn stored_time(at: DateTime<Utc>) -> Result<String, StoreError> {
    time_text(at).ok_or(StoreError::Unstorable {
        table: ACCOUNT_GRANTS,
        column: "granted_at",
    })
}

/// Checks every value of a stored row; one that doesn't read back is
/// [`StoreError::Corrupt`], naming the column and the rowid, never the
/// value.
fn grant_from_row(row: &GrantRow) -> Result<AccountGrant, StoreError> {
    let corrupt = |column| StoreError::Corrupt {
        table: ACCOUNT_GRANTS,
        column,
        rowid: row.rowid,
    };
    Ok(AccountGrant {
        account: parse_id(&row.account_id).ok_or_else(|| corrupt("account_id"))?,
        user: parse_id(&row.user_id).ok_or_else(|| corrupt("user_id"))?,
        level: row.level.parse().map_err(|_| corrupt("level"))?,
        granted_by: parse_id(&row.granted_by).ok_or_else(|| corrupt("granted_by"))?,
        granted_at: parse_time(&row.granted_at).ok_or_else(|| corrupt("granted_at"))?,
    })
}

/// A list row: the grant, then both names, each checked against its own
/// `users` row.
fn listed_from_row(row: ListedRow) -> Result<ListedGrant, StoreError> {
    let name = |text: &str, rowid| {
        Username::try_from(text).map_err(|_| StoreError::Corrupt {
            table: "users",
            column: "username",
            rowid,
        })
    };
    let user_name = name(&row.user_name, row.user_rowid)?;
    let granted_by_name = name(&row.granted_by_name, row.granter_rowid)?;
    let grant = grant_from_row(&GrantRow {
        rowid: row.rowid,
        account_id: row.account_id,
        user_id: row.user_id,
        level: row.level,
        granted_by: row.granted_by,
        granted_at: row.granted_at,
    })?;
    Ok(ListedGrant {
        grant,
        user_name,
        granted_by_name,
    })
}
