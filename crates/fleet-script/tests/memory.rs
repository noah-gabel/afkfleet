//! What a script can take of the process's memory, measured (P15.7;
//! ADR-0036, D5; the user's reviewer's notes).
//!
//! A binary of its own: it replaces the global allocator with one that
//! counts, `peak_alloc`, so the numbers are real allocations, not the
//! state's own size estimate. The tests share that counter, so they run
//! one at a time behind a lock.
//!
//! `cargo nextest run -p fleet-script --test memory --no-capture` prints
//! the numbers the ADR quotes, as `MEMORY` lines.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, TimeZone, Utc};
use fleet_core::mode::HotbarSlot;
use fleet_core::script::{
    ActionLimiter, BlockPosition, Blocks, BotFacts, HookEvent, Identifier, Position, Script,
    ScriptDraft, Snapshot,
};
use fleet_script::{Instance, Moment, Outcome, Presence, Program, compile};
use peak_alloc::PeakAlloc;

#[global_allocator]
static ALLOCATOR: PeakAlloc = PeakAlloc;

/// One measurement at a time: the counter is the whole process's.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// A stopwatch that stands still.
fn still() -> Arc<dyn fleet_core::script::Stopwatch> {
    Arc::new(fleet_testkit::stopwatch::ManualStopwatch::standing())
}

const MIB: usize = 1024 * 1024;

/// The most a hook that attacks the state may add to the process's
/// memory. ADR-0036's D5 asks for "never more than a few MiB"; measured,
/// the worst is a small part of one (ADR-0040), so the bound is one.
const STATE_ATTACK_BOUND: usize = MIB;

/// The most a hook that grows its own variables may add while it runs.
/// Rhai checks a map's size where it's passed on, not where an index
/// assignment adds to it, so what bounds a local map is the hook's
/// operations (ADR-0040).
const LOCAL_ATTACK_BOUND: usize = 8 * MIB;

fn program(source: &str) -> Arc<Program> {
    let draft = ScriptDraft {
        api: 1,
        source: source.to_owned(),
        leash: 8,
        tool_guard: 2,
        block_guard: Vec::new(),
        messages: Vec::new(),
        patterns: Vec::new(),
        signs: Vec::new(),
    };
    Arc::new(compile(&Script::try_from(draft).unwrap()).unwrap())
}

fn facts() -> Snapshot {
    let bot = BotFacts {
        health: 20.0,
        food: 20,
        position: Position::try_new(0.5, 64.0, 0.5).unwrap(),
        yaw: 0.0,
        pitch: 0.0,
        dimension: Identifier::try_from("overworld").unwrap(),
        riding: None,
        held: None,
        selected: HotbarSlot::FIRST,
        looking_at: None,
    };
    let blocks = Blocks::read(BlockPosition { x: 0, y: 64, z: 0 }, |_| None);
    Snapshot::new(bot, Vec::new(), blocks)
}

fn start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 17, 12, 0, 0).unwrap()
}

/// `bytes` in KiB, rounded down.
const fn kib(bytes: usize) -> usize {
    bytes / 1024
}

/// Runs `source`'s `on_join` once and returns how many bytes the process
/// held at most while it ran, above what it held before, with the outcome
/// and how far what it held after the instance was dropped is from before.
fn measure(program: &Arc<Program>) -> (usize, Outcome, usize) {
    let facts = facts();
    let mut limiter = ActionLimiter::new();
    let before = ALLOCATOR.current_usage();
    let mut instance = Instance::new(program.clone(), 7, still());
    ALLOCATOR.reset_peak_usage();
    let ran = instance.handle(
        &HookEvent::Join { first: true },
        &Moment {
            facts: &facts,
            now: start(),
            presence: Presence::InWorld,
            leash: None,
            menu: &fleet_core::mc::MenuView::default(),
            name: "AfkBot1",
        },
        &mut limiter,
    );
    let peak = ALLOCATOR.peak_usage().saturating_sub(before);
    let outcome = ran.outcome.clone();
    drop(ran);
    drop(instance);
    let left = ALLOCATOR.current_usage().abs_diff(before);
    (peak, outcome, left)
}

/// Scripts that try to fill the agent's memory through the state, each
/// within one hook's 10,000 operations.
const STATE_ATTACKS: [(&str, &str); 7] = [
    (
        "doubling the state by assigning it to itself",
        "fn on_join(first) {\n  state.a = \"0123456789abcdef0123456789abcdef\";\n  let i = 0;\n  loop { state[`k${i}`] = state; i += 1; }\n}",
    ),
    (
        "doubling two entries into each other",
        "fn on_join(first) {\n  state.a = [1, 2, 3, 4, 5, 6, 7, 8];\n  state.b = [1, 2, 3, 4, 5, 6, 7, 8];\n  loop { state.a += state.b; state.b += state.a; }\n}",
    ),
    (
        "a longest text under ever new keys",
        "fn on_join(first) {\n  let text = \"\";\n  text.pad(1000, 'x');\n  let i = 0;\n  loop { state[`k${i}`] = text + i; i += 1; }\n}",
    ),
    (
        "a largest array under ever new keys",
        "fn on_join(first) {\n  let items = [];\n  items.pad(256, 1.5);\n  let i = 0;\n  loop { state[`k${i}`] = items; i += 1; }\n}",
    ),
    (
        "a largest map of texts under ever new keys",
        "fn on_join(first) {\n  let text = \"\";\n  text.pad(15, 'x');\n  let entry = #{};\n  for i in 0..64 { entry[`e${i}`] = text; }\n  let i = 0;\n  loop { state[`k${i}`] = entry; i += 1; }\n}",
    ),
    (
        "a list in the state that only grows",
        "fn on_join(first) {\n  state.list = [];\n  let text = \"\";\n  text.pad(990, 'x');\n  loop { state.list.push(text + state.list.len); }\n}",
    ),
    (
        "nested lists copied into each other",
        "fn on_join(first) {\n  state.list = [[1, 2, 3, 4]];\n  loop { state.list.push(state.list); }\n}",
    ),
];

#[test]
fn a_script_that_fills_its_state_never_takes_more_than_a_few_mib() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let mut worst = 0;

    for (what, source) in STATE_ATTACKS {
        let program = program(source);
        // Once to warm up whatever Rhai allocates once for a process.
        measure(&program);
        let (peak, outcome, left) = measure(&program);

        println!(
            "MEMORY state attack: {what}: {} KiB at most, stopped with {}",
            kib(peak),
            match &outcome {
                Outcome::Stopped(error) => error.to_string(),
                other => format!("{other:?}"),
            }
        );
        assert!(
            matches!(outcome, Outcome::Stopped(_)),
            "{what}: {outcome:?}"
        );
        assert!(peak < STATE_ATTACK_BOUND, "{what}: {peak} bytes");
        assert!(left < 64 * 1024, "{what}: {left} bytes left");
        worst = worst.max(peak);
    }

    println!(
        "MEMORY state attacks, the worst: {} KiB (the bound is {} KiB)",
        kib(worst),
        kib(STATE_ATTACK_BOUND)
    );
}

/// Scripts that grow their own variables instead, which no state check
/// sees.
const LOCAL_ATTACKS: [(&str, &str); 4] = [
    (
        "a local map of longest texts",
        "fn on_join(first) {\n  let text = \"\";\n  text.pad(1000, 'x');\n  let m = #{};\n  let i = 0;\n  loop { m[`k${i}`] = text + i; i += 1; }\n}",
    ),
    (
        "a local map of largest arrays",
        "fn on_join(first) {\n  let items = [];\n  items.pad(256, 1.5);\n  let m = #{};\n  let i = 0;\n  loop { m[`k${i}`] = items; i += 1; }\n}",
    ),
    (
        "a local map of maps of texts",
        "fn on_join(first) {\n  let text = \"\";\n  text.pad(15, 'x');\n  let entry = #{};\n  for i in 0..64 { entry[`e${i}`] = text; }\n  let m = #{};\n  let i = 0;\n  loop { m[`k${i}`] = entry; i += 1; }\n}",
    ),
    (
        "a local map of arrays that each hold a long text too",
        "fn on_join(first) {\n  let text = \"\";\n  text.pad(1000, 'x');\n  let items = [];\n  items.pad(255, 1.5);\n  let m = #{};\n  let i = 0;\n  loop { let item = items; item.push(text + i); m[`k${i}`] = item; i += 1; }\n}",
    ),
];

#[test]
fn a_script_that_fills_its_own_variables_is_bounded_by_its_operations() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let mut worst = 0;

    for (what, source) in LOCAL_ATTACKS {
        let program = program(source);
        measure(&program);
        let (peak, outcome, left) = measure(&program);

        println!(
            "MEMORY local attack: {what}: {} KiB at most, stopped with {}",
            kib(peak),
            match &outcome {
                Outcome::Stopped(error) => error.to_string(),
                other => format!("{other:?}"),
            }
        );
        assert!(
            matches!(outcome, Outcome::Stopped(_)),
            "{what}: {outcome:?}"
        );
        assert!(peak < LOCAL_ATTACK_BOUND, "{what}: {peak} bytes");
        // What a hook's variables held is gone with the hook.
        assert!(left < 64 * 1024, "{what}: {left} bytes left");
        worst = worst.max(peak);
    }

    println!(
        "MEMORY local attacks, the worst: {} KiB (the bound is {} KiB)",
        kib(worst),
        kib(LOCAL_ATTACK_BOUND)
    );
}

/// Scripts whose state isn't plain data when they stop: a function pointer
/// that carries the state, kept in the state.
///
/// With an anonymous function this was a state that held itself and was
/// never freed (the user's reviewer's finding 3). A script has none now;
/// what's left of it is a pointer that carries a copy. `state.rs`'s own
/// tests build the state that holds itself by hand.
const CYCLES: [(&str, &str); 3] = [
    (
        "a function pointer with the state, in the state",
        "fn on_join(first) {\n  state.text = \"\";\n  state.text.pad(1000, 'x');\n  state.f = Fn(\"on_join\").curry(state);\n}",
    ),
    (
        "a function pointer with the state, in a list in the state",
        "fn on_join(first) {\n  state.text = \"\";\n  state.text.pad(1000, 'x');\n  state.list = [Fn(\"on_join\").curry(state)];\n}",
    ),
    (
        "a function pointer with the state, and then more work",
        "fn on_join(first) {\n  state.text = \"\";\n  state.text.pad(1000, 'x');\n  state.f = Fn(\"on_join\").curry(state);\n  let n = 0;\n  while n < 1000 { n += 1; }\n}",
    ),
];

#[test]
fn a_stopped_instances_state_is_freed() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);

    for (what, source) in CYCLES {
        let program = program(source);
        measure(&program);
        let before = ALLOCATOR.current_usage();

        // Each instance's state holds a text of 1,000 bytes: left behind
        // 2,000 times, that's 2 MiB.
        for _ in 0..2_000 {
            let (_, outcome, _) = measure(&program);
            assert!(
                matches!(outcome, Outcome::Stopped(_)),
                "{what}: {outcome:?}"
            );
        }

        let left = ALLOCATOR.current_usage().abs_diff(before);
        println!("MEMORY cycle: {what}: {left} bytes left after 2,000 stopped instances");
        assert!(left < 64 * 1024, "{what}: {left} bytes left");
    }
}

#[test]
fn an_instance_that_ends_or_is_dropped_frees_its_state() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let program = program(
        "fn on_join(first) {\n  state.text = \"\";\n  state.text.pad(1000, 'x');\n  state.list = [1, 2, 3];\n}\nfn on_stop() { }",
    );
    let facts = facts();
    measure(&program);
    let before = ALLOCATOR.current_usage();

    for round in 0..2_000 {
        let mut limiter = ActionLimiter::new();
        let mut instance = Instance::new(program.clone(), 7, still());
        let menu = fleet_core::mc::MenuView::default();
        let moment = Moment {
            facts: &facts,
            now: start(),
            presence: Presence::InWorld,
            leash: None,
            menu: &menu,
            name: "AfkBot1",
        };
        instance.handle(&HookEvent::Join { first: true }, &moment, &mut limiter);
        // Half of them end in an orderly way; the others are just dropped.
        if round % 2 == 0 {
            instance.handle(&HookEvent::Stop, &moment, &mut limiter);
        }
    }

    let left = ALLOCATOR.current_usage().abs_diff(before);
    println!("MEMORY 2,000 instances ended or dropped: {left} bytes left");
    assert!(left < 64 * 1024, "{left} bytes left");
}

#[test]
fn two_hundred_instances_of_one_program_cost_little() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let program = program(
        "fn on_join(first) { state.n = 1; timer.every(1000, \"t\"); }\nfn on_timer(name) { state.n += 1; }",
    );
    drop(Instance::new(program.clone(), 0, still()));
    let before = ALLOCATOR.current_usage();

    let instances: Vec<Instance> = (0..200)
        .map(|seed| Instance::new(program.clone(), seed, still()))
        .collect();
    let held = ALLOCATOR.current_usage().saturating_sub(before);

    println!(
        "MEMORY 200 instances of one program: {} KiB, {} bytes each",
        kib(held),
        held / 200
    );
    assert_eq!(instances.len(), 200);
    // Each has an engine of its own over shared packages: tens of KiB at
    // most, not Engine::new()'s 220 KiB.
    assert!(held < 8 * MIB, "{held} bytes");
}
