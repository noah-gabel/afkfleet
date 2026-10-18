//! The real managed run under test: fleet-testkit's fake connector for the
//! bots, an identity from the seeded test PKI, the test's signals, the
//! protocol's link timings (short ones only where a test measures them), and
//! its log lines captured.

use core::num::NonZeroUsize;
use core::time::Duration;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::{DateTime, Utc};

use fleet_agent::config::{AgentConfig, AgentMode, LogConfig, LogFilter, LogFormat};
use fleet_agent::diagnostics::{HostDiagnostics, HostSample};
use fleet_agent::heartbeat::Heartbeat;
use fleet_agent::managed::identity::ManagedIdentity;
use fleet_agent::managed::link::LinkTimings;
use fleet_agent::run::{Managed, Outcome, RunParts, run};
use fleet_agent::signals::{ShutdownSignals, Signal};
use fleet_agent::telemetry::layer;
use fleet_core::id::AgentId;
use fleet_core::resilience::{CircuitPolicy, RetryPolicy};
use fleet_mc::McConfig;
use fleet_runtime::RuntimeConfig;
use fleet_testkit::log_buffer::LogBuffer;
use fleet_testkit::mc::FakeConnector;
use rustls::pki_types::UnixTime;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::Instrument as _;
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::layer::SubscriberExt as _;

use crate::pki::{ALWAYS, TestCa, agent_id, agent_key, identity_files};
use crate::server::PATIENCE;

/// The agent's ID in every test.
pub(crate) fn agent() -> AgentId {
    agent_id(1)
}

/// Short timings, for the tests that measure the welcome timeout or the
/// keepalive: a missing welcome or a silent connection shows within seconds.
/// Every other test runs the protocol's, so a slow machine can't end its
/// stream early.
pub(crate) const SHORT: LinkTimings = LinkTimings {
    welcome: Duration::from_secs(2),
    keepalive_interval: Duration::from_secs(1),
    keepalive_timeout: Duration::from_secs(2),
};

/// How a test runs the agent.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    /// `[runtime] max_bots`.
    pub(crate) max_bots: usize,
    /// The link's timings.
    pub(crate) timings: LinkTimings,
    /// The run's start, which its clock counts from: R52's check reads it.
    pub(crate) anchor: DateTime<Utc>,
}

/// Four bots, the protocol's timings, the run starting now.
impl Default for Options {
    fn default() -> Self {
        Self {
            max_bots: 4,
            timings: LinkTimings::default(),
            anchor: Utc::now(),
        }
    }
}

/// Never samples anything.
#[derive(Debug, Clone, Default)]
struct Quiet;

impl HostDiagnostics for Quiet {
    fn sample(&self) -> HostSample {
        HostSample::default()
    }
}

/// Counts the beats.
#[derive(Debug, Clone, Default)]
struct Beats(Arc<AtomicUsize>);

impl Heartbeat for Beats {
    fn touch(&self) -> impl Future<Output = io::Result<()>> + Send {
        self.0.fetch_add(1, Ordering::SeqCst);
        core::future::ready(Ok(()))
    }
}

/// The test's signals.
#[derive(Debug)]
struct Signals(mpsc::Receiver<Signal>);

impl ShutdownSignals for Signals {
    async fn recv(&mut self) -> Signal {
        match self.0.recv().await {
            Some(signal) => signal,
            None => core::future::pending().await,
        }
    }
}

/// The managed agent running against `url`.
pub(crate) struct Agent {
    /// The bots' fake connector.
    pub(crate) fake: FakeConnector,
    /// Every log line of the run.
    pub(crate) logs: LogBuffer,
    _guard: DefaultGuard,
    signals: mpsc::Sender<Signal>,
    task: JoinHandle<Outcome>,
}

/// The agent's config for `url`, its files in `dir`, at most `max_bots`.
fn config(dir: &Path, url: &str, ca: &TestCa, max_bots: usize) -> AgentConfig {
    let key = agent_key(1);
    let certificate = ca.agent_certificate(&key, agent(), ALWAYS);
    let plane = identity_files(dir, url, ca, &key, &certificate);
    AgentConfig {
        name: "agent-1".try_into().unwrap(),
        runtime: RuntimeConfig {
            max_bots: NonZeroUsize::new(max_bots).unwrap(),
            ..RuntimeConfig::default()
        },
        mc: McConfig::default(),
        retry: RetryPolicy::try_new(
            Duration::from_secs(5),
            Duration::from_mins(5),
            Duration::from_mins(5),
        )
        .unwrap(),
        circuit: CircuitPolicy::try_new(
            NonZeroUsize::new(8).unwrap(),
            Duration::from_mins(10),
            Duration::from_mins(15),
        )
        .unwrap(),
        heartbeat_file: dir.join("agent.alive"),
        log: LogConfig::default(),
        mode: AgentMode::ControlPlane(plane),
    }
}

impl Agent {
    /// Starts a managed agent for the server at `url`, of the test CA
    /// `ca`, its files in `dir`.
    pub(crate) fn start(dir: &Path, url: &str, ca: &TestCa) -> Self {
        Self::launch(dir, url, ca, Options::default())
    }

    /// [`Agent::start`] as `options` say.
    pub(crate) fn launch(dir: &Path, url: &str, ca: &TestCa, options: Options) -> Self {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let logs = LogBuffer::default();
        let log = LogConfig {
            format: LogFormat::Json,
            filter: LogFilter::try_from("debug").unwrap(),
        };
        let subscriber = tracing_subscriber::registry().with(layer(&log, false, logs.clone()));
        let guard = tracing::subscriber::set_default(subscriber);
        let config = config(dir, url, ca, options.max_bots);
        let AgentMode::ControlPlane(plane) = &config.mode else {
            unreachable!("managed");
        };
        let identity = ManagedIdentity::load(plane, UnixTime::now()).unwrap();
        let fake = FakeConnector::new();
        let (signals, receiver) = mpsc::channel(4);
        let task = tokio::spawn(
            run(RunParts {
                config,
                connector: Arc::new(fake.clone()),
                diagnostics: Quiet,
                signals: Signals(receiver),
                heartbeat: Beats::default(),
                anchor: options.anchor,
                seed: 1,
                managed: Some(Managed {
                    identity,
                    seed: 7,
                    timings: options.timings,
                }),
            })
            .in_current_span(),
        );
        Self {
            fake,
            logs,
            _guard: guard,
            signals,
            task,
        }
    }

    /// Sends `signal`.
    pub(crate) async fn signal(&self, signal: Signal) {
        self.signals.send(signal).await.unwrap();
    }

    /// How the run ended, within [`PATIENCE`].
    pub(crate) async fn outcome(self) -> Outcome {
        tokio::time::timeout(PATIENCE, self.task)
            .await
            .expect("the run should end")
            .unwrap()
    }

    /// Stops the run with a signal and waits for it.
    pub(crate) async fn stop(self) -> Outcome {
        self.signal(Signal::Terminate).await;
        self.outcome().await
    }
}
