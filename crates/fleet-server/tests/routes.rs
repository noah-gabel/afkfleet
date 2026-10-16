//! The route registry and the route-coverage test (Plan.md P7.11, security
//! rule 1, ADR-0016).
//!
//! - The public routes are exactly the allowlist, outside dev mode and in
//!   it, each with its rate-limit class.
//! - Every route that needs credentials (authed, step-up and setup routes)
//!   answers 401 without them. The test adds a test-only authed route whose
//!   handler takes `AuthUser`, so the authed list is never empty, and every
//!   later route joins the check on its own. A route registered as authed
//!   whose handler forgets `AuthUser` is caught.
//! - A live setup token is refused on every authed and step-up route, and a
//!   live access token on every setup route (P7.7).
//! - Every step-up route answers 403 `step_up_required` to a live session
//!   that hasn't stepped up, and a step-up route whose handler forgets the
//!   check is caught.
//! - The registry lists exactly the `OpenAPI` document's operations, and
//!   each operation's security and step-up mark match its access, so a route
//!   added past the helpers fails.
//!
//! Each test gets a fresh database from `#[sqlx::test]`; time isn't paused
//! (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::Method;
use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::auth::SessionPolicy;
use fleet_core::authz::Role;
use fleet_core::id::{LoginStepId, UserId};
use fleet_core::value::Username;
use fleet_server::app::auth::TokenService;
use fleet_server::app::challenges::{ChallengeUser, Challenges, SetupPurpose};
use fleet_server::app::health::HealthService;
use fleet_server::app::login::LoginService;
use fleet_server::app::mfa::{MfaPorts, MfaService};
use fleet_server::config::{HttpConfig, RateLimitConfig};
use fleet_server::http::extractors::AuthUser;
use fleet_server::http::middleware::RequestIds;
use fleet_server::http::openapi::{ApiDocs, OPENAPI_PATH};
use fleet_server::http::rate_limit::{RateLimits, TrustedProxies};
use fleet_server::http::router::{AppState, Services, api_routes};
use fleet_server::http::routes::{Access, RateClass, RouteEntry, Routes};
use fleet_server::infra::crypto::attempt_keys::HmacAttemptKeys;
use fleet_server::infra::crypto::passwords::Argon2Passwords;
use fleet_server::infra::crypto::recovery_codes::Sha256RecoveryCodes;
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::infra::crypto::totp::TotpRs;
use fleet_server::infra::crypto::vault::{VaultKeys, XChaChaVault};
use fleet_server::infra::qr::SvgQr;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::store::Store as _;
use fleet_server::ports::users::{NewUser, PasswordHash};
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::{ExposeSecret as _, SecretBox, SecretString};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tower::ServiceExt as _;
use utoipa_axum::routes;

/// What stands in for a path parameter (`{id}`) when a route is called: a
/// version 7 UUID, the shape of every ID in a path.
const PATH_PARAMETER: &str = "01890000-0000-7000-8000-000000000000";

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";

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

/// A route registered as a step-up route whose handler takes `AuthUser`
/// instead of `SteppedUpUser`.
#[utoipa::path(
    post,
    path = "/test/forgot-step-up",
    responses((status = 200, description = "Always, for any session.")),
)]
async fn forgot_step_up(user: AuthUser) -> String {
    user.actor().id.to_string()
}

async fn open(options: SqliteConnectOptions) -> Database {
    Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap()
}

fn start() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(1_700_000_000_000).unwrap()
}

fn clock() -> ManualClock {
    ManualClock::new(start())
}

/// The default limits, on the tests' clock.
fn limits() -> RateLimits {
    RateLimits::new(&RateLimitConfig::default(), Arc::new(clock()))
}

/// What a test needs besides the router: the services that make
/// credentials.
struct Built {
    app: Router,
    entries: Vec<RouteEntry>,
    tokens: TokenService,
    challenges: Challenges,
}

fn http() -> HttpConfig {
    HttpConfig {
        bind: HttpConfig::DEFAULT_BIND,
        request_timeout: Duration::from_secs(15),
        max_body_bytes: 65_536,
        trusted_proxies: TrustedProxies::default(),
    }
}

/// The production routes, plus whatever `extra` adds, finished.
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

async fn build(db: &Database, docs: ApiDocs, extra: impl FnOnce(Routes) -> Routes) -> Built {
    let random = SeededRandom::new(13);
    let token_port = Arc::new(Sha256Tokens::new(Arc::new(random.clone())));
    let tokens = TokenService::new(
        Arc::new(clock()),
        Arc::new(random.clone()),
        Arc::new(db.clone()),
        token_port.clone(),
        SessionPolicy::default(),
    );
    let challenges = Challenges::new(Arc::new(clock()), token_port);
    let keys = VaultKeys::from_keys(
        BTreeMap::from([(1, SecretBox::new(Box::new([0x42; 32])))]),
        1,
    )
    .unwrap();
    let mfa = MfaService::new(
        Arc::new(clock()),
        Arc::new(random.clone()),
        Arc::new(db.clone()),
        Arc::new(XChaChaVault::new(keys, Arc::new(random))),
        MfaPorts {
            totp: Arc::new(TotpRs::new()),
            recovery: Arc::new(Sha256RecoveryCodes::new()),
            qr: Arc::new(SvgQr::new()),
        },
        challenges.clone(),
    );
    let limits = limits();
    let login = login(db, &tokens, &mfa).await;
    let (users, invites) = accounts(db).await;
    let state = AppState::new(Services {
        health: HealthService::new(Arc::new(db.clone())),
        tokens: tokens.clone(),
        limits: limits.clone(),
        mfa,
        login,
        users,
        invites,
        link_flows: idle_link_flows(db),
        accounts: idle_accounts(db),
        grants: idle_grants(db),
        enrollment_tokens: None,
        agents: idle_agents(db),
    });
    let ids = RequestIds::new(Arc::new(clock()), Arc::new(SeededRandom::new(5)));
    let (app, entries) = extra(api_routes(Routes::new(&limits))).finish(state, &http(), ids, docs);
    Built {
        app: app.layer(MockConnectInfo(SocketAddr::from(([192, 0, 2, 10], 4711)))),
        entries,
        tokens,
        challenges,
    }
}

/// The test-only audit entry a setup commits with.
fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        start(),
        AuditAction::try_from("test.routes_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// Adds a Member, and returns their ID.
async fn add_user(db: &Database) -> UserId {
    let id = UserId::new_v7(start(), [1; 10]).unwrap();
    let mut tx = db.write().await.unwrap();
    tx.users()
        .insert(&NewUser {
            id,
            username: Username::try_from("alice").unwrap(),
            password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
            role: Role::Member,
            created_at: start(),
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    id
}

/// A live access token of `user`, whose session never stepped up.
async fn access_token(db: &Database, tokens: &TokenService, user: UserId) -> SecretString {
    let prepared = tokens.prepare(user).unwrap();
    let mut tx = db.write().await.unwrap();
    tx.sessions().insert(&prepared.new_session).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    prepared.tokens.access_token
}

/// A live setup token of `user` for a first enrollment.
fn setup_token(challenges: &Challenges, user: UserId) -> SecretString {
    challenges
        .issue_setup(
            ChallengeUser {
                id: user,
                role: Role::Member,
                password_changed_at: start(),
            },
            SetupPurpose::FirstEnrollment,
        )
        .unwrap()
        .token
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

/// What a route answered: the status, the `WWW-Authenticate` header and the
/// error code.
async fn call(
    app: &Router,
    route: &RouteEntry,
    bearer: Option<&SecretString>,
) -> (u16, Option<String>, Option<String>) {
    let mut request = Request::builder()
        .method(route.method.clone())
        .uri(concrete(&route.path));
    if let Some(token) = bearer {
        request = request.header("authorization", format!("Bearer {}", token.expose_secret()));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let challenge = response
        .headers()
        .get("www-authenticate")
        .map(|value| value.to_str().unwrap().to_owned());
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let code = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|body| body["error"]["code"].as_str().map(str::to_owned));
    (status, challenge, code)
}

/// Every route of `entries` with an access in `accesses` that didn't
/// answer `bearer` (none: no credentials) with 401, `WWW-Authenticate:
/// Bearer` and the `unauthorized` code, as "METHOD path: what it answered".
async fn routes_without_401(
    app: &Router,
    entries: &[RouteEntry],
    accesses: &[Access],
    bearer: Option<&SecretString>,
) -> Vec<String> {
    let mut offenders = Vec::new();
    for route in entries
        .iter()
        .filter(|entry| accesses.contains(&entry.access))
    {
        let (status, challenge, code) = call(app, route, bearer).await;
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

/// Every access that needs credentials.
const NEEDS_CREDENTIALS: [Access; 3] = [Access::Authed, Access::AuthedStepUp, Access::Setup];

/// Every access an access token is for.
const NEEDS_ACCESS_TOKEN: [Access; 2] = [Access::Authed, Access::AuthedStepUp];

#[sqlx::test(migrations = false)]
async fn outside_dev_mode_the_public_routes_are_exactly_the_allowlist(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;

    let built = build(&db, ApiDocs::Hidden, |routes| routes).await;

    assert_eq!(
        with_access(&built.entries, Access::Public),
        BTreeSet::from([
            entry(Method::GET, "/health/live", RateClass::Standard),
            entry(Method::GET, "/health/ready", RateClass::Standard),
            entry(Method::POST, "/api/v1/auth/login", RateClass::Login),
            entry(Method::POST, "/api/v1/auth/login/mfa", RateClass::Login),
            entry(Method::POST, "/api/v1/auth/refresh", RateClass::Refresh),
            entry(Method::POST, "/api/v1/auth/register", RateClass::Register),
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

    let built = build(&db, ApiDocs::Served, |routes| routes).await;

    assert_eq!(
        with_access(&built.entries, Access::Public),
        BTreeSet::from([
            entry(Method::GET, "/health/live", RateClass::Standard),
            entry(Method::GET, "/health/ready", RateClass::Standard),
            entry(Method::POST, "/api/v1/auth/login", RateClass::Login),
            entry(Method::POST, "/api/v1/auth/login/mfa", RateClass::Login),
            entry(Method::POST, "/api/v1/auth/refresh", RateClass::Refresh),
            entry(Method::POST, "/api/v1/auth/register", RateClass::Register),
            entry(Method::GET, OPENAPI_PATH, RateClass::Standard),
        ])
    );
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn the_setup_and_step_up_routes_are_registered_with_their_access_and_class(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;

    let built = build(&db, ApiDocs::Hidden, |routes| routes).await;

    assert_eq!(
        with_access(&built.entries, Access::Setup),
        BTreeSet::from([
            entry(Method::POST, "/api/v1/me/mfa/enroll", RateClass::Standard),
            entry(Method::POST, "/api/v1/me/mfa/confirm", RateClass::Standard),
        ])
    );
    assert_eq!(
        with_access(&built.entries, Access::AuthedStepUp),
        BTreeSet::from([
            entry(Method::POST, "/api/v1/me/mfa/reset", RateClass::Standard),
            entry(Method::POST, "/api/v1/me/password", RateClass::Login),
            entry(Method::PATCH, "/api/v1/users/{id}", RateClass::Standard),
            entry(Method::POST, "/api/v1/invites", RateClass::Standard),
            entry(
                Method::POST,
                "/api/v1/agents/enrollment-tokens",
                RateClass::Standard
            ),
            entry(
                Method::POST,
                "/api/v1/agents/{id}/disable",
                RateClass::Standard
            ),
            entry(Method::DELETE, "/api/v1/invites/{id}", RateClass::Standard),
            entry(Method::DELETE, "/api/v1/accounts/{id}", RateClass::Standard),
            entry(
                Method::POST,
                "/api/v1/accounts/{id}/approve",
                RateClass::Standard
            ),
            entry(
                Method::POST,
                "/api/v1/accounts/{id}/refuse",
                RateClass::Standard
            ),
            entry(
                Method::POST,
                "/api/v1/accounts/{id}/grants",
                RateClass::Standard
            ),
            entry(
                Method::PUT,
                "/api/v1/accounts/{id}/grants/{user}",
                RateClass::Standard
            ),
            entry(
                Method::DELETE,
                "/api/v1/accounts/{id}/grants/{user}",
                RateClass::Standard
            ),
        ])
    );
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn the_authed_routes_are_registered_with_their_class(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;

    let built = build(&db, ApiDocs::Hidden, |routes| routes).await;

    assert_eq!(
        with_access(&built.entries, Access::Authed),
        BTreeSet::from([
            entry(Method::POST, "/api/v1/auth/logout", RateClass::Standard),
            entry(Method::POST, "/api/v1/auth/step-up", RateClass::Login),
            entry(Method::GET, "/api/v1/me", RateClass::Standard),
            entry(Method::GET, "/api/v1/me/sessions", RateClass::Standard),
            entry(
                Method::DELETE,
                "/api/v1/me/sessions/{id}",
                RateClass::Standard
            ),
            entry(Method::GET, "/api/v1/users", RateClass::Standard),
            entry(Method::GET, "/api/v1/agents", RateClass::Standard),
            entry(Method::GET, "/api/v1/invites", RateClass::Standard),
            entry(Method::POST, "/api/v1/accounts/link", RateClass::Standard),
            entry(
                Method::GET,
                "/api/v1/accounts/link/{flow_id}",
                RateClass::Standard
            ),
            entry(
                Method::POST,
                "/api/v1/accounts/offline",
                RateClass::Standard
            ),
            entry(Method::GET, "/api/v1/accounts", RateClass::Standard),
            entry(
                Method::GET,
                "/api/v1/accounts/awaiting-approval",
                RateClass::Standard
            ),
            entry(Method::GET, "/api/v1/accounts/{id}", RateClass::Standard),
            entry(
                Method::POST,
                "/api/v1/accounts/{id}/relink",
                RateClass::Standard
            ),
            entry(
                Method::GET,
                "/api/v1/accounts/{id}/grants",
                RateClass::Standard
            ),
        ])
    );
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn every_route_that_needs_credentials_answers_401_without_them(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;
    for docs in [ApiDocs::Hidden, ApiDocs::Served] {
        let built = build(&db, docs, |routes| {
            routes.authed_route(RateClass::Standard, routes!(whoami))
        })
        .await;

        for access in NEEDS_CREDENTIALS {
            assert!(
                !with_access(&built.entries, access).is_empty(),
                "the check never runs on an empty list: {access:?}"
            );
        }
        assert_eq!(
            routes_without_401(&built.app, &built.entries, &NEEDS_CREDENTIALS, None).await,
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
    let built = build(&db, ApiDocs::Hidden, |routes| {
        routes
            .authed_route(RateClass::Standard, routes!(whoami))
            .authed_route(RateClass::Standard, routes!(forgot_auth_user))
    })
    .await;

    let offenders = routes_without_401(&built.app, &built.entries, &NEEDS_CREDENTIALS, None).await;

    assert_eq!(offenders.len(), 1, "{offenders:?}");
    assert!(
        offenders[0].starts_with("GET /test/forgot-auth-user: 200"),
        "{offenders:?}"
    );
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn a_setup_token_is_refused_on_every_route_an_access_token_is_for(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;
    let user = add_user(&db).await;
    let built = build(&db, ApiDocs::Hidden, |routes| {
        routes.authed_route(RateClass::Standard, routes!(whoami))
    })
    .await;
    let token = setup_token(&built.challenges, user);

    let offenders = routes_without_401(
        &built.app,
        &built.entries,
        &NEEDS_ACCESS_TOKEN,
        Some(&token),
    )
    .await;

    assert_eq!(offenders, Vec::<String>::new());
    db.close().await;
}

/// A live mfa token of `user`.
fn mfa_token(challenges: &Challenges, user: UserId) -> SecretString {
    challenges
        .issue_mfa(
            ChallengeUser {
                id: user,
                role: Role::Member,
                password_changed_at: start(),
            },
            None,
            LoginStepId::new_v7(start(), [9; 10]).unwrap(),
        )
        .unwrap()
        .token
}

#[sqlx::test(migrations = false)]
async fn an_mfa_token_is_refused_on_every_route_that_needs_credentials(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;
    let user = add_user(&db).await;
    let built = build(&db, ApiDocs::Hidden, |routes| {
        routes.authed_route(RateClass::Standard, routes!(whoami))
    })
    .await;
    let token = mfa_token(&built.challenges, user);

    let offenders =
        routes_without_401(&built.app, &built.entries, &NEEDS_CREDENTIALS, Some(&token)).await;

    assert_eq!(offenders, Vec::<String>::new());
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn an_access_token_is_refused_on_every_setup_route(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;
    let user = add_user(&db).await;
    let built = build(&db, ApiDocs::Hidden, |routes| routes).await;
    let token = access_token(&db, &built.tokens, user).await;

    let offenders =
        routes_without_401(&built.app, &built.entries, &[Access::Setup], Some(&token)).await;

    assert_eq!(offenders, Vec::<String>::new());
    db.close().await;
}

/// Every step-up route of `entries` that didn't answer `token`'s session,
/// which never stepped up, with 403 `step_up_required`.
async fn step_up_routes_without_403(
    app: &Router,
    entries: &[RouteEntry],
    token: &SecretString,
) -> Vec<String> {
    let mut offenders = Vec::new();
    for route in entries
        .iter()
        .filter(|entry| entry.access == Access::AuthedStepUp)
    {
        let (status, _, code) = call(app, route, Some(token)).await;
        if status != 403 || code.as_deref() != Some("step_up_required") {
            offenders.push(format!(
                "{} {}: {status} {code:?}",
                route.method, route.path
            ));
        }
    }
    offenders
}

#[sqlx::test(migrations = false)]
async fn every_step_up_route_answers_403_without_a_fresh_step_up(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;
    let user = add_user(&db).await;
    let built = build(&db, ApiDocs::Hidden, |routes| routes).await;
    let token = access_token(&db, &built.tokens, user).await;

    assert!(!with_access(&built.entries, Access::AuthedStepUp).is_empty());
    assert_eq!(
        step_up_routes_without_403(&built.app, &built.entries, &token).await,
        Vec::<String>::new()
    );
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn the_check_catches_a_step_up_route_that_doesnt_check_the_stamp(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;
    let user = add_user(&db).await;
    let built = build(&db, ApiDocs::Hidden, |routes| {
        routes.step_up_route(RateClass::Standard, routes!(forgot_step_up))
    })
    .await;
    let token = access_token(&db, &built.tokens, user).await;

    let offenders = step_up_routes_without_403(&built.app, &built.entries, &token).await;

    assert_eq!(offenders.len(), 1, "{offenders:?}");
    assert!(
        offenders[0].starts_with("POST /test/forgot-step-up: 200"),
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

    let built = build(&db, ApiDocs::Hidden, |routes| {
        routes
            .authed_route(RateClass::Login, routes!(whoami))
            .public_route(RateClass::Register, routes!(forgot_auth_user))
    })
    .await;

    assert!(built.entries.contains(&RouteEntry {
        method: Method::GET,
        path: "/test/whoami".to_owned(),
        access: Access::Authed,
        class: RateClass::Login,
    }));
    assert!(built.entries.contains(&RouteEntry {
        method: Method::GET,
        path: "/test/forgot-auth-user".to_owned(),
        access: Access::Public,
        class: RateClass::Register,
    }));
    db.close().await;
}

/// The served document.
async fn document(app: &Router) -> Value {
    let request = Request::builder()
        .uri(OPENAPI_PATH)
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[sqlx::test(migrations = false)]
async fn the_registry_lists_exactly_the_documents_operations(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;
    let built = build(&db, ApiDocs::Served, |routes| routes).await;
    let document = document(&built.app).await;

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
    let registered: BTreeSet<(String, String)> = built
        .entries
        .iter()
        .filter(|entry| entry.path != OPENAPI_PATH)
        .map(|entry| (entry.method.to_string(), entry.path.clone()))
        .collect();

    assert!(!registered.is_empty());
    assert_eq!(registered, documented);
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn each_operation_names_its_access_in_the_document(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = open(options).await;
    let built = build(&db, ApiDocs::Served, |routes| {
        routes.authed_route(RateClass::Standard, routes!(whoami))
    })
    .await;
    let document = document(&built.app).await;

    for route in built
        .entries
        .iter()
        .filter(|entry| entry.path != OPENAPI_PATH)
    {
        let operation =
            &document["paths"][&route.path][route.method.as_str().to_lowercase().as_str()];
        let expected = route
            .access
            .scheme()
            .map(|scheme| serde_json::json!([{ scheme: [] }]));

        assert_eq!(
            operation.get("security"),
            expected.as_ref(),
            "{} {}",
            route.method,
            route.path
        );
        assert_eq!(
            operation.get("x-step-up"),
            (route.access == Access::AuthedStepUp).then_some(&serde_json::json!(true)),
            "{} {}",
            route.method,
            route.path
        );
    }
    let schemes = &document["components"]["securitySchemes"];
    for scheme in ["access_token", "setup_token"] {
        assert_eq!(
            schemes[scheme],
            serde_json::json!({ "type": "http", "scheme": "bearer" }),
            "{scheme}"
        );
    }
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
