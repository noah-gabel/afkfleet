//! The health endpoints through the server's router (Plan.md P6.7,
//! ADR-0015): `GET /health/live` and `GET /health/ready` answer 200 with an
//! empty body, a failed readiness check answers like any store failure, and
//! no response reveals anything about the server.
//!
//! Each test gets a fresh database from `#[sqlx::test]` and opens it with
//! `Database::connect`, the production settings. Time isn't paused: sqlx's
//! worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::Method;
use chrono::DateTime;
use fleet_core::id::RequestId;
use fleet_server::app::health::HealthService;
use fleet_server::config::{HttpConfig, LogConfig, LogFormat};
use fleet_server::http::middleware::RequestIds;
use fleet_server::http::router::{AppState, router};
use fleet_server::infra::sqlite::{Database, DatabaseOptions, READ_CONNECTIONS};
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::log_buffer::LogBuffer;
use fleet_testkit::system::{ManualClock, SeededRandom};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tower::ServiceExt as _;
use tracing_subscriber::layer::SubscriberExt as _;

/// The acquire timeout of the test that waits for it (ADR-0015: it also
/// bounds opening the database, so it leaves room for a slow CI runner).
const SHORT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(1);

/// The headers a test looks at, and a snapshot holds.
const SEEN_HEADERS: [&str; 10] = [
    "cache-control",
    "content-length",
    "content-security-policy",
    "content-type",
    "referrer-policy",
    "retry-after",
    "x-content-type-options",
    "x-frame-options",
    "x-request-id",
    "allow",
];

/// The server's router over `db`.
fn app(db: &Database) -> Router {
    let ids = RequestIds::new(
        Arc::new(ManualClock::new(
            DateTime::from_timestamp_millis(1_700_000_000_000).unwrap(),
        )),
        Arc::new(SeededRandom::new(11)),
    );
    let http = HttpConfig {
        bind: HttpConfig::DEFAULT_BIND,
        request_timeout: Duration::from_secs(15),
        max_body_bytes: 65_536,
    };
    let state = AppState::new(HealthService::new(Arc::new(db.clone())));
    router(state, &http, ids)
}

async fn open(options: SqliteConnectOptions, database: DatabaseOptions) -> Database {
    Database::connect(options, database).await.unwrap()
}

/// What a client sees of a response.
#[derive(Debug, Serialize)]
struct Seen {
    /// The status code.
    status: u16,
    /// The headers in `SEEN_HEADERS` that the response has.
    headers: BTreeMap<String, String>,
    /// The body, parsed as JSON (`null` when it's empty).
    body: Value,
    /// The body as text.
    #[serde(skip)]
    text: String,
}

/// Sends `method path` and reads the response, with the server's log layer
/// at `info` writing into a buffer. The tests run on tokio's current-thread
/// runtime, so the router runs on the thread that holds the subscriber.
async fn send(app: &Router, method: Method, path: &str) -> (Seen, LogBuffer) {
    let buffer = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: "info".parse().unwrap(),
    };
    let subscriber =
        tracing_subscriber::registry().with(layer_with(&config, &NoRules, false, buffer.clone()));
    let _guard = tracing::subscriber::set_default(subscriber);
    let request = Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let headers: BTreeMap<String, String> = SEEN_HEADERS
        .into_iter()
        .filter_map(|name| {
            let value = response.headers().get(name)?;
            Some((name.to_owned(), value.to_str().unwrap().to_owned()))
        })
        .collect();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let body = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap()
    };
    let seen = Seen {
        status,
        headers,
        body,
        text,
    };
    (seen, buffer)
}

/// `seen` with its request ID, checked first, replaced by a placeholder for a
/// snapshot: the ID comes from `SeededRandom`, whose bytes are never
/// hard-coded.
fn redacted(mut seen: Seen) -> Seen {
    let id = seen.headers["x-request-id"].clone();
    id.parse::<RequestId>().unwrap();
    seen.headers
        .insert("x-request-id".to_owned(), "[request_id]".to_owned());
    if !seen.body.is_null() {
        assert_eq!(seen.body["error"]["request_id"], id);
        seen.body["error"]["request_id"] = json!("[request_id]");
    }
    seen
}

/// GET `path` answers 200 with an empty body, no content type and no line
/// at `info`; snapshot `name`.
async fn assert_answers_200_with_an_empty_body(
    options: SqliteConnectOptions,
    path: &str,
    name: &str,
) {
    let db = open(options, DatabaseOptions::default()).await;

    let (seen, logs) = send(&app(&db), Method::GET, path).await;

    assert_eq!(seen.status, 200);
    assert_eq!(seen.text, "");
    assert_eq!(seen.headers.get("content-type"), None);
    assert_eq!(logs.text(), "", "a health check logs at debug only");
    insta::assert_json_snapshot!(format!("health_{name}"), redacted(seen));
    db.close().await;
}

/// HEAD `path` answers 200 without a body.
async fn assert_answers_head_without_a_body(options: SqliteConnectOptions, path: &str) {
    let db = open(options, DatabaseOptions::default()).await;

    let (seen, _logs) = send(&app(&db), Method::HEAD, path).await;

    assert_eq!(seen.status, 200);
    assert_eq!(seen.text, "");
    db.close().await;
}

/// POST `path` answers 405 with `Allow: GET,HEAD`.
async fn assert_takes_only_get_and_head(options: SqliteConnectOptions, path: &str) {
    let db = open(options, DatabaseOptions::default()).await;

    let (seen, _logs) = send(&app(&db), Method::POST, path).await;

    assert_eq!(seen.status, 405);
    assert_eq!(seen.body["error"]["code"], "method_not_allowed");
    assert_eq!(seen.headers["allow"], "GET,HEAD");
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn liveness_answers_200_with_an_empty_body(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    assert_answers_200_with_an_empty_body(options, "/health/live", "live").await;
}

#[sqlx::test(migrations = false)]
async fn readiness_answers_200_with_an_empty_body_while_the_database_answers(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    assert_answers_200_with_an_empty_body(options, "/health/ready", "ready").await;
}

#[sqlx::test(migrations = false)]
async fn liveness_answers_head_without_a_body(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    assert_answers_head_without_a_body(options, "/health/live").await;
}

#[sqlx::test(migrations = false)]
async fn readiness_answers_head_without_a_body(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    assert_answers_head_without_a_body(options, "/health/ready").await;
}

#[sqlx::test(migrations = false)]
async fn liveness_takes_only_get_and_head(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    assert_takes_only_get_and_head(options, "/health/live").await;
}

#[sqlx::test(migrations = false)]
async fn readiness_takes_only_get_and_head(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    assert_takes_only_get_and_head(options, "/health/ready").await;
}

#[sqlx::test(migrations = false)]
async fn liveness_answers_even_when_the_database_is_closed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options, DatabaseOptions::default()).await;
    db.close().await;

    let (seen, _logs) = send(&app(&db), Method::GET, "/health/live").await;

    assert_eq!(seen.status, 200);
}

#[sqlx::test(migrations = false)]
async fn readiness_is_busy_while_no_read_connection_is_free(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(
        options,
        DatabaseOptions::default().with_acquire_timeout(SHORT_ACQUIRE_TIMEOUT),
    )
    .await;
    let mut held = Vec::new();
    for _ in 0..READ_CONNECTIONS {
        held.push(db.read_pool().acquire().await.unwrap());
    }

    let (seen, logs) = send(&app(&db), Method::GET, "/health/ready").await;

    assert_eq!(seen.status, 503);
    assert_eq!(seen.body["error"]["code"], "busy");
    assert_eq!(seen.headers["retry-after"], "1");
    let lines = logs.json_lines().unwrap();
    let warnings: Vec<&Value> = lines
        .iter()
        .filter(|line| line["level"] == "WARN")
        .collect();
    assert_eq!(warnings.len(), 1, "{}", logs.text());
    assert_eq!(warnings[0]["request_id"], seen.headers["x-request-id"]);
    insta::assert_json_snapshot!("health_ready_busy", redacted(seen));
    drop(held);
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn readiness_fails_with_the_internal_error_when_the_database_fails(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options, DatabaseOptions::default()).await;
    db.close().await;

    let (seen, logs) = send(&app(&db), Method::GET, "/health/ready").await;

    assert_eq!(seen.status, 500);
    assert_eq!(seen.body["error"]["code"], "internal");
    for detail in ["database", "pool", "closed", "sqlx"] {
        assert!(!seen.text.contains(detail), "{detail} in {}", seen.text);
    }
    let lines = logs.json_lines().unwrap();
    let errors: Vec<&Value> = lines
        .iter()
        .filter(|line| line["level"] == "ERROR")
        .collect();
    assert_eq!(errors.len(), 1, "{}", logs.text());
    assert_eq!(errors[0]["request_id"], seen.headers["x-request-id"]);
    assert!(
        errors[0]["error"]
            .as_str()
            .is_some_and(|chain| chain.starts_with("the database failed: ")),
        "{}",
        logs.text()
    );
    insta::assert_json_snapshot!("health_ready_internal", redacted(seen));
}
