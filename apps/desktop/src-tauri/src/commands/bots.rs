//! The bots' and modes' commands (Plan.md P11.9; ADR-0020, U32, U34, S73):
//! the reads the bot screens show, the bot's writes (its server after a
//! step-up, its mode, its lifecycle calls), and its chat: the history, a
//! line, and a command after a step-up.

use fleet_api_types::app::{BotCommandResponse, CommandError};
use fleet_api_types::bots::{BotModeRequest, BotPage, BotPatchRequest, BotResponse};
use fleet_api_types::chat::{ChatCommandRequest, ChatPage, ChatSendRequest, ChatSendResponse};
use fleet_api_types::modes::ModePage;
use fleet_api_types::{BotId, ModeId, SafeInt};

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

#[tauri::command]
pub(crate) async fn bot_chat(
    state: Managed<'_>,
    id: BotId,
    before: Option<SafeInt>,
    limit: Option<u32>,
) -> Result<ChatPage, CommandError> {
    state.bot_chat(id, before, limit).await
}

#[tauri::command]
pub(crate) async fn send_chat(
    state: Managed<'_>,
    id: BotId,
    request: ChatSendRequest,
) -> Result<ChatSendResponse, CommandError> {
    state.send_chat(id, request).await
}

#[tauri::command]
pub(crate) async fn send_command(
    state: Managed<'_>,
    id: BotId,
    request: ChatCommandRequest,
) -> Result<ChatSendResponse, CommandError> {
    state.send_command(id, request).await
}
