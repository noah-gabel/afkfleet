//! Instances are independent: each has its own engine, state and budget,
//! whatever thread runs it (P15.7; the user's reviewer's finding 1).
//!
//! Rhai's progress callback gets only an operation count, no context. With
//! one engine shared between bots it couldn't tell whose state to check or
//! whose budget is running, so every instance has an engine of its own
//! over shared packages.

use std::sync::{Arc, Barrier};
use std::thread;

use fleet_core::script::{HookEvent, limits};
use fleet_script::{Instance, Limit, Outcome, ScriptErrorKind};

use crate::support::{World, logged, program, stopped, timer};

/// A script that either fills its state past the limit or only works,
/// by what `on_join` was told: `first` picks the one that fills.
const SOURCE: &str = "fn on_join(first) {\n  state.fill = first;\n  state.rounds = 0;\n}\nfn on_timer(name) {\n  if state.fill { fill(); }\n  let n = 0;\n  while n < 1000 { n += 1; }\n  state.rounds += 1;\n  log.info(`${state.rounds}`);\n}\nfn fill() {\n  for i in 0..200 { state[`k${i}`] = i; }\n}";

#[test]
fn two_instances_on_two_threads_each_stop_only_for_their_own_state() {
    let program = program(SOURCE);
    let start = Arc::new(Barrier::new(2));
    let run = |fills: bool| {
        let program = program.clone();
        let start = start.clone();
        thread::spawn(move || {
            let mut world = World::new();
            let mut instance = Instance::new(program, 7);
            world.run(&mut instance, &HookEvent::Join { first: fills });
            start.wait();
            let mut outcomes = Vec::new();
            for _ in 0..200 {
                world.now += core::time::Duration::from_secs(2);
                let ran = world.run(&mut instance, &HookEvent::Timer(timer("t")));
                let over = !matches!(ran.outcome, Outcome::Ran { .. });
                outcomes.push(ran);
                if over {
                    break;
                }
            }
            outcomes
        })
    };

    let filling = run(true);
    let working = run(false);
    let filled = filling.join().unwrap();
    let worked = working.join().unwrap();

    // The one that fills its state stops at its first hook, for its
    // state, while the other runs every one of its 200 hooks beside it.
    assert_eq!(filled.len(), 1);
    assert_eq!(
        stopped(filled.first().unwrap()).kind,
        ScriptErrorKind::Limit(Limit::StateEntries)
    );
    assert_eq!(worked.len(), 200);
    assert!(
        worked
            .iter()
            .all(|ran| matches!(ran.outcome, Outcome::Ran { .. }))
    );
    assert_eq!(
        worked.last().map(|ran| logged(&ran.actions)),
        Some(vec!["200"])
    );
}

#[test]
fn one_instances_second_is_not_another_instances_second() {
    // Two instances of one program, hooks at the same moments: each has
    // its own 30,000 operations.
    let program = program("fn on_timer(name) { let n = 0; while n < 1000 { n += 1; } }");
    let mut world = World::new();
    let mut one = Instance::new(program.clone(), 1);
    let mut two = Instance::new(program, 2);
    let each = match world.run(&mut one, &HookEvent::Timer(timer("t"))).outcome {
        Outcome::Ran { operations } => operations,
        other => panic!("{other:?}"),
    };
    let fit = limits::OPERATIONS_PER_SECOND / each;

    // `one` uses its whole second; `two` still has all of its own.
    for _ in 1..fit {
        world.run(&mut one, &HookEvent::Timer(timer("t")));
    }
    let over = world.run(&mut one, &HookEvent::Timer(timer("t")));
    let other = world.run(&mut two, &HookEvent::Timer(timer("t")));

    assert_eq!(
        stopped(&over).kind,
        ScriptErrorKind::Limit(Limit::OperationsPerSecond)
    );
    assert!(matches!(other.outcome, Outcome::Ran { .. }));
}

#[test]
fn an_instance_can_move_to_another_thread_between_hooks() {
    let mut world = World::new();
    let mut instance = Instance::new(
        program(
            "fn on_join(first) { state.n = 1; }\nfn on_timer(name) { state.n += 1; log.info(`${state.n}`); }",
        ),
        7,
    );
    world.run(&mut instance, &HookEvent::Join { first: true });

    let (instance, lines) = thread::spawn(move || {
        let mut world = World::new();
        world.run(&mut instance, &HookEvent::Timer(timer("t")));
        (instance, world.log)
    })
    .join()
    .unwrap();

    assert_eq!(lines, ["2"]);
    assert_eq!(instance.state_size().0, 1);
}

#[test]
fn a_compiled_program_and_an_instance_cross_threads() {
    fn crosses<T: Send + Sync>() {}
    fn moves<T: Send>() {}

    crosses::<fleet_script::Program>();
    moves::<Instance>();
}
