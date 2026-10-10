//! [`ApiError`]: how every failed request answers (Plan.md P6.5, P6.6,
//! ADR-0015).
//!
//! # Rendering
//! A handler returns `Result<_, ApiError>`. `IntoResponse` can't see the
//! request, so it doesn't know the request ID: [`ApiError::into_response`]
//! sets the status and the headers (`Retry-After`, `WWW-Authenticate`) and
//! puts a private marker into the response's extensions, with an empty
//! body. [`render_errors`], a middleware that runs inside the request-ID
//! layer ([`middleware`](super::middleware)), reads the ID from the
//! request's [`REQUEST_ID_HEADER`], takes the marker, logs, and writes the
//! JSON body: an [`ErrorResponse`]. The marker never reaches the wire.
//!
//! # The safety net
//! Some errors don't come from an `ApiError`: tower-http's timeout (408) and
//! body limit (413), and axum's own 413 when a handler reads past the limit.
//! [`render_errors`] gives every 4xx or 5xx response without a marker the
//! same envelope, so no error reaches a client without it:
//! - The code comes from the status. A status without a code of its own
//!   keeps its status and says `bad_request` (4xx) or `internal` (5xx); a
//!   bare 422 says `validation_failed` with no fields.
//! - The old body is replaced, and the headers that describe it
//!   (`Content-Length`, `Content-Encoding`) are removed. Every other header
//!   stays, such as axum's `Allow` and a rate limiter's `Retry-After`.
//! - A 401 gets `WWW-Authenticate: Bearer` and a 503 `Retry-After: 1` when
//!   they're missing, as an `ApiError` would send them.
//!
//! A success, or anything else that isn't an error, passes unchanged.
//!
//! # What a client sees
//! Only the code's fixed message ([`ErrorCode::message`]), the request ID
//! and, for `validation_failed`, the broken rules. Never an internal
//! error's text, nor the body a bare error status came with.
//!
//! # Logging
//! - `internal`: at `error`, with `request_id` and the whole source chain
//!   (`the database failed: disk I/O error`), cleaned by fleet-core's
//!   `sanitize_untrusted` and cut at 1024 characters.
//! - `busy`: at `warn`, with `request_id`.
//! - Every other code: not here; the trace layer logs each request's status
//!   and code, which this middleware leaves in the response's extensions for
//!   it, never on the wire.
//! - A marker without a request ID is a wiring bug: the body says
//!   `"unknown"` and it's logged at `error`.
//! - A bare error status is a wiring bug too, logged at `error` with its
//!   status: its source should return an `ApiError`. Only the timeout's 408
//!   and the body limit's 413 come bare by design and aren't logged.

use core::time::Duration;
use std::error::Error;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::http::header::{
    CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER, WWW_AUTHENTICATE,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use fleet_api_types::error::REQUEST_ID_MAX_CHARS;
use fleet_api_types::{
    BoundedText, ErrorBody, ErrorCode, ErrorResponse, FieldError, OpenEnum as _,
};
use fleet_core::authz::AuthzError;
use fleet_core::text::sanitize_untrusted;
use serde::Serialize;
use tracing::{error, warn};

use crate::ports::store::StoreError;

/// The header that carries the request's ID. The request-ID layer
/// ([`middleware`](super::middleware)) sets it on every request and every
/// response, always with a server-generated ID.
pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// How long a `busy` response asks the client to wait: a busy database
/// clears well within the pools' 5 s acquire timeout.
pub const BUSY_RETRY_AFTER: Duration = Duration::from_secs(1);

/// The most characters of an internal error's source chain in the log.
const CHAIN_MAX_CHARS: usize = 1024;

/// The request ID a body names when the request has none.
const UNKNOWN_REQUEST_ID: &str = "unknown";

/// The body sent if the real one can't be written, which can't happen for
/// these types: a fixed `internal` error. A test checks that it equals the
/// body serde would write.
const FALLBACK_BODY: &str = r#"{"error":{"code":"internal","message":"Something went wrong on the server. Quote the request ID when reporting it.","request_id":"unknown"}}"#;

/// Why a request failed, one variant per [`ErrorCode`]. Handlers return it;
/// [`render_errors`] turns it into the error envelope.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The request couldn't be read (400).
    #[error("the request couldn't be read")]
    BadRequest,
    /// No valid credentials came with the request (401).
    #[error("no valid credentials came with the request")]
    Unauthorized,
    /// The caller may see the resource, but not do this (403).
    #[error("the caller may not do this")]
    Forbidden,
    /// The resource doesn't exist, or the caller may not see it (404).
    #[error("the resource doesn't exist")]
    NotFound,
    /// The path doesn't take this method (405).
    #[error("the path doesn't take this method")]
    MethodNotAllowed,
    /// The request took too long (408).
    #[error("the request took too long")]
    Timeout,
    /// The request conflicts with the current state (409).
    #[error("the request conflicts with the current state")]
    Conflict,
    /// The request body is too large (413).
    #[error("the request body is too large")]
    PayloadTooLarge,
    /// The request body isn't JSON (415).
    #[error("the request body isn't JSON")]
    UnsupportedMediaType,
    /// Fields broke their rules (422).
    #[error("{} field rules were broken", .0.len())]
    Validation(Vec<FieldError>),
    /// Too many requests; the client may retry after `retry_after` (429).
    #[error("too many requests")]
    RateLimited {
        /// When the client may try again; sent in whole seconds, rounded up.
        retry_after: Duration,
    },
    /// The server failed (500). The source is logged; the client sees only
    /// the fixed message.
    #[error("the server failed")]
    Internal(#[source] Box<dyn Error + Send + Sync>),
    /// The server is busy, e.g. the database (503). The client may retry
    /// after [`BUSY_RETRY_AFTER`].
    #[error("the server is busy")]
    Busy,
}

impl ApiError {
    /// An internal error caused by `source`, which is logged and never
    /// shown to the client.
    pub fn internal(source: impl Into<Box<dyn Error + Send + Sync>>) -> Self {
        Self::Internal(source.into())
    }

    /// The error's code.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::BadRequest => ErrorCode::BadRequest,
            Self::Unauthorized => ErrorCode::Unauthorized,
            Self::Forbidden => ErrorCode::Forbidden,
            Self::NotFound => ErrorCode::NotFound,
            Self::MethodNotAllowed => ErrorCode::MethodNotAllowed,
            Self::Timeout => ErrorCode::Timeout,
            Self::Conflict => ErrorCode::Conflict,
            Self::PayloadTooLarge => ErrorCode::PayloadTooLarge,
            Self::UnsupportedMediaType => ErrorCode::UnsupportedMediaType,
            Self::Validation(_) => ErrorCode::ValidationFailed,
            Self::RateLimited { .. } => ErrorCode::RateLimited,
            Self::Internal(_) => ErrorCode::Internal,
            Self::Busy => ErrorCode::Busy,
        }
    }

    /// The error's HTTP status.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        status(self.code())
    }
}

/// Each code's HTTP status.
#[must_use]
pub const fn status(code: ErrorCode) -> StatusCode {
    match code {
        ErrorCode::BadRequest => StatusCode::BAD_REQUEST,
        ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
        ErrorCode::Forbidden => StatusCode::FORBIDDEN,
        ErrorCode::NotFound => StatusCode::NOT_FOUND,
        ErrorCode::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
        ErrorCode::Timeout => StatusCode::REQUEST_TIMEOUT,
        ErrorCode::Conflict => StatusCode::CONFLICT,
        ErrorCode::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        ErrorCode::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ErrorCode::ValidationFailed => StatusCode::UNPROCESSABLE_ENTITY,
        ErrorCode::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        ErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        ErrorCode::Busy => StatusCode::SERVICE_UNAVAILABLE,
    }
}

/// `busy` for a busy database; every other store error is internal.
impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Busy => Self::Busy,
            other @ (StoreError::Corrupt { .. }
            | StoreError::Unstorable { .. }
            | StoreError::Backend(_)) => Self::internal(other),
        }
    }
}

/// `not_found` and `forbidden` for a denial; `WrongResource` is a bug in the
/// handler, so it's internal and logged at `error`.
impl From<AuthzError> for ApiError {
    fn from(error: AuthzError) -> Self {
        match error {
            AuthzError::NotFound => Self::NotFound,
            AuthzError::Forbidden => Self::Forbidden,
            AuthzError::WrongResource => Self::internal(error),
        }
    }
}

/// `validation_failed`, with one field error per broken rule, in garde's
/// order and path notation. garde's messages name the rule, never the value.
impl From<garde::Report> for ApiError {
    fn from(report: garde::Report) -> Self {
        let fields = report
            .iter()
            .map(|(path, error)| FieldError::new(&path.to_string(), error.message()))
            .collect();
        Self::Validation(fields)
    }
}

/// What [`render_errors`] needs to write the body. It lives only in the
/// response's extensions, never on the wire.
#[derive(Debug, Clone)]
struct Marker {
    /// The code.
    code: ErrorCode,
    /// The broken rules of `validation_failed`.
    fields: Option<Vec<FieldError>>,
    /// The source of `internal`, for the log.
    source: Option<Arc<dyn Error + Send + Sync>>,
}

impl Marker {
    /// The marker of an error status that came without one: the status's
    /// code, and no fields for a bare 422.
    fn for_bare(status: StatusCode) -> Self {
        let code = bare_code(status);
        Self {
            code,
            fields: (code == ErrorCode::ValidationFailed).then(Vec::new),
            source: None,
        }
    }
}

/// The code of an error status without a marker. A status without a code of
/// its own gets `bad_request` (4xx) or `internal` (5xx). Only 4xx and 5xx
/// statuses come here.
const fn bare_code(status: StatusCode) -> ErrorCode {
    match status.as_u16() {
        401 => ErrorCode::Unauthorized,
        403 => ErrorCode::Forbidden,
        404 => ErrorCode::NotFound,
        405 => ErrorCode::MethodNotAllowed,
        408 => ErrorCode::Timeout,
        409 => ErrorCode::Conflict,
        413 => ErrorCode::PayloadTooLarge,
        415 => ErrorCode::UnsupportedMediaType,
        422 => ErrorCode::ValidationFailed,
        429 => ErrorCode::RateLimited,
        503 => ErrorCode::Busy,
        500..=599 => ErrorCode::Internal,
        // 400 itself, and every other 4xx.
        _ => ErrorCode::BadRequest,
    }
}

/// The value of `WWW-Authenticate` on every 401: the scheme, with no realm
/// and no error detail.
const BEARER: HeaderValue = HeaderValue::from_static("Bearer");

impl ApiError {
    /// The response's status and headers, with an empty body, and the marker
    /// that [`render_errors`] needs to write the body.
    fn into_parts(self) -> (Response, Marker) {
        let code = self.code();
        let mut response = Response::new(Body::empty());
        *response.status_mut() = status(code);
        let headers = response.headers_mut();
        let (fields, source) = match self {
            Self::Unauthorized => {
                headers.insert(WWW_AUTHENTICATE, BEARER);
                (None, None)
            }
            Self::RateLimited { retry_after } => {
                headers.insert(RETRY_AFTER, retry_after_seconds(retry_after));
                (None, None)
            }
            Self::Busy => {
                headers.insert(RETRY_AFTER, retry_after_seconds(BUSY_RETRY_AFTER));
                (None, None)
            }
            Self::Validation(fields) => (Some(fields), None),
            Self::Internal(source) => (None, Some(Arc::from(source))),
            Self::BadRequest
            | Self::Forbidden
            | Self::NotFound
            | Self::MethodNotAllowed
            | Self::Timeout
            | Self::Conflict
            | Self::PayloadTooLarge
            | Self::UnsupportedMediaType => (None, None),
        };
        (
            response,
            Marker {
                code,
                fields,
                source,
            },
        )
    }
}

/// Sets the status and headers and leaves the marker for [`render_errors`],
/// with an empty body.
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (mut response, marker) = self.into_parts();
        response.extensions_mut().insert(marker);
        response
    }
}

/// The code of an error response [`render_errors`] wrote, for the trace
/// layer's line. It lives only in the response's extensions, and the trace
/// layer takes it out, so it never reaches the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RenderedCode(pub(crate) ErrorCode);

/// `Retry-After` for `delay`: whole seconds, rounded up.
fn retry_after_seconds(delay: Duration) -> HeaderValue {
    let seconds = delay
        .as_secs()
        .saturating_add(u64::from(delay.subsec_nanos() > 0));
    HeaderValue::from(seconds)
}

/// The middleware that writes every error response's body: an [`ApiError`]'s,
/// and through the safety net any other 4xx or 5xx; see the
/// [module docs](self). It must run inside the request-ID layer.
pub async fn render_errors(request: Request, next: Next) -> Response {
    let request_id = request_id(&request);
    let mut response = next.run(request).await;
    let status = response.status();
    let (marker, bare) = match response.extensions_mut().remove::<Marker>() {
        Some(marker) => (marker, false),
        None if status.is_client_error() || status.is_server_error() => {
            (Marker::for_bare(status), true)
        }
        None => return response,
    };
    let request_id = request_id.unwrap_or_else(|| {
        error!(
            code = marker.code.as_str(),
            "an error response has no request ID: the error middleware runs outside the request-ID layer"
        );
        BoundedText::new(UNKNOWN_REQUEST_ID)
    });
    if bare {
        add_missing_headers(response.headers_mut(), marker.code);
        log_bare(status, &request_id);
    } else {
        log(&marker, &request_id);
    }
    write(response, marker, request_id)
}

/// The response for `error` when no request ID exists, with `"unknown"` in
/// its body. Nothing is logged: the caller, the request-ID layer, logs why.
pub(crate) fn render_without_id(error: ApiError) -> Response {
    let (response, marker) = error.into_parts();
    write(response, marker, BoundedText::new(UNKNOWN_REQUEST_ID))
}

/// Writes the envelope into `response`, replacing its body and removing the
/// headers that described the old one, and leaves the code for the trace
/// layer.
fn write(
    mut response: Response,
    marker: Marker,
    request_id: BoundedText<REQUEST_ID_MAX_CHARS>,
) -> Response {
    let code = marker.code;
    let body = ErrorResponse {
        error: ErrorBody::new(code, request_id, marker.fields),
    };
    let Some(bytes) = encode(&body) else {
        return fallback();
    };
    let headers = response.headers_mut();
    headers.remove(CONTENT_LENGTH);
    headers.remove(CONTENT_ENCODING);
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    *response.body_mut() = Body::from(bytes);
    response.extensions_mut().insert(RenderedCode(code));
    response
}

/// Adds the headers an [`ApiError`] with `code` always sends, when a bare
/// error status lacks them; a header that's there already stays as it is.
fn add_missing_headers(headers: &mut HeaderMap, code: ErrorCode) {
    match code {
        ErrorCode::Unauthorized => {
            headers.entry(WWW_AUTHENTICATE).or_insert(BEARER);
        }
        ErrorCode::Busy => {
            headers
                .entry(RETRY_AFTER)
                .or_insert_with(|| retry_after_seconds(BUSY_RETRY_AFTER));
        }
        ErrorCode::BadRequest
        | ErrorCode::Forbidden
        | ErrorCode::NotFound
        | ErrorCode::MethodNotAllowed
        | ErrorCode::Timeout
        | ErrorCode::Conflict
        | ErrorCode::PayloadTooLarge
        | ErrorCode::UnsupportedMediaType
        | ErrorCode::ValidationFailed
        | ErrorCode::RateLimited
        | ErrorCode::Internal => {}
    }
}

/// Logs a bare error status as a wiring bug, except the two that come bare
/// by design: the timeout's 408 and the body limit's 413.
fn log_bare(status: StatusCode, request_id: &BoundedText<REQUEST_ID_MAX_CHARS>) {
    if status == StatusCode::REQUEST_TIMEOUT || status == StatusCode::PAYLOAD_TOO_LARGE {
        return;
    }
    error!(
        status = status.as_u16(),
        request_id = %request_id,
        "an error response came without an ApiError: its source must return one"
    );
}

/// The request's ID from [`REQUEST_ID_HEADER`], cleaned and capped; `None`
/// when the header is missing, isn't text, or is empty.
fn request_id(request: &Request) -> Option<BoundedText<REQUEST_ID_MAX_CHARS>> {
    let text = request.headers().get(REQUEST_ID_HEADER)?.to_str().ok()?;
    let id = BoundedText::new(text);
    (!id.as_str().is_empty()).then_some(id)
}

/// Logs what an operator must see: an internal error (the only kind with a
/// source) with its whole source chain, and a busy server.
fn log(marker: &Marker, request_id: &BoundedText<REQUEST_ID_MAX_CHARS>) {
    if let Some(source) = &marker.source {
        error!(
            request_id = %request_id,
            error = %chain(source.as_ref()),
            "a request failed with an internal error"
        );
    } else if marker.code == ErrorCode::Busy {
        warn!(
            request_id = %request_id,
            "a request was refused because the server is busy"
        );
    }
}

/// `error` and every source below it, joined with `: `, cleaned and cut at
/// [`CHAIN_MAX_CHARS`] characters for one log line.
pub(crate) fn chain(error: &(dyn Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    sanitize_untrusted(&text, CHAIN_MAX_CHARS).text
}

/// `body` as JSON, or `None`, logged, if serde fails, which can't happen
/// for the error envelope.
fn encode<T: Serialize>(body: &T) -> Option<Vec<u8>> {
    serde_json::to_vec(body)
        .inspect_err(|failure| {
            error!(
                error = %sanitize_untrusted(&failure.to_string(), CHAIN_MAX_CHARS).text,
                "an error body couldn't be written; the fixed internal error was sent instead"
            );
        })
        .ok()
}

/// The fixed `internal` error sent when the real body can't be written.
fn fallback() -> Response {
    let mut response = Response::new(Body::from(FALLBACK_BODY));
    *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
        .extensions_mut()
        .insert(RenderedCode(ErrorCode::Internal));
    response
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::to_bytes;
    use axum::middleware::from_fn;
    use axum::routing::get;
    use fleet_startup::config::LogConfig;
    use fleet_startup::telemetry::{NoRules, layer_with};
    use fleet_testkit::log_buffer::LogBuffer;
    use serde::Serializer;
    use tower::ServiceExt as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;

    /// A value serde can't write.
    struct Unwritable;

    impl Serialize for Unwritable {
        fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("this value can't be written"))
        }
    }

    #[test]
    fn a_body_serde_cant_write_is_none_and_logged() {
        let buffer = LogBuffer::default();
        let subscriber = tracing_subscriber::registry().with(layer_with(
            &LogConfig::default(),
            &NoRules,
            false,
            buffer.clone(),
        ));

        let encoded = tracing::subscriber::with_default(subscriber, || encode(&Unwritable));

        assert_eq!(encoded, None);
        let lines = buffer.json_lines().unwrap();
        assert_eq!(lines.len(), 1, "{}", buffer.text());
        assert_eq!(lines[0]["level"], "ERROR");
        assert_eq!(lines[0]["error"], "this value can't be written");
    }

    #[tokio::test]
    async fn the_fallback_is_the_fixed_internal_error_serde_would_write() {
        let expected = ErrorResponse {
            error: ErrorBody::new(ErrorCode::Internal, BoundedText::new("unknown"), None),
        };

        let response = fallback();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body, serde_json::to_vec(&expected).unwrap());
        assert_eq!(body, FALLBACK_BODY.as_bytes());
    }

    #[tokio::test]
    async fn the_marker_never_leaves_the_middleware() {
        let app = Router::new()
            .route("/", get(|| async { Err::<(), _>(ApiError::NotFound) }))
            .layer(from_fn(render_errors));
        let request = Request::builder()
            .uri("/")
            .header(REQUEST_ID_HEADER, "test-request-id")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert!(response.extensions().get::<Marker>().is_none());
    }

    #[test]
    fn the_fallback_names_its_code_for_the_trace_layer() {
        let response = fallback();

        assert_eq!(
            response.extensions().get::<RenderedCode>(),
            Some(&RenderedCode(ErrorCode::Internal))
        );
    }

    #[test]
    fn every_codes_status_maps_back_to_the_code_when_it_comes_bare() {
        for code in ErrorCode::ALL {
            assert_eq!(bare_code(status(*code)), *code, "{code:?}");
        }
    }

    #[rstest::rstest]
    #[case::client_status_without_a_code(StatusCode::IM_A_TEAPOT, ErrorCode::BadRequest)]
    #[case::header_fields_too_large(
        StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
        ErrorCode::BadRequest
    )]
    #[case::length_required(StatusCode::LENGTH_REQUIRED, ErrorCode::BadRequest)]
    #[case::the_last_client_status(StatusCode::from_u16(499).unwrap(), ErrorCode::BadRequest)]
    #[case::not_implemented(StatusCode::NOT_IMPLEMENTED, ErrorCode::Internal)]
    #[case::bad_gateway(StatusCode::BAD_GATEWAY, ErrorCode::Internal)]
    #[case::gateway_timeout(StatusCode::GATEWAY_TIMEOUT, ErrorCode::Internal)]
    #[case::the_last_server_status(StatusCode::from_u16(599).unwrap(), ErrorCode::Internal)]
    fn a_bare_status_without_a_code_gets_the_generic_code_of_its_class(
        #[case] status: StatusCode,
        #[case] code: ErrorCode,
    ) {
        assert_eq!(bare_code(status), code);
    }

    #[test]
    fn only_a_bare_422_has_an_empty_field_list() {
        assert_eq!(
            Marker::for_bare(StatusCode::UNPROCESSABLE_ENTITY).fields,
            Some(Vec::new())
        );
        assert_eq!(Marker::for_bare(StatusCode::BAD_REQUEST).fields, None);
    }

    #[test]
    fn an_api_error_carries_its_marker_until_rendered() {
        let response =
            ApiError::Validation(vec![FieldError::new("name", "too short")]).into_response();

        let marker = response.extensions().get::<Marker>().unwrap();
        assert_eq!(marker.code, ErrorCode::ValidationFailed);
        assert_eq!(
            marker.fields,
            Some(vec![FieldError::new("name", "too short")])
        );
        assert!(marker.source.is_none());
    }
}
