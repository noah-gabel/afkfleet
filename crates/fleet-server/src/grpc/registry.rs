//! [`AgentRegistry`]: each agent's one live stream (Plan.md P10.5; ADR-0019,
//! S25–S28, R13, R21).
//!
//! - **One stream per agent** (S26): a session claims its agent's slot once
//!   the hello passed its checks; a claim while the slot is taken is
//!   refused (`Reject(AlreadyConnected)`), and the live stream is untouched.
//!   The slot frees when the session drops its [`Lease`], before it sends
//!   its last answer and records its end.
//! - **Activation** (R13, S27): after the hello's transaction the session
//!   activates its claim with its queue. It fails when [`AgentRegistry::end`]
//!   ended the claim meanwhile, so a stream whose permission ended during
//!   its hello is never welcomed.
//! - **Ending** (S27): `end` throws the stream's switch, so nothing queued
//!   leaves any more and `send` and `hold` refuse it, and hands the
//!   session the reason, whose last answer it sends at once. A sweep every
//!   [`SWEEP_INTERVAL`] ends each stream whose certificate stopped admitting
//!   it, which is how a disable on the server's host reaches a running
//!   server.
//! - **Sending** (S25): [`AgentRegistry::send`] never waits. A message over
//!   [`MAX_SERVER_MESSAGE_BYTES`] is `TooLarge` and the stream stays; a full
//!   queue is `QueueFull` and ends the stream, so the agent resyncs.
//! - **Reports** (S28): every message an agent sends after `Welcome` passes
//!   [`AgentRegistry::admit`]. A status or an event for a bot the agent
//!   doesn't hold is dropped and counted; a releasing bot passes only its
//!   `Removed`, which ends the hold; a session request is denied, retryable,
//!   until P10.6 grants them. The holds stay empty until P10.5's reconciler
//!   fills them.
//! - **Conflicts** (S26): a message on the live stream after a refused claim
//!   proves two processes hold the identity: `admit` hands the session a
//!   [`Conflict`] to record, at most once every [`CONFLICT_INTERVAL`] per
//!   agent.
//!
//! One `std` mutex guards it all, never held across an `.await`.

use core::fmt;
use core::time::Duration;
use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use fleet_core::id::{AgentId, BotId};
use fleet_core::pki::CertFingerprint;
use fleet_core::system::Clock;
use fleet_core::value::AgentName;
use fleet_proto::control::{
    AgentMessage, BotEventKind, MAX_SERVER_MESSAGE_BYTES, ServerMessage, SessionRequest,
    server_message_len,
};
use fleet_proto::generated as wire;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::http::error::chain;
use crate::ports::agent_connections::{
    Admitted, AgentConnections, Conflict, LiveCertificate, StreamEnd,
};
use crate::ports::live_agents::{LiveAgents, LiveStream, PermissionEnd};

/// How often the sweep checks the live streams' certificates (S27).
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(5);

/// How often one agent's identity conflict is recorded at most (S26).
pub const CONFLICT_INTERVAL: Duration = Duration::from_hours(1);

/// The registry; clones share it. See the [module docs](self).
#[derive(Clone)]
pub struct AgentRegistry {
    shared: Arc<Shared>,
}

impl fmt::Debug for AgentRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentRegistry").finish_non_exhaustive()
    }
}

struct Shared {
    clock: Arc<dyn Clock>,
    limit: usize,
    state: Mutex<State>,
}

/// What the mutex guards.
#[derive(Default)]
struct State {
    /// The last generation handed out; the first is 1.
    generations: u64,
    slots: HashMap<AgentId, Slot>,
    /// The reports dropped since the last report, per agent.
    refused_reports: BTreeMap<AgentId, (AgentName, u64)>,
    /// When each agent's last conflict was handed out.
    conflicts: HashMap<AgentId, DateTime<Utc>>,
}

/// An agent's slot: claimed by a session, or live.
struct Slot {
    generation: u64,
    fingerprint: CertFingerprint,
    name: AgentName,
    phase: Phase,
    /// The latest claim refused since the live stream last spoke.
    refused: Option<RefusedClaim>,
}

/// A claim refused while the slot was taken.
#[derive(Clone, Copy)]
struct RefusedClaim {
    fingerprint: CertFingerprint,
    peer: Option<IpAddr>,
    at: DateTime<Utc>,
}

enum Phase {
    /// The hello is being accepted; `ended` once its permission ended.
    Claimed { ended: Option<StreamEnd> },
    /// Welcomed.
    Live(Live),
}

/// A welcomed stream.
struct Live {
    queue: mpsc::Sender<wire::ServerMessage>,
    /// The stream's switch, thrown when it's ended.
    halt: Halt,
    /// Taken by the first end.
    ender: Option<oneshot::Sender<StreamEnd>>,
    holds: HashMap<BotId, Hold>,
    connected_at: DateTime<Utc>,
    last_seen: DateTime<Utc>,
}

/// How an agent holds a bot (S28).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// The bot is assigned to the agent.
    Assigned,
    /// The bot was unassigned; only its `Removed` is still expected.
    Releasing,
}

/// A stream's switch: once thrown, the stream sends nothing more but its
/// last answer. The registry throws it when it ends the stream, the
/// session when it ends its own; clones share it.
#[derive(Debug, Clone, Default)]
pub struct Halt(Arc<AtomicBool>);

impl Halt {
    /// Throws the switch.
    pub fn halt(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether the switch is thrown.
    #[must_use]
    pub fn is_halted(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// A session's claim on its agent's slot; dropping it frees the slot.
pub struct Lease {
    shared: Arc<Shared>,
    agent: AgentId,
    generation: u64,
}

impl fmt::Debug for Lease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lease")
            .field("agent", &self.agent)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl Lease {
    /// The agent whose slot it holds.
    #[must_use]
    pub const fn agent(&self) -> AgentId {
        self.agent
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = lock(&self.shared);
        if state
            .slots
            .get(&self.agent)
            .is_some_and(|slot| slot.generation == self.generation)
        {
            state.slots.remove(&self.agent);
        }
    }
}

/// The agent has a stream already, or one being welcomed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the agent has a stream already")]
pub struct AlreadyConnected;

/// Why a message wasn't queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    /// The agent has no live stream.
    #[error("the agent isn't connected")]
    NotConnected,
    /// The message is larger than an agent reads; the stream stays.
    #[error("the message is larger than an agent reads")]
    TooLarge,
    /// The agent's queue was full: its stream ends, and it resyncs.
    #[error("the agent's queue is full")]
    QueueFull,
    /// The message doesn't convert to the wire.
    #[error("the message can't be written")]
    Unwritable,
}

/// What happens to a message an agent sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// It passes.
    Pass,
    /// A report about a bot the agent doesn't hold: dropped and counted.
    Dropped,
    /// A session request, which is denied, retryable, until P10.6.
    DenySession(SessionRequest),
}

/// How many reports one agent sent that were dropped since the last
/// report (S28, R21).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedReports {
    /// The agent.
    pub agent: AgentId,
    /// Its name.
    pub name: AgentName,
    /// How many.
    pub count: u64,
}

impl AgentRegistry {
    /// An empty registry, stamping with `clock`.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self::with_limit(clock, MAX_SERVER_MESSAGE_BYTES)
    }

    /// An empty registry whose messages may be at most `limit` bytes.
    fn with_limit(clock: Arc<dyn Clock>, limit: usize) -> Self {
        Self {
            shared: Arc::new(Shared {
                clock,
                limit,
                state: Mutex::new(State::default()),
            }),
        }
    }

    /// Claims `admitted`'s agent's slot.
    ///
    /// # Errors
    /// [`AlreadyConnected`] while the slot is taken; the refusal is kept,
    /// and the live stream's next message makes it a [`Conflict`].
    pub fn claim(&self, admitted: &Admitted) -> Result<Lease, AlreadyConnected> {
        let now = self.shared.clock.now();
        let mut state = self.state();
        if let Some(slot) = state.slots.get_mut(&admitted.agent) {
            slot.refused = Some(RefusedClaim {
                fingerprint: admitted.fingerprint,
                peer: admitted.peer,
                at: now,
            });
            return Err(AlreadyConnected);
        }
        state.generations = state.generations.saturating_add(1);
        let generation = state.generations;
        state.slots.insert(
            admitted.agent,
            Slot {
                generation,
                fingerprint: admitted.fingerprint,
                name: admitted.name.clone(),
                phase: Phase::Claimed { ended: None },
                refused: None,
            },
        );
        Ok(Lease {
            shared: Arc::clone(&self.shared),
            agent: admitted.agent,
            generation,
        })
    }

    /// Makes `lease`'s claim live with `queue` and its switch `halt`,
    /// welcomed at `connected_at`: the receiver gets the reason when the
    /// stream is ended.
    ///
    /// # Errors
    /// The reason the claim was ended with meanwhile.
    pub fn activate(
        &self,
        lease: &Lease,
        queue: mpsc::Sender<wire::ServerMessage>,
        halt: Halt,
        connected_at: DateTime<Utc>,
    ) -> Result<oneshot::Receiver<StreamEnd>, StreamEnd> {
        let mut state = self.state();
        let Some(slot) = state.slots.get_mut(&lease.agent) else {
            return Err(StreamEnd::StreamError);
        };
        if slot.generation != lease.generation {
            return Err(StreamEnd::StreamError);
        }
        match slot.phase {
            Phase::Claimed {
                ended: Some(reason),
            } => Err(reason),
            Phase::Live(_) => Err(StreamEnd::StreamError),
            Phase::Claimed { ended: None } => {
                let (ender, notice) = oneshot::channel();
                slot.phase = Phase::Live(Live {
                    queue,
                    halt,
                    ender: Some(ender),
                    holds: HashMap::new(),
                    connected_at,
                    last_seen: connected_at,
                });
                Ok(notice)
            }
        }
    }

    /// Ends `agent`'s stream, or its claim, for `reason`. Whether there was
    /// one.
    #[must_use = "whether the agent had a stream"]
    pub fn end(&self, agent: AgentId, reason: StreamEnd) -> bool {
        let mut state = self.state();
        let Some(slot) = state.slots.get_mut(&agent) else {
            return false;
        };
        end_slot(slot, reason);
        true
    }

    /// Queues `message` for `agent`'s stream; see the [module docs](self).
    ///
    /// # Errors
    /// [`SendError`].
    pub fn send(&self, agent: AgentId, message: ServerMessage) -> Result<(), SendError> {
        let message = wire::ServerMessage::try_from(message).map_err(|_| SendError::Unwritable)?;
        if server_message_len(&message) > self.shared.limit {
            return Err(SendError::TooLarge);
        }
        let mut state = self.state();
        let Some(slot) = state.slots.get_mut(&agent) else {
            return Err(SendError::NotConnected);
        };
        let Phase::Live(live) = &mut slot.phase else {
            return Err(SendError::NotConnected);
        };
        if live.halt.is_halted() {
            return Err(SendError::NotConnected);
        }
        match live.queue.try_send(message) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                end_slot(slot, StreamEnd::TooSlow);
                Err(SendError::QueueFull)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(SendError::NotConnected),
        }
    }

    /// Judges `message` from `lease`'s stream, and notes that the agent was
    /// heard from: what happens to it, and a conflict to record, if it
    /// proves one.
    #[must_use]
    pub fn admit(&self, lease: &Lease, message: &AgentMessage) -> (Verdict, Option<Conflict>) {
        let now = self.shared.clock.now();
        let mut guard = self.state();
        let state = &mut *guard;
        let Some(slot) = state
            .slots
            .get_mut(&lease.agent)
            .filter(|slot| slot.generation == lease.generation)
        else {
            return (Verdict::Dropped, None);
        };
        let Phase::Live(live) = &mut slot.phase else {
            return (Verdict::Dropped, None);
        };
        live.last_seen = now;
        let conflict = slot.refused.take().and_then(|refused| {
            let since = fleet_core::time::sub(now, CONFLICT_INTERVAL);
            let recent = state
                .conflicts
                .get(&lease.agent)
                .is_some_and(|last| *last > since);
            if recent {
                return None;
            }
            state.conflicts.insert(lease.agent, now);
            Some(Conflict {
                fingerprint: refused.fingerprint,
                peer: refused.peer,
                refused_at: refused.at,
                connected_at: live.connected_at,
            })
        });
        let verdict = judge(&mut live.holds, message);
        if verdict == Verdict::Dropped {
            let (_, count) = state
                .refused_reports
                .entry(lease.agent)
                .or_insert_with(|| (slot.name.clone(), 0));
            *count = count.saturating_add(1);
        }
        (verdict, conflict)
    }

    /// Marks `bot` as assigned to `agent`'s live stream. Whether it has one.
    #[must_use = "whether the agent had a stream"]
    pub fn hold(&self, agent: AgentId, bot: BotId) -> bool {
        let mut state = self.state();
        match state.slots.get_mut(&agent).map(|slot| &mut slot.phase) {
            Some(Phase::Live(live)) if !live.halt.is_halted() => {
                live.holds.insert(bot, Hold::Assigned);
                true
            }
            _ => false,
        }
    }

    /// Marks `bot` as unassigned from `agent`'s live stream, which still
    /// reports its `Removed`. Whether it held it.
    #[must_use = "whether the agent held the bot"]
    pub fn release(&self, agent: AgentId, bot: BotId) -> bool {
        let mut state = self.state();
        match state.slots.get_mut(&agent).map(|slot| &mut slot.phase) {
            Some(Phase::Live(live)) if !live.halt.is_halted() => match live.holds.get_mut(&bot) {
                Some(hold) => {
                    *hold = Hold::Releasing;
                    true
                }
                None => false,
            },
            _ => false,
        }
    }

    /// The reports dropped since the last call, per agent.
    #[must_use]
    pub fn take_refused_reports(&self) -> Vec<RefusedReports> {
        core::mem::take(&mut self.state().refused_reports)
            .into_iter()
            .map(|(agent, (name, count))| RefusedReports { agent, name, count })
            .collect()
    }

    /// The certificates the live streams connected with.
    #[must_use]
    pub fn live_certificates(&self) -> Vec<LiveCertificate> {
        self.state()
            .slots
            .iter()
            .filter(|(_, slot)| matches!(slot.phase, Phase::Live(_)))
            .map(|(agent, slot)| LiveCertificate {
                agent: *agent,
                fingerprint: slot.fingerprint,
            })
            .collect()
    }

    /// One sweep: asks `connections` which live streams lost their
    /// permission, and ends them; skipped without a live stream. A failed
    /// read keeps every stream.
    pub async fn sweep_once(&self, connections: &dyn AgentConnections) {
        let swept = self.live_certificates();
        if swept.is_empty() {
            return;
        }
        // The store's calls are bounded by the pools' acquire timeout and
        // SQLite's busy timeout.
        match connections.sweep(&swept).await {
            Ok(ends) => self.end_swept(&swept, &ends),
            Err(failure) => warn!(
                error = %chain(&failure),
                "the agents' certificates couldn't be checked; their streams stay until the next sweep"
            ),
        }
    }

    /// Sweeps every `period` until `stop` is cancelled.
    pub async fn sweep_every(
        self,
        connections: Arc<dyn AgentConnections>,
        period: Duration,
        stop: CancellationToken,
    ) {
        let mut ticks = interval_at(Instant::now() + period, period);
        // A sweep that took longer than a period isn't followed by a burst.
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                // Cancel-safe: waiting for the token loses nothing.
                () = stop.cancelled() => return,
                // Cancel-safe, as tokio documents `Interval::tick`.
                _ = ticks.tick() => self.sweep_once(connections.as_ref()).await,
            }
        }
    }

    /// Ends each of `ends` whose stream still has the certificate `swept` found
    /// it with, so a newer stream of the same agent stays.
    fn end_swept(&self, swept: &[LiveCertificate], ends: &[(AgentId, PermissionEnd)]) {
        let mut state = self.state();
        for (agent, why) in ends {
            let Some(certificate) = swept.iter().find(|swept| swept.agent == *agent) else {
                continue;
            };
            let Some(slot) = state
                .slots
                .get_mut(agent)
                .filter(|slot| slot.fingerprint == certificate.fingerprint)
            else {
                continue;
            };
            info!(
                agent_id = %agent,
                agent = %slot.name,
                reason = StreamEnd::from(*why).as_str(),
                "the sweep ended an agent's stream"
            );
            end_slot(slot, (*why).into());
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.shared)
    }
}

/// `shared`'s state; every section leaves it consistent, so a poisoned
/// lock's value is still good.
fn lock(shared: &Shared) -> MutexGuard<'_, State> {
    shared.state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl LiveAgents for AgentRegistry {
    fn end(&self, agent: AgentId, reason: PermissionEnd) -> bool {
        Self::end(self, agent, reason.into())
    }

    fn live(&self, agent: AgentId) -> Option<LiveStream> {
        match &self.state().slots.get(&agent)?.phase {
            Phase::Live(live) => Some(LiveStream {
                connected_at: live.connected_at,
                last_seen: live.last_seen,
            }),
            Phase::Claimed { .. } => None,
        }
    }
}

/// Ends `slot`'s stream for `reason`: a claim is never activated; a live
/// stream's switch is thrown and its session gets the reason. Only the
/// first reason counts.
fn end_slot(slot: &mut Slot, reason: StreamEnd) {
    match &mut slot.phase {
        Phase::Claimed { ended } => {
            ended.get_or_insert(reason);
        }
        Phase::Live(live) => {
            live.halt.halt();
            if let Some(ender) = live.ender.take() {
                // A session that's ending dropped its receiver: nothing to tell.
                let _ = ender.send(reason);
            }
        }
    }
}

/// What happens to `message` from an agent holding `holds` (S28).
fn judge(holds: &mut HashMap<BotId, Hold>, message: &AgentMessage) -> Verdict {
    match message {
        AgentMessage::Hello(_) | AgentMessage::Heartbeat => Verdict::Pass,
        AgentMessage::SessionRequest(request) => Verdict::DenySession(*request),
        AgentMessage::Status(snapshot) => match holds.get(&snapshot.bot_id) {
            Some(Hold::Assigned) => Verdict::Pass,
            Some(Hold::Releasing) | None => Verdict::Dropped,
        },
        AgentMessage::Event(event) => match (holds.get(&event.bot_id), &event.kind) {
            (Some(Hold::Assigned), _) => Verdict::Pass,
            (Some(Hold::Releasing), BotEventKind::Removed) => {
                holds.remove(&event.bot_id);
                Verdict::Pass
            }
            _ => Verdict::Dropped,
        },
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU64;
    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;
    use fleet_core::bot::{BotSnapshot, BotState};
    use fleet_core::id::RequestId;
    use fleet_core::value::{AgentCapacity, AgentVersion};
    use fleet_proto::control::{BotEvent, BotEventKind, SessionRequestId};
    use fleet_testkit::system::ManualClock;

    use super::*;
    use crate::ports::agent_connections::{AdmitError, Ended, Presented};

    const T0: &str = "2027-01-15T08:00:00Z";

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    fn registry() -> (AgentRegistry, ManualClock) {
        let clock = ManualClock::new(at(T0));
        (AgentRegistry::new(Arc::new(clock.clone())), clock)
    }

    fn agent(n: u8) -> AgentId {
        AgentId::new_v7(at(T0), [n; 10]).unwrap()
    }

    fn bot(n: u8) -> BotId {
        BotId::new_v7(at(T0), [n; 10]).unwrap()
    }

    /// Agent `n` admitted with certificate `n` from `192.0.2.<n>`.
    fn admitted(n: u8) -> Admitted {
        Admitted {
            request: RequestId::new_v7(at(T0), [n; 10]).unwrap(),
            agent: agent(1),
            name: AgentName::try_from("agent-1").unwrap(),
            fingerprint: CertFingerprint::from_digest([n; 32]),
            not_after: at("2027-03-16T08:00:00Z"),
            peer: Some(IpAddr::from([192, 0, 2, n])),
        }
    }

    /// Claims and activates `admitted(n)` with a queue of `depth`.
    fn live(
        registry: &AgentRegistry,
        n: u8,
        depth: usize,
    ) -> (
        Lease,
        mpsc::Receiver<wire::ServerMessage>,
        oneshot::Receiver<StreamEnd>,
    ) {
        let lease = registry.claim(&admitted(n)).unwrap();
        let (queue, queued) = mpsc::channel(depth);
        let ended = registry
            .activate(&lease, queue, Halt::default(), at(T0))
            .unwrap();
        (lease, queued, ended)
    }

    fn status(bot: BotId) -> AgentMessage {
        AgentMessage::Status(BotSnapshot {
            bot_id: bot,
            state: BotState::Stopped,
            since: at(T0),
            last_disconnect: None,
        })
    }

    fn event(bot: BotId, kind: BotEventKind) -> AgentMessage {
        AgentMessage::Event(BotEvent {
            bot_id: bot,
            at: at(T0),
            kind,
        })
    }

    fn unassign(n: u8) -> ServerMessage {
        ServerMessage::Unassign(bot(n))
    }

    // --- One stream per agent -------------------------------------------------------------

    #[test]
    fn a_second_claim_is_refused_while_the_first_is_claimed_or_live() {
        let (registry, _) = registry();
        let first = registry.claim(&admitted(1)).unwrap();

        let while_claimed = registry.claim(&admitted(2));
        let (queue, _queued) = mpsc::channel(1);
        let _ended = registry
            .activate(&first, queue, Halt::default(), at(T0))
            .unwrap();
        let while_live = registry.claim(&admitted(2));

        assert!(
            matches!(while_claimed, Err(AlreadyConnected)),
            "{while_claimed:?}"
        );
        assert!(
            matches!(while_live, Err(AlreadyConnected)),
            "{while_live:?}"
        );
    }

    #[test]
    fn a_dropped_lease_frees_the_slot() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 1);

        drop(lease);

        assert!(registry.claim(&admitted(2)).is_ok());
        assert_eq!(LiveAgents::live(&registry, agent(1)), None);
    }

    // --- Ending -----------------------------------------------------------------------------

    #[test]
    fn a_claim_ended_meanwhile_is_never_activated() {
        let (registry, _) = registry();
        let lease = registry.claim(&admitted(1)).unwrap();

        let found = registry.end(agent(1), StreamEnd::Disabled);
        let (queue, _queued) = mpsc::channel(1);
        let activated = registry.activate(&lease, queue, Halt::default(), at(T0));

        assert!(found);
        assert_eq!(activated.unwrap_err(), StreamEnd::Disabled);
    }

    #[test]
    fn ending_a_live_stream_hands_its_session_the_reason_once() {
        let (registry, _) = registry();
        let (_lease, _queued, mut ended) = live(&registry, 1, 1);

        let found = LiveAgents::end(&registry, agent(1), PermissionEnd::CertificateRevoked);
        let again = registry.end(agent(1), StreamEnd::Disabled);

        assert!(found);
        assert!(again, "the stream is still there until its session ends");
        assert_eq!(ended.try_recv().unwrap(), StreamEnd::CertificateRevoked);
        assert!(!registry.end(agent(9), StreamEnd::Disabled));
    }

    #[test]
    fn an_ended_stream_takes_no_more_messages_or_bots_and_its_switch_is_thrown() {
        let (registry, _) = registry();
        let lease = registry.claim(&admitted(1)).unwrap();
        let (queue, mut queued) = mpsc::channel(4);
        let halt = Halt::default();
        let _ended = registry
            .activate(&lease, queue, halt.clone(), at(T0))
            .unwrap();
        assert!(registry.hold(agent(1), bot(1)));

        assert!(registry.end(agent(1), StreamEnd::Disabled));

        assert!(halt.is_halted());
        assert_eq!(
            registry.send(agent(1), unassign(1)),
            Err(SendError::NotConnected)
        );
        assert!(!registry.hold(agent(1), bot(2)));
        assert!(!registry.release(agent(1), bot(1)));
        assert!(queued.try_recv().is_err());
    }

    #[test]
    fn a_live_stream_shows_when_it_connected_and_was_last_heard_from() {
        let (registry, clock) = registry();
        let lease = registry.claim(&admitted(1)).unwrap();
        assert_eq!(
            LiveAgents::live(&registry, agent(1)),
            None,
            "claimed isn't live"
        );
        let (queue, _queued) = mpsc::channel(1);
        let _ended = registry
            .activate(&lease, queue, Halt::default(), at(T0))
            .unwrap();
        clock.advance(Duration::from_secs(5));

        let _ = registry.admit(&lease, &AgentMessage::Heartbeat);

        assert_eq!(
            LiveAgents::live(&registry, agent(1)),
            Some(LiveStream {
                connected_at: at(T0),
                last_seen: at("2027-01-15T08:00:05Z"),
            })
        );
        assert_eq!(
            registry.live_certificates(),
            vec![LiveCertificate {
                agent: agent(1),
                fingerprint: CertFingerprint::from_digest([1; 32]),
            }]
        );
    }

    // --- Sending ----------------------------------------------------------------------------

    #[test]
    fn a_message_is_queued_and_a_full_queue_ends_the_stream_too_slow() {
        let (registry, _) = registry();
        let (_lease, mut queued, mut ended) = live(&registry, 1, 1);

        let first = registry.send(agent(1), unassign(1));
        let second = registry.send(agent(1), unassign(2));

        assert_eq!(first, Ok(()));
        assert_eq!(second, Err(SendError::QueueFull));
        assert_eq!(
            queued.try_recv().unwrap(),
            wire::ServerMessage::try_from(unassign(1)).unwrap()
        );
        assert_eq!(ended.try_recv().unwrap(), StreamEnd::TooSlow);
        assert_eq!(
            registry.send(agent(9), unassign(1)),
            Err(SendError::NotConnected)
        );
    }

    #[test]
    fn a_message_over_the_limit_is_too_large_and_the_stream_stays() {
        let clock = ManualClock::new(at(T0));
        let registry = AgentRegistry::with_limit(Arc::new(clock), 4);
        let (_lease, mut queued, mut ended) = live(&registry, 1, 1);

        let sent = registry.send(agent(1), unassign(1));

        assert_eq!(sent, Err(SendError::TooLarge));
        assert!(queued.try_recv().is_err());
        assert!(ended.try_recv().is_err());
    }

    // --- Reports ----------------------------------------------------------------------------

    #[test]
    fn reports_about_bots_the_agent_doesnt_hold_are_dropped_and_counted() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 1);

        let verdicts = [
            registry.admit(&lease, &status(bot(1))).0,
            registry.admit(&lease, &event(bot(1), BotEventKind::Died)).0,
            registry
                .admit(&lease, &event(bot(2), BotEventKind::Removed))
                .0,
            registry.admit(&lease, &AgentMessage::Heartbeat).0,
        ];
        let refused = registry.take_refused_reports();

        assert_eq!(
            verdicts,
            [
                Verdict::Dropped,
                Verdict::Dropped,
                Verdict::Dropped,
                Verdict::Pass
            ]
        );
        assert_eq!(
            refused,
            vec![RefusedReports {
                agent: agent(1),
                name: AgentName::try_from("agent-1").unwrap(),
                count: 3,
            }]
        );
        assert_eq!(registry.take_refused_reports(), Vec::new());
    }

    #[test]
    fn an_assigned_bots_reports_pass() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 1);

        assert!(registry.hold(agent(1), bot(1)));
        let verdicts = [
            registry.admit(&lease, &status(bot(1))).0,
            registry.admit(&lease, &event(bot(1), BotEventKind::Died)).0,
        ];

        assert_eq!(verdicts, [Verdict::Pass, Verdict::Pass]);
        assert!(!registry.hold(agent(9), bot(1)), "no live stream");
    }

    #[test]
    fn a_releasing_bot_passes_only_its_removed_which_ends_the_hold() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 1);
        assert!(registry.hold(agent(1), bot(1)));

        assert!(registry.release(agent(1), bot(1)));
        let verdicts = [
            registry.admit(&lease, &status(bot(1))).0,
            registry.admit(&lease, &event(bot(1), BotEventKind::Died)).0,
            registry
                .admit(&lease, &event(bot(1), BotEventKind::Removed))
                .0,
            registry
                .admit(&lease, &event(bot(1), BotEventKind::Removed))
                .0,
        ];

        assert_eq!(
            verdicts,
            [
                Verdict::Dropped,
                Verdict::Dropped,
                Verdict::Pass,
                Verdict::Dropped
            ]
        );
        assert!(!registry.release(agent(1), bot(2)), "never held");
    }

    #[test]
    fn a_session_request_is_denied() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 1);
        let request = SessionRequest {
            id: SessionRequestId::new(NonZeroU64::MIN),
            bot_id: bot(1),
            fresh: false,
        };

        let (verdict, _) = registry.admit(&lease, &AgentMessage::SessionRequest(request));

        assert_eq!(verdict, Verdict::DenySession(request));
    }

    // --- Conflicts --------------------------------------------------------------------------

    #[test]
    fn a_message_after_a_refused_claim_is_a_conflict_once_an_hour() {
        let (registry, clock) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 1);
        clock.advance(Duration::from_secs(2));
        let _ = registry.claim(&admitted(2));

        let first = registry.admit(&lease, &AgentMessage::Heartbeat).1;
        let quiet = registry.admit(&lease, &AgentMessage::Heartbeat).1;
        let _ = registry.claim(&admitted(3));
        let within_the_hour = registry.admit(&lease, &AgentMessage::Heartbeat).1;
        clock.advance(CONFLICT_INTERVAL);
        let _ = registry.claim(&admitted(3));
        let an_hour_later = registry.admit(&lease, &AgentMessage::Heartbeat).1;

        assert_eq!(
            first,
            Some(Conflict {
                fingerprint: CertFingerprint::from_digest([2; 32]),
                peer: Some(IpAddr::from([192, 0, 2, 2])),
                refused_at: at("2027-01-15T08:00:02Z"),
                connected_at: at(T0),
            })
        );
        assert_eq!(quiet, None, "no refusal since the last message");
        assert_eq!(within_the_hour, None);
        assert!(an_hour_later.is_some());
    }

    #[test]
    fn a_refused_claim_the_live_stream_never_answers_is_no_conflict() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 1);
        let _ = registry.claim(&admitted(2));

        // The old process is gone: its stream ends without a word.
        drop(lease);
        let next = registry.claim(&admitted(2)).unwrap();
        let (queue, _queued) = mpsc::channel(1);
        let _ended = registry
            .activate(&next, queue, Halt::default(), at(T0))
            .unwrap();

        assert_eq!(registry.admit(&next, &AgentMessage::Heartbeat).1, None);
    }

    // --- The sweep --------------------------------------------------------------------------

    /// A fake that names the streams to end, or fails, and counts its calls.
    #[derive(Debug, Default)]
    struct Sweeper {
        ends: StdMutex<Vec<(AgentId, PermissionEnd)>>,
        fails: bool,
        calls: StdMutex<Vec<Vec<LiveCertificate>>>,
    }

    #[async_trait]
    impl AgentConnections for Sweeper {
        async fn admit(&self, _presented: Presented) -> Result<Admitted, AdmitError> {
            Err(AdmitError::Refused)
        }

        async fn hello(
            &self,
            _admitted: &Admitted,
            _capacity: AgentCapacity,
            _version: &AgentVersion,
        ) -> Result<(), AdmitError> {
            Ok(())
        }

        async fn name_refused(&self, _admitted: &Admitted) {}

        async fn ended(&self, _admitted: &Admitted, _ended: Ended) {}

        async fn conflict(&self, _admitted: &Admitted, _conflict: Conflict) {}

        async fn sweep(
            &self,
            live: &[LiveCertificate],
        ) -> Result<Vec<(AgentId, PermissionEnd)>, AdmitError> {
            self.calls.lock().unwrap().push(live.to_vec());
            if self.fails {
                return Err(AdmitError::Busy);
            }
            Ok(self.ends.lock().unwrap().clone())
        }
    }

    #[tokio::test]
    async fn the_sweep_ends_the_streams_the_app_names_and_skips_without_any() {
        let (registry, _) = registry();
        let sweeper = Sweeper::default();
        registry.sweep_once(&sweeper).await;
        assert_eq!(sweeper.calls.lock().unwrap().len(), 0, "no live stream");
        let (_lease, _queued, mut ended) = live(&registry, 1, 1);
        sweeper
            .ends
            .lock()
            .unwrap()
            .push((agent(1), PermissionEnd::Disabled));

        registry.sweep_once(&sweeper).await;

        assert_eq!(sweeper.calls.lock().unwrap().len(), 1);
        assert_eq!(ended.try_recv().unwrap(), StreamEnd::Disabled);
    }

    #[tokio::test]
    async fn a_failed_sweep_keeps_every_stream() {
        let (registry, _) = registry();
        let sweeper = Sweeper {
            fails: true,
            ..Sweeper::default()
        };
        let (_lease, _queued, mut ended) = live(&registry, 1, 1);

        registry.sweep_once(&sweeper).await;

        assert_eq!(sweeper.calls.lock().unwrap().len(), 1);
        assert!(ended.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_sweep_never_ends_a_newer_stream_of_the_same_agent() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 1);
        let swept = registry.live_certificates();
        drop(lease);
        let (_lease, _queued, mut ended) = live(&registry, 2, 1);

        registry.end_swept(&swept, &[(agent(1), PermissionEnd::CertificateRevoked)]);

        assert!(
            ended.try_recv().is_err(),
            "the new stream's certificate wasn't swept"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_sweep_runs_every_period_until_stopped() {
        let (registry, _) = registry();
        let sweeper = Arc::new(Sweeper::default());
        let (_lease, _queued, _ended) = live(&registry, 1, 1);
        let stop = CancellationToken::new();
        let task = tokio::spawn(registry.clone().sweep_every(
            sweeper.clone(),
            Duration::from_secs(5),
            stop.clone(),
        ));

        tokio::time::sleep(Duration::from_millis(12_500)).await;
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(sweeper.calls.lock().unwrap().len(), 2);
    }
}
