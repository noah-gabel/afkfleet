//! [`SteppedUpUser`]: an authenticated user who entered their password
//! again within 5 minutes (Plan.md §7.2, P7.6, ADR-0016).
//!
//! A step-up action's handler takes it instead of `AuthUser`, and the route
//! registers with `step_up_route`, so the route-coverage test can call it
//! with a live session but no fresh stamp and expect 403. A request without
//! valid credentials gets `AuthUser`'s 401 first; a session without a stamp
//! of at most 5 minutes (`fleet_core::auth::is_stepped_up`) gets 403
//! `step_up_required`, and the app sends `POST /auth/step-up` and retries.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use tracing::debug;

use super::AuthUser;
use crate::http::error::ApiError;
use crate::http::router::AppState;

/// An [`AuthUser`] whose session stepped up within the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SteppedUpUser(AuthUser);

impl SteppedUpUser {
    /// The authenticated user.
    #[must_use]
    pub const fn user(&self) -> &AuthUser {
        &self.0
    }
}

impl FromRequestParts<AppState> for SteppedUpUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if !state.tokens.is_stepped_up(user.stepped_up_at()) {
            debug!(
                session_id = %user.session_id(),
                user_id = %user.actor().id,
                "a step-up action was refused without a fresh step-up"
            );
            return Err(ApiError::StepUpRequired);
        }
        Ok(Self(user))
    }
}
