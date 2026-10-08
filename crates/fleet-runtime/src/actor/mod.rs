//! [`BotActor`]: one bot's actor (Plan.md P4.2; ADR-0010, ADR-0013).
//!
//! The actor is the only place a bot's lifecycle is driven from. It feeds
//! its commands, its session's events and its timers to
//! `fleet_core::bot::transition` and executes the effects that come back, in
//! order:
//! - It asks for session credentials (bounded by the session-request
//!   timeout), connects, opens the chat queue and starts the mode runner,
//!   respawns the bot after a death, and tears sessions down.
//! - It tells the circuit breaker how each session went, and backs off for
//!   `RetryPolicy::delay` or until the breaker's cool-down ends, whichever
//!   is later.
//! - It publishes a `watch` snapshot and [`FleetEvent`]s: every state
//!   change, deaths, incoming chat, and alerts for a human.
//!
//! A [`BotCommand`] comes in per `Fleet` call. Start and stop come only from
//! the spec's desired state. Before any input that ends a session, the actor
//! applies the session events that are already ready, so a duplicate-login
//! kick that's already queued always pauses the bot first.

mod command;
mod effects;
mod respawn;
mod session;
mod watchdog;

use core::fmt;
use core::future::Future;
use core::ops::ControlFlow;
use core::pin::Pin;
use core::time::Duration;
use std::collections::VecDeque;
use std::sync::Arc;

use fleet_core::bot::{BotEvent, BotRules, BotSnapshot, BotSpec, BotState, DesiredRunState};
use fleet_core::disconnect::DisconnectReason;
use fleet_core::mc::{
    CredentialError, MinecraftConnector, SessionCredentialProvider, SessionCredentials,
    SessionEvent, SessionHandle as _,
};
use fleet_core::mode::ModeDefinition;
use fleet_core::resilience::{CircuitBreaker, RetryPolicy};
use rand::rngs::StdRng;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::{JoinError, JoinSet};
use tokio::time::Sleep;
use tokio::time::error::Elapsed;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument as _, Span, error, info_span, warn};

pub use command::{BotCommand, BotInbox, InboxError};

use self::respawn::RespawnOutcome;
use self::session::{Session, next_event};
use self::watchdog::Stall;
use crate::chat::{ChatBucket, ChatQueue, ChatTickets};
use crate::clock::RuntimeClock;
use crate::config::RuntimeConfig;
use crate::event::FleetEvent;

/// Everything a [`BotActor`] is built from (ADR-0013).
///
/// The caller owns the snapshot and breaker `watch`es, the chat bucket and
/// the ticket counter, so a supervisor can keep them across actor restarts
/// (P4.7).
#[derive(Debug)]
pub struct BotActorParts<C, P> {
    /// What the bot should be doing. Its desired state starts the bot.
    pub spec: BotSpec,
    /// Starts the bot's sessions.
    pub connector: Arc<C>,
    /// Hands out the bot's session credentials.
    pub credentials: Arc<P>,
    /// How long to back off, and when a session counts as stable.
    pub retry: RetryPolicy,
    /// The runtime's settings.
    pub config: RuntimeConfig,
    /// The runtime's clock.
    pub clock: RuntimeClock,
    /// The bot's randomness: backoff jitter, and the seeds of its mode
    /// runners.
    pub rng: StdRng,
    /// The fleet's events.
    pub events: broadcast::Sender<FleetEvent>,
    /// The bot's chat rate limit.
    pub bucket: ChatBucket,
    /// The fleet's chat ticket counter.
    pub tickets: ChatTickets,
    /// Where the actor publishes the bot's snapshot.
    pub snapshot: watch::Sender<BotSnapshot>,
    /// Holds the bot's circuit breaker: its current value is where the actor
    /// starts, and the actor writes a copy after each breaker effect.
    pub breaker: watch::Sender<CircuitBreaker>,
}

/// Why [`BotActor::run`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorExit {
    /// It was cancelled, or every inbox sender was dropped. The bot went
    /// through `Stop`, and every teardown has finished.
    Stopped,
    /// One of its tasks panicked. The actor tore the session down without a
    /// state change, so its last published state stays; a supervisor counts
    /// this like a panic of the actor itself (P4.7).
    TaskCrashed {
        /// The task that panicked.
        task: CrashedTask,
    },
}

/// A task of a [`BotActor`] that panicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashedTask {
    /// The mode runner.
    ModeRunner,
    /// The chat queue's delivery.
    ChatDelivery,
    /// A session's teardown.
    Teardown,
}

/// A future the actor waits for in its `select!`, if there is one.
type Slot<T> = Option<Pin<Box<dyn Future<Output = T> + Send>>>;

/// The answer to a session request, or its timeout.
type SessionAnswer = Result<Result<SessionCredentials, CredentialError>, Elapsed>;

/// One input to the actor's loop.
enum Input {
    /// Cancelled, or every inbox sender is gone.
    End,
    /// A task panicked.
    Crashed(CrashedTask),
    /// The teardown of the session with this generation has finished.
    TornDown(u64),
    /// A task ended normally.
    Nothing,
    Command(BotCommand),
    Answer(SessionAnswer),
    RetryDue,
    Respawn(RespawnOutcome),
    /// Time for the watchdog to check the session.
    WatchdogCheck,
    Session(Option<SessionEvent>),
}

/// One bot's actor (Plan.md P4.2; ADR-0010, ADR-0013): see the module docs.
///
/// [`new`](Self::new) builds it with its inbox, and [`run`](Self::run) drives
/// it until it's cancelled. Every task it spawns runs in its `JoinSet`s,
/// instrumented with the bot's span.
pub struct BotActor<C: MinecraftConnector, P> {
    spec: BotSpec,
    rules: BotRules,
    connector: Arc<C>,
    credentials: Arc<P>,
    config: RuntimeConfig,
    clock: RuntimeClock,
    rng: StdRng,
    events: broadcast::Sender<FleetEvent>,
    snapshot: watch::Sender<BotSnapshot>,
    breaker_copy: watch::Sender<CircuitBreaker>,
    breaker: CircuitBreaker,
    mode: watch::Sender<ModeDefinition>,
    queue: ChatQueue,
    inbox: mpsc::Receiver<BotCommand>,
    span: Span,
    /// The parent of every session's token; a child of `run`'s token.
    run_token: CancellationToken,
    state: BotState,
    last_disconnect: Option<DisconnectReason>,
    /// Events that effects produced, applied after the current one.
    pending: VecDeque<BotEvent>,
    held_credentials: Option<SessionCredentials>,
    session_request: Slot<SessionAnswer>,
    retry: Option<Pin<Box<Sleep>>>,
    respawn: Slot<RespawnOutcome>,
    /// The watchdog's next check, while the bot is Online.
    watchdog: Option<Pin<Box<Sleep>>>,
    session: Option<Session<C::Session>>,
    session_events: Option<C::Events>,
    /// How many sessions have connected.
    generation: u64,
    runners: JoinSet<()>,
    deliveries: JoinSet<()>,
    teardowns: JoinSet<u64>,
}

impl<C: MinecraftConnector, P> fmt::Debug for BotActor<C, P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BotActor")
            .field("bot_id", &self.spec.id)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl<C: MinecraftConnector, P: SessionCredentialProvider> BotActor<C, P> {
    /// Builds the actor and its bounded inbox. The bot starts Stopped; the
    /// snapshot `watch` gets that state without an event, and
    /// [`run`](Self::run) applies the spec's desired state.
    #[must_use]
    pub fn new(parts: BotActorParts<C, P>) -> (Self, BotInbox) {
        let BotActorParts {
            spec,
            connector,
            credentials,
            retry,
            config,
            clock,
            rng,
            events,
            bucket,
            tickets,
            snapshot,
            breaker,
        } = parts;
        let (sender, inbox) = mpsc::channel(config.actor_inbox.get());
        snapshot.send_replace(BotSnapshot {
            bot_id: spec.id,
            state: BotState::Stopped,
            since: clock.now(),
            last_disconnect: None,
        });
        let start = breaker.borrow().clone();
        let actor = Self {
            rules: BotRules {
                retry,
                conflict_texts: spec.conflict_texts.clone(),
            },
            connector,
            credentials,
            config,
            clock,
            rng,
            events,
            snapshot,
            breaker_copy: breaker,
            breaker: start,
            mode: watch::Sender::new(spec.mode.clone()),
            queue: ChatQueue::new(spec.id, config.chat_queue, bucket, tickets),
            inbox,
            span: info_span!("bot", bot_id = %spec.id),
            run_token: CancellationToken::new(),
            state: BotState::Stopped,
            last_disconnect: None,
            pending: VecDeque::new(),
            held_credentials: None,
            session_request: None,
            retry: None,
            respawn: None,
            watchdog: None,
            session: None,
            session_events: None,
            generation: 0,
            runners: JoinSet::new(),
            deliveries: JoinSet::new(),
            teardowns: JoinSet::new(),
            spec,
        };
        (actor, BotInbox::new(sender))
    }

    /// Runs the bot until `cancel` is cancelled or every inbox sender is
    /// dropped, then stops it and waits for its teardowns. It returns early
    /// if one of its tasks panics.
    pub async fn run(mut self, cancel: CancellationToken) -> ActorExit {
        let span = self.span.clone();
        async move {
            self.run_token = cancel.child_token();
            if self.spec.desired == DesiredRunState::Running {
                self.feed(BotEvent::Start).await;
            }
            loop {
                let input = self.next_input(&cancel).await;
                if let ControlFlow::Break(exit) = self.handle(input).await {
                    return exit;
                }
            }
        }
        .instrument(span)
        .await
    }

    /// Waits for the next input. The order is fixed (`biased`), so tests on
    /// paused time are reproducible. The session's events come last, since
    /// the server controls how fast they arrive: a flood of chat can't hold
    /// up the inbox, the timers or the tasks (ADR-0013).
    async fn next_input(&mut self, cancel: &CancellationToken) -> Input {
        tokio::select! {
            biased;
            // Every branch is cancel-safe. `cancelled`, `join_next` and
            // `recv` lose nothing when another branch wins. The slots keep
            // their futures between turns, so a pending request, timer or
            // respawn carries on. `next` is cancel-safe by the port contract.
            () = cancel.cancelled() => Input::End,
            Some(done) = self.runners.join_next(), if !self.runners.is_empty() => {
                finished(done, CrashedTask::ModeRunner, |()| Input::Nothing)
            }
            Some(done) = self.deliveries.join_next(), if !self.deliveries.is_empty() => {
                finished(done, CrashedTask::ChatDelivery, |()| Input::Nothing)
            }
            Some(done) = self.teardowns.join_next(), if !self.teardowns.is_empty() => {
                finished(done, CrashedTask::Teardown, Input::TornDown)
            }
            command = self.inbox.recv() => command.map_or(Input::End, Input::Command),
            answer = wait(&mut self.session_request), if self.session_request.is_some() => {
                Input::Answer(answer)
            }
            () = wait(&mut self.retry), if self.retry.is_some() => Input::RetryDue,
            outcome = wait(&mut self.respawn), if self.respawn.is_some() => {
                Input::Respawn(outcome)
            }
            () = wait(&mut self.watchdog), if self.watchdog.is_some() => Input::WatchdogCheck,
            event = next_event(&mut self.session_events), if self.session_events.is_some() => {
                Input::Session(event)
            }
        }
    }

    /// Handles one input; breaks when the actor ends.
    async fn handle(&mut self, input: Input) -> ControlFlow<ActorExit> {
        match input {
            Input::End => return ControlFlow::Break(self.finish().await),
            Input::Crashed(task) => return ControlFlow::Break(self.crash(task).await),
            Input::Nothing => {}
            Input::TornDown(generation) => self.torn_down(generation).await,
            Input::Command(command) => self.command(command).await,
            Input::Answer(answer) => {
                self.session_request = None;
                self.answer(answer).await;
            }
            Input::RetryDue => {
                self.retry = None;
                self.feed(BotEvent::RetryDue).await;
            }
            Input::Respawn(outcome) => {
                self.respawn = None;
                if outcome == RespawnOutcome::GaveUp {
                    self.respawn_failed().await;
                }
            }
            Input::WatchdogCheck => {
                self.watchdog = None;
                self.check_liveness().await;
            }
            Input::Session(event) => self.session_event(event).await,
        }
        ControlFlow::Continue(())
    }

    /// A teardown finished: `SessionClosed`, unless a newer session has
    /// connected since (ADR-0010).
    async fn torn_down(&mut self, generation: u64) {
        if generation == self.generation {
            self.feed(BotEvent::SessionClosed).await;
        }
    }

    async fn command(&mut self, command: BotCommand) {
        match command {
            BotCommand::UpdateSpec(spec) => self.update_spec(*spec).await,
            BotCommand::Restart => {
                if self.spec.desired == DesiredRunState::Running {
                    self.restart().await;
                }
            }
            BotCommand::Reset => self.feed(BotEvent::Reset).await,
            BotCommand::Resume => self.feed(BotEvent::Resume).await,
            BotCommand::SendChat { message, reply } => {
                // An error only means the caller stopped waiting.
                let _ = reply.send(self.queue.send(message));
            }
        }
    }

    /// Stop, then Start: a new run. While Connecting or Online that goes
    /// through `Stopping{restart}`; with no session yet it starts over at
    /// once. Paused and Failed ignore both (ADR-0013).
    async fn restart(&mut self) {
        self.drain().await;
        self.feed(BotEvent::Stop).await;
        self.feed(BotEvent::Start).await;
    }

    /// Takes on a new spec: the mode and the conflict texts at once, a new
    /// server through a restart, and the desired state as Start or Stop.
    async fn update_spec(&mut self, spec: BotSpec) {
        if spec.id != self.spec.id || spec.account != self.spec.account {
            error!(
                other_bot_id = %spec.id,
                same_account = spec.account == self.spec.account,
                "a spec for another bot or account was ignored"
            );
            return;
        }
        let server_changed = spec.server != self.spec.server;
        self.mode.send_if_modified(|mode| {
            if *mode == spec.mode {
                false
            } else {
                mode.clone_from(&spec.mode);
                true
            }
        });
        self.rules.conflict_texts.clone_from(&spec.conflict_texts);
        self.spec = spec;
        match self.spec.desired {
            DesiredRunState::Stopped => {
                self.drain().await;
                self.feed(BotEvent::Stop).await;
            }
            DesiredRunState::Running if server_changed && has_run(self.state) => {
                self.restart().await;
            }
            DesiredRunState::Running => self.feed(BotEvent::Start).await,
        }
    }

    /// Applies the answer to a session request.
    async fn answer(&mut self, answer: SessionAnswer) {
        match answer {
            Ok(Ok(credentials)) => {
                self.held_credentials = Some(credentials);
                self.feed(BotEvent::SessionReady).await;
            }
            Ok(Err(CredentialError { retryable })) => {
                if retryable {
                    warn!("no session is available right now; backing off");
                }
                self.feed(BotEvent::SessionUnavailable { retryable }).await;
            }
            Err(_) => {
                warn!(
                    timeout = ?self.config.session_request_timeout,
                    "the session request timed out; backing off"
                );
                self.feed(BotEvent::SessionUnavailable { retryable: true })
                    .await;
            }
        }
    }

    /// Every respawn failed: the session ends with `RespawnFailed`, unless a
    /// ready event ended it first.
    async fn respawn_failed(&mut self) {
        let generation = self.generation;
        self.drain().await;
        if self.is_current_online(generation) {
            self.ended(
                DisconnectReason::RespawnFailed,
                BotEvent::Disconnected(DisconnectReason::RespawnFailed),
            )
            .await;
        }
    }

    /// The watchdog's check (P4.6): a tick stall ends the session with
    /// `WatchdogTimeout`, a packet stall with `LivenessTimeout`, after the
    /// ready session events; otherwise the next check is armed.
    async fn check_liveness(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        let generation = session.generation;
        let found = watchdog::stall(
            session.handle.liveness(),
            tokio::time::Instant::now().into_std(),
            self.config.watchdog_timeout,
            self.config.packet_liveness_timeout,
        );
        let Some(found) = found else {
            self.arm_watchdog();
            return;
        };
        self.drain().await;
        if !self.is_current_online(generation) {
            return;
        }
        match found {
            Stall::Tick => {
                self.ended(DisconnectReason::WatchdogTimeout, BotEvent::WatchdogTimeout)
                    .await;
            }
            Stall::Packet => {
                self.ended(
                    DisconnectReason::LivenessTimeout,
                    BotEvent::Disconnected(DisconnectReason::LivenessTimeout),
                )
                .await;
            }
        }
    }

    /// Arms the watchdog's next check, one period from now. A zero period
    /// counts as 1 ms, so the loop can't spin.
    fn arm_watchdog(&mut self) {
        let period = self.config.watchdog_period.max(Duration::from_millis(1));
        self.watchdog = Some(Box::pin(tokio::time::sleep(period)));
    }

    /// Whether the session with `generation` is still the bot's, online.
    fn is_current_online(&self, generation: u64) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.generation == generation)
            && matches!(self.state, BotState::Online { .. })
    }

    /// Ends the actor: applies the ready session events, stops the bot
    /// through `transition()` and waits for every teardown.
    async fn finish(&mut self) -> ActorExit {
        self.drain().await;
        self.feed(BotEvent::Stop).await;
        let mut crashed = None;
        while let Some(done) = self.teardowns.join_next().await {
            match finished(done, CrashedTask::Teardown, Input::TornDown) {
                Input::TornDown(generation) => self.torn_down(generation).await,
                Input::Crashed(task) => crashed = Some(task),
                _ => {}
            }
        }
        match self.join_tasks().await.or(crashed) {
            Some(task) => {
                error!(?task, "a task of the bot crashed while it stopped");
                ActorExit::TaskCrashed { task }
            }
            None => ActorExit::Stopped,
        }
    }

    /// Ends the actor after `task` panicked: applies the ready session
    /// events, then tears the session down without a state change, so the
    /// last published state stays and a restarted actor never skips its
    /// backoff (ADR-0013).
    async fn crash(&mut self, task: CrashedTask) -> ActorExit {
        error!(bot_id = %self.spec.id, ?task, "a task of the bot crashed; the actor ends");
        self.drain().await;
        self.session_events = None;
        self.session_request = None;
        self.retry = None;
        self.respawn = None;
        self.watchdog = None;
        if let Some(session) = self.session.take() {
            self.close_session(session);
        }
        self.queue.close();
        while let Some(done) = self.teardowns.join_next().await {
            if let Input::Crashed(teardown) = finished(done, CrashedTask::Teardown, Input::TornDown)
            {
                error!(task = ?teardown, "another task of the bot crashed while it ended");
            }
        }
        if let Some(other) = self.join_tasks().await {
            error!(task = ?other, "another task of the bot crashed while it ended");
        }
        ActorExit::TaskCrashed { task }
    }

    /// Waits for the mode runners and chat deliveries, which their sessions'
    /// tokens have cancelled, and returns the first that panicked.
    async fn join_tasks(&mut self) -> Option<CrashedTask> {
        let mut crashed = None;
        while let Some(done) = self.runners.join_next().await {
            if let Input::Crashed(task) =
                finished(done, CrashedTask::ModeRunner, |()| Input::Nothing)
            {
                crashed = crashed.or(Some(task));
            }
        }
        while let Some(done) = self.deliveries.join_next().await {
            if let Input::Crashed(task) =
                finished(done, CrashedTask::ChatDelivery, |()| Input::Nothing)
            {
                crashed = crashed.or(Some(task));
            }
        }
        crashed
    }
}

/// Turns a finished task into an input: its output, or a crash if it
/// panicked.
fn finished<T>(
    done: Result<T, JoinError>,
    task: CrashedTask,
    input: impl FnOnce(T) -> Input,
) -> Input {
    match done {
        Ok(output) => input(output),
        Err(error) if error.is_panic() => Input::Crashed(task),
        // Only a panic or an abort ends a task early, and the actor never
        // aborts one.
        Err(_) => Input::Nothing,
    }
}

/// Waits for the future in `slot`, or forever when there's none.
async fn wait<F: Future + ?Sized>(slot: &mut Option<Pin<Box<F>>>) -> F::Output {
    match slot {
        Some(future) => future.as_mut().await,
        None => core::future::pending().await,
    }
}

/// Whether the bot is in a run that a server change restarts.
const fn has_run(state: BotState) -> bool {
    matches!(
        state,
        BotState::AwaitingSession { .. }
            | BotState::Connecting { .. }
            | BotState::Online { .. }
            | BotState::Backoff { .. }
    )
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use chrono::DateTime;
    use fleet_core::bot::{BotAccount, PauseReason};
    use fleet_core::disconnect::{ConflictKind, ConflictTexts};
    use fleet_core::mc::SessionEvent;
    use fleet_core::mode::ModeDraft;
    use fleet_core::resilience::CircuitPolicy;
    use fleet_testkit::mc::{EmitOutcome, FakeConnector, FakeCredentials, SessionController};
    use rand::SeedableRng as _;

    use super::*;

    const PAUSED: BotState = BotState::Paused {
        reason: PauseReason::Conflict {
            kind: ConflictKind::DuplicateLogin,
        },
    };

    fn duplicate_login() -> SessionEvent {
        SessionEvent::Disconnected(DisconnectReason::kicked(
            Some("multiplayer.disconnect.duplicate_login"),
            "You logged in from another location",
        ))
    }

    /// An actor whose bot is online in the fake's first session, driven by
    /// hand instead of by `run`, so a test can line inputs up exactly.
    async fn online() -> (
        BotActor<FakeConnector, FakeCredentials>,
        watch::Receiver<BotSnapshot>,
        SessionController,
    ) {
        let connector = FakeConnector::new();
        let spec = BotSpec {
            id: "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap(),
            account: BotAccount::Offline("AfkBot1".parse().unwrap()),
            server: "localhost".try_into().unwrap(),
            mode: ModeDraft { steps: Vec::new() }.validate().unwrap(),
            desired: DesiredRunState::Running,
            conflict_texts: ConflictTexts::default(),
        };
        let config = RuntimeConfig::default();
        let clock = RuntimeClock::new(DateTime::from_timestamp(1_700_000_000, 0).unwrap());
        let (snapshot, snapshots) = watch::channel(BotSnapshot {
            bot_id: spec.id,
            state: BotState::Stopped,
            since: clock.now(),
            last_disconnect: None,
        });
        let policy = CircuitPolicy::try_new(
            core::num::NonZeroUsize::new(8).unwrap(),
            Duration::from_mins(10),
            Duration::from_mins(15),
        )
        .unwrap();
        let (actor, _inbox) = BotActor::new(BotActorParts {
            spec,
            connector: Arc::new(connector.clone()),
            credentials: Arc::new(FakeCredentials::new()),
            retry: RetryPolicy::try_new(
                Duration::from_secs(5),
                Duration::from_mins(5),
                Duration::from_mins(5),
            )
            .unwrap(),
            config,
            clock,
            rng: StdRng::seed_from_u64(1),
            events: broadcast::channel(64).0,
            bucket: ChatBucket::new(config.chat_interval, config.chat_burst).unwrap(),
            tickets: ChatTickets::new(),
            snapshot,
            breaker: watch::Sender::new(CircuitBreaker::new(policy)),
        });
        let mut actor = actor;
        actor.feed(BotEvent::Start).await;
        let answer = wait(&mut actor.session_request).await;
        let _ = actor.handle(Input::Answer(answer)).await;
        let controller = connector.session(0).await;
        assert_eq!(controller.emit(SessionEvent::Joined), EmitOutcome::Queued);
        actor.drain().await;
        assert!(matches!(actor.state, BotState::Online { .. }));
        (actor, snapshots, controller)
    }

    #[tokio::test(start_paused = true)]
    async fn a_queued_duplicate_login_goes_before_a_failed_respawn() {
        let (mut actor, snapshots, controller) = online().await;
        assert_eq!(controller.emit(duplicate_login()), EmitOutcome::Queued);

        let flow = actor.handle(Input::Respawn(RespawnOutcome::GaveUp)).await;

        assert_eq!(flow, ControlFlow::Continue(()));
        assert_eq!(snapshots.borrow().state, PAUSED);
        assert!(matches!(
            snapshots.borrow().last_disconnect,
            Some(DisconnectReason::Kicked(_))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_queued_event_a_failed_respawn_ends_the_session() {
        let (mut actor, snapshots, controller) = online().await;

        let _ = actor.handle(Input::Respawn(RespawnOutcome::GaveUp)).await;

        assert!(matches!(snapshots.borrow().state, BotState::Backoff { .. }));
        assert_eq!(
            snapshots.borrow().last_disconnect,
            Some(DisconnectReason::RespawnFailed)
        );
        actor.teardowns.join_next().await;
        assert!(controller.is_torn_down());
    }

    #[tokio::test(start_paused = true)]
    async fn a_respawn_that_worked_or_found_the_session_ended_changes_nothing() {
        let (mut actor, snapshots, _controller) = online().await;

        let _ = actor
            .handle(Input::Respawn(RespawnOutcome::Respawned))
            .await;
        let _ = actor
            .handle(Input::Respawn(RespawnOutcome::SessionEnded))
            .await;

        assert!(matches!(snapshots.borrow().state, BotState::Online { .. }));
    }

    #[tokio::test(start_paused = true)]
    async fn a_queued_duplicate_login_goes_before_a_crash_exit() {
        let (mut actor, snapshots, controller) = online().await;
        assert_eq!(controller.emit(duplicate_login()), EmitOutcome::Queued);

        let exit = actor
            .handle(Input::Crashed(CrashedTask::ChatDelivery))
            .await;

        assert_eq!(
            exit,
            ControlFlow::Break(ActorExit::TaskCrashed {
                task: CrashedTask::ChatDelivery
            })
        );
        assert_eq!(snapshots.borrow().state, PAUSED);
        assert!(controller.is_torn_down());
    }

    #[tokio::test(start_paused = true)]
    async fn a_crash_exit_tears_down_without_a_state_change() {
        let (mut actor, snapshots, controller) = online().await;
        let before = snapshots.borrow().clone();

        let exit = actor.handle(Input::Crashed(CrashedTask::Teardown)).await;

        assert_eq!(
            exit,
            ControlFlow::Break(ActorExit::TaskCrashed {
                task: CrashedTask::Teardown
            })
        );
        assert_eq!(*snapshots.borrow(), before);
        assert!(controller.is_torn_down());
    }

    #[test]
    fn a_task_that_ended_normally_is_no_crash() {
        let input = finished(Ok(7_u64), CrashedTask::Teardown, Input::TornDown);

        assert!(matches!(input, Input::TornDown(7)));
    }

    #[test]
    fn a_server_change_restarts_only_a_bot_in_a_run() {
        let attempt = core::num::NonZeroU32::MIN;
        assert!(has_run(BotState::Backoff { attempt }));
        assert!(has_run(BotState::AwaitingSession {
            attempt,
            fresh: false
        }));
        assert!(!has_run(BotState::Stopped));
        assert!(!has_run(BotState::Stopping { restart: true }));
        assert!(!has_run(PAUSED));
    }

    #[tokio::test]
    async fn an_aborted_task_is_no_crash() {
        let mut tasks = JoinSet::new();
        tasks.spawn(core::future::pending::<u64>()).abort();
        let done = tasks.join_next().await.unwrap();

        let input = finished(done, CrashedTask::Teardown, Input::TornDown);

        assert!(matches!(input, Input::Nothing));
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_future_or_a_session_there_is_nothing_to_wait_for() {
        let mut slot: Slot<()> = None;
        let mut events: Option<fleet_testkit::mc::FakeEvents> = None;

        let slot = tokio::time::timeout(Duration::from_secs(1), wait(&mut slot)).await;
        let event = tokio::time::timeout(Duration::from_secs(1), next_event(&mut events)).await;

        assert!(slot.is_err());
        assert!(event.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn joining_the_tasks_reports_the_first_that_crashed() {
        let (mut actor, _snapshots, _controller) = online().await;
        // The session's own runner and delivery end with its token.
        actor.session.as_ref().unwrap().token.cancel();
        actor
            .deliveries
            .spawn(async { panic!("a test panic in a delivery") });
        actor
            .runners
            .spawn(async { panic!("a test panic in a runner") });

        assert_eq!(actor.join_tasks().await, Some(CrashedTask::ModeRunner));
        assert!(actor.runners.is_empty());
        assert!(actor.deliveries.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_crash_exit_waits_for_the_other_tasks_even_when_they_crash_too() {
        let (mut actor, _snapshots, _controller) = online().await;
        actor
            .teardowns
            .spawn(async { panic!("a test panic in a teardown") });
        actor
            .runners
            .spawn(async { panic!("a test panic in a runner") });

        let exit = actor.crash(CrashedTask::ChatDelivery).await;

        assert_eq!(
            exit,
            ActorExit::TaskCrashed {
                task: CrashedTask::ChatDelivery
            }
        );
        assert!(actor.teardowns.is_empty());
        assert!(actor.runners.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_connect_without_credentials_fails_the_attempt() {
        let (mut actor, _snapshots, _controller) = online().await;

        actor.connect().await;

        assert_eq!(
            actor.pending.pop_front(),
            Some(BotEvent::ConnectFailed(
                fleet_core::disconnect::ConnectFailure::Other
            ))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn debug_output_names_the_bot_and_its_state_only() {
        let (actor, _snapshots, _controller) = online().await;

        let debug = format!("{actor:?}");

        assert!(
            debug.starts_with("BotActor { bot_id: BotId(018bcfe5"),
            "{debug}"
        );
        assert!(debug.contains("state: Online"), "{debug}");
        assert!(debug.ends_with(", .. }"), "{debug}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_queued_duplicate_login_goes_before_a_watchdog_trip() {
        let (mut actor, snapshots, controller) = online().await;
        controller.freeze_ticks();
        tokio::time::advance(Duration::from_secs(30)).await;
        assert_eq!(controller.emit(duplicate_login()), EmitOutcome::Queued);

        let _ = actor.handle(Input::WatchdogCheck).await;

        assert_eq!(snapshots.borrow().state, PAUSED);
        assert!(matches!(
            snapshots.borrow().last_disconnect,
            Some(DisconnectReason::Kicked(_))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_queued_event_a_stalled_session_trips_the_watchdog() {
        let (mut actor, snapshots, controller) = online().await;
        controller.freeze_ticks();
        tokio::time::advance(Duration::from_secs(30)).await;

        let _ = actor.handle(Input::WatchdogCheck).await;

        assert!(matches!(snapshots.borrow().state, BotState::Backoff { .. }));
        assert_eq!(
            snapshots.borrow().last_disconnect,
            Some(DisconnectReason::WatchdogTimeout)
        );
        assert!(actor.watchdog.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_live_session_arms_the_next_check() {
        let (mut actor, snapshots, _controller) = online().await;
        actor.watchdog = None;

        let _ = actor.handle(Input::WatchdogCheck).await;

        assert!(matches!(snapshots.borrow().state, BotState::Online { .. }));
        assert!(actor.watchdog.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn a_zero_watchdog_period_still_waits_a_millisecond() {
        let (mut actor, _snapshots, _controller) = online().await;
        actor.config.watchdog_period = Duration::ZERO;
        actor.arm_watchdog();
        let started = tokio::time::Instant::now();

        wait(&mut actor.watchdog).await;

        assert_eq!(started.elapsed(), Duration::from_millis(1));
    }
}
