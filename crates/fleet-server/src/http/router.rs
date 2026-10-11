//! The server's router (Plan.md P6.7, Appendix B): every route, behind the
//! middleware stack.
//!
//! | Route | Handler |
//! |---|---|
//! | `GET /health/live` | always 200: the process runs and answers |
//! | `GET /health/ready` | 200 when the database answers ([`HealthService::ready`]) |
//! | `GET /api/openapi.json` | the `OpenAPI` document, only in dev mode ([`openapi`]) |
//!
//! The health routes have no `/api/v1` prefix. Both answer `200` with an
//! empty body; a failed readiness check answers like any store failure:
//! `busy` (503) when the database is busy, `internal` (500) otherwise, with
//! nothing of the cause in the response. P7.11 registers them as public
//! routes.
//!
//! Every API route is registered through utoipa-axum's `OpenApiRouter`, so
//! it can't exist without its entry in the `OpenAPI` document. The document's
//! own route isn't listed in it.

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::handlers::health;
use super::middleware::{self, RequestIds};
use super::openapi::{self, ApiDocs, OPENAPI_PATH};
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
/// the body limit from `http` and request IDs from `ids`. `docs` says
/// whether `GET /api/openapi.json` exists: only in dev mode.
pub fn router(state: AppState, http: &HttpConfig, ids: RequestIds, docs: ApiDocs) -> Router {
    let (routes, document) = OpenApiRouter::with_openapi(openapi::base())
        .routes(routes!(health::live))
        .routes(routes!(health::ready))
        .with_state(state)
        .split_for_parts();
    let routes = match docs {
        ApiDocs::Served => {
            let document = Arc::new(document);
            routes.route(
                OPENAPI_PATH,
                get(move || core::future::ready(openapi::document(&document))),
            )
        }
        ApiDocs::Hidden => routes,
    };
    middleware::apply(routes, http, ids)
}
