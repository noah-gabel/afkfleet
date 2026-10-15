//! The pacing fake's own behavior: the link flows' tests rely on it to
//! show every wait and move the clock exactly.

use core::time::Duration;
use std::sync::Arc;

use fleet_core::system::Clock as _;
use fleet_server::ports::pace::Pace as _;
use fleet_testkit::system::ManualClock;

use crate::support::pace::FakePace;
use crate::support::service::{T0, at, bounded, poll_once};

fn pace() -> (FakePace, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(at(T0)));
    (FakePace::new(clock.clone()), clock)
}

#[tokio::test]
async fn each_wait_is_recorded_and_moves_the_clock_by_its_duration() {
    let (pace, clock) = pace();

    bounded(pace.wait(Duration::from_secs(5))).await;
    bounded(pace.wait(Duration::from_secs(10))).await;

    assert_eq!(
        pace.waits(),
        [Duration::from_secs(5), Duration::from_secs(10)]
    );
    assert_eq!(clock.now(), at(T0 + 15_000));
}

#[tokio::test]
async fn a_held_wait_ends_when_released_and_only_then_moves_the_clock() {
    let (pace, clock) = pace();
    let mut gate = pace.hold_next();
    let mut wait = Box::pin(pace.wait(Duration::from_secs(5)));

    assert!(poll_once(&mut wait).await.is_none());
    gate.entered().await;
    assert_eq!(clock.now(), at(T0));
    gate.release();
    bounded(wait).await;

    assert_eq!(clock.now(), at(T0 + 5_000));
    assert_eq!(pace.waits(), [Duration::from_secs(5)]);
}

#[tokio::test]
async fn only_the_next_wait_is_held() {
    let (pace, clock) = pace();
    let gate = pace.hold_next();
    let mut held = Box::pin(pace.wait(Duration::from_secs(5)));
    assert!(poll_once(&mut held).await.is_none());

    bounded(pace.wait(Duration::from_secs(1))).await;

    assert_eq!(clock.now(), at(T0 + 1_000));
    gate.release();
    bounded(held).await;
    assert_eq!(clock.now(), at(T0 + 6_000));
}

#[tokio::test]
async fn a_held_wait_whose_gate_is_dropped_never_ends() {
    let (pace, clock) = pace();
    let gate = pace.hold_next();
    let mut wait = Box::pin(pace.wait(Duration::from_secs(5)));
    assert!(poll_once(&mut wait).await.is_none());

    drop(gate);

    assert!(poll_once(&mut wait).await.is_none());
    assert_eq!(clock.now(), at(T0));
}
