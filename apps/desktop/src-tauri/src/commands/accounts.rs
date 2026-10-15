//! The Microsoft accounts' commands: the lists, one account, offline
//! accounts, deleting, and reviewing a pending link (Plan.md P9.8).

use fleet_api_types::AccountId;
use fleet_api_types::accounts::{
    AccountPage, AccountResponse, CreateOfflineAccountRequest, CreatedOfflineAccountResponse,
};
use fleet_api_types::app::CommandError;

use super::Managed;

#[tauri::command]
pub(crate) async fn create_offline_account(
    state: Managed<'_>,
    request: CreateOfflineAccountRequest,
) -> Result<CreatedOfflineAccountResponse, CommandError> {
    state.create_offline_account(request).await
}

#[tauri::command]
pub(crate) async fn accounts(
    state: Managed<'_>,
    before: Option<AccountId>,
    limit: Option<u32>,
) -> Result<AccountPage, CommandError> {
    state.accounts(before, limit).await
}

#[tauri::command]
pub(crate) async fn accounts_awaiting_approval(
    state: Managed<'_>,
    before: Option<AccountId>,
    limit: Option<u32>,
) -> Result<AccountPage, CommandError> {
    state.accounts_awaiting_approval(before, limit).await
}

#[tauri::command]
pub(crate) async fn account(
    state: Managed<'_>,
    id: AccountId,
) -> Result<AccountResponse, CommandError> {
    state.account(id).await
}

#[tauri::command]
pub(crate) async fn delete_account(state: Managed<'_>, id: AccountId) -> Result<(), CommandError> {
    state.delete_account(id).await
}

#[tauri::command]
pub(crate) async fn approve_account(
    state: Managed<'_>,
    id: AccountId,
) -> Result<AccountResponse, CommandError> {
    state.approve_account(id).await
}

#[tauri::command]
pub(crate) async fn refuse_account(state: Managed<'_>, id: AccountId) -> Result<(), CommandError> {
    state.refuse_account(id).await
}
