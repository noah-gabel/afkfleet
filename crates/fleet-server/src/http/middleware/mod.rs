//! The middleware stack every request passes through (Plan.md P6.6, §7.6,
//! ADR-0015).
//!
//! [`apply`] adds the router's fallbacks and the stack to a set of routes.
//! The layers, outermost first, i.e. in the order a request meets them:
//!
//! | # | Layer | Why it sits here |
//! |---|---|---|
//! | 1 | sensitive headers | `Authorization`, `Proxy-Authorization` and `Cookie` are marked sensitive before anything else sees the request, and `Set-Cookie` on every response, so a `Debug` print shows `Sensitive` instead of the value |
//! | 2 | security headers | Set on every response, even those the layers below make themselves (a refused request, a timeout, a panic), replacing any value a handler set |
//! | 3 | request ID | [`request_id`]: the server mints every ID; a client's never survives |
//! | 4 | trace | One span and one line per request, with the ID; it logs no header and no body |
//! | 5 | [`render_errors`] | Inside the request-ID layer, so every error body names the ID, and inside the trace span, so its lines carry it too |
//! | 6 | catch-panic | A panicking handler answers with the fixed internal error, which `render_errors` renders |
//! | 7 | timeout | `[http] request_timeout_secs`; it answers 408, which `render_errors` renders |
//! | 8 | body limit | `[http] max_body_bytes`; it answers 413 when the `Content-Length` is too large, and fails a body read past the limit |
//!
//! Inside them, the router answers a path no route matches with
//! `not_found`, and a method the matched path doesn't take with
//! `method_not_allowed`, to which axum adds the `Allow` header.
//!
//! The security headers are `X-Content-Type-Options: nosniff`,
//! `Referrer-Policy: no-referrer`, `Cache-Control: no-store`,
//! `Content-Security-Policy: default-src 'none'; frame-ancestors 'none'` and
//! `X-Frame-Options: DENY`; Caddy adds HSTS. There is no CORS layer (§7.6).
//! Rate limiting arrives in P7.10.

use core::any::Any;

use axum::Router;
use axum::http::header::{
    AUTHORIZATION, CACHE_CONTROL, CONTENT_SECURITY_POLICY, COOKIE, PROXY_AUTHORIZATION,
    REFERRER_POLICY, SET_COOKIE, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::middleware::{from_fn, from_fn_with_state};
use axum::response::{IntoResponse as _, Response};
use tower::ServiceBuilder;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::sensitive_headers::{
    SetSensitiveRequestHeadersLayer, SetSensitiveResponseHeadersLayer,
};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;

use super::error::{ApiError, render_errors};
use crate::config::HttpConfig;

pub mod request_id;
mod trace;

pub use request_id::RequestIds;

/// The request headers marked sensitive: the credentials a client sends.
const SENSITIVE_REQUEST_HEADERS: [HeaderName; 3] = [AUTHORIZATION, PROXY_AUTHORIZATION, COOKIE];

/// The response headers marked sensitive: a session the server hands out.
const SENSITIVE_RESPONSE_HEADERS: [HeaderName; 1] = [SET_COOKIE];

/// The headers on every response (§7.6, ADR-0015).
const SECURITY_HEADERS: [(HeaderName, HeaderValue); 5] = [
    (X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
    (REFERRER_POLICY, HeaderValue::from_static("no-referrer")),
    (CACHE_CONTROL, HeaderValue::from_static("no-store")),
    (
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
    ),
    (X_FRAME_OPTIONS, HeaderValue::from_static("DENY")),
];

/// Adds the fallbacks and the middleware stack to `routes`, with the timeout
/// and the body limit from `http` and request IDs minted by `ids`; see the
/// [module docs](self). It's the last step of building a router: routes
/// added afterwards would bypass the stack.
pub fn apply(routes: Router, http: &HttpConfig, ids: RequestIds) -> Router {
    let [nosniff, referrer, cache, csp, frame] = SECURITY_HEADERS;
    routes
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(
            ServiceBuilder::new()
                .layer(SetSensitiveRequestHeadersLayer::new(
                    SENSITIVE_REQUEST_HEADERS,
                ))
                .layer(SetSensitiveResponseHeadersLayer::new(
                    SENSITIVE_RESPONSE_HEADERS,
                ))
                .layer(SetResponseHeaderLayer::overriding(nosniff.0, nosniff.1))
                .layer(SetResponseHeaderLayer::overriding(referrer.0, referrer.1))
                .layer(SetResponseHeaderLayer::overriding(cache.0, cache.1))
                .layer(SetResponseHeaderLayer::overriding(csp.0, csp.1))
                .layer(SetResponseHeaderLayer::overriding(frame.0, frame.1))
                .layer(from_fn_with_state(ids, request_id::set_request_id))
                .layer(from_fn(trace::trace))
                .layer(from_fn(render_errors))
                .layer(CatchPanicLayer::custom(panic_response))
                .layer(TimeoutLayer::with_status_code(
                    StatusCode::REQUEST_TIMEOUT,
                    http.request_timeout,
                ))
                .layer(RequestBodyLimitLayer::new(http.max_body_bytes)),
        )
}

/// The answer for a path no route matches.
async fn not_found() -> ApiError {
    ApiError::NotFound
}

/// The answer for a method a matched path doesn't take; axum adds `Allow`.
async fn method_not_allowed() -> ApiError {
    ApiError::MethodNotAllowed
}

/// A handler panicked; its source is a fixed text.
#[derive(Debug, thiserror::Error)]
#[error("a request handler panicked")]
struct HandlerPanicked;

/// The answer for a panic: the fixed internal error. The payload never
/// reaches the client or this error; fleet-startup's panic hook has already
/// logged it, cleaned, inside the request's span.
fn panic_response(payload: Box<dyn Any + Send + 'static>) -> Response {
    drop(payload);
    ApiError::internal(HandlerPanicked).into_response()
}
