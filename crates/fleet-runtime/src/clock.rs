//! [`RuntimeClock`]: the runtime's wall clock, driven by tokio's clock, and
//! the clock of the chat bucket.

use chrono::{DateTime, Utc};
use fleet_core::time;
use tokio::time::Instant;

/// The runtime's wall clock (ADR-0010, ADR-0013).
///
/// It starts at a wall time that its caller passes in and moves with tokio's
/// clock, so the runtime never reads the system clock, and
/// `#[tokio::test(start_paused = true)]` controls it. fleet-core takes the
/// time as a `DateTime<Utc>`; this is where the runtime gets it from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeClock {
    anchor: DateTime<Utc>,
    base: Instant,
}

impl RuntimeClock {
    /// Starts a clock that reads `anchor` now.
    #[must_use]
    pub fn new(anchor: DateTime<Utc>) -> Self {
        Self {
            anchor,
            base: Instant::now(),
        }
    }

    /// Returns the current wall time: the anchor, plus however far tokio's
    /// clock has moved since [`new`](Self::new).
    #[must_use]
    pub fn now(&self) -> DateTime<Utc> {
        time::add(self.anchor, self.base.elapsed())
    }

    /// Returns the tokio instant at which this clock reads `at`, to sleep
    /// until then. A time before the anchor maps to the moment the clock
    /// started, so it's due at once. `None` if tokio's clock can't represent
    /// a time that far ahead.
    #[must_use]
    pub fn deadline(&self, at: DateTime<Utc>) -> Option<Instant> {
        self.base.checked_add(time::elapsed(self.anchor, at))
    }
}

/// governor's clock for the chat bucket: tokio's clock, as a std `Instant`,
/// so paused test time moves the bucket too (Plan.md §8; ADR-0013).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TokioClock;

impl governor::clock::Clock for TokioClock {
    type Instant = std::time::Instant;

    fn now(&self) -> Self::Instant {
        Instant::now().into_std()
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use super::*;

    fn anchor() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn the_clock_starts_at_its_anchor_and_moves_with_tokio_time() {
        let clock = RuntimeClock::new(anchor());
        assert_eq!(clock.now(), anchor());

        tokio::time::advance(Duration::from_millis(1_500)).await;

        assert_eq!(
            clock.now(),
            time::add(anchor(), Duration::from_millis(1_500))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_deadline_is_when_the_clock_reads_that_time() {
        let clock = RuntimeClock::new(anchor());
        let at = time::add(anchor(), Duration::from_secs(2));

        tokio::time::sleep_until(clock.deadline(at).unwrap()).await;

        assert_eq!(clock.now(), at);
    }

    #[tokio::test(start_paused = true)]
    async fn a_time_before_the_anchor_is_due_at_once() {
        let clock = RuntimeClock::new(anchor());
        tokio::time::advance(Duration::from_secs(5)).await;

        let deadline = clock.deadline(DateTime::<Utc>::MIN_UTC);

        assert!(deadline.unwrap() <= Instant::now());
        assert_eq!(deadline, clock.deadline(anchor()), "both map to the start");
    }

    #[tokio::test(start_paused = true)]
    async fn the_latest_time_is_far_ahead_or_has_no_deadline() {
        let clock = RuntimeClock::new(anchor());

        let deadline = clock.deadline(DateTime::<Utc>::MAX_UTC);

        let a_year = Duration::from_hours(365 * 24);
        assert!(
            deadline.is_none_or(|deadline| deadline > Instant::now() + a_year),
            "{deadline:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_bucket_clock_moves_with_tokio_time() {
        use governor::clock::Clock as _;

        let before = TokioClock.now();
        tokio::time::advance(Duration::from_secs(3)).await;

        assert_eq!(TokioClock.now() - before, Duration::from_secs(3));
    }
}
