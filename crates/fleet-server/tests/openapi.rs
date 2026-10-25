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
use fleet_server::app::challenges::Challenges;
use fleet_server::app::health::HealthService;
use fleet_server::app::login::LoginService;
use fleet_server::app::mfa::{MfaPorts, MfaService};
use fleet_server::config::{HttpConfig, RateLimitConfig};
use fleet_server::http::middleware::RequestIds;
use fleet_server::http::openapi::{ApiDocs, OPENAPI_PATH};
use fleet_server::http::rate_limit::{RateLimits, TrustedProxies};
use fleet_server::http::router::{AppState, Services, router};
use fleet_server::infra::crypto::attempt_keys::HmacAttemptKeys;
use fleet_server::infra::crypto::passwords::Argon2Passwords;
use fleet_server::infra::crypto::recovery_codes::Sha256RecoveryCodes;
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::infra::crypto::totp::TotpRs;
use fleet_server::infra::crypto::vault::{VaultKeys, XChaChaVault};
use fleet_server::infra::qr::SvgQr;
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

/// The 2FA service over `db`, its vault on an in-memory key. No test here
/// enrolls, but the state carries it.
fn mfa(db: &Database) -> MfaService {
    let clock = Arc::new(ManualClock::new(
        chrono::DateTime::from_timestamp_millis(1_700_000_000_000).unwrap(),
    ));
    let random = SeededRandom::new(17);
    let keys = VaultKeys::from_keys(
        std::collections::BTreeMap::from([(1, secrecy::SecretBox::new(Box::new([0x42; 32])))]),
        1,
    )
    .unwrap();
    MfaService::new(
        clock.clone(),
        Arc::new(random.clone()),
        Arc::new(db.clone()),
        Arc::new(XChaChaVault::new(keys, Arc::new(random.clone()))),
        MfaPorts {
            totp: Arc::new(TotpRs::new()),
            recovery: Arc::new(Sha256RecoveryCodes::new()),
            qr: Arc::new(SvgQr::new()),
        },
        Challenges::new(clock, Arc::new(Sha256Tokens::new(Arc::new(random)))),
    )
}

/// The login over `db`, with `tokens` and `mfa`. No test here logs in, but
/// the state carries it.
async fn login(db: &Database, tokens: &TokenService, mfa: &MfaService) -> LoginService {
    let clock = Arc::new(ManualClock::new(
        chrono::DateTime::from_timestamp_millis(1_700_000_000_000).unwrap(),
    ));
    let passwords = Argon2Passwords::new(
        fleet_server::config::Argon2Params::default(),
        Arc::new(SeededRandom::new(19)),
    )
    .await
    .unwrap();
    let lockout = fleet_server::app::lockout::LockoutService::new(
        clock.clone(),
        Arc::new(db.clone()),
        Arc::new(HmacAttemptKeys::new(secrecy::SecretBox::new(Box::new(
            [7; 32],
        )))),
        fleet_core::auth::LockoutPolicy::default(),
    );
    LoginService::new(
        clock,
        Arc::new(SeededRandom::new(23)),
        Arc::new(db.clone()),
        Arc::new(passwords),
        lockout,
        tokens.clone(),
        mfa.clone(),
    )
}

/// The users' and invites' services over `db`. No test here uses them, but
/// the state carries them.
async fn accounts(
    db: &Database,
) -> (
    fleet_server::app::users::UserService,
    fleet_server::app::invites::InviteService,
) {
    let clock = Arc::new(ManualClock::new(
        chrono::DateTime::from_timestamp_millis(1_700_000_000_000).unwrap(),
    ));
    let random = Arc::new(SeededRandom::new(23));
    let passwords = Argon2Passwords::new(
        fleet_server::config::Argon2Params::default(),
        random.clone(),
    )
    .await
    .unwrap();
    let users = fleet_server::app::users::UserService::new(
        clock.clone(),
        random.clone(),
        Arc::new(db.clone()),
        Arc::new(passwords),
    );
    let invites = fleet_server::app::invites::InviteService::new(
        clock,
        random.clone(),
        Arc::new(db.clone()),
        Arc::new(fleet_server::infra::crypto::tokens::Sha256Tokens::new(
            random,
        )),
        fleet_core::auth::InviteTtl::default(),
    );
    (users, invites)
}

/// The server's router over `db`, with or without the document.
async fn app(db: &Database, docs: ApiDocs) -> Router {
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
    let tokens = tokens(db);
    let mfa = mfa(db);
    let login = login(db, &tokens, &mfa).await;
    let (users, invites) = accounts(db).await;
    let state = AppState::new(Services {
        health: HealthService::new(Arc::new(db.clone())),
        tokens,
        limits,
        mfa,
        login,
        users,
        invites,
        link_flows: idle_link_flows(db),
        accounts: idle_accounts(db),
        grants: idle_grants(db),
        enrollment_tokens: None,
        agents: idle_agents(db),
        bots: idle_bots(db),
        modes: idle_modes(db),
        events: idle_events(db),
        audit: fleet_server::app::audit::AuditService::new(
            std::sync::Arc::new(fleet_testkit::system::ManualClock::new(
                chrono::DateTime::UNIX_EPOCH,
            )),
            std::sync::Arc::new(db.clone()),
        ),
    });
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
    let answer = send(&app(&db, ApiDocs::Served).await, Method::GET, OPENAPI_PATH).await;
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

    let answer = send(&app(&db, ApiDocs::Served).await, Method::GET, OPENAPI_PATH).await;

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

    let answer = send(&app(&db, ApiDocs::Served).await, Method::HEAD, OPENAPI_PATH).await;

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

    let answer = send(&app(&db, ApiDocs::Served).await, Method::POST, OPENAPI_PATH).await;

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
    let app = app(&db, ApiDocs::Hidden).await;

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
        BTreeSet::from([
            ("/api/v1/accounts", "get"),
            ("/api/v1/accounts/awaiting-approval", "get"),
            ("/api/v1/accounts/link", "post"),
            ("/api/v1/accounts/link/{flow_id}", "get"),
            ("/api/v1/accounts/offline", "post"),
            ("/api/v1/accounts/{id}", "delete"),
            ("/api/v1/accounts/{id}", "get"),
            ("/api/v1/accounts/{id}/approve", "post"),
            ("/api/v1/accounts/{id}/grants", "get"),
            ("/api/v1/accounts/{id}/grants", "post"),
            ("/api/v1/accounts/{id}/grants/{user}", "delete"),
            ("/api/v1/accounts/{id}/grants/{user}", "put"),
            ("/api/v1/accounts/{id}/refuse", "post"),
            ("/api/v1/accounts/{id}/relink", "post"),
            ("/api/v1/agents", "get"),
            ("/api/v1/agents/enrollment-tokens", "post"),
            ("/api/v1/agents/{id}/disable", "post"),
            ("/api/v1/agents/{id}/release", "post"),
            ("/api/v1/audit", "get"),
            ("/api/v1/users/{id}/grants", "get"),
            ("/api/v1/users/{id}/shared-accounts", "get"),
            ("/api/v1/bots", "get"),
            ("/api/v1/bots/{id}", "get"),
            ("/api/v1/bots/{id}", "patch"),
            ("/api/v1/bots/{id}/mode", "put"),
            ("/api/v1/bots/{id}/start", "post"),
            ("/api/v1/bots/{id}/stop", "post"),
            ("/api/v1/bots/{id}/restart", "post"),
            ("/api/v1/bots/{id}/reset", "post"),
            ("/api/v1/bots/{id}/resume", "post"),
            ("/api/v1/bots/{id}/chat", "get"),
            ("/api/v1/bots/{id}/chat", "post"),
            ("/api/v1/bots/{id}/command", "post"),
            ("/api/v1/modes", "get"),
            ("/api/v1/modes", "post"),
            ("/api/v1/modes/with-commands", "post"),
            ("/api/v1/modes/{id}", "put"),
            ("/api/v1/modes/{id}", "delete"),
            ("/api/v1/modes/{id}/with-commands", "put"),
            ("/api/v1/events/ticket", "post"),
            ("/api/v1/events", "get"),
            ("/api/v1/auth/login", "post"),
            ("/api/v1/auth/login/mfa", "post"),
            ("/api/v1/auth/logout", "post"),
            ("/api/v1/auth/refresh", "post"),
            ("/api/v1/auth/register", "post"),
            ("/api/v1/auth/step-up", "post"),
            ("/api/v1/invites", "get"),
            ("/api/v1/invites", "post"),
            ("/api/v1/invites/{id}", "delete"),
            ("/api/v1/me", "get"),
            ("/api/v1/me/mfa/confirm", "post"),
            ("/api/v1/me/mfa/enroll", "post"),
            ("/api/v1/me/mfa/reset", "post"),
            ("/api/v1/me/password", "post"),
            ("/api/v1/me/sessions", "get"),
            ("/api/v1/me/sessions/{id}", "delete"),
            ("/api/v1/users", "get"),
            ("/api/v1/users/{id}", "patch"),
            ("/health/live", "get"),
            ("/health/ready", "get"),
        ])
    );
}

#[sqlx::test(migrations = false)]
async fn every_operation_id_is_unique(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let document = document(options).await;

    let ids: Vec<&str> = document["paths"]
        .as_object()
        .unwrap()
        .values()
        .flat_map(|operations| operations.as_object().unwrap().values())
        .map(|operation| operation["operationId"].as_str().unwrap())
        .collect();
    let unique: BTreeSet<&str> = ids.iter().copied().collect();

    assert_eq!(unique.len(), ids.len(), "{ids:?}");
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

/// The link flows, which these tests never start (P9.4): a service over
/// `db` with Microsoft's real client, never called.
fn idle_link_flows(
    db: &fleet_server::infra::sqlite::Database,
) -> fleet_server::app::link_flows::LinkFlowService {
    use fleet_server::app::link_flows::{LinkFlowPorts, LinkFlowService, LinkFlowSettings};
    let clock: Arc<dyn fleet_core::system::Clock> =
        Arc::new(fleet_testkit::system::ManualClock::new(
            chrono::DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
        ));
    let random: Arc<dyn fleet_core::system::SecureRandom> =
        Arc::new(fleet_testkit::system::SeededRandom::new(3));
    let keys = fleet_server::infra::crypto::vault::VaultKeys::from_keys(
        std::collections::BTreeMap::from([(1, secrecy::SecretBox::new(Box::new([1_u8; 32])))]),
        1,
    )
    .unwrap();
    LinkFlowService::new(
        LinkFlowPorts {
            live_hints: std::sync::Arc::new(fleet_server::ports::live_hints::NoLiveHints),
            microsoft: Arc::new(
                fleet_server::infra::msauth::ReqwestMicrosoftAuth::new(Arc::clone(&clock)).unwrap(),
            ),
            vault: Arc::new(fleet_server::infra::crypto::vault::XChaChaVault::new(
                keys,
                Arc::clone(&random),
            )),
            store: Arc::new(db.clone()),
            pace: Arc::new(fleet_server::infra::system::TokioPace),
            clock,
            random,
        },
        LinkFlowSettings {
            total: core::num::NonZeroU32::MIN,
            per_user_per_day: core::num::NonZeroU32::MIN,
            max_per_member: core::num::NonZeroU32::MIN,
        },
    )
}

/// The grants, which these tests never write.
fn idle_grants(
    db: &fleet_server::infra::sqlite::Database,
) -> fleet_server::app::grants::GrantService {
    fleet_server::app::grants::GrantService::new(
        Arc::new(fleet_testkit::system::ManualClock::new(
            chrono::DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
        )),
        Arc::new(db.clone()),
    )
}

/// The account service, which these tests never call (P9.7).
fn idle_accounts(
    db: &fleet_server::infra::sqlite::Database,
) -> fleet_server::app::accounts::AccountService {
    fleet_server::app::accounts::AccountService::new(
        Arc::new(fleet_testkit::system::ManualClock::new(
            chrono::DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
        )),
        Arc::new(fleet_testkit::system::SeededRandom::new(4)),
        Arc::new(db.clone()),
        fleet_server::app::accounts::AccountSettings {
            allow_offline: false,
            max_per_member: core::num::NonZeroU32::MIN,
        },
    )
}

/// The agents' service, which these tests never call (P10.10).
fn idle_agents(
    db: &fleet_server::infra::sqlite::Database,
) -> fleet_server::app::agents::AgentService {
    let clock = Arc::new(fleet_testkit::system::ManualClock::new(
        chrono::DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
    ));
    fleet_server::app::agents::AgentService::new(
        clock.clone(),
        Arc::new(db.clone()),
        Arc::new(fleet_server::grpc::registry::AgentRegistry::new(clock)),
    )
}

/// The bots' service, which these tests never call (P11.2).
fn idle_bots(db: &fleet_server::infra::sqlite::Database) -> fleet_server::app::bots::BotService {
    let clock = Arc::new(fleet_testkit::system::ManualClock::new(
        chrono::DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
    ));
    fleet_server::app::bots::BotService::new(
        clock.clone(),
        Arc::new(fleet_testkit::system::SeededRandom::new(1)),
        Arc::new(db.clone()),
        Arc::new(fleet_server::grpc::registry::AgentRegistry::new(clock)),
        fleet_server::app::events::EventBus::new(),
        false,
    )
}

/// The modes' service, which these tests never write through (P11.4).
fn idle_modes(db: &fleet_server::infra::sqlite::Database) -> fleet_server::app::modes::ModeService {
    fleet_server::app::modes::ModeService::new(
        Arc::new(fleet_testkit::system::ManualClock::new(
            chrono::DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
        )),
        Arc::new(fleet_testkit::system::SeededRandom::new(1)),
        Arc::new(db.clone()),
    )
}

/// The live events with no ticket held and no socket open (P11.5).
fn idle_events(db: &fleet_server::infra::sqlite::Database) -> fleet_server::http::ws::LiveEvents {
    let clock = Arc::new(fleet_testkit::system::ManualClock::new(
        chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
    ));
    let tokens = fleet_server::infra::crypto::tokens::Sha256Tokens::new(Arc::new(
        fleet_testkit::system::SeededRandom::new(77),
    ));
    fleet_server::http::ws::LiveEvents::new(
        fleet_server::app::event_tickets::EventTickets::new(clock, Arc::new(tokens)),
        fleet_server::app::events::EventBus::new(),
        fleet_server::http::ws::LiveSockets::new(tokio_util::sync::CancellationToken::new()),
        fleet_server::app::events::LiveRights::new(Arc::new(db.clone())),
    )
}
