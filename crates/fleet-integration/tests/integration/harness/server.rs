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

use core::time::Duration;
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::TimeDelta;
use fleet_core::audit::AuditAction;
use fleet_core::auth::NewPassword;
use fleet_core::enrollment::EnrollmentBundle;
use fleet_core::id::{AgentId, BotId};
use fleet_core::pki::CertFingerprint;
use fleet_core::system::{Clock, SecureRandom};
use fleet_core::value::Username;
use fleet_server::app::enrollment::{Creator, EnrollmentTokenService};
use fleet_server::app::users::UserService;
use fleet_server::cli::pki;
use fleet_server::cli::serve::{
    ControlPlane, Exit, ServerPorts, ShutdownTimeouts, app_with, grpc_port, open, serve,
};
use fleet_server::config::{
    AccountsConfig, AuditConfig, AuthConfig, ChatConfig, DatabaseConfig, GrpcConfig, Heartbeats,
    HttpConfig, LogConfig, LogFormat, PkiPaths, RateLimitConfig, ServerConfig, VaultConfig,
};
use fleet_server::grpc::registry::AgentRegistry;
use fleet_server::http::rate_limit::TrustedProxies;
use fleet_server::infra::crypto::passwords::Argon2Passwords;
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::infra::crypto::vault::{DerivedKey, VaultKeys};
use fleet_server::infra::msauth::ReqwestMicrosoftAuth;
use fleet_server::infra::pki::load;
use fleet_server::infra::sqlite::{self, Database, DatabaseOptions};
use fleet_server::infra::system::{MonotonicClock, OsRandom, SystemClock, TokioPace};
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

use super::logs::PATIENCE;

/// The Owner's name.
pub(crate) const OWNER: &str = "owner";
/// A loopback case's Owner's password: strong, so the server takes it.
pub(crate) const PASSWORD: &str = "correct horse battery staple";

/// How a loopback case's server runs.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ServerOptions {
    /// `[grpc] heartbeat_interval_secs` and `heartbeat_timeout_secs`: the
    /// production 10 s and 30 s, but where a case measures them.
    pub(crate) heartbeats: Heartbeats,
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
    keys: Keys,
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
    stop: mpsc::Sender<Signal>,
    serving: JoinSet<Exit>,
}

/// Everything a first start needs.
struct Begin {
    heartbeats: Heartbeats,
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
        let manual = ManualClock::new(whole_millis(SystemClock.now()));
        Self::begin(
            dir,
            Begin {
                heartbeats: options.heartbeats,
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
                heartbeats: Heartbeats::default(),
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
            begin.heartbeats,
            begin.allow_offline,
        );
        std::fs::create_dir_all(dir.join("pki"))
            .map_err(|_| "the PKI's folder couldn't be made")?;
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
            keys: begin.keys,
            ca_fingerprint,
            running: None,
        };
        server.boot(grpc).await?;
        Ok(server)
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
        let ports = ServerPorts {
            clock: Arc::clone(&self.clock),
            limiter_clock: Arc::new(MonotonicClock::start()),
            random: Arc::clone(&self.random),
            microsoft: Arc::clone(&self.microsoft),
            pace: Arc::new(TokioPace),
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
        let (stop, signals) = ChannelSignals::new();
        let timeouts = ShutdownTimeouts::of(&self.config.http);
        let db = database.clone();
        let mut serving = JoinSet::new();
        serving.spawn(async move { serve(http, port, app, &database, signals, timeouts).await });
        self.running = Some(Running {
            db,
            registry,
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
        .create(Creator::Host)
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

    /// Bot `bot`'s row.
    pub(crate) async fn bot(&self, bot: BotId) -> Bot {
        self.running().db.bots().find(bot).await.unwrap().unwrap()
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
/// files in `dir`, `heartbeats`, and whether offline accounts are on (a
/// loopback case's only accounts, since it has no Microsoft). The vault key
/// comes from memory, so its file is never read.
fn config(
    dir: &Path,
    http: SocketAddr,
    grpc: SocketAddr,
    heartbeats: Heartbeats,
    allow_offline: bool,
) -> ServerConfig {
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
        },
        log: LogConfig {
            format: LogFormat::Json,
            filter: "info".parse().unwrap(),
        },
        auth: AuthConfig::default(),
        rate_limits: RateLimitConfig::default(),
        vault: VaultConfig {
            key_files: BTreeMap::from([(1, dir.join("vault-key-1"))]),
            active_key_id: 1,
        },
        accounts: AccountsConfig {
            allow_offline,
            ..AccountsConfig::default()
        },
        chat: ChatConfig::default(),
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
    }
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
