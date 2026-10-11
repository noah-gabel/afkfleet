//! `afkfleet-server serve` (Plan.md P6.11, P6.12, ADR-0015): the HTTP API,
//! until a signal stops it.
//!
//! main.rs loads the config, starts logging, logs the dev-mode warning,
//! loads the vault's keys (plain file reads, before any runtime exists),
//! builds a multi-threaded runtime, installs the stop signals and binds
//! `[http] bind`; [`run`] opens the database (running every pending
//! migration), builds the router over the real clock, the OS's randomness,
//! the vault and the database, and serves. Every startup failure is one `error` event,
//! "the server can't start", with the error's whole chain, and exit 1. The
//! last line, "the server stopped", carries the exit code.
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
//! 3. The database closes with the rest of the deadline, at least
//!    [`DATABASE_CLOSE`]. If it doesn't close in time, that's an `error` and
//!    exit 4.
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
use tokio::net::TcpListener;
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use super::DATABASE_CLOSE;
use crate::app::auth::TokenService;
use crate::app::health::HealthService;
use crate::config::{HttpConfig, ServerConfig};
use crate::http::error::chain;
use crate::http::middleware::RequestIds;
use crate::http::openapi::ApiDocs;
use crate::http::router::{AppState, router};
use crate::infra::crypto::tokens::Sha256Tokens;
use crate::infra::crypto::vault::{VaultKeyError, VaultKeys, XChaChaVault};
use crate::infra::sqlite::{self, Database, DatabaseOptions, OpenError};
use crate::infra::system::{OsRandom, SystemClock};

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
}

/// Logs `error` with its whole chain as "the server can't start" and
/// returns [`Exit::StartupFailed`].
pub fn cant_start(error: &StartError) -> Exit {
    error!(error = %chain(error), "the server can't start");
    Exit::StartupFailed
}

/// The router `serve` runs: the routes over `database`, sessions with
/// `[auth]`'s lifetimes, request IDs and tokens from the real clock and the
/// OS's randomness, and the `OpenAPI` document in dev mode. `serve` and
/// `user create-owner` are the only places the real clock and randomness are
/// built.
///
/// It builds the vault from `keys` too. Nothing reads it until group D's
/// 2FA service, which takes it into the app's state; until then it's built,
/// which proves the keys make one, and dropped, wiping them (ADR-0016).
pub fn app(config: &ServerConfig, database: &Database, keys: VaultKeys) -> Router {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let random: Arc<dyn SecureRandom> = Arc::new(OsRandom);
    let _vault = XChaChaVault::new(keys, Arc::clone(&random));
    let tokens = TokenService::new(
        Arc::clone(&clock),
        Arc::clone(&random),
        Arc::new(database.clone()),
        Arc::new(Sha256Tokens::new(Arc::clone(&random))),
        config.auth.sessions,
    );
    let ids = RequestIds::new(clock, random);
    let state = AppState::new(HealthService::new(Arc::new(database.clone())), tokens);
    router(
        state,
        &config.http,
        ids,
        ApiDocs::for_dev_mode(config.dev_mode),
    )
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
    let database = match sqlite::open(&config.database, DatabaseOptions::default()).await {
        Ok(database) => database,
        Err(error) => return cant_start(&StartError::Database(error)),
    };
    let app = app(config, &database, keys);
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

/// Serves `app` on `listener` until a signal from `signals` stops it, then
/// shuts down within `timeouts` and closes `database`.
pub async fn serve<S: ShutdownSignals>(
    listener: TcpListener,
    app: Router,
    database: &Database,
    signals: S,
    timeouts: ShutdownTimeouts,
) -> Exit {
    let shutdown = CancellationToken::new();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.clone().cancelled_owned())
        .into_future();
    drive(server, &shutdown, database.close(), signals, timeouts).await
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
    fn the_drain_gets_the_request_timeout_and_the_close_at_least_5_s() {
        let http = HttpConfig {
            bind: HttpConfig::DEFAULT_BIND,
            request_timeout: Duration::from_secs(15),
            max_body_bytes: 65_536,
        };

        let timeouts = ShutdownTimeouts::of(&http);

        assert_eq!(timeouts.drain, Duration::from_secs(15));
        assert_eq!(timeouts.close, Duration::from_secs(5));
        assert_eq!(timeouts.deadline(), Duration::from_secs(20));
    }
}
