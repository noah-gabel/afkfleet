//! Saturating arithmetic on points in time and durations.
//!
//! `fleet-core` takes the current time as a `DateTime<Utc>` and durations as
//! `std::time::Duration` (ADR-0010). chrono's `+` panics on overflow, so all
//! mixed arithmetic goes through these two functions, which never panic.

use core::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};

/// Returns `at + duration`, or the latest representable time if that overflows.
#[must_use]
pub fn add(at: DateTime<Utc>, duration: Duration) -> DateTime<Utc> {
    TimeDelta::from_std(duration)
        .ok()
        .and_then(|delta| at.checked_add_signed(delta))
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
}

/// Returns how long ago `since` was at `now`, or zero if `now` is earlier
/// (the clock went backwards).
#[must_use]
pub fn elapsed(since: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    now.signed_duration_since(since)
        .to_std()
        .unwrap_or(Duration::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    #[test]
    fn add_adds() {
        assert_eq!(add(at(100), Duration::from_secs(5)), at(105));
    }

    #[test]
    fn add_saturates_instead_of_overflowing() {
        assert_eq!(add(at(100), Duration::MAX), DateTime::<Utc>::MAX_UTC);
        assert_eq!(
            add(DateTime::<Utc>::MAX_UTC, Duration::from_secs(1)),
            DateTime::<Utc>::MAX_UTC
        );
    }

    #[test]
    fn elapsed_measures_forward_time() {
        assert_eq!(elapsed(at(100), at(130)), Duration::from_secs(30));
        assert_eq!(elapsed(at(100), at(100)), Duration::ZERO);
    }

    #[test]
    fn elapsed_is_zero_when_the_clock_went_backwards() {
        assert_eq!(elapsed(at(130), at(100)), Duration::ZERO);
    }

    #[test]
    fn elapsed_handles_the_whole_range() {
        assert!(elapsed(DateTime::<Utc>::MIN_UTC, DateTime::<Utc>::MAX_UTC) > Duration::ZERO);
    }
}
