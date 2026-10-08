//! [`Supervisor`]: owns the fleet's actors, restarts the ones that crash,
//! and answers the [`Fleet`](super::Fleet) handle's calls.

use core::fmt;
use core::time::Duration;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::bot::{
    BotEvent, BotNotification, BotRules, BotSnapshot, BotSpec, BotState, FailReason, StickyState,
    Transition, restore, transition,
};
use fleet_core::chat::ChatMessage;
use fleet_core::id::BotId;
use fleet_core::mc::{MinecraftConnector, SessionCredentialProvider};
use fleet_core::resilience::{CircuitBreaker, CircuitPolicy, FailureWindow, RetryPolicy};
use rand::rngs::StdRng;
use rand::{Rng as _, SeedableRng as _};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::{self, AbortHandle, JoinError, JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::{Span, debug, error, info, info_span, warn};

use super::command::{ChatAnswer, Command, Lifecycle, Reply};
use super::{FleetError, FleetParts, SendChatError, ShutdownReport};
use crate::actor::{ActorExit, BotActor, BotActorParts, BotCommand, BotInbox, CrashedTask};
use crate::chat::{ChatBucket, ChatError, ChatQuota, ChatTickets};
use crate::clock::RuntimeClock;
use crate::config::RuntimeConfig;
use crate::event::{FleetEvent, FleetEventKind};

/// What the supervisor keeps of one bot, across its actors.
struct Entry {
    spec: BotSpec,
    /// The bot's snapshot. Each actor gets a clone and publishes into it;
    /// its current value is where a restarted actor picks up.
    snapshot: watch::Sender<BotSnapshot>,
    /// The actor's copy of its circuit breaker, so a restart keeps it.
    breaker: watch::Sender<CircuitBreaker>,
    /// The bot's chat rate limit, kept so a restart doesn't refill it.
    bucket: ChatBucket,
    /// The actor's recent crashes.
    crashes: FailureWindow,
    /// The running actor; none after a second crash loop.
    actor: Option<Live>,
    /// Whether the bot is crash-looped: its actor was started for a crash
    /// loop, or it has none after the crash-loop actor crashed too.
    crash_looped: bool,
    /// Whether the bot is being removed.
    removing: bool,
    /// The bot's span, for the supervisor's logs about it.
    span: Span,
}

/// A running actor.
struct Live {
    inbox: BotInbox,
    token: CancellationToken,
    /// Its task in the supervisor's `JoinSet`.
    task: AbortHandle,
}

/// Owns the fleet's actors and answers the [`Fleet`](super::Fleet) handle's
/// calls (Plan.md P4.7; ADR-0013): see the module docs.
///
/// [`Fleet::new`](super::Fleet::new) builds it, and its caller runs
/// [`run`](Self::run) in a task it owns.
pub struct Supervisor<C, P> {
    connector: Arc<C>,
    credentials: Arc<P>,
    retry: RetryPolicy,
    circuit: CircuitPolicy,
    config: RuntimeConfig,
    clock: RuntimeClock,
    quota: ChatQuota,
    rng: StdRng,
    events: broadcast::Sender<FleetEvent>,
    tickets: ChatTickets,
    commands: mpsc::Receiver<Command>,
    /// The parent of every actor's token; a child of `run`'s token.
    run_token: CancellationToken,
    bots: BTreeMap<BotId, Entry>,
    actors: JoinSet<ActorExit>,
    /// Which bot each actor task runs.
    tasks: HashMap<task::Id, BotId>,
}

impl<C, P> fmt::Debug for Supervisor<C, P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Supervisor")
            .field("bots", &self.bots.len())
            .finish_non_exhaustive()
    }
}

impl<C: MinecraftConnector, P: SessionCredentialProvider> Supervisor<C, P> {
    /// Builds the supervisor from its parts, the checked chat quota, its
    /// command queue and the fleet's events.
    pub(super) fn new(
        parts: FleetParts<C, P>,
        quota: ChatQuota,
        commands: mpsc::Receiver<Command>,
        events: broadcast::Sender<FleetEvent>,
    ) -> Self {
        let FleetParts {
            connector,
            credentials,
            retry,
            circuit,
            config,
            anchor,
            seed,
        } = parts;
        Self {
            connector,
            credentials,
            retry,
            circuit,
            config,
            clock: RuntimeClock::new(anchor),
            quota,
            rng: StdRng::seed_from_u64(seed),
            events,
            tickets: ChatTickets::new(),
            commands,
            run_token: CancellationToken::new(),
            bots: BTreeMap::new(),
            actors: JoinSet::new(),
            tasks: HashMap::new(),
        }
    }

    /// Runs the fleet until a shutdown, until `cancel` is cancelled, or until
    /// every [`Fleet`](super::Fleet) handle is dropped. The last two shut the
    /// fleet down as `shutdown` does, within `RuntimeConfig::shutdown_timeout`.
    ///
    /// Every actor's token is a child of `cancel`.
    pub async fn run(mut self, cancel: CancellationToken) {
        self.run_token = cancel.child_token();
        let ending = loop {
            tokio::select! {
                biased;
                // Every branch is cancel-safe: `cancelled`, `join_next_with_id`
                // and `recv` lose nothing when another branch wins.
                () = cancel.cancelled() => break Ending::Cancelled,
                Some(done) = self.actors.join_next_with_id(), if !self.actors.is_empty() => {
                    self.joined(done);
                }
                command = self.commands.recv() => match command {
                    Some(command) => {
                        if let Some(ending) = self.command(command) {
                            break ending;
                        }
                    }
                    None => break Ending::Dropped,
                },
            }
        };
        self.shut_down(ending).await;
    }

    /// Handles one call. A shutdown ends the loop instead.
    fn command(&mut self, command: Command) -> Option<Ending> {
        // An error only means the caller stopped waiting.
        match command {
            Command::Apply {
                spec,
                restore,
                reply,
            } => {
                let _ = reply.send(self.apply(*spec, restore));
            }
            Command::Remove { id, reply } => {
                let _ = reply.send(self.remove(id));
            }
            Command::Lifecycle { id, action, reply } => {
                let _ = reply.send(self.lifecycle(id, action));
            }
            Command::SendChat { id, message, reply } => {
                let _ = reply.send(self.send_chat(id, message));
            }
            Command::Snapshot { id, reply } => {
                let _ = reply.send(self.snapshot(id));
            }
            Command::SnapshotAll { reply } => {
                let _ = reply.send(Ok(self.snapshot_all()));
            }
            Command::Shutdown { timeout, reply } => {
                return Some(Ending::Requested { timeout, reply });
            }
        }
        None
    }

    /// Takes on `spec`: a new bot, or a new spec for a known one.
    fn apply(&mut self, spec: BotSpec, restore: Option<StickyState>) -> Result<(), FleetError> {
        match self.bots.get_mut(&spec.id) {
            Some(entry) => update(entry, spec, restore),
            None => self.add(spec, restore),
        }
    }

    /// Adds a new bot and starts its actor: Stopped, or in `restore`.
    fn add(&mut self, spec: BotSpec, restore: Option<StickyState>) -> Result<(), FleetError> {
        let mut held_by_removing = false;
        for other in self
            .bots
            .values()
            .filter(|other| other.spec.account.clashes_with(&spec.account))
        {
            if !other.removing {
                return Err(FleetError::AccountInUse);
            }
            held_by_removing = true;
        }
        // The account is free once that bot's `Removed` goes out.
        if held_by_removing {
            return Err(FleetError::Busy);
        }
        if self.bots.len() >= self.config.max_bots.get() {
            return Err(FleetError::AtCapacity);
        }
        let id = spec.id;
        let entry = Entry {
            snapshot: watch::Sender::new(BotSnapshot {
                bot_id: id,
                state: BotState::Stopped,
                since: self.clock.now(),
                last_disconnect: None,
            }),
            breaker: watch::Sender::new(CircuitBreaker::new(self.circuit)),
            bucket: ChatBucket::with_quota(self.quota),
            crashes: FailureWindow::new(self.config.restart_limit, self.config.restart_window),
            actor: None,
            crash_looped: false,
            removing: false,
            span: info_span!("bot", bot_id = %id),
            spec,
        };
        self.bots.insert(id, entry);
        let start = Transition {
            state: restore.map_or(BotState::Stopped, BotState::from),
            effects: Vec::new(),
        };
        self.start_actor(id, start);
        Ok(())
    }

    /// Removes a bot: its actor stops it, and `Removed` follows once the
    /// actor has ended. A bot without an actor is removed at once.
    fn remove(&mut self, id: BotId) -> Result<(), FleetError> {
        let entry = self.bots.get_mut(&id).ok_or(FleetError::UnknownBot)?;
        if entry.removing {
            return Ok(());
        }
        entry.removing = true;
        if let Some(live) = &entry.actor {
            live.token.cancel();
        } else {
            self.removed(id);
        }
        Ok(())
    }

    /// Forgets a removed bot and publishes `Removed`.
    fn removed(&mut self, id: BotId) {
        if let Some(entry) = self.bots.remove(&id) {
            let _entered = entry.span.enter();
            info!("the bot was removed");
        }
        self.publish(id, FleetEventKind::Removed);
    }

    /// Resets, resumes or restarts a bot. Without an actor (after a second
    /// crash loop) the bot acts like a Failed one: only a reset does
    /// something, and it starts a new actor.
    fn lifecycle(&mut self, id: BotId, action: Lifecycle) -> Result<(), FleetError> {
        let now = self.clock.now();
        let entry = self
            .bots
            .get_mut(&id)
            .filter(|entry| !entry.removing)
            .ok_or(FleetError::UnknownBot)?;
        let state = entry.snapshot.borrow().state;
        let mut start = None;
        if let Some(live) = &entry.actor {
            let command = match action {
                Lifecycle::Reset => BotCommand::Reset,
                Lifecycle::Resume => BotCommand::Resume,
                Lifecycle::Restart => BotCommand::Restart,
            };
            live.inbox.try_send(command).map_err(|_| FleetError::Busy)?;
        } else if action == Lifecycle::Reset {
            start = Some(transition(
                &state,
                BotEvent::Reset,
                now,
                &rules(self.retry, entry),
            ));
        }
        // A human chose to try again after a crash loop: the new run gets
        // the whole restart window. The supervisor's own record counts too,
        // since the crash-loop actor may not have published its state yet.
        if action == Lifecycle::Reset && (entry.crash_looped || state == CRASH_LOOP) {
            entry.crashes.clear();
            entry.crash_looped = false;
        }
        if let Some(start) = start {
            self.start_actor(id, start);
        }
        Ok(())
    }

    /// Hands a user's chat message to the bot's actor, which answers on the
    /// receiver this returns.
    fn send_chat(&self, id: BotId, message: ChatMessage) -> Result<ChatAnswer, SendChatError> {
        let entry = self
            .bots
            .get(&id)
            .filter(|entry| !entry.removing)
            .ok_or(FleetError::UnknownBot)?;
        // Without an actor, the bot is Failed.
        let live = entry.actor.as_ref().ok_or(ChatError::NotOnline)?;
        let (reply, answer) = oneshot::channel();
        live.inbox
            .try_send(BotCommand::SendChat { message, reply })
            .map_err(|_| FleetError::Busy)?;
        Ok(answer)
    }

    fn snapshot(&self, id: BotId) -> Result<BotSnapshot, FleetError> {
        self.bots
            .get(&id)
            .map(|entry| entry.snapshot.borrow().clone())
            .ok_or(FleetError::UnknownBot)
    }

    fn snapshot_all(&self) -> Vec<BotSnapshot> {
        self.bots
            .values()
            .map(|entry| entry.snapshot.borrow().clone())
            .collect()
    }

    /// An actor's task finished outside a shutdown.
    fn joined(&mut self, done: Result<(task::Id, ActorExit), JoinError>) {
        let (task, outcome) = match done {
            Ok((task, exit)) => (task, Ok(exit)),
            Err(error) => (error.id(), Err(error)),
        };
        if let Some(&id) = self.tasks.get(&task) {
            self.exited(id, outcome);
        }
    }

    /// Bot `id`'s actor ended with `outcome`, outside a shutdown. A removed
    /// bot is gone; otherwise the actor crashed, and a new one starts from
    /// `restore()`, or with `CrashLoop` after too many crashes (ADR-0013).
    fn exited(&mut self, id: BotId, outcome: Result<ActorExit, JoinError>) {
        let now = self.clock.now();
        let Some(entry) = self.bots.get_mut(&id) else {
            return;
        };
        if let Some(live) = entry.actor.take() {
            self.tasks.remove(&live.task.id());
        }
        let crash = match outcome {
            Ok(ActorExit::Stopped) => None,
            Ok(ActorExit::TaskCrashed { task }) => Some(Crash::Task(task)),
            Err(error) if error.is_panic() => Some(Crash::Panic),
            Err(_) => Some(Crash::Aborted),
        };
        // The new actor makes its own span, so it starts outside this one.
        let span = entry.span.clone();
        if let Some(start) = span.in_scope(|| self.after_exit(id, crash, now)) {
            self.start_actor(id, start);
        }
    }

    /// Decides, in the bot's span, what follows its actor's end: nothing for
    /// a removed bot, otherwise where the next actor starts, if one does.
    fn after_exit(
        &mut self,
        id: BotId,
        crash: Option<Crash>,
        now: DateTime<Utc>,
    ) -> Option<Transition> {
        let entry = self.bots.get_mut(&id)?;
        if entry.removing {
            if let Some(crash) = crash {
                warn!(%crash, "the bot's actor crashed while it was removed");
            }
            self.removed(id);
            return None;
        }
        // Only the supervisor cancels or aborts an actor, so an actor that
        // ended without a crash is a bug: it's handled like a crash.
        let crash = crash.unwrap_or(Crash::Stopped);
        if matches!(crash, Crash::Stopped | Crash::Aborted) {
            error!(
                %crash,
                "the bot's actor stopped without being asked to; it restarts like after a crash"
            );
        }
        if entry.crash_looped {
            error!(
                %crash,
                "the bot's actor crashed again after a crash loop; it stays Failed until a Reset"
            );
            self.fail_with_crash_loop(id, now);
            return None;
        }
        let rules = rules(self.retry, entry);
        let restored = restore(&entry.snapshot.borrow().state, now, &rules);
        let looped = entry.crashes.record(now);
        let crashes = entry.crashes.len();
        let start = if looped {
            error!(
                %crash,
                crashes, "the bot's actor crashed too often; the bot fails with CrashLoop"
            );
            entry.crash_looped = true;
            transition(&restored.state, BotEvent::CrashLoop, now, &rules)
        } else {
            if matches!(crash, Crash::Panic | Crash::Task(_)) {
                warn!(
                    %crash,
                    crashes, "the bot's actor crashed; it restarts from its last state"
                );
            }
            restored
        };
        Some(start)
    }

    /// A bot's actor crashed again after a crash loop: no actor runs until a
    /// Reset, and the bot shows `Failed(CrashLoop)`, published only if it
    /// changed.
    fn fail_with_crash_loop(&mut self, id: BotId, now: DateTime<Utc>) {
        let Some(entry) = self.bots.get(&id) else {
            return;
        };
        let snapshot = {
            let last = entry.snapshot.borrow();
            if last.state == CRASH_LOOP {
                return;
            }
            BotSnapshot {
                bot_id: id,
                state: CRASH_LOOP,
                since: now,
                last_disconnect: last.last_disconnect.clone(),
            }
        };
        entry.snapshot.send_replace(snapshot.clone());
        self.publish(id, FleetEventKind::StateChanged(snapshot));
        self.publish(
            id,
            FleetEventKind::Alert(BotNotification::Failed {
                reason: FailReason::CrashLoop,
            }),
        );
    }

    /// Starts an actor for bot `id` at `start`, with the bot's spec, `watch`es
    /// and chat bucket.
    fn start_actor(&mut self, id: BotId, start: Transition) {
        let Some(entry) = self.bots.get_mut(&id) else {
            return;
        };
        let (actor, inbox) = BotActor::new(BotActorParts {
            spec: entry.spec.clone(),
            connector: Arc::clone(&self.connector),
            credentials: Arc::clone(&self.credentials),
            retry: self.retry,
            config: self.config,
            clock: self.clock,
            rng: StdRng::seed_from_u64(self.rng.next_u64()),
            events: self.events.clone(),
            bucket: entry.bucket.clone(),
            tickets: self.tickets.clone(),
            snapshot: entry.snapshot.clone(),
            breaker: entry.breaker.clone(),
            start,
        });
        let token = self.run_token.child_token();
        let task = self.actors.spawn(actor.run(token.clone()));
        self.tasks.insert(task.id(), id);
        entry.actor = Some(Live { inbox, token, task });
    }

    /// Publishes `kind` for bot `id`, stamped now.
    fn publish(&self, id: BotId, kind: FleetEventKind) {
        let event = FleetEvent {
            bot_id: id,
            at: self.clock.now(),
            kind,
        };
        // An error only means nobody subscribes right now.
        let _ = self.events.send(event);
    }

    /// Shuts the fleet down: cancels every actor, waits for them up to the
    /// timeout, aborts the rest, and answers further calls with
    /// `ShuttingDown` meanwhile.
    async fn shut_down(&mut self, ending: Ending) {
        let mut open = !matches!(ending, Ending::Dropped);
        let (timeout, reply) = match ending {
            Ending::Requested { timeout, reply } => (timeout, Some(reply)),
            Ending::Cancelled | Ending::Dropped => (self.config.shutdown_timeout, None),
        };
        self.run_token.cancel();
        let mut report = ShutdownReport::default();
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let mut aborted = false;
        while !self.actors.is_empty() {
            tokio::select! {
                biased;
                // Every branch is cancel-safe: `join_next_with_id` and `recv`
                // lose nothing, and the pinned deadline keeps its time.
                Some(done) = self.actors.join_next_with_id() => self.ended(done, &mut report),
                () = &mut deadline, if !aborted => {
                    aborted = true;
                    self.actors.abort_all();
                }
                command = self.commands.recv(), if open => match command {
                    Some(command) => command.refuse(FleetError::ShuttingDown),
                    None => open = false,
                },
            }
        }
        if report.aborted > 0 {
            warn!(
                aborted = report.aborted,
                "the shutdown timed out; the remaining actors were aborted"
            );
        }
        info!(?report, "the fleet has shut down");
        if let Some(reply) = reply {
            // An error only means the caller stopped waiting.
            let _ = reply.send(Ok(report));
        }
    }

    /// An actor's task finished during a shutdown: counts how it ended.
    fn ended(
        &mut self,
        done: Result<(task::Id, ActorExit), JoinError>,
        report: &mut ShutdownReport,
    ) {
        let (task, outcome) = match done {
            Ok((task, exit)) => (task, Ok(exit)),
            Err(error) => (error.id(), Err(error)),
        };
        let Some(id) = self.tasks.remove(&task) else {
            return;
        };
        let Some(entry) = self.bots.get_mut(&id) else {
            return;
        };
        entry.actor = None;
        let span = entry.span.clone();
        let _entered = span.enter();
        match outcome {
            Ok(ActorExit::Stopped) => report.stopped = report.stopped.saturating_add(1),
            Err(error) if error.is_cancelled() => {
                report.aborted = report.aborted.saturating_add(1);
            }
            Ok(ActorExit::TaskCrashed { .. }) | Err(_) => {
                report.crashed = report.crashed.saturating_add(1);
                warn!("the bot's actor crashed while the fleet shut down");
            }
        }
        if entry.removing {
            self.removed(id);
        }
    }
}

/// Takes on `spec` for a known bot: refused while it's being removed or if
/// its account changes, forwarded to its actor if it changed.
fn update(
    entry: &mut Entry,
    spec: BotSpec,
    restore: Option<StickyState>,
) -> Result<(), FleetError> {
    if entry.removing {
        return Err(FleetError::Busy);
    }
    if spec.account != entry.spec.account {
        return Err(FleetError::AccountChanged);
    }
    if restore.is_some() {
        let _entered = entry.span.enter();
        debug!("a restore for a bot the fleet already runs was ignored");
    }
    if spec == entry.spec {
        return Ok(());
    }
    if let Some(live) = &entry.actor {
        live.inbox
            .try_send(BotCommand::UpdateSpec(Box::new(spec.clone())))
            .map_err(|_| FleetError::Busy)?;
    }
    entry.spec = spec;
    Ok(())
}

/// The rules `transition()` and `restore()` apply to `entry`'s bot.
fn rules(retry: RetryPolicy, entry: &Entry) -> BotRules {
    BotRules {
        retry,
        conflict_texts: entry.spec.conflict_texts.clone(),
    }
}

/// `Failed(CrashLoop)`.
const CRASH_LOOP: BotState = BotState::Failed {
    reason: FailReason::CrashLoop,
};

/// Why the supervisor's loop ended.
enum Ending {
    /// `run`'s token was cancelled.
    Cancelled,
    /// Every `Fleet` handle was dropped.
    Dropped,
    /// A `shutdown` call.
    Requested {
        timeout: Duration,
        reply: Reply<ShutdownReport>,
    },
}

/// How an actor ended without being asked to.
#[derive(Clone, Copy)]
enum Crash {
    /// It panicked.
    Panic,
    /// One of its tasks panicked.
    Task(CrashedTask),
    /// It stopped although nothing cancelled it: a bug.
    Stopped,
    /// It was aborted although nothing aborted it: a bug.
    Aborted,
}

impl fmt::Display for Crash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Panic => f.write_str("panic"),
            Self::Task(task) => write!(f, "crashed task {task:?}"),
            Self::Stopped => f.write_str("unexpected stop"),
            Self::Aborted => f.write_str("unexpected abort"),
        }
    }
}

#[cfg(test)]
mod tests {
    use core::fmt::{self, Write as _};
    use core::time::Duration;
    use std::sync::Mutex;

    use chrono::DateTime;
    use fleet_core::bot::{BotAccount, BotNotification, BotState, DesiredRunState, FailReason};
    use fleet_core::disconnect::ConflictTexts;
    use fleet_core::mode::ModeDraft;
    use fleet_testkit::mc::{FakeConnector, FakeCredentials};
    use tokio::sync::oneshot;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Level, Metadata};

    use super::*;
    use crate::actor::{BotCommand, CrashedTask};
    use crate::chat::ChatError;
    use crate::event::FleetEventKind;
    use crate::fleet::command::Lifecycle;
    use crate::fleet::{Fleet, SendChatError};

    const BOT: &str = "018bcfe5-6800-7bab-abab-abababababab";

    const CRASH_LOOP: BotState = BotState::Failed {
        reason: FailReason::CrashLoop,
    };

    type TestSupervisor = Supervisor<FakeConnector, FakeCredentials>;

    fn bot() -> BotId {
        BOT.parse().unwrap()
    }

    fn spec(desired: DesiredRunState) -> BotSpec {
        BotSpec {
            id: bot(),
            account: BotAccount::Offline("AfkBot1".parse().unwrap()),
            server: "localhost".try_into().unwrap(),
            mode: ModeDraft { steps: Vec::new() }.validate().unwrap(),
            desired,
            conflict_texts: ConflictTexts::default(),
        }
    }

    /// A supervisor that isn't running, so a test calls its handlers
    /// directly. The actors it starts run on the test's runtime.
    fn supervisor() -> (
        TestSupervisor,
        FakeConnector,
        broadcast::Receiver<FleetEvent>,
    ) {
        let connector = FakeConnector::new();
        let (fleet, supervisor) = Fleet::new(FleetParts {
            connector: Arc::new(connector.clone()),
            credentials: Arc::new(FakeCredentials::new()),
            retry: RetryPolicy::try_new(
                Duration::from_secs(5),
                Duration::from_mins(5),
                Duration::from_mins(5),
            )
            .unwrap(),
            circuit: CircuitPolicy::try_new(
                core::num::NonZeroUsize::new(8).unwrap(),
                Duration::from_mins(10),
                Duration::from_mins(15),
            )
            .unwrap(),
            config: RuntimeConfig::default(),
            anchor: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            seed: 1,
        })
        .unwrap();
        (supervisor, connector, fleet.subscribe())
    }

    /// Lets every spawned task run until it waits.
    async fn settle() {
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
    }

    fn kinds(events: &mut broadcast::Receiver<FleetEvent>) -> Vec<FleetEventKind> {
        let mut kinds = Vec::new();
        while let Ok(event) = events.try_recv() {
            kinds.push(event.kind);
        }
        kinds
    }

    fn apply(supervisor: &mut TestSupervisor, spec: BotSpec) -> Result<(), FleetError> {
        let (reply, mut answer) = oneshot::channel();
        supervisor.command(Command::Apply {
            spec: Box::new(spec),
            restore: None,
            reply,
        });
        answer.try_recv().unwrap()
    }

    fn lifecycle(supervisor: &mut TestSupervisor, action: Lifecycle) -> Result<(), FleetError> {
        let (reply, mut answer) = oneshot::channel();
        supervisor.command(Command::Lifecycle {
            id: bot(),
            action,
            reply,
        });
        answer.try_recv().unwrap()
    }

    fn entry(supervisor: &TestSupervisor) -> &Entry {
        supervisor.bots.get(&bot()).unwrap()
    }

    /// Ends the bot's actor without the supervisor knowing: aborting it
    /// leaves its last published state as it was.
    async fn end_actor_behind_its_back(supervisor: &mut TestSupervisor) {
        entry(supervisor).actor.as_ref().unwrap().task.abort();
        let done = supervisor.actors.join_next_with_id().await.unwrap();
        assert!(done.is_err_and(|error| error.is_cancelled()));
    }

    /// Runs a bot, then lets its actor crash right after a crash loop, which
    /// leaves the bot without an actor.
    async fn crash_looped(
        supervisor: &mut TestSupervisor,
        events: &mut broadcast::Receiver<FleetEvent>,
    ) {
        apply(supervisor, spec(DesiredRunState::Running)).unwrap();
        settle().await;
        let entry_mut = supervisor.bots.get_mut(&bot()).unwrap();
        entry_mut.crash_looped = true;
        let _ = entry_mut
            .crashes
            .record(DateTime::from_timestamp(1_700_000_000, 0).unwrap());
        end_actor_behind_its_back(supervisor).await;
        supervisor.exited(
            bot(),
            Ok(ActorExit::TaskCrashed {
                task: CrashedTask::ModeRunner,
            }),
        );
        assert!(entry(supervisor).actor.is_none());
        let _ = kinds(events);
    }

    /// Records each event's level and fields, for the log assertions here;
    /// the component tests have a fuller one.
    #[derive(Debug, Clone, Default)]
    struct Logs(Arc<Mutex<Vec<(Level, String)>>>);

    impl Logs {
        fn of(&self, needle: &str) -> Vec<Level> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, fields)| fields.contains(needle))
                .map(|(level, _)| *level)
                .collect()
        }
    }

    struct Fields(String);

    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            let _ = write!(self.0, "{}={value:?} ", field.name());
        }
    }

    impl tracing::Subscriber for Logs {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _: &Id, _: &Record<'_>) {}

        fn record_follows_from(&self, _: &Id, _: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut fields = Fields(String::new());
            event.record(&mut fields);
            self.0
                .lock()
                .unwrap()
                .push((*event.metadata().level(), fields.0));
        }

        fn enter(&self, _: &Id) {}

        fn exit(&self, _: &Id) {}
    }

    #[tokio::test(start_paused = true)]
    async fn an_unexpected_stop_is_handled_like_a_crash() {
        let logs = Logs::default();
        let _guard = tracing::subscriber::set_default(logs.clone());
        let (mut supervisor, connector, mut events) = supervisor();
        apply(&mut supervisor, spec(DesiredRunState::Running)).unwrap();
        settle().await;
        assert_eq!(connector.connects().len(), 1);
        end_actor_behind_its_back(&mut supervisor).await;
        let _ = kinds(&mut events);

        supervisor.exited(bot(), Ok(ActorExit::Stopped));
        settle().await;

        assert_eq!(logs.of("stopped without being asked"), [Level::ERROR]);
        let entry = entry(&supervisor);
        assert_eq!(entry.crashes.len(), 1);
        assert!(entry.actor.is_some(), "restarted");
        let backoff = BotState::Backoff {
            attempt: core::num::NonZeroU32::MIN,
        };
        assert_eq!(entry.snapshot.borrow().state, backoff, "from restore()");
        let kinds = kinds(&mut events);
        assert!(
            matches!(
                &kinds[..],
                [FleetEventKind::StateChanged(snapshot)] if snapshot.state == backoff
            ),
            "{kinds:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_crash_after_a_crash_loop_leaves_the_bot_failed_without_an_actor() {
        let logs = Logs::default();
        let _guard = tracing::subscriber::set_default(logs.clone());
        let (mut supervisor, _connector, mut events) = supervisor();
        apply(&mut supervisor, spec(DesiredRunState::Running)).unwrap();
        settle().await;
        supervisor.bots.get_mut(&bot()).unwrap().crash_looped = true;
        end_actor_behind_its_back(&mut supervisor).await;
        let _ = kinds(&mut events);

        supervisor.exited(
            bot(),
            Ok(ActorExit::TaskCrashed {
                task: CrashedTask::ModeRunner,
            }),
        );

        let entry = entry(&supervisor);
        assert!(entry.actor.is_none());
        assert_eq!(entry.snapshot.borrow().state, CRASH_LOOP);
        let kinds = kinds(&mut events);
        assert!(
            matches!(
                &kinds[..],
                [
                    FleetEventKind::StateChanged(snapshot),
                    FleetEventKind::Alert(BotNotification::Failed {
                        reason: FailReason::CrashLoop
                    }),
                ] if snapshot.state == CRASH_LOOP
            ),
            "{kinds:?}"
        );
        assert_eq!(logs.of("stays Failed until a Reset"), [Level::ERROR]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_crash_after_a_crash_loop_publishes_nothing_when_the_bot_already_failed() {
        let (mut supervisor, _connector, mut events) = supervisor();
        apply(&mut supervisor, spec(DesiredRunState::Running)).unwrap();
        settle().await;
        let entry_mut = supervisor.bots.get_mut(&bot()).unwrap();
        entry_mut.crash_looped = true;
        entry_mut
            .snapshot
            .send_modify(|snapshot| snapshot.state = CRASH_LOOP);
        end_actor_behind_its_back(&mut supervisor).await;
        let _ = kinds(&mut events);

        supervisor.exited(
            bot(),
            Ok(ActorExit::TaskCrashed {
                task: CrashedTask::ChatDelivery,
            }),
        );

        assert!(entry(&supervisor).actor.is_none());
        assert_eq!(kinds(&mut events), []);
    }

    #[tokio::test(start_paused = true)]
    async fn a_bot_without_an_actor_keeps_a_new_spec_for_the_next_reset() {
        let (mut supervisor, connector, mut events) = supervisor();
        crash_looped(&mut supervisor, &mut events).await;
        let mut moved = spec(DesiredRunState::Running);
        moved.server = "example.com".try_into().unwrap();

        assert_eq!(apply(&mut supervisor, moved.clone()), Ok(()));
        assert_eq!(entry(&supervisor).spec, moved);
        assert!(entry(&supervisor).actor.is_none());
        assert_eq!(lifecycle(&mut supervisor, Lifecycle::Reset), Ok(()));
        settle().await;

        let connects = connector.connects();
        assert_eq!(connects.len(), 2);
        assert_eq!(connects.last().unwrap().server, moved.server);
    }

    #[tokio::test(start_paused = true)]
    async fn resume_and_restart_do_nothing_for_a_bot_without_an_actor() {
        let (mut supervisor, connector, mut events) = supervisor();
        crash_looped(&mut supervisor, &mut events).await;

        assert_eq!(lifecycle(&mut supervisor, Lifecycle::Resume), Ok(()));
        assert_eq!(lifecycle(&mut supervisor, Lifecycle::Restart), Ok(()));
        settle().await;

        assert!(entry(&supervisor).actor.is_none());
        assert_eq!(kinds(&mut events), []);
        assert_eq!(connector.connects().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn chat_to_a_bot_without_an_actor_is_not_online() {
        let (mut supervisor, _connector, mut events) = supervisor();
        crash_looped(&mut supervisor, &mut events).await;
        let (reply, mut answer) = oneshot::channel();

        supervisor.command(Command::SendChat {
            id: bot(),
            message: "hi".parse().unwrap(),
            reply,
        });

        assert!(matches!(
            answer.try_recv(),
            Ok(Err(SendChatError::Chat(ChatError::NotOnline)))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn removing_a_bot_without_an_actor_publishes_removed_at_once() {
        let (mut supervisor, _connector, mut events) = supervisor();
        crash_looped(&mut supervisor, &mut events).await;
        let (reply, mut answer) = oneshot::channel();

        supervisor.command(Command::Remove { id: bot(), reply });

        assert_eq!(answer.try_recv().unwrap(), Ok(()));
        assert_eq!(kinds(&mut events), [FleetEventKind::Removed]);
        assert!(supervisor.bots.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_reset_starts_an_actor_and_clears_the_restart_window() {
        let (mut supervisor, _connector, mut events) = supervisor();
        crash_looped(&mut supervisor, &mut events).await;
        assert!(!entry(&supervisor).crashes.is_empty());

        assert_eq!(lifecycle(&mut supervisor, Lifecycle::Reset), Ok(()));
        settle().await;

        let entry = entry(&supervisor);
        assert!(entry.actor.is_some());
        assert!(entry.crashes.is_empty());
        assert!(!entry.crash_looped);
        assert!(!matches!(
            entry.snapshot.borrow().state,
            BotState::Failed { .. }
        ));
    }

    // Found in the PR #17 review: a Reset that came before the crash-loop
    // actor had published Failed(CrashLoop) left the flag and the window as
    // they were, so a single crash much later failed the bot at once.
    #[tokio::test(start_paused = true)]
    async fn a_reset_right_after_a_crash_loop_starts_still_clears_the_window() {
        let (mut supervisor, _connector, mut events) = supervisor();
        apply(&mut supervisor, spec(DesiredRunState::Running)).unwrap();
        settle().await;
        let now = supervisor.clock.now();
        let entry_mut = supervisor.bots.get_mut(&bot()).unwrap();
        for _ in 0..5 {
            let _ = entry_mut.crashes.record(now);
        }
        end_actor_behind_its_back(&mut supervisor).await;
        supervisor.exited(
            bot(),
            Ok(ActorExit::TaskCrashed {
                task: CrashedTask::ModeRunner,
            }),
        );
        assert!(entry(&supervisor).crash_looped, "the 6th crash");

        // Before the crash-loop actor has run, so its state isn't published.
        assert_eq!(lifecycle(&mut supervisor, Lifecycle::Reset), Ok(()));

        assert!(!entry(&supervisor).crash_looped);
        assert!(entry(&supervisor).crashes.is_empty());
        settle().await;
        tokio::time::advance(Duration::from_hours(24)).await;
        settle().await;
        end_actor_behind_its_back(&mut supervisor).await;
        let _ = kinds(&mut events);
        supervisor.exited(
            bot(),
            Ok(ActorExit::TaskCrashed {
                task: CrashedTask::ModeRunner,
            }),
        );
        let entry = entry(&supervisor);
        assert!(entry.actor.is_some(), "a single crash restarts the bot");
        assert_ne!(entry.snapshot.borrow().state, CRASH_LOOP);
    }

    /// Swaps the actor's inbox for one that's full.
    fn fill_inbox(supervisor: &mut TestSupervisor) -> mpsc::Receiver<BotCommand> {
        let (sender, receiver) = mpsc::channel(1);
        let inbox = BotInbox::new(sender);
        inbox.try_send(BotCommand::Restart).unwrap();
        supervisor
            .bots
            .get_mut(&bot())
            .unwrap()
            .actor
            .as_mut()
            .unwrap()
            .inbox = inbox;
        receiver
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_inbox_refuses_a_changed_spec_without_keeping_it() {
        let (mut supervisor, _connector, _events) = supervisor();
        apply(&mut supervisor, spec(DesiredRunState::Stopped)).unwrap();
        let _receiver = fill_inbox(&mut supervisor);

        let answer = apply(&mut supervisor, spec(DesiredRunState::Running));

        assert_eq!(answer, Err(FleetError::Busy));
        assert_eq!(entry(&supervisor).spec, spec(DesiredRunState::Stopped));
    }

    #[tokio::test(start_paused = true)]
    async fn an_equal_spec_is_answered_without_reaching_the_actor() {
        let (mut supervisor, _connector, _events) = supervisor();
        apply(&mut supervisor, spec(DesiredRunState::Stopped)).unwrap();
        let _receiver = fill_inbox(&mut supervisor);

        let answer = apply(&mut supervisor, spec(DesiredRunState::Stopped));

        assert_eq!(answer, Ok(()), "a forwarded spec would find the inbox full");
    }

    #[rstest::rstest]
    #[case::panic(Crash::Panic, "panic")]
    #[case::task(Crash::Task(CrashedTask::Teardown), "crashed task Teardown")]
    #[case::stopped(Crash::Stopped, "unexpected stop")]
    #[case::aborted(Crash::Aborted, "unexpected abort")]
    fn a_crash_reads_well_in_the_logs(#[case] crash: Crash, #[case] text: &str) {
        assert_eq!(crash.to_string(), text);
    }

    #[tokio::test(start_paused = true)]
    async fn debug_output_counts_the_bots_only() {
        let (supervisor, _connector, _events) = supervisor();

        assert_eq!(format!("{supervisor:?}"), "Supervisor { bots: 0, .. }");
    }
}
