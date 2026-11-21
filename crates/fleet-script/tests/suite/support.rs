//! What the suite's tests share: scripts from text, and a fake world that
//! hands an instance its facts and does what its actions ask.

use core::time::Duration;
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use fleet_core::mode::HotbarSlot;
use fleet_core::script::{
    ActionLimiter, BlockPosition, Blocks, BotFacts, CaptureDraft, CaptureKind, DecimalMark,
    EntityFact, HeldItem, HookEvent, Identifier, Input, Leash, MessageDraft, MouseButton,
    PatternDraft, Position, Script, ScriptDraft, Snapshot, Stopwatch, TimerName, ValueDraft,
    ValueKind, When,
};
use fleet_core::time;
use fleet_script::{Action, Instance, Moment, Outcome, Presence, PressKind, Program, Ran, compile};
use fleet_testkit::stopwatch::ManualStopwatch;

/// A scripted mode's body as written, with nothing declared.
pub(crate) fn draft(source: &str) -> ScriptDraft {
    ScriptDraft {
        api: 1,
        source: source.to_owned(),
        leash: 8,
        tool_guard: 2,
        block_guard: Vec::new(),
        messages: Vec::new(),
        patterns: Vec::new(),
    }
}

/// A declared message without values.
pub(crate) fn message(key: &str, text: &str) -> MessageDraft {
    MessageDraft {
        key: key.to_owned(),
        text: text.to_owned(),
        values: Vec::new(),
    }
}

/// The balance example's declarations: `/balance`, `/pay AfkMain {amount}`,
/// the reply `Balance: ${amount}`, and a line that may come at any time.
pub(crate) fn balance_draft(source: &str) -> ScriptDraft {
    let mut draft = draft(source);
    draft.messages = vec![
        message("balance", "/balance"),
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
        message("hello", "Hello there"),
    ];
    draft.patterns = vec![
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
    ];
    draft
}

/// The script of `draft`, which must be one fleet-core accepts.
pub(crate) fn script_of(draft: ScriptDraft) -> Script {
    Script::try_from(draft).expect("the draft is a scripted mode's body")
}

/// `source` compiled with nothing declared; it must compile.
pub(crate) fn program(source: &str) -> Arc<Program> {
    program_of(draft(source))
}

/// `draft` compiled; it must compile.
pub(crate) fn program_of(draft: ScriptDraft) -> Arc<Program> {
    Arc::new(compile(&script_of(draft)).expect("the script compiles"))
}

/// A stopwatch that stands still: no hook it times ever takes any time.
pub(crate) fn still() -> Arc<dyn Stopwatch> {
    Arc::new(ManualStopwatch::standing())
}

/// An instance of `source` with a fixed seed, timed by `stopwatch`.
pub(crate) fn timed(source: &str, stopwatch: &ManualStopwatch) -> Instance {
    Instance::new(program(source), 7, Arc::new(stopwatch.clone()))
}

/// An instance of `source` with a fixed seed.
pub(crate) fn instance(source: &str) -> Instance {
    Instance::new(program(source), 7, crate::support::still())
}

/// A registry identifier from text.
pub(crate) fn id(text: &str) -> Identifier {
    Identifier::try_from(text).expect("an identifier")
}

/// A timer's name from text.
pub(crate) fn timer(name: &str) -> TimerName {
    TimerName::try_from(name).expect("a timer's name")
}

/// A point in the world.
pub(crate) fn at(x: f64, y: f64, z: f64) -> Position {
    Position::try_new(x, y, z).expect("a position")
}

/// The moment the tests start at.
pub(crate) fn start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 17, 12, 0, 0).unwrap()
}

/// The texts of the log lines among `actions`.
pub(crate) fn logged(actions: &[Action]) -> Vec<&str> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::Log(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// A pickaxe with `durability` uses left.
pub(crate) fn pickaxe(durability: u32) -> HeldItem {
    HeldItem {
        id: id("iron_pickaxe"),
        count: 1,
        durability: Some(durability),
    }
}

/// An angle as the facts hold it. Written out and read back: a cast would
/// do, and the lints here allow none.
fn narrow(angle: f64) -> f32 {
    angle.to_string().parse().expect("an angle")
}

/// The block a coordinate is in.
fn block_of(coordinate: f64) -> i32 {
    format!("{:.0}", coordinate.floor())
        .parse()
        .expect("a coordinate inside the world")
}

/// A fake world: one bot, what's near it, and time.
///
/// It gives an instance its facts and performs the simple part of what a
/// hook asks for: a hotbar slot, a look, a vehicle. Everything a hook
/// asked for is kept in `performed`.
pub(crate) struct World {
    pub(crate) bot: BotFacts,
    pub(crate) entities: Vec<EntityFact>,
    pub(crate) blocks: Vec<(BlockPosition, Identifier)>,
    pub(crate) hotbar: [Option<HeldItem>; 9],
    pub(crate) now: DateTime<Utc>,
    pub(crate) presence: Presence,
    pub(crate) leash: Option<Leash>,
    pub(crate) limiter: ActionLimiter,
    pub(crate) performed: Vec<Action>,
    pub(crate) sent: Vec<String>,
    pub(crate) log: Vec<String>,
}

impl World {
    /// A bot standing at the origin of the overworld with empty hands.
    pub(crate) fn new() -> Self {
        Self {
            bot: BotFacts {
                health: 20.0,
                food: 20,
                position: at(0.5, 64.0, 0.5),
                yaw: 0.0,
                pitch: 0.0,
                dimension: id("overworld"),
                riding: None,
                held: None,
                selected: HotbarSlot::FIRST,
                looking_at: None,
            },
            entities: Vec::new(),
            blocks: Vec::new(),
            hotbar: [const { None }; 9],
            now: start(),
            presence: Presence::InWorld,
            leash: None,
            limiter: ActionLimiter::new(),
            performed: Vec::new(),
            sent: Vec::new(),
            log: Vec::new(),
        }
    }

    /// What a hook sees right now.
    pub(crate) fn snapshot(&self) -> Snapshot {
        let feet = BlockPosition {
            x: block_of(self.bot.position.x()),
            y: block_of(self.bot.position.y()),
            z: block_of(self.bot.position.z()),
        };
        let blocks = Blocks::read(feet, |place| {
            self.blocks
                .iter()
                .find(|(known, _)| *known == place)
                .map(|(_, kind)| kind.clone())
        });
        Snapshot::new(self.bot.clone(), self.entities.clone(), blocks)
    }

    /// Hands `event` to `instance` and performs what its hook asked for.
    pub(crate) fn run(&mut self, instance: &mut Instance, event: &HookEvent) -> Ran {
        match event {
            HookEvent::Death => self.presence = Presence::Dead,
            HookEvent::Join { .. } | HookEvent::Respawn | HookEvent::WorldChange => {
                self.presence = Presence::InWorld;
            }
            HookEvent::Timer(_) | HookEvent::Chat(_) | HookEvent::Stop => {}
        }
        let facts = self.snapshot();
        let moment = Moment {
            facts: &facts,
            now: self.now,
            presence: self.presence,
            leash: self.leash,
        };
        let ran = instance.handle(event, &moment, &mut self.limiter);
        self.perform(&ran.actions);
        ran
    }

    /// Wakes `instance` now and performs what its timers' hooks asked for.
    pub(crate) fn wake(&mut self, instance: &mut Instance) -> Ran {
        let facts = self.snapshot();
        let moment = Moment {
            facts: &facts,
            now: self.now,
            presence: self.presence,
            leash: self.leash,
        };
        let ran = instance.wake(&moment, &mut self.limiter);
        self.perform(&ran.actions);
        ran
    }

    /// Lets `length` pass, waking `instance` at each moment it asks for on
    /// the way. Returns what each wake did; stops early when the script
    /// does.
    pub(crate) fn pass(&mut self, instance: &mut Instance, length: Duration) -> Vec<Ran> {
        let end = time::add(self.now, length);
        let mut wakes = Vec::new();
        // Far more wakes than any test needs; a wake that asks for the
        // same moment again would otherwise never end.
        for _ in 0..10_000 {
            match instance.next_wake() {
                Some(due) if due <= end => {
                    self.now = self.now.max(due);
                    let ran = self.wake(instance);
                    let over = !matches!(ran.outcome, Outcome::Ran { .. } | Outcome::NotDefined);
                    wakes.push(ran);
                    if over {
                        break;
                    }
                }
                _ => break,
            }
        }
        self.now = end;
        wakes
    }

    /// Does the simple part of `actions` and remembers them all.
    fn perform(&mut self, actions: &[Action]) {
        for action in actions {
            match action {
                Action::SelectHotbar(slot) => {
                    self.bot.selected = *slot;
                    self.bot.held = self.hotbar.get(usize::from(slot.get())).cloned().flatten();
                }
                Action::LookAt { yaw, pitch } => {
                    self.bot.yaw = narrow(*yaw);
                    self.bot.pitch = narrow(*pitch);
                }
                Action::LookTurn { yaw, pitch } => {
                    self.bot.yaw += narrow(*yaw);
                    self.bot.pitch = (self.bot.pitch + narrow(*pitch)).clamp(-90.0, 90.0);
                }
                Action::Mount(kind) => {
                    let free = self.entities.iter_mut().find(|entity| {
                        entity.kind == *kind
                            && !entity.occupied
                            && self.bot.position.distance_to(entity.position) <= 4.0
                    });
                    if let (None, Some(vehicle)) = (&self.bot.riding, free) {
                        vehicle.occupied = true;
                        self.bot.riding = Some(kind.clone());
                    }
                }
                Action::Dismount => self.bot.riding = None,
                Action::Send { message, .. } => self.sent.push(message.as_str().to_owned()),
                Action::Log(text) => self.log.push(text.clone()),
                Action::ReleaseAll
                | Action::ReleaseKeys
                | Action::Press { .. }
                | Action::Release(_)
                | Action::LookAtPosition(_)
                | Action::Expect { .. } => {}
            }
        }
        self.performed.extend(actions.iter().cloned());
    }

    /// Puts a free minecart at `position`.
    pub(crate) fn minecart(&mut self, position: Position) {
        self.entities.push(EntityFact {
            kind: id("minecart"),
            position,
            occupied: false,
        });
    }

    /// The presses among everything performed, as (input, kind).
    pub(crate) fn presses(&self) -> Vec<(Input, PressKind)> {
        self.performed
            .iter()
            .filter_map(|action| match action {
                Action::Press { input, kind } => Some((*input, *kind)),
                _ => None,
            })
            .collect()
    }

    /// How often the left button was tapped.
    pub(crate) fn left_taps(&self) -> usize {
        self.presses()
            .iter()
            .filter(|press| **press == (Input::Mouse(MouseButton::Left), PressKind::Tap))
            .count()
    }
}

/// Runs `source`'s `on_join(true)` once in a fresh world and returns what
/// it did, with the world it ran in.
pub(crate) fn join(source: &str) -> (Ran, World, Instance) {
    let mut world = World::new();
    let mut instance = instance(source);
    let ran = world.run(&mut instance, &HookEvent::Join { first: true });
    (ran, world, instance)
}

/// The error a run stopped with; panics if it didn't stop.
pub(crate) fn stopped(ran: &Ran) -> &fleet_script::ScriptError {
    match &ran.outcome {
        Outcome::Stopped(error) => error,
        other => panic!("the script didn't stop: {other:?}"),
    }
}

/// The operations a run used; panics if its hook didn't run to its end.
pub(crate) fn operations(ran: &Ran) -> u64 {
    match &ran.outcome {
        Outcome::Ran { operations } => *operations,
        other => panic!("the hook didn't run to its end: {other:?}"),
    }
}
