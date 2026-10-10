//! `ApiError` and the `render_errors` middleware through a router (Plan.md
//! P6.5, ADR-0015): every variant's status, headers and envelope, what is
//! logged and what never reaches the client, and the conversions from the
//! store, `authorize()` and garde.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::collections::BTreeMap;
use std::io;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderValue, Request, StatusCode};
use axum::middleware::from_fn;
use axum::response::IntoResponse as _;
use axum::routing::get;
use fleet_api_types::error::{FIELD_MESSAGE_MAX_CHARS, FIELD_PATH_MAX_CHARS};
use fleet_api_types::{BoundedText, ErrorCode, FieldError, OpenEnum as _};
use fleet_core::authz::AuthzError;
use fleet_server::config::LogConfig;
use fleet_server::http::error::{ApiError, REQUEST_ID_HEADER, render_errors, status};
use fleet_server::ports::store::StoreError;
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::log_buffer::LogBuffer;
use garde::Validate;
use rstest::rstest;
use serde::Serialize;
use serde_json::{Value, json};
use tower::ServiceExt as _;
use tracing_subscriber::layer::SubscriberExt as _;

/// The request ID every test request carries, unless it tests a missing one.
const REQUEST_ID: &str = "test-request-id";

/// A router whose one route fails with `make()`, behind the render middleware.
fn app(make: fn() -> ApiError) -> Router {
    Router::new()
        .route("/", get(move || async move { Err::<(), _>(make()) }))
        .layer(from_fn(render_errors))
}

/// What a client sees of a response.
#[derive(Debug, Serialize)]
struct Seen {
    /// The status code.
    status: u16,
    /// The headers that matter for an error.
    headers: BTreeMap<String, String>,
    /// The body, parsed as JSON (`null` when it isn't JSON).
    body: Value,
    /// The body as text.
    #[serde(skip)]
    text: String,
}

/// Sends one request to `app`, with `request_id` in its header.
async fn send(app: Router, request_id: Option<HeaderValue>) -> Seen {
    let mut request = Request::builder().uri("/").body(Body::empty()).unwrap();
    if let Some(id) = request_id {
        request.headers_mut().insert(REQUEST_ID_HEADER, id);
    }
    let response = app.oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let headers: BTreeMap<String, String> = ["content-type", "retry-after", "www-authenticate"]
        .into_iter()
        .filter_map(|name| {
            let value = response.headers().get(name)?;
            Some((name.to_owned(), value.to_str().unwrap().to_owned()))
        })
        .collect();
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
    }
}

/// [`send`], with the server's log layer (fleet-startup's, as `serve` will
/// install it) writing into a buffer. The tests run on tokio's current-thread
/// runtime, so the router runs on the thread that holds the subscriber.
async fn send_logged(app: Router, request_id: Option<HeaderValue>) -> (Seen, LogBuffer) {
    let buffer = LogBuffer::default();
    let subscriber = tracing_subscriber::registry().with(layer_with(
        &LogConfig::default(),
        &NoRules,
        false,
        buffer.clone(),
    ));
    let _guard = tracing::subscriber::set_default(subscriber);
    let seen = send(app, request_id).await;
    (seen, buffer)
}

fn test_request_id() -> HeaderValue {
    HeaderValue::from_static(REQUEST_ID)
}

// --- Every variant ------------------------------------------------------------

#[rstest]
#[case::bad_request("bad_request", || ApiError::BadRequest)]
#[case::unauthorized("unauthorized", || ApiError::Unauthorized)]
#[case::forbidden("forbidden", || ApiError::Forbidden)]
#[case::not_found("not_found", || ApiError::NotFound)]
#[case::method_not_allowed("method_not_allowed", || ApiError::MethodNotAllowed)]
#[case::timeout("timeout", || ApiError::Timeout)]
#[case::conflict("conflict", || ApiError::Conflict)]
#[case::payload_too_large("payload_too_large", || ApiError::PayloadTooLarge)]
#[case::unsupported_media_type("unsupported_media_type", || ApiError::UnsupportedMediaType)]
#[case::validation_failed("validation_failed", || {
    ApiError::Validation(vec![
        FieldError::new("name", "length is lower than 3"),
        FieldError::new("steps[0].angle", "greater than 90"),
    ])
})]
#[case::rate_limited("rate_limited", || ApiError::RateLimited {
    retry_after: Duration::from_millis(1500),
})]
#[case::internal("internal", || ApiError::internal(io::Error::other("disk I/O error")))]
#[case::busy("busy", || ApiError::Busy)]
#[tokio::test]
async fn every_variant_answers_with_its_status_headers_and_envelope(
    #[case] code: &str,
    #[case] make: fn() -> ApiError,
) {
    let seen = send(app(make), Some(test_request_id())).await;

    assert_eq!(seen.body["error"]["code"], code);
    insta::assert_json_snapshot!(code, seen);
}

#[rstest]
#[case(ErrorCode::BadRequest, StatusCode::BAD_REQUEST)]
#[case(ErrorCode::Unauthorized, StatusCode::UNAUTHORIZED)]
#[case(ErrorCode::Forbidden, StatusCode::FORBIDDEN)]
#[case(ErrorCode::NotFound, StatusCode::NOT_FOUND)]
#[case(ErrorCode::MethodNotAllowed, StatusCode::METHOD_NOT_ALLOWED)]
#[case(ErrorCode::Timeout, StatusCode::REQUEST_TIMEOUT)]
#[case(ErrorCode::Conflict, StatusCode::CONFLICT)]
#[case(ErrorCode::PayloadTooLarge, StatusCode::PAYLOAD_TOO_LARGE)]
#[case(ErrorCode::UnsupportedMediaType, StatusCode::UNSUPPORTED_MEDIA_TYPE)]
#[case(ErrorCode::ValidationFailed, StatusCode::UNPROCESSABLE_ENTITY)]
#[case(ErrorCode::RateLimited, StatusCode::TOO_MANY_REQUESTS)]
#[case(ErrorCode::Internal, StatusCode::INTERNAL_SERVER_ERROR)]
#[case(ErrorCode::Busy, StatusCode::SERVICE_UNAVAILABLE)]
fn every_code_has_its_status(#[case] code: ErrorCode, #[case] expected: StatusCode) {
    assert_eq!(status(code), expected);
}

#[rstest]
#[case::a_second_and_a_half(Duration::from_millis(1500), "2")]
#[case::whole_seconds(Duration::from_secs(2), "2")]
#[case::a_nanosecond(Duration::from_nanos(1), "1")]
#[case::zero(Duration::ZERO, "0")]
#[case::the_longest(Duration::MAX, "18446744073709551615")]
fn retry_after_is_whole_seconds_rounded_up(#[case] retry_after: Duration, #[case] header: &str) {
    let response = ApiError::RateLimited { retry_after }.into_response();

    assert_eq!(response.headers()["retry-after"], header);
}

// --- What is logged, and what the client never sees -----------------------

/// An error whose text and source come from outside, such as a dependency's.
#[derive(Debug, thiserror::Error)]
#[error("the first line\nlooks like a second")]
struct Outer(#[source] io::Error);

#[tokio::test]
async fn an_internal_error_is_logged_with_its_whole_chain_and_the_client_sees_only_the_fixed_message()
 {
    let make =
        || -> ApiError { StoreError::Backend(Box::new(io::Error::other("disk I/O error"))).into() };

    let (seen, logs) = send_logged(app(make), Some(test_request_id())).await;

    assert_eq!(seen.status, 500);
    assert_eq!(seen.body["error"]["message"], ErrorCode::Internal.message());
    assert_eq!(seen.body["error"]["request_id"], REQUEST_ID);
    for detail in ["database", "disk", "I/O"] {
        assert!(!seen.text.contains(detail), "{detail} in {}", seen.text);
    }
    let lines = logs.json_lines().unwrap();
    assert_eq!(lines.len(), 1, "{}", logs.text());
    assert_eq!(lines[0]["level"], "ERROR");
    assert_eq!(lines[0]["request_id"], REQUEST_ID);
    assert_eq!(lines[0]["error"], "the database failed: disk I/O error");
}

#[tokio::test]
async fn the_logged_chain_is_cleaned_and_capped_at_1024_characters() {
    let make = || ApiError::internal(Outer(io::Error::other("x".repeat(2000))));

    let (_seen, logs) = send_logged(app(make), Some(test_request_id())).await;

    let lines = logs.json_lines().unwrap();
    let chain = lines[0]["error"].as_str().unwrap();
    assert!(
        chain.starts_with("the first line | looks like a second: xxx"),
        "{chain}"
    );
    assert_eq!(chain.chars().count(), 1024);
}

#[tokio::test]
async fn a_busy_server_is_logged_as_a_warning() {
    let (seen, logs) = send_logged(app(|| ApiError::Busy), Some(test_request_id())).await;

    assert_eq!(seen.status, 503);
    let lines = logs.json_lines().unwrap();
    assert_eq!(lines.len(), 1, "{}", logs.text());
    assert_eq!(lines[0]["level"], "WARN");
    assert_eq!(lines[0]["request_id"], REQUEST_ID);
}

#[rstest]
#[case::not_found(|| ApiError::NotFound)]
#[case::unauthorized(|| ApiError::Unauthorized)]
#[case::validation(|| ApiError::Validation(vec![FieldError::new("name", "too short")]))]
#[case::rate_limited(|| ApiError::RateLimited { retry_after: Duration::from_secs(3) })]
#[tokio::test]
async fn a_client_error_is_not_logged(#[case] make: fn() -> ApiError) {
    let (seen, logs) = send_logged(app(make), Some(test_request_id())).await;

    assert!(seen.status >= 400 && seen.status < 500);
    assert_eq!(logs.text(), "");
}

#[rstest]
#[case::missing(None)]
#[case::not_text(Some(HeaderValue::from_bytes(b"id\xff").unwrap()))]
#[case::empty(Some(HeaderValue::from_static("")))]
#[tokio::test]
async fn without_a_request_id_the_body_says_unknown_and_the_wiring_bug_is_logged(
    #[case] request_id: Option<HeaderValue>,
) {
    let (seen, logs) = send_logged(app(|| ApiError::NotFound), request_id).await;

    assert_eq!(seen.status, 404);
    assert_eq!(seen.body["error"]["request_id"], "unknown");
    let lines = logs.json_lines().unwrap();
    assert_eq!(lines.len(), 1, "{}", logs.text());
    assert_eq!(lines[0]["level"], "ERROR");
    assert_eq!(lines[0]["code"], "not_found");
}

#[tokio::test]
async fn a_response_without_an_api_error_passes_unchanged() {
    let app = Router::new()
        .route("/", get(|| async { (StatusCode::IM_A_TEAPOT, "tea") }))
        .layer(from_fn(render_errors));

    let seen = send(app, Some(test_request_id())).await;

    assert_eq!(seen.status, 418);
    assert_eq!(seen.text, "tea");
    assert_eq!(
        seen.headers.get("content-type").unwrap(),
        "text/plain; charset=utf-8"
    );
}

// --- Conversions ---------------------------------------------------------------

#[test]
fn a_busy_store_is_busy() {
    assert_eq!(ApiError::from(StoreError::Busy).code(), ErrorCode::Busy);
}

#[rstest]
#[case::corrupt(StoreError::Corrupt { table: "users", column: "role", rowid: 7 })]
#[case::unstorable(StoreError::Unstorable { table: "users", column: "created_at" })]
#[case::backend(StoreError::Backend(Box::new(io::Error::other("disk I/O error"))))]
fn every_other_store_error_is_internal_with_the_store_error_as_its_source(
    #[case] error: StoreError,
) {
    let expected = error.to_string();

    let api_error = ApiError::from(error);

    assert_eq!(api_error.code(), ErrorCode::Internal);
    let source = std::error::Error::source(&api_error).unwrap();
    assert!(source.downcast_ref::<StoreError>().is_some(), "{source:?}");
    assert_eq!(source.to_string(), expected);
}

#[rstest]
#[case::not_found(AuthzError::NotFound, ErrorCode::NotFound)]
#[case::forbidden(AuthzError::Forbidden, ErrorCode::Forbidden)]
#[case::wrong_resource(AuthzError::WrongResource, ErrorCode::Internal)]
fn an_authorization_denial_maps_to_its_code(#[case] error: AuthzError, #[case] code: ErrorCode) {
    assert_eq!(ApiError::from(error).code(), code);
}

#[tokio::test]
async fn a_permission_checked_against_the_wrong_resource_is_logged_at_error() {
    let (seen, logs) = send_logged(
        app(|| AuthzError::WrongResource.into()),
        Some(test_request_id()),
    )
    .await;

    assert_eq!(seen.status, 500);
    let lines = logs.json_lines().unwrap();
    assert_eq!(lines[0]["level"], "ERROR");
    assert_eq!(
        lines[0]["error"],
        "the permission doesn't apply to this kind of resource"
    );
}

/// A request a test validates with garde: one length and one range rule, and
/// a nested list for garde's path notation.
#[derive(Debug, Validate)]
struct Signup {
    #[garde(length(min = 3, max = 8))]
    name: String,
    #[garde(range(min = 1, max = 10))]
    count: i64,
    #[garde(dive)]
    steps: Vec<Step>,
}

#[derive(Debug, Validate)]
struct Step {
    #[garde(range(min = -90, max = 90))]
    angle: i32,
}

/// A request whose every value breaks its rule, each easy to spot.
fn broken_signup() -> Signup {
    Signup {
        name: "hunter2-secret".to_owned(),
        count: 7_654_321,
        steps: vec![Step { angle: 12_345 }],
    }
}

#[tokio::test]
async fn a_garde_report_becomes_field_errors_that_name_rules_never_values() {
    let make = || -> ApiError { broken_signup().validate().unwrap_err().into() };

    let seen = send(app(make), Some(test_request_id())).await;

    assert_eq!(seen.status, 422);
    let report = broken_signup().validate().unwrap_err();
    let expected: Vec<Value> = report
        .iter()
        .map(|(path, error)| json!({ "path": path.to_string(), "message": error.message() }))
        .collect();
    assert_eq!(seen.body["error"]["fields"], Value::Array(expected));
    let paths: Vec<String> = report.iter().map(|(path, _)| path.to_string()).collect();
    assert!(
        paths.iter().any(|path| path == "steps[0].angle"),
        "{paths:?}"
    );
    for value in ["hunter2-secret", "hunter2", "7654321", "12345"] {
        assert!(!seen.text.contains(value), "{value} in {}", seen.text);
    }
}

#[test]
fn garde_texts_fit_their_caps_uncut() {
    let report = broken_signup().validate().unwrap_err();

    for (path, error) in report.iter() {
        let path = path.to_string();
        assert_eq!(
            BoundedText::<FIELD_PATH_MAX_CHARS>::new(&path).as_str(),
            path
        );
        assert_eq!(
            BoundedText::<FIELD_MESSAGE_MAX_CHARS>::new(error.message()).as_str(),
            error.message()
        );
    }
}

#[test]
fn every_code_is_some_variants_code_with_its_status() {
    let variants = [
        ApiError::BadRequest,
        ApiError::Unauthorized,
        ApiError::Forbidden,
        ApiError::NotFound,
        ApiError::MethodNotAllowed,
        ApiError::Timeout,
        ApiError::Conflict,
        ApiError::PayloadTooLarge,
        ApiError::UnsupportedMediaType,
        ApiError::Validation(Vec::new()),
        ApiError::RateLimited {
            retry_after: Duration::ZERO,
        },
        ApiError::internal("test"),
        ApiError::Busy,
    ];

    let codes: Vec<&str> = variants.iter().map(|error| error.code().as_str()).collect();
    let all: Vec<&str> = ErrorCode::ALL.iter().map(|code| code.as_str()).collect();
    assert_eq!(codes, all);
    for error in &variants {
        assert_eq!(error.status(), status(error.code()), "{error:?}");
    }
}
