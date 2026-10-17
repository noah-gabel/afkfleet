//! The rate limits through the server's router (Plan.md P7.10, §7.4,
//! ADR-0016): every request spends its client's per-IP limit, a route its
//! class's, an authed request its user's; each refusal is `rate_limited`
//! with `Retry-After` in whole seconds, rounded up; the client is the peer,
//! or behind a trusted proxy the right-most untrusted `X-Forwarded-For`
//! entry; and a refused request's line is logged at `debug`.
//!
//! Requests carry their peer as axum's `ConnectInfo`, as `serve`'s
//! connections do. Test-only routes stand in for the login, refresh and
//! register routes D and E add. A real database from `#[sqlx::test]` and a
//! `ManualClock` the limiters run on; time isn't paused (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::num::NonZeroU32;
use core::time::Duration;
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, Request};
use axum::http::Method;
use chrono::{DateTime, TimeZone as _, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::auth::SessionPolicy;
use fleet_core::authz::Role;
use fleet_core::id::UserId;
use fleet_core::value::Username;
use fleet_server::app::auth::TokenService;
use fleet_server::app::challenges::Challenges;
use fleet_server::app::health::HealthService;
use fleet_server::app::login::LoginService;
use fleet_server::app::mfa::{MfaPorts, MfaService};
use fleet_server::config::{
    HttpConfig, LinkFlowLimits, LogConfig, LogFormat, Rate, RateLimitConfig,
};
use fleet_server::http::extractors::AuthUser;
use fleet_server::http::middleware::RequestIds;
use fleet_server::http::openapi::ApiDocs;
use fleet_server::http::rate_limit::{ClientIp, RateLimits, Refusals, TrustedProxies};
use fleet_server::http::router::{AppState, Services, api_routes};
use fleet_server::http::routes::{RateClass, Routes};
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
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::log_buffer::LogBuffer;
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tower::ServiceExt as _;
use tracing_subscriber::layer::SubscriberExt as _;
use utoipa_axum::routes;

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// The client of most tests.
const CLIENT: &str = "192.0.2.10";
/// Another client.
const OTHER: &str = "203.0.113.20";
/// The trusted proxy of the tests that have one.
const PROXY: &str = "198.51.100.1";

fn start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2027, 1, 15, 8, 0, 0).unwrap()
}

fn nonzero(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap()
}

/// Loose limits: no test reaches one it doesn't test.
fn loose() -> RateLimitConfig {
    let loose = Rate::per_second(nonzero(1000), nonzero(1000));
    RateLimitConfig {
        per_ip: loose,
        login: loose,
        refresh: loose,
        register: loose,
        per_user: loose,
        link_flows: LinkFlowLimits::default(),
        grants_by_name: loose,
        enroll: loose,
        connect: loose,
        bot_actions: loose,
    }
}

// --- Test-only routes ------------------------------------------------------------

/// The client's address, as the stack resolved it.
#[utoipa::path(get, path = "/test/client", responses((status = 200, description = "The client.")))]
async fn client(ClientIp(ip): ClientIp) -> String {
    ip.to_string()
}

/// A stand-in for `POST /auth/login`.
#[utoipa::path(post, path = "/test/login", responses((status = 200, description = "Ok.")))]
async fn login() -> &'static str {
    "ok"
}

/// A stand-in for `POST /auth/login/mfa`.
#[utoipa::path(post, path = "/test/login/mfa", responses((status = 200, description = "Ok.")))]
async fn login_mfa() -> &'static str {
    "ok"
}

/// A stand-in for `POST /auth/refresh`.
#[utoipa::path(post, path = "/test/refresh", responses((status = 200, description = "Ok.")))]
async fn refresh() -> &'static str {
    "ok"
}

/// A stand-in for `POST /auth/register`.
#[utoipa::path(post, path = "/test/register", responses((status = 200, description = "Ok.")))]
async fn register() -> &'static str {
    "ok"
}

/// An authed route: who asks.
#[utoipa::path(get, path = "/test/whoami", responses((status = 200, description = "The user.")))]
async fn whoami(user: AuthUser) -> String {
    user.actor().id.to_string()
}

// --- The setup -----------------------------------------------------------------

struct Setup {
    app: Router,
    limits: RateLimits,
    service: TokenService,
    clock: ManualClock,
    db: Database,
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
async fn login_service(db: &Database, tokens: &TokenService, mfa: &MfaService) -> LoginService {
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

async fn setup(options: SqliteConnectOptions, config: RateLimitConfig, proxies: &[&str]) -> Setup {
    let db = Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap();
    let clock = ManualClock::new(start());
    let random = SeededRandom::new(31);
    let service = TokenService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(db.clone()),
        Arc::new(Sha256Tokens::new(Arc::new(random.clone()))),
        SessionPolicy::default(),
    );
    let limits = RateLimits::new(&config, Arc::new(clock.clone()));
    let mfa = mfa(&db);
    let logins = login_service(&db, &service, &mfa).await;
    let (users, invites) = accounts(&db).await;
    let state = AppState::new(Services {
        health: HealthService::new(Arc::new(db.clone())),
        tokens: service.clone(),
        limits: limits.clone(),
        mfa,
        login: logins,
        users,
        invites,
        link_flows: idle_link_flows(&db),
        accounts: idle_accounts(&db),
        grants: idle_grants(&db),
        enrollment_tokens: None,
        agents: idle_agents(&db),
        bots: idle_bots(&db),
    });
    let http = HttpConfig {
        bind: HttpConfig::DEFAULT_BIND,
        request_timeout: Duration::from_secs(15),
        max_body_bytes: 65_536,
        trusted_proxies: TrustedProxies::new(
            proxies.iter().map(|text| text.parse().unwrap()).collect(),
        ),
    };
    let ids = RequestIds::new(Arc::new(clock.clone()), Arc::new(SeededRandom::new(5)));
    let routes = api_routes(Routes::new(&limits))
        .public_route(RateClass::Standard, routes!(client))
        .public_route(RateClass::Login, routes!(login))
        .public_route(RateClass::Login, routes!(login_mfa))
        .public_route(RateClass::Refresh, routes!(refresh))
        .public_route(RateClass::Register, routes!(register))
        .authed_route(RateClass::Standard, routes!(whoami));
    let (app, _) = routes.finish(state, &http, ids, ApiDocs::Hidden);
    Setup {
        app,
        limits,
        service,
        clock,
        db,
    }
}

/// What a client sees of a response.
#[derive(Debug, Serialize)]
struct Seen {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Value,
}

/// The headers a test looks at, and the snapshot holds.
const SEEN_HEADERS: [&str; 5] = [
    "content-type",
    "retry-after",
    "x-content-type-options",
    "x-request-id",
    "cache-control",
];

/// One request: `method path` from `peer`, with `headers`.
fn request(method: Method, path: &str, peer: &str, headers: &[(&str, &str)]) -> Request {
    let peer = SocketAddr::new(peer.parse::<IpAddr>().unwrap(), 4711);
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .extension(ConnectInfo(peer));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request.body(Body::empty()).unwrap()
}

async fn answer(app: &Router, request: Request) -> Seen {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let headers = SEEN_HEADERS
        .into_iter()
        .filter_map(|name| {
            let value = response.headers().get(name)?;
            Some((name.to_owned(), value.to_str().unwrap().to_owned()))
        })
        .collect();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8(bytes.to_vec()).unwrap()));
    Seen {
        status,
        headers,
        body,
    }
}

impl Setup {
    async fn send(&self, method: Method, path: &str, peer: &str) -> Seen {
        answer(&self.app, request(method, path, peer, &[])).await
    }

    async fn get(&self, path: &str, peer: &str) -> Seen {
        self.send(Method::GET, path, peer).await
    }

    async fn post(&self, path: &str, peer: &str) -> Seen {
        self.send(Method::POST, path, peer).await
    }

    /// The client the stack resolves for a request from `peer` with these
    /// `X-Forwarded-For` values.
    async fn client_of(&self, peer: &str, forwarded: &[&str]) -> String {
        let headers: Vec<(&str, &str)> = forwarded
            .iter()
            .map(|value| ("x-forwarded-for", *value))
            .collect();
        let seen = answer(
            &self.app,
            request(Method::GET, "/test/client", peer, &headers),
        )
        .await;
        assert_eq!(seen.status, 200, "{seen:?}");
        seen.body.as_str().unwrap().to_owned()
    }

    /// A user `n` with a fresh session, and its access token.
    async fn session(&self, n: u8) -> (UserId, SecretString) {
        let user = UserId::new_v7(start(), [n; 10]).unwrap();
        let prepared = self.service.prepare(user).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.users()
            .insert(&NewUser {
                id: user,
                username: Username::try_from(format!("user{n}").as_str()).unwrap(),
                password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
                role: Role::Member,
                created_at: start(),
            })
            .await
            .unwrap();
        tx.sessions().insert(&prepared.new_session).await.unwrap();
        tx.commit(NewAuditEntry::new(
            start(),
            AuditAction::try_from("test.session_create").unwrap(),
            AuditOutcome::Success,
        ))
        .await
        .unwrap();
        (user, prepared.tokens.access_token)
    }

    async fn whoami(&self, token: &SecretString) -> Seen {
        let bearer = format!("Bearer {}", token.expose_secret());
        answer(
            &self.app,
            request(
                Method::GET,
                "/test/whoami",
                CLIENT,
                &[("authorization", &bearer)],
            ),
        )
        .await
    }
}

/// Asserts `seen` is the `rate_limited` refusal with `Retry-After: seconds`.
fn assert_refused(seen: &Seen, seconds: u64) {
    assert_eq!(seen.status, 429, "{seen:?}");
    assert_eq!(seen.body["error"]["code"], "rate_limited", "{seen:?}");
    assert_eq!(seen.headers["retry-after"], seconds.to_string(), "{seen:?}");
}

/// `seen` with its request ID, checked first, replaced for the snapshot.
fn redacted(mut seen: Seen) -> Seen {
    let id = seen.headers["x-request-id"].clone();
    assert_eq!(seen.body["error"]["request_id"], id);
    seen.headers
        .insert("x-request-id".to_owned(), "[request_id]".to_owned());
    seen.body["error"]["request_id"] = json!("[request_id]");
    seen
}

/// The server's log layer at `level`, into a buffer, as this thread's
/// default; the tests run on tokio's current-thread runtime.
fn capture(level: &str) -> (LogBuffer, tracing::subscriber::DefaultGuard) {
    let buffer = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: level.parse().unwrap(),
    };
    let subscriber =
        tracing_subscriber::registry().with(layer_with(&config, &NoRules, false, buffer.clone()));
    let guard = tracing::subscriber::set_default(subscriber);
    (buffer, guard)
}

/// Two requests a second per IP, a burst of 3; everything else loose.
fn per_ip_2_burst_3() -> RateLimitConfig {
    RateLimitConfig {
        per_ip: Rate::per_second(nonzero(2), nonzero(3)),
        ..loose()
    }
}

// --- The per-IP limit ------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_clients_burst_is_allowed_then_refused_until_its_bucket_refills(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options, per_ip_2_burst_3(), &[]).await;
    for _ in 0..3 {
        assert_eq!(setup.get("/health/live", CLIENT).await.status, 200);
    }

    let refused = setup.get("/health/live", CLIENT).await;

    assert_refused(&refused, 1);
    setup.clock.advance(Duration::from_millis(499));
    assert_refused(&setup.get("/health/live", CLIENT).await, 1);
    setup.clock.advance(Duration::from_millis(1));
    assert_eq!(setup.get("/health/live", CLIENT).await.status, 200);
}

#[sqlx::test(migrations = false)]
async fn a_refusal_is_the_error_envelope_with_its_request_id(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options, per_ip_2_burst_3(), &[]).await;
    for _ in 0..3 {
        setup.get("/health/live", CLIENT).await;
    }
    let (logs, guard) = capture("info");

    let refused = setup.get("/health/live", CLIENT).await;

    drop(guard);
    assert_refused(&refused, 1);
    assert_eq!(logs.text(), "", "no wiring bug, and the line is at debug");
    insta::assert_json_snapshot!("per_ip_429", redacted(refused));
}

#[sqlx::test(migrations = false)]
async fn retry_after_is_the_wait_in_whole_seconds_rounded_up(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let config = RateLimitConfig {
        login: Rate::per_minute(nonzero(5)),
        ..loose()
    };
    let setup = setup(options, config, &[]).await;
    for _ in 0..5 {
        assert_eq!(setup.post("/test/login", CLIENT).await.status, 200);
    }

    assert_refused(&setup.post("/test/login", CLIENT).await, 12);
    setup.clock.advance(Duration::from_millis(11_001));
    assert_refused(&setup.post("/test/login", CLIENT).await, 1);
    setup.clock.advance(Duration::from_millis(999));
    assert_eq!(setup.post("/test/login", CLIENT).await.status, 200);
}

#[sqlx::test(migrations = false)]
async fn unmatched_paths_and_wrong_methods_spend_the_per_ip_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options, per_ip_2_burst_3(), &[]).await;

    assert_eq!(setup.get("/no/such/route", CLIENT).await.status, 404);
    assert_eq!(setup.post("/health/live", CLIENT).await.status, 405);
    assert_eq!(setup.get("/health/ready", CLIENT).await.status, 200);

    assert_refused(&setup.get("/health/ready", CLIENT).await, 1);
}

#[sqlx::test(migrations = false)]
async fn each_client_has_its_own_bucket(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options, per_ip_2_burst_3(), &[]).await;
    for _ in 0..3 {
        setup.get("/health/live", CLIENT).await;
    }

    assert_refused(&setup.get("/health/live", CLIENT).await, 1);
    assert_eq!(setup.get("/health/live", OTHER).await.status, 200);
}

#[sqlx::test(migrations = false)]
async fn an_ipv6_client_is_counted_by_its_64(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options, per_ip_2_burst_3(), &[]).await;
    for address in ["2001:db8:0:1::1", "2001:db8:0:1::2", "2001:db8:0:1:ab::3"] {
        assert_eq!(setup.get("/health/live", address).await.status, 200);
    }

    assert_refused(&setup.get("/health/live", "2001:db8:0:1:ffff::4").await, 1);
    assert_eq!(
        setup.get("/health/live", "2001:db8:0:2::1").await.status,
        200
    );
}

#[sqlx::test(migrations = false)]
async fn a_request_without_its_peer_address_is_an_internal_error(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options, loose(), &[]).await;
    let (logs, guard) = capture("info");
    let request = Request::builder()
        .uri("/health/live")
        .body(Body::empty())
        .unwrap();

    let seen = answer(&setup.app, request).await;

    drop(guard);
    assert_eq!(seen.status, 500);
    assert_eq!(seen.body["error"]["code"], "internal");
    let errors = logs
        .lines_with("a request failed with an internal error")
        .unwrap();
    assert_eq!(errors.len(), 1, "{}", logs.text());
    assert!(
        errors[0]["error"]
            .as_str()
            .unwrap()
            .contains("into_make_service_with_connect_info"),
        "{}",
        logs.text()
    );
}

// --- Who the client is -------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn an_untrusted_peers_forwarded_for_is_ignored(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options, per_ip_2_burst_3(), &["198.51.100.1/32"]).await;

    assert_eq!(setup.client_of(CLIENT, &[OTHER]).await, CLIENT);
    assert_eq!(setup.client_of(CLIENT, &["203.0.113.21"]).await, CLIENT);
    assert_eq!(setup.client_of(CLIENT, &["203.0.113.22"]).await, CLIENT);
    assert_refused(
        &answer(
            &setup.app,
            request(
                Method::GET,
                "/health/live",
                CLIENT,
                &[("x-forwarded-for", "203.0.113.23")],
            ),
        )
        .await,
        1,
    );
}

#[sqlx::test(migrations = false)]
async fn a_trusted_proxys_forwarded_for_names_the_client(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options, loose(), &["198.51.100.0/24"]).await;

    assert_eq!(setup.client_of(PROXY, &[CLIENT]).await, CLIENT);
    assert_eq!(
        setup
            .client_of(PROXY, &["203.0.113.5, 192.0.2.10, 198.51.100.7"])
            .await,
        CLIENT,
        "the right-most untrusted entry, never the left-most"
    );
    assert_eq!(setup.client_of(PROXY, &[]).await, PROXY);
    assert_eq!(
        setup.client_of("::ffff:198.51.100.1", &[CLIENT]).await,
        CLIENT,
        "a trusted IPv4 proxy seen as a mapped IPv6 peer"
    );
}

#[sqlx::test(migrations = false)]
async fn clients_behind_a_trusted_proxy_have_their_own_buckets(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options, per_ip_2_burst_3(), &["198.51.100.0/24"]).await;
    let through = |client: &'static str| {
        request(
            Method::GET,
            "/health/live",
            PROXY,
            &[("x-forwarded-for", client)],
        )
    };
    for _ in 0..3 {
        assert_eq!(answer(&setup.app, through(CLIENT)).await.status, 200);
    }

    assert_refused(&answer(&setup.app, through(CLIENT)).await, 1);
    assert_eq!(answer(&setup.app, through(OTHER)).await.status, 200);
    assert_eq!(setup.get("/health/live", PROXY).await.status, 200);
}

// --- The classes ---------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn the_two_login_routes_share_one_bucket(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let config = RateLimitConfig {
        login: Rate::per_minute(nonzero(5)),
        ..loose()
    };
    let setup = setup(options, config, &[]).await;
    for path in [
        "/test/login",
        "/test/login/mfa",
        "/test/login",
        "/test/login/mfa",
        "/test/login",
    ] {
        assert_eq!(setup.post(path, CLIENT).await.status, 200, "{path}");
    }

    assert_refused(&setup.post("/test/login/mfa", CLIENT).await, 12);
    assert_refused(&setup.post("/test/login", CLIENT).await, 12);
    assert_eq!(setup.post("/test/login", OTHER).await.status, 200);
}

#[sqlx::test(migrations = false)]
async fn refresh_and_register_have_their_own_limits(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let config = RateLimitConfig {
        refresh: Rate::per_minute(nonzero(2)),
        register: Rate::per_hour(nonzero(1)),
        ..loose()
    };
    let setup = setup(options, config, &[]).await;
    for _ in 0..2 {
        assert_eq!(setup.post("/test/refresh", CLIENT).await.status, 200);
    }
    assert_eq!(setup.post("/test/register", CLIENT).await.status, 200);

    assert_refused(&setup.post("/test/refresh", CLIENT).await, 30);
    assert_refused(&setup.post("/test/register", CLIENT).await, 3600);
    assert_eq!(setup.post("/test/login", CLIENT).await.status, 200);
    assert_eq!(setup.get("/health/live", CLIENT).await.status, 200);
}

#[sqlx::test(migrations = false)]
async fn a_wrong_method_spends_no_class_token(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let config = RateLimitConfig {
        login: Rate::per_minute(nonzero(1)),
        ..loose()
    };
    let setup = setup(options, config, &[]).await;
    for _ in 0..5 {
        assert_eq!(setup.get("/test/login", CLIENT).await.status, 405);
    }

    assert_eq!(setup.post("/test/login", CLIENT).await.status, 200);
}

// --- The per-user limit ----------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_users_burst_is_allowed_then_refused_and_users_dont_share(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let config = RateLimitConfig {
        per_user: Rate::per_second(nonzero(1), nonzero(2)),
        ..loose()
    };
    let setup = setup(options, config, &[]).await;
    let (first, first_token) = setup.session(1).await;
    let (_, second_token) = setup.session(2).await;
    for _ in 0..2 {
        let seen = setup.whoami(&first_token).await;
        assert_eq!(seen.status, 200, "{seen:?}");
        assert_eq!(seen.body, Value::String(first.to_string()));
    }

    assert_refused(&setup.whoami(&first_token).await, 1);
    assert_eq!(setup.whoami(&second_token).await.status, 200);
    setup.clock.advance(Duration::from_secs(1));
    assert_eq!(setup.whoami(&first_token).await.status, 200);
}

#[sqlx::test(migrations = false)]
async fn a_request_without_credentials_spends_no_users_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let config = RateLimitConfig {
        per_user: Rate::per_second(nonzero(1), nonzero(1)),
        ..loose()
    };
    let setup = setup(options, config, &[]).await;
    let (_, token) = setup.session(1).await;
    for _ in 0..3 {
        assert_eq!(setup.get("/test/whoami", CLIENT).await.status, 401);
    }

    assert_eq!(setup.whoami(&token).await.status, 200);
}

// --- Counting and logging refusals ---------------------------------------------------

#[sqlx::test(migrations = false)]
async fn every_refusal_is_counted_and_its_line_is_at_debug(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options, per_ip_2_burst_3(), &[]).await;
    let (info, info_guard) = capture("info");
    for _ in 0..10 {
        setup.get("/health/live", OTHER).await;
        setup.get("/no/such/route", CLIENT).await;
    }
    drop(info_guard);
    let (debug, debug_guard) = capture("debug");
    setup.get("/no/such/route", CLIENT).await;
    drop(debug_guard);

    let lines = info.json_lines().unwrap();
    assert_eq!(
        lines.iter().filter(|line| line["status"] == 429).count(),
        0,
        "{}",
        info.text()
    );
    assert_eq!(
        lines.iter().filter(|line| line["status"] == 404).count(),
        3,
        "the allowed requests are logged at info as before"
    );
    let refused = debug.lines_with("request finished").unwrap();
    assert_eq!(refused.len(), 1, "{}", debug.text());
    assert_eq!(refused[0]["level"], "DEBUG");
    assert_eq!(refused[0]["status"], 429);
    assert_eq!(refused[0]["code"], "rate_limited");
    assert_eq!(
        setup.limits.take_refusals(),
        Refusals {
            per_ip: 15,
            ..Refusals::default()
        }
    );
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

/// The bots' service, which these tests never call (P11.2).
fn idle_bots(db: &fleet_server::infra::sqlite::Database) -> fleet_server::app::bots::BotService {
    let clock = Arc::new(fleet_testkit::system::ManualClock::new(
        chrono::DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
    ));
    fleet_server::app::bots::BotService::new(
        clock.clone(),
        Arc::new(db.clone()),
        Arc::new(fleet_server::grpc::registry::AgentRegistry::new(clock)),
        false,
    )
}
