//! The `audit_log` table's SQL: insert and list, nothing else.

use std::net::IpAddr;

use fleet_core::audit::{AuditAction, RecordedMetadata, RecordedTarget};
use serde_json::Value;
use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::convert::{id_text, parse_id, parse_time, time_text};
use super::error::store_error;
use crate::ports::audit::{AuditEntryId, AuditPage, AuditRecord, NewAuditEntry};
use crate::ports::store::StoreError;

/// The table, as errors name it.
const TABLE: &str = "audit_log";

/// A row as stored, before its values are checked.
struct AuditRow {
    id: i64,
    at: String,
    actor_user_id: Option<String>,
    actor_ip: Option<String>,
    action: String,
    target_type: Option<String>,
    target_id: Option<String>,
    outcome: String,
    metadata_json: Option<String>,
}

/// Inserts `entry` and returns its ID. Empty metadata is stored as `NULL`;
/// its size was checked when it was built, so it never fails here.
pub(super) async fn record(
    conn: &mut SqliteConnection,
    entry: &NewAuditEntry,
) -> Result<AuditEntryId, StoreError> {
    let at = time_text(entry.at).ok_or(StoreError::Unstorable {
        table: TABLE,
        column: "at",
    })?;
    let actor = entry.actor.map(id_text);
    let ip = entry.ip.map(|ip| ip.to_canonical().to_string());
    let action = entry.action.as_str();
    let target_type = entry.target.map(|target| target.kind());
    let target_id = entry.target.map(|target| id_text(target.id()));
    let outcome = entry.outcome.as_str();
    let metadata = (!entry.metadata.is_empty()).then(|| entry.metadata.to_json());
    let done = sqlx::query!(
        "INSERT INTO audit_log
             (at, actor_user_id, actor_ip, action, target_type, target_id, outcome, metadata_json)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        at,
        actor,
        ip,
        action,
        target_type,
        target_id,
        outcome,
        metadata
    )
    .execute(conn)
    .await
    .map_err(store_error)?;
    Ok(AuditEntryId::new(done.last_insert_rowid()))
}

/// One page, newest first by ID.
pub(super) async fn list(
    pool: &SqlitePool,
    page: AuditPage,
) -> Result<Vec<AuditRecord>, StoreError> {
    let before = page.before.map_or(i64::MAX, AuditEntryId::get);
    let limit = i64::from(page.limit.get());
    sqlx::query_as!(
        AuditRow,
        r#"SELECT id AS "id!: i64", at, actor_user_id, actor_ip, action, target_type, target_id,
                  outcome, metadata_json
           FROM audit_log WHERE id < ? ORDER BY id DESC LIMIT ?"#,
        before,
        limit
    )
    .fetch_all(pool)
    .await
    .map_err(store_error)?
    .iter()
    .map(record_from_row)
    .collect()
}

/// Checks every value of a stored row. Reading is tolerant of a target type
/// or a metadata value only a newer version knows; anything that breaks the
/// format is [`StoreError::Corrupt`], naming the column and the entry's ID.
fn record_from_row(row: &AuditRow) -> Result<AuditRecord, StoreError> {
    let corrupt = |column| StoreError::Corrupt {
        table: TABLE,
        column,
        rowid: row.id,
    };
    let actor = match &row.actor_user_id {
        Some(text) => Some(parse_id(text).ok_or_else(|| corrupt("actor_user_id"))?),
        None => None,
    };
    let ip = match &row.actor_ip {
        Some(text) => Some(text.parse::<IpAddr>().map_err(|_| corrupt("actor_ip"))?),
        None => None,
    };
    let target = match (&row.target_type, &row.target_id) {
        (None, None) => None,
        (Some(kind), Some(id)) => {
            let id: Uuid = parse_id(id).ok_or_else(|| corrupt("target_id"))?;
            Some(RecordedTarget::from_parts(kind, id).map_err(|_| corrupt("target_type"))?)
        }
        _ => return Err(corrupt("target_type")),
    };
    let metadata = match &row.metadata_json {
        Some(json) => metadata_from_json(json).ok_or_else(|| corrupt("metadata_json"))?,
        None => RecordedMetadata::new(),
    };
    Ok(AuditRecord {
        id: AuditEntryId::new(row.id),
        at: parse_time(&row.at).ok_or_else(|| corrupt("at"))?,
        actor,
        ip,
        action: AuditAction::try_from(row.action.as_str()).map_err(|_| corrupt("action"))?,
        target,
        outcome: row.outcome.parse().map_err(|_| corrupt("outcome"))?,
        metadata,
    })
}

/// Reads stored metadata: any JSON object, with a value this version doesn't
/// know kept as its JSON text. `None` when the text isn't a JSON object.
fn metadata_from_json(json: &str) -> Option<RecordedMetadata> {
    let Value::Object(map) = serde_json::from_str(json).ok()? else {
        return None;
    };
    let mut metadata = RecordedMetadata::new();
    for (key, value) in &map {
        match value {
            Value::String(text) => metadata.push_text(key, text),
            Value::Bool(flag) => metadata.push_bool(key, *flag),
            Value::Number(number) => match number.as_i64() {
                Some(n) => metadata.push_int(key, n),
                None => metadata.push_unrecognized(key, &number.to_string()),
            },
            other => metadata.push_unrecognized(key, &other.to_string()),
        }
    }
    Some(metadata)
}
