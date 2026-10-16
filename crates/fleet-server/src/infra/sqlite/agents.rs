//! The `agents` and `agent_certs` tables' SQL.
//!
//! A certificate's fingerprint is bound as its 32 bytes. A name taken by an
//! enabled agent is the `agents_enabled_name` index's violation, mapped to
//! [`InsertAgentError::NameTaken`]; [`name_taken`] asks the same question
//! with the same `lower(name)` the index uses.

use chrono::{DateTime, Utc};
use fleet_core::id::AgentId;
use fleet_core::pki::CertFingerprint;
use fleet_core::value::{AgentCapacity, AgentName, AgentVersion};
use sqlx::SqliteConnection;

use super::convert::{id_text, parse_id, parse_time, time_text};
use super::error::{store_error, unique_violation};
use crate::ports::agents::{
    Agent, AgentCert, AgentHello, InsertAgentError, ListedAgent, NewAgent, NewAgentCert,
};
use crate::ports::page::Page;
use crate::ports::store::StoreError;

/// The tables, as errors name them.
const AGENTS: &str = "agents";
const AGENT_CERTS: &str = "agent_certs";

/// How SQLite names the index that keeps enabled agents' names apart.
const TAKEN_NAME: &str = "index 'agents_enabled_name'";

/// An `agents` row, before its values are checked.
struct AgentRow {
    rowid: i64,
    id: String,
    name: String,
    enabled: i64,
    capacity: Option<i64>,
    version: Option<String>,
    last_seen_at: Option<String>,
    created_at: String,
}

/// An `agents` row with its latest unrevoked `not_after`, before its
/// values are checked.
struct ListedRow {
    rowid: i64,
    id: String,
    name: String,
    enabled: i64,
    capacity: Option<i64>,
    version: Option<String>,
    last_seen_at: Option<String>,
    created_at: String,
    not_after: Option<String>,
}

/// An `agent_certs` row, before its values are checked.
struct CertRow {
    rowid: i64,
    fingerprint_sha256: Vec<u8>,
    agent_id: String,
    not_after: String,
    revoked_at: Option<String>,
    created_at: String,
}

/// The stored form of `at` in `table.column`, or [`StoreError::Unstorable`].
fn stored_time(
    at: DateTime<Utc>,
    table: &'static str,
    column: &'static str,
) -> Result<String, StoreError> {
    time_text(at).ok_or(StoreError::Unstorable { table, column })
}

/// The agent `id`.
pub(super) async fn find<'c, E>(executor: E, id: AgentId) -> Result<Option<Agent>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let id = id_text(id);
    sqlx::query_as!(
        AgentRow,
        r#"SELECT rowid AS "rowid!: i64", id AS "id!", name AS "name!", enabled AS "enabled!: i64",
                  capacity AS "capacity?: i64", version AS "version?",
                  last_seen_at AS "last_seen_at?", created_at AS "created_at!"
           FROM agents WHERE id = ?"#,
        id
    )
    .fetch_optional(executor)
    .await
    .map_err(store_error)?
    .map(agent_from_row)
    .transpose()
}

/// Whether an enabled agent has `name`, in any case.
pub(super) async fn name_taken<'c, E>(executor: E, name: &AgentName) -> Result<bool, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let name = name.as_str();
    sqlx::query_scalar!(
        r#"SELECT EXISTS (
               SELECT 1 FROM agents WHERE lower(name) = lower(?) AND enabled = 1
           ) AS "taken!: bool""#,
        name
    )
    .fetch_one(executor)
    .await
    .map_err(store_error)
}

/// The certificates of agent `id`, oldest first.
pub(super) async fn certs<'c, E>(executor: E, id: AgentId) -> Result<Vec<AgentCert>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let id = id_text(id);
    sqlx::query_as!(
        CertRow,
        r#"SELECT rowid AS "rowid!: i64", fingerprint_sha256 AS "fingerprint_sha256!: Vec<u8>",
                  agent_id AS "agent_id!", not_after AS "not_after!",
                  revoked_at AS "revoked_at?", created_at AS "created_at!"
           FROM agent_certs WHERE agent_id = ?
           ORDER BY created_at, rowid"#,
        id
    )
    .fetch_all(executor)
    .await
    .map_err(store_error)?
    .into_iter()
    .map(cert_from_row)
    .collect()
}

/// Inserts `agent`, enabled.
pub(super) async fn insert(
    conn: &mut SqliteConnection,
    agent: &NewAgent,
) -> Result<(), InsertAgentError> {
    let id = id_text(agent.id);
    let name = agent.name.as_str();
    let created_at = stored_time(agent.created_at, AGENTS, "created_at")?;
    sqlx::query!(
        "INSERT INTO agents (id, name, enabled, created_at) VALUES (?, ?, 1, ?)",
        id,
        name,
        created_at
    )
    .execute(conn)
    .await
    .map_err(|error| match unique_violation(&error).as_deref() {
        Some(TAKEN_NAME) => InsertAgentError::NameTaken,
        _ => InsertAgentError::Store(store_error(error)),
    })?;
    Ok(())
}

/// Stores `cert`, unrevoked.
pub(super) async fn insert_cert(
    conn: &mut SqliteConnection,
    cert: &NewAgentCert,
) -> Result<(), StoreError> {
    let fingerprint = cert.fingerprint.as_bytes().as_slice();
    let agent = id_text(cert.agent);
    let not_after = stored_time(cert.not_after, AGENT_CERTS, "not_after")?;
    let created_at = stored_time(cert.created_at, AGENT_CERTS, "created_at")?;
    sqlx::query!(
        "INSERT INTO agent_certs (fingerprint_sha256, agent_id, not_after, created_at)
         VALUES (?, ?, ?, ?)",
        fingerprint,
        agent,
        not_after,
        created_at
    )
    .execute(conn)
    .await
    .map(drop)
    .map_err(store_error)
}

/// The certificate whose fingerprint is `fingerprint`.
pub(super) async fn cert<'c, E>(
    executor: E,
    fingerprint: CertFingerprint,
) -> Result<Option<AgentCert>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let fingerprint = fingerprint.as_bytes().as_slice();
    sqlx::query_as!(
        CertRow,
        r#"SELECT rowid AS "rowid!: i64", fingerprint_sha256 AS "fingerprint_sha256!: Vec<u8>",
                  agent_id AS "agent_id!", not_after AS "not_after!",
                  revoked_at AS "revoked_at?", created_at AS "created_at!"
           FROM agent_certs WHERE fingerprint_sha256 = ?"#,
        fingerprint
    )
    .fetch_optional(executor)
    .await
    .map_err(store_error)?
    .map(cert_from_row)
    .transpose()
}

/// Every certificate that admits its agent at `now`.
pub(super) async fn admissible<'c, E>(
    executor: E,
    now: DateTime<Utc>,
) -> Result<Vec<CertFingerprint>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let now = stored_time(now, AGENT_CERTS, "not_after")?;
    let rows = sqlx::query!(
        r#"SELECT c.rowid AS "rowid!: i64", c.fingerprint_sha256 AS "fingerprint_sha256!: Vec<u8>"
           FROM agent_certs c JOIN agents a ON a.id = c.agent_id
           WHERE c.revoked_at IS NULL AND c.not_after > ? AND a.enabled = 1"#,
        now
    )
    .fetch_all(executor)
    .await
    .map_err(store_error)?;
    rows.into_iter()
        .map(|row| {
            <[u8; 32]>::try_from(row.fingerprint_sha256)
                .map(CertFingerprint::from_digest)
                .map_err(|_| StoreError::Corrupt {
                    table: AGENT_CERTS,
                    column: "fingerprint_sha256",
                    rowid: row.rowid,
                })
        })
        .collect()
}

/// The agents of `page`, newest first, each with its latest unrevoked
/// `not_after`.
pub(super) async fn list(
    pool: &sqlx::SqlitePool,
    page: Page<AgentId>,
) -> Result<Vec<ListedAgent>, StoreError> {
    let before = page.before.map(|id| id_text(id).to_string());
    let fetch = page.limit.fetch();
    sqlx::query_as!(
        ListedRow,
        r#"SELECT agents.rowid AS "rowid!: i64", agents.id AS "id!", agents.name AS "name!",
                  agents.enabled AS "enabled!: i64", agents.capacity AS "capacity?: i64",
                  agents.version AS "version?", agents.last_seen_at AS "last_seen_at?",
                  agents.created_at AS "created_at!",
                  (SELECT max(agent_certs.not_after) FROM agent_certs
                   WHERE agent_certs.agent_id = agents.id AND agent_certs.revoked_at IS NULL)
                      AS "not_after?: String"
           FROM agents
           WHERE ? IS NULL OR agents.id < ?
           ORDER BY agents.id DESC
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

/// The agent `id`, as the list shows it.
pub(super) async fn listed(
    pool: &sqlx::SqlitePool,
    id: AgentId,
) -> Result<Option<ListedAgent>, StoreError> {
    let id = id_text(id);
    sqlx::query_as!(
        ListedRow,
        r#"SELECT agents.rowid AS "rowid!: i64", agents.id AS "id!", agents.name AS "name!",
                  agents.enabled AS "enabled!: i64", agents.capacity AS "capacity?: i64",
                  agents.version AS "version?", agents.last_seen_at AS "last_seen_at?",
                  agents.created_at AS "created_at!",
                  (SELECT max(agent_certs.not_after) FROM agent_certs
                   WHERE agent_certs.agent_id = agents.id AND agent_certs.revoked_at IS NULL)
                      AS "not_after?: String"
           FROM agents WHERE agents.id = ?"#,
        id
    )
    .fetch_optional(pool)
    .await
    .map_err(store_error)?
    .map(listed_from_row)
    .transpose()
}

/// Disables agent `id` while it's enabled; whether it changed.
pub(super) async fn disable(conn: &mut SqliteConnection, id: AgentId) -> Result<bool, StoreError> {
    let id = id_text(id);
    sqlx::query!(
        "UPDATE agents SET enabled = 0 WHERE id = ? AND enabled = 1",
        id
    )
    .execute(conn)
    .await
    .map(|done| done.rows_affected() == 1)
    .map_err(store_error)
}

/// Revokes every unrevoked certificate of agent `id` at `at`; how many.
pub(super) async fn revoke_certs(
    conn: &mut SqliteConnection,
    id: AgentId,
    at: DateTime<Utc>,
) -> Result<u64, StoreError> {
    let id = id_text(id);
    let at = stored_time(at, AGENT_CERTS, "revoked_at")?;
    sqlx::query!(
        "UPDATE agent_certs SET revoked_at = ? WHERE agent_id = ? AND revoked_at IS NULL",
        at,
        id
    )
    .execute(conn)
    .await
    .map(|done| done.rows_affected())
    .map_err(store_error)
}

/// Records `hello` on its agent while it's enabled; `last_seen_at` only
/// moves forward. Whether a row changed.
pub(super) async fn record_hello(
    conn: &mut SqliteConnection,
    hello: &AgentHello,
) -> Result<bool, StoreError> {
    let id = id_text(hello.agent);
    let capacity = i64::from(u32::from(hello.capacity));
    let version = hello.version.as_str();
    let at = stored_time(hello.at, AGENTS, "last_seen_at")?;
    let done = sqlx::query!(
        "UPDATE agents SET capacity = ?, version = ?,
                last_seen_at = CASE WHEN last_seen_at IS NULL OR last_seen_at < ?
                                    THEN ? ELSE last_seen_at END
         WHERE id = ? AND enabled = 1",
        capacity,
        version,
        at,
        at,
        id
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// Moves agent `id`'s `last_seen_at` forward to `at`.
pub(super) async fn record_seen(
    conn: &mut SqliteConnection,
    id: AgentId,
    at: DateTime<Utc>,
) -> Result<(), StoreError> {
    let id = id_text(id);
    let at = stored_time(at, AGENTS, "last_seen_at")?;
    sqlx::query!(
        "UPDATE agents SET last_seen_at = ?
         WHERE id = ? AND (last_seen_at IS NULL OR last_seen_at < ?)",
        at,
        id,
        at
    )
    .execute(conn)
    .await
    .map(drop)
    .map_err(store_error)
}

/// Reads a listed row back, or [`StoreError::Corrupt`] naming the first
/// column that doesn't.
fn listed_from_row(row: ListedRow) -> Result<ListedAgent, StoreError> {
    let rowid = row.rowid;
    let not_after = row
        .not_after
        .as_deref()
        .map(|at| {
            parse_time(at).ok_or(StoreError::Corrupt {
                table: AGENT_CERTS,
                column: "not_after",
                rowid,
            })
        })
        .transpose()?;
    let agent = agent_from_row(AgentRow {
        rowid,
        id: row.id,
        name: row.name,
        enabled: row.enabled,
        capacity: row.capacity,
        version: row.version,
        last_seen_at: row.last_seen_at,
        created_at: row.created_at,
    })?;
    Ok(ListedAgent { agent, not_after })
}

/// Reads an `agents` row back, or [`StoreError::Corrupt`] naming the first
/// column that doesn't.
fn agent_from_row(row: AgentRow) -> Result<Agent, StoreError> {
    let rowid = row.rowid;
    let corrupt = |column| StoreError::Corrupt {
        table: AGENTS,
        column,
        rowid,
    };
    let enabled = match row.enabled {
        0 => false,
        1 => true,
        _ => return Err(corrupt("enabled")),
    };
    let capacity = row
        .capacity
        .map(|capacity| {
            u32::try_from(capacity)
                .ok()
                .and_then(|capacity| AgentCapacity::try_from(capacity).ok())
                .ok_or_else(|| corrupt("capacity"))
        })
        .transpose()?;
    let version = row
        .version
        .as_deref()
        .map(|version| AgentVersion::try_from(version).map_err(|_| corrupt("version")))
        .transpose()?;
    let last_seen_at = row
        .last_seen_at
        .as_deref()
        .map(|at| parse_time(at).ok_or_else(|| corrupt("last_seen_at")))
        .transpose()?;
    Ok(Agent {
        id: parse_id(&row.id).ok_or_else(|| corrupt("id"))?,
        name: AgentName::try_from(row.name).map_err(|_| corrupt("name"))?,
        enabled,
        capacity,
        version,
        last_seen_at,
        created_at: parse_time(&row.created_at).ok_or_else(|| corrupt("created_at"))?,
    })
}

/// Reads an `agent_certs` row back, or [`StoreError::Corrupt`] naming the
/// first column that doesn't.
fn cert_from_row(row: CertRow) -> Result<AgentCert, StoreError> {
    let rowid = row.rowid;
    let corrupt = |column| StoreError::Corrupt {
        table: AGENT_CERTS,
        column,
        rowid,
    };
    let digest =
        <[u8; 32]>::try_from(row.fingerprint_sha256).map_err(|_| corrupt("fingerprint_sha256"))?;
    let revoked_at = row
        .revoked_at
        .as_deref()
        .map(|at| parse_time(at).ok_or_else(|| corrupt("revoked_at")))
        .transpose()?;
    Ok(AgentCert {
        fingerprint: CertFingerprint::from_digest(digest),
        agent: parse_id(&row.agent_id).ok_or_else(|| corrupt("agent_id"))?,
        not_after: parse_time(&row.not_after).ok_or_else(|| corrupt("not_after"))?,
        revoked_at,
        created_at: parse_time(&row.created_at).ok_or_else(|| corrupt("created_at"))?,
    })
}
