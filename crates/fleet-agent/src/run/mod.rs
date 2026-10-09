//! The agent's run: fleet-mc's connector wired to fleet-runtime's fleet, for
//! the bots of a standalone config (Plan.md P5.3, ADR-0014).
//!
//! [`run`] is generic over the connector, the diagnostics and the signals,
//! so tests drive it with fleet-testkit's fakes and a signal channel on
//! paused time; main.rs passes fleet-mc's `AzaleaConnector` for the first
//! two and the OS's signals.
//!
//! 1. A `[control_plane]` config is refused until Phase 10.
//! 2. Each standalone bot gets a fresh random v7 ID.
//! 3. The fleet starts, with offline credentials. Its supervisor runs in a
//!    task the run owns, and one more task logs chat at `debug`.
//! 4. Every bot is applied, each with one `info` line that ties its ID to
//!    its username, server and mode.
//! 5. The run watches the supervisor and the stop signals, and samples
//!    fleet-mc's diagnostics every 5 s. A signal or the abandoned-thread
//!    limit shuts the fleet down; a later signal is only logged.
//!
//! Every shutdown has one deadline, the shutdown timeout plus the reply
//! timeout. The [`Outcome`] says how the run ended, and its [`Exit`] gives
//! the process's exit code.

mod events;
mod finish;
mod specs;
mod watch;

use core::fmt;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::bot::BotSpec;
use fleet_core::mc::MinecraftConnector;
use fleet_core::value::AgentName;
use fleet_runtime::{Fleet, FleetParts, OfflineCredentials, ShutdownReport, Supervisor};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument as _, Span, error, info, info_span};

use self::finish::{Timeouts, finish};
use self::specs::{StartupError, os_random, standalone_specs};
use self::watch::{Reason, watch};
use crate::config::{AgentConfig, AgentMode, StandaloneBot};
use crate::diagnostics::{self, HostDiagnostics};
use crate::signals::ShutdownSignals;

/// Everything a run is built from.
pub struct RunParts<C, D, S> {
    /// The validated config.
    pub config: AgentConfig,
    /// Starts every bot's sessions.
    pub connector: Arc<C>,
    /// The Minecraft adapter's numbers, sampled every 5 s.
    pub diagnostics: D,
    /// The signals that stop the agent. They're installed before the run
    /// starts, so an early signal isn't lost.
    pub signals: S,
    /// The wall time at which the run starts: the runtime's clock starts
    /// here, and the bot IDs carry it.
    pub anchor: DateTime<Utc>,
    /// The seed of the runtime's randomness.
    pub seed: u64,
}

impl<C, D, S> fmt::Debug for RunParts<C, D, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunParts")
            .field("config", &self.config)
            .field("anchor", &self.anchor)
            .field("seed", &self.seed)
            .finish_non_exhaustive()
    }
}

/// How a run ended, which decides the process's exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Exit {
    /// A signal stopped the agent: every bot was stopped, or aborted at the
    /// shutdown timeout.
    Stopped,
    /// The agent couldn't start: the config, logging, or the fleet's setup.
    StartupFailed,
    /// The abandoned-thread limit was reached; the agent shut down first.
    AbandonedLimit,
    /// The fleet's supervisor ended unasked, or didn't end when asked.
    SupervisorFailed,
}

impl Exit {
    /// The process's exit code (Plan.md P5.3, ADR-0014).
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Stopped => 0,
            Self::StartupFailed => 1,
            Self::AbandonedLimit => 3,
            Self::SupervisorFailed => 4,
        }
    }
}

/// How a run ended, with the fleet's shutdown report if there is one.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// Why the run ended.
    pub exit: Exit,
    /// The fleet's report, when it confirmed its shutdown.
    pub report: Option<ShutdownReport>,
}

impl Outcome {
    /// A run that couldn't start, with no report.
    pub const fn startup_failed() -> Self {
        Self {
            exit: Exit::StartupFailed,
            report: None,
        }
    }

    /// Logs the agent's last line, "the agent stopped", with the exit code
    /// and the report's counts when there's a report.
    pub fn log(&self) {
        // An empty `Option` field isn't recorded at all.
        info!(
            exit_code = self.exit.code(),
            stopped = self.report.map(|report| report.stopped),
            aborted = self.report.map(|report| report.aborted),
            crashed = self.report.map(|report| report.crashed),
            "the agent stopped"
        );
    }
}

/// The span every line of the agent's run is in: `agent{agent=<name>}`.
#[must_use]
pub fn agent_span(name: &AgentName) -> Span {
    info_span!("agent", agent = %name)
}

/// Runs the agent until a signal stops it or it has to stop, and says why
/// it stopped.
///
/// Startup errors are logged at `error` and end the run with
/// [`Exit::StartupFailed`]; a bot the fleet refuses at startup shuts the
/// fleet down first.
pub async fn run<C, D, S>(parts: RunParts<C, D, S>) -> Outcome
where
    C: MinecraftConnector,
    D: HostDiagnostics,
    S: ShutdownSignals,
{
    let span = agent_span(&parts.config.name);
    run_in_span(parts).instrument(span).await
}

/// The run, inside the agent's span.
async fn run_in_span<C, D, S>(parts: RunParts<C, D, S>) -> Outcome
where
    C: MinecraftConnector,
    D: HostDiagnostics,
    S: ShutdownSignals,
{
    let RunParts {
        config,
        connector,
        diagnostics,
        mut signals,
        anchor,
        seed,
    } = parts;
    let Built {
        bots,
        specs,
        fleet,
        supervisor,
    } = match build(&config, connector, anchor, seed) {
        Ok(built) => built,
        Err(error) => {
            error!(%error, "the agent can't start");
            return Outcome::startup_failed();
        }
    };
    let cancel = CancellationToken::new();
    let mut logger = JoinSet::new();
    logger
        .spawn(events::log_fleet_events(fleet.subscribe(), cancel.child_token()).in_current_span());
    let mut supervisor_task = JoinSet::new();
    // In the agent's span, so every bot's span nests in it.
    supervisor_task.spawn(supervisor.run(cancel.child_token()).in_current_span());

    let reason = if apply_all(&fleet, bots, specs).await {
        info!(bots = bots.len(), "the agent is running");
        watch(
            &mut supervisor_task,
            &mut signals,
            &diagnostics,
            config.mc.max_abandoned_threads,
        )
        .await
    } else {
        Reason::StartupFailed
    };
    let outcome = finish(
        reason,
        &fleet,
        &mut supervisor_task,
        &cancel,
        &mut signals,
        Timeouts::of(&config.runtime),
    )
    .await;
    cancel.cancel();
    logger.shutdown().await;
    outcome
}

/// A fleet built for a config's standalone bots.
struct Built<'a, C> {
    /// The config's bots.
    bots: &'a [StandaloneBot],
    /// Their specs, in the same order, with new IDs.
    specs: Vec<BotSpec>,
    /// The fleet's handle.
    fleet: Fleet,
    /// The fleet's supervisor, not running yet.
    supervisor: Supervisor<C, OfflineCredentials>,
}

/// Builds the fleet for `config`'s standalone bots, with their new IDs.
///
/// # Errors
/// [`StartupError::ManagedMode`] for a `[control_plane]` config, then the
/// errors of [`standalone_specs`] and [`StartupError::Setup`].
fn build<C: MinecraftConnector>(
    config: &AgentConfig,
    connector: Arc<C>,
    anchor: DateTime<Utc>,
    seed: u64,
) -> Result<Built<'_, C>, StartupError> {
    let AgentMode::Standalone(bots) = &config.mode else {
        return Err(StartupError::ManagedMode);
    };
    let specs = standalone_specs(bots, anchor, os_random)?;
    let (fleet, supervisor) = Fleet::new(FleetParts {
        connector,
        credentials: Arc::new(OfflineCredentials),
        retry: config.retry,
        circuit: config.circuit,
        config: config.runtime,
        anchor,
        seed,
    })
    .map_err(StartupError::Setup)?;
    diagnostics::register();
    Ok(Built {
        bots,
        specs,
        fleet,
        supervisor,
    })
}

/// Applies every bot, each with its `info` line, and says whether all were
/// applied. A bot the fleet refuses is logged at `error` and stops the
/// start.
async fn apply_all(fleet: &Fleet, bots: &[StandaloneBot], specs: Vec<BotSpec>) -> bool {
    for (bot, spec) in bots.iter().zip(specs) {
        let id = spec.id;
        info!(
            bot_id = %id,
            username = %bot.username,
            server = %bot.server,
            mode = bot.mode.name(),
            "starting a standalone bot"
        );
        if let Err(error) = fleet.apply(spec, None).await {
            error!(
                bot_id = %id,
                username = %bot.username,
                %error,
                "the fleet refused a standalone bot; shutting down"
            );
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::telemetry::capture::json_on_this_thread;

    #[rstest]
    #[case::stopped(Exit::Stopped, 0)]
    #[case::startup_failed(Exit::StartupFailed, 1)]
    #[case::abandoned_limit(Exit::AbandonedLimit, 3)]
    #[case::supervisor_failed(Exit::SupervisorFailed, 4)]
    fn each_exit_has_its_code(#[case] exit: Exit, #[case] code: u8) {
        assert_eq!(exit.code(), code);
    }

    #[test]
    fn the_last_line_has_the_report_counts_when_there_is_a_report() {
        let (capture, _guard) = json_on_this_thread();
        let outcome = Outcome {
            exit: Exit::AbandonedLimit,
            report: Some(ShutdownReport {
                stopped: 2,
                aborted: 1,
                crashed: 0,
            }),
        };

        outcome.log();

        let lines = capture.lines_with("the agent stopped");
        assert_eq!(lines.len(), 1, "{}", capture.text());
        let line = &lines[0];
        assert_eq!(line["level"], "INFO");
        assert_eq!(line["exit_code"], 3);
        assert_eq!(line["stopped"], 2);
        assert_eq!(line["aborted"], 1);
        assert_eq!(line["crashed"], 0);
    }

    #[test]
    fn the_last_line_has_only_the_exit_code_without_a_report() {
        let (capture, _guard) = json_on_this_thread();

        Outcome::startup_failed().log();

        let lines = capture.lines_with("the agent stopped");
        assert_eq!(lines.len(), 1, "{}", capture.text());
        let line = &lines[0];
        assert_eq!(line["exit_code"], 1);
        assert!(line.get("stopped").is_none());
        assert!(line.get("aborted").is_none());
        assert!(line.get("crashed").is_none());
    }

    #[test]
    fn the_agent_span_carries_the_agents_name() {
        let (capture, _guard) = json_on_this_thread();
        let name = AgentName::try_from("agent-1").unwrap();

        agent_span(&name).in_scope(|| tracing::info!("inside"));

        let line = &capture.lines_with("inside")[0];
        assert_eq!(line["span"]["name"], "agent");
        assert_eq!(line["span"]["agent"], "agent-1");
    }
}
