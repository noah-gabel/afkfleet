//! [`Doubling`]: a backoff without jitter that doubles with every failure in
//! a row, up to a maximum (Plan.md P9.5, ADR-0018).

use core::num::NonZeroU32;
use core::time::Duration;

use chrono::{DateTime, Utc};

use crate::time;

/// How long to wait after the `n`-th failure in a row: `first`, doubled with
/// each further failure, never more than `max`. No jitter: the server's
/// session-token service uses it, where one process decides for every
/// caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Doubling {
    first: Duration,
    max: Duration,
}

impl Doubling {
    /// A backoff from `first` up to `max`. A `max` below `first` is raised
    /// to `first`, so every pair makes a valid policy and constants need no
    /// check.
    #[must_use]
    pub const fn new(first: Duration, max: Duration) -> Self {
        let max = if max.as_nanos() < first.as_nanos() {
            first
        } else {
            max
        };
        Self { first, max }
    }

    /// The first delay.
    #[must_use]
    pub const fn first(&self) -> Duration {
        self.first
    }

    /// The longest delay.
    #[must_use]
    pub const fn max(&self) -> Duration {
        self.max
    }

    /// The delay after `failures` failures in a row.
    #[must_use]
    pub fn delay(&self, failures: NonZeroU32) -> Duration {
        let doublings = failures.get().saturating_sub(1);
        2_u32
            .checked_pow(doublings)
            .and_then(|factor| self.first.checked_mul(factor))
            .map_or(self.max, |delay| delay.min(self.max))
    }

    /// When the wait after `failures` failures in a row, the last one at
    /// `now`, ends.
    #[must_use]
    pub fn until(&self, now: DateTime<Utc>, failures: NonZeroU32) -> DateTime<Utc> {
        time::add(now, self.delay(failures))
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    const FIVE_SECONDS: Duration = Duration::from_secs(5);
    const FIVE_MINUTES: Duration = Duration::from_mins(5);

    fn failures(n: u32) -> NonZeroU32 {
        NonZeroU32::new(n).unwrap()
    }

    #[rstest]
    #[case::first(1, 5)]
    #[case::second(2, 10)]
    #[case::third(3, 20)]
    #[case::sixth(6, 160)]
    #[case::seventh_is_capped(7, 300)]
    #[case::far_past_the_cap(40, 300)]
    #[case::largest(u32::MAX, 300)]
    fn the_delay_doubles_up_to_the_maximum(#[case] n: u32, #[case] secs: u64) {
        let backoff = Doubling::new(FIVE_SECONDS, FIVE_MINUTES);

        assert_eq!(backoff.delay(failures(n)), Duration::from_secs(secs));
    }

    #[test]
    fn a_maximum_below_the_first_delay_is_raised_to_it() {
        let backoff = Doubling::new(FIVE_MINUTES, FIVE_SECONDS);

        assert_eq!(backoff.max(), FIVE_MINUTES);
        assert_eq!(backoff.delay(failures(1)), FIVE_MINUTES);
        assert_eq!(backoff.delay(failures(3)), FIVE_MINUTES);
    }

    #[test]
    fn a_first_delay_that_overflows_when_doubled_stays_at_the_maximum() {
        let backoff = Doubling::new(Duration::MAX, Duration::MAX);

        assert_eq!(backoff.delay(failures(2)), Duration::MAX);
    }

    #[test]
    fn until_adds_the_delay_to_now() {
        let backoff = Doubling::new(FIVE_SECONDS, FIVE_MINUTES);
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();

        assert_eq!(
            backoff.until(now, failures(2)),
            DateTime::from_timestamp(1_800_000_010, 0).unwrap()
        );
        assert_eq!(
            backoff.until(DateTime::<Utc>::MAX_UTC, failures(1)),
            DateTime::<Utc>::MAX_UTC
        );
    }

    #[test]
    fn the_accessors_return_the_bounds() {
        let backoff = Doubling::new(FIVE_SECONDS, FIVE_MINUTES);

        assert_eq!(backoff.first(), FIVE_SECONDS);
        assert_eq!(backoff.max(), FIVE_MINUTES);
    }
}
