//! `afkfleet-server`: the afkfleet server (Plan.md P6.11, ADR-0015).
//!
//! It parses the command line and loads the config, before any async
//! runtime exists, since loading reads a file. Errors up to here can't be
//! logged yet: they go to stderr as plain text, and the process exits with
//! 1. Then, by command:
//!
//! - `serve` starts logging, warns once if dev mode is on, loads the vault's
//!   keys (ADR-0016), installs aws-lc-rs as the process's one rustls provider
//!   (ADR-0018), builds a multi-threaded runtime, installs the stop
//!   signals, binds `[http] bind` and runs [`serve::run`] until a signal
//!   stops it. Every startup error
//!   from here on is one `error` event, "the server can't start", and the
//!   last line, "the server stopped", carries the exit code.
//! - `migrate` starts logging and runs [`migrate::run`] on a current-thread
//!   runtime.
//! - `healthcheck` runs [`healthcheck::check`] on a current-thread runtime
//!   without logging, prints one line on stdout and exits only 0 or 1, since
//!   Docker reserves 2 for healthchecks.
//! - `user create-owner` starts logging and runs [`user::create_owner`] on a
//!   current-thread runtime, with the terminal or stdin as the password's
//!   source.
//! - `pki init` and `pki server-cert` start logging and run [`pki::init`]
//!   or [`pki::server_cert`] with the real clock and the OS's randomness,
//!   without a runtime: they only read and write files.
//! - `vault status` and `vault rotate-key` start logging and run
//!   [`vault::status`] or [`vault::rotate_key`] on a current-thread runtime.
//!
//! The library holds the logic; this file only wires it together.

use core::fmt::Display;
use std::io::{self, IsTerminal as _, Write as _};
use std::net::IpAddr;
use std::path::Path;
use std::process::ExitCode;

use clap::Parser as _;
use fleet_server::cli::serve::StartError;
use fleet_server::cli::user::{self, PasswordInput, TerminalPrompt};
use fleet_server::cli::{
    self, Cli, Command, PkiCommand, UserCommand, VaultCommand, healthcheck, migrate, pki, serve,
    vault,
};
use fleet_server::config::{self, ServerConfig};
use fleet_server::infra::crypto::vault::VaultKeys;
use fleet_server::infra::system::{OsRandom, SystemClock};
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
        Command::User {
            command:
                UserCommand::CreateOwner {
                    config,
                    username,
                    password_stdin,
                },
        } => create_owner(&config, &username, password_stdin),
        Command::Vault {
            command: VaultCommand::Status { config },
        } => run_vault(&config, VaultRun::Status),
        Command::Vault {
            command: VaultCommand::RotateKey { config },
        } => run_vault(&config, VaultRun::RotateKey),
        Command::Pki {
            command: PkiCommand::Init { config, dns, ip },
        } => run_pki(&config, PkiRun::Init, &dns, &ip),
        Command::Pki {
            command: PkiCommand::ServerCert { config, dns, ip },
        } => run_pki(&config, PkiRun::ServerCert, &dns, &ip),
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
    // Plain file reads, before any runtime exists: a bad key stops startup.
    let exit = match VaultKeys::load(&config.vault) {
        Ok(keys) => start(&config, keys),
        Err(error) => serve::cant_start(&StartError::VaultKeys(error)),
    };
    exit.log();
    ExitCode::from(exit.code())
}

/// Installs the TLS provider, builds the runtime, installs the stop
/// signals, binds the address and runs the server with the vault's `keys`.
///
/// The provider is installed here, never in the library's `serve::run`:
/// in-process tests start several servers in one process, and a second
/// install fails (ADR-0018).
fn start(config: &ServerConfig, keys: VaultKeys) -> serve::Exit {
    if rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .is_err()
    {
        return serve::cant_start(&StartError::CryptoProvider);
    }
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
        serve::run(config, listener, signals, keys).await
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

/// `afkfleet-server user create-owner`: the password from the terminal, or
/// from stdin with `--password-stdin`.
fn create_owner(path: &Path, username: &str, password_stdin: bool) -> ExitCode {
    let config = match config::load(path) {
        Ok(config) => config,
        Err(error) => return fail_early(&error),
    };
    if let Err(error) = init_with(&config.log, &NoRules) {
        return fail_early(&error);
    }
    let runtime = match current_thread() {
        Ok(runtime) => runtime,
        Err(error) => return fail_early(&StartError::Runtime(error)),
    };
    let stdin = io::stdin();
    let is_terminal = stdin.is_terminal();
    let input = if password_stdin {
        PasswordInput::Stdin {
            reader: Box::new(stdin),
            is_terminal,
        }
    } else {
        PasswordInput::Prompt(Box::new(TerminalPrompt))
    };
    let exit = runtime.block_on(user::create_owner(
        &config,
        username,
        input,
        &mut io::stdout(),
        &mut io::stderr(),
    ));
    ExitCode::from(exit.code())
}

/// Which vault command [`run_vault`] runs.
#[derive(Debug, Clone, Copy)]
enum VaultRun {
    /// `vault status`.
    Status,
    /// `vault rotate-key`.
    RotateKey,
}

/// `afkfleet-server vault status` or `vault rotate-key`.
fn run_vault(path: &Path, command: VaultRun) -> ExitCode {
    let config = match config::load(path) {
        Ok(config) => config,
        Err(error) => return fail_early(&error),
    };
    if let Err(error) = init_with(&config.log, &NoRules) {
        return fail_early(&error);
    }
    let runtime = match current_thread() {
        Ok(runtime) => runtime,
        Err(error) => return fail_early(&StartError::Runtime(error)),
    };
    let mut out = io::stdout();
    let mut err = io::stderr();
    let exit = runtime.block_on(async {
        match command {
            VaultRun::Status => vault::status(&config, &mut out, &mut err).await,
            VaultRun::RotateKey => vault::rotate_key(&config, &mut out, &mut err).await,
        }
    });
    ExitCode::from(exit.code())
}

/// Which `pki` command [`run_pki`] runs.
#[derive(Debug, Clone, Copy)]
enum PkiRun {
    /// `pki init`.
    Init,
    /// `pki server-cert`.
    ServerCert,
}

/// `afkfleet-server pki init` or `pki server-cert`, with the real clock and
/// the OS's randomness. Plain file work: no runtime.
fn run_pki(path: &Path, command: PkiRun, dns: &[String], ips: &[IpAddr]) -> ExitCode {
    let config = match config::load(path) {
        Ok(config) => config,
        Err(error) => return fail_early(&error),
    };
    if let Err(error) = init_with(&config.log, &NoRules) {
        return fail_early(&error);
    }
    let run = match command {
        PkiRun::Init => pki::init,
        PkiRun::ServerCert => pki::server_cert,
    };
    let exit = run(
        &config,
        dns,
        ips,
        &SystemClock,
        &OsRandom,
        &mut io::stdout(),
        &mut io::stderr(),
    );
    ExitCode::from(exit.code())
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
