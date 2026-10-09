//! What the fleet's test files share: values, paused-time helpers, and the
//! fleet under test with the test's handles on it.

use core::num::{NonZeroU32, NonZeroUsize};
use core::time::Duration;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::bot::{BotAccount, BotSpec, BotState, DesiredRunState, FailReason, PauseReason};
use fleet_core::chat::ChatMessage;
use fleet_core::disconnect::{ConflictKind, ConflictTexts, DisconnectReason};
use fleet_core::id::BotId;
use fleet_core::mc::SessionEvent;
use fleet_core::mode::{Action, ModeDefinition, ModeDraft, Schedule, Step};
use fleet_core::resilience::{CircuitPolicy, RetryPolicy};
use fleet_runtime::{Fleet, FleetEvent, FleetEventKind, FleetParts, RuntimeConfig};
use fleet_testkit::mc::{EmitOutcome, FakeConnector, FakeCredentials, SessionController};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::panicky::PanickyConnector;

// --- Values ---

pub(crate) const BOT: &str = "018bcfe5-6800-7bab-abab-abababababab";
pub(crate) const OTHER: &str = "018bcfe5-6800-7cdc-8dcd-cdcdcdcdcdcd";

pub(crate) const fn ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

pub(crate) const fn secs(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

pub(crate) fn n(attempt: u32) -> NonZeroU32 {
    NonZeroU32::new(attempt).unwrap()
}

pub(crate) fn id(bot: &str) -> BotId {
    bot.parse().unwrap()
}

/// The runtime clock starts here in every test.
pub(crate) fn anchor() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).unwrap()
}

pub(crate) fn message(text: &str) -> ChatMessage {
    text.parse().unwrap()
}

pub(crate) fn mode(steps: Vec<Step>) -> ModeDefinition {
    ModeDraft { steps }.validate().unwrap()
}

pub(crate) fn spec(bot: &str, name: &str, desired: DesiredRunState) -> BotSpec {
    BotSpec {
        id: id(bot),
        account: BotAccount::Offline(name.parse().unwrap()),
        server: "localhost".try_into().unwrap(),
        mode: mode(Vec::new()),
        desired,
        conflict_texts: ConflictTexts::default(),
    }
}

pub(crate) fn running(bot: &str, name: &str) -> BotSpec {
    spec(bot, name, DesiredRunState::Running)
}

pub(crate) fn stopped(bot: &str, name: &str) -> BotSpec {
    spec(bot, name, DesiredRunState::Stopped)
}

/// A bot that should run, with a mode that jumps when it joins, so a
/// session that panics on a jump crashes the mode runner.
pub(crate) fn jumping(bot: &str, name: &str) -> BotSpec {
    let mut spec = running(bot, name);
    spec.mode = mode(vec![Step {
        action: Action::Jump,
        schedule: Schedule::AtStart,
        probability: 100,
    }]);
    spec
}

/// The defaults of Appendix A: 5 s to 300 s, stable after 300 s.
pub(crate) fn policy() -> RetryPolicy {
    RetryPolicy::try_new(secs(5), secs(300), secs(300)).unwrap()
}

/// `threshold` failures within 600 s open the breaker for 900 s.
pub(crate) fn circuit(threshold: usize) -> CircuitPolicy {
    CircuitPolicy::try_new(NonZeroUsize::new(threshold).unwrap(), secs(600), secs(900)).unwrap()
}

pub(crate) const DUPLICATE_LOGIN: PauseReason = PauseReason::Conflict {
    kind: ConflictKind::DuplicateLogin,
};

pub(crate) const PAUSED: BotState = BotState::Paused {
    reason: DUPLICATE_LOGIN,
};

pub(crate) const CRASH_LOOP: BotState = BotState::Failed {
    reason: FailReason::CrashLoop,
};

pub(crate) const fn awaiting(attempt: NonZeroU32) -> BotState {
    BotState::AwaitingSession {
        attempt,
        fresh: false,
    }
}

pub(crate) const fn connecting(attempt: NonZeroU32) -> BotState {
    BotState::Connecting {
        attempt,
        auth_retried: false,
    }
}

pub(crate) const fn backoff(attempt: NonZeroU32) -> BotState {
    BotState::Backoff { attempt }
}

pub(crate) fn kick(key: &str) -> SessionEvent {
    SessionEvent::Disconnected(DisconnectReason::kicked(Some(key), "Kicked"))
}

pub(crate) fn emit(controller: &SessionController, event: SessionEvent) {
    assert_eq!(controller.emit(event), EmitOutcome::Queued);
}

// --- Time ---

/// Lets every spawned task run until it waits.
pub(crate) async fn settle() {
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}

/// Moves paused time forward by `duration` and lets the tasks run.
pub(crate) async fn advance(duration: Duration) {
    tokio::time::advance(duration).await;
    settle().await;
}

/// Steps paused time forward one second at a time, so each timer fires at
/// its own time and the next one is set from there.
pub(crate) async fn step_seconds(seconds: u64) {
    for _ in 0..seconds {
        advance(secs(1)).await;
    }
}

// --- The fleet under test ---

/// What a test sets up before the fleet starts.
pub(crate) struct Setup {
    pub(crate) config: RuntimeConfig,
    pub(crate) circuit: CircuitPolicy,
    pub(crate) fake: FakeConnector,
    pub(crate) credentials: FakeCredentials,
}

impl Setup {
    pub(crate) fn new() -> Self {
        Self {
            config: RuntimeConfig::default(),
            circuit: circuit(8),
            fake: FakeConnector::new(),
            credentials: FakeCredentials::new(),
        }
    }

    pub(crate) fn parts(
        &self,
        connector: PanickyConnector,
    ) -> FleetParts<PanickyConnector, FakeCredentials> {
        FleetParts {
            connector: Arc::new(connector),
            credentials: Arc::new(self.credentials.clone()),
            retry: policy(),
            circuit: self.circuit,
            config: self.config,
            anchor: anchor(),
            seed: 1,
        }
    }

    /// Builds the fleet and runs its supervisor.
    pub(crate) async fn start(self) -> Harness {
        let connector = PanickyConnector::new(self.fake.clone());
        let (fleet, supervisor) = Fleet::new(self.parts(connector.clone())).unwrap();
        let events = fleet.subscribe();
        let cancel = CancellationToken::new();
        let task = tokio::spawn(supervisor.run(cancel.clone()));
        settle().await;
        Harness {
            fleet,
            connector,
            fake: self.fake,
            credentials: self.credentials,
            events,
            cancel,
            task,
        }
    }
}

/// A running fleet and the test's handles on it.
pub(crate) struct Harness {
    pub(crate) fleet: Fleet,
    pub(crate) connector: PanickyConnector,
    pub(crate) fake: FakeConnector,
    pub(crate) credentials: FakeCredentials,
    pub(crate) events: broadcast::Receiver<FleetEvent>,
    pub(crate) cancel: CancellationToken,
    pub(crate) task: JoinHandle<()>,
}

impl Harness {
    pub(crate) async fn apply(&self, spec: BotSpec) {
        self.fleet.apply(spec, None).await.unwrap();
        settle().await;
    }

    pub(crate) async fn remove(&self, bot: &str) {
        self.fleet.remove(id(bot)).await.unwrap();
        settle().await;
    }

    pub(crate) async fn session(&self, index: usize) -> SessionController {
        tokio::time::timeout(secs(1), self.fake.session(index))
            .await
            .expect("the session should have started")
    }

    /// Runs a new bot and joins it in the fake's session `index`.
    pub(crate) async fn online(&self, bot: &str, name: &str, index: usize) -> SessionController {
        self.apply(running(bot, name)).await;
        let controller = self.session(index).await;
        emit(&controller, SessionEvent::Joined);
        settle().await;
        assert!(matches!(self.state(bot).await, BotState::Online { .. }));
        controller
    }

    pub(crate) async fn state(&self, bot: &str) -> BotState {
        self.fleet.snapshot(id(bot)).await.unwrap().state
    }

    /// Every event published since the last call, in order.
    pub(crate) fn drain(&mut self) -> Vec<FleetEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            events.push(event);
        }
        events
    }

    /// The events of `bot` published since the last call, in order. The
    /// other bots' events are dropped.
    pub(crate) fn kinds_of(&mut self, bot: &str) -> Vec<FleetEventKind> {
        self.drain()
            .into_iter()
            .filter(|event| event.bot_id == id(bot))
            .map(|event| event.kind)
            .collect()
    }

    /// The states `bot` went through since the last call, in order.
    pub(crate) fn states_of(&mut self, bot: &str) -> Vec<BotState> {
        self.kinds_of(bot)
            .into_iter()
            .filter_map(|kind| match kind {
                FleetEventKind::StateChanged(snapshot) => Some(snapshot.state),
                _ => None,
            })
            .collect()
    }

    /// Waits for the supervisor to end.
    pub(crate) async fn ended(self) {
        tokio::time::timeout(secs(60), self.task)
            .await
            .expect("the supervisor should have ended")
            .unwrap();
    }
}

/// Runs a bot whose every connect panics until its 6th crash within 10 min.
pub(crate) async fn crash_loop(harness: &mut Harness) {
    harness.connector.panic_always(true);
    harness.apply(running(BOT, "AfkBot1")).await;
    step_seconds(400).await;
    assert_eq!(harness.connector.attempts(), 6);
    assert_eq!(harness.state(BOT).await, CRASH_LOOP);
}
