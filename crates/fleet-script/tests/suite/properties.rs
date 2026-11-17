//! The properties of an instance, over any sequence of events (ADR-0036,
//! D18): a hook never exceeds its budget; no sequence makes two actions of
//! a kind closer than its minimum; after any stop, nothing is held; state
//! survives exactly what D5 says.

use core::fmt::Write;
use core::time::Duration;

use chrono::{DateTime, Utc};
use fleet_core::script::{
    Captured, ChatMatch, HookEvent, Ident, Input, Key, Limited, MouseButton, Press, limits,
};
use fleet_core::time;
use fleet_script::{Action, Instance, Outcome, PressKind, Ran};
use proptest::prelude::*;

use crate::support::{World, at, balance_draft, logged, program, program_of, timer};

/// What every hook of the busy script calls: something of everything,
/// picked by its own random numbers.
const BUSY_BODY: &str = r#"
fn act() {
    if !("n" in state) {
        state.n = 0;
    }
    state.n += 1;
    press(random.between(0, 14));
    let ms = random.between(250, 3000);
    let which = random.between(0, 20);
    if random.chance(40) {
        timer.after(ms, `t${which}`);
    }
    if random.chance(20) {
        timer.every(ms, "again");
    }
    if random.chance(15) {
        mouse.left.release();
        mouse.right.release();
    }
    if random.chance(10) {
        chat.send("hello");
    }
    if random.chance(10) {
        ask();
    }
}

fn ask() {
    if chat.send("balance").ok {
        chat.expect("balance", 1000);
    }
}

fn press(pick) {
    let a = random.between(-180, 180);
    let b = random.between(-90, 90);
    let ms = random.between(50, 2000);
    let slot = random.between(1, 9);
    switch pick {
        0 => keys.forward.hold(),
        1 => keys.forward.tap(),
        2 => keys.jump.hold_for(ms),
        3 => mouse.left.tap(),
        4 => mouse.left.hold(),
        5 => mouse.right.tap(),
        6 => mouse.right.hold(),
        7 => look.at(a, b),
        8 => look.turn(1, 1),
        9 => hotbar.select(slot),
        10 => vehicle.mount("minecart"),
        11 => vehicle.dismount(),
        12 => keys.release_all(),
        13 => mouse.right.hold_until_used(),
        _ => keys.sneak.hold_for(60),
    }
}
"#;

/// The busy script's hooks, each with its parameters.
const HOOKS: [&str; 6] = [
    "on_join(first)",
    "on_world_change()",
    "on_death()",
    "on_respawn()",
    "on_timer(name)",
    "on_chat(key, values)",
];

/// A script that does something of everything in every hook. With
/// `spins`, that hook holds a key and then never returns.
fn busy(spins: Option<usize>) -> String {
    let mut source = String::from(BUSY_BODY);
    for (index, hook) in HOOKS.iter().enumerate() {
        let spin = if spins == Some(index) {
            " keys.sprint.hold(); loop { }"
        } else {
            ""
        };
        let _ = writeln!(source, "fn {hook} {{ act();{spin} }}");
    }
    source
}

/// A script that counts the hooks it ran, and says the number when it
/// stops.
const COUNTER: &str = "fn on_join(first) { if first { state.n = 0; } state.n += 1; }\nfn on_world_change() { state.n += 1; }\nfn on_death() { state.n += 1; }\nfn on_respawn() { state.n += 1; }\nfn on_timer(name) { state.n += 1; }\nfn on_stop() { log.info(`${state.n}`); }";

/// Something that happens to a bot, after a pause.
#[derive(Debug, Clone)]
enum Step {
    Event(u8),
    Disconnect,
}

fn steps() -> impl Strategy<Value = Vec<(u64, Step)>> {
    let step = prop_oneof![
        8 => (0..6_u8).prop_map(Step::Event),
        1 => Just(Step::Disconnect),
    ];
    prop::collection::vec((0..2_500_u64, step), 1..40)
}

/// How many of `steps` are events.
fn events(steps: &[(u64, Step)]) -> usize {
    steps
        .iter()
        .filter(|(_, step)| matches!(step, Step::Event(_)))
        .count()
}

/// The event number `kind` stands for.
fn event(kind: u8, first: bool) -> HookEvent {
    match kind {
        0 => HookEvent::Join { first },
        1 => HookEvent::WorldChange,
        2 => HookEvent::Death,
        3 => HookEvent::Respawn,
        4 => HookEvent::Timer(timer("from outside")),
        _ => HookEvent::Chat(ChatMatch {
            key: Ident::try_from("balance").unwrap(),
            values: vec![(Ident::try_from("amount").unwrap(), Captured::Amount(1.5))],
        }),
    }
}

/// The limited kind `action` is, if it's one.
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
        Action::Send { message, .. } if !message.is_command() => Limited::PlainChat,
        Action::Send { .. }
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

/// What a run of steps left behind, for the properties to judge.
struct Trace {
    /// Each hook's moment and operations.
    hooks: Vec<(DateTime<Utc>, u64)>,
    /// Each performed action's moment.
    actions: Vec<(DateTime<Utc>, Action)>,
    /// Whether the script stopped by itself.
    stopped: bool,
}

/// Runs `steps` on `instance` in `world`.
fn run(world: &mut World, instance: &mut Instance, steps: &[(u64, Step)]) -> Trace {
    let mut trace = Trace {
        hooks: Vec::new(),
        actions: Vec::new(),
        stopped: false,
    };
    let note = |ran: &Ran, now: DateTime<Utc>, trace: &mut Trace| {
        match &ran.outcome {
            Outcome::Ran { operations } => trace.hooks.push((now, *operations)),
            Outcome::Stopped(_) => trace.stopped = true,
            Outcome::NotDefined | Outcome::Ended { .. } | Outcome::Over => {}
        }
        trace
            .actions
            .extend(ran.actions.iter().cloned().map(|action| (now, action)));
    };
    let mut first = true;
    for (pause, step) in steps {
        // Let the pause pass, waking the script whenever it asks.
        let end = time::add(world.now, Duration::from_millis(*pause));
        for _ in 0..1_000 {
            match instance.next_wake() {
                Some(due) if due <= end => {
                    world.now = world.now.max(due);
                    let ran = world.wake(instance);
                    note(&ran, world.now, &mut trace);
                }
                _ => break,
            }
        }
        world.now = end;
        match step {
            Step::Event(kind) => {
                let ran = world.run(instance, &event(*kind, first));
                first &= *kind != 0;
                note(&ran, world.now, &mut trace);
            }
            Step::Disconnect => instance.disconnected(),
        }
    }
    trace
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn a_hook_never_runs_more_than_a_hooks_budget_nor_a_second_more_than_a_seconds(
        steps in steps(),
        seed in any::<u64>(),
    ) {
        let mut world = World::new();
        world.minecart(at(1.5, 64.0, 0.5));
        let mut instance = Instance::new(program_of(balance_draft(&busy(None))), seed);

        let trace = run(&mut world, &mut instance, &steps);

        // Every event's hook ran: the script defines them all.
        prop_assert!(!trace.stopped);
        prop_assert!(trace.hooks.len() >= events(&steps));
        for (_, operations) in &trace.hooks {
            prop_assert!(*operations <= limits::OPERATIONS_PER_HOOK);
        }
        // A wake may hold several timers' hooks, each with a budget of
        // its own, so a second's sum is taken over what started in it.
        for (index, (start, _)) in trace.hooks.iter().enumerate() {
            let within: u64 = trace
                .hooks
                .iter()
                .skip(index)
                .take_while(|(at, _)| time::elapsed(*start, *at) < Duration::from_secs(1))
                .map(|(_, operations)| operations)
                .sum();
            prop_assert!(within <= limits::OPERATIONS_PER_SECOND, "{within}");
        }
    }

    #[test]
    fn no_sequence_of_events_makes_two_actions_of_a_kind_closer_than_its_minimum(
        steps in steps(),
        seed in any::<u64>(),
    ) {
        let mut world = World::new();
        world.minecart(at(1.5, 64.0, 0.5));
        let mut instance = Instance::new(program_of(balance_draft(&busy(None))), seed);

        let trace = run(&mut world, &mut instance, &steps);

        prop_assert!(!trace.stopped);
        prop_assert!(trace.hooks.len() >= events(&steps));
        let mut last: Vec<(Limited, DateTime<Utc>)> = Vec::new();
        for (now, action) in &trace.actions {
            let Some(kind) = kind_of(action) else { continue };
            if let Some((_, before)) = last.iter_mut().find(|(known, _)| *known == kind) {
                let gap = time::elapsed(*before, *now);
                prop_assert!(gap >= kind.minimum(), "{kind:?} after {gap:?}");
                *before = *now;
            } else {
                last.push((kind, *now));
            }
        }
    }

    #[test]
    fn a_new_instance_gains_nothing_on_the_limits(
        steps in steps(),
        seed in any::<u64>(),
    ) {
        // The mode is saved again halfway: a fresh instance, the same bot,
        // the same limiter.
        let mut world = World::new();
        let half = steps.len() / 2;
        let mut one = Instance::new(program_of(balance_draft(&busy(None))), seed);
        let mut two = Instance::new(program_of(balance_draft(&busy(None))), seed);

        let mut trace = run(&mut world, &mut one, steps.get(..half).unwrap_or_default());
        let stop = world.run(&mut one, &HookEvent::Stop);
        prop_assert_eq!(stop.actions.last(), Some(&Action::ReleaseAll));
        let second = run(&mut world, &mut two, steps.get(half..).unwrap_or_default());
        trace.actions.extend(second.actions);

        let mut last: Vec<(Limited, DateTime<Utc>)> = Vec::new();
        for (now, action) in &trace.actions {
            let Some(kind) = kind_of(action) else { continue };
            if let Some((_, before)) = last.iter_mut().find(|(known, _)| *known == kind) {
                prop_assert!(time::elapsed(*before, *now) >= kind.minimum(), "{kind:?}");
                *before = *now;
            } else {
                last.push((kind, *now));
            }
        }
    }

    #[test]
    fn after_any_stop_nothing_is_held(
        steps in steps(),
        seed in any::<u64>(),
        fails in any::<bool>(),
    ) {
        // The script holds what it likes, then stops: by a limit in its
        // next hook, or in an orderly way.
        let source = format!(
            "{}\nfn on_stop() {{ keys.forward.hold(); if {fails} {{ loop {{ }} }} }}",
            busy(None)
        );
        let mut world = World::new();
        let mut instance = Instance::new(program_of(balance_draft(&source)), seed);

        let trace = run(&mut world, &mut instance, &steps);
        let stop = world.run(&mut instance, &HookEvent::Stop);

        if !trace.stopped {
            let ended = matches!(stop.outcome, Outcome::Ended { .. });
            prop_assert!(ended);
            prop_assert_eq!(stop.actions.last(), Some(&Action::ReleaseAll));
        }
        for input in inputs() {
            prop_assert!(!instance.is_held(input), "{input:?}");
        }
        prop_assert_eq!(instance.next_wake(), None);
        prop_assert_eq!(instance.timers(), 0);
        prop_assert_eq!(instance.state_size(), (0, 0));
        // And nothing comes after it.
        let later = world.run(&mut instance, &HookEvent::Join { first: false });
        prop_assert_eq!(later.outcome, Outcome::Over);
        prop_assert_eq!(later.actions.len(), 0);
    }

    #[test]
    fn a_script_stopped_by_a_limit_holds_nothing(
        steps in steps(),
        seed in any::<u64>(),
        at_hook in 0..6_u8,
    ) {
        // One of its hooks holds a key and runs for ever.
        let source = busy(Some(usize::from(at_hook)));
        let mut world = World::new();
        let mut instance = Instance::new(program_of(balance_draft(&source)), seed);

        let trace = run(&mut world, &mut instance, &steps);

        if trace.stopped {
            for input in inputs() {
                prop_assert!(!instance.is_held(input), "{input:?}");
            }
            prop_assert!(instance.error().is_some());
            prop_assert_eq!(instance.next_wake(), None);
            let last_release = trace
                .actions
                .iter()
                .rposition(|(_, action)| *action == Action::ReleaseAll);
            prop_assert_eq!(last_release, Some(trace.actions.len() - 1));
        }
    }

    #[test]
    fn state_survives_everything_but_a_new_start(steps in steps()) {
        let mut world = World::new();
        let mut instance = Instance::new(program(COUNTER), 7);
        // The first event is always the first join, which sets the
        // counter up.
        world.run(&mut instance, &HookEvent::Join { first: true });
        let mut expected = 1;
        for (pause, step) in &steps {
            world.now += Duration::from_millis(*pause);
            match step {
                Step::Event(kind @ 0..=4) => {
                    world.run(&mut instance, &event(*kind, false));
                    expected += 1;
                }
                // The counter has no `on_chat`.
                Step::Event(_) => {
                    world.run(&mut instance, &event(5, false));
                }
                Step::Disconnect => instance.disconnected(),
            }
        }

        let stop = world.run(&mut instance, &HookEvent::Stop);

        prop_assert_eq!(logged(&stop.actions), [expected.to_string()]);
    }
}
