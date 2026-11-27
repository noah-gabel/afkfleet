//! What a call tells whoever runs the script about what it refused
//! (P15.18; ADR-0036, D7; ADR-0037, C7): each kind and reason once, with
//! how often, beside the result the script itself got.

use core::time::Duration;

use fleet_core::script::{HookEvent, Refusal};
use fleet_script::{Asked, Instance, Outcome, Presence, Refused};

use crate::support::{World, balance_draft, instance, program_of, still};

const fn refused(asked: Asked, reason: Refusal, times: u32) -> Refused {
    Refused {
        asked,
        reason,
        times,
    }
}

const JOIN: HookEvent = HookEvent::Join { first: true };

#[test]
fn a_hook_that_was_given_everything_reports_nothing_refused() {
    let source = "fn on_join(first) {\n  keys.forward.hold();\n  look.at(90.0, 0.0);\n  hotbar.select(2);\n  keys.forward.release();\n  log.info(\"hello\");\n}";
    let mut world = World::new();

    let ran = world.run(&mut instance(source), &JOIN);

    assert!(matches!(ran.outcome, Outcome::Ran { .. }), "{ran:?}");
    assert_eq!(ran.refused, []);
}

#[test]
fn each_kind_and_reason_is_reported_once_with_how_often() {
    // Three taps of one key in one hook: the second and third are too
    // soon. Two looks with an angle out of range, one hotbar slot 10.
    let source = "fn on_join(first) {\n  keys.jump.tap();\n  keys.jump.tap();\n  keys.jump.tap();\n  look.at(400.0, 0.0);\n  look.at(0.0, 100.0);\n  hotbar.select(10);\n}";
    let mut world = World::new();

    let ran = world.run(&mut instance(source), &JOIN);

    assert_eq!(
        ran.refused,
        [
            refused(Asked::Press, Refusal::TooSoon, 2),
            refused(Asked::Look, Refusal::BadValue, 2),
            refused(Asked::Hotbar, Refusal::BadValue, 1),
        ]
    );
}

#[test]
fn one_kind_refused_for_two_reasons_is_reported_for_each() {
    let source = "fn on_join(first) {\n  mouse.left.hold();\n  mouse.left.hold_for(1);\n  keys.sneak.hold_for(1);\n}";
    let mut world = World::new();

    let ran = world.run(&mut instance(source), &JOIN);

    // A hold of 1 ms is no hold: a bad value, for the button that's down
    // and for the key that isn't.
    assert_eq!(ran.refused, [refused(Asked::Press, Refusal::BadValue, 2)]);
}

#[test]
fn a_dead_bots_hook_reports_everything_it_asked_for() {
    let source = "fn on_death() {\n  keys.forward.hold();\n  vehicle.mount(\"minecart\");\n  vehicle.dismount();\n  chat.send(\"hello\");\n}";
    let mut world = World::new();
    let mut instance = Instance::new(program_of(balance_draft(source)), 7, still());

    let ran = world.run(&mut instance, &HookEvent::Death);

    assert_eq!(world.presence, Presence::Dead);
    assert_eq!(
        ran.refused,
        [
            refused(Asked::Press, Refusal::Dead, 1),
            refused(Asked::Vehicle, Refusal::Dead, 2),
            refused(Asked::Send, Refusal::Dead, 1),
        ]
    );
}

#[test]
fn a_window_asked_for_without_a_command_is_reported() {
    let source = "fn on_join(first) {\n  chat.expect(\"balance\", 5000);\n  chat.send(\"hello\");\n  chat.expect(\"balance\", 5000);\n}";
    let mut world = World::new();
    let mut instance = Instance::new(program_of(balance_draft(source)), 7, still());

    let ran = world.run(&mut instance, &JOIN);

    // Plain chat is no command either.
    assert_eq!(ran.refused, [refused(Asked::Expect, Refusal::NoCommand, 2)]);
}

#[test]
fn a_timer_and_a_log_line_too_many_are_reported() {
    let source = "fn on_join(first) {\n  timer.after(10, \"too short\");\n  for n in 0..22 {\n    log.info(`line ${n}`);\n  }\n}";
    let mut world = World::new();

    let ran = world.run(&mut instance(source), &JOIN);

    assert_eq!(
        ran.refused,
        [
            refused(Asked::Timer, Refusal::BadValue, 1),
            refused(Asked::Log, Refusal::TooSoon, 2),
        ]
    );
}

#[test]
fn each_wake_reports_what_its_own_hook_was_refused() {
    // Two timers due at once, each looking twice: the second look of each
    // hook is too soon, and so is the first of the second hook.
    let source = "fn on_join(first) {\n  timer.after(1000, \"a\");\n  timer.after(1000, \"b\");\n}\nfn on_timer(name) {\n  look.at(0.0, 0.0);\n  look.at(0.0, 0.0);\n}";
    let mut world = World::new();
    let mut instance = instance(source);
    world.run(&mut instance, &JOIN);

    let wakes = world.pass(&mut instance, Duration::from_millis(1_000));

    assert_eq!(wakes.len(), 2);
    assert_eq!(
        wakes[0].refused,
        [refused(Asked::Look, Refusal::TooSoon, 1)]
    );
    assert_eq!(
        wakes[1].refused,
        [refused(Asked::Look, Refusal::TooSoon, 2)]
    );
}

#[test]
fn a_hook_that_fails_still_reports_what_it_was_refused_before() {
    let source = "fn on_join(first) {\n  hotbar.select(0);\n  keys.forward.hodl();\n}";
    let mut world = World::new();

    let ran = world.run(&mut instance(source), &JOIN);

    assert!(matches!(ran.outcome, Outcome::Stopped(_)), "{ran:?}");
    assert_eq!(ran.refused, [refused(Asked::Hotbar, Refusal::BadValue, 1)]);
}

#[test]
fn what_one_call_was_refused_isnt_reported_again_by_the_next() {
    let source =
        "fn on_join(first) {\n  hotbar.select(0);\n}\nfn on_respawn() {\n  hotbar.select(1);\n}";
    let mut world = World::new();
    let mut instance = instance(source);

    let join = world.run(&mut instance, &JOIN);
    let respawn = world.run(&mut instance, &HookEvent::Respawn);

    assert_eq!(join.refused.len(), 1);
    assert_eq!(respawn.refused, []);
}

#[test]
fn a_script_that_is_over_reports_nothing() {
    let source = "fn on_join(first) {\n  keys.forward.hodl();\n}";
    let mut world = World::new();
    let mut instance = instance(source);
    world.run(&mut instance, &JOIN);

    let after = world.run(&mut instance, &HookEvent::Respawn);

    assert_eq!(after.outcome, Outcome::Over);
    assert_eq!(after.refused, []);
}

#[test]
fn every_kind_has_its_own_short_name() {
    let mut names: Vec<&str> = Asked::ALL.iter().map(|asked| asked.as_str()).collect();

    assert_eq!(
        names,
        [
            "press", "look", "hotbar", "vehicle", "send", "expect", "timer", "log", "click",
            "screen", "sign"
        ]
    );
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), Asked::ALL.len());
}
