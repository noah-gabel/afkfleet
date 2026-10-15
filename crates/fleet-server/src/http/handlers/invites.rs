//! The invite routes (Plan.md P7.9, P7.1, Appendix B, ADR-0010,
//! ADR-0016): `POST /api/v1/invites`, `GET /api/v1/invites` and `DELETE
//! /api/v1/invites/{id}`.
//!
//! - **Creating** a Member invite is Admin+, an Admin invite the Owner's,
//!   and an Owner invite nobody's. The code is in this one answer.
//! - **Listing** is Admin+: every invite, in every state.
//! - **Revoking** follows creating. Only a pending invite changes; a used
//!   one is 409, a revoked or expired one 204 again.
//!
//! Each write is authorized here and again in the service's transaction.
//! Creating and revoking need a step-up (`SteppedUpUser`, the handlers'
//! first extractor; Phase 8 group E, ADR-0016's amendment): the password
//! again within 5 minutes, checked when the request arrives, before anything
//! else is read. Listing doesn't.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fleet_api_types::invites::{
    CreateInviteRequest, CreatedInviteResponse, InvitePage, InviteResponse,
};
use fleet_api_types::users::UserRole;
use fleet_api_types::{ErrorResponse, PageQuery};
use fleet_core::authz::{Permission, ResourceContext, Role};
use fleet_core::id::InviteId;

use super::{acting, label, page, secret_text, timestamp};
use crate::app::invites::{CreateInviteError, InviteView, RevokeInviteError};
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, SteppedUpUser, ValidJson, ValidPath, ValidQuery};
use crate::http::rate_limit::ClientIp;
use crate::http::router::AppState;

/// An invite as the API shows it.
fn invite_response(view: &InviteView) -> Result<InviteResponse, ApiError> {
    let listed = &view.invite;
    let invite = &listed.invite;
    Ok(InviteResponse {
        id: invite.id,
        role: UserRole::from(invite.role).into(),
        state: fleet_api_types::invites::InviteState::from(view.state).into(),
        created_by: label(invite.created_by, &listed.created_by_name),
        created_at: timestamp(invite.created_at)?,
        expires_at: timestamp(invite.expires_at)?,
        used_by: invite
            .used_by
            .zip(listed.used_by_name.as_ref())
            .map(|(id, name)| label(id, name)),
        used_at: invite.used_at.map(timestamp).transpose()?,
        revoked_at: invite.revoked_at.map(timestamp).transpose()?,
    })
}

/// `POST /api/v1/invites`: a new invite and its code, shown this once.
/// Needs a step-up.
#[utoipa::path(
    post,
    path = "/api/v1/invites",
    operation_id = "create_invite",
    tag = "invites",
    request_body = CreateInviteRequest,
    responses(
        (status = 201, description = "The invite and its code: send the code to the invitee.", body = CreatedInviteResponse),
        (status = 400, description = "A body of the wrong shape.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or the role isn't the caller's to invite.", body = ErrorResponse),
        (status = 409, description = "The caller already holds 20 pending invites.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn create(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidJson(request): ValidJson<CreateInviteRequest>,
) -> Result<(StatusCode, Json<CreatedInviteResponse>), ApiError> {
    let user = user.user();
    let role = Role::from(request.role);
    user.require(Permission::CreateInvite { role }, &ResourceContext::Global)?;
    let created = state
        .invites
        .create(&acting(user, ip), role)
        .await
        .map_err(|error| match error {
            CreateInviteError::SessionGone => ApiError::Unauthorized,
            CreateInviteError::Denied(error) => ApiError::from(error),
            CreateInviteError::CapReached => ApiError::Conflict,
            CreateInviteError::Store(error) => ApiError::from(error),
            other @ (CreateInviteError::Token(_)
            | CreateInviteError::Id(_)
            | CreateInviteError::Audit(_)) => ApiError::internal(other),
        })?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedInviteResponse {
            code: secret_text(&created.code),
            invite: invite_response(&created.view)?,
        }),
    ))
}

/// `GET /api/v1/invites`: one page of invites, newest first, in every
/// state.
#[utoipa::path(
    get,
    path = "/api/v1/invites",
    operation_id = "list_invites",
    tag = "invites",
    params(PageQuery<InviteId>),
    responses(
        (status = 200, description = "One page of invites, newest first.", body = InvitePage),
        (status = 400, description = "A query that doesn't parse.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Only Admins and the Owner list invites.", body = ErrorResponse),
        (status = 422, description = "A `limit` outside 1 to 100.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn list(
    user: AuthUser,
    State(state): State<AppState>,
    ValidQuery(query): ValidQuery<PageQuery<InviteId>>,
) -> Result<Json<InvitePage>, ApiError> {
    user.require(Permission::ListInvites, &ResourceContext::Global)?;
    let listed = state.invites.list(page(&query)?).await?;
    Ok(Json(InvitePage {
        items: listed
            .items
            .iter()
            .map(invite_response)
            .collect::<Result<_, _>>()?,
        next: listed.next,
    }))
}

/// `DELETE /api/v1/invites/{id}`: revokes a pending invite. Needs a step-up.
#[utoipa::path(
    delete,
    path = "/api/v1/invites/{id}",
    operation_id = "revoke_invite",
    tag = "invites",
    params(("id" = String, Path, description = "The invite's ID.")),
    responses(
        (status = 204, description = "The invite can't be used any more."),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or the invite isn't the caller's to revoke.", body = ErrorResponse),
        (status = 404, description = "No such invite, or one the caller can't see.", body = ErrorResponse),
        (status = 409, description = "The invite was used: it's history.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn revoke(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<InviteId>,
) -> Result<StatusCode, ApiError> {
    let user = user.user();
    let role = state.invites.role_of(id).await?.ok_or(ApiError::NotFound)?;
    user.require(Permission::RevokeInvite, &ResourceContext::Invite { role })?;
    state
        .invites
        .revoke(&acting(user, ip), id)
        .await
        .map_err(|error| match error {
            RevokeInviteError::NotFound => ApiError::NotFound,
            RevokeInviteError::SessionGone => ApiError::Unauthorized,
            RevokeInviteError::Denied(error) => ApiError::from(error),
            RevokeInviteError::Used => ApiError::Conflict,
            RevokeInviteError::Store(error) => ApiError::from(error),
            other @ RevokeInviteError::Audit(_) => ApiError::internal(other),
        })?;
    Ok(StatusCode::NO_CONTENT)
}
