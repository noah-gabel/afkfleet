//! The `audit_log` table's SQL: insert, list, and the counts behind the
//! user's own routes and the link flows' daily cap.

use std::net::IpAddr;

use fleet_core::audit::{AuditAction, AuditTarget, RecordedMetadata, RecordedTarget};
use fleet_core::id::UserId;
use serde_json::Value;
use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::convert::{id_text, parse_id, parse_time, time_text};
use super::error::store_error;
use crate::ports::audit::{
    AuditEntryId, AuditPage, AuditRecord, FailedCodeAttempts, NewAuditEntry,
};
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

/// The wrong 2FA codes typed for `user` since their previous successful
/// login: `auth.login` Failure entries with `step` = `code` targeting the
/// user, after the second-newest `auth.login` Success (or all of them),
/// except those of the login step that ended in the newest Success (the
/// same `login_step_id`): the user's own typos at that login. An entry
/// without a `login_step_id` (older than Phase 8, group E) always counts.
/// The literals are fixed names, never input (`AuditTarget::User`'s kind,
/// the action, the outcome, the step and the metadata keys).
pub(super) async fn failed_code_attempts(
    pool: &SqlitePool,
    user: UserId,
) -> Result<FailedCodeAttempts, StoreError> {
    let user = id_text(user);
    let row = sqlx::query!(
        r#"WITH successes AS (
               SELECT id, json_extract(metadata_json, '$.login_step_id') AS step
               FROM audit_log
               WHERE target_type = 'user' AND target_id = ?1 AND action = 'auth.login'
                     AND outcome = 'success'
               ORDER BY id DESC LIMIT 2
           ),
           failures AS (
               SELECT id, at FROM audit_log
               WHERE target_type = 'user' AND target_id = ?1 AND action = 'auth.login'
                     AND outcome = 'failure'
                     AND json_extract(metadata_json, '$.step') = 'code'
                     AND id > coalesce((SELECT id FROM successes ORDER BY id DESC LIMIT 1 OFFSET 1), 0)
                     AND coalesce(
                             json_extract(metadata_json, '$.login_step_id')
                                 = (SELECT step FROM successes ORDER BY id DESC LIMIT 1),
                             0
                         ) = 0
           )
           SELECT (SELECT count(*) FROM failures) AS "count!: i64",
                  (SELECT id FROM failures ORDER BY id DESC LIMIT 1) AS "last_id?: i64",
                  (SELECT at FROM failures ORDER BY id DESC LIMIT 1) AS "last_at?: String""#,
        user
    )
    .fetch_one(pool)
    .await
    .map_err(store_error)?;
    let corrupt = |column| StoreError::Corrupt {
        table: TABLE,
        column,
        rowid: row.last_id.unwrap_or_default(),
    };
    Ok(FailedCodeAttempts {
        count: u64::try_from(row.count).map_err(|_| corrupt("id"))?,
        last_at: row
            .last_at
            .map(|text| parse_time(&text).ok_or_else(|| corrupt("at")))
            .transpose()?,
    })
}

/// When the Success entries of `action` whose target is `user` were
/// recorded, for those after `since`, in commit order. The `(target_type,
/// target_id, action, id)` index finds them; the daily cap keeps them few.
/// How many Failure entries of `action` name `target`.
pub(super) async fn failures_on<'c, E>(
    executor: E,
    target: AuditTarget,
    action: &AuditAction,
) -> Result<u32, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let kind = target.kind();
    let id = id_text(target.id());
    let action = action.as_str();
    let count = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!: i64" FROM audit_log
           WHERE target_type = ? AND target_id = ? AND action = ? AND outcome = 'failure'"#,
        kind,
        id,
        action
    )
    .fetch_one(executor)
    .await
    .map_err(store_error)?;
    // A count is never negative, and never near 2^32 entries.
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

pub(super) async fn user_successes_since<'c, E>(
    executor: E,
    user: UserId,
    action: &AuditAction,
    since: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<chrono::DateTime<chrono::Utc>>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let user = id_text(user);
    let action = action.as_str();
    let since = time_text(since).ok_or(StoreError::Unstorable {
        table: TABLE,
        column: "at",
    })?;
    let rows = sqlx::query!(
        r#"SELECT id AS "id!: i64", at AS "at!" FROM audit_log
           WHERE target_type = 'user' AND target_id = ? AND action = ? AND outcome = 'success'
                 AND at > ?
           ORDER BY id"#,
        user,
        action,
        since
    )
    .fetch_all(executor)
    .await
    .map_err(store_error)?;
    rows.into_iter()
        .map(|row| {
            parse_time(&row.at).ok_or(StoreError::Corrupt {
                table: TABLE,
                column: "at",
                rowid: row.id,
            })
        })
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
