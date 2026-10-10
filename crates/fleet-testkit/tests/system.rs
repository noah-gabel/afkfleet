//! Tests for `ManualClock` and `SeededRandom`: the time only moves when the
//! test moves it, and the bytes depend only on the seed (Plan.md P6,
//! ADR-0015). No test hard-codes `SeededRandom`'s bytes.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;

use chrono::{DateTime, SubsecRound, Utc};
use fleet_core::system::{Clock, RandomError, SecureRandom};
use fleet_testkit::system::{ManualClock, SeededRandom};

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

fn bytes(random: &SeededRandom) -> [u8; 32] {
    let mut buffer = [0; 32];
    random.fill(&mut buffer).unwrap();
    buffer
}

#[test]
fn the_clock_reads_its_start() {
    let clock = ManualClock::new(at(1_800_000_000_123));

    assert_eq!(clock.now(), at(1_800_000_000_123));
}

#[test]
fn the_clock_truncates_to_milliseconds() {
    let start = DateTime::from_timestamp(1_800_000_000, 123_456_789).unwrap();

    let clock = ManualClock::new(start);
    assert_eq!(clock.now(), at(1_800_000_000_123));

    clock.set(DateTime::from_timestamp(1_800_000_001, 999_999).unwrap());
    assert_eq!(clock.now(), at(1_800_000_001_000));

    clock.advance(Duration::from_micros(2_500));
    assert_eq!(clock.now(), at(1_800_000_001_002));
}

#[test]
fn advance_moves_the_time_forward() {
    let clock = ManualClock::new(at(1_800_000_000_000));

    clock.advance(Duration::from_secs(90));

    assert_eq!(clock.now(), at(1_800_000_090_000));
}

#[test]
fn advance_stops_at_the_latest_time() {
    let clock = ManualClock::new(at(1_800_000_000_000));

    clock.advance(Duration::MAX);

    assert_eq!(clock.now(), DateTime::<Utc>::MAX_UTC.trunc_subsecs(3));
}

#[test]
fn set_jumps_backwards_too() {
    let clock = ManualClock::new(at(1_800_000_000_000));

    clock.set(at(1_700_000_000_000));

    assert_eq!(clock.now(), at(1_700_000_000_000));
}

#[test]
fn clock_clones_share_the_time() {
    let clock = ManualClock::new(at(1_800_000_000_000));
    let clone = clock.clone();

    clone.advance(Duration::from_secs(1));

    assert_eq!(clock.now(), at(1_800_000_001_000));
}

#[test]
fn the_clock_works_as_a_trait_object() {
    let clock: &dyn Clock = &ManualClock::new(at(1_800_000_000_000));

    assert_eq!(clock.now(), at(1_800_000_000_000));
}

#[test]
fn the_same_seed_gives_the_same_bytes() {
    assert_eq!(bytes(&SeededRandom::new(42)), bytes(&SeededRandom::new(42)));
}

#[test]
fn different_seeds_give_different_bytes() {
    assert_ne!(bytes(&SeededRandom::new(42)), bytes(&SeededRandom::new(43)));
}

#[test]
fn consecutive_fills_differ() {
    let random = SeededRandom::new(42);

    assert_ne!(bytes(&random), bytes(&random));
}

#[test]
fn clones_continue_one_stream() {
    let single = SeededRandom::new(42);
    let expected = (bytes(&single), bytes(&single));

    let random = SeededRandom::new(42);
    let clone = random.clone();

    assert_eq!((bytes(&random), bytes(&clone)), expected);
}

#[test]
fn fail_next_fails_once_and_keeps_the_stream() {
    let random = SeededRandom::new(42);
    random.fail_next(RandomError::Os { code: 5 });
    let mut buffer = [0; 32];

    assert_eq!(random.fill(&mut buffer), Err(RandomError::Os { code: 5 }));
    assert_eq!(bytes(&random), bytes(&SeededRandom::new(42)));
}

#[test]
fn queued_failures_are_used_in_order() {
    let random = SeededRandom::new(42);
    random.fail_next(RandomError::Unavailable);
    random.fail_next(RandomError::Os { code: 7 });
    let mut buffer = [0; 4];

    assert_eq!(random.fill(&mut buffer), Err(RandomError::Unavailable));
    assert_eq!(random.fill(&mut buffer), Err(RandomError::Os { code: 7 }));
    assert_eq!(random.fill(&mut buffer), Ok(()));
}

#[test]
fn the_source_works_as_a_trait_object() {
    let random: &dyn SecureRandom = &SeededRandom::new(42);
    let mut first = [0; 16];
    let mut second = [0; 16];

    random.fill(&mut first).unwrap();
    SeededRandom::new(42).fill(&mut second).unwrap();

    assert_eq!(first, second);
}
