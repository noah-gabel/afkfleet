//! [`SystemClock`] and [`OsRandom`]: the server's real [`Clock`] and
//! [`SecureRandom`] (ADR-0015), [`MonotonicClock`], the rate limiters'
//! clock (ADR-0016), and [`TokioPace`], the real [`Pace`] (ADR-0018).
//!
//! They're the only code in fleet-server that reads the wall clock or the
//! OS's randomness: `clippy.toml` bans every direct read, and each of the two
//! carries the one approved exception. Everything else gets time and random
//! bytes from the ports, so tests can control both.

use core::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, SubsecRound, Utc};
use fleet_core::system::{Clock, RandomError, SecureRandom};
use fleet_core::time::add;
use tokio::time::Instant;

use crate::ports::pace::Pace;

/// The wall clock, in UTC and whole milliseconds.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    #[expect(
        clippy::disallowed_methods,
        reason = "SystemClock is fleet-server's one read of the wall clock; everything else gets time from the Clock port (ADR-0015, approved)"
    )]
    fn now(&self) -> DateTime<Utc> {
        Utc::now().trunc_subsecs(3)
    }
}

/// A clock that never goes back, for the rate limiters only (ADR-0016): one
/// [`SystemClock`] reading as its anchor, plus the time tokio's monotonic
/// clock has measured since, in whole milliseconds. A step of the wall clock
/// can't move it, so it can't make every limiter refuse until the wall clock
/// catches up.
///
/// It ignores NTP corrections and, on Linux, the time a VM spent suspended,
/// so it drifts from real time over a long uptime: nothing stored or audited
/// may read it, only interval measurements like the limiters'.
#[derive(Debug, Clone, Copy)]
pub struct MonotonicClock {
    anchor: DateTime<Utc>,
    start: Instant,
}

impl MonotonicClock {
    /// A clock that starts at the wall clock's time now.
    #[must_use]
    pub fn start() -> Self {
        Self::anchored(SystemClock.now())
    }

    /// A clock that reads `anchor` now and moves with tokio's clock.
    fn anchored(anchor: DateTime<Utc>) -> Self {
        Self {
            anchor,
            start: Instant::now(),
        }
    }
}

impl Clock for MonotonicClock {
    fn now(&self) -> DateTime<Utc> {
        add(self.anchor, self.start.elapsed()).trunc_subsecs(3)
    }
}

/// The OS's secure random source, through getrandom. The only
/// [`SecureRandom`] outside tests, and the one `main.rs` always wires.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsRandom;

impl SecureRandom for OsRandom {
    #[expect(
        clippy::disallowed_methods,
        reason = "OsRandom is fleet-server's one read of the OS's randomness; everything else gets random bytes from the SecureRandom port (ADR-0015, approved)"
    )]
    fn fill(&self, dest: &mut [u8]) -> Result<(), RandomError> {
        getrandom::fill(dest).map_err(|error| random_error(error.raw_os_error()))
    }
}

/// Waits with tokio's timer: the only [`Pace`] outside tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioPace;

#[async_trait]
impl Pace for TokioPace {
    async fn wait(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

/// Maps getrandom's error, which may carry the OS's error number.
fn random_error(raw_os_error: Option<i32>) -> RandomError {
    raw_os_error.map_or(RandomError::Unavailable, |code| RandomError::Os { code })
}

#[cfg(test)]
mod tests {
    use chrono::{Datelike, TimeDelta, TimeZone as _, Timelike};

    use super::*;

    fn anchor() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2027, 1, 15, 8, 0, 0).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn the_monotonic_clock_moves_with_tokios_clock_in_whole_milliseconds() {
        let clock = MonotonicClock::anchored(anchor());

        tokio::time::advance(Duration::from_micros(1_500_700)).await;

        assert_eq!(clock.now(), anchor() + TimeDelta::milliseconds(1500));
    }

    #[tokio::test(start_paused = true)]
    async fn the_monotonic_clock_never_goes_back() {
        let clock = MonotonicClock::anchored(anchor());
        let mut last = clock.now();

        for _ in 0..5 {
            tokio::time::advance(Duration::from_millis(250)).await;
            let now = clock.now();
            assert!(now > last, "{now} after {last}");
            last = now;
        }
    }

    #[tokio::test]
    async fn the_monotonic_clock_starts_at_the_wall_clocks_time() {
        let before = SystemClock.now();

        let started = MonotonicClock::start().now();

        let after = SystemClock.now();
        assert!(
            started >= before && started <= after,
            "{before} {started} {after}"
        );
    }

    #[test]
    fn the_system_clock_reads_a_current_time_in_milliseconds() {
        let now = SystemClock.now();

        assert!(now.year() >= 2026, "{now}");
        assert_eq!(now.nanosecond() % 1_000_000, 0, "{now}");
    }

    #[test]
    fn os_random_fills_the_whole_buffer() {
        let mut first = [0_u8; 32];
        let mut second = [0_u8; 32];

        OsRandom.fill(&mut first).unwrap();
        OsRandom.fill(&mut second).unwrap();

        // Two all-zero or equal 32-byte draws are practically impossible.
        assert_ne!(first, [0; 32]);
        assert_ne!(first, second);
    }

    #[test]
    fn an_os_error_keeps_its_number() {
        assert_eq!(random_error(Some(5)), RandomError::Os { code: 5 });
    }

    #[tokio::test(start_paused = true)]
    async fn the_real_pace_waits_its_whole_duration() {
        let start = Instant::now();

        TokioPace.wait(Duration::from_secs(5)).await;

        assert_eq!(start.elapsed(), Duration::from_secs(5));
    }

    #[tokio::test(start_paused = true)]
    async fn a_dropped_wait_ends_before_its_duration() {
        let start = Instant::now();

        let waited = tokio::time::timeout(
            Duration::from_secs(1),
            TokioPace.wait(Duration::from_secs(60)),
        )
        .await;

        assert!(waited.is_err());
        assert_eq!(start.elapsed(), Duration::from_secs(1));
    }

    #[test]
    fn an_error_without_a_number_is_unavailable() {
        assert_eq!(random_error(None), RandomError::Unavailable);
    }
}
