//! A managed agent (ADR-0019, S72): enrolled with the server for real
//! (`afkfleet-agent enroll`'s code, a host's token), then run in-process
//! through `fleet_agent::run::run` with its identity loaded as main.rs
//! loads it, its bots on the ledger's fake connector. It reaches the gRPC
//! port only through its own [`Proxy`].
//!
//! A run ends by a signal ([`Agent::stop`]), as an operator stops it, or
//! by a crash ([`Agent::crash`]): its connection is silenced first, so the
//! server sees a host that went away without a word, then its task is
//! aborted. [`Agent::start`] runs it again from the same files.

use core::num::NonZeroUsize;
use core::time::Duration;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fleet_agent::config::{AgentConfig, AgentMode, ControlPlaneConfig, LogConfig};
use fleet_agent::diagnostics::{HostDiagnostics, HostSample};
use fleet_agent::enroll::{self, Enrollment};
use fleet_agent::heartbeat::Heartbeat;
use fleet_agent::managed::identity::ManagedIdentity;
use fleet_agent::managed::link::LinkTimings;
use fleet_agent::run::{Exit, Managed, Outcome, RunParts, run};
use fleet_agent::signals::{ShutdownSignals, Signal};
use fleet_core::enrollment::EnrollmentBundle;
use fleet_core::id::AgentId;
use fleet_core::net::PrivateTargets;
use fleet_core::resilience::{CircuitPolicy, RetryPolicy};
use fleet_core::system::{Clock as _, SecureRandom};
use fleet_core::value::AgentName;
use fleet_mc::McConfig;
use fleet_runtime::RuntimeConfig;
use fleet_server::infra::system::SystemClock;
use fleet_testkit::mc::FakeConnector;
use fleet_testkit::system::SeededRandom;
use rustls::pki_types::UnixTime;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::Instrument as _;

use super::ledger::{Connector, Ledger};
use super::logs::PATIENCE;
use super::proxy::Proxy;
use super::server::Server;

/// How a run of the agent goes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RunOptions {
    /// `[runtime] max_bots`, the capacity its hello reports.
    pub(crate) capacity: usize,
    /// The link's timings: the protocol's, but where a case measures them.
    pub(crate) timings: LinkTimings,
}

impl RunOptions {
    /// `capacity` bots, the protocol's timings.
    pub(crate) fn capacity(capacity: usize) -> Self {
        Self {
            capacity,
            timings: LinkTimings::default(),
        }
    }
}

/// One enrolled agent; see the [module docs](self).
#[derive(Debug)]
pub(crate) struct Agent {
    /// The name it enrolled with.
    pub(crate) name: String,
    /// The ID the server gave it.
    pub(crate) id: AgentId,
    /// Its proxy to the gRPC port.
    pub(crate) proxy: Proxy,
    dir: PathBuf,
    plane: ControlPlaneConfig,
    seed: u64,
    running: Option<Running>,
}

#[derive(Debug)]
struct Running {
    connector: Connector,
    signals: mpsc::Sender<Signal>,
    task: JoinSet<Outcome>,
}

impl Agent {
    /// Enrolls the agent `name` with `server`, its files in `dir/name`,
    /// with a host's token; `seed` makes its key.
    pub(crate) async fn enroll(server: &Server, dir: &Path, name: &str, seed: u64) -> Self {
        let proxy = Proxy::start(server.grpc()).await;
        let bundle = server.bundle().await;
        let plane = enroll_files(
            server,
            dir,
            name,
            &proxy.url(),
            bundle,
            SeededRandom::new(seed),
        )
        .await;
        let dir = dir.join(name);
        let id = server.agent_id(name).await;
        Self {
            name: name.to_owned(),
            id,
            proxy,
            dir,
            plane,
            seed,
            running: None,
        }
    }

    /// Runs the agent as `options` say, its bots booked in `ledger`.
    pub(crate) fn start(&mut self, ledger: &Ledger, options: RunOptions) {
        assert!(self.running.is_none(), "the agent isn't running");
        let identity = ManagedIdentity::load(&self.plane, UnixTime::now()).unwrap();
        let connector = ledger.run(&self.name);
        let config = AgentConfig {
            name: AgentName::try_from(self.name.as_str()).unwrap(),
            runtime: RuntimeConfig {
                max_bots: NonZeroUsize::new(options.capacity).unwrap(),
                ..RuntimeConfig::default()
            },
            mc: McConfig::default(),
            // As tests/control has them: a case's outages never trip the
            // circuit breaker.
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
            heartbeat_file: self.dir.join("agent.alive"),
            log: LogConfig::default(),
            mode: AgentMode::ControlPlane(self.plane.clone()),
        };
        let (signals, receiver) = mpsc::channel(4);
        let mut task = JoinSet::new();
        task.spawn(
            run(RunParts {
                config,
                connector: Arc::new(connector.clone()),
                diagnostics: Quiet,
                signals: Signals(receiver),
                heartbeat: Beats,
                anchor: SystemClock.now(),
                seed: self.seed,
                managed: Some(Managed {
                    identity,
                    seed: self.seed,
                    timings: options.timings,
                }),
            })
            .in_current_span(),
        );
        self.running = Some(Running {
            connector,
            signals,
            task,
        });
    }

    fn running(&self) -> &Running {
        self.running.as_ref().expect("the agent runs")
    }

    /// The current run's fake connector.
    pub(crate) fn fake(&self) -> &FakeConnector {
        self.running().connector.fake()
    }

    /// Stops the run with a signal, as an operator does: every bot leaves
    /// its server first.
    pub(crate) async fn stop(&mut self) {
        let mut running = self.running.take().expect("the agent runs");
        running.signals.send(Signal::Terminate).await.unwrap();
        let outcome = tokio::time::timeout(PATIENCE, running.task.join_next())
            .await
            .expect("the agent stops")
            .unwrap()
            .unwrap();
        assert_eq!(outcome.exit, Exit::Stopped);
        assert!(outcome.report.is_some(), "the fleet confirmed its shutdown");
    }

    /// The host dies: the connection goes silent and stays open, then the
    /// run is aborted, its bots with it.
    pub(crate) async fn crash(&mut self, ledger: &Ledger) {
        let mut running = self.running.take().expect("the agent runs");
        self.proxy.silence_open();
        running.task.abort_all();
        while running.task.join_next().await.is_some() {}
        ledger.dead(running.connector.run());
    }
}

/// Enrolls the agent `name` with `server` through `bundle`, as
/// `afkfleet-agent enroll` does, its files in `dir/name`: the CA the server
/// signs with, and the key `random` makes with the certificate the server
/// signed for it; returns its `[control_plane]` for `url`.
pub(crate) async fn enroll_files(
    server: &Server,
    dir: &Path,
    name: &str,
    url: &str,
    bundle: EnrollmentBundle,
    random: impl SecureRandom + 'static,
) -> ControlPlaneConfig {
    let dir = dir.join(name);
    let plane = ControlPlaneConfig {
        url: url.to_owned(),
        ca_cert_file: dir.join("ca.crt"),
        cert_file: dir.join("agent.crt"),
        key_file: dir.join("agent.key"),
        private_targets: PrivateTargets::default(),
    };
    let enrolled = {
        let plane = plane.clone();
        let ca = server.ca_cert_file();
        let name = AgentName::try_from(name).unwrap();
        // `enroll` blocks on a runtime of its own, as the binary's command
        // does.
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::copy(ca, &plane.ca_cert_file).unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let exit = enroll::enroll(
                Enrollment {
                    plane: &plane,
                    name: &name,
                    bundle,
                },
                &random,
                &runtime,
                &mut out,
                &mut err,
            );
            (exit, String::from_utf8_lossy(&err).into_owned())
        })
    };
    let (exit, err) = tokio::time::timeout(PATIENCE, enrolled)
        .await
        .expect("the enrollment ends")
        .unwrap();
    assert_eq!(exit, enroll::Exit::Enrolled, "{err}");
    plane
}

/// Never samples anything: the fake connector has no host threads.
#[derive(Debug, Clone, Copy)]
struct Quiet;

impl HostDiagnostics for Quiet {
    fn sample(&self) -> HostSample {
        HostSample::default()
    }
}

/// A heartbeat nobody reads.
#[derive(Debug, Clone, Copy)]
struct Beats;

impl Heartbeat for Beats {
    fn touch(&self) -> impl Future<Output = io::Result<()>> + Send {
        core::future::ready(Ok(()))
    }
}

/// The case's signals.
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
