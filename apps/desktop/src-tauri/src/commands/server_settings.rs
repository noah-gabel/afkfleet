//! The servers' settings commands (Plan.md P11.9; ADR-0020, U54, U56, S77):
//! Admins and the Owner read every server's settings a page at a time, and
//! after a step-up create, edit and delete them.

use fleet_api_types::ServerSettingsId;
use fleet_api_types::app::CommandError;
use fleet_api_types::server_settings::{
    CreateServerSettingsRequest, ServerSettingsPage, ServerSettingsResponse,
    UpdateServerSettingsRequest,
};

use super::Managed;

#[tauri::command]
pub(crate) async fn server_settings(
    state: Managed<'_>,
    before: Option<ServerSettingsId>,
    limit: Option<u32>,
) -> Result<ServerSettingsPage, CommandError> {
    state.server_settings(before, limit).await
}

#[tauri::command]
pub(crate) async fn create_server_settings(
    state: Managed<'_>,
    request: CreateServerSettingsRequest,
) -> Result<ServerSettingsResponse, CommandError> {
    state.create_server_settings(request).await
}

#[tauri::command]
pub(crate) async fn update_server_settings(
    state: Managed<'_>,
    id: ServerSettingsId,
    request: UpdateServerSettingsRequest,
) -> Result<ServerSettingsResponse, CommandError> {
    state.update_server_settings(id, request).await
}

#[tauri::command]
pub(crate) async fn delete_server_settings(
    state: Managed<'_>,
    id: ServerSettingsId,
) -> Result<(), CommandError> {
    state.delete_server_settings(id).await
}
