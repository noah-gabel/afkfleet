//! What the auth tests share: a server over a fresh database, its clock and
//! its services, a user with a real Argon2id password, more users by role,
//! their sessions and invites, requests, and redactions for snapshots.

use core::time::Duration;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::Method;
use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::auth::{InviteTtl, LockoutPolicy, NewPassword, SessionPolicy};
use fleet_core::authz::Role;
use fleet_core::id::{InviteId, SessionId, UserId};
use fleet_core::pki::CertFingerprint;
use fleet_core::system::Clock as _;
use fleet_core::value::Username;
use fleet_server::app::auth::TokenService;
use fleet_server::app::challenges::{ChallengeUser, Challenges, SetupPurpose};
use fleet_server::app::enrollment::EnrollmentTokenService;
use fleet_server::app::health::HealthService;
use fleet_server::app::invites::InviteService;
use fleet_server::app::lockout::LockoutService;
use fleet_server::app::login::LoginService;
use fleet_server::app::mfa::{MfaPorts, MfaService};
use fleet_server::app::users::UserService;
use fleet_server::config::{Argon2Params, HttpConfig, RateLimitConfig};
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
use fleet_server::ports::accounts::AccountReads;
use fleet_server::ports::agents::AgentReads;
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditReads, AuditRecord, NewAuditEntry};
use fleet_server::ports::enrollment_tokens::EnrollmentTokenReads;
use fleet_server::ports::envelopes::EnvelopeReads;
use fleet_server::ports::grants::GrantReads;
use fleet_server::ports::invites::{InviteReads, NewInvite};
use fleet_server::ports::login_attempts::LoginAttemptReads;
use fleet_server::ports::mfa::MfaReads;
use fleet_server::ports::passwords::{HashError, Passwords, VerifyError};
use fleet_server::ports::sessions::SessionReads;
use fleet_server::ports::store::{Store, StoreError, WriteTx};
use fleet_server::ports::tokens::TokenHash;
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
pub(crate) const ENROLLMENT_TOKENS: &str = "/api/v1/agents/enrollment-tokens";
pub(crate) const USERS: &str = "/api/v1/users";
pub(crate) const INVITES: &str = "/api/v1/invites";
pub(crate) const REGISTER: &str = "/api/v1/auth/register";
pub(crate) const ME: &str = "/api/v1/me";
pub(crate) const ME_PASSWORD: &str = "/api/v1/me/password";
pub(crate) const ME_SESSIONS: &str = "/api/v1/me/sessions";

/// A valid Argon2id PHC string (not a real hash), for users who never log
/// in with a password.
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// An invite's default lifetime, in milliseconds.
pub(crate) const INVITE_TTL: i64 = 72 * 3_600_000;
/// The CA fingerprint the tests' enrollment tokens pin.
pub(crate) const CA_FINGERPRINT: CertFingerprint = CertFingerprint::from_digest([0xAB; 32]);

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
    pub(crate) invite_ttl: InviteTtl,
    /// Whether the control plane is on: the enrollment tokens' route
    /// answers 404 without it.
    pub(crate) control_plane: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            limits: RateLimitConfig::default(),
            rehash: Rehash::Works,
            hash: None,
            invite_ttl: InviteTtl::default(),
            control_plane: false,
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
    /// A user's role changes.
    SetRole(UserId, Role),
    /// A user is disabled.
    DisableUser(UserId),
    /// An invite is used by a user.
    UseInvite(InviteId, UserId),
    /// Another user takes this username.
    AddUser(&'static str),
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
            Meanwhile::SetRole(user, role) => {
                let mut tx = self.inner.write().await.unwrap();
                assert!(tx.users().set_role(user, role).await.unwrap());
                tx.commit(setup_entry()).await.unwrap();
            }
            Meanwhile::DisableUser(user) => {
                let mut tx = self.inner.write().await.unwrap();
                assert!(tx.users().set_disabled(user, true).await.unwrap());
                tx.commit(setup_entry()).await.unwrap();
            }
            Meanwhile::UseInvite(invite, user) => {
                let mut tx = self.inner.write().await.unwrap();
                assert!(tx.invites().use_invite(invite, user, now).await.unwrap());
                tx.commit(setup_entry()).await.unwrap();
            }
            Meanwhile::AddUser(name) => {
                let mut tx = self.inner.write().await.unwrap();
                tx.users()
                    .insert(&NewUser {
                        id: UserId::new_v7(now, [0xEE; 10]).unwrap(),
                        username: Username::try_from(name).unwrap(),
                        password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
                        role: Role::Member,
                        created_at: now,
                    })
                    .await
                    .unwrap();
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

    fn accounts(&self) -> &dyn AccountReads {
        self.inner.accounts()
    }

    fn grants(&self) -> &dyn GrantReads {
        self.inner.grants()
    }

    fn enrollment_tokens(&self) -> &dyn EnrollmentTokenReads {
        self.inner.enrollment_tokens()
    }

    fn agents(&self) -> &dyn AgentReads {
        self.inner.agents()
    }

    fn envelopes(&self) -> &dyn EnvelopeReads {
        self.inner.envelopes()
    }

    fn audit(&self) -> &dyn AuditReads {
        self.inner.audit()
    }

    async fn ping(&self) -> Result<(), StoreError> {
        self.inner.ping().await
    }
}

/// A user with a live session.
pub(crate) struct Person {
    pub(crate) id: UserId,
    pub(crate) session: SessionId,
    pub(crate) token: SecretString,
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
    /// The login service the app runs, for a test of an error the HTTP
    /// answer doesn't tell apart.
    pub(crate) login: LoginService,
    /// The random source every service shares, for a test that makes it fail.
    pub(crate) random: SeededRandom,
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
    let passwords = Arc::new(TestPasswords {
        real: Argon2Passwords::new(Argon2Params::default(), Arc::new(random.clone()))
            .await
            .unwrap(),
        rehash: settings.rehash,
    });
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
        Arc::new(random.clone()),
        Arc::new(hooked.clone()),
        passwords.clone(),
        lockout.clone(),
        tokens.clone(),
        mfa.clone(),
    );
    let (users, invites) = accounts(&clock, &random, &hooked, passwords, settings.invite_ttl);
    let limits = RateLimits::new(&settings.limits, Arc::new(clock.clone()));
    let enrollment_tokens = settings
        .control_plane
        .then(|| enrollment_tokens(&clock, &random, &hooked));
    let state = AppState::new(Services {
        health: HealthService::new(Arc::new(db.clone())),
        tokens: tokens.clone(),
        limits,
        mfa: mfa.clone(),
        login: login.clone(),
        users,
        invites,
        link_flows: idle_link_flows(&db),
        accounts: idle_accounts(&db),
        grants: idle_grants(&db),
        enrollment_tokens,
    });
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
        login,
        random,
        alice,
    }
}

/// The enrollment tokens' service over `hooked`, pinning [`CA_FINGERPRINT`].
fn enrollment_tokens(
    clock: &ManualClock,
    random: &SeededRandom,
    hooked: &HookedStore,
) -> EnrollmentTokenService {
    EnrollmentTokenService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(hooked.clone()),
        Arc::new(Sha256Tokens::new(Arc::new(random.clone()))),
        CA_FINGERPRINT,
    )
}

/// The users' and invites' services over `hooked`, so a queued change
/// lands right before their write transactions too.
fn accounts(
    clock: &ManualClock,
    random: &SeededRandom,
    hooked: &HookedStore,
    passwords: Arc<TestPasswords>,
    ttl: InviteTtl,
) -> (UserService, InviteService) {
    let users = UserService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(hooked.clone()),
        passwords,
    );
    let invites = InviteService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(hooked.clone()),
        Arc::new(Sha256Tokens::new(Arc::new(random.clone()))),
        ttl,
    );
    (users, invites)
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
        self.request(Method::POST, path, bearer, body).await
    }

    /// `GET path` with `bearer`.
    pub(crate) async fn get(&self, path: &str, bearer: Option<&SecretString>) -> Seen {
        self.request(Method::GET, path, bearer, None).await
    }

    /// `PATCH path` with `bearer` and the JSON `body`.
    pub(crate) async fn patch(
        &self,
        path: &str,
        bearer: Option<&SecretString>,
        body: Value,
    ) -> Seen {
        self.request(Method::PATCH, path, bearer, Some(body)).await
    }

    /// `DELETE path` with `bearer`.
    pub(crate) async fn delete(&self, path: &str, bearer: Option<&SecretString>) -> Seen {
        self.request(Method::DELETE, path, bearer, None).await
    }

    /// `method path` with `bearer` and the JSON `body`, if any.
    pub(crate) async fn request(
        &self,
        method: Method,
        path: &str,
        bearer: Option<&SecretString>,
        body: Option<Value>,
    ) -> Seen {
        let mut request = Request::builder().method(method).uri(path);
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
        self.session_of(self.alice).await
    }

    /// A live session of `user`: its ID and its access token.
    pub(crate) async fn session_of(&self, user: UserId) -> (SessionId, SecretString) {
        let prepared = self.tokens.prepare(user).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.sessions().insert(&prepared.new_session).await.unwrap();
        tx.commit(setup_entry()).await.unwrap();
        (prepared.new_session.id, prepared.tokens.access_token)
    }

    /// Adds `name` with `role`, as [`Setup::add_user`] does, with a live
    /// session.
    pub(crate) async fn person(&self, n: u8, name: &str, role: Role) -> Person {
        let id = self.add_user(n, name, role).await;
        let (session, token) = self.session_of(id).await;
        Person { id, session, token }
    }

    /// Alice, a Member, with a live session.
    pub(crate) async fn alice_person(&self) -> Person {
        let (session, token) = self.session().await;
        Person {
            id: self.alice,
            session,
            token,
        }
    }

    /// Stamps `session`'s step-up now, as `POST /auth/step-up` does.
    pub(crate) async fn stamp(&self, session: SessionId) {
        let mut tx = self.db.write().await.unwrap();
        assert!(
            tx.sessions()
                .stamp_step_up(session, self.clock.now())
                .await
                .unwrap()
        );
        tx.commit(setup_entry()).await.unwrap();
    }

    /// A session of `user` whose login was `millis` from `T0` (in the past
    /// for an expired one): its ID and its access token.
    pub(crate) async fn session_since(
        &self,
        user: UserId,
        millis: i64,
    ) -> (SessionId, SecretString) {
        let mut prepared = self.tokens.prepare(user).unwrap();
        prepared.new_session.created_at = at(T0 + millis);
        let mut tx = self.db.write().await.unwrap();
        tx.sessions().insert(&prepared.new_session).await.unwrap();
        tx.commit(setup_entry()).await.unwrap();
        (prepared.new_session.id, prepared.tokens.access_token)
    }

    /// Adds an invite by `creator` for `role` with a real code, created
    /// `millis` after `T0` and living 72 hours: its ID and its code.
    pub(crate) async fn invite_code(
        &self,
        n: u8,
        creator: UserId,
        role: Role,
        millis: i64,
    ) -> (InviteId, SecretString) {
        use fleet_server::ports::tokens::{TokenKind, Tokens as _};
        let minted = Sha256Tokens::new(Arc::new(SeededRandom::new(u64::from(n) + 1_000)))
            .mint(TokenKind::Invite)
            .unwrap();
        let id = InviteId::new_v7(at(T0 + millis), [n; 10]).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.invites()
            .insert(&NewInvite {
                id,
                code_hash: minted.hash,
                role,
                created_by: creator,
                created_at: at(T0 + millis),
                expires_at: at(T0 + millis + INVITE_TTL),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        (id, minted.token)
    }

    /// Adds user `name` with `role`, its ID minted from `n`, created a minute
    /// before `T0`. Its hash is no real one: it never logs in by password.
    pub(crate) async fn add_user(&self, n: u8, name: &str, role: Role) -> UserId {
        let id = UserId::new_v7(at(T0 - 60_000), [n; 10]).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.users()
            .insert(&NewUser {
                id,
                username: Username::try_from(name).unwrap(),
                password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
                role,
                created_at: at(T0 - 60_000),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        id
    }

    /// Adds an invite by `creator` for `role`, its ID and its code's hash
    /// minted from `n`, created `millis` after `T0` and living 72 hours.
    pub(crate) async fn add_invite(
        &self,
        n: u8,
        creator: UserId,
        role: Role,
        millis: i64,
    ) -> InviteId {
        let id = InviteId::new_v7(at(T0 + millis), [n; 10]).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.invites()
            .insert(&NewInvite {
                id,
                code_hash: TokenHash::from_bytes([n; 32]),
                role,
                created_by: creator,
                created_at: at(T0 + millis),
                expires_at: at(T0 + millis + INVITE_TTL),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        id
    }

    /// Marks `invite` used by `user` now.
    pub(crate) async fn use_invite(&self, invite: InviteId, user: UserId) {
        let mut tx = self.db.write().await.unwrap();
        assert!(
            tx.invites()
                .use_invite(invite, user, self.clock.now())
                .await
                .unwrap()
        );
        tx.commit(setup_entry()).await.unwrap();
    }

    /// Whether `user`'s session `id` is still live.
    pub(crate) async fn live(&self, id: SessionId) -> bool {
        let mut tx = self.db.write().await.unwrap();
        tx.sessions()
            .find(id)
            .await
            .unwrap()
            .is_some_and(|found| found.session.revoked_at.is_none())
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

/// `value` for a snapshot: every ID that `names` lists becomes its name,
/// any other ID `[id]`, and every time its distance from `T0`
/// (`[T0+60s]`), so nothing minted from `SeededRandom` is pinned.
pub(crate) fn redacted(value: &Value, names: &[(String, &str)]) -> Value {
    match value {
        Value::String(text) => {
            if let Some((_, name)) = names.iter().find(|(id, _)| id == text) {
                return json!(format!("[{name}]"));
            }
            if text.parse::<UserId>().is_ok() {
                return json!("[id]");
            }
            if let Ok(time) = serde_json::from_value::<fleet_api_types::Timestamp>(value.clone()) {
                let millis = time.get().timestamp_millis() - T0;
                return json!(match millis {
                    0 => "[T0]".to_owned(),
                    m if m % 1000 == 0 => format!("[T0{:+}s]", m / 1000),
                    m => format!("[T0{m:+}ms]"),
                });
            }
            value.clone()
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| redacted(item, names)).collect())
        }
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, item)| (key.clone(), redacted(item, names)))
                .collect(),
        ),
        other => other.clone(),
    }
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
