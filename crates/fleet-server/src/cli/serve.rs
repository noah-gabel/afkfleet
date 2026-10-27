//! `afkfleet-server serve` (Plan.md P6.11, P6.12, ADR-0015): the HTTP API,
//! until a signal stops it.
//!
//! main.rs loads the config, starts logging, logs the dev-mode warning,
//! loads the vault's keys (plain file reads, before any runtime exists),
//! builds a multi-threaded runtime, installs the stop signals and binds
//! `[http] bind`; [`run`] derives the lockout's key from the vault's, opens
//! the database (running every pending migration), builds the app over the
//! real clock, the OS's randomness, the vault, the database and Microsoft's
//! client (after main.rs installed the TLS provider), and serves.
//! Every startup failure is one `error` event, "the server can't start",
//! with the error's whole chain, and exit 1. The last line, "the server
//! stopped", carries the exit code.
//!
//! # The app
//! [`app`] builds the router and its [`Housekeeping`] (ADR-0016) through
//! [`app_with`], over the real clocks and randomness of
//! [`ServerPorts::system`]. Tests that run the whole server over a socket,
//! `fleet-client`'s among them, call [`app_with`] with a manual clock and
//! seeded randomness instead (ADR-0017):
//! - **Two clocks:** the rate limiters run on a [`MonotonicClock`], so a
//!   step of the wall clock can't make them refuse everyone; everything
//!   stored or audited (sessions, the lockout, audit entries) runs on the
//!   [`SystemClock`].
//! - **Every connection carries its peer's address** (axum's
//!   `into_make_service_with_connect_info`): the per-IP limits need it, and a
//!   request without it is refused as a wiring bug.
//! - **The housekeeping** and **the retention task** (P11.6; ADR-0020,
//!   S45) are the server's long-lived background tasks, owned by
//!   [`serve`]'s `JoinSet`, with or without the control plane, and stop at the
//!   first signal through a child of the shutdown token; the retention
//!   records its summary at the stop, before the database closes.
//! - **The link flows** (P9.4) poll Microsoft on their service's own
//!   tasks; a task on the same `JoinSet` stops them at the first signal and
//!   waits for them (ADR-0018).
//! - **The reconciler** (P10.5), while the control plane is on, is one more
//!   task on that `JoinSet`: the registry, the accounts' and the users'
//!   services hint it through one bounded queue, and it stops at the first
//!   signal, after the pass it's in (ADR-0019, S37). It's watched: if it
//!   ends or panics before that, it's logged at `error` and the server
//!   stops as one that ended unasked (exit 4); nothing restarts it.
//!
//! # Shutdown
//! The first signal starts it, and one deadline bounds it,
//! [`ShutdownTimeouts::deadline`] from that signal; nothing waits past it.
//! 1. The server stops accepting and closes idle connections, and in-flight
//!    requests finish (axum's graceful shutdown, through a
//!    `CancellationToken`).
//! 2. The drain may take up to `[http] request_timeout_secs`, which the
//!    timeout layer already enforces on every request, so no handler still
//!    runs at its limit. A connection still open then (a slow reader) is
//!    logged at `warn` and left: axum owns its connection tasks, and the
//!    runtime's shutdown ends them right after.
//! 3. The housekeeping ends (it stopped at the signal, or after its current
//!    purge batch), the retention task (after its current batch, and its
//!    summary, which waits at most 1 s for the write connection), the link
//!    flows (each stopped at the signal, or after a commit that started)
//!    and the reconciler (after its current pass),
//!    then the database closes, all with the rest of the deadline, at least
//!    [`DATABASE_CLOSE`]. If that doesn't finish in time, that's an `error`
//!    and exit 4.
//!
//! A later signal is only logged: the shutdown is already running.
//!
//! Tests bind `127.0.0.1:0` themselves and pass the listener to [`run`], so
//! the config's rule against port 0 never needs an exception.

use core::pin::pin;
use core::time::Duration;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use chrono::{DateTime, Utc};
use fleet_core::system::{Clock, SecureRandom};
use fleet_proto::enrollment::SERVER_DEADLINE;
use fleet_startup::signals::ShutdownSignals;
use secrecy::SecretBox;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until, timeout};
use tokio_util::sync::CancellationToken;
use tonic::transport::ServerTlsConfig;
use tracing::{error, info, warn};

use super::DATABASE_CLOSE;
use super::housekeeping::Housekeeping;
use super::retention::Retention;
use crate::app::accounts::{AccountService, AccountSettings};
use crate::app::agents::ingest::{IngestBounds, IngestTimings, IngestWriter, ingest};
use crate::app::agents::{
    AgentConnectionService, AgentService, AssignmentService, SessionGrantService,
};
use crate::app::audit::AuditService;
use crate::app::auth::TokenService;
use crate::app::bots::{BotService, ChatWait};
use crate::app::challenges::Challenges;
use crate::app::enrollment::{EnrollmentService, EnrollmentTokenService};
use crate::app::event_tickets::EventTickets;
use crate::app::events::{EventBus, LiveRights};
use crate::app::grants::GrantService;
use crate::app::health::HealthService;
use crate::app::invites::InviteService;
use crate::app::link_flows::{LinkFlowPorts, LinkFlowService, LinkFlowSettings};
use crate::app::lockout::LockoutService;
use crate::app::login::LoginService;
use crate::app::mfa::{MfaPorts, MfaService};
use crate::app::modes::ModeService;
use crate::app::retention::{RetentionService, RetentionTimings};
use crate::app::session_tokens::{SessionTokenPorts, SessionTokenService};
use crate::app::users::UserService;
use crate::config::{HttpConfig, ServerConfig};
use crate::grpc::control::{ControlPorts, ControlRoute, ControlTimings, Sessions, control_service};
use crate::grpc::enrollment::enrollment_service;
use crate::grpc::listener::{self as grpc_listener, EnrollmentRoute};
use crate::grpc::reconciler::{HINT_QUEUE, HintInbox, ReconcileTimings, Reconciler, hints};
use crate::grpc::registry::{AgentRegistry, SWEEP_INTERVAL};
use crate::http::error::chain;
use crate::http::middleware::RequestIds;
use crate::http::openapi::ApiDocs;
use crate::http::rate_limit::RateLimits;
use crate::http::router::{AppState, Services, router};
use crate::http::ws::{LiveEvents, LiveSockets};
use crate::infra::crypto::attempt_keys::HmacAttemptKeys;
use crate::infra::crypto::passwords::Argon2Passwords;
use crate::infra::crypto::recovery_codes::Sha256RecoveryCodes;
use crate::infra::crypto::tokens::Sha256Tokens;
use crate::infra::crypto::totp::TotpRs;
use crate::infra::crypto::vault::{DerivedKey, VaultKeyError, VaultKeys, XChaChaVault};
use crate::infra::msauth::{BuildError, ReqwestMicrosoftAuth};
use crate::infra::pki::X509AgentNames;
use crate::infra::pki::{AgentIssuer, CertificateAuthority, LoadedPki, PkiLoadError};
use crate::infra::qr::SvgQr;
use crate::infra::sqlite::{self, Database, DatabaseOptions, OpenError};
use crate::infra::system::{MonotonicClock, OsRandom, SystemClock, TokioPace};
use crate::ports::agent_connections::AgentConnections;
use crate::ports::live_hints::LiveHints;
use crate::ports::msauth::MicrosoftAuthProvider;
use crate::ports::pace::Pace;
use crate::ports::passwords::{HashError, Passwords};
use crate::ports::reconcile::{NoReconcile, Reconcile};
use crate::ports::session_grants::SessionGrants;
use crate::ports::store::Store;
use crate::ports::tokens::Tokens;
use crate::ports::vault::Vault;

/// How long main.rs's runtime waits for its tasks once [`run`] returned;
/// the connections axum still serves end then. Docker's `stop_grace_period`
/// must be longer than the shutdown deadline plus this (P12).
pub const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(1);

/// How the server's run ended, which decides the process's exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// A signal stopped the server and the database closed in time, even if
    /// connections were still open at the drain limit.
    Stopped,
    /// The server couldn't start: the runtime, the signals, the address or
    /// the database.
    StartupFailed,
    /// The server ended without being asked to, or the database didn't
    /// close by the shutdown deadline.
    ServerFailed,
}

impl Exit {
    /// The process's exit code (Plan.md P6.11, ADR-0015).
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Stopped => 0,
            Self::StartupFailed => 1,
            Self::ServerFailed => 4,
        }
    }

    /// Logs the server's last line, "the server stopped", with the exit
    /// code.
    pub fn log(self) {
        info!(exit_code = self.code(), "the server stopped");
    }
}

/// How long each stage of the shutdown may take, counted from the first
/// signal: see the [module docs](self).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownTimeouts {
    /// How long in-flight requests may take to finish.
    pub drain: Duration,
    /// The least time the database gets to close; it also gets whatever the
    /// drain didn't use.
    pub close: Duration,
}

impl ShutdownTimeouts {
    /// The server's: the drain gets `[http] request_timeout_secs`, and the
    /// close [`DATABASE_CLOSE`].
    #[must_use]
    pub const fn of(http: &HttpConfig) -> Self {
        Self {
            drain: http.request_timeout,
            close: DATABASE_CLOSE,
        }
    }

    /// The whole shutdown's deadline, from the first signal: the drain's
    /// limit plus the close's.
    #[must_use]
    pub const fn deadline(self) -> Duration {
        self.drain.saturating_add(self.close)
    }
}

/// Why the server couldn't start once logging runs.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The aws-lc-rs provider couldn't become the process's rustls
    /// provider: another one was installed first (ADR-0018).
    #[error("a TLS crypto provider is already installed")]
    CryptoProvider,
    /// The async runtime couldn't be built.
    #[error("the async runtime can't be built")]
    Runtime(#[source] io::Error),
    /// The stop signals' handlers couldn't be installed.
    #[error("the stop signals can't be installed")]
    Signals(#[source] io::Error),
    /// The address couldn't be bound, or its local address read.
    #[error("can't listen on {address}")]
    Listen {
        /// The configured address.
        address: SocketAddr,
        /// What the OS reported.
        #[source]
        source: io::Error,
    },
    /// The database couldn't be opened or migrated.
    #[error("the database can't be opened")]
    Database(#[source] OpenError),
    /// A vault key file can't be used.
    #[error("the vault's keys can't be loaded")]
    VaultKeys(#[source] VaultKeyError),
    /// The password service couldn't make its dummy hash.
    #[error("the password service can't start")]
    Passwords(#[source] HashError),
    /// The HTTP client for Microsoft couldn't be built (ADR-0018).
    #[error(transparent)]
    Microsoft(BuildError),
    /// `[grpc]`'s PKI couldn't be read or doesn't hold together (ADR-0019).
    #[error("the control plane's PKI can't be used")]
    Pki(#[source] PkiLoadError),
    /// The gRPC port couldn't be set up with the PKI (ADR-0019).
    #[error("the gRPC port can't be set up")]
    Grpc(#[source] tonic::transport::Error),
}

/// Logs `error` with its whole chain as "the server can't start" and
/// returns [`Exit::StartupFailed`].
pub fn cant_start(error: &StartError) -> Exit {
    error!(error = %chain(error), "the server can't start");
    Exit::StartupFailed
}

/// What [`serve`] runs: the router, its periodic housekeeping and retention,
/// and the link flows it stops at the first signal.
#[derive(Debug)]
pub struct App {
    /// Every route behind the middleware stack.
    pub router: Router,
    /// The background task that looks after the rate limits, the lockout,
    /// the login's challenges and the finished link flows.
    pub housekeeping: Housekeeping,
    /// The background task that deletes what the retention no longer keeps
    /// (P11.6).
    pub retention: Retention,
    /// The Microsoft link flows, whose pollers run until the shutdown.
    pub link_flows: LinkFlowService,
    /// The gRPC port's services, while the control plane is on: [`run`]
    /// serves them on the gRPC port.
    pub grpc: Option<GrpcRoutes>,
    /// The internal event bus, what agents report as the ingest takes it in
    /// (P10.8), for P11.5's live events.
    pub events: EventBus,
    /// The live events sockets' owner: [`serve`] closes them at the shutdown
    /// and waits for them, within the drain, before the database closes
    /// (P11.5; ADR-0020, S35).
    pub sockets: LiveSockets,
    /// The shutdown's token, which [`serve`] cancels at the first signal:
    /// the session-token service keeps a child of it (ADR-0019, S64).
    pub shutdown: CancellationToken,
}

/// The gRPC port's services: the agents' enrollment and their `Connect`,
/// each behind its gate, and the owner of the `Connect` streams' sessions,
/// which [`serve`] ends at the shutdown.
#[derive(Debug)]
pub struct GrpcRoutes {
    /// `AgentEnrollment.Enroll`.
    pub enrollment: EnrollmentRoute,
    /// `AgentControl.Connect`.
    pub control: ControlRoute,
    /// The sessions of the `Connect` streams.
    pub sessions: Sessions,
    /// The sweep that ends streams whose permission ended: each agent's one
    /// stream, and what checks their certificates.
    pub sweep: Sweep,
    /// Which agent runs which bot, kept true.
    pub reconciler: Reconciler,
    /// The Minecraft sessions the grants hand out, stopped within the
    /// shutdown's drain (ADR-0019, S64).
    pub session_tokens: SessionTokenService,
    /// The ingest's batch writer, stopped once every stream ended (S70).
    pub ingest: IngestWriter,
}

/// The sweep [`serve`] runs while the gRPC port is on (ADR-0019, S27).
#[derive(Debug)]
pub struct Sweep {
    /// Each agent's one stream.
    pub registry: AgentRegistry,
    /// What checks the live streams' certificates.
    pub connections: Arc<dyn AgentConnections>,
}

/// The control plane, while `[grpc]` is configured: the listener main.rs
/// bound on `[grpc] bind` and the PKI it read before the runtime started.
#[derive(Debug)]
pub struct ControlPlane {
    /// The gRPC port's listener.
    pub listener: TcpListener,
    /// The CA, which signs agents' certificates, and the server's identity.
    pub pki: LoadedPki,
}

/// The gRPC port [`serve`] runs beside the API: its listener and its
/// server, TLS and services set up, and the owner of its sessions.
#[derive(Debug)]
pub struct GrpcPort {
    /// The port's listener.
    pub listener: TcpListener,
    /// Its server.
    pub router: tonic::transport::server::Router,
    /// The sessions of its `Connect` streams, ended at the shutdown.
    pub sessions: Sessions,
    /// The sweep of its streams, run until the shutdown.
    pub sweep: Sweep,
    /// The reconciler, run until the shutdown.
    pub reconciler: Reconciler,
    /// The Minecraft sessions its grants hand out, stopped within the drain.
    pub session_tokens: SessionTokenService,
    /// The ingest's batch writer, run until every stream ended.
    pub ingest: IngestWriter,
}

/// The clocks, the randomness, Microsoft and the pacing the app runs on.
#[derive(Debug, Clone)]
pub struct ServerPorts {
    /// Everything stored or audited: sessions, the lockout, audit entries,
    /// request IDs, TOTP steps, the link flows' expiries.
    pub clock: Arc<dyn Clock>,
    /// The rate limiters' clock, which only measures intervals.
    pub limiter_clock: Arc<dyn Clock>,
    /// Tokens, IDs, nonces and secrets.
    pub random: Arc<dyn SecureRandom>,
    /// Microsoft's account flows.
    pub microsoft: Arc<dyn MicrosoftAuthProvider>,
    /// The link flows' waits between their calls.
    pub pace: Arc<dyn Pace>,
}

impl ServerPorts {
    /// The real ones: [`SystemClock`], a [`MonotonicClock`] for the
    /// limiters, [`OsRandom`], [`ReqwestMicrosoftAuth`] on the system clock,
    /// and [`TokioPace`]. Only [`app`] calls it, so `serve`, `user
    /// create-owner` and the `vault` commands stay the only places the real
    /// clock and randomness are built.
    ///
    /// # Errors
    /// [`BuildError`] when Microsoft's HTTP client can't be built.
    pub fn system() -> Result<Self, BuildError> {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        Ok(Self {
            microsoft: Arc::new(ReqwestMicrosoftAuth::new(Arc::clone(&clock))?),
            clock,
            limiter_clock: Arc::new(MonotonicClock::start()),
            random: Arc::new(OsRandom),
            pace: Arc::new(TokioPace),
        })
    }
}

/// The app `serve` runs: [`app_with`] over [`ServerPorts::system`], the
/// real clocks, the OS's randomness and Microsoft; see the [module
/// docs](self).
///
/// # Errors
/// [`StartError::Microsoft`] when Microsoft's HTTP client can't be built.
pub fn app(
    config: &ServerConfig,
    database: &Database,
    keys: VaultKeys,
    lockout_key: SecretBox<[u8; 32]>,
    passwords: Arc<dyn Passwords>,
    ca: Option<CertificateAuthority>,
) -> Result<App, StartError> {
    let ports = ServerPorts::system().map_err(StartError::Microsoft)?;
    Ok(app_with(
        config,
        database,
        keys,
        lockout_key,
        passwords,
        ports,
        ca,
    ))
}

/// The app over `ports`: the routes over `database`, sessions with
/// `[auth]`'s lifetimes, the lockout with its threshold and `lockout_key`,
/// the `[rate_limits]` on the limiters' clock, request IDs and tokens from
/// the clock and the randomness, and the `OpenAPI` document in dev mode.
///
/// It builds the vault from `keys` too, for the 2FA service, which seals and
/// opens the users' TOTP secrets with it (ADR-0016), and logs in through
/// `passwords`, which [`run`] builds (its dummy hash is async work).
///
/// With `[grpc]` and its `ca`, the control plane is on: the API creates
/// enrollment tokens that pin the CA, and [`App::grpc`] enrolls agents,
/// signing their certificates with it, and admits their streams
/// (ADR-0019), and its reconciler places the bots on them (P10.5). Without
/// either, the token route answers 404, there is no gRPC port, and nothing
/// is hinted.
#[must_use]
pub fn app_with(
    config: &ServerConfig,
    database: &Database,
    keys: VaultKeys,
    lockout_key: SecretBox<[u8; 32]>,
    passwords: Arc<dyn Passwords>,
    ports: ServerPorts,
    ca: Option<CertificateAuthority>,
) -> App {
    let ServerPorts {
        clock,
        limiter_clock,
        random,
        microsoft,
        pace,
    } = ports;
    let store: Arc<dyn Store> = Arc::new(database.clone());
    // One bus, built first: the agents' answers, a chat send's wait (S5) and
    // the services' hints to the live events sockets (R36).
    let events = EventBus::new();
    let vault: Arc<dyn Vault> = Arc::new(XChaChaVault::new(keys, Arc::clone(&random)));
    let (reconcile, inbox) = hint_queue(config.grpc.is_some() && ca.is_some());
    let hints = Hints::new(&reconcile, &events);
    let shared = Shared {
        clock: &clock,
        random: &random,
        store: &store,
    };
    let (link_flows, accounts) = account_services(
        config,
        &reconcile,
        shared.link_flow_ports(&vault, &microsoft, pace, hints.live()),
    );
    let token_port: Arc<dyn Tokens> = Arc::new(Sha256Tokens::new(Arc::clone(&random)));
    let auth = auth_services(
        config,
        shared,
        AuthPorts {
            vault: Arc::clone(&vault),
            tokens: &token_port,
            passwords,
            lockout_key,
        },
        &hints,
    );
    let limits = RateLimits::new(&config.rate_limits, limiter_clock);
    let ids = RequestIds::new(Arc::clone(&clock), Arc::clone(&random));
    let audit = AuditService::new(Arc::clone(&clock), Arc::clone(&store));
    let fleet = fleet_services(config, shared, &hints, &events, reconcile);
    let retention = retention(config, &clock, &store);
    let shutdown = CancellationToken::new();
    let (sockets, live) = live_events(&auth.tickets, &events, &store, &shutdown);
    let (enrollment_tokens, grpc) = control_plane_services(
        config,
        ca,
        inbox,
        EnrollmentPorts {
            clock,
            random,
            store: Arc::clone(&store),
            tokens: token_port,
            limits: limits.clone(),
            ids: ids.clone(),
            registry: fleet.registry.clone(),
            vault,
            microsoft,
            shutdown: shutdown.clone(),
            bus: events.clone(),
        },
    );
    let state = AppState::new(Services {
        health: HealthService::new(store),
        tokens: auth.tokens,
        limits: limits.clone(),
        mfa: auth.mfa,
        login: auth.login,
        users: auth.users,
        invites: auth.invites,
        link_flows: link_flows.clone(),
        accounts,
        grants: fleet.grants,
        enrollment_tokens,
        agents: fleet.agents,
        bots: fleet.bots,
        modes: fleet.modes,
        events: live,
        audit,
    });
    App {
        router: router(
            state,
            &config.http,
            ids,
            ApiDocs::for_dev_mode(config.dev_mode),
        ),
        housekeeping: housekeeping(
            limits,
            auth.lockout,
            auth.challenges,
            auth.tickets,
            &link_flows,
            fleet.registry,
        ),
        retention,
        link_flows,
        grpc,
        events,
        sockets,
        shutdown,
    }
}

/// The live events over `tickets` and `events`, their rights read through
/// `store`, and their sockets' owner, stopped with `shutdown`.
fn live_events(
    tickets: &EventTickets,
    events: &EventBus,
    store: &Arc<dyn Store>,
    shutdown: &CancellationToken,
) -> (LiveSockets, LiveEvents) {
    let sockets = LiveSockets::new(shutdown.child_token());
    let live = LiveEvents::new(
        tickets.clone(),
        events.clone(),
        sockets.clone(),
        LiveRights::new(Arc::clone(store)),
    );
    (sockets, live)
}

/// The fleet's side of the app.
struct FleetServices {
    grants: GrantService,
    /// Each agent's one stream.
    registry: AgentRegistry,
    agents: AgentService,
    modes: ModeService,
    bots: BotService,
}

/// The fleet's side over `shared`, hinting with `hints` and `reconcile`, the
/// bots' chat waiting on `events`: the accounts' sharing, the agents (each
/// one's stream, and the service that ends them), modes and bots.
fn fleet_services(
    config: &ServerConfig,
    shared: Shared<'_>,
    hints: &Hints,
    events: &EventBus,
    reconcile: Arc<dyn Reconcile>,
) -> FleetServices {
    let Shared {
        clock,
        random,
        store,
    } = shared;
    let grants =
        GrantService::new(Arc::clone(clock), Arc::clone(store)).with_live_hints(hints.live());
    let (registry, agents) = agent_services(clock, store, hints);
    // One allowlist for both: a mode's `unlisted_command` (S68) is what the bots' mode switch
    // authorizes with (R113).
    let allowlist = &config.chat.command_allowlist;
    let modes = mode_service(clock, random, store, hints).with_allowlist(allowlist.clone());
    let bots = bot_service(config, clock, random, store, &registry, events, reconcile)
        .with_allowlist(allowlist.clone());
    FleetServices {
        grants,
        registry,
        agents,
        modes,
        bots,
    }
}

/// What every service shares: the app's clock, randomness and store.
#[derive(Clone, Copy)]
struct Shared<'a> {
    clock: &'a Arc<dyn Clock>,
    random: &'a Arc<dyn SecureRandom>,
    store: &'a Arc<dyn Store>,
}

impl Shared<'_> {
    /// The link flows' ports: these, sealing with `vault`, asking
    /// `microsoft`, waiting at `pace`, telling the sockets through
    /// `live_hints`.
    fn link_flow_ports(
        self,
        vault: &Arc<dyn Vault>,
        microsoft: &Arc<dyn MicrosoftAuthProvider>,
        pace: Arc<dyn Pace>,
        live_hints: Arc<dyn LiveHints>,
    ) -> LinkFlowPorts {
        LinkFlowPorts {
            clock: Arc::clone(self.clock),
            random: Arc::clone(self.random),
            store: Arc::clone(self.store),
            vault: Arc::clone(vault),
            microsoft: Arc::clone(microsoft),
            pace,
            live_hints,
        }
    }
}

/// What the sign-in side runs on besides [`Shared`]: the vault that seals
/// TOTP secrets, the tokens' port, the password hasher and the lockout's
/// key.
struct AuthPorts<'a> {
    vault: Arc<dyn Vault>,
    tokens: &'a Arc<dyn Tokens>,
    passwords: Arc<dyn Passwords>,
    lockout_key: SecretBox<[u8; 32]>,
}

/// The sign-in side of the app.
struct AuthServices {
    tokens: TokenService,
    invites: InviteService,
    challenges: Challenges,
    tickets: EventTickets,
    mfa: MfaService,
    lockout: LockoutService,
    users: UserService,
    login: LoginService,
}

/// The sign-in side over `shared` and `ports`, with `[auth]`'s policies:
/// sessions and invites, login challenges, a live events ticket for every
/// socket the cap allows at once, second factors, the lockout, users
/// (hinting with `hints`) and logins.
fn auth_services(
    config: &ServerConfig,
    shared: Shared<'_>,
    ports: AuthPorts<'_>,
    hints: &Hints,
) -> AuthServices {
    let Shared {
        clock,
        random,
        store,
    } = shared;
    let (tokens, invites) = token_services(config, clock, random, store, ports.tokens);
    let challenges = Challenges::new(Arc::clone(clock), Arc::clone(ports.tokens));
    let tickets = EventTickets::new(Arc::clone(clock), Arc::clone(ports.tokens))
        .with_per_user(config.rate_limits.ws_connections_per_user);
    let mfa = mfa_service(clock, random, store, ports.vault, &challenges);
    let lockout = lockout_service(config, clock, store, ports.lockout_key);
    let users = user_service(clock, random, store, &ports.passwords, hints);
    let login = LoginService::new(
        Arc::clone(clock),
        Arc::clone(random),
        Arc::clone(store),
        ports.passwords,
        lockout.clone(),
        tokens.clone(),
        mfa.clone(),
    );
    AuthServices {
        tokens,
        invites,
        challenges,
        tickets,
        mfa,
        lockout,
        users,
        login,
    }
}

/// The retention task over `store`, by the config's retention and session
/// lifetimes, at its default pace.
fn retention(config: &ServerConfig, clock: &Arc<dyn Clock>, store: &Arc<dyn Store>) -> Retention {
    Retention::new(
        RetentionService::new(
            Arc::clone(clock),
            Arc::clone(store),
            config.retention(),
            config.auth.sessions,
        ),
        RetentionTimings::default(),
    )
}

/// The housekeeping of the app's limits, lockout, login challenges, live
/// events tickets, link flows and agents' streams.
fn housekeeping(
    limits: RateLimits,
    lockout: LockoutService,
    challenges: Challenges,
    event_tickets: EventTickets,
    link_flows: &LinkFlowService,
    agents: AgentRegistry,
) -> Housekeeping {
    Housekeeping::new(
        limits,
        lockout,
        challenges,
        event_tickets,
        link_flows.flows(),
        agents,
    )
}

/// The per-username lockout over the app's ports, with `[auth]`'s policy
/// and the key its rows are kept under.
fn lockout_service(
    config: &ServerConfig,
    clock: &Arc<dyn Clock>,
    store: &Arc<dyn Store>,
    lockout_key: SecretBox<[u8; 32]>,
) -> LockoutService {
    LockoutService::new(
        Arc::clone(clock),
        Arc::clone(store),
        Arc::new(HmacAttemptKeys::new(lockout_key)),
        config.auth.lockout,
    )
}

/// The sessions' and the invites' services over the app's ports, with
/// `[auth]`'s lifetimes.
fn token_services(
    config: &ServerConfig,
    clock: &Arc<dyn Clock>,
    random: &Arc<dyn SecureRandom>,
    store: &Arc<dyn Store>,
    token_port: &Arc<dyn Tokens>,
) -> (TokenService, InviteService) {
    let tokens = TokenService::new(
        Arc::clone(clock),
        Arc::clone(random),
        Arc::clone(store),
        Arc::clone(token_port),
        config.auth.sessions,
    );
    let invites = InviteService::new(
        Arc::clone(clock),
        Arc::clone(random),
        Arc::clone(store),
        Arc::clone(token_port),
        config.auth.invites,
    );
    (tokens, invites)
}

/// The reconciler's one hint queue while the control plane is `on`, and
/// what reads it; without it, hints go nowhere.
fn hint_queue(on: bool) -> (Arc<dyn Reconcile>, Option<HintInbox>) {
    if on {
        let (hints, inbox) = hints(HINT_QUEUE);
        (Arc::new(hints), Some(inbox))
    } else {
        (Arc::new(NoReconcile), None)
    }
}

/// The users' service over the app's ports, hashing with `passwords` and
/// hinting `reconcile` when a user is disabled or enabled.
fn user_service(
    clock: &Arc<dyn Clock>,
    random: &Arc<dyn SecureRandom>,
    store: &Arc<dyn Store>,
    passwords: &Arc<dyn Passwords>,
    hints: &Hints,
) -> UserService {
    UserService::new(
        Arc::clone(clock),
        Arc::clone(random),
        Arc::clone(store),
        Arc::clone(passwords),
    )
    .with_reconcile(Arc::clone(&hints.reconcile))
    .with_live_hints(hints.live())
}

/// Each agent's one stream, hinting `reconcile`, and the agents' service
/// that ends them: the registry stays empty without `[grpc]`, where the
/// service still disables agents and no stream ever lives.
fn agent_services(
    clock: &Arc<dyn Clock>,
    store: &Arc<dyn Store>,
    hints: &Hints,
) -> (AgentRegistry, AgentService) {
    let agents = AgentRegistry::with_reconcile(Arc::clone(clock), Arc::clone(&hints.reconcile));
    let service = AgentService::new(
        Arc::clone(clock),
        Arc::clone(store),
        Arc::new(agents.clone()),
    )
    .with_live_hints(hints.live())
    .with_reconcile(Arc::clone(&hints.reconcile));
    (agents, service)
}

/// The bots' service over the app's ports, reading `registry`'s streams and
/// the agents' answers to chat on `events`, with a chat line's wait under the
/// request timeout, and hinting `reconcile` after each write; its allowlist
/// comes from [`fleet_services`].
fn bot_service(
    config: &ServerConfig,
    clock: &Arc<dyn Clock>,
    random: &Arc<dyn SecureRandom>,
    store: &Arc<dyn Store>,
    registry: &AgentRegistry,
    events: &EventBus,
    reconcile: Arc<dyn Reconcile>,
) -> BotService {
    BotService::new(
        Arc::clone(clock),
        Arc::clone(random),
        Arc::clone(store),
        Arc::new(registry.clone()),
        events.clone(),
        config.accounts.allow_offline,
    )
    .with_reconcile(reconcile)
    .with_live_hints(Arc::new(events.clone()))
    .with_chat_wait(ChatWait::new(config.http.request_timeout))
}

/// The modes' service over the app's ports, hinting `reconcile` after each
/// real edit so the bots get it; its allowlist comes from [`fleet_services`].
fn mode_service(
    clock: &Arc<dyn Clock>,
    random: &Arc<dyn SecureRandom>,
    store: &Arc<dyn Store>,
    hints: &Hints,
) -> ModeService {
    ModeService::new(Arc::clone(clock), Arc::clone(random), Arc::clone(store))
        .with_reconcile(Arc::clone(&hints.reconcile))
        .with_live_hints(hints.live())
}

/// What the services tell after their commits: the reconciler, and the live
/// events sockets through the bus (ADR-0020, R36).
struct Hints {
    reconcile: Arc<dyn Reconcile>,
    events: EventBus,
}

impl Hints {
    /// `reconcile`'s and `events`' hints.
    fn new(reconcile: &Arc<dyn Reconcile>, events: &EventBus) -> Self {
        Self {
            reconcile: Arc::clone(reconcile),
            events: events.clone(),
        }
    }

    /// The live events' hints.
    fn live(&self) -> Arc<dyn LiveHints> {
        Arc::new(self.events.clone())
    }
}

/// The 2FA service over the app's ports, sealing TOTP secrets with `vault`
/// and reading the login's `challenges`.
fn mfa_service(
    clock: &Arc<dyn Clock>,
    random: &Arc<dyn SecureRandom>,
    store: &Arc<dyn Store>,
    vault: Arc<dyn Vault>,
    challenges: &Challenges,
) -> MfaService {
    MfaService::new(
        Arc::clone(clock),
        Arc::clone(random),
        Arc::clone(store),
        vault,
        MfaPorts {
            totp: Arc::new(TotpRs::new()),
            recovery: Arc::new(Sha256RecoveryCodes::new()),
            qr: Arc::new(SvgQr::new()),
        },
        challenges.clone(),
    )
}

/// What the control plane's services run on: the app's clock, randomness,
/// store and tokens, the rate limits and the request IDs; and, for the
/// session grants, the vault, Microsoft and serve's shutdown token.
struct EnrollmentPorts {
    clock: Arc<dyn Clock>,
    random: Arc<dyn SecureRandom>,
    store: Arc<dyn Store>,
    tokens: Arc<dyn Tokens>,
    limits: RateLimits,
    ids: RequestIds,
    registry: AgentRegistry,
    vault: Arc<dyn Vault>,
    microsoft: Arc<dyn MicrosoftAuthProvider>,
    shutdown: CancellationToken,
    bus: EventBus,
}

/// The session-token service over `ports`' Microsoft and vault, stopping
/// with a child of serve's shutdown token (ADR-0019, S64), and the session
/// grants over it, an offline account's bot granted only while `[accounts]
/// allow_offline` is on (S59).
fn session_grants(
    config: &ServerConfig,
    ports: &EnrollmentPorts,
) -> (SessionTokenService, Arc<dyn SessionGrants>) {
    let tokens = SessionTokenService::new(
        SessionTokenPorts {
            clock: Arc::clone(&ports.clock),
            store: Arc::clone(&ports.store),
            vault: Arc::clone(&ports.vault),
            microsoft: Arc::clone(&ports.microsoft),
            live_hints: Arc::new(ports.bus.clone()),
        },
        &ports.shutdown,
    );
    let grants = SessionGrantService::new(
        Arc::clone(&ports.clock),
        Arc::clone(&ports.store),
        Arc::new(ports.registry.clone()),
        Arc::new(tokens.clone()),
        config.accounts.allow_offline,
    );
    (tokens, Arc::new(grants))
}

/// The control plane's services over `ports`, while `config` has `[grpc]`
/// and there is a `ca`: the enrollment tokens' for the API, pinning the
/// CA; for the gRPC port, the enrollment, signing with the CA for
/// `[grpc] agent_cert_lifetime_days` within [`SERVER_DEADLINE`],
/// `Connect` with `[grpc]`'s heartbeats, and the reconciler, hinted
/// through `inbox`, placing offline accounts' bots only while `[accounts]
/// allow_offline` is on.
fn control_plane_services(
    config: &ServerConfig,
    ca: Option<CertificateAuthority>,
    inbox: Option<HintInbox>,
    ports: EnrollmentPorts,
) -> (Option<EnrollmentTokenService>, Option<GrpcRoutes>) {
    let Some(((grpc, ca), inbox)) = config.grpc.as_ref().zip(ca).zip(inbox) else {
        return (None, None);
    };
    let reconciler = Reconciler::new(
        ports.registry.clone(),
        Arc::new(AssignmentService::new(
            Arc::clone(&ports.clock),
            Arc::clone(&ports.store),
        )),
        Arc::clone(&ports.clock),
        inbox,
        ReconcileTimings::default(),
        config.accounts.allow_offline,
        config.rate_limits.chat_per_bot,
    )
    .with_live_hints(Arc::new(ports.bus.clone()));
    let tokens = EnrollmentTokenService::new(
        Arc::clone(&ports.clock),
        Arc::clone(&ports.random),
        Arc::clone(&ports.store),
        Arc::clone(&ports.tokens),
        ca.fingerprint(),
    );
    let issuer = AgentIssuer::new(ca, Arc::clone(&ports.random), grpc.agent_cert_lifetime);
    let connections: Arc<dyn AgentConnections> = Arc::new(
        AgentConnectionService::new(
            Arc::clone(&ports.clock),
            Arc::clone(&ports.store),
            Arc::new(X509AgentNames),
        )
        .with_live_hints(Arc::new(ports.bus.clone())),
    );
    let sessions = Sessions::new(CancellationToken::new());
    let (session_tokens, grants) = session_grants(config, &ports);
    let (ingest, ingest_writer) = ingest(
        Arc::clone(&ports.store),
        Arc::new(ports.registry.clone()),
        ports.bus.clone(),
        IngestTimings::default(),
        IngestBounds::default(),
    );
    let control = control_service(
        ControlPorts {
            connections: Arc::clone(&connections),
            grants,
            ingest: Arc::new(ingest),
            clock: Arc::clone(&ports.clock),
        },
        ports.limits.clone(),
        ports.ids.clone(),
        ControlTimings::new(grpc.heartbeats),
        sessions.clone(),
        ports.registry.clone(),
    );
    let enrollment = EnrollmentService::new(
        ports.clock,
        ports.random,
        ports.store,
        ports.tokens,
        Arc::new(issuer),
    );
    (
        Some(tokens),
        Some(GrpcRoutes {
            enrollment: enrollment_service(enrollment, ports.limits, ports.ids, SERVER_DEADLINE),
            control,
            sessions,
            sweep: Sweep {
                registry: ports.registry,
                connections,
            },
            reconciler,
            session_tokens,
            ingest: ingest_writer,
        }),
    )
}

/// The Microsoft link flows and the other account use cases over `ports`,
/// with `config`'s `[rate_limits]` and `[accounts]`, the latter hinting
/// `reconcile`.
fn account_services(
    config: &ServerConfig,
    reconcile: &Arc<dyn Reconcile>,
    ports: LinkFlowPorts,
) -> (LinkFlowService, AccountService) {
    let accounts = AccountService::new(
        Arc::clone(&ports.clock),
        Arc::clone(&ports.random),
        Arc::clone(&ports.store),
        AccountSettings {
            allow_offline: config.accounts.allow_offline,
            max_per_member: config.accounts.max_per_member,
        },
    )
    .with_reconcile(Arc::clone(reconcile))
    .with_live_hints(Arc::clone(&ports.live_hints));
    let link_flows = LinkFlowService::new(
        ports,
        LinkFlowSettings {
            total: config.rate_limits.link_flows.total,
            per_user_per_day: config.rate_limits.link_flows.per_user_per_day,
            max_per_member: config.accounts.max_per_member,
        },
    );
    (link_flows, accounts)
}

/// Opens the database `config` names and serves the API on `listener`, and
/// the gRPC port on the `control_plane`'s while `[grpc]` is configured,
/// until a signal from `signals` stops them. `keys` are the vault's, which
/// main.rs loads before the runtime starts, like the control plane's PKI.
pub async fn run<S: ShutdownSignals>(
    config: &ServerConfig,
    listener: TcpListener,
    control_plane: Option<ControlPlane>,
    signals: S,
    keys: VaultKeys,
) -> Exit {
    let address = match listener.local_addr() {
        Ok(address) => address,
        Err(source) => {
            return cant_start(&StartError::Listen {
                address: config.http.bind,
                source,
            });
        }
    };
    let lockout_key = match keys.derive_key(DerivedKey::LoginAttempts) {
        Ok(key) => key,
        Err(error) => return cant_start(&StartError::VaultKeys(error)),
    };
    let database = match sqlite::open(&config.database, DatabaseOptions::default()).await {
        Ok(database) => database,
        Err(error) => return cant_start(&StartError::Database(error)),
    };
    let passwords: Arc<dyn Passwords> =
        match Argon2Passwords::new(config.auth.argon2, Arc::new(OsRandom)).await {
            Ok(passwords) => Arc::new(passwords),
            Err(error) => return cant_start(&StartError::Passwords(error)),
        };
    warn_about_kept_offline_accounts(config, &database).await;
    let (opened, ca) = if let Some(plane) = control_plane {
        let (opened, ca) = open(plane, SystemClock.now());
        (Some(opened), Some(ca))
    } else {
        info!(
            "the control plane is off: [grpc] isn't configured, so no agent can enroll or connect"
        );
        (None, None)
    };
    let mut app = match app(config, &database, keys, lockout_key, passwords, ca) {
        Ok(app) => app,
        Err(error) => return cant_start(&error),
    };
    let grpc = match grpc_port(config, opened, app.grpc.take()) {
        Ok(grpc) => grpc,
        Err(error) => return cant_start(&error),
    };
    info!(%address, "the server is listening");
    serve(
        listener,
        grpc,
        app,
        &database,
        signals,
        ShutdownTimeouts::of(&config.http),
    )
    .await
}

/// The gRPC port's listener and TLS, and the CA, from `plane`. A server
/// certificate that expired by `now` is logged at `error`: agents can't
/// connect, but the API keeps working (ADR-0019, S12).
///
/// [`run`] calls it before [`app_with`], which takes the CA, and
/// [`grpc_port`] after; tests that build the app over their own ports call
/// both too, so their wiring is serve's (ADR-0019, S72).
#[must_use]
pub fn open(
    plane: ControlPlane,
    now: DateTime<Utc>,
) -> ((TcpListener, ServerTlsConfig), CertificateAuthority) {
    let ControlPlane { listener, pki } = plane;
    if pki.server_not_after <= now {
        error!(
            not_after = %pki.server_not_after,
            "the server certificate has expired: no agent can connect until `afkfleet-server pki server-cert` replaces it"
        );
    }
    let tls = grpc_listener::tls(&pki);
    ((listener, tls), pki.ca)
}

/// The gRPC port over `opened` serving `services`, when `config` has
/// `[grpc]` and both are there: `[grpc]`'s heartbeats, the listener's TLS
/// from [`open`], the services from [`App::grpc`].
///
/// # Errors
/// [`StartError::Listen`] when the listener's address can't be read, and
/// [`StartError::Grpc`] when its TLS can't be set up.
pub fn grpc_port(
    config: &ServerConfig,
    opened: Option<(TcpListener, ServerTlsConfig)>,
    services: Option<GrpcRoutes>,
) -> Result<Option<GrpcPort>, StartError> {
    let (Some(grpc), Some((listener, tls)), Some(services)) =
        (config.grpc.as_ref(), opened, services)
    else {
        return Ok(None);
    };
    let address = listener.local_addr().map_err(|source| StartError::Listen {
        address: grpc.bind,
        source,
    })?;
    let GrpcRoutes {
        enrollment,
        control,
        sessions,
        sweep,
        reconciler,
        session_tokens,
        ingest,
    } = services;
    let router = grpc_listener::router(tls, enrollment, control, grpc.heartbeats)
        .map_err(StartError::Grpc)?;
    info!(%address, "the control plane is listening");
    Ok(Some(GrpcPort {
        listener,
        router,
        sessions,
        sweep,
        reconciler,
        session_tokens,
        ingest,
    }))
}

/// What a start logs while offline accounts are kept with the switch off.
const KEPT_OFFLINE: &str = "offline accounts are kept while [accounts] allow_offline is off; no bot may use them until it's on again";

/// Logs one `warn` when offline accounts exist while `[accounts]
/// allow_offline` is off (P9.7, ADR-0018): they stay untouched, and no bot
/// may run on them. A failed count is a `warn` too, and the start goes on.
async fn warn_about_kept_offline_accounts(config: &ServerConfig, database: &Database) {
    if config.accounts.allow_offline {
        return;
    }
    match database.accounts().count_offline().await {
        Ok(0) => {}
        Ok(offline_accounts) => warn!(offline_accounts, "{KEPT_OFFLINE}"),
        Err(error) => warn!(
            error = %chain(&error),
            "the offline accounts couldn't be counted at startup"
        ),
    }
}

/// Serves `app` on `listener`, every connection with its peer's address,
/// and the `grpc` port if there is one, and runs the housekeeping and the
/// retention, until a signal from `signals` stops them; then shuts down
/// within `timeouts`, waits for both and closes `database`.
pub async fn serve<S: ShutdownSignals>(
    listener: TcpListener,
    grpc: Option<GrpcPort>,
    app: App,
    database: &Database,
    signals: S,
    timeouts: ShutdownTimeouts,
) -> Exit {
    let shutdown = app.shutdown;
    let mut background = JoinSet::new();
    background.spawn(app.housekeeping.run(shutdown.child_token()));
    background.spawn(app.retention.run(shutdown.child_token()));
    let flows_stop = shutdown.child_token();
    let link_flows = app.link_flows;
    background.spawn(async move {
        flows_stop.cancelled().await;
        link_flows.shutdown().await;
    });
    // axum's graceful shutdown doesn't wait for an upgraded connection, so
    // the sockets' owner does, within the drain, before the database closes.
    let sockets = app.sockets;
    let counted = sockets.clone();
    background.spawn(stop_live_sockets(
        shutdown.child_token(),
        async move { sockets.shutdown().await },
        timeouts.drain,
        move || counted.open(),
    ));
    let http = axum::serve(
        listener,
        app.router
            .into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown.clone().cancelled_owned())
    .into_future();
    // Boxed, so the server's future stays as small with the port as without.
    let grpc = grpc.map(|port| {
        // The `Connect` streams end at the shutdown, so tonic's drain isn't
        // held by them, and each records its end before the database closes.
        // The ingest's writer stops once they all have, so no report is
        // admitted after its last batch (S70).
        let sessions_stop = shutdown.child_token();
        let sessions = port.sessions;
        let ingest_stop = CancellationToken::new();
        background.spawn(end_streams_then_ingest(
            sessions_stop,
            async move { sessions.shutdown().await },
            ingest_stop.clone(),
        ));
        background.spawn(port.sweep.registry.sweep_every(
            port.sweep.connections,
            SWEEP_INTERVAL,
            shutdown.child_token(),
        ));
        // Its refreshes in flight get the drain's time, so the database
        // still gets its own (ADR-0019, S64).
        let tokens = port.session_tokens;
        let counted = tokens.clone();
        background.spawn(stop_session_tokens(
            shutdown.child_token(),
            async move { tokens.shutdown().await },
            timeouts.drain,
            move || counted.running(),
        ));
        // Joined below before the database closes, like every task here,
        // and watched: an end before the shutdown stops the server.
        let stop = shutdown.child_token();
        let (watcher, watch) = task_watch();
        spawn_watched(
            &mut background,
            Watched::Reconciler,
            port.reconciler.run(stop.clone()),
            stop,
            watcher.clone(),
        );
        spawn_watched(
            &mut background,
            Watched::Ingest,
            port.ingest.run(ingest_stop.clone()),
            ingest_stop,
            watcher,
        );
        Box::pin(watched(
            grpc_listener::serve(
                port.router,
                port.listener,
                grpc_listener::CONNECTION_AGE,
                shutdown.child_token(),
            ),
            watch,
            shutdown.clone(),
        ))
    });
    let close = async move {
        while let Some(ended) = background.join_next().await {
            if let Err(error) = ended {
                error!(%error, "a background task failed");
            }
        }
        database.close().await;
    };
    drive(
        both(http, grpc, &shutdown),
        &shutdown,
        close,
        signals,
        timeouts,
    )
    .await
}

/// Waits for `stop`, then for `streams_ended`, the end of every `Connect`
/// stream, and only then cancels `ingest_stop`, the ingest's writer's stop
/// (P10.8; ADR-0019, S70): a stream admits reports until it has ended, so the
/// writer's final flush comes after the last of them. Generic, so tests
/// drive it on paused time.
pub async fn end_streams_then_ingest<F>(
    stop: CancellationToken,
    streams_ended: F,
    ingest_stop: CancellationToken,
) where
    F: Future<Output = ()>,
{
    stop.cancelled().await;
    streams_ended.await;
    ingest_stop.cancel();
}

/// What serve logs when the session-token service's refreshes outlast the
/// drain.
pub const SESSION_TOKENS_LEFT: &str = "session-token refreshes were still running when the database had to close; the server stops waiting, and a refresh token one of them rotated but hadn't stored is lost, as in a crash";

/// Waits for `stop`, then for `stopped`, the session-token service's
/// shutdown, at most `bound` (ADR-0019, S64): no refresh starts after the
/// signal, and those in flight get the drain's time, so the database still
/// gets its own. Refreshes still running then are left with one `warn`
/// naming how many (`running`); they end with the runtime. Generic, so
/// tests drive it on paused time.
pub async fn stop_session_tokens<F, R>(
    stop: CancellationToken,
    stopped: F,
    bound: Duration,
    running: R,
) where
    F: Future<Output = ()>,
    R: Fn() -> usize,
{
    stop.cancelled().await;
    // Dropping the wait is cancel-safe: the refreshes stay with their
    // tracker, and end with the runtime.
    if timeout(bound, stopped).await.is_err() {
        warn!(flights = running(), "{SESSION_TOKENS_LEFT}");
    }
}

/// What serve logs when live events sockets outlast the drain.
pub const LIVE_SOCKETS_LEFT: &str = "live events sockets were still open when the database had to close; the server stops waiting, and they end with the process";

/// Waits for `stop`, then for `stopped`, the live events sockets' shutdown,
/// at most `bound` (P11.5; ADR-0020, S35, R20): each socket ends within two
/// close bounds of the stop (a stuck send gets one to finish, else the
/// socket is dropped; then its 1001 gets one). Sockets still open then are
/// left with one
/// `warn` naming how many (`open`); they end with the runtime. Generic, so
/// tests drive it on paused time.
pub async fn stop_live_sockets<F, R>(stop: CancellationToken, stopped: F, bound: Duration, open: R)
where
    F: Future<Output = ()>,
    R: Fn() -> usize,
{
    stop.cancelled().await;
    // Dropping the wait is cancel-safe: the sockets stay with their tracker,
    // and end with the runtime.
    if timeout(bound, stopped).await.is_err() {
        warn!(sockets = open(), "{LIVE_SOCKETS_LEFT}");
    }
}

/// A background task serve watches: if it ends or panics before its stop
/// was asked, the server stops as one that ended unasked (exit 4); nothing
/// restarts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Watched {
    /// The reconciler (P10.5; PR #52's review).
    Reconciler,
    /// The ingest's batch writer (P10.8, S70): without it, every agent's
    /// chat would be dropped unseen.
    Ingest,
}

impl Watched {
    /// The error [`watched`] ends with when this task ended unasked.
    #[must_use]
    pub const fn ended(self) -> &'static str {
        match self {
            Self::Reconciler => RECONCILER_ENDED,
            Self::Ingest => INGEST_ENDED,
        }
    }
}

/// What [`watched`] waits on: the first watched task that ended unasked.
#[derive(Debug)]
pub struct TaskWatch(mpsc::Receiver<Watched>);

/// What a watched task's watcher tells its [`TaskWatch`] with; clones share
/// it.
#[derive(Debug, Clone)]
pub struct TaskWatcher(mpsc::Sender<Watched>);

/// A watch over the tasks given its watcher: [`spawn_watched`] hands each a
/// clone.
#[must_use]
pub fn task_watch() -> (TaskWatcher, TaskWatch) {
    // One slot per kind of task: each tells at most once.
    let (watcher, watch) = mpsc::channel(2);
    (TaskWatcher(watcher), TaskWatch(watch))
}

/// Runs `task`, the `what`, on `tasks`, which serve joins before the
/// database closes, watched: if it ends or panics before `stop` is
/// cancelled, that's logged at `error` and `watcher` tells [`watched`].
/// Nothing restarts it.
pub fn spawn_watched<T>(
    tasks: &mut JoinSet<()>,
    what: Watched,
    task: T,
    stop: CancellationToken,
    watcher: TaskWatcher,
) where
    T: Future<Output = ()> + Send + 'static,
{
    tasks.spawn(async move {
        // Its own owner, so a panic comes back as a `JoinError` here.
        let mut own = JoinSet::new();
        own.spawn(task);
        let outcome = own.join_next().await;
        let asked = stop.is_cancelled();
        match (what, outcome) {
            (Watched::Reconciler, Some(Err(failure))) if asked => {
                error!(error = %failure, "the reconciler failed while the server stopped");
            }
            (Watched::Reconciler, Some(Err(failure))) => {
                error!(error = %failure, "the reconciler failed; the server stops");
            }
            (Watched::Ingest, Some(Err(failure))) if asked => {
                error!(error = %failure, "the ingest's writer failed while the server stopped");
            }
            (Watched::Ingest, Some(Err(failure))) => {
                error!(error = %failure, "the ingest's writer failed; the server stops");
            }
            (_, Some(Ok(())) | None) if asked => return,
            (Watched::Reconciler, Some(Ok(())) | None) => {
                error!("the reconciler ended unasked; the server stops");
            }
            (Watched::Ingest, Some(Ok(())) | None) => {
                error!("the ingest's writer ended unasked; the server stops");
            }
        }
        if !asked {
            // A full watch already tells of an earlier end, and a watch
            // that's gone belongs to a server that stopped: either way,
            // nothing more to tell.
            let _ = watcher.0.try_send(what);
        }
    });
}

/// `port` until it ends, or until `watch` says a watched task ended
/// unasked: then `shutdown` is cancelled, `port` drains, and the answer is
/// an error, so serve ends like a server that ended unasked (exit 4).
///
/// # Errors
/// `port`'s error, or the watched task's [`Watched::ended`].
pub async fn watched<F, E>(
    port: F,
    mut watch: TaskWatch,
    shutdown: CancellationToken,
) -> io::Result<()>
where
    F: Future<Output = Result<(), E>>,
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let mut port = pin!(async { port.await.map_err(io::Error::other) });
    // A watch whose watchers are all gone had tasks that stopped as asked.
    let ended = async {
        match watch.0.recv().await {
            Some(what) => what,
            None => core::future::pending().await,
        }
    };
    tokio::select! {
        // The pinned port is polled to its end here or below, and `ended`
        // only once: cancel-safe.
        result = &mut port => result,
        what = ended => {
            shutdown.cancel();
            // The port drains; its own end doesn't matter any more.
            let _ = port.await;
            Err(io::Error::other(what.ended()))
        }
    }
}

/// The error [`watched`] ends with when the reconciler ended unasked.
pub const RECONCILER_ENDED: &str = "the reconciler ended unasked";

/// The error [`watched`] ends with when the ingest's writer ended unasked.
pub const INGEST_ENDED: &str = "the ingest's writer ended unasked";

/// `http` and, if there is one, `grpc`, as the one server [`drive`] runs:
/// when either ends, it cancels `shutdown`, so the other drains too, and it
/// ends once both have, with the first error. A gRPC port that fails
/// before any signal is thus a server that ended unasked.
///
/// # Errors
/// The first of the two servers' errors, the gRPC port's as an
/// [`io::Error`].
pub async fn both<H, G, E>(http: H, grpc: Option<G>, shutdown: &CancellationToken) -> io::Result<()>
where
    H: Future<Output = io::Result<()>>,
    G: Future<Output = Result<(), E>>,
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let Some(grpc) = grpc else {
        return http.await;
    };
    let mut http = pin!(http);
    let mut grpc = pin!(async { grpc.await.map_err(io::Error::other) });
    tokio::select! {
        // Both branches are pinned futures, each polled to its end, here
        // or right after: cancel-safe.
        ended = &mut http => {
            shutdown.cancel();
            let other = grpc.await;
            ended.and(other)
        }
        ended = &mut grpc => {
            shutdown.cancel();
            let other = http.await;
            other.and(ended)
        }
    }
}

/// The shutdown sequence of the [module docs](self): runs `server` until a
/// signal, cancels `shutdown` (which asks the server to drain), waits for
/// the drain up to its limit, then runs `close` up to the deadline. It's
/// generic over both futures, so tests drive it on paused time.
///
/// If `server` ends before any signal, that's an error: `shutdown` is
/// cancelled anyway and `close` gets [`ShutdownTimeouts::close`].
pub async fn drive<S, F, C>(
    server: F,
    shutdown: &CancellationToken,
    close: C,
    mut signals: S,
    timeouts: ShutdownTimeouts,
) -> Exit
where
    S: ShutdownSignals,
    F: Future<Output = io::Result<()>>,
    C: Future<Output = ()>,
{
    let mut server = pin!(server);
    let asked = tokio::select! {
        biased;
        // Polling the pinned future again later is fine: this branch only
        // completes once.
        ended = &mut server => {
            match ended {
                Ok(()) => error!("the server ended unasked"),
                Err(error) => error!(%error, "the server ended unasked"),
            }
            false
        }
        // Cancel-safe: `ShutdownSignals::recv` loses no signal.
        signal = signals.recv() => {
            info!(signal = signal.name(), "shutting down");
            true
        }
    };
    // Also ends axum's own task that waits for the signal.
    shutdown.cancel();
    let start = Instant::now();
    if !asked {
        close_by(close, start + timeouts.close, &mut signals).await;
        return Exit::ServerFailed;
    }
    let mut drain_limit = pin!(sleep_until(start + timeouts.drain));
    loop {
        tokio::select! {
            biased;
            // The pinned server future, polled until it ends: cancel-safe.
            _drained = &mut server => break,
            () = &mut drain_limit => {
                warn!("connections were still open at the drain limit; the server stops waiting for them");
                break;
            }
            // Cancel-safe, as above.
            signal = signals.recv() => info!(signal = signal.name(), "already shutting down"),
        }
    }
    if close_by(close, start + timeouts.deadline(), &mut signals).await {
        Exit::Stopped
    } else {
        Exit::ServerFailed
    }
}

/// Runs `close` until `deadline`, logging later signals; whether it
/// finished in time.
async fn close_by<S, C>(close: C, deadline: Instant, signals: &mut S) -> bool
where
    S: ShutdownSignals,
    C: Future<Output = ()>,
{
    let mut close = pin!(close);
    let mut limit = pin!(sleep_until(deadline));
    loop {
        tokio::select! {
            biased;
            // The pinned close future, polled until it ends: cancel-safe.
            () = &mut close => return true,
            () = &mut limit => {
                error!("the database didn't close by the shutdown deadline");
                return false;
            }
            // Cancel-safe: `ShutdownSignals::recv` loses no signal.
            signal = signals.recv() => info!(signal = signal.name(), "already shutting down"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::rate_limit::TrustedProxies;

    #[test]
    fn each_exit_has_its_code() {
        assert_eq!(Exit::Stopped.code(), 0);
        assert_eq!(Exit::StartupFailed.code(), 1);
        assert_eq!(Exit::ServerFailed.code(), 4);
    }

    #[test]
    fn the_last_line_carries_the_exit_code() {
        use fleet_startup::config::{LogConfig, LogFormat};
        use fleet_startup::telemetry::{NoRules, layer_with};
        use fleet_testkit::log_buffer::LogBuffer;
        use tracing_subscriber::layer::SubscriberExt as _;

        let logs = LogBuffer::default();
        let config = LogConfig {
            format: LogFormat::Json,
            filter: "info".parse().unwrap(),
        };
        let subscriber =
            tracing_subscriber::registry().with(layer_with(&config, &NoRules, false, logs.clone()));
        let _guard = tracing::subscriber::set_default(subscriber);

        Exit::ServerFailed.log();

        let lines = logs.lines_with("the server stopped").unwrap();
        assert_eq!(lines.len(), 1, "{}", logs.text());
        assert_eq!(lines[0]["level"], "INFO");
        assert_eq!(lines[0]["exit_code"], 4);
    }

    #[test]
    fn a_microsoft_client_that_cant_be_built_stops_the_start_with_its_cause() {
        let error =
            StartError::Microsoft(BuildError::caused_by(io::Error::other("no TLS backend")));

        assert_eq!(
            error.to_string(),
            "the HTTP client for Microsoft can't be built"
        );
        assert_eq!(
            chain(&error),
            "the HTTP client for Microsoft can't be built: no TLS backend"
        );
        assert_eq!(cant_start(&error), Exit::StartupFailed);
    }

    #[test]
    fn a_crypto_provider_installed_first_has_its_reason() {
        assert_eq!(
            StartError::CryptoProvider.to_string(),
            "a TLS crypto provider is already installed"
        );
    }

    #[test]
    fn the_drain_gets_the_request_timeout_and_the_close_at_least_5_s() {
        let http = HttpConfig {
            bind: HttpConfig::DEFAULT_BIND,
            request_timeout: Duration::from_secs(15),
            max_body_bytes: 65_536,
            trusted_proxies: TrustedProxies::default(),
        };

        let timeouts = ShutdownTimeouts::of(&http);

        assert_eq!(timeouts.drain, Duration::from_secs(15));
        assert_eq!(timeouts.close, Duration::from_secs(5));
        assert_eq!(timeouts.deadline(), Duration::from_secs(20));
    }

    /// serve's retention keeps what the config's three keys say, cuts
    /// sessions by the config's absolute lifetime, and runs at S45's pace.
    #[tokio::test]
    async fn the_retention_takes_the_configs_keys_and_lifetimes_at_the_default_pace() {
        use fleet_core::auth::SessionPolicy;
        use fleet_core::retention::{AuditRetention, ChatBound, ChatRetention};
        use fleet_startup::config::LogConfig;
        use fleet_testkit::system::ManualClock;

        use crate::config::{
            AccountsConfig, AuditConfig, AuthConfig, ChatConfig, DatabaseConfig, RateLimitConfig,
            VaultConfig,
        };
        use crate::http::rate_limit::TrustedProxies;
        use crate::infra::sqlite::{Database, DatabaseOptions};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("afkfleet.db");
        std::fs::File::create(&path).unwrap();
        let database = Database::connect(
            sqlx::sqlite::SqliteConnectOptions::new().filename(&path),
            DatabaseOptions::default(),
        )
        .await
        .unwrap();
        let sessions = SessionPolicy::try_new(
            Duration::from_mins(5),
            Duration::from_hours(48),
            Duration::from_hours(240),
        )
        .unwrap();
        let config = ServerConfig {
            dev_mode: false,
            http: HttpConfig {
                bind: HttpConfig::DEFAULT_BIND,
                request_timeout: Duration::from_secs(15),
                max_body_bytes: 65_536,
                trusted_proxies: TrustedProxies::default(),
            },
            database: DatabaseConfig { path },
            log: LogConfig::default(),
            auth: AuthConfig {
                sessions,
                ..AuthConfig::default()
            },
            rate_limits: RateLimitConfig::default(),
            vault: VaultConfig {
                key_files: std::collections::BTreeMap::new(),
                active_key_id: 1,
            },
            accounts: AccountsConfig::default(),
            chat: ChatConfig {
                retention: ChatRetention::try_new(3).unwrap(),
                bound: ChatBound::try_new(500).unwrap(),
                ..ChatConfig::default()
            },
            audit: AuditConfig {
                retention: AuditRetention::try_new(120).unwrap(),
            },
            grpc: None,
        };
        let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(chrono::DateTime::UNIX_EPOCH));
        let store: Arc<dyn Store> = Arc::new(database.clone());

        let built = retention(&config, &clock, &store);

        assert_eq!(built.service().policy(), config.retention());
        assert_eq!(built.service().policy().bound.lines(), 500);
        assert_eq!(built.service().sessions(), sessions);
        assert_eq!(built.timings(), RetentionTimings::default());
        assert_eq!(built.service().pause(), RetentionTimings::default().pause);
        drop(built);
        database.close().await;
    }

    /// The bots' and the modes' services judge commands by `[chat]`'s one
    /// allowlist, so a mode's `unlisted_command` is what the mode switch
    /// authorizes with (S68, R113).
    #[tokio::test]
    async fn the_bots_and_the_modes_judge_commands_by_the_configs_one_allowlist() {
        use fleet_core::authz::CommandAllowlist;
        use fleet_core::chat::ChatMessage;
        use fleet_core::mode::ModeDefinition;
        use fleet_testkit::system::{ManualClock, SeededRandom};

        use fleet_startup::config::LogConfig;

        use crate::config::{
            AccountsConfig, AuditConfig, AuthConfig, ChatConfig, DatabaseConfig, RateLimitConfig,
            VaultConfig,
        };
        use crate::infra::sqlite::{Database, DatabaseOptions};
        use crate::ports::reconcile::NoReconcile;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("afkfleet.db");
        std::fs::File::create(&path).unwrap();
        let database = Database::connect(
            sqlx::sqlite::SqliteConnectOptions::new().filename(&path),
            DatabaseOptions::default(),
        )
        .await
        .unwrap();
        let config = ServerConfig {
            dev_mode: false,
            http: HttpConfig {
                bind: HttpConfig::DEFAULT_BIND,
                request_timeout: Duration::from_secs(15),
                max_body_bytes: 65_536,
                trusted_proxies: TrustedProxies::default(),
            },
            database: DatabaseConfig { path },
            log: LogConfig::default(),
            auth: AuthConfig::default(),
            rate_limits: RateLimitConfig::default(),
            vault: VaultConfig {
                key_files: std::collections::BTreeMap::new(),
                active_key_id: 1,
            },
            accounts: AccountsConfig::default(),
            chat: ChatConfig {
                command_allowlist: CommandAllowlist::try_new(&["/spawn"]).unwrap(),
                ..ChatConfig::default()
            },
            audit: AuditConfig::default(),
            grpc: None,
        };
        let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(chrono::DateTime::UNIX_EPOCH));
        let random: Arc<dyn SecureRandom> = Arc::new(SeededRandom::new(7));
        let store: Arc<dyn Store> = Arc::new(database.clone());
        let events = EventBus::new();
        let reconcile: Arc<dyn Reconcile> = Arc::new(NoReconcile);
        let hints = Hints::new(&reconcile, &events);
        let shared = Shared {
            clock: &clock,
            random: &random,
            store: &store,
        };

        let services = fleet_services(&config, shared, &hints, &events, reconcile);

        for (line, needs_manage) in [("/spawn home", false), ("/op AfkBot2", true)] {
            let message = ChatMessage::try_from(line).unwrap();
            let definition: ModeDefinition = serde_json::from_value(serde_json::json!({ "steps": [
                { "action": { "type": "send_chat", "message": line }, "schedule": { "type": "at_start" }, "probability": 100 }
            ] }))
            .unwrap();
            assert_eq!(
                services.bots.command_check(&message).needs_manage(),
                needs_manage,
                "{line}"
            );
            assert_eq!(
                services.modes.command_check(&definition).needs_manage(),
                needs_manage,
                "{line}"
            );
        }
        drop(services);
        database.close().await;
    }
}
