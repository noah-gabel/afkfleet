//! The game objects, in a hook and in a script's own functions (P15.8;
//! ADR-0036, D4, D7, D9 to D12).

use core::time::Duration;

use fleet_core::mode::HotbarSlot;
use fleet_core::script::{
    BlockPosition, EntityFact, Hook, HookEvent, Input, Key, Leash, LeashRadius, LookingAt,
    MouseButton, Refusal, limits,
};
use fleet_script::{Action, Misuse, Outcome, Presence, PressKind, ScriptErrorKind, SourcePosition};
use rstest::rstest;

use crate::support::{World, at, id, instance, join, logged, pickaxe, stopped, timer};

/// `body` as `on_join`'s own, and as a helper's that `on_join` calls.
fn in_hook_and_in_helper(body: &str) -> [String; 2] {
    [
        format!("fn on_join(first) {{\n{body}\n}}"),
        format!("fn on_join(first) {{\n  helper();\n}}\nfn helper() {{\n{body}\n}}"),
    ]
}

/// The actions `body` asks for, which must be the same from a hook and
/// from a helper.
fn asked(body: &str) -> Vec<Action> {
    let [hook, helper] = in_hook_and_in_helper(body);
    let (from_hook, ..) = join(&hook);
    let (from_helper, ..) = join(&helper);
    assert!(
        matches!(from_hook.outcome, Outcome::Ran { .. }),
        "{from_hook:?}"
    );
    assert_eq!(from_hook.actions, from_helper.actions, "{body}");
    // `on_join` lets go of everything first.
    assert_eq!(from_hook.actions.first(), Some(&Action::ReleaseAll));
    from_hook.actions.into_iter().skip(1).collect()
}

/// What `body`, which logs one line, logs.
fn says(body: &str) -> String {
    let actions = asked(body);
    let lines = logged(&actions);
    assert_eq!(lines.len(), 1, "{body}: {actions:?}");
    lines.first().map(ToString::to_string).unwrap_or_default()
}

const fn press(input: Input, kind: PressKind) -> Action {
    Action::Press { input, kind }
}

#[rstest]
#[case("forward", Key::Forward)]
#[case("back", Key::Back)]
#[case("left", Key::Left)]
#[case("right", Key::Right)]
#[case("jump", Key::Jump)]
#[case("sneak", Key::Sneak)]
#[case("sprint", Key::Sprint)]
#[case("drop", Key::Drop)]
#[case("drop_stack", Key::DropStack)]
#[case("swap_offhand", Key::SwapOffhand)]
fn every_key_taps(#[case] name: &str, #[case] key: Key) {
    assert_eq!(
        asked(&format!("  keys.{name}.tap();")),
        [press(Input::Key(key), PressKind::Tap)]
    );
}

#[rstest]
#[case("forward", Key::Forward)]
#[case("back", Key::Back)]
#[case("left", Key::Left)]
#[case("right", Key::Right)]
#[case("jump", Key::Jump)]
#[case("sneak", Key::Sneak)]
#[case("sprint", Key::Sprint)]
fn the_seven_keys_that_can_be_held_hold_and_release(#[case] name: &str, #[case] key: Key) {
    let input = Input::Key(key);

    assert_eq!(
        asked(&format!(
            "  keys.{name}.hold();\n  log.info(`${{keys.{name}.held}}`);\n  keys.{name}.release();\n  log.info(`${{keys.{name}.held}}`);"
        )),
        [
            press(input, PressKind::Hold),
            Action::Log("true".to_owned()),
            Action::Release(input),
            Action::Log("false".to_owned()),
        ]
    );
}

#[rstest]
#[case("drop")]
#[case("drop_stack")]
#[case("swap_offhand")]
fn a_key_that_only_taps_cant_be_held(#[case] name: &str) {
    for method in ["hold()", "hold_for(500)"] {
        let (ran, ..) = join(&format!(
            "fn on_join(first) {{\n  keys.{name}.{method};\n}}"
        ));

        assert_eq!(stopped(&ran).kind, ScriptErrorKind::Misuse(Misuse::TapOnly));
        assert_eq!(
            stopped(&ran).at,
            Some(SourcePosition {
                line: 2,
                column: 9 + u32::try_from(name.len()).unwrap()
            })
        );
    }
}

#[test]
fn both_mouse_buttons_tap_hold_and_release() {
    let left = Input::Mouse(MouseButton::Left);
    let right = Input::Mouse(MouseButton::Right);

    assert_eq!(
        asked("  mouse.left.tap();\n  mouse.right.tap();"),
        [press(left, PressKind::Tap), press(right, PressKind::Tap)]
    );
    assert_eq!(
        asked(
            "  mouse.left.hold();\n  mouse.right.hold();\n  log.info(`${mouse.left.held} ${mouse.right.held}`);\n  mouse.left.release();\n  mouse.right.release();"
        ),
        [
            press(left, PressKind::Hold),
            press(right, PressKind::Hold),
            Action::Log("true true".to_owned()),
            Action::Release(left),
            Action::Release(right),
        ]
    );
}

#[test]
fn the_right_button_holds_until_the_item_is_used_and_no_other_input_does() {
    let right = Input::Mouse(MouseButton::Right);

    assert_eq!(
        asked("  mouse.right.hold_until_used();\n  log.info(`${mouse.right.held}`);"),
        [
            press(right, PressKind::HoldUntilUsed),
            Action::Log("true".to_owned())
        ]
    );
    for other in ["mouse.left", "keys.jump"] {
        let (ran, ..) = join(&format!(
            "fn on_join(first) {{ {other}.hold_until_used(); }}"
        ));

        assert_eq!(
            stopped(&ran).kind,
            ScriptErrorKind::Misuse(Misuse::RightButtonOnly)
        );
    }
}

#[test]
fn every_action_returns_a_result_a_script_can_ignore_or_read() {
    assert_eq!(
        says("  let result = keys.jump.tap();\n  log.info(`${result.ok} [${result.reason}]`);"),
        "true []"
    );
    assert_eq!(
        says(
            "  keys.jump.tap();\n  let again = keys.jump.tap();\n  log.info(`${again.ok} [${again.reason}]`);"
        ),
        "false [too_soon]"
    );
    assert_eq!(
        says("  if !keys.jump.hold_for(1).ok { log.info(\"refused\"); }"),
        "refused"
    );
}

#[test]
fn holding_an_input_that_is_down_presses_nothing_again() {
    let forward = Input::Key(Key::Forward);

    assert_eq!(
        asked(
            "  keys.forward.hold();\n  let again = keys.forward.hold();\n  let timed = keys.forward.hold_for(500);\n  log.info(`${again.ok} ${timed.ok}`);"
        ),
        [
            press(forward, PressKind::Hold),
            Action::Log("true true".to_owned())
        ]
    );
}

#[test]
fn a_tap_of_an_input_that_is_down_lets_go_of_it() {
    let jump = Input::Key(Key::Jump);
    let (first, mut world, mut instance) = join(
        "fn on_join(first) { keys.jump.hold(); }\nfn on_timer(name) { keys.jump.tap(); log.info(`${keys.jump.held}`); }",
    );
    assert_eq!(first.actions.last(), Some(&press(jump, PressKind::Hold)));

    world.now += Duration::from_secs(1);
    let ran = world.run(&mut instance, &HookEvent::Timer(timer("t")));

    assert_eq!(
        ran.actions,
        [press(jump, PressKind::Tap), Action::Log("false".to_owned())]
    );
    assert!(!instance.is_held(jump));
}

#[test]
fn hold_for_lets_go_by_itself_when_its_time_is_up() {
    let forward = Input::Key(Key::Forward);
    let sprint = Input::Key(Key::Sprint);
    let (ran, mut world, mut instance) = join(
        "fn on_join(first) {\n  keys.sprint.hold_for(3000);\n  keys.forward.hold_for(1000);\n}",
    );
    assert_eq!(
        ran.actions,
        [
            Action::ReleaseAll,
            press(sprint, PressKind::Hold),
            press(forward, PressKind::Hold)
        ]
    );
    assert_eq!(ran.next_wake, Some(world.now + Duration::from_millis(1000)));

    let early = world.pass(&mut instance, Duration::from_millis(999));
    assert_eq!(early.len(), 0);
    assert!(instance.is_held(forward));
    let first = world.pass(&mut instance, Duration::from_millis(1));
    let second = world.pass(&mut instance, Duration::from_secs(5));

    assert_eq!(first.len(), 1);
    assert_eq!(
        first.first().map(|ran| ran.actions.clone()),
        Some(vec![Action::Release(forward)])
    );
    assert_eq!(
        first.first().and_then(|ran| ran.next_wake),
        Some(world.now - Duration::from_secs(5) + Duration::from_millis(2000))
    );
    assert_eq!(
        second.first().map(|ran| ran.actions.clone()),
        Some(vec![Action::Release(sprint)])
    );
    assert!(!instance.is_held(forward) && !instance.is_held(sprint));
    assert_eq!(instance.next_wake(), None);
}

#[rstest]
#[case(49, false)]
#[case(50, true)]
#[case(600_000, true)]
#[case(600_001, false)]
#[case(0, false)]
#[case(-5, false)]
fn hold_for_takes_50_ms_to_10_minutes(#[case] ms: i64, #[case] ok: bool) {
    let expected = if ok { "true []" } else { "false [bad_value]" };

    assert_eq!(
        says(&format!(
            "  let r = keys.forward.hold_for({ms});\n  log.info(`${{r.ok}} [${{r.reason}}]`);"
        )),
        expected
    );
}

#[test]
fn hold_for_takes_no_time_that_isnt_a_number() {
    assert_eq!(
        says("  let r = mouse.right.hold_for(\"long\");\n  log.info(`${r.ok} [${r.reason}]`);"),
        "false [bad_value]"
    );
    assert_eq!(
        says("  let r = mouse.right.hold_for(1500.0);\n  log.info(`${r.ok} [${r.reason}]`);"),
        "true []"
    );
}

#[test]
fn keys_release_all_lets_go_of_the_keys_and_not_of_the_buttons() {
    let actions = asked(
        "  keys.forward.hold();\n  keys.sneak.hold();\n  mouse.left.hold();\n  let r = keys.release_all();\n  log.info(`${r.ok} ${keys.forward.held} ${keys.sneak.held} ${mouse.left.held}`);",
    );

    assert_eq!(
        actions.get(3..),
        Some(
            &[
                Action::ReleaseKeys,
                Action::Log("true false false true".to_owned())
            ][..]
        )
    );
}

#[test]
fn a_release_is_never_refused() {
    // Dead, and released twice within a millisecond: still taken.
    let source = "fn on_join(first) { keys.forward.hold(); }\nfn on_death() {\n  let a = keys.forward.release();\n  let b = keys.forward.release();\n  let c = keys.release_all();\n  let d = mouse.left.release();\n  log.info(`${a.ok} ${b.ok} ${c.ok} ${d.ok}`);\n}";
    let (_, mut world, mut instance) = join(source);

    let ran = world.run(&mut instance, &HookEvent::Death);

    assert_eq!(logged(&ran.actions), ["true true true true"]);
}

#[test]
fn everything_is_let_go_before_the_four_hooks_that_put_the_bot_somewhere_new() {
    let source = "fn on_join(first) { keys.forward.hold(); mouse.left.hold(); }\nfn on_world_change() { log.info(`${keys.forward.held}`); }\nfn on_death() { log.info(`${keys.forward.held}`); }\nfn on_respawn() { log.info(`${keys.forward.held}`); }\nfn on_timer(name) { log.info(`${keys.forward.held}`); keys.forward.hold(); }";
    let (_, mut world, mut instance) = join(source);

    world.now += Duration::from_secs(1);
    let timer_hook = world.run(&mut instance, &HookEvent::Timer(timer("t")));
    assert_eq!(timer_hook.actions, [Action::Log("true".to_owned())]);
    for event in [
        HookEvent::WorldChange,
        HookEvent::Death,
        HookEvent::Respawn,
        HookEvent::Join { first: false },
    ] {
        world.now += Duration::from_secs(1);
        world.run(&mut instance, &HookEvent::Timer(timer("t")));
        // A dead bot holds nothing; before every other one of the four,
        // the timer's hook has pressed the key again.
        assert_eq!(
            instance.is_held(Input::Key(Key::Forward)),
            event != HookEvent::Respawn,
            "{event:?}"
        );
        let ran = world.run(&mut instance, &event);

        assert_eq!(ran.actions.first(), Some(&Action::ReleaseAll), "{event:?}");
        assert!(!instance.is_held(Input::Key(Key::Forward)));
        if event != (HookEvent::Join { first: false }) {
            assert_eq!(logged(&ran.actions), ["false"], "{event:?}");
        }
    }
}

#[test]
fn a_hook_the_script_doesnt_define_still_lets_go_of_everything() {
    let (_, mut world, mut instance) = join("fn on_join(first) { keys.forward.hold(); }");

    let ran = world.run(&mut instance, &HookEvent::Death);
    let timer_hook = world.run(&mut instance, &HookEvent::Timer(timer("t")));

    assert_eq!(ran.outcome, Outcome::NotDefined);
    assert_eq!(ran.actions, [Action::ReleaseAll]);
    assert!(!instance.is_held(Input::Key(Key::Forward)));
    assert_eq!(timer_hook.outcome, Outcome::NotDefined);
    assert_eq!(timer_hook.actions.len(), 0);
}

#[test]
fn the_runtime_can_tell_the_script_an_input_was_let_go_from_below() {
    let left = Input::Mouse(MouseButton::Left);
    let (_, mut world, mut instance) = join(
        "fn on_join(first) { mouse.left.hold(); }\nfn on_timer(name) { log.info(`${mouse.left.held}`); }",
    );
    assert!(instance.is_held(left));

    instance.let_go([left]);
    let ran = world.run(&mut instance, &HookEvent::Timer(timer("t")));

    assert_eq!(logged(&ran.actions), ["false"]);
}

#[rstest]
#[case("keys.jump.tap()")]
#[case("keys.forward.hold()")]
#[case("keys.forward.hold_for(500)")]
#[case("mouse.left.tap()")]
#[case("mouse.right.hold()")]
#[case("mouse.right.hold_until_used()")]
#[case("look.at(90.0, 10.0)")]
#[case("look.turn(5, 5)")]
#[case("look.at_position(1, 2, 3)")]
#[case("hotbar.select(2)")]
#[case("vehicle.mount(\"minecart\")")]
#[case("vehicle.dismount()")]
#[case("chat.send(\"hello\")")]
fn a_dead_bot_and_one_between_worlds_act_on_nothing(#[case] call: &str) {
    for (presence, reason) in [
        (Presence::Dead, Refusal::Dead),
        (Presence::NotInWorld, Refusal::NotInWorld),
    ] {
        let source = format!(
            "fn on_timer(name) {{\n  let r = {call};\n  log.info(`${{r.ok}} [${{r.reason}}]`);\n}}"
        );
        let mut world = World::new();
        world.presence = presence;
        // The balance mode declares the message `hello`.
        let program = crate::support::program_of(crate::support::balance_draft(&source));
        let mut instance = fleet_script::Instance::new(program, 7);

        let ran = world.run(&mut instance, &HookEvent::Timer(timer("t")));

        assert_eq!(
            ran.actions,
            [Action::Log(format!("false [{reason}]"))],
            "{call}"
        );
    }
}

#[rstest]
#[case("keys.jump.tap()", "keys.jump.tap()", 250)]
#[case("keys.jump.tap()", "keys.jump.hold()", 250)]
#[case("keys.forward.hold_for(50)", "keys.forward.tap()", 250)]
#[case("mouse.left.tap()", "mouse.left.tap()", 500)]
#[case("mouse.left.tap()", "mouse.left.hold()", 500)]
#[case("mouse.right.tap()", "mouse.right.tap()", 200)]
#[case("mouse.right.hold()", "mouse.right.hold_until_used()", 500)]
#[case("look.at(1, 1)", "look.at_position(1, 2, 3)", 250)]
#[case("look.turn(1, 1)", "look.turn(1, 1)", 250)]
#[case("hotbar.select(1)", "hotbar.select(2)", 250)]
#[case("vehicle.mount(\"minecart\")", "vehicle.mount(\"minecart\")", 250)]
#[case("vehicle.dismount()", "vehicle.dismount()", 250)]
fn no_two_actions_of_a_kind_come_closer_than_the_kinds_minimum(
    #[case] first: &str,
    #[case] second: &str,
    #[case] minimum: u64,
) {
    // The first hook acts and lets go; the second tries the same kind
    // again after `name` milliseconds.
    let source = format!(
        "fn on_join(first) {{\n  {first};\n  keys.release_all();\n  mouse.left.release();\n  mouse.right.release();\n}}\nfn on_timer(name) {{\n  let r = {second};\n  log.info(`${{r.ok}} [${{r.reason}}]`);\n}}"
    );
    for (after, expected) in [
        (0, "false [too_soon]"),
        (minimum - 1, "false [too_soon]"),
        (minimum, "true []"),
    ] {
        let (_, mut world, mut instance) = join(&source);
        world.now += Duration::from_millis(after);

        let ran = world.run(&mut instance, &HookEvent::Timer(timer("t")));

        assert_eq!(
            logged(&ran.actions),
            [expected],
            "{second} after {after} ms"
        );
    }
}

#[test]
fn kinds_dont_limit_each_other() {
    let actions = asked(
        "  keys.jump.tap();\n  keys.sneak.tap();\n  mouse.left.tap();\n  mouse.right.tap();\n  look.at(1, 1);\n  look.turn(1, 1);\n  hotbar.select(3);\n  vehicle.mount(\"minecart\");\n  vehicle.dismount();",
    );

    assert_eq!(actions.len(), 9);
}

#[test]
fn the_limiter_outlives_the_script_so_a_new_instance_gains_nothing() {
    let source = "fn on_join(first) { let r = mouse.left.tap(); log.info(`${r.ok}`); }";
    let mut world = World::new();
    let mut first = instance(source);
    let mut second = instance(source);

    let one = world.run(&mut first, &HookEvent::Join { first: true });
    world.now += Duration::from_millis(100);
    let two = world.run(&mut second, &HookEvent::Join { first: true });

    assert_eq!(logged(&one.actions), ["true"]);
    assert_eq!(logged(&two.actions), ["false"]);
}

#[test]
fn look_takes_whole_numbers_and_decimals_and_refuses_what_isnt_a_number() {
    assert_eq!(
        asked("  look.at(90, 10.5);"),
        [Action::LookAt {
            yaw: 90.0,
            pitch: 10.5
        }]
    );
    assert_eq!(
        asked("  look.turn(-30, 10);"),
        [Action::LookTurn {
            yaw: -30.0,
            pitch: 10.0
        }]
    );
    assert_eq!(
        asked("  look.at_position(1, 64.5, -3);"),
        [Action::LookAtPosition(at(1.0, 64.5, -3.0))]
    );
    for call in [
        "look.at(\"north\", 0)",
        "look.at(0.0 / 0.0, 0)",
        "look.at(181, 0)",
        "look.at(0, 91)",
        "look.at(-181, 0)",
        "look.at(0, -91)",
        "look.turn(1.0 / 0.0, 0)",
        "look.turn(0, ())",
        "look.at_position(0.0 / 0.0, 1, 2)",
        "look.at_position(1, 2, true)",
        "look.at_entity(())",
        "look.at_entity(5)",
        "look.at_entity(#{ x: 1 })",
    ] {
        assert_eq!(
            says(&format!(
                "  let r = {call};\n  log.info(`${{r.ok}} [${{r.reason}}]`);"
            )),
            "false [bad_value]",
            "{call}"
        );
    }
}

#[test]
fn look_reads_where_the_bot_looks_and_looks_at_an_entity_world_returned() {
    let source = "fn on_join(first) {\n  log.info(`${look.yaw} ${look.pitch}`);\n  let cart = world.nearest(\"minecart\");\n  look.at_entity(cart);\n}";
    let mut world = World::new();
    world.bot.yaw = 45.0;
    world.bot.pitch = -10.5;
    world.minecart(at(3.5, 64.0, 0.5));
    let mut instance = instance(source);

    let ran = world.run(&mut instance, &HookEvent::Join { first: true });

    assert_eq!(
        ran.actions,
        [
            Action::ReleaseAll,
            Action::Log("45.0 -10.5".to_owned()),
            Action::LookAtPosition(at(3.5, 64.0, 0.5)),
        ]
    );
}

#[rstest]
#[case(1, Some(0))]
#[case(9, Some(8))]
#[case(0, None)]
#[case(10, None)]
#[case(-1, None)]
fn the_hotbar_selects_slots_1_to_9(#[case] number: i64, #[case] slot: Option<u8>) {
    let actions = asked(&format!(
        "  let r = hotbar.select({number});\n  log.info(`${{r.ok}} [${{r.reason}}]`);"
    ));

    match slot {
        Some(slot) => assert_eq!(
            actions,
            [
                Action::SelectHotbar(HotbarSlot::try_from(slot).unwrap()),
                Action::Log("true []".to_owned())
            ]
        ),
        None => assert_eq!(actions, [Action::Log("false [bad_value]".to_owned())]),
    }
}

#[test]
fn the_hotbar_says_which_slot_is_selected_as_the_number_on_its_key() {
    let mut world = World::new();
    world.bot.selected = HotbarSlot::try_from(4).unwrap();
    let mut instance =
        instance("fn on_join(first) { log.info(`${hotbar.selected}`); hotbar.select(\"two\"); }");

    let ran = world.run(&mut instance, &HookEvent::Join { first: true });

    assert_eq!(logged(&ran.actions), ["5"]);
    assert_eq!(ran.actions.len(), 2);
}

#[test]
fn a_vehicle_is_mounted_by_its_kind_and_left() {
    assert_eq!(
        asked("  vehicle.mount(\"minecart\");\n  vehicle.dismount();"),
        [Action::Mount(id("minecraft:minecart")), Action::Dismount]
    );
    assert_eq!(
        asked("  vehicle.mount(\"minecraft:minecart\");"),
        [Action::Mount(id("minecart"))]
    );
    for kind in ["\"boat\"", "\"Not An Id\"", "\"\"", "5"] {
        assert_eq!(
            says(&format!(
                "  let r = vehicle.mount({kind});\n  log.info(`${{r.ok}} [${{r.reason}}]`);"
            )),
            "false [bad_value]",
            "{kind}"
        );
    }
}

#[test]
fn random_gives_whole_numbers_in_its_range_and_the_same_for_the_same_seed() {
    let source = "fn on_join(first) {\n  let all = [];\n  for i in 0..50 { all.push(random.between(-3, 3)); }\n  log.info(`${all}`);\n  log.info(`${random.between(5, 5)} ${random.between(9, 7) >= 7} ${random.chance(0)} ${random.chance(100)} ${random.chance(-5)} ${random.chance(250.5)}`);\n}";
    let run = |seed: u64| {
        let mut world = World::new();
        let mut instance = fleet_script::Instance::new(crate::support::program(source), seed);
        world.run(&mut instance, &HookEvent::Join { first: true });
        world.log
    };

    let one = run(1);
    let again = run(1);
    let other = run(2);

    assert_eq!(one, again);
    assert_ne!(one.first(), other.first());
    assert_eq!(
        one.get(1).map(String::as_str),
        Some("5 true false true false true")
    );
    let numbers: Vec<i64> = one
        .first()
        .unwrap()
        .trim_matches(['[', ']'])
        .split(", ")
        .map(|number| number.parse().unwrap())
        .collect();
    assert_eq!(numbers.len(), 50);
    assert!(numbers.iter().all(|number| (-3..=3).contains(number)));
    assert!(numbers.contains(&-3) && numbers.contains(&3));
}

#[test]
fn random_chance_is_true_about_that_often() {
    let source = "fn on_join(first) {\n  let hits = 0;\n  for i in 0..400 { if random.chance(30) { hits += 1; } }\n  log.info(`${hits}`);\n}";

    let (_, world, _) = join(source);

    let hits: u32 = world.log.first().unwrap().parse().unwrap();
    assert!((80..=160).contains(&hits), "{hits}");
}

#[test]
fn the_log_takes_a_line_of_200_characters_and_cuts_a_longer_one() {
    let source = "fn on_join(first) {\n  let long = \"\";\n  long.pad(300, 'x');\n  let r = log.info(long);\n  log.info(`${r.ok}`);\n  log.info(\"line one\\nline two \\u202e tail\");\n  log.info(42);\n}";

    let (_, world, _) = join(source);

    assert_eq!(
        world.log.first().map(|line| line.chars().count()),
        Some(limits::LOG_LINE_CHARS)
    );
    assert_eq!(world.log.get(1).map(String::as_str), Some("true"));
    assert_eq!(
        world.log.get(2).map(String::as_str),
        Some("line one | line two  tail")
    );
    assert_eq!(world.log.get(3).map(String::as_str), Some("42"));
}

#[test]
fn the_log_takes_20_lines_a_minute() {
    let source = "fn on_join(first) {\n  let taken = 0;\n  for i in 0..30 { if log.info(`line ${i}`).ok { taken += 1; } }\n  state.taken = taken;\n  state.last = log.info(\"one more\").reason;\n}\nfn on_timer(name) { let r = log.info(`${state.taken} ${state.last}`); state.taken = r.ok; }";
    let (ran, mut world, mut instance) = join(source);
    assert_eq!(logged(&ran.actions).len(), 20);

    world.now += Duration::from_secs(59);
    let early = world.run(&mut instance, &HookEvent::Timer(timer("t")));
    world.now += Duration::from_secs(2);
    let later = world.run(&mut instance, &HookEvent::Timer(timer("t")));

    assert_eq!(logged(&early.actions).len(), 0);
    assert_eq!(logged(&later.actions), ["false too_soon"]);
}

#[test]
fn the_logs_limits_hold_in_a_helper_function_too() {
    let source = "fn on_join(first) { write(); }\nfn write() {\n  let long = \"\";\n  long.pad(300, 'x');\n  log.info(long);\n  let taken = 1;\n  for i in 0..30 { if log.info(`line ${i}`).ok { taken += 1; } }\n  state.taken = taken;\n}\nfn on_stop() { log.info(`${state.taken}`); }";
    let (ran, mut world, mut instance) = join(source);

    assert_eq!(logged(&ran.actions).len(), 20);
    assert_eq!(
        logged(&ran.actions)
            .first()
            .map(|line| line.chars().count()),
        Some(limits::LOG_LINE_CHARS)
    );
    // An hour later the minute's lines are long past: the count is said.
    world.now += Duration::from_secs(3600);
    let stop = world.run(&mut instance, &HookEvent::Stop);
    assert_eq!(logged(&stop.actions), ["20"]);
}

#[test]
fn print_and_debug_go_nowhere() {
    let (ran, world, _) = join(
        "fn on_join(first) {\n  print(\"to stdout?\");\n  debug(state);\n  print(42);\n  log.info(\"only this\");\n}",
    );

    assert_eq!(
        ran.actions,
        [Action::ReleaseAll, Action::Log("only this".to_owned())]
    );
    assert_eq!(world.log, ["only this"]);
}

#[test]
fn the_bot_object_reads_the_snapshot() {
    let source = "fn on_join(first) {\n  log.info(`${bot.health} ${bot.food} ${bot.x} ${bot.y} ${bot.z} ${bot.dimension} ${bot.riding}`);\n  log.info(`${bot.held == ()} ${bot.looking_at == ()}`);\n}\nfn on_timer(name) { show(); }\nfn show() {\n  log.info(`${bot.held.id} ${bot.held.count} ${bot.held.durability} ${bot.riding}`);\n  let at = bot.looking_at;\n  log.info(`${at.kind} ${at.id} ${at.x} ${at.y} ${at.z}`);\n}";
    let mut world = World::new();
    world.bot.health = 17.5;
    world.bot.food = 18;
    world.bot.position = at(1.5, 70.0, -2.25);
    let mut instance = instance(source);

    world.run(&mut instance, &HookEvent::Join { first: true });
    world.bot.held = Some(pickaxe(123));
    world.bot.riding = Some(id("minecart"));
    world.bot.looking_at = Some(LookingAt::Block {
        id: id("cobblestone"),
        at: BlockPosition { x: 1, y: 70, z: -4 },
    });
    world.run(&mut instance, &HookEvent::Timer(timer("t")));
    world.bot.held = Some(fleet_core::script::HeldItem {
        id: id("bread"),
        count: 12,
        durability: None,
    });
    world.bot.looking_at = Some(LookingAt::Entity {
        kind: id("zombie"),
        at: at(1.5, 70.0, 0.5),
    });
    world.now += Duration::from_secs(1);
    world.run(&mut instance, &HookEvent::Timer(timer("t")));

    assert_eq!(
        world.log,
        [
            "17.5 18 1.5 70.0 -2.25 minecraft:overworld false",
            "true true",
            "minecraft:iron_pickaxe 1 123 true",
            "block minecraft:cobblestone 1 70 -4",
            // Nothing reads as an empty text.
            "minecraft:bread 12  true",
            "entity minecraft:zombie 1.5 70.0 0.5",
        ]
    );
}

#[test]
fn the_world_object_finds_the_nearest_entity_and_reads_blocks() {
    let source = "fn on_join(first) { find(); }\nfn find() {\n  let cart = world.nearest(\"minecart\");\n  log.info(`${cart.kind} ${cart.x} ${cart.y} ${cart.z} ${cart.distance} ${cart.occupied}`);\n  let free = world.nearest_free(\"minecraft:minecart\");\n  log.info(`${free.x} ${free.occupied}`);\n  log.info(`${world.nearest(\"boat\") == ()} ${world.nearest(\"Not An Id\") == ()} ${world.nearest_free(5) == ()}`);\n  log.info(`${world.block(0, 63, 0)} ${world.block(0.9, 63.9, 0.2)} ${world.block(0, 64, 0) == ()} ${world.block(40, 64, 0) == ()} ${world.block(\"a\", 1, 2) == ()}`);\n}";
    let mut world = World::new();
    world.entities.push(EntityFact {
        kind: id("minecart"),
        position: at(2.5, 64.0, 0.5),
        occupied: true,
    });
    world.minecart(at(4.5, 64.0, 0.5));
    world
        .blocks
        .push((BlockPosition { x: 0, y: 63, z: 0 }, id("stone")));
    let mut instance = instance(source);

    world.run(&mut instance, &HookEvent::Join { first: true });

    assert_eq!(
        world.log,
        [
            "minecraft:minecart 2.5 64.0 0.5 2.0 true",
            "4.5 false",
            "true true true",
            "minecraft:stone minecraft:stone true true true",
        ]
    );
}

#[test]
fn a_hook_sees_the_world_as_it_was_when_it_started() {
    let source = "fn on_join(first) {\n  hotbar.select(5);\n  look.at(90, 0);\n  log.info(`${hotbar.selected} ${look.yaw}`);\n}\nfn on_timer(name) { log.info(`${hotbar.selected} ${look.yaw}`); }";
    let (_, mut world, mut instance) = join(source);

    world.run(&mut instance, &HookEvent::Timer(timer("t")));

    assert_eq!(world.log, ["1 0.0", "5 90.0"]);
}

#[test]
fn beyond_the_leash_a_key_that_leads_further_is_refused_and_one_that_leads_back_is_taken() {
    // The bot stands 10 blocks east of where it was put, on a leash of 8,
    // and looks east: forward leads further, back leads home.
    let source = "fn on_join(first) {\n  let f = keys.forward.hold();\n  let b = keys.back.hold();\n  let j = keys.jump.tap();\n  log.info(`${f.ok} [${f.reason}] ${b.ok} ${j.ok} ${keys.forward.held} ${keys.back.held}`);\n}";
    let mut world = World::new();
    world.leash = Some(Leash::new(LeashRadius::DEFAULT, at(0.5, 64.0, 0.5)));
    world.bot.position = at(10.5, 64.0, 0.5);
    world.bot.yaw = -90.0;
    let mut instance = instance(source);

    let ran = world.run(&mut instance, &HookEvent::Join { first: true });

    assert_eq!(
        logged(&ran.actions),
        ["false [leashed] true true false true"]
    );
    assert_eq!(
        world.presses(),
        [
            (Input::Key(Key::Back), PressKind::Hold),
            (Input::Key(Key::Jump), PressKind::Tap)
        ]
    );
}

#[test]
fn within_the_leash_every_direction_is_taken() {
    let source = "fn on_join(first) { let f = keys.forward.hold(); log.info(`${f.ok}`); }";
    let mut world = World::new();
    world.leash = Some(Leash::new(LeashRadius::DEFAULT, at(0.5, 64.0, 0.5)));
    world.bot.position = at(7.5, 64.0, 0.5);
    world.bot.yaw = -90.0;
    let mut instance = instance(source);

    let ran = world.run(&mut instance, &HookEvent::Join { first: true });

    assert_eq!(logged(&ran.actions), ["true"]);
}

#[test]
fn a_hold_of_the_left_button_on_a_block_below_the_feet_is_refused() {
    let source = "fn on_join(first) {\n  let h = mouse.left.hold();\n  let t = mouse.left.tap();\n  log.info(`${h.ok} [${h.reason}] ${t.ok} ${mouse.left.held}`);\n}";
    let looking_at = |y: i32| {
        let mut world = World::new();
        world.bot.looking_at = Some(LookingAt::Block {
            id: id("stone"),
            at: BlockPosition { x: 0, y, z: 1 },
        });
        let mut instance = instance(source);
        world.run(&mut instance, &HookEvent::Join { first: true });
        world.log
    };

    // The bot's feet are at 64: the block under them is at 63.
    assert_eq!(looking_at(63), ["false [mining_guard] true false"]);
    assert_eq!(looking_at(64), ["true [] false true"]);
    assert_eq!(looking_at(65), ["true [] false true"]);
}

#[test]
fn a_hold_of_the_left_button_with_a_nearly_used_tool_is_refused() {
    let source = "fn on_join(first) {\n  let h = mouse.left.hold();\n  log.info(`${h.ok} [${h.reason}] ${bot.held.durability}`);\n}";
    let holding = |durability: u32, guard: u8| {
        let mut world = World::new();
        world.bot.held = Some(pickaxe(durability));
        let mut draft = crate::support::draft(source);
        draft.tool_guard = guard;
        let mut instance = fleet_script::Instance::new(crate::support::program_of(draft), 7);
        world.run(&mut instance, &HookEvent::Join { first: true });
        world.log
    };

    assert_eq!(holding(2, 2), ["false [tool_guard] 2"]);
    assert_eq!(holding(3, 2), ["true [] 3"]);
    assert_eq!(holding(1, 0), ["true [] 1"]);
    assert_eq!(holding(10, 10), ["false [tool_guard] 10"]);
}

#[test]
fn a_method_that_doesnt_exist_stops_the_script_where_it_runs_with_its_line() {
    let (ran, ..) = join("fn on_join(first) {\n  keys.forward.hodl();\n}");

    let error = stopped(&ran);
    assert_eq!(error.hook, Hook::Join);
    assert_eq!(
        error.at,
        Some(SourcePosition {
            line: 2,
            column: 16
        })
    );
    assert_eq!(
        error.kind,
        ScriptErrorKind::Failed {
            message: "Function not found: hodl (input)".to_owned()
        }
    );
    assert_eq!(ran.actions, [Action::ReleaseAll]);
}

#[test]
fn an_error_in_a_helper_names_the_helpers_line() {
    let (ran, ..) = join(
        "fn on_join(first) {\n  helper();\n}\n\nfn helper() {\n  let a = 1;\n  a.nothing_like_this();\n}",
    );

    assert_eq!(
        stopped(&ran).at,
        Some(SourcePosition { line: 7, column: 5 })
    );
}

#[test]
fn a_thrown_value_stops_the_script_and_a_caught_one_doesnt() {
    let (thrown, ..) = join("fn on_join(first) {\n  throw \"no pickaxe\";\n}");
    let (caught, ..) = join(
        "fn on_join(first) {\n  try { throw \"x\"; } catch (e) { log.info(`caught ${e}`); }\n  try { keys.forward.hodl(); } catch { log.info(\"and this\"); }\n}",
    );

    assert_eq!(
        stopped(&thrown).kind,
        ScriptErrorKind::Failed {
            message: "Runtime error: no pickaxe".to_owned()
        }
    );
    assert_eq!(logged(&caught.actions), ["caught x", "and this"]);
}

#[test]
fn a_game_object_cant_be_replaced_or_changed() {
    for statement in [
        "keys = 1;",
        "bot.health = 20.0;",
        "mouse.left = 1;",
        "world = #{};",
    ] {
        let (ran, ..) = join(&format!("fn on_join(first) {{\n  {statement}\n}}"));

        assert!(
            matches!(ran.outcome, Outcome::Stopped(_)),
            "{statement}: {ran:?}"
        );
    }
}

#[test]
fn a_failed_scripts_message_is_one_line_of_at_most_200_characters() {
    let (ran, ..) = join(
        "fn on_join(first) {\n  let long = \"\";\n  long.pad(900, 'x');\n  throw long + \"\\nsecond line\";\n}",
    );

    match &stopped(&ran).kind {
        ScriptErrorKind::Failed { message } => {
            assert_eq!(
                message.chars().count(),
                fleet_script::ScriptError::MAX_MESSAGE_CHARS
            );
            assert!(!message.contains('\n'));
        }
        other => panic!("{other:?}"),
    }
}
