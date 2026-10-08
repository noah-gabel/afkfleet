//! Component tests for the bot actor (Plan.md P4.2; ADR-0010, ADR-0013),
//! against fleet-testkit's fakes on paused time.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

mod common;

use core::future::Future;
use core::num::{NonZeroU32, NonZeroUsize};
use core::time::Duration;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::bot::{
    BotAccount, BotNotification, BotSnapshot, BotSpec, BotState, DesiredRunState, FailReason,
    PauseReason,
};
use fleet_core::chat::{ChatMessage, IncomingChat};
use fleet_core::disconnect::{
    ConflictKind, ConflictTexts, ConnectFailure, DisconnectReason, PermanentKind,
};
use fleet_core::id::BotId;
use fleet_core::mc::{
    ConnectError, ConnectParams, CredentialError, Liveness, MinecraftConnector, SessionError,
    SessionEvent, SessionHandle,
};
use fleet_core::mode::{Action, GameAction, ModeDefinition, ModeDraft, Schedule, Step};
use fleet_core::resilience::{CircuitBreaker, CircuitPolicy, RetryPolicy};
use fleet_core::time;
use fleet_runtime::{
    ActorExit, BotActor, BotActorParts, BotCommand, BotInbox, ChatBucket, ChatError, ChatTickets,
    CrashedTask, FleetEvent, FleetEventKind, InboxError, RuntimeClock, RuntimeConfig,
};
use fleet_testkit::mc::{
    EmitOutcome, FakeConnector, FakeCredentials, FakeEvents, FakeSession, Performed,
    SessionController,
};
use rand::SeedableRng;
use rand::rngs::StdRng;
use tokio::sync::{broadcast, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::Level;

use common::Levels;

// --- Values ---

const BOT: &str = "018bcfe5-6800-7bab-abab-abababababab";

const fn ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

const fn secs(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

fn n(attempt: u32) -> NonZeroU32 {
    NonZeroU32::new(attempt).unwrap()
}

fn bot_id() -> BotId {
    BOT.parse().unwrap()
}

/// The runtime clock starts here in every test.
fn anchor() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).unwrap()
}

fn message(text: &str) -> ChatMessage {
    text.parse().unwrap()
}

fn mode(steps: Vec<Step>) -> ModeDefinition {
    ModeDraft { steps }.validate().unwrap()
}

fn at_start(action: Action) -> Step {
    Step {
        action,
        schedule: Schedule::AtStart,
        probability: 100,
    }
}

fn idle() -> ModeDefinition {
    mode(Vec::new())
}

fn spec(desired: DesiredRunState) -> BotSpec {
    BotSpec {
        id: bot_id(),
        account: BotAccount::Offline("AfkBot1".parse().unwrap()),
        server: "localhost".try_into().unwrap(),
        mode: idle(),
        desired,
        conflict_texts: ConflictTexts::default(),
    }
}

fn running() -> BotSpec {
    spec(DesiredRunState::Running)
}

fn stopped() -> BotSpec {
    spec(DesiredRunState::Stopped)
}

/// The defaults of Appendix A: 5 s to 300 s, stable after 300 s.
fn policy() -> RetryPolicy {
    RetryPolicy::try_new(secs(5), secs(300), secs(300)).unwrap()
}

/// `threshold` failures within 600 s open the breaker for 900 s.
fn circuit(threshold: usize) -> CircuitPolicy {
    CircuitPolicy::try_new(NonZeroUsize::new(threshold).unwrap(), secs(600), secs(900)).unwrap()
}

fn kick(key: &str) -> SessionEvent {
    SessionEvent::Disconnected(DisconnectReason::kicked(Some(key), "Kicked"))
}

fn duplicate_login() -> SessionEvent {
    kick("multiplayer.disconnect.duplicate_login")
}

fn transient_kick() -> SessionEvent {
    kick("multiplayer.disconnect.server_shutdown")
}

const DUPLICATE_LOGIN: BotState = BotState::Paused {
    reason: PauseReason::Conflict {
        kind: ConflictKind::DuplicateLogin,
    },
};

const fn online(since: DateTime<Utc>, attempt: NonZeroU32) -> BotState {
    BotState::Online { since, attempt }
}

const fn awaiting(attempt: NonZeroU32) -> BotState {
    BotState::AwaitingSession {
        attempt,
        fresh: false,
    }
}

const fn connecting(attempt: NonZeroU32) -> BotState {
    BotState::Connecting {
        attempt,
        auth_retried: false,
    }
}

const fn backoff(attempt: NonZeroU32) -> BotState {
    BotState::Backoff { attempt }
}

fn emit(controller: &SessionController, event: SessionEvent) {
    assert_eq!(controller.emit(event), EmitOutcome::Queued);
}

// --- Time ---

/// Lets every spawned task run until it waits.
async fn settle() {
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}

/// Moves paused time forward by `duration` and lets the tasks run.
async fn advance(duration: Duration) {
    tokio::time::advance(duration).await;
    settle().await;
}

// --- The actor under test ---

/// What a test sets up before the actor starts.
struct Setup {
    spec: BotSpec,
    config: RuntimeConfig,
    retry: RetryPolicy,
    circuit: CircuitPolicy,
    seed: u64,
    connector: FakeConnector,
    credentials: FakeCredentials,
}

impl Setup {
    fn new(spec: BotSpec) -> Self {
        Self {
            spec,
            config: RuntimeConfig::default(),
            retry: policy(),
            circuit: circuit(8),
            seed: 1,
            connector: FakeConnector::new(),
            credentials: FakeCredentials::new(),
        }
    }

    async fn start(self) -> Bot {
        let connector = self.connector.clone();
        self.start_with(connector).await
    }

    /// Starts the actor on `connector`, which may wrap this setup's fake.
    async fn start_with<C: MinecraftConnector>(self, connector: C) -> Bot {
        let clock = RuntimeClock::new(anchor());
        let (events_tx, events) = broadcast::channel(256);
        let (snapshot_tx, snapshot) = watch::channel(BotSnapshot {
            bot_id: bot_id(),
            state: BotState::Stopped,
            since: anchor(),
            last_disconnect: None,
        });
        let (breaker_tx, breaker) = watch::channel(CircuitBreaker::new(self.circuit));
        let bucket = ChatBucket::new(self.config.chat_interval, self.config.chat_burst).unwrap();
        let (actor, inbox) = BotActor::new(BotActorParts {
            spec: self.spec,
            connector: Arc::new(connector),
            credentials: Arc::new(self.credentials.clone()),
            retry: self.retry,
            config: self.config,
            clock,
            rng: StdRng::seed_from_u64(self.seed),
            events: events_tx,
            bucket,
            tickets: ChatTickets::new(),
            snapshot: snapshot_tx,
            breaker: breaker_tx,
        });
        let cancel = CancellationToken::new();
        let task = tokio::spawn(actor.run(cancel.clone()));
        settle().await;
        Bot {
            connector: self.connector,
            credentials: self.credentials,
            circuit: self.circuit,
            inbox: Some(inbox),
            snapshot,
            breaker,
            events,
            cancel,
            task,
            clock,
        }
    }
}

/// A running actor and the test's handles on it.
struct Bot {
    connector: FakeConnector,
    credentials: FakeCredentials,
    circuit: CircuitPolicy,
    inbox: Option<BotInbox>,
    snapshot: watch::Receiver<BotSnapshot>,
    breaker: watch::Receiver<CircuitBreaker>,
    events: broadcast::Receiver<FleetEvent>,
    cancel: CancellationToken,
    task: JoinHandle<ActorExit>,
    clock: RuntimeClock,
}

impl Bot {
    /// Starts a bot that should run, joins its first session and forgets
    /// the events so far.
    async fn online(setup: Setup) -> (Self, SessionController) {
        let mut bot = setup.start().await;
        let controller = bot.session(0).await;
        emit(&controller, SessionEvent::Joined);
        settle().await;
        assert!(matches!(bot.state(), BotState::Online { .. }));
        let _ = bot.kinds();
        (bot, controller)
    }

    fn state(&self) -> BotState {
        self.snapshot.borrow().state
    }

    fn last_disconnect(&self) -> Option<DisconnectReason> {
        self.snapshot.borrow().last_disconnect.clone()
    }

    fn connects(&self) -> usize {
        self.connector.connects().len()
    }

    async fn session(&self, index: usize) -> SessionController {
        tokio::time::timeout(secs(1), self.connector.session(index))
            .await
            .expect("the session should have started")
    }

    /// Every event published since the last call, in order.
    fn kinds(&mut self) -> Vec<FleetEventKind> {
        let mut kinds = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            assert_eq!(event.bot_id, bot_id());
            kinds.push(event.kind);
        }
        kinds
    }

    /// The states published since the last call, in order.
    fn states(&mut self) -> Vec<BotState> {
        self.kinds()
            .into_iter()
            .filter_map(|kind| match kind {
                FleetEventKind::StateChanged(snapshot) => Some(snapshot.state),
                _ => None,
            })
            .collect()
    }

    fn inbox(&self) -> &BotInbox {
        self.inbox.as_ref().unwrap()
    }

    async fn send(&self, command: BotCommand) {
        self.inbox().try_send(command).unwrap();
        settle().await;
    }

    async fn update(&self, spec: BotSpec) {
        self.send(BotCommand::UpdateSpec(Box::new(spec))).await;
    }

    async fn chat(&self, text: &str) -> Result<fleet_runtime::ChatTicket, ChatError> {
        let (reply, answer) = oneshot::channel();
        self.send(BotCommand::SendChat {
            message: message(text),
            reply,
        })
        .await;
        answer.await.unwrap()
    }

    async fn exit(self) -> ActorExit {
        tokio::time::timeout(secs(60), self.task)
            .await
            .expect("the actor should have ended")
            .unwrap()
    }
}

// --- The scenarios of P4.2 ---

#[tokio::test(start_paused = true)]
async fn the_happy_path_asks_for_a_session_connects_and_goes_online() {
    let mut bot = Setup::new(running()).start().await;
    let controller = bot.session(0).await;

    emit(&controller, SessionEvent::Joined);
    settle().await;

    let since = bot.clock.now();
    assert_eq!(bot.state(), online(since, n(1)));
    assert_eq!(
        bot.states(),
        [awaiting(n(1)), connecting(n(1)), online(since, n(1))]
    );
    let requests = bot.credentials.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].bot_id, bot_id());
    assert!(!requests[0].fresh);
    let connects = bot.connector.connects();
    assert_eq!(connects.len(), 1);
    assert_eq!(connects[0].bot_id, bot_id());
    assert_eq!(connects[0].server, running().server);
    assert_eq!(connects[0].credentials.username().as_str(), "AfkBot1");
    assert_eq!(connects[0].connect_timeout, secs(30));
    assert_eq!(*bot.breaker.borrow(), CircuitBreaker::new(bot.circuit));
}

#[tokio::test(start_paused = true)]
async fn a_transient_kick_backs_off_then_reconnects_within_the_policy_bounds() {
    let (mut bot, controller) = Bot::online(Setup::new(running())).await;

    emit(&controller, transient_kick());
    settle().await;

    assert_eq!(bot.state(), backoff(n(1)));
    assert!(controller.is_torn_down());
    assert!(matches!(
        bot.last_disconnect(),
        Some(DisconnectReason::Kicked(_))
    ));
    assert_ne!(
        *bot.breaker.borrow(),
        CircuitBreaker::new(bot.circuit),
        "the failure was recorded"
    );
    let (lower, upper) = policy().bounds(n(1));
    advance(lower.checked_sub(ms(1)).unwrap()).await;
    assert_eq!(bot.connects(), 1);
    advance(upper.checked_sub(lower).unwrap() + ms(1)).await;
    assert_eq!(bot.connects(), 2);
    assert_eq!(bot.state(), connecting(n(2)));
    assert_eq!(
        bot.states(),
        [backoff(n(1)), awaiting(n(2)), connecting(n(2))]
    );
}

#[tokio::test(start_paused = true)]
async fn a_permanent_kick_fails_the_bot_for_good() {
    let (mut bot, controller) = Bot::online(Setup::new(running())).await;

    emit(&controller, kick("multiplayer.disconnect.banned"));
    settle().await;
    advance(secs(3_600)).await;

    let reason = FailReason::Permanent {
        kind: PermanentKind::Banned,
    };
    assert_eq!(bot.state(), BotState::Failed { reason });
    assert_eq!(bot.connects(), 1);
    let kinds = bot.kinds();
    assert!(matches!(
        &kinds[..],
        [
            FleetEventKind::StateChanged(snapshot),
            FleetEventKind::Alert(BotNotification::Failed { reason: alerted }),
        ] if snapshot.state == BotState::Failed { reason } && *alerted == reason
    ));
}

#[tokio::test(start_paused = true)]
async fn a_duplicate_login_pauses_the_bot_and_never_fights_the_human() {
    let (mut bot, controller) = Bot::online(Setup::new(running())).await;

    emit(&controller, duplicate_login());
    settle().await;
    advance(secs(3_600)).await;

    assert_eq!(bot.state(), DUPLICATE_LOGIN);
    assert_eq!(bot.connects(), 1);
    assert_eq!(
        bot.kinds().last(),
        Some(&FleetEventKind::Alert(BotNotification::Paused {
            reason: PauseReason::Conflict {
                kind: ConflictKind::DuplicateLogin
            }
        }))
    );
}

#[tokio::test(start_paused = true)]
async fn a_kick_text_on_the_conflict_list_pauses_the_bot() {
    let mut spec = running();
    spec.conflict_texts = ConflictTexts::try_new(&["You logged in from another location"]).unwrap();
    let (bot, controller) = Bot::online(Setup::new(spec)).await;

    emit(
        &controller,
        SessionEvent::Disconnected(DisconnectReason::kicked(
            None,
            "You logged in from another location",
        )),
    );
    settle().await;

    assert_eq!(bot.state(), DUPLICATE_LOGIN);
}

#[tokio::test(start_paused = true)]
async fn stopping_during_backoff_stops_at_once_and_never_reconnects() {
    let (mut bot, controller) = Bot::online(Setup::new(running())).await;
    emit(&controller, transient_kick());
    settle().await;

    bot.update(stopped()).await;
    advance(secs(3_600)).await;

    assert_eq!(bot.state(), BotState::Stopped);
    assert_eq!(bot.connects(), 1);
    assert_eq!(bot.states(), [backoff(n(1)), BotState::Stopped]);
}

#[tokio::test(start_paused = true)]
async fn a_server_change_while_online_reconnects_to_the_new_server() {
    let (mut bot, controller) = Bot::online(Setup::new(running())).await;
    let mut moved = running();
    moved.server = "example.com".try_into().unwrap();

    bot.update(moved.clone()).await;

    assert!(controller.is_torn_down());
    assert_eq!(
        bot.states(),
        [
            BotState::Stopping { restart: false },
            BotState::Stopping { restart: true },
            awaiting(n(1)),
            connecting(n(1)),
        ]
    );
    let connects = bot.connector.connects();
    assert_eq!(connects.len(), 2);
    assert_eq!(connects[1].server, moved.server);
}

#[tokio::test(start_paused = true)]
async fn a_mode_change_while_online_does_not_reconnect() {
    let mut spec = running();
    spec.mode = mode(vec![at_start(Action::Jump)]);
    let (mut bot, controller) = Bot::online(Setup::new(spec.clone())).await;

    spec.mode = mode(vec![at_start(Action::SwingArm)]);
    bot.update(spec).await;

    assert_eq!(
        controller.log(),
        [
            Performed::Action(GameAction::Jump),
            Performed::Action(GameAction::HoldUse { on: false }),
            Performed::Action(GameAction::Sneak { on: false }),
            Performed::Action(GameAction::SwingArm),
        ]
    );
    assert!(!controller.is_torn_down());
    assert_eq!(bot.connects(), 1);
    assert_eq!(bot.states(), []);
}

// --- Sessions and credentials ---

#[tokio::test(start_paused = true)]
async fn the_mode_runs_and_chat_goes_out_once_online() {
    let mut spec = running();
    spec.mode = mode(vec![
        at_start(Action::Jump),
        at_start(Action::SendChat {
            message: message("hello"),
        }),
    ]);
    let mut bot = Setup::new(spec).start().await;
    let controller = bot.session(0).await;

    emit(&controller, SessionEvent::Joined);
    settle().await;
    let ticket = bot.chat("hi there").await.unwrap();
    settle().await;

    assert_eq!(
        controller.log(),
        [
            Performed::Action(GameAction::Jump),
            Performed::Chat(message("hello")),
            Performed::Chat(message("hi there")),
        ]
    );
    let chat: Vec<_> = bot
        .kinds()
        .into_iter()
        .filter(|kind| !matches!(kind, FleetEventKind::StateChanged(_)))
        .collect();
    assert_eq!(
        chat,
        [
            FleetEventKind::ModeChatSent {
                message: message("hello")
            },
            FleetEventKind::ChatSent { ticket },
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn user_chat_is_refused_until_the_bot_is_online() {
    let bot = Setup::new(running()).start().await;

    assert_eq!(bot.state(), connecting(n(1)));
    assert_eq!(bot.chat("too early").await, Err(ChatError::NotOnline));
}

#[tokio::test(start_paused = true)]
async fn incoming_chat_and_deaths_are_published() {
    let (mut bot, controller) = Bot::online(Setup::new(running())).await;

    emit(
        &controller,
        SessionEvent::Chat(IncomingChat::system("Welcome")),
    );
    emit(&controller, SessionEvent::Died);
    settle().await;

    assert_eq!(
        bot.kinds(),
        [
            FleetEventKind::ChatReceived(IncomingChat::system("Welcome")),
            FleetEventKind::Died,
        ]
    );
    assert_eq!(controller.log(), [Performed::Respawn]);
}

#[tokio::test(start_paused = true)]
async fn a_session_request_that_never_answers_times_out_after_30_s() {
    let setup = Setup::new(running());
    setup.credentials.push_never_answer();
    let bot = setup.start().await;

    advance(ms(29_999)).await;
    assert_eq!(bot.state(), awaiting(n(1)));
    advance(ms(1)).await;

    assert_eq!(bot.state(), backoff(n(1)));
    assert_eq!(bot.connects(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_rejected_session_is_retried_once_with_a_fresh_token_then_fails() {
    let mut bot = Setup::new(running()).start().await;
    let first = bot.session(0).await;

    emit(
        &first,
        SessionEvent::Disconnected(DisconnectReason::AuthRejected),
    );
    settle().await;
    let second = bot.session(1).await;
    emit(
        &second,
        SessionEvent::Disconnected(DisconnectReason::AuthRejected),
    );
    settle().await;

    let fresh: Vec<bool> = bot
        .credentials
        .requests()
        .iter()
        .map(|request| request.fresh)
        .collect();
    assert_eq!(fresh, [false, true]);
    assert_eq!(bot.connects(), 2);
    assert_eq!(
        bot.state(),
        BotState::Failed {
            reason: FailReason::Auth
        }
    );
    assert!(bot.states().contains(&BotState::AwaitingSession {
        attempt: n(1),
        fresh: true
    }));
}

#[tokio::test(start_paused = true)]
async fn a_session_that_cannot_be_issued_fails_the_bot() {
    let setup = Setup::new(running());
    setup
        .credentials
        .push_answer(Err(CredentialError { retryable: false }));
    let bot = setup.start().await;

    assert_eq!(
        bot.state(),
        BotState::Failed {
            reason: FailReason::SessionDenied
        }
    );
    assert_eq!(bot.connects(), 0);
}

#[tokio::test(start_paused = true)]
async fn no_host_thread_backs_off_and_is_recorded() {
    let setup = Setup::new(running());
    setup
        .connector
        .push_connect_result(Err(ConnectError::HostUnavailable));
    let bot = setup.start().await;

    assert_eq!(bot.state(), backoff(n(1)));
    assert_eq!(
        bot.last_disconnect(),
        Some(DisconnectReason::ConnectFailed {
            failure: ConnectFailure::HostUnavailable
        })
    );
}

#[tokio::test(start_paused = true)]
async fn a_connection_that_fails_backs_off_and_is_recorded() {
    let bot = Setup::new(running()).start().await;
    let controller = bot.session(0).await;

    emit(
        &controller,
        SessionEvent::ConnectionFailed(ConnectFailure::Refused),
    );
    settle().await;

    assert_eq!(bot.state(), backoff(n(1)));
    assert_eq!(
        bot.last_disconnect(),
        Some(DisconnectReason::ConnectFailed {
            failure: ConnectFailure::Refused
        })
    );
}

#[tokio::test(start_paused = true)]
async fn a_session_that_is_unavailable_for_now_backs_off() {
    let setup = Setup::new(running());
    setup
        .credentials
        .push_answer(Err(CredentialError { retryable: true }));
    let bot = setup.start().await;

    assert_eq!(bot.state(), backoff(n(1)));
    assert_eq!(bot.connects(), 0);
    assert_eq!(bot.last_disconnect(), None, "no session ended");
}

/// A connector whose `connect` never resolves.
#[derive(Debug, Clone, Copy)]
struct HangingConnector;

impl MinecraftConnector for HangingConnector {
    type Session = FakeSession;
    type Events = FakeEvents;

    fn connect(
        &self,
        _: ConnectParams,
    ) -> impl Future<Output = Result<(Self::Session, Self::Events), ConnectError>> + Send {
        core::future::pending()
    }
}

#[tokio::test(start_paused = true)]
async fn a_connect_that_hangs_times_out_and_backs_off() {
    let bot = Setup::new(running()).start_with(HangingConnector).await;
    assert_eq!(bot.state(), connecting(n(1)));

    advance(ms(29_999)).await;
    assert_eq!(bot.state(), connecting(n(1)));
    advance(ms(1)).await;

    assert_eq!(bot.state(), backoff(n(1)));
    assert_eq!(
        bot.last_disconnect(),
        Some(DisconnectReason::ConnectFailed {
            failure: ConnectFailure::TimedOut
        })
    );
}

/// A connector that keeps a clone of every session's handle, so a test can
/// end a session from outside the actor.
#[derive(Debug, Clone, Default)]
struct KeepingConnector {
    fake: FakeConnector,
    handles: Arc<std::sync::Mutex<Vec<FakeSession>>>,
}

impl MinecraftConnector for KeepingConnector {
    type Session = FakeSession;
    type Events = FakeEvents;

    fn connect(
        &self,
        params: ConnectParams,
    ) -> impl Future<Output = Result<(Self::Session, Self::Events), ConnectError>> + Send {
        let connector = self.clone();
        async move {
            let (session, events) = connector.fake.connect(params).await?;
            connector.handles.lock().unwrap().push(session.clone());
            Ok((session, events))
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_session_whose_events_end_without_a_reason_counts_as_a_crash() {
    let connector = KeepingConnector::default();
    let mut setup = Setup::new(running());
    setup.connector = connector.fake.clone();
    let bot = setup.start_with(connector.clone()).await;
    let controller = bot.session(0).await;
    emit(&controller, SessionEvent::Joined);
    settle().await;

    // A teardown the actor didn't start ends the events without a terminal
    // event, as when a session's host thread is gone.
    let handle = connector.handles.lock().unwrap()[0].clone();
    handle.disconnect().await;
    settle().await;

    assert_eq!(bot.state(), backoff(n(1)));
    assert_eq!(
        bot.last_disconnect(),
        Some(DisconnectReason::SessionCrashed)
    );
}

// --- The breaker and the backoff ---

#[tokio::test(start_paused = true)]
async fn an_open_breaker_holds_the_retry_until_its_cool_down_ends() {
    let mut setup = Setup::new(running());
    setup.circuit = circuit(2);
    setup
        .connector
        .push_connect_result(Err(ConnectError::HostUnavailable));
    setup
        .connector
        .push_connect_result(Err(ConnectError::HostUnavailable));
    let bot = setup.start().await;
    assert_eq!(bot.breaker.borrow().open_until(), None);

    advance(policy().bounds(n(1)).1).await;

    assert_eq!(bot.connects(), 2);
    assert_eq!(bot.state(), backoff(n(2)));
    let until = bot
        .breaker
        .borrow()
        .open_until()
        .expect("the breaker opened");
    let wait = time::elapsed(bot.clock.now(), until);
    assert!(
        wait > policy().bounds(n(2)).1,
        "the cool-down is the later one"
    );
    advance(wait.checked_sub(ms(1)).unwrap()).await;
    assert_eq!(bot.connects(), 2);
    advance(ms(2)).await;
    assert_eq!(bot.connects(), 3);
}

#[tokio::test(start_paused = true)]
async fn a_stable_session_records_a_success_and_starts_the_attempts_over() {
    let mut setup = Setup::new(running());
    setup.retry = RetryPolicy::try_new(secs(5), secs(300), secs(60)).unwrap();
    setup
        .connector
        .push_connect_result(Err(ConnectError::HostUnavailable));
    let bot = setup.start().await;
    assert_ne!(*bot.breaker.borrow(), CircuitBreaker::new(bot.circuit));
    advance(secs(10)).await;
    let controller = bot.session(0).await;
    emit(&controller, SessionEvent::Joined);
    settle().await;
    assert!(matches!(bot.state(), BotState::Online { attempt, .. } if attempt == n(2)));

    advance(secs(60)).await;
    emit(&controller, transient_kick());
    settle().await;

    assert_eq!(bot.state(), backoff(n(1)));
    assert_eq!(*bot.breaker.borrow(), CircuitBreaker::new(bot.circuit));
}

/// A seed whose first backoff (the old timer) is due before the second one
/// would be, so a timer that survived a restart would fire early.
const RESTART_SEED: u64 = 3;

#[tokio::test(start_paused = true)]
async fn a_restart_during_backoff_starts_over_and_drops_the_old_timer() {
    let mut rng = StdRng::seed_from_u64(RESTART_SEED);
    let old = policy().delay(n(1), &mut rng);
    let new = policy().delay(n(1), &mut rng);
    assert!(
        old + ms(3) < new,
        "pick a seed whose old timer is due first"
    );
    let mut setup = Setup::new(running());
    setup.seed = RESTART_SEED;
    setup
        .connector
        .push_connect_result(Err(ConnectError::HostUnavailable));
    setup
        .connector
        .push_connect_result(Err(ConnectError::HostUnavailable));
    let mut bot = setup.start().await;
    advance(ms(1)).await;

    bot.send(BotCommand::Restart).await;

    assert_eq!(bot.connects(), 2);
    assert_eq!(
        bot.states(),
        [
            awaiting(n(1)),
            connecting(n(1)),
            backoff(n(1)),
            BotState::Stopped,
            awaiting(n(1)),
            connecting(n(1)),
            backoff(n(1)),
        ]
    );
    advance(new.checked_sub(ms(1)).unwrap()).await;
    assert_eq!(bot.connects(), 2, "the old timer must not fire");
    advance(ms(2)).await;
    assert_eq!(bot.connects(), 3);
}

#[tokio::test(start_paused = true)]
async fn a_restart_while_waiting_for_a_session_drops_the_old_request() {
    let setup = Setup::new(running());
    setup.credentials.push_never_answer();
    setup.credentials.push_never_answer();
    let bot = setup.start().await;
    advance(secs(10)).await;

    bot.send(BotCommand::Restart).await;
    advance(secs(20) + ms(1)).await;

    assert_eq!(bot.credentials.requests().len(), 2);
    assert_eq!(bot.state(), awaiting(n(1)), "the old timeout must not fire");
    advance(secs(10)).await;
    assert_eq!(bot.state(), backoff(n(1)));
}

// --- Teardown ---

#[tokio::test(start_paused = true)]
async fn the_bot_is_stopped_only_once_its_teardown_has_finished() {
    let (bot, controller) = Bot::online(Setup::new(running())).await;
    controller.delay_disconnect(secs(20));

    bot.update(stopped()).await;
    advance(ms(19_999)).await;
    assert_eq!(bot.state(), BotState::Stopping { restart: false });
    advance(ms(1)).await;

    assert_eq!(bot.state(), BotState::Stopped);
}

#[tokio::test(start_paused = true)]
async fn an_old_sessions_slow_teardown_never_ends_a_newer_session() {
    let (bot, old) = Bot::online(Setup::new(running())).await;
    old.delay_disconnect(secs(60));
    emit(&old, transient_kick());
    settle().await;
    advance(policy().bounds(n(1)).1).await;
    let new = bot.session(1).await;
    emit(&new, SessionEvent::Joined);
    settle().await;
    assert!(matches!(bot.state(), BotState::Online { .. }));

    advance(secs(60)).await;

    assert!(matches!(bot.state(), BotState::Online { .. }));
    assert!(!new.is_torn_down());
    assert_eq!(bot.connects(), 2);
}

// --- Respawn ---

#[tokio::test(start_paused = true)]
async fn a_failed_respawn_is_retried_every_5_s_until_it_works() {
    let levels = Levels::default();
    let _guard = tracing::subscriber::set_default(levels.clone());
    let (bot, controller) = Bot::online(Setup::new(running())).await;
    controller.fail_respawn(SessionError::TimedOut);

    emit(&controller, SessionEvent::Died);
    settle().await;
    advance(secs(5)).await;
    controller.succeed_respawn();
    advance(ms(4_999)).await;
    assert_eq!(controller.log(), []);
    advance(ms(1)).await;

    assert_eq!(controller.log(), [Performed::Respawn]);
    assert!(matches!(bot.state(), BotState::Online { .. }));
    assert_eq!(levels.of("a respawn failed"), [Level::WARN, Level::DEBUG]);
}

#[tokio::test(start_paused = true)]
async fn twelve_failed_respawns_end_the_session() {
    let (bot, controller) = Bot::online(Setup::new(running())).await;
    controller.fail_respawn(SessionError::TimedOut);

    emit(&controller, SessionEvent::Died);
    settle().await;
    for _ in 0..10 {
        advance(secs(5)).await;
    }
    advance(ms(4_999)).await;
    assert!(matches!(bot.state(), BotState::Online { .. }));
    advance(ms(1)).await;

    assert_eq!(bot.state(), backoff(n(1)));
    assert_eq!(bot.last_disconnect(), Some(DisconnectReason::RespawnFailed));
    assert!(controller.is_torn_down());
}

#[tokio::test(start_paused = true)]
async fn a_respawn_that_finds_the_session_closed_stops_without_a_warning() {
    let levels = Levels::default();
    let _guard = tracing::subscriber::set_default(levels.clone());
    let (bot, controller) = Bot::online(Setup::new(running())).await;
    controller.fail_respawn(SessionError::Closed);

    emit(&controller, SessionEvent::Died);
    settle().await;
    advance(secs(120)).await;

    assert!(matches!(bot.state(), BotState::Online { .. }));
    assert_eq!(levels.of("the respawn stops"), [Level::DEBUG]);
    assert!(!levels.any_warning());
}

#[tokio::test(start_paused = true)]
async fn each_death_warns_on_its_first_failed_respawn() {
    let levels = Levels::default();
    let _guard = tracing::subscriber::set_default(levels.clone());
    let (_bot, controller) = Bot::online(Setup::new(running())).await;
    controller.fail_respawn(SessionError::TimedOut);
    emit(&controller, SessionEvent::Died);
    settle().await;
    advance(secs(5)).await;
    controller.succeed_respawn();
    advance(secs(5)).await;

    controller.fail_respawn(SessionError::TimedOut);
    emit(&controller, SessionEvent::Died);
    settle().await;

    assert_eq!(
        levels.of("a respawn failed"),
        [Level::WARN, Level::DEBUG, Level::WARN]
    );
}

// --- Commands ---

#[tokio::test(start_paused = true)]
async fn a_restart_while_online_reconnects() {
    let (mut bot, controller) = Bot::online(Setup::new(running())).await;

    bot.send(BotCommand::Restart).await;

    assert!(controller.is_torn_down());
    assert_eq!(
        bot.states(),
        [
            BotState::Stopping { restart: false },
            BotState::Stopping { restart: true },
            awaiting(n(1)),
            connecting(n(1)),
        ]
    );
    assert_eq!(bot.connects(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_restart_does_nothing_while_the_bot_should_be_stopped() {
    let mut bot = Setup::new(stopped()).start().await;

    bot.send(BotCommand::Restart).await;

    assert_eq!(bot.state(), BotState::Stopped);
    assert_eq!(bot.states(), []);
    assert_eq!(bot.connects(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_restart_leaves_a_paused_bot_paused() {
    let (mut bot, controller) = Bot::online(Setup::new(running())).await;
    emit(&controller, duplicate_login());
    settle().await;
    let _ = bot.kinds();

    bot.send(BotCommand::Restart).await;

    assert_eq!(bot.state(), DUPLICATE_LOGIN);
    assert_eq!(bot.states(), []);
    assert_eq!(bot.connects(), 1);
}

#[tokio::test(start_paused = true)]
async fn resume_and_reset_connect_again() {
    let (bot, controller) = Bot::online(Setup::new(running())).await;
    emit(&controller, duplicate_login());
    settle().await;

    bot.send(BotCommand::Resume).await;
    assert_eq!(bot.state(), connecting(n(1)));
    let second = bot.session(1).await;
    emit(&second, kick("multiplayer.disconnect.not_whitelisted"));
    settle().await;
    assert!(matches!(bot.state(), BotState::Failed { .. }));
    bot.send(BotCommand::Reset).await;

    assert_eq!(bot.state(), connecting(n(1)));
    assert_eq!(bot.connects(), 3);
}

#[tokio::test(start_paused = true)]
async fn a_bot_that_should_be_stopped_starts_when_its_spec_says_so() {
    let mut bot = Setup::new(stopped()).start().await;
    assert_eq!(bot.connects(), 0);

    bot.update(running()).await;

    assert_eq!(bot.states(), [awaiting(n(1)), connecting(n(1))]);
    assert_eq!(bot.connects(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_spec_for_another_bot_or_account_is_ignored() {
    let levels = Levels::default();
    let _guard = tracing::subscriber::set_default(levels.clone());
    let (mut bot, controller) = Bot::online(Setup::new(running())).await;
    let mut other_bot = stopped();
    other_bot.id = "018bcfe5-6800-7cdc-8dcd-cdcdcdcdcdcd".parse().unwrap();
    let mut other_account = stopped();
    other_account.account = BotAccount::Offline("AfkBot2".parse().unwrap());

    bot.update(other_bot).await;
    bot.update(other_account).await;

    assert!(matches!(bot.state(), BotState::Online { .. }));
    assert!(!controller.is_torn_down());
    assert_eq!(bot.states(), []);
    assert_eq!(levels.of("was ignored"), [Level::ERROR, Level::ERROR]);
}

#[tokio::test(start_paused = true)]
async fn a_full_inbox_refuses_a_command_and_an_ended_actor_closes_it() {
    let config = RuntimeConfig {
        actor_inbox: NonZeroUsize::MIN,
        ..RuntimeConfig::default()
    };
    let mut setup = Setup::new(stopped());
    setup.config = config;
    let bot = setup.start().await;
    let inbox = bot.inbox().clone();
    bot.cancel.cancel();
    // Ending takes a turn of the scheduler; until then the inbox still holds
    // one command.
    assert_eq!(inbox.try_send(BotCommand::Restart), Ok(()));
    assert_eq!(inbox.try_send(BotCommand::Restart), Err(InboxError::Full));

    assert_eq!(bot.exit().await, ActorExit::Stopped);

    assert_eq!(inbox.try_send(BotCommand::Restart), Err(InboxError::Closed));
}

// --- Ending ---

#[tokio::test(start_paused = true)]
async fn cancellation_stops_the_bot_and_ends_the_actor() {
    let (mut bot, controller) = Bot::online(Setup::new(running())).await;

    bot.cancel.cancel();
    settle().await;

    assert!(controller.is_torn_down());
    assert_eq!(
        bot.states(),
        [BotState::Stopping { restart: false }, BotState::Stopped]
    );
    assert_eq!(bot.exit().await, ActorExit::Stopped);
}

#[tokio::test(start_paused = true)]
async fn a_dropped_inbox_stops_the_bot_and_ends_the_actor() {
    let (mut bot, controller) = Bot::online(Setup::new(running())).await;

    bot.inbox = None;
    settle().await;

    assert!(controller.is_torn_down());
    assert_eq!(bot.state(), BotState::Stopped);
    assert_eq!(bot.exit().await, ActorExit::Stopped);
}

#[tokio::test(start_paused = true)]
async fn the_actor_waits_for_a_slow_teardown_before_it_ends() {
    let (bot, controller) = Bot::online(Setup::new(running())).await;
    controller.delay_disconnect(secs(20));

    bot.cancel.cancel();
    settle().await;
    advance(ms(19_999)).await;
    assert!(!bot.task.is_finished());
    advance(ms(1)).await;

    assert_eq!(bot.state(), BotState::Stopped);
    assert_eq!(bot.exit().await, ActorExit::Stopped);
}

// --- A crashed task ---

/// Where a [`PanickySession`] panics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PanicOn {
    /// When it's asked to jump, which the mode runner does.
    Jump,
    /// In its teardown.
    Disconnect,
}

/// A connector whose sessions panic at one call: a stand-in for a bug in a
/// `SessionHandle`. The fakes stay panic-free (ADR-0013).
#[derive(Debug, Clone)]
struct PanickyConnector(FakeConnector, PanicOn);

#[derive(Debug, Clone)]
struct PanickySession(FakeSession, PanicOn);

impl SessionHandle for PanickySession {
    fn perform(&self, action: GameAction) -> impl Future<Output = Result<(), SessionError>> + Send {
        let session = self.0.clone();
        let jump_panics = self.1 == PanicOn::Jump;
        async move {
            assert!(
                !(jump_panics && action == GameAction::Jump),
                "a test panic in perform"
            );
            session.perform(action).await
        }
    }

    fn send_chat(
        &self,
        message: ChatMessage,
    ) -> impl Future<Output = Result<(), SessionError>> + Send {
        self.0.send_chat(message)
    }

    fn respawn(&self) -> impl Future<Output = Result<(), SessionError>> + Send {
        self.0.respawn()
    }

    fn disconnect(&self) -> impl Future<Output = ()> + Send {
        let session = self.0.clone();
        let panics = self.1 == PanicOn::Disconnect;
        async move {
            session.disconnect().await;
            assert!(!panics, "a test panic in disconnect");
        }
    }

    fn liveness(&self) -> Liveness {
        self.0.liveness()
    }
}

impl MinecraftConnector for PanickyConnector {
    type Session = PanickySession;
    type Events = FakeEvents;

    fn connect(
        &self,
        params: ConnectParams,
    ) -> impl Future<Output = Result<(Self::Session, Self::Events), ConnectError>> + Send {
        let connector = self.0.clone();
        let panic_on = self.1;
        async move {
            let (session, events) = connector.connect(params).await?;
            Ok((PanickySession(session, panic_on), events))
        }
    }
}

/// Starts a bot whose sessions panic at `panic_on`, with `spec`.
async fn panicky(spec: BotSpec, panic_on: PanicOn) -> Bot {
    let setup = Setup::new(spec);
    let connector = PanickyConnector(setup.connector.clone(), panic_on);
    setup.start_with(connector).await
}

#[tokio::test(start_paused = true)]
async fn a_crashed_mode_runner_ends_the_actor_without_a_state_change() {
    let levels = Levels::default();
    let _guard = tracing::subscriber::set_default(levels.clone());
    let mut spec = running();
    spec.mode = mode(vec![at_start(Action::Jump)]);
    let mut bot = panicky(spec, PanicOn::Jump).await;
    let controller = bot.session(0).await;

    emit(&controller, SessionEvent::Joined);
    settle().await;

    let since = bot.clock.now();
    assert_eq!(bot.state(), online(since, n(1)), "the last state stays");
    assert_eq!(
        bot.states(),
        [awaiting(n(1)), connecting(n(1)), online(since, n(1))]
    );
    assert!(controller.is_torn_down());
    assert_eq!(levels.of("a task of the bot crashed"), [Level::ERROR]);
    assert_eq!(
        bot.exit().await,
        ActorExit::TaskCrashed {
            task: CrashedTask::ModeRunner
        }
    );
}

#[tokio::test(start_paused = true)]
async fn a_crashed_teardown_ends_the_actor_without_a_state_change() {
    let bot = panicky(running(), PanicOn::Disconnect).await;
    let controller = bot.session(0).await;
    emit(&controller, SessionEvent::Joined);
    settle().await;

    bot.update(stopped()).await;

    assert_eq!(bot.state(), BotState::Stopping { restart: false });
    assert!(controller.is_torn_down());
    assert_eq!(
        bot.exit().await,
        ActorExit::TaskCrashed {
            task: CrashedTask::Teardown
        }
    );
}

#[tokio::test(start_paused = true)]
async fn a_teardown_that_crashes_while_the_actor_stops_is_reported() {
    let levels = Levels::default();
    let _guard = tracing::subscriber::set_default(levels.clone());
    let bot = panicky(running(), PanicOn::Disconnect).await;
    let controller = bot.session(0).await;
    emit(&controller, SessionEvent::Joined);
    settle().await;

    bot.cancel.cancel();
    settle().await;

    assert_eq!(levels.of("crashed while it stopped"), [Level::ERROR]);
    assert_eq!(
        bot.exit().await,
        ActorExit::TaskCrashed {
            task: CrashedTask::Teardown
        }
    );
}

// --- Logs and spans ---

#[tokio::test(start_paused = true)]
async fn the_actor_and_its_tasks_log_in_the_bots_span() {
    let levels = Levels::default();
    let _guard = tracing::subscriber::set_default(levels.clone());
    let mut spec = running();
    spec.mode = mode(vec![at_start(Action::Jump)]);
    let setup = Setup::new(spec);
    let bot = setup.start().await;
    let controller = bot.session(0).await;
    controller.fail_actions(SessionError::TimedOut);
    controller.fail_chat(SessionError::TimedOut);

    emit(&controller, SessionEvent::Joined);
    settle().await;
    bot.chat("hi").await.unwrap();
    settle().await;

    let span = format!("bot bot_id={BOT}");
    for needle in [
        "the bot's state changed",
        "a mode action failed",
        "user chat wasn't sent",
    ] {
        let spans = levels.spans_of(needle);
        assert!(!spans.is_empty(), "{needle}: nothing logged");
        assert!(
            spans.iter().all(|logged| logged.contains(&span)),
            "{needle}: {spans:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_disconnect_never_warns_about_mode_chat_in_flight() {
    let levels = Levels::default();
    let _guard = tracing::subscriber::set_default(levels.clone());
    let mut spec = running();
    spec.mode = mode(vec![at_start(Action::SendChat {
        message: message("hello"),
    })]);
    let bot = Setup::new(spec).start().await;
    let controller = bot.session(0).await;

    emit(&controller, SessionEvent::Joined);
    emit(&controller, transient_kick());
    settle().await;

    assert_eq!(bot.state(), backoff(n(1)));
    assert!(!levels.of("mode chat").contains(&Level::WARN));
    assert_eq!(levels.of("the session ended"), [Level::WARN]);
}

// --- Ready session events go first ---

#[tokio::test(start_paused = true)]
async fn a_restart_applies_a_queued_duplicate_login_first_and_stays_paused() {
    let (mut bot, controller) = Bot::online(Setup::new(running())).await;
    for i in 0..64 {
        emit(
            &controller,
            SessionEvent::Chat(IncomingChat::system(&format!("chat {i}"))),
        );
    }
    emit(&controller, duplicate_login());

    bot.send(BotCommand::Restart).await;

    assert_eq!(bot.state(), DUPLICATE_LOGIN);
    assert_eq!(bot.connects(), 1);
    let kinds = bot.kinds();
    let chats = kinds
        .iter()
        .filter(|kind| matches!(kind, FleetEventKind::ChatReceived(_)))
        .count();
    assert_eq!(chats, 64);
}

#[tokio::test(start_paused = true)]
async fn stopping_applies_a_queued_duplicate_login_first() {
    let (bot, controller) = Bot::online(Setup::new(running())).await;
    emit(&controller, duplicate_login());

    bot.update(stopped()).await;

    assert_eq!(bot.state(), DUPLICATE_LOGIN, "Paused is sticky");
}

#[tokio::test(start_paused = true)]
async fn a_server_change_applies_a_queued_duplicate_login_first() {
    let (bot, controller) = Bot::online(Setup::new(running())).await;
    emit(&controller, duplicate_login());
    let mut moved = running();
    moved.server = "example.com".try_into().unwrap();

    bot.update(moved).await;

    assert_eq!(bot.state(), DUPLICATE_LOGIN);
    assert_eq!(bot.connects(), 1);
}

#[tokio::test(start_paused = true)]
async fn cancellation_applies_a_queued_duplicate_login_first() {
    let (bot, controller) = Bot::online(Setup::new(running())).await;
    emit(&controller, duplicate_login());

    bot.cancel.cancel();
    settle().await;

    assert_eq!(bot.state(), DUPLICATE_LOGIN);
    assert_eq!(bot.exit().await, ActorExit::Stopped);
}
