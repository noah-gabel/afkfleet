//! `afkfleet-server`: the afkfleet server (Plan.md P6.11, ADR-0015).
//!
//! It parses the command line and loads the config, before any async
//! runtime exists, since loading reads a file. Errors up to here can't be
//! logged yet: they go to stderr as plain text, and the process exits with
//! 1. Then, by command:
//!
//! - `serve` starts logging, warns once if dev mode is on, builds a
//!   multi-threaded runtime, installs the stop signals, binds `[http] bind`
//!   and runs [`serve::run`] until a signal stops it. Every startup error
//!   from here on is one `error` event, "the server can't start", and the
//!   last line, "the server stopped", carries the exit code.
//! - `migrate` starts logging and runs [`migrate::run`] on a current-thread
//!   runtime.
//! - `healthcheck` runs [`healthcheck::check`] on a current-thread runtime
//!   without logging, prints one line on stdout and exits only 0 or 1, since
//!   Docker reserves 2 for healthchecks.
//!
//! The library holds the logic; this file only wires it together.

use core::fmt::Display;
use std::io::{self, Write as _};
use std::path::Path;
use std::process::ExitCode;

use clap::Parser as _;
use fleet_server::cli::serve::StartError;
use fleet_server::cli::{self, Cli, Command, healthcheck, migrate, serve};
use fleet_server::config::{self, ServerConfig};
use fleet_startup::signals::OsSignals;
use fleet_startup::telemetry::{NoRules, init_with};
use tokio::net::TcpListener;
use tokio::runtime::{Builder, Runtime};

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            // clap writes help and version to stdout and errors to stderr.
            let _ = error.print();
            return ExitCode::from(cli::exit_code(&error, std::env::args_os()));
        }
    };
    match cli.command {
        Command::Serve { config } => run_server(&config),
        Command::Migrate { config } => migrate_database(&config),
        Command::Healthcheck { config } => check_health(&config),
    }
}

/// `afkfleet-server serve`.
fn run_server(path: &Path) -> ExitCode {
    let config = match config::load(path) {
        Ok(config) => config,
        Err(error) => return fail_early(&error),
    };
    if let Err(error) = init_with(&config.log, &NoRules) {
        return fail_early(&error);
    }
    config.warn_if_dev_mode();
    let exit = start(&config);
    exit.log();
    ExitCode::from(exit.code())
}

/// Builds the runtime, installs the stop signals, binds the address and
/// runs the server.
fn start(config: &ServerConfig) -> serve::Exit {
    let runtime = match Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => return serve::cant_start(&StartError::Runtime(error)),
    };
    let exit = runtime.block_on(async {
        // Inside the runtime, which tokio's signal handling needs, and before
        // anything else, so an early signal isn't lost.
        let signals = match OsSignals::install() {
            Ok(signals) => signals,
            Err(error) => return serve::cant_start(&StartError::Signals(error)),
        };
        // Binding a local address doesn't wait on anything remote.
        let listener = match TcpListener::bind(config.http.bind).await {
            Ok(listener) => listener,
            Err(source) => {
                return serve::cant_start(&StartError::Listen {
                    address: config.http.bind,
                    source,
                });
            }
        };
        serve::run(config, listener, signals).await
    });
    runtime.shutdown_timeout(serve::RUNTIME_SHUTDOWN);
    exit
}

/// `afkfleet-server migrate`.
fn migrate_database(path: &Path) -> ExitCode {
    let config = match config::load(path) {
        Ok(config) => config,
        Err(error) => return fail_early(&error),
    };
    if let Err(error) = init_with(&config.log, &NoRules) {
        return fail_early(&error);
    }
    let exit = match current_thread() {
        Ok(runtime) => runtime.block_on(migrate::run(&config.database)),
        Err(error) => migrate::cant_migrate(&StartError::Runtime(error)),
    };
    ExitCode::from(exit.code())
}

/// `afkfleet-server healthcheck`.
fn check_health(path: &Path) -> ExitCode {
    let config = match config::load(path) {
        Ok(config) => config,
        Err(error) => return fail_early(&error),
    };
    let runtime = match current_thread() {
        Ok(runtime) => runtime,
        Err(error) => return fail_early(&StartError::Runtime(error)),
    };
    let health = runtime.block_on(healthcheck::check(&config.http));
    // A failed write leaves the exit code to tell the result.
    let _ = writeln!(io::stdout().lock(), "{health}");
    ExitCode::from(health.exit_code())
}

/// A runtime for one task: `migrate` and `healthcheck` do one thing.
fn current_thread() -> io::Result<Runtime> {
    Builder::new_current_thread().enable_all().build()
}

/// Reports an error from before logging exists on stderr, as one plain
/// message, and exits with 1.
fn fail_early(error: &dyn Display) -> ExitCode {
    // Nothing is left to report a failed write to.
    let _ = writeln!(io::stderr().lock(), "afkfleet-server: {error}");
    ExitCode::from(1)
}
