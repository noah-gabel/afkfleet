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
//!    purge batch), and the link flows (each stopped at the signal, or after
//!    a commit that started), then the database closes, all with the rest of
//!    the deadline, at least [`DATABASE_CLOSE`]. If that doesn't finish in
//!    time, that's an `error` and exit 4.
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
use fleet_core::system::{Clock, SecureRandom};
use fleet_startup::signals::ShutdownSignals;
use secrecy::SecretBox;
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use super::DATABASE_CLOSE;
use super::housekeeping::Housekeeping;
use crate::app::auth::TokenService;
use crate::app::challenges::Challenges;
use crate::app::health::HealthService;
use crate::app::invites::InviteService;
use crate::app::link_flows::{LinkFlowPorts, LinkFlowService, LinkFlowSettings};
use crate::app::lockout::LockoutService;
use crate::app::login::LoginService;
use crate::app::mfa::{MfaPorts, MfaService};
use crate::app::users::UserService;
use crate::config::{HttpConfig, ServerConfig};
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
use crate::infra::qr::SvgQr;
use crate::infra::sqlite::{self, Database, DatabaseOptions, OpenError};
use crate::infra::system::{MonotonicClock, OsRandom, SystemClock, TokioPace};
use crate::ports::msauth::MicrosoftAuthProvider;
use crate::ports::pace::Pace;
use crate::ports::passwords::{HashError, Passwords};
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
) -> Result<App, StartError> {
    let ports = ServerPorts::system().map_err(StartError::Microsoft)?;
    Ok(app_with(
        config,
        database,
        keys,
        lockout_key,
        passwords,
        ports,
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
#[must_use]
pub fn app_with(
    config: &ServerConfig,
    database: &Database,
    keys: VaultKeys,
    lockout_key: SecretBox<[u8; 32]>,
    passwords: Arc<dyn Passwords>,
    ports: ServerPorts,
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
    let link_flows = LinkFlowService::new(
        LinkFlowPorts {
            clock: Arc::clone(&clock),
            random: Arc::clone(&random),
            store: Arc::clone(&store),
            vault: Arc::clone(&vault),
            microsoft,
            pace,
        },
        LinkFlowSettings {
            total: config.rate_limits.link_flows.total,
            per_user_per_day: config.rate_limits.link_flows.per_user_per_day,
            max_per_member: config.accounts.max_per_member,
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
    let challenges = Challenges::new(Arc::clone(&clock), token_port);
    let mfa = MfaService::new(
        Arc::clone(&clock),
        Arc::clone(&random),
        Arc::clone(&store),
        vault,
        MfaPorts {
            totp: Arc::new(TotpRs::new()),
            recovery: Arc::new(Sha256RecoveryCodes::new()),
            qr: Arc::new(SvgQr::new()),
        },
        challenges.clone(),
    );
    let limits = RateLimits::new(&config.rate_limits, limiter_clock);
    let lockout = LockoutService::new(
        Arc::clone(&clock),
        Arc::clone(&store),
        Arc::new(HmacAttemptKeys::new(lockout_key)),
        config.auth.lockout,
    );
    let users = UserService::new(
        Arc::clone(&clock),
        Arc::clone(&random),
        Arc::clone(&store),
        Arc::clone(&passwords),
    );
    let login = LoginService::new(
        Arc::clone(&clock),
        Arc::clone(&random),
        Arc::clone(&store),
        passwords,
        lockout.clone(),
        tokens.clone(),
        mfa.clone(),
    );
    let ids = RequestIds::new(clock, random);
    let state = AppState::new(Services {
        health: HealthService::new(store),
        tokens,
        limits: limits.clone(),
        mfa,
        login,
        users,
        invites,
        link_flows: link_flows.clone(),
    });
    App {
        router: router(
            state,
            &config.http,
            ids,
            ApiDocs::for_dev_mode(config.dev_mode),
        ),
        housekeeping: Housekeeping::new(limits, lockout, challenges, link_flows.flows()),
        link_flows,
    }
}

/// Opens the database `config` names and serves the API on `listener`
/// until a signal from `signals` stops it. `keys` are the vault's, which
/// main.rs loads before the runtime starts.
pub async fn run<S: ShutdownSignals>(
    config: &ServerConfig,
    listener: TcpListener,
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
    let app = match app(config, &database, keys, lockout_key, passwords) {
        Ok(app) => app,
        Err(error) => return cant_start(&error),
    };
    info!(%address, "the server is listening");
    serve(
        listener,
        app,
        &database,
        signals,
        ShutdownTimeouts::of(&config.http),
    )
    .await
}

/// Serves `app` on `listener`, every connection with its peer's address,
/// and runs its housekeeping, until a signal from `signals` stops both; then
/// shuts down within `timeouts`, waits for the housekeeping and closes
/// `database`.
pub async fn serve<S: ShutdownSignals>(
    listener: TcpListener,
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
    let server = axum::serve(
        listener,
        app.router
            .into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown.clone().cancelled_owned())
    .into_future();
    let close = async move {
        while let Some(ended) = background.join_next().await {
            if let Err(error) = ended {
                error!(%error, "a background task failed");
            }
        }
        database.close().await;
    };
    drive(server, &shutdown, close, signals, timeouts).await
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
