//! The bots' and modes' commands (Plan.md P11.9; ADR-0020, U32, S73): the
//! reads the bot screens show, and the bot's writes: its server (after a
//! step-up), its mode, and its lifecycle calls.

use fleet_api_types::app::{BotCommandResponse, CommandError};
use fleet_api_types::bots::{BotModeRequest, BotPage, BotPatchRequest, BotResponse};
use fleet_api_types::modes::ModePage;
use fleet_api_types::{BotId, ModeId};

use super::Managed;

#[tauri::command]
pub(crate) async fn bots(
    state: Managed<'_>,
    before: Option<BotId>,
    limit: Option<u32>,
) -> Result<BotPage, CommandError> {
    state.bots(before, limit).await
}

#[tauri::command]
pub(crate) async fn bot(state: Managed<'_>, id: BotId) -> Result<BotResponse, CommandError> {
    state.bot(id).await
}

#[tauri::command]
pub(crate) async fn modes(
    state: Managed<'_>,
    before: Option<ModeId>,
    limit: Option<u32>,
) -> Result<ModePage, CommandError> {
    state.modes(before, limit).await
}

#[tauri::command]
pub(crate) async fn patch_bot(
    state: Managed<'_>,
    id: BotId,
    request: BotPatchRequest,
) -> Result<BotResponse, CommandError> {
    state.patch_bot(id, request).await
}

#[tauri::command]
pub(crate) async fn set_bot_mode(
    state: Managed<'_>,
    id: BotId,
    request: BotModeRequest,
) -> Result<BotResponse, CommandError> {
    state.set_bot_mode(id, request).await
}

#[tauri::command]
pub(crate) async fn start_bot(state: Managed<'_>, id: BotId) -> Result<BotResponse, CommandError> {
    state.start_bot(id).await
}

#[tauri::command]
pub(crate) async fn stop_bot(state: Managed<'_>, id: BotId) -> Result<BotResponse, CommandError> {
    state.stop_bot(id).await
}

#[tauri::command]
pub(crate) async fn restart_bot(
    state: Managed<'_>,
    id: BotId,
) -> Result<BotCommandResponse, CommandError> {
    state.restart_bot(id).await
}

#[tauri::command]
pub(crate) async fn reset_bot(
    state: Managed<'_>,
    id: BotId,
) -> Result<BotCommandResponse, CommandError> {
    state.reset_bot(id).await
}

#[tauri::command]
pub(crate) async fn resume_bot(
    state: Managed<'_>,
    id: BotId,
) -> Result<BotCommandResponse, CommandError> {
    state.resume_bot(id).await
}
