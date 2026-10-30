//! The modes' write commands (Plan.md P11.9; ADR-0020, U62, S16–S18): a
//! request whose steps send a command off the allowlist goes to its
//! `_with_commands` twin, after a step-up, and `delete_mode` needs one always.

use fleet_api_types::ModeId;
use fleet_api_types::app::CommandError;
use fleet_api_types::modes::{CreateModeRequest, ModeResponse, UpdateModeRequest};

use super::Managed;

#[tauri::command]
pub(crate) async fn create_mode(
    state: Managed<'_>,
    request: CreateModeRequest,
) -> Result<ModeResponse, CommandError> {
    state.create_mode(request).await
}

#[tauri::command]
pub(crate) async fn create_mode_with_commands(
    state: Managed<'_>,
    request: CreateModeRequest,
) -> Result<ModeResponse, CommandError> {
    state.create_mode_with_commands(request).await
}

#[tauri::command]
pub(crate) async fn update_mode(
    state: Managed<'_>,
    id: ModeId,
    request: UpdateModeRequest,
) -> Result<ModeResponse, CommandError> {
    state.update_mode(id, request).await
}

#[tauri::command]
pub(crate) async fn update_mode_with_commands(
    state: Managed<'_>,
    id: ModeId,
    request: UpdateModeRequest,
) -> Result<ModeResponse, CommandError> {
    state.update_mode_with_commands(id, request).await
}

#[tauri::command]
pub(crate) async fn delete_mode(state: Managed<'_>, id: ModeId) -> Result<(), CommandError> {
    state.delete_mode(id).await
}
