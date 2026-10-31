//! The 2FA routes over HTTP (Plan.md P7.7, P7.12, §7.2, ADR-0016):
//! `POST /api/v1/me/mfa/enroll`, `/confirm` and `/reset`, through the real
//! router with `oneshot`.
//!
//! - A setup token works for enroll and confirm, expires after 10 minutes,
//!   and can't be used again after confirm (P7.7). That it's refused on
//!   every other route is the route-coverage test's (`tests/routes.rs`).
//! - Confirm answers 409 before enroll, 422 for a wrong or malformed code
//!   (never echoing it), and the recovery codes once.
//! - A request body's JSON failures answer 415, 400 or 413, and only the
//!   rules 422 (P7.6's Phase 6 note).
//! - Reset needs a fresh step-up, answers a setup token, and its confirm
//!   revokes the user's other sessions while this one keeps working.
//! - Each route spends its user's limit (429).
//!
//! Each test gets a fresh database from `#[sqlx::test]`; time is a
//! `ManualClock`, never paused (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::num::NonZeroU32;
use core::time::Duration;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::extract::connect_info::MockConnectInfo;
use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::auth::SessionPolicy;
use fleet_core::authz::Role;
use fleet_core::id::{SessionId, UserId};
use fleet_core::system::Clock as _;
use fleet_core::value::Username;
use fleet_server::app::auth::TokenService;
use fleet_server::app::challenges::{ChallengeUser, Challenges, SetupPurpose};
use fleet_server::app::health::HealthService;
use fleet_server::app::login::LoginService;
use fleet_server::app::mfa::{MfaPorts, MfaService};
use fleet_server::config::{HttpConfig, Rate, RateLimitConfig};
use fleet_server::http::middleware::RequestIds;
use fleet_server::http::openapi::ApiDocs;
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
use fleet_server::ports::audit::{AuditLimit, AuditPage, NewAuditEntry};
use fleet_server::ports::store::Store as _;
use fleet_server::ports::users::{NewUser, PasswordHash};
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::{ExposeSecret as _, SecretBox, SecretString};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use totp_rs::Totp;
use tower::ServiceExt as _;

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// When every test starts: in the middle of a TOTP step.
const T0: i64 = 1_800_000_015_000;

const ENROLL: &str = "/api/v1/me/mfa/enroll";
const CONFIRM: &str = "/api/v1/me/mfa/confirm";
const RESET: &str = "/api/v1/me/mfa/reset";

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

struct Setup {
    app: Router,
    db: Database,
    clock: ManualClock,
    tokens: TokenService,
    challenges: Challenges,
    user: UserId,
}

fn nonzero(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap()
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

async fn setup(options: SqliteConnectOptions) -> Setup {
    setup_with(options, &RateLimitConfig::default()).await
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

async fn setup_with(options: SqliteConnectOptions, limits: &RateLimitConfig) -> Setup {
    let db = Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap();
    let clock = ManualClock::new(at(T0));
    let random = SeededRandom::new(71);
    let token_port = Arc::new(Sha256Tokens::new(Arc::new(random.clone())));
    let tokens = TokenService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(db.clone()),
        token_port.clone(),
        SessionPolicy::default(),
    );
    let challenges = Challenges::new(Arc::new(clock.clone()), token_port);
    let keys = VaultKeys::from_keys(
        BTreeMap::from([(1, SecretBox::new(Box::new([0x42; 32])))]),
        1,
    )
    .unwrap();
    let mfa = MfaService::new(
        Arc::new(clock.clone()),
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
    let limits = RateLimits::new(limits, Arc::new(clock.clone()));
    let login = login(&db, &tokens, &mfa).await;
    let (users, invites) = accounts(&db).await;
    let state = AppState::new(Services {
        health: HealthService::new(Arc::new(db.clone())),
        tokens: tokens.clone(),
        limits,
        mfa,
        login,
        users,
        invites,
        link_flows: idle_link_flows(&db),
        accounts: idle_accounts(&db),
        grants: idle_grants(&db),
        agents: idle_agents(&db),
        bots: idle_bots(&db),
        modes: idle_modes(&db),
        server_settings: idle_server_settings(&db),
        events: idle_events(&db),
        audit: fleet_server::app::audit::AuditService::new(
            std::sync::Arc::new(fleet_testkit::system::ManualClock::new(
                chrono::DateTime::UNIX_EPOCH,
            )),
            std::sync::Arc::new(db.clone()),
        ),
    });
    let http = HttpConfig {
        bind: HttpConfig::DEFAULT_BIND,
        request_timeout: Duration::from_secs(15),
        max_body_bytes: 65_536,
        trusted_proxies: TrustedProxies::default(),
    };
    let ids = RequestIds::new(Arc::new(clock.clone()), Arc::new(SeededRandom::new(5)));
    let app = router(state, &http, ids, ApiDocs::Hidden)
        .layer(MockConnectInfo(SocketAddr::from(([192, 0, 2, 10], 4711))));
    let user = UserId::new_v7(at(T0), [1; 10]).unwrap();
    let mut tx = db.write().await.unwrap();
    tx.users()
        .insert(&NewUser {
            id: user,
            username: Username::try_from("alice").unwrap(),
            password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
            role: Role::Member,
            created_at: at(T0 - 1_000),
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    Setup {
        app,
        db,
        clock,
        tokens,
        challenges,
        user,
    }
}

fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from("test.mfa_http").unwrap(),
        AuditOutcome::Success,
    )
}

/// What a client sees of a response.
#[derive(Debug)]
struct Seen {
    status: u16,
    body: Value,
    text: String,
    cache_control: Option<String>,
}

impl Seen {
    fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or_default()
    }
}

impl Setup {
    /// A first enrollment's setup token.
    fn setup_token(&self) -> SecretString {
        self.challenges
            .issue_setup(
                ChallengeUser {
                    id: self.user,
                    role: Role::Member,
                    password_changed_at: at(T0 - 1_000),
                },
                SetupPurpose::FirstEnrollment,
            )
            .unwrap()
            .token
    }

    /// A live session: its ID and its access token.
    async fn session(&self) -> (SessionId, SecretString) {
        let prepared = self.tokens.prepare(self.user).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.sessions().insert(&prepared.new_session).await.unwrap();
        tx.commit(setup_entry()).await.unwrap();
        (prepared.new_session.id, prepared.tokens.access_token)
    }

    /// Stamps `session`'s step-up now.
    async fn step_up(&self, session: SessionId) {
        let mut tx = self.db.write().await.unwrap();
        assert!(
            tx.sessions()
                .stamp_step_up(session, self.clock.now())
                .await
                .unwrap()
        );
        tx.commit(setup_entry()).await.unwrap();
    }

    async fn send(&self, request: Request) -> Seen {
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status().as_u16();
        let cache_control = response
            .headers()
            .get("cache-control")
            .map(|value| value.to_str().unwrap().to_owned());
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        Seen {
            status,
            body,
            text,
            cache_control,
        }
    }

    /// `POST path` with `bearer` and a JSON `body`, if any.
    async fn post(&self, path: &str, bearer: Option<&SecretString>, body: Option<Value>) -> Seen {
        let mut request = Request::post(path);
        if let Some(token) = bearer {
            request = request.header("authorization", format!("Bearer {}", token.expose_secret()));
        }
        let request = match body {
            Some(body) => request
                .header("content-type", "application/json")
                .body(Body::from(body.to_string())),
            None => request.body(Body::empty()),
        };
        self.send(request.unwrap()).await
    }

    /// Enrolls with `token` and returns the authenticator app's view.
    async fn enroll(&self, token: &SecretString) -> Totp {
        let seen = self.post(ENROLL, Some(token), None).await;
        assert_eq!(seen.status, 200, "{}", seen.text);
        Totp::from_url(seen.body["otpauth_uri"].as_str().unwrap()).unwrap()
    }

    /// The app's code now, `steps` steps away.
    fn code(&self, app: &Totp, steps: i64) -> String {
        let now = u64::try_from(self.clock.now().timestamp() + steps * 30).unwrap();
        app.generate(now).to_string()
    }

    async fn confirm(&self, token: &SecretString, code: &str) -> Seen {
        self.post(CONFIRM, Some(token), Some(json!({ "code": code })))
            .await
    }

    /// A user with confirmed 2FA: the app.
    async fn enrolled(&self) -> Totp {
        let token = self.setup_token();
        let app = self.enroll(&token).await;
        let seen = self.confirm(&token, &self.code(&app, 0)).await;
        assert_eq!(seen.status, 200, "{}", seen.text);
        app
    }
}

// --- Enroll ------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn enroll_answers_the_uri_the_qr_code_and_the_secret_without_caching(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let token = setup.setup_token();

    let seen = setup.post(ENROLL, Some(&token), None).await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    let secret = seen.body["secret"].as_str().unwrap();
    assert_eq!(
        seen.body["otpauth_uri"],
        format!("otpauth://totp/afkfleet:alice?secret={secret}&issuer=afkfleet")
    );
    assert!(
        seen.body["qr_svg"]
            .as_str()
            .unwrap()
            .starts_with("data:image/svg+xml;base64,")
    );
    assert_eq!(seen.cache_control.as_deref(), Some("no-store"));
}

#[sqlx::test(migrations = false)]
async fn enroll_and_confirm_refuse_anything_but_a_live_setup_token(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let (_, access) = setup.session().await;
    let made_up = SecretString::from("afk_st_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");

    for path in [ENROLL, CONFIRM] {
        for bearer in [None, Some(&access), Some(&made_up)] {
            let seen = setup.post(path, bearer, None).await;

            assert_eq!(seen.status, 401, "{path}: {}", seen.text);
            assert_eq!(seen.code(), "unauthorized");
        }
    }
}

#[sqlx::test(migrations = false)]
async fn a_setup_token_expires_after_10_minutes(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let token = setup.setup_token();
    setup.clock.advance(Duration::from_millis(599_999));
    assert_eq!(setup.post(ENROLL, Some(&token), None).await.status, 200);
    setup.clock.advance(Duration::from_millis(1));

    let seen = setup.post(ENROLL, Some(&token), None).await;

    assert_eq!(seen.status, 401, "{}", seen.text);
}

#[sqlx::test(migrations = false)]
async fn a_setup_token_cant_be_used_again_after_confirm(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let token = setup.setup_token();
    let app = setup.enroll(&token).await;
    let code = setup.code(&app, 0);
    assert_eq!(setup.confirm(&token, &code).await.status, 200);

    let enroll_again = setup.post(ENROLL, Some(&token), None).await;
    let confirm_again = setup.confirm(&token, &code).await;

    assert_eq!(enroll_again.status, 401, "{}", enroll_again.text);
    assert_eq!(confirm_again.status, 401, "{}", confirm_again.text);
}

#[sqlx::test(migrations = false)]
async fn enroll_spends_the_users_limit(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let one = Rate::per_second(nonzero(1), nonzero(1));
    let setup = setup_with(
        options,
        &RateLimitConfig {
            per_user: one,
            ..RateLimitConfig::default()
        },
    )
    .await;
    let token = setup.setup_token();

    let first = setup.post(ENROLL, Some(&token), None).await;
    let second = setup.post(ENROLL, Some(&token), None).await;

    assert_eq!(first.status, 200);
    assert_eq!(second.status, 429, "{}", second.text);
}

// --- Confirm -----------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn confirm_answers_the_ten_recovery_codes_once(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let token = setup.setup_token();
    let app = setup.enroll(&token).await;

    let seen = setup.confirm(&token, &setup.code(&app, 0)).await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    let codes = seen.body["recovery_codes"].as_array().unwrap();
    assert_eq!(codes.len(), 10);
    for code in codes {
        assert_eq!(code.as_str().unwrap().len(), 24, "{code}");
    }
    assert_eq!(seen.cache_control.as_deref(), Some("no-store"));
    let entries = setup
        .db
        .audit()
        .list(AuditPage {
            before: None,
            limit: AuditLimit::new(10).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(entries[0].action, AuditAction::MFA_ENROLL);
    assert_eq!(entries[0].ip, Some("192.0.2.10".parse().unwrap()));
}

#[sqlx::test(migrations = false)]
async fn confirm_before_enroll_is_a_conflict(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let token = setup.setup_token();

    let seen = setup.confirm(&token, "123456").await;

    assert_eq!(seen.status, 409, "{}", seen.text);
    assert_eq!(seen.code(), "conflict");
}

#[sqlx::test(migrations = false)]
async fn a_wrong_code_is_422_on_code_and_the_token_stays_usable(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let token = setup.setup_token();
    let app = setup.enroll(&token).await;
    let wrong = setup.code(&app, 3);

    let seen = setup.confirm(&token, &wrong).await;

    assert_eq!(seen.status, 422, "{}", seen.text);
    assert_eq!(
        seen.body["error"]["fields"],
        json!([{
            "path": "code",
            "message": "doesn't match the authenticator app; enter its current code"
        }])
    );
    assert!(!seen.text.contains(&wrong));
    assert_eq!(
        setup.confirm(&token, &setup.code(&app, 0)).await.status,
        200
    );
}

#[sqlx::test(migrations = false)]
async fn a_code_that_isnt_six_digits_is_422_without_showing_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let token = setup.setup_token();
    setup.enroll(&token).await;

    for code in ["12345", "abcdefg", "9876543", &"7".repeat(65)] {
        let seen = setup.confirm(&token, code).await;

        assert_eq!(seen.status, 422, "{code}: {}", seen.text);
        assert_eq!(seen.body["error"]["fields"][0]["path"], "code");
        assert!(!seen.text.contains(code), "{}", seen.text);
    }
}

// --- The JSON rejections (P7.6's Phase 6 note) --------------------------------------

/// `POST /confirm` with `token`, `content_type` and a raw `body`.
async fn raw_confirm(
    setup: &Setup,
    token: &SecretString,
    content_type: Option<&str>,
    body: String,
) -> Seen {
    let mut request =
        Request::post(CONFIRM).header("authorization", format!("Bearer {}", token.expose_secret()));
    if let Some(content_type) = content_type {
        request = request.header("content-type", content_type);
    }
    setup.send(request.body(Body::from(body)).unwrap()).await
}

#[sqlx::test(migrations = false)]
async fn a_body_that_isnt_json_or_has_the_wrong_shape_answers_without_details(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let token = setup.setup_token();
    let json = Some("application/json");
    let cases: [(&str, Option<&str>, String, u16, &str); 7] = [
        (
            "no content type",
            None,
            r#"{"code":"123456"}"#.to_owned(),
            415,
            "unsupported_media_type",
        ),
        (
            "a text content type",
            Some("text/plain"),
            r#"{"code":"123456"}"#.to_owned(),
            415,
            "unsupported_media_type",
        ),
        (
            "malformed",
            json,
            r#"{"code":"1234"#.to_owned(),
            400,
            "bad_request",
        ),
        (
            "an unknown field",
            json,
            r#"{"code":"123456","secret":"hunter2"}"#.to_owned(),
            400,
            "bad_request",
        ),
        ("a missing field", json, "{}".to_owned(), 400, "bad_request"),
        (
            "a mistyped field",
            json,
            r#"{"code":654321}"#.to_owned(),
            400,
            "bad_request",
        ),
        (
            "too large",
            json,
            format!(r#"{{"code":"{}"}}"#, "1".repeat(70_000)),
            413,
            "payload_too_large",
        ),
    ];

    for (case, content_type, body, status, code) in cases {
        let seen = raw_confirm(&setup, &token, content_type, body).await;

        assert_eq!(seen.status, status, "{case}: {}", seen.text);
        assert_eq!(seen.code(), code, "{case}");
        assert!(seen.body["error"].get("fields").is_none(), "{case}");
        for value in ["hunter2", "654321", "secret"] {
            assert!(!seen.text.contains(value), "{case}: {}", seen.text);
        }
    }
}

// --- Reset (the replacement) -------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn reset_needs_a_fresh_step_up(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    setup.enrolled().await;
    let (session, access) = setup.session().await;

    let without = setup.post(RESET, Some(&access), None).await;
    setup.step_up(session).await;
    setup.clock.advance(Duration::from_secs(301));
    let stale = setup.post(RESET, Some(&access), None).await;

    assert_eq!(without.status, 403, "{}", without.text);
    assert_eq!(without.code(), "step_up_required");
    assert_eq!(stale.status, 403, "{}", stale.text);
    assert_eq!(stale.code(), "step_up_required");
}

#[sqlx::test(migrations = false)]
async fn reset_without_2fa_is_a_conflict(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = setup(options).await;
    let (session, access) = setup.session().await;
    setup.step_up(session).await;

    let seen = setup.post(RESET, Some(&access), None).await;

    assert_eq!(seen.status, 409, "{}", seen.text);
}

#[sqlx::test(migrations = false)]
async fn reset_answers_a_setup_token_whose_confirm_revokes_the_other_sessions(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    setup.enrolled().await;
    let (session, access) = setup.session().await;
    let (other, other_access) = setup.session().await;
    setup.step_up(session).await;
    setup.step_up(other).await;

    let started = setup.post(RESET, Some(&access), None).await;
    assert_eq!(started.status, 200, "{}", started.text);
    assert_eq!(started.body["expires_in"], 600);
    assert_eq!(started.cache_control.as_deref(), Some("no-store"));
    let token = SecretString::from(started.body["setup_token"].as_str().unwrap());
    setup.clock.advance(Duration::from_secs(30));
    let app = setup.enroll(&token).await;
    let confirmed = setup.confirm(&token, &setup.code(&app, 0)).await;

    assert_eq!(confirmed.status, 200, "{}", confirmed.text);
    let other_now = setup.post(RESET, Some(&other_access), None).await;
    assert_eq!(
        other_now.status, 401,
        "the other session is revoked: {}",
        other_now.text
    );
    let this_now = setup.post(RESET, Some(&access), None).await;
    assert_eq!(
        this_now.status, 200,
        "this session keeps working: {}",
        this_now.text
    );
}

#[sqlx::test(migrations = false)]
async fn reset_spends_the_users_limit(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let one = Rate::per_second(nonzero(1), nonzero(1));
    let setup = setup_with(
        options,
        &RateLimitConfig {
            per_user: one,
            ..RateLimitConfig::default()
        },
    )
    .await;
    let (session, access) = setup.session().await;
    setup.step_up(session).await;

    let first = setup.post(RESET, Some(&access), None).await;
    let second = setup.post(RESET, Some(&access), None).await;

    assert_eq!(first.status, 409, "no 2FA yet: {}", first.text);
    assert_eq!(second.status, 429, "{}", second.text);
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

/// The servers' settings' service, which these tests never write through
/// (P11.9).
fn idle_server_settings(
    db: &fleet_server::infra::sqlite::Database,
) -> fleet_server::app::server_settings::ServerSettingsService {
    fleet_server::app::server_settings::ServerSettingsService::new(
        Arc::new(fleet_testkit::system::ManualClock::new(
            chrono::DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
        )),
        Arc::new(fleet_testkit::system::SeededRandom::new(1)),
        Arc::new(db.clone()),
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
