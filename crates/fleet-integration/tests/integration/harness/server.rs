//! The real server, in-process (ADR-0019, S72, S73): `app_with` over the
//! case's ports, then serve's own `open`, `grpc_port` and `serve`, so its
//! wiring is serve's: the HTTP API, the gRPC port with its sweep, the
//! reconciler, the grants, the ingest and the shutdown order.
//!
//! - **Its files** live in the case's folder: the database, and the PKI
//!   `afkfleet-server pki init` writes and serve's loader reads at every
//!   start. The vault key comes from memory.
//! - **A loopback case's server** ([`Server::start`]) runs on a
//!   [`ManualClock`] at the real time, for TOTP steps, and seeded
//!   randomness, with offline accounts on; TLS checks certificates on real
//!   time, and the certificates are valid from an hour before the clock's
//!   time. The rate limiters measure real intervals, as in production, so a
//!   case's requests and reconnects refill their buckets.
//! - **The managed check's server** ([`Server::start_real`]) runs on the real
//!   clock, the OS's randomness and the Microsoft adapter it's given, with
//!   offline accounts off, the Owner's password and the vault's key the
//!   check drew for its run.
//! - **A restart** ([`Server::restart`]) stops serve with a signal, as an
//!   operator does, and starts it again on the same database, PKI and
//!   addresses, with the same clock and randomness: a loopback case's
//!   server only, since the managed check's key is used once.
//! - **A crash case's server** runs in a child process between two serves of
//!   the case's own ([`Server::resume`], which the child calls on the files
//!   the case's server left; [`Server::moved_to`], which points the agents'
//!   proxies at it): the real clock and the OS's randomness, the same fixed
//!   vault key (P13.5, ADR-0033).

use core::num::NonZeroU32;
use core::time::Duration;
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::TimeDelta;
use fleet_core::audit::AuditAction;
use fleet_core::auth::NewPassword;
use fleet_core::backup::BackupPolicy;
use fleet_core::enrollment::EnrollmentBundle;
use fleet_core::id::{AgentId, BotId};
use fleet_core::pki::CertFingerprint;
use fleet_core::retention::ChatBound;
use fleet_core::system::{Clock, SecureRandom};
use fleet_core::value::Username;
use fleet_server::app::backup::BackupService;
use fleet_server::app::enrollment::EnrollmentTokenService;
use fleet_server::app::retention::{RetentionService, RetentionTimings, Run};
use fleet_server::app::users::UserService;
use fleet_server::cli::pki;
use fleet_server::cli::serve::{
    ControlPlane, Exit, ServerPorts, ShutdownTimeouts, app_with, grpc_port, open, serve,
};
use fleet_server::config::{
    AccountsConfig, AuditConfig, AuthConfig, BackupConfig, ChatConfig, DatabaseConfig, GrpcConfig,
    Heartbeats, HttpConfig, LogConfig, LogFormat, PkiPaths, Rate, RateLimitConfig, ServerConfig,
    VaultConfig,
};
use fleet_server::grpc::reconciler::ReconcileTimings;
use fleet_server::grpc::registry::AgentRegistry;
use fleet_server::http::rate_limit::TrustedProxies;
use fleet_server::infra::crypto::passwords::Argon2Passwords;
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::infra::crypto::vault::{DerivedKey, VaultKeys};
use fleet_server::infra::msauth::ReqwestMicrosoftAuth;
use fleet_server::infra::pki::load;
use fleet_server::infra::sqlite::{self, Database, DatabaseOptions};
use fleet_server::infra::system::{MonotonicClock, OsRandom, SystemClock, TokioPace};
use fleet_server::metrics::{Metrics, MetricsRender};
use fleet_server::ports::agents::AgentCert;
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditRecord};
use fleet_server::ports::bots::Bot;
use fleet_server::ports::msauth::MicrosoftAuthProvider;
use fleet_server::ports::page::{Page, PageLimit};
use fleet_server::ports::passwords::Passwords;
use fleet_server::ports::store::Store as _;
use fleet_startup::signals::{ShutdownSignals, Signal};
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::{ExposeSecret as _, SecretBox, SecretString};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::logs::PATIENCE;

/// The Owner's name.
pub(crate) const OWNER: &str = "owner";
/// The backups' folder in a case's folder, when its backups are on.
pub(crate) const BACKUPS: &str = "backups";
/// A loopback case's Owner's password: strong, so the server takes it.
pub(crate) const PASSWORD: &str = "correct horse battery staple";

/// How a loopback case's server runs.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ServerOptions {
    /// `[grpc] heartbeat_interval_secs` and `heartbeat_timeout_secs`: the
    /// production 10 s and 30 s, but where a case measures them.
    pub(crate) heartbeats: Heartbeats,
    /// The reconciler's timing: the production one, but where a case must
    /// tell a hinted pass from a periodic one
    /// ([`QUIET_RECONCILE`](super::logs::QUIET_RECONCILE)).
    pub(crate) reconcile: ReconcileTimings,
    /// How far behind the real time the server's clock starts: nothing,
    /// but for the renewal's case, which enrolls an agent in the past and
    /// then moves the clock to the real time TLS reads (P13.6).
    pub(crate) clock_behind: Duration,
    /// `[rate_limits]`: the production ones, but where a case's own setup
    /// would spend them (P13.4).
    pub(crate) limits: Limits,
    /// Whether the metrics port listens, on a loopback port of its own: off,
    /// as without `[metrics]`, but where a case scrapes it (P13.4).
    pub(crate) metrics: bool,
    /// Whether the case starts the retention's runs itself
    /// ([`Server::retention_run`]): then the task's own runs are a day
    /// apart, and each bot keeps its newest 100 lines, the least `[chat]
    /// max_lines_per_bot` allows, so a run after a few hundred lines a bot
    /// has work to do (P13.4).
    pub(crate) retention_by_hand: bool,
    /// Whether backups are on, into [`BACKUPS`] in the case's folder, at the
    /// default interval and count: off, as without `[database] backup_dir`,
    /// but where a case takes one (P13.5).
    pub(crate) backups: bool,
}

/// Which `[rate_limits]` a case's server runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Limits {
    /// The production ones.
    #[default]
    Production,
    /// The production ones, but for those the load test's own setup would
    /// spend: hundreds of requests from one address and one user, twenty
    /// registrations and logins, 400 grants by name. The limits on what the
    /// test loads (the bots' chat, their actions, the sockets a user) stay.
    LoadSetup,
}

impl Limits {
    fn config(self) -> RateLimitConfig {
        match self {
            Self::Production => RateLimitConfig::default(),
            Self::LoadSetup => {
                let many = NonZeroU32::new(10_000).unwrap_or(NonZeroU32::MIN);
                RateLimitConfig {
                    per_ip: Rate::per_second(many, many),
                    per_user: Rate::per_second(many, many),
                    login: Rate::per_minute(many),
                    register: Rate::per_hour(many),
                    grants_by_name: Rate::per_hour(many),
                    ..RateLimitConfig::default()
                }
            }
        }
    }
}

/// What the managed check's server runs on, beside the real clock and the OS's
/// randomness.
pub(crate) struct RealSetup {
    /// Microsoft's account flows.
    pub(crate) microsoft: Arc<dyn MicrosoftAuthProvider>,
    /// The Owner's password.
    pub(crate) password: SecretString,
    /// The vault's one key, drawn for the run.
    pub(crate) key: SecretBox<[u8; 32]>,
}

/// The gRPC port's current address, which every start moves: a fresh port
/// each time, so a port the stop freed can't be taken by another process
/// and dialled meanwhile. The agents' proxies read it at every connection.
/// Clones share it.
#[derive(Debug, Clone)]
pub(crate) struct GrpcAddress(Arc<std::sync::Mutex<SocketAddr>>);

impl GrpcAddress {
    /// Where the gRPC port listens now.
    pub(crate) fn get(&self) -> SocketAddr {
        *self.0.lock().unwrap()
    }

    fn set(&self, address: SocketAddr) {
        *self.0.lock().unwrap() = address;
    }
}

/// The real clock, a fixed time ahead: a crash case's child process starts
/// where the case's own clock stands, which the Owner's logins moved ahead
/// of the real time by their TOTP steps, and runs on from there. A server
/// whose clock were behind the one before it would find every bot's stored
/// state newer than what its agents report now, and store none of it
/// (ADR-0019, S40: a state is never recorded backwards).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Ahead(pub(crate) Duration);

impl Clock for Ahead {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        whole_millis(fleet_core::time::add(SystemClock.now(), self.0))
    }
}

/// Why a server didn't start: a fixed text, never a path, an address or a
/// command's output.
pub(crate) type SetupError = &'static str;

/// The server; see the [module docs](self).
#[derive(Debug)]
pub(crate) struct Server {
    config: ServerConfig,
    clock: Arc<dyn Clock>,
    /// A loopback case's clock, which the Owner's TOTP steps move.
    manual: Option<ManualClock>,
    random: Arc<dyn SecureRandom>,
    microsoft: Arc<dyn MicrosoftAuthProvider>,
    reconcile: ReconcileTimings,
    keys: Keys,
    /// Whether each serve gets a metrics port.
    metrics: bool,
    /// Whether the case starts the retention's runs.
    retention_by_hand: bool,
    /// The API's listener, bound once for the server's whole life: each
    /// serve gets a clone of it, so a restart never frees its port and the
    /// Owner's client keeps its URL.
    http: std::net::TcpListener,
    http_address: SocketAddr,
    grpc: GrpcAddress,
    ca_fingerprint: CertFingerprint,
    running: Option<Running>,
}

/// Where the vault's key comes from.
enum Keys {
    /// The same bytes at every start: a loopback case's.
    Fixed,
    /// The managed check's, used once.
    Drawn(Option<SecretBox<[u8; 32]>>),
}

impl core::fmt::Debug for Keys {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Fixed => "Fixed",
            Self::Drawn(_) => "Drawn(..)",
        })
    }
}

impl Keys {
    fn take(&mut self) -> Result<VaultKeys, SetupError> {
        let key = match self {
            Self::Fixed => SecretBox::new(Box::new([0x5A; 32])),
            Self::Drawn(key) => key.take().ok_or("the vault's key was used already")?,
        };
        VaultKeys::from_keys(BTreeMap::from([(1, key)]), 1)
            .map_err(|_| "the vault's key couldn't be set up")
    }
}

/// A serve that runs.
#[derive(Debug)]
struct Running {
    db: Database,
    registry: AgentRegistry,
    metrics: MetricsRender,
    /// Where this serve's metrics port listens, if it has one.
    metrics_port: Option<SocketAddr>,
    /// The retention the task runs, for a case that starts a run itself.
    retention: RetentionService,
    stop: mpsc::Sender<Signal>,
    serving: JoinSet<Exit>,
}

/// Everything a first start needs.
struct Begin {
    /// How it runs; the clock's start is the caller's.
    options: ServerOptions,
    allow_offline: bool,
    clock: Arc<dyn Clock>,
    manual: Option<ManualClock>,
    random: Arc<dyn SecureRandom>,
    microsoft: Arc<dyn MicrosoftAuthProvider>,
    keys: Keys,
    password: SecretString,
}

impl Server {
    /// A loopback case's server with its files in `dir`; see the [module
    /// docs](self).
    pub(crate) async fn start(dir: &Path, options: ServerOptions) -> Self {
        let manual = ManualClock::new(fleet_core::time::sub(
            whole_millis(SystemClock.now()),
            options.clock_behind,
        ));
        Self::begin(
            dir,
            Begin {
                options,
                allow_offline: true,
                clock: Arc::new(manual.clone()),
                // Never called: the cases' accounts are offline ones.
                microsoft: Arc::new(ReqwestMicrosoftAuth::new(Arc::new(manual.clone())).unwrap()),
                manual: Some(manual),
                random: Arc::new(SeededRandom::new(11)),
                keys: Keys::Fixed,
                password: SecretString::from(PASSWORD),
            },
        )
        .await
        .unwrap_or_else(|why| panic!("the loopback server didn't start: {why}"))
    }

    /// The managed check's server with its files in `dir`; see the [module
    /// docs](self).
    ///
    /// # Errors
    /// A fixed text saying which step failed.
    pub(crate) async fn start_real(dir: &Path, setup: RealSetup) -> Result<Self, SetupError> {
        Self::begin(
            dir,
            Begin {
                options: ServerOptions::default(),
                allow_offline: false,
                clock: Arc::new(SystemClock),
                manual: None,
                random: Arc::new(OsRandom),
                microsoft: setup.microsoft,
                keys: Keys::Drawn(Some(setup.key)),
                password: setup.password,
            },
        )
        .await
    }

    /// The PKI from `pki init`, the database with its Owner, then serve.
    async fn begin(dir: &Path, begin: Begin) -> Result<Self, SetupError> {
        const BIND: SetupError = "the server's ports couldn't be bound";
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let http = std::net::TcpListener::bind("127.0.0.1:0").map_err(|_| BIND)?;
        let http_address = http.local_addr().map_err(|_| BIND)?;
        let grpc = TcpListener::bind("127.0.0.1:0").await.map_err(|_| BIND)?;
        let grpc_address = grpc.local_addr().map_err(|_| BIND)?;
        let config = config(
            dir,
            http_address,
            grpc_address,
            begin.options,
            begin.allow_offline,
        );
        std::fs::create_dir_all(dir.join("pki"))
            .map_err(|_| "the PKI's folder couldn't be made")?;
        if begin.options.backups {
            std::fs::create_dir_all(dir.join(BACKUPS))
                .map_err(|_| "the backups' folder couldn't be made")?;
        }
        // Its output names the files: never shown.
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let created = pki::init(
            &config,
            &["localhost".to_owned()],
            &[IpAddr::from([127, 0, 0, 1])],
            &*begin.clock,
            &*begin.random,
            &mut out,
            &mut err,
        );
        if created != pki::Exit::Created {
            return Err("the PKI couldn't be created");
        }
        let database = sqlite::open(&config.database, DatabaseOptions::default())
            .await
            .map_err(|_| "the database couldn't be opened")?;
        let username = Username::try_from(OWNER).map_err(|_| "the Owner's name was refused")?;
        let password = NewPassword::try_from(SecretString::from(begin.password.expose_secret()))
            .map_err(|_| "the Owner's password was refused")?;
        let created = UserService::new(
            Arc::clone(&begin.clock),
            Arc::clone(&begin.random),
            Arc::new(database.clone()),
            passwords(&config, &begin.random).await?,
        )
        .create_owner(&username, &password)
        .await;
        database.close().await;
        created.map_err(|_| "the Owner couldn't be created")?;
        let ca_fingerprint = load(&grpc_config(&config).pki, begin.clock.now())
            .map_err(|_| "the PKI couldn't be read")?
            .ca
            .fingerprint();
        let mut server = Self {
            http,
            http_address,
            grpc: GrpcAddress(Arc::new(std::sync::Mutex::new(grpc_address))),
            config,
            clock: begin.clock,
            manual: begin.manual,
            random: begin.random,
            microsoft: begin.microsoft,
            reconcile: begin.options.reconcile,
            keys: begin.keys,
            metrics: begin.options.metrics,
            retention_by_hand: begin.options.retention_by_hand,
            ca_fingerprint,
            running: None,
        };
        server.boot(grpc).await?;
        Ok(server)
    }

    /// A crash case's server in its child process (P13.5, ADR-0033): serve
    /// again on the database and the PKI the case's own server left in
    /// `dir`, as `options` say, on ports of its own, a clock `ahead` of the
    /// real one ([`Ahead`]) and the OS's randomness. Nothing is created: no
    /// PKI, no Owner.
    pub(crate) async fn resume(dir: &Path, options: ServerOptions, ahead: Duration) -> Self {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let http_address = http.local_addr().unwrap();
        let grpc = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let grpc_address = grpc.local_addr().unwrap();
        let config = config(dir, http_address, grpc_address, options, true);
        let clock: Arc<dyn Clock> = Arc::new(Ahead(ahead));
        let ca_fingerprint = load(&grpc_config(&config).pki, clock.now())
            .unwrap()
            .ca
            .fingerprint();
        let mut server = Self {
            http,
            http_address,
            grpc: GrpcAddress(Arc::new(std::sync::Mutex::new(grpc_address))),
            config,
            // Never called: the cases' accounts are offline ones.
            microsoft: Arc::new(ReqwestMicrosoftAuth::new(Arc::clone(&clock)).unwrap()),
            clock,
            manual: None,
            random: Arc::new(OsRandom),
            reconcile: options.reconcile,
            keys: Keys::Fixed,
            metrics: options.metrics,
            retention_by_hand: options.retention_by_hand,
            ca_fingerprint,
            running: None,
        };
        server
            .boot(grpc)
            .await
            .unwrap_or_else(|why| panic!("the child's server didn't start: {why}"));
        server
    }

    /// The gRPC port is at `address` now, in a child process: the agents'
    /// proxies dial it there until the next [`Server::start_again`].
    pub(crate) fn moved_to(&self, address: SocketAddr) {
        assert!(self.running.is_none(), "the server is stopped");
        self.grpc.set(address);
    }

    /// The running server's database.
    pub(crate) fn database(&self) -> &Database {
        &self.running().db
    }

    /// The whole config, as serve got it.
    pub(crate) fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// The backups as serve's schedule and the `backup` command take them,
    /// on this server's clock.
    pub(crate) fn backups(&self) -> BackupService {
        BackupService::from_config(
            &self.config,
            Arc::clone(&self.clock),
            DatabaseOptions::default(),
        )
        .expect("the case turned backups on")
    }

    /// Serve over a clone of the API's listener and `grpc`, on the database
    /// and PKI in the folder.
    async fn boot(&mut self, grpc: TcpListener) -> Result<(), SetupError> {
        let http = self
            .http
            .try_clone()
            .and_then(|clone| {
                clone.set_nonblocking(true)?;
                TcpListener::from_std(clone)
            })
            .map_err(|_| "the API's listener couldn't be cloned")?;
        let pki = load(&grpc_config(&self.config).pki, self.clock.now())
            .map_err(|_| "the PKI couldn't be read")?;
        let database = sqlite::open(&self.config.database, DatabaseOptions::default())
            .await
            .map_err(|_| "the database couldn't be opened")?;
        let keys = self.keys.take()?;
        let lockout_key = keys
            .derive_key(DerivedKey::LoginAttempts)
            .map_err(|_| "the vault's key couldn't be derived")?;
        let metrics = Metrics::prometheus();
        let render = metrics.render().clone();
        let ports = ServerPorts {
            clock: Arc::clone(&self.clock),
            limiter_clock: Arc::new(MonotonicClock::start()),
            random: Arc::clone(&self.random),
            microsoft: Arc::clone(&self.microsoft),
            pace: Arc::new(TokioPace),
            reconcile: self.reconcile,
            metrics,
        };
        let (opened, ca) = open(
            ControlPlane {
                listener: grpc,
                pki,
            },
            self.clock.now(),
        );
        let mut app = app_with(
            &self.config,
            &database,
            keys,
            lockout_key,
            passwords(&self.config, &self.random).await?,
            ports,
            Some(ca),
        );
        let registry = app
            .grpc
            .as_ref()
            .ok_or("the control plane is off")?
            .sweep
            .registry
            .clone();
        let port = grpc_port(&self.config, Some(opened), app.grpc.take())
            .map_err(|_| "the gRPC port couldn't be set up")?;
        if self.retention_by_hand {
            app.retention = app.retention.with_timings(RetentionTimings {
                interval: Duration::from_hours(24),
                ..RetentionTimings::default()
            });
        }
        let retention = app.retention.service().clone();
        let (stop, signals) = ChannelSignals::new();
        let timeouts = ShutdownTimeouts::of(&self.config.http);
        let db = database.clone();
        let scraped = if self.metrics {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .map_err(|_| "the metrics port couldn't be bound")?;
            Some(listener)
        } else {
            None
        };
        let metrics_port = scraped
            .as_ref()
            .and_then(|listener| listener.local_addr().ok());
        let mut serving = JoinSet::new();
        serving.spawn(async move {
            serve(http, port, scraped, app, &database, signals, timeouts).await
        });
        self.running = Some(Running {
            db,
            registry,
            metrics: render,
            metrics_port,
            retention,
            stop,
            serving,
        });
        Ok(())
    }

    /// Stops serve with a signal within `within`: whether it stopped
    /// cleanly, every stream ended and recorded, the database closed. It
    /// never panics, so the managed check's teardown goes on whatever
    /// happened; a serve still running at `within` is aborted.
    pub(crate) async fn try_stop(&mut self, within: Duration) -> bool {
        let Some(mut running) = self.running.take() else {
            return false;
        };
        if running.stop.send(Signal::Terminate).await.is_err() {
            return false;
        }
        matches!(
            tokio::time::timeout(within, running.serving.join_next()).await,
            Ok(Some(Ok(Exit::Stopped)))
        )
    }

    /// [`Server::try_stop`] within [`PATIENCE`], which must stop cleanly.
    pub(crate) async fn stop(&mut self) {
        assert!(self.try_stop(PATIENCE).await, "the server stops cleanly");
    }

    /// Serve again, on the same database, PKI and API listener, the gRPC
    /// port on a fresh port the proxies follow: the agents' URLs and the
    /// Owner's session still hold.
    pub(crate) async fn start_again(&mut self) {
        assert!(self.running.is_none(), "the server is stopped");
        let grpc = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = grpc.local_addr().unwrap();
        self.grpc.set(address);
        if let Some(plane) = self.config.grpc.as_mut() {
            plane.bind = address;
        }
        self.boot(grpc)
            .await
            .unwrap_or_else(|why| panic!("the loopback server didn't start again: {why}"));
    }

    /// Stops serve and starts it again.
    pub(crate) async fn restart(&mut self) {
        self.stop().await;
        self.start_again().await;
    }

    fn running(&self) -> &Running {
        self.running.as_ref().expect("the server runs")
    }

    /// The API's URL.
    pub(crate) fn url(&self) -> String {
        format!("http://{}", self.http_address)
    }

    /// The gRPC port's address now.
    pub(crate) fn grpc(&self) -> SocketAddr {
        self.grpc.get()
    }

    /// The gRPC port's address as it moves, for a proxy.
    pub(crate) fn grpc_address(&self) -> GrpcAddress {
        self.grpc.clone()
    }

    /// The CA certificate agents trust.
    pub(crate) fn ca_cert_file(&self) -> PathBuf {
        grpc_config(&self.config).pki.ca_cert_file.clone()
    }

    /// A loopback case's clock: the Owner's TOTP steps move it.
    pub(crate) fn clock(&self) -> &ManualClock {
        self.manual.as_ref().expect("a loopback case's server")
    }

    /// The running server's series `name`, every label's sample summed, as
    /// its metrics port renders them now; 0 for a series it doesn't have.
    pub(crate) fn series(&self, name: &str) -> f64 {
        self.running()
            .metrics
            .text()
            .lines()
            .filter_map(|line| {
                let (series, value) = line.rsplit_once(' ')?;
                let series = series.split_once('{').map_or(series, |(bare, _)| bare);
                (series == name).then(|| value.parse::<f64>().ok())?
            })
            .sum()
    }

    /// One run of the running server's retention, now, at the task's own
    /// batch size and pause: what it did.
    pub(crate) async fn retention_run(&self) -> Run {
        self.running()
            .retention
            .run_once(&CancellationToken::new())
            .await
    }

    /// How many bytes the database holds on disk now: its file and its
    /// write-ahead log together.
    pub(crate) fn database_bytes(&self) -> u64 {
        let file = &self.config.database.path;
        let mut wal = file.clone().into_os_string();
        wal.push("-wal");
        [file.clone(), PathBuf::from(wal)]
            .iter()
            .filter_map(|path| std::fs::metadata(path).ok())
            .map(|found| found.len())
            .sum()
    }

    /// Where the running server's metrics port listens.
    pub(crate) fn metrics_port(&self) -> SocketAddr {
        self.running()
            .metrics_port
            .expect("the case turned the metrics port on")
    }

    /// The running server's agents' streams.
    pub(crate) fn registry(&self) -> &AgentRegistry {
        &self.running().registry
    }

    /// An enrollment token's bundle, as the host's `agent
    /// create-enrollment-token` makes it.
    ///
    /// # Errors
    /// A fixed text when the server isn't running or the token can't be
    /// made.
    pub(crate) async fn try_bundle(&self) -> Result<EnrollmentBundle, SetupError> {
        let running = self.running.as_ref().ok_or("the server isn't running")?;
        let random = Arc::clone(&self.random);
        EnrollmentTokenService::new(
            Arc::clone(&self.clock),
            Arc::clone(&random),
            Arc::new(running.db.clone()),
            Arc::new(Sha256Tokens::new(random)),
            self.ca_fingerprint,
        )
        .create()
        .await
        .map(|created| created.bundle)
        .map_err(|_| "the enrollment token couldn't be made")
    }

    /// [`Server::try_bundle`] for a loopback case, which must succeed.
    pub(crate) async fn bundle(&self) -> EnrollmentBundle {
        self.try_bundle().await.unwrap()
    }

    /// The ID of the agent enrolled as `name`.
    pub(crate) async fn agent_id(&self, name: &str) -> AgentId {
        self.running()
            .db
            .agents()
            .list(Page {
                before: None,
                limit: PageLimit::new(100).unwrap(),
            })
            .await
            .unwrap()
            .into_iter()
            .find(|listed| listed.agent.name.as_str() == name)
            .expect("the agent is enrolled")
            .agent
            .id
    }

    /// Agent `agent`'s certificates, oldest first.
    pub(crate) async fn certs(&self, agent: AgentId) -> Vec<AgentCert> {
        self.running().db.agents().certs(agent).await.unwrap()
    }

    /// Bot `bot`'s row.
    pub(crate) async fn bot(&self, bot: BotId) -> Bot {
        self.running().db.bots().find(bot).await.unwrap().unwrap()
    }

    /// How many chat lines the database holds for `bot`.
    pub(crate) async fn chat_lines(&self, bot: BotId) -> usize {
        let db = &self.running().db;
        let limit = PageLimit::new(u32::from(PageLimit::MAX)).unwrap();
        let whole = usize::from(PageLimit::MAX);
        let mut count = 0;
        let mut before = None;
        loop {
            // One row more than the limit when another page follows.
            let rows = db.chat().page(bot, Page { before, limit }).await.unwrap();
            count += rows.len().min(whole);
            match rows.get(whole.saturating_sub(1)) {
                Some(last) if rows.len() > whole => before = Some(last.record.id),
                _ => return count,
            }
        }
    }

    /// Every audit entry of `action`, oldest first.
    pub(crate) async fn entries(&self, action: &AuditAction) -> Vec<AuditRecord> {
        let mut all = self.audit().await;
        all.retain(|record| &record.action == action);
        all
    }

    /// Every audit entry, oldest first.
    pub(crate) async fn audit(&self) -> Vec<AuditRecord> {
        let db = &self.running().db;
        let mut all = Vec::new();
        let mut before = None;
        loop {
            let page = db
                .audit()
                .list(AuditPage {
                    before,
                    limit: AuditLimit::new(100).unwrap(),
                })
                .await
                .unwrap();
            let Some(last) = page.last() else { break };
            before = Some(last.id);
            all.extend(page);
        }
        all.reverse();
        all
    }
}

/// The production config, but for what a case sets: its addresses, its
/// files in `dir`, `options`' heartbeats and limits, whether each bot keeps
/// only the least chat a config allows (`retention_by_hand`) and whether
/// backups go into the folder's [`BACKUPS`], and whether offline accounts
/// are on (a loopback case's only accounts, since it has no Microsoft). The
/// vault key comes from memory, so its file is never read.
fn config(
    dir: &Path,
    http: SocketAddr,
    grpc: SocketAddr,
    options: ServerOptions,
    allow_offline: bool,
) -> ServerConfig {
    let ServerOptions {
        heartbeats,
        limits,
        retention_by_hand: least_chat,
        backups,
        ..
    } = options;
    let pki = dir.join("pki");
    ServerConfig {
        dev_mode: false,
        http: HttpConfig {
            bind: http,
            request_timeout: Duration::from_secs(15),
            max_body_bytes: 65_536,
            trusted_proxies: TrustedProxies::default(),
        },
        database: DatabaseConfig {
            path: dir.join("afkfleet.db"),
            backups: backups.then(|| BackupConfig {
                dir: dir.join(BACKUPS),
                policy: BackupPolicy::default(),
            }),
        },
        log: LogConfig {
            format: LogFormat::Json,
            filter: "info".parse().unwrap(),
        },
        auth: AuthConfig::default(),
        rate_limits: limits.config(),
        vault: VaultConfig {
            key_files: BTreeMap::from([(1, dir.join("vault-key-1"))]),
            active_key_id: 1,
        },
        accounts: AccountsConfig {
            allow_offline,
            ..AccountsConfig::default()
        },
        chat: if least_chat {
            ChatConfig {
                bound: ChatBound::try_new(ChatBound::MIN_LINES).unwrap_or_default(),
                ..ChatConfig::default()
            }
        } else {
            ChatConfig::default()
        },
        audit: AuditConfig::default(),
        grpc: Some(GrpcConfig {
            bind: grpc,
            pki: PkiPaths {
                ca_cert_file: pki.join("ca.crt"),
                ca_key_file: pki.join("ca.key"),
                server_cert_file: pki.join("server.crt"),
                server_key_file: pki.join("server.key"),
            },
            agent_cert_lifetime: TimeDelta::days(60),
            heartbeats,
        }),
        metrics: None,
    }
}

/// The config of the server whose files are in `dir`, run as `options` say,
/// for a command that binds nothing (a crash case's `backup`): the
/// addresses are never used.
pub(crate) fn config_in(dir: &Path, options: ServerOptions) -> ServerConfig {
    let unused = SocketAddr::from(([127, 0, 0, 1], 1));
    config(dir, unused, unused, options, true)
}

fn grpc_config(config: &ServerConfig) -> &GrpcConfig {
    config.grpc.as_ref().expect("the control plane is on")
}

async fn passwords(
    config: &ServerConfig,
    random: &Arc<dyn SecureRandom>,
) -> Result<Arc<dyn Passwords>, SetupError> {
    Argon2Passwords::new(config.auth.argon2, Arc::clone(random))
        .await
        .map(|passwords| Arc::new(passwords) as Arc<dyn Passwords>)
        .map_err(|_| "the password service couldn't start")
}

/// `at` in whole milliseconds, as the database stores times.
fn whole_millis(at: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp_millis(at.timestamp_millis()).unwrap()
}

/// Delivers the signals the case sends.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stopping_a_stopped_server_says_so_and_never_panics() {
        let dir = tempfile::tempdir().unwrap();
        let mut server = Server::start(dir.path(), ServerOptions::default()).await;
        assert!(server.try_stop(PATIENCE).await, "a running one stops");

        let again = server.try_stop(PATIENCE).await;

        assert!(!again);
    }

    #[tokio::test]
    async fn a_setup_step_that_fails_is_a_fixed_text_never_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        // A PKI's file already there: `pki init` never overwrites one.
        std::fs::create_dir_all(dir.path().join("pki")).unwrap();
        std::fs::write(dir.path().join("pki/ca.crt"), "placeholder").unwrap();
        let setup = RealSetup {
            // Never called: the setup ends before serve.
            microsoft: Arc::new(ReqwestMicrosoftAuth::new(Arc::new(SystemClock)).unwrap()),
            password: SecretString::from(PASSWORD),
            key: SecretBox::new(Box::new([7; 32])),
        };

        let started = Server::start_real(dir.path(), setup).await;

        assert_eq!(started.unwrap_err(), "the PKI couldn't be created");
    }
}
