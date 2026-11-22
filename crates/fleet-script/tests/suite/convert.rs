//! "Convert to script" (Plan.md P15.27; ADR-0036, D17; ADR-0046): a step
//! mode's script, run beside the plan it replaces, schedule for schedule.
//!
//! Each case runs the step mode's `ModePlan` and the converted script's
//! instance over the same session: a join, the repeating steps' runs, and
//! deaths and respawns between two of them. What the plan hands out is
//! turned into what a script does for it and put through the limits below
//! a script, which a script never gets past. The script must then have
//! done exactly that, at the same moments and in the same order.
//!
//! The runtime's own releases (everything before `on_join`, `on_death`
//! and `on_respawn`; a step mode's let-go at a respawn) aren't compared:
//! they're the runtimes', not the steps'.

use core::time::Duration;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::chat::ChatMessage;
use fleet_core::mode::{
    Action as Step, GameAction, HotbarSlot, ModeDefinition, ModeDraft, ModePlan, PlannedAction,
    Schedule, Step as ModeStep,
};
use fleet_core::script::{
    ActionLimiter, EntityFact, HookEvent, Input, Key, Limited, LookingAt, MouseButton, Press,
    Script, ScriptDraft, TemplateError,
};
use fleet_core::time;
use fleet_script::{
    Action, ConvertError, Instance, Note, NoteKind, Outcome, PressKind, compile, convert,
};
use proptest::prelude::*;
use rand::SeedableRng as _;
use rand::rngs::StdRng;
use rstest::rstest;

use crate::support::{World, at, id, start, still};

// --- The two sides ------------------------------------------------------------------------

/// What a session did, as a script does it.
#[derive(Debug, Clone, PartialEq)]
enum Did {
    Look(f32, f32),
    Turn(f32, f32),
    Press(Input, PressKind),
    Release(Input),
    Hotbar(u8),
    Mount,
    Send(String),
}

/// What the bot's crosshair is on, all session long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Crosshair {
    Nothing,
    Entity,
}

/// A session: the join at 0, then each death and its respawn, and the end,
/// in milliseconds.
#[derive(Debug, Clone)]
struct Course {
    lives: Vec<(u64, u64)>,
    end: u64,
}

impl Course {
    /// A death at 20.51 s and its respawn at 20.74 s: between two runs of
    /// every interval the cases use (200, 250, 500, 750, 1,000 and 30,000
    /// ms), so no run is due while the bot is dead. Then 25 s in all, or
    /// 95 s with a chat that repeats.
    fn one_death(end: u64) -> Self {
        Self {
            lives: vec![(20_510, 20_740)],
            end,
        }
    }

    /// Three deaths, each between two runs as in [`one_death`](Self::one_death):
    /// the second respawn 9.5 s after the first, inside an on-respawn
    /// chat's 30 s, and the third 40 s after the first.
    fn three_deaths() -> Self {
        Self {
            lives: vec![(20_510, 20_740), (30_010, 30_240), (60_510, 60_740)],
            end: 95_000,
        }
    }

    /// No death: a join and `end` milliseconds.
    fn alive(end: u64) -> Self {
        Self {
            lives: Vec::new(),
            end,
        }
    }

    /// The deaths and respawns in order, each with whether it's a respawn.
    fn events(&self) -> Vec<(u64, bool)> {
        self.lives
            .iter()
            .flat_map(|(death, respawn)| [(*death, false), (*respawn, true)])
            .collect()
    }
}

/// `ms` milliseconds after the start.
fn after(ms: u64) -> DateTime<Utc> {
    time::add(start(), Duration::from_millis(ms))
}

/// The milliseconds from the start to `moment`, a part of one counted as a
/// whole: a plan draws a jittered gap to the nanosecond, a script's timer
/// to the millisecond.
fn millis(moment: DateTime<Utc>) -> u64 {
    let since = moment - start();
    let whole = since.num_milliseconds();
    let up = since > chrono::TimeDelta::milliseconds(whole);
    u64::try_from(whole).unwrap() + u64::from(up)
}

/// What the plan hands out over `course`, as a script does it, when it
/// does: a swing that would hit an entity, and an attack with no entity
/// under the crosshair, do nothing.
fn planned(definition: &ModeDefinition, course: &Course, crosshair: Crosshair) -> Vec<(u64, Did)> {
    let mut rng = StdRng::seed_from_u64(1);
    let (mut plan, first) = ModePlan::start(definition, after(0), &mut rng);
    let mut done = Vec::new();
    record_plan(&mut done, 0, first.actions, crosshair);
    let mut next = first.next_due;
    let mut events = course.events().into_iter().peekable();
    let mut dead_since = None;
    loop {
        // The plan ticks at the exact moment it's due, as the runner sleeps
        // until it.
        let due = next.filter(|due| millis(*due) <= course.end);
        let event = events.peek().copied();
        match (due, event) {
            (Some(due), event) if event.is_none_or(|(at, _)| millis(due) < at) => {
                let at = millis(due);
                assert!(
                    dead_since.is_none(),
                    "a run is due at {at} ms, while the bot is dead: the course must avoid it"
                );
                let tick = plan.tick(due, &mut rng);
                record_plan(&mut done, at, tick.actions, crosshair);
                next = tick.next_due;
            }
            (_, Some((at, respawn))) => {
                events.next();
                if respawn {
                    dead_since = None;
                    if let Some(respawn) = plan.respawned(after(at), &mut rng) {
                        record_plan(&mut done, at, respawn.actions, crosshair);
                    }
                } else {
                    dead_since = Some(at);
                }
            }
            (None, None) => return done,
            (Some(_), None) => unreachable!("matched above"),
        }
    }
}

fn record_plan(
    done: &mut Vec<(u64, Did)>,
    at: u64,
    actions: Vec<PlannedAction>,
    crosshair: Crosshair,
) {
    let on_entity = crosshair == Crosshair::Entity;
    for action in actions {
        let did = match action {
            PlannedAction::Chat(message) => Some(Did::Send(message.as_str().to_owned())),
            PlannedAction::Game(game) => match game {
                GameAction::Look { yaw, pitch } => Some(Did::Look(yaw, pitch)),
                GameAction::Turn { yaw, pitch } => Some(Did::Turn(yaw, pitch)),
                GameAction::Jump => Some(Did::Press(Input::Key(Key::Jump), PressKind::Tap)),
                GameAction::Sneak { on: true } => {
                    Some(Did::Press(Input::Key(Key::Sneak), PressKind::Hold))
                }
                GameAction::Sneak { on: false } => Some(Did::Release(Input::Key(Key::Sneak))),
                GameAction::SwingArm => (!on_entity).then_some(LEFT_TAP),
                GameAction::AttackFacingEntity => on_entity.then_some(LEFT_TAP),
                GameAction::UseItem => {
                    Some(Did::Press(Input::Mouse(MouseButton::Right), PressKind::Tap))
                }
                GameAction::HoldUse { on: true } => Some(Did::Press(
                    Input::Mouse(MouseButton::Right),
                    PressKind::Hold,
                )),
                GameAction::HoldUse { on: false } => {
                    Some(Did::Release(Input::Mouse(MouseButton::Right)))
                }
                GameAction::SelectHotbarSlot { slot } => Some(Did::Hotbar(slot.get())),
                GameAction::EnterMinecart => Some(Did::Mount),
                other => panic!("a plan never hands out {other:?}"),
            },
        };
        done.extend(did.map(|did| (at, did)));
    }
}

const LEFT_TAP: Did = Did::Press(Input::Mouse(MouseButton::Left), PressKind::Tap);

/// What `planned` leaves when a script's inputs do it over `course`:
/// - a key or button that's down already isn't pressed again by a hold, as
///   in vanilla, where a held button can't be pressed; a tap of one lets
///   go of it. Nothing is down after a join, a death or a respawn, since
///   the runtime lets go of everything before those hooks;
/// - every press, look, slot, mount and plain chat goes through the limits
///   below a script, which a script never gets past.
fn as_a_script_does(planned: Vec<(u64, Did)>, course: &Course) -> Vec<(u64, Did)> {
    let mut limiter = ActionLimiter::new();
    let mut held: Vec<Input> = Vec::new();
    let mut lets_go = course.events().into_iter().map(|(at, _)| at).peekable();
    planned
        .into_iter()
        .filter(|(at, did)| {
            while lets_go.next_if(|moment| moment <= at).is_some() {
                held.clear();
            }
            match did {
                Did::Press(input, PressKind::Hold) if held.contains(input) => return false,
                Did::Release(input) => {
                    held.retain(|down| down != input);
                    return true;
                }
                _ => {}
            }
            let taken = kind_of(did).is_none_or(|kind| limiter.try_act(kind, after(*at)).is_ok());
            match did {
                Did::Press(input, PressKind::Hold) if taken => held.push(*input),
                Did::Press(input, PressKind::Tap) if taken => held.retain(|down| down != input),
                _ => {}
            }
            taken
        })
        .collect()
}

/// The limiter's kind of `did`; none for a release and a command.
fn kind_of(did: &Did) -> Option<Limited> {
    match did {
        Did::Look(..) => Some(Limited::LookAt),
        Did::Turn(..) => Some(Limited::LookTurn),
        Did::Press(input, kind) => Some(Limited::of_press(
            *input,
            match kind {
                PressKind::Tap => Press::Tap,
                PressKind::Hold | PressKind::HoldUntilUsed => Press::Hold,
            },
        )),
        Did::Release(_) => None,
        Did::Hotbar(_) => Some(Limited::Hotbar),
        Did::Mount => Some(Limited::Mount),
        Did::Send(text) => (!text.starts_with('/')).then_some(Limited::PlainChat),
    }
}

/// What the converted script does over `course`, in a fake world whose
/// crosshair is on `crosshair` and with seed `seed`.
fn scripted(script: &Script, course: &Course, crosshair: Crosshair, seed: u64) -> Vec<(u64, Did)> {
    let program = Arc::new(compile(script).expect("a converted script compiles"));
    let mut instance = Instance::new(program, seed, still());
    let mut world = World::new();
    if crosshair == Crosshair::Entity {
        world.bot.looking_at = Some(LookingAt::Entity {
            kind: id("cow"),
            at: at(2.5, 64.0, 0.5),
        });
    }
    world.now = after(0);
    let mut done = Vec::new();
    let ran = world.run(&mut instance, &HookEvent::Join { first: true });
    record_script(&mut done, 0, &ran);
    let mut events = course.events().into_iter().peekable();
    loop {
        let due = instance
            .next_wake()
            .map(millis)
            .filter(|due| *due <= course.end);
        let event = events.peek().copied();
        let (at, ran) = match (due, event) {
            (Some(due), event) if event.is_none_or(|(at, _)| due < at) => {
                // A timer that came due while the bot was dead wakes it at
                // once after the respawn: time never goes back.
                let at = due.max(millis(world.now));
                world.now = after(at);
                (at, world.wake(&mut instance))
            }
            (_, Some((at, respawn))) => {
                events.next();
                world.now = after(at);
                let event = if respawn {
                    HookEvent::Respawn
                } else {
                    HookEvent::Death
                };
                (at, world.run(&mut instance, &event))
            }
            (None, None) => return done,
            (Some(_), None) => unreachable!("matched above"),
        };
        record_script(&mut done, at, &ran);
    }
}

fn record_script(done: &mut Vec<(u64, Did)>, at: u64, ran: &fleet_script::Ran) {
    assert!(
        matches!(ran.outcome, Outcome::Ran { .. } | Outcome::NotDefined),
        "the converted script stopped: {:?}",
        ran.outcome
    );
    for action in &ran.actions {
        let did = match action {
            // The runtime's own, before a hook that puts the bot somewhere new.
            Action::ReleaseAll => None,
            Action::Press { input, kind } => Some(Did::Press(*input, *kind)),
            Action::Release(input) => Some(Did::Release(*input)),
            Action::LookAt { yaw, pitch } => Some(Did::Look(narrow(*yaw), narrow(*pitch))),
            Action::LookTurn { yaw, pitch } => Some(Did::Turn(narrow(*yaw), narrow(*pitch))),
            Action::SelectHotbar(slot) => Some(Did::Hotbar(slot.get())),
            Action::Mount(kind) => {
                assert_eq!(kind.as_str(), "minecraft:minecart");
                Some(Did::Mount)
            }
            Action::Send { message, .. } => Some(Did::Send(message.as_str().to_owned())),
            other => panic!("a converted script never asks for {other:?}"),
        };
        done.extend(did.map(|did| (at, did)));
    }
}

/// An angle as fleet-mc reads it: the nearest `f32`.
fn narrow(angle: f64) -> f32 {
    angle.to_string().parse().unwrap()
}

/// Holds `scripted` equal to `expected`, a turn to any amount within
/// `turns`' maxima.
fn assert_same(scripted: &[(u64, Did)], expected: &[(u64, Did)], turns: (f32, f32)) {
    let shown = || format!("the script did:\n{scripted:#?}\nthe plan, limited:\n{expected:#?}");
    assert_eq!(scripted.len(), expected.len(), "{}", shown());
    for ((at, did), (planned_at, planned)) in scripted.iter().zip(expected) {
        assert_eq!(at, planned_at, "{}", shown());
        match (did, planned) {
            (Did::Turn(yaw, pitch), Did::Turn(..)) => {
                assert!(
                    yaw.abs() <= turns.0 && pitch.abs() <= turns.1,
                    "{}",
                    shown()
                );
            }
            _ => assert_eq!(did, planned, "{}", shown()),
        }
    }
}

/// Runs `definition` both ways over `course` and holds them equal, against
/// the plan of `as_planned`: the definition itself unless the converter
/// raised an interval. Returns what both did.
fn same_as_the_plan(
    definition: &ModeDefinition,
    as_planned: &ModeDefinition,
    course: &Course,
    crosshair: Crosshair,
) -> Vec<(u64, Did)> {
    let conversion = convert(definition).expect("the mode converts");
    let script = scripted(conversion.script(), course, crosshair, 7);
    let expected = as_a_script_does(planned(as_planned, course, crosshair), course);
    assert_same(&script, &expected, largest_turn(definition));
    script
}

/// Whether `action` holds an input down: a hold presses it once, and again
/// only after something let go of it.
fn holds(action: &Step) -> bool {
    matches!(
        action,
        Step::Sneak { on: true } | Step::HoldUse { on: true }
    )
}

/// Whether `action` does anything with the crosshair on `crosshair`: a
/// swing only off an entity, an attack only on one.
fn acts(action: &Step, crosshair: Crosshair) -> bool {
    match action {
        Step::SwingArm => crosshair == Crosshair::Nothing,
        Step::AttackFacingEntity => crosshair == Crosshair::Entity,
        _ => true,
    }
}

/// The largest turn any of `definition`'s steps makes.
fn largest_turn(definition: &ModeDefinition) -> (f32, f32) {
    definition
        .steps()
        .iter()
        .fold((0.0, 0.0), |(yaw, pitch), step| match step.action {
            Step::RotateRandom { max_yaw, max_pitch } => (yaw.max(max_yaw), pitch.max(max_pitch)),
            _ => (yaw, pitch),
        })
}

// --- Steps ----------------------------------------------------------------------------------

fn ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

fn every(interval: u64) -> Schedule {
    Schedule::Every {
        interval: ms(interval),
        jitter: Duration::ZERO,
    }
}

fn jittered(interval: u64, jitter: u64) -> Schedule {
    Schedule::Every {
        interval: ms(interval),
        jitter: ms(jitter),
    }
}

fn step(action: Step, schedule: Schedule) -> ModeStep {
    ModeStep {
        action,
        schedule,
        probability: 100,
    }
}

fn mode(steps: Vec<ModeStep>) -> ModeDefinition {
    ModeDraft { steps }.validate().expect("a valid step mode")
}

fn slot(n: u8) -> HotbarSlot {
    HotbarSlot::try_from(n).unwrap()
}

fn chat(text: &str) -> Step {
    Step::SendChat {
        message: text.parse::<ChatMessage>().unwrap(),
    }
}

/// A step mode's shortest interval for `action`.
fn step_minimum(action: &Step) -> u64 {
    match action {
        Step::UseItem => 200,
        Step::AttackFacingEntity | Step::HoldUse { .. } => 500,
        Step::SendChat { .. } => 30_000,
        _ => 250,
    }
}

/// A script's shortest interval for `action`: a timer's 250 ms, and a left
/// click's 500 ms for a swing.
fn script_minimum(action: &Step) -> u64 {
    match action {
        Step::SwingArm => 500,
        other => step_minimum(other).max(250),
    }
}

/// One step of every kind, each a case of the matrix below.
fn kinds() -> Vec<(&'static str, Step)> {
    vec![
        (
            "look",
            Step::Look {
                yaw: -90.5,
                pitch: 12.25,
            },
        ),
        (
            "rotate",
            Step::RotateRandom {
                max_yaw: 30.0,
                max_pitch: 10.5,
            },
        ),
        ("jump", Step::Jump),
        ("sneak", Step::Sneak { on: true }),
        ("stop sneaking", Step::Sneak { on: false }),
        ("swing", Step::SwingArm),
        ("use", Step::UseItem),
        ("hold use", Step::HoldUse { on: true }),
        ("let go of use", Step::HoldUse { on: false }),
        ("attack", Step::AttackFacingEntity),
        ("hotbar", Step::SelectHotbarSlot { slot: slot(4) }),
        ("plain chat", chat("hello there")),
        ("a command", chat("/login hunter2")),
        ("enter a minecart", Step::EnterMinecart),
    ]
}

// --- Every kind of step, on every schedule -----------------------------------------------

#[rstest]
fn at_start_every_kind_of_step_does_what_the_plan_does(
    #[values(Crosshair::Nothing, Crosshair::Entity)] crosshair: Crosshair,
) {
    for (name, action) in kinds() {
        let acts = acts(&action, crosshair);
        let definition = mode(vec![step(action, Schedule::AtStart)]);

        let did = same_as_the_plan(
            &definition,
            &definition,
            &Course::one_death(25_000),
            crosshair,
        );

        // Once at start, and never again in the session: not at the respawn.
        let expected: &[u64] = if acts { &[0] } else { &[] };
        let times: Vec<u64> = did.iter().map(|(at, _)| *at).collect();
        assert_eq!(times, expected, "{name}: {did:?}");
    }
}

#[rstest]
fn repeating_every_kind_of_step_does_what_the_plan_does(
    #[values(Crosshair::Nothing, Crosshair::Entity)] crosshair: Crosshair,
) {
    for (name, action) in kinds() {
        let acts = acts(&action, crosshair);
        let holds = holds(&action);
        let interval = step_minimum(&action);
        let raised = script_minimum(&action);
        let end = if interval >= 30_000 { 95_000 } else { 25_000 };
        let definition = mode(vec![step(action.clone(), every(interval))]);
        let as_planned = mode(vec![step(action, every(raised))]);
        let course = Course::one_death(end);

        let did = same_as_the_plan(&definition, &as_planned, &course, crosshair);

        // Every `raised` milliseconds from the join, across the death; a
        // hold once, and once more after the respawn let go of it.
        let respawn = course.lives[0].1;
        let runs = (1..=end / raised).map(|n| n * raised);
        let expected: Vec<u64> = match (acts, holds) {
            (false, _) => Vec::new(),
            (true, false) => runs.collect(),
            (true, true) => vec![raised, runs.clone().find(|at| *at > respawn).unwrap()],
        };
        let times: Vec<u64> = did.iter().map(|(at, _)| *at).collect();
        assert_eq!(times, expected, "{name}");
    }
}

#[rstest]
fn on_respawn_every_kind_of_step_does_what_the_plan_does(
    #[values(Crosshair::Nothing, Crosshair::Entity)] crosshair: Crosshair,
) {
    for (_, action) in kinds() {
        let definition = mode(vec![step(action, Schedule::OnRespawn)]);
        same_as_the_plan(&definition, &definition, &Course::three_deaths(), crosshair);
    }
}

#[test]
fn an_on_respawn_chat_goes_at_most_once_every_30_seconds_as_the_step_did() {
    for text in ["hello there", "/spawn"] {
        let definition = mode(vec![step(chat(text), Schedule::OnRespawn)]);
        let script = scripted(
            convert(&definition).unwrap().script(),
            &Course::three_deaths(),
            Crosshair::Nothing,
            7,
        );

        // The first respawn and the third; the second came 9.5 s after the first.
        assert_eq!(
            script,
            [
                (20_740, Did::Send(text.to_owned())),
                (60_740, Did::Send(text.to_owned()))
            ],
            "{text}"
        );
    }
}

#[test]
fn a_mode_of_every_kind_at_once_does_what_its_plan_does_less_what_the_limits_refuse() {
    let definition = mode(vec![
        step(Step::SelectHotbarSlot { slot: slot(0) }, Schedule::AtStart),
        step(
            Step::Look {
                yaw: 90.0,
                pitch: 10.0,
            },
            Schedule::AtStart,
        ),
        step(
            Step::RotateRandom {
                max_yaw: 30.0,
                max_pitch: 10.0,
            },
            Schedule::AtStart,
        ),
        step(chat("/login hunter2"), Schedule::AtStart),
        step(Step::UseItem, every(1_000)),
        step(Step::AttackFacingEntity, every(500)),
        step(Step::Jump, every(750)),
        step(Step::Sneak { on: true }, every(1_000)),
        step(Step::Sneak { on: false }, every(250)),
        step(Step::SwingArm, every(1_000)),
        step(Step::HoldUse { on: true }, Schedule::OnRespawn),
        step(Step::EnterMinecart, Schedule::OnRespawn),
        step(chat("/spawn"), Schedule::OnRespawn),
        step(
            Step::Look {
                yaw: -45.0,
                pitch: 0.0,
            },
            Schedule::OnRespawn,
        ),
        step(
            Step::Look {
                yaw: 45.0,
                pitch: 0.0,
            },
            Schedule::OnRespawn,
        ),
    ]);

    for crosshair in [Crosshair::Nothing, Crosshair::Entity] {
        same_as_the_plan(&definition, &definition, &Course::three_deaths(), crosshair);
    }
}

#[test]
fn adjacent_steps_on_the_same_schedule_share_a_timer_and_keep_their_order() {
    // Steps 1 and 3 repeat every second, step 2 every two: at 2 s all three
    // are due, and they run in step order. Steps 3 and 4 are next to each
    // other among the repeating steps, with one schedule: one timer.
    let definition = mode(vec![
        step(Step::SelectHotbarSlot { slot: slot(1) }, every(1_000)),
        step(Step::UseItem, every(2_000)),
        step(Step::SelectHotbarSlot { slot: slot(2) }, every(1_000)),
        step(Step::Jump, every(1_000)),
        step(Step::SwingArm, Schedule::AtStart),
    ]);

    same_as_the_plan(
        &definition,
        &definition,
        &Course::one_death(25_000),
        Crosshair::Nothing,
    );
    let source = convert(&definition)
        .unwrap()
        .script()
        .source()
        .as_str()
        .to_owned();
    assert_eq!(source.matches("timer.every(").count(), 3, "{source}");
}

proptest! {
    // Each case compiles a script and runs it for 10 s of game time, as the
    // run properties do (`properties.rs`); a failure shrinks for a bounded
    // number of tries.
    #![proptest_config(ProptestConfig {
        max_shrink_iters: 512,
        ..ProptestConfig::with_cases(64)
    })]

    /// Any step mode whose steps run every time and repeat without jitter
    /// does exactly what its plan does, less what the limits below a
    /// script refuse.
    #[test]
    fn any_step_mode_does_what_its_plan_does_less_what_the_limits_refuse(
        steps in proptest::collection::vec(any_step(), 0..=10),
        on_entity in any::<bool>(),
    ) {
        let Ok(definition) = ModeDraft { steps }.validate() else {
            return Err(TestCaseError::reject("not a valid step mode"));
        };
        let as_planned = raised(&definition);
        let crosshair = if on_entity { Crosshair::Entity } else { Crosshair::Nothing };
        let conversion = convert(&definition).expect("any such mode converts");
        let course = Course::alive(10_000);
        let script = scripted(conversion.script(), &course, crosshair, 7);
        let expected = as_a_script_does(planned(&as_planned, &course, crosshair), &course);

        assert_same(&script, &expected, largest_turn(&definition));
    }
}

/// `definition` with each interval raised to a script's shortest for it.
fn raised(definition: &ModeDefinition) -> ModeDefinition {
    mode(
        definition
            .steps()
            .iter()
            .map(|step| match step.schedule {
                Schedule::Every { interval, jitter } => ModeStep {
                    schedule: Schedule::Every {
                        interval: interval.max(ms(script_minimum(&step.action))),
                        jitter,
                    },
                    ..step.clone()
                },
                _ => step.clone(),
            })
            .collect(),
    )
}

fn any_action() -> impl Strategy<Value = Step> {
    prop_oneof![
        (-720i16..=720, -360i16..=360).prop_map(|(yaw, pitch)| Step::Look {
            yaw: f32::from(yaw) / 4.0,
            pitch: f32::from(pitch) / 4.0,
        }),
        (1u8..=180, 0u8..=90).prop_map(|(yaw, pitch)| Step::RotateRandom {
            max_yaw: f32::from(yaw),
            max_pitch: f32::from(pitch),
        }),
        Just(Step::Jump),
        any::<bool>().prop_map(|on| Step::Sneak { on }),
        Just(Step::SwingArm),
        Just(Step::UseItem),
        any::<bool>().prop_map(|on| Step::HoldUse { on }),
        Just(Step::AttackFacingEntity),
        (0u8..=8).prop_map(|n| Step::SelectHotbarSlot { slot: slot(n) }),
        prop::sample::select(vec!["hello", "/spawn", "/home farm", "brb {soon}"]).prop_map(chat),
        Just(Step::EnterMinecart),
    ]
}

fn any_step() -> impl Strategy<Value = ModeStep> {
    (any_action(), 0u8..3, 0u64..=12).prop_map(|(action, schedule, extra)| {
        let schedule = match schedule {
            0 => Schedule::AtStart,
            1 => Schedule::OnRespawn,
            _ => every(step_minimum(&action) + extra * 50),
        };
        step(action, schedule)
    })
}

// --- Randomness ------------------------------------------------------------------------------

#[test]
fn a_jittered_step_repeats_within_its_gap_as_the_plan_does() {
    let definition = mode(vec![step(Step::Jump, jittered(1_000, 500))]);
    let course = Course::alive(120_000);
    let conversion = convert(&definition).unwrap();

    for runs in [
        planned(&definition, &course, Crosshair::Nothing),
        scripted(conversion.script(), &course, Crosshair::Nothing, 7),
    ] {
        let times: Vec<u64> = runs.iter().map(|(at, _)| *at).collect();
        let gaps: Vec<u64> = times.windows(2).map(|pair| pair[1] - pair[0]).collect();
        let first = times.first().copied().unwrap();

        assert!((1_000..=1_500).contains(&first), "{times:?}");
        assert!(
            gaps.iter().all(|gap| (1_000..=1_500).contains(gap)),
            "{gaps:?}"
        );
        // Spread over the gap, not one length.
        assert!(gaps.iter().any(|gap| *gap < 1_150), "{gaps:?}");
        assert!(gaps.iter().any(|gap| *gap > 1_350), "{gaps:?}");
    }
}

#[test]
fn a_step_runs_as_often_as_its_probability_says_and_is_due_again_either_way() {
    let definition = mode(vec![ModeStep {
        probability: 30,
        ..step(Step::Jump, every(1_000))
    }]);
    let course = Course::alive(300_000);
    let conversion = convert(&definition).unwrap();

    for runs in [
        planned(&definition, &course, Crosshair::Nothing),
        scripted(conversion.script(), &course, Crosshair::Nothing, 7),
    ] {
        // 300 chances at 30 %: about 90 runs, each on a second.
        assert!((60..=120).contains(&runs.len()), "{}", runs.len());
        assert!(runs.iter().all(|(at, _)| at % 1_000 == 0), "{runs:?}");
    }
}

#[test]
fn a_step_at_start_runs_as_often_as_its_probability_says() {
    let definition = mode(vec![ModeStep {
        probability: 30,
        ..step(Step::Jump, Schedule::AtStart)
    }]);
    let conversion = convert(&definition).unwrap();
    let joins = 1_000;
    let ran = (0..joins)
        .filter(|seed| {
            !scripted(
                conversion.script(),
                &Course::alive(0),
                Crosshair::Nothing,
                *seed,
            )
            .is_empty()
        })
        .count();

    // 1,000 joins at 30 %: about 300.
    assert!((240..=360).contains(&ran), "{ran}");
}

#[test]
fn a_turn_stays_within_its_maxima_and_turns_both_ways() {
    let definition = mode(vec![step(
        Step::RotateRandom {
            max_yaw: 30.5,
            max_pitch: 0.0,
        },
        every(250),
    )]);
    let runs = scripted(
        convert(&definition).unwrap().script(),
        &Course::alive(60_000),
        Crosshair::Nothing,
        7,
    );
    let yaws: Vec<f32> = runs
        .iter()
        .map(|(_, did)| match did {
            Did::Turn(yaw, pitch) => {
                assert!(pitch.abs() < f32::EPSILON, "{pitch}");
                *yaw
            }
            other => panic!("{other:?}"),
        })
        .collect();

    assert_eq!(yaws.len(), 240);
    assert!(yaws.iter().all(|yaw| yaw.abs() <= 30.5), "{yaws:?}");
    assert!(yaws.iter().any(|yaw| *yaw < -25.0) && yaws.iter().any(|yaw| *yaw > 25.0));
    // Not only whole degrees: the maximum's half degree is reached too.
    assert!(yaws.iter().any(|yaw| yaw.fract().abs() > 0.05), "{yaws:?}");
}

// --- What the script holds -------------------------------------------------------------------

#[test]
fn a_chat_step_becomes_a_declared_message_and_its_text_stays_out_of_the_script() {
    let definition = mode(vec![
        step(Step::Jump, Schedule::AtStart),
        step(chat("/login hunter2"), Schedule::AtStart),
    ]);
    let conversion = convert(&definition).unwrap();
    let script = conversion.script();
    let draft = ScriptDraft::from(script.clone());

    assert!(!script.source().as_str().contains("hunter2"));
    assert!(!script.source().as_str().contains("/login"));
    assert_eq!(draft.messages.len(), 1);
    assert_eq!(draft.messages[0].key, "join_2");
    assert_eq!(draft.messages[0].text, "/login hunter2");
    assert_eq!(draft.messages[0].values, []);
    assert_eq!(
        scripted(script, &Course::alive(0), Crosshair::Nothing, 7),
        [
            (0, Did::Press(Input::Key(Key::Jump), PressKind::Tap)),
            (0, Did::Send("/login hunter2".to_owned()))
        ]
    );
}

#[test]
fn a_brace_in_a_chat_step_is_declared_doubled_and_sent_as_written() {
    let definition = mode(vec![step(chat("/say {hi} and }{"), Schedule::AtStart)]);
    let conversion = convert(&definition).unwrap();
    let draft = ScriptDraft::from(conversion.script().clone());

    assert_eq!(draft.messages[0].text, "/say {{hi}} and }}{{");
    assert_eq!(
        scripted(
            conversion.script(),
            &Course::alive(0),
            Crosshair::Nothing,
            7
        ),
        [(0, Did::Send("/say {hi} and }{".to_owned()))]
    );
}

#[rstest]
#[case::a_brace_in_the_commands_name("/sa{y hi", TemplateError::BraceInCommandName { position: 3 })]
#[case::an_invisible_character("hi\u{200b}there", TemplateError::ForbiddenChar { position: 2 })]
fn a_chat_step_a_mode_cant_declare_is_named_with_its_step(
    #[case] text: &str,
    #[case] reason: TemplateError,
) {
    let definition = mode(vec![
        step(Step::Jump, Schedule::AtStart),
        step(chat(text), every(30_000)),
    ]);

    assert_eq!(
        convert(&definition),
        Err(ConvertError::Message { step: 1, reason })
    );
}

#[test]
fn enter_minecart_becomes_a_mount_that_passes_an_occupied_minecart() {
    let definition = mode(vec![step(Step::EnterMinecart, Schedule::AtStart)]);
    let conversion = convert(&definition).unwrap();
    let program = Arc::new(compile(conversion.script()).unwrap());
    let mut instance = Instance::new(program, 7, still());
    let mut world = World::new();
    // The nearer minecart holds someone; the free one is further.
    world.entities.push(EntityFact {
        kind: id("minecart"),
        position: at(1.5, 64.0, 0.5),
        occupied: true,
    });
    world.minecart(at(3.5, 64.0, 0.5));

    world.run(&mut instance, &HookEvent::Join { first: true });

    assert!(
        conversion
            .script()
            .source()
            .as_str()
            .contains("vehicle.mount(\"minecart\"); // when it joins, step 1")
    );
    assert_eq!(world.bot.riding, Some(id("minecart")));
    assert!(world.entities.iter().all(|entity| entity.occupied));
    assert_eq!(
        conversion.notes(),
        [Note {
            step: 0,
            kind: NoteKind::MountSkipsOccupied
        }]
    );
}

/// A mode of most kinds of step, in an order the editor wouldn't keep.
fn a_mode_of_most_kinds() -> ModeDefinition {
    ModeDraft {
        steps: vec![
            step(Step::SelectHotbarSlot { slot: slot(0) }, Schedule::AtStart),
            step(
                Step::Look {
                    yaw: 90.0,
                    pitch: 10.0,
                },
                Schedule::AtStart,
            ),
            step(Step::AttackFacingEntity, jittered(650, 150)),
            step(chat("/login hunter2"), Schedule::AtStart),
            step(Step::EnterMinecart, Schedule::OnRespawn),
            step(chat("/spawn"), Schedule::OnRespawn),
            ModeStep {
                action: Step::RotateRandom {
                    max_yaw: 30.0,
                    max_pitch: 10.5,
                },
                schedule: jittered(45_000, 75_000),
                probability: 50,
            },
            step(Step::SwingArm, every(20_000)),
        ],
    }
    .validate()
    .unwrap()
}

#[test]
fn the_script_reads_as_its_steps() {
    let conversion = convert(&a_mode_of_most_kinds()).unwrap();

    assert_eq!(
        conversion.script().source().as_str(),
        r#"// Converted from a step mode: each line marked with a step does what
// that step did. What a script does differently is in the script
// reference, under "Converting a step mode".

fn on_join(first) {
    state.respawn_chat = false;
    hotbar.select(1); // when it joins, step 1
    look.at(90.0, 10.0); // when it joins, step 2
    chat.send("join_3"); // when it joins, step 3
    timer.after(650 + random.between(0, 150), "online_1"); // while it's online, step 1
    timer.after(45000 + random.between(0, 75000), "online_2"); // while it's online, step 2
    timer.every(20000, "online_3"); // while it's online, step 3
}

fn on_respawn() {
    vehicle.mount("minecart"); // after it respawns, step 1
    if !state.respawn_chat { // after it respawns, step 2, at most once every 30 seconds
        state.respawn_chat = true;
        timer.after(30000, "respawn_chat");
        chat.send("respawn_2");
    }
}

fn on_timer(name) {
    switch name {
        "online_1" => {
            attack(); // while it's online, step 1
            timer.after(650 + random.between(0, 150), "online_1");
        },
        "online_2" => {
            if random.chance(50) {
                look.turn(random.between(-30, 30), random.between(-105, 105) / 10.0); // while it's online, step 2
            }
            timer.after(45000 + random.between(0, 75000), "online_2");
        },
        "online_3" => {
            swing(); // while it's online, step 3
        },
        "respawn_chat" => {
            state.respawn_chat = false;
        },
    }
}

// Attacks what the crosshair is on if it's an entity within reach, as the
// step did: a left click on anything else would swing.
fn attack() {
    let target = bot.looking_at;
    if target != () && target.kind == "entity" {
        mouse.left.tap();
    }
}

// Swings the arm as a left click does, unless the crosshair is on an
// entity, which a left click would hit.
fn swing() {
    let target = bot.looking_at;
    if target == () || target.kind != "entity" {
        mouse.left.tap();
    }
}
"#
    );
}

#[test]
fn the_notes_come_by_step_and_a_steps_own_before_a_shared_limit() {
    let conversion = convert(&a_mode_of_most_kinds()).unwrap();

    assert_eq!(
        conversion.notes(),
        [
            Note {
                step: 2,
                kind: NoteKind::SharesLimit {
                    other: 7,
                    every: ms(500)
                }
            },
            Note {
                step: 4,
                kind: NoteKind::MountSkipsOccupied
            },
            Note {
                step: 7,
                kind: NoteKind::SwingSkipsEntities
            },
            Note {
                step: 7,
                kind: NoteKind::SharesLimit {
                    other: 2,
                    every: ms(500)
                }
            },
        ]
    );
}

// --- Notes and refusals ----------------------------------------------------------------------

#[rstest]
#[case::a_swing_faster_than_a_click(step(Step::SwingArm, every(250)), vec![
    NoteKind::SwingSkipsEntities,
    NoteKind::Slower { every: ms(500) },
])]
#[case::a_use_faster_than_a_timer(step(Step::UseItem, every(200)), vec![
    NoteKind::Slower { every: ms(250) },
])]
#[case::a_hold(step(Step::HoldUse { on: true }, Schedule::AtStart), vec![NoteKind::HoldRepeats])]
#[case::a_gap_past_a_day(step(Step::Jump, jittered(20 * 3_600_000, 10 * 3_600_000)), vec![
    NoteKind::GapCapped,
])]
#[case::a_minecart(step(Step::EnterMinecart, every(250)), vec![NoteKind::MountSkipsOccupied])]
#[case::a_look(step(Step::Look { yaw: 0.0, pitch: 0.0 }, every(250)), vec![])]
fn each_difference_is_a_note_at_its_step(
    #[case] at_its_step: ModeStep,
    #[case] kinds: Vec<NoteKind>,
) {
    let definition = mode(vec![step(Step::Jump, Schedule::AtStart), at_its_step]);

    let notes = convert(&definition).unwrap().notes().to_vec();

    assert_eq!(
        notes,
        kinds
            .into_iter()
            .map(|kind| Note { step: 1, kind })
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_gap_past_a_day_is_cut_at_a_day() {
    let definition = mode(vec![step(
        Step::Jump,
        jittered(20 * 3_600_000, 10 * 3_600_000),
    )]);
    let source = convert(&definition)
        .unwrap()
        .script()
        .source()
        .as_str()
        .to_owned();

    assert!(
        source.contains("timer.after(72000000 + random.between(0, 14400000), \"online_1\")"),
        "{source}"
    );
}

#[test]
fn steps_that_share_a_limit_are_noted_where_one_can_be_refused() {
    let definition = mode(vec![
        step(
            Step::Look {
                yaw: 0.0,
                pitch: 0.0,
            },
            Schedule::AtStart,
        ),
        step(
            Step::Look {
                yaw: 9.0,
                pitch: 0.0,
            },
            Schedule::AtStart,
        ),
        // At start and repeating never meet: the first run is a whole
        // interval after the join.
        step(
            Step::Look {
                yaw: 0.0,
                pitch: 9.0,
            },
            every(250),
        ),
        step(Step::UseItem, every(1_000)),
        step(Step::UseItem, every(1_500)),
        // Releases and commands have no limit.
        step(Step::Sneak { on: false }, every(250)),
        step(Step::Sneak { on: false }, every(300)),
        step(chat("/spawn"), Schedule::AtStart),
        step(chat("/home"), every(30_000)),
    ]);
    let shares = |step: usize, other: usize, every: u64| Note {
        step,
        kind: NoteKind::SharesLimit {
            other,
            every: ms(every),
        },
    };

    assert_eq!(
        convert(&definition).unwrap().notes(),
        [shares(1, 0, 250), shares(3, 4, 200), shares(4, 3, 200)]
    );
}

#[test]
fn a_scripted_mode_isnt_converted() {
    let script = Script::try_from(crate::support::draft("fn on_join(first) {}")).unwrap();

    assert_eq!(
        convert(&ModeDefinition::scripted(script)),
        Err(ConvertError::NotSteps)
    );
}

#[test]
fn more_timers_than_a_script_has_are_refused_and_shared_ones_count_once() {
    let distinct: Vec<ModeStep> = (0..17)
        .map(|n| step(Step::Jump, every(1_000 + n * 50)))
        .collect();
    let same: Vec<ModeStep> = (0..17).map(|_| step(Step::Jump, every(1_000))).collect();
    let mut with_a_respawn_chat: Vec<ModeStep> = (0..16)
        .map(|n| step(Step::Jump, every(1_000 + n * 50)))
        .collect();
    with_a_respawn_chat.push(step(chat("/spawn"), Schedule::OnRespawn));

    assert_eq!(
        convert(&mode(distinct)),
        Err(ConvertError::TooManyTimers { count: 17 })
    );
    assert!(convert(&mode(same)).is_ok());
    assert_eq!(
        convert(&mode(with_a_respawn_chat)),
        Err(ConvertError::TooManyTimers { count: 17 })
    );
}

#[test]
fn an_empty_step_mode_is_a_script_that_does_nothing() {
    let conversion = convert(&mode(Vec::new())).unwrap();

    assert_eq!(
        scripted(
            conversion.script(),
            &Course::one_death(25_000),
            Crosshair::Nothing,
            7
        ),
        []
    );
    assert_eq!(conversion.notes(), []);
}

#[rstest]
#[case::afk(ModeDefinition::afk(), vec![Note { step: 1, kind: NoteKind::SwingSkipsEntities }])]
#[case::farm(ModeDefinition::farm(), vec![])]
fn the_built_in_modes_convert(#[case] definition: ModeDefinition, #[case] notes: Vec<Note>) {
    let conversion = convert(&definition).unwrap();

    assert!(compile(conversion.script()).is_ok());
    assert_eq!(conversion.notes(), notes);
}

#[test]
fn a_run_due_while_the_bot_is_dead_comes_right_after_the_respawn() {
    // A step mode's run while the bot is dead does nothing in the session;
    // a script's timer waits, and its run comes with the respawn.
    let definition = mode(vec![step(Step::Jump, every(1_000))]);
    let course = Course {
        lives: vec![(2_500, 3_700)],
        end: 5_000,
    };

    let script = scripted(
        convert(&definition).unwrap().script(),
        &course,
        Crosshair::Nothing,
        7,
    );

    let times: Vec<u64> = script.iter().map(|(at, _)| *at).collect();
    assert_eq!(times, [1_000, 2_000, 3_700, 4_700]);
}
