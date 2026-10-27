//! The bots' and modes' commands (Plan.md P11.9; ADR-0020, U32): the reads
//! the bot screens show.

use fleet_api_types::app::CommandError;
use fleet_api_types::bots::{BotPage, BotResponse};
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
