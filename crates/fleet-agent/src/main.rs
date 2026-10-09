//! `afkfleet-agent`: runs afkfleet's Minecraft AFK bots (Plan.md Phase 5).
//!
//! This binary only wires the library together (ADR-0014):
//! 1. It parses the command line and loads the config, before any async
//!    runtime exists, since loading reads a file.
//! 2. Errors up to here can't be logged yet: they go to stderr as plain
//!    text, and the agent exits with 1.
//! 3. It sets up logging, installs the aws-lc-rs TLS provider and the
//!    Prometheus recorder (before the fleet registers its metrics), and runs
//!    the fleet on a multi-threaded tokio runtime, with fleet-mc's
//!    `AzaleaConnector` as the connector and the diagnostics.
//! 4. Its last log line, "the agent stopped", carries the exit code.

use core::fmt::Display;
use core::time::Duration;
use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser as _;
use fleet_agent::cli::{Cli, Command};
use fleet_agent::config::{self, AgentConfig};
use fleet_agent::run::{Exit, Outcome, RunParts, agent_span, run};
use fleet_agent::telemetry;
use fleet_mc::AzaleaConnector;
use metrics_exporter_prometheus::PrometheusBuilder;
use tracing::{Span, error};

/// How long the runtime's remaining tasks get once the exit code is
/// decided, so nothing stuck can keep the process alive.
const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(1);

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            // clap's own output: usage errors exit 2, `--help` and
            // `--version` 0. A failed write has nowhere to be reported.
            let _ = error.print();
            return ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(2));
        }
    };
    match cli.command {
        Command::Run { config } => run_agent(&config),
    }
}

/// `afkfleet-agent run`.
fn run_agent(path: &Path) -> ExitCode {
    let config = match config::load(path) {
        Ok(config) => config,
        Err(error) => return fail_early(&error),
    };
    if let Err(error) = telemetry::init(&config.log) {
        return fail_early(&error);
    }
    let span = agent_span(&config.name);
    let outcome = start(config, &span);
    span.in_scope(|| outcome.log());
    ExitCode::from(outcome.exit.code())
}

/// Installs what the fleet needs and runs it. Each startup error is logged
/// once, at `error`, in `span`.
fn start(config: AgentConfig, span: &Span) -> Outcome {
    if rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .is_err()
    {
        return cant_start(span, &"a TLS crypto provider is already installed");
    }
    // The fleet registers its metrics in the recorder installed when it's
    // built, so this comes first. The handle isn't kept: nothing serves it
    // until P12.4, and without histograms the recorder needs no upkeep.
    if let Err(error) = PrometheusBuilder::new().install_recorder() {
        return cant_start(
            span,
            &format_args!("the metrics recorder can't be installed: {error}"),
        );
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            return cant_start(
                span,
                &format_args!("the async runtime can't be built: {error}"),
            );
        }
    };
    let seed = match getrandom::u64() {
        Ok(seed) => seed,
        Err(error) => {
            return cant_start(
                span,
                &format_args!("the OS's random source failed: {error}"),
            );
        }
    };
    let connector = AzaleaConnector::new(&config.mc);
    let parts = RunParts {
        config,
        connector: Arc::new(connector.clone()),
        diagnostics: connector,
        anchor: chrono::Utc::now(),
        seed,
    };
    let outcome = runtime.block_on(run(parts));
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN);
    outcome
}

/// Logs a startup error once, at `error`, as the run does its own.
fn cant_start(span: &Span, error: &dyn Display) -> Outcome {
    span.in_scope(|| error!(%error, "the agent can't start"));
    Outcome::startup_failed()
}

/// Reports an error that happened before logging was set up: plainly, on
/// stderr. Returns the startup error's exit code.
fn fail_early(error: &dyn Display) -> ExitCode {
    // A failed write to stderr has nowhere else to go.
    let _ = writeln!(std::io::stderr().lock(), "afkfleet-agent: {error}");
    ExitCode::from(Exit::StartupFailed.code())
}
