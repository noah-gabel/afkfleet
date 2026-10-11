//! [`SetupToken`]: a request to enroll a TOTP authenticator (Plan.md P7.7,
//! §7.2, ADR-0016).
//!
//! A handler that takes a `SetupToken` runs only for a request with exactly
//! one `Authorization: Bearer afk_st_…` header whose setup token is live:
//! from the password step of a user without 2FA, or from a replacement
//! started with step-up. Only enroll and confirm take one, so a setup token
//! works nowhere else, and an access or mfa token doesn't work here.
//!
//! - **Every refusal is the same 401**, as `AuthUser`'s: no header, more than
//!   one, another scheme, another kind of token, an unknown, expired or used
//!   one. The reason is one `debug` line; no token is ever logged.
//! - **Every request spends its user's rate limit**, right after the token
//!   is accepted, as an authed request does.
//! - **Authorizing:** [`SetupToken::require`] asks
//!   `fleet_core::authz::authorize` for the token's user, as `AuthUser`'s
//!   does.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use fleet_core::authz::{Actor, AuthzError, Permission, ResourceContext, authorize};
use secrecy::ExposeSecret as _;
use tracing::debug;

use super::bearer::bearer;
use crate::app::challenges::SetupGrant;
use crate::http::error::ApiError;
use crate::http::router::AppState;

/// A live setup token: who it's for, and what for.
#[derive(Debug, Clone, Copy)]
pub struct SetupToken {
    grant: SetupGrant,
}

impl SetupToken {
    /// The token's challenge, for the 2FA service.
    #[must_use]
    pub const fn grant(&self) -> &SetupGrant {
        &self.grant
    }

    /// Who asks: the token's user, with their role when it was issued.
    #[must_use]
    pub const fn actor(&self) -> Actor {
        Actor {
            id: self.grant.user.id,
            role: self.grant.user.role,
        }
    }

    /// Whether the token's user may do `permission` to `resource`, decided
    /// by `fleet_core::authz::authorize`.
    ///
    /// # Errors
    /// [`AuthzError`], as `authorize` returns it.
    pub fn require(
        &self,
        permission: Permission,
        resource: &ResourceContext,
    ) -> Result<(), AuthzError> {
        authorize(&self.actor(), permission, resource)
    }
}

impl FromRequestParts<AppState> for SetupToken {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let presented = bearer(parts, state).await.map_err(refuse)?;
        let grant = state
            .mfa
            .challenges()
            .setup(presented.expose_secret())
            .ok_or_else(|| refuse("not a live setup token"))?;
        state
            .limits
            .check_user(grant.user.id)
            .map_err(ApiError::from)?;
        Ok(Self { grant })
    }
}

/// The 401 for a refused request, with its one `debug` line.
fn refuse(reason: &'static str) -> ApiError {
    debug!(reason, "a request's setup token was refused");
    ApiError::Unauthorized
}
