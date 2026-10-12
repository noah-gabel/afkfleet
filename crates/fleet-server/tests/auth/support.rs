//! What the login tests share: a server over a fresh database, its clock and
//! its services, a user with a real Argon2id password, and requests.

use core::time::Duration;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::extract::connect_info::MockConnectInfo;
use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::auth::{LockoutPolicy, NewPassword, SessionPolicy};
use fleet_core::authz::Role;
use fleet_core::id::{SessionId, UserId};
use fleet_core::system::Clock as _;
use fleet_core::value::Username;
use fleet_server::app::auth::TokenService;
use fleet_server::app::challenges::{ChallengeUser, Challenges, SetupPurpose};
use fleet_server::app::health::HealthService;
use fleet_server::app::lockout::LockoutService;
use fleet_server::app::login::LoginService;
use fleet_server::app::mfa::{MfaPorts, MfaService};
use fleet_server::config::{Argon2Params, HttpConfig, RateLimitConfig};
use fleet_server::http::middleware::RequestIds;
use fleet_server::http::openapi::ApiDocs;
use fleet_server::http::rate_limit::{RateLimits, TrustedProxies};
use fleet_server::http::router::{AppState, router};
use fleet_server::infra::crypto::attempt_keys::HmacAttemptKeys;
use fleet_server::infra::crypto::passwords::Argon2Passwords;
use fleet_server::infra::crypto::recovery_codes::Sha256RecoveryCodes;
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::infra::crypto::totp::TotpRs;
use fleet_server::infra::crypto::vault::{VaultKeys, XChaChaVault};
use fleet_server::infra::qr::SvgQr;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditReads, AuditRecord, NewAuditEntry};
use fleet_server::ports::invites::InviteReads;
use fleet_server::ports::login_attempts::LoginAttemptReads;
use fleet_server::ports::mfa::MfaReads;
use fleet_server::ports::passwords::{HashError, Passwords, VerifyError};
use fleet_server::ports::sessions::SessionReads;
use fleet_server::ports::store::{Store, StoreError, WriteTx};
use fleet_server::ports::users::UserReads;
use fleet_server::ports::users::{NewUser, PasswordHash};
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::{ExposeSecret as _, SecretBox, SecretString};
use serde_json::{Value, json};
use sqlx::sqlite::SqliteConnectOptions;
use totp_rs::Totp;
use tower::ServiceExt as _;

/// When every test starts: in the middle of a TOTP step.
pub(crate) const T0: i64 = 1_800_000_015_000;
/// Alice's password: strong, so the password service hashes it.
pub(crate) const PASSWORD: &str = "correct horse battery staple";

pub(crate) const LOGIN: &str = "/api/v1/auth/login";
pub(crate) const LOGIN_MFA: &str = "/api/v1/auth/login/mfa";
pub(crate) const REFRESH: &str = "/api/v1/auth/refresh";
pub(crate) const LOGOUT: &str = "/api/v1/auth/logout";
pub(crate) const STEP_UP: &str = "/api/v1/auth/step-up";
pub(crate) const RESET: &str = "/api/v1/me/mfa/reset";
pub(crate) const ENROLL: &str = "/api/v1/me/mfa/enroll";
pub(crate) const CONFIRM: &str = "/api/v1/me/mfa/confirm";

pub(crate) fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

/// What the re-hash does in a test's password service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rehash {
    /// The real one.
    Works,
    /// The queue is full.
    Busy,
    /// Hashing fails.
    Fails,
}

/// The real password service, whose re-hash a test can make fail.
#[derive(Debug)]
pub(crate) struct TestPasswords {
    real: Argon2Passwords,
    rehash: Rehash,
}

#[async_trait]
impl Passwords for TestPasswords {
    async fn hash(
        &self,
        password: &NewPassword,
        username: &Username,
    ) -> Result<PasswordHash, HashError> {
        self.real.hash(password, username).await
    }

    async fn verify(
        &self,
        password: &SecretString,
        hash: &PasswordHash,
    ) -> Result<bool, VerifyError> {
        self.real.verify(password, hash).await
    }

    async fn verify_unknown_user(&self, password: &SecretString) -> Result<bool, VerifyError> {
        self.real.verify_unknown_user(password).await
    }

    fn needs_rehash(&self, hash: &PasswordHash) -> bool {
        self.real.needs_rehash(hash)
    }

    async fn rehash_verified(&self, password: &SecretString) -> Result<PasswordHash, HashError> {
        match self.rehash {
            Rehash::Works => self.real.rehash_verified(password).await,
            Rehash::Busy => Err(HashError::Busy),
            Rehash::Fails => Err(HashError::Failed("a test's hashing failure".into())),
        }
    }
}

/// How a test's server is set up.
pub(crate) struct Options {
    pub(crate) limits: RateLimitConfig,
    pub(crate) rehash: Rehash,
    /// Alice's stored hash, when it isn't one of today's parameters.
    pub(crate) hash: Option<PasswordHash>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            limits: RateLimitConfig::default(),
            rehash: Rehash::Works,
            hash: None,
        }
    }
}

/// What changes, as if another request committed it, right before the
/// login service's next write transaction opens: between its checks and
/// its commit.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Meanwhile {
    /// The session is revoked.
    RevokeSession(SessionId),
    /// Alice's password changes (only its time; the hash stays).
    ChangePassword,
    /// Alice is disabled.
    Disable,
    /// Alice's authenticator is replaced (only its confirmation time).
    ReplaceMfa,
}

/// The database, with a change queued to land right before the next write
/// transaction opens.
#[derive(Debug, Clone)]
pub(crate) struct HookedStore {
    inner: Database,
    alice: UserId,
    clock: ManualClock,
    queued: Arc<Mutex<Option<Meanwhile>>>,
}

impl HookedStore {
    /// Queues `change` for the next write transaction.
    pub(crate) fn queue(&self, change: Meanwhile) {
        *self.queued.lock().unwrap() = Some(change);
    }

    async fn apply(&self, change: Meanwhile) {
        let now = self.clock.now();
        let id = self.alice.to_string();
        match change {
            Meanwhile::RevokeSession(session) => {
                let mut tx = self.inner.write().await.unwrap();
                assert!(tx.sessions().revoke(session, now).await.unwrap());
                tx.commit(setup_entry()).await.unwrap();
            }
            Meanwhile::ChangePassword => {
                let changed = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
                sqlx::query!(
                    "UPDATE users SET password_changed_at = ? WHERE id = ?",
                    changed,
                    id
                )
                .execute(self.inner.write_pool())
                .await
                .unwrap();
            }
            Meanwhile::Disable => {
                sqlx::query!("UPDATE users SET disabled = 1 WHERE id = ?", id)
                    .execute(self.inner.write_pool())
                    .await
                    .unwrap();
            }
            Meanwhile::ReplaceMfa => {
                let mut tx = self.inner.write().await.unwrap();
                let mut current = tx.mfa().find(self.alice).await.unwrap().unwrap();
                current.confirmed_at = now + chrono::TimeDelta::milliseconds(1);
                assert!(tx.mfa().replace(&current).await.unwrap());
                tx.commit(setup_entry()).await.unwrap();
            }
        }
    }
}

#[async_trait]
impl Store for HookedStore {
    async fn write(&self) -> Result<Box<dyn WriteTx>, StoreError> {
        let queued = self.queued.lock().unwrap().take();
        if let Some(change) = queued {
            self.apply(change).await;
        }
        self.inner.write().await
    }

    fn users(&self) -> &dyn UserReads {
        self.inner.users()
    }

    fn sessions(&self) -> &dyn SessionReads {
        self.inner.sessions()
    }

    fn login_attempts(&self) -> &dyn LoginAttemptReads {
        self.inner.login_attempts()
    }

    fn mfa(&self) -> &dyn MfaReads {
        self.inner.mfa()
    }

    fn invites(&self) -> &dyn InviteReads {
        self.inner.invites()
    }

    fn audit(&self) -> &dyn AuditReads {
        self.inner.audit()
    }

    async fn ping(&self) -> Result<(), StoreError> {
        self.inner.ping().await
    }
}

pub(crate) struct Setup {
    pub(crate) app: Router,
    pub(crate) hooked: HookedStore,
    pub(crate) db: Database,
    pub(crate) clock: ManualClock,
    pub(crate) tokens: TokenService,
    pub(crate) challenges: Challenges,
    pub(crate) mfa: MfaService,
    pub(crate) lockout: LockoutService,
    pub(crate) alice: UserId,
}

/// The 2FA service over `db`, its vault on an in-memory key.
fn mfa_service(
    db: &Database,
    clock: &ManualClock,
    random: &SeededRandom,
    challenges: &Challenges,
) -> MfaService {
    let keys = VaultKeys::from_keys(
        BTreeMap::from([(1, SecretBox::new(Box::new([0x42; 32])))]),
        1,
    )
    .unwrap();
    MfaService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(db.clone()),
        Arc::new(XChaChaVault::new(keys, Arc::new(random.clone()))),
        MfaPorts {
            totp: Arc::new(TotpRs::new()),
            recovery: Arc::new(Sha256RecoveryCodes::new()),
            qr: Arc::new(SvgQr::new()),
        },
        challenges.clone(),
    )
}

/// The router over `state`, behind the real stack, every request from
/// 192.0.2.10.
fn app(state: AppState, clock: &ManualClock) -> Router {
    let http = HttpConfig {
        bind: HttpConfig::DEFAULT_BIND,
        request_timeout: Duration::from_secs(15),
        max_body_bytes: 65_536,
        trusted_proxies: TrustedProxies::default(),
    };
    let ids = RequestIds::new(Arc::new(clock.clone()), Arc::new(SeededRandom::new(5)));
    router(state, &http, ids, ApiDocs::Hidden)
        .layer(MockConnectInfo(SocketAddr::from(([192, 0, 2, 10], 4711))))
}

pub(crate) async fn setup(options: SqliteConnectOptions) -> Setup {
    setup_with(options, Options::default()).await
}

pub(crate) async fn setup_with(options: SqliteConnectOptions, settings: Options) -> Setup {
    let db = Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap();
    let clock = ManualClock::new(at(T0));
    let random = SeededRandom::new(81);
    let token_port = Arc::new(Sha256Tokens::new(Arc::new(random.clone())));
    let tokens = TokenService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(db.clone()),
        token_port.clone(),
        SessionPolicy::default(),
    );
    let challenges = Challenges::new(Arc::new(clock.clone()), token_port);
    let mfa = mfa_service(&db, &clock, &random, &challenges);
    let passwords = TestPasswords {
        real: Argon2Passwords::new(Argon2Params::default(), Arc::new(random.clone()))
            .await
            .unwrap(),
        rehash: settings.rehash,
    };
    let alice_hash = match settings.hash {
        Some(hash) => hash,
        None => passwords
            .hash(
                &NewPassword::try_from(SecretString::from(PASSWORD)).unwrap(),
                &Username::try_from("alice").unwrap(),
            )
            .await
            .unwrap(),
    };
    let lockout = LockoutService::new(
        Arc::new(clock.clone()),
        Arc::new(db.clone()),
        Arc::new(HmacAttemptKeys::new(SecretBox::new(Box::new([7; 32])))),
        LockoutPolicy::default(),
    );
    let alice = UserId::new_v7(at(T0), [1; 10]).unwrap();
    let hooked = HookedStore {
        inner: db.clone(),
        alice,
        clock: clock.clone(),
        queued: Arc::default(),
    };
    let login = LoginService::new(
        Arc::new(clock.clone()),
        Arc::new(hooked.clone()),
        Arc::new(passwords),
        lockout.clone(),
        tokens.clone(),
        mfa.clone(),
    );
    let limits = RateLimits::new(&settings.limits, Arc::new(clock.clone()));
    let state = AppState::new(
        HealthService::new(Arc::new(db.clone())),
        tokens.clone(),
        limits,
        mfa.clone(),
        login,
    );
    let app = app(state, &clock);
    let mut tx = db.write().await.unwrap();
    tx.users()
        .insert(&NewUser {
            id: alice,
            username: Username::try_from("alice").unwrap(),
            password_hash: alice_hash,
            role: Role::Member,
            created_at: at(T0 - 60_000),
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    Setup {
        app,
        hooked,
        db,
        clock,
        tokens,
        challenges,
        mfa,
        lockout,
        alice,
    }
}

pub(crate) fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from("test.login_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// What a client sees of a response.
#[derive(Debug)]
pub(crate) struct Seen {
    pub(crate) status: u16,
    pub(crate) body: Value,
    pub(crate) text: String,
    pub(crate) headers: BTreeMap<String, String>,
}

impl Seen {
    pub(crate) fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or_default()
    }

    pub(crate) fn secret(&self, field: &str) -> SecretString {
        SecretString::from(self.body[field].as_str().unwrap())
    }

    /// The body without its request ID, which differs per request.
    pub(crate) fn body_without_request_id(&self) -> Value {
        let mut body = self.body.clone();
        if let Some(error) = body.get_mut("error").and_then(Value::as_object_mut) {
            error.remove("request_id");
        }
        body
    }
}

impl Setup {
    pub(crate) async fn send(&self, request: Request) -> Seen {
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter(|(name, _)| name.as_str() != "x-request-id")
            .map(|(name, value)| (name.to_string(), value.to_str().unwrap().to_owned()))
            .collect();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        Seen {
            status,
            body,
            text,
            headers,
        }
    }

    /// `POST path` with `bearer` and the JSON `body`, if any.
    pub(crate) async fn post(
        &self,
        path: &str,
        bearer: Option<&SecretString>,
        body: Option<Value>,
    ) -> Seen {
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

    pub(crate) async fn login(&self, username: &str, password: &str) -> Seen {
        self.post(
            LOGIN,
            None,
            Some(json!({ "username": username, "password": password })),
        )
        .await
    }

    pub(crate) async fn code_step(&self, mfa_token: &SecretString, code: &str) -> Seen {
        self.post(
            LOGIN_MFA,
            None,
            Some(json!({ "mfa_token": mfa_token.expose_secret(), "code": code })),
        )
        .await
    }

    /// Alice's 2FA, confirmed through the service: the authenticator app and
    /// the recovery codes. The clock moves on a step, so the confirming code
    /// is behind.
    pub(crate) async fn enroll_alice(&self) -> (Totp, Vec<SecretString>) {
        let issued = self
            .challenges
            .issue_setup(self.alice_user(), SetupPurpose::FirstEnrollment)
            .unwrap();
        let grant = self.challenges.setup(issued.token.expose_secret()).unwrap();
        let enrollment = self.mfa.enroll(&grant).await.unwrap();
        let app = Totp::from_url(enrollment.otpauth_uri.expose_secret()).unwrap();
        let code = fleet_core::auth::TotpCode::parse(&self.code(&app, 0)).unwrap();
        let codes = self
            .mfa
            .confirm(&grant, &code, "192.0.2.10".parse().unwrap())
            .await
            .unwrap();
        self.clock.advance(Duration::from_secs(30));
        (app, codes)
    }

    pub(crate) fn alice_user(&self) -> ChallengeUser {
        ChallengeUser {
            id: self.alice,
            role: Role::Member,
            password_changed_at: at(T0 - 60_000),
        }
    }

    /// The app's code now, `steps` steps away.
    pub(crate) fn code(&self, app: &Totp, steps: i64) -> String {
        let now = u64::try_from(self.clock.now().timestamp() + steps * 30).unwrap();
        app.generate(now).to_string()
    }

    /// Logs Alice in with her app: the token response.
    pub(crate) async fn full_login(&self, app: &Totp) -> Seen {
        let first = self.login("alice", PASSWORD).await;
        assert_eq!(first.status, 200, "{}", first.text);
        let seen = self
            .code_step(&first.secret("token"), &self.code(app, 0))
            .await;
        assert_eq!(seen.status, 200, "{}", seen.text);
        self.clock.advance(Duration::from_secs(30));
        seen
    }

    /// A live session of Alice: its ID and its access token.
    pub(crate) async fn session(&self) -> (SessionId, SecretString) {
        let prepared = self.tokens.prepare(self.alice).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.sessions().insert(&prepared.new_session).await.unwrap();
        tx.commit(setup_entry()).await.unwrap();
        (prepared.new_session.id, prepared.tokens.access_token)
    }

    /// The audit log, newest first.
    pub(crate) async fn audit(&self) -> Vec<AuditRecord> {
        self.db
            .audit()
            .list(AuditPage {
                before: None,
                limit: AuditLimit::new(100).unwrap(),
            })
            .await
            .unwrap()
    }

    /// The newest entry of `action`.
    pub(crate) async fn last(&self, action: &AuditAction) -> AuditRecord {
        self.audit()
            .await
            .into_iter()
            .find(|entry| &entry.action == action)
            .unwrap()
    }

    /// Alice's stored password hash.
    pub(crate) async fn alice_hash(&self) -> String {
        self.db
            .users()
            .get(self.alice)
            .await
            .unwrap()
            .unwrap()
            .password_hash
            .expose_secret()
            .to_owned()
    }

    /// Alice's failure streak, if any.
    pub(crate) async fn streak(&self) -> Option<u32> {
        use fleet_server::ports::login_attempts::AttemptKeys as _;
        let key = HmacAttemptKeys::new(SecretBox::new(Box::new([7; 32])))
            .key(&Username::try_from("alice").unwrap());
        self.db
            .login_attempts()
            .find(&key)
            .await
            .unwrap()
            .map(|attempt| attempt.failures)
    }
}
