//! The link flow's commands: starting a link or a link again, its status,
//! and opening Microsoft's sign-in page (Plan.md P9.8, ADR-0018).

use fleet_api_types::accounts::{LinkFlowResponse, LinkStartedResponse};
use fleet_api_types::app::CommandError;
use fleet_api_types::{AccountId, LinkFlowId};

use super::Managed;

#[tauri::command]
pub(crate) async fn start_link(state: Managed<'_>) -> Result<LinkStartedResponse, CommandError> {
    state.start_link().await
}

#[tauri::command]
pub(crate) async fn link_status(
    state: Managed<'_>,
    id: LinkFlowId,
) -> Result<LinkFlowResponse, CommandError> {
    state.link_status(id).await
}

#[tauri::command]
pub(crate) async fn relink_account(
    state: Managed<'_>,
    id: AccountId,
) -> Result<LinkStartedResponse, CommandError> {
    state.relink_account(id).await
}

/// Synchronous, so it runs on the main thread: the system's shell call
/// belongs where `WebView2` initialized COM, and no runtime thread blocks.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri passes a command's State by value; a synchronous command only borrows it (the user's approval, ADR-0018)"
)]
#[tauri::command]
pub(crate) fn open_verification_link(state: Managed<'_>) -> Result<(), CommandError> {
    state.open_verification_link()
}
