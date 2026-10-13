//! The server's router (Plan.md P6.7, P7.11, Appendix B): every route,
//! registered through the route registry ([`super::routes`]), behind the
//! middleware stack.
//!
//! | Route | Access | Handler |
//! |---|---|---|
//! | `GET /health/live` | public | always 200: the process runs and answers |
//! | `GET /health/ready` | public | 200 when the database answers ([`HealthService::ready`]) |
//! | `POST /api/v1/auth/login` | public (Login) | the password step: an mfa or setup token, never real tokens |
//! | `POST /api/v1/auth/login/mfa` | public (Login) | the code step: the session's tokens |
//! | `POST /api/v1/auth/refresh` | public (Refresh) | a new token pair |
//! | `POST /api/v1/auth/logout` | user | ends this session |
//! | `POST /api/v1/auth/step-up` | user (Login) | the password again: step-up actions for 5 minutes |
//! | `POST /api/v1/auth/register` | public (Register) | an account from an invite; no tokens |
//! | `POST /api/v1/me/mfa/enroll` | setup token | a new TOTP secret, waiting for confirm |
//! | `POST /api/v1/me/mfa/confirm` | setup token | confirms it; answers the recovery codes |
//! | `POST /api/v1/me/mfa/reset` | user + step-up | a setup token that replaces the authenticator |
//! | `GET /api/v1/me` | user | the user's profile and the wrong codes since the previous login |
//! | `POST /api/v1/me/password` | user + step-up (Login) | a new password; every other session ends |
//! | `GET /api/v1/me/sessions` | user | one page of the user's live sessions |
//! | `DELETE /api/v1/me/sessions/{id}` | user | ends another of the user's sessions |
//! | `GET /api/v1/users` | user (Admin+) | one page of users |
//! | `PATCH /api/v1/users/{id}` | user + step-up (the Owner; Admins for a Member's `disabled`) | one change: a role or `disabled` |
//! | `POST /api/v1/invites` | user + step-up (Admin+; Admin invites the Owner) | a new invite and its code |
//! | `GET /api/v1/invites` | user (Admin+) | one page of invites |
//! | `DELETE /api/v1/invites/{id}` | user + step-up (as creating) | revokes a pending invite |
//! | `GET /api/openapi.json` | public | the `OpenAPI` document, only in dev mode ([`openapi`](super::openapi)) |
//!
//! The health routes have no `/api/v1` prefix. Both answer `200` with an
//! empty body; a failed readiness check answers like any store failure:
//! `busy` (503) when the database is busy, `internal` (500) otherwise, with
//! nothing of the cause in the response.
//!
//! Every API route is registered through the registry, so it can't exist
//! without its access, its rate-limit class and its entry in the `OpenAPI`
//! document. The document's own route isn't listed in it.

use axum::Router;
use utoipa_axum::routes;

use super::handlers::{auth, health, invites, me, mfa, users};
use super::middleware::RequestIds;
use super::openapi::ApiDocs;
use super::rate_limit::RateLimits;
use super::routes::{RateClass, Routes};
use crate::app::auth::TokenService;
use crate::app::health::HealthService;
use crate::app::invites::InviteService;
use crate::app::login::LoginService;
use crate::app::mfa::MfaService;
use crate::app::users::UserService;
use crate::config::HttpConfig;

/// What the handlers share: the services.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The readiness check.
    pub(crate) health: HealthService,
    /// Sessions and their tokens: `AuthUser` authenticates every authed
    /// request through it.
    pub(crate) tokens: TokenService,
    /// The request limits: `AuthUser` spends each authed request's per-user
    /// limit, and the router's layers the others.
    pub(crate) limits: RateLimits,
    /// The second factor, and the login's challenges, which `SetupToken`
    /// reads.
    pub(crate) mfa: MfaService,
    /// Logging in and stepping up.
    pub(crate) login: LoginService,
    /// The users: the admins' list and changes.
    pub(crate) users: UserService,
    /// The invites to register.
    pub(crate) invites: InviteService,
}

impl AppState {
    /// The state over the server's services and its request `limits`.
    #[must_use]
    pub fn new(
        health: HealthService,
        tokens: TokenService,
        limits: RateLimits,
        mfa: MfaService,
        login: LoginService,
        users: UserService,
        invites: InviteService,
    ) -> Self {
        Self {
            health,
            tokens,
            limits,
            mfa,
            login,
            users,
            invites,
        }
    }
}

/// Adds every route of the API to `routes`; see the [module docs](self).
#[must_use]
pub fn api_routes(routes: Routes) -> Routes {
    routes
        .public_route(RateClass::Standard, routes!(health::live))
        .public_route(RateClass::Standard, routes!(health::ready))
        .public_route(RateClass::Login, routes!(auth::login))
        .public_route(RateClass::Login, routes!(auth::login_mfa))
        .public_route(RateClass::Refresh, routes!(auth::refresh))
        .authed_route(RateClass::Standard, routes!(auth::logout))
        .authed_route(RateClass::Login, routes!(auth::step_up))
        .setup_route(RateClass::Standard, routes!(mfa::enroll))
        .setup_route(RateClass::Standard, routes!(mfa::confirm))
        .step_up_route(RateClass::Standard, routes!(mfa::reset))
        .public_route(RateClass::Register, routes!(auth::register))
        .authed_route(RateClass::Standard, routes!(me::me))
        .step_up_route(RateClass::Login, routes!(me::change_password))
        .authed_route(RateClass::Standard, routes!(me::list_sessions))
        .authed_route(RateClass::Standard, routes!(me::revoke_session))
        .authed_route(RateClass::Standard, routes!(users::list))
        .step_up_route(RateClass::Standard, routes!(users::patch))
        .step_up_route(RateClass::Standard, routes!(invites::create))
        .authed_route(RateClass::Standard, routes!(invites::list))
        .step_up_route(RateClass::Standard, routes!(invites::revoke))
}

/// Every route with `state`, behind the middleware stack with the timeout,
/// the body limit and the trusted proxies from `http`, request IDs from `ids`
/// and the limits `state` carries. `docs` says
/// whether `GET /api/openapi.json` exists: only in dev mode.
pub fn router(state: AppState, http: &HttpConfig, ids: RequestIds, docs: ApiDocs) -> Router {
    let routes = Routes::new(&state.limits);
    api_routes(routes).finish(state, http, ids, docs).0
}
