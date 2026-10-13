//! What the tests share: the real server, in-process with a manual clock
//! and its Owner, clients over a `MemoryStore`, and the authenticator app.

use core::time::Duration;
use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::DateTime;
use fleet_api_types::auth::{LoginRequest, RegisterRequest, StepUpRequest};
use fleet_api_types::mfa::{CODE_MAX_CHARS, MfaEnrollResponse};
use fleet_api_types::{ErrorCode, SecretText};
use fleet_client::{
    Client, ClientConfig, ClientError, LoginStep, MemoryStore, ServerUrl, SessionStore, Trust,
};
use fleet_core::auth::NewPassword;
use fleet_core::system::Clock as _;
use fleet_core::value::Username;
use fleet_server::app::users::UserService;
use fleet_server::cli::serve::{Exit, ServerPorts, ShutdownTimeouts, app_with, serve};
use fleet_server::config::{
    Argon2Params, AuthConfig, DatabaseConfig, HttpConfig, LogConfig, LogFormat, RateLimitConfig,
    ServerConfig, VaultConfig,
};
use fleet_server::http::rate_limit::TrustedProxies;
use fleet_server::infra::crypto::passwords::Argon2Passwords;
use fleet_server::infra::crypto::vault::{DerivedKey, VaultKeys};
use fleet_server::infra::sqlite::{self, DatabaseOptions};
use fleet_server::ports::passwords::Passwords;
use fleet_startup::signals::{ShutdownSignals, Signal};
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::{SecretBox, SecretString};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::timeout;
use totp_rs::Totp;

/// The Owner's username.
pub(crate) const OWNER: &str = "owner";
/// Every account's password: strong, so the server hashes it.
pub(crate) const PASSWORD: &str = "correct horse battery staple";
/// Where every test server's clock starts: the middle of a TOTP step.
const T0: i64 = 1_800_000_015_000;
/// How long a test waits for the server to stop.
const STOP_WAIT: Duration = Duration::from_secs(10);

/// The real server, in-process, with its Owner (who has no 2FA yet).
pub(crate) struct Server {
    pub(crate) url: ServerUrl,
    /// The server's clock, for sessions, TOTP steps and rate limits alike.
    pub(crate) clock: ManualClock,
    stop: mpsc::Sender<Signal>,
    running: JoinSet<Exit>,
    _dir: TempDir,
}

impl Server {
    /// A fresh server on 127.0.0.1:0: a new database in a temporary
    /// folder, the production wiring (`app_with`) over a manual clock and
    /// seeded randomness, and the production rate limits.
    pub(crate) async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::new(DateTime::from_timestamp_millis(T0).unwrap());
        let random = SeededRandom::new(8);
        let config = config(&dir);
        let database = sqlite::open(&config.database, DatabaseOptions::default())
            .await
            .unwrap();
        let passwords: Arc<dyn Passwords> = Arc::new(
            Argon2Passwords::new(Argon2Params::default(), Arc::new(random.clone()))
                .await
                .unwrap(),
        );
        UserService::new(
            Arc::new(clock.clone()),
            Arc::new(random.clone()),
            Arc::new(database.clone()),
            Arc::clone(&passwords),
        )
        .create_owner(
            &Username::try_from(OWNER).unwrap(),
            &NewPassword::try_from(SecretString::from(PASSWORD)).unwrap(),
        )
        .await
        .unwrap();
        let keys = VaultKeys::from_keys(
            BTreeMap::from([(1, SecretBox::new(Box::new([0x5A; 32])))]),
            1,
        )
        .unwrap();
        let lockout_key = keys.derive_key(DerivedKey::LoginAttempts).unwrap();
        let ports = ServerPorts {
            clock: Arc::new(clock.clone()),
            limiter_clock: Arc::new(clock.clone()),
            random: Arc::new(random),
        };
        let app = app_with(&config, &database, keys, lockout_key, passwords, ports);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = ServerUrl::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let (stop, signals) = ChannelSignals::new();
        let mut running = JoinSet::new();
        running.spawn(async move {
            let timeouts = ShutdownTimeouts {
                drain: Duration::from_secs(1),
                close: Duration::from_secs(2),
            };
            serve(listener, app, &database, signals, timeouts).await
        });
        Self {
            url,
            clock,
            stop,
            running,
            _dir: dir,
        }
    }

    /// Stops the server as a signal would, and checks it stopped cleanly.
    pub(crate) async fn stop(mut self) {
        self.stop.send(Signal::Terminate).await.unwrap();
        let exit = timeout(STOP_WAIT, self.running.join_next())
            .await
            .expect("the server didn't stop within 10 s")
            .unwrap()
            .unwrap();
        assert_eq!(exit, Exit::Stopped);
    }

    /// A client of this server over `store`.
    pub(crate) fn client_over<S: SessionStore>(&self, store: S) -> Client<S> {
        Client::new(
            ClientConfig {
                server: self.url.clone(),
                trust: Trust::System,
            },
            store,
        )
        .unwrap()
    }

    /// A new client with its own empty store, which the test keeps too.
    pub(crate) fn client(&self) -> (Client<MemoryStore>, MemoryStore) {
        let store = MemoryStore::new();
        (self.client_over(store.clone()), store)
    }

    /// Moves the clock on by `by`.
    pub(crate) fn advance(&self, by: Duration) {
        self.clock.advance(by);
    }

    /// Moves the clock to the next TOTP step, so a new code can log in. It
    /// also refills two of the five logins a minute per address.
    pub(crate) fn next_step(&self) {
        self.advance(Duration::from_secs(30));
    }

    /// The authenticator app's code at the clock's time.
    pub(crate) fn code(&self, app: &Totp) -> SecretText<CODE_MAX_CHARS> {
        let now = u64::try_from(self.clock.now().timestamp()).unwrap();
        SecretText::new(app.generate(now).to_string())
    }

    /// `username`'s first login: the setup step, enroll, confirm, then the
    /// login with the next step's code, as the app does it. The client is
    /// logged in afterwards, and the clock is on the next step.
    pub(crate) async fn first_login<S: SessionStore>(
        &self,
        client: &Client<S>,
        username: &str,
    ) -> Totp {
        let step = client.login(&login(username, PASSWORD)).await.unwrap();
        assert!(matches!(step, LoginStep::SetupRequired { .. }), "{step:?}");
        let app = app_of(&client.mfa_enroll().await.unwrap());
        let codes = client.mfa_confirm(self.code(&app)).await.unwrap();
        assert_eq!(codes.recovery_codes.len(), 10);
        self.next_step();
        self.log_in(client, username, &app).await;
        app
    }

    /// Logs `username` in with the app's current code, then moves the clock
    /// to the next step.
    pub(crate) async fn log_in<S: SessionStore>(
        &self,
        client: &Client<S>,
        username: &str,
        app: &Totp,
    ) {
        let step = client.login(&login(username, PASSWORD)).await.unwrap();
        assert!(matches!(step, LoginStep::MfaRequired { .. }), "{step:?}");
        client.login_mfa(self.code(app)).await.unwrap();
        self.next_step();
    }

    /// The Owner, logged in with 2FA: the client, its store and the app.
    pub(crate) async fn owner(&self) -> (Client<MemoryStore>, MemoryStore, Totp) {
        let (client, store) = self.client();
        let app = self.first_login(&client, OWNER).await;
        (client, store, app)
    }
}

/// The password step's request.
pub(crate) fn login(username: &str, password: &str) -> LoginRequest {
    LoginRequest {
        username: SecretText::new(username.to_owned()),
        password: SecretText::new(password.to_owned()),
    }
}

/// Steps `client`'s session up with the shared password: the admins'
/// writes need it (ADR-0016's amendment in Phase 8 group E). It spends one
/// of the per-IP Login bucket's requests, so it's never part of `owner()`.
pub(crate) async fn step_up<S: SessionStore>(client: &Client<S>) {
    client
        .step_up(&StepUpRequest {
            password: SecretText::new(PASSWORD.to_owned()),
        })
        .await
        .unwrap();
}

/// The registration of `username` with the invite `code`.
pub(crate) fn register(code: &str, username: &str, password: &str) -> RegisterRequest {
    RegisterRequest {
        invite: SecretText::new(code.to_owned()),
        username: SecretText::new(username.to_owned()),
        password: SecretText::new(password.to_owned()),
    }
}

/// The authenticator app of an enrollment.
pub(crate) fn app_of(enrollment: &MfaEnrollResponse) -> Totp {
    Totp::from_url(enrollment.otpauth_uri.expose()).unwrap()
}

/// The stored refresh token's text.
pub(crate) async fn stored(store: &MemoryStore) -> Option<String> {
    use secrecy::ExposeSecret as _;
    store
        .load()
        .await
        .unwrap()
        .map(|token| token.expose_secret().to_owned())
}

/// Whether `error` is the server's refusal with `status` and `code`.
pub(crate) fn is_api(error: &ClientError, status: u16, code: ErrorCode) -> bool {
    error
        .api()
        .is_some_and(|failure| failure.status == status && failure.is(code))
}

/// The server's config: production defaults, its database in `dir`. The
/// vault's key comes from memory, so its file is never read.
fn config(dir: &TempDir) -> ServerConfig {
    ServerConfig {
        dev_mode: false,
        http: HttpConfig {
            bind: HttpConfig::DEFAULT_BIND,
            request_timeout: Duration::from_secs(15),
            max_body_bytes: 65_536,
            trusted_proxies: TrustedProxies::default(),
        },
        database: DatabaseConfig {
            path: dir.path().join("afkfleet.db"),
        },
        log: LogConfig {
            format: LogFormat::Json,
            filter: "info".parse().unwrap(),
        },
        auth: AuthConfig::default(),
        rate_limits: RateLimitConfig::default(),
        vault: VaultConfig {
            key_files: BTreeMap::from([(1, dir.path().join("vault-key-1"))]),
            active_key_id: 1,
        },
    }
}

/// Delivers the signals the test sends; waits forever once the sender is
/// gone, like a real source whose streams ended.
#[derive(Debug)]
struct ChannelSignals(mpsc::Receiver<Signal>);

impl ChannelSignals {
    fn new() -> (mpsc::Sender<Signal>, Self) {
        let (sender, receiver) = mpsc::channel(4);
        (sender, Self(receiver))
    }
}

impl ShutdownSignals for ChannelSignals {
    async fn recv(&mut self) -> Signal {
        match self.0.recv().await {
            Some(signal) => signal,
            None => core::future::pending().await,
        }
    }
}
