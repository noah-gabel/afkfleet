//! The login routes (Plan.md P7.6, §7.2, Appendix B, ADR-0016):
//! `POST /api/v1/auth/login`, `/auth/login/mfa`, `/auth/refresh`,
//! `/auth/logout`, `/auth/step-up` and `/auth/register`.
//!
//! - **Login's refusals are one answer:** an unknown name, a wrong password,
//!   a disabled user, a wrong code and an unknown, expired or used-up mfa
//!   token all get 401 `invalid_credentials` with the same body, which
//!   never means a session is over.
//! - **A refused refresh** is 401 `unauthorized`: the session is over, log
//!   in again.
//! - **A wrong step-up password** is 403 `forbidden`, never 401, so the app
//!   doesn't take its session for dead.
//! - **Logout and step-up** take `AuthUser` before the body, and both
//!   authorize `UsePersonal` on the user's own resources.
//! - **Registering** answers 201 with the new user and no tokens. Every
//!   invite it can't use is the same 422 on `invite`; a taken name is 422 on
//!   `username`, and a weak password 422 on `password` with zxcvbn's
//!   hints.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fleet_api_types::auth::{
    INVALID_INVITE, LoginMfaRequest, LoginRequest, LoginResponse, LoginStatus, RefreshRequest,
    RegisterRequest, RegisterResponse, StepUpRequest, TokenResponse,
};
use fleet_api_types::users::UserRole;
use fleet_api_types::{ErrorResponse, FieldError, Open, SecretText};
use fleet_core::auth::{NewPassword, SecondFactor};
use fleet_core::authz::{Permission, ResourceContext};
use fleet_core::value::Username;
use secrecy::{ExposeSecret as _, SecretString};

use crate::app::auth::{IssuedTokens, LogoutError, RefreshError};
use crate::app::lockout::RecordFailureError;
use crate::app::login::{LoginError, PasswordStep, StepUpError};
use crate::app::mfa::CheckError;
use crate::app::users::RegisterError;
use crate::http::error::ApiError;
use crate::http::extractors::{AuthUser, ValidJson};
use crate::http::rate_limit::ClientIp;
use crate::http::router::AppState;
use crate::ports::passwords::VerifyError;

use super::{refused_password, username};

/// The 422 message for a username another user has.
pub(super) const USERNAME_TAKEN: &str = "is taken";

/// A secret the service made, as a response field.
fn text<const MAX: usize>(secret: &SecretString) -> SecretText<MAX> {
    SecretText::new(secret.expose_secret().to_owned())
}

/// Seconds left as a response field; a lifetime past `u32` is a bug.
fn seconds(left: u64) -> Result<u32, ApiError> {
    u32::try_from(left).map_err(ApiError::internal)
}

/// A session's tokens as the response.
fn tokens(issued: &IssuedTokens) -> Result<TokenResponse, ApiError> {
    Ok(TokenResponse {
        access_token: text(&issued.access_token),
        refresh_token: text(&issued.refresh_token),
        access_expires_in: seconds(issued.access_expires_in)?,
        refresh_expires_in: seconds(issued.refresh_expires_in)?,
    })
}

/// A login step's failure as the API answers it.
fn login_error(error: LoginError) -> ApiError {
    match error {
        LoginError::Refused => ApiError::InvalidCredentials,
        LoginError::Lockout(error) => ApiError::from(error),
        LoginError::Verify(VerifyError::Busy) => ApiError::Busy,
        LoginError::Check(CheckError::Store(error))
        | LoginError::Record(RecordFailureError::Store(error))
        | LoginError::Store(error) => ApiError::from(error),
        other @ (LoginError::Verify(_)
        | LoginError::Check(_)
        | LoginError::Issue(_)
        | LoginError::Token(_)
        | LoginError::Record(_)
        | LoginError::Audit(_)) => ApiError::internal(other),
    }
}

/// `POST /api/v1/auth/login`: the password step; never real tokens.
#[utoipa::path(
    post,
    path = "/api/v1/auth/login",
    tag = "auth",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "An mfa token for the code step, or a setup token for enrolling first.", body = LoginResponse),
        (status = 401, description = "The username or the password is wrong; nothing tells which.", body = ErrorResponse),
        (status = 422, description = "The username isn't a valid one, or the password is too long.", body = ErrorResponse),
        (
            status = 429,
            description = "Too many logins from this address, or the username is locked.",
            body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again.")),
        ),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn login(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidJson(request): ValidJson<LoginRequest>,
) -> Result<Json<LoginResponse>, ApiError> {
    // The rule already holds: garde checked it.
    let username = Username::try_from(request.username.expose()).map_err(ApiError::internal)?;
    let password = request.password.into_secret();
    let step = state
        .login
        .password_step(&username, &password, ip)
        .await
        .map_err(login_error)?;
    let (status, issued) = match step {
        PasswordStep::MfaRequired(issued) => (LoginStatus::MfaRequired, issued),
        PasswordStep::MfaSetupRequired(issued) => (LoginStatus::MfaSetupRequired, issued),
    };
    Ok(Json(LoginResponse {
        status: Open::from(status),
        token: text(&issued.token),
        expires_in: seconds(issued.expires_in)?,
    }))
}

/// `POST /api/v1/auth/login/mfa`: the code step; the session's tokens.
#[utoipa::path(
    post,
    path = "/api/v1/auth/login/mfa",
    tag = "auth",
    request_body = LoginMfaRequest,
    responses(
        (status = 200, description = "The session's tokens.", body = TokenResponse),
        (status = 401, description = "The code is wrong, or the mfa token is no longer valid; nothing tells which.", body = ErrorResponse),
        (status = 422, description = "The code is neither 6 digits nor a recovery code, or a field is too long.", body = ErrorResponse),
        (
            status = 429,
            description = "Too many logins from this address, or the username is locked.",
            body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again.")),
        ),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn login_mfa(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidJson(request): ValidJson<LoginMfaRequest>,
) -> Result<Json<TokenResponse>, ApiError> {
    // The rule already holds: garde checked it.
    let factor = SecondFactor::parse(request.code.expose())
        .ok_or_else(|| ApiError::internal("a code that passed its rule doesn't parse"))?;
    let issued = state
        .login
        .code_step(&request.mfa_token.into_secret(), &factor, ip)
        .await
        .map_err(login_error)?;
    Ok(Json(tokens(&issued)?))
}

/// `POST /api/v1/auth/refresh`: a new token pair for the refresh token.
#[utoipa::path(
    post,
    path = "/api/v1/auth/refresh",
    tag = "auth",
    request_body = RefreshRequest,
    responses(
        (status = 200, description = "The session's new tokens; the old ones stop working.", body = TokenResponse),
        (status = 401, description = "The session is over: log in again.", body = ErrorResponse),
        (status = 422, description = "The token field is too long.", body = ErrorResponse),
        (status = 429, description = "Too many refreshes from this address.", body = ErrorResponse),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn refresh(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidJson(request): ValidJson<RefreshRequest>,
) -> Result<Json<TokenResponse>, ApiError> {
    let issued = state
        .tokens
        .refresh(&request.refresh_token.into_secret(), ip)
        .await
        .map_err(|error| match error {
            RefreshError::Rejected => ApiError::Unauthorized,
            RefreshError::Store(error) => ApiError::from(error),
            other @ (RefreshError::Token(_) | RefreshError::Audit(_)) => ApiError::internal(other),
        })?;
    Ok(Json(tokens(&issued)?))
}

/// `POST /api/v1/auth/logout`: ends this session.
#[utoipa::path(
    post,
    path = "/api/v1/auth/logout",
    tag = "auth",
    responses(
        (status = 204, description = "The session is over."),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn logout(
    user: AuthUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
) -> Result<StatusCode, ApiError> {
    let owner = user.actor().id;
    user.require(
        Permission::UsePersonal,
        &ResourceContext::Personal { owner },
    )?;
    state
        .tokens
        .logout(owner, user.session_id(), ip)
        .await
        .map_err(|error| match error {
            LogoutError::Store(error) => ApiError::from(error),
            other @ LogoutError::Audit(_) => ApiError::internal(other),
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/auth/step-up`: the password again; step-up actions are
/// allowed in this session for 5 minutes.
#[utoipa::path(
    post,
    path = "/api/v1/auth/step-up",
    tag = "auth",
    request_body = StepUpRequest,
    responses(
        (status = 204, description = "Step-up actions are allowed for 5 minutes."),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "The password is wrong.", body = ErrorResponse),
        (status = 422, description = "The password is too long.", body = ErrorResponse),
        (
            status = 429,
            description = "Too many attempts from this address or for this user, or the username is locked.",
            body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again.")),
        ),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn step_up(
    user: AuthUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidJson(request): ValidJson<StepUpRequest>,
) -> Result<StatusCode, ApiError> {
    let owner = user.actor().id;
    user.require(
        Permission::UsePersonal,
        &ResourceContext::Personal { owner },
    )?;
    state
        .login
        .step_up(
            owner,
            user.session_id(),
            &request.password.into_secret(),
            ip,
        )
        .await
        .map_err(|error| match error {
            StepUpError::SessionGone => ApiError::Unauthorized,
            StepUpError::Refused => ApiError::Forbidden,
            StepUpError::Lockout(error) => ApiError::from(error),
            StepUpError::Verify(VerifyError::Busy) => ApiError::Busy,
            StepUpError::Record(RecordFailureError::Store(error)) | StepUpError::Store(error) => {
                ApiError::from(error)
            }
            other @ (StepUpError::Verify(_) | StepUpError::Record(_) | StepUpError::Audit(_)) => {
                ApiError::internal(other)
            }
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/auth/register`: an account from an invite; no tokens.
#[utoipa::path(
    post,
    path = "/api/v1/auth/register",
    tag = "auth",
    request_body = RegisterRequest,
    responses(
        (status = 201, description = "The new user. Log in next; the first login sets up 2FA.", body = RegisterResponse),
        (status = 400, description = "A body of the wrong shape.", body = ErrorResponse),
        (status = 422, description = "The invite can't be used, the username is taken or invalid, or the password breaks its rules or is too weak (with hints).", body = ErrorResponse),
        (
            status = 429,
            description = "Too many registrations from this address.",
            body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again.")),
        ),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn register(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidJson(request): ValidJson<RegisterRequest>,
) -> Result<(StatusCode, Json<RegisterResponse>), ApiError> {
    // The rules already hold: garde checked them.
    let name = Username::try_from(request.username.expose()).map_err(ApiError::internal)?;
    let password = NewPassword::try_from(request.password.into_secret())
        .map_err(|_| ApiError::internal("the rule already holds: garde checked it"))?;
    let registered = state
        .users
        .register(
            &state.invites,
            &request.invite.into_secret(),
            &name,
            &password,
            ip,
        )
        .await
        .map_err(|error| match error {
            RegisterError::BadInvite => {
                ApiError::Validation(vec![FieldError::new("invite", INVALID_INVITE)])
            }
            RegisterError::UsernameTaken => {
                ApiError::Validation(vec![FieldError::new("username", USERNAME_TAKEN)])
            }
            RegisterError::Password(error) => refused_password("password", error),
            RegisterError::Store(error) => ApiError::from(error),
            other @ (RegisterError::Mint(_) | RegisterError::Audit(_)) => ApiError::internal(other),
        })?;
    Ok((
        StatusCode::CREATED,
        Json(RegisterResponse {
            id: registered.id,
            username: username(&registered.username),
            role: UserRole::from(registered.role).into(),
        }),
    ))
}
