//! A managed agent (ADR-0019, S72): enrolled with the server for real
//! (`afkfleet-agent enroll`'s code, a host's token), then run in-process
//! through `fleet_agent::run::run` with its identity loaded as main.rs
//! loads it, its bots on the ledger's fake connector, or on fleet-mc's real
//! one for the slow on-respawn case (ADR-0020, S26). It reaches the gRPC
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
use fleet_mc::{AzaleaConnector, McConfig};
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
    /// The bots' `[retry]`: 5 s to 5 min, as tests/control has it, so a
    /// case's outages never trip the circuit breaker; a case that waits for
    /// a bot's next attempt caps it lower.
    pub(crate) retry: RetryPolicy,
}

impl RunOptions {
    /// `capacity` bots, the protocol's timings, the default retries.
    pub(crate) fn capacity(capacity: usize) -> Self {
        Self {
            capacity,
            timings: LinkTimings::default(),
            retry: RetryPolicy::try_new(
                Duration::from_secs(5),
                Duration::from_mins(5),
                Duration::from_mins(5),
            )
            .unwrap(),
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
    /// The ledger's fake connector, or `None` on the real one.
    connector: Option<Connector>,
    signals: mpsc::Sender<Signal>,
    task: JoinSet<Outcome>,
}

impl Agent {
    /// Enrolls the agent `name` with `server`, its files in `dir/name`,
    /// with a host's token; `seed` makes its key.
    pub(crate) async fn enroll(server: &Server, dir: &Path, name: &str, seed: u64) -> Self {
        let proxy = Proxy::start(server.grpc_address()).await;
        let bundle = server.bundle().await;
        let plane = enroll_files(
            server,
            dir,
            name,
            &proxy.url(),
            bundle,
            SeededRandom::new(seed),
            PATIENCE,
        )
        .await
        .unwrap_or_else(|error| panic!("{}: {}", error.why, error.output));
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
        let connector = ledger.run(&self.name);
        self.launch(options, connector.clone(), self.plane.clone());
        if let Some(running) = &mut self.running {
            running.connector = Some(connector);
        }
    }

    /// Runs the agent as `options` say, its bots on fleet-mc's real azalea
    /// connector, allowed to dial `127.0.0.1` only, where a case's Minecraft
    /// container publishes its port (ADR-0019, S46; ADR-0020, S26). Nothing
    /// is booked in a ledger.
    pub(crate) fn start_on_minecraft(&mut self, options: RunOptions) {
        let loopback = "127.0.0.1/32".parse().unwrap();
        let plane = ControlPlaneConfig {
            private_targets: PrivateTargets::try_new(vec![loopback]).unwrap(),
            ..self.plane.clone()
        };
        let connector = AzaleaConnector::new(&McConfig::default())
            .with_targets(AgentMode::ControlPlane(plane.clone()).target_policy());
        self.launch(options, connector, plane);
    }

    /// Runs the agent on `connector` with `plane`.
    fn launch<C>(&mut self, options: RunOptions, connector: C, plane: ControlPlaneConfig)
    where
        C: fleet_core::mc::MinecraftConnector,
    {
        assert!(self.running.is_none(), "the agent isn't running");
        let identity = ManagedIdentity::load(&plane, UnixTime::now()).unwrap();
        let config = AgentConfig {
            name: AgentName::try_from(self.name.as_str()).unwrap(),
            runtime: RuntimeConfig {
                max_bots: NonZeroUsize::new(options.capacity).unwrap(),
                ..RuntimeConfig::default()
            },
            mc: McConfig::default(),
            retry: options.retry,
            // As tests/control has it: a case's outages never trip the
            // circuit breaker.
            circuit: CircuitPolicy::try_new(
                NonZeroUsize::new(8).unwrap(),
                Duration::from_mins(10),
                Duration::from_mins(15),
            )
            .unwrap(),
            heartbeat_file: self.dir.join("agent.alive"),
            log: LogConfig::default(),
            mode: AgentMode::ControlPlane(plane),
        };
        let (signals, receiver) = mpsc::channel(4);
        let mut task = JoinSet::new();
        task.spawn(
            run(RunParts {
                config,
                connector: Arc::new(connector),
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
            connector: None,
            signals,
            task,
        });
    }

    fn running(&self) -> &Running {
        self.running.as_ref().expect("the agent runs")
    }

    /// The current run's fake connector.
    pub(crate) fn fake(&self) -> &FakeConnector {
        self.running()
            .connector
            .as_ref()
            .expect("the agent runs on the fake connector")
            .fake()
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
        let connector = running
            .connector
            .expect("a crash runs on the fake connector");
        ledger.dead(connector.run());
    }
}

/// Why an enrollment failed: a fixed text, and `enroll`'s own output for a
/// loopback case's message. The managed check shows only the text, since
/// the output names files and the server's address.
#[derive(Debug)]
pub(crate) struct EnrollError {
    /// What failed, in fixed words.
    pub(crate) why: &'static str,
    /// `enroll`'s stderr, if it ran.
    pub(crate) output: String,
}

impl EnrollError {
    fn of(why: &'static str) -> Self {
        Self {
            why,
            output: String::new(),
        }
    }
}

/// Enrolls the agent `name` with `server` through `bundle`, as
/// `afkfleet-agent enroll` does, its files in `dir/name`: the CA the server
/// signs with, and the key `random` makes with the certificate the server
/// signed for it; returns its `[control_plane]` for `url`. The whole
/// enrollment is bounded by `within`.
///
/// # Errors
/// [`EnrollError`] for each step that fails.
pub(crate) async fn enroll_files(
    server: &Server,
    dir: &Path,
    name: &str,
    url: &str,
    bundle: EnrollmentBundle,
    random: impl SecureRandom + 'static,
    within: Duration,
) -> Result<ControlPlaneConfig, EnrollError> {
    let dir = dir.join(name);
    let plane = ControlPlaneConfig {
        url: url.to_owned(),
        ca_cert_file: dir.join("ca.crt"),
        cert_file: dir.join("agent.crt"),
        key_file: dir.join("agent.key"),
        private_targets: PrivateTargets::default(),
        public_targets: true,
    };
    let name =
        AgentName::try_from(name).map_err(|_| EnrollError::of("the agent's name was refused"))?;
    let enrolled = {
        let plane = plane.clone();
        let ca = server.ca_cert_file();
        // `enroll` blocks on a runtime of its own, as the binary's command
        // does.
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&dir)
                .map_err(|_| EnrollError::of("the agent's folder couldn't be made"))?;
            std::fs::copy(ca, &plane.ca_cert_file)
                .map_err(|_| EnrollError::of("the CA couldn't be copied for the agent"))?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| EnrollError::of("the enrollment's runtime couldn't be built"))?;
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
            if exit == enroll::Exit::Enrolled {
                Ok(())
            } else {
                Err(EnrollError {
                    why: "the agent didn't enroll",
                    output: String::from_utf8_lossy(&err).into_owned(),
                })
            }
        })
    };
    match tokio::time::timeout(within, enrolled).await {
        Ok(Ok(Ok(()))) => Ok(plane),
        Ok(Ok(Err(error))) => Err(error),
        Ok(Err(_)) => Err(EnrollError::of("the enrollment's task failed")),
        Err(_) => Err(EnrollError::of("the enrollment didn't end in time")),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::server::ServerOptions;

    #[tokio::test]
    async fn an_enrollment_that_fails_is_a_fixed_text_never_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let mut server = Server::start(dir.path(), ServerOptions::default()).await;
        let bundle = server.bundle().await;
        // Nothing listens there.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://127.0.0.1:{}", closed.local_addr().unwrap().port());
        drop(closed);

        let enrolled = enroll_files(
            &server,
            dir.path(),
            "agent-a",
            &url,
            bundle,
            SeededRandom::new(1),
            PATIENCE,
        )
        .await;

        assert_eq!(enrolled.unwrap_err().why, "the agent didn't enroll");
        server.stop().await;
    }
}
