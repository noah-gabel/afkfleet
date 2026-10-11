//! The route registry and the route-coverage test (Plan.md P7.11, security
//! rule 1, ADR-0016).
//!
//! - The public routes are exactly the allowlist, outside dev mode and in
//!   it, each with its rate-limit class.
//! - Every authed route answers 401 without credentials. The production
//!   router has no authed route before group D, so the test adds a
//!   test-only one through `authed_route` whose handler takes `AuthUser`:
//!   the authed list is never empty, and D's and E's routes join the check
//!   on their own. A route registered as authed whose handler forgets
//!   `AuthUser` is caught.
//! - The registry lists exactly the `OpenAPI` document's operations, so a
//!   route added past the helpers fails.
//!
//! Each test gets a fresh database from `#[sqlx::test]`; time isn't paused
//! (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::collections::BTreeSet;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::Method;
use chrono::DateTime;
use fleet_core::auth::SessionPolicy;
use fleet_server::app::auth::TokenService;
use fleet_server::app::health::HealthService;
use fleet_server::config::HttpConfig;
use fleet_server::http::extractors::AuthUser;
use fleet_server::http::middleware::RequestIds;
use fleet_server::http::openapi::{ApiDocs, OPENAPI_PATH};
use fleet_server::http::router::{AppState, api_routes};
use fleet_server::http::routes::{Access, RateClass, RouteEntry, Routes};
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_testkit::system::{ManualClock, SeededRandom};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tower::ServiceExt as _;
use utoipa_axum::routes;

/// What stands in for a path parameter (`{id}`) when a route is called: a
/// version 7 UUID, the shape of every ID in a path.
const PATH_PARAMETER: &str = "01890000-0000-7000-8000-000000000000";

/// A test-only authed route: who asks.
#[utoipa::path(
    get,
    path = "/test/whoami",
    responses((status = 200, description = "The caller's user ID.")),
)]
async fn whoami(user: AuthUser) -> String {
    user.actor().id.to_string()
}

/// A route registered as authed whose handler forgot `AuthUser`.
#[utoipa::path(
    get,
    path = "/test/forgot-auth-user",
    responses((status = 200, description = "Always.")),
)]
async fn forgot_auth_user() -> &'static str {
    "ok"
}

async fn open(options: SqliteConnectOptions) -> Database {
    Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap()
}

fn clock() -> ManualClock {
    ManualClock::new(DateTime::from_timestamp_millis(1_700_000_000_000).unwrap())
}

fn state(db: &Database) -> AppState {
    let random = SeededRandom::new(13);
    let tokens = TokenService::new(
        Arc::new(clock()),
        Arc::new(random.clone()),
        Arc::new(db.clone()),
        Arc::new(Sha256Tokens::new(Arc::new(random))),
        SessionPolicy::default(),
    );
    AppState::new(HealthService::new(Arc::new(db.clone())), tokens)
}

fn http() -> HttpConfig {
    HttpConfig {
        bind: HttpConfig::DEFAULT_BIND,
        request_timeout: Duration::from_secs(15),
        max_body_bytes: 65_536,
    }
}

/// The production routes, plus whatever `extra` adds, finished.
fn build(
    db: &Database,
    docs: ApiDocs,
    extra: impl FnOnce(Routes) -> Routes,
) -> (Router, Vec<RouteEntry>) {
    let ids = RequestIds::new(Arc::new(clock()), Arc::new(SeededRandom::new(5)));
    extra(api_routes(Routes::new())).finish(state(db), &http(), ids, docs)
}

/// The routes with `access`, as (method, path, class).
fn with_access(entries: &[RouteEntry], access: Access) -> BTreeSet<(Method, String, RateClass)> {
    entries
        .iter()
        .filter(|entry| entry.access == access)
        .map(|entry| (entry.method.clone(), entry.path.clone(), entry.class))
        .collect()
}

fn entry(method: Method, path: &str, class: RateClass) -> (Method, String, RateClass) {
    (method, path.to_owned(), class)
}

/// `path` with every parameter (`{id}`) replaced by a stand-in.
fn concrete(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            if segment.starts_with('{') && segment.ends_with('}') {
                PATH_PARAMETER
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Every authed route of `entries` that didn't answer a request without
/// credentials with 401, `WWW-Authenticate: Bearer` and the `unauthorized`
/// code, as "METHOD path: what it answered".
async fn authed_routes_without_401(app: &Router, entries: &[RouteEntry]) -> Vec<String> {
    let mut offenders = Vec::new();
    for route in entries
        .iter()
        .filter(|entry| entry.access == Access::Authed)
    {
        let request = Request::builder()
            .method(route.method.clone())
            .uri(concrete(&route.path))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let challenge = response
            .headers()
            .get("www-authenticate")
            .map(|value| value.to_str().unwrap().to_owned());
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let code = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|body| body["error"]["code"].as_str().map(str::to_owned));
        if status != 401
            || challenge.as_deref() != Some("Bearer")
            || code.as_deref() != Some("unauthorized")
        {
            offenders.push(format!(
                "{} {}: {status} {challenge:?} {code:?}",
                route.method, route.path
            ));
        }
    }
    offenders
}

#[sqlx::test(migrations = false)]
async fn outside_dev_mode_the_public_routes_are_exactly_the_allowlist(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;

    let (_, entries) = build(&db, ApiDocs::Hidden, |routes| routes);

    assert_eq!(
        with_access(&entries, Access::Public),
        BTreeSet::from([
            entry(Method::GET, "/health/live", RateClass::Standard),
            entry(Method::GET, "/health/ready", RateClass::Standard),
        ])
    );
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn dev_mode_adds_only_the_document_to_the_public_routes(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;

    let (_, entries) = build(&db, ApiDocs::Served, |routes| routes);

    assert_eq!(
        with_access(&entries, Access::Public),
        BTreeSet::from([
            entry(Method::GET, "/health/live", RateClass::Standard),
            entry(Method::GET, "/health/ready", RateClass::Standard),
            entry(Method::GET, OPENAPI_PATH, RateClass::Standard),
        ])
    );
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn every_authed_route_answers_401_without_credentials(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;
    for docs in [ApiDocs::Hidden, ApiDocs::Served] {
        let (app, entries) = build(&db, docs, |routes| {
            routes.authed_route(RateClass::Standard, routes!(whoami))
        });

        let authed = with_access(&entries, Access::Authed);
        assert!(
            authed.contains(&entry(Method::GET, "/test/whoami", RateClass::Standard)),
            "the check never runs on an empty list: {authed:?}"
        );
        assert_eq!(
            authed_routes_without_401(&app, &entries).await,
            Vec::<String>::new()
        );
    }
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn the_check_catches_an_authed_route_that_doesnt_ask_for_credentials(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;
    let (app, entries) = build(&db, ApiDocs::Hidden, |routes| {
        routes
            .authed_route(RateClass::Standard, routes!(whoami))
            .authed_route(RateClass::Standard, routes!(forgot_auth_user))
    });

    let offenders = authed_routes_without_401(&app, &entries).await;

    assert_eq!(offenders.len(), 1, "{offenders:?}");
    assert!(
        offenders[0].starts_with("GET /test/forgot-auth-user: 200"),
        "{offenders:?}"
    );
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn each_registration_records_its_class(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;

    let (_, entries) = build(&db, ApiDocs::Hidden, |routes| {
        routes
            .authed_route(RateClass::Login, routes!(whoami))
            .public_route(RateClass::Register, routes!(forgot_auth_user))
    });

    assert!(entries.contains(&RouteEntry {
        method: Method::GET,
        path: "/test/whoami".to_owned(),
        access: Access::Authed,
        class: RateClass::Login,
    }));
    assert!(entries.contains(&RouteEntry {
        method: Method::GET,
        path: "/test/forgot-auth-user".to_owned(),
        access: Access::Public,
        class: RateClass::Register,
    }));
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn the_registry_lists_exactly_the_documents_operations(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;
    let (app, entries) = build(&db, ApiDocs::Served, |routes| routes);
    let request = Request::builder()
        .uri(OPENAPI_PATH)
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let document: Value = serde_json::from_slice(&body).unwrap();

    let documented: BTreeSet<(String, String)> = document["paths"]
        .as_object()
        .unwrap()
        .iter()
        .flat_map(|(path, operations)| {
            operations
                .as_object()
                .unwrap()
                .keys()
                .map(move |method| (method.to_uppercase(), path.clone()))
        })
        .collect();
    let registered: BTreeSet<(String, String)> = entries
        .iter()
        .filter(|entry| entry.path != OPENAPI_PATH)
        .map(|entry| (entry.method.to_string(), entry.path.clone()))
        .collect();

    assert!(!registered.is_empty());
    assert_eq!(registered, documented);
    db.close().await;
}

#[test]
fn a_path_parameter_is_replaced_by_an_id() {
    assert_eq!(
        concrete("/api/v1/bots/{id}/chat"),
        format!("/api/v1/bots/{PATH_PARAMETER}/chat")
    );
    assert_eq!(concrete("/health/live"), "/health/live");
}
