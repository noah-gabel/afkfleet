//! [`AgentRegistry`]: each agent's one live stream (Plan.md P10.5; ADR-0019,
//! S25–S28, S32–S45, R13, R21).
//!
//! - **One stream per agent** (S26): a session claims its agent's slot once
//!   the hello passed its checks; a claim while the slot is taken is
//!   refused (`Reject(AlreadyConnected)`), and the live stream is untouched.
//!   The slot frees when the session frees its [`Lease`], before it sends
//!   its last answer and records its end.
//! - **Activation** (R13, S27): after the hello's transaction the session
//!   activates its claim with its queue, its `Welcome` and its hello's
//!   facts, all under one lock: `Welcome` is the stream's first message, and
//!   the bots its hello listed fence every other agent from that moment. It
//!   fails when [`AgentRegistry::end`] ended the claim meanwhile, so a stream
//!   whose permission ended during its hello is never welcomed.
//! - **Ending** (S27): `end` throws the stream's switch, so nothing queued
//!   leaves any more and nothing more is sent or held, and hands the session
//!   the reason, whose last answer it sends at once. A sweep every
//!   [`SWEEP_INTERVAL`] ends each stream whose certificate stopped admitting
//!   it, which is how a disable on the server's host reaches a running
//!   server.
//! - **Sending** (S25): nothing here waits. A message over
//!   [`MAX_SERVER_MESSAGE_BYTES`] is `TooLarge` and the stream stays; a full
//!   queue is `QueueFull` and ends the stream, so the agent resyncs.
//! - **Holds** (S28, S37): only the reconciler fills them, from the server's
//!   own assignments: a welcome's full reconcile with [`AgentRegistry::settle`],
//!   then one bot at a time with [`AgentRegistry::hold_and_send`], each
//!   checked against the stream's generation, so a plan never lands on a
//!   newer stream. [`AgentRegistry::placement_view`] is what it plans from.
//! - **Reports** (S28, S40, S44): every message an agent sends after
//!   `Welcome` passes [`AgentRegistry::admit`]. A status of a bot the stream
//!   holds assigned is its latest report, kept per bot past the stream's end
//!   (the server's memory of `last_state`); a status of a bot its hello
//!   listed counts before the welcome is reconciled. Anything else about a
//!   bot it doesn't hold is dropped and counted. A releasing bot passes only
//!   its `Removed`, which ends the hold and leaves a note for the reconciler;
//!   a `Removed` of an assigned bot frees nothing.
//! - **Session requests** (P10.6; S57): one passes only while the stream is
//!   reconciled, not ended, and holds its bot assigned; any other is denied
//!   at once, retryable, and counted with the dropped reports. A request's
//!   answer goes only to the stream that asked, by its generation, through
//!   [`LiveAgents::answer_session`]: a grant only while that stream still
//!   holds the bot assigned, checked and queued in one critical section.
//! - **Hints** (S37): a welcome, a `Settled` verdict and a live stream's end
//!   tell the reconciler to look again, once the lock is released.
//! - **Conflicts** (S26): a message on the live stream after a refused claim
//!   proves two processes hold the identity: `admit` hands the session a
//!   [`Conflict`] to record, at most once every [`CONFLICT_INTERVAL`] per
//!   agent.
//!
//! One `std` mutex guards it all, never held across an `.await`; messages are
//! converted and measured before it's taken.

use core::fmt;
use core::time::Duration;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use fleet_core::bot::{BotSnapshot, BotSpec, LastReport, LastState, Lifecycle, StickyState};
use fleet_core::id::{AgentId, BotId};
use fleet_core::pki::CertFingerprint;
use fleet_core::system::Clock;
use fleet_core::value::{AgentCapacity, AgentName};
use fleet_proto::control::{
    AgentMessage, BotEventKind, LifecycleCommand, MAX_SERVER_MESSAGE_BYTES, ServerMessage,
    SessionDenied, SessionGrant, SessionRequest, SessionRequestId, server_message_len,
};
use fleet_proto::generated as wire;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::http::error::chain;
use crate::ports::agent_connections::{
    Admitted, AgentConnections, Conflict, LiveCertificate, StreamEnd,
};
use crate::ports::ingest::{StickyMarks, StickyWrite};
pub use crate::ports::live_agents::LatestReport;
use crate::ports::live_agents::{
    AnswerRefused, CommandRefused, HeldBot, LiveAgents, LiveStream, PermissionEnd, SessionAnswer,
    SessionOutcome,
};
use crate::ports::reconcile::{Change, NoReconcile, Reconcile};

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
    /// Hinted after the lock is released.
    reconcile: Arc<dyn Reconcile>,
    state: Mutex<State>,
}

/// What the mutex guards.
#[derive(Default)]
struct State {
    /// The last generation handed out; the first is 1.
    generations: u64,
    slots: HashMap<AgentId, Slot>,
    /// The reports dropped and the session requests denied since the last
    /// report, per agent.
    refused_reports: BTreeMap<AgentId, (AgentName, RefusedCounts)>,
    /// When each agent's last conflict was handed out.
    conflicts: HashMap<AgentId, DateTime<Utc>>,
    /// Each bot's latest report from an agent that held it.
    latest: HashMap<BotId, LatestReport>,
    /// The bots whose latest report crossed into or out of Paused or Failed
    /// since their row stored it, for the ingest's next batch (U16).
    sticky: BTreeSet<BotId>,
    /// The bots released streams reported `Removed` for, not yet taken.
    released: Vec<Released>,
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
    connected_at: DateTime<Utc>,
    last_seen: DateTime<Utc>,
    /// The capacity its hello reported.
    capacity: AgentCapacity,
    /// Whether its welcome was reconciled yet.
    welcome: Welcome,
    holds: HashMap<BotId, HoldEntry>,
}

/// Whether a stream's welcome was reconciled.
enum Welcome {
    /// Not yet: the bots its hello listed, with their latest reports.
    Listed(BTreeMap<BotId, ListedBot>),
    /// Its full reconcile was queued.
    Reconciled,
}

/// A bot a hello listed, before its welcome is reconciled.
struct ListedBot {
    /// Its latest report on this stream: the hello's, or a later status.
    report: LatestReport,
    /// The sticky part of the hello's report, which the hello's
    /// transaction stored for a bot assigned to this agent: a settle marks
    /// the bot when its latest report crossed it since (U16).
    stored: Option<StickyState>,
}

/// How an agent holds a bot (S28).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// The bot is assigned to the agent.
    Assigned,
    /// The bot was unassigned; only its `Removed` is still expected.
    Releasing,
}

/// A bot a stream holds, as the reconciler left it.
#[derive(Debug, Clone, PartialEq)]
pub struct HoldEntry {
    /// How.
    pub hold: Hold,
    /// Since when, by the server's clock.
    pub since: DateTime<Utc>,
    /// The spec last sent for it, to tell whether a new one must be.
    pub sent: Option<BotSpec>,
    /// Whether the server is stopping it before it unassigns it.
    pub stopping: Option<StopState>,
}

/// Why the server is stopping a held bot, and its bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopState {
    /// It stopped qualifying: unassigned when it rests, or at `until`.
    Unqualified {
        /// The bound (S33, S42).
        until: Instant,
    },
    /// It moves off its agent: unassigned when it rests, with no bound.
    Move,
}

/// A `Removed` of a releasing bot, for the reconciler to clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Released {
    /// The agent that no longer runs it.
    pub agent: AgentId,
    /// The generation of the stream that sent it.
    pub generation: u64,
    /// The bot.
    pub bot: BotId,
}

/// A live stream as the reconciler plans from it.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveView {
    /// The agent.
    pub agent: AgentId,
    /// The stream's generation.
    pub generation: u64,
    /// Whether its switch is thrown: it's ending.
    pub halted: bool,
    /// The capacity its hello reported.
    pub capacity: AgentCapacity,
    /// Where it is.
    pub phase: ViewPhase,
}

/// Where a live stream is, as [`LiveView`] shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewPhase {
    /// Welcomed, not yet reconciled: the bots its hello listed.
    Listed(Vec<LatestReport>),
    /// Reconciled: what it holds, each with its latest report on this
    /// stream.
    Reconciled(Vec<HeldView>),
}

/// A bot a reconciled stream holds.
#[derive(Debug, Clone, PartialEq)]
pub struct HeldView {
    /// The bot.
    pub bot: BotId,
    /// How.
    pub entry: HoldEntry,
    /// Its latest report on this stream, if any.
    pub report: Option<LatestReport>,
}

/// What a session activates its claim with.
pub struct Activation {
    /// The stream's queue to its agent.
    pub queue: mpsc::Sender<wire::ServerMessage>,
    /// The stream's switch.
    pub halt: Halt,
    /// When its hello was accepted.
    pub connected_at: DateTime<Utc>,
    /// The capacity its hello reported.
    pub capacity: AgentCapacity,
    /// The bots its hello listed.
    pub running: Vec<BotSnapshot>,
    /// Its `Welcome`, queued first.
    pub welcome: wire::ServerMessage,
}

impl fmt::Debug for Activation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Activation")
            .field("connected_at", &self.connected_at)
            .field("capacity", &self.capacity)
            .field("running", &self.running.len())
            .finish_non_exhaustive()
    }
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

/// A session's claim on its agent's slot; freeing or dropping it frees the
/// slot.
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

    /// The generation of the stream that holds it.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Frees the slot, and hands over the latest report this stream sent of
    /// each bot it held, for its end's record (S40).
    #[must_use]
    pub fn free(self) -> Vec<(BotId, LastReport)> {
        let mut guard = lock(&self.shared);
        let state = &mut *guard;
        if state
            .slots
            .get(&self.agent)
            .is_none_or(|slot| slot.generation != self.generation)
        {
            return Vec::new();
        }
        let Some(Slot {
            phase: Phase::Live(live),
            ..
        }) = state.slots.remove(&self.agent)
        else {
            return Vec::new();
        };
        let mut held: Vec<(BotId, LastReport)> = live
            .holds
            .keys()
            .filter_map(|bot| {
                state
                    .latest
                    .get(bot)
                    .filter(|report| {
                        report.agent == self.agent && report.generation == self.generation
                    })
                    .map(|report| (*bot, report.last()))
            })
            .collect();
        held.sort_by_key(|(bot, _)| *bot);
        drop(guard);
        self.shared.reconcile.changed(Change::Ended(self.agent));
        held
        // Dropping `self` then finds the slot gone: nothing more to free.
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = lock(&self.shared);
        let live = if state
            .slots
            .get(&self.agent)
            .is_some_and(|slot| slot.generation == self.generation)
        {
            state
                .slots
                .remove(&self.agent)
                .is_some_and(|slot| matches!(slot.phase, Phase::Live(_)))
        } else {
            false
        };
        drop(state);
        if live {
            self.shared.reconcile.changed(Change::Ended(self.agent));
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
    /// The agent has no live stream, or not the one the message was for.
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
    /// It passes, and the reconciler should look again: a releasing bot's
    /// `Removed`, or a bot being stopped that rests now on a stream that's
    /// ending.
    Settled,
    /// A report about a bot the stream holds assigned, on a reconciled
    /// stream that isn't ended: it goes to the ingest, received `at` by the
    /// server's clock (S28, S70); `settles` when it's also a resting report
    /// of a bot being stopped, and the reconciler should look again.
    Held {
        /// When the server received it.
        at: DateTime<Utc>,
        /// Whether the reconciler should look again.
        settles: bool,
    },
    /// A report about a bot the agent doesn't hold: dropped and counted.
    Dropped,
    /// A session request for a bot the stream holds assigned, on a
    /// reconciled stream that isn't ended: its grant is worked out.
    Session(SessionRequest),
    /// Any other session request: denied at once, retryable, and counted
    /// (S57).
    DenySession(SessionRequest),
}

/// What one agent sent that was refused since the last report: reports
/// about bots it doesn't hold (S28, R21) and session requests denied before
/// any grant was worked out (S57, R43).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedReports {
    /// The agent.
    pub agent: AgentId,
    /// Its name.
    pub name: AgentName,
    /// How many reports were dropped.
    pub count: u64,
    /// How many session requests were denied: for a bot the stream didn't
    /// hold assigned, or with its grant queue full.
    pub sessions_denied: u64,
    /// How many chat messages the ingest dropped: past the stream's share
    /// of its queue, or with it full (S70).
    pub chat_dropped: u64,
}

/// One agent's refusals since the last report.
#[derive(Debug, Clone, Copy, Default)]
struct RefusedCounts {
    reports: u64,
    sessions: u64,
    chat: u64,
}

impl AgentRegistry {
    /// An empty registry, stamping with `clock`, whose changes hint no
    /// reconciler.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self::with_reconcile(clock, Arc::new(NoReconcile))
    }

    /// An empty registry, stamping with `clock`, that hints `reconcile`
    /// when a stream is welcomed, reports what the reconciler waits for, or
    /// ends.
    #[must_use]
    pub fn with_reconcile(clock: Arc<dyn Clock>, reconcile: Arc<dyn Reconcile>) -> Self {
        Self::with_limit(clock, MAX_SERVER_MESSAGE_BYTES, reconcile)
    }

    /// An empty registry whose messages may be at most `limit` bytes.
    fn with_limit(clock: Arc<dyn Clock>, limit: usize, reconcile: Arc<dyn Reconcile>) -> Self {
        Self {
            shared: Arc::new(Shared {
                clock,
                limit,
                reconcile,
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

    /// Makes `lease`'s claim live with `activation`: its `Welcome` is queued
    /// first, and the bots its hello listed fence every other agent from now
    /// on. The receiver gets the reason when the stream is ended.
    ///
    /// # Errors
    /// The reason the claim was ended with meanwhile, or
    /// [`StreamEnd::StreamError`] when the stream is gone already.
    pub fn activate(
        &self,
        lease: &Lease,
        activation: Activation,
    ) -> Result<oneshot::Receiver<StreamEnd>, StreamEnd> {
        let notice = self.go_live(lease, activation)?;
        self.shared.reconcile.changed(Change::Welcomed(lease.agent));
        Ok(notice)
    }

    /// [`activate`](Self::activate) under the lock.
    fn go_live(
        &self,
        lease: &Lease,
        activation: Activation,
    ) -> Result<oneshot::Receiver<StreamEnd>, StreamEnd> {
        let now = self.shared.clock.now();
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
                // The queue is new and empty; only a stream already gone
                // refuses it.
                if activation.queue.try_send(activation.welcome).is_err() {
                    return Err(StreamEnd::StreamError);
                }
                let listed = activation
                    .running
                    .into_iter()
                    .map(|snapshot| {
                        let bot = snapshot.bot_id;
                        let stored = LastState::from(snapshot.state).sticky();
                        let report = LatestReport {
                            agent: lease.agent,
                            generation: lease.generation,
                            snapshot,
                            at: now,
                        };
                        (bot, ListedBot { report, stored })
                    })
                    .collect();
                let (ender, notice) = oneshot::channel();
                slot.phase = Phase::Live(Live {
                    queue: activation.queue,
                    halt: activation.halt,
                    ender: Some(ender),
                    connected_at: activation.connected_at,
                    last_seen: activation.connected_at,
                    capacity: activation.capacity,
                    welcome: Welcome::Listed(listed),
                    holds: HashMap::new(),
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

    /// Queues `message` for `agent`'s stream, whichever it is; see the
    /// [module docs](self).
    ///
    /// # Errors
    /// [`SendError`].
    pub fn send(&self, agent: AgentId, message: ServerMessage) -> Result<(), SendError> {
        let message = self.wire(message)?;
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
        queue(slot, message)
    }

    /// Sends a stream's welcome its full reconcile, `full`, and gives it
    /// `holds`, but only while generation `generation` of `agent`'s stream
    /// lives and isn't reconciled yet. Its hello's reports of the bots it
    /// holds assigned become their latest.
    ///
    /// # Errors
    /// [`SendError`]: `NotConnected` for another or an ended stream; nothing
    /// is held then.
    pub fn settle(
        &self,
        agent: AgentId,
        generation: u64,
        holds: Vec<(BotId, HoldEntry)>,
        full: ServerMessage,
    ) -> Result<(), SendError> {
        let full = self.wire(full)?;
        let mut guard = self.state();
        let state = &mut *guard;
        let Some(slot) = state
            .slots
            .get_mut(&agent)
            .filter(|slot| slot.generation == generation)
        else {
            return Err(SendError::NotConnected);
        };
        match &slot.phase {
            Phase::Live(live)
                if !live.halt.is_halted() && matches!(live.welcome, Welcome::Listed(_)) => {}
            _ => return Err(SendError::NotConnected),
        }
        queue(slot, full)?;
        let Phase::Live(live) = &mut slot.phase else {
            return Err(SendError::NotConnected);
        };
        live.holds = holds.into_iter().collect();
        if let Welcome::Listed(listed) = core::mem::replace(&mut live.welcome, Welcome::Reconciled)
        {
            // Only a bot the server gives it: a stray's report says nothing
            // about the bot's own state.
            let assigned = listed.into_iter().filter(|(bot, _)| {
                live.holds
                    .get(bot)
                    .is_some_and(|entry| entry.hold == Hold::Assigned)
            });
            for (bot, listed) in assigned {
                // A status since the hello crossed what its transaction
                // stored: the next batch writes it (U16).
                if LastState::from(listed.report.snapshot.state).sticky() != listed.stored {
                    state.sticky.insert(bot);
                }
                state.latest.insert(bot, listed.report);
            }
        }
        Ok(())
    }

    /// Sends `message`, if any, to generation `generation` of `agent`'s
    /// reconciled stream, and then holds `bot` as `entry`.
    ///
    /// # Errors
    /// [`SendError`]: `NotConnected` for another, an ended or an unreconciled
    /// stream; nothing is held then.
    pub fn hold_and_send(
        &self,
        agent: AgentId,
        generation: u64,
        bot: BotId,
        entry: HoldEntry,
        message: Option<ServerMessage>,
    ) -> Result<(), SendError> {
        let message = message.map(|message| self.wire(message)).transpose()?;
        let mut state = self.state();
        let Some(slot) = state
            .slots
            .get_mut(&agent)
            .filter(|slot| slot.generation == generation)
        else {
            return Err(SendError::NotConnected);
        };
        match &slot.phase {
            Phase::Live(live)
                if !live.halt.is_halted() && matches!(live.welcome, Welcome::Reconciled) => {}
            _ => return Err(SendError::NotConnected),
        }
        if let Some(message) = message {
            queue(slot, message)?;
        }
        let Phase::Live(live) = &mut slot.phase else {
            return Err(SendError::NotConnected);
        };
        live.holds.insert(bot, entry);
        Ok(())
    }

    /// Every welcomed stream, as the reconciler plans from it.
    #[must_use]
    pub fn placement_view(&self) -> Vec<LiveView> {
        let state = self.state();
        let mut views: Vec<LiveView> = state
            .slots
            .iter()
            .filter_map(|(agent, slot)| {
                let Phase::Live(live) = &slot.phase else {
                    return None;
                };
                let phase = match &live.welcome {
                    Welcome::Listed(listed) => ViewPhase::Listed(
                        listed
                            .values()
                            .map(|listed| listed.report.clone())
                            .collect(),
                    ),
                    Welcome::Reconciled => {
                        let mut held: Vec<HeldView> = live
                            .holds
                            .iter()
                            .map(|(bot, entry)| HeldView {
                                bot: *bot,
                                entry: entry.clone(),
                                report: state
                                    .latest
                                    .get(bot)
                                    .filter(|report| {
                                        report.agent == *agent
                                            && report.generation == slot.generation
                                    })
                                    .cloned(),
                            })
                            .collect();
                        // A Removed still to be cleared holds its slot, and
                        // its bot is never sent again.
                        held.extend(
                            state
                                .released
                                .iter()
                                .filter(|note| {
                                    note.agent == *agent
                                        && note.generation == slot.generation
                                        && !live.holds.contains_key(&note.bot)
                                })
                                .map(|note| HeldView {
                                    bot: note.bot,
                                    entry: HoldEntry {
                                        hold: Hold::Releasing,
                                        since: live.last_seen,
                                        sent: None,
                                        stopping: None,
                                    },
                                    report: None,
                                }),
                        );
                        held.sort_by_key(|held| held.bot);
                        held.dedup_by_key(|held| held.bot);
                        ViewPhase::Reconciled(held)
                    }
                };
                Some(LiveView {
                    agent: *agent,
                    generation: slot.generation,
                    halted: live.halt.is_halted(),
                    capacity: live.capacity,
                    phase,
                })
            })
            .collect();
        views.sort_by_key(|view| view.agent);
        views
    }

    /// The `Removed`s of releasing bots since the last call, without the
    /// stale ones: a note is dropped once a newer stream of its agent has
    /// been welcomed, whose welcome may have given the bot back, and while
    /// its agent's stream holds the bot assigned. Clearing such a bot's
    /// assignment would leave an agent that runs it unnamed.
    #[must_use]
    pub fn take_released(&self) -> Vec<Released> {
        let mut state = self.state();
        let notes = core::mem::take(&mut state.released);
        let (fresh, dropped): (Vec<Released>, Vec<Released>) = notes
            .into_iter()
            .partition(|note| !stale(&state.slots, note));
        drop(state);
        for note in dropped {
            debug!(
                agent_id = %note.agent,
                bot_id = %note.bot,
                "a Removed from an older stream was dropped: a newer one holds the agent's word"
            );
        }
        fresh
    }

    /// Puts `released` back, for the next call: its clearing failed.
    pub fn requeue_released(&self, released: Released) {
        self.state().released.push(released);
    }

    /// `bot`'s latest report, from whichever stream sent it.
    #[must_use]
    pub fn latest(&self, bot: BotId) -> Option<LatestReport> {
        self.state().latest.get(&bot).cloned()
    }

    /// Forgets the latest reports of the bots `keep` doesn't keep.
    pub fn retain_latest(&self, keep: &dyn Fn(BotId) -> bool) {
        self.state().latest.retain(|bot, _| keep(*bot));
    }

    /// The nearest bound of a bot being stopped that's later than `after`,
    /// if any.
    #[must_use]
    pub fn next_deadline(&self, after: Instant) -> Option<Instant> {
        self.state()
            .slots
            .values()
            .filter_map(|slot| match &slot.phase {
                Phase::Live(live) => Some(live),
                Phase::Claimed { .. } => None,
            })
            .flat_map(|live| live.holds.values())
            .filter_map(|entry| match entry.stopping {
                Some(StopState::Unqualified { until }) => Some(until),
                Some(StopState::Move) | None => None,
            })
            .filter(|until| *until > after)
            .min()
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
        let verdict = judge(
            Stream {
                agent: lease.agent,
                generation: lease.generation,
                live,
                now,
            },
            &mut state.latest,
            &mut state.released,
            &mut state.sticky,
            message,
        );
        if matches!(verdict, Verdict::Dropped | Verdict::DenySession(_)) {
            let (_, counts) = state
                .refused_reports
                .entry(lease.agent)
                .or_insert_with(|| (slot.name.clone(), RefusedCounts::default()));
            if verdict == Verdict::Dropped {
                counts.reports = counts.reports.saturating_add(1);
            } else {
                counts.sessions = counts.sessions.saturating_add(1);
            }
        }
        drop(guard);
        if matches!(
            verdict,
            Verdict::Settled | Verdict::Held { settles: true, .. }
        ) {
            self.shared.reconcile.changed(Change::Settled(lease.agent));
        }
        (verdict, conflict)
    }

    /// Counts one more session request of `lease`'s stream denied before
    /// its grant was worked out: its grant queue was full (S57, R43).
    pub fn count_denied_session(&self, lease: &Lease) {
        let mut guard = self.state();
        let state = &mut *guard;
        let Some(slot) = state
            .slots
            .get(&lease.agent)
            .filter(|slot| slot.generation == lease.generation)
        else {
            return;
        };
        let (_, counts) = state
            .refused_reports
            .entry(lease.agent)
            .or_insert_with(|| (slot.name.clone(), RefusedCounts::default()));
        counts.sessions = counts.sessions.saturating_add(1);
    }

    /// The reports dropped and the session requests denied since the last
    /// call, per agent.
    #[must_use]
    pub fn take_refused_reports(&self) -> Vec<RefusedReports> {
        core::mem::take(&mut self.state().refused_reports)
            .into_iter()
            .map(|(agent, (name, counts))| RefusedReports {
                agent,
                name,
                count: counts.reports,
                sessions_denied: counts.sessions,
                chat_dropped: counts.chat,
            })
            .collect()
    }

    /// Counts one more chat message of `lease`'s stream the ingest dropped
    /// (S70).
    pub fn count_dropped_chat(&self, lease: &Lease) {
        let mut guard = self.state();
        let state = &mut *guard;
        let Some(slot) = state
            .slots
            .get(&lease.agent)
            .filter(|slot| slot.generation == lease.generation)
        else {
            return;
        };
        let (_, counts) = state
            .refused_reports
            .entry(lease.agent)
            .or_insert_with(|| (slot.name.clone(), RefusedCounts::default()));
        counts.chat = counts.chat.saturating_add(1);
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

    /// `message` on the wire, measured against the limit.
    fn wire(&self, message: ServerMessage) -> Result<wire::ServerMessage, SendError> {
        let message = wire::ServerMessage::try_from(message).map_err(|_| SendError::Unwritable)?;
        if server_message_len(&message) > self.shared.limit {
            return Err(SendError::TooLarge);
        }
        Ok(message)
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

/// Queues `message` on `slot`'s live stream; a full queue ends it.
fn queue(slot: &mut Slot, message: wire::ServerMessage) -> Result<(), SendError> {
    let Phase::Live(live) = &mut slot.phase else {
        return Err(SendError::NotConnected);
    };
    match live.queue.try_send(message) {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Full(_)) => {
            end_slot(slot, StreamEnd::TooSlow);
            Err(SendError::QueueFull)
        }
        Err(mpsc::error::TrySendError::Closed(_)) => Err(SendError::NotConnected),
    }
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

    fn held(&self, agent: AgentId, bot: BotId) -> Option<HeldBot> {
        let state = self.state();
        let slot = state.slots.get(&agent)?;
        let Phase::Live(live) = &slot.phase else {
            return None;
        };
        let entry = live
            .holds
            .get(&bot)
            .filter(|entry| entry.hold == Hold::Assigned)?;
        Some(HeldBot {
            since: entry.since,
            generation: slot.generation,
            report: state
                .latest
                .get(&bot)
                .filter(|report| report.agent == agent && report.generation == slot.generation)
                .map(|report| report.snapshot.clone()),
        })
    }

    fn latest(&self, bot: BotId) -> Option<LatestReport> {
        Self::latest(self, bot)
    }

    fn take_latest(&self, bot: BotId) -> Option<LatestReport> {
        self.state().latest.remove(&bot)
    }

    fn put_back_latest(&self, report: LatestReport) {
        let bot = report.snapshot.bot_id;
        self.state().latest.entry(bot).or_insert(report);
    }

    fn command(
        &self,
        agent: AgentId,
        generation: u64,
        bot: BotId,
        command: Lifecycle,
    ) -> Result<(), CommandRefused> {
        let unsendable = |error| match error {
            SendError::NotConnected => CommandRefused::NotConnected,
            SendError::QueueFull => CommandRefused::QueueFull,
            SendError::TooLarge | SendError::Unwritable => CommandRefused::Unsendable,
        };
        let message = self
            .wire(ServerMessage::Lifecycle(LifecycleCommand {
                bot_id: bot,
                command,
            }))
            .map_err(unsendable)?;
        // One critical section, as `hold_and_send`'s (ADR-0019, S48): the
        // stream the caller found holding the bot, still live, not ended and
        // still holding it assigned, gets the command; a stream that
        // replaced it gets nothing, and a command never assigns.
        let mut state = self.state();
        let Some(slot) = state
            .slots
            .get_mut(&agent)
            .filter(|slot| slot.generation == generation)
        else {
            return Err(CommandRefused::NotConnected);
        };
        let Phase::Live(live) = &slot.phase else {
            return Err(CommandRefused::NotConnected);
        };
        if live.halt.is_halted() {
            return Err(CommandRefused::NotConnected);
        }
        if !live
            .holds
            .get(&bot)
            .is_some_and(|entry| entry.hold == Hold::Assigned)
        {
            return Err(CommandRefused::NotHeld);
        }
        queue(slot, message).map_err(unsendable)
    }

    fn answer_session(
        &self,
        agent: AgentId,
        generation: u64,
        answer: SessionAnswer,
    ) -> Result<(), AnswerRefused> {
        let unsendable = |error| match error {
            SendError::NotConnected => AnswerRefused::NotConnected,
            SendError::QueueFull => AnswerRefused::QueueFull,
            SendError::TooLarge | SendError::Unwritable => AnswerRefused::Unsendable,
        };
        let id = SessionRequestId::new(answer.number);
        let bot = answer.bot;
        let (message, grant) = match answer.outcome {
            SessionOutcome::Grant(credentials) => (
                ServerMessage::SessionGrant(SessionGrant {
                    id,
                    bot_id: bot,
                    credentials,
                }),
                true,
            ),
            SessionOutcome::Denied { retryable } => (
                ServerMessage::SessionDenied(SessionDenied {
                    id,
                    bot_id: bot,
                    retryable,
                }),
                false,
            ),
        };
        // Written before the lock: a refused grant's wire copy of the token
        // is wiped when it's dropped (S7).
        let message = self.wire(message).map_err(unsendable)?;
        // One critical section, as `command`'s (S57): the stream that asked,
        // still live and not ended, gets the answer, and a grant only while
        // that stream holds the bot assigned; a stream that replaced it,
        // whose request numbers start at 1 again, gets nothing.
        let mut state = self.state();
        let Some(slot) = state
            .slots
            .get_mut(&agent)
            .filter(|slot| slot.generation == generation)
        else {
            return Err(AnswerRefused::NotConnected);
        };
        let Phase::Live(live) = &slot.phase else {
            return Err(AnswerRefused::NotConnected);
        };
        if live.halt.is_halted() {
            return Err(AnswerRefused::NotConnected);
        }
        if grant
            && !live
                .holds
                .get(&bot)
                .is_some_and(|entry| entry.hold == Hold::Assigned)
        {
            return Err(AnswerRefused::NotHeld);
        }
        queue(slot, message).map_err(unsendable)
    }
}

impl StickyMarks for AgentRegistry {
    fn any(&self) -> bool {
        !self.state().sticky.is_empty()
    }

    fn take(&self) -> Vec<StickyWrite> {
        let mut guard = self.state();
        let state = &mut *guard;
        let marked = core::mem::take(&mut state.sticky);
        marked
            .into_iter()
            .filter_map(|bot| {
                // Still its latest, from the live, unended stream that sent
                // it and holds the bot assigned: a resume's or a reset's
                // clear took the report, a release or a newer stream moved
                // the bot, and an ended stream's end records its own (S40).
                let report = state.latest.get(&bot)?;
                let slot = state
                    .slots
                    .get(&report.agent)
                    .filter(|slot| slot.generation == report.generation)?;
                let Phase::Live(live) = &slot.phase else {
                    return None;
                };
                let held = !live.halt.is_halted()
                    && live
                        .holds
                        .get(&bot)
                        .is_some_and(|entry| entry.hold == Hold::Assigned);
                held.then(|| StickyWrite {
                    bot,
                    agent: report.agent,
                    last: report.last(),
                })
            })
            .collect()
    }

    fn remark(&self, bots: &[BotId]) {
        self.state().sticky.extend(bots.iter().copied());
    }
}

/// Whether `note` no longer speaks for its agent: a newer stream of it was
/// welcomed, or its live stream holds the bot assigned.
fn stale(slots: &HashMap<AgentId, Slot>, note: &Released) -> bool {
    let Some(slot) = slots.get(&note.agent) else {
        return false;
    };
    let Phase::Live(live) = &slot.phase else {
        // A hello still being accepted isn't a welcome yet.
        return false;
    };
    slot.generation != note.generation
        || live
            .holds
            .get(&note.bot)
            .is_some_and(|entry| entry.hold == Hold::Assigned)
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

/// The stream a message came on, as [`judge`] needs it.
struct Stream<'a> {
    agent: AgentId,
    generation: u64,
    live: &'a mut Live,
    now: DateTime<Utc>,
}

/// What happens to `message` from `stream` (S28, S40, S44, S70): its reports
/// update `latest`, a status that crosses into or out of Paused or Failed
/// marks its bot in `sticky` (U16), and a releasing bot's `Removed` leaves a
/// note in `released`.
fn judge(
    stream: Stream<'_>,
    latest: &mut HashMap<BotId, LatestReport>,
    released: &mut Vec<Released>,
    sticky: &mut BTreeSet<BotId>,
    message: &AgentMessage,
) -> Verdict {
    let Stream {
        agent,
        generation,
        live,
        now,
    } = stream;
    match message {
        AgentMessage::Hello(_) | AgentMessage::Heartbeat => Verdict::Pass,
        // Only a reconciled stream that isn't ended, for a bot it holds
        // assigned: never one held releasing or listed in a hello not yet
        // reconciled (S57).
        AgentMessage::SessionRequest(request) => {
            let holds = !live.halt.is_halted()
                && matches!(live.welcome, Welcome::Reconciled)
                && live
                    .holds
                    .get(&request.bot_id)
                    .is_some_and(|entry| entry.hold == Hold::Assigned);
            if holds {
                Verdict::Session(*request)
            } else {
                Verdict::DenySession(*request)
            }
        }
        AgentMessage::Status(snapshot) => {
            let report = LatestReport {
                agent,
                generation,
                snapshot: snapshot.clone(),
                at: now,
            };
            // Before its welcome is reconciled, a listed bot's status is
            // what the planner reads it by.
            if let Welcome::Listed(listed) = &mut live.welcome {
                return match listed.get_mut(&snapshot.bot_id) {
                    Some(listed) => {
                        listed.report = report;
                        Verdict::Pass
                    }
                    None => Verdict::Dropped,
                };
            }
            match live.holds.get(&snapshot.bot_id) {
                Some(entry) if entry.hold == Hold::Assigned => {
                    let state = LastState::from(snapshot.state);
                    let settles = entry.stopping.is_some() && state.rests();
                    let before = latest
                        .get(&snapshot.bot_id)
                        .and_then(|report| LastState::from(report.snapshot.state).sticky());
                    latest.insert(snapshot.bot_id, report);
                    // An ended stream's report stays its latest, which its
                    // end records (S40), but goes nowhere else.
                    if live.halt.is_halted() {
                        return if settles {
                            Verdict::Settled
                        } else {
                            Verdict::Pass
                        };
                    }
                    // Into or out of Paused or Failed, or another reason: the
                    // next batch writes it (U16).
                    if state.sticky() != before {
                        sticky.insert(snapshot.bot_id);
                    }
                    Verdict::Held { at: now, settles }
                }
                Some(_) | None => Verdict::Dropped,
            }
        }
        AgentMessage::Event(event) => {
            let hold = live.holds.get(&event.bot_id).map(|entry| entry.hold);
            match (hold, &event.kind) {
                // An honest agent sends it only after a removal the server
                // asked for: it crossed a newer assignment (S44).
                (Some(Hold::Assigned), BotEventKind::Removed) => Verdict::Dropped,
                // Holds exist only once the welcome is reconciled.
                (Some(Hold::Assigned), _) if live.halt.is_halted() => Verdict::Pass,
                (Some(Hold::Assigned), _) => Verdict::Held {
                    at: now,
                    settles: false,
                },
                (Some(Hold::Releasing), BotEventKind::Removed) => {
                    live.holds.remove(&event.bot_id);
                    released.push(Released {
                        agent,
                        generation,
                        bot: event.bot_id,
                    });
                    Verdict::Settled
                }
                _ => Verdict::Dropped,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU64;
    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;
    use fleet_core::bot::{BotState, DesiredRunState, FailReason};
    use fleet_core::id::RequestId;
    use fleet_core::mc::SessionCredentials;
    use fleet_core::value::{AgentCapacity, AgentVersion};
    use fleet_proto::control::{BotEvent, BotEventKind, SessionRequestId, Welcome};
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

    /// Agent 1 admitted with certificate `n` from `192.0.2.<n>`.
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

    fn welcome() -> wire::ServerMessage {
        wire::ServerMessage::try_from(ServerMessage::Welcome(Welcome {
            heartbeat_interval: Duration::from_secs(10),
            agent_id: agent(1),
        }))
        .unwrap()
    }

    fn activation(
        queue: mpsc::Sender<wire::ServerMessage>,
        running: Vec<BotSnapshot>,
    ) -> Activation {
        Activation {
            queue,
            halt: Halt::default(),
            connected_at: at(T0),
            capacity: AgentCapacity::try_from(10_u32).unwrap(),
            running,
            welcome: welcome(),
        }
    }

    /// Claims and activates `admitted(n)` with a queue of `depth` and the hello
    /// `running`; the queue's `Welcome` is taken off.
    fn live_with(
        registry: &AgentRegistry,
        n: u8,
        depth: usize,
        running: Vec<BotSnapshot>,
    ) -> (
        Lease,
        mpsc::Receiver<wire::ServerMessage>,
        oneshot::Receiver<StreamEnd>,
    ) {
        let lease = registry.claim(&admitted(n)).unwrap();
        let (queue, mut queued) = mpsc::channel(depth);
        let ended = registry
            .activate(&lease, activation(queue, running))
            .unwrap();
        assert_eq!(queued.try_recv().unwrap(), welcome(), "Welcome comes first");
        (lease, queued, ended)
    }

    fn live(
        registry: &AgentRegistry,
        n: u8,
        depth: usize,
    ) -> (
        Lease,
        mpsc::Receiver<wire::ServerMessage>,
        oneshot::Receiver<StreamEnd>,
    ) {
        live_with(registry, n, depth, Vec::new())
    }

    fn snapshot(bot: BotId, state: BotState) -> BotSnapshot {
        BotSnapshot {
            bot_id: bot,
            state,
            since: at(T0),
            last_disconnect: None,
        }
    }

    fn status(bot: BotId) -> AgentMessage {
        AgentMessage::Status(snapshot(bot, BotState::Stopped))
    }

    fn online(bot: BotId) -> AgentMessage {
        AgentMessage::Status(snapshot(
            bot,
            BotState::Online {
                since: at(T0),
                attempt: core::num::NonZeroU32::MIN,
            },
        ))
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

    fn entry(hold: Hold, stopping: Option<StopState>) -> HoldEntry {
        HoldEntry {
            hold,
            since: at(T0),
            sent: None,
            stopping,
        }
    }

    /// Settles agent 1's welcome with `holds` and an empty reconcile.
    fn settle(registry: &AgentRegistry, lease: &Lease, holds: &[(u8, Hold)]) {
        registry
            .settle(
                lease.agent,
                lease.generation,
                holds
                    .iter()
                    .map(|&(n, hold)| (bot(n), entry(hold, None)))
                    .collect(),
                ServerMessage::Reconcile(Vec::new()),
            )
            .unwrap();
    }

    // --- One stream per agent -------------------------------------------------------------

    #[test]
    fn a_second_claim_is_refused_while_the_first_is_claimed_or_live() {
        let (registry, _) = registry();
        let first = registry.claim(&admitted(1)).unwrap();

        let while_claimed = registry.claim(&admitted(2));
        let (queue, _queued) = mpsc::channel(1);
        let _ended = registry
            .activate(&first, activation(queue, Vec::new()))
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
        let (lease, _queued, _ended) = live(&registry, 1, 2);

        drop(lease);

        assert!(registry.claim(&admitted(2)).is_ok());
        assert_eq!(LiveAgents::live(&registry, agent(1)), None);
    }

    // --- Activation -------------------------------------------------------------------------

    #[test]
    fn activation_queues_welcome_first_and_lists_the_hellos_bots() {
        let (registry, _) = registry();

        let (lease, _queued, _ended) =
            live_with(&registry, 1, 2, vec![snapshot(bot(1), BotState::Stopped)]);

        assert_eq!(
            registry.placement_view(),
            [LiveView {
                agent: agent(1),
                generation: lease.generation,
                halted: false,
                capacity: AgentCapacity::try_from(10_u32).unwrap(),
                phase: ViewPhase::Listed(vec![LatestReport {
                    agent: agent(1),
                    generation: lease.generation,
                    snapshot: snapshot(bot(1), BotState::Stopped),
                    at: at(T0),
                }]),
            }]
        );
    }

    #[test]
    fn a_stream_gone_before_its_welcome_is_never_activated() {
        let (registry, _) = registry();
        let lease = registry.claim(&admitted(1)).unwrap();
        let (queue, queued) = mpsc::channel(1);
        drop(queued);

        let activated = registry.activate(&lease, activation(queue, Vec::new()));

        assert_eq!(activated.unwrap_err(), StreamEnd::StreamError);
        assert_eq!(registry.placement_view(), []);
    }

    // --- Ending -----------------------------------------------------------------------------

    #[test]
    fn a_claim_ended_meanwhile_is_never_activated() {
        let (registry, _) = registry();
        let lease = registry.claim(&admitted(1)).unwrap();

        let found = registry.end(agent(1), StreamEnd::Disabled);
        let (queue, mut queued) = mpsc::channel(1);
        let activated = registry.activate(&lease, activation(queue, Vec::new()));

        assert!(found);
        assert_eq!(activated.unwrap_err(), StreamEnd::Disabled);
        assert!(queued.try_recv().is_err(), "no Welcome");
    }

    #[test]
    fn ending_a_live_stream_hands_its_session_the_reason_once() {
        let (registry, _) = registry();
        let (_lease, _queued, mut ended) = live(&registry, 1, 2);

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
            .activate(
                &lease,
                Activation {
                    halt: halt.clone(),
                    ..activation(queue, Vec::new())
                },
            )
            .unwrap();
        let _welcome = queued.try_recv().unwrap();
        settle(&registry, &lease, &[(1, Hold::Assigned)]);
        let _reconcile = queued.try_recv().unwrap();

        assert!(registry.end(agent(1), StreamEnd::Disabled));

        assert!(halt.is_halted());
        assert_eq!(
            registry.send(agent(1), unassign(1)),
            Err(SendError::NotConnected)
        );
        assert_eq!(
            registry.hold_and_send(
                agent(1),
                lease.generation,
                bot(2),
                entry(Hold::Assigned, None),
                None
            ),
            Err(SendError::NotConnected)
        );
        assert!(queued.try_recv().is_err());
        let view = registry.placement_view();
        assert!(view.first().unwrap().halted);
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
            .activate(&lease, activation(queue, Vec::new()))
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
        // Room for the Welcome, then one more.
        let (_lease, mut queued, mut ended) = live(&registry, 1, 2);

        let first = registry.send(agent(1), unassign(1));
        let second = registry.send(agent(1), unassign(2));
        let third = registry.send(agent(1), unassign(3));

        assert_eq!(first, Ok(()));
        assert_eq!(second, Ok(()));
        assert_eq!(third, Err(SendError::QueueFull));
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
        let registry = AgentRegistry::with_limit(Arc::new(clock), 4, Arc::new(NoReconcile));
        let (_lease, mut queued, mut ended) = live(&registry, 1, 2);

        let sent = registry.send(agent(1), unassign(1));

        assert_eq!(sent, Err(SendError::TooLarge));
        assert!(queued.try_recv().is_err());
        assert!(ended.try_recv().is_err());
    }

    // --- Settling and holding -----------------------------------------------------------------

    #[test]
    fn settling_queues_the_full_reconcile_holds_the_bots_and_keeps_the_hellos_reports() {
        let (registry, _) = registry();
        let (lease, mut queued, _ended) =
            live_with(&registry, 1, 4, vec![snapshot(bot(1), BotState::Stopped)]);

        let settled = registry.settle(
            agent(1),
            lease.generation,
            vec![
                (bot(1), entry(Hold::Assigned, None)),
                (bot(2), entry(Hold::Releasing, None)),
            ],
            ServerMessage::Reconcile(Vec::new()),
        );

        assert_eq!(settled, Ok(()));
        assert_eq!(
            queued.try_recv().unwrap(),
            wire::ServerMessage::try_from(ServerMessage::Reconcile(Vec::new())).unwrap()
        );
        let report = registry.latest(bot(1)).unwrap();
        assert_eq!(report.snapshot, snapshot(bot(1), BotState::Stopped));
        let view = registry.placement_view();
        let ViewPhase::Reconciled(held) = &view.first().unwrap().phase else {
            panic!("reconciled");
        };
        assert_eq!(
            held.iter()
                .map(|held| (held.bot, held.entry.hold))
                .collect::<Vec<_>>(),
            {
                let mut expected = vec![(bot(1), Hold::Assigned), (bot(2), Hold::Releasing)];
                expected.sort_by_key(|(bot, _)| *bot);
                expected
            }
        );
    }

    #[test]
    fn settling_keeps_only_the_listed_reports_of_the_bots_it_assigns() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live_with(
            &registry,
            1,
            4,
            vec![
                snapshot(bot(1), BotState::Stopped),
                snapshot(bot(2), BotState::Stopped),
                snapshot(bot(3), BotState::Stopped),
            ],
        );

        settle(
            &registry,
            &lease,
            &[(1, Hold::Assigned), (2, Hold::Releasing)],
        );

        assert!(registry.latest(bot(1)).is_some());
        assert_eq!(registry.latest(bot(2)), None, "a stray's report");
        assert_eq!(registry.latest(bot(3)), None);
    }

    #[test]
    fn settling_needs_its_own_unreconciled_stream() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 4);
        let reconcile = || ServerMessage::Reconcile(Vec::new());

        let other_generation = registry.settle(
            agent(1),
            lease.generation.saturating_add(1),
            Vec::new(),
            reconcile(),
        );
        let first = registry.settle(agent(1), lease.generation, Vec::new(), reconcile());
        let twice = registry.settle(agent(1), lease.generation, Vec::new(), reconcile());
        let unknown = registry.settle(agent(9), 1, Vec::new(), reconcile());

        assert_eq!(other_generation, Err(SendError::NotConnected));
        assert_eq!(first, Ok(()));
        assert_eq!(twice, Err(SendError::NotConnected));
        assert_eq!(unknown, Err(SendError::NotConnected));
    }

    #[test]
    fn holding_needs_the_reconciled_stream_of_its_generation() {
        let (registry, _) = registry();
        let (lease, mut queued, _ended) = live(&registry, 1, 4);
        let hold = |generation| {
            registry.hold_and_send(
                agent(1),
                generation,
                bot(1),
                entry(Hold::Assigned, None),
                Some(unassign(1)),
            )
        };

        let unreconciled = hold(lease.generation);
        settle(&registry, &lease, &[]);
        let _reconcile = queued.try_recv().unwrap();
        let other = hold(lease.generation.saturating_add(1));
        let own = hold(lease.generation);

        assert_eq!(unreconciled, Err(SendError::NotConnected));
        assert_eq!(other, Err(SendError::NotConnected));
        assert_eq!(own, Ok(()));
        assert_eq!(
            queued.try_recv().unwrap(),
            wire::ServerMessage::try_from(unassign(1)).unwrap()
        );
        assert_ne!(registry.admit(&lease, &status(bot(1))).0, Verdict::Dropped);
    }

    #[test]
    fn a_hold_whose_message_fills_the_queue_ends_the_stream_and_holds_nothing() {
        let (registry, _) = registry();
        // Room for the Welcome and the reconcile only.
        let (lease, _queued, mut ended) = live(&registry, 1, 1);
        // The Welcome was taken off: the reconcile fits, then nothing.
        settle(&registry, &lease, &[]);

        let held = registry.hold_and_send(
            agent(1),
            lease.generation,
            bot(1),
            entry(Hold::Assigned, None),
            Some(unassign(1)),
        );

        assert_eq!(held, Err(SendError::QueueFull));
        assert_eq!(ended.try_recv().unwrap(), StreamEnd::TooSlow);
        let view = registry.placement_view();
        let ViewPhase::Reconciled(holds) = &view.first().unwrap().phase else {
            panic!("reconciled");
        };
        assert_eq!(holds, &[]);
    }

    // --- Reports ----------------------------------------------------------------------------

    #[test]
    fn reports_about_bots_the_agent_doesnt_hold_are_dropped_and_counted() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 4);
        settle(&registry, &lease, &[]);

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
                sessions_denied: 0,
                chat_dropped: 0,
            }]
        );
        assert_eq!(registry.take_refused_reports(), Vec::new());
    }

    #[test]
    fn an_assigned_bots_status_is_its_latest_report_which_outlives_the_stream() {
        let (registry, clock) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 4);
        settle(&registry, &lease, &[(1, Hold::Assigned)]);
        clock.advance(Duration::from_secs(3));

        let verdicts = [
            registry.admit(&lease, &online(bot(1))).0,
            registry.admit(&lease, &event(bot(1), BotEventKind::Died)).0,
        ];
        drop(lease);

        let held = Verdict::Held {
            at: at("2027-01-15T08:00:03Z"),
            settles: false,
        };
        assert_eq!(verdicts, [held.clone(), held]);
        let latest = registry.latest(bot(1)).unwrap();
        assert_eq!(latest.agent, agent(1));
        assert_eq!(latest.at, at("2027-01-15T08:00:03Z"));
        assert_eq!(latest.last().state, LastState::Online);
    }

    #[test]
    fn a_listed_bots_status_before_the_reconcile_replaces_its_listed_report_uncounted() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) =
            live_with(&registry, 1, 4, vec![snapshot(bot(1), BotState::Stopped)]);

        let verdict = registry.admit(&lease, &online(bot(1))).0;

        assert_eq!(verdict, Verdict::Pass);
        assert_eq!(registry.take_refused_reports(), Vec::new());
        let view = registry.placement_view();
        let ViewPhase::Listed(listed) = &view.first().unwrap().phase else {
            panic!("listed");
        };
        assert_eq!(listed.first().unwrap().last().state, LastState::Online);
    }

    #[test]
    fn a_releasing_bot_passes_only_its_removed_which_ends_the_hold_and_is_noted() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 4);
        settle(&registry, &lease, &[(1, Hold::Releasing)]);

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
                Verdict::Settled,
                Verdict::Dropped
            ]
        );
        assert_eq!(
            registry.take_released(),
            [Released {
                agent: agent(1),
                generation: lease.generation,
                bot: bot(1),
            }]
        );
        assert_eq!(registry.take_released(), []);
    }

    #[test]
    fn a_removed_of_an_assigned_bot_frees_nothing() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 4);
        settle(&registry, &lease, &[(1, Hold::Assigned)]);

        let removed = registry
            .admit(&lease, &event(bot(1), BotEventKind::Removed))
            .0;
        let after = registry.admit(&lease, &status(bot(1))).0;

        assert_eq!(removed, Verdict::Dropped);
        assert_eq!(
            after,
            Verdict::Held {
                at: at(T0),
                settles: false
            },
            "still held"
        );
        assert_eq!(registry.take_released(), []);
    }

    #[test]
    fn a_stopping_bots_resting_report_settles_and_a_running_one_is_held() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 4);
        registry
            .settle(
                agent(1),
                lease.generation,
                vec![(bot(1), entry(Hold::Assigned, Some(StopState::Move)))],
                ServerMessage::Reconcile(Vec::new()),
            )
            .unwrap();

        let running = registry.admit(&lease, &online(bot(1))).0;
        let resting = registry
            .admit(
                &lease,
                &AgentMessage::Status(snapshot(
                    bot(1),
                    BotState::Failed {
                        reason: FailReason::Auth,
                    },
                )),
            )
            .0;

        assert_eq!(
            running,
            Verdict::Held {
                at: at(T0),
                settles: false
            }
        );
        assert_eq!(
            resting,
            Verdict::Held {
                at: at(T0),
                settles: true
            }
        );
    }

    #[test]
    fn a_requeued_release_comes_back() {
        let (registry, _) = registry();
        let released = Released {
            agent: agent(1),
            generation: 1,
            bot: bot(1),
        };

        registry.requeue_released(released);

        assert_eq!(registry.take_released(), [released]);
    }

    /// Agent 1's `Removed` of bot 1 from a stream that held it releasing,
    /// and that stream's generation; the stream has ended.
    fn removed_on_an_ended_stream(registry: &AgentRegistry) -> u64 {
        let (lease, _queued, _ended) = live(registry, 1, 4);
        settle(registry, &lease, &[(1, Hold::Releasing)]);
        let _ = registry.admit(&lease, &event(bot(1), BotEventKind::Removed));
        let generation = lease.generation;
        let _ = lease.free();
        generation
    }

    #[test]
    fn a_note_outlives_its_stream_until_a_newer_stream_of_its_agent_is_welcomed() {
        let (registry, _) = registry();
        let generation = removed_on_an_ended_stream(&registry);

        let alone = registry.take_released();
        for note in &alone {
            registry.requeue_released(*note);
        }
        let claimed = registry.claim(&admitted(2)).unwrap();
        let while_claimed = registry.take_released();
        for note in &while_claimed {
            registry.requeue_released(*note);
        }
        let (queue, _queued) = mpsc::channel(4);
        let _ended = registry
            .activate(&claimed, activation(queue, Vec::new()))
            .unwrap();
        let once_welcomed = registry.take_released();

        let note = Released {
            agent: agent(1),
            generation,
            bot: bot(1),
        };
        assert_eq!(alone, [note]);
        assert_eq!(while_claimed, [note], "a hello isn't a welcome yet");
        assert_eq!(once_welcomed, [], "a newer stream was welcomed");
    }

    #[test]
    fn a_removed_still_to_be_cleared_reads_as_releasing_in_the_view() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 4);
        settle(&registry, &lease, &[(1, Hold::Releasing)]);
        let _ = registry.admit(&lease, &event(bot(1), BotEventKind::Removed));

        let pending = registry.placement_view();
        let taken = registry.take_released();
        let cleared = registry.placement_view();
        registry.requeue_released(*taken.first().unwrap());
        let requeued = registry.placement_view();

        let holds = |view: &[LiveView]| match &view.first().unwrap().phase {
            ViewPhase::Reconciled(held) => held
                .iter()
                .map(|held| (held.bot, held.entry.hold))
                .collect::<Vec<_>>(),
            ViewPhase::Listed(_) => panic!("reconciled"),
        };
        assert_eq!(holds(&pending), [(bot(1), Hold::Releasing)]);
        assert_eq!(holds(&cleared), [], "taken: its row is being cleared");
        assert_eq!(holds(&requeued), [(bot(1), Hold::Releasing)]);
    }

    #[test]
    fn a_note_is_dropped_while_its_agents_stream_holds_the_bot_assigned() {
        let (registry, _) = registry();
        let generation = removed_on_an_ended_stream(&registry);
        let (lease, _queued, _ended) = live(&registry, 2, 4);
        settle(&registry, &lease, &[(1, Hold::Assigned)]);
        // As if the note came from this very stream.
        let _ = registry.take_released();
        registry.requeue_released(Released {
            agent: agent(1),
            generation: lease.generation,
            bot: bot(1),
        });

        let taken = registry.take_released();

        assert!(generation < lease.generation);
        assert_eq!(taken, []);
    }

    // --- Session requests (P10.6; S57) -------------------------------------------------

    fn session_request(number: u64, bot: BotId) -> SessionRequest {
        SessionRequest {
            id: SessionRequestId::new(NonZeroU64::new(number).unwrap()),
            bot_id: bot,
            fresh: false,
        }
    }

    fn ask(registry: &AgentRegistry, lease: &Lease, request: SessionRequest) -> Verdict {
        registry
            .admit(lease, &AgentMessage::SessionRequest(request))
            .0
    }

    /// An online session whose token is `token`.
    fn online_session(token: &str) -> SessionCredentials {
        SessionCredentials::Online {
            username: "AfkBot1".parse().unwrap(),
            uuid: "4566e69f-c907-48ee-8d71-d7ba5aa00d20"
                .parse::<fleet_core::value::McUuid>()
                .unwrap()
                .into(),
            access_token: secrecy::SecretString::from(token),
            expires_at: at("2027-01-16T08:00:00Z"),
        }
    }

    fn grant(number: u64, bot: BotId, token: &str) -> SessionAnswer {
        SessionAnswer {
            number: NonZeroU64::new(number).unwrap(),
            bot,
            outcome: SessionOutcome::Grant(online_session(token)),
        }
    }

    fn denial(number: u64, bot: BotId, retryable: bool) -> SessionAnswer {
        SessionAnswer {
            number: NonZeroU64::new(number).unwrap(),
            bot,
            outcome: SessionOutcome::Denied { retryable },
        }
    }

    /// The session answer `message` is, read back as the agent reads it.
    fn answered(message: wire::ServerMessage) -> ServerMessage {
        let answer = ServerMessage::try_from(message).unwrap();
        assert!(
            matches!(
                answer,
                ServerMessage::SessionGrant(_) | ServerMessage::SessionDenied(_)
            ),
            "{answer:?}"
        );
        answer
    }

    #[test]
    fn a_session_request_passes_only_for_a_bot_its_reconciled_stream_holds_assigned() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) =
            live_with(&registry, 1, 8, vec![snapshot(bot(1), BotState::Stopped)]);

        let unreconciled = ask(&registry, &lease, session_request(1, bot(1)));
        settle(
            &registry,
            &lease,
            &[(1, Hold::Assigned), (2, Hold::Releasing)],
        );
        let assigned = ask(&registry, &lease, session_request(2, bot(1)));
        let releasing = ask(&registry, &lease, session_request(3, bot(2)));
        let unknown = ask(&registry, &lease, session_request(4, bot(3)));
        assert!(registry.end(agent(1), StreamEnd::StreamError));
        let ended = ask(&registry, &lease, session_request(5, bot(1)));

        assert_eq!(
            unreconciled,
            Verdict::DenySession(session_request(1, bot(1))),
            "a hello's bot before its welcome is reconciled"
        );
        assert_eq!(assigned, Verdict::Session(session_request(2, bot(1))));
        assert_eq!(releasing, Verdict::DenySession(session_request(3, bot(2))));
        assert_eq!(unknown, Verdict::DenySession(session_request(4, bot(3))));
        assert_eq!(ended, Verdict::DenySession(session_request(5, bot(1))));
    }

    #[test]
    fn denied_session_requests_are_counted_with_the_agents_refused_reports() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 8);
        settle(&registry, &lease, &[(1, Hold::Assigned)]);

        let _ = ask(&registry, &lease, session_request(1, bot(2)));
        let _ = ask(&registry, &lease, session_request(2, bot(1)));
        registry.count_denied_session(&lease);
        let _ = registry.admit(&lease, &status(bot(3)));

        assert_eq!(
            registry.take_refused_reports(),
            [RefusedReports {
                agent: agent(1),
                name: AgentName::try_from("agent-1").unwrap(),
                count: 1,
                sessions_denied: 2,
                chat_dropped: 0,
            }],
            "the unheld bot's request and the full queue; not the held bot's"
        );
        assert_eq!(registry.take_refused_reports(), []);
    }

    #[test]
    fn a_grant_goes_only_to_its_stream_while_it_holds_the_bot_assigned() {
        let (registry, _) = registry();
        let (lease, mut queued, _ended) = live(&registry, 1, 8);
        settle(
            &registry,
            &lease,
            &[(1, Hold::Assigned), (2, Hold::Releasing)],
        );
        assert!(queued.try_recv().is_ok(), "the reconcile");
        let generation = lease.generation;

        let granted = registry.answer_session(agent(1), generation, grant(7, bot(1), "token-1"));
        let releasing = registry.answer_session(agent(1), generation, grant(8, bot(2), "token-2"));
        let unheld = registry.answer_session(agent(1), generation, grant(9, bot(3), "token-3"));
        let elsewhere = registry.answer_session(agent(2), generation, grant(7, bot(1), "token-1"));

        assert_eq!(granted, Ok(()));
        let ServerMessage::SessionGrant(sent) = answered(queued.try_recv().unwrap()) else {
            unreachable!("answered checks the kind");
        };
        assert_eq!(sent.id.get().get(), 7, "the agent's own number");
        assert_eq!(sent.bot_id, bot(1));
        let SessionCredentials::Online { access_token, .. } = sent.credentials else {
            panic!("an online session");
        };
        assert_eq!(
            secrecy::ExposeSecret::expose_secret(&access_token),
            "token-1"
        );
        assert_eq!(releasing, Err(AnswerRefused::NotHeld));
        assert_eq!(unheld, Err(AnswerRefused::NotHeld));
        assert_eq!(elsewhere, Err(AnswerRefused::NotConnected));
        assert!(queued.try_recv().is_err(), "nothing else was queued");
    }

    #[test]
    fn a_denial_goes_to_its_stream_whatever_it_holds() {
        let (registry, _) = registry();
        let (lease, mut queued, _ended) = live(&registry, 1, 8);

        let denied = registry.answer_session(agent(1), lease.generation, denial(3, bot(4), true));

        assert_eq!(denied, Ok(()));
        let ServerMessage::SessionDenied(sent) = answered(queued.try_recv().unwrap()) else {
            panic!("a denial");
        };
        assert_eq!(
            sent,
            SessionDenied {
                id: SessionRequestId::new(NonZeroU64::new(3).unwrap()),
                bot_id: bot(4),
                retryable: true,
            }
        );
    }

    /// A newer stream of the agent numbers its requests from 1 again: an
    /// answer for an older stream never reaches it (S57).
    #[test]
    fn a_session_answer_never_reaches_a_newer_or_an_ended_stream() {
        let (registry, _) = registry();
        let (old, _old_queued, _old_ended) = live(&registry, 1, 8);
        settle(&registry, &old, &[(1, Hold::Assigned)]);
        let asked = old.generation;
        drop(old);
        let (new, mut queued, _ended) = live(&registry, 1, 8);
        settle(&registry, &new, &[(1, Hold::Assigned)]);
        assert!(queued.try_recv().is_ok(), "the reconcile");

        let granted = registry.answer_session(agent(1), asked, grant(1, bot(1), "token-1"));
        let denied = registry.answer_session(agent(1), asked, denial(1, bot(1), true));
        assert!(registry.end(agent(1), StreamEnd::StreamError));
        let ended = registry.answer_session(agent(1), new.generation, denial(2, bot(1), true));

        assert_eq!(granted, Err(AnswerRefused::NotConnected));
        assert_eq!(denied, Err(AnswerRefused::NotConnected));
        assert_eq!(ended, Err(AnswerRefused::NotConnected), "halted");
        assert!(
            queued.try_recv().is_err(),
            "nothing reached the newer stream"
        );
    }

    #[test]
    fn a_session_answer_to_a_full_queue_ends_the_stream_too_slow() {
        let (registry, _) = registry();
        let (lease, _queued, mut ended) = live(&registry, 1, 1);
        settle(&registry, &lease, &[(1, Hold::Assigned)]);

        let refused = registry.answer_session(agent(1), lease.generation, grant(1, bot(1), "t"));

        assert_eq!(refused, Err(AnswerRefused::QueueFull));
        assert_eq!(ended.try_recv(), Ok(StreamEnd::TooSlow));
    }

    #[test]
    fn a_session_answer_over_the_limit_is_unsendable_and_the_stream_stays() {
        let clock = ManualClock::new(at(T0));
        let registry = AgentRegistry::with_limit(Arc::new(clock), 64, Arc::new(NoReconcile));
        let (lease, mut queued, mut ended) = live(&registry, 1, 8);
        settle(&registry, &lease, &[(1, Hold::Assigned)]);
        let _ = queued.try_recv();

        let refused = registry.answer_session(
            agent(1),
            lease.generation,
            grant(1, bot(1), &"t".repeat(128)),
        );

        assert_eq!(refused, Err(AnswerRefused::Unsendable));
        assert!(queued.try_recv().is_err());
        assert!(ended.try_recv().is_err(), "the stream stays");
    }

    // --- Reading the holds --------------------------------------------------------------------

    #[test]
    fn freeing_the_lease_frees_the_slot_and_hands_over_this_streams_reports() {
        let (registry, clock) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 4);
        settle(
            &registry,
            &lease,
            &[(1, Hold::Assigned), (2, Hold::Assigned)],
        );
        clock.advance(Duration::from_secs(2));
        let _ = registry.admit(&lease, &online(bot(1)));

        let held = lease.free();

        assert_eq!(
            held,
            [(
                bot(1),
                LastReport {
                    state: LastState::Online,
                    at: at("2027-01-15T08:00:02Z"),
                }
            )]
        );
        assert!(registry.claim(&admitted(2)).is_ok(), "the slot is free");
    }

    #[test]
    fn held_answers_only_from_the_agents_current_stream() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 4);
        settle(
            &registry,
            &lease,
            &[(1, Hold::Assigned), (2, Hold::Releasing)],
        );
        let _ = registry.admit(&lease, &online(bot(1)));

        let assigned = registry.held(agent(1), bot(1)).unwrap();
        let releasing = registry.held(agent(1), bot(2));
        drop(lease);
        let (next, _queued, _ended) = live(&registry, 2, 4);
        settle(&registry, &next, &[(1, Hold::Assigned)]);
        let on_the_next_stream = registry.held(agent(1), bot(1)).unwrap();

        assert_eq!(assigned.since, at(T0));
        assert!(matches!(
            assigned.report.map(|report| report.state),
            Some(BotState::Online { .. })
        ));
        assert_eq!(releasing, None);
        assert_eq!(on_the_next_stream.report, None, "an older stream's report");
        assert_eq!(registry.held(agent(9), bot(1)), None);
    }

    #[tokio::test(start_paused = true)]
    async fn the_next_deadline_is_the_nearest_bound_of_a_bot_being_stopped_after_a_moment() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 4);
        let now = Instant::now();
        let stopping = |seconds| {
            entry(
                Hold::Assigned,
                Some(StopState::Unqualified {
                    until: now + Duration::from_secs(seconds),
                }),
            )
        };
        assert_eq!(registry.next_deadline(now), None);

        registry
            .settle(
                agent(1),
                lease.generation,
                vec![
                    (bot(1), stopping(60)),
                    (bot(2), stopping(30)),
                    (bot(3), entry(Hold::Assigned, Some(StopState::Move))),
                ],
                ServerMessage::Reconcile(Vec::new()),
            )
            .unwrap();

        assert_eq!(
            registry.next_deadline(now),
            Some(now + Duration::from_secs(30))
        );
        assert_eq!(
            registry.next_deadline(now + Duration::from_secs(30)),
            Some(now + Duration::from_secs(60)),
            "a bound that isn't later is left out"
        );
        assert_eq!(registry.next_deadline(now + Duration::from_secs(60)), None);
    }

    /// The hints a registry sent.
    #[derive(Debug, Default)]
    struct Hinted(StdMutex<Vec<Change>>);

    impl Reconcile for Hinted {
        fn changed(&self, change: Change) {
            self.0.lock().unwrap().push(change);
        }
    }

    impl Hinted {
        fn take(&self) -> Vec<Change> {
            core::mem::take(&mut *self.0.lock().unwrap())
        }
    }

    fn hinting() -> (AgentRegistry, Arc<Hinted>) {
        let hinted = Arc::new(Hinted::default());
        let clock = ManualClock::new(at(T0));
        let registry = AgentRegistry::with_reconcile(
            Arc::new(clock),
            Arc::clone(&hinted) as Arc<dyn Reconcile>,
        );
        (registry, hinted)
    }

    #[test]
    fn a_welcome_a_settled_report_and_an_end_each_hint_the_reconciler_once() {
        let (registry, hinted) = hinting();

        let (lease, _queued, _ended) = live(&registry, 1, 4);
        let welcomed = hinted.take();
        settle(
            &registry,
            &lease,
            &[(1, Hold::Assigned), (2, Hold::Releasing)],
        );
        let _ = registry.admit(&lease, &online(bot(1)));
        let passed = hinted.take();
        let _ = registry.admit(&lease, &event(bot(2), BotEventKind::Removed));
        let settled = hinted.take();
        let _ = lease.free();
        let ended = hinted.take();

        assert_eq!(welcomed, [Change::Welcomed(agent(1))]);
        assert_eq!(passed, [], "a report that passes changes nothing");
        assert_eq!(settled, [Change::Settled(agent(1))]);
        assert_eq!(ended, [Change::Ended(agent(1))]);
    }

    #[test]
    fn a_dropped_live_lease_hints_its_end_and_a_claim_never_welcomed_hints_nothing() {
        let (registry, hinted) = hinting();
        let claimed = registry.claim(&admitted(1)).unwrap();

        drop(claimed);
        let unwelcomed = hinted.take();
        let (lease, _queued, _ended) = live(&registry, 2, 4);
        drop(lease);

        assert_eq!(unwelcomed, []);
        assert_eq!(
            hinted.take(),
            [Change::Welcomed(agent(1)), Change::Ended(agent(1))]
        );
    }

    #[test]
    fn forgotten_bots_lose_their_latest_report() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 4);
        settle(
            &registry,
            &lease,
            &[(1, Hold::Assigned), (2, Hold::Assigned)],
        );
        let _ = registry.admit(&lease, &online(bot(1)));
        let _ = registry.admit(&lease, &online(bot(2)));

        registry.retain_latest(&|id| id == bot(2));

        assert_eq!(registry.latest(bot(1)), None);
        assert!(registry.latest(bot(2)).is_some());
    }

    #[test]
    fn a_held_bots_entry_keeps_the_spec_last_sent() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 4);
        settle(&registry, &lease, &[]);
        let spec = BotSpec {
            id: bot(1),
            account: fleet_core::bot::BotAccount::Offline("AfkBot1".parse().unwrap()),
            server: "play.example.com".parse().unwrap(),
            mode: fleet_core::mode::ModeDefinition::afk(),
            desired: DesiredRunState::Running,
            conflict_texts: fleet_core::disconnect::ConflictTexts::default(),
            chat_rate: fleet_core::chat::ChatRate::default(),
        };

        registry
            .hold_and_send(
                agent(1),
                lease.generation,
                bot(1),
                HoldEntry {
                    sent: Some(spec.clone()),
                    ..entry(Hold::Assigned, None)
                },
                None,
            )
            .unwrap();

        let view = registry.placement_view();
        let ViewPhase::Reconciled(holds) = &view.first().unwrap().phase else {
            panic!("reconciled");
        };
        assert_eq!(holds.first().unwrap().entry.sent, Some(spec));
    }

    // --- Conflicts --------------------------------------------------------------------------

    #[test]
    fn a_message_after_a_refused_claim_is_a_conflict_once_an_hour() {
        let (registry, clock) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 2);
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
        let (lease, _queued, _ended) = live(&registry, 1, 2);
        let _ = registry.claim(&admitted(2));

        // The old process is gone: its stream ends without a word.
        drop(lease);
        let (next, _queued, _ended) = live(&registry, 2, 2);

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
            _running: &[BotSnapshot],
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
        let (_lease, _queued, mut ended) = live(&registry, 1, 2);
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
        let (_lease, _queued, mut ended) = live(&registry, 1, 2);

        registry.sweep_once(&sweeper).await;

        assert_eq!(sweeper.calls.lock().unwrap().len(), 1);
        assert!(ended.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_sweep_never_ends_a_newer_stream_of_the_same_agent() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 2);
        let swept = registry.live_certificates();
        drop(lease);
        let (_lease, _queued, mut ended) = live(&registry, 2, 2);

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
        let (_lease, _queued, _ended) = live(&registry, 1, 2);
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

    // --- The bots' endpoints (P11.2; ADR-0019, S48, S49) --------------------------------

    /// The lifecycle command for `bot` as the wire carries it.
    fn lifecycle(bot: BotId, command: Lifecycle) -> wire::ServerMessage {
        wire::ServerMessage::try_from(ServerMessage::Lifecycle(LifecycleCommand {
            bot_id: bot,
            command,
        }))
        .unwrap()
    }

    #[test]
    fn a_command_goes_only_to_the_stream_that_holds_its_bot_assigned() {
        let (registry, _) = registry();
        let (lease, mut queued, _ended) = live(&registry, 1, 4);
        settle(
            &registry,
            &lease,
            &[(1, Hold::Assigned), (2, Hold::Releasing)],
        );
        assert!(queued.try_recv().is_ok(), "the reconcile");

        let generation = lease.generation;
        let sent = registry.command(agent(1), generation, bot(1), Lifecycle::Resume);
        let releasing = registry.command(agent(1), generation, bot(2), Lifecycle::Resume);
        let unheld = registry.command(agent(1), generation, bot(3), Lifecycle::Restart);
        let elsewhere = registry.command(agent(2), generation, bot(1), Lifecycle::Reset);

        assert_eq!(sent, Ok(()));
        assert_eq!(
            queued.try_recv().unwrap(),
            lifecycle(bot(1), Lifecycle::Resume)
        );
        assert_eq!(releasing, Err(CommandRefused::NotHeld));
        assert_eq!(unheld, Err(CommandRefused::NotHeld));
        assert_eq!(
            elsewhere,
            Err(CommandRefused::NotConnected),
            "no stream at all"
        );
        assert!(queued.try_recv().is_err(), "nothing else was queued");
    }

    #[test]
    fn a_command_never_reaches_an_unreconciled_or_ended_stream() {
        let (registry, _) = registry();
        let (lease, mut queued, _ended) = live(&registry, 1, 4);

        let unreconciled = registry.command(agent(1), lease.generation, bot(1), Lifecycle::Resume);
        settle(&registry, &lease, &[(1, Hold::Assigned)]);
        assert!(registry.end(agent(1), StreamEnd::StreamError));
        let ended = registry.command(agent(1), lease.generation, bot(1), Lifecycle::Resume);

        assert_eq!(unreconciled, Err(CommandRefused::NotHeld));
        assert_eq!(
            ended,
            Err(CommandRefused::NotConnected),
            "halted until its lease drops"
        );
        while let Ok(message) = queued.try_recv() {
            assert!(
                !matches!(message.msg, Some(wire::server_message::Msg::Lifecycle(_))),
                "no command was queued"
            );
        }
    }

    /// The stream a command's caller found holding the bot, and no other: a
    /// stream that replaced it, settled or not, gets nothing (S48, PR #54's
    /// review).
    #[test]
    fn a_command_goes_only_to_the_generation_found_holding_its_bot() {
        let (registry, _) = registry();
        let (old, _old_queued, _old_ended) = live(&registry, 1, 4);
        settle(&registry, &old, &[(1, Hold::Assigned)]);
        let found = LiveAgents::held(&registry, agent(1), bot(1)).unwrap();
        // The stream is replaced: the old lease freed, a new hello welcomed.
        drop(old);
        let (new, mut queued, _ended) = live(&registry, 1, 4);

        let unsettled = registry.command(agent(1), found.generation, bot(1), Lifecycle::Resume);
        settle(&registry, &new, &[(1, Hold::Assigned)]);
        assert!(queued.try_recv().is_ok(), "the reconcile");
        let settled = registry.command(agent(1), found.generation, bot(1), Lifecycle::Resume);
        let current = registry.command(agent(1), new.generation, bot(1), Lifecycle::Restart);

        assert_ne!(found.generation, new.generation);
        assert_eq!(unsettled, Err(CommandRefused::NotConnected));
        assert_eq!(settled, Err(CommandRefused::NotConnected));
        assert_eq!(current, Ok(()));
        assert_eq!(
            queued.try_recv().unwrap(),
            lifecycle(bot(1), Lifecycle::Restart),
            "only the current generation's command"
        );
        assert!(queued.try_recv().is_err(), "nothing else was queued");
    }

    #[test]
    fn a_command_to_a_full_queue_ends_the_stream_too_slow() {
        let (registry, _) = registry();
        let (lease, _queued, mut ended) = live(&registry, 1, 1);
        settle(&registry, &lease, &[(1, Hold::Assigned)]);

        let refused = registry.command(agent(1), lease.generation, bot(1), Lifecycle::Restart);

        assert_eq!(refused, Err(CommandRefused::QueueFull));
        assert_eq!(ended.try_recv(), Ok(StreamEnd::TooSlow));
    }

    #[test]
    fn a_bots_kept_report_is_read_taken_and_put_back_unless_a_newer_one_came() {
        let (registry, clock) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 4);
        settle(&registry, &lease, &[(1, Hold::Assigned)]);
        assert!(matches!(
            registry.admit(&lease, &status(bot(1))).0,
            Verdict::Held { .. }
        ));
        let kept = LiveAgents::latest(&registry, bot(1)).unwrap();

        let taken = registry.take_latest(bot(1));
        let gone = LiveAgents::latest(&registry, bot(1));
        registry.put_back_latest(kept.clone());
        let back = LiveAgents::latest(&registry, bot(1));
        registry.take_latest(bot(1));
        clock.advance(Duration::from_secs(1));
        assert!(matches!(
            registry.admit(&lease, &online(bot(1))).0,
            Verdict::Held { .. }
        ));
        registry.put_back_latest(kept.clone());

        assert_eq!(kept.snapshot.state, BotState::Stopped);
        assert_eq!(kept.agent, agent(1));
        assert_eq!(taken, Some(kept.clone()));
        assert_eq!(gone, None);
        assert_eq!(back, Some(kept));
        assert!(
            matches!(
                LiveAgents::latest(&registry, bot(1))
                    .unwrap()
                    .snapshot
                    .state,
                BotState::Online { .. }
            ),
            "the newer report stays"
        );
        assert_eq!(registry.take_latest(bot(9)), None);
    }

    // --- What reaches the ingest (P10.8; S28, S70) ---------------------------------------

    fn paused_state() -> BotState {
        BotState::Paused {
            reason: fleet_core::bot::PauseReason::Conflict {
                kind: fleet_core::disconnect::ConflictKind::DuplicateLogin,
            },
        }
    }

    fn paused(bot: BotId) -> AgentMessage {
        AgentMessage::Status(snapshot(bot, paused_state()))
    }

    fn failed(bot: BotId, reason: FailReason) -> AgentMessage {
        AgentMessage::Status(snapshot(bot, BotState::Failed { reason }))
    }

    #[test]
    fn a_held_bots_status_and_events_go_to_the_ingest_with_their_receipt_time() {
        let (registry, clock) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 4);
        settle(&registry, &lease, &[(1, Hold::Assigned)]);
        clock.advance(Duration::from_secs(2));

        let verdicts = [
            registry.admit(&lease, &online(bot(1))).0,
            registry.admit(&lease, &event(bot(1), BotEventKind::Died)).0,
            registry
                .admit(
                    &lease,
                    &event(
                        bot(1),
                        BotEventKind::ChatReceived(fleet_core::chat::IncomingChat::system("hello")),
                    ),
                )
                .0,
        ];

        let held = Verdict::Held {
            at: at("2027-01-15T08:00:02Z"),
            settles: false,
        };
        assert_eq!(verdicts, [held.clone(), held.clone(), held]);
    }

    #[test]
    fn a_listed_releasing_or_unheld_bots_reports_never_go_to_the_ingest() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) =
            live_with(&registry, 1, 4, vec![snapshot(bot(1), BotState::Stopped)]);

        let listed = [
            registry.admit(&lease, &online(bot(1))).0,
            registry.admit(&lease, &event(bot(1), BotEventKind::Died)).0,
        ];
        settle(&registry, &lease, &[(2, Hold::Releasing)]);
        let after = [
            registry.admit(&lease, &online(bot(1))).0,
            registry.admit(&lease, &online(bot(2))).0,
            registry.admit(&lease, &event(bot(2), BotEventKind::Died)).0,
            registry.admit(&lease, &online(bot(3))).0,
        ];

        assert_eq!(listed, [Verdict::Pass, Verdict::Dropped]);
        assert!(
            after.iter().all(|verdict| *verdict == Verdict::Dropped),
            "{after:?}"
        );
    }

    #[test]
    fn an_ended_streams_reports_never_go_to_the_ingest() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 4);
        settle(&registry, &lease, &[(1, Hold::Assigned)]);

        assert!(registry.end(agent(1), StreamEnd::Disabled));
        let verdicts = [
            registry.admit(&lease, &online(bot(1))).0,
            registry.admit(&lease, &event(bot(1), BotEventKind::Died)).0,
        ];

        assert_eq!(verdicts, [Verdict::Pass, Verdict::Pass]);
    }

    #[test]
    fn dropped_chat_is_counted_with_the_agents_refused_reports() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 4);
        settle(&registry, &lease, &[(1, Hold::Assigned)]);

        registry.count_dropped_chat(&lease);
        registry.count_dropped_chat(&lease);

        assert_eq!(
            registry.take_refused_reports(),
            vec![RefusedReports {
                agent: agent(1),
                name: AgentName::try_from("agent-1").unwrap(),
                count: 0,
                sessions_denied: 0,
                chat_dropped: 2,
            }]
        );
    }

    // --- Sticky crossings (U16) ----------------------------------------------------------

    fn written(bot: BotId, state: LastState) -> StickyWrite {
        StickyWrite {
            bot,
            agent: agent(1),
            last: LastReport { state, at: at(T0) },
        }
    }

    #[test]
    fn a_status_entering_or_leaving_paused_or_failed_marks_its_bot_once() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 4);
        settle(
            &registry,
            &lease,
            &[(1, Hold::Assigned), (2, Hold::Assigned)],
        );

        let _ = registry.admit(&lease, &online(bot(1)));
        let none_yet = StickyMarks::any(&registry);
        let _ = registry.admit(&lease, &paused(bot(1)));
        let _ = registry.admit(&lease, &failed(bot(2), FailReason::Auth));
        let entered = StickyMarks::any(&registry);
        let first = StickyMarks::take(&registry);
        let after_take = StickyMarks::any(&registry);
        let _ = registry.admit(&lease, &paused(bot(1)));
        let same_again = StickyMarks::take(&registry);
        let _ = registry.admit(&lease, &online(bot(1)));
        let _ = registry.admit(&lease, &failed(bot(2), FailReason::CrashLoop));
        let second = StickyMarks::take(&registry);

        assert!(!none_yet, "online crosses nothing");
        assert!(entered);
        assert_eq!(
            first,
            vec![
                written(bot(1), LastState::from(paused_state())),
                written(bot(2), LastState::Failed(FailReason::Auth)),
            ]
        );
        assert!(!after_take);
        assert_eq!(same_again, Vec::new(), "the same sticky state again");
        assert_eq!(
            second,
            vec![
                written(bot(1), LastState::Online),
                written(bot(2), LastState::Failed(FailReason::CrashLoop)),
            ],
            "leaving it, and another reason"
        );
    }

    #[test]
    fn a_listed_bots_crossing_before_the_reconcile_is_marked_at_the_settle() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live_with(
            &registry,
            1,
            4,
            vec![
                snapshot(bot(1), BotState::Stopped),
                snapshot(bot(2), BotState::Stopped),
            ],
        );

        let _ = registry.admit(&lease, &paused(bot(1)));
        let _ = registry.admit(&lease, &online(bot(2)));
        let before = StickyMarks::any(&registry);
        settle(
            &registry,
            &lease,
            &[(1, Hold::Assigned), (2, Hold::Assigned)],
        );
        let taken = StickyMarks::take(&registry);

        assert!(!before, "nothing is marked before the reconcile");
        assert_eq!(taken.iter().map(|w| w.bot).collect::<Vec<_>>(), [bot(1)]);
    }

    #[test]
    fn the_take_writes_only_a_bots_latest_report_from_the_stream_that_holds_it_assigned() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 8);
        settle(
            &registry,
            &lease,
            &[
                (1, Hold::Assigned),
                (2, Hold::Assigned),
                (3, Hold::Assigned),
            ],
        );
        for n in 1..=3 {
            let _ = registry.admit(&lease, &paused(bot(n)));
        }

        // A resume took the first bot's kept report (S48); the second is
        // being released.
        let _ = registry.take_latest(bot(1));
        registry
            .hold_and_send(
                agent(1),
                lease.generation,
                bot(2),
                entry(Hold::Releasing, None),
                Some(unassign(2)),
            )
            .unwrap();
        let taken = StickyMarks::take(&registry);

        assert_eq!(taken.iter().map(|w| w.bot).collect::<Vec<_>>(), [bot(3)]);
        assert!(
            !StickyMarks::any(&registry),
            "the refused marks are dropped"
        );
    }

    #[test]
    fn an_ended_streams_marks_are_never_taken() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 4);
        settle(&registry, &lease, &[(1, Hold::Assigned)]);
        let _ = registry.admit(&lease, &paused(bot(1)));

        assert!(registry.end(agent(1), StreamEnd::Disabled));

        assert_eq!(StickyMarks::take(&registry), Vec::new());
    }

    #[test]
    fn a_remark_marks_its_bots_again() {
        let (registry, _) = registry();
        let (lease, _queued, _ended) = live(&registry, 1, 4);
        settle(&registry, &lease, &[(1, Hold::Assigned)]);
        let _ = registry.admit(&lease, &paused(bot(1)));
        let first = StickyMarks::take(&registry);

        StickyMarks::remark(&registry, &[bot(1)]);
        let again = StickyMarks::take(&registry);

        assert_eq!(first.len(), 1);
        assert_eq!(again, first);
    }
}
