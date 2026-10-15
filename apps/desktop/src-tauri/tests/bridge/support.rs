//! What the bridge tests share: the real server, in-process with a manual
//! clock and its Owner; an [`AppState`] over a keychain of one memory store
//! per origin; and the authenticator app.

use core::time::Duration;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use afkfleet_desktop::keychain::StoreFactory;
use afkfleet_desktop::profile::PROFILE_FILE;
use afkfleet_desktop::state::{AppState, LinkOpener, OpenError};
use chrono::DateTime;
use fleet_api_types::app::{
    CommandError, CommandErrorCode, LoginStepKind, ServerProfileRequest, ServerProfileResponse,
};
use fleet_api_types::auth::{LoginRequest, RegisterRequest, StepUpRequest};
use fleet_api_types::invites::CreateInviteRequest;
use fleet_api_types::mfa::{CODE_MAX_CHARS, MfaEnrollResponse};
use fleet_api_types::users::UserRole;
use fleet_api_types::{ErrorCode, SecretText};
use fleet_client::{
    Client, ClientConfig, MemoryStore, ServerUrl, SessionStore, StoreError, StoreOp, Trust,
    VerifiedLink,
};
use fleet_core::account::AccountStatus;
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::auth::NewPassword;
use fleet_core::id::{AccountId, UserId};
use fleet_core::system::Clock as _;
use fleet_core::value::Username;
use fleet_server::app::users::UserService;
use fleet_server::cli::serve::{Exit, ServerPorts, ShutdownTimeouts, app_with, serve};
use fleet_server::config::{
    AccountsConfig, Argon2Params, AuthConfig, DatabaseConfig, HttpConfig, LogConfig, LogFormat,
    RateLimitConfig, ServerConfig, VaultConfig,
};
use fleet_server::http::rate_limit::TrustedProxies;
use fleet_server::infra::crypto::passwords::Argon2Passwords;
use fleet_server::infra::crypto::vault::{DerivedKey, VaultKeys};
use fleet_server::infra::msauth::ReqwestMicrosoftAuth;
use fleet_server::infra::sqlite::{self, Database, DatabaseOptions};
use fleet_server::infra::system::TokioPace;
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::passwords::Passwords;
use fleet_server::ports::store::Store as _;
use fleet_startup::signals::{ShutdownSignals, Signal};
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::{ExposeSecret as _, SecretBox, SecretString};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinSet;
use tokio::time::timeout;
use totp_rs::Totp;

/// The Owner's username.
pub(crate) const OWNER: &str = "owner";
/// Every account's password: strong, so the server hashes it.
pub(crate) const PASSWORD: &str = "correct horse battery staple";
/// Where every test server's clock starts: the middle of a TOTP step.
const T0: i64 = 1_800_000_015_000;
/// How long a test waits for something that must happen.
pub(crate) const WAIT: Duration = Duration::from_secs(10);
/// The test server's own bounds. These tests check the app against a real server,
/// not the server's speed (its own tests do that, with the production values). CI's
/// coverage build, with every crate instrumented, ran them up to 4 times slower on a
/// busy runner, where the production 5 s to get a database connection and a 3 s
/// shutdown weren't enough (PR #37), so the server gets generous ones.
const DATABASE_WAIT: Duration = Duration::from_secs(30);
const SHUTDOWN: ShutdownTimeouts = ShutdownTimeouts {
    drain: Duration::from_secs(2),
    close: Duration::from_secs(15),
};
/// How long `Server::stop` waits: more than the shutdown's whole budget.
const STOP_WAIT: Duration = Duration::from_secs(30);

/// The real server, in-process, with its Owner (who has no 2FA yet).
pub(crate) struct Server {
    pub(crate) url: ServerUrl,
    clock: ManualClock,
    /// The server's database, for rows no route can make without
    /// Microsoft: a pending link ([`Server::pending_account`]).
    db: Database,
    stop: mpsc::Sender<Signal>,
    running: JoinSet<Exit>,
    _dir: TempDir,
}

impl Server {
    /// A fresh server on 127.0.0.1:0, as fleet-client's tests run it: a new
    /// database in a temporary folder, the production wiring over a manual
    /// clock and seeded randomness, and the production rate limits.
    pub(crate) async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::new(DateTime::from_timestamp_millis(T0).unwrap());
        let random = SeededRandom::new(8);
        let config = config(&dir);
        let database = sqlite::open(
            &config.database,
            DatabaseOptions::default().with_acquire_timeout(DATABASE_WAIT),
        )
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
            // Never called: these tests link no Microsoft account.
            microsoft: Arc::new(ReqwestMicrosoftAuth::new(Arc::new(clock.clone())).unwrap()),
            pace: Arc::new(TokioPace),
        };
        let app = app_with(&config, &database, keys, lockout_key, passwords, ports);
        let db = database.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = ServerUrl::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let (stop, signals) = ChannelSignals::new();
        let mut running = JoinSet::new();
        running.spawn(async move { serve(listener, app, &database, signals, SHUTDOWN).await });
        Self {
            url,
            clock,
            db,
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
            .expect("the server didn't stop in time")
            .unwrap()
            .unwrap();
        assert_eq!(exit, Exit::Stopped);
    }

    /// The server's address as a profile request.
    pub(crate) fn profile(&self) -> ServerProfileRequest {
        ServerProfileRequest {
            url: self.url.as_str().to_owned(),
            ca_pem: None,
        }
    }

    /// A plain fleet-client client of this server over `store`, as a second
    /// device of the same user would be.
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

    /// Moves the clock to the next TOTP step, so a new code can log in. It
    /// also refills two of the five logins a minute per address.
    pub(crate) fn next_step(&self) {
        self.clock.advance(Duration::from_secs(30));
    }

    /// The authenticator app's code at the clock's time.
    pub(crate) fn code(&self, app: &Totp) -> SecretText<CODE_MAX_CHARS> {
        let now = u64::try_from(self.clock.now().timestamp()).unwrap();
        SecretText::new(app.generate(now).to_string())
    }

    /// `username`'s first login through the bridge: the setup step, enroll,
    /// confirm, then the login with the next step's code. Logged in
    /// afterwards, the clock on the next step.
    pub(crate) async fn first_login(&self, bridge: &Bridge, username: &str) -> Totp {
        let step = bridge.state.login(login(username, PASSWORD)).await.unwrap();
        assert_eq!(step.step, LoginStepKind::SetupRequired, "{step:?}");
        let app = app_of(&bridge.state.mfa_enroll().await.unwrap());
        let codes = bridge.state.mfa_confirm(self.code(&app)).await.unwrap();
        assert_eq!(codes.recovery_codes.len(), 10);
        self.next_step();
        self.log_in(bridge, username, &app).await;
        app
    }

    /// Logs `username` in with the app's current code, then moves the clock
    /// to the next step.
    pub(crate) async fn log_in(&self, bridge: &Bridge, username: &str, app: &Totp) {
        let step = bridge.state.login(login(username, PASSWORD)).await.unwrap();
        assert_eq!(step.step, LoginStepKind::MfaRequired, "{step:?}");
        bridge.state.login_mfa(self.code(app)).await.unwrap();
        self.next_step();
    }

    /// Moves the clock on by `by`.
    pub(crate) fn advance(&self, by: Duration) {
        self.clock.advance(by);
    }

    /// A Member named `username`, invited by `owner` (stepped up within 5
    /// minutes) and logged in through a bridge of their own: the bridge and
    /// the user's ID.
    pub(crate) async fn member(&self, owner: &Bridge, username: &str) -> (Bridge, UserId) {
        let invite = owner
            .state
            .create_invite(CreateInviteRequest {
                role: UserRole::Member,
            })
            .await
            .unwrap();
        let bridge = Bridge::on(self).await;
        let registered = bridge
            .state
            .register(register(invite.code.expose(), username))
            .await
            .unwrap();
        self.first_login(&bridge, username).await;
        (bridge, registered.id)
    }

    /// A Microsoft account of `owner`'s, numbered `n`, waiting for
    /// approval, as a Member's link leaves it. It's written straight to the
    /// database, since a link needs Microsoft; its envelope is opaque bytes,
    /// which approving or refusing never opens.
    pub(crate) async fn pending_account(&self, owner: UserId, n: u8) -> AccountId {
        let now = self.clock.now();
        let id = AccountId::new_v7(now, [n; 10]).unwrap();
        let account = McAccount {
            id,
            owner,
            name: format!("AfkBot{n}").parse().unwrap(),
            created_at: now,
            credential: AccountCredential::Microsoft {
                uuid: format!("00000000-0000-4000-8000-0000000000{n:02x}")
                    .parse()
                    .unwrap(),
                refresh_envelope: vec![n; 61],
                status: AccountStatus::Pending,
            },
        };
        let mut tx = self.db.write().await.unwrap();
        tx.accounts().insert(&account).await.unwrap();
        let entry = NewAuditEntry::new(
            now,
            AuditAction::try_from("test.account_setup").unwrap(),
            AuditOutcome::Success,
        );
        tx.commit(entry).await.unwrap();
        id
    }

    /// A bridge set up for this server, with the Owner logged in.
    pub(crate) async fn owner(&self) -> (Bridge, Totp) {
        let bridge = Bridge::on(self).await;
        let app = self.first_login(&bridge, OWNER).await;
        (bridge, app)
    }
}

/// An [`AppState`] as the app holds it, over a profile file in a fresh
/// folder, a test keychain and an opener that records what it opens.
pub(crate) struct Bridge {
    pub(crate) state: AppState<TestFactory>,
    pub(crate) keychain: TestFactory,
    pub(crate) opener: Arc<RecordingOpener>,
    pub(crate) dir: TempDir,
}

impl Bridge {
    /// A fresh state: no profile file yet, an empty keychain.
    pub(crate) fn new() -> Self {
        Self::over(tempfile::tempdir().unwrap(), TestFactory::default())
    }

    /// The state the app would load from `dir`'s profile file and
    /// `keychain`, as after a restart.
    pub(crate) fn over(dir: TempDir, keychain: TestFactory) -> Self {
        let opener = Arc::new(RecordingOpener::default());
        let state = AppState::load(
            dir.path().join(PROFILE_FILE),
            keychain.clone(),
            Arc::clone(&opener) as Arc<dyn LinkOpener>,
        );
        Self {
            state,
            keychain,
            opener,
            dir,
        }
    }

    /// A fresh state set up for `server`.
    pub(crate) async fn on(server: &Server) -> Self {
        let bridge = Self::new();
        let answer = bridge
            .state
            .set_server_profile(server.profile())
            .await
            .unwrap();
        assert!(
            matches!(answer, ServerProfileResponse::Switched { .. }),
            "{answer:?}"
        );
        bridge
    }
}

/// Records each link it's asked to open, and opens nothing.
#[derive(Default)]
pub(crate) struct RecordingOpener {
    opened: Mutex<Vec<String>>,
}

impl RecordingOpener {
    /// The links it was asked to open, in order.
    pub(crate) fn opened(&self) -> Vec<String> {
        self.opened.lock().unwrap().clone()
    }
}

impl LinkOpener for RecordingOpener {
    fn open(&self, link: &VerifiedLink) -> Result<(), OpenError> {
        self.opened.lock().unwrap().push(link.as_str().to_owned());
        Ok(())
    }
}

/// A keychain of one memory store per origin, which can refuse an origin
/// or fail a clear.
#[derive(Clone, Default)]
pub(crate) struct TestFactory {
    stores: Arc<Mutex<HashMap<String, TestStore>>>,
    refuse: Arc<Mutex<Option<String>>>,
}

impl TestFactory {
    /// The store of `origin`, as the bridge would get it.
    pub(crate) fn store(&self, origin: &ServerUrl) -> TestStore {
        self.stores
            .lock()
            .unwrap()
            .entry(origin.as_str().to_owned())
            .or_default()
            .clone()
    }

    /// The refresh token stored for `origin`.
    pub(crate) async fn stored(&self, origin: &ServerUrl) -> Option<String> {
        self.store(origin)
            .inner
            .load()
            .await
            .unwrap()
            .map(|token| token.expose_secret().to_owned())
    }

    /// Stores `token` for `origin`, as an earlier run would have.
    pub(crate) async fn plant(&self, origin: &ServerUrl, token: &str) {
        self.store(origin)
            .inner
            .save(&SecretString::from(token))
            .await
            .unwrap();
    }

    /// From now on, `origin`'s store can't be built.
    pub(crate) fn refuse(&self, origin: &ServerUrl) {
        *self.refuse.lock().unwrap() = Some(origin.as_str().to_owned());
    }
}

impl StoreFactory for TestFactory {
    type Store = TestStore;

    fn for_origin(&self, origin: &ServerUrl) -> Result<TestStore, StoreError> {
        if self.refuse.lock().unwrap().as_deref() == Some(origin.as_str()) {
            return Err(StoreError::new(StoreOp::Load, "refused for the test"));
        }
        Ok(self.store(origin))
    }
}

/// One origin's store: a [`MemoryStore`] whose clears can fail and whose
/// loads can be held until released. Clones share all.
#[derive(Clone, Default)]
pub(crate) struct TestStore {
    inner: MemoryStore,
    fail_clear: Arc<AtomicBool>,
    hold: Arc<Mutex<Option<Arc<Hold>>>>,
}

/// A held load: `entered` when it starts waiting, then `release`d.
#[derive(Default)]
pub(crate) struct Hold {
    pub(crate) entered: Notify,
    pub(crate) release: Notify,
}

impl TestStore {
    /// From now on, every clear fails.
    pub(crate) fn fail_clears(&self) {
        self.fail_clear.store(true, Ordering::SeqCst);
    }

    /// Holds the next load until the returned hold is released.
    pub(crate) fn hold_next_load(&self) -> Arc<Hold> {
        let hold = Arc::new(Hold::default());
        *self.hold.lock().unwrap() = Some(Arc::clone(&hold));
        hold
    }
}

impl SessionStore for TestStore {
    async fn load(&self) -> Result<Option<SecretString>, StoreError> {
        let hold = self.hold.lock().unwrap().take();
        if let Some(hold) = hold {
            hold.entered.notify_one();
            hold.release.notified().await;
        }
        self.inner.load().await
    }

    async fn save(&self, refresh_token: &SecretString) -> Result<(), StoreError> {
        self.inner.save(refresh_token).await
    }

    async fn clear(&self) -> Result<(), StoreError> {
        if self.fail_clear.load(Ordering::SeqCst) {
            return Err(StoreError::new(StoreOp::Clear, "the keychain is locked"));
        }
        self.inner.clear().await
    }
}

/// The password step's request.
pub(crate) fn login(username: &str, password: &str) -> LoginRequest {
    LoginRequest {
        username: SecretText::new(username.to_owned()),
        password: SecretText::new(password.to_owned()),
    }
}

/// The step-up with the shared password: the admins' writes need it
/// (ADR-0016's amendment in Phase 8 group E), and so do the password change
/// and the 2FA replacement.
pub(crate) fn step_up_request() -> StepUpRequest {
    StepUpRequest {
        password: SecretText::new(PASSWORD.to_owned()),
    }
}

/// The registration of `username` with the invite `code`.
pub(crate) fn register(code: &str, username: &str) -> RegisterRequest {
    RegisterRequest {
        invite: SecretText::new(code.to_owned()),
        username: SecretText::new(username.to_owned()),
        password: SecretText::new(PASSWORD.to_owned()),
    }
}

/// The authenticator app of an enrollment.
pub(crate) fn app_of(enrollment: &MfaEnrollResponse) -> Totp {
    Totp::from_url(enrollment.otpauth_uri.expose()).unwrap()
}

/// Whether `error` is the server's refusal with `status` and `code`.
pub(crate) fn is_api(error: &CommandError, status: u16, code: ErrorCode) -> bool {
    error.code == CommandErrorCode::Api
        && error
            .api
            .as_ref()
            .is_some_and(|api| api.status == status && api.code.known() == Some(code))
}

/// Whether `error` is `invalid_input` for exactly the field `path`.
pub(crate) fn is_invalid(error: &CommandError, path: &str) -> bool {
    error.code == CommandErrorCode::InvalidInput
        && error.fields.as_ref().is_some_and(|fields| {
            fields.len() == 1 && fields.first().is_some_and(|f| f.path.as_str() == path)
        })
}

/// Fails if `answer`'s JSON holds anything like a token.
pub(crate) fn assert_no_token(answer: &impl serde::Serialize) {
    let json = serde_json::to_string(answer).unwrap();
    for prefix in ["afk_at_", "afk_rt_", "afk_mt_", "afk_st_"] {
        assert!(!json.contains(prefix), "{prefix} in {json}");
    }
}

/// The server's config: production defaults, its database in `dir`. The
/// vault's key comes from memory, so its file is never read. One exception:
/// offline accounts are on, since they're the only accounts these tests can
/// create without Microsoft (ADR-0018, group F).
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
        accounts: AccountsConfig {
            allow_offline: true,
            ..AccountsConfig::default()
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
