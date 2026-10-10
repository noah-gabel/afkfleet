//! [`SystemClock`] and [`OsRandom`]: the server's real [`Clock`] and
//! [`SecureRandom`] (ADR-0015).
//!
//! They're the only code in fleet-server that reads the wall clock or the
//! OS's randomness: `clippy.toml` bans every direct read, and each of the two
//! carries the one approved exception. Everything else gets time and random
//! bytes from the ports, so tests can control both.

use chrono::{DateTime, SubsecRound, Utc};
use fleet_core::system::{Clock, RandomError, SecureRandom};

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

/// Maps getrandom's error, which may carry the OS's error number.
fn random_error(raw_os_error: Option<i32>) -> RandomError {
    raw_os_error.map_or(RandomError::Unavailable, |code| RandomError::Os { code })
}

#[cfg(test)]
mod tests {
    use chrono::{Datelike, Timelike};

    use super::*;

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

    #[test]
    fn an_error_without_a_number_is_unavailable() {
        assert_eq!(random_error(None), RandomError::Unavailable);
    }
}
