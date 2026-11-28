//! A scripted mode in the fleet (Plan.md P15.16 to P15.18, P15.33;
//! ADR-0036, ADR-0037, ADR-0043): the script runner inside the bot's own
//! actor, against the fake session, on paused time.
//!
//! A script is compiled off the async workers, in `spawn_blocking`. tokio
//! doesn't move paused time while a blocking task runs, so these tests let
//! time pass by sleeping ([`pass`]), never by `advance`, which wouldn't
//! wait for the compiler.

use core::time::Duration;

use fleet_core::bot::{BotSpec, BotState, DesiredRunState};
use fleet_core::chat::{ChatFormats, IncomingChat, PlayerChatKind};
use fleet_core::mc::{Guards, LetGo, Observation, SessionError, SessionEvent};
use fleet_core::mode::{Action, GameAction, HotbarSlot, ModeDefinition, Schedule, Step};
use fleet_core::script::{
    BlockGuard, BlockPosition, Blocks, BotFacts, CaptureDraft, CaptureKind, DecimalMark, Hook,
    Identifier, Input, Key, LeashRadius, MessageDraft, MouseButton, PatternDraft, Position,
    Refusal, Script, ScriptDraft, Snapshot, ToolGuard, ValueDraft, ValueKind, When,
};
use fleet_runtime::{
    ChatError, ChatFailure, FleetEventKind, NotSent, ScriptEvent, ScriptLog, ScriptStatus,
};
use fleet_script::{Asked, Limit, Refused, ScriptErrorKind};
use fleet_testkit::mc::{Performed, SessionController};
use tracing::Level;

use crate::common::Levels;
use crate::harness::{
    BOT, Harness, OTHER, Setup, emit, id, kick, message, mode, n, policy, running, secs, settle,
};
use crate::recorder::{
    Recorder, SCRIPT_CHAT_DROPPED, SCRIPT_HOOKS, SCRIPT_LOG_DROPPED, SCRIPT_REFUSED,
    SCRIPTS_STOPPED,
};

const TRANSIENT: &str = "multiplayer.disconnect.server_shutdown";

const FORWARD: Input = Input::Key(Key::Forward);
const JUMP: Input = Input::Key(Key::Jump);
const LEFT: Input = Input::Mouse(MouseButton::Left);

const RELEASE_ALL: Performed = Performed::Action(GameAction::ReleaseAll);

const fn ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

/// Lets `length` of paused time pass, waiting for a script that's being
/// compiled first.
async fn pass(length: Duration) {
    tokio::time::sleep(length).await;
    settle().await;
}

// --- Scripts ---

/// A scripted mode's body with nothing declared.
fn draft(source: &str) -> ScriptDraft {
    ScriptDraft {
        api: 1,
        source: source.to_owned(),
        leash: 8,
        tool_guard: 2,
        block_guard: Vec::new(),
        messages: Vec::new(),
        patterns: Vec::new(),
        signs: Vec::new(),
    }
}

fn declared(key: &str, text: &str) -> MessageDraft {
    MessageDraft {
        key: key.to_owned(),
        text: text.to_owned(),
        values: Vec::new(),
    }
}

/// The balance example's declarations: `/balance`, `/pay AfkMain {amount}`,
/// plain chat `hello`, the reply `Balance: ${amount}` and a line that may
/// come at any time.
fn balance(source: &str) -> ScriptDraft {
    ScriptDraft {
        messages: vec![
            declared("balance", "/balance"),
            MessageDraft {
                key: "pay".to_owned(),
                text: "/pay AfkMain {amount}".to_owned(),
                values: vec![ValueDraft {
                    name: "amount".to_owned(),
                    kind: ValueKind::Decimal {
                        min: 1.0,
                        max: 1_000_000.0,
                        places: 2,
                    },
                }],
            },
            declared("hello", "Hello there"),
        ],
        patterns: vec![
            PatternDraft {
                key: "balance".to_owned(),
                when: When::Reply,
                text: "Balance: ${amount}".to_owned(),
                captures: vec![CaptureDraft {
                    name: "amount".to_owned(),
                    kind: CaptureKind::Amount {
                        min: 0.0,
                        max: 1_000_000_000.0,
                        decimal_mark: DecimalMark::Point,
                    },
                }],
            },
            PatternDraft {
                key: "restart".to_owned(),
                when: When::AnyTime,
                text: "Server restarting in {minutes} minutes".to_owned(),
                captures: vec![CaptureDraft {
                    name: "minutes".to_owned(),
                    kind: CaptureKind::Integer { min: 0, max: 60 },
                }],
            },
        ],
        ..draft(source)
    }
}

fn scripted_mode(draft: ScriptDraft) -> ModeDefinition {
    ModeDefinition::scripted(Script::try_from(draft).expect("a scripted mode's body"))
}

/// A bot that should run the script of `draft`.
fn scripted(draft: ScriptDraft) -> BotSpec {
    BotSpec {
        mode: scripted_mode(draft),
        ..running(BOT, "AfkBot1")
    }
}

// --- The session ---

/// What the fake session shows a bot that stands at the origin of the
/// overworld, where its leash is anchored.
pub(crate) fn seen() -> Observation {
    let place = Position::try_new(0.5, 64.0, 0.5).unwrap();
    let bot = BotFacts {
        health: 20.0,
        food: 20,
        position: place,
        yaw: 0.0,
        pitch: 0.0,
        dimension: Identifier::try_from("overworld").unwrap(),
        riding: None,
        held: None,
        selected: HotbarSlot::FIRST,
        looking_at: None,
    };
    let blocks = Blocks::read(BlockPosition { x: 0, y: 64, z: 0 }, |_| None);
    Observation {
        facts: Snapshot::new(bot, Vec::new(), blocks),
        anchor: Some(place),
        menu: fleet_core::mc::MenuView::default(),
    }
}

/// Joins the bot in the fake's session `index`, placed in its world, and
/// lets its script start.
async fn join(harness: &Harness, index: usize) -> SessionController {
    let controller = harness.session(index).await;
    controller.show(seen());
    emit(&controller, SessionEvent::Joined);
    pass(ms(1)).await;
    assert!(matches!(harness.state(BOT).await, BotState::Online { .. }));
    controller
}

/// A fleet whose one bot runs `draft`'s script, online in session 0.
async fn online(draft: ScriptDraft) -> (Harness, SessionController) {
    let harness = Setup::new().start().await;
    harness.apply(scripted(draft)).await;
    let controller = join(&harness, 0).await;
    (harness, controller)
}

/// Ends the session with a kick the bot comes back from, and joins it
/// again in the fake's session `index`.
async fn rejoin(
    harness: &Harness,
    controller: &SessionController,
    index: usize,
) -> SessionController {
    emit(controller, kick(TRANSIENT));
    // The session before didn't last: this is the `index`-th attempt.
    let attempt = u32::try_from(index).unwrap();
    pass(policy().bounds(n(attempt)).1).await;
    join(harness, index).await
}

/// Notes in which order a hook of a script and another task's turn were
/// logged: 0 for a timer's hook, 1 for the other task.
#[derive(Debug, Clone, Default)]
struct Order {
    seen: std::sync::Arc<std::sync::Mutex<Vec<usize>>>,
}

impl Order {
    /// What another task logs when it gets its turn.
    const OTHER: &'static str = "another task's turn";

    fn seen(&self) -> Vec<usize> {
        self.seen.lock().unwrap().clone()
    }
}

/// An event's fields as text.
struct Written(String);

impl tracing::field::Visit for Written {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn core::fmt::Debug) {
        use core::fmt::Write as _;
        let _ = write!(self.0, "{}={value:?} ", field.name());
    }
}

impl tracing::Subscriber for Order {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut written = Written(String::new());
        event.record(&mut written);
        let which = ["hook=\"on_timer\"", Self::OTHER]
            .iter()
            .position(|needle| written.0.contains(needle));
        if let Some(which) = which {
            self.seen.lock().unwrap().push(which);
        }
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

/// What the session was asked to do, without its snapshots.
fn done(controller: &SessionController) -> Vec<Performed> {
    controller
        .log()
        .into_iter()
        .filter(|performed| *performed != Performed::Observed)
        .collect()
}

/// The game actions the session performed, in order.
fn actions(controller: &SessionController) -> Vec<GameAction> {
    controller
        .log()
        .into_iter()
        .filter_map(|performed| match performed {
            Performed::Action(action) => Some(action),
            _ => None,
        })
        .collect()
}

/// How many snapshots the session took.
fn snapshots(controller: &SessionController) -> usize {
    controller
        .log()
        .iter()
        .filter(|performed| **performed == Performed::Observed)
        .count()
}

/// The bot's script events since the last call, in order.
fn script_events(harness: &mut Harness) -> Vec<ScriptEvent> {
    harness
        .kinds_of(BOT)
        .into_iter()
        .filter_map(|kind| match kind {
            FleetEventKind::Script(event) => Some(event),
            _ => None,
        })
        .collect()
}

/// The script's log since the last call, in order.
fn logged(harness: &mut Harness) -> Vec<ScriptLog> {
    script_events(harness)
        .into_iter()
        .filter_map(|event| match event {
            ScriptEvent::Log(entry) => Some(entry),
            ScriptEvent::Status(_) => None,
        })
        .collect()
}

/// The lines the script wrote itself since the last call.
fn lines(harness: &mut Harness) -> Vec<String> {
    logged(harness)
        .into_iter()
        .filter_map(|entry| match entry {
            ScriptLog::Line(line) => Some(line),
            _ => None,
        })
        .collect()
}

/// The script's statuses since the last call, in order.
fn statuses(harness: &mut Harness) -> Vec<ScriptStatus> {
    script_events(harness)
        .into_iter()
        .filter_map(|event| match event {
            ScriptEvent::Status(status) => Some(status),
            ScriptEvent::Log(_) => None,
        })
        .collect()
}

fn key(text: &str) -> fleet_core::script::Ident {
    fleet_core::script::Ident::try_from(text).unwrap()
}

fn system(text: &str) -> SessionEvent {
    SessionEvent::Chat(IncomingChat::system(text))
}

// --- The runner (Plan.md P15.16) ---

const HOLDS_FORWARD: &str = "fn on_join(first) {\n  keys.forward.hold();\n}";

#[tokio::test(start_paused = true)]
async fn a_join_sets_the_modes_guards_takes_a_snapshot_and_runs_on_join() {
    let script = ScriptDraft {
        leash: 3,
        tool_guard: 0,
        block_guard: vec!["cobblestone".to_owned()],
        ..draft(HOLDS_FORWARD)
    };

    let (mut harness, controller) = online(script).await;

    // The guards come before anything is pressed, and everything is let
    // go of before `on_join` presses.
    assert_eq!(
        controller.log(),
        [
            Performed::Guards(Guards {
                leash: LeashRadius::try_from(3).unwrap(),
                tool_guard: ToolGuard::OFF,
                block_guard: BlockGuard::try_from(vec!["cobblestone".to_owned()]).unwrap(),
            }),
            Performed::Observed,
            RELEASE_ALL,
            Performed::Action(GameAction::Press(FORWARD)),
        ]
    );
    assert_eq!(controller.held(), [FORWARD]);
    assert_eq!(statuses(&mut harness), [ScriptStatus::Running]);
}

#[tokio::test(start_paused = true)]
async fn on_join_waits_until_the_server_has_placed_the_bot() {
    let harness = Setup::new().start().await;
    harness.apply(scripted(draft(HOLDS_FORWARD))).await;
    let controller = harness.session(0).await;
    emit(&controller, SessionEvent::Joined);

    pass(secs(2)).await;
    let before = controller.log();
    controller.show(seen());
    pass(ms(100)).await;

    // The guards are set at once; the hook waits for the snapshot.
    assert_eq!(before, [Performed::Guards(Guards::default())]);
    assert_eq!(
        done(&controller),
        [
            Performed::Guards(Guards::default()),
            RELEASE_ALL,
            Performed::Action(GameAction::Press(FORWARD)),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn guards_the_session_doesnt_take_are_set_again_before_anything_is_pressed() {
    let harness = Setup::new().start().await;
    harness.apply(scripted(draft(HOLDS_FORWARD))).await;
    let controller = harness.session(0).await;
    controller.show(seen());
    controller.fail_actions(SessionError::QueueFull);
    emit(&controller, SessionEvent::Joined);

    pass(secs(1)).await;
    let while_refused = controller.log();
    controller.succeed_actions();
    pass(ms(100)).await;

    assert_eq!(while_refused, []);
    assert_eq!(
        done(&controller),
        [
            Performed::Guards(Guards::default()),
            RELEASE_ALL,
            Performed::Action(GameAction::Press(FORWARD)),
        ]
    );
}

/// A timer and a `hold_for` run on tokio's time: paused time moves them,
/// to the millisecond.
#[tokio::test(start_paused = true)]
async fn timers_and_hold_for_run_on_tokios_time() {
    let source = "fn on_join(first) {\n  keys.forward.hold_for(3000);\n  timer.every(1000, \"jump\");\n}\nfn on_timer(name) {\n  keys.jump.tap();\n}";
    let (_harness, controller) = online(draft(source)).await;
    let tap = GameAction::Tap(JUMP);
    let taps = |controller: &SessionController| {
        actions(controller)
            .iter()
            .filter(|action| **action == tap)
            .count()
    };
    // The join took the first millisecond.
    assert_eq!(taps(&controller), 0);

    pass(ms(998)).await;
    let just_before = taps(&controller);
    pass(ms(1)).await;
    let at_a_second = taps(&controller);
    pass(ms(1_999)).await;
    let just_before_the_release = actions(&controller);
    pass(ms(1)).await;

    assert_eq!((just_before, at_a_second), (0, 1));
    assert_eq!(
        just_before_the_release,
        [GameAction::ReleaseAll, GameAction::Press(FORWARD), tap, tap]
    );
    // At 3,000 ms: the hold's time is up, then the third timer.
    assert_eq!(
        actions(&controller).get(4..),
        Some([GameAction::Release(FORWARD), tap].as_slice())
    );
    assert_eq!(controller.held(), []);
}

/// Three timers are due at the same moment, and three other tasks of the
/// runtime are woken at that moment too. The runner runs one hook, then
/// gives its worker to whoever waits for one, before the script's next
/// hook: no script holds a worker for longer than one hook (ADR-0043).
#[tokio::test(start_paused = true)]
async fn the_tasks_that_wait_for_a_worker_get_it_between_two_hooks_that_are_due_at_once() {
    let order = Order::default();
    let _logs = tracing::subscriber::set_default(order.clone());
    let source = "fn on_join(first) {\n  timer.after(1000, \"a\");\n  timer.after(1000, \"b\");\n  timer.after(1000, \"c\");\n}\nfn on_timer(name) {\n  keys.jump.tap();\n}";
    let (_harness, _controller) = online(draft(source)).await;
    let mut others = tokio::task::JoinSet::new();
    for _ in 0..3 {
        others.spawn(async {
            tokio::time::sleep(ms(999)).await;
            tracing::debug!("{}", Order::OTHER);
        });
    }

    pass(secs(1)).await;
    while let Some(done) = others.join_next().await {
        done.unwrap();
    }

    // The runner's timer was set first, so its first hook runs first: with
    // the three others waiting behind it, as the test means to have it.
    // They all run before the script's second hook.
    assert_eq!(order.seen(), [0, 1, 1, 1, 0, 0]);
}

/// Four bots' timers are due at the same moment, and so is another task
/// of the runtime, which was woken last. A test's runtime has one worker,
/// so one hook runs at a time: the scripts whose turn hasn't come aren't
/// in the worker's queue, and the other task runs after one hook, not
/// after one of every script (ADR-0043).
#[tokio::test(start_paused = true)]
async fn what_else_is_due_waits_for_one_hook_not_for_one_of_every_script() {
    let order = Order::default();
    let _logs = tracing::subscriber::set_default(order.clone());
    let source = "fn on_join(first) {\n  timer.after(1000, \"a\");\n}\nfn on_timer(name) {\n  keys.jump.tap();\n}";
    let harness = Setup::new().start().await;
    let bots = [
        (BOT, "AfkBot1"),
        (OTHER, "AfkBot2"),
        ("018bcfe5-6800-7bab-abab-ababababab01", "AfkBot3"),
        ("018bcfe5-6800-7bab-abab-ababababab02", "AfkBot4"),
    ];
    for (bot, name) in bots {
        harness
            .apply(BotSpec {
                mode: scripted_mode(draft(source)),
                ..running(bot, name)
            })
            .await;
    }
    for index in 0..bots.len() {
        let session = harness.session(index).await;
        session.show(seen());
        emit(&session, SessionEvent::Joined);
    }
    // Every script joins in the same millisecond, so their timers are due
    // together.
    pass(ms(1)).await;
    let other = tokio::spawn(async {
        tokio::time::sleep(ms(999)).await;
        tracing::debug!("{}", Order::OTHER);
    });

    pass(secs(1)).await;
    other.await.unwrap();

    assert_eq!(order.seen(), [0, 1, 0, 0, 0]);
}

#[tokio::test(start_paused = true)]
async fn a_hook_sees_a_snapshot_taken_right_before_it() {
    let source = "fn on_join(first) {\n  timer.every(1000, \"look\");\n}\nfn on_timer(name) {\n  log.info(`${bot.health} ${bot.x}`);\n}";
    let (mut harness, controller) = online(draft(source)).await;
    assert_eq!(snapshots(&controller), 1);

    pass(secs(1)).await;
    let mut hurt = seen();
    hurt.facts = {
        let mut bot = hurt.facts.bot().clone();
        bot.health = 7.5;
        bot.position = Position::try_new(3.5, 64.0, 0.5).unwrap();
        Snapshot::new(bot, Vec::new(), hurt.facts.blocks().clone())
    };
    controller.show(hurt);
    pass(secs(1)).await;

    assert_eq!(lines(&mut harness), ["20.0 0.5", "7.5 3.5"]);
    assert_eq!(snapshots(&controller), 3);
}

/// Every action passes the bot's limiter, which outlives the hook: two
/// jumps a quarter second apart at most.
#[tokio::test(start_paused = true)]
async fn a_hooks_actions_pass_the_bots_limiter() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let source = "fn on_join(first) {\n  keys.jump.tap();\n  let again = keys.jump.tap();\n  log.info(again.reason);\n  timer.after(250, \"later\");\n}\nfn on_timer(name) {\n  log.info(`${keys.jump.tap().ok}`);\n}";

    let (mut harness, controller) = online(draft(source)).await;
    pass(ms(250)).await;

    assert_eq!(lines(&mut harness), ["too_soon", "true"]);
    assert_eq!(
        actions(&controller),
        [
            GameAction::ReleaseAll,
            GameAction::Tap(JUMP),
            GameAction::Tap(JUMP)
        ]
    );
    assert_eq!(
        recorder.counter(SCRIPT_REFUSED, &[("reason", "too_soon")]),
        1
    );
}

const COUNTS_JOINS: &str = "fn on_join(first) {\n  if first {\n    state.joins = 0;\n  }\n  state.joins += 1;\n  log.info(`${first} ${state.joins}`);\n}";

#[tokio::test(start_paused = true)]
async fn the_state_is_kept_across_sessions_and_only_the_first_join_is_the_first() {
    let (mut harness, first) = online(draft(COUNTS_JOINS)).await;

    let second = rejoin(&harness, &first, 1).await;
    let _third = rejoin(&harness, &second, 2).await;

    assert_eq!(lines(&mut harness), ["true 1", "false 2", "false 3"]);
    // One start of the script: it says so at each join.
    assert_eq!(statuses(&mut harness), []);
}

#[tokio::test(start_paused = true)]
async fn a_stop_ends_the_scripts_run_and_a_start_is_a_new_one() {
    let (mut harness, _first) = online(draft(COUNTS_JOINS)).await;

    harness
        .apply(BotSpec {
            desired: DesiredRunState::Stopped,
            ..scripted(draft(COUNTS_JOINS))
        })
        .await;
    pass(ms(1)).await;
    assert_eq!(harness.state(BOT).await, BotState::Stopped);
    harness.apply(scripted(draft(COUNTS_JOINS))).await;
    let _second = join(&harness, 1).await;

    assert_eq!(lines(&mut harness), ["true 1", "true 1"]);
}

/// No runner lives while the bot is between two sessions: the actor
/// itself ends the script's run at a Stop.
#[tokio::test(start_paused = true)]
async fn a_stop_while_the_bot_is_offline_ends_the_scripts_run_too() {
    let (mut harness, first) = online(draft(COUNTS_JOINS)).await;

    emit(&first, kick(TRANSIENT));
    pass(ms(1)).await;
    assert!(matches!(harness.state(BOT).await, BotState::Backoff { .. }));
    harness
        .apply(BotSpec {
            desired: DesiredRunState::Stopped,
            ..scripted(draft(COUNTS_JOINS))
        })
        .await;
    harness.apply(scripted(draft(COUNTS_JOINS))).await;
    let _second = join(&harness, 1).await;

    assert_eq!(lines(&mut harness), ["true 1", "true 1"]);
}

/// A script that stood stopped pressed nothing in this session, so its
/// end has nothing to release.
#[tokio::test(start_paused = true)]
async fn a_script_that_never_ran_in_the_session_leaves_it_alone_at_its_end() {
    let source = "fn on_join(first) {\n  eval(\"1\");\n}";
    let (harness, controller) = online(draft(source)).await;

    harness
        .apply(BotSpec {
            desired: DesiredRunState::Stopped,
            ..scripted(draft(source))
        })
        .await;
    pass(ms(1)).await;

    assert_eq!(
        done(&controller),
        [Performed::Guards(Guards::default()), Performed::Disconnect]
    );
}

#[tokio::test(start_paused = true)]
async fn a_changed_script_starts_anew_in_the_same_session() {
    let (mut harness, controller) = online(draft(COUNTS_JOINS)).await;
    let other = "fn on_join(first) {\n  log.info(`other ${first}`);\n  keys.jump.hold();\n}";

    harness.apply(scripted(draft(other))).await;
    pass(ms(1)).await;

    assert_eq!(lines(&mut harness), ["true 1", "other true"]);
    // The old script's end released everything, the new one set its
    // guards and began with its own join.
    assert_eq!(
        done(&controller),
        [
            Performed::Guards(Guards::default()),
            RELEASE_ALL,
            RELEASE_ALL,
            Performed::Guards(Guards::default()),
            RELEASE_ALL,
            Performed::Action(GameAction::Press(JUMP)),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_mode_that_is_saved_as_it_was_changes_nothing() {
    let (mut harness, controller) = online(draft(COUNTS_JOINS)).await;
    let before = controller.log();

    harness.apply(scripted(draft(COUNTS_JOINS))).await;
    pass(ms(1)).await;

    assert_eq!(controller.log(), before);
    assert_eq!(lines(&mut harness), ["true 1"]);
}

fn jumps_every_second() -> ModeDefinition {
    mode(vec![Step {
        action: Action::Jump,
        schedule: Schedule::Every {
            interval: secs(1),
            jitter: Duration::ZERO,
        },
        probability: 100,
    }])
}

#[tokio::test(start_paused = true)]
async fn a_step_mode_takes_over_from_a_script_and_a_script_from_a_step_mode() {
    let (mut harness, controller) = online(draft(HOLDS_FORWARD)).await;

    harness
        .apply(BotSpec {
            mode: jumps_every_second(),
            ..running(BOT, "AfkBot1")
        })
        .await;
    pass(secs(1)).await;
    let as_steps = actions(&controller);
    harness.apply(scripted(draft(COUNTS_JOINS))).await;
    pass(secs(3)).await;

    // The script's end let go of the key before the steps began.
    assert_eq!(
        as_steps,
        [
            GameAction::ReleaseAll,
            GameAction::Press(FORWARD),
            GameAction::ReleaseAll,
            GameAction::Jump,
        ]
    );
    // The steps' end lets go as every mode change does, and no step runs
    // under the script.
    assert_eq!(
        actions(&controller).get(4..),
        Some(
            [
                GameAction::HoldUse { on: false },
                GameAction::Sneak { on: false },
                GameAction::ReleaseAll,
            ]
            .as_slice()
        )
    );
    assert_eq!(lines(&mut harness), ["true 1"]);
    assert_eq!(controller.held(), []);
}

#[tokio::test(start_paused = true)]
async fn a_death_and_a_respawn_run_their_hooks_with_everything_released_first() {
    let source = "fn on_join(first) {\n  keys.forward.hold();\n}\nfn on_death() {\n  log.info(`dead ${keys.forward.held} ${keys.forward.hold().reason}`);\n}\nfn on_respawn() {\n  log.info(`back ${keys.sneak.hold().ok}`);\n}";
    let (mut harness, controller) = online(draft(source)).await;
    controller.withhold_respawns();

    emit(&controller, SessionEvent::Died);
    pass(ms(1)).await;
    let dead = (lines(&mut harness), controller.held());
    // The actor asked for the respawn; now the server confirms it.
    controller.confirm_respawns();
    pass(ms(1)).await;

    assert_eq!(dead, (vec!["dead false dead".to_owned()], Vec::new()));
    assert_eq!(lines(&mut harness), ["back true"]);
    assert_eq!(
        actions(&controller),
        [
            GameAction::ReleaseAll,
            GameAction::Press(FORWARD),
            GameAction::ReleaseAll,
            GameAction::ReleaseAll,
            GameAction::Press(Input::Key(Key::Sneak)),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_dead_bots_timers_wait_until_it_lives_again() {
    let source = "fn on_join(first) {\n  timer.every(1000, \"tick\");\n}\nfn on_timer(name) {\n  log.info(\"tick\");\n}";
    let (mut harness, controller) = online(draft(source)).await;
    controller.withhold_respawns();

    emit(&controller, SessionEvent::Died);
    pass(ms(3_500)).await;
    let while_dead = lines(&mut harness);
    controller.confirm_respawns();
    pass(ms(600)).await;

    assert_eq!(while_dead, [""; 0]);
    // It doesn't catch up: one tick, then every second again.
    assert_eq!(lines(&mut harness), ["tick"]);
}

#[tokio::test(start_paused = true)]
async fn a_world_change_sets_the_guards_again_and_runs_its_hook() {
    let source = "fn on_join(first) {\n  keys.forward.hold();\n}\nfn on_world_change() {\n  log.info(`moved ${keys.forward.held}`);\n  keys.back.hold();\n}";
    let (mut harness, controller) = online(draft(source)).await;

    emit(&controller, SessionEvent::WorldChanged);
    pass(ms(1)).await;

    assert_eq!(lines(&mut harness), ["moved false"]);
    assert_eq!(
        done(&controller),
        [
            Performed::Guards(Guards::default()),
            RELEASE_ALL,
            Performed::Action(GameAction::Press(FORWARD)),
            Performed::Guards(Guards::default()),
            RELEASE_ALL,
            Performed::Action(GameAction::Press(Input::Key(Key::Back))),
        ]
    );
    assert_eq!(controller.held(), [Input::Key(Key::Back)]);
}

#[tokio::test(start_paused = true)]
async fn what_the_session_let_go_of_the_script_no_longer_holds() {
    let source = "fn on_join(first) {\n  keys.forward.hold();\n  mouse.left.hold();\n  timer.every(1000, \"look\");\n}\nfn on_timer(name) {\n  log.info(`${keys.forward.held} ${mouse.left.held}`);\n}";
    let (mut harness, controller) = online(draft(source)).await;

    pass(secs(1)).await;
    emit(&controller, SessionEvent::LetGo(LetGo::Leash));
    pass(secs(1)).await;
    emit(&controller, SessionEvent::LetGo(LetGo::ToolGuard));
    pass(secs(1)).await;

    assert_eq!(
        lines(&mut harness),
        ["true true", "false true", "false false"]
    );
}

#[tokio::test(start_paused = true)]
async fn a_hold_the_session_didnt_take_isnt_held_by_the_script() {
    let source = "fn on_join(first) {\n  timer.after(1000, \"press\");\n  timer.after(2000, \"look\");\n}\nfn on_timer(name) {\n  if name == \"press\" {\n    keys.forward.hold();\n  } else {\n    log.info(`${keys.forward.held}`);\n  }\n}";
    let (mut harness, controller) = online(draft(source)).await;

    controller.fail_performs(SessionError::QueueFull);
    pass(secs(1)).await;
    controller.succeed_performs();
    pass(secs(1)).await;

    assert_eq!(lines(&mut harness), ["false"]);
    assert_eq!(controller.held(), []);
}

/// While the session is too busy to say what the bot sees, a timer isn't
/// lost and isn't told the bot left its world: it runs when the session
/// answers again.
#[tokio::test(start_paused = true)]
async fn a_timer_waits_for_a_session_that_is_too_busy_to_observe() {
    let source = "fn on_join(first) {\n  timer.every(1000, \"tick\");\n}\nfn on_timer(name) {\n  log.info(\"tick\");\n}";
    let (mut harness, controller) = online(draft(source)).await;

    controller.fail_actions(SessionError::TimedOut);
    pass(ms(1_600)).await;
    let while_busy = lines(&mut harness);
    controller.succeed_actions();
    pass(ms(400)).await;
    let once_it_answers = lines(&mut harness);
    pass(secs(1)).await;

    assert_eq!(while_busy, [""; 0]);
    assert_eq!(once_it_answers, ["tick"]);
    assert_eq!(lines(&mut harness), ["tick"]);
}

#[tokio::test(start_paused = true)]
async fn two_bots_run_their_scripts_apart() {
    let harness = Setup::new().start().await;
    harness.apply(scripted(draft(HOLDS_FORWARD))).await;
    let first = join(&harness, 0).await;
    harness
        .apply(BotSpec {
            mode: scripted_mode(draft("fn on_join(first) {\n  keys.jump.hold();\n}")),
            ..running(OTHER, "AfkBot2")
        })
        .await;
    let second = harness.session(1).await;
    second.show(seen());
    emit(&second, SessionEvent::Joined);
    pass(ms(1)).await;

    assert_eq!(first.held(), [FORWARD]);
    assert_eq!(second.held(), [JUMP]);
}

// --- Every exit releases everything (Plan.md P15.17) ---

/// Holds two keys and a button from its join on, and says so at its end.
const HOLDS: &str = "fn on_join(first) {\n  keys.forward.hold();\n  keys.sneak.hold();\n  mouse.left.hold();\n  timer.every(1000, \"tick\");\n}\nfn on_timer(name) {\n  if state.fail == true {\n    keys.forward.hodl();\n  }\n}\nfn on_stop() {\n  log.info(\"bye\");\n}";

/// What [`HOLDS`] holds once it has joined.
const HELD: [Input; 3] = [FORWARD, Input::Key(Key::Sneak), LEFT];

#[tokio::test(start_paused = true)]
async fn a_stop_runs_on_stop_and_releases_everything_before_the_session_goes() {
    let (mut harness, controller) = online(draft(HOLDS)).await;
    assert_eq!(controller.held(), HELD);

    harness
        .apply(BotSpec {
            desired: DesiredRunState::Stopped,
            ..scripted(draft(HOLDS))
        })
        .await;
    pass(ms(1)).await;

    assert_eq!(lines(&mut harness), ["bye"]);
    assert_eq!(
        done(&controller).get(5..),
        Some([RELEASE_ALL, Performed::Disconnect].as_slice())
    );
    assert_eq!(controller.held(), []);
    assert_eq!(harness.state(BOT).await, BotState::Stopped);
}

#[tokio::test(start_paused = true)]
async fn a_mode_change_runs_on_stop_and_releases_everything() {
    let (mut harness, controller) = online(draft(HOLDS)).await;

    harness
        .apply(BotSpec {
            mode: mode(Vec::new()),
            ..running(BOT, "AfkBot1")
        })
        .await;
    pass(ms(1)).await;

    assert_eq!(lines(&mut harness), ["bye"]);
    assert_eq!(done(&controller).last(), Some(&RELEASE_ALL));
    assert_eq!(controller.held(), []);
    assert!(!controller.is_torn_down());
}

#[tokio::test(start_paused = true)]
async fn a_script_that_fails_stops_with_everything_released_and_the_bot_stays_online() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let source = HOLDS.replace(
        "timer.every(1000, \"tick\");",
        "state.fail = true;\n  timer.every(1000, \"tick\");",
    );
    let (mut harness, controller) = online(draft(&source)).await;
    let _ = statuses(&mut harness);

    pass(secs(1)).await;

    let stopped = statuses(&mut harness);
    let [ScriptStatus::Stopped(error)] = stopped.as_slice() else {
        panic!("the script stopped once: {stopped:?}");
    };
    assert_eq!(error.hook, Hook::Timer);
    assert_eq!(error.at.map(|at| at.line), Some(10));
    assert_eq!(
        error.kind,
        ScriptErrorKind::Failed {
            message: "Function not found: hodl (input)".to_owned()
        }
    );
    assert_eq!(done(&controller).last(), Some(&RELEASE_ALL));
    assert_eq!(controller.held(), []);
    assert!(matches!(harness.state(BOT).await, BotState::Online { .. }));
    assert_eq!(recorder.counter(SCRIPTS_STOPPED, &[("reason", "error")]), 1);
    // An error is no orderly end: `on_stop` didn't run.
    assert_eq!(lines(&mut harness), [""; 0]);
}

/// The runner hands the instance the fleet's stopwatch: with one that
/// moves a millisecond at each reading, a loop is over its 5 ms at once.
#[tokio::test(start_paused = true)]
async fn a_script_that_hits_a_limit_stops_with_everything_released() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let source = "fn on_join(first) {\n  keys.forward.hold();\n  timer.after(1000, \"spin\");\n}\nfn on_timer(name) {\n  let n = 0;\n  while n < 1000 {\n    n += 1;\n  }\n}";
    let setup = Setup::new();
    let stopwatch = setup.stopwatch.clone();
    let mut harness = setup.start().await;
    harness.apply(scripted(draft(source))).await;
    let controller = join(&harness, 0).await;
    let _ = statuses(&mut harness);

    stopwatch.set_step(ms(1));
    pass(secs(1)).await;

    let stopped = statuses(&mut harness);
    let [ScriptStatus::Stopped(error)] = stopped.as_slice() else {
        panic!("the script stopped once: {stopped:?}");
    };
    assert_eq!(error.kind, ScriptErrorKind::Limit(Limit::HookTime));
    assert_eq!((error.hook, error.at), (Hook::Timer, None));
    assert_eq!(done(&controller).last(), Some(&RELEASE_ALL));
    assert_eq!(controller.held(), []);
    assert_eq!(recorder.counter(SCRIPTS_STOPPED, &[("reason", "limit")]), 1);
}

#[tokio::test(start_paused = true)]
async fn a_death_releases_everything() {
    let (_harness, controller) = online(draft(HOLDS)).await;
    controller.withhold_respawns();

    emit(&controller, SessionEvent::Died);
    pass(ms(1)).await;

    let after_the_presses = done(&controller).split_off(5);
    assert!(
        after_the_presses.contains(&RELEASE_ALL),
        "{after_the_presses:?}"
    );
    assert!(after_the_presses.contains(&Performed::Respawn));
    assert_eq!(controller.held(), []);
}

#[tokio::test(start_paused = true)]
async fn a_world_change_releases_everything() {
    let (_harness, controller) = online(draft(HOLDS)).await;
    // Something the session itself still held would show here: the fake
    // lets go at a world change as fleet-mc does, and the script's own
    // `ReleaseAll` comes before its hook.
    emit(&controller, SessionEvent::WorldChanged);
    pass(ms(1)).await;

    assert_eq!(done(&controller).last(), Some(&RELEASE_ALL));
    assert_eq!(controller.held(), []);
}

#[tokio::test(start_paused = true)]
async fn after_a_disconnect_nothing_is_held_in_the_next_session_either() {
    let source = "fn on_join(first) {\n  log.info(`${first} ${keys.forward.held}`);\n  keys.forward.hold();\n  if first {\n    timer.every(1000, \"tick\");\n  }\n}\nfn on_timer(name) {\n  log.info(name);\n}\nfn on_stop() {\n  log.info(\"bye\");\n}";
    let (mut harness, first) = online(draft(source)).await;
    assert_eq!(first.held(), [FORWARD]);

    let second = rejoin(&harness, &first, 1).await;
    pass(secs(3)).await;

    // The session's end let go of everything, and the script knows: its
    // key is up when it joins again, and the timer it set before is gone.
    assert_eq!(first.held(), []);
    assert_eq!(lines(&mut harness), ["true false", "false false"]);
    assert_eq!(
        done(&second),
        [
            Performed::Guards(Guards::default()),
            RELEASE_ALL,
            Performed::Action(GameAction::Press(FORWARD)),
        ]
    );
}

/// The runner's task ends by a panic: the bot's actor, which owns the
/// task, releases what the script held before it tears the session down.
#[tokio::test(start_paused = true)]
async fn a_runner_that_panics_leaves_nothing_held() {
    let source = HOLDS.replace(
        "if state.fail == true {\n    keys.forward.hodl();\n  }",
        "keys.jump.tap();",
    );
    let (harness, controller) = online(draft(&source)).await;
    assert_eq!(controller.held(), HELD);

    harness.connector.script(id(BOT), |script| {
        script.perform_panics = true;
    });
    pass(secs(1)).await;

    // The tap panicked inside the runner's task. The actor sent the
    // `ReleaseAll` itself, then ended as after any task's crash.
    assert_eq!(
        done(&controller).get(5..),
        Some([RELEASE_ALL, Performed::Disconnect].as_slice())
    );
    assert_eq!(controller.held(), []);
    assert_eq!(harness.connector.crashes().len(), 1);
}

/// A session that hangs can't hold a Stop: the mode's orderly end is
/// bounded, and the session's own end releases what it held.
#[tokio::test(start_paused = true)]
async fn a_stop_doesnt_wait_for_a_session_that_hangs() {
    let (harness, controller) = online(draft(HOLDS)).await;

    controller.hang();
    harness
        .apply(BotSpec {
            desired: DesiredRunState::Stopped,
            ..scripted(draft(HOLDS))
        })
        .await;
    pass(secs(3)).await;

    assert_eq!(harness.state(BOT).await, BotState::Stopped);
    assert!(controller.is_torn_down());
    assert_eq!(controller.held(), []);
}

#[tokio::test(start_paused = true)]
async fn on_stop_runs_at_an_orderly_stop_only() {
    let (mut harness, first) = online(draft(HOLDS)).await;

    // A disconnect is none.
    let second = rejoin(&harness, &first, 1).await;
    let after_a_disconnect = lines(&mut harness);
    // A removal stops the bot: that's one.
    harness.remove(BOT).await;
    pass(ms(1)).await;

    assert_eq!(after_a_disconnect, [""; 0]);
    assert_eq!(lines(&mut harness), ["bye"]);
    assert_eq!(second.held(), []);
}

#[tokio::test(start_paused = true)]
async fn on_stops_one_message_goes_out_before_the_session_ends() {
    let source = "fn on_join(first) {\n  keys.forward.hold();\n}\nfn on_stop() {\n  let sent = chat.send(\"hello\");\n  let again = chat.send(\"balance\");\n  log.info(`${sent.ok} ${again.reason}`);\n}";
    let (mut harness, controller) = online(balance(source)).await;

    harness
        .apply(BotSpec {
            desired: DesiredRunState::Stopped,
            ..scripted(balance(source))
        })
        .await;
    pass(ms(1)).await;

    assert_eq!(
        done(&controller).get(3..),
        Some(
            [
                Performed::Chat(message("Hello there")),
                RELEASE_ALL,
                Performed::Disconnect
            ]
            .as_slice()
        )
    );
    assert_eq!(
        logged(&mut harness),
        [
            ScriptLog::Refused(Refused {
                asked: Asked::Send,
                reason: Refusal::NotInWorld,
                times: 1
            }),
            ScriptLog::Sent {
                key: key("hello"),
                message: message("Hello there")
            },
            ScriptLog::Line("true not_in_world".to_owned()),
        ]
    );
}

// --- Status, error and log (Plan.md P15.18) ---

#[tokio::test(start_paused = true)]
async fn a_script_that_stopped_isnt_started_again_at_the_next_join() {
    let source = "fn on_join(first) {\n  log.info(\"joined\");\n  keys.forward.hodl();\n}";
    let (mut harness, first) = online(draft(source)).await;
    let at_first = script_events(&mut harness);

    let second = rejoin(&harness, &first, 1).await;

    // What a hook that fails asked for is dropped, its log line too
    // (ADR-0040): the error with its line is what the app shows.
    let [
        ScriptEvent::Status(ScriptStatus::Running),
        stopped @ ScriptEvent::Status(ScriptStatus::Stopped(_)),
    ] = at_first.as_slice()
    else {
        panic!("it started and stopped: {at_first:?}");
    };
    // The app hears again that it's stopped; nothing runs.
    assert_eq!(script_events(&mut harness), core::slice::from_ref(stopped));
    assert_eq!(done(&second), [Performed::Guards(Guards::default())]);
}

#[tokio::test(start_paused = true)]
async fn a_script_this_engine_doesnt_take_is_refused_and_the_bot_stays_idle() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    // It's a scripted mode's body, but no program: `eval` isn't part of
    // the language here.
    let source = "fn on_join(first) {\n  eval(\"1\");\n}";

    let (mut harness, controller) = online(draft(source)).await;
    pass(secs(5)).await;

    let refused = statuses(&mut harness);
    let [ScriptStatus::Refused(error)] = refused.as_slice() else {
        panic!("the script was refused once: {refused:?}");
    };
    assert_eq!(error.at.map(|at| at.line), Some(2));
    assert_eq!(done(&controller), [Performed::Guards(Guards::default())]);
    assert!(matches!(harness.state(BOT).await, BotState::Online { .. }));
    assert_eq!(
        recorder.counter(SCRIPTS_STOPPED, &[("reason", "refused")]),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn a_scripts_own_log_lines_are_events_in_order() {
    let source = "fn on_join(first) {\n  log.info(\"one\");\n  log.info(\"two\");\n}";

    let (mut harness, _controller) = online(draft(source)).await;

    assert_eq!(
        script_events(&mut harness),
        [
            ScriptEvent::Status(ScriptStatus::Running),
            ScriptEvent::Log(ScriptLog::Line("one".to_owned())),
            ScriptEvent::Log(ScriptLog::Line("two".to_owned())),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn hooks_and_what_they_were_refused_are_counted() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let source = "fn on_join(first) {\n  look.at(400.0, 0.0);\n  look.at(400.0, 0.0);\n  timer.every(1000, \"tick\");\n}\nfn on_timer(name) {\n  hotbar.select(0);\n}";

    let (_harness, controller) = online(draft(source)).await;
    pass(secs(2)).await;
    emit(&controller, SessionEvent::Died);
    pass(ms(1)).await;

    let hooks = |hook: &str| recorder.counter(SCRIPT_HOOKS, &[("hook", hook)]);
    assert_eq!(
        (hooks("on_join"), hooks("on_timer"), hooks("on_death")),
        (1, 2, 1)
    );
    assert_eq!(hooks("on_respawn"), 1, "the fake confirmed the respawn");
    assert_eq!(hooks("on_chat") + hooks("on_stop"), 0);
    assert_eq!(
        recorder.counter(SCRIPT_REFUSED, &[("reason", "bad_value")]),
        4
    );
}

/// A script's own text never reaches a log line above `debug`: not its
/// log lines, not Rhai's message about it, not a declared message.
#[tokio::test(start_paused = true)]
async fn a_scripts_text_is_logged_only_at_debug() {
    let levels = Levels::default();
    let _logs = tracing::subscriber::set_default(levels.clone());
    let source = "fn on_join(first) {\n  log.info(\"a-line-of-the-scripts-own\");\n  chat.send(\"hello\");\n  timer.after(1000, \"fail\");\n}\nfn on_timer(name) {\n  keys.forward.a_name_of_the_scripts_own();\n}";

    let (mut harness, _controller) = online(balance(source)).await;
    pass(secs(1)).await;

    assert!(matches!(
        statuses(&mut harness).last(),
        Some(ScriptStatus::Stopped(_))
    ));
    for text in [
        "a-line-of-the-scripts-own",
        "a_name_of_the_scripts_own",
        "Hello there",
    ] {
        let at = levels.of(text);
        assert!(!at.is_empty(), "{text} was logged");
        assert!(
            at.iter().all(|level| *level == Level::DEBUG),
            "{text}: {at:?}"
        );
    }
    // What a human reads at `info` names the hook and the line, no text.
    assert_eq!(
        levels.of("the bot's script stopped; the bot stays online and idle"),
        [Level::INFO]
    );
    assert_eq!(levels.of("the bot's script started"), [Level::INFO]);
}

// --- Chat in the runner (Plan.md P15.33) ---

/// Asks for the balance at its join and expects the reply for 5 s.
const ASKS: &str = "fn on_join(first) {\n  chat.send(\"balance\");\n  chat.expect(\"balance\", 5000);\n}\nfn on_chat(key, values) {\n  log.info(`${key} ${values.amount}`);\n}";

const REPLY: &str = "Balance: $1,250.50";

fn sent_and_opened() -> [ScriptLog; 2] {
    [
        ScriptLog::Sent {
            key: key("balance"),
            message: message("/balance"),
        },
        ScriptLog::WindowOpened {
            pattern: key("balance"),
        },
    ]
}

#[tokio::test(start_paused = true)]
async fn a_reply_inside_its_window_runs_on_chat_once() {
    let (mut harness, controller) = online(balance(ASKS)).await;
    assert_eq!(logged(&mut harness), sent_and_opened());

    pass(secs(2)).await;
    emit(&controller, system(REPLY));
    pass(ms(1)).await;
    let first = logged(&mut harness);
    emit(&controller, system(REPLY));
    pass(ms(1)).await;

    let ScriptLog::Matched { pattern, values } = &first[0] else {
        panic!("a match is logged first: {first:?}");
    };
    assert_eq!(pattern.as_str(), "balance");
    assert_eq!(
        (values[0].name.as_str(), values[0].value.as_str()),
        ("amount", "1250.5")
    );
    assert_eq!(
        first.get(1),
        Some(&ScriptLog::Line("balance 1250.5".to_owned()))
    );
    // One reply an `expect`: the second line finds the window closed.
    assert_eq!(logged(&mut harness), []);
}

/// The window is counted on tokio's time from the moment the session took
/// the command: open at 5,000 ms, closed a millisecond later.
#[tokio::test(start_paused = true)]
async fn a_reply_after_its_window_is_nothing_and_the_log_says_it_ran_out() {
    let (mut harness, controller) = online(balance(ASKS)).await;
    let _ = logged(&mut harness);

    // The join took one millisecond of the five thousand.
    pass(ms(4_999)).await;
    let at_its_end = logged(&mut harness);
    pass(ms(1)).await;
    let a_millisecond_later = logged(&mut harness);
    emit(&controller, system(REPLY));
    pass(ms(1)).await;

    assert_eq!(at_its_end, []);
    assert_eq!(
        a_millisecond_later,
        [ScriptLog::WindowExpired {
            pattern: key("balance")
        }]
    );
    assert_eq!(logged(&mut harness), []);
}

#[tokio::test(start_paused = true)]
async fn a_reply_at_its_windows_last_millisecond_still_counts() {
    let (mut harness, controller) = online(balance(ASKS)).await;
    let _ = logged(&mut harness);

    pass(ms(4_998)).await;
    emit(&controller, system(REPLY));
    pass(ms(1)).await;

    assert_eq!(
        logged(&mut harness).last(),
        Some(&ScriptLog::Line("balance 1250.5".to_owned()))
    );
}

/// The bot's chat rate is used up: the queue refuses the command at once,
/// so no window opens, and the reply's text is nothing to the script.
#[tokio::test(start_paused = true)]
async fn a_command_the_rate_limit_refuses_opens_no_window() {
    let source = ASKS.replace(
        "fn on_join(first) {",
        "fn on_join(first) {\n  timer.after(1000, \"ask\");\n}\nfn on_timer(name) {",
    );
    let (mut harness, controller) = online(balance(&source)).await;
    // The bucket holds three messages: the user's chat takes them all.
    for _ in 0..3 {
        harness
            .fleet
            .send_chat(id(BOT), message("hi"))
            .await
            .unwrap();
    }

    pass(secs(1)).await;
    emit(&controller, system(REPLY));
    pass(ms(1)).await;

    assert_eq!(
        logged(&mut harness),
        [
            ScriptLog::NotSent {
                key: key("balance"),
                reason: NotSent::Refused(ChatError::RateLimited),
            },
            ScriptLog::WindowDropped {
                pattern: key("balance")
            },
        ]
    );
    assert!(
        !controller
            .log()
            .contains(&Performed::Chat(message("/balance")))
    );
}

/// A queue of one message: the hook's second command finds it full.
#[tokio::test(start_paused = true)]
async fn a_command_the_full_queue_refuses_opens_no_window() {
    let source = "fn on_join(first) {\n  chat.send(\"pay\", #{ amount: 5 });\n  chat.send(\"balance\");\n  chat.expect(\"balance\", 5000);\n}\nfn on_chat(key, values) {\n  log.info(`${key} ${values.amount}`);\n}";
    let mut setup = Setup::new();
    setup.config.chat_queue = core::num::NonZeroUsize::MIN;
    let mut harness = setup.start().await;
    harness.apply(scripted(balance(source))).await;
    let controller = join(&harness, 0).await;

    emit(&controller, system(REPLY));
    pass(ms(1)).await;

    assert_eq!(
        logged(&mut harness),
        [
            ScriptLog::NotSent {
                key: key("balance"),
                reason: NotSent::Refused(ChatError::QueueFull),
            },
            ScriptLog::WindowDropped {
                pattern: key("balance")
            },
            ScriptLog::Sent {
                key: key("pay"),
                message: message("/pay AfkMain 5.00"),
            },
        ]
    );
}

/// The queue took the command, and the session didn't: it can't sign it.
/// No window opens.
#[tokio::test(start_paused = true)]
async fn a_command_the_session_doesnt_take_opens_no_window() {
    let harness = Setup::new().start().await;
    harness.apply(scripted(balance(ASKS))).await;
    let controller = harness.session(0).await;
    controller.show(seen());
    controller.fail_chat(SessionError::ChatUnavailable);
    emit(&controller, SessionEvent::Joined);
    pass(ms(1)).await;
    let mut harness = harness;

    controller.succeed_chat();
    emit(&controller, system(REPLY));
    pass(ms(1)).await;

    assert_eq!(
        logged(&mut harness),
        [
            ScriptLog::NotSent {
                key: key("balance"),
                reason: NotSent::Failed(ChatFailure::ChatUnavailable),
            },
            ScriptLog::WindowDropped {
                pattern: key("balance")
            },
        ]
    );
}

/// The limiter refuses plain chat within 30 s: nothing is queued, and a
/// window asked for after it has no command to wait for.
#[tokio::test(start_paused = true)]
async fn chat_the_limiter_refuses_is_never_queued_and_opens_no_window() {
    let source = "fn on_join(first) {\n  chat.send(\"hello\");\n  let again = chat.send(\"hello\");\n  let window = chat.expect(\"balance\", 5000);\n  log.info(`${again.reason} ${window.reason}`);\n}\nfn on_chat(key, values) {\n  log.info(\"matched\");\n}";
    let (mut harness, controller) = online(balance(source)).await;

    emit(&controller, system(REPLY));
    pass(ms(1)).await;

    assert_eq!(
        logged(&mut harness),
        [
            ScriptLog::Refused(Refused {
                asked: Asked::Send,
                reason: Refusal::TooSoon,
                times: 1
            }),
            ScriptLog::Refused(Refused {
                asked: Asked::Expect,
                reason: Refusal::NoCommand,
                times: 1
            }),
            ScriptLog::Line("too_soon no_command".to_owned()),
            ScriptLog::Sent {
                key: key("hello"),
                message: message("Hello there"),
            },
        ]
    );
    let chats = controller
        .log()
        .iter()
        .filter(|performed| matches!(performed, Performed::Chat(_)))
        .count();
    assert_eq!(chats, 1);
}

/// What a player writes is never read, whatever its text: not chat, not a
/// whisper, not `/me`. Only the server's own line is.
#[tokio::test(start_paused = true)]
async fn a_players_line_with_the_replys_text_matches_nothing() {
    let (mut harness, controller) = online(balance(ASKS)).await;
    let _ = logged(&mut harness);

    for kind in [
        PlayerChatKind::Chat,
        PlayerChatKind::Whisper,
        PlayerChatKind::Emote,
        PlayerChatKind::Announcement,
    ] {
        emit(
            &controller,
            SessionEvent::Chat(IncomingChat::player(kind, "AfkBot2", None, REPLY)),
        );
    }
    // Nor a line that only holds the text, or a message of two lines.
    emit(&controller, system("AfkBot2 says Balance: $1,250.50"));
    emit(&controller, system("Balance: $1,250.50\nBalance: $1"));
    pass(ms(1)).await;
    let from_players = logged(&mut harness);
    emit(&controller, system(REPLY));
    pass(ms(1)).await;

    assert_eq!(from_players, []);
    assert_eq!(
        logged(&mut harness).len(),
        2,
        "the server's own line matches"
    );
}

/// A mode that reads `Server: restarting in {minutes} minutes` at any time:
/// a line a plugin's format `{sender}: ` would print for a player named
/// `Server` (ADR-0037, C2).
fn server_says(source: &str) -> ScriptDraft {
    ScriptDraft {
        patterns: vec![PatternDraft {
            key: "restart".to_owned(),
            when: When::AnyTime,
            text: "Server: restarting in {minutes} minutes".to_owned(),
            captures: vec![CaptureDraft {
                name: "minutes".to_owned(),
                kind: CaptureKind::Integer { min: 0, max: 60 },
            }],
        }],
        ..draft(source)
    }
}

const LOGS_A_RESTART: &str =
    "fn on_chat(key, values) {\n  log.info(`${key} ${values.minutes}`);\n}";

/// The server's chat formats come with the bot's spec (P15.34): a line one
/// of them fits is a player's, and never read.
#[tokio::test(start_paused = true)]
async fn a_line_the_servers_chat_formats_name_a_players_is_never_read() {
    let harness = Setup::new().start().await;
    harness
        .apply(BotSpec {
            chat_formats: ChatFormats::try_new(&["{sender}: "]).unwrap(),
            ..scripted(server_says(LOGS_A_RESTART))
        })
        .await;
    let controller = join(&harness, 0).await;
    let mut harness = harness;

    emit(&controller, system("Server: restarting in 5 minutes"));
    pass(ms(1)).await;

    assert_eq!(lines(&mut harness), Vec::<String>::new());
}

/// A spec whose formats changed reaches a running script at once, without a
/// new session: a format added stops a line from being read, and one taken
/// away lets it through again.
#[tokio::test(start_paused = true)]
async fn a_change_of_the_chat_formats_applies_within_the_session() {
    let (mut harness, controller) = online(server_says(LOGS_A_RESTART)).await;
    let line = || system("Server: restarting in 5 minutes");

    emit(&controller, line());
    pass(ms(1)).await;
    let without = lines(&mut harness);
    harness
        .apply(BotSpec {
            chat_formats: ChatFormats::try_new(&["{sender}: "]).unwrap(),
            ..scripted(server_says(LOGS_A_RESTART))
        })
        .await;
    pass(ms(1_000)).await;
    emit(&controller, line());
    pass(ms(1)).await;
    let with = lines(&mut harness);
    harness.apply(scripted(server_says(LOGS_A_RESTART))).await;
    pass(ms(1_000)).await;
    emit(&controller, line());
    pass(ms(1)).await;

    assert_eq!(without, ["restart 5"]);
    assert_eq!(with, Vec::<String>::new());
    assert_eq!(lines(&mut harness), ["restart 5"]);
    assert_eq!(
        harness.fake.session_count(),
        1,
        "the same session throughout"
    );
}

#[tokio::test(start_paused = true)]
async fn a_disconnect_closes_the_window() {
    let (mut harness, first) = online(balance(
        ASKS.replace(
            "fn on_join(first) {",
            "fn on_join(first) {\n  if !first {\n    return;\n  }",
        )
        .as_str(),
    ))
    .await;
    let _ = logged(&mut harness);

    let second = rejoin(&harness, &first, 1).await;
    emit(&second, system(REPLY));
    pass(ms(1)).await;

    assert_eq!(logged(&mut harness), []);
}

/// A pattern that may come at any time matches once a second; a flood of
/// lines costs the script one hook and its log one line.
#[tokio::test(start_paused = true)]
async fn an_any_time_pattern_matches_once_a_second() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let source = "fn on_chat(key, values) {\n  log.info(`${key} ${values.minutes}`);\n}";
    let (mut harness, controller) = online(balance(source)).await;

    for minutes in [5, 4, 3] {
        emit(
            &controller,
            system(&format!("Server restarting in {minutes} minutes")),
        );
    }
    pass(ms(999)).await;
    // 999 ms after the first match: still inside its second.
    emit(&controller, system("Server restarting in 2 minutes"));
    pass(ms(1)).await;
    let within_a_second = lines(&mut harness);
    // A second after it.
    emit(&controller, system("Server restarting in 1 minutes"));
    pass(ms(1)).await;

    assert_eq!(within_a_second, ["restart 5"]);
    assert_eq!(lines(&mut harness), ["restart 1"]);
    assert_eq!(
        recorder.counter(SCRIPT_CHAT_DROPPED, &[("reason", "gap")]),
        3
    );
}

/// More lines than the script's queue holds are dropped and counted; the
/// queue doesn't grow.
#[tokio::test(start_paused = true)]
async fn chat_beyond_the_scripts_queue_is_dropped_and_counted() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let source = "fn on_chat(key, values) {\n  log.info(`${key} ${values.minutes}`);\n}";
    let mut setup = Setup::new();
    setup.config.script_chat_lines = core::num::NonZeroUsize::new(2).unwrap();
    let mut harness = setup.start().await;
    harness.apply(scripted(balance(source))).await;
    let controller = join(&harness, 0).await;

    // The actor reads them all in one go; the runner gets its turn after.
    for n in 0..10 {
        emit(&controller, system(&format!("line {n}")));
    }
    emit(&controller, system("Server restarting in 1 minutes"));
    pass(ms(1)).await;

    assert_eq!(
        lines(&mut harness),
        [""; 0],
        "the match was among the dropped"
    );
    assert_eq!(
        recorder.counter(SCRIPT_CHAT_DROPPED, &[("reason", "queue")]),
        9
    );
}

#[tokio::test(start_paused = true)]
async fn a_script_without_patterns_is_handed_no_chat() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let mut setup = Setup::new();
    setup.config.script_chat_lines = core::num::NonZeroUsize::new(2).unwrap();
    let harness = setup.start().await;
    harness.apply(scripted(draft(HOLDS_FORWARD))).await;
    let controller = join(&harness, 0).await;

    for n in 0..10 {
        emit(&controller, system(&format!("line {n}")));
    }
    pass(ms(1)).await;

    assert_eq!(
        recorder.counter(SCRIPT_CHAT_DROPPED, &[("reason", "queue")]),
        0
    );
}

/// A script that asks for a window again and again, without a command,
/// gets twenty lines a minute about it and no more.
#[tokio::test(start_paused = true)]
async fn what_a_script_is_refused_is_logged_at_most_twenty_times_a_minute() {
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    let source = "fn on_join(first) {\n  timer.every(250, \"ask\");\n}\nfn on_timer(name) {\n  chat.expect(\"balance\", 5000);\n}";
    let (mut harness, _controller) = online(balance(source)).await;

    pass(secs(30)).await;

    let refused = logged(&mut harness)
        .into_iter()
        .filter(|entry| matches!(entry, ScriptLog::Refused(_)))
        .count();
    assert_eq!(refused, 20);
    assert_eq!(recorder.counter(SCRIPT_LOG_DROPPED, &[]), 100);
    assert_eq!(
        recorder.counter(SCRIPT_REFUSED, &[("reason", "no_command")]),
        120
    );
}

// --- Screens, clicks and signs (Plan.md P16.13; ADR-0052) ---

mod menus {
    use fleet_core::mc::{MenuCommand, MenuError, MenuView, Observation, SessionError};
    use fleet_core::script::menu::{
        ClickFate, ClickGate, ClickResult, Item, ItemData, MenuAction, MenuRefusal, Screen,
        ScreenKind, StateId, WindowId,
    };

    use super::*;

    fn level_2(source: &str) -> ScriptDraft {
        ScriptDraft {
            api: 2,
            ..draft(source)
        }
    }

    /// A chest of one row with a diamond in slot 0, opened in `gate`.
    fn chest(gate: &mut ClickGate) -> Screen {
        let diamond = Item::new(
            Identifier::try_from("diamond").unwrap(),
            core::num::NonZeroU32::new(1).unwrap(),
            ItemData::new(Vec::new()),
        );
        Screen::new(
            gate.opened(WindowId::new(1)),
            StateId::new(1),
            ScreenKind::Generic9x1,
            "Chest",
            [Some(diamond)],
            None,
        )
    }

    /// What the session shows with `screen` open.
    fn showing(screen: &Screen) -> Observation {
        Observation {
            menu: MenuView {
                screen: Some(screen.clone()),
                ..MenuView::default()
            },
            ..seen()
        }
    }

    /// The screen commands the session carried out, in order.
    fn menu_commands(controller: &SessionController) -> Vec<MenuCommand> {
        controller
            .log()
            .into_iter()
            .filter_map(|performed| match performed {
                Performed::Menu(command) => Some(command),
                _ => None,
            })
            .collect()
    }

    /// The script's log since the last call, as the app reads it.
    fn texts(harness: &mut Harness) -> Vec<String> {
        logged(harness)
            .into_iter()
            .map(|entry| entry.report().text.as_str().to_owned())
            .collect()
    }

    /// The farm the user runs at level 1: the right button held from
    /// `on_join`.
    const FARM: &str = "fn on_join(first) { mouse.right.hold(); }";
    const RIGHT: Input = Input::Mouse(MouseButton::Right);

    #[tokio::test(start_paused = true)]
    async fn a_level_1_farm_has_a_servers_screen_closed_and_its_button_pressed_again() {
        let (_harness, controller) = online(draft(FARM)).await;
        pass(secs(2)).await;
        assert_eq!(controller.held(), [RIGHT]);
        let mut gate = ClickGate::new();
        let screen = chest(&mut gate);

        controller.show(showing(&screen));
        emit(&controller, SessionEvent::MenuOpened(screen.id()));
        let let_go = controller.held();
        pass(ms(10)).await;

        assert_eq!(let_go, []);
        assert_eq!(
            menu_commands(&controller),
            [MenuCommand::Close(screen.id())]
        );
        assert_eq!(controller.held(), [RIGHT]);
        let after_the_close: Vec<Performed> = controller
            .log()
            .into_iter()
            .skip_while(|performed| !matches!(performed, Performed::Menu(_)))
            .filter(|performed| matches!(performed, Performed::Action(_)))
            .collect();
        assert_eq!(
            after_the_close,
            [Performed::Action(GameAction::Press(RIGHT))]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_click_goes_out_and_on_click_hears_its_fate_with_a_line_for_a_correction() {
        let source = "fn on_menu_open() { menu.click(0); }\nfn on_click(result) { log.info(`${result.ok} ${result.reason} ${result.slot}`); }";
        let (mut harness, controller) = online(level_2(source)).await;
        let mut gate = ClickGate::new();
        let screen = chest(&mut gate);
        controller.show(showing(&screen));
        emit(&controller, SessionEvent::MenuOpened(screen.id()));
        pass(ms(10)).await;
        let sent = menu_commands(&controller);
        let _ = logged(&mut harness);

        emit(
            &controller,
            SessionEvent::Clicked(ClickResult {
                screen: screen.id(),
                slot: Some(0),
                fate: ClickFate::Rejected,
            }),
        );
        pass(ms(10)).await;

        assert!(matches!(
            sent.as_slice(),
            [MenuCommand::Act(MenuAction::Click(click))] if click.slot() == 0
        ));
        assert_eq!(
            texts(&mut harness),
            [
                "a click on slot 0 was put back by the server (rejected)",
                "false rejected 0"
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_click_the_session_doesnt_send_is_heard_in_on_click_and_written_into_the_log() {
        let source = "fn on_menu_open() { menu.click(0); }\nfn on_click(result) { log.info(`${result.reason} ${menu.pending}`); menu.click(0); }";
        let (mut harness, controller) = online(level_2(source)).await;
        let mut gate = ClickGate::new();
        let screen = chest(&mut gate);
        controller.show(showing(&screen));
        controller.refuse_menu(MenuError::Refused(MenuRefusal::ScreenChanged));
        let _ = logged(&mut harness);

        emit(&controller, SessionEvent::MenuOpened(screen.id()));
        pass(ms(10)).await;

        // The second click, from on_click, waits for its 50 ms; a hook's
        // refusals are written before what it logged itself.
        assert_eq!(
            texts(&mut harness),
            [
                "a click on slot 0 wasn't sent: another screen is open than the one the hook saw, or than the one the slot was read on (screen_changed)",
                "click refused (too_soon), 1 time",
                "screen_changed false"
            ]
        );
        assert_eq!(menu_commands(&controller), []);
    }

    #[tokio::test(start_paused = true)]
    async fn a_fate_that_never_comes_is_unanswered_once_and_a_late_one_is_dropped() {
        let source = "fn on_menu_open() { menu.click(0); }\nfn on_click(result) { log.info(result.reason); }";
        let (mut harness, controller) = online(level_2(source)).await;
        let mut gate = ClickGate::new();
        let screen = chest(&mut gate);
        controller.show(showing(&screen));
        // The session doesn't answer the send in time.
        controller.refuse_menu(MenuError::Session(SessionError::TimedOut));
        emit(&controller, SessionEvent::MenuOpened(screen.id()));
        pass(ms(10)).await;
        let _ = logged(&mut harness);

        pass(secs(9)).await;
        let before = texts(&mut harness);
        pass(secs(1)).await;
        let at_patience = texts(&mut harness);
        emit(
            &controller,
            SessionEvent::Clicked(ClickResult {
                screen: screen.id(),
                slot: Some(0),
                fate: ClickFate::Ok,
            }),
        );
        pass(ms(10)).await;

        assert_eq!(before, Vec::<String>::new());
        assert_eq!(
            at_patience,
            [
                "a click on slot 0 got no answer from the server, which may or may not have done it (unanswered)",
                "unanswered"
            ]
        );
        assert_eq!(texts(&mut harness), Vec::<String>::new());
    }

    #[tokio::test(start_paused = true)]
    async fn a_screens_updates_run_its_hook_at_most_once_a_game_tick() {
        let source = "fn on_menu_update() { log.info(\"update\"); }";
        let (mut harness, controller) = online(level_2(source)).await;
        let mut gate = ClickGate::new();
        let screen = chest(&mut gate);
        controller.show(showing(&screen));
        emit(&controller, SessionEvent::MenuOpened(screen.id()));
        pass(ms(10)).await;
        let _ = lines(&mut harness);

        emit(&controller, SessionEvent::MenuUpdated(screen.id()));
        pass(ms(1)).await;
        let first = lines(&mut harness);
        for _ in 0..5 {
            emit(&controller, SessionEvent::MenuUpdated(screen.id()));
            pass(ms(5)).await;
        }
        let within_the_tick = lines(&mut harness);
        pass(ms(50)).await;

        assert_eq!(first, ["update"]);
        assert_eq!(within_the_tick, Vec::<String>::new());
        assert_eq!(lines(&mut harness), ["update"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_refusal_in_the_hook_is_written_into_the_log_every_time() {
        let source = "fn on_menu_open() { menu.click(0); menu.click(0); menu.click(99); }";
        let (mut harness, controller) = online(level_2(source)).await;
        let mut gate = ClickGate::new();
        let screen = chest(&mut gate);
        controller.show(showing(&screen));
        let _ = logged(&mut harness);

        emit(&controller, SessionEvent::MenuOpened(screen.id()));
        pass(ms(10)).await;

        assert_eq!(
            texts(&mut harness),
            [
                "click refused (click_pending), 1 time",
                "click refused (bad_value), 1 time"
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_bots_name_is_its_accounts() {
        let (mut harness, _controller) =
            online(level_2("fn on_join(first) { log.info(bot.name); }")).await;

        assert_eq!(lines(&mut harness), ["AfkBot1"]);
    }

    // --- A script's end leaves no screen open (Plan.md P16.17, the user's
    // point 3) ---

    /// A script that clicks in every screen that opens and waits for the
    /// fate; its timer `spin` never ends.
    const CLICKS: &str = r#"fn on_menu_open() { menu.click(0); timer.after(300, "spin"); }
fn on_click(result) {}
fn on_timer(name) { loop {} }"#;

    /// What the session carried out from the click on, its screen
    /// commands and its actions.
    fn from_the_click(controller: &SessionController) -> Vec<Performed> {
        controller
            .log()
            .into_iter()
            .skip_while(|performed| !matches!(performed, Performed::Menu(MenuCommand::Act(_))))
            .skip(1)
            .filter(|performed| matches!(performed, Performed::Menu(_) | Performed::Action(_)))
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn a_script_that_hits_a_limit_mid_click_closes_its_screen_and_the_bot_stays_online() {
        let (_harness, controller) = online(level_2(CLICKS)).await;
        let mut gate = ClickGate::new();
        let screen = chest(&mut gate);
        controller.show(showing(&screen));
        emit(&controller, SessionEvent::MenuOpened(screen.id()));
        pass(ms(10)).await;

        // No fate comes: the server is slow. The timer's hook hits the
        // operations' limit while the click waits.
        pass(ms(400)).await;

        assert_eq!(
            from_the_click(&controller),
            [
                Performed::Menu(MenuCommand::Close(screen.id())),
                RELEASE_ALL
            ]
        );
        assert_eq!(controller.held(), []);
    }

    #[tokio::test(start_paused = true)]
    async fn a_mode_change_mid_click_closes_the_screen_before_the_next_mode_begins() {
        let (harness, controller) = online(level_2(CLICKS)).await;
        let mut gate = ClickGate::new();
        let screen = chest(&mut gate);
        controller.show(showing(&screen));
        emit(&controller, SessionEvent::MenuOpened(screen.id()));
        pass(ms(10)).await;

        harness
            .apply(BotSpec {
                mode: jumps_every_second(),
                ..running(BOT, "AfkBot1")
            })
            .await;
        pass(ms(100)).await;

        assert_eq!(
            from_the_click(&controller),
            [
                Performed::Menu(MenuCommand::Close(screen.id())),
                RELEASE_ALL
            ]
        );
    }
}
