//! The `users` table's SQL.

use fleet_core::id::UserId;
use fleet_core::value::Username;
use secrecy::SecretString;
use sqlx::{SqliteConnection, SqlitePool};

use super::convert::{id_text, parse_id, parse_time, time_text};
use super::error::{store_error, unique_violation};
use crate::ports::store::StoreError;
use crate::ports::users::{InsertUserError, NewUser, PasswordHash, User};

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
