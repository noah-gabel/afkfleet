//! Tests for the `AuthUser` extractor and `require()` (Plan.md P7.8, §7.2,
//! ADR-0006, ADR-0010, ADR-0016), through a test-only router behind the
//! server's middleware stack: no endpoint uses `AuthUser` before group D.
//! The route registry's own test, with an authed route, is
//! `tests/routes.rs`; the per-user limit's is `tests/rate_limits.rs`.
//!
//! - A live session's access token reaches the handler as the actor.
//! - Every refusal is the same 401 with a bare `WWW-Authenticate: Bearer`,
//!   including P7.12's disabled user and expired sessions.
//! - A database failure isn't a refusal, and the touch of `last_used_at`
//!   never fails or holds up a request.
//! - No log line shows any part of a presented token.
//!
//! A real database from `#[sqlx::test]`, `ManualClock` and `SeededRandom`.
//! Time isn't paused: sqlx's worker threads would make paused timeouts fire
//! early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{StatusCode, header};
use axum::routing::get;
use chrono::{DateTime, TimeZone as _, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::auth::SessionPolicy;
use fleet_core::authz::{Permission, ResourceContext, Role};
use fleet_core::id::UserId;
use fleet_core::system::Clock as _;
use fleet_core::value::Username;
use fleet_server::app::auth::{IssuedTokens, TokenService};
use fleet_server::app::challenges::Challenges;
use fleet_server::app::health::HealthService;
use fleet_server::app::login::LoginService;
use fleet_server::app::mfa::{MfaPorts, MfaService};
use fleet_server::config::{HttpConfig, RateLimitConfig};
use fleet_server::http::error::ApiError;
use fleet_server::http::extractors::AuthUser;
use fleet_server::http::middleware::{self, RequestIds};
use fleet_server::http::rate_limit::{RateLimits, TrustedProxies};
use fleet_server::http::router::{AppState, Services};
use fleet_server::infra::crypto::attempt_keys::HmacAttemptKeys;
use fleet_server::infra::crypto::passwords::Argon2Passwords;
use fleet_server::infra::crypto::recovery_codes::Sha256RecoveryCodes;
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::infra::crypto::totp::TotpRs;
use fleet_server::infra::crypto::vault::{VaultKeys, XChaChaVault};
use fleet_server::infra::qr::SvgQr;
use fleet_server::infra::sqlite::{Database, DatabaseOptions, READ_CONNECTIONS};
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::store::Store as _;
use fleet_server::ports::tokens::{TokenKind, Tokens as _};
use fleet_server::ports::users::{NewUser, PasswordHash};
use fleet_startup::config::{LogConfig, LogFormat};
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::log_buffer::LogBuffer;
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::Value;
use sqlx::ConnectOptions as _;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tower::ServiceExt as _;
use tracing_subscriber::layer::SubscriberExt as _;

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
const MINUTE: Duration = Duration::from_mins(1);
/// The acquire timeout of the test that waits for it (ADR-0015: it also
/// bounds opening the database, so it leaves room for a slow CI runner).
const SHORT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(1);

fn start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2027, 1, 15, 8, 0, 0).unwrap()
}

struct Setup {
    app: Router,
    service: TokenService,
    tokens: Sha256Tokens,
    clock: ManualClock,
    db: Database,
    user: UserId,
}

/// The test-only routes: who asks, and three `require` checks.
fn routes(state: AppState) -> Router {
    Router::new()
        .route("/whoami", get(whoami))
        .route("/own", get(own))
        .route("/others", get(others))
        .route("/users", get(users))
        .with_state(state)
}

async fn whoami(user: AuthUser) -> String {
    let actor = user.actor();
    format!("{} {} {}", actor.id, actor.role.as_str(), user.session_id())
}

/// The actor's own profile: allowed.
async fn own(user: AuthUser) -> Result<&'static str, ApiError> {
    user.require(
        Permission::UsePersonal,
        &ResourceContext::Personal {
            owner: user.actor().id,
        },
    )?;
    Ok("ok")
}

/// Someone else's profile: hidden (404).
async fn others(user: AuthUser) -> Result<&'static str, ApiError> {
    let other = UserId::new_v7(start(), [9; 10]).unwrap();
    user.require(
        Permission::UsePersonal,
        &ResourceContext::Personal { owner: other },
    )?;
    Ok("ok")
}

/// The user list: Admin+, so a Member is forbidden (403).
async fn users(user: AuthUser) -> Result<&'static str, ApiError> {
    user.require(Permission::ListUsers, &ResourceContext::Global)?;
    Ok("ok")
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

async fn setup_with(options: SqliteConnectOptions, database: DatabaseOptions) -> Setup {
    let db = Database::connect(options, database).await.unwrap();
    let clock = ManualClock::new(start());
    let random = SeededRandom::new(23);
    let tokens = Sha256Tokens::new(Arc::new(random.clone()));
    let service = TokenService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(db.clone()),
        Arc::new(tokens.clone()),
        SessionPolicy::default(),
    );
    let user = UserId::new_v7(start(), [1; 10]).unwrap();
    let mut tx = db.write().await.unwrap();
    tx.users()
        .insert(&NewUser {
            id: user,
            username: Username::try_from("alice").unwrap(),
            password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
            role: Role::Member,
            created_at: start(),
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    let limits = RateLimits::new(&RateLimitConfig::default(), Arc::new(clock.clone()));
    let mfa = mfa(&db);
    let login = login(&db, &service, &mfa).await;
    let (users, invites) = accounts(&db).await;
    let state = AppState::new(Services {
        health: HealthService::new(Arc::new(db.clone())),
        tokens: service.clone(),
        limits: limits.clone(),
        mfa,
        login,
        users,
        invites,
        link_flows: idle_link_flows(&db),
        accounts: idle_accounts(&db),
        grants: idle_grants(&db),
    });
    let http = HttpConfig {
        bind: HttpConfig::DEFAULT_BIND,
        request_timeout: Duration::from_secs(15),
        max_body_bytes: 65_536,
        trusted_proxies: TrustedProxies::default(),
    };
    let ids = RequestIds::new(Arc::new(clock.clone()), Arc::new(SeededRandom::new(5)));
    let app = middleware::apply(routes(state), &http, ids, &limits)
        .layer(MockConnectInfo(SocketAddr::from(([192, 0, 2, 10], 4711))));
    Setup {
        app,
        service,
        tokens,
        clock,
        db,
        user,
    }
}

async fn setup(options: SqliteConnectOptions) -> Setup {
    setup_with(options, DatabaseOptions::default()).await
}

/// The entry a test commits its setup with: a test-only name, built here and
/// never listed in `audit_actions!`.
fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        start(),
        AuditAction::try_from("test.session_create").unwrap(),
        AuditOutcome::Success,
    )
}

/// What a client sees of a response.
#[derive(Debug, PartialEq)]
struct Seen {
    status: StatusCode,
    www_authenticate: Option<String>,
    body: Value,
}

impl Setup {
    async fn login(&self) -> IssuedTokens {
        let prepared = self.service.prepare(self.user).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.sessions().insert(&prepared.new_session).await.unwrap();
        tx.commit(setup_entry()).await.unwrap();
        prepared.tokens
    }

    /// Sends `GET path` with each of `authorizations` as an `Authorization`
    /// header.
    async fn send(&self, path: &str, authorizations: &[&str]) -> Seen {
        let mut request = Request::builder().uri(path);
        for value in authorizations {
            request = request.header(header::AUTHORIZATION, *value);
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let www_authenticate = response
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .map(|value| value.to_str().unwrap().to_owned());
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8(bytes.to_vec()).unwrap()));
        Seen {
            status,
            www_authenticate,
            body,
        }
    }

    async fn whoami(&self, token: &SecretString) -> Seen {
        let value = format!("Bearer {}", token.expose_secret());
        self.send("/whoami", &[&value]).await
    }

    async fn last_used(&self, token: &SecretString) -> DateTime<Utc> {
        let hash = self
            .tokens
            .hash_presented(TokenKind::Access, token.expose_secret())
            .unwrap();
        self.db
            .sessions()
            .find_by_access_hash(&hash)
            .await
            .unwrap()
            .unwrap()
            .session
            .last_used_at
    }

    async fn set_user(&self, column_role: &str, disabled: bool) {
        let id = self.user.to_string();
        let disabled = i64::from(disabled);
        sqlx::query!(
            "UPDATE users SET role = ?, disabled = ? WHERE id = ?",
            column_role,
            disabled,
            id
        )
        .execute(self.db.write_pool())
        .await
        .unwrap();
    }
}

/// A 401 as every refusal looks, without its request ID.
fn refusal(mut seen: Seen) -> Seen {
    if let Some(error) = seen.body.get_mut("error") {
        error["request_id"] = Value::Null;
    }
    seen
}

// --- A live session ----------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_live_sessions_access_token_reaches_the_handler_as_its_user(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let tokens = setup.login().await;

    let seen = setup.whoami(&tokens.access_token).await;

    assert_eq!(seen.status, StatusCode::OK);
    assert_eq!(
        seen.body,
        Value::String(format!("{} member {}", setup.user, tokens.session_id))
    );
}

#[sqlx::test(migrations = false)]
async fn the_scheme_is_matched_without_regard_to_case(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let tokens = setup.login().await;
    let value = format!("bearer {}", tokens.access_token.expose_secret());

    let seen = setup.send("/whoami", &[&value]).await;

    assert_eq!(seen.status, StatusCode::OK);
}

/// RFC 6750's `"Bearer" 1*SP b64token`: headers 0.4 trims the spaces after
/// the scheme, and the token's own shape check does the rest.
#[sqlx::test(migrations = false)]
async fn one_or_more_spaces_may_follow_the_scheme(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let tokens = setup.login().await;
    let value = format!("Bearer   {}", tokens.access_token.expose_secret());

    let seen = setup.send("/whoami", &[&value]).await;

    assert_eq!(seen.status, StatusCode::OK);
}

#[sqlx::test(migrations = false)]
async fn a_role_change_counts_on_the_next_request(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let tokens = setup.login().await;
    assert_eq!(
        setup.send("/users", &[&bearer(&tokens)]).await.status,
        StatusCode::FORBIDDEN
    );

    setup.set_user("admin", false).await;

    let seen = setup.whoami(&tokens.access_token).await;
    assert!(seen.body.as_str().unwrap().contains(" admin "), "{seen:?}");
    assert_eq!(
        setup.send("/users", &[&bearer(&tokens)]).await.status,
        StatusCode::OK
    );
}

fn bearer(tokens: &IssuedTokens) -> String {
    format!("Bearer {}", tokens.access_token.expose_secret())
}

// --- Every refusal is the same ------------------------------------------------------

/// Builds every kind of refused request and checks that they all answer the
/// same 401: no detail, nothing that tells whether a token existed.
#[sqlx::test(migrations = false)]
async fn every_refusal_is_the_same_bare_401(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let live = setup.login().await;
    let valid = bearer(&live);
    let unknown = format!("Bearer afk_at_{}", "A".repeat(43));
    let refresh = format!("Bearer {}", live.refresh_token.expose_secret());
    let header_refusals: Vec<(&str, Vec<&str>)> = vec![
        ("no header", vec![]),
        ("two headers", vec![&valid, &valid]),
        ("Basic", vec!["Basic dXNlcjpwYXNz"]),
        ("a refresh token", vec![&refresh]),
        ("malformed", vec!["Bearer abc"]),
        ("unknown", vec![&unknown]),
        ("empty", vec!["Bearer "]),
    ];
    let mut seen = Vec::new();
    for (case, values) in &header_refusals {
        seen.push((*case, refusal(setup.send("/whoami", values).await)));
    }

    let expected = seen[0].1.body.clone();
    for (case, seen) in &seen {
        assert_eq!(seen.status, StatusCode::UNAUTHORIZED, "{case}");
        assert_eq!(seen.www_authenticate.as_deref(), Some("Bearer"), "{case}");
        assert_eq!(seen.body, expected, "{case}");
    }
    assert_eq!(expected["error"]["code"], "unauthorized");
}

#[sqlx::test(migrations = false)]
async fn an_expired_access_token_is_refused_like_any_other(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let tokens = setup.login().await;
    let none = refusal(setup.send("/whoami", &[]).await);
    setup.clock.advance(15 * MINUTE);

    let expired = refusal(setup.whoami(&tokens.access_token).await);

    assert_eq!(expired, none);
}

#[sqlx::test(migrations = false)]
async fn an_access_token_counts_until_its_last_millisecond(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let tokens = setup.login().await;
    setup
        .clock
        .advance((15 * MINUTE).checked_sub(Duration::from_millis(1)).unwrap());

    assert_eq!(
        setup.whoami(&tokens.access_token).await.status,
        StatusCode::OK
    );
}

#[sqlx::test(migrations = false)]
async fn a_revoked_session_is_refused_like_any_other(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let first = setup.login().await;
    let none = refusal(setup.send("/whoami", &[]).await);
    let ip = IpAddr::from([192, 0, 2, 10]);
    let second = setup
        .service
        .refresh(&first.refresh_token, ip)
        .await
        .unwrap();
    assert!(
        setup
            .service
            .refresh(&first.refresh_token, ip)
            .await
            .is_err()
    );

    let revoked = refusal(setup.whoami(&second.access_token).await);

    assert_eq!(revoked, none);
}

/// P7.12: a disabled user's session is rejected by `AuthUser`.
#[sqlx::test(migrations = false)]
async fn a_disabled_users_session_is_rejected(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let tokens = setup.login().await;
    let none = refusal(setup.send("/whoami", &[]).await);
    assert_eq!(
        setup.whoami(&tokens.access_token).await.status,
        StatusCode::OK
    );

    setup.set_user("member", true).await;

    let disabled = refusal(setup.whoami(&tokens.access_token).await);
    assert_eq!(disabled, none);
}

#[sqlx::test(migrations = false)]
async fn a_session_whose_user_is_gone_is_refused_like_any_other(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options.clone()).await;
    let tokens = setup.login().await;
    let none = refusal(setup.send("/whoami", &[]).await);
    // Only with the foreign keys off can a user with a session be deleted.
    let mut conn = options.foreign_keys(false).connect().await.unwrap();
    let id = setup.user.to_string();
    sqlx::query!("DELETE FROM users WHERE id = ?", id)
        .execute(&mut conn)
        .await
        .unwrap();

    let gone = refusal(setup.whoami(&tokens.access_token).await);

    assert_eq!(gone, none);
}

// --- require() --------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn require_allows_or_answers_404_and_403(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let tokens = setup.login().await;
    let value = bearer(&tokens);

    let own = setup.send("/own", &[&value]).await;
    let others = setup.send("/others", &[&value]).await;
    let users = setup.send("/users", &[&value]).await;

    assert_eq!(own.status, StatusCode::OK);
    assert_eq!(others.status, StatusCode::NOT_FOUND);
    assert_eq!(others.body["error"]["code"], "not_found");
    assert_eq!(users.status, StatusCode::FORBIDDEN);
    assert_eq!(users.body["error"]["code"], "forbidden");
}

// --- The database -------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_busy_database_is_503_not_a_refusal(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        DatabaseOptions::default().with_acquire_timeout(SHORT_ACQUIRE_TIMEOUT),
    )
    .await;
    let tokens = setup.login().await;
    let mut held = Vec::new();
    for _ in 0..READ_CONNECTIONS {
        held.push(setup.db.read_pool().acquire().await.unwrap());
    }

    let seen = setup.whoami(&tokens.access_token).await;

    assert_eq!(seen.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(seen.body["error"]["code"], "busy");
    assert_eq!(seen.www_authenticate, None);
    drop(held);
}

// --- The touch ------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_request_notes_its_sessions_use_once_a_minute(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let tokens = setup.login().await;

    setup.clock.advance(30 * Duration::from_secs(1));
    assert_eq!(
        setup.whoami(&tokens.access_token).await.status,
        StatusCode::OK
    );
    assert_eq!(
        setup.last_used(&tokens.access_token).await,
        start(),
        "not due yet"
    );

    setup.clock.advance(30 * Duration::from_secs(1));
    assert_eq!(
        setup.whoami(&tokens.access_token).await.status,
        StatusCode::OK
    );
    assert_eq!(
        setup.last_used(&tokens.access_token).await,
        setup.clock.now(),
        "due after a minute"
    );
}

#[sqlx::test(migrations = false)]
async fn a_held_write_connection_neither_fails_nor_holds_up_the_request(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let tokens = setup.login().await;
    setup.clock.advance(5 * MINUTE);
    let held = setup.db.write_pool().acquire().await.unwrap();

    let started = tokio::time::Instant::now();
    let seen = setup.whoami(&tokens.access_token).await;
    let took = started.elapsed();

    drop(held);
    assert_eq!(seen.status, StatusCode::OK);
    assert!(took < Duration::from_secs(1), "{took:?}");
    assert_eq!(
        setup.last_used(&tokens.access_token).await,
        start(),
        "skipped"
    );
}

// --- The log -------------------------------------------------------------------------

/// Every 8-character window of the random part of `token`, and the token.
fn parts(token: &str) -> Vec<String> {
    let secret = &token[7..];
    let mut parts: Vec<String> = (0..=secret.len() - 8)
        .map(|start| secret[start..start + 8].to_owned())
        .collect();
    parts.push(token.to_owned());
    parts
}

#[sqlx::test(migrations = false)]
async fn a_refusal_is_logged_with_its_reason_and_no_part_of_the_token(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let tokens = setup.login().await;
    let unknown = format!("afk_at_{}", "B".repeat(43));
    setup.clock.advance(15 * MINUTE);
    let logs = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: "debug".parse().unwrap(),
    };
    let subscriber =
        tracing_subscriber::registry().with(layer_with(&config, &NoRules, false, logs.clone()));
    let guard = tracing::subscriber::set_default(subscriber);

    let expired = setup.whoami(&tokens.access_token).await;
    let not_found = setup.send("/whoami", &[&format!("Bearer {unknown}")]).await;

    drop(guard);
    assert_eq!(expired.status, StatusCode::UNAUTHORIZED);
    assert_eq!(not_found.status, StatusCode::UNAUTHORIZED);
    let lines = logs
        .lines_with("a request's credentials were refused")
        .unwrap();
    assert_eq!(lines.len(), 2, "{}", logs.text());
    assert_eq!(lines[0]["level"], "DEBUG");
    assert_eq!(lines[0]["reason"], "expired");
    assert_eq!(lines[0]["session_id"], tokens.session_id.to_string());
    assert_eq!(lines[0]["user_id"], setup.user.to_string());
    assert_eq!(lines[1]["reason"], "unknown");
    assert!(lines[1].get("session_id").is_none(), "{}", lines[1]);
    let text = logs.text();
    for token in [tokens.access_token.expose_secret(), unknown.as_str()] {
        for part in parts(token) {
            assert!(!text.contains(&part), "{part} in {text}");
        }
    }
}

#[sqlx::test(migrations = false)]
async fn a_skipped_touch_is_logged_at_debug(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let tokens = setup.login().await;
    setup.clock.advance(5 * MINUTE);
    let held = setup.db.write_pool().acquire().await.unwrap();
    let logs = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: "debug".parse().unwrap(),
    };
    let subscriber =
        tracing_subscriber::registry().with(layer_with(&config, &NoRules, false, logs.clone()));
    let guard = tracing::subscriber::set_default(subscriber);

    let seen = setup.whoami(&tokens.access_token).await;

    drop(guard);
    drop(held);
    assert_eq!(seen.status, StatusCode::OK);
    let lines = logs
        .lines_with("the session's last use wasn't written")
        .unwrap();
    assert_eq!(lines.len(), 1, "{}", logs.text());
    assert_eq!(lines[0]["level"], "DEBUG");
    assert_eq!(lines[0]["session_id"], tokens.session_id.to_string());
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
