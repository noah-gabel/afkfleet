//! The audit log's read, for Admins and the Owner (Plan.md P11.9; ADR-0020, U77,
//! S89).

use fleet_api_types::SafeInt;
use fleet_api_types::app::CommandError;
use fleet_api_types::audit::AuditPage;

use super::Managed;

/// A page of the audit log, optionally narrowed to one target's entries.
#[tauri::command]
pub(crate) async fn audit(
    state: Managed<'_>,
    before: Option<SafeInt>,
    limit: Option<u32>,
    target_kind: Option<String>,
    target_id: Option<String>,
) -> Result<AuditPage, CommandError> {
    state.audit(before, limit, target_kind, target_id).await
}
