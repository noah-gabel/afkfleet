//! The trace layer: one span and one line per request (Plan.md P6.6,
//! ADR-0015).
//!
//! The span, `request` at `info`, carries the request's ID, its method and
//! its route. The inner layers and the handler run inside it, so every line
//! they log carries those fields, the panic hook's report included. When the
//! response is ready, one line, "request finished", names its `status`,
//! `latency_ms` and, for an error, the `code`
//! [`render_errors`](crate::http::error::render_errors) wrote. This is where
//! a client error is logged.
//!
//! Only the server's own text reaches the log:
//! - `route` is the matched route's template (`/api/v1/bots/{id}`), never
//!   the path the client sent, and `"<unmatched>"` when no route matched.
//!   The query string is never logged: it will carry the single-use
//!   WebSocket ticket (P11).
//! - `method` is one of the nine standard methods, or `"<other>"`.
//! - No header and no body is ever logged.
//!
//! The health routes (`/health/…`) log their line at `debug`, so a probe
//! every few seconds doesn't flood the log; a failed readiness check is
//! still logged by `render_errors`.

use axum::extract::{MatchedPath, Request};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use fleet_api_types::OpenEnum as _;
use fleet_core::id::RequestId;
use tokio::time::Instant;
use tracing::{Instrument as _, debug, field, info, info_span};

use crate::http::error::RenderedCode;

/// The route a request that matched none is logged under.
const UNMATCHED: &str = "<unmatched>";

/// How an extension method is logged.
const OTHER_METHOD: &str = "<other>";

/// The prefix of the routes whose line is logged at `debug`.
const QUIET_PREFIX: &str = "/health/";

/// The methods logged by name.
static STANDARD_METHODS: [Method; 9] = [
    Method::GET,
    Method::HEAD,
    Method::POST,
    Method::PUT,
    Method::DELETE,
    Method::CONNECT,
    Method::OPTIONS,
    Method::TRACE,
    Method::PATCH,
];

/// The middleware; see the [module docs](self).
pub(super) async fn trace(request: Request, next: Next) -> Response {
    let route = request.extensions().get::<MatchedPath>().cloned();
    let route = route.as_ref().map_or(UNMATCHED, MatchedPath::as_str);
    let request_id = request.extensions().get::<RequestId>().copied();
    let span = info_span!(
        "request",
        request_id = request_id.map(field::display),
        method = method_label(request.method()),
        route,
    );
    let started = Instant::now();
    let mut response = next.run(request).instrument(span.clone()).await;
    let latency_ms = milliseconds(started.elapsed());
    let code = response
        .extensions_mut()
        .remove::<RenderedCode>()
        .map(|RenderedCode(code)| code.as_str());
    let status = response.status().as_u16();
    span.in_scope(|| {
        if is_quiet(route) {
            debug!(status, code, latency_ms, "request finished");
        } else {
            info!(status, code, latency_ms, "request finished");
        }
    });
    response
}

/// The method's name if it's a standard one, otherwise `"<other>"`.
fn method_label(method: &Method) -> &'static str {
    STANDARD_METHODS
        .iter()
        .find(|standard| *standard == method)
        .map_or(OTHER_METHOD, Method::as_str)
}

/// Whether `route`'s line is logged at `debug`: the health routes.
fn is_quiet(route: &str) -> bool {
    route.starts_with(QUIET_PREFIX)
}

/// `elapsed` in milliseconds, to the microsecond.
fn milliseconds(elapsed: core::time::Duration) -> f64 {
    (elapsed.as_secs_f64() * 1_000_000.0).round() / 1_000.0
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use rstest::rstest;

    use super::*;

    #[test]
    fn every_standard_method_is_logged_by_its_name() {
        for method in &STANDARD_METHODS {
            assert_eq!(method_label(method), method.as_str());
        }
    }

    #[test]
    fn an_extension_method_is_other() {
        assert_eq!(
            method_label(&Method::from_bytes(b"BREW").unwrap()),
            "<other>"
        );
    }

    #[rstest]
    #[case::live("/health/live", true)]
    #[case::ready("/health/ready", true)]
    #[case::health_itself("/health", false)]
    #[case::unmatched(UNMATCHED, false)]
    #[case::api("/api/v1/bots/{id}", false)]
    fn only_the_health_routes_are_quiet(#[case] route: &str, #[case] quiet: bool) {
        assert_eq!(is_quiet(route), quiet);
    }

    #[rstest]
    #[case::zero(Duration::ZERO, 0.0)]
    #[case::a_microsecond(Duration::from_micros(1), 0.001)]
    #[case::under_a_microsecond_rounds(Duration::from_nanos(1_499), 0.001)]
    #[case::two_seconds(Duration::from_secs(2), 2_000.0)]
    fn latency_is_milliseconds_to_the_microsecond(#[case] elapsed: Duration, #[case] ms: f64) {
        assert!(
            (milliseconds(elapsed) - ms).abs() < f64::EPSILON,
            "{}",
            milliseconds(elapsed)
        );
    }
}
