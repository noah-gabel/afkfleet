//! The `bots` table's SQL.
//!
//! A bot's stored values are read back through fleet-core's types: the
//! server address through [`ServerAddress`], the desired state and the last
//! state through their stored names. A value that doesn't read back is
//! [`StoreError::Corrupt`], naming its column and never the value.
//!
//! **Assignments** change only conditionally: [`assign`] from no agent, to an
//! enabled one; [`detach`] and [`release_all`] only what names the agent.
//! **Release records** are written with `ON CONFLICT DO NOTHING` on their key,
//! the bot and the agent, never `OR IGNORE`, which would hide a CHECK's
//! refusal too.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use fleet_core::account::{AccountKind, AccountStatus};
use fleet_core::bot::{BotAccount, DesiredRunState, LastReport, LastState};
use fleet_core::id::{AccountId, AgentId, BotId};
use fleet_core::mode::ModeDefinition;
use fleet_core::placement::McIdentity;
use fleet_core::value::{McUsername, McUuid, ServerAddress};
use sqlx::SqliteConnection;

use super::convert::{id_text, parse_id, parse_time, time_text};
use super::error::store_error;
use crate::ports::bots::{Bot, BotRecord, Fleet, NewBot, ReleaseRecord, UnreadableBot};
use crate::ports::page::Page;
use crate::ports::store::StoreError;

/// The tables, as errors name them.
const BOTS: &str = "bots";
const RELEASING_BOTS: &str = "releasing_bots";

/// A bot's row with its account, owner and mode, before its values are
/// checked.
struct RecordRow {
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
    account_rowid: i64,
    kind: String,
    mc_uuid: Option<String>,
    mc_name: String,
    status: String,
    status_reason: Option<String>,
    owner_rowid: i64,
    owner_disabled: i64,
    mode_rowid: i64,
    definition_json: String,
}

/// A `releasing_bots` row, before its values are checked.
struct ReleaseRow {
    rowid: i64,
    bot_id: String,
    agent_id: String,
    kind: String,
    mc_uuid: Option<String>,
    mc_name: String,
}

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

/// The agent `id`'s row assigns it to, from that column alone.
pub(super) async fn agent_of(
    conn: &mut SqliteConnection,
    id: BotId,
) -> Result<Option<Option<AgentId>>, StoreError> {
    let id = id_text(id);
    let Some(row) = sqlx::query!(
        r#"SELECT rowid AS "rowid!: i64", agent_id AS "agent_id?" FROM bots WHERE id = ?"#,
        id
    )
    .fetch_optional(&mut *conn)
    .await
    .map_err(store_error)?
    else {
        return Ok(None);
    };
    let agent = row
        .agent_id
        .as_deref()
        .map(|agent| {
            parse_id(agent).ok_or(StoreError::Corrupt {
                table: BOTS,
                column: "agent_id",
                rowid: row.rowid,
            })
        })
        .transpose()?;
    Ok(Some(agent))
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

/// The bot `id` with its account, owner and mode.
pub(super) async fn record<'c, E>(executor: E, id: BotId) -> Result<Option<BotRecord>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let id = id_text(id);
    sqlx::query_as!(
        RecordRow,
        r#"SELECT b.rowid AS "rowid!: i64", b.id AS "id!", b.account_id AS "account_id!",
                  b.server_address AS "server_address?", b.mode_id AS "mode_id!",
                  b.desired_state AS "desired_state!", b.agent_id AS "agent_id?",
                  b.last_state AS "last_state?", b.last_state_reason AS "last_state_reason?",
                  b.last_state_at AS "last_state_at?",
                  a.rowid AS "account_rowid!: i64", a.kind AS "kind!", a.mc_uuid AS "mc_uuid?",
                  a.mc_name AS "mc_name!", a.status AS "status!",
                  a.status_reason AS "status_reason?",
                  u.rowid AS "owner_rowid!: i64", u.disabled AS "owner_disabled!: i64",
                  m.rowid AS "mode_rowid!: i64", m.definition_json AS "definition_json!"
           FROM bots b
           JOIN mc_accounts a ON a.id = b.account_id
           JOIN users u ON u.id = a.owner_user_id
           JOIN modes m ON m.id = b.mode_id
           WHERE b.id = ?"#,
        id
    )
    .fetch_optional(executor)
    .await
    .map_err(store_error)?
    .map(|row| record_from_row(&row))
    .transpose()
}

/// The bots of `page` with their accounts, owners and modes, newest first:
/// one more than its limit when another page follows.
pub(super) async fn records<'c, E>(
    executor: E,
    page: Page<BotId>,
) -> Result<Vec<BotRecord>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let before = page.before.map(|id| id_text(id).to_string());
    let fetch = page.limit.fetch();
    sqlx::query_as!(
        RecordRow,
        r#"SELECT b.rowid AS "rowid!: i64", b.id AS "id!", b.account_id AS "account_id!",
                  b.server_address AS "server_address?", b.mode_id AS "mode_id!",
                  b.desired_state AS "desired_state!", b.agent_id AS "agent_id?",
                  b.last_state AS "last_state?", b.last_state_reason AS "last_state_reason?",
                  b.last_state_at AS "last_state_at?",
                  a.rowid AS "account_rowid!: i64", a.kind AS "kind!", a.mc_uuid AS "mc_uuid?",
                  a.mc_name AS "mc_name!", a.status AS "status!",
                  a.status_reason AS "status_reason?",
                  u.rowid AS "owner_rowid!: i64", u.disabled AS "owner_disabled!: i64",
                  m.rowid AS "mode_rowid!: i64", m.definition_json AS "definition_json!"
           FROM bots b
           JOIN mc_accounts a ON a.id = b.account_id
           JOIN users u ON u.id = a.owner_user_id
           JOIN modes m ON m.id = b.mode_id
           WHERE ? IS NULL OR b.id < ?
           ORDER BY b.id DESC
           LIMIT ?"#,
        before,
        before,
        fetch
    )
    .fetch_all(executor)
    .await
    .map_err(store_error)?
    .iter()
    .map(record_from_row)
    .collect()
}

/// Every bot, every open release record and the enabled agents, in one read
/// transaction, so the three agree with each other (WAL gives the
/// transaction one snapshot).
pub(super) async fn fleet(pool: &sqlx::SqlitePool) -> Result<Fleet, StoreError> {
    let mut tx = pool.begin().await.map_err(store_error)?;
    let bot_rows = sqlx::query_as!(
        RecordRow,
        r#"SELECT b.rowid AS "rowid!: i64", b.id AS "id!", b.account_id AS "account_id!",
                  b.server_address AS "server_address?", b.mode_id AS "mode_id!",
                  b.desired_state AS "desired_state!", b.agent_id AS "agent_id?",
                  b.last_state AS "last_state?", b.last_state_reason AS "last_state_reason?",
                  b.last_state_at AS "last_state_at?",
                  a.rowid AS "account_rowid!: i64", a.kind AS "kind!", a.mc_uuid AS "mc_uuid?",
                  a.mc_name AS "mc_name!", a.status AS "status!",
                  a.status_reason AS "status_reason?",
                  u.rowid AS "owner_rowid!: i64", u.disabled AS "owner_disabled!: i64",
                  m.rowid AS "mode_rowid!: i64", m.definition_json AS "definition_json!"
           FROM bots b
           JOIN mc_accounts a ON a.id = b.account_id
           JOIN users u ON u.id = a.owner_user_id
           JOIN modes m ON m.id = b.mode_id
           ORDER BY b.id"#
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(store_error)?;
    let mut bots = Vec::new();
    let mut unreadable = Vec::new();
    for row in &bot_rows {
        match record_from_row(row) {
            Ok(record) => bots.push(record),
            Err(StoreError::Corrupt {
                table,
                column,
                rowid,
            }) => unreadable.push(unreadable_bot(row, table, column, rowid)?),
            Err(other) => return Err(other),
        }
    }
    let release_rows = sqlx::query_as!(
        ReleaseRow,
        r#"SELECT rowid AS "rowid!: i64", bot_id AS "bot_id!", agent_id AS "agent_id!",
                  kind AS "kind!", mc_uuid AS "mc_uuid?", mc_name AS "mc_name!"
           FROM releasing_bots
           ORDER BY bot_id, agent_id"#
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(store_error)?;
    let mut releases = Vec::new();
    for row in &release_rows {
        match release_from_row(row) {
            Ok(record) => releases.push(record),
            Err(StoreError::Corrupt {
                table,
                column,
                rowid,
            }) => unreadable.push(unreadable_record(row, table, column, rowid)?),
            Err(other) => return Err(other),
        }
    }
    let enabled_agents =
        sqlx::query!(r#"SELECT rowid AS "rowid!: i64", id AS "id!" FROM agents WHERE enabled = 1"#)
            .fetch_all(&mut *tx)
            .await
            .map_err(store_error)?
            .into_iter()
            .map(|row| {
                parse_id(&row.id).ok_or(StoreError::Corrupt {
                    table: "agents",
                    column: "id",
                    rowid: row.rowid,
                })
            })
            .collect::<Result<BTreeSet<AgentId>, _>>()?;
    // Read only: nothing to commit, and dropping it ends it.
    drop(tx);
    Ok(Fleet {
        bots,
        releases,
        unreadable,
        enabled_agents,
    })
}

/// Assigns `bot` to `agent` while it has none and `agent` is enabled.
pub(super) async fn assign(
    conn: &mut SqliteConnection,
    bot: BotId,
    agent: AgentId,
) -> Result<bool, StoreError> {
    let bot = id_text(bot);
    let agent = id_text(agent);
    let done = sqlx::query!(
        "UPDATE bots SET agent_id = ?
         WHERE id = ? AND agent_id IS NULL
           AND EXISTS (SELECT 1 FROM agents WHERE agents.id = ? AND agents.enabled = 1)",
        agent,
        bot,
        agent
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Stores `report` as `bot`'s last state while its row names `agent` and
/// no later one is stored.
pub(super) async fn record_state(
    conn: &mut SqliteConnection,
    bot: BotId,
    agent: AgentId,
    report: &LastReport,
) -> Result<bool, StoreError> {
    let bot = id_text(bot);
    let agent = id_text(agent);
    let state = report.state.as_str();
    let reason = report.state.reason();
    let at = time_text(report.at).ok_or(StoreError::Unstorable {
        table: BOTS,
        column: "last_state_at",
    })?;
    let done = sqlx::query!(
        "UPDATE bots SET last_state = ?, last_state_reason = ?, last_state_at = ?
         WHERE id = ? AND agent_id = ? AND (last_state_at IS NULL OR last_state_at <= ?)",
        state,
        reason,
        at,
        bot,
        agent,
        at
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Clears `bot`'s assignment to `agent` and `agent`'s record of it.
pub(super) async fn detach(
    conn: &mut SqliteConnection,
    bot: BotId,
    agent: AgentId,
) -> Result<bool, StoreError> {
    let bot = id_text(bot);
    let agent = id_text(agent);
    let cleared = sqlx::query!(
        "UPDATE bots SET agent_id = NULL WHERE id = ? AND agent_id = ?",
        bot,
        agent
    )
    .execute(&mut *conn)
    .await
    .map_err(store_error)?;
    let closed = sqlx::query!(
        "DELETE FROM releasing_bots WHERE bot_id = ? AND agent_id = ?",
        bot,
        agent
    )
    .execute(&mut *conn)
    .await
    .map_err(store_error)?;
    Ok(cleared.rows_affected() > 0 || closed.rows_affected() > 0)
}

/// Writes a release record of `bot` for `agent`: from its row and account,
/// or from another agent's record of it when the row is gone.
pub(super) async fn stray(
    conn: &mut SqliteConnection,
    bot: BotId,
    agent: AgentId,
    at: DateTime<Utc>,
) -> Result<bool, StoreError> {
    let bot = id_text(bot);
    let agent = id_text(agent);
    let at = time_text(at).ok_or(StoreError::Unstorable {
        table: RELEASING_BOTS,
        column: "created_at",
    })?;
    let from_row = sqlx::query!(
        "INSERT INTO releasing_bots (bot_id, agent_id, account_id, kind, mc_uuid, mc_name, created_at)
         SELECT b.id, ?, b.account_id, a.kind, a.mc_uuid, a.mc_name, ?
         FROM bots b JOIN mc_accounts a ON a.id = b.account_id
         WHERE b.id = ?
         ON CONFLICT (bot_id, agent_id) DO NOTHING",
        agent,
        at,
        bot
    )
    .execute(&mut *conn)
    .await
    .map_err(store_error)?;
    if from_row.rows_affected() > 0 {
        return Ok(true);
    }
    let from_record = sqlx::query!(
        "INSERT INTO releasing_bots (bot_id, agent_id, account_id, kind, mc_uuid, mc_name, created_at)
         SELECT r.bot_id, ?, r.account_id, r.kind, r.mc_uuid, r.mc_name, ?
         FROM releasing_bots r
         WHERE r.bot_id = ? AND r.agent_id <> ?
           AND NOT EXISTS (SELECT 1 FROM bots WHERE bots.id = r.bot_id)
         ORDER BY r.agent_id
         LIMIT 1
         ON CONFLICT (bot_id, agent_id) DO NOTHING",
        agent,
        at,
        bot,
        agent
    )
    .execute(&mut *conn)
    .await
    .map_err(store_error)?;
    Ok(from_record.rows_affected() > 0)
}

/// Clears every assignment to `agent` and its release records: the bots, each
/// once, in ID order.
pub(super) async fn release_all(
    conn: &mut SqliteConnection,
    agent: AgentId,
) -> Result<Vec<BotId>, StoreError> {
    let agent = id_text(agent);
    let assigned = sqlx::query!(
        r#"SELECT rowid AS "rowid!: i64", id AS "id!" FROM bots WHERE agent_id = ?"#,
        agent
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(store_error)?;
    let recorded = sqlx::query!(
        r#"SELECT rowid AS "rowid!: i64", bot_id AS "id!" FROM releasing_bots WHERE agent_id = ?"#,
        agent
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(store_error)?;
    let mut released = BTreeSet::new();
    for row in assigned {
        released.insert(parse_id(&row.id).ok_or(StoreError::Corrupt {
            table: BOTS,
            column: "id",
            rowid: row.rowid,
        })?);
    }
    for row in recorded {
        released.insert(parse_id(&row.id).ok_or(StoreError::Corrupt {
            table: RELEASING_BOTS,
            column: "bot_id",
            rowid: row.rowid,
        })?);
    }
    sqlx::query!("UPDATE bots SET agent_id = NULL WHERE agent_id = ?", agent)
        .execute(&mut *conn)
        .await
        .map_err(store_error)?;
    sqlx::query!("DELETE FROM releasing_bots WHERE agent_id = ?", agent)
        .execute(&mut *conn)
        .await
        .map_err(store_error)?;
    Ok(released.into_iter().collect())
}

/// Reads a bot's row with its account, owner and mode back, or
/// [`StoreError::Corrupt`] naming the first column that doesn't.
fn record_from_row(row: &RecordRow) -> Result<BotRecord, StoreError> {
    let bot = bot_from_row(&BotRow {
        rowid: row.rowid,
        id: row.id.clone(),
        account_id: row.account_id.clone(),
        server_address: row.server_address.clone(),
        mode_id: row.mode_id.clone(),
        desired_state: row.desired_state.clone(),
        agent_id: row.agent_id.clone(),
        last_state: row.last_state.clone(),
        last_state_reason: row.last_state_reason.clone(),
        last_state_at: row.last_state_at.clone(),
    })?;
    let account = |column| StoreError::Corrupt {
        table: "mc_accounts",
        column,
        rowid: row.account_rowid,
    };
    let kind: AccountKind = row.kind.parse().map_err(|_| account("kind"))?;
    let status = AccountStatus::from_columns(&row.status, row.status_reason.as_deref())
        .map_err(|_| account("status"))?;
    let name = McUsername::try_from(row.mc_name.as_str()).map_err(|_| account("mc_name"))?;
    let (spec_account, identity) = match kind {
        AccountKind::Microsoft => {
            let uuid: McUuid = row
                .mc_uuid
                .as_deref()
                .and_then(|uuid| uuid.parse().ok())
                .ok_or_else(|| account("mc_uuid"))?;
            (BotAccount::Online(bot.account), McIdentity::Microsoft(uuid))
        }
        AccountKind::Offline => (
            BotAccount::Offline(name.clone()),
            McIdentity::offline(&name),
        ),
    };
    let owner_enabled = match row.owner_disabled {
        0 => true,
        1 => false,
        _ => {
            return Err(StoreError::Corrupt {
                table: "users",
                column: "disabled",
                rowid: row.owner_rowid,
            });
        }
    };
    // serde_json's error can quote the stored text: it's dropped here.
    let mode: ModeDefinition =
        serde_json::from_str(&row.definition_json).map_err(|_| StoreError::Corrupt {
            table: "modes",
            column: "definition_json",
            rowid: row.mode_rowid,
        })?;
    Ok(BotRecord {
        bot,
        account: spec_account,
        identity,
        kind,
        status,
        owner_enabled,
        mode,
    })
}

/// A bot `row` whose `table`'s `column` doesn't read back, as far as it
/// does: its ID and agent (checked by the schema, so they always do), and its
/// account's identity if that reads.
fn unreadable_bot(
    row: &RecordRow,
    table: &'static str,
    column: &'static str,
    rowid: i64,
) -> Result<UnreadableBot, StoreError> {
    let corrupt = |column| StoreError::Corrupt {
        table: BOTS,
        column,
        rowid: row.rowid,
    };
    Ok(UnreadableBot {
        bot: parse_id(&row.id).ok_or_else(|| corrupt("id"))?,
        agent: row
            .agent_id
            .as_deref()
            .map(|agent| parse_id(agent).ok_or_else(|| corrupt("agent_id")))
            .transpose()?,
        identity: identity_of(&row.kind, row.mc_uuid.as_deref(), &row.mc_name),
        table,
        column,
        rowid,
    })
}

/// A release record `row` whose `column` doesn't read back, as far as it
/// does; see [`unreadable_bot`].
fn unreadable_record(
    row: &ReleaseRow,
    table: &'static str,
    column: &'static str,
    rowid: i64,
) -> Result<UnreadableBot, StoreError> {
    let corrupt = |column| StoreError::Corrupt {
        table: RELEASING_BOTS,
        column,
        rowid: row.rowid,
    };
    Ok(UnreadableBot {
        bot: parse_id(&row.bot_id).ok_or_else(|| corrupt("bot_id"))?,
        agent: Some(parse_id(&row.agent_id).ok_or_else(|| corrupt("agent_id"))?),
        identity: identity_of(&row.kind, row.mc_uuid.as_deref(), &row.mc_name),
        table,
        column,
        rowid,
    })
}

/// The Minecraft account `kind`, `uuid` and `name` describe, if they read.
fn identity_of(kind: &str, uuid: Option<&str>, name: &str) -> Option<McIdentity> {
    match kind.parse::<AccountKind>().ok()? {
        AccountKind::Microsoft => Some(McIdentity::Microsoft(uuid?.parse().ok()?)),
        AccountKind::Offline => Some(McIdentity::offline(&McUsername::try_from(name).ok()?)),
    }
}

/// Reads a `releasing_bots` row back, or [`StoreError::Corrupt`] naming the
/// first column that doesn't.
fn release_from_row(row: &ReleaseRow) -> Result<ReleaseRecord, StoreError> {
    let corrupt = |column| StoreError::Corrupt {
        table: RELEASING_BOTS,
        column,
        rowid: row.rowid,
    };
    let identity = match row
        .kind
        .parse::<AccountKind>()
        .map_err(|_| corrupt("kind"))?
    {
        AccountKind::Microsoft => McIdentity::Microsoft(
            row.mc_uuid
                .as_deref()
                .and_then(|uuid| uuid.parse().ok())
                .ok_or_else(|| corrupt("mc_uuid"))?,
        ),
        AccountKind::Offline => McIdentity::offline(
            &McUsername::try_from(row.mc_name.as_str()).map_err(|_| corrupt("mc_name"))?,
        ),
    };
    Ok(ReleaseRecord {
        bot: parse_id(&row.bot_id).ok_or_else(|| corrupt("bot_id"))?,
        agent: parse_id(&row.agent_id).ok_or_else(|| corrupt("agent_id"))?,
        identity,
    })
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
