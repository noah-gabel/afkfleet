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

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use fleet_core::account::{AccountKind, AccountStatus};
use fleet_core::authz::{Actor, GrantLevel, Role};
use fleet_core::bot::{BotAccount, DesiredRunState, LastReport, LastState};
use fleet_core::id::{AccountId, AgentId, BotId, ModeId, UserId};
use fleet_core::mode::ModeDefinition;
use fleet_core::placement::McIdentity;
use fleet_core::value::{McUsername, McUuid, ServerAddress, ServerKey};
use sqlx::SqliteConnection;

use super::convert::{id_text, parse_id, parse_time, time_text};
use super::error::store_error;
use super::{accounts, server_settings};
use crate::ports::bots::{
    Access, Bot, BotAccess, BotRecord, Fleet, GrantAccess, ListedBot, NewBot, ReleaseRecord,
    UnreadableBot, UserAccess,
};
use crate::ports::page::Page;
use crate::ports::server_settings::ServerTexts;
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

/// A bot's row with its account, owner and mode, and that account as one
/// viewer sees it, before its values are checked.
struct ListedBotRow {
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
    created_at: String,
    owner_user_id: String,
    owner_role: String,
    owner_name: String,
    owner_rowid: i64,
    owner_disabled: i64,
    mode_rowid: i64,
    definition_json: String,
    grant_level: Option<String>,
    grant_rowid: Option<i64>,
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

/// The ID of the account `account`'s bot.
pub(super) async fn id_of_account(
    conn: &mut SqliteConnection,
    account: AccountId,
) -> Result<Option<BotId>, StoreError> {
    let account = id_text(account);
    sqlx::query!(
        r#"SELECT rowid AS "rowid!: i64", id AS "id!" FROM bots WHERE account_id = ?"#,
        account
    )
    .fetch_optional(&mut *conn)
    .await
    .map_err(store_error)?
    .map(|row| bot_id(row.rowid, &row.id))
    .transpose()
}

/// The IDs of `owner`'s accounts' bots, in ID order.
pub(super) async fn owned_by(
    conn: &mut SqliteConnection,
    owner: UserId,
) -> Result<Vec<BotId>, StoreError> {
    let owner = id_text(owner);
    sqlx::query!(
        r#"SELECT b.rowid AS "rowid!: i64", b.id AS "id!"
           FROM bots b JOIN mc_accounts a ON a.id = b.account_id
           WHERE a.owner_user_id = ?
           ORDER BY b.id"#,
        owner
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(store_error)?
    .iter()
    .map(|row| bot_id(row.rowid, &row.id))
    .collect()
}

/// The IDs of the bots assigned to `agent`, in ID order.
pub(super) async fn assigned_to(
    conn: &mut SqliteConnection,
    agent: AgentId,
) -> Result<Vec<BotId>, StoreError> {
    let agent = id_text(agent);
    sqlx::query!(
        r#"SELECT rowid AS "rowid!: i64", id AS "id!" FROM bots WHERE agent_id = ? ORDER BY id"#,
        agent
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(store_error)?
    .iter()
    .map(|row| bot_id(row.rowid, &row.id))
    .collect()
}

/// A bot's ID as its row `rowid` stores it.
fn bot_id(rowid: i64, id: &str) -> Result<BotId, StoreError> {
    parse_id(id).ok_or(StoreError::Corrupt {
        table: BOTS,
        column: "id",
        rowid,
    })
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

/// The bot `id` as `viewer` sees it, with their grant on its account.
pub(super) async fn listed<'c, E>(
    executor: E,
    id: BotId,
    viewer: UserId,
) -> Result<Option<ListedBot>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let id = id_text(id);
    let viewer = id_text(viewer);
    sqlx::query_as!(
        ListedBotRow,
        r#"SELECT b.rowid AS "rowid!: i64", b.id AS "id!", b.account_id AS "account_id!",
                  b.server_address AS "server_address?", b.mode_id AS "mode_id!",
                  b.desired_state AS "desired_state!", b.agent_id AS "agent_id?",
                  b.last_state AS "last_state?", b.last_state_reason AS "last_state_reason?",
                  b.last_state_at AS "last_state_at?",
                  a.rowid AS "account_rowid!: i64", a.kind AS "kind!", a.mc_uuid AS "mc_uuid?",
                  a.mc_name AS "mc_name!", a.status AS "status!",
                  a.status_reason AS "status_reason?", a.created_at AS "created_at!",
                  a.owner_user_id AS "owner_user_id!", u.role AS "owner_role!",
                  u.username AS "owner_name!", u.rowid AS "owner_rowid!: i64",
                  u.disabled AS "owner_disabled!: i64",
                  m.rowid AS "mode_rowid!: i64", m.definition_json AS "definition_json!",
                  g.level AS "grant_level?", g.rowid AS "grant_rowid?: i64"
           FROM bots b
           JOIN mc_accounts a ON a.id = b.account_id
           JOIN users u ON u.id = a.owner_user_id
           JOIN modes m ON m.id = b.mode_id
           LEFT JOIN account_grants g ON g.account_id = a.id AND g.user_id = ?
           WHERE b.id = ?"#,
        viewer,
        id
    )
    .fetch_optional(executor)
    .await
    .map_err(store_error)?
    .map(|row| listed_bot_from_row(&row))
    .transpose()
}

/// The bots among `ids` that exist, each as [`listed`] reads it for
/// `viewer`, in ID order: one query, the IDs bound as one JSON array.
pub(super) async fn listed_in(
    pool: &sqlx::SqlitePool,
    ids: &[BotId],
    viewer: UserId,
) -> Result<Vec<ListedBot>, StoreError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids = serde_json::Value::from(
        ids.iter()
            .map(|id| id_text(*id).to_string())
            .collect::<Vec<_>>(),
    )
    .to_string();
    let viewer = id_text(viewer);
    sqlx::query_as!(
        ListedBotRow,
        r#"SELECT b.rowid AS "rowid!: i64", b.id AS "id!", b.account_id AS "account_id!",
                  b.server_address AS "server_address?", b.mode_id AS "mode_id!",
                  b.desired_state AS "desired_state!", b.agent_id AS "agent_id?",
                  b.last_state AS "last_state?", b.last_state_reason AS "last_state_reason?",
                  b.last_state_at AS "last_state_at?",
                  a.rowid AS "account_rowid!: i64", a.kind AS "kind!", a.mc_uuid AS "mc_uuid?",
                  a.mc_name AS "mc_name!", a.status AS "status!",
                  a.status_reason AS "status_reason?", a.created_at AS "created_at!",
                  a.owner_user_id AS "owner_user_id!", u.role AS "owner_role!",
                  u.username AS "owner_name!", u.rowid AS "owner_rowid!: i64",
                  u.disabled AS "owner_disabled!: i64",
                  m.rowid AS "mode_rowid!: i64", m.definition_json AS "definition_json!",
                  g.level AS "grant_level?", g.rowid AS "grant_rowid?: i64"
           FROM bots b
           JOIN mc_accounts a ON a.id = b.account_id
           JOIN users u ON u.id = a.owner_user_id
           JOIN modes m ON m.id = b.mode_id
           LEFT JOIN account_grants g ON g.account_id = a.id AND g.user_id = ?
           WHERE b.id IN (SELECT value FROM json_each(?))
           ORDER BY b.id"#,
        viewer,
        ids
    )
    .fetch_all(pool)
    .await
    .map_err(store_error)?
    .iter()
    .map(listed_bot_from_row)
    .collect()
}

/// One page of the bots `viewer` may see, newest first: those of the
/// accounts `accounts::list_visible` lists for them. One more than its limit
/// when another page follows.
pub(super) async fn visible<'c, E>(
    executor: E,
    viewer: Actor,
    page: Page<BotId>,
) -> Result<Vec<ListedBot>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let role = viewer.role.as_str();
    let viewer = id_text(viewer.id);
    let before = page.before.map(|id| id_text(id).to_string());
    let fetch = page.limit.fetch();
    sqlx::query_as!(
        ListedBotRow,
        r#"SELECT b.rowid AS "rowid!: i64", b.id AS "id!", b.account_id AS "account_id!",
                  b.server_address AS "server_address?", b.mode_id AS "mode_id!",
                  b.desired_state AS "desired_state!", b.agent_id AS "agent_id?",
                  b.last_state AS "last_state?", b.last_state_reason AS "last_state_reason?",
                  b.last_state_at AS "last_state_at?",
                  a.rowid AS "account_rowid!: i64", a.kind AS "kind!", a.mc_uuid AS "mc_uuid?",
                  a.mc_name AS "mc_name!", a.status AS "status!",
                  a.status_reason AS "status_reason?", a.created_at AS "created_at!",
                  a.owner_user_id AS "owner_user_id!", u.role AS "owner_role!",
                  u.username AS "owner_name!", u.rowid AS "owner_rowid!: i64",
                  u.disabled AS "owner_disabled!: i64",
                  m.rowid AS "mode_rowid!: i64", m.definition_json AS "definition_json!",
                  g.level AS "grant_level?", g.rowid AS "grant_rowid?: i64"
           FROM bots b
           JOIN mc_accounts a ON a.id = b.account_id
           JOIN users u ON u.id = a.owner_user_id
           JOIN modes m ON m.id = b.mode_id
           LEFT JOIN account_grants g ON g.account_id = a.id AND g.user_id = ?
           WHERE (? IS NULL OR b.id < ?)
             AND (? = 'owner'
                  OR a.owner_user_id = ?
                  OR g.level IS NOT NULL
                  OR (? = 'admin' AND u.role = 'member'))
           ORDER BY b.id DESC
           LIMIT ?"#,
        viewer,
        before,
        before,
        role,
        viewer,
        role,
        fetch
    )
    .fetch_all(executor)
    .await
    .map_err(store_error)?
    .iter()
    .map(listed_bot_from_row)
    .collect()
}

/// Every bot, every open release record and the enabled agents, in one read
/// transaction, so the three agree with each other (WAL gives the
/// transaction one snapshot).
pub(super) async fn fleet(pool: &sqlx::SqlitePool) -> Result<Fleet, StoreError> {
    let mut tx = pool.begin().await.map_err(store_error)?;
    let server_texts = server_settings::texts_by_server(&mut tx).await?;
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
    let (bots, mut unreadable) = fleet_records(&bot_rows, &server_texts)?;
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
    let agent_rows = sqlx::query!(
        r#"SELECT agents.rowid AS "rowid!: i64", agents.id AS "id!",
                  (SELECT max(agent_certs.not_after) FROM agent_certs
                   WHERE agent_certs.agent_id = agents.id AND agent_certs.revoked_at IS NULL)
                      AS "not_after?: String"
           FROM agents WHERE agents.enabled = 1"#
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(store_error)?;
    let mut enabled_agents = BTreeSet::new();
    let mut agent_expiry = BTreeMap::new();
    for row in agent_rows {
        let corrupt = |column| StoreError::Corrupt {
            table: "agents",
            column,
            rowid: row.rowid,
        };
        let agent: AgentId = parse_id(&row.id).ok_or_else(|| corrupt("id"))?;
        enabled_agents.insert(agent);
        if let Some(not_after) = row.not_after.as_deref() {
            agent_expiry.insert(
                agent,
                parse_time(not_after).ok_or_else(|| corrupt("not_after"))?,
            );
        }
    }
    // Read only: nothing to commit, and dropping it ends it.
    drop(tx);
    Ok(Fleet {
        bots,
        releases,
        unreadable,
        enabled_agents,
        agent_expiry,
        server_texts,
    })
}

/// The fleet's bots from `rows`, and those that don't read back: a row of
/// their own, or their server's texts (R132).
fn fleet_records(
    rows: &[RecordRow],
    server_texts: &ServerTexts,
) -> Result<(Vec<BotRecord>, Vec<UnreadableBot>), StoreError> {
    let mut bots = Vec::new();
    let mut unreadable = Vec::new();
    for row in rows {
        match record_from_row(row) {
            Ok(record) => match unreadable_texts(server_texts, &record) {
                // Left alone, as a bot whose mode doesn't read (S37, R132),
                // and logged by its server's row, not on its own.
                Some(rowid) => unreadable.push(UnreadableBot {
                    by_server: true,
                    ..unreadable_bot(
                        row,
                        server_settings::SERVER_SETTINGS,
                        "conflict_texts",
                        rowid,
                    )?
                }),
                None => bots.push(record),
            },
            Err(StoreError::Corrupt {
                table,
                column,
                rowid,
            }) => unreadable.push(unreadable_bot(row, table, column, rowid)?),
            Err(other) => return Err(other),
        }
    }
    Ok((bots, unreadable))
}

/// The rowid of `record`'s server's settings when their texts don't read
/// back.
fn unreadable_texts(texts: &ServerTexts, record: &BotRecord) -> Option<i64> {
    let server = record.bot.server.as_ref()?;
    texts
        .unreadable
        .get(&ServerKey::from(server))
        .map(|row| row.rowid)
}

/// Who may see which bot (S30): every bot with its account's owner, every
/// grant and every user, in one read transaction, so the three agree. Any
/// row that doesn't read back fails the read.
pub(super) async fn access(pool: &sqlx::SqlitePool) -> Result<Access, StoreError> {
    let mut tx = pool.begin().await.map_err(store_error)?;
    let bots = sqlx::query!(
        r#"SELECT b.rowid AS "rowid!: i64", b.id AS "id!", b.account_id AS "account_id!",
                  a.rowid AS "account_rowid!: i64", a.owner_user_id AS "owner_user_id!"
           FROM bots b
           JOIN mc_accounts a ON a.id = b.account_id"#
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(store_error)?
    .into_iter()
    .map(|row| {
        Ok(BotAccess {
            bot: parse_id(&row.id).ok_or(StoreError::Corrupt {
                table: BOTS,
                column: "id",
                rowid: row.rowid,
            })?,
            account: parse_id(&row.account_id).ok_or(StoreError::Corrupt {
                table: BOTS,
                column: "account_id",
                rowid: row.rowid,
            })?,
            owner: parse_id(&row.owner_user_id).ok_or(StoreError::Corrupt {
                table: "mc_accounts",
                column: "owner_user_id",
                rowid: row.account_rowid,
            })?,
        })
    })
    .collect::<Result<Vec<_>, StoreError>>()?;
    let grants = sqlx::query!(
        r#"SELECT rowid AS "rowid!: i64", account_id AS "account_id!", user_id AS "user_id!",
                  level AS "level!"
           FROM account_grants"#
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(store_error)?
    .into_iter()
    .map(|row| {
        let corrupt = |column| StoreError::Corrupt {
            table: "account_grants",
            column,
            rowid: row.rowid,
        };
        Ok(GrantAccess {
            account: parse_id(&row.account_id).ok_or_else(|| corrupt("account_id"))?,
            user: parse_id(&row.user_id).ok_or_else(|| corrupt("user_id"))?,
            level: row
                .level
                .parse::<GrantLevel>()
                .map_err(|_| corrupt("level"))?,
        })
    })
    .collect::<Result<Vec<_>, StoreError>>()?;
    let users = sqlx::query!(
        r#"SELECT rowid AS "rowid!: i64", id AS "id!", role AS "role!",
                  disabled AS "disabled!: i64"
           FROM users"#
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(store_error)?
    .into_iter()
    .map(|row| {
        let corrupt = |column| StoreError::Corrupt {
            table: "users",
            column,
            rowid: row.rowid,
        };
        Ok(UserAccess {
            user: parse_id(&row.id).ok_or_else(|| corrupt("id"))?,
            role: row.role.parse::<Role>().map_err(|_| corrupt("role"))?,
            enabled: match row.disabled {
                0 => true,
                1 => false,
                _ => return Err(corrupt("disabled")),
            },
        })
    })
    .collect::<Result<Vec<_>, StoreError>>()?;
    // Read only: nothing to commit, and dropping it ends it.
    drop(tx);
    Ok(Access {
        bots,
        grants,
        users,
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

/// Sets `bot`'s server address to `server`'s written form, when it's
/// another.
pub(super) async fn set_server(
    conn: &mut SqliteConnection,
    bot: BotId,
    server: &ServerAddress,
) -> Result<bool, StoreError> {
    let bot = id_text(bot);
    let server = server.to_string();
    let done = sqlx::query!(
        "UPDATE bots SET server_address = ?
         WHERE id = ? AND server_address IS NOT ?",
        server,
        bot,
        server
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Switches `bot` to `mode`, when it's another.
pub(super) async fn set_mode(
    conn: &mut SqliteConnection,
    bot: BotId,
    mode: ModeId,
) -> Result<bool, StoreError> {
    let bot = id_text(bot);
    let mode = id_text(mode);
    let done = sqlx::query!(
        "UPDATE bots SET mode_id = ? WHERE id = ? AND mode_id <> ?",
        mode,
        bot,
        mode
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// The accounts whose bots run `mode`; see [`BotWrites::accounts_with_mode`].
///
/// [`BotWrites::accounts_with_mode`]: crate::ports::bots::BotWrites::accounts_with_mode
pub(super) async fn accounts_with_mode(
    conn: &mut SqliteConnection,
    mode: ModeId,
) -> Result<Vec<AccountId>, StoreError> {
    let mode = id_text(mode);
    // The index bots_mode_id finds them (0014).
    sqlx::query!(
        r#"SELECT rowid AS "rowid!: i64", account_id AS "account_id!"
           FROM bots WHERE mode_id = ? ORDER BY account_id"#,
        mode
    )
    .fetch_all(conn)
    .await
    .map_err(store_error)?
    .into_iter()
    .map(|row| {
        parse_id(&row.account_id).ok_or(StoreError::Corrupt {
            table: BOTS,
            column: "account_id",
            rowid: row.rowid,
        })
    })
    .collect()
}

/// Sets whether `bot` should run, when it says otherwise.
pub(super) async fn set_desired(
    conn: &mut SqliteConnection,
    bot: BotId,
    desired: DesiredRunState,
) -> Result<bool, StoreError> {
    let bot = id_text(bot);
    let desired = desired.as_str();
    let done = sqlx::query!(
        "UPDATE bots SET desired_state = ? WHERE id = ? AND desired_state <> ?",
        desired,
        bot,
        desired
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Clears `bot`'s stored last state while it has no agent and the stored
/// one is still `last`.
pub(super) async fn clear_sticky(
    conn: &mut SqliteConnection,
    bot: BotId,
    last: &LastReport,
) -> Result<bool, StoreError> {
    let bot = id_text(bot);
    let state = last.state.as_str();
    let reason = last.state.reason();
    let at = time_text(last.at).ok_or(StoreError::Unstorable {
        table: BOTS,
        column: "last_state_at",
    })?;
    let done = sqlx::query!(
        "UPDATE bots SET last_state = NULL, last_state_reason = NULL, last_state_at = NULL
         WHERE id = ? AND agent_id IS NULL
           AND last_state = ? AND last_state_reason IS ? AND last_state_at = ?",
        bot,
        state,
        reason,
        at
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Reads a bot as one viewer sees it back, through the same checks as its
/// record and as the account's view.
fn listed_bot_from_row(row: &ListedBotRow) -> Result<ListedBot, StoreError> {
    let record = record_from_row(&RecordRow {
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
        account_rowid: row.account_rowid,
        kind: row.kind.clone(),
        mc_uuid: row.mc_uuid.clone(),
        mc_name: row.mc_name.clone(),
        status: row.status.clone(),
        status_reason: row.status_reason.clone(),
        owner_rowid: row.owner_rowid,
        owner_disabled: row.owner_disabled,
        mode_rowid: row.mode_rowid,
        definition_json: row.definition_json.clone(),
    })?;
    let account = accounts::listed_from_row(&accounts::ListedRow {
        rowid: row.account_rowid,
        id: row.account_id.clone(),
        owner_user_id: row.owner_user_id.clone(),
        owner_role: row.owner_role.clone(),
        owner_name: row.owner_name.clone(),
        owner_rowid: row.owner_rowid,
        kind: row.kind.clone(),
        mc_uuid: row.mc_uuid.clone(),
        mc_name: row.mc_name.clone(),
        status: row.status.clone(),
        status_reason: row.status_reason.clone(),
        created_at: row.created_at.clone(),
        grant_level: row.grant_level.clone(),
        grant_rowid: row.grant_rowid,
    })?;
    Ok(ListedBot { record, account })
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
        by_server: false,
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
        by_server: false,
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
