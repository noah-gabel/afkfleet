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
//! - **The housekeeping** is the server's one long-lived background task,
//!   owned by [`serve`]'s `JoinSet`, and stops at the first signal through a
//!   child of the shutdown token.
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
//!    purge batch), the link flows (each stopped at the signal, or after a
//!    commit that started) and the reconciler (after its current pass),
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
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;
use tonic::transport::ServerTlsConfig;
use tracing::{error, info, warn};

use super::DATABASE_CLOSE;
use super::housekeeping::Housekeeping;
use crate::app::accounts::{AccountService, AccountSettings};
use crate::app::agents::{AgentConnectionService, AgentService, AssignmentService};
use crate::app::auth::TokenService;
use crate::app::bots::BotService;
use crate::app::challenges::Challenges;
use crate::app::enrollment::{EnrollmentService, EnrollmentTokenService};
use crate::app::grants::GrantService;
use crate::app::health::HealthService;
use crate::app::invites::InviteService;
use crate::app::link_flows::{LinkFlowPorts, LinkFlowService, LinkFlowSettings};
use crate::app::lockout::LockoutService;
use crate::app::login::LoginService;
use crate::app::mfa::{MfaPorts, MfaService};
use crate::app::users::UserService;
use crate::config::{HttpConfig, ServerConfig};
use crate::grpc::control::{ControlRoute, ControlTimings, Sessions, control_service};
use crate::grpc::enrollment::enrollment_service;
use crate::grpc::listener::{self as grpc_listener, EnrollmentRoute};
use crate::grpc::reconciler::{HINT_QUEUE, HintInbox, ReconcileTimings, Reconciler, hints};
use crate::grpc::registry::{AgentRegistry, SWEEP_INTERVAL};
use crate::http::error::chain;
use crate::http::middleware::RequestIds;
use crate::http::openapi::ApiDocs;
use crate::http::rate_limit::RateLimits;
use crate::http::router::{AppState, Services, router};
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
use crate::ports::msauth::MicrosoftAuthProvider;
use crate::ports::pace::Pace;
use crate::ports::passwords::{HashError, Passwords};
use crate::ports::reconcile::{NoReconcile, Reconcile};
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

/// What [`serve`] runs: the router, its periodic housekeeping, and the link
/// flows it stops at the first signal.
#[derive(Debug)]
pub struct App {
    /// Every route behind the middleware stack.
    pub router: Router,
    /// The background task that looks after the rate limits, the lockout,
    /// the login's challenges and the finished link flows.
    pub housekeeping: Housekeeping,
    /// The Microsoft link flows, whose pollers run until the shutdown.
    pub link_flows: LinkFlowService,
    /// The gRPC port's services, while the control plane is on: [`run`]
    /// serves them on the gRPC port.
    pub grpc: Option<GrpcRoutes>,
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
    let vault: Arc<dyn Vault> = Arc::new(XChaChaVault::new(keys, Arc::clone(&random)));
    let (reconcile, inbox) = hint_queue(config.grpc.is_some() && ca.is_some());
    let (link_flows, accounts) = account_services(
        config,
        &reconcile,
        LinkFlowPorts {
            clock: Arc::clone(&clock),
            random: Arc::clone(&random),
            store: Arc::clone(&store),
            vault: Arc::clone(&vault),
            microsoft,
            pace,
        },
    );
    let token_port: Arc<dyn Tokens> = Arc::new(Sha256Tokens::new(Arc::clone(&random)));
    let tokens = TokenService::new(
        Arc::clone(&clock),
        Arc::clone(&random),
        Arc::clone(&store),
        Arc::clone(&token_port),
        config.auth.sessions,
    );
    let invites = InviteService::new(
        Arc::clone(&clock),
        Arc::clone(&random),
        Arc::clone(&store),
        Arc::clone(&token_port),
        config.auth.invites,
    );
    let challenges = Challenges::new(Arc::clone(&clock), Arc::clone(&token_port));
    let mfa = mfa_service(&clock, &random, &store, vault, &challenges);
    let limits = RateLimits::new(&config.rate_limits, limiter_clock);
    let lockout = LockoutService::new(
        Arc::clone(&clock),
        Arc::clone(&store),
        Arc::new(HmacAttemptKeys::new(lockout_key)),
        config.auth.lockout,
    );
    let users = user_service(&clock, &random, &store, &passwords, &reconcile);
    let login = LoginService::new(
        Arc::clone(&clock),
        Arc::clone(&random),
        Arc::clone(&store),
        passwords,
        lockout.clone(),
        tokens.clone(),
        mfa.clone(),
    );
    let grants = GrantService::new(Arc::clone(&clock), Arc::clone(&store));
    let ids = RequestIds::new(Arc::clone(&clock), Arc::clone(&random));
    let (agents, agent_service) = agent_services(&clock, &store, Arc::clone(&reconcile));
    let bots = bot_service(config, &clock, &store, &agents, reconcile);
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
            registry: agents.clone(),
        },
    );
    let state = AppState::new(Services {
        health: HealthService::new(store),
        tokens,
        limits: limits.clone(),
        mfa,
        login,
        users,
        invites,
        link_flows: link_flows.clone(),
        accounts,
        grants,
        enrollment_tokens,
        agents: agent_service,
        bots,
    });
    App {
        router: router(
            state,
            &config.http,
            ids,
            ApiDocs::for_dev_mode(config.dev_mode),
        ),
        housekeeping: Housekeeping::new(limits, lockout, challenges, link_flows.flows(), agents),
        link_flows,
        grpc,
    }
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
    reconcile: &Arc<dyn Reconcile>,
) -> UserService {
    UserService::new(
        Arc::clone(clock),
        Arc::clone(random),
        Arc::clone(store),
        Arc::clone(passwords),
    )
    .with_reconcile(Arc::clone(reconcile))
}

/// Each agent's one stream, hinting `reconcile`, and the agents' service
/// that ends them: the registry stays empty without `[grpc]`, where the
/// service still disables agents and no stream ever lives.
fn agent_services(
    clock: &Arc<dyn Clock>,
    store: &Arc<dyn Store>,
    reconcile: Arc<dyn Reconcile>,
) -> (AgentRegistry, AgentService) {
    let agents = AgentRegistry::with_reconcile(Arc::clone(clock), reconcile);
    let service = AgentService::new(
        Arc::clone(clock),
        Arc::clone(store),
        Arc::new(agents.clone()),
    );
    (agents, service)
}

/// The bots' service over the app's ports, reading `registry`'s streams,
/// with `[chat]`'s allowlist, and hinting `reconcile` after each write.
fn bot_service(
    config: &ServerConfig,
    clock: &Arc<dyn Clock>,
    store: &Arc<dyn Store>,
    registry: &AgentRegistry,
    reconcile: Arc<dyn Reconcile>,
) -> BotService {
    BotService::new(
        Arc::clone(clock),
        Arc::clone(store),
        Arc::new(registry.clone()),
        config.accounts.allow_offline,
    )
    .with_allowlist(config.chat.command_allowlist.clone())
    .with_reconcile(reconcile)
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
/// store and tokens, the rate limits and the request IDs.
struct EnrollmentPorts {
    clock: Arc<dyn Clock>,
    random: Arc<dyn SecureRandom>,
    store: Arc<dyn Store>,
    tokens: Arc<dyn Tokens>,
    limits: RateLimits,
    ids: RequestIds,
    registry: AgentRegistry,
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
    );
    let tokens = EnrollmentTokenService::new(
        Arc::clone(&ports.clock),
        Arc::clone(&ports.random),
        Arc::clone(&ports.store),
        Arc::clone(&ports.tokens),
        ca.fingerprint(),
    );
    let issuer = AgentIssuer::new(ca, Arc::clone(&ports.random), grpc.agent_cert_lifetime);
    let connections: Arc<dyn AgentConnections> = Arc::new(AgentConnectionService::new(
        Arc::clone(&ports.clock),
        Arc::clone(&ports.store),
        Arc::new(X509AgentNames),
    ));
    let sessions = Sessions::new(CancellationToken::new());
    let control = control_service(
        Arc::clone(&connections),
        Arc::clone(&ports.clock),
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
    .with_reconcile(Arc::clone(reconcile));
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
fn open(
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
/// `[grpc]` and both are there.
///
/// # Errors
/// [`StartError::Listen`] when the listener's address can't be read, and
/// [`StartError::Grpc`] when its TLS can't be set up.
fn grpc_port(
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
/// and the `grpc` port if there is one, and runs the housekeeping, until a
/// signal from `signals` stops them; then shuts down within `timeouts`,
/// waits for the housekeeping and closes `database`.
pub async fn serve<S: ShutdownSignals>(
    listener: TcpListener,
    grpc: Option<GrpcPort>,
    app: App,
    database: &Database,
    signals: S,
    timeouts: ShutdownTimeouts,
) -> Exit {
    let shutdown = CancellationToken::new();
    let mut background = JoinSet::new();
    background.spawn(app.housekeeping.run(shutdown.child_token()));
    let flows_stop = shutdown.child_token();
    let link_flows = app.link_flows;
    background.spawn(async move {
        flows_stop.cancelled().await;
        link_flows.shutdown().await;
    });
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
        let sessions_stop = shutdown.child_token();
        let sessions = port.sessions;
        background.spawn(async move {
            sessions_stop.cancelled().await;
            sessions.shutdown().await;
        });
        background.spawn(port.sweep.registry.sweep_every(
            port.sweep.connections,
            SWEEP_INTERVAL,
            shutdown.child_token(),
        ));
        // Joined below before the database closes, like every task here,
        // and watched: an end before the shutdown stops the server.
        let stop = shutdown.child_token();
        let watch = spawn_reconciler(&mut background, port.reconciler.run(stop.clone()), stop);
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

/// Whether the reconciler ended before its stop was asked; see
/// [`spawn_reconciler`].
#[derive(Debug)]
pub struct ReconcilerWatch(oneshot::Receiver<()>);

/// Runs `reconciler` on `tasks`, which serve joins before the database
/// closes, watched: if it ends or panics before `stop` is cancelled, that's
/// logged at `error` and the watch tells [`watched`]. Nothing restarts it.
pub fn spawn_reconciler<R>(
    tasks: &mut JoinSet<()>,
    reconciler: R,
    stop: CancellationToken,
) -> ReconcilerWatch
where
    R: Future<Output = ()> + Send + 'static,
{
    let (ended, watch) = oneshot::channel();
    tasks.spawn(async move {
        // Its own owner, so a panic comes back as a `JoinError` here.
        let mut task = JoinSet::new();
        task.spawn(reconciler);
        let outcome = task.join_next().await;
        let asked = stop.is_cancelled();
        match outcome {
            Some(Err(failure)) if asked => {
                error!(error = %failure, "the reconciler failed while the server stopped");
            }
            Some(Err(failure)) => {
                error!(error = %failure, "the reconciler failed; the server stops");
            }
            Some(Ok(())) | None if asked => return,
            Some(Ok(())) | None => error!("the reconciler ended unasked; the server stops"),
        }
        if !asked {
            // The server may have stopped meanwhile: nobody to tell.
            let _ = ended.send(());
        }
    });
    ReconcilerWatch(watch)
}

/// `port` until it ends, or until `watch` says the reconciler ended
/// unasked: then `shutdown` is cancelled, `port` drains, and the answer is
/// an error, so serve ends like a server that ended unasked (exit 4).
///
/// # Errors
/// `port`'s error, or [`RECONCILER_ENDED`].
pub async fn watched<F, E>(
    port: F,
    watch: ReconcilerWatch,
    shutdown: CancellationToken,
) -> io::Result<()>
where
    F: Future<Output = Result<(), E>>,
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let mut port = pin!(async { port.await.map_err(io::Error::other) });
    // A watch whose sender is gone was a reconciler that stopped as asked.
    let ended = async {
        if watch.0.await.is_err() {
            core::future::pending::<()>().await;
        }
    };
    tokio::select! {
        // The pinned port is polled to its end here or below, and `ended`
        // only once: cancel-safe.
        result = &mut port => result,
        () = ended => {
            shutdown.cancel();
            // The port drains; its own end doesn't matter any more.
            let _ = port.await;
            Err(io::Error::other(RECONCILER_ENDED))
        }
    }
}

/// The error [`watched`] ends with when the reconciler ended unasked.
pub const RECONCILER_ENDED: &str = "the reconciler ended unasked";

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
}
