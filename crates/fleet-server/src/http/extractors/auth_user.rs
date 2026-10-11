//! [`AuthUser`]: who a request comes from (Plan.md P7.8, §7.2, ADR-0006,
//! ADR-0010, ADR-0016).
//!
//! A handler that takes an `AuthUser` runs only for a request with exactly
//! one `Authorization: Bearer <access token>` header whose session is live
//! and whose user is enabled. The session and the user are looked up on
//! every request, so a logout, a revocation, a disabled user or a role
//! change counts on the very next one.
//!
//! - **Every refusal is the same 401,** `ApiError::Unauthorized` with a bare
//!   `WWW-Authenticate: Bearer`: no header, more than one, another scheme, a
//!   malformed, unknown or expired token, a revoked session, a disabled
//!   user. Nothing tells a client whether a token existed. The reason is one
//!   `debug` line, with the session's and the user's IDs where they're
//!   known; no token, hash or header value is ever logged.
//! - **A database failure isn't a refusal:** busy is 503, anything else 500.
//! - **Every authed request spends its user's rate limit** (§7.4), right after
//!   authentication: past it, 429 with `Retry-After`.
//! - **Authorizing:** [`AuthUser::require`] asks `fleet_core::authz::authorize`
//!   for the actor, so every authed handler decides through the one
//!   function. Step-up is checked by the step-up actions themselves, with
//!   [`AuthUser::stepped_up_at`] and `fleet_core::auth::is_stepped_up`.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use chrono::{DateTime, Utc};
use fleet_core::authz::{Actor, AuthzError, Permission, ResourceContext, authorize};
use fleet_core::id::SessionId;
use tracing::debug;

use super::bearer::bearer;
use crate::app::auth::{AuthenticateError, RejectedAccess};
use crate::http::error::ApiError;
use crate::http::router::AppState;

/// The authenticated user of a request: the actor (their ID and current
/// role), their session, and when they last entered their password again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthUser {
    actor: Actor,
    session_id: SessionId,
    stepped_up_at: Option<DateTime<Utc>>,
}

impl AuthUser {
    /// Who asks: the user's ID and their role as it is now.
    #[must_use]
    pub const fn actor(&self) -> Actor {
        self.actor
    }

    /// The session the request's access token belongs to.
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// When the user last entered their password again in this session, for
    /// the step-up actions (§7.2); `None` if never.
    #[must_use]
    pub const fn stepped_up_at(&self) -> Option<DateTime<Utc>> {
        self.stepped_up_at
    }

    /// Whether the actor may do `permission` to `resource`, decided by
    /// `fleet_core::authz::authorize`. A handler returns the error with `?`:
    /// 404 when the actor may not even see the resource, 403 when they may
    /// see it but not do this, 500 for a permission checked against the
    /// wrong kind of resource.
    ///
    /// # Errors
    /// [`AuthzError`], as `authorize` returns it.
    pub fn require(
        &self,
        permission: Permission,
        resource: &ResourceContext,
    ) -> Result<(), AuthzError> {
        authorize(&self.actor, permission, resource)
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let presented = bearer(parts, state)
            .await
            .map_err(|reason| refuse(&RejectedAccess::reason(reason)))?;
        match state.tokens.authenticate(&presented).await {
            Ok(authenticated) => {
                state
                    .limits
                    .check_user(authenticated.actor.id)
                    .map_err(ApiError::from)?;
                Ok(Self {
                    actor: authenticated.actor,
                    session_id: authenticated.session_id,
                    stepped_up_at: authenticated.stepped_up_at,
                })
            }
            Err(AuthenticateError::Rejected(rejected)) => Err(refuse(&rejected)),
            Err(AuthenticateError::Store(error)) => Err(ApiError::from(error)),
        }
    }
}

/// The 401 for a refused request, with its one `debug` line.
fn refuse(rejected: &RejectedAccess) -> ApiError {
    debug!(
        reason = rejected.reason,
        session_id = rejected.session_id.map(tracing::field::display),
        user_id = rejected.user_id.map(tracing::field::display),
        "a request's credentials were refused"
    );
    ApiError::Unauthorized
}
