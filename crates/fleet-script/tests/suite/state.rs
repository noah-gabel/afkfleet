//! The state object: what it holds, how long, and the checks while a hook
//! runs and after it (P15.7; ADR-0036, D5).

use fleet_core::script::{Hook, HookEvent, limits};
use fleet_script::{
    Action, Instance, Limit, Outcome, ScriptError, ScriptErrorKind, SourcePosition,
};
use rstest::rstest;

use crate::support::{World, instance, join, logged, operations, program, stopped, timer};

#[test]
fn state_is_kept_from_one_hook_to_the_next_and_in_helpers() {
    let source = "fn on_join(first) {\n  if first { state.joins = 0; }\n  state.joins += 1;\n  more();\n}\nfn more() { state[\"extra\"] = state.joins * 10; }\nfn on_timer(name) { log.info(`${state.joins} ${state.extra}`); }";
    let mut world = World::new();
    let mut instance = instance(source);

    world.run(&mut instance, &HookEvent::Join { first: true });
    world.run(&mut instance, &HookEvent::Join { first: false });
    let ran = world.run(&mut instance, &HookEvent::Timer(timer("show")));

    assert_eq!(logged(&ran.actions), ["2 20"]);
}

#[test]
fn state_survives_a_death_a_respawn_a_world_change_and_a_disconnect() {
    let source = "fn on_join(first) { if first { state.n = 0; } state.n += 1; }\nfn on_death() { state.n += 10; }\nfn on_respawn() { state.n += 100; }\nfn on_world_change() { state.n += 1000; }\nfn on_timer(name) { log.info(`${state.n}`); }";
    let mut world = World::new();
    let mut instance = instance(source);

    world.run(&mut instance, &HookEvent::Join { first: true });
    world.run(&mut instance, &HookEvent::Death);
    world.run(&mut instance, &HookEvent::Respawn);
    world.run(&mut instance, &HookEvent::WorldChange);
    instance.disconnected();
    world.run(&mut instance, &HookEvent::Join { first: false });
    let ran = world.run(&mut instance, &HookEvent::Timer(timer("show")));

    assert_eq!(logged(&ran.actions), ["1112"]);
}

#[test]
fn a_new_instance_starts_with_an_empty_state() {
    let program = program("fn on_join(first) { log.info(`${state.len()}`); state.seen = true; }");
    let mut world = World::new();
    let mut first = Instance::new(program.clone(), 1);
    let mut second = Instance::new(program, 1);

    let one = world.run(&mut first, &HookEvent::Join { first: true });
    let two = world.run(&mut second, &HookEvent::Join { first: true });

    assert_eq!(logged(&one.actions), ["0"]);
    assert_eq!(logged(&two.actions), ["0"]);
    assert_eq!(first.state_size().0, 1);
}

#[test]
fn reading_a_property_that_was_never_set_stops_the_script_with_its_line() {
    let (read, ..) = join("fn on_join(first) {\n  let joins = state.joins;\n}");
    // Counting up what was never set reads it too. Rhai then names the
    // sum that failed, not the property.
    let (counted, ..) = join("fn on_join(first) {\n  state.joins += 1;\n}");

    assert_eq!(
        stopped(&read),
        &ScriptError {
            kind: ScriptErrorKind::Failed {
                message: "Property not found: joins".to_owned()
            },
            hook: Hook::Join,
            at: Some(SourcePosition {
                line: 2,
                column: 21
            }),
        }
    );
    assert_eq!(
        stopped(&counted),
        &ScriptError {
            kind: ScriptErrorKind::Failed {
                message: "Function not found: + ((), i64)".to_owned()
            },
            hook: Hook::Join,
            at: Some(SourcePosition {
                line: 2,
                column: 15
            }),
        }
    );
}

#[test]
fn state_holds_plain_data_at_any_depth() {
    let (ran, _, instance) = join(
        "fn on_join(first) {\n  state.nothing = ();\n  state.yes = true;\n  state.count = 3;\n  state.ratio = 0.5;\n  state.name = \"farm\";\n  state.letter = 'x';\n  state.list = [1, [2, \"three\"], #{ four: 4 }];\n  state.map = #{ inner: #{ deep: [true] } };\n  state.result = keys.jump.tap();\n  state.entity = world.nearest(\"minecart\");\n}",
    );

    assert!(matches!(ran.outcome, Outcome::Ran { .. }), "{ran:?}");
    // Ten properties, three array items with two and one inside, and the
    // maps' own entries: nested ones count.
    assert!(instance.state_size().0 > 10);
}

#[rstest]
#[case::replaced_by_a_number("state = 5;", ScriptErrorKind::StateNotAMap)]
#[case::replaced_by_a_text("state = \"gone\";", ScriptErrorKind::StateNotAMap)]
#[case::replaced_by_an_array("state = [1, 2];", ScriptErrorKind::StateNotAMap)]
#[case::a_function_pointer("state.f = Fn(\"helper\");", ScriptErrorKind::StateNotPlain)]
#[case::a_function_pointer_with_a_value(
    "state.f = Fn(\"helper\").curry(1);",
    ScriptErrorKind::StateNotPlain
)]
#[case::a_function_pointer_with_the_state(
    "state.f = Fn(\"helper\").curry(state);",
    ScriptErrorKind::StateNotPlain
)]
#[case::a_function_pointer_in_a_list(
    "state.list = [Fn(\"helper\")];",
    ScriptErrorKind::StateNotPlain
)]
#[case::a_function_pointer_deep_inside(
    "state.a = #{ b: [1, #{ c: Fn(\"helper\") }] };",
    ScriptErrorKind::StateNotPlain
)]
#[case::a_game_object("state.k = keys;", ScriptErrorKind::StateNotPlain)]
#[case::an_input("state.k = keys.forward;", ScriptErrorKind::StateNotPlain)]
#[case::the_bot("state.b = [bot];", ScriptErrorKind::StateNotPlain)]
fn state_that_isnt_a_map_of_plain_data_stops_the_script(
    #[case] statement: &str,
    #[case] kind: ScriptErrorKind,
) {
    for source in [
        format!("fn on_join(first) {{ {statement} }}\nfn helper() {{ }}"),
        format!("fn on_join(first) {{ set(); }}\nfn set() {{ {statement} }}\nfn helper() {{ }}"),
    ] {
        let (ran, _, instance) = join(&source);

        assert_eq!(stopped(&ran).kind, kind, "{source}");
        assert_eq!(stopped(&ran).hook, Hook::Join);
        assert_eq!(ran.actions, [Action::ReleaseAll]);
        // A stopped script's state is emptied.
        assert_eq!(instance.state_size(), (0, 0), "{source}");
    }
}

#[test]
fn a_copy_of_the_state_is_plain_data_and_no_cycle() {
    let (ran, _, instance) = join(
        "fn on_join(first) {\n  state.n = 1;\n  let copy = state;\n  copy.n = 2;\n  state.me = state;\n  log.info(`${state.n} ${state.me.n} ${copy.n}`);\n}",
    );

    assert_eq!(logged(&ran.actions), ["1 1 2"]);
    assert_eq!(instance.state_size().0, 3);
}

#[test]
fn the_state_holds_as_many_entries_as_the_limit_and_no_more() {
    let fill = |count: usize| {
        format!("fn on_join(first) {{ for i in 0..{count} {{ state[`k${{i}}`] = i; }} }}")
    };
    let fill_in_a_helper = |count: usize| {
        format!(
            "fn on_join(first) {{ fill(); }}\nfn fill() {{ for i in 0..{count} {{ state[`k${{i}}`] = i; }} }}"
        )
    };

    let (within, _, kept) = join(&fill(limits::STATE_MAX_ENTRIES));
    let (beyond, ..) = join(&fill(limits::STATE_MAX_ENTRIES + 1));
    let (helper_within, ..) = join(&fill_in_a_helper(limits::STATE_MAX_ENTRIES));
    let (helper_beyond, ..) = join(&fill_in_a_helper(limits::STATE_MAX_ENTRIES + 1));

    assert!(
        matches!(helper_within.outcome, Outcome::Ran { .. }),
        "{helper_within:?}"
    );
    assert_eq!(
        stopped(&helper_beyond).kind,
        ScriptErrorKind::Limit(Limit::StateEntries)
    );

    assert!(matches!(within.outcome, Outcome::Ran { .. }), "{within:?}");
    assert_eq!(kept.state_size().0, limits::STATE_MAX_ENTRIES);
    assert_eq!(
        stopped(&beyond),
        &ScriptError {
            kind: ScriptErrorKind::Limit(Limit::StateEntries),
            hook: Hook::Join,
            at: None,
        }
    );
}

#[test]
fn nested_entries_count_towards_the_states_limit() {
    // Eight lists of eight numbers: 8 entries and 64 items.
    let (ran, ..) =
        join("fn on_join(first) { for i in 0..8 { state[`k${i}`] = [1, 2, 3, 4, 5, 6, 7, 8]; } }");

    assert_eq!(
        stopped(&ran).kind,
        ScriptErrorKind::Limit(Limit::StateEntries)
    );
}

#[test]
fn the_state_holds_as_many_bytes_as_the_limit_and_no_more() {
    // Each entry is a key of 2 bytes and a text of 510: 512 bytes.
    let fill = |count: usize| {
        format!(
            "fn on_join(first) {{ let text = \"\"; text.pad(510, 'x'); for i in 0..{count} {{ state[`k${{i}}`] = text; }} }}"
        )
    };

    let fill_in_a_helper = |count: usize| {
        fill(count).replace(
            "fn on_join(first) {",
            "fn on_join(first) { fill(); }\nfn fill() {",
        )
    };

    let (within, _, kept) = join(&fill(8));
    let (beyond, ..) = join(&fill(9));
    let (helper_within, ..) = join(&fill_in_a_helper(8));
    let (helper_beyond, ..) = join(&fill_in_a_helper(9));

    assert!(
        matches!(helper_within.outcome, Outcome::Ran { .. }),
        "{helper_within:?}"
    );
    assert_eq!(
        stopped(&helper_beyond).kind,
        ScriptErrorKind::Limit(Limit::StateBytes)
    );

    assert!(matches!(within.outcome, Outcome::Ran { .. }), "{within:?}");
    assert_eq!(kept.state_size(), (8, limits::STATE_MAX_BYTES));
    assert_eq!(
        stopped(&beyond).kind,
        ScriptErrorKind::Limit(Limit::StateBytes)
    );
}

#[test]
fn the_state_is_checked_while_the_hook_runs_not_only_after_it() {
    // The hook grows the state past its limit, works on, and empties it
    // again before it returns: only a check during the hook can see it.
    let over_and_back = |kind: &str| {
        format!(
            "fn on_join(first) {{\n  {kind}\n  let n = 0;\n  while n < 600 {{ n += 1; }}\n  state.clear();\n  log.info(\"returned\");\n}}"
        )
    };
    let entries = over_and_back("for i in 0..100 { state[`k${i}`] = i; }");
    let bytes = over_and_back(
        "let text = \"\"; text.pad(1000, 'x'); for i in 0..6 { state[`k${i}`] = text; }",
    );
    let pointer = over_and_back("state.f = Fn(\"on_join\").curry(state);");
    let replaced =
        "fn on_join(first) {\n  state = 5;\n  let n = 0;\n  while n < 600 { n += 1; }\n}";

    for (source, kind) in [
        (
            entries.as_str(),
            ScriptErrorKind::Limit(Limit::StateEntries),
        ),
        (bytes.as_str(), ScriptErrorKind::Limit(Limit::StateBytes)),
        (pointer.as_str(), ScriptErrorKind::StateNotPlain),
        (replaced, ScriptErrorKind::StateNotAMap),
    ] {
        let (ran, world, _) = join(source);

        assert_eq!(stopped(&ran).kind, kind, "{source}");
        assert_eq!(world.log.len(), 0, "{source}");
    }
}

#[test]
fn a_state_that_doubles_itself_stops_within_one_check() {
    // An assignment copies its value, so each round doubles the state
    // (ADR-0036, D5). It must stop at the first check, long before the
    // hook's budget.
    let source = "fn on_join(first) {\n  state.a = \"0123456789abcdef\";\n  let i = 0;\n  loop { state[`k${i}`] = state; i += 1; }\n}";

    let (ran, _, instance) = join(source);

    assert!(
        matches!(
            stopped(&ran).kind,
            ScriptErrorKind::Limit(Limit::StateEntries | Limit::StateBytes | Limit::MapEntries)
        ),
        "{ran:?}"
    );
    assert_eq!(instance.state_size(), (0, 0));
}

#[test]
fn a_hook_that_never_touches_the_state_pays_nothing_for_the_check() {
    // The check runs every 256 operations; a hook of fewer never runs it,
    // and a longer one runs it on an empty state.
    let (short, ..) = join("fn on_join(first) { let n = 0; while n < 10 { n += 1; } }");
    let (long, ..) = join("fn on_join(first) { let n = 0; while n < 1000 { n += 1; } }");

    assert!(operations(&short) < limits::STATE_CHECK_EVERY_OPERATIONS);
    assert!(operations(&long) > 4 * limits::STATE_CHECK_EVERY_OPERATIONS);
}

#[test]
fn an_orderly_stop_empties_the_state() {
    let source = "fn on_join(first) { state.n = 1; }\nfn on_stop() { state.n += 1; log.info(`${state.n}`); }";
    let (_, mut world, mut instance) = join(source);

    let ran = world.run(&mut instance, &HookEvent::Stop);

    assert_eq!(logged(&ran.actions), ["2"]);
    assert_eq!(instance.state_size(), (0, 0));
}
