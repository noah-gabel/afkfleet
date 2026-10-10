//! The middleware stack through a test router (Plan.md P6.6, ADR-0015): every
//! error the router or a layer can produce reaches the error envelope with a
//! server-generated request ID, the security and sensitive headers, and what
//! the trace layer logs and never logs.
//!
//! Every test request goes through `middleware::apply`, the server's own stack,
//! around test routes that are slow, panic, read their body, or answer with a
//! bare status. The time is paused, so the request timeout fires at once; the
//! request IDs come from a `ManualClock` and a `SeededRandom` and are only ever
//! compared with each other.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Path, Request};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, PROXY_AUTHORIZATION,
    RETRY_AFTER, SET_COOKIE, X_FRAME_OPTIONS,
};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header::AUTHORIZATION};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use chrono::DateTime;
use fleet_api_types::BoundedText;
use fleet_api_types::error::REQUEST_ID_MAX_CHARS;
use fleet_core::id::RequestId;
use fleet_core::system::RandomError;
use fleet_server::config::{HttpConfig, LogConfig, LogFormat};
use fleet_server::http::error::REQUEST_ID_HEADER;
use fleet_server::http::middleware::{self, RequestIds};
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::log_buffer::LogBuffer;
use fleet_testkit::system::{ManualClock, SeededRandom};
use rstest::rstest;
use serde::Serialize;
use serde_json::{Value, json};
use tower::ServiceExt as _;
use tracing_subscriber::layer::SubscriberExt as _;

/// The test stack's request timeout.
const TIMEOUT: Duration = Duration::from_secs(2);

/// The test stack's body limit.
const MAX_BODY: usize = 1024;

/// An `x-request-id` a client sends; it must never survive.
const CLIENT_ID: &str = "client-chosen-id";

/// What the panicking route panics with; it must never reach a client.
const PANIC_PAYLOAD: &str = "secret panic payload";

/// The headers every response must carry, with their values.
const SECURITY_HEADERS: [(&str, &str); 5] = [
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    ("cache-control", "no-store"),
    (
        "content-security-policy",
        "default-src 'none'; frame-ancestors 'none'",
    ),
    ("x-frame-options", "DENY"),
];

/// The headers a test looks at, and a snapshot holds.
const SEEN_HEADERS: [&str; 15] = [
    "allow",
    "cache-control",
    "content-encoding",
    "content-length",
    "content-security-policy",
    "content-type",
    "ratelimit-policy",
    "referrer-policy",
    "retry-after",
    "set-cookie",
    "www-authenticate",
    "x-content-type-options",
    "x-frame-options",
    "x-request-id",
    "x-test-handler",
];

// --- The test stack -----------------------------------------------------------

/// The stack's `[http]` settings.
fn http_config() -> HttpConfig {
    HttpConfig {
        bind: HttpConfig::DEFAULT_BIND,
        request_timeout: TIMEOUT,
        max_body_bytes: MAX_BODY,
    }
}

/// The test routes behind the server's stack, and the random source its IDs
/// come from.
struct Stack {
    app: Router,
    random: SeededRandom,
}

fn stack() -> Stack {
    let clock = ManualClock::new(DateTime::from_timestamp_millis(1_700_000_000_000).unwrap());
    let random = SeededRandom::new(7);
    let ids = RequestIds::new(Arc::new(clock), Arc::new(random.clone()));
    Stack {
        app: middleware::apply(routes(), &http_config(), ids),
        random,
    }
}

fn routes() -> Router {
    Router::new()
        .route("/ok", get(|| async { "fine" }))
        .route("/slow", get(slow))
        .route("/panic", get(panics))
        .route(
            "/echo",
            post(|body: Bytes| async move { body.len().to_string() }),
        )
        .route("/bare/{status}", get(bare))
        .route("/bare-with-retry/{status}", get(bare_with_retry))
        .route("/request-ids", get(request_ids))
        .route("/request-headers", get(request_headers))
        .route(
            "/cookie",
            get(|| async { ([(SET_COOKIE, "session=marker-set-cookie")], "set") }),
        )
        .route("/weak-headers", get(weak_headers))
        .route(
            "/handler-id",
            get(|| async { ([(REQUEST_ID_HEADER, "handler-chosen")], "x") }),
        )
        .route("/health/probe", get(|| async { "quiet" }))
}

/// Answers long after the request timeout.
async fn slow() -> &'static str {
    tokio::time::sleep(TIMEOUT * 10).await;
    "too late"
}

async fn panics() -> &'static str {
    panic!("{PANIC_PAYLOAD}")
}

/// A bare `status`, as a layer or rejection that doesn't use `ApiError`
/// would answer: a text body, a `Content-Length` that fits it, and a
/// `Content-Encoding` that describes it.
async fn bare(Path(status): Path<u16>) -> impl IntoResponse {
    (
        StatusCode::from_u16(status).unwrap(),
        [
            (CONTENT_LENGTH, "15"),
            (CONTENT_ENCODING, "gzip"),
            (HeaderName::from_static("x-test-handler"), "kept"),
        ],
        "upstream detail",
    )
}

/// A bare `status` with a `Retry-After` and a rate-limit header, as P7.10's
/// limiter would answer.
async fn bare_with_retry(Path(status): Path<u16>) -> impl IntoResponse {
    (
        StatusCode::from_u16(status).unwrap(),
        [
            (RETRY_AFTER, "7"),
            (HeaderName::from_static("ratelimit-policy"), "20;w=1"),
        ],
        "slow down",
    )
}

/// Every `x-request-id` the handler sees, comma-separated.
async fn request_ids(request: Request) -> String {
    request
        .headers()
        .get_all(REQUEST_ID_HEADER)
        .iter()
        .map(|value| value.to_str().unwrap().to_owned())
        .collect::<Vec<_>>()
        .join(",")
}

/// The request's headers as `Debug` prints them.
async fn request_headers(request: Request) -> String {
    format!("{:?}", request.headers())
}

/// A handler that tries to weaken the security headers.
async fn weak_headers() -> impl IntoResponse {
    (
        [
            (CACHE_CONTROL, "public, max-age=600"),
            (X_FRAME_OPTIONS, "ALLOWALL"),
        ],
        "weak",
    )
}

// --- Sending and reading ------------------------------------------------------

/// What a client sees of a response.
#[derive(Debug, Serialize)]
struct Seen {
    /// The status code.
    status: u16,
    /// The headers in `SEEN_HEADERS` that the response has.
    headers: BTreeMap<String, String>,
    /// The body, parsed as JSON (`null` when it isn't JSON).
    body: Value,
    /// The body as text.
    #[serde(skip)]
    text: String,
    /// The `Set-Cookie` header's sensitive flag, if there is one.
    #[serde(skip)]
    set_cookie_sensitive: Option<bool>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

fn request(method: Method, uri: &str) -> Request {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn get_request(uri: &str) -> Request {
    request(Method::GET, uri)
}

async fn send(app: &Router, request: Request) -> Seen {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let headers: BTreeMap<String, String> = SEEN_HEADERS
        .into_iter()
        .filter_map(|name| {
            let value = response.headers().get(name)?;
            Some((name.to_owned(), value.to_str().unwrap().to_owned()))
        })
        .collect();
    let set_cookie_sensitive = response
        .headers()
        .get(SET_COOKIE)
        .map(HeaderValue::is_sensitive);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let is_json = headers
        .get("content-type")
        .is_some_and(|value| value == "application/json");
    let body = if is_json {
        serde_json::from_str(&text).unwrap()
    } else {
        Value::Null
    };
    Seen {
        status,
        headers,
        body,
        text,
        set_cookie_sensitive,
    }
}

/// [`send`], with the server's log layer (fleet-startup's, as `serve` will
/// install it) under `filter` writing into a buffer. The tests run on tokio's
/// current-thread runtime, so the router runs on the thread that holds the
/// subscriber.
async fn send_logged(app: &Router, request: Request, filter: &str) -> (Seen, LogBuffer) {
    let buffer = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: filter.parse().unwrap(),
    };
    let subscriber =
        tracing_subscriber::registry().with(layer_with(&config, &NoRules, false, buffer.clone()));
    let _guard = tracing::subscriber::set_default(subscriber);
    let seen = send(app, request).await;
    (seen, buffer)
}

/// The response's request ID, after checking it: a version 7 ID in its
/// canonical form that fits `BoundedText<64>` unchanged, never the client's,
/// and the same in the body when there is one.
fn checked_id(seen: &Seen) -> String {
    let id = seen.header("x-request-id").expect("an x-request-id header");
    let parsed: RequestId = id.parse().unwrap();
    assert_eq!(parsed.to_string(), id);
    assert_eq!(BoundedText::<REQUEST_ID_MAX_CHARS>::new(id).as_str(), id);
    assert_ne!(id, CLIENT_ID);
    if !seen.body.is_null() {
        assert_eq!(seen.body["error"]["request_id"], id);
    }
    id.to_owned()
}

/// `seen` with its request ID, checked before, replaced by a placeholder for a
/// snapshot: the ID comes from `SeededRandom`, whose bytes are never
/// hard-coded.
fn redacted(mut seen: Seen) -> Seen {
    if seen.headers.contains_key("x-request-id") {
        seen.headers
            .insert("x-request-id".to_owned(), "[request_id]".to_owned());
    }
    if seen.body["error"]["request_id"].is_string() {
        seen.body["error"]["request_id"] = json!("[request_id]");
    }
    seen
}

/// Checks what every error response must be: the envelope with its fixed
/// message, JSON, a `Content-Length` that fits the body (or none), no
/// `Content-Encoding`, and the security headers.
fn assert_envelope(seen: &Seen, status: u16, code: &str) {
    assert_eq!(seen.status, status, "{}", seen.text);
    assert_eq!(seen.header("content-type"), Some("application/json"));
    assert_eq!(seen.body["error"]["code"], code, "{}", seen.text);
    assert!(seen.body["error"]["message"].is_string(), "{}", seen.text);
    assert!(
        seen.body["error"]["request_id"].is_string(),
        "{}",
        seen.text
    );
    if let Some(length) = seen.header("content-length") {
        assert_eq!(length, seen.text.len().to_string());
    }
    assert_eq!(seen.header("content-encoding"), None);
    assert_security_headers(seen);
}

fn assert_security_headers(seen: &Seen) {
    for (name, value) in SECURITY_HEADERS {
        assert_eq!(seen.header(name), Some(value), "{name}");
    }
}

/// The JSON lines whose message is `message`.
fn lines_with_message(buffer: &LogBuffer, message: &str) -> Vec<Value> {
    buffer
        .json_lines()
        .unwrap()
        .into_iter()
        .filter(|line| line["message"] == message)
        .collect()
}

/// The one trace line of a request.
fn finish_line(buffer: &LogBuffer) -> Value {
    let lines = lines_with_message(buffer, "request finished");
    assert_eq!(lines.len(), 1, "{}", buffer.text());
    lines.into_iter().next().unwrap()
}

/// The lines at `error`.
fn error_lines(buffer: &LogBuffer) -> Vec<Value> {
    buffer
        .json_lines()
        .unwrap()
        .into_iter()
        .filter(|line| line["level"] == "ERROR")
        .collect()
}

// --- Every error the router or a layer produces -------------------------------

/// A request with a body of `len` bytes, with or without its
/// `Content-Length`.
fn post_echo(len: usize, with_length: bool) -> Request {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/echo")
        .body(Body::from(vec![b'x'; len]))
        .unwrap();
    if with_length {
        request
            .headers_mut()
            .insert(CONTENT_LENGTH, HeaderValue::from(len));
    }
    request
}

#[rstest]
#[case::not_found("not_found", || get_request("/nowhere"), 404, "not_found")]
#[case::method_not_allowed(
    "method_not_allowed",
    || request(Method::DELETE, "/ok"),
    405,
    "method_not_allowed"
)]
#[case::timeout("timeout", || get_request("/slow"), 408, "timeout")]
#[case::body_too_large_by_its_length(
    "payload_too_large_by_length",
    || post_echo(MAX_BODY + 1, true),
    413,
    "payload_too_large"
)]
#[case::body_too_large_when_read(
    "payload_too_large_when_read",
    || post_echo(MAX_BODY + 1, false),
    413,
    "payload_too_large"
)]
#[case::panic("panic", || get_request("/panic"), 500, "internal")]
#[tokio::test(start_paused = true)]
async fn every_error_the_stack_produces_answers_with_the_envelope_and_a_request_id(
    #[case] name: &str,
    #[case] make: fn() -> Request,
    #[case] status: u16,
    #[case] code: &str,
) {
    let stack = stack();
    let mut request = make();
    request
        .headers_mut()
        .insert(REQUEST_ID_HEADER, HeaderValue::from_static(CLIENT_ID));

    let seen = send(&stack.app, request).await;

    assert_envelope(&seen, status, code);
    checked_id(&seen);
    insta::assert_json_snapshot!(format!("error_{name}"), redacted(seen));
}

#[tokio::test]
async fn a_405_keeps_axums_allow_header() {
    let seen = send(&stack().app, request(Method::DELETE, "/ok")).await;

    assert_envelope(&seen, 405, "method_not_allowed");
    assert_eq!(seen.header("allow"), Some("GET,HEAD"));
}

#[tokio::test]
async fn a_body_within_the_limit_is_read() {
    let seen = send(&stack().app, post_echo(MAX_BODY, true)).await;

    assert_eq!(seen.status, 200);
    assert_eq!(seen.text, MAX_BODY.to_string());
}

#[tokio::test(start_paused = true)]
async fn a_request_within_the_timeout_is_answered() {
    let app = middleware::apply(
        Router::new().route(
            "/",
            get(|| async {
                tokio::time::sleep(TIMEOUT.saturating_sub(Duration::from_millis(1))).await;
                "in time"
            }),
        ),
        &http_config(),
        RequestIds::new(
            Arc::new(ManualClock::new(DateTime::UNIX_EPOCH)),
            Arc::new(SeededRandom::new(1)),
        ),
    );

    let seen = send(&app, get_request("/")).await;

    assert_eq!(seen.status, 200);
    assert_eq!(seen.text, "in time");
}

#[tokio::test]
async fn a_panic_answers_with_the_fixed_internal_error_and_never_its_payload() {
    let (seen, logs) = send_logged(&stack().app, get_request("/panic"), "info").await;

    assert_envelope(&seen, 500, "internal");
    assert!(!seen.text.contains("secret"), "{}", seen.text);
    assert!(!seen.text.contains("panic payload"), "{}", seen.text);
    let errors = error_lines(&logs);
    assert_eq!(errors.len(), 1, "{}", logs.text());
    assert_eq!(errors[0]["error"], "a request handler panicked");
    assert_eq!(errors[0]["request_id"], checked_id(&seen));
    assert!(!logs.text().contains(PANIC_PAYLOAD), "{}", logs.text());
}

// --- The safety net: errors that don't come from ApiError -----------------------

#[rstest]
#[case::status_without_a_code("431", "/bare/431", 431, "bad_request")]
#[case::server_status_without_a_code("502", "/bare/502", 502, "internal")]
#[case::unauthorized("401", "/bare/401", 401, "unauthorized")]
#[case::busy("503", "/bare/503", 503, "busy")]
#[case::rate_limited_without_retry_after("429", "/bare/429", 429, "rate_limited")]
#[case::rate_limited_with_retry_after(
    "429_with_retry",
    "/bare-with-retry/429",
    429,
    "rate_limited"
)]
#[case::busy_with_retry_after("503_with_retry", "/bare-with-retry/503", 503, "busy")]
#[case::validation_failed("422", "/bare/422", 422, "validation_failed")]
#[case::teapot("418", "/bare/418", 418, "bad_request")]
#[tokio::test]
async fn a_bare_error_status_is_wrapped_in_the_envelope_and_logged_as_a_wiring_bug(
    #[case] name: &str,
    #[case] uri: &str,
    #[case] status: u16,
    #[case] code: &str,
) {
    let (seen, logs) = send_logged(&stack().app, get_request(uri), "info").await;

    assert_envelope(&seen, status, code);
    assert!(!seen.text.contains("upstream detail"), "{}", seen.text);
    assert!(!seen.text.contains("slow down"), "{}", seen.text);
    let id = checked_id(&seen);
    let errors = error_lines(&logs);
    assert_eq!(errors.len(), 1, "{}", logs.text());
    assert_eq!(errors[0]["status"], status);
    assert_eq!(errors[0]["request_id"], id);
    insta::assert_json_snapshot!(format!("bare_{name}"), redacted(seen));
}

#[tokio::test]
async fn the_safety_net_keeps_the_responses_own_headers() {
    let stack = stack();

    let bare = send(&stack.app, get_request("/bare/502")).await;
    let limited = send(&stack.app, get_request("/bare-with-retry/429")).await;

    assert_eq!(bare.header("x-test-handler"), Some("kept"));
    assert_eq!(limited.header("retry-after"), Some("7"));
    assert_eq!(limited.header("ratelimit-policy"), Some("20;w=1"));
}

#[tokio::test]
async fn the_safety_net_adds_the_headers_its_code_always_sends_only_when_missing() {
    let stack = stack();

    let unauthorized = send(&stack.app, get_request("/bare/401")).await;
    let busy = send(&stack.app, get_request("/bare/503")).await;
    let busy_with_retry = send(&stack.app, get_request("/bare-with-retry/503")).await;
    let limited = send(&stack.app, get_request("/bare/429")).await;

    assert_eq!(unauthorized.header("www-authenticate"), Some("Bearer"));
    assert_eq!(busy.header("retry-after"), Some("1"));
    assert_eq!(busy_with_retry.header("retry-after"), Some("7"));
    assert_eq!(limited.header("retry-after"), None);
}

#[tokio::test]
async fn a_bare_422_names_no_fields() {
    let seen = send(&stack().app, get_request("/bare/422")).await;

    assert_eq!(seen.body["error"]["fields"], json!([]));
}

#[rstest]
#[case::timeout(get_request("/slow"))]
#[case::body_limit_by_length(post_echo(MAX_BODY + 1, true))]
#[case::body_limit_when_read(post_echo(MAX_BODY + 1, false))]
#[tokio::test(start_paused = true)]
async fn the_timeout_and_the_body_limit_are_not_wiring_bugs(#[case] request: Request) {
    let (seen, logs) = send_logged(&stack().app, request, "info").await;

    assert!(seen.status == 408 || seen.status == 413, "{}", seen.status);
    assert_eq!(error_lines(&logs), Vec::<Value>::new(), "{}", logs.text());
    assert_eq!(finish_line(&logs)["status"], seen.status);
}

#[tokio::test]
async fn a_success_passes_the_safety_net_unchanged() {
    let seen = send(&stack().app, get_request("/ok")).await;

    assert_eq!(seen.status, 200);
    assert_eq!(seen.text, "fine");
    assert_eq!(
        seen.header("content-type"),
        Some("text/plain; charset=utf-8")
    );
}

// --- The request ID ---------------------------------------------------------------

#[rstest]
#[case::none(&[])]
#[case::one(&[CLIENT_ID])]
#[case::two(&[CLIENT_ID, "another-client-id"])]
#[tokio::test]
async fn the_handler_and_the_client_see_only_the_servers_request_id(#[case] client: &[&str]) {
    let mut request = get_request("/request-ids");
    for id in client {
        request
            .headers_mut()
            .append(REQUEST_ID_HEADER, HeaderValue::from_str(id).unwrap());
    }

    let seen = send(&stack().app, request).await;

    let id = checked_id(&seen);
    assert_eq!(seen.text, id);
}

#[tokio::test]
async fn the_logged_request_id_is_the_returned_one() {
    let mut request = get_request("/ok");
    request
        .headers_mut()
        .insert(REQUEST_ID_HEADER, HeaderValue::from_static(CLIENT_ID));

    let (seen, logs) = send_logged(&stack().app, request, "info").await;

    let id = checked_id(&seen);
    assert_eq!(finish_line(&logs)["span"]["request_id"], id);
    assert!(!logs.text().contains(CLIENT_ID), "{}", logs.text());
}

#[tokio::test]
async fn every_request_gets_its_own_id() {
    let stack = stack();

    let first = send(&stack.app, get_request("/ok")).await;
    let second = send(&stack.app, get_request("/ok")).await;

    assert_ne!(checked_id(&first), checked_id(&second));
}

#[tokio::test]
async fn a_handler_cannot_choose_the_response_id() {
    let seen = send(&stack().app, get_request("/handler-id")).await;

    assert_ne!(checked_id(&seen), "handler-chosen");
}

#[tokio::test]
async fn without_a_request_id_the_request_is_refused_with_the_internal_error() {
    let stack = stack();
    stack.random.fail_next(RandomError::Os { code: 5 });

    let (seen, logs) = send_logged(&stack.app, get_request("/ok"), "debug").await;

    assert_envelope(&seen, 500, "internal");
    assert_eq!(seen.body["error"]["request_id"], "unknown");
    assert_eq!(seen.header("x-request-id"), None);
    let lines = logs.json_lines().unwrap();
    assert_eq!(lines.len(), 1, "{}", logs.text());
    assert_eq!(lines[0]["level"], "ERROR");
    assert_eq!(
        lines[0]["error"],
        "no random bytes for the ID: the OS's secure random source failed (OS error 5)"
    );
    insta::assert_json_snapshot!("error_no_request_id", seen);
}

#[tokio::test]
async fn the_next_request_after_a_failed_mint_gets_an_id() {
    let stack = stack();
    stack.random.fail_next(RandomError::Unavailable);
    let refused = send(&stack.app, get_request("/ok")).await;

    let seen = send(&stack.app, get_request("/ok")).await;

    assert_eq!(refused.status, 500);
    assert_eq!(seen.status, 200);
    checked_id(&seen);
}

// --- Security and sensitive headers ---------------------------------------------

#[tokio::test]
async fn a_success_carries_the_security_headers() {
    let seen = send(&stack().app, get_request("/ok")).await;

    assert_security_headers(&seen);
}

#[tokio::test]
async fn a_handler_cannot_weaken_the_security_headers() {
    let seen = send(&stack().app, get_request("/weak-headers")).await;

    assert_security_headers(&seen);
}

#[tokio::test]
async fn credentials_are_marked_sensitive_before_any_handler_sees_them() {
    let mut request = get_request("/request-headers");
    let headers = request.headers_mut();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_static("Bearer marker-authorization"),
    );
    headers.insert(
        PROXY_AUTHORIZATION,
        HeaderValue::from_static("Basic marker-proxy"),
    );
    headers.insert(COOKIE, HeaderValue::from_static("session=marker-cookie"));

    let seen = send(&stack().app, request).await;

    assert!(!seen.text.contains("marker"), "{}", seen.text);
    assert_eq!(seen.text.matches("Sensitive").count(), 3, "{}", seen.text);
}

#[tokio::test]
async fn a_set_cookie_header_is_marked_sensitive() {
    let seen = send(&stack().app, get_request("/cookie")).await;

    assert_eq!(seen.set_cookie_sensitive, Some(true));
}

// --- The trace layer --------------------------------------------------------------

#[tokio::test]
async fn the_trace_line_names_the_status_latency_and_the_requests_id_method_and_route() {
    let (seen, logs) = send_logged(&stack().app, get_request("/ok"), "info").await;

    let line = finish_line(&logs);
    assert_eq!(line["level"], "INFO");
    assert_eq!(line["status"], 200);
    assert!(line["latency_ms"].is_f64(), "{line}");
    assert!(line.get("code").is_none(), "{line}");
    assert_eq!(line["span"]["name"], "request");
    assert_eq!(line["span"]["request_id"], checked_id(&seen));
    assert_eq!(line["span"]["method"], "GET");
    assert_eq!(line["span"]["route"], "/ok");
}

#[rstest]
#[case::not_found(
    get_request("/nowhere?ticket=secret-ticket"),
    404,
    "not_found",
    "<unmatched>"
)]
#[case::method_not_allowed(request(Method::DELETE, "/ok"), 405, "method_not_allowed", "/ok")]
#[case::bare(get_request("/bare/502"), 502, "internal", "/bare/{status}")]
#[case::timeout(get_request("/slow"), 408, "timeout", "/slow")]
#[tokio::test(start_paused = true)]
async fn the_trace_line_logs_a_failure_at_info_with_its_code(
    #[case] request: Request,
    #[case] status: u16,
    #[case] code: &str,
    #[case] route: &str,
) {
    let (_seen, logs) = send_logged(&stack().app, request, "info").await;

    let line = finish_line(&logs);
    assert_eq!(line["level"], "INFO");
    assert_eq!(line["status"], status);
    assert_eq!(line["code"], code);
    assert_eq!(line["span"]["route"], route);
    assert!(!logs.text().contains("secret-ticket"), "{}", logs.text());
}

#[rstest]
#[case(Method::GET, "GET")]
#[case(Method::HEAD, "HEAD")]
#[case(Method::POST, "POST")]
#[case(Method::PUT, "PUT")]
#[case(Method::DELETE, "DELETE")]
#[case(Method::CONNECT, "CONNECT")]
#[case(Method::OPTIONS, "OPTIONS")]
#[case(Method::TRACE, "TRACE")]
#[case(Method::PATCH, "PATCH")]
#[case(Method::from_bytes(b"BREW").unwrap(), "<other>")]
#[case(Method::from_bytes(b"AN-EXTENSION-METHOD-LONGER-THAN-FIFTEEN").unwrap(), "<other>")]
#[tokio::test]
async fn the_method_is_logged_by_its_standard_name_or_as_other(
    #[case] method: Method,
    #[case] logged: &str,
) {
    let (_seen, logs) = send_logged(&stack().app, request(method, "/ok"), "info").await;

    assert_eq!(finish_line(&logs)["span"]["method"], logged);
}

#[tokio::test]
async fn a_health_route_is_logged_at_debug() {
    let stack = stack();

    let (_seen, info) = send_logged(&stack.app, get_request("/health/probe"), "info").await;
    let (_seen, debug) = send_logged(&stack.app, get_request("/health/probe"), "debug").await;

    assert_eq!(info.text(), "");
    let line = finish_line(&debug);
    assert_eq!(line["level"], "DEBUG");
    assert_eq!(line["span"]["route"], "/health/probe");
}

#[tokio::test]
async fn no_header_or_body_is_ever_logged() {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/echo")
        .header(AUTHORIZATION, "Bearer marker-authorization-1")
        .header(COOKIE, "session=marker-cookie-2")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"password":"marker-body-3"}"#))
        .unwrap();
    request.headers_mut().insert(
        REQUEST_ID_HEADER,
        HeaderValue::from_static("marker-client-id-4"),
    );

    let (seen, logs) = send_logged(&stack().app, request, "debug").await;

    assert_eq!(seen.status, 200);
    assert_eq!(finish_line(&logs)["status"], 200);
    assert!(!logs.text().contains("marker"), "{}", logs.text());
}
