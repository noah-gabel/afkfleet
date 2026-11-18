//! Every limit of ADR-0036's table, hit from a hook and from a helper
//! function, and the two operation budgets (P15.5, P15.7).
//!
//! The limits that are checked when a script is saved (its size, its
//! functions, an expression's depth) are in `validation`; the state's are
//! in `state`; the timers' in `timers`; the log's in `objects`.

use core::fmt::Write;
use core::time::Duration;

use fleet_core::script::{Hook, HookEvent, limits};
use fleet_script::{Action, Instance, Limit, Outcome, ScriptError, ScriptErrorKind};
use rstest::rstest;

use crate::support::{World, instance, join, logged, operations, stopped, timer};

/// What a script that hit `limit` in `on_join` stopped with.
const fn hit(limit: Limit) -> ScriptError {
    ScriptError {
        kind: ScriptErrorKind::Limit(limit),
        hook: Hook::Join,
        at: None,
    }
}

/// `body` as `on_join`'s own, and as a helper's that `on_join` calls.
fn in_hook_and_in_helper(body: &str) -> [String; 2] {
    [
        format!("fn on_join(first) {{\n{body}\n}}"),
        format!("fn on_join(first) {{\n  helper();\n}}\nfn helper() {{\n{body}\n}}"),
    ]
}

#[rstest]
#[case::operations("  loop { }", Limit::OperationsPerHook)]
#[case::operations_counting("  let n = 0;\n  loop { n += 1; }", Limit::OperationsPerHook)]
#[case::operations_in_a_range("  for i in 0..1000000000 { }", Limit::OperationsPerHook)]
#[case::text("  let s = \"x\";\n  loop { s += s; }", Limit::TextBytes)]
#[case::text_padded("  let s = \"\";\n  s.pad(2000, 'x');", Limit::TextBytes)]
#[case::array("  let a = [];\n  loop { a.push(1); }", Limit::ArrayItems)]
#[case::array_padded("  let a = [];\n  a.pad(1000, 0);", Limit::ArrayItems)]
#[case::map(
    "  let m = #{};\n  for i in 0..100 { m[`k${i}`] = i; }\n  m.len();",
    Limit::MapEntries
)]
#[case::map_mixed_in(
    "  let m = #{};\n  for i in 0..100 { m[`k${i}`] = i; }\n  let other = #{};\n  other += m;",
    Limit::MapEntries
)]
#[case::map_of_copies(
    "  let m = #{};\n  for i in 0..100 { m[`k${i}`] = m; }",
    Limit::MapEntries
)]
fn a_limit_stops_the_script_from_a_hook_and_from_a_helper(
    #[case] body: &str,
    #[case] limit: Limit,
) {
    for source in in_hook_and_in_helper(body) {
        let (ran, _, instance) = join(&source);

        assert_eq!(stopped(&ran), &hit(limit), "{source}");
        assert_eq!(ran.actions, [Action::ReleaseAll], "{source}");
        assert_eq!(instance.error(), Some(&hit(limit)), "{source}");
    }
}

#[test]
fn endless_recursion_stops_the_script_from_a_hook_and_from_a_helper() {
    let sources = [
        "fn on_join(first) {\n  on_join(first);\n}",
        "fn on_join(first) {\n  down(1);\n}\nfn down(n) {\n  down(n + 1);\n}",
        "fn on_join(first) {\n  a();\n}\nfn a() { b(); }\nfn b() { a(); }",
    ];

    for source in sources {
        let (ran, ..) = join(source);

        assert_eq!(stopped(&ran), &hit(Limit::CallDepth), "{source}");
        assert_eq!(ran.actions, [Action::ReleaseAll]);
    }
}

#[test]
fn functions_call_each_other_as_deep_as_the_limit_and_no_deeper() {
    let depth = |levels: usize| {
        format!(
            "fn on_join(first) {{ down({levels}); }}\nfn down(n) {{ if n > 1 {{ down(n - 1); }} }}"
        )
    };

    // The limit counts the calls below the hook itself.
    let (within, ..) = join(&depth(limits::MAX_CALL_DEPTH));
    let (beyond, ..) = join(&depth(limits::MAX_CALL_DEPTH + 1));

    assert!(matches!(within.outcome, Outcome::Ran { .. }), "{within:?}");
    assert_eq!(stopped(&beyond), &hit(Limit::CallDepth));
}

#[test]
fn a_script_cant_catch_a_limit() {
    let bodies = [
        "  try { loop { } } catch { log.info(\"caught\"); }\n  log.info(\"went on\");",
        "  try { let s = \"x\"; loop { s += s; } } catch { log.info(\"caught\"); }\n  log.info(\"went on\");",
        "  try { let a = []; loop { a.push(1); } } catch { log.info(\"caught\"); }\n  log.info(\"went on\");",
        "  try { again(); } catch { log.info(\"caught\"); }\n  log.info(\"went on\");",
    ];

    for body in bodies {
        let source = format!("fn on_join(first) {{\n{body}\n}}\nfn again() {{ again(); }}");
        let (ran, ..) = join(&source);

        assert!(matches!(ran.outcome, Outcome::Stopped(_)), "{source}");
        assert_eq!(ran.actions, [Action::ReleaseAll], "{source}");
    }
}

#[test]
fn what_a_hook_asked_for_before_it_hit_a_limit_is_dropped() {
    let (ran, world, _) = join(
        "fn on_join(first) {\n  keys.forward.hold();\n  chat.send(\"x\");\n  timer.after(1000, \"t\");\n  loop { }\n}",
    );

    assert_eq!(ran.actions, [Action::ReleaseAll]);
    assert_eq!(ran.next_wake, None);
    assert_eq!(world.sent.len(), 0);
}

#[test]
fn a_text_is_limited_in_bytes_not_in_characters() {
    // 400 times a two-byte letter is 800 bytes; 600 times is 1,200 bytes
    // and only 600 characters.
    let filled = |count: u32| {
        format!(
            "fn on_join(first) {{ let s = \"\"; for i in 0..{count} {{ s += \"\u{e4}\"; }} log.info(`${{s.len}}`); }}"
        )
    };

    let (within, ..) = join(&filled(400));
    let (beyond, ..) = join(&filled(600));

    assert_eq!(logged(&within.actions), ["400"]);
    assert_eq!(stopped(&beyond), &hit(Limit::TextBytes));
}

#[test]
fn a_text_an_array_and_a_map_at_their_limits_are_fine() {
    let (ran, ..) = join(
        "fn on_join(first) {\n  let s = \"\";\n  s.pad(1024, 'x');\n  let a = [];\n  a.pad(256, 0);\n  let m = #{};\n  for i in 0..64 { m[`k${i}`] = i; }\n  let copy = m;\n  log.info(`${s.len} ${a.len} ${copy.len()}`);\n}",
    );

    assert_eq!(logged(&ran.actions), ["1024 256 64"]);
}

#[test]
fn more_variables_than_the_limit_stop_the_script() {
    let declare = |count: usize| {
        let lets = (0..count).fold(String::new(), |mut lets, index| {
            let _ = writeln!(lets, "  let v{index} = {index};");
            lets
        });
        format!("fn on_join(first) {{\n{lets}}}")
    };

    let (within, ..) = join(&declare(60));
    let (beyond, ..) = join(&declare(80));

    assert!(matches!(within.outcome, Outcome::Ran { .. }), "{within:?}");
    assert_eq!(
        stopped(&beyond).kind,
        ScriptErrorKind::Limit(Limit::Variables)
    );
}

#[test]
fn a_hook_below_its_budget_says_how_many_operations_it_ran() {
    let (empty, ..) = join("fn on_join(first) { }");
    let (looped, ..) = join("fn on_join(first) { let n = 0; while n < 100 { n += 1; } }");

    assert!(operations(&empty) < 10);
    assert!((100..2_000).contains(&operations(&looped)));
}

#[test]
fn a_stopped_script_runs_nothing_more() {
    let source = "fn on_join(first) { loop { } }\nfn on_timer(name) { keys.jump.tap(); }\nfn on_stop() { log.info(\"bye\"); }";
    let (first, mut world, mut instance) = join(source);
    assert!(matches!(first.outcome, Outcome::Stopped(_)));

    let timer_event = world.run(&mut instance, &HookEvent::Timer(timer("t")));
    let rejoin = world.run(&mut instance, &HookEvent::Join { first: false });
    let woken = world.wake(&mut instance);
    let stop = world.run(&mut instance, &HookEvent::Stop);

    for later in [timer_event, rejoin, woken, stop] {
        assert_eq!(later.outcome, Outcome::Over);
        assert_eq!(later.actions.len(), 0);
        assert_eq!(later.next_wake, None);
    }
    assert_eq!(instance.error(), Some(&hit(Limit::OperationsPerHook)));
}

/// A script whose `on_timer` runs a loop as often as the timer's name says.
const WORK: &str = "fn on_timer(name) { let n = 0; let rounds = parse_int(name); while n < rounds { n += 1; } }\nfn on_death() { log.info(\"ran\"); }";

/// Runs [`WORK`]'s loop for `rounds` and returns what the hook did.
fn work(world: &mut World, instance: &mut Instance, rounds: u64) -> fleet_script::Ran {
    world.run(instance, &HookEvent::Timer(timer(&rounds.to_string())))
}

/// How many operations [`WORK`]'s `on_timer` runs for `rounds`, in a second
/// of its own.
fn cost(rounds: u64) -> u64 {
    let mut world = World::new();
    let mut instance = instance(WORK);
    operations(&work(&mut world, &mut instance, rounds))
}

/// How many rounds make [`WORK`]'s `on_timer` cost `operations`, or as
/// near below as a whole round comes.
fn rounds_costing(operations: u64) -> u64 {
    let base = cost(0);
    let step = cost(1) - base;
    (operations - base) / step
}

#[test]
fn the_work_script_is_linear_in_its_rounds() {
    let base = cost(0);
    let step = cost(1) - base;

    assert!(step > 0);
    assert_eq!(cost(100), base + 100 * step);
    assert_eq!(cost(1_000), base + 1_000 * step);
    assert!(cost(rounds_costing(9_000)) <= 9_000);
    assert!(cost(rounds_costing(9_000)) > 9_000 - step);
}

#[test]
fn a_second_holds_no_more_operations_than_its_budget() {
    // Each hook stays under a hook's own budget; together they pass a
    // second's.
    let rounds = rounds_costing(7_000);
    let each = cost(rounds);
    assert!(each < limits::OPERATIONS_PER_HOOK, "{each}");
    let fit = limits::OPERATIONS_PER_SECOND / each;
    assert!(fit >= 3, "{each}");

    let mut world = World::new();
    let mut instance = instance(WORK);
    let mut used = 0;
    for _ in 0..fit {
        world.now += Duration::from_millis(10);
        used += operations(&work(&mut world, &mut instance, rounds));
    }
    assert!(used <= limits::OPERATIONS_PER_SECOND);
    world.now += Duration::from_millis(10);
    let over = work(&mut world, &mut instance, rounds);

    assert_eq!(
        stopped(&over),
        &ScriptError {
            kind: ScriptErrorKind::Limit(Limit::OperationsPerSecond),
            hook: Hook::Timer,
            at: None,
        }
    );
    assert_eq!(over.actions, [Action::ReleaseAll]);
}

#[test]
fn a_seconds_budget_counts_the_work_of_helper_functions() {
    // The same loop, three calls down from the hook.
    let source = "fn on_timer(name) { one(parse_int(name)); }\nfn one(rounds) { two(rounds); }\nfn two(rounds) { let n = 0; while n < rounds { n += 1; } }";
    let rounds = rounds_costing(7_000);
    let mut world = World::new();
    let mut instance = instance(source);
    let each = operations(&work(&mut world, &mut instance, rounds));
    assert!(each < limits::OPERATIONS_PER_HOOK, "{each}");

    // The first hook ran above. As many as fit run; the one after stops
    // the script.
    let fit = limits::OPERATIONS_PER_SECOND / each;
    assert!(fit >= 3, "{each}");
    for hook in 1..fit {
        let ran = work(&mut world, &mut instance, rounds);
        assert!(
            matches!(ran.outcome, Outcome::Ran { .. }),
            "hook {hook}: {ran:?}"
        );
    }
    let over = work(&mut world, &mut instance, rounds);

    assert_eq!(
        stopped(&over).kind,
        ScriptErrorKind::Limit(Limit::OperationsPerSecond)
    );
}

#[test]
fn the_budget_is_whole_again_a_second_later() {
    let rounds = rounds_costing(7_000);
    let fit = limits::OPERATIONS_PER_SECOND / cost(rounds);

    let mut world = World::new();
    let mut instance = instance(WORK);
    for second in 0..3 {
        for _ in 0..fit {
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
fn a_hook_that_finds_nothing_left_of_the_second_isnt_run_at_all() {
    // Hooks whose operations add up to a second's budget exactly: the
    // loop's rounds are chosen from what one round costs.
    let base = cost(0);
    let step = cost(1) - base;
    let budget = limits::OPERATIONS_PER_SECOND;
    let (hooks, rounds) = (3..=12_u64)
        .find_map(|hooks| {
            let left = budget.checked_sub(hooks * base)?;
            (left % step == 0).then_some((hooks, left / step))
        })
        .expect("some number of hooks adds up to the budget");
    // Spread the rounds over the hooks; the last takes the rest.
    let share = rounds / hooks;
    let last = rounds - share * (hooks - 1);
    assert!(base + last * step <= limits::OPERATIONS_PER_HOOK);

    let mut world = World::new();
    let mut instance = instance(WORK);
    let mut used = 0;
    for hook in 0..hooks {
        let rounds = if hook + 1 == hooks { last } else { share };
        used += operations(&work(&mut world, &mut instance, rounds));
    }
    assert_eq!(used, budget);

    // Nothing is left, so nothing runs: not one operation, and never an
    // unlimited hook, which a budget of 0 would be to Rhai.
    let refused = world.run(&mut instance, &HookEvent::Death);

    assert_eq!(
        stopped(&refused),
        &ScriptError {
            kind: ScriptErrorKind::Limit(Limit::OperationsPerSecond),
            hook: Hook::Death,
            at: None,
        }
    );
    assert_eq!(world.log.len(), 0);
    assert_eq!(refused.actions, [Action::ReleaseAll]);
}

#[test]
fn the_last_hook_of_a_second_gets_only_what_is_left() {
    // Three hooks near a hook's budget leave less than a fourth needs: it
    // stops for the second's budget, far below a hook's own.
    let rounds = rounds_costing(9_000);
    let each = cost(rounds);
    assert!(each < limits::OPERATIONS_PER_HOOK);
    assert!(each * 3 < limits::OPERATIONS_PER_SECOND && each * 4 > limits::OPERATIONS_PER_SECOND);

    let mut world = World::new();
    let mut instance = instance(WORK);
    for _ in 0..3 {
        work(&mut world, &mut instance, rounds);
    }
    let fourth = work(&mut world, &mut instance, rounds);

    assert_eq!(
        stopped(&fourth).kind,
        ScriptErrorKind::Limit(Limit::OperationsPerSecond)
    );
}

/// The ways a function nests the call that goes one level down: what
/// stands before it, the call itself (`NEXT` is the function called) or
/// what stands there in the last function, and what stands after it.
const NESTS: [(&str, &str, &str, &str, &str); 9] = [
    ("parentheses", "(", "NEXT()", "1", ")"),
    ("blocks", "if true { ", "NEXT();", "1;", " }"),
    ("loops", "for i in 0..1 { ", "NEXT();", "1;", " }"),
    ("arguments", "pass(", "NEXT()", "1", ")"),
    ("texts", "`${", "NEXT()", "1", "}`"),
    ("arrays", "[", "NEXT()", "1", "]"),
    ("sums", "1 + (", "NEXT()", "1", ")"),
    (
        "functions handed to an array's method",
        "(",
        "[1].map(Fn(\"NEXT\"))",
        "1",
        ")",
    ),
    (
        "functions called by name",
        "(",
        "call(Fn(\"NEXT\"))",
        "1",
        ")",
    ),
];

/// A script whose hook calls `chain` functions, one inside the other, each
/// nesting its call `depth` deep in one of [`NESTS`]' ways.
fn nested_all_the_way(nest: (&str, &str, &str, &str, &str), depth: usize, chain: usize) -> String {
    let (_, open, call, last, close) = nest;
    let body = |inside: &str| format!("{}{inside}{}", open.repeat(depth), close.repeat(depth));
    let mut source = format!(
        "fn on_join(first) {{ {} }}\nfn pass(x) {{ x }}\n",
        body(&call.replace("NEXT", "f1"))
    );
    for level in 1..=chain {
        let inside = if level == chain {
            last.to_owned()
        } else {
            call.replace("NEXT", &format!("f{}", level + 1))
        };
        let _ = writeln!(source, "fn f{level}() {{ {} }}", body(&inside));
    }
    source
}

// The user's answer to ADR-0040: 32 levels of nesting inside a function,
// Rhai's own default for a release build, now that the instance bounds the
// stack by measuring. So the most deeply nested script the limits allow
// must be safe on a thread of the async runtime, 2 MiB, in whatever build
// this is: it's compiled there and run there, and a stack that ran out
// would end this whole test binary. In a release build such a script
// runs; in a debug build, where a level takes several times the stack,
// the stack guard ends it.
#[test]
fn the_most_deeply_nested_script_never_overflows_a_runtime_threads_stack() {
    const RUNTIME_THREAD_STACK: usize = 2 * 1024 * 1024;

    let ended = std::thread::Builder::new()
        .stack_size(RUNTIME_THREAD_STACK)
        .spawn(|| {
            let compiled = |nest, depth, chain| {
                // Too long to be a script at all, or nested too deep: `None`.
                let draft = crate::support::draft(&nested_all_the_way(nest, depth, chain));
                let script = fleet_core::script::Script::try_from(draft).ok()?;
                fleet_script::compile(&script).ok()
            };
            NESTS
                .iter()
                .map(|nest| {
                    // The deepest nesting that compiles, found by compiling
                    // here.
                    let depth = (0..=80)
                        .rev()
                        .find(|depth| compiled(*nest, *depth, 1).is_some())
                        .expect("some depth compiles");
                    // And the longest chain of calls the call limit lets
                    // run: a nest that calls a function of its own (an
                    // argument's `pass`, an array's method) takes a level
                    // of the limit itself.
                    let (chain, outcome) = (1..=limits::MAX_CALL_DEPTH)
                        .rev()
                        .find_map(|chain| {
                            let program = compiled(*nest, depth, chain)?;
                            let mut world = World::new();
                            let mut instance = Instance::new(
                                std::sync::Arc::new(program),
                                7,
                                crate::support::still(),
                            );
                            let ran = world.run(&mut instance, &HookEvent::Join { first: true });
                            let too_many_calls = matches!(
                                &ran.outcome,
                                Outcome::Stopped(error) if error == &hit(Limit::CallDepth)
                            );
                            (!too_many_calls).then_some((chain, ran.outcome))
                        })
                        .expect("some chain of calls is within the call limit");
                    (nest.0, depth, chain, outcome)
                })
                .collect::<Vec<_>>()
        })
        .unwrap()
        .join()
        .expect("no nest overflowed the thread's stack");

    for (name, depth, chain, outcome) in ended {
        println!("STACK {depth} nested {name} in each of {chain} calls: {outcome:?}");
        assert!(depth >= 4, "{name}: {depth}");
        assert!(chain >= 4, "{name}: {chain}");
        match outcome {
            Outcome::Ran { .. } => {}
            Outcome::Stopped(error) => {
                assert_eq!(error, hit(Limit::Stack), "{name}");
            }
            other => panic!("{name}: {other:?}"),
        }
    }
}
