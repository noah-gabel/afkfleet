//! The admins' user routes (Plan.md P7.9, Appendix B, ADR-0010, ADR-0016):
//! `GET /api/v1/users` and `PATCH /api/v1/users/{id}`.
//!
//! - **Listing** is Admin+ (`ListUsers`).
//! - **A change** is one field. The handler authorizes it against the
//!   target as it reads now (only the Owner sets roles; Admins disable and
//!   enable Members), and the service authorizes it again on the rows it
//!   re-reads in its transaction. An actor who can't see the target gets
//!   404, like an unknown ID.
//! - **A change needs a step-up** (`SteppedUpUser`, the handler's first
//!   extractor; Phase 8 group E, ADR-0016's amendment): the password again
//!   within 5 minutes, checked when the request arrives. Without one, any
//!   caller gets 403 `step_up_required` before the target is read, so the
//!   answer says nothing about it.

use axum::Json;
use axum::extract::State;
use fleet_api_types::users::{UserPage, UserPatchRequest, UserResponse, UserRole};
use fleet_api_types::{ErrorResponse, PageQuery};
use fleet_core::authz::{Permission, ResourceContext};
use fleet_core::id::UserId;

use super::{page, timestamp, username};
use crate::app::users::{ActingUser, SetUserError, UserChange};
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, SteppedUpUser, ValidJson, ValidPath, ValidQuery};
use crate::http::rate_limit::ClientIp;
use crate::http::router::AppState;
use crate::ports::users::ListedUser;

/// A user as the API shows them.
fn user_response(user: &ListedUser) -> Result<UserResponse, ApiError> {
    Ok(UserResponse {
        id: user.id,
        username: username(&user.username),
        role: UserRole::from(user.role).into(),
        disabled: user.disabled,
        created_at: timestamp(user.created_at)?,
        mfa_enrolled: user.mfa_enrolled,
        last_seen_at: user.last_seen_at.map(timestamp).transpose()?,
    })
}

/// `GET /api/v1/users`: one page of users, newest first.
#[utoipa::path(
    get,
    path = "/api/v1/users",
    operation_id = "list_users",
    tag = "users",
    params(PageQuery<UserId>),
    responses(
        (status = 200, description = "One page of users, newest first.", body = UserPage),
        (status = 400, description = "A query that doesn't parse.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Only Admins and the Owner list users.", body = ErrorResponse),
        (status = 422, description = "A `limit` outside 1 to 100.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn list(
    user: AuthUser,
    State(state): State<AppState>,
    ValidQuery(query): ValidQuery<PageQuery<UserId>>,
) -> Result<Json<UserPage>, ApiError> {
    user.require(Permission::ListUsers, &ResourceContext::Global)?;
    let listed = state.users.list(page(&query)?).await?;
    Ok(Json(UserPage {
        items: listed
            .items
            .iter()
            .map(user_response)
            .collect::<Result<_, _>>()?,
        next: listed.next,
    }))
}

/// `PATCH /api/v1/users/{id}`: one change, a role or `disabled`; either
/// ends every session of the user. Needs a step-up.
#[utoipa::path(
    patch,
    path = "/api/v1/users/{id}",
    operation_id = "patch_user",
    tag = "users",
    params(("id" = String, Path, description = "The user's ID.")),
    request_body = UserPatchRequest,
    responses(
        (status = 200, description = "The user after the change.", body = UserResponse),
        (status = 400, description = "A body of the wrong shape.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or the change isn't the caller's to make.", body = ErrorResponse),
        (status = 404, description = "No such user, or one the caller can't see.", body = ErrorResponse),
        (status = 422, description = "Not exactly one of `role` and `disabled`.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn patch(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<UserId>,
    ValidJson(request): ValidJson<UserPatchRequest>,
) -> Result<Json<UserResponse>, ApiError> {
    let user = user.user();
    let change = match (request.role, request.disabled) {
        (Some(role), None) => UserChange::Role(role.into()),
        (None, Some(disabled)) => UserChange::Disabled(disabled),
        _ => {
            return Err(ApiError::internal(
                "the rule already holds: garde checked it",
            ));
        }
    };
    let target = state.users.user_ref(id).await?.ok_or(ApiError::NotFound)?;
    user.require(change.permission(), &ResourceContext::User(target))?;
    let by = ActingUser {
        actor: user.actor(),
        session_id: user.session_id(),
        ip,
    };
    let changed = state
        .users
        .set(&by, id, change)
        .await
        .map_err(|error| match error {
            SetUserError::NotFound => ApiError::NotFound,
            SetUserError::SessionGone => ApiError::Unauthorized,
            SetUserError::Denied(error) => ApiError::from(error),
            SetUserError::Store(error) => ApiError::from(error),
            other @ SetUserError::Audit(_) => ApiError::internal(other),
        })?;
    Ok(Json(user_response(&changed)?))
}
