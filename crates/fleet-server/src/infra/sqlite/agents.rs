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
use crate::ports::agents::{Agent, AgentCert, InsertAgentError, NewAgent, NewAgentCert};
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
