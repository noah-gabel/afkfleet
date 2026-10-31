//! The grants' commands: who else has access to an account (Plan.md P9.8).

use fleet_api_types::accounts::{ChangeGrantRequest, CreateGrantRequest, GrantPage, GrantResponse};
use fleet_api_types::app::CommandError;
use fleet_api_types::{AccountId, UserId};

use super::Managed;

#[tauri::command]
pub(crate) async fn account_grants(
    state: Managed<'_>,
    id: AccountId,
    before: Option<UserId>,
    limit: Option<u32>,
) -> Result<GrantPage, CommandError> {
    state.account_grants(id, before, limit).await
}

#[tauri::command]
pub(crate) async fn create_grant(
    state: Managed<'_>,
    id: AccountId,
    request: CreateGrantRequest,
) -> Result<GrantResponse, CommandError> {
    state.create_grant(id, request).await
}

#[tauri::command]
pub(crate) async fn change_grant(
    state: Managed<'_>,
    id: AccountId,
    user: UserId,
    request: ChangeGrantRequest,
) -> Result<GrantResponse, CommandError> {
    state.change_grant(id, user, request).await
}

#[tauri::command]
pub(crate) async fn revoke_grant(
    state: Managed<'_>,
    id: AccountId,
    user: UserId,
) -> Result<(), CommandError> {
    state.revoke_grant(id, user).await
}

/// Gives up the caller's own grant on a shared account, after a step-up.
#[tauri::command]
pub(crate) async fn leave_account(state: Managed<'_>, id: AccountId) -> Result<(), CommandError> {
    state.leave_account(id).await
}
