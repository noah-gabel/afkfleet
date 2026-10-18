//! [`Conductor`]: the one task that owns every Fleet call and the mapping
//! state of a managed agent (Plan.md P10.7; ADR-0019, S65, R47–R49).
//!
//! - **One FIFO.** The stream's reader hands the six instructions to an
//!   [`Inbox`] of [`INBOUND_QUEUE`] (the server's queue depth) and never
//!   waits: a full one ends the stream, and the next hello resyncs. Session
//!   answers go straight to the credentials, never through here.
//! - **The hello barrier** (R48): before each hello, the link queues a
//!   barrier behind what's queued, so the previous stream's instructions are
//!   applied first; the barrier drains the fleet's ready events, prepares the
//!   outbox, and answers with `snapshot_all` (bots being removed included),
//!   never with the conductor's own view. It clears what was pending and
//!   told: the reconcile resends what still holds.
//! - **Instructions:** a full reconcile opens the outbox's gate first (R49),
//!   removes every bot the hello listed or the stream told that it leaves
//!   out, in ID order, then applies in message order; an assignment applies
//!   with its restore; an unassignment removes; an update applies only to a
//!   bot this stream told, keeping a pending apply's restore; a spec that
//!   says Stopped forgets the bot's kept grant first. A removal the fleet
//!   doesn't know is answered with `Removed` at once (S44). `SendChat` and
//!   lifecycle commands apply only while their stream lives, and die with
//!   it; a lifecycle command makes exactly one Fleet call (S49), and a chat
//!   ticket is mapped inline, so its outcome can't beat it.
//! - **The fleet's answers** follow the `mapping` module's tables: `Busy` and
//!   `TimedOut` stay pending and are tried again after any `Removed` and
//!   every [`RETRY_TICK`]; `AtCapacity` and `AccountInUse` wait for a
//!   `Removed`, with one `warn`; `AccountChanged` is an `error` naming the
//!   bot; `ShuttingDown` is dropped.
//! - **Events** go to the outbox; a `Removed` forgets the bot's kept grant.
//!   A lag is one `warn` with the count and a resync from `snapshot_all`: a
//!   status per bot, and a `Removed` only for a bot being removed that the
//!   snapshot no longer lists. Lost alerts, answers and chat are counted,
//!   never invented.
//!
//! Every Fleet call is awaited in a branch body, never as a `select!`
//! future, and the ready events are drained after each.

mod chat;
mod mapping;
mod state;

use core::time::Duration;
use std::collections::BTreeSet;
use std::sync::Arc;

use fleet_core::bot::{BotSnapshot, BotSpec, DesiredRunState, Lifecycle, StickyState};
use fleet_core::id::BotId;
use fleet_proto::control::{Assignment, LifecycleCommand, SendChat};
use fleet_runtime::{ChatTicket, Fleet, FleetError, FleetEvent, FleetEventKind, RuntimeClock};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use self::chat::ChatRequests;
use self::mapping::Unapplied;
use self::state::{Book, Wait};
use crate::managed::credentials::{LinkGeneration, ManagedCredentials};
use crate::managed::outbound::Outbox;

/// How many instructions wait for the conductor: the server's queue depth.
pub const INBOUND_QUEUE: usize = 2048;

/// How often pending applies and busy removals are tried again.
pub const RETRY_TICK: Duration = Duration::from_secs(1);

/// The six instructions of a stream, in the order the reader read them.
#[derive(Debug, Clone)]
pub enum Instruction {
    /// The full reconcile: every bot the agent should run.
    Reconcile(Vec<Assignment>),
    /// Run this bot.
    Assign(Assignment),
    /// Stop running this bot.
    Unassign(BotId),
    /// A told bot's new spec.
    Update(BotSpec),
    /// Send a user's chat message.
    SendChat(SendChat),
    /// Restart, reset or resume a bot.
    Lifecycle(LifecycleCommand),
}

/// What the conductor takes, in order.
#[derive(Debug)]
enum Inbound {
    /// An instruction of the stream of `generation`.
    Instruction {
        generation: LinkGeneration,
        instruction: Instruction,
    },
    /// The barrier before a hello: its answer is the hello's snapshot.
    Barrier(oneshot::Sender<Result<Vec<BotSnapshot>, FleetError>>),
}

/// The conductor's FIFO, as the link holds it. Clones share it.
#[derive(Debug, Clone)]
pub struct Inbox(mpsc::Sender<Inbound>);

/// The FIFO was full, or the conductor is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InboxError {
    /// [`INBOUND_QUEUE`] instructions wait already.
    #[error("the conductor's queue is full")]
    Full,
    /// The conductor ended.
    #[error("the conductor ended")]
    Gone,
}

impl Inbox {
    /// Queues `instruction` of the stream of `generation`, never waiting.
    ///
    /// # Errors
    /// [`InboxError`].
    pub fn instruction(
        &self,
        generation: LinkGeneration,
        instruction: Instruction,
    ) -> Result<(), InboxError> {
        self.0
            .try_send(Inbound::Instruction {
                generation,
                instruction,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => InboxError::Full,
                mpsc::error::TrySendError::Closed(_) => InboxError::Gone,
            })
    }

    /// The barrier before a hello (R48): once everything queued is applied,
    /// the hello's snapshot. The caller bounds the wait.
    ///
    /// # Errors
    /// [`BarrierError`].
    pub async fn barrier(&self) -> Result<Vec<BotSnapshot>, BarrierError> {
        let (reply, answer) = oneshot::channel();
        // Waits for room: the previous stream's instructions drain first.
        self.0
            .send(Inbound::Barrier(reply))
            .await
            .map_err(|_| BarrierError::Gone)?;
        answer
            .await
            .map_err(|_| BarrierError::Gone)?
            .map_err(BarrierError::Fleet)
    }
}

/// Why the barrier gave no snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BarrierError {
    /// The conductor ended.
    #[error("the conductor ended")]
    Gone,
    /// The fleet didn't give its snapshot.
    #[error("the fleet's snapshot failed: {0}")]
    Fleet(FleetError),
}

/// The conductor's FIFO, as the conductor reads it: what the [`Inbox`] queued.
#[derive(Debug)]
pub struct Instructions(mpsc::Receiver<Inbound>);

/// A new FIFO of [`INBOUND_QUEUE`] instructions.
#[must_use]
pub fn inbox() -> (Inbox, Instructions) {
    let (sender, receiver) = mpsc::channel(INBOUND_QUEUE);
    (Inbox(sender), Instructions(receiver))
}

/// Everything a conductor is built from.
#[derive(Debug)]
pub struct ConductorParts {
    /// The fleet it drives.
    pub fleet: Fleet,
    /// The fleet's events, subscribed before its supervisor runs.
    pub events: broadcast::Receiver<FleetEvent>,
    /// Where events and answers go.
    pub outbox: Outbox,
    /// The managed credentials, whose kept grants it forgets.
    pub credentials: Arc<ManagedCredentials<Outbox>>,
    /// Its FIFO.
    pub inbound: Instructions,
    /// The run's clock, for the `Removed`s it answers itself.
    pub clock: RuntimeClock,
    /// `[runtime] max_bots`: the most pending applies.
    pub max_bots: usize,
    /// `[runtime] chat_queue`: with `max_bots`, the most chat tickets
    /// waiting.
    pub chat_queue: usize,
    /// `[runtime] event_buffer`: the most events drained after a call.
    pub event_buffer: usize,
}

/// The conductor; see the [module docs](self).
#[derive(Debug)]
pub struct Conductor {
    fleet: Fleet,
    events: broadcast::Receiver<FleetEvent>,
    events_open: bool,
    outbox: Outbox,
    credentials: Arc<ManagedCredentials<Outbox>>,
    inbound: mpsc::Receiver<Inbound>,
    clock: RuntimeClock,
    event_buffer: usize,
    book: Book,
    tickets: ChatRequests<ChatTicket>,
    /// A lag's resync that failed, tried again on the tick.
    resync_owed: bool,
}

impl Conductor {
    /// A conductor of `parts`.
    #[must_use]
    pub fn new(parts: ConductorParts) -> Self {
        Self {
            fleet: parts.fleet,
            events: parts.events,
            events_open: true,
            outbox: parts.outbox,
            credentials: parts.credentials,
            inbound: parts.inbound.0,
            clock: parts.clock,
            event_buffer: parts.event_buffer,
            book: Book::new(parts.max_bots),
            tickets: ChatRequests::new(parts.max_bots.saturating_mul(parts.chat_queue)),
            resync_owed: false,
        }
    }

    /// Runs until `stop`.
    pub async fn run(mut self, stop: CancellationToken) {
        let mut tick = tokio::time::interval(RETRY_TICK);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                // Every branch is cancel-safe: the token, `mpsc::recv`,
                // `Interval::tick` and `broadcast::recv` lose nothing when
                // another wins; the Fleet calls run in the bodies.
                () = stop.cancelled() => return,
                inbound = self.inbound.recv() => match inbound {
                    Some(inbound) => self.take(inbound).await,
                    // The run keeps an inbox until it ends.
                    None => return stop.cancelled().await,
                },
                _ = tick.tick() => self.tick().await,
                received = self.events.recv(), if self.events_open => {
                    self.received(received).await;
                }
            }
            self.drain().await;
        }
    }

    /// Takes one item of the FIFO.
    async fn take(&mut self, inbound: Inbound) {
        match inbound {
            Inbound::Barrier(reply) => {
                let snapshot = self.barrier().await;
                // A link that gave up waiting has ended its attempt.
                let _ = reply.send(snapshot);
            }
            Inbound::Instruction {
                generation,
                instruction,
            } => self.instruction(generation, instruction).await,
        }
    }

    /// The hello barrier (R48).
    async fn barrier(&mut self) -> Result<Vec<BotSnapshot>, FleetError> {
        self.drain().await;
        self.outbox.prepare();
        let snapshot = self.fleet.snapshot_all().await?;
        self.book
            .barrier(snapshot.iter().map(|bot| bot.bot_id).collect());
        Ok(snapshot)
    }

    /// One instruction of the stream of `generation`.
    async fn instruction(&mut self, generation: LinkGeneration, instruction: Instruction) {
        match instruction {
            Instruction::Reconcile(assignments) => self.reconcile(generation, assignments).await,
            Instruction::Assign(assignment) => {
                self.book.tell(assignment.spec.id);
                self.apply(assignment.spec, assignment.restore).await;
            }
            Instruction::Unassign(bot) => {
                self.book.untell(bot);
                self.credentials.forget(bot);
                self.remove(bot).await;
            }
            Instruction::Update(spec) => {
                if !self.book.told(spec.id) {
                    debug!(bot_id = %spec.id, "an update of a bot this stream didn't give was dropped");
                    return;
                }
                let restore = self.book.pending_restore(spec.id);
                self.apply(spec, restore).await;
            }
            Instruction::SendChat(chat) => self.send_chat(generation, chat).await,
            Instruction::Lifecycle(command) => self.lifecycle(generation, command).await,
        }
    }

    /// The stream's full reconcile: the gate, the removals, the applies.
    async fn reconcile(&mut self, generation: LinkGeneration, assignments: Vec<Assignment>) {
        let given: BTreeSet<BotId> = assignments
            .iter()
            .map(|assignment| assignment.spec.id)
            .collect();
        self.outbox.reconciled(generation, &given);
        for bot in self.book.reconcile(&given) {
            self.credentials.forget(bot);
            self.remove(bot).await;
        }
        let mut assignments = assignments.into_iter();
        while let Some(assignment) = assignments.next() {
            if self.apply(assignment.spec, assignment.restore).await == Outcome::EndsBatch {
                // The fleet is slow: the rest waits for the tick.
                for rest in assignments.by_ref() {
                    self.hold(rest.spec, rest.restore, Wait::Retry);
                }
            }
        }
    }

    /// Applies `spec` with `restore`, keeping it pending when the fleet
    /// says to try again.
    async fn apply(&mut self, spec: BotSpec, restore: Option<StickyState>) -> Outcome {
        let bot = spec.id;
        if spec.desired == DesiredRunState::Stopped {
            // The bot stops and stays; its session is over (S63).
            self.credentials.forget(bot);
        }
        let error = match self.fleet.apply(spec.clone(), restore).await {
            Ok(()) => {
                let _ = self.book.take_pending(bot);
                return Outcome::Applied;
            }
            Err(error) => error,
        };
        match mapping::unapplied(error) {
            Unapplied::Retry { ends_batch } => {
                self.hold(spec, restore, Wait::Retry);
                if ends_batch {
                    return Outcome::EndsBatch;
                }
            }
            Unapplied::AwaitRemoved => {
                warn!(bot_id = %bot, %error, "a bot waits for another bot's removal");
                self.hold(spec, restore, Wait::Removed);
            }
            Unapplied::Rejected => {
                error!(bot_id = %bot, %error, "the fleet refused a bot's spec; it was dropped");
                let _ = self.book.take_pending(bot);
            }
            Unapplied::Dropped => {
                debug!(bot_id = %bot, %error, "a bot's spec was dropped");
                let _ = self.book.take_pending(bot);
            }
        }
        Outcome::Pending
    }

    /// Keeps `spec` pending, or drops it with a `warn` when the book is full.
    fn hold(&mut self, spec: BotSpec, restore: Option<StickyState>, wait: Wait) {
        let bot = spec.id;
        if !self.book.hold(spec, restore, wait) {
            warn!(bot_id = %bot, "too many bots wait to be applied; this one was dropped");
        }
    }

    /// Removes `bot`; one the fleet doesn't know gets its `Removed` at once.
    async fn remove(&mut self, bot: BotId) {
        match self.fleet.remove(bot).await {
            Ok(()) => self.book.removing(bot),
            Err(FleetError::UnknownBot) => {
                self.outbox.removed(bot, self.clock.now());
            }
            Err(FleetError::Busy | FleetError::TimedOut) => self.book.unremoved(bot),
            Err(
                error @ (FleetError::ShuttingDown
                | FleetError::AtCapacity
                | FleetError::AccountChanged
                | FleetError::AccountInUse),
            ) => {
                debug!(bot_id = %bot, %error, "a removal was dropped");
            }
        }
    }

    /// A user's chat message, only while its stream lives.
    async fn send_chat(&mut self, generation: LinkGeneration, chat: SendChat) {
        let SendChat {
            request,
            bot_id: bot,
            message,
        } = chat;
        if !self.outbox.is_live(generation) {
            debug!(bot_id = %bot, "a chat message of an ended stream was dropped");
            return;
        }
        match self.fleet.send_chat(bot, message).await {
            // Mapped before any event is read, so its outcome finds it.
            Ok(ticket) => {
                if self.tickets.insert(ticket, request, bot).is_some() {
                    debug!(bot_id = %bot, "the oldest chat ticket was forgotten");
                }
            }
            Err(error) => {
                self.outbox
                    .chat_failed(bot, self.clock.now(), request, mapping::refused(error));
            }
        }
    }

    /// A user's lifecycle command: one Fleet call, only while its stream
    /// lives; a refusal is dropped (S49).
    async fn lifecycle(&mut self, generation: LinkGeneration, command: LifecycleCommand) {
        let LifecycleCommand {
            bot_id: bot,
            command,
        } = command;
        if !self.outbox.is_live(generation) {
            debug!(bot_id = %bot, "a lifecycle command of an ended stream was dropped");
            return;
        }
        let done = match command {
            Lifecycle::Restart => self.fleet.restart(bot).await,
            Lifecycle::Reset => self.fleet.reset(bot).await,
            Lifecycle::Resume => self.fleet.resume(bot).await,
        };
        if let Err(error) = done {
            debug!(bot_id = %bot, %error, "a lifecycle command was refused");
        }
    }

    /// The retry tick: pending applies, busy removals and an owed resync.
    async fn tick(&mut self) {
        self.retry(false).await;
        for bot in self.book.take_unremoved() {
            self.remove(bot).await;
        }
        if self.resync_owed {
            self.resync().await;
        }
    }

    /// Tries the pending applies again; after a `Removed`, those waiting for
    /// one too.
    async fn retry(&mut self, removed: bool) {
        let mut due = self.book.due(removed).into_iter();
        while let Some(pending) = due.next() {
            if self.apply(pending.spec, pending.restore).await == Outcome::EndsBatch {
                for rest in due.by_ref() {
                    self.hold(rest.spec, rest.restore, rest.wait);
                }
            }
        }
    }

    /// One `recv` of the fleet's events.
    async fn received(&mut self, received: Result<FleetEvent, RecvError>) {
        match received {
            Ok(event) => {
                if self.forward(event) {
                    self.retry(true).await;
                }
            }
            Err(RecvError::Lagged(missed)) => self.lagged(missed).await,
            Err(RecvError::Closed) => self.events_open = false,
        }
    }

    /// The ready events, at most `event_buffer`, without waiting.
    async fn drain(&mut self) {
        let mut removed = false;
        let mut lag = None;
        for _ in 0..self.event_buffer {
            match self.events.try_recv() {
                Ok(event) => removed |= self.forward(event),
                Err(TryRecvError::Lagged(missed)) => {
                    lag = Some(missed);
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => {
                    self.events_open = false;
                    break;
                }
            }
        }
        if let Some(missed) = lag {
            self.lagged(missed).await;
        }
        if removed {
            self.retry(true).await;
        }
    }

    /// Hands `event` to the outbox: whether it was a `Removed`.
    fn forward(&mut self, event: FleetEvent) -> bool {
        let FleetEvent {
            bot_id: bot,
            at,
            kind,
        } = event;
        match kind {
            FleetEventKind::StateChanged(snapshot) => self.outbox.status(snapshot),
            FleetEventKind::Died => self.outbox.died(bot, at),
            FleetEventKind::ChatReceived(chat) => self.outbox.received(bot, at, chat),
            FleetEventKind::ModeChatSent { message } => self.outbox.mode_sent(bot, at, message),
            FleetEventKind::Alert(notification) => self.outbox.alert(bot, at, notification),
            FleetEventKind::ChatSent { ticket } => {
                if let Some((request, owner)) = self.tickets.take(ticket) {
                    self.outbox.chat_sent(owner, at, request);
                } else {
                    debug!(bot_id = %bot, "a chat outcome nobody waits for was dropped");
                }
            }
            FleetEventKind::ChatFailed { ticket, reason } => {
                if let Some((request, owner)) = self.tickets.take(ticket) {
                    self.outbox
                        .chat_failed(owner, at, request, mapping::failed(reason));
                } else {
                    debug!(bot_id = %bot, "a chat outcome nobody waits for was dropped");
                }
            }
            FleetEventKind::Removed => {
                let _ = self.book.removed(bot);
                self.credentials.forget(bot);
                self.outbox.removed(bot, at);
                return true;
            }
        }
        false
    }

    /// The events lagged by `missed`: one `warn`, and a resync.
    async fn lagged(&mut self, missed: u64) {
        warn!(
            missed,
            "the agent's fleet events lagged; it resyncs from the fleet's snapshots, and the alerts, answers and chat lost are only counted"
        );
        self.resync().await;
    }

    /// A status for every bot, and a `Removed` for each bot being removed
    /// that the snapshot no longer lists.
    async fn resync(&mut self) {
        let snapshots = match self.fleet.snapshot_all().await {
            Ok(snapshots) => snapshots,
            Err(error) => {
                warn!(%error, "the fleet's snapshots for a resync failed; tried again on the tick");
                self.resync_owed = true;
                return;
            }
        };
        self.resync_owed = false;
        let present: BTreeSet<BotId> = snapshots.iter().map(|bot| bot.bot_id).collect();
        for snapshot in snapshots {
            self.outbox.status(snapshot);
        }
        for bot in self.book.vanished(&present) {
            self.credentials.forget(bot);
            self.outbox.removed(bot, self.clock.now());
        }
        self.retry(true).await;
    }
}

/// How an apply went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The fleet took it.
    Applied,
    /// It's pending, or dropped.
    Pending,
    /// It's pending, and the fleet was slow: the batch ends here.
    EndsBatch,
}
