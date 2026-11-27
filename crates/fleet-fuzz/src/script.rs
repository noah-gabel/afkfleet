//! The targets over scripted modes (P15.9, P15.31; ADR-0036, D18;
//! ADR-0037, C9): a script's text, a running script, the script API's
//! arguments, a declared message and a declared chat pattern.
//!
//! A script is written by an Admin or the Owner, so its text isn't
//! untrusted the way a server's chat is. It is the one place where a
//! program of someone's own runs inside the agent, though, and whatever
//! it does, the sandbox must hold: a bounded hook, the limits below it,
//! nothing held after a stop.

use core::fmt::Write as _;
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::mc::{MenuCommand, MenuView};
use fleet_core::mode::HotbarSlot;
use fleet_core::script::{
    ActionLimiter, BlockPosition, Blocks, BotFacts, Captured, ChatMatch, ChatPattern, ChatPatterns,
    EntityFact, Given, HeldItem, HookEvent, Ident, Identifier, Input, Key, Leash, LeashRadius,
    Limited, LookingAt, MessageDraft, MessageTemplate, MouseButton, PatternDraft, Position, Press,
    Script, ScriptDraft, Snapshot, Stopwatch, TimerName, limits,
};
use fleet_core::time;
use fleet_script::{
    API, Action, CompileError, EXAMPLES, Instance, MemberKind, Moment, Outcome, Presence,
    PressKind, Program, Ran, ScriptError, ScriptErrorKind, compile, reasons,
};

use crate::{halves, lossy};

/// The moment every run starts at: any moment will do, since a script
/// has no clock but the one it's given.
fn start() -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH
}

/// A registry identifier this crate writes out itself.
fn id(text: &str) -> Option<Identifier> {
    Identifier::try_from(text).ok()
}

/// The script `source` under the balance example's declarations, if it's
/// one fleet-core takes: two messages and a reply pattern, so `chat` has
/// something to send and to expect.
fn script(source: &str) -> Option<Script> {
    let declared = EXAMPLES.iter().find(|example| example.id == "balance")?;
    Script::try_from(ScriptDraft {
        source: source.to_owned(),
        ..declared.draft()
    })
    .ok()
}

/// [`script`] at API level 2, where every member of the API is.
fn script_2(source: &str) -> Option<Script> {
    let declared = EXAMPLES.iter().find(|example| example.id == "balance")?;
    Script::try_from(ScriptDraft {
        api: 2,
        source: source.to_owned(),
        ..declared.draft()
    })
    .ok()
}

/// What a hook sees, drawn from one byte: whether the bot rides, what it
/// holds and what it looks at.
fn facts(shape: u8) -> Option<Snapshot> {
    let bot = BotFacts {
        health: f32::from(shape % 21),
        food: shape % 21,
        position: Position::try_new(f64::from(shape % 12), 64.0, 0.5).ok()?,
        yaw: f32::from(shape) - 128.0,
        pitch: 0.0,
        dimension: id("overworld")?,
        riding: (shape & 0b1 == 0b1).then(|| id("minecart")).flatten(),
        held: match (shape & 0b10 == 0b10, id("iron_pickaxe")) {
            (true, Some(id)) => Some(HeldItem {
                id,
                count: 1,
                durability: Some(u32::from(shape % 5)),
            }),
            _ => None,
        },
        selected: HotbarSlot::try_from(shape % 9).unwrap_or(HotbarSlot::FIRST),
        looking_at: match shape & 0b1100 {
            0b0100 => Some(LookingAt::Block {
                id: id("cobblestone")?,
                at: BlockPosition {
                    x: 0,
                    y: 62 + i32::from(shape % 4),
                    z: 1,
                },
            }),
            0b1000 => Some(LookingAt::Entity {
                kind: id("zombie")?,
                at: Position::try_new(1.5, 64.0, 0.5).ok()?,
            }),
            _ => None,
        },
    };
    let entities = [
        EntityFact {
            kind: id("minecart")?,
            position: Position::try_new(2.5, 64.0, 0.5).ok()?,
            occupied: shape & 0b1_0000 == 0b1_0000,
        },
        EntityFact {
            kind: id("minecart")?,
            position: Position::try_new(4.5, 64.0, 0.5).ok()?,
            occupied: false,
        },
    ];
    let stone = id("stone")?;
    let blocks = Blocks::read(BlockPosition { x: 0, y: 64, z: 0 }, |place| {
        (place.y < 64).then(|| stone.clone())
    });
    Some(Snapshot::new(bot, entities, blocks))
}

/// The event a byte stands for.
fn event(kind: u8, first: bool) -> Option<HookEvent> {
    Some(match kind % 7 {
        0 => HookEvent::Join { first },
        1 => HookEvent::WorldChange,
        2 => HookEvent::Death,
        3 => HookEvent::Respawn,
        4 => HookEvent::Timer(TimerName::try_from("fuzz").ok()?),
        5 => HookEvent::Chat(ChatMatch {
            key: Ident::try_from("balance").ok()?,
            values: vec![(
                Ident::try_from("amount").ok()?,
                Captured::Amount(f64::from(kind) * 500.0),
            )],
        }),
        _ => HookEvent::Stop,
    })
}

/// The limited kind an action is, if it's one.
fn kind_of(action: &Action) -> Option<Limited> {
    Some(match action {
        Action::Press { input, kind } => Limited::of_press(
            *input,
            match kind {
                PressKind::Tap => Press::Tap,
                PressKind::Hold | PressKind::HoldUntilUsed => Press::Hold,
            },
        ),
        Action::LookAt { .. } | Action::LookAtPosition(_) => Limited::LookAt,
        Action::LookTurn { .. } => Limited::LookTurn,
        Action::SelectHotbar(_) => Limited::Hotbar,
        Action::Mount(_) => Limited::Mount,
        Action::Dismount => Limited::Dismount,
        Action::Menu(MenuCommand::Act(_) | MenuCommand::AnswerSign { .. }) => Limited::Click,
        Action::Send { message, .. } if !message.is_command() => Limited::PlainChat,
        Action::Send { .. }
        | Action::Menu(MenuCommand::Close(_) | MenuCommand::OpenInventory)
        | Action::Expect { .. }
        | Action::ReleaseAll
        | Action::ReleaseKeys
        | Action::Release(_)
        | Action::Log(_) => return None,
    })
}

/// Every input a script can hold.
fn inputs() -> impl Iterator<Item = Input> {
    Key::ALL
        .into_iter()
        .map(Input::Key)
        .chain(MouseButton::ALL.into_iter().map(Input::Mouse))
}

/// A stopwatch that moves on by a fixed step at each reading: a run's own
/// measure of how long its hooks take, the same on every machine. (The
/// one tests use is fleet-testkit's, which is a dev-dependency only.)
#[derive(Debug)]
struct Ticking {
    /// What it shows, in nanoseconds.
    shown: AtomicU64,
    /// How far each reading moves it, in nanoseconds.
    step: u64,
}

impl Ticking {
    /// What it shows, without moving it.
    fn shown(&self) -> Duration {
        Duration::from_nanos(self.shown.load(Ordering::Relaxed))
    }

    /// How far each reading moves it.
    const fn step(&self) -> Duration {
        Duration::from_nanos(self.step)
    }
}

impl Stopwatch for Ticking {
    fn elapsed(&self) -> Duration {
        let before = self.shown.fetch_add(self.step, Ordering::Relaxed);
        Duration::from_nanos(before.saturating_add(self.step))
    }
}

/// One bot's script, run step by step, with everything it did judged as
/// it happens: the properties of ADR-0036's D18.
struct Run {
    instance: Instance,
    program: Arc<Program>,
    limiter: ActionLimiter,
    now: DateTime<Utc>,
    presence: Presence,
    /// When each limited kind of action last happened.
    last: Vec<(Limited, DateTime<Utc>)>,
    /// Each hook's moment and operations, of the last second.
    hooks: Vec<(DateTime<Utc>, u64)>,
    /// What the script's hooks are timed by.
    stopwatch: Arc<Ticking>,
    /// Each call's moment and what the stopwatch moved by in it, of the
    /// last second.
    times: Vec<(DateTime<Utc>, Duration)>,
    /// Whether the script stopped or ended.
    over: bool,
}

impl Run {
    fn new(program: Arc<Program>, seed: u64) -> Self {
        // A stretch between two readings takes 0 to 127 µs, by the seed:
        // at 0 no hook ever takes any time, at a few microseconds a busy
        // hook's operations end it, above that its time does.
        let stopwatch = Arc::new(Ticking {
            shown: AtomicU64::new(0),
            step: (seed % 128).saturating_mul(1_000),
        });
        Self {
            instance: Instance::new(
                Arc::clone(&program),
                seed,
                Arc::<Ticking>::clone(&stopwatch),
            ),
            stopwatch,
            times: Vec::new(),
            program,
            limiter: ActionLimiter::new(),
            now: start(),
            presence: Presence::InWorld,
            last: Vec::new(),
            hooks: Vec::new(),
            over: false,
        }
    }

    /// Lets `pause` pass, waking the script whenever it asks, then hands
    /// it `event`, each time with facts of `shape`.
    fn step(&mut self, pause: Duration, shape: u8, event: Option<&HookEvent>) {
        let Some(facts) = facts(shape) else { return };
        let menu = MenuView::default();
        let leash = Leash::new(LeashRadius::DEFAULT, facts.bot().position);
        let end = time::add(self.now, pause);
        // At most as many wakes as timers and holds can be due in a pause.
        for _ in 0..64 {
            match self.instance.next_wake() {
                Some(due) if due <= end => {
                    self.now = self.now.max(due);
                    let moment = Moment {
                        facts: &facts,
                        now: self.now,
                        presence: self.presence,
                        leash: Some(leash),
                        menu: &menu,
                        name: "AfkBot1",
                    };
                    let before = self.stopwatch.shown();
                    let ran = self.instance.wake(&moment, &mut self.limiter);
                    self.judge_time(before, limits::MAX_TIMERS);
                    self.judge(&ran);
                }
                _ => break,
            }
        }
        self.now = end;
        let Some(event) = event else { return };
        match event {
            HookEvent::Death => self.presence = Presence::Dead,
            HookEvent::Join { .. } | HookEvent::Respawn | HookEvent::WorldChange => {
                self.presence = Presence::InWorld;
            }
            HookEvent::Timer(_)
            | HookEvent::Chat(_)
            | HookEvent::Stop
            | HookEvent::MenuOpen
            | HookEvent::MenuUpdate
            | HookEvent::MenuClose
            | HookEvent::Click(_)
            | HookEvent::SignOpen => {}
        }
        let moment = Moment {
            facts: &facts,
            now: self.now,
            presence: self.presence,
            leash: Some(leash),
            menu: &menu,
            name: "AfkBot1",
        };
        let before = self.stopwatch.shown();
        let ran = self.instance.handle(event, &moment, &mut self.limiter);
        self.judge_time(before, 1);
        self.judge(&ran);
    }

    /// Holds the time one call took to a hook's and a second's budget.
    /// The stopwatch showed `before` when the call began, and the call ran
    /// at most `hooks` hooks.
    ///
    /// A hook ends at the first reading that finds it past its time, and
    /// is read once more at its end: it takes at most its time and two
    /// stretches more. The stopwatch also moves at the hook's first
    /// reading, which is no stretch of the hook's.
    fn judge_time(&mut self, before: Duration, hooks: usize) {
        let step = self.stopwatch.step();
        let took = self.stopwatch.shown().saturating_sub(before);
        let hooks = u32::try_from(hooks).unwrap_or(u32::MAX);
        let most = limits::HOOK_TIME
            .saturating_add(step.saturating_mul(3))
            .saturating_mul(hooks);
        assert!(took <= most, "a call's hooks took {took:?}");
        if took.is_zero() {
            return;
        }
        self.times.push((self.now, took));
        self.times
            .retain(|(at, _)| time::elapsed(*at, self.now) < Duration::from_secs(1));
        // A second's hooks take its budget at most, and the one that went
        // past it two stretches more. Each hook that read the stopwatch
        // took at least one stretch, so the first readings, which are no
        // hook's, are at most as much again.
        let second: Duration = self.times.iter().map(|(_, took)| *took).sum();
        let most = limits::TIME_PER_SECOND
            .saturating_add(step.saturating_mul(2))
            .saturating_mul(2);
        assert!(second <= most, "a second's hooks took {second:?}");
    }

    /// Holds one call's answer to every property.
    fn judge(&mut self, ran: &Ran) {
        let was_over = self.over;
        match &ran.outcome {
            Outcome::Ran { operations } => {
                assert!(!was_over, "a script that was over ran a hook");
                // A wake runs each due timer as a hook of its own, at most
                // as many as a script has timers.
                let most = limits::OPERATIONS_PER_HOOK
                    .saturating_mul(u64::try_from(limits::MAX_TIMERS).unwrap_or(u64::MAX));
                assert!(*operations <= most, "a wake ran {operations} operations");
                self.hooks.push((self.now, *operations));
                self.hooks
                    .retain(|(at, _)| time::elapsed(*at, self.now) < Duration::from_secs(1));
                let second: u64 = self.hooks.iter().map(|(_, operations)| operations).sum();
                assert!(
                    second <= limits::OPERATIONS_PER_SECOND,
                    "a second held {second} operations"
                );
            }
            Outcome::NotDefined => assert!(!was_over, "a script that was over was asked again"),
            Outcome::Stopped(error) => {
                assert!(!was_over, "a script stopped twice");
                assert_eq!(ran.actions, [Action::ReleaseAll], "a stopped script acted");
                assert_eq!(self.instance.error(), Some(error));
                bounded(error);
                self.over = true;
            }
            Outcome::Ended { error } => {
                assert!(!was_over, "a script ended twice");
                assert_eq!(ran.actions.last(), Some(&Action::ReleaseAll));
                if let Some(error) = error {
                    bounded(error);
                }
                self.over = true;
            }
            Outcome::Over => {
                assert!(was_over, "a running script said it was over");
                assert!(ran.actions.is_empty(), "a script that was over acted");
            }
        }
        if self.over {
            assert_eq!(ran.next_wake, None, "a script that's over wants a wake");
            assert_eq!(self.instance.next_wake(), None);
            assert_eq!(self.instance.timers(), 0, "a script that's over has timers");
            assert_eq!(self.instance.state_size(), (0, 0));
            for input in inputs() {
                assert!(
                    !self.instance.is_held(input),
                    "a script that's over holds {input:?}"
                );
            }
        }
        if let Some(wake) = ran.next_wake {
            assert!(wake >= self.now, "a wake before now");
        }
        for action in &ran.actions {
            self.judge_action(action);
        }
    }

    /// Holds one action to the limits below a script.
    fn judge_action(&mut self, action: &Action) {
        match action {
            Action::Log(line) => {
                assert!(
                    line.chars().count() <= limits::LOG_LINE_CHARS,
                    "a long log line"
                );
                assert!(!line.contains(['\n', '\r']), "a log line of two lines");
            }
            // Whatever a script does, what it sends is one of the mode's
            // declared messages: the declaration's own command, its own
            // text around the values.
            Action::Send { key, message, .. } => {
                let declared = self.program.script().message(key.as_str());
                assert!(
                    declared.is_some(),
                    "a message the mode doesn't declare was sent"
                );
                if let Some(declared) = declared {
                    assert_eq!(message.command_name(), declared.command_name());
                    assert!(message.as_str().encode_utf16().count() <= declared.max_len());
                }
            }
            Action::Expect {
                pattern, window, ..
            } => {
                assert!(
                    self.program
                        .script()
                        .patterns()
                        .get(pattern.as_str())
                        .is_some()
                );
                assert!(
                    *window >= Duration::from_millis(250) && *window <= Duration::from_secs(30)
                );
            }
            Action::LookAt { yaw, pitch } => {
                assert!((-180.0..=180.0).contains(yaw) && (-90.0..=90.0).contains(pitch));
            }
            Action::LookTurn { yaw, pitch } => assert!(yaw.is_finite() && pitch.is_finite()),
            Action::Mount(kind) => assert_eq!(kind.as_str(), "minecraft:minecart"),
            Action::ReleaseAll
            | Action::ReleaseKeys
            | Action::Press { .. }
            | Action::Release(_)
            | Action::LookAtPosition(_)
            | Action::SelectHotbar(_)
            | Action::Dismount
            // A level-1 script never acts in a screen; level 2's are
            // judged by the menus' targets (Plan.md P16.14).
            | Action::Menu(_) => {}
        }
        let Some(kind) = kind_of(action) else { return };
        if let Some((_, before)) = self.last.iter_mut().find(|(known, _)| *known == kind) {
            let gap = time::elapsed(*before, self.now);
            assert!(gap >= kind.minimum(), "two {kind:?} within {gap:?}");
            *before = self.now;
        } else {
            self.last.push((kind, self.now));
        }
    }
}

/// An error's message, where it has one of Rhai's, is one bounded line.
fn bounded(error: &ScriptError) {
    if let ScriptErrorKind::Failed { message } = &error.kind {
        assert!(message.chars().count() <= ScriptError::MAX_MESSAGE_CHARS);
        assert!(!message.contains(['\n', '\r']), "an error of two lines");
    }
}

/// A script's text over any bytes: no panic; an error's place is inside
/// the text and its message one bounded line; what compiles holds the
/// script it was compiled from.
///
/// # Panics
/// When a property breaks.
pub fn script_compile(data: &[u8]) {
    let source = lossy(data);
    let Some(script) = script(&source) else {
        return;
    };
    match compile(&script) {
        Ok(program) => assert_eq!(program.script(), &script),
        Err(error) => placed(&error, script.source().as_str()),
    }
}

/// A compile error's place is one the text has, and its message is one
/// bounded line.
fn placed(error: &CompileError, source: &str) {
    if let Some(at) = error.at {
        let lines = u32::try_from(source.split('\n').count()).unwrap_or(u32::MAX);
        assert!(
            at.line >= 1 && at.line <= lines,
            "line {} of {lines}",
            at.line
        );
        let line = usize::try_from(at.line - 1)
            .ok()
            .and_then(|index| source.split('\n').nth(index))
            .unwrap_or_default();
        // One past the line's end is where "something is missing" points.
        let columns = u32::try_from(line.chars().count()).unwrap_or(u32::MAX);
        assert!(
            at.column >= 1 && at.column <= columns.saturating_add(1),
            "column {} of {columns}",
            at.column
        );
    }
    let shown = error.to_string();
    assert!(!shown.contains(['\n', '\r']), "an error of two lines");
    assert!(shown.chars().count() <= CompileError::MAX_MESSAGE_CHARS + 128);
}

/// A script and a list of events over any bytes: the script's text up to
/// the first zero byte, then three bytes an event (a pause, the facts'
/// shape, the event). No panic, and every property of a run (ADR-0036, D18).
///
/// # Panics
/// When a property breaks.
pub fn script_run(data: &[u8]) {
    let (source, steps) = halves(data);
    let Some(program) = script(&lossy(source)).and_then(|script| compile(&script).ok()) else {
        return;
    };
    let seed = steps.iter().fold(0_u64, |seed, byte| {
        seed.wrapping_mul(31).wrapping_add(u64::from(*byte))
    });
    let mut run = Run::new(Arc::new(program), seed);
    let mut first = true;
    // More events than this tell a fuzzer nothing new and cost it time.
    for step in steps.chunks(3).take(48) {
        let (pause, shape, kind) = match *step {
            [pause, shape, kind] => (pause, shape, kind),
            [pause, shape] => (pause, shape, 4),
            [pause] => (pause, 0, 4),
            _ => continue,
        };
        // Pauses from none to four minutes: within a second, and across
        // the limiter's longest minimum.
        let pause = Duration::from_millis(u64::from(pause).pow(2) * 4);
        let event = event(kind, first);
        first &= !matches!(event, Some(HookEvent::Join { .. }));
        run.step(pause, shape, event.as_ref());
    }
    // And an orderly stop at the end, whatever came before.
    run.step(Duration::ZERO, 0, Some(&HookEvent::Stop));
    assert!(run.over, "a script outlived its stop");
}

/// What the API's functions are called with: Rhai expressions of every
/// kind a script can write, the fitting ones and the wrong ones.
const ARGUMENTS: [&str; 32] = [
    "0",
    "1",
    "-1",
    "9",
    "250",
    "5000",
    "86400000",
    "9223372036854775807",
    "-9223372036854775807",
    "0.0",
    "1.5",
    "-90.5",
    "1.0e300",
    "0.0 / 0.0",
    "1.0 / 0.0",
    "()",
    "true",
    "\"\"",
    "\"minecart\"",
    "\"minecraft:minecart\"",
    "\"balance\"",
    "\"pay\"",
    "\"line\\nbreak \\u202e\"",
    "long",
    "[]",
    "[1, 2, 3]",
    "#{}",
    "#{ amount: 5 }",
    "#{ amount: 2500.5 }",
    "#{ x: 1, y: 64.5, z: -3 }",
    "state",
    "keys",
];

/// The API's functions and properties, as (object, member, whether it's a
/// function), with `input`'s under both a key and a button.
fn members() -> Vec<(String, &'static str, bool)> {
    API.iter()
        .flat_map(|object| {
            let receivers: Vec<String> = match object.name {
                "hooks" => Vec::new(),
                "input" => vec!["keys.forward".to_owned(), "mouse.right".to_owned()],
                // Maps the API hands out, reached as a script reaches them.
                "slot" => vec!["menu.slot(0)".to_owned(), "inventory.slot(36)".to_owned()],
                "time" => vec!["clock.local(\"UTC\")".to_owned()],
                name => vec![name.to_owned()],
            };
            receivers.into_iter().flat_map(move |receiver| {
                object.members.iter().map(move |member| {
                    (
                        receiver.clone(),
                        member.name,
                        member.kind == MemberKind::Method,
                    )
                })
            })
        })
        .collect()
}

/// The script that calls the API as `data` says: two bytes pick a member,
/// then one byte an argument each, as many as the first byte's upper bits
/// ask for.
fn api_calls(data: &[u8]) -> String {
    let members = members();
    let mut source = String::from(
        "fn on_join(first) {\n    let long = \"\";\n    long.pad(300, 'x');\n    state.n = 0;\n",
    );
    let mut bytes = data.iter().copied();
    // As many calls as a hook's operations comfortably hold.
    for _ in 0..40 {
        let (Some(pick), Some(shape)) = (bytes.next(), bytes.next()) else {
            break;
        };
        let Some((receiver, member, is_method)) =
            members.get(usize::from(pick) % members.len().max(1))
        else {
            break;
        };
        let call = if *is_method {
            let count = usize::from(shape % 5);
            let arguments: Vec<&str> = bytes
                .by_ref()
                .take(count)
                .filter_map(|byte| ARGUMENTS.get(usize::from(byte) % ARGUMENTS.len()).copied())
                .collect();
            format!("{receiver}.{member}({})", arguments.join(", "))
        } else {
            format!("{receiver}.{member}")
        };
        // A call that isn't one (a wrong number of arguments, a key the
        // mode doesn't declare) is caught, so the next one still runs;
        // what a call answers goes to `seen`.
        let _ = writeln!(source, "    try {{ seen({call}); }} catch {{ }}");
    }
    source.push_str("}\n");
    // A result's reason is one of the closed list, or empty.
    let listed = reasons().map(|reason| format!("\"{reason}\"")).join(", ");
    let _ = writeln!(
        source,
        "fn seen(answer) {{\n    if type_of(answer) != \"map\" {{ return; }}\n    if !(\"reason\" in answer) {{ return; }}\n    let known = [\"\", {listed}];\n    if !(answer.reason in known) {{ state.n = keys; }}\n}}"
    );
    source
}

/// Any arguments to every function of the script API: no panic, every
/// property of a run, and no result with a reason outside the closed
/// list.
///
/// # Panics
/// When a property breaks.
pub fn script_api(data: &[u8]) {
    let (calls, rest) = halves(data);
    let source = api_calls(calls);
    let program = script_2(&source).map(|script| compile(&script));
    // Whatever the bytes say, the calls are a script that compiles.
    let Some(Ok(program)) = program else {
        assert!(
            program.is_none(),
            "the API's calls don't compile: {program:?}"
        );
        return;
    };
    let shape = rest.first().copied().unwrap_or(0);
    let mut run = Run::new(Arc::new(program), u64::from(shape));
    run.presence = match shape % 4 {
        0 => Presence::Dead,
        1 => Presence::NotInWorld,
        _ => Presence::InWorld,
    };
    let join = HookEvent::Join { first: true };
    // `seen` puts a game object into the state for a reason it doesn't
    // know, which stops the script as "not plain data".
    let facts = facts(shape);
    if let Some(facts) = &facts {
        let menu = MenuView::default();
        let moment = Moment {
            facts,
            now: run.now,
            presence: run.presence,
            leash: None,
            menu: &menu,
            name: "AfkBot1",
        };
        let ran = run.instance.handle(&join, &moment, &mut run.limiter);
        if let Outcome::Stopped(error) = &ran.outcome {
            assert_ne!(
                error.kind,
                ScriptErrorKind::StateNotPlain,
                "a result's reason isn't one of the closed list"
            );
        }
        run.judge(&ran);
    }
    run.step(Duration::from_secs(1), shape, Some(&HookEvent::Stop));
}

/// A value for a declared message, from text: a whole number, a decimal
/// one, or the text itself.
fn given(text: &str) -> Given<'_> {
    if let Ok(whole) = text.parse::<i64>() {
        Given::Integer(whole)
    } else if let Ok(decimal) = text.parse::<f64>() {
        Given::Decimal(decimal)
    } else {
        Given::Text(text)
    }
}

/// A declared message over any bytes: its JSON up to the first zero byte,
/// then a line `name=value` for each value given. No panic; a declaration
/// that's accepted reads back as itself and fits a chat message at its
/// longest; and whatever values are given, what's rendered is that
/// declaration's message: its command, within its longest length.
///
/// # Panics
/// When a property breaks.
pub fn message_template(data: &[u8]) {
    let (declaration, values) = halves(data);
    let Ok(draft) = serde_json::from_slice::<MessageDraft>(declaration) else {
        return;
    };
    let Ok(template) = MessageTemplate::try_from(draft) else {
        return;
    };
    let again = MessageTemplate::try_from(MessageDraft::from(template.clone()));
    assert_eq!(
        again.as_ref(),
        Ok(&template),
        "a declaration doesn't read back"
    );
    assert!(
        template.max_len() <= 256,
        "a declaration longer than a message"
    );
    assert_eq!(template.is_command(), template.command_name().is_some());

    let values = lossy(values);
    let given: Vec<(&str, Given<'_>)> = values
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name, given(value)))
        .collect();
    if let Ok(message) = template.render(&given) {
        assert!(message.as_str().encode_utf16().count() <= template.max_len());
        assert_eq!(message.command_name(), template.command_name());
        assert_eq!(message.is_command(), template.is_command());
        // A value never starts a second line or brings a formatting code.
        assert!(!message.as_str().contains(['\n', '\r', '\u{a7}']));
    }
}

/// A declared chat pattern and a line over any bytes: the pattern's JSON
/// up to the first zero byte, then the line. No panic; a declaration
/// that's accepted reads back as itself; and a match gives exactly the
/// declared captures, each a value its declaration allows.
///
/// # Panics
/// When a property breaks.
pub fn chat_pattern(data: &[u8]) {
    let (declaration, line) = halves(data);
    let Ok(draft) = serde_json::from_slice::<PatternDraft>(declaration) else {
        return;
    };
    let Ok(pattern) = ChatPattern::try_from(draft) else {
        return;
    };
    let again = ChatPattern::try_from(PatternDraft::from(pattern.clone()));
    assert_eq!(
        again.as_ref(),
        Ok(&pattern),
        "a declaration doesn't read back"
    );

    let line = lossy(line);
    let found = pattern.matches(&line);
    assert_eq!(pattern.matches(&line), found, "a line fits and doesn't");
    let Some(values) = found else { return };
    let names: Vec<&Ident> = values.iter().map(|(name, _)| name).collect();
    let declared: Vec<&Ident> = pattern
        .captures()
        .iter()
        .map(fleet_core::script::Capture::name)
        .collect();
    assert_eq!(
        names, declared,
        "a match's values aren't the declared captures"
    );
    for ((_, value), capture) in values.iter().zip(pattern.captures()) {
        allowed(value, &capture.kind());
    }
    // The whole line must fit, and a declaration is one line: nothing
    // with a line break inside it does.
    assert!(!line.trim().contains('\n'), "a line of two lines fits");
    // And as one of a mode's patterns it's found by the same rule.
    if let Ok(patterns) = ChatPatterns::try_new(vec![pattern.clone()]) {
        let first = patterns.first_match(&line, |_| true);
        assert_eq!(first.map(|found| found.values), Some(values));
    }
}

/// A captured value is one its declaration allows: a number in its range,
/// a word of its list.
fn allowed(value: &Captured, kind: &fleet_core::script::CaptureKind) {
    use fleet_core::script::CaptureKind;
    let fits = match (value, kind) {
        (Captured::Integer(number), CaptureKind::Integer { min, max }) => {
            min <= number && number <= max
        }
        (Captured::Amount(number), CaptureKind::Amount { min, max, .. }) => {
            number.is_finite() && min <= number && number <= max
        }
        (Captured::Word(word), CaptureKind::Word { options }) => {
            options.iter().any(|option| option == word.as_str())
        }
        // A value of another kind than its capture declares.
        _ => false,
    };
    assert!(
        fits,
        "a captured value its declaration doesn't allow: {value:?}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_api_member_is_called_under_its_object() {
        let members = members();

        assert!(members.contains(&("keys.forward".to_owned(), "tap", true)));
        assert!(members.contains(&("mouse.right".to_owned(), "hold_until_used", true)));
        assert!(members.contains(&("chat".to_owned(), "expect", true)));
        assert!(members.contains(&("bot".to_owned(), "health", false)));
        assert!(members.contains(&("menu".to_owned(), "click", true)));
        assert!(members.contains(&("menu.slot(0)".to_owned(), "lore", false)));
        assert!(members.iter().all(|(receiver, ..)| receiver != "hooks"));
    }

    #[test]
    fn the_api_calls_are_a_script_whatever_the_bytes() {
        for data in [&b""[..], &[0, 0], &[255; 300], b"\x10\x04\x00\x01\x02\x03"] {
            let source = api_calls(data);

            assert!(
                script_2(&source)
                    .and_then(|script| compile(&script).ok())
                    .is_some(),
                "{source}"
            );
        }
    }

    #[test]
    #[should_panic(expected = "two Mount within")]
    fn two_actions_of_a_kind_too_close_fail() {
        let program = script("fn on_join(first) { }").and_then(|script| compile(&script).ok());
        let mut run = Run::new(Arc::new(program.unwrap()), 0);
        let mount = Action::Mount(id("minecart").unwrap());

        run.judge_action(&mount);
        run.now = time::add(run.now, Duration::from_millis(100));
        run.judge_action(&mount);
    }

    /// A run of `source` whose stopwatch moves 100 µs at each reading.
    fn timed(source: &str) -> Run {
        let program = script(source).and_then(|script| compile(&script).ok());
        let run = Run::new(Arc::new(program.unwrap()), 100);
        assert_eq!(run.stopwatch.step(), Duration::from_micros(100));
        run
    }

    #[test]
    #[should_panic(expected = "a call's hooks took 6ms")]
    fn a_hook_that_took_longer_than_a_hook_may_fails() {
        let mut run = timed("fn on_join(first) { }");
        let before = run.stopwatch.shown();

        // What an instance that didn't end its hook in time would show.
        for _ in 0..60 {
            let _ = run.stopwatch.elapsed();
        }
        run.judge_time(before, 1);
    }

    #[test]
    #[should_panic(expected = "a second's hooks took 41ms")]
    fn a_second_whose_hooks_took_longer_than_they_may_fails() {
        let mut run = timed("fn on_join(first) { }");

        // Each within a hook's time; eleven of them in one second aren't.
        for _ in 0..11 {
            let before = run.stopwatch.shown();
            for _ in 0..41 {
                let _ = run.stopwatch.elapsed();
            }
            run.now = time::add(run.now, Duration::from_millis(10));
            run.judge_time(before, 1);
        }
    }

    #[test]
    fn a_busy_script_on_a_slow_stopwatch_stops_for_its_time_within_the_properties() {
        let mut run = timed("fn on_join(first) { loop { } }");

        run.step(Duration::ZERO, 0, Some(&HookEvent::Join { first: true }));

        assert!(run.over);
        assert_eq!(
            run.instance.error().map(|error| &error.kind),
            Some(&ScriptErrorKind::Limit(fleet_script::Limit::HookTime))
        );
    }

    #[test]
    #[should_panic(expected = "a message the mode doesn't declare was sent")]
    fn a_message_the_mode_doesnt_declare_fails() {
        let program = script("fn on_join(first) { }").and_then(|script| compile(&script).ok());
        let mut run = Run::new(Arc::new(program.unwrap()), 0);

        run.judge_action(&Action::Send {
            id: fleet_core::script::SendId(0),
            key: Ident::try_from("other").unwrap(),
            message: fleet_core::chat::ChatMessage::try_from("/op me").unwrap(),
        });
    }

    #[test]
    #[should_panic(expected = "column 40 of 3")]
    fn an_errors_place_outside_the_text_fails() {
        placed(
            &CompileError {
                kind: fleet_script::CompileErrorKind::Eval,
                at: Some(fleet_script::SourcePosition {
                    line: 1,
                    column: 40,
                }),
            },
            "abc",
        );
    }

    #[test]
    #[should_panic(expected = "a captured value its declaration doesn't allow: Integer(70)")]
    fn a_captured_number_outside_its_range_fails() {
        allowed(
            &Captured::Integer(70),
            &fleet_core::script::CaptureKind::Integer { min: 0, max: 60 },
        );
    }

    #[test]
    #[should_panic(expected = "a captured value its declaration doesn't allow")]
    fn a_captured_value_of_another_kind_than_declared_fails() {
        allowed(
            &Captured::Amount(5.0),
            &fleet_core::script::CaptureKind::Integer { min: 0, max: 60 },
        );
    }

    #[test]
    fn a_captured_value_inside_its_declaration_passes() {
        allowed(
            &Captured::Integer(60),
            &fleet_core::script::CaptureKind::Integer { min: 0, max: 60 },
        );
    }

    #[test]
    fn a_given_value_is_a_number_where_it_reads_as_one() {
        assert_eq!(given("42"), Given::Integer(42));
        assert_eq!(given("2.5"), Given::Decimal(2.5));
        assert_eq!(given("farm"), Given::Text("farm"));
    }
}
