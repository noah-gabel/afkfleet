//! Validation: what `compile` refuses, with its line and column (P15.6;
//! ADR-0036, D4, D14).

use core::fmt::Write;

use fleet_core::script::Hook;
use fleet_script::{CompileError, CompileErrorKind, SourcePosition, compile};
use rstest::rstest;

use crate::support::{balance_draft, draft, script_of};

/// The error `source` is refused with; panics if it compiles.
fn refused(source: &str) -> CompileError {
    match compile(&script_of(draft(source))) {
        Err(error) => error,
        Ok(_) => panic!("the script compiled:\n{source}"),
    }
}

const fn at(line: u32, column: u32) -> SourcePosition {
    SourcePosition { line, column }
}

#[test]
fn a_script_of_hooks_and_helpers_compiles() {
    let script = script_of(draft(
        "// Stays awake.\nfn on_join(first) {\n    timer.every(45000, \"move\");\n}\n\nfn on_timer(name) {\n    wiggle();\n}\n\nfn wiggle() {\n    look.turn(random.between(-30, 30), 0);\n    state.turns = 1;\n}\n",
    ));

    let program = compile(&script).unwrap();

    assert!(program.defines(Hook::Join));
    assert!(program.defines(Hook::Timer));
    assert!(!program.defines(Hook::Death));
    assert_eq!(program.script(), &script);
}

#[test]
fn an_empty_script_and_one_of_comments_compile_and_define_no_hook() {
    for source in ["", "// nothing yet\n/* at all */\n"] {
        let program = compile(&script_of(draft(source))).unwrap();

        assert!(Hook::ALL.iter().all(|hook| !program.defines(*hook)));
    }
}

#[test]
fn every_hook_compiles_with_its_own_parameters() {
    let source = "fn on_join(first) {}\nfn on_world_change() {}\nfn on_death() {}\nfn on_respawn() {}\nfn on_timer(name) {}\nfn on_chat(key, values) {}\nfn on_stop() {}\n";

    let program = compile(&script_of(draft(source))).unwrap();

    assert!(Hook::ALL.iter().all(|hook| program.defines(*hook)));
}

#[test]
fn a_syntax_error_has_rhais_message_and_its_place() {
    let error = refused("fn on_join(first) {\n  let a = 1;\n  let b = ;\n}");

    assert_eq!(
        error,
        CompileError {
            kind: CompileErrorKind::Syntax {
                message: "Unexpected ';'".to_owned()
            },
            at: Some(at(3, 11)),
        }
    );
    assert_eq!(error.to_string(), "line 3, column 11: Unexpected ';'");
}

#[test]
fn an_undefined_variable_is_refused_in_a_hook_and_in_a_helper() {
    let in_hook = refused("fn on_join(first) {\n  kees.forward.hold();\n}");
    let in_helper =
        refused("fn on_join(first) { walk(); }\nfn walk() {\n    let a = 1;\n    a + b\n}");

    assert_eq!(
        in_hook,
        CompileError {
            kind: CompileErrorKind::UndefinedVariable {
                name: "kees".to_owned()
            },
            at: Some(at(2, 3)),
        }
    );
    assert_eq!(
        in_helper,
        CompileError {
            kind: CompileErrorKind::UndefinedVariable {
                name: "b".to_owned()
            },
            at: Some(at(4, 9)),
        }
    );
}

#[rstest]
#[case("keys")]
#[case("mouse")]
#[case("look")]
#[case("hotbar")]
#[case("vehicle")]
#[case("chat")]
#[case("timer")]
#[case("random")]
#[case("log")]
#[case("bot")]
#[case("world")]
#[case("state")]
fn every_game_object_is_known_in_a_hook_and_in_a_helper(#[case] object: &str) {
    let source = format!(
        "fn on_join(first) {{ let a = {object}; helper(); }}\nfn helper() {{ let b = {object}; }}\n"
    );

    assert!(compile(&script_of(draft(&source))).is_ok(), "{object}");
}

#[test]
fn import_is_refused() {
    let error = refused("fn on_join(first) {\n  import \"x\" as x;\n}");

    assert_eq!(
        error,
        CompileError {
            kind: CompileErrorKind::Import,
            at: Some(at(2, 3)),
        }
    );
}

#[test]
fn eval_is_refused() {
    let error = refused("fn on_join(first) {\n  eval(\"1 + 1\");\n}");

    assert_eq!(
        error,
        CompileError {
            kind: CompileErrorKind::Eval,
            at: Some(at(2, 3)),
        }
    );
}

#[rstest]
#[case("keys")]
#[case("mouse")]
#[case("look")]
#[case("hotbar")]
#[case("vehicle")]
#[case("chat")]
#[case("timer")]
#[case("random")]
#[case("log")]
#[case("bot")]
#[case("world")]
#[case("state")]
fn a_variable_named_like_a_game_object_is_refused(#[case] object: &str) {
    let source = format!("fn on_join(first) {{\n  let {object} = 1;\n}}");

    assert_eq!(
        refused(&source),
        CompileError {
            kind: CompileErrorKind::ReservedName {
                name: object.to_owned()
            },
            at: Some(at(2, 7)),
        }
    );
}

#[test]
fn a_constant_and_a_loop_variable_named_like_a_game_object_are_refused() {
    let constant = refused("fn on_join(first) {\n  const bot = 1;\n}");
    let looped = refused("fn on_join(first) {\n  for keys in [1, 2] { }\n}");

    assert_eq!(
        constant.kind,
        CompileErrorKind::ReservedName {
            name: "bot".to_owned()
        }
    );
    assert_eq!(
        looped.kind,
        CompileErrorKind::ReservedName {
            name: "keys".to_owned()
        }
    );
}

#[rstest]
#[case("keys")]
#[case("mouse")]
#[case("look")]
#[case("hotbar")]
#[case("vehicle")]
#[case("chat")]
#[case("timer")]
#[case("random")]
#[case("log")]
#[case("bot")]
#[case("world")]
#[case("state")]
fn a_parameter_named_like_a_game_object_is_refused_at_its_function(#[case] object: &str) {
    let source =
        format!("fn on_join(first) {{ walk(1, 2); }}\n\n  fn walk(a, {object}) {{\n  a\n}}");

    assert_eq!(
        refused(&source),
        CompileError {
            kind: CompileErrorKind::ReservedName {
                name: object.to_owned()
            },
            at: Some(at(3, 3)),
        }
    );
}

#[test]
fn a_hooks_parameter_named_like_a_game_object_is_refused() {
    let error = refused("fn on_timer(timer) { }");

    assert_eq!(
        error,
        CompileError {
            kind: CompileErrorKind::ReservedName {
                name: "timer".to_owned()
            },
            at: Some(at(1, 1)),
        }
    );
}

#[rstest]
#[case("fn on_join(first) {\n  let f = |x| x + 1;\n}", 2, 11)]
#[case("fn on_join(first) {\n  let f = || 1;\n}", 2, 11)]
#[case("fn on_join(first) {\n  [1, 2].filter(|x| x > 1);\n}", 2, 17)]
#[case("fn on_join(first) {\n  state.f = || state;\n}", 2, 13)]
fn an_anonymous_function_is_refused(#[case] source: &str, #[case] line: u32, #[case] column: u32) {
    assert_eq!(
        refused(source),
        CompileError {
            kind: CompileErrorKind::AnonymousFunction,
            at: Some(at(line, column)),
        }
    );
}

#[test]
fn a_named_function_is_passed_where_an_anonymous_one_would_be() {
    let source = "fn on_join(first) {\n  let big = [1, 2, 3].filter(Fn(\"is_big\"));\n}\nfn is_big(x) { x > 1 }";

    assert!(compile(&script_of(draft(source))).is_ok());
}

#[rstest]
#[case("fn on_join(first) {\n  for keys in [1, 2] { }\n}", "keys", 2, 7)]
#[case(
    "fn on_join(first) {\n  for (item, state) in [1, 2] { }\n}",
    "state",
    2,
    14
)]
#[case(
    "fn on_join(first) {\n  for (mouse, index) in [1, 2] { }\n}",
    "mouse",
    2,
    8
)]
#[case(
    "fn on_join(first) {\n  try { throw 1; } catch (bot) { }\n}",
    "bot",
    2,
    27
)]
fn a_loop_variable_and_a_caught_error_named_like_a_game_object_are_refused(
    #[case] source: &str,
    #[case] name: &str,
    #[case] line: u32,
    #[case] column: u32,
) {
    assert_eq!(
        refused(source),
        CompileError {
            kind: CompileErrorKind::ReservedName {
                name: name.to_owned()
            },
            at: Some(at(line, column)),
        }
    );
}

#[test]
fn a_function_named_on_something_that_is_no_hook_is_refused() {
    let error = refused("fn on_join(first) { }\n\nfn on_reconnect() { }\n");

    assert_eq!(
        error,
        CompileError {
            kind: CompileErrorKind::UnknownHook {
                name: "on_reconnect".to_owned()
            },
            at: Some(at(3, 1)),
        }
    );
}

#[rstest]
#[case("fn on_join() { }", Hook::Join, 0)]
#[case("fn on_join(first, again) { }", Hook::Join, 2)]
#[case("fn on_death(why) { }", Hook::Death, 1)]
#[case("fn on_timer() { }", Hook::Timer, 0)]
#[case("fn on_chat(key) { }", Hook::Chat, 1)]
#[case("fn on_stop(a, b, c) { }", Hook::Stop, 3)]
fn a_hook_with_the_wrong_parameters_is_refused(
    #[case] source: &str,
    #[case] hook: Hook,
    #[case] found: usize,
) {
    assert_eq!(
        refused(source),
        CompileError {
            kind: CompileErrorKind::HookParameters { hook, found },
            at: Some(at(1, 1)),
        }
    );
}

#[test]
fn a_second_hook_of_one_name_with_other_parameters_is_refused() {
    let error = refused("fn on_join(first) { }\nfn on_join() { }\n");

    assert_eq!(
        error,
        CompileError {
            kind: CompileErrorKind::HookParameters {
                hook: Hook::Join,
                found: 0
            },
            at: Some(at(2, 1)),
        }
    );
}

#[rstest]
#[case("let x = 1;\nfn on_join(first) { }", 1, 1)]
#[case("fn on_join(first) { }\non_join(true);", 2, 1)]
#[case("fn on_join(first) { }\n\n   1 + 1;", 3, 4)]
#[case("// a comment\n/* and\n another */ keys.jump.tap();", 3, 13)]
#[case("fn a() { \"}\" }\nloop { }", 2, 1)]
fn anything_outside_a_function_is_refused(
    #[case] source: &str,
    #[case] line: u32,
    #[case] column: u32,
) {
    assert_eq!(
        refused(source),
        CompileError {
            kind: CompileErrorKind::TopLevelStatement,
            at: Some(at(line, column)),
        }
    );
}

#[test]
fn more_functions_than_the_limit_are_refused() {
    let many = |count: usize| {
        (0..count).fold(String::new(), |mut source, index| {
            let _ = writeln!(source, "fn helper_{index}() {{ }}");
            source
        })
    };

    assert!(compile(&script_of(draft(&many(64)))).is_ok());
    assert_eq!(refused(&many(65)).kind, CompileErrorKind::TooManyFunctions);
}

#[test]
fn an_expression_nested_too_deep_is_refused_in_a_hook_and_in_a_helper() {
    let nested = |depth: usize| format!("{}1{}", "(".repeat(depth), ")".repeat(depth));
    let in_hook = format!("fn on_join(first) {{ {} }}", nested(17));
    let in_helper = format!(
        "fn on_join(first) {{ deep() }}\nfn deep() {{ {} }}",
        nested(17)
    );
    let allowed = format!("fn on_join(first) {{ {} }}", nested(6));

    assert_eq!(refused(&in_hook).kind, CompileErrorKind::ExpressionTooDeep);
    assert_eq!(
        refused(&in_helper).kind,
        CompileErrorKind::ExpressionTooDeep
    );
    assert!(refused(&in_helper).at.is_some_and(|at| at.line == 2));
    assert!(compile(&script_of(draft(&allowed))).is_ok());
}

/// The deepest `depth` at which `script(depth)` compiles, up to 40.
fn deepest(script: impl Fn(usize) -> String) -> usize {
    (0..=40)
        .rev()
        .find(|depth| compile(&script_of(draft(&script(*depth)))).is_ok())
        .expect("nothing nested compiles")
}

// What "16 levels inside a function" means in a script's own terms, as
// the reference says it (docs/scripting.md, "Limits"). Rhai counts levels
// its own way; these are the numbers an author meets.
#[test]
fn the_nesting_limit_in_a_scripts_own_terms() {
    let wrapped = |open: &'static str, inside: &'static str, close: &'static str| {
        move |depth: usize| {
            format!(
                "fn on_join(first) {{ {}{inside}{} }}\nfn pass(x) {{ x }}",
                open.repeat(depth),
                close.repeat(depth)
            )
        }
    };

    assert_eq!(deepest(wrapped("if first { ", "1;", " }")), 4);
    assert_eq!(deepest(wrapped("for i in 0..1 { ", "1;", " }")), 4);
    assert_eq!(deepest(wrapped("(", "1", ")")), 6);
    assert_eq!(deepest(wrapped("pass(", "1", ")")), 6);
    // A sum of that many terms, and an `if … else if` chain of that many
    // branches before its `else`.
    let sum = |terms: usize| {
        format!(
            "fn on_join(first) {{ {} }}",
            vec!["1"; terms.max(1)].join(" + ")
        )
    };
    let chain = |branches: usize| {
        format!(
            "fn on_timer(name) {{ {}{{ }} }}",
            "if name == \"a\" { } else ".repeat(branches)
        )
    };
    assert_eq!(deepest(sum), 13);
    assert_eq!(deepest(chain), 11);
    // A `switch` isn't a chain: it takes as many cases as an author likes.
    let cases = (0..40).fold(String::new(), |mut cases, case| {
        let _ = write!(cases, "{case} => {case}, ");
        cases
    });
    let switch = format!("fn on_join(first) {{ switch 5 {{ {cases}_ => 0 }} }}");
    assert!(compile(&script_of(draft(&switch))).is_ok());
}

#[test]
fn a_misspelled_method_compiles_because_rhai_cant_know_it() {
    assert!(
        compile(&script_of(draft(
            "fn on_join(first) {\n  keys.forward.hodl();\n}"
        )))
        .is_ok()
    );
}

#[test]
fn a_script_as_long_as_the_limit_compiles() {
    let line = "// 0123456789 0123456789 0123456789 0123456789 0123456789 01234\n";
    let mut source = String::from("fn on_join(first) { }\n");
    while source.len() + line.len() <= 16 * 1024 {
        source.push_str(line);
    }

    assert!(source.len() > 16 * 1024 - line.len());
    assert!(compile(&script_of(draft(&source))).is_ok());
}

#[test]
fn a_rhai_message_is_one_line_of_at_most_200_characters() {
    // An unknown operator's message repeats the script's own text.
    let long = "x".repeat(400);
    let error = refused(&format!("fn on_join(first) {{ let a = 1 {long}; }}"));

    match error.kind {
        CompileErrorKind::Syntax { message } => {
            assert!(message.chars().count() <= CompileError::MAX_MESSAGE_CHARS);
            assert!(!message.contains('\n'));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_declarations_are_the_programs_own() {
    let script = script_of(balance_draft(
        "fn on_join(first) { chat.send(\"balance\"); }",
    ));

    let program = compile(&script).unwrap();

    assert_eq!(program.script().messages().len(), 3);
    assert_eq!(program.script().patterns().len(), 2);
}
