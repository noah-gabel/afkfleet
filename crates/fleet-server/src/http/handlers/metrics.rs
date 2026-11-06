//! `GET /metrics` (Plan.md P12.4; ADR-0022, S9): the server's key metrics
//! in Prometheus' text format, on `[metrics] bind` only, never on the API's
//! port.
//!
//! It takes no input and needs no credentials: the port is internal, never
//! published beyond the host nor routed through a proxy (U1, U5), and what
//! it shows is counts under fixed labels, never a name, an address, an ID, a
//! token or chat (S11). The router adds the registry's render as an
//! extension (`router::metrics_router`).

use axum::Extension;
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use fleet_api_types::ErrorResponse;

use crate::metrics::MetricsRender;

/// The content type of Prometheus' text format.
pub(crate) const TEXT_FORMAT: &str = "text/plain; version=0.0.4; charset=utf-8";

/// `GET /metrics`: every series, now.
#[utoipa::path(
    get,
    path = "/metrics",
    tag = "metrics",
    responses(
        (status = 200, description = "Every series, in Prometheus' text format.", content_type = "text/plain"),
        (status = "default", description = "Any other answer the server gives, in the error envelope.", body = ErrorResponse),
    ),
)]
pub(crate) async fn render(Extension(render): Extension<MetricsRender>) -> impl IntoResponse {
    ([(CONTENT_TYPE, TEXT_FORMAT)], render.text())
}
