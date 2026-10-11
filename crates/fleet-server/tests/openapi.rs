//! The `OpenAPI` document through the server's router (Plan.md P6.10,
//! ADR-0015): `GET /api/openapi.json` serves it in dev mode, and outside dev
//! mode the path answers like any unknown route.
//!
//! Each test gets a fresh database from `#[sqlx::test]`, which the router's
//! health service needs; time isn't paused (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{HeaderMap, Method};
use chrono::DateTime;
use fleet_core::auth::SessionPolicy;
use fleet_server::app::auth::TokenService;
use fleet_server::app::health::HealthService;
use fleet_server::config::{HttpConfig, RateLimitConfig};
use fleet_server::http::middleware::RequestIds;
use fleet_server::http::openapi::{ApiDocs, OPENAPI_PATH};
use fleet_server::http::rate_limit::{RateLimits, TrustedProxies};
use fleet_server::http::router::{AppState, router};
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_testkit::system::{ManualClock, SeededRandom};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tower::ServiceExt as _;

/// The session tokens over `db`, at §7.2's lifetimes. No test here
/// authenticates, but the state carries them.
fn tokens(db: &Database) -> TokenService {
    let random = SeededRandom::new(13);
    TokenService::new(
        Arc::new(ManualClock::new(
            DateTime::from_timestamp_millis(1_700_000_000_000).unwrap(),
        )),
        Arc::new(random.clone()),
        Arc::new(db.clone()),
        Arc::new(Sha256Tokens::new(Arc::new(random))),
        SessionPolicy::default(),
    )
}

/// The server's router over `db`, with or without the document.
fn app(db: &Database, docs: ApiDocs) -> Router {
    let ids = RequestIds::new(
        Arc::new(ManualClock::new(
            DateTime::from_timestamp_millis(1_700_000_000_000).unwrap(),
        )),
        Arc::new(SeededRandom::new(5)),
    );
    let http = HttpConfig {
        bind: HttpConfig::DEFAULT_BIND,
        request_timeout: Duration::from_secs(15),
        max_body_bytes: 65_536,
        trusted_proxies: TrustedProxies::default(),
    };
    let limits = RateLimits::new(
        &RateLimitConfig::default(),
        Arc::new(ManualClock::new(
            DateTime::from_timestamp_millis(1_700_000_000_000).unwrap(),
        )),
    );
    let state = AppState::new(HealthService::new(Arc::new(db.clone())), tokens(db), limits);
    router(state, &http, ids, docs)
        .layer(MockConnectInfo(SocketAddr::from(([192, 0, 2, 10], 4711))))
}

async fn open(options: SqliteConnectOptions) -> Database {
    Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap()
}

/// A response's status, headers and body.
struct Answer {
    status: u16,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Answer {
    /// The body as JSON.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }

    /// A header as text.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|value| value.to_str().unwrap())
    }
}

async fn send(app: &Router, method: Method, path: &str) -> Answer {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Answer {
        status,
        headers,
        body,
    }
}

/// The document dev mode serves.
async fn document(options: SqliteConnectOptions) -> Value {
    let db = open(options).await;
    let answer = send(&app(&db, ApiDocs::Served), Method::GET, OPENAPI_PATH).await;
    db.close().await;
    answer.json()
}

#[test]
fn only_dev_mode_serves_the_document() {
    assert_eq!(ApiDocs::for_dev_mode(true), ApiDocs::Served);
    assert_eq!(ApiDocs::for_dev_mode(false), ApiDocs::Hidden);
}

#[sqlx::test(migrations = false)]
async fn dev_mode_serves_the_document_as_json_with_the_security_headers(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;

    let answer = send(&app(&db, ApiDocs::Served), Method::GET, OPENAPI_PATH).await;

    assert_eq!(answer.status, 200);
    assert_eq!(answer.header("content-type"), Some("application/json"));
    assert_eq!(answer.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(answer.header("cache-control"), Some("no-store"));
    assert_eq!(
        answer.header("content-security-policy"),
        Some("default-src 'none'; frame-ancestors 'none'")
    );
    assert!(answer.header("x-request-id").is_some());
    insta::assert_json_snapshot!("openapi_document", answer.json());
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn head_on_the_document_answers_200_without_a_body(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;

    let answer = send(&app(&db, ApiDocs::Served), Method::HEAD, OPENAPI_PATH).await;

    assert_eq!(answer.status, 200);
    assert_eq!(answer.body, Vec::<u8>::new());
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn the_document_takes_only_get_and_head(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;

    let answer = send(&app(&db, ApiDocs::Served), Method::POST, OPENAPI_PATH).await;

    assert_eq!(answer.status, 405);
    assert_eq!(answer.header("allow"), Some("GET,HEAD"));
    assert_eq!(answer.json()["error"]["code"], "method_not_allowed");
    db.close().await;
}

/// `answer` without what differs between two requests: the request ID.
fn without_request_id(answer: &Answer) -> (u16, Vec<(String, String)>, Value) {
    let headers = answer
        .headers
        .iter()
        .filter(|(name, _)| name.as_str() != "x-request-id")
        .map(|(name, value)| (name.to_string(), value.to_str().unwrap().to_owned()))
        .collect();
    let mut body = answer.json();
    body["error"]["request_id"] = json!("[request_id]");
    (answer.status, headers, body)
}

#[sqlx::test(migrations = false)]
async fn outside_dev_mode_the_path_answers_like_an_unknown_route(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;
    let app = app(&db, ApiDocs::Hidden);

    for method in [Method::GET, Method::POST] {
        let hidden = send(&app, method.clone(), OPENAPI_PATH).await;
        let unknown = send(&app, method, "/api/no-such-route.json").await;

        assert_eq!(hidden.status, 404);
        assert_eq!(without_request_id(&hidden), without_request_id(&unknown));
    }
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn the_documents_schemas_are_exactly_the_api_types(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let document = document(options).await;

    let expected = serde_json::to_value(fleet_api_types::openapi::components()).unwrap();

    assert_eq!(document["components"]["schemas"], expected);
}

#[sqlx::test(migrations = false)]
async fn the_document_lists_exactly_the_api_routes(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let document = document(options).await;

    let paths: BTreeSet<(&str, &str)> = document["paths"]
        .as_object()
        .unwrap()
        .iter()
        .flat_map(|(path, operations)| {
            operations
                .as_object()
                .unwrap()
                .keys()
                .map(move |method| (path.as_str(), method.as_str()))
        })
        .collect();

    assert_eq!(
        paths,
        BTreeSet::from([("/health/live", "get"), ("/health/ready", "get")])
    );
}

#[sqlx::test(migrations = false)]
async fn every_operation_answers_any_other_status_with_the_error_envelope(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let document = document(options).await;

    for (path, operations) in document["paths"].as_object().unwrap() {
        for (method, operation) in operations.as_object().unwrap() {
            assert_eq!(
                operation["responses"]["default"]["content"]["application/json"]["schema"]["$ref"],
                "#/components/schemas/ErrorResponse",
                "{method} {path}"
            );
        }
    }
}

#[sqlx::test(migrations = false)]
async fn the_document_names_its_license_and_version_but_no_contact_or_server(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let document = document(options).await;

    assert_eq!(document["info"]["title"], "afkfleet");
    assert_eq!(document["info"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        document["info"]["license"]["identifier"],
        "GPL-3.0-or-later"
    );
    assert!(document["info"].get("contact").is_none());
    assert!(document.get("servers").is_none());
}
