//! The `bots` table's SQL.
//!
//! A bot's stored values are read back through fleet-core's types: the
//! server address through [`ServerAddress`], the desired state and the last
//! state through their stored names. A value that doesn't read back is
//! [`StoreError::Corrupt`], naming its column and never the value.

use fleet_core::bot::{DesiredRunState, LastReport, LastState};
use fleet_core::id::{AccountId, BotId};
use fleet_core::value::ServerAddress;
use sqlx::SqliteConnection;

use super::convert::{id_text, parse_id, parse_time};
use super::error::store_error;
use crate::ports::bots::{Bot, NewBot};
use crate::ports::store::StoreError;

/// The table, as errors name it.
const BOTS: &str = "bots";

/// A `bots` row, before its values are checked.
struct BotRow {
    rowid: i64,
    id: String,
    account_id: String,
    server_address: Option<String>,
    mode_id: String,
    desired_state: String,
    agent_id: Option<String>,
    last_state: Option<String>,
    last_state_reason: Option<String>,
    last_state_at: Option<String>,
}

/// The bot `id`.
pub(super) async fn find<'c, E>(executor: E, id: BotId) -> Result<Option<Bot>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let id = id_text(id);
    sqlx::query_as!(
        BotRow,
        r#"SELECT rowid AS "rowid!: i64", id AS "id!", account_id AS "account_id!",
                  server_address AS "server_address?", mode_id AS "mode_id!",
                  desired_state AS "desired_state!", agent_id AS "agent_id?",
                  last_state AS "last_state?", last_state_reason AS "last_state_reason?",
                  last_state_at AS "last_state_at?"
           FROM bots WHERE id = ?"#,
        id
    )
    .fetch_optional(executor)
    .await
    .map_err(store_error)?
    .map(|row| bot_from_row(&row))
    .transpose()
}

/// The bot of the account `account`.
pub(super) async fn of_account<'c, E>(
    executor: E,
    account: AccountId,
) -> Result<Option<Bot>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let account = id_text(account);
    sqlx::query_as!(
        BotRow,
        r#"SELECT rowid AS "rowid!: i64", id AS "id!", account_id AS "account_id!",
                  server_address AS "server_address?", mode_id AS "mode_id!",
                  desired_state AS "desired_state!", agent_id AS "agent_id?",
                  last_state AS "last_state?", last_state_reason AS "last_state_reason?",
                  last_state_at AS "last_state_at?"
           FROM bots WHERE account_id = ?"#,
        account
    )
    .fetch_optional(executor)
    .await
    .map_err(store_error)?
    .map(|row| bot_from_row(&row))
    .transpose()
}

/// Inserts `bot`: no server, stopped, unassigned, no state reported.
pub(super) async fn insert(conn: &mut SqliteConnection, bot: &NewBot) -> Result<(), StoreError> {
    let id = id_text(bot.id);
    let account = id_text(bot.account);
    let mode = id_text(bot.mode);
    let stopped = DesiredRunState::Stopped.as_str();
    sqlx::query!(
        "INSERT INTO bots (id, account_id, server_address, mode_id, desired_state)
         VALUES (?, ?, NULL, ?, ?)",
        id,
        account,
        mode,
        stopped
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(())
}

/// Reads a `bots` row back, or [`StoreError::Corrupt`] naming the first
/// column that doesn't.
fn bot_from_row(row: &BotRow) -> Result<Bot, StoreError> {
    let rowid = row.rowid;
    let corrupt = |column| StoreError::Corrupt {
        table: BOTS,
        column,
        rowid,
    };
    let server = row
        .server_address
        .as_deref()
        .map(|server| ServerAddress::try_from(server).map_err(|_| corrupt("server_address")))
        .transpose()?;
    let agent = row
        .agent_id
        .as_deref()
        .map(|agent| parse_id(agent).ok_or_else(|| corrupt("agent_id")))
        .transpose()?;
    let last = match (row.last_state.as_deref(), row.last_state_at.as_deref()) {
        (Some(state), Some(at)) => Some(LastReport {
            state: LastState::from_columns(state, row.last_state_reason.as_deref())
                .map_err(|_| corrupt("last_state"))?,
            at: parse_time(at).ok_or_else(|| corrupt("last_state_at"))?,
        }),
        (None, None) => None,
        // The table's CHECK keeps them together.
        (Some(_), None) | (None, Some(_)) => return Err(corrupt("last_state_at")),
    };
    Ok(Bot {
        id: parse_id(&row.id).ok_or_else(|| corrupt("id"))?,
        account: parse_id(&row.account_id).ok_or_else(|| corrupt("account_id"))?,
        server,
        mode: parse_id(&row.mode_id).ok_or_else(|| corrupt("mode_id"))?,
        desired: row
            .desired_state
            .parse()
            .map_err(|_| corrupt("desired_state"))?,
        agent,
        last,
    })
}
