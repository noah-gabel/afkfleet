//! What the run's test files share: values, paused-time helpers, and the run
//! under test with the test's handles on it.

use core::num::NonZeroUsize;
use core::time::Duration;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_agent::config::{AgentConfig, AgentMode, LogConfig, LogFilter, LogFormat, StandaloneBot};
use fleet_agent::run::{Outcome, RunParts, run};
use fleet_agent::telemetry::layer;
use fleet_core::disconnect::ConflictTexts;
use fleet_core::mc::SessionEvent;
use fleet_core::mode::ModePreset;
use fleet_core::resilience::{CircuitPolicy, RetryPolicy};
use fleet_mc::McConfig;
use fleet_runtime::RuntimeConfig;
use fleet_testkit::mc::{EmitOutcome, FakeConnector, SessionController};
use serde_json::Value;
use tokio::task::JoinHandle;
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::layer::SubscriberExt as _;

use crate::common::Capture;
use crate::fakes::FakeDiagnostics;

// --- Values ---

pub(crate) const fn secs(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

/// The agent's name in every test.
pub(crate) const AGENT: &str = "test-agent";

/// The run starts at this wall time in every test.
pub(crate) fn anchor() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).unwrap()
}

/// A standalone config with Appendix A's defaults and one bot per entry,
/// each joining `localhost`.
pub(crate) fn config(bots: &[(&str, ModePreset)]) -> AgentConfig {
    AgentConfig {
        name: AGENT.try_into().unwrap(),
        runtime: RuntimeConfig::default(),
        mc: McConfig::default(),
        retry: RetryPolicy::try_new(secs(5), secs(300), secs(300)).unwrap(),
        circuit: CircuitPolicy::try_new(NonZeroUsize::new(8).unwrap(), secs(600), secs(900))
            .unwrap(),
        heartbeat_file: std::env::temp_dir().join("afkfleet-agent-test.alive"),
        log: LogConfig::default(),
        mode: AgentMode::Standalone(
            bots.iter()
                .map(|&(name, mode)| StandaloneBot {
                    username: name.parse().unwrap(),
                    server: "localhost".try_into().unwrap(),
                    mode,
                    conflict_texts: ConflictTexts::default(),
                })
                .collect(),
        ),
    }
}

/// One `afk` bot per name.
pub(crate) fn afk(names: &[&str]) -> AgentConfig {
    let bots: Vec<_> = names.iter().map(|&name| (name, ModePreset::Afk)).collect();
    config(&bots)
}

/// A path for `[control_plane]`'s files; nothing reads them.
pub(crate) fn unread_file(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

// --- Time ---

/// Lets every spawned task run until it waits; time doesn't move.
pub(crate) async fn settle() {
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}

/// Moves paused time forward by `duration` and lets the tasks run.
pub(crate) async fn advance(duration: Duration) {
    tokio::time::advance(duration).await;
    settle().await;
}

// --- Logs ---

/// Every JSON line in `capture` whose message is `message`.
pub(crate) fn lines_with(capture: &Capture, message: &str) -> Vec<Value> {
    capture
        .json_lines()
        .into_iter()
        .filter(|line| line["message"] == message)
        .collect()
}

/// Captures this thread's events as the agent's JSON lines, `debug` and
/// up, until the guard drops. The run's tasks run on this thread too.
fn capture_logs() -> (Capture, DefaultGuard) {
    let capture = Capture::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: LogFilter::try_from("debug").unwrap(),
    };
    let subscriber = tracing_subscriber::registry().with(layer(&config, false, capture.clone()));
    (capture, tracing::subscriber::set_default(subscriber))
}

// --- The run under test ---

/// A running agent and the test's handles on it.
pub(crate) struct Agent {
    pub(crate) fake: FakeConnector,
    pub(crate) diagnostics: FakeDiagnostics,
    pub(crate) capture: Capture,
    task: JoinHandle<Outcome>,
    _logs: DefaultGuard,
}

impl Agent {
    /// Runs the agent with `config` and lets it start.
    pub(crate) async fn start(config: AgentConfig) -> Self {
        let (capture, logs) = capture_logs();
        let fake = FakeConnector::new();
        let diagnostics = FakeDiagnostics::default();
        let task = tokio::spawn(run(RunParts {
            config,
            connector: Arc::new(fake.clone()),
            diagnostics: diagnostics.clone(),
            anchor: anchor(),
            seed: 1,
        }));
        settle().await;
        Self {
            fake,
            diagnostics,
            capture,
            task,
            _logs: logs,
        }
    }

    /// The controller of session `index`, which must have started.
    pub(crate) async fn session(&self, index: usize) -> SessionController {
        tokio::time::timeout(secs(1), self.fake.session(index))
            .await
            .expect("the session should have started")
    }

    /// Joins the first `count` sessions, so their bots come online.
    pub(crate) async fn join(&self, count: usize) {
        for index in 0..count {
            let session = self.session(index).await;
            assert_eq!(session.emit(SessionEvent::Joined), EmitOutcome::Queued);
        }
        settle().await;
    }

    /// Whether the run has ended.
    pub(crate) fn has_ended(&self) -> bool {
        self.task.is_finished()
    }

    /// Waits for the run's outcome; a run that hangs fails the test.
    pub(crate) async fn outcome(self) -> Outcome {
        tokio::time::timeout(secs(60), self.task)
            .await
            .expect("the run should end")
            .unwrap()
    }

    /// Every JSON line logged so far whose message is `message`.
    pub(crate) fn lines(&self, message: &str) -> Vec<Value> {
        lines_with(&self.capture, message)
    }
}
