//! A managed agent's run (Plan.md P10.7; ADR-0019, S62–S68, R47–R57): the
//! server's control stream drives the fleet.
//!
//! 1. The fleet starts with [`ManagedCredentials`] over the [`Outbox`]'s
//!    lane, and the run subscribes to its events before its supervisor
//!    runs, so the conductor misses none.
//! 2. The conductor and the link run in tasks the run owns, beside the
//!    supervisor, the chat logger and the heartbeat, which starts at once:
//!    a managed agent runs no bot until the server gives it one.
//! 3. The watch ends on a signal, the abandoned-thread limit, the
//!    supervisor's end, the server's refusal (exit 5, every bot stopped
//!    first), or the conductor's or the link's unasked end (exit 4).
//! 4. The link and the conductor stop first, so no instruction reaches the
//!    fleet while it shuts down, and the outbox closes the stream the link
//!    may have left open, so the server sees it end; then the fleet's
//!    shutdown, as standalone.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::mc::MinecraftConnector;
use fleet_core::value::{AgentCapacity, AgentVersion};
use fleet_runtime::{Fleet, FleetParts, RuntimeClock, Supervisor};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tonic::transport::Uri;
use tracing::{Instrument as _, error, info};

use super::finish::{Timeouts, finish};
use super::heartbeat::beat;
use super::specs::StartupError;
use super::watch::{ManagedTasks, watch};
use super::{Managed, Outcome, RunParts, events};
use crate::config::{AgentConfig, ControlPlaneConfig};
use crate::diagnostics::{self, HostDiagnostics};
use crate::enroll::client::server_name;
use crate::heartbeat::Heartbeat;
use crate::managed::conductor::{self, Conductor, ConductorParts, Inbox};
use crate::managed::credentials::ManagedCredentials;
use crate::managed::link::backoff::{self, Backoff};
use crate::managed::link::{self, HelloParts, Link, LinkParts};
use crate::managed::outbound::Outbox;
use crate::signals::ShutdownSignals;

/// Runs a managed agent for `plane`; see the [module docs](self).
pub(super) async fn run_managed<C, D, S, H>(
    parts: RunParts<C, D, S, H>,
    plane: &ControlPlaneConfig,
) -> Outcome
where
    C: MinecraftConnector,
    D: HostDiagnostics,
    S: ShutdownSignals,
    H: Heartbeat,
{
    let RunParts {
        config,
        connector,
        diagnostics,
        mut signals,
        heartbeat,
        anchor,
        seed,
        managed,
    } = parts;
    let Built {
        fleet,
        supervisor,
        conductor,
        link,
        inbox,
        outbox,
        credentials,
    } = match build(&config, plane, connector, anchor, seed, managed) {
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
    supervisor_task.spawn(supervisor.run(cancel.child_token()).in_current_span());
    let tasks_stop = cancel.child_token();
    let mut tasks = ManagedTasks {
        conductor: JoinSet::new(),
        link: JoinSet::new(),
    };
    tasks
        .conductor
        .spawn(conductor.run(tasks_stop.child_token()).in_current_span());
    tasks
        .link
        .spawn(link.run(tasks_stop.child_token()).in_current_span());
    info!(
        max_bots = config.runtime.max_bots.get(),
        "the agent is running in managed mode"
    );
    let beats = cancel.child_token();
    let mut heartbeat_task = JoinSet::new();
    heartbeat_task.spawn(
        beat(
            fleet.clone(),
            heartbeat,
            config.heartbeat_file.clone(),
            beats.clone(),
        )
        .in_current_span(),
    );
    let reason = watch(
        &mut supervisor_task,
        Some(&mut tasks),
        &mut signals,
        &diagnostics,
        config.mc.max_abandoned_threads,
    )
    .await;
    beats.cancel();
    heartbeat_task.shutdown().await;
    // No instruction reaches the fleet while it shuts down.
    tasks_stop.cancel();
    tasks.link.shutdown().await;
    // The aborted link may leave its stream open, and hyper would keep
    // sending its heartbeats while the bots stop: the server sees it end.
    if let Some(generation) = outbox.close() {
        credentials.link_ended(generation);
    }
    tasks.conductor.shutdown().await;
    drop(inbox);
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

/// A managed agent's parts, not running yet.
struct Built<C> {
    fleet: Fleet,
    supervisor: Supervisor<C, ManagedCredentials<Outbox>>,
    conductor: Conductor,
    link: Link,
    /// Kept until the end, so the conductor's FIFO never closes before.
    inbox: Inbox,
    /// The outbox, whose stream the run's end closes.
    outbox: Outbox,
    /// The credentials, told of that end.
    credentials: Arc<ManagedCredentials<Outbox>>,
}

/// Builds a managed agent's fleet, conductor and link.
///
/// # Errors
/// [`StartupError::NoIdentity`] without `managed`, then
/// [`StartupError::Setup`], [`StartupError::ServerUrl`],
/// [`StartupError::Reconnect`], [`StartupError::Capacity`] and
/// [`StartupError::Version`].
fn build<C: MinecraftConnector>(
    config: &AgentConfig,
    plane: &ControlPlaneConfig,
    connector: Arc<C>,
    anchor: DateTime<Utc>,
    seed: u64,
    managed: Option<Managed>,
) -> Result<Built<C>, StartupError> {
    let managed = managed.ok_or(StartupError::NoIdentity)?;
    let runtime = config.runtime;
    // One clock for the credentials, the conductor and the link.
    let clock = RuntimeClock::new(anchor);
    let outbox = Outbox::new(runtime.max_bots, runtime.chat_queue);
    let credentials = Arc::new(ManagedCredentials::new(
        outbox.clone(),
        clock,
        runtime.connect_timeout,
        runtime.session_request_timeout,
    ));
    let (fleet, supervisor) = Fleet::new(FleetParts {
        connector,
        credentials: Arc::clone(&credentials),
        retry: config.retry,
        circuit: config.circuit,
        config: runtime,
        anchor,
        seed,
    })
    .map_err(StartupError::Setup)?;
    diagnostics::register();
    // Before the supervisor runs, so the conductor misses no event.
    let events = fleet.subscribe();
    let (inbox, instructions) = conductor::inbox();
    let conductor = Conductor::new(ConductorParts {
        fleet: fleet.clone(),
        events,
        outbox: outbox.clone(),
        credentials: Arc::clone(&credentials),
        inbound: instructions,
        clock,
        max_bots: runtime.max_bots.get(),
        chat_queue: runtime.chat_queue.get(),
        event_buffer: runtime.event_buffer.get(),
    });
    let domain = plane
        .url
        .parse::<Uri>()
        .ok()
        .as_ref()
        .and_then(server_name)
        .ok_or(StartupError::ServerUrl)?;
    let (tls, kept) = managed.identity.into_tls(domain);
    let endpoint =
        link::endpoint(&plane.url, tls, managed.timings).map_err(|_| StartupError::ServerUrl)?;
    let policy = backoff::policy().map_err(StartupError::Reconnect)?;
    let capacity = u32::try_from(runtime.max_bots.get())
        .ok()
        .and_then(|bots| AgentCapacity::try_from(bots).ok())
        .ok_or(StartupError::Capacity)?;
    let agent_version =
        AgentVersion::try_from(env!("CARGO_PKG_VERSION")).map_err(|_| StartupError::Version)?;
    let link = Link::new(LinkParts {
        endpoint,
        kept,
        hello: HelloParts {
            name: config.name.clone(),
            agent_version,
            capacity,
        },
        outbox: outbox.clone(),
        credentials: Arc::clone(&credentials),
        inbox: inbox.clone(),
        clock,
        backoff: Backoff::new(policy, managed.seed),
        timings: managed.timings,
    });
    Ok(Built {
        fleet,
        supervisor,
        conductor,
        link,
        inbox,
        outbox,
        credentials,
    })
}
