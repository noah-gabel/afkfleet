//! The user's own routes (Plan.md P7.6, §7.2, Appendix B, ADR-0016):
//! `GET /api/v1/me`, `POST /api/v1/me/password`, `GET /api/v1/me/sessions`
//! and `DELETE /api/v1/me/sessions/{id}`.
//!
//! - **The profile** counts the wrong 2FA codes since the previous login.
//! - **A new password** needs a fresh step-up (`step_up_route`); a weak one
//!   is 422 with zxcvbn's hints, one field error each, and the current one
//!   is 422 too. It ends every other session and this one's step-up.
//! - **The sessions** are the live ones; another session of the user's
//!   ends with `DELETE`, the current one only through `POST /auth/logout`
//!   (409 here). Someone else's session is 404, like an unknown one.
//!
//! Each handler authorizes `UsePersonal` on the user's own resources.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fleet_api_types::me::{FailedCodeAttempts, MeResponse, PasswordChangeRequest};
use fleet_api_types::sessions::{SessionPage, SessionResponse};
use fleet_api_types::users::UserRole;
use fleet_api_types::{ErrorResponse, FieldError, PageQuery};
use fleet_core::auth::NewPassword;
use fleet_core::authz::{Permission, ResourceContext};
use fleet_core::id::SessionId;

use super::{page, refused_password, timestamp, username};
use crate::app::auth::{RevokeSessionError, SessionView};
use crate::app::users::{ChangePasswordError, MeView};
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, SteppedUpUser, ValidJson, ValidPath, ValidQuery};
use crate::http::rate_limit::ClientIp;
use crate::http::router::AppState;
use crate::ports::passwords::VerifyError;

/// The 422 message for a new password that is the current one.
pub(super) const SAME_PASSWORD: &str = "is the current password";

/// Authorizes `user` on their own resources.
fn own(user: &AuthUser) -> Result<(), ApiError> {
    let owner = user.actor().id;
    user.require(
        Permission::UsePersonal,
        &ResourceContext::Personal { owner },
    )?;
    Ok(())
}

/// The profile as the API shows it.
fn me_response(me: &MeView) -> Result<MeResponse, ApiError> {
    Ok(MeResponse {
        id: me.id,
        username: username(&me.username),
        role: UserRole::from(me.role).into(),
        created_at: timestamp(me.created_at)?,
        password_changed_at: timestamp(me.password_changed_at)?,
        mfa_confirmed_at: me.mfa_confirmed_at.map(timestamp).transpose()?,
        recovery_codes_left: me.recovery_codes_left,
        failed_code_attempts: FailedCodeAttempts {
            count: u32::try_from(me.failed_code_attempts.count).unwrap_or(u32::MAX),
            last_at: me.failed_code_attempts.last_at.map(timestamp).transpose()?,
        },
    })
}

/// A session as the API shows it.
fn session_response(view: &SessionView) -> Result<SessionResponse, ApiError> {
    Ok(SessionResponse {
        id: view.session.id,
        created_at: timestamp(view.session.created_at)?,
        last_used_at: timestamp(view.session.last_used_at)?,
        expires_at: timestamp(view.expires_at)?,
        current: view.current,
    })
}

/// `GET /api/v1/me`: the user's profile and security facts.
#[utoipa::path(
    get,
    path = "/api/v1/me",
    operation_id = "me",
    tag = "me",
    responses(
        (status = 200, description = "The user's profile.", body = MeResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn me(
    user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<MeResponse>, ApiError> {
    own(&user)?;
    let me = state
        .users
        .me(user.actor().id)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    Ok(Json(me_response(&me)?))
}

/// `POST /api/v1/me/password`: a new password, after stepping up.
#[utoipa::path(
    post,
    path = "/api/v1/me/password",
    operation_id = "change_password",
    tag = "me",
    request_body = PasswordChangeRequest,
    responses(
        (status = 204, description = "The password is changed; every other session ended."),
        (status = 400, description = "A body of the wrong shape.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`.", body = ErrorResponse),
        (status = 422, description = "The new password breaks its rules, is too weak (with hints), or is the current one.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn change_password(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidJson(request): ValidJson<PasswordChangeRequest>,
) -> Result<StatusCode, ApiError> {
    let user = user.user();
    own(user)?;
    let password = NewPassword::try_from(request.new_password.into_secret())
        .map_err(|_| ApiError::internal("the rule already holds: garde checked it"))?;
    state
        .users
        .change_password(user.actor().id, user.session_id(), &password, ip)
        .await
        .map_err(|error| match error {
            ChangePasswordError::SessionGone => ApiError::Unauthorized,
            ChangePasswordError::StepUpRequired => ApiError::StepUpRequired,
            ChangePasswordError::SamePassword => {
                ApiError::Validation(vec![FieldError::new("new_password", SAME_PASSWORD)])
            }
            ChangePasswordError::Password(error) => refused_password("new_password", error),
            ChangePasswordError::Verify(VerifyError::Busy) => ApiError::busy(),
            ChangePasswordError::Store(error) => ApiError::from(error),
            other @ (ChangePasswordError::Verify(_) | ChangePasswordError::Audit(_)) => {
                ApiError::internal(other)
            }
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/me/sessions`: one page of the user's live sessions.
#[utoipa::path(
    get,
    path = "/api/v1/me/sessions",
    operation_id = "list_sessions",
    tag = "me",
    params(PageQuery<SessionId>),
    responses(
        (status = 200, description = "One page of live sessions, newest first.", body = SessionPage),
        (status = 400, description = "A query that doesn't parse.", body = ErrorResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 422, description = "A `limit` outside 1 to 100.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn list_sessions(
    user: AuthUser,
    State(state): State<AppState>,
    ValidQuery(query): ValidQuery<PageQuery<SessionId>>,
) -> Result<Json<SessionPage>, ApiError> {
    own(&user)?;
    let listed = state
        .tokens
        .list_sessions(user.actor().id, user.session_id(), page(&query)?)
        .await?;
    Ok(Json(SessionPage {
        items: listed
            .items
            .iter()
            .map(session_response)
            .collect::<Result<_, _>>()?,
        next: listed.next,
    }))
}

/// `DELETE /api/v1/me/sessions/{id}`: ends another session of the user's.
#[utoipa::path(
    delete,
    path = "/api/v1/me/sessions/{id}",
    operation_id = "revoke_session",
    tag = "me",
    params(("id" = String, Path, description = "The session's ID.")),
    responses(
        (status = 204, description = "The session is over."),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 404, description = "No live session of the user's with this ID.", body = ErrorResponse),
        (status = 409, description = "It's this request's session: log out instead.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn revoke_session(
    user: AuthUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidPath(id): ValidPath<SessionId>,
) -> Result<StatusCode, ApiError> {
    own(&user)?;
    state
        .tokens
        .revoke_own(user.actor().id, user.session_id(), id, ip)
        .await
        .map_err(|error| match error {
            RevokeSessionError::Current => ApiError::Conflict,
            RevokeSessionError::NotFound => ApiError::NotFound,
            RevokeSessionError::Store(error) => ApiError::from(error),
            other @ RevokeSessionError::Audit(_) => ApiError::internal(other),
        })?;
    Ok(StatusCode::NO_CONTENT)
}
