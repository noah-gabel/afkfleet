//! The lowest API level a script runs at, and why it needs it (Plan.md
//! P16.15; ADR-0052, G1): what the server stamps at save and the app's
//! check shows while a script is typed.

use fleet_core::script::menu::SignDraft;
use fleet_core::script::{ScriptApiLevel, ScriptDraft};
use fleet_script::{CompileErrorKind, EXAMPLES, LevelReason, compile_lowest};
use rstest::rstest;

use super::support::{draft, script_of};

/// `source` as a draft at level 2: what the server builds from a request
/// before it knows the level a script needs.
fn newest(source: &str) -> ScriptDraft {
    ScriptDraft {
        api: ScriptApiLevel::V2.get(),
        ..draft(source)
    }
}

/// The level `source` is stamped at, and why.
fn stamped(source: &str) -> (ScriptApiLevel, Vec<LevelReason>) {
    let leveled = compile_lowest(&script_of(newest(source))).unwrap();
    (leveled.program.script().api(), leveled.reasons)
}

#[rstest]
#[case::nothing("")]
#[case::a_farm("fn on_join(first) {\n  mouse.right.hold();\n}")]
#[case::its_own_names_at_level_1(
    "fn on_join(first) {\n  let menu = 1; let sign = 2; let clock = 3; let inventory = 4;\n  log.info(`${menu + sign + clock + inventory}`);\n}"
)]
#[case::a_map_key("fn on_join(first) {\n  state.seen = #{ menu: 1, clock: 2 };\n}")]
#[case::a_member_of_its_own(
    "fn on_join(first) {\n  let x = #{ menu: 1 };\n  log.info(`${x.menu}`);\n}"
)]
#[case::bot_name_in_a_text("fn on_join(first) {\n  log.info(\"bot.name\");\n}")]
#[case::bot_name_in_a_comment("fn on_join(first) {\n  // bot.name\n  log.info(\"x\");\n}")]
#[case::a_function_of_its_own_named_menu(
    "fn menu() { 1 }\nfn on_join(first) {\n  log.info(`${menu()}`);\n}"
)]
fn a_script_without_level_2s_names_is_stamped_at_level_1_with_no_reason(#[case] source: &str) {
    assert_eq!(
        stamped(source),
        (ScriptApiLevel::V1, Vec::new()),
        "{source}"
    );
}

#[rstest]
#[case::the_menu("fn on_join(first) {\n  if menu.open { menu.close(); }\n}", &[LevelReason::Menus])]
#[case::the_inventory("fn on_join(first) {\n  log.info(`${inventory.count(\"minecraft:stone\")}`);\n}", &[LevelReason::Menus])]
#[case::a_screens_hook("fn on_menu_open() {\n}", &[LevelReason::Menus])]
#[case::a_clicks_hook("fn on_click(result) {\n}", &[LevelReason::Menus])]
#[case::the_clock("fn on_join(first) {\n  let now = clock.local(\"Europe/Berlin\");\n}", &[LevelReason::Clock])]
#[case::the_sign("fn on_sign_open() {\n  sign.cancel();\n}", &[LevelReason::Signs])]
#[case::the_bots_name("fn on_join(first) {\n  log.info(bot.name);\n}", &[LevelReason::BotName])]
#[case::the_bots_name_in_a_template("fn on_join(first) {\n  log.info(`I am ${bot.name}`);\n}", &[LevelReason::BotName])]
#[case::the_bots_name_on_a_question_mark("fn on_join(first) {\n  log.info(bot?.name);\n}", &[LevelReason::BotName])]
#[case::several(
    "fn on_join(first) {\n  let now = clock.local(\"Europe/Berlin\");\n  log.info(bot.name);\n}\nfn on_menu_open() {}\nfn on_sign_open() {}",
    &[LevelReason::Menus, LevelReason::Signs, LevelReason::Clock, LevelReason::BotName]
)]
fn a_script_that_uses_level_2_is_stamped_at_level_2_with_each_reason_once(
    #[case] source: &str,
    #[case] reasons: &[LevelReason],
) {
    assert_eq!(
        stamped(source),
        (ScriptApiLevel::V2, reasons.to_vec()),
        "{source}"
    );
}

#[test]
fn declared_signs_need_level_2_whatever_the_text_uses() {
    let mut draft = newest("fn on_join(first) {\n}");
    draft.signs = vec![SignDraft {
        key: "note".to_owned(),
        lines: vec!["afk".to_owned()],
        values: Vec::new(),
    }];

    let leveled = compile_lowest(&script_of(draft)).unwrap();

    assert_eq!(leveled.program.script().api(), ScriptApiLevel::V2);
    assert_eq!(leveled.program.script().signs().len(), 1);
    assert_eq!(leveled.reasons, [LevelReason::Signs]);
}

#[test]
fn every_example_is_stamped_at_its_own_level() {
    for example in EXAMPLES {
        let leveled = compile_lowest(&script_of(ScriptDraft {
            api: ScriptApiLevel::V2.get(),
            ..example.draft()
        }))
        .unwrap();

        assert_eq!(
            leveled.program.script().api(),
            example.level,
            "{}",
            example.id
        );
        assert_eq!(
            leveled.reasons.is_empty(),
            example.level == ScriptApiLevel::V1,
            "{}: {:?}",
            example.id,
            leveled.reasons
        );
    }
}

#[test]
fn a_level_1_script_stays_at_level_1() {
    let leveled = compile_lowest(&script_of(draft("fn on_join(first) {\n}"))).unwrap();

    assert_eq!(leveled.program.script().api(), ScriptApiLevel::V1);
    assert_eq!(leveled.reasons, []);
}

#[test]
fn a_problem_before_level_2s_first_name_is_the_one_found() {
    let error = compile_lowest(&script_of(newest(
        "fn on_join(first) {\n  kees.jump.tap();\n  menu.close();\n}",
    )))
    .unwrap_err();

    assert_eq!(
        error.kind,
        CompileErrorKind::UndefinedVariable {
            name: "kees".to_owned()
        }
    );
    assert_eq!(error.at.map(|at| at.line), Some(2));
}

#[test]
fn a_problem_after_level_2s_first_name_is_found_at_level_2() {
    let error = compile_lowest(&script_of(newest(
        "fn on_join(first) {\n  menu.close();\n  kees.jump.tap();\n}",
    )))
    .unwrap_err();

    assert_eq!(
        error.kind,
        CompileErrorKind::UndefinedVariable {
            name: "kees".to_owned()
        }
    );
    assert_eq!(error.at.map(|at| at.line), Some(3));
}

#[test]
fn a_script_that_needs_level_2_and_names_an_object_as_its_own_is_refused_at_level_2() {
    // `let menu` is a level-1 script's own, but `bot.name` needs level 2,
    // where `menu` is the object's.
    let error = compile_lowest(&script_of(newest(
        "fn on_join(first) {\n  let menu = 1;\n  log.info(bot.name);\n}",
    )))
    .unwrap_err();

    assert_eq!(
        error.kind,
        CompileErrorKind::ReservedName {
            name: "menu".to_owned()
        }
    );
}

#[rstest]
#[case(LevelReason::Menus, "menus")]
#[case(LevelReason::Signs, "signs")]
#[case(LevelReason::Clock, "clock")]
#[case(LevelReason::BotName, "bot_name")]
fn a_reason_has_a_fixed_name(#[case] reason: LevelReason, #[case] name: &str) {
    assert_eq!(reason.as_str(), name);
    assert!(LevelReason::ALL.contains(&reason));
}
