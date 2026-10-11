//! The `OpenAPI` document (Plan.md P6.10, ADR-0015), at
//! `GET /api/openapi.json`, which exists only in dev mode.
//!
//! - **Paths:** every API route is registered through utoipa-axum's
//!   `OpenApiRouter` with `#[utoipa::path]` on its handler, so a route can't
//!   exist without its entry. Each operation lists its own statuses, and a
//!   `default` response with the error envelope covers every other status the
//!   stack can answer (404, 405, 408, 413, …). The document doesn't list
//!   itself.
//! - **Schemas** come from fleet-api-types' [`components`], whose test keeps
//!   them equal to the TypeScript export.
//! - **Info:** the title "afkfleet", the crate's version and its license;
//!   no contact and no servers.
//!
//! Outside dev mode the route doesn't exist, so the path answers like any
//! unknown one. The document is written for each request: it's small, and
//! only developers ask for it.
//!
//! [`components`]: fleet_api_types::openapi::components

use axum::http::HeaderValue;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse as _, Response};
use utoipa::OpenApi;

use super::error::ApiError;

/// Where dev mode serves the document.
pub const OPENAPI_PATH: &str = "/api/openapi.json";

/// Whether the router serves the `OpenAPI` document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiDocs {
    /// `GET /api/openapi.json` answers with the document.
    Served,
    /// The path doesn't exist.
    Hidden,
}

impl ApiDocs {
    /// Served in dev mode, hidden otherwise (P6.10).
    #[must_use]
    pub const fn for_dev_mode(dev_mode: bool) -> Self {
        if dev_mode { Self::Served } else { Self::Hidden }
    }
}

/// The document's fixed part; the routes add the paths.
#[derive(OpenApi)]
#[openapi(info(
    title = "afkfleet",
    description = "The afkfleet server's HTTP API. Served only in dev mode.",
    license(name = "GPL-3.0-or-later", identifier = "GPL-3.0-or-later"),
))]
struct ApiDoc;

/// The document without paths: the info and every schema of the API.
pub(crate) fn base() -> utoipa::openapi::OpenApi {
    let mut document = ApiDoc::openapi();
    document
        .components
        .get_or_insert_with(Default::default)
        .schemas
        .extend(fleet_api_types::openapi::components());
    document
}

/// `GET /api/openapi.json`: the document as JSON.
pub(crate) fn document(document: &utoipa::openapi::OpenApi) -> Result<Response, ApiError> {
    let body = serde_json::to_vec(document).map_err(ApiError::internal)?;
    Ok((
        [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
        body,
    )
        .into_response())
}
