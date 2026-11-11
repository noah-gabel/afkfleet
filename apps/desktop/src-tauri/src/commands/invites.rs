//! The invites that let someone register.

use fleet_api_types::InviteId;
use fleet_api_types::app::CommandError;
use fleet_api_types::invites::{CreateInviteRequest, CreatedInviteResponse, InvitePage};

use super::Managed;

/// A page of the invites.
#[tauri::command]
pub(crate) async fn invites(
    state: Managed<'_>,
    before: Option<InviteId>,
    limit: Option<u32>,
) -> Result<InvitePage, CommandError> {
    state.invites(before, limit).await
}

/// A new invite, whose code is answered this once.
#[tauri::command]
pub(crate) async fn create_invite(
    state: Managed<'_>,
    request: CreateInviteRequest,
) -> Result<CreatedInviteResponse, CommandError> {
    state.create_invite(request).await
}

/// Revokes a pending invite.
#[tauri::command]
pub(crate) async fn revoke_invite(state: Managed<'_>, id: InviteId) -> Result<(), CommandError> {
    state.revoke_invite(id).await
}
