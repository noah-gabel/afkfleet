//! A hook's time: bounded like its operations, a hook and a second
//! (ADR-0040's amendment).
//!
//! Operations alone don't bound it. The user's reviewer timed hooks that
//! ran to the budget of 10,000 operations in a release build: a plain loop
//! took 0.38 ms, a loop over an array method that calls a function of the
//! script's 41 ms, and one that builds a text or an array past its limit
//! inside such a call and catches the refusal 106 to 120 ms.
//!
//! Most tests here time a hook with fleet-testkit's `ManualStopwatch`,
//! which moves by a fixed step at each reading, so they say how long a
//! hook took and give the same answer on any machine. The reviewer's
//! three cases run against the OS's own clock too, at the end.

use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
use std::sync::Arc;
use std::time::Instant;

use fleet_core::script::{Hook, HookEvent, Stopwatch, limits};
use fleet_script::{Action, Instance, Limit, Outcome, Ran, ScriptError, ScriptErrorKind};
use fleet_testkit::stopwatch::ManualStopwatch;

use crate::support::{World, logged, operations, program, stopped, timed, timer};

const MICROS_100: Duration = Duration::from_micros(100);

/// What a script that hit `limit` in `hook` stopped with.
const fn hit(limit: Limit, hook: Hook) -> ScriptError {
    ScriptError {
        kind: ScriptErrorKind::Limit(limit),
        hook,
        at: None,
    }
}

/// Runs `on_join` of `source` once, timed by `stopwatch`.
fn join(source: &str, stopwatch: &ManualStopwatch) -> (Ran, World, Instance) {
    let mut world = World::new();
    let mut instance = timed(source, stopwatch);
    let ran = world.run(&mut instance, &HookEvent::Join { first: true });
    (ran, world, instance)
}

/// A script whose `on_timer` runs a loop as often as the timer's name says.
const WORK: &str = "fn on_timer(name) { let n = 0; let rounds = parse_int(name); while n < rounds { n += 1; } }\nfn on_death() { log.info(\"ran\"); }\nfn on_stop() { loop { } }";

/// Runs [`WORK`]'s loop for `rounds`.
fn work(world: &mut World, instance: &mut Instance, rounds: u64) -> Ran {
    world.run(instance, &HookEvent::Timer(timer(&rounds.to_string())))
}

/// How many stretches of time [`WORK`]'s `on_timer` is counted in for
/// `rounds`: one between each two readings of its stopwatch.
fn stretches(rounds: u64) -> u64 {
    let stopwatch = ManualStopwatch::standing();
    let mut world = World::new();
    let mut instance = timed(WORK, &stopwatch);
    let ran = work(&mut world, &mut instance, rounds);
    assert!(matches!(ran.outcome, Outcome::Ran { .. }), "{ran:?}");
    stopwatch.readings() - 1
}

/// The rounds that make [`WORK`]'s `on_timer` be counted in exactly
/// `wanted` stretches.
fn rounds_of(wanted: u64) -> u64 {
    (0..2_000)
        .find(|rounds| stretches(*rounds) == wanted)
        .expect("some number of rounds is counted in that many stretches")
}

#[test]
fn a_hooks_stopwatch_is_read_at_its_start_every_few_operations_and_at_its_end() {
    let stopwatch = ManualStopwatch::standing();
    let (empty, ..) = join("fn on_join(first) { }", &stopwatch);
    let after_empty = stopwatch.readings();
    let (looped, ..) = join(
        "fn on_join(first) { let n = 0; while n < 100 { n += 1; } }",
        &stopwatch,
    );

    assert_eq!(after_empty, 2);
    assert_eq!(
        stopwatch.readings() - after_empty,
        2 + operations(&looped) / limits::TIME_CHECK_EVERY_OPERATIONS
    );
    assert!(operations(&empty) < limits::TIME_CHECK_EVERY_OPERATIONS);
}

#[test]
fn a_hook_that_runs_past_its_time_stops_the_script() {
    // 100 µs at each reading: a hook's 5 ms are over after 51 of them,
    // long before its operations are.
    let stopwatch = ManualStopwatch::stepping(MICROS_100);

    let (ran, _, instance) = join("fn on_join(first) { loop { } }", &stopwatch);

    assert_eq!(stopped(&ran), &hit(Limit::HookTime, Hook::Join));
    assert_eq!(ran.actions, [Action::ReleaseAll]);
    assert_eq!(ran.next_wake, None);
    assert_eq!(instance.error(), Some(&hit(Limit::HookTime, Hook::Join)));
    // The reading at its start, the 51 that took it past its time, and
    // the one at its end.
    assert_eq!(stopwatch.readings(), 53);
}

#[test]
fn a_plain_loop_is_ended_by_its_operations_long_before_its_time() {
    // The reviewer's first case: 0.38 ms for 10,000 operations is 0.3 µs
    // for each stretch of 8. At 1 µs a stretch the hook's time is a
    // quarter of what it may take when its operations run out.
    let stopwatch = ManualStopwatch::stepping(Duration::from_micros(1));

    let (ran, ..) = join("fn on_join(first) { loop { } }", &stopwatch);

    assert_eq!(stopped(&ran), &hit(Limit::OperationsPerHook, Hook::Join));
    assert!(stopwatch.shown() < limits::HOOK_TIME / 2);
}

#[test]
fn a_hook_may_take_exactly_its_time_and_no_more() {
    // 50 stretches of 100 µs are a hook's 5 ms exactly; 51 are over.
    let stopwatch = ManualStopwatch::stepping(MICROS_100);
    let mut world = World::new();

    let exact = work(&mut world, &mut timed(WORK, &stopwatch), rounds_of(50));
    world.now += Duration::from_secs(5);
    let over = work(&mut world, &mut timed(WORK, &stopwatch), rounds_of(51));

    assert!(matches!(exact.outcome, Outcome::Ran { .. }), "{exact:?}");
    assert_eq!(stopped(&over), &hit(Limit::HookTime, Hook::Timer));
}

#[test]
fn time_counts_in_the_functions_a_hook_calls() {
    let stopwatch = ManualStopwatch::stepping(MICROS_100);

    let (ran, ..) = join(
        "fn on_join(first) { one(); }\nfn one() { two(); }\nfn two() { loop { } }",
        &stopwatch,
    );

    assert_eq!(stopped(&ran), &hit(Limit::HookTime, Hook::Join));
}

#[test]
fn a_script_cant_catch_its_way_past_its_time() {
    // The loop runs inside a function an array's method calls, which wraps
    // its end into an error a script can catch. Caught, the next thing the
    // script does ends it again.
    let source = "fn on_join(first) {\n  try { [1].for_each(Fn(\"spin\")); } catch { log.info(\"caught\"); }\n  log.info(\"went on\");\n}\nfn spin() { loop { } }";
    let stopwatch = ManualStopwatch::stepping(MICROS_100);

    let (ran, world, _) = join(source, &stopwatch);

    assert_eq!(stopped(&ran), &hit(Limit::HookTime, Hook::Join));
    assert_eq!(ran.actions, [Action::ReleaseAll]);
    assert_eq!(world.log.len(), 0);
}

#[test]
fn a_gap_longer_than_a_stretch_may_be_counts_as_one() {
    // The host stood still for an hour between every two readings. Each
    // gap counts for the longest stretch, 1 ms: five of them are a hook's
    // time exactly, six are over.
    let stopwatch = ManualStopwatch::stepping(Duration::from_hours(1));
    let mut world = World::new();

    let five = work(&mut world, &mut timed(WORK, &stopwatch), rounds_of(5));
    world.now += Duration::from_secs(5);
    let six = work(&mut world, &mut timed(WORK, &stopwatch), rounds_of(6));

    assert!(matches!(five.outcome, Outcome::Ran { .. }), "{five:?}");
    assert_eq!(stopped(&six), &hit(Limit::HookTime, Hook::Timer));
}

/// A broken stopwatch: each reading is a millisecond earlier than the last.
#[derive(Debug)]
struct Backwards(AtomicU64);

impl Stopwatch for Backwards {
    fn elapsed(&self) -> Duration {
        Duration::from_millis(self.0.fetch_sub(1, Ordering::Relaxed))
    }
}

#[test]
fn a_stopwatch_that_goes_back_counts_as_one_that_stands_still() {
    let stopwatch = Arc::new(Backwards(AtomicU64::new(1_000_000)));
    let mut world = World::new();
    let mut instance = Instance::new(
        program("fn on_join(first) { loop { } }"),
        7,
        Arc::<Backwards>::clone(&stopwatch),
    );

    let ran = world.run(&mut instance, &HookEvent::Join { first: true });

    // Its operations end it, as if no time had passed.
    assert_eq!(stopped(&ran), &hit(Limit::OperationsPerHook, Hook::Join));
    assert!(stopwatch.0.load(Ordering::Relaxed) < 1_000_000 - 1_000);
}

#[test]
fn a_second_holds_no_more_of_a_scripts_time_than_its_budget() {
    // Five hooks of 4 ms each are a second's 20 ms.
    let rounds = rounds_of(40);
    let stopwatch = ManualStopwatch::stepping(MICROS_100);
    let mut world = World::new();
    let mut instance = timed(WORK, &stopwatch);
    for hook in 0..5 {
        world.now += Duration::from_millis(10);
        let ran = work(&mut world, &mut instance, rounds);
        assert!(
            matches!(ran.outcome, Outcome::Ran { .. }),
            "hook {hook}: {ran:?}"
        );
    }
    let readings = stopwatch.readings();

    // Nothing is left, so the next hook isn't run at all.
    world.now += Duration::from_millis(10);
    let refused = world.run(&mut instance, &HookEvent::Death);

    assert_eq!(stopped(&refused), &hit(Limit::TimePerSecond, Hook::Death));
    assert_eq!(refused.actions, [Action::ReleaseAll]);
    assert_eq!(world.log.len(), 0);
    assert_eq!(stopwatch.readings(), readings);
}

#[test]
fn the_last_hook_of_a_second_gets_only_the_time_that_is_left() {
    // Four hooks of 4 ms leave 4 ms; a hook that needs 4.5 ms stops for
    // the second's time, below a hook's own 5 ms.
    let stopwatch = ManualStopwatch::stepping(MICROS_100);
    let mut world = World::new();
    let mut instance = timed(WORK, &stopwatch);
    for _ in 0..4 {
        world.now += Duration::from_millis(10);
        work(&mut world, &mut instance, rounds_of(40));
    }

    world.now += Duration::from_millis(10);
    let fifth = work(&mut world, &mut instance, rounds_of(45));

    assert_eq!(stopped(&fifth), &hit(Limit::TimePerSecond, Hook::Timer));
}

#[test]
fn a_scripts_time_is_whole_again_a_second_later() {
    let rounds = rounds_of(40);
    let stopwatch = ManualStopwatch::stepping(MICROS_100);
    let mut world = World::new();
    let mut instance = timed(WORK, &stopwatch);

    for second in 0..3 {
        for _ in 0..5 {
            world.now += Duration::from_millis(1);
            let ran = work(&mut world, &mut instance, rounds);
            assert!(
                matches!(ran.outcome, Outcome::Ran { .. }),
                "second {second}: {ran:?}"
            );
        }
        world.now += Duration::from_millis(1_100);
    }
}

#[test]
fn on_stop_is_bounded_in_time_like_every_hook() {
    let stopwatch = ManualStopwatch::stepping(MICROS_100);
    let mut world = World::new();
    let mut instance = timed(WORK, &stopwatch);

    let ended = world.run(&mut instance, &HookEvent::Stop);

    assert_eq!(
        ended.outcome,
        Outcome::Ended {
            error: Some(hit(Limit::HookTime, Hook::Stop))
        }
    );
    assert_eq!(ended.actions, [Action::ReleaseAll]);
}

#[test]
fn a_hook_within_its_time_does_what_it_asked_for() {
    let stopwatch = ManualStopwatch::stepping(MICROS_100);

    let (ran, ..) = join(
        "fn on_join(first) { keys.jump.tap(); log.info(\"done\"); }",
        &stopwatch,
    );

    assert!(matches!(ran.outcome, Outcome::Ran { .. }), "{ran:?}");
    assert_eq!(logged(&ran.actions), ["done"]);
    assert!(ran.actions.len() > 1, "{ran:?}");
}

/// The OS's own clock, as whoever runs a script for real hands it in.
#[derive(Debug)]
struct Wall(Instant);

impl Stopwatch for Wall {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
}

/// Runs `on_join` of `source` against the OS's clock. Returns the limit
/// it stopped for and how long the hook really took.
#[expect(
    clippy::disallowed_methods,
    reason = "the three tests of the reviewer's cases time a hook with the OS's clock, as whoever runs a script does; ADR-0040's amendment, A1"
)]
fn against_the_clock(name: &str, source: &str) -> (Limit, Duration) {
    let mut world = World::new();
    let mut instance = Instance::new(program(source), 7, Arc::new(Wall(Instant::now())));
    let started = Instant::now();
    let ran = world.run(&mut instance, &HookEvent::Join { first: true });
    let took = started.elapsed();
    let ScriptErrorKind::Limit(limit) = stopped(&ran).kind else {
        panic!("{name}: {ran:?}");
    };
    println!("TIME {name}: stopped for {limit:?} after {took:?}");
    (limit, took)
}

/// The reviewer's second case: an array's method calls a function of the
/// script's for each of 30 items, again and again.
const MAP_OVER_THIRTY: &str = "fn on_join(first) {\n  let a = [];\n  a.pad(30, 1);\n  loop { a.map(Fn(\"id\")); }\n}\nfn id(x) { x }";

/// The reviewer's third case: a function builds a text of 1 MiB out of
/// one of 1 KiB, which Rhai refuses only once it's built; called through
/// an array's method, the refusal can be caught, and the loop goes on.
const TEXT_BOMB: &str = "fn on_join(first) {\n  loop { try { [1].map(Fn(\"bomb\")); } catch { } }\n}\nfn bomb(x) {\n  let s = \"\";\n  s.pad(1024, 'a');\n  s.replace(\"a\", s);\n  s\n}";

/// The same with an array: a text of 1 KiB split into its letters.
const SPLIT_BOMB: &str = "fn on_join(first) {\n  loop { try { [1].map(Fn(\"bomb\")); } catch { } }\n}\nfn bomb(x) {\n  let s = \"\";\n  s.pad(1024, 'a');\n  s.split(\"\")\n}";

// The three tests below read the OS's clock: what they hold is that the
// time bound ends these hooks on a real machine, not only on a stopwatch
// a test steps. They assert why a hook stopped, never how long it took:
// that's printed, and ADR-0040's amendment has the numbers.

#[test]
fn against_the_real_clock_a_plain_loop_ends_for_a_limit() {
    // Its operations in a release build; in a debug build, where an
    // operation costs many times as much, its time may come first.
    let (limit, _) = against_the_clock("a plain loop", "fn on_join(first) { loop { } }");

    assert!(
        matches!(limit, Limit::OperationsPerHook | Limit::HookTime),
        "{limit:?}"
    );
}

#[test]
fn against_the_real_clock_a_loop_over_an_arrays_method_ends_for_its_time() {
    let (limit, _) = against_the_clock("map over 30 items", MAP_OVER_THIRTY);

    assert_eq!(limit, Limit::HookTime);
}

#[test]
fn against_the_real_clock_a_caught_data_size_error_in_a_loop_ends_for_its_time() {
    let (text, _) = against_the_clock("a text of 1 MiB, caught", TEXT_BOMB);
    let (array, _) = against_the_clock("a text split into 1,024, caught", SPLIT_BOMB);

    assert_eq!((text, array), (Limit::HookTime, Limit::HookTime));
}

#[test]
fn the_reviewers_scripts_do_what_their_names_say() {
    // With a stopwatch that stands still, each runs to a hook's operations:
    // the refusals are caught and the loops go on.
    for source in [MAP_OVER_THIRTY, TEXT_BOMB, SPLIT_BOMB] {
        let (ran, ..) = join(source, &ManualStopwatch::standing());

        assert_eq!(
            stopped(&ran),
            &hit(Limit::OperationsPerHook, Hook::Join),
            "{source}"
        );
    }
    // And outside an array's method, the same refusal can't be caught.
    let (text, ..) = join(
        "fn on_join(first) { try { bomb(1); } catch { } log.info(\"went on\"); }\nfn bomb(x) { let s = \"\"; s.pad(1024, 'a'); s.replace(\"a\", s); s }",
        &ManualStopwatch::standing(),
    );
    assert_eq!(stopped(&text), &hit(Limit::TextBytes, Hook::Join));
}
