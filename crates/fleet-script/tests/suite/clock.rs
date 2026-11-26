//! `clock`: the time of day of the hook's moment in a named time zone
//! (Plan.md P16.21; ADR-0052).

use chrono::{DateTime, TimeZone as _, Utc};
use fleet_core::script::{HookEvent, ScriptDraft};
use fleet_script::{CompileErrorKind, Instance, Misuse, Outcome, ScriptErrorKind, compile};

use crate::support::{World, draft, program_of, script_of, still, timer};

/// The reference's example, word for word (docs/scripting.md, "The time
/// of day").
const WINDOW: &str = r#"// Only between 14:00 and 19:00 German time.
fn in_window() {
    let now = clock.local("Europe/Berlin");
    now.hour >= 14 && now.hour < 19
}

fn on_timer(name) {
    if in_window() {
        chat.send("balance");
    }
}"#;

fn level_2(source: &str) -> Instance {
    let draft = ScriptDraft {
        api: 2,
        ..draft(source)
    };
    Instance::new(program_of(draft), 7, still())
}

fn utc(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
        .unwrap()
}

#[test]
fn a_script_reads_the_time_of_its_hooks_moment_in_a_named_zone() {
    let mut world = World::new();
    world.now = utc(2026, 11, 17, 12, 34);
    let mut instance = level_2(
        r#"fn on_join(first) {
  let now = clock.local("Europe/Berlin");
  log.info(`${now.year}-${now.month}-${now.day} ${now.hour}:${now.minute}:${now.second} ${now.weekday} ${now.offset}`);
  let utc = clock.local("UTC");
  log.info(`${utc.hour}:${utc.minute} ${utc.offset}`);
}"#,
    );

    world.run(&mut instance, &HookEvent::Join { first: true });

    assert_eq!(world.log, ["2026-11-17 13:34:0 2 60", "12:34 0"]);
}

#[test]
fn summer_time_moves_berlins_hour_and_offset() {
    let mut world = World::new();
    world.now = utc(2026, 7, 1, 12, 0);
    let mut instance = level_2(
        "fn on_join(first) { let now = clock.local(\"Europe/Berlin\"); log.info(`${now.hour} ${now.offset}`); }",
    );

    world.run(&mut instance, &HookEvent::Join { first: true });

    assert_eq!(world.log, ["14 120"]);
}

#[test]
fn an_unknown_zone_stops_the_script_at_its_line() {
    let mut world = World::new();
    let mut instance =
        level_2("fn on_join(first) {\n  let now = clock.local(\"Europe/Berln\");\n}");

    let ran = world.run(&mut instance, &HookEvent::Join { first: true });

    let Outcome::Stopped(error) = ran.outcome else {
        panic!("{ran:?}");
    };
    assert_eq!(error.kind, ScriptErrorKind::Misuse(Misuse::UnknownTimeZone));
    assert_eq!(error.at.map(|at| at.line), Some(2));
}

#[test]
fn a_zone_that_is_no_text_stops_the_script_too() {
    let mut world = World::new();
    let mut instance = level_2("fn on_join(first) { clock.local(1); }");

    let ran = world.run(&mut instance, &HookEvent::Join { first: true });

    assert!(matches!(
        ran.outcome,
        Outcome::Stopped(ref error) if error.kind == ScriptErrorKind::Misuse(Misuse::UnknownTimeZone)
    ));
}

#[test]
fn the_clock_is_level_2s() {
    let script = script_of(draft("fn on_join(first) { clock.local(\"UTC\"); }"));

    assert_eq!(
        compile(&script).unwrap_err().kind,
        CompileErrorKind::NeedsLevel {
            name: "clock".to_owned(),
            level: 2
        }
    );
}

#[rstest::rstest]
#[case::before_the_window_in_winter(utc(2026, 11, 17, 12, 59), false)]
#[case::its_start_in_winter(utc(2026, 11, 17, 13, 0), true)]
#[case::its_last_minute_in_winter(utc(2026, 11, 17, 17, 59), true)]
#[case::its_end_in_winter(utc(2026, 11, 17, 18, 0), false)]
#[case::its_start_in_summer(utc(2026, 7, 1, 12, 0), true)]
#[case::past_its_end_in_summer(utc(2026, 7, 1, 17, 0), false)]
#[case::the_spring_switch_day(utc(2026, 3, 29, 12, 0), true)]
#[case::the_autumn_switch_day(utc(2026, 10, 25, 12, 59), false)]
fn the_references_window_asks_only_between_14_and_19_german_time(
    #[case] at: DateTime<Utc>,
    #[case] asks: bool,
) {
    let mut draft = ScriptDraft {
        api: 2,
        ..draft(WINDOW)
    };
    draft.messages = vec![crate::support::message("balance", "/balance")];
    let mut instance = Instance::new(program_of(draft), 7, still());
    let mut world = World::new();
    world.now = at;

    world.run(&mut instance, &HookEvent::Timer(timer("payout")));

    assert_eq!(world.sent.len(), usize::from(asks), "{at}");
}

#[test]
fn the_reference_shows_the_window_word_for_word() {
    let reference = include_str!("../../../../docs/scripting.md");

    assert!(reference.contains(WINDOW));
}
