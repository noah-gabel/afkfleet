//! The grant routes (Plan.md P9.6, P11.9, §7.3, Appendix B; ADR-0010,
//! ADR-0018, ADR-0020): `GET` and `POST /api/v1/accounts/{id}/grants`, `PUT`
//! and `DELETE /api/v1/accounts/{id}/grants/{user}`, and `DELETE
//! /api/v1/accounts/{id}/grants/me`.
//!
//! - **Listing** needs Manage on the account (`ManageGrants`); an account
//!   the caller may not see is 404.
//! - **Creating** names the grantee by their username, since Members can't
//!   list users. Every name that can't be granted gets one 422 on
//!   `username` ([`UNGRANTABLE`]), and the route spends its own per-user
//!   limit, so names can't be probed fast (ADR-0018, group E).
//! - **Changing** and **revoking** go by the grantee's ID.
//! - **Leaving** is the grantee's own: any level, without implicit Manage
//!   (`LeaveAccount`; ADR-0020, S51). The path's `me` names no one else, so
//!   it never reaches another user's grant.
//!
//! Every write needs a step-up (`SteppedUpUser`, the handlers' first
//! extractor), and is authorized here and again in the service's
//! transaction.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fleet_api_types::accounts::{
    ChangeGrantRequest, CreateGrantRequest, GrantLevel as ApiGrantLevel, GrantPage, GrantResponse,
    UNGRANTABLE,
};
use fleet_api_types::{ErrorResponse, FieldError, PageQuery};
use fleet_core::authz::{Actor, GrantLevel, Permission};
use fleet_core::id::{AccountId, UserId};
use fleet_core::value::Username;

use super::{acting, label, page, timestamp};
use crate::app::grants::{ChangeGrantError, CreateGrantError, LeaveGrantError, RevokeGrantError};
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, SteppedUpUser, ValidJson, ValidPath, ValidQuery};
use crate::http::rate_limit::ClientIp;
use crate::http::router::AppState;
use crate::ports::accounts::ListedAccount;
use crate::ports::grants::ListedGrant;

/// `GET /api/v1/accounts/{id}/grants`: who the account is shared with.
#[utoipa::path(
    get,
    path = "/api/v1/accounts/{id}/grants",
    operation_id = "list_grants",
    tag = "accounts",
    params(("id" = String, Path, description = "The account's ID."), PageQuery<UserId>),
    responses(
        (status = 200, description = "One page of the account's grants, newest grantee first.", body = GrantPage),
        (status = 400, description = "A query that doesn't parse.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "The caller may see the account but not its grants (Manage).", body = ErrorResponse),
        (status = 404, description = "No such account, or one the caller may not see.", body = ErrorResponse),
        (status = 422, description = "A `limit` outside 1 to 100.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn list(
    user: AuthUser,
    State(state): State<AppState>,
    ValidPath(id): ValidPath<AccountId>,
    ValidQuery(query): ValidQuery<PageQuery<UserId>>,
) -> Result<Json<GrantPage>, ApiError> {
    let account = facts(&state, &user.actor(), id).await?;
    user.require(Permission::ManageGrants, &account.context())?;
    let listed = state.grants.list(id, page(&query)?).await?;
    Ok(Json(GrantPage {
        items: listed
            .items
            .iter()
            .map(grant_response)
            .collect::<Result<_, _>>()?,
        next: listed.next,
    }))
}

/// `POST /api/v1/accounts/{id}/grants`: shares the account with the user
/// named in the body. Needs a step-up.
#[utoipa::path(
    post,
    path = "/api/v1/accounts/{id}/grants",
    operation_id = "create_grant",
    tag = "accounts",
    params(("id" = String, Path, description = "The account's ID.")),
    request_body = CreateGrantRequest,
    responses(
        (status = 201, description = "The grant.", body = GrantResponse),
        (status = 400, description = "A body that isn't the request's shape.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or the caller may see the account but not share it (Manage), or names themselves.", body = ErrorResponse),
        (status = 404, description = "No such account, or one the caller may not see.", body = ErrorResponse),
        (status = 409, description = "The user holds a grant on the account already (change it instead), or the account is pending.", body = ErrorResponse),
        (status = 415, description = "A body that isn't JSON.", body = ErrorResponse),
        (status = 422, description = "A name that isn't a username, or one that can't be given a grant: the same answer for an unknown name, a disabled user and someone who has access already.", body = ErrorResponse),
        (status = 429, description = "Too many requests, or too many grants by name in the last hour.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn create(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<AccountId>,
    body: Result<ValidJson<CreateGrantRequest>, ApiError>,
) -> Result<(StatusCode, Json<GrantResponse>), ApiError> {
    let user = user.user();
    // Each request tries a name, so it spends its limit before anything is
    // read.
    state.limits.check_grant_by_name(user.actor().id)?;
    let ValidJson(request) = body?;
    let account = facts(&state, &user.actor(), id).await?;
    user.require(Permission::ManageGrants, &account.context())?;
    // garde checked it's a username.
    let name = Username::try_from(request.username.expose()).map_err(ApiError::internal)?;
    let grant = state
        .grants
        .create_by_name(&acting(user, ip), id, &name, request.level.into())
        .await
        .map_err(|error| match error {
            CreateGrantError::NotFound => ApiError::NotFound,
            CreateGrantError::SessionGone => ApiError::Unauthorized,
            CreateGrantError::Denied(error) => ApiError::from(error),
            CreateGrantError::Pending | CreateGrantError::Exists => ApiError::Conflict,
            CreateGrantError::Unusable(_) => {
                ApiError::Validation(vec![FieldError::new("username", UNGRANTABLE)])
            }
            CreateGrantError::Store(error) => ApiError::from(error),
            other @ CreateGrantError::Audit(_) => ApiError::internal(other),
        })?;
    Ok((StatusCode::CREATED, Json(grant_response(&grant)?)))
}

/// `PUT /api/v1/accounts/{id}/grants/{user}`: changes the user's grant.
/// Needs a step-up.
#[utoipa::path(
    put,
    path = "/api/v1/accounts/{id}/grants/{user}",
    operation_id = "change_grant",
    tag = "accounts",
    params(
        ("id" = String, Path, description = "The account's ID."),
        ("user" = String, Path, description = "The grantee's ID."),
    ),
    request_body = ChangeGrantRequest,
    responses(
        (status = 200, description = "The grant, changed.", body = GrantResponse),
        (status = 400, description = "A body that isn't the request's shape.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or the caller may see the account but not share it (Manage), or names themselves.", body = ErrorResponse),
        (status = 404, description = "No such account, one the caller may not see, or no grant of this user on it.", body = ErrorResponse),
        (status = 409, description = "A raise on a pending account, or for a user who has access through their role by now.", body = ErrorResponse),
        (status = 415, description = "A body that isn't JSON.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn change(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath((id, grantee)): ValidPath<(AccountId, UserId)>,
    ValidJson(request): ValidJson<ChangeGrantRequest>,
) -> Result<Json<GrantResponse>, ApiError> {
    let user = user.user();
    let level: GrantLevel = request.level.into();
    let account = facts(&state, &user.actor(), id).await?;
    user.require(Permission::Grant { level, grantee }, &account.context())?;
    let grant = state
        .grants
        .set_level(&acting(user, ip), id, grantee, level)
        .await
        .map_err(|error| match error {
            ChangeGrantError::NotFound | ChangeGrantError::NoGrant => ApiError::NotFound,
            ChangeGrantError::SessionGone => ApiError::Unauthorized,
            ChangeGrantError::Denied(error) => ApiError::from(error),
            ChangeGrantError::Pending | ChangeGrantError::ImplicitManage => ApiError::Conflict,
            ChangeGrantError::Store(error) => ApiError::from(error),
            other @ ChangeGrantError::Audit(_) => ApiError::internal(other),
        })?;
    Ok(Json(grant_response(&grant)?))
}

/// `DELETE /api/v1/accounts/{id}/grants/{user}`: revokes the user's grant.
/// Needs a step-up.
#[utoipa::path(
    delete,
    path = "/api/v1/accounts/{id}/grants/{user}",
    operation_id = "revoke_grant",
    tag = "accounts",
    params(
        ("id" = String, Path, description = "The account's ID."),
        ("user" = String, Path, description = "The grantee's ID."),
    ),
    responses(
        (status = 204, description = "The grant is revoked."),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or the caller may see the account but not its grants (Manage).", body = ErrorResponse),
        (status = 404, description = "No such account, one the caller may not see, or no grant of this user on it.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn revoke(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath((id, grantee)): ValidPath<(AccountId, UserId)>,
) -> Result<StatusCode, ApiError> {
    let user = user.user();
    let account = facts(&state, &user.actor(), id).await?;
    user.require(Permission::ManageGrants, &account.context())?;
    state
        .grants
        .revoke(&acting(user, ip), id, grantee)
        .await
        .map_err(|error| match error {
            RevokeGrantError::NotFound | RevokeGrantError::NoGrant => ApiError::NotFound,
            RevokeGrantError::SessionGone => ApiError::Unauthorized,
            RevokeGrantError::Denied(error) => ApiError::from(error),
            RevokeGrantError::Store(error) => ApiError::from(error),
            other @ RevokeGrantError::Audit(_) => ApiError::internal(other),
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/v1/accounts/{id}/grants/me`: the caller gives up their own
/// grant on the account. Needs a step-up.
#[utoipa::path(
    delete,
    path = "/api/v1/accounts/{id}/grants/me",
    operation_id = "leave_account",
    tag = "accounts",
    params(("id" = String, Path, description = "The account's ID.")),
    responses(
        (status = 204, description = "The caller's grant is gone: they left the account."),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or the caller has the account through ownership or their role, not a grant.", body = ErrorResponse),
        (status = 404, description = "No such account, or one the caller holds no grant on.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn leave(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<AccountId>,
) -> Result<StatusCode, ApiError> {
    let user = user.user();
    let account = facts(&state, &user.actor(), id).await?;
    user.require(Permission::LeaveAccount, &account.context())?;
    state
        .grants
        .leave(&acting(user, ip), id)
        .await
        .map_err(|error| match error {
            LeaveGrantError::NotFound => ApiError::NotFound,
            LeaveGrantError::SessionGone => ApiError::Unauthorized,
            LeaveGrantError::Denied(error) => ApiError::from(error),
            LeaveGrantError::Store(error) => ApiError::from(error),
            other @ LeaveGrantError::Audit(_) => ApiError::internal(other),
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// The account `id` as `actor` sees it, or 404.
async fn facts(state: &AppState, actor: &Actor, id: AccountId) -> Result<ListedAccount, ApiError> {
    state
        .accounts
        .view(actor, id)
        .await?
        .ok_or(ApiError::NotFound)
}

/// A grant as the API shows it.
fn grant_response(listed: &ListedGrant) -> Result<GrantResponse, ApiError> {
    let grant = &listed.grant;
    Ok(GrantResponse {
        user: label(grant.user, &listed.user_name),
        level: ApiGrantLevel::from(grant.level).into(),
        granted_by: label(grant.granted_by, &listed.granted_by_name),
        granted_at: timestamp(grant.granted_at)?,
    })
}
