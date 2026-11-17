//! Timers: 16 of them, 250 ms to 24 hours, waiting while the bot is dead,
//! cleared at a disconnect (P15.7; ADR-0036, D3, D6).

use core::time::Duration;

use fleet_core::script::{HookEvent, limits};
use fleet_script::{Action, Outcome, Presence};
use rstest::rstest;

use crate::support::{join, logged};

/// Every log line of the wakes in `wakes`.
fn lines(wakes: &[fleet_script::Ran]) -> Vec<String> {
    wakes
        .iter()
        .flat_map(|ran| logged(&ran.actions).into_iter().map(ToString::to_string))
        .collect()
}

#[test]
fn a_timer_set_with_after_runs_on_timer_once_with_its_name() {
    let (ran, mut world, mut instance) = join(
        "fn on_join(first) { let r = timer.after(1000, \"once\"); log.info(`${r.ok}`); }\nfn on_timer(name) { log.info(`timer ${name}`); }",
    );
    assert_eq!(ran.next_wake, Some(world.now + Duration::from_millis(1000)));
    assert_eq!(logged(&ran.actions), ["true"]);

    let early = world.pass(&mut instance, Duration::from_millis(999));
    let due = world.pass(&mut instance, Duration::from_millis(1));
    let later = world.pass(&mut instance, Duration::from_secs(60));

    assert_eq!(early.len(), 0);
    assert_eq!(lines(&due), ["timer once"]);
    assert_eq!(later.len(), 0);
    assert_eq!(instance.timers(), 0);
    assert_eq!(instance.next_wake(), None);
}

#[test]
fn a_timer_set_with_every_runs_again_and_again_first_after_its_interval() {
    let (_, mut world, mut instance) = join(
        "fn on_join(first) { timer.every(600, \"swing\"); state.n = 0; }\nfn on_timer(name) { state.n += 1; log.info(`${name} ${state.n}`); }",
    );

    let first = world.pass(&mut instance, Duration::from_millis(599));
    let rest = world.pass(&mut instance, Duration::from_millis(1801));

    assert_eq!(first.len(), 0);
    assert_eq!(lines(&rest), ["swing 1", "swing 2", "swing 3", "swing 4"]);
    assert_eq!(instance.timers(), 1);
}

#[test]
fn timers_work_from_a_helper_and_from_on_timer_itself() {
    let (_, mut world, mut instance) = join(
        "fn on_join(first) { again(); }\nfn again() { timer.after(1000, \"again\"); }\nfn on_timer(name) { log.info(name); again(); }",
    );

    let wakes = world.pass(&mut instance, Duration::from_millis(3500));

    assert_eq!(lines(&wakes), ["again", "again", "again"]);
}

#[test]
fn setting_a_name_again_replaces_its_timer() {
    let (_, mut world, mut instance) = join(
        "fn on_join(first) {\n  timer.every(1000, \"t\");\n  timer.after(5000, \"t\");\n}\nfn on_timer(name) { log.info(name); }",
    );
    assert_eq!(instance.timers(), 1);

    let wakes = world.pass(&mut instance, Duration::from_secs(20));

    assert_eq!(lines(&wakes), ["t"]);
}

#[test]
fn a_cancelled_timer_never_runs_and_cancelling_nothing_is_fine() {
    let (ran, mut world, mut instance) = join(
        "fn on_join(first) {\n  timer.every(1000, \"a\");\n  timer.after(1000, \"b\");\n  let c = timer.cancel(\"a\");\n  let d = timer.cancel(\"never set\");\n  log.info(`${c.ok} ${d.ok}`);\n}\nfn on_timer(name) { log.info(name); }",
    );
    assert_eq!(logged(&ran.actions), ["true true"]);

    let wakes = world.pass(&mut instance, Duration::from_secs(5));

    assert_eq!(lines(&wakes), ["b"]);
}

#[rstest]
#[case(249, false)]
#[case(250, true)]
#[case(36_000_000, true)]
#[case(86_400_000, true)]
#[case(86_400_001, false)]
#[case(0, false)]
#[case(-250, false)]
fn a_timer_is_250_ms_to_24_hours_long(#[case] ms: i64, #[case] ok: bool) {
    for method in ["after", "every"] {
        let (ran, _, instance) = join(&format!(
            "fn on_join(first) {{ let r = timer.{method}({ms}, \"t\"); log.info(`${{r.ok}} [${{r.reason}}]`); }}"
        ));

        let expected = if ok { "true []" } else { "false [bad_value]" };
        assert_eq!(logged(&ran.actions), [expected], "{method} {ms}");
        assert_eq!(instance.timers(), usize::from(ok));
    }
}

#[test]
fn a_timer_of_ten_hours_runs_ten_hours_later() {
    let (ran, mut world, mut instance) = join(
        "fn on_join(first) { timer.every(36000000, \"balance\"); }\nfn on_timer(name) { log.info(name); }",
    );
    assert_eq!(ran.next_wake, Some(world.now + Duration::from_hours(10)));

    let before = world.pass(
        &mut instance,
        Duration::from_hours(10) - Duration::from_millis(1),
    );
    let then = world.pass(&mut instance, Duration::from_millis(1));

    assert_eq!(before.len(), 0);
    assert_eq!(lines(&then), ["balance"]);
}

#[rstest]
#[case("\"\"")]
#[case("\"a name of more than thirty-two characters\"")]
#[case("\"line\\nbreak\"")]
#[case("5")]
#[case("()")]
fn a_timers_name_is_a_short_plain_text(#[case] name: &str) {
    let (ran, _, instance) = join(&format!(
        "fn on_join(first) {{ let r = timer.after(1000, {name}); log.info(`${{r.ok}} [${{r.reason}}]`); }}"
    ));

    assert_eq!(logged(&ran.actions), ["false [bad_value]"], "{name}");
    assert_eq!(instance.timers(), 0);
}

#[test]
fn a_script_holds_16_timers_and_no_more_from_a_hook_and_from_a_helper() {
    for source in [
        "fn on_join(first) {\n  for i in 0..16 { timer.after(1000, `t${i}`); }\n  let r = timer.after(1000, \"one more\");\n  let again = timer.every(2000, \"t3\");\n  log.info(`${r.ok} [${r.reason}] ${again.ok}`);\n}",
        "fn on_join(first) { set(); }\nfn set() {\n  for i in 0..16 { timer.after(1000, `t${i}`); }\n  let r = timer.after(1000, \"one more\");\n  let again = timer.every(2000, \"t3\");\n  log.info(`${r.ok} [${r.reason}] ${again.ok}`);\n}",
    ] {
        let (ran, _, instance) = join(source);

        // Replacing one of the sixteen is no seventeenth.
        assert_eq!(logged(&ran.actions), ["false [too_many] true"]);
        assert_eq!(instance.timers(), limits::MAX_TIMERS);
    }
}

#[test]
fn timers_due_at_once_run_in_the_order_they_are_due_each_as_its_own_hook() {
    let (_, mut world, mut instance) = join(
        "fn on_join(first) {\n  timer.after(3000, \"c\");\n  timer.after(1000, \"a\");\n  timer.after(2000, \"b\");\n}\nfn on_timer(name) { log.info(name); }",
    );

    // Nothing woke the script for five seconds: all three are due.
    world.now += Duration::from_secs(5);
    let ran = world.wake(&mut instance);

    assert_eq!(logged(&ran.actions), ["a", "b", "c"]);
    assert!(matches!(ran.outcome, Outcome::Ran { .. }));
    assert_eq!(ran.next_wake, None);
}

#[test]
fn a_repeating_timer_doesnt_catch_up_on_what_it_missed() {
    let (_, mut world, mut instance) = join(
        "fn on_join(first) { timer.every(1000, \"t\"); }\nfn on_timer(name) { log.info(name); }",
    );

    world.now += Duration::from_secs(10);
    let late = world.wake(&mut instance);

    assert_eq!(logged(&late.actions), ["t"]);
    assert_eq!(
        late.next_wake,
        Some(world.now + Duration::from_millis(1000))
    );
}

#[test]
fn timers_wait_while_the_bot_is_dead_and_run_once_it_lives_again() {
    let (_, mut world, mut instance) = join(
        "fn on_join(first) { timer.every(1000, \"t\"); }\nfn on_timer(name) { log.info(name); }",
    );
    world.run(&mut instance, &HookEvent::Death);
    assert_eq!(instance.next_wake(), None);

    world.now += Duration::from_secs(30);
    let dead = world.wake(&mut instance);
    assert_eq!(dead.actions.len(), 0);
    assert_eq!(dead.next_wake, None);
    let respawn = world.run(&mut instance, &HookEvent::Respawn);
    assert_eq!(respawn.next_wake, Some(world.now));
    let alive = world.wake(&mut instance);

    // Once, not thirty times.
    assert_eq!(logged(&alive.actions), ["t"]);
    assert_eq!(instance.timers(), 1);
}

#[test]
fn timers_wait_while_the_bot_is_between_worlds() {
    let (_, mut world, mut instance) = join(
        "fn on_join(first) { timer.after(1000, \"t\"); }\nfn on_timer(name) { log.info(name); }",
    );

    world.now += Duration::from_secs(2);
    world.presence = Presence::NotInWorld;
    let between = world.wake(&mut instance);
    world.presence = Presence::InWorld;
    let back = world.wake(&mut instance);

    assert_eq!(between.actions.len(), 0);
    assert_eq!(between.next_wake, None);
    assert_eq!(logged(&back.actions), ["t"]);
}

#[test]
fn a_hold_for_still_ends_while_the_bot_is_dead() {
    let (_, mut world, mut instance) = join(
        "fn on_join(first) { }\nfn on_death() { }\nfn on_timer(name) { keys.sneak.hold_for(1000); }",
    );
    world.run(&mut instance, &HookEvent::Timer(crate::support::timer("t")));
    world.presence = Presence::Dead;

    world.now += Duration::from_secs(2);
    let ran = world.wake(&mut instance);

    assert_eq!(
        ran.actions,
        [Action::Release(fleet_core::script::Input::Key(
            fleet_core::script::Key::Sneak
        ))]
    );
}

#[test]
fn a_disconnect_clears_the_timers_and_what_was_held() {
    let (_, mut world, mut instance) = join(
        "fn on_join(first) { if first { timer.every(1000, \"t\"); keys.forward.hold_for(5000); } }\nfn on_timer(name) { log.info(name); }",
    );
    assert_eq!(instance.timers(), 1);

    instance.disconnected();

    assert_eq!(instance.timers(), 0);
    assert_eq!(instance.next_wake(), None);
    assert!(!instance.is_held(fleet_core::script::Input::Key(
        fleet_core::script::Key::Forward
    )));
    world.run(&mut instance, &HookEvent::Join { first: false });
    let wakes = world.pass(&mut instance, Duration::from_secs(10));
    assert_eq!(wakes.len(), 0);
}

#[test]
fn a_script_that_stops_in_on_timer_loses_its_timers() {
    let (_, mut world, mut instance) = join(
        "fn on_join(first) { timer.every(1000, \"a\"); timer.every(1000, \"b\"); }\nfn on_timer(name) { log.info(name); loop { } }",
    );

    world.now += Duration::from_secs(1);
    let ran = world.wake(&mut instance);

    assert!(matches!(ran.outcome, Outcome::Stopped(_)));
    assert_eq!(ran.actions, [Action::ReleaseAll]);
    assert_eq!(ran.next_wake, None);
    assert_eq!(instance.timers(), 0);
}

#[test]
fn an_orderly_stop_clears_the_timers() {
    let (_, mut world, mut instance) = join(
        "fn on_join(first) { timer.every(1000, \"t\"); }\nfn on_timer(name) { log.info(name); }",
    );

    let stop = world.run(&mut instance, &HookEvent::Stop);
    world.now += Duration::from_secs(5);
    let after = world.wake(&mut instance);

    assert_eq!(stop.outcome, Outcome::Ended { error: None });
    assert_eq!(stop.actions, [Action::ReleaseAll]);
    assert_eq!(after.outcome, Outcome::Over);
    assert_eq!(instance.timers(), 0);
}

#[test]
fn an_error_in_on_stop_is_reported_and_the_script_still_ends() {
    let (_, mut world, mut instance) = join(
        "fn on_join(first) { keys.forward.hold(); }\nfn on_stop() {\n  chat.send(\"x\");\n  nothing_like_this();\n}",
    );

    let stop = world.run(&mut instance, &HookEvent::Stop);

    match &stop.outcome {
        Outcome::Ended { error: Some(error) } => {
            assert_eq!(error.hook, fleet_core::script::Hook::Stop);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(stop.actions, [Action::ReleaseAll]);
    // The orderly end is no failure of the script.
    assert_eq!(instance.error(), None);
}
