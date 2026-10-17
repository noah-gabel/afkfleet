//! The `modes` table's SQL.
//!
//! A mode's JSON is read back only through fleet-core's validated
//! deserializer; `serde_json`'s error, which can quote the stored text, is
//! dropped for [`StoreError::Corrupt`], which never holds it.

use core::num::NonZeroU32;

use fleet_core::authz::ModeVisibility;
use fleet_core::id::ModeId;
use fleet_core::mode::ModeDefinition;

use super::convert::{id_text, parse_id, parse_time};
use super::error::store_error;
use crate::ports::modes::StoredMode;
use crate::ports::store::StoreError;

/// The table, as errors name it.
const MODES: &str = "modes";

/// A `modes` row, before its values are checked.
struct ModeRow {
    rowid: i64,
    id: String,
    owner_user_id: Option<String>,
    visibility: String,
    definition_json: String,
    version: i64,
    created_at: String,
    updated_at: String,
}

/// The mode `id`.
pub(super) async fn find<'c, E>(executor: E, id: ModeId) -> Result<Option<StoredMode>, StoreError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let id = id_text(id);
    sqlx::query_as!(
        ModeRow,
        r#"SELECT rowid AS "rowid!: i64", id AS "id!", owner_user_id AS "owner_user_id?",
                  visibility AS "visibility!", definition_json AS "definition_json!",
                  version AS "version!: i64", created_at AS "created_at!",
                  updated_at AS "updated_at!"
           FROM modes WHERE id = ?"#,
        id
    )
    .fetch_optional(executor)
    .await
    .map_err(store_error)?
    .map(|row| mode_from_row(&row))
    .transpose()
}

/// Reads a `modes` row back, or [`StoreError::Corrupt`] naming the first
/// column that doesn't.
fn mode_from_row(row: &ModeRow) -> Result<StoredMode, StoreError> {
    let rowid = row.rowid;
    let corrupt = |column| StoreError::Corrupt {
        table: MODES,
        column,
        rowid,
    };
    let owner = row
        .owner_user_id
        .as_deref()
        .map(|owner| parse_id(owner).ok_or_else(|| corrupt("owner_user_id")))
        .transpose()?;
    // serde_json's error can quote the stored text: it's dropped here.
    let definition: ModeDefinition =
        serde_json::from_str(&row.definition_json).map_err(|_| corrupt("definition_json"))?;
    Ok(StoredMode {
        id: parse_id(&row.id).ok_or_else(|| corrupt("id"))?,
        owner,
        visibility: row
            .visibility
            .parse::<ModeVisibility>()
            .map_err(|_| corrupt("visibility"))?,
        definition,
        version: u32::try_from(row.version)
            .ok()
            .and_then(NonZeroU32::new)
            .ok_or_else(|| corrupt("version"))?,
        created_at: parse_time(&row.created_at).ok_or_else(|| corrupt("created_at"))?,
        updated_at: parse_time(&row.updated_at).ok_or_else(|| corrupt("updated_at"))?,
    })
}
