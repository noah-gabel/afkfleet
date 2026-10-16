//! The agents' routes (Plan.md P10.3, Appendix B, ADR-0019): for now
//! `POST /api/v1/agents/enrollment-tokens`.
//!
//! - **Creating an enrollment token** is the Owner's and needs a step-up
//!   (`SteppedUpUser`, the handler's first extractor), like the admin writes
//!   of §7.2. It's authorized here and again in the service's transaction.
//!   The route takes no input; the token lives 15 minutes, and at most 20
//!   are pending. The bundle (the token and the CA's fingerprint) is in this
//!   one answer.
//! - **While the control plane is off** (`[grpc]` isn't configured), the
//!   route answers 404 like an unknown one, after the step-up check, as
//!   `POST /accounts/offline` does while it's switched off.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fleet_api_types::agents::CreatedEnrollmentTokenResponse;
use fleet_api_types::{BoundedText, ErrorResponse};
use fleet_core::authz::{Permission, ResourceContext};

use super::{acting, secret_text, timestamp};
use crate::app::enrollment::{CreateTokenError, Creator};
use crate::http::error::ApiError;
use crate::http::extractors::SteppedUpUser;
use crate::http::rate_limit::ClientIp;
use crate::http::router::AppState;

/// `POST /api/v1/agents/enrollment-tokens`: a new enrollment token, in its
/// bundle, shown this once. Needs a step-up.
#[utoipa::path(
    post,
    path = "/api/v1/agents/enrollment-tokens",
    operation_id = "create_enrollment_token",
    tag = "agents",
    responses(
        (status = 201, description = "The token, in its bundle: pipe it into `afkfleet-agent enroll` within 15 minutes.", body = CreatedEnrollmentTokenResponse),
        (status = 401, description = "No valid access token.", body = ErrorResponse),
        (status = 403, description = "Step up first: `POST /auth/step-up`; or only the Owner creates enrollment tokens.", body = ErrorResponse),
        (status = 404, description = "The server's control plane is off: `[grpc]` isn't configured.", body = ErrorResponse),
        (status = 409, description = "20 enrollment tokens are pending already.", body = ErrorResponse),
        (status = 429, description = "Too many requests.", body = ErrorResponse,
            headers(("Retry-After" = u32, description = "Seconds to wait before trying again."))),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn create_enrollment_token(
    user: SteppedUpUser,
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
) -> Result<(StatusCode, Json<CreatedEnrollmentTokenResponse>), ApiError> {
    let Some(enrollment) = state.enrollment_tokens.as_ref() else {
        return Err(ApiError::NotFound);
    };
    let user = user.user();
    user.require(Permission::EnrollAgent, &ResourceContext::Global)?;
    let created = enrollment
        .create(Creator::Owner(&acting(user, ip)))
        .await
        .map_err(|error| match error {
            CreateTokenError::SessionGone => ApiError::Unauthorized,
            CreateTokenError::Denied => ApiError::Forbidden,
            CreateTokenError::CapReached => ApiError::Conflict,
            CreateTokenError::Store(error) => ApiError::from(error),
            other @ CreateTokenError::Internal(_) => ApiError::internal(other),
        })?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedEnrollmentTokenResponse {
            id: created.id,
            bundle: secret_text(&created.bundle.to_text()),
            ca_fingerprint: BoundedText::new(&created.bundle.ca_fingerprint().to_string()),
            expires_at: timestamp(created.expires_at)?,
        }),
    ))
}
