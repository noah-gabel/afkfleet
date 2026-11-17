//! What the sandbox holds beyond the limits' numbers: the state while a
//! call on it runs, what the compiler's placeholders never reach, and how
//! much of a thread's stack the deepest script takes (ADR-0040).

use core::fmt::Write;
use std::thread;

use fleet_core::script::{Input, Key, limits};
use fleet_script::{Action, Limit, PressKind, ScriptErrorKind, compile};
use rstest::rstest;

use crate::support::{draft, join, logged, script_of, stopped};

#[rstest]
#[case::writing(
    "fn on_join(first) {\n  state.list = [1, 2, 3];\n  state.list.for_each(Fn(\"touch\"));\n  log.info(\"returned\");\n}\nfn touch() { state.seen = true; }"
)]
#[case::reading(
    "fn on_join(first) {\n  state.min = 2;\n  state.list = [1, 2, 3];\n  let big = state.list.filter(Fn(\"big\"));\n  log.info(\"returned\");\n}\nfn big(x) { x >= state.min }"
)]
#[case::catching(
    "fn on_join(first) {\n  state.list = [1, 2, 3];\n  try { state.list.for_each(Fn(\"touch\")); } catch { log.info(\"caught\"); }\n  log.info(\"returned\");\n}\nfn touch() { state.seen = true; }"
)]
#[case::catching_inside(
    "fn on_join(first) {\n  state.list = [1, 2, 3];\n  state.list.for_each(Fn(\"touch\"));\n  log.info(\"returned\");\n}\nfn touch() { try { state.seen = true; } catch { log.info(\"caught\"); } }"
)]
fn using_the_state_inside_a_function_that_a_call_on_the_state_runs_stops_the_script(
    #[case] source: &str,
) {
    // Rhai holds the state locked while `for_each` runs on a list inside
    // it, and would wait 50 ms for it each time the function it calls
    // touches the state, then fail in a way a script can catch. The
    // script stops at once instead, and can't catch that.
    let (ran, world, instance) = join(source);

    assert_eq!(stopped(&ran).kind, ScriptErrorKind::StateInUse, "{source}");
    assert_eq!(ran.actions, [Action::ReleaseAll]);
    assert_eq!(world.log.len(), 0);
    assert_eq!(instance.state_size(), (0, 0));
}

#[test]
fn a_copy_of_a_list_from_the_state_is_free_to_call_functions_that_use_the_state() {
    let (ran, ..) = join(
        "fn on_join(first) {\n  state.min = 2;\n  state.list = [1, 2, 3];\n  let list = state.list;\n  let big = list.filter(Fn(\"big\"));\n  log.info(`${big}`);\n  state.list = big;\n}\nfn big(x) { x >= state.min }",
    );

    assert_eq!(logged(&ran.actions), ["[2, 3]"]);
}

#[test]
fn a_function_that_doesnt_use_the_state_may_run_inside_a_call_on_it() {
    let (ran, ..) = join(
        "fn on_join(first) {\n  state.list = [3, 1, 2];\n  let doubled = state.list.map(Fn(\"double\"));\n  log.info(`${doubled}`);\n}\nfn double(x) { keys.jump.tap(); x * 2 }",
    );

    assert_eq!(logged(&ran.actions), ["[6, 2, 4]"]);
}

#[test]
fn the_state_is_still_checked_after_a_call_that_held_it() {
    // While `map` runs on a list in the state, the state can't be read:
    // those checks are skipped. The one after the hook sees it.
    let (ran, ..) = join(
        "fn on_join(first) {\n  state.list = [1, 2, 3];\n  state.list = state.list.map(Fn(\"work\"));\n  state.f = Fn(\"work\");\n}\nfn work(x) { let n = 0; while n < 200 { n += 1; } x }",
    );

    assert_eq!(stopped(&ran).kind, ScriptErrorKind::StateNotPlain);
}

// The compiler knows the game objects as placeholders. Nothing of a
// placeholder may reach a running script: every use is the instance's own
// object, also where an optimizer would have copied a constant in.
#[test]
fn every_use_of_the_state_reaches_the_instances_own_state() {
    let (ran, ..) = join(
        "fn on_join(first) {\n  state.a = 1;\n  let has = \"a\" in state;\n  let alias = state;\n  let size = count(state);\n  let kind = type_of(state);\n  log.info(`${has} ${alias.a} ${size} ${kind}`);\n}\nfn count(map) { map.len() }",
    );

    assert_eq!(logged(&ran.actions), ["true 1 1 map"]);
}

#[test]
fn a_game_object_passed_on_or_kept_in_a_variable_is_the_instances_own() {
    let source = "fn on_join(first) {\n  let hand = mouse;\n  let jump = keys.jump;\n  press(jump);\n  click(hand);\n  log.info(`${type_of(keys)} ${type_of(jump)} ${type_of(bot)}`);\n}\nfn press(key) { key.tap(); }\nfn click(buttons) { buttons.left.tap(); }";

    let (ran, world, _) = join(source);

    assert_eq!(
        world.presses(),
        [
            (Input::Key(Key::Jump), PressKind::Tap),
            (
                Input::Mouse(fleet_core::script::MouseButton::Left),
                PressKind::Tap
            )
        ]
    );
    assert_eq!(logged(&ran.actions), ["keys input bot"]);
}

#[test]
fn eval_cant_be_reached_through_a_function_pointer_either() {
    for call in [
        "Fn(\"eval\").call(\"1 + 1\")",
        "call(Fn(\"eval\"), \"1 + 1\")",
    ] {
        let (ran, ..) = join(&format!("fn on_join(first) {{\n  {call};\n}}"));

        assert_eq!(
            stopped(&ran).kind,
            ScriptErrorKind::Failed {
                message: "Syntax error: 'eval' is a reserved keyword".to_owned()
            },
            "{call}"
        );
    }
}

#[rstest]
#[case("sleep(1)")]
#[case("sleep(0.5)")]
#[case("exit()")]
#[case("timestamp()")]
#[case("parse_json(\"{}\")")]
#[case("blob(8)")]
fn a_script_has_no_function_that_blocks_ends_or_reads_outside(#[case] call: &str) {
    let (ran, ..) = join(&format!("fn on_join(first) {{\n  {call};\n}}"));

    match &stopped(&ran).kind {
        ScriptErrorKind::Failed { message } => {
            assert!(
                message.starts_with("Function not found: "),
                "{call}: {message}"
            );
        }
        other => panic!("{call}: {other:?}"),
    }
}

/// The deepest `depth`, at most 12, at which `script(depth)` still
/// compiles, with that script.
fn deepest(script: impl Fn(usize) -> String) -> (usize, String) {
    (0..=12)
        .rev()
        .map(|depth| (depth, script(depth)))
        .find(|(_, source)| compile(&script_of(draft(source))).is_ok())
        .expect("a script without any nest compiles")
}

/// A function that calls itself for ever, each call from inside `depth`
/// nested calls of a function that hands its argument back: the nest that
/// takes the most of a thread's stack (ADR-0040).
fn recursion(depth: usize) -> String {
    format!(
        "fn on_join(first) {{ down(); }}\nfn down() {{ {}down(){} }}\nfn pass(x) {{ x }}",
        "pass(".repeat(depth),
        ")".repeat(depth)
    )
}

/// A chain of as many calls as the limit allows below the hook, each from
/// inside `depth` nested parentheses, which ends by itself.
fn chain(depth: usize) -> String {
    let (open, close) = ("(".repeat(depth), ")".repeat(depth));
    let mut source = String::from("fn on_join(first) { let n = f1(1); log.info(`${n}`); }\n");
    for level in 1..limits::MAX_CALL_DEPTH {
        let _ = writeln!(
            source,
            "fn f{level}(n) {{ {open}f{next}(n + 1){close} }}",
            next = level + 1
        );
    }
    let _ = writeln!(source, "fn f{}(n) {{ n }}", limits::MAX_CALL_DEPTH);
    source
}

/// The stack a thread of the async runtime has, and so a hook's.
const RUNTIME_THREAD_STACK: usize = 2 * 1024 * 1024;

#[test]
fn a_chain_of_calls_as_long_as_the_limit_runs_to_its_end() {
    // One pair of parentheses around each call: the chain itself is what
    // this is about.
    let (ran, ..) = join(&chain(1));

    assert_eq!(logged(&ran.actions), [limits::MAX_CALL_DEPTH.to_string()]);
}

// A stack that ran out would end the whole process, and every bot of the
// agent with it. Whatever the limits allow, and whatever the build, the
// hook ends as an error of the script: for the calls' depth where the
// frames are small, for the stack where they're large.
#[test]
fn the_deepest_recursion_the_limits_allow_ends_the_script_and_never_the_process() {
    let (depth, source) = deepest(recursion);
    assert!(depth >= 3, "{depth}");

    let ran = thread::Builder::new()
        .stack_size(RUNTIME_THREAD_STACK)
        .spawn(move || join(&source).0)
        .unwrap()
        .join()
        .unwrap();

    assert!(
        matches!(
            stopped(&ran).kind,
            ScriptErrorKind::Limit(Limit::CallDepth | Limit::Stack)
        ),
        "{ran:?}"
    );
    assert_eq!(ran.actions, [Action::ReleaseAll]);
}

#[test]
fn every_way_to_nest_a_call_ends_the_script_and_never_the_process() {
    let nests = [
        ("(", "down()", ")"),
        ("if true { ", "down();", " }"),
        ("for i in 0..1 { ", "down();", " }"),
        ("`${", "down()", "}`"),
        ("[", "down()", "]"),
        ("1 + (", "down()", ")"),
        ("(", "[1].map(Fn(\"down\"))", ")"),
        ("(", "call(Fn(\"down\"))", ")"),
        ("switch 1 { _ => ", "down()", " }"),
        ("try { ", "down();", " } catch { }"),
    ];

    for (open, call, close) in nests {
        let (_, source) = deepest(|depth| {
            format!(
                "fn on_join(first) {{ down(); }}\nfn down() {{ {}{call}{} }}",
                open.repeat(depth),
                close.repeat(depth)
            )
        });
        let shown = source.clone();

        let ran = thread::Builder::new()
            .stack_size(RUNTIME_THREAD_STACK)
            .spawn(move || join(&source).0)
            .unwrap()
            .join()
            .unwrap();

        assert!(
            matches!(
                stopped(&ran).kind,
                ScriptErrorKind::Limit(Limit::CallDepth | Limit::Stack)
            ),
            "{shown}: {ran:?}"
        );
    }
}

#[test]
fn a_script_of_the_longest_size_and_deepest_nesting_compiles_on_a_small_stack() {
    // As many functions as a script may have, each as deeply nested as
    // still compiles, filled up to the size limit with comments. A quarter
    // of a runtime thread's stack is plenty for Rhai's parser.
    let (depth, _) = deepest(|depth| {
        format!(
            "fn f(a) {{ {}1{} }}",
            "if a { ".repeat(depth),
            " }".repeat(depth)
        )
    });
    assert!(depth >= 3, "{depth}");
    let mut source = String::new();
    for index in 0..limits::MAX_FUNCTIONS {
        let _ = writeln!(
            source,
            "fn f{index}(a) {{ {}1{} }}",
            "if a { ".repeat(depth),
            " }".repeat(depth)
        );
    }
    while source.len() + 64 <= 16 * 1024 {
        source.push_str("// 0123456789 0123456789 0123456789 0123456789 0123456789 012345\n");
    }
    let script = script_of(draft(&source));

    let compiled = thread::Builder::new()
        .stack_size(RUNTIME_THREAD_STACK / 4)
        .spawn(move || compile(&script).map(|_| ()))
        .unwrap()
        .join()
        .unwrap();

    assert_eq!(compiled, Ok(()));
}
