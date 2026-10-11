//! The 2FA routes (Plan.md P7.7, §7.2, Appendix B, ADR-0016):
//! `POST /api/v1/me/mfa/enroll`, `/confirm` and `/reset`.
//!
//! Enroll and confirm take only a setup token (`SetupToken`); reset takes a
//! session that stepped up within 5 minutes (`SteppedUpUser`) and answers
//! with a setup token that replaces the authenticator once confirmed. Every
//! secret in a response is a `SecretText`, and every response is `no-store`
//! (the security headers' `Cache-Control`).

use axum::Json;
use axum::extract::State;
use fleet_api_types::mfa::{
    MfaConfirmRequest, MfaConfirmResponse, MfaEnrollResponse, MfaResetResponse,
};
use fleet_api_types::{ErrorResponse, FieldError, SecretText};
use fleet_core::auth::SecondFactor;
use fleet_core::authz::{Permission, ResourceContext};
use secrecy::{ExposeSecret as _, SecretString};

use crate::app::mfa::{ConfirmError, EnrollError, ReplaceError};
use crate::http::error::ApiError;
use crate::http::extractors::{SetupToken, SteppedUpUser, ValidJson};
use crate::http::rate_limit::ClientIp;
use crate::http::router::AppState;

/// The message of a confirm code that doesn't match the new authenticator.
const WRONG_CODE: &str = "doesn't match the authenticator app; enter its current code";

/// A secret the service made, as a response field.
fn text<const MAX: usize>(secret: &SecretString) -> SecretText<MAX> {
    SecretText::new(secret.expose_secret().to_owned())
}

/// `POST /api/v1/me/mfa/enroll`: a new TOTP secret for the authenticator
/// app; each call replaces the pending one.
#[utoipa::path(
    post,
    path = "/api/v1/me/mfa/enroll",
    tag = "mfa",
    responses(
        (status = 200, description = "A new secret, waiting for confirm.", body = MfaEnrollResponse),
        (status = 401, description = "No live setup token.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn enroll(
    setup: SetupToken,
    State(state): State<AppState>,
) -> Result<Json<MfaEnrollResponse>, ApiError> {
    let owner = setup.actor().id;
    setup.require(
        Permission::UsePersonal,
        &ResourceContext::Personal { owner },
    )?;
    let enrollment = state
        .mfa
        .enroll(setup.grant())
        .await
        .map_err(|error| match error {
            EnrollError::Gone => ApiError::Unauthorized,
            EnrollError::Store(error) => ApiError::from(error),
            other @ (EnrollError::Random(_) | EnrollError::Uri(_) | EnrollError::Qr(_)) => {
                ApiError::internal(other)
            }
        })?;
    Ok(Json(MfaEnrollResponse {
        otpauth_uri: text(&enrollment.otpauth_uri),
        qr_svg: text(&enrollment.qr_svg),
        secret: text(&enrollment.secret),
    }))
}

/// `POST /api/v1/me/mfa/confirm`: the new authenticator's first code; the
/// answer holds the recovery codes, shown once.
#[utoipa::path(
    post,
    path = "/api/v1/me/mfa/confirm",
    tag = "mfa",
    request_body = MfaConfirmRequest,
    responses(
        (status = 200, description = "The authenticator is confirmed; its recovery codes, shown once.", body = MfaConfirmResponse),
        (status = 401, description = "No live setup token.", body = ErrorResponse),
        (status = 409, description = "Enroll first, or 2FA is already set up.", body = ErrorResponse),
        (status = 422, description = "The code isn't 6 digits, or doesn't match.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn confirm(
    setup: SetupToken,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ValidJson(request): ValidJson<MfaConfirmRequest>,
) -> Result<Json<MfaConfirmResponse>, ApiError> {
    let owner = setup.actor().id;
    setup.require(
        Permission::UsePersonal,
        &ResourceContext::Personal { owner },
    )?;
    let Some(SecondFactor::Totp(code)) = SecondFactor::parse(request.code.expose()) else {
        return Err(wrong_code());
    };
    let codes = state
        .mfa
        .confirm(setup.grant(), &code, ip)
        .await
        .map_err(|error| match error {
            ConfirmError::NotEnrolled | ConfirmError::AlreadyEnrolled => ApiError::Conflict,
            ConfirmError::WrongCode => wrong_code(),
            ConfirmError::Gone => ApiError::Unauthorized,
            ConfirmError::Store(error) => ApiError::from(error),
            other @ (ConfirmError::Random(_) | ConfirmError::Vault(_) | ConfirmError::Audit(_)) => {
                ApiError::internal(other)
            }
        })?;
    Ok(Json(MfaConfirmResponse {
        recovery_codes: codes.iter().map(text).collect(),
    }))
}

/// The 422 of a confirm code that doesn't match.
fn wrong_code() -> ApiError {
    ApiError::Validation(vec![FieldError::new("code", WRONG_CODE)])
}

/// `POST /api/v1/me/mfa/reset`: a setup token that replaces the
/// authenticator once confirmed; until then, the current one keeps
/// working. Needs a step-up.
#[utoipa::path(
    post,
    path = "/api/v1/me/mfa/reset",
    tag = "mfa",
    responses(
        (status = 200, description = "A setup token for enroll and confirm.", body = MfaResetResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`.", body = ErrorResponse),
        (status = 409, description = "The user has no 2FA to replace.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn reset(
    user: SteppedUpUser,
    State(state): State<AppState>,
) -> Result<Json<MfaResetResponse>, ApiError> {
    let user = user.user();
    let owner = user.actor().id;
    user.require(
        Permission::UsePersonal,
        &ResourceContext::Personal { owner },
    )?;
    let issued = state
        .mfa
        .start_replace(owner, user.session_id())
        .await
        .map_err(|error| match error {
            ReplaceError::NotEnrolled => ApiError::Conflict,
            ReplaceError::Gone => ApiError::Unauthorized,
            ReplaceError::Store(error) => ApiError::from(error),
            other @ ReplaceError::Token(_) => ApiError::internal(other),
        })?;
    Ok(Json(MfaResetResponse {
        setup_token: text(&issued.token),
        expires_in: u32::try_from(issued.expires_in).map_err(ApiError::internal)?,
    }))
}
