//! The server's router (Plan.md P6.7, P7.11, Appendix B): every route,
//! registered through the route registry ([`super::routes`]), behind the
//! middleware stack.
//!
//! | Route | Access | Handler |
//! |---|---|---|
//! | `GET /health/live` | public | always 200: the process runs and answers |
//! | `GET /health/ready` | public | 200 when the database answers ([`HealthService::ready`]) |
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

use super::handlers::health;
use super::middleware::RequestIds;
use super::openapi::ApiDocs;
use super::rate_limit::RateLimits;
use super::routes::{RateClass, Routes};
use crate::app::auth::TokenService;
use crate::app::health::HealthService;
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
}

impl AppState {
    /// The state over the server's services and its request `limits`.
    #[must_use]
    pub fn new(health: HealthService, tokens: TokenService, limits: RateLimits) -> Self {
        Self {
            health,
            tokens,
            limits,
        }
    }
}

/// Adds every route of the API to `routes`; see the [module docs](self).
#[must_use]
pub fn api_routes(routes: Routes) -> Routes {
    routes
        .public_route(RateClass::Standard, routes!(health::live))
        .public_route(RateClass::Standard, routes!(health::ready))
}

/// Every route with `state`, behind the middleware stack with the timeout,
/// the body limit and the trusted proxies from `http`, request IDs from `ids`
/// and the limits `state` carries. `docs` says
/// whether `GET /api/openapi.json` exists: only in dev mode.
pub fn router(state: AppState, http: &HttpConfig, ids: RequestIds, docs: ApiDocs) -> Router {
    let routes = Routes::new(&state.limits);
    api_routes(routes).finish(state, http, ids, docs).0
}
