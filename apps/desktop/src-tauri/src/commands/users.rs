//! The users Admins and the Owner list and change.

use fleet_api_types::accounts::{AccountPage, HeldGrantPage};
use fleet_api_types::app::CommandError;
use fleet_api_types::users::{UserPage, UserPatchRequest, UserResponse};
use fleet_api_types::{AccountId, UserId};

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

/// One change of a user: a role or `disabled`, after a step-up.
#[tauri::command]
pub(crate) async fn patch_user(
    state: Managed<'_>,
    id: UserId,
    request: UserPatchRequest,
) -> Result<UserResponse, CommandError> {
    state.patch_user(id, request).await
}

/// A page of the grants a user holds, the Owner's review (ADR-0020, S50).
#[tauri::command]
pub(crate) async fn user_grants(
    state: Managed<'_>,
    id: UserId,
    before: Option<AccountId>,
    limit: Option<u32>,
) -> Result<HeldGrantPage, CommandError> {
    state.user_grants(id, before, limit).await
}

/// A page of a user's accounts that a grant shares, the Owner's review (S50).
#[tauri::command]
pub(crate) async fn shared_accounts(
    state: Managed<'_>,
    id: UserId,
    before: Option<AccountId>,
    limit: Option<u32>,
) -> Result<AccountPage, CommandError> {
    state.shared_accounts(id, before, limit).await
}
