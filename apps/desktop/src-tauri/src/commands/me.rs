//! The user's own profile, password and sessions.

use fleet_api_types::SessionId;
use fleet_api_types::app::CommandError;
use fleet_api_types::me::{MeResponse, PasswordChangeRequest};
use fleet_api_types::sessions::SessionPage;

use super::Managed;

/// The user's profile.
#[tauri::command]
pub(crate) async fn me(state: Managed<'_>) -> Result<MeResponse, CommandError> {
    state.me().await
}

/// A new password, after a step-up.
#[tauri::command]
pub(crate) async fn change_password(
    state: Managed<'_>,
    request: PasswordChangeRequest,
) -> Result<(), CommandError> {
    state.change_password(request).await
}

/// A page of the user's live sessions.
#[tauri::command]
pub(crate) async fn sessions(
    state: Managed<'_>,
    before: Option<SessionId>,
    limit: Option<u32>,
) -> Result<SessionPage, CommandError> {
    state.sessions(before, limit).await
}

/// Ends another session of the user.
#[tauri::command]
pub(crate) async fn revoke_session(state: Managed<'_>, id: SessionId) -> Result<(), CommandError> {
    state.revoke_session(id).await
}
