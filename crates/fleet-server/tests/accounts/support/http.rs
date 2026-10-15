//! The link routes' harness (P9.4): serve's own app (`app_with`) over a
//! fresh database, behind the real router and middleware, each request
//! through tower's `oneshot` from 192.0.2.10. Users of every role get live
//! sessions with real tokens, so logout and refresh go through their own
//! routes. Every value here is invented.
//!
//! - [`Server`]: the app over any ports; the hands-on check runs it over
//!   Microsoft itself.
//! - [`HttpSetup`]: the app over the Microsoft fake, the pacing fake and a
//!   manual clock, for the route tests.

use core::ops::{Deref, DerefMut};
use core::time::Duration;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request};
use fleet_core::account::AccountStatus;
use fleet_core::audit::{AuditAction, RecordedValue};
use fleet_core::auth::SessionPolicy;
use fleet_core::authz::{Actor, GrantLevel, Role};
use fleet_core::id::{AccountId, SessionId, UserId};
use fleet_core::system::{Clock, SecureRandom};
use fleet_core::value::Username;
use fleet_server::app::auth::TokenService;
use fleet_server::app::link_flows::LinkFlowService;
use fleet_server::app::users::ActingUser;
use fleet_server::cli::serve::{App, ServerPorts, app_with};
use fleet_server::config::{
    AccountsConfig, Argon2Params, AuthConfig, DatabaseConfig, HttpConfig, LogConfig, LogFormat,
    RateLimitConfig, ServerConfig, VaultConfig,
};
use fleet_server::http::rate_limit::TrustedProxies;
use fleet_server::infra::crypto::passwords::Argon2Passwords;
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::infra::crypto::vault::{DerivedKey, VaultKeys};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditRecord};
use fleet_server::ports::grants::AccountGrant;
use fleet_server::ports::store::Store as _;
use fleet_server::ports::users::{NewUser, PasswordHash};
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::{ExposeSecret as _, SecretBox, SecretString};
use serde_json::Value;
use sqlx::sqlite::SqliteConnectOptions;
use tower::ServiceExt as _;

use super::link::IP;
use super::msauth::FakeMicrosoftAuth;
use super::pace::FakePace;
use super::service::{T0, at, profile, setup_entry};

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";

/// A user with a live session: their ID and role, the session, and its
/// tokens.
pub(crate) struct Person {
    pub(crate) id: UserId,
    pub(crate) role: Role,
    pub(crate) session: SessionId,
    pub(crate) access: SecretString,
    pub(crate) refresh: SecretString,
}

impl Person {
    /// The person as the link service sees their requests.
    pub(crate) const fn acting(&self) -> ActingUser {
        ActingUser {
            actor: Actor {
                id: self.id,
                role: self.role,
            },
            session_id: self.session,
            ip: IP,
        }
    }
}

/// What a client sees of a response.
#[derive(Debug)]
pub(crate) struct Seen {
    pub(crate) status: u16,
    pub(crate) body: Value,
    pub(crate) text: String,
    pub(crate) retry_after: Option<String>,
}

impl Seen {
    pub(crate) fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or_default()
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

/// The server's config with `limits` and `accounts`; the database's path
/// and the vault's key file are never read here.
fn config(limits: RateLimitConfig, accounts: AccountsConfig) -> ServerConfig {
    ServerConfig {
        dev_mode: false,
        http: HttpConfig {
            bind: HttpConfig::DEFAULT_BIND,
            request_timeout: Duration::from_secs(15),
            max_body_bytes: 65_536,
            trusted_proxies: TrustedProxies::default(),
        },
        database: DatabaseConfig {
            path: std::env::temp_dir().join("unused.db"),
        },
        log: LogConfig {
            format: LogFormat::Json,
            filter: "info".parse().unwrap(),
        },
        auth: AuthConfig::default(),
        rate_limits: limits,
        vault: VaultConfig {
            key_files: BTreeMap::from([(1, std::env::temp_dir().join("unused-key"))]),
            active_key_id: 1,
        },
        accounts,
    }
}

/// serve's app over a fresh database, and what tests need around it.
pub(crate) struct Server {
    pub(crate) db: Database,
    /// The router behind the middleware stack, every request from
    /// 192.0.2.10.
    pub(crate) router: Router,
    /// The app's link flows.
    pub(crate) link_flows: LinkFlowService,
    /// The app as `serve` takes it, until a test takes it.
    pub(crate) app: Option<App>,
    tokens: TokenService,
}

impl Server {
    /// The app over `ports` with `limits` and `accounts`, its vault's one
    /// key `key`; sessions are minted on the ports' clock and randomness.
    pub(crate) async fn start(
        options: SqliteConnectOptions,
        limits: RateLimitConfig,
        accounts: AccountsConfig,
        key: &SecretBox<[u8; 32]>,
        ports: ServerPorts,
    ) -> Self {
        let db = Database::connect(options, DatabaseOptions::default())
            .await
            .unwrap();
        let keys = VaultKeys::from_keys(
            BTreeMap::from([(1, SecretBox::new(Box::new(*key.expose_secret())))]),
            1,
        )
        .unwrap();
        let lockout_key = keys.derive_key(DerivedKey::LoginAttempts).unwrap();
        let clock: Arc<dyn Clock> = Arc::clone(&ports.clock);
        let random: Arc<dyn SecureRandom> = Arc::clone(&ports.random);
        let passwords = Argon2Passwords::new(Argon2Params::default(), Arc::clone(&random))
            .await
            .unwrap();
        let app = app_with(
            &config(limits, accounts),
            &db,
            keys,
            lockout_key,
            Arc::new(passwords),
            ports,
        );
        let tokens = TokenService::new(
            clock,
            Arc::clone(&random),
            Arc::new(db.clone()),
            Arc::new(Sha256Tokens::new(random)),
            SessionPolicy::default(),
        );
        Self {
            router: app
                .router
                .clone()
                .layer(MockConnectInfo(SocketAddr::new(IP, 4711))),
            link_flows: app.link_flows.clone(),
            app: Some(app),
            db,
            tokens,
        }
    }

    /// Adds `user<n>` with `role`.
    pub(crate) async fn add_user(&self, n: u8, role: Role) -> UserId {
        let id = UserId::new_v7(at(T0), [n; 10]).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.users()
            .insert(&NewUser {
                id,
                username: Username::try_from(format!("user{n}").as_str()).unwrap(),
                password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
                role,
                created_at: at(T0),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        id
    }

    /// A new live session of `id`, who has `role`.
    pub(crate) async fn session_of(&self, id: UserId, role: Role) -> Person {
        let prepared = self.tokens.prepare(id).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.sessions().insert(&prepared.new_session).await.unwrap();
        tx.commit(setup_entry()).await.unwrap();
        Person {
            id,
            role,
            session: prepared.new_session.id,
            access: prepared.tokens.access_token,
            refresh: prepared.tokens.refresh_token,
        }
    }

    /// Adds `user<n>` with `role` and a live session.
    pub(crate) async fn person(&self, n: u8, role: Role) -> Person {
        let id = self.add_user(n, role).await;
        self.session_of(id, role).await
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
        let response = self.router.clone().oneshot(request.unwrap()).await.unwrap();
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .map(|value| value.to_str().unwrap().to_owned());
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        Seen {
            status,
            body,
            text,
            retry_after,
        }
    }

    /// `POST /api/v1/accounts/link` as `by`.
    pub(crate) async fn start_link(&self, by: &Person) -> Seen {
        self.request(
            Method::POST,
            "/api/v1/accounts/link",
            Some(&by.access),
            None,
        )
        .await
    }

    /// `GET /api/v1/accounts/link/{flow}` as `by`.
    pub(crate) async fn status(&self, by: &Person, flow: &str) -> Seen {
        self.request(
            Method::GET,
            &format!("/api/v1/accounts/link/{flow}"),
            Some(&by.access),
            None,
        )
        .await
    }

    /// `method /api/v1{path}` as `by`, with `body`.
    pub(crate) async fn api(
        &self,
        method: Method,
        path: &str,
        by: &Person,
        body: Option<Value>,
    ) -> Seen {
        self.request(method, &format!("/api/v1{path}"), Some(&by.access), body)
            .await
    }

    /// Adds account `n` of `owner` directly: a Microsoft one in `status`
    /// for `profile(n)`, its envelope all `n`, or an offline one for `None`.
    pub(crate) async fn add_account(
        &self,
        owner: UserId,
        n: u8,
        status: Option<AccountStatus>,
    ) -> AccountId {
        let id = AccountId::new_v7(at(T0 + i64::from(n)), [n; 10]).unwrap();
        let credential = status.map_or(AccountCredential::Offline, |status| {
            AccountCredential::Microsoft {
                uuid: profile(n).parse().unwrap(),
                refresh_envelope: vec![n; 61],
                status,
            }
        });
        let mut tx = self.db.write().await.unwrap();
        tx.accounts()
            .insert(&McAccount {
                id,
                owner,
                name: format!("AfkBot{n}").parse().unwrap(),
                created_at: at(T0),
                credential,
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        id
    }

    /// Gives `user` `level` on `account` directly, as set by `by`.
    pub(crate) async fn add_grant(
        &self,
        account: AccountId,
        user: UserId,
        level: GrantLevel,
        by: UserId,
    ) {
        let mut tx = self.db.write().await.unwrap();
        tx.grants()
            .insert(&AccountGrant {
                account,
                user,
                level,
                granted_by: by,
                granted_at: at(T0),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
    }

    /// The account `id` as stored, if it is.
    pub(crate) async fn stored(&self, id: AccountId) -> Option<McAccount> {
        self.db.accounts().find(id).await.unwrap()
    }

    /// `user`'s grant on `account` as stored, if any.
    pub(crate) async fn grant_of(&self, account: AccountId, user: UserId) -> Option<AccountGrant> {
        let mut tx = self.db.write().await.unwrap();
        tx.grants().find(account, user).await.unwrap()
    }

    /// Sets `id`'s role directly.
    pub(crate) async fn set_role(&self, id: UserId, role: Role) {
        let mut tx = self.db.write().await.unwrap();
        assert!(tx.users().set_role(id, role).await.unwrap());
        tx.commit(setup_entry()).await.unwrap();
    }

    /// Disables or enables `id` directly.
    pub(crate) async fn set_disabled(&self, id: UserId, disabled: bool) {
        let mut tx = self.db.write().await.unwrap();
        assert!(tx.users().set_disabled(id, disabled).await.unwrap());
        tx.commit(setup_entry()).await.unwrap();
    }

    /// Every audit entry with `action`, oldest first.
    pub(crate) async fn entries(&self, action: &AuditAction) -> Vec<AuditRecord> {
        let page = AuditPage {
            before: None,
            limit: AuditLimit::new(100).unwrap(),
        };
        let mut entries: Vec<AuditRecord> = self
            .db
            .audit()
            .list(page)
            .await
            .unwrap()
            .into_iter()
            .filter(|entry| &entry.action == action)
            .collect();
        entries.reverse();
        entries
    }
}

/// `entry`'s metadata value for `key`, if it has one.
pub(crate) fn meta(entry: &AuditRecord, key: &str) -> Option<RecordedValue> {
    entry
        .metadata
        .entries()
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
}

/// A text metadata value.
pub(crate) fn text(value: &str) -> RecordedValue {
    RecordedValue::Text(value.to_owned())
}

/// The app over the fakes and a manual clock, for the route tests.
pub(crate) struct HttpSetup {
    server: Server,
    pub(crate) clock: ManualClock,
    pub(crate) fake: Arc<FakeMicrosoftAuth>,
    pub(crate) pace: Arc<FakePace>,
}

impl Deref for HttpSetup {
    type Target = Server;

    fn deref(&self) -> &Server {
        &self.server
    }
}

impl DerefMut for HttpSetup {
    fn deref_mut(&mut self) -> &mut Server {
        &mut self.server
    }
}

impl HttpSetup {
    pub(crate) async fn new(options: SqliteConnectOptions) -> Self {
        Self::with(
            options,
            RateLimitConfig::default(),
            AccountsConfig::default(),
        )
        .await
    }

    pub(crate) async fn with(
        options: SqliteConnectOptions,
        limits: RateLimitConfig,
        accounts: AccountsConfig,
    ) -> Self {
        let clock = ManualClock::new(at(T0));
        let random = SeededRandom::new(21);
        let fake = Arc::new(FakeMicrosoftAuth::new());
        let pace = Arc::new(FakePace::new(Arc::new(clock.clone())));
        let ports = ServerPorts {
            clock: Arc::new(clock.clone()),
            limiter_clock: Arc::new(clock.clone()),
            random: Arc::new(random),
            microsoft: fake.clone(),
            pace: pace.clone(),
        };
        Self {
            server: Server::start(
                options,
                limits,
                accounts,
                &SecretBox::new(Box::new([0x21; 32])),
                ports,
            )
            .await,
            clock,
            fake,
            pace,
        }
    }

    /// `POST /api/v1/accounts/link` as `by`.
    pub(crate) async fn start(&self, by: &Person) -> Seen {
        self.server.start_link(by).await
    }

    /// Stamps `by`'s session as stepped up now, as `POST /auth/step-up`
    /// does.
    pub(crate) async fn step_up(&self, by: &Person) {
        let mut tx = self.db.write().await.unwrap();
        assert!(
            tx.sessions()
                .stamp_step_up(by.session, self.clock.now())
                .await
                .unwrap()
        );
        tx.commit(setup_entry()).await.unwrap();
    }

    /// `user<n>` with `role`, a live session and a fresh step-up.
    pub(crate) async fn stepped_up(&self, n: u8, role: Role) -> Person {
        let person = self.person(n, role).await;
        self.step_up(&person).await;
        person
    }
}
