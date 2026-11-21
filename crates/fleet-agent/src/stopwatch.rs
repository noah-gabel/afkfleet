//! [`OsStopwatch`]: the OS's monotonic clock as the stopwatch a script's
//! hook is timed by (ADR-0040's amendment, ADR-0043).
//!
//! A hook of a scripted mode is bounded in time: 5 ms a hook, 20 ms of a
//! second for one script. Neither fleet-script nor fleet-runtime reads a
//! clock of the OS's, so the agent, which runs the scripts, hands the
//! fleet this one. Tests hand it fleet-testkit's `ManualStopwatch`.

use core::time::Duration;
use std::time::Instant;

use fleet_core::script::Stopwatch;

/// A stopwatch on the OS's monotonic clock, running since it was started.
/// It never goes back, and a reading is one clock read.
#[derive(Debug, Clone, Copy)]
pub struct OsStopwatch {
    started: Instant,
}

impl OsStopwatch {
    /// Starts a stopwatch now.
    #[must_use]
    pub fn start() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl Stopwatch for OsStopwatch {
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_runs_and_never_goes_back() {
        let stopwatch = OsStopwatch::start();

        let mut last = stopwatch.elapsed();
        for _ in 0..1_000 {
            let now = stopwatch.elapsed();
            assert!(now >= last);
            last = now;
        }
    }

    #[test]
    fn a_copy_reads_the_same_time() {
        let stopwatch = OsStopwatch::start();
        let copy = stopwatch;

        let first = stopwatch.elapsed();
        let second = copy.elapsed();

        assert!(second >= first);
    }
}
