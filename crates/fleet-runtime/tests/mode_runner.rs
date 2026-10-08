//! Component tests for the mode runner (Plan.md P4.4; ADR-0013), against
//! fleet-testkit's fake session and a real chat queue, on paused time.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::fmt::{self, Write as _};
use core::time::Duration;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use fleet_core::chat::ChatMessage;
use fleet_core::disconnect::DisconnectReason;
use fleet_core::mc::{
    ConnectParams, MinecraftConnector, SessionCredentials, SessionError, SessionEvent,
};
use fleet_core::mode::{Action, GameAction, HotbarSlot, ModeDefinition, ModeDraft, Schedule, Step};
use fleet_core::time;
use fleet_runtime::{
    ChatBucket, ChatQueue, ChatTickets, FleetEvent, FleetEventKind, ModeRunner, RuntimeClock,
    RuntimeConfig,
};
use fleet_testkit::mc::{EmitOutcome, FakeConnector, Performed, SessionController};
use rand::SeedableRng;
use rand::rngs::StdRng;
use tokio::sync::{broadcast, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata};

// --- Modes ---

const fn ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

fn step(action: Action, schedule: Schedule) -> Step {
    Step {
        action,
        schedule,
        probability: 100,
    }
}

fn at_start(action: Action) -> Step {
    step(action, Schedule::AtStart)
}

const fn every(interval: Duration, jitter: Duration) -> Schedule {
    Schedule::Every { interval, jitter }
}

fn mode(steps: Vec<Step>) -> ModeDefinition {
    ModeDraft { steps }.validate().unwrap()
}

fn slot(n: u8) -> HotbarSlot {
    HotbarSlot::try_from(n).unwrap()
}

fn message(text: &str) -> ChatMessage {
    text.parse().unwrap()
}

fn chat_step(text: &str) -> Step {
    step(
        Action::SendChat {
            message: message(text),
        },
        every(ms(30_000), Duration::ZERO),
    )
}

const fn action(action: GameAction) -> Performed {
    Performed::Action(action)
}

const JUMP: Performed = action(GameAction::Jump);

// --- The runner and its session ---

/// The runtime clock starts here in every test.
fn anchor() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).unwrap()
}

fn params() -> ConnectParams {
    ConnectParams {
        bot_id: "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap(),
        server: "localhost".try_into().unwrap(),
        credentials: SessionCredentials::Offline {
            username: "AfkBot1".try_into().unwrap(),
        },
        connect_timeout: Duration::from_secs(30),
    }
}

/// Lets every spawned task run until it waits.
async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

/// Moves paused time forward by `ms` and lets the tasks run. One call wakes
/// the runner at most once, and a late tick doesn't catch up (P2.8), so a
/// test that wants every run steps through them one at a time.
async fn advance(ms: u64) {
    tokio::time::advance(Duration::from_millis(ms)).await;
    settle().await;
}

/// Moves paused time forward in `times` steps of `ms` each.
async fn advance_steps(times: usize, ms: u64) {
    for _ in 0..times {
        advance(ms).await;
    }
}

/// A mode runner in a session that has joined, with the bot's chat queue
/// open, wired as the actor will wire them.
struct Runner {
    controller: SessionController,
    queue: ChatQueue,
    mode: watch::Sender<ModeDefinition>,
    cancel: CancellationToken,
    events: broadcast::Receiver<FleetEvent>,
    task: JoinHandle<()>,
    _delivery: JoinSet<()>,
}

impl Runner {
    /// Starts `definition` with seed 1 and lets the at-start steps run.
    async fn start(definition: ModeDefinition) -> Self {
        Self::start_seeded(definition, 1).await
    }

    async fn start_seeded(definition: ModeDefinition, seed: u64) -> Self {
        let connector = FakeConnector::new();
        let (session, _events) = connector.connect(params()).await.unwrap();
        let controller = connector.session(0).await;
        assert_eq!(controller.emit(SessionEvent::Joined), EmitOutcome::Queued);

        let config = RuntimeConfig::default();
        let bucket = ChatBucket::new(config.chat_interval, config.chat_burst).unwrap();
        let mut queue = ChatQueue::new(
            params().bot_id,
            config.chat_queue,
            bucket,
            ChatTickets::new(),
        );
        let (events, subscriber) = broadcast::channel(64);
        let clock = RuntimeClock::new(anchor());
        let (delivery, mode_chat) =
            queue.open(session.clone(), events, clock, CancellationToken::new());
        let mut delivery_task = JoinSet::new();
        delivery_task.spawn(delivery.run());

        let (mode, watched) = watch::channel(definition);
        let cancel = CancellationToken::new();
        let runner = ModeRunner::new(
            session,
            mode_chat,
            clock,
            StdRng::seed_from_u64(seed),
            watched,
        );
        let task = tokio::spawn(runner.run(cancel.clone()));
        settle().await;

        Self {
            controller,
            queue,
            mode,
            cancel,
            events: subscriber,
            task,
            _delivery: delivery_task,
        }
    }

    /// What the runner did in the session so far.
    fn log(&self) -> Vec<Performed> {
        self.controller.log()
    }

    /// Changes the mode, as the actor does on a spec update.
    async fn change_mode(&self, definition: ModeDefinition) {
        self.mode.send_replace(definition);
        settle().await;
    }
}

// --- Log levels ---

/// Records the level and the fields of every event, so a test can check
/// what was logged at which level. It's installed for the test's thread; the
/// current-thread runtime polls the spawned tasks there too.
#[derive(Debug, Clone, Default)]
struct Levels {
    events: Arc<Mutex<Vec<(Level, String)>>>,
}

impl Levels {
    /// The levels of the events whose fields contain `needle`, in order.
    fn of(&self, needle: &str) -> Vec<Level> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, text)| text.contains(needle))
            .map(|(level, _)| *level)
            .collect()
    }

    /// Whether any event was logged at `warn` or `error`.
    fn any_warning(&self) -> bool {
        self.events
            .lock()
            .unwrap()
            .iter()
            .any(|(level, _)| matches!(*level, Level::WARN | Level::ERROR))
    }
}

struct Fields(String);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let _ = write!(self.0, "{}={value:?} ", field.name());
    }
}

impl tracing::Subscriber for Levels {
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
        self.events
            .lock()
            .unwrap()
            .push((*event.metadata().level(), fields.0));
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

// --- Tests ---

#[tokio::test(start_paused = true)]
async fn the_at_start_steps_run_at_once_in_step_order() {
    let runner = Runner::start(mode(vec![
        at_start(Action::SelectHotbarSlot { slot: slot(2) }),
        at_start(Action::Sneak { on: true }),
        at_start(Action::HoldUse { on: true }),
    ]))
    .await;

    assert_eq!(
        runner.log(),
        [
            action(GameAction::SelectHotbarSlot { slot: slot(2) }),
            action(GameAction::Sneak { on: true }),
            action(GameAction::HoldUse { on: true }),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_repeating_step_runs_exactly_on_its_schedule() {
    let runner = Runner::start(mode(vec![step(
        Action::Jump,
        every(ms(1_000), Duration::ZERO),
    )]))
    .await;
    assert_eq!(runner.log(), []);

    advance(999).await;
    assert_eq!(runner.log(), []);
    advance(1).await;
    assert_eq!(runner.log(), [JUMP]);
    advance(999).await;
    assert_eq!(runner.log(), [JUMP]);
    advance(1).await;
    assert_eq!(runner.log(), [JUMP, JUMP]);
    advance_steps(3, 1_000).await;
    assert_eq!(runner.log(), [JUMP; 5]);
}

#[tokio::test(start_paused = true)]
async fn a_jittered_step_repeats_within_its_interval_and_jitter() {
    let runner =
        Runner::start_seeded(mode(vec![step(Action::Jump, every(ms(1_000), ms(500)))]), 7).await;

    // Step 1 ms at a time and note when each jump happens.
    let mut jumps = Vec::new();
    for now in 1..=20_000_u64 {
        advance(1).await;
        if runner.log().len() > jumps.len() {
            jumps.push(now);
        }
    }

    // Each gap is 1–1.5 s; tokio's timer rounds a deadline up to the next
    // millisecond.
    let gaps: Vec<_> = core::iter::once(0)
        .chain(jumps.iter().copied())
        .zip(jumps.iter().copied())
        .map(|(before, at)| at - before)
        .collect();
    assert!(gaps.len() >= 13, "{gaps:?}");
    assert!(
        gaps.iter().all(|gap| (1_000..=1_501).contains(gap)),
        "{gaps:?}"
    );
    assert!(
        gaps.iter().any(|gap| *gap != gaps[0]),
        "no jitter: {gaps:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_failed_action_is_skipped_and_the_rest_of_the_mode_runs() {
    let runner = Runner::start(mode(vec![
        step(Action::Jump, every(ms(30_000), Duration::ZERO)),
        chat_step("/spawn"),
    ]))
    .await;
    runner.controller.fail_actions(SessionError::TimedOut);

    advance(30_000).await;
    assert_eq!(
        runner.log(),
        [Performed::Chat(message("/spawn"))],
        "the jump failed, the chat after it still went out"
    );

    runner.controller.succeed_actions();
    advance(30_000).await;
    assert_eq!(
        runner.log(),
        [
            Performed::Chat(message("/spawn")),
            JUMP,
            Performed::Chat(message("/spawn"))
        ]
    );
    assert!(!runner.task.is_finished());
}

#[tokio::test(start_paused = true)]
async fn the_first_failure_of_each_kind_warns_and_the_rest_are_debug() {
    let levels = Levels::default();
    let _guard = tracing::subscriber::set_default(levels.clone());
    let runner = Runner::start(mode(vec![step(
        Action::Jump,
        every(ms(1_000), Duration::ZERO),
    )]))
    .await;

    runner.controller.fail_actions(SessionError::TimedOut);
    advance_steps(2, 1_000).await;
    runner.controller.fail_actions(SessionError::NotInWorld);
    advance_steps(2, 1_000).await;

    assert_eq!(
        levels.of("didn't answer in time"),
        [Level::WARN, Level::DEBUG]
    );
    assert_eq!(levels.of("isn't in a world"), [Level::WARN, Level::DEBUG]);
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_runner_stops_at_once() {
    let runner = Runner::start(mode(vec![step(
        Action::Jump,
        every(ms(1_000), Duration::ZERO),
    )]))
    .await;
    advance_steps(2, 1_000).await;

    runner.cancel.cancel();
    settle().await;
    advance(10_000).await;

    assert!(runner.task.is_finished());
    assert_eq!(runner.log(), [JUMP, JUMP]);
}

#[tokio::test(start_paused = true)]
async fn the_runner_ends_by_itself_once_the_session_has_ended() {
    let runner = Runner::start(mode(vec![step(
        Action::Jump,
        every(ms(1_000), Duration::ZERO),
    )]))
    .await;
    advance(1_000).await;

    let ended = SessionEvent::Disconnected(DisconnectReason::ConnectionClosed);
    assert_eq!(runner.controller.emit(ended), EmitOutcome::Queued);
    advance(1_000).await;

    assert!(runner.task.is_finished());
    assert_eq!(runner.log(), [JUMP]);
}

#[tokio::test(start_paused = true)]
async fn the_runner_ends_when_its_owner_drops_the_mode() {
    let Runner {
        mode,
        task,
        _delivery,
        ..
    } = Runner::start(mode(vec![step(
        Action::Jump,
        every(ms(1_000), Duration::ZERO),
    )]))
    .await;

    drop(mode);
    settle().await;

    assert!(task.is_finished());
}

#[tokio::test(start_paused = true)]
async fn a_mode_change_lets_go_then_starts_the_new_mode_mid_sleep() {
    let runner = Runner::start(mode(vec![
        at_start(Action::SelectHotbarSlot { slot: slot(3) }),
        at_start(Action::HoldUse { on: true }),
        step(Action::Jump, every(ms(1_000), Duration::ZERO)),
    ]))
    .await;
    advance(500).await;

    runner
        .change_mode(mode(vec![
            at_start(Action::Sneak { on: true }),
            step(Action::SwingArm, every(ms(1_000), Duration::ZERO)),
        ]))
        .await;
    let changed = vec![
        action(GameAction::SelectHotbarSlot { slot: slot(3) }),
        action(GameAction::HoldUse { on: true }),
        action(GameAction::HoldUse { on: false }),
        action(GameAction::Sneak { on: false }),
        action(GameAction::Sneak { on: true }),
    ];
    assert_eq!(runner.log(), changed, "let go first; the slot stays");

    // The old mode's jump would be due at 1 s; the new swing is due 1 s after
    // the change, at 1.5 s.
    advance(999).await;
    assert_eq!(runner.log(), changed);
    advance(1).await;
    let mut swung = changed;
    swung.push(action(GameAction::SwingArm));
    assert_eq!(runner.log(), swung);
}

#[tokio::test(start_paused = true)]
async fn an_equal_mode_changes_nothing() {
    let definition = mode(vec![step(Action::Jump, every(ms(1_000), Duration::ZERO))]);
    let runner = Runner::start(definition.clone()).await;
    advance(500).await;

    runner.change_mode(definition).await;
    assert_eq!(runner.log(), [], "nothing let go, nothing started");

    advance(500).await;
    assert_eq!(runner.log(), [JUMP], "still on the first schedule");
}

#[tokio::test(start_paused = true)]
async fn mode_chat_goes_through_the_chat_queue() {
    let mut runner = Runner::start(mode(vec![chat_step("/spawn")])).await;

    advance(30_000).await;

    assert_eq!(runner.log(), [Performed::Chat(message("/spawn"))]);
    assert_eq!(
        runner.events.try_recv().unwrap(),
        FleetEvent {
            bot_id: params().bot_id,
            at: time::add(anchor(), ms(30_000)),
            kind: FleetEventKind::ModeChatSent {
                message: message("/spawn")
            },
        }
    );
}

#[tokio::test(start_paused = true)]
async fn rate_limited_mode_chat_is_skipped_and_runs_again_on_schedule() {
    let levels = Levels::default();
    let _guard = tracing::subscriber::set_default(levels.clone());
    let runner = Runner::start(mode(vec![chat_step("/spawn")])).await;
    let user_chat = || {
        for _ in 0..3 {
            runner.queue.send(message("hello")).unwrap();
        }
    };

    // User chat empties the bucket just before each of the first two runs.
    advance(29_000).await;
    user_chat();
    advance(1_000).await;
    advance(29_000).await;
    user_chat();
    advance(1_000).await;
    assert_eq!(runner.log(), vec![Performed::Chat(message("hello")); 6]);

    advance(30_000).await;
    assert_eq!(
        runner.log().last(),
        Some(&Performed::Chat(message("/spawn")))
    );
    assert_eq!(levels.of("rate limit"), [Level::WARN, Level::DEBUG]);
}

#[tokio::test(start_paused = true)]
async fn mode_chat_on_a_closed_queue_is_not_online_and_logs_below_warn() {
    let levels = Levels::default();
    let _guard = tracing::subscriber::set_default(levels.clone());
    let mut runner = Runner::start(mode(vec![chat_step("/spawn")])).await;

    advance(10_000).await;
    runner.queue.close();
    settle().await;
    advance(20_000).await;

    assert_eq!(runner.log(), [], "nothing was sent");
    assert!(!runner.task.is_finished(), "the runner keeps going");
    assert_eq!(levels.of("isn't online"), [Level::DEBUG]);
    assert!(!levels.any_warning(), "a closed queue never warns");
}
