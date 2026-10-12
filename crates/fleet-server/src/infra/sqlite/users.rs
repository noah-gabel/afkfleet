//! The `users` table's SQL.

use fleet_core::authz::Role;
use fleet_core::id::UserId;
use fleet_core::value::Username;
use secrecy::SecretString;
use sqlx::{SqliteConnection, SqlitePool};

use super::convert::{id_text, parse_id, parse_time, time_text};
use super::error::{store_error, unique_violation};
use crate::ports::page::Page;
use crate::ports::store::StoreError;
use crate::ports::users::{InsertUserError, ListedUser, NewUser, PasswordHash, User};

/// The table, as errors name it.
const TABLE: &str = "users";

/// A row as stored, before its values are checked.
struct UserRow {
    rowid: i64,
    id: String,
    username: String,
    password_hash: String,
    role: String,
    disabled: i64,
    created_at: String,
    password_changed_at: String,
}

/// Inserts `user`, enabled, with `password_changed_at` = `created_at`.
pub(super) async fn insert(
    conn: &mut SqliteConnection,
    user: &NewUser,
) -> Result<(), InsertUserError> {
    let id = id_text(user.id);
    let username = user.username.as_str();
    let password_hash = user.password_hash.expose_secret();
    let role = user.role.as_str();
    let created_at = time_text(user.created_at).ok_or(StoreError::Unstorable {
        table: TABLE,
        column: "created_at",
    })?;
    sqlx::query!(
        "INSERT INTO users (id, username, password_hash, role, disabled, created_at, password_changed_at)
         VALUES (?, ?, ?, ?, 0, ?, ?)",
        id,
        username,
        password_hash,
        role,
        created_at,
        created_at
    )
    .execute(conn)
    .await
    .map_err(|error| match unique_violation(&error).as_deref() {
        Some("users.username") => InsertUserError::UsernameTaken,
        Some("users.role") => InsertUserError::OwnerExists,
        _ => InsertUserError::Store(store_error(error)),
    })?;
    Ok(())
}

/// The user with this ID.
pub(super) async fn get(pool: &SqlitePool, id: UserId) -> Result<Option<User>, StoreError> {
    let id = id_text(id);
    sqlx::query_as!(
        UserRow,
        r#"SELECT rowid AS "rowid!: i64", id, username, password_hash, role, disabled,
                  created_at, password_changed_at
           FROM users WHERE id = ?"#,
        id
    )
    .fetch_optional(pool)
    .await
    .map_err(store_error)?
    .map(user_from_row)
    .transpose()
}

/// The user with this normalized username.
pub(super) async fn find_by_username(
    pool: &SqlitePool,
    username: &Username,
) -> Result<Option<User>, StoreError> {
    let username = username.as_str();
    sqlx::query_as!(
        UserRow,
        r#"SELECT rowid AS "rowid!: i64", id, username, password_hash, role, disabled,
                  created_at, password_changed_at
           FROM users WHERE username = ?"#,
        username
    )
    .fetch_optional(pool)
    .await
    .map_err(store_error)?
    .map(user_from_row)
    .transpose()
}

/// The user with this ID, read in a write transaction.
pub(super) async fn get_in(
    conn: &mut SqliteConnection,
    id: UserId,
) -> Result<Option<User>, StoreError> {
    let id = id_text(id);
    sqlx::query_as!(
        UserRow,
        r#"SELECT rowid AS "rowid!: i64", id, username, password_hash, role, disabled,
                  created_at, password_changed_at
           FROM users WHERE id = ?"#,
        id
    )
    .fetch_optional(conn)
    .await
    .map_err(store_error)?
    .map(user_from_row)
    .transpose()
}

/// Replaces the user's password hash; `password_changed_at` stays.
pub(super) async fn set_password_hash(
    conn: &mut SqliteConnection,
    id: UserId,
    hash: &PasswordHash,
) -> Result<bool, StoreError> {
    let id = id_text(id);
    let hash = hash.expose_secret();
    let done = sqlx::query!("UPDATE users SET password_hash = ? WHERE id = ?", hash, id)
        .execute(conn)
        .await
        .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// The users of `page`, newest first, as the list shows them: one more
/// than the page holds.
pub(super) async fn list(
    pool: &SqlitePool,
    page: Page<UserId>,
) -> Result<Vec<ListedUser>, StoreError> {
    let before = page.before.map(|id| id_text(id).to_string());
    let fetch = page.limit.fetch();
    sqlx::query_as!(
        ListedRow,
        r#"SELECT users.rowid AS "rowid!: i64", users.id AS "id!", users.username AS "username!",
                  users.role AS "role!", users.disabled AS "disabled!: i64",
                  users.created_at AS "created_at!",
                  EXISTS (SELECT 1 FROM user_mfa WHERE user_mfa.user_id = users.id)
                      AS "mfa_enrolled!: bool",
                  (SELECT max(sessions.last_used_at) FROM sessions
                   WHERE sessions.user_id = users.id) AS "last_seen_at?: String"
           FROM users
           WHERE ? IS NULL OR users.id < ?
           ORDER BY users.id DESC
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

/// The user with this ID, as the list shows them.
pub(super) async fn listed(
    pool: &SqlitePool,
    id: UserId,
) -> Result<Option<ListedUser>, StoreError> {
    let id = id_text(id);
    sqlx::query_as!(
        ListedRow,
        r#"SELECT users.rowid AS "rowid!: i64", users.id AS "id!", users.username AS "username!",
                  users.role AS "role!", users.disabled AS "disabled!: i64",
                  users.created_at AS "created_at!",
                  EXISTS (SELECT 1 FROM user_mfa WHERE user_mfa.user_id = users.id)
                      AS "mfa_enrolled!: bool",
                  (SELECT max(sessions.last_used_at) FROM sessions
                   WHERE sessions.user_id = users.id) AS "last_seen_at?: String"
           FROM users WHERE users.id = ?"#,
        id
    )
    .fetch_optional(pool)
    .await
    .map_err(store_error)?
    .map(listed_from_row)
    .transpose()
}

/// Sets the user's role.
pub(super) async fn set_role(
    conn: &mut SqliteConnection,
    id: UserId,
    role: Role,
) -> Result<bool, StoreError> {
    let id = id_text(id);
    let role = role.as_str();
    let done = sqlx::query!("UPDATE users SET role = ? WHERE id = ?", role, id)
        .execute(conn)
        .await
        .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Sets whether the user is disabled.
pub(super) async fn set_disabled(
    conn: &mut SqliteConnection,
    id: UserId,
    disabled: bool,
) -> Result<bool, StoreError> {
    let id = id_text(id);
    let disabled = i64::from(disabled);
    let done = sqlx::query!("UPDATE users SET disabled = ? WHERE id = ?", disabled, id)
        .execute(conn)
        .await
        .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Whether a user has the Owner role.
pub(super) async fn owner_exists(pool: &SqlitePool) -> Result<bool, StoreError> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM users WHERE role = 'owner') AS "exists!: bool""#
    )
    .fetch_one(pool)
    .await
    .map_err(store_error)
}

/// A listed row, before its values are checked.
struct ListedRow {
    rowid: i64,
    id: String,
    username: String,
    role: String,
    disabled: i64,
    created_at: String,
    mfa_enrolled: bool,
    last_seen_at: Option<String>,
}

/// Checks every value of a listed row, as [`user_from_row`] does; a bad
/// `last_seen_at` names the users row, since the time is the newest of its
/// sessions'.
fn listed_from_row(row: ListedRow) -> Result<ListedUser, StoreError> {
    let corrupt = |column| StoreError::Corrupt {
        table: TABLE,
        column,
        rowid: row.rowid,
    };
    Ok(ListedUser {
        id: parse_id(&row.id).ok_or_else(|| corrupt("id"))?,
        username: Username::try_from(row.username.as_str()).map_err(|_| corrupt("username"))?,
        role: row.role.parse().map_err(|_| corrupt("role"))?,
        disabled: match row.disabled {
            0 => false,
            1 => true,
            _ => return Err(corrupt("disabled")),
        },
        created_at: parse_time(&row.created_at).ok_or_else(|| corrupt("created_at"))?,
        mfa_enrolled: row.mfa_enrolled,
        last_seen_at: row
            .last_seen_at
            .map(|text| parse_time(&text).ok_or_else(|| corrupt("last_seen_at")))
            .transpose()?,
    })
}

/// Checks every value of a stored row; one that doesn't read back is
/// [`StoreError::Corrupt`], naming the column and the rowid but never the
/// value.
fn user_from_row(row: UserRow) -> Result<User, StoreError> {
    let corrupt = |column| StoreError::Corrupt {
        table: TABLE,
        column,
        rowid: row.rowid,
    };
    Ok(User {
        id: parse_id(&row.id).ok_or_else(|| corrupt("id"))?,
        username: Username::try_from(row.username.as_str()).map_err(|_| corrupt("username"))?,
        role: row.role.parse().map_err(|_| corrupt("role"))?,
        disabled: match row.disabled {
            0 => false,
            1 => true,
            _ => return Err(corrupt("disabled")),
        },
        created_at: parse_time(&row.created_at).ok_or_else(|| corrupt("created_at"))?,
        password_changed_at: parse_time(&row.password_changed_at)
            .ok_or_else(|| corrupt("password_changed_at"))?,
        password_hash: PasswordHash::new(SecretString::from(row.password_hash))
            .map_err(|_| corrupt("password_hash"))?,
    })
}
