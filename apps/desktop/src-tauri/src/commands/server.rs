//! The server profile's commands: which server the app talks to.

use fleet_api_types::app::{
    CommandError, ServerProfileRequest, ServerProfileResponse, ServerProfileState,
};

use super::Managed;

/// The current server profile, or why there's none.
#[tauri::command]
pub(crate) async fn server_profile(state: Managed<'_>) -> Result<ServerProfileState, CommandError> {
    Ok(state.server_profile().await)
}

/// Sets the server profile; see [`crate::state::AppState::set_server_profile`].
#[tauri::command]
pub(crate) async fn set_server_profile(
    state: Managed<'_>,
    request: ServerProfileRequest,
) -> Result<ServerProfileResponse, CommandError> {
    state.set_server_profile(request).await
}

/// Whether the current server can serve requests.
#[tauri::command]
pub(crate) async fn check_server(state: Managed<'_>) -> Result<(), CommandError> {
    state.check_server().await
}
