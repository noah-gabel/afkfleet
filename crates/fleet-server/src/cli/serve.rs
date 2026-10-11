//! `afkfleet-server serve` (Plan.md P6.11, ADR-0015): the HTTP API.
//!
//! main.rs loads the config, starts logging, logs the dev-mode warning,
//! builds a multi-threaded runtime and binds `[http] bind`; [`run`] opens
//! the database (running every pending migration), builds the router over
//! the real clock, the OS's randomness and the database, and serves. Every
//! startup failure is one `error` event, "the server can't start", with the
//! error's whole chain, and exit 1. The last line, "the server stopped",
//! carries the exit code.
//!
//! Tests bind `127.0.0.1:0` themselves and pass the listener to [`run`], so
//! the config's rule against port 0 never needs an exception.

use core::time::Duration;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use tokio::net::TcpListener;
use tracing::{error, info};

use super::DATABASE_CLOSE;
use crate::app::health::HealthService;
use crate::config::ServerConfig;
use crate::http::error::chain;
use crate::http::middleware::RequestIds;
use crate::http::openapi::ApiDocs;
use crate::http::router::{AppState, router};
use crate::infra::sqlite::{self, Database, DatabaseOptions, OpenError};
use crate::infra::system::{OsRandom, SystemClock};

/// How long main.rs's runtime waits for its tasks once [`run`] returned;
/// the connections axum still serves end then.
pub const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(1);

/// How the server's run ended, which decides the process's exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The server couldn't start: the runtime, the address or the database.
    StartupFailed,
    /// The server ended without being asked to.
    ServerFailed,
}

impl Exit {
    /// The process's exit code (Plan.md P6.11, ADR-0015).
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
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

/// Why the server couldn't start once logging runs.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The async runtime couldn't be built.
    #[error("the async runtime can't be built")]
    Runtime(#[source] io::Error),
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
}

/// Logs `error` with its whole chain as "the server can't start" and
/// returns [`Exit::StartupFailed`].
pub fn cant_start(error: &StartError) -> Exit {
    error!(error = %chain(error), "the server can't start");
    Exit::StartupFailed
}

/// The router `serve` runs: the routes over `database`, request IDs from the
/// real clock and the OS's randomness (the only place either is built), and
/// the `OpenAPI` document in dev mode.
pub fn app(config: &ServerConfig, database: &Database) -> Router {
    let ids = RequestIds::new(Arc::new(SystemClock), Arc::new(OsRandom));
    let state = AppState::new(HealthService::new(Arc::new(database.clone())));
    router(
        state,
        &config.http,
        ids,
        ApiDocs::for_dev_mode(config.dev_mode),
    )
}

/// Opens the database `config` names and serves the API on `listener`
/// until the server ends.
pub async fn run(config: &ServerConfig, listener: TcpListener) -> Exit {
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
    let app = app(config, &database);
    info!(%address, "the server is listening");
    serve(listener, app, &database).await
}

/// Serves `app` on `listener` until the server ends, then closes
/// `database` within [`DATABASE_CLOSE`].
pub async fn serve(listener: TcpListener, app: Router, database: &Database) -> Exit {
    // axum's accept loop handles its own errors and never ends unless a
    // graceful shutdown asks it to.
    match axum::serve(listener, app).await {
        Ok(()) => error!("the server ended unasked"),
        Err(error) => error!(%error, "the server ended unasked"),
    }
    if let Err(error) = database.close_within(DATABASE_CLOSE).await {
        error!(%error, "the database didn't close");
    }
    Exit::ServerFailed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_exit_has_its_code() {
        assert_eq!(Exit::StartupFailed.code(), 1);
        assert_eq!(Exit::ServerFailed.code(), 4);
    }
}
