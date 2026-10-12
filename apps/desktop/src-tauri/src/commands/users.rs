//! The users Admins and the Owner list and change.

use fleet_api_types::UserId;
use fleet_api_types::app::CommandError;
use fleet_api_types::users::{UserPage, UserPatchRequest, UserResponse};

use super::Managed;

/// A page of the users.
#[tauri::command]
pub(crate) async fn users(
    state: Managed<'_>,
    before: Option<UserId>,
    limit: Option<u32>,
) -> Result<UserPage, CommandError> {
    state.users(before, limit).await
}

/// One change of a user: a role or `disabled`.
#[tauri::command]
pub(crate) async fn patch_user(
    state: Managed<'_>,
    id: UserId,
    request: UserPatchRequest,
) -> Result<UserResponse, CommandError> {
    state.patch_user(id, request).await
}
