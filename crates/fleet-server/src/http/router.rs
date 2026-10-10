//! The server's router (Plan.md P6.7, Appendix B): every route, behind the
//! middleware stack.
//!
//! | Route | Handler |
//! |---|---|
//! | `GET /health/live` | always 200: the process runs and answers |
//! | `GET /health/ready` | 200 when the database answers ([`HealthService::ready`]) |
//!
//! The health routes have no `/api/v1` prefix. Both answer `200` with an
//! empty body; a failed readiness check answers like any store failure:
//! `busy` (503) when the database is busy, `internal` (500) otherwise, with
//! nothing of the cause in the response. P7.11 registers them as public
//! routes.

use axum::Router;
use axum::routing::get;

use super::handlers::health;
use super::middleware::{self, RequestIds};
use crate::app::health::HealthService;
use crate::config::HttpConfig;

/// What the handlers share: the services.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The readiness check.
    pub(crate) health: HealthService,
}

impl AppState {
    /// The state over the server's services.
    #[must_use]
    pub fn new(health: HealthService) -> Self {
        Self { health }
    }
}

/// Every route with `state`, behind the middleware stack with the timeout and
/// the body limit from `http` and request IDs from `ids`.
pub fn router(state: AppState, http: &HttpConfig, ids: RequestIds) -> Router {
    let routes = Router::new()
        .route(health::LIVE, get(health::live))
        .route(health::READY, get(health::ready))
        .with_state(state);
    middleware::apply(routes, http, ids)
}
