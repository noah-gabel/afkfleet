//! [`ManualStopwatch`]: a test double for fleet-core's [`Stopwatch`], the
//! time a script's hook is measured by (ADR-0040's amendment).

use core::time::Duration;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use fleet_core::script::Stopwatch;

/// A [`Stopwatch`] that moves only as the test says: by a fixed step at
/// each reading, and when the test moves it. Clones share one time.
///
/// **A step is how a test says how long a script's work takes.** A running
/// hook reads its stopwatch every few operations, so with a step of
/// 100 µs a hook "takes" 100 µs for each of those readings, on any machine
/// and in any build. With no step, time stands still and no hook ever
/// meets a time limit.
#[derive(Debug, Clone, Default)]
pub struct ManualStopwatch {
    state: Arc<Mutex<State>>,
}

#[derive(Debug, Default)]
struct State {
    elapsed: Duration,
    step: Duration,
    readings: u64,
}

impl ManualStopwatch {
    /// A stopwatch that stands still until the test moves it.
    #[must_use]
    pub fn standing() -> Self {
        Self::default()
    }

    /// A stopwatch that moves on by `step` at each reading, before it
    /// answers: its first reading is `step`.
    #[must_use]
    pub fn stepping(step: Duration) -> Self {
        let stopwatch = Self::default();
        stopwatch.set_step(step);
        stopwatch
    }

    /// From now on, each reading moves the stopwatch on by `step`.
    pub fn set_step(&self, step: Duration) {
        self.lock().step = step;
    }

    /// Moves the stopwatch on by `by`. It stops at the longest time it can
    /// show instead of overflowing.
    pub fn advance(&self, by: Duration) {
        let mut state = self.lock();
        state.elapsed = state.elapsed.saturating_add(by);
    }

    /// Moves the stopwatch **back** by `by`, which a real one never does:
    /// for the test that a broken one frees nothing.
    pub fn rewind(&self, by: Duration) {
        let mut state = self.lock();
        state.elapsed = state.elapsed.saturating_sub(by);
    }

    /// What the stopwatch shows, without counting as a reading and without
    /// a step.
    #[must_use]
    pub fn shown(&self) -> Duration {
        self.lock().elapsed
    }

    /// How often it was read through [`Stopwatch::elapsed`].
    #[must_use]
    pub fn readings(&self) -> u64 {
        self.lock().readings
    }

    /// Locks the state. A test that panicked while holding the lock leaves
    /// consistent data behind, so a poisoned lock is used as it is.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Stopwatch for ManualStopwatch {
    fn elapsed(&self) -> Duration {
        let mut state = self.lock();
        state.elapsed = state.elapsed.saturating_add(state.step);
        state.readings = state.readings.saturating_add(1);
        state.elapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn a_standing_stopwatch_shows_nothing_however_often_it_is_read() {
        let stopwatch = ManualStopwatch::standing();

        assert_eq!(stopwatch.elapsed(), Duration::ZERO);
        assert_eq!(stopwatch.elapsed(), Duration::ZERO);
        assert_eq!(stopwatch.readings(), 2);
    }

    #[test]
    fn a_stepping_stopwatch_moves_on_at_each_reading() {
        let stopwatch = ManualStopwatch::stepping(MS);

        assert_eq!(stopwatch.elapsed(), MS);
        assert_eq!(stopwatch.elapsed(), 2 * MS);
        assert_eq!(stopwatch.shown(), 2 * MS);
        assert_eq!(stopwatch.readings(), 2);

        stopwatch.set_step(Duration::ZERO);
        assert_eq!(stopwatch.elapsed(), 2 * MS);
    }

    #[test]
    fn the_test_moves_it_on_and_back() {
        let stopwatch = ManualStopwatch::standing();

        stopwatch.advance(5 * MS);
        assert_eq!(stopwatch.elapsed(), 5 * MS);
        stopwatch.rewind(2 * MS);
        assert_eq!(stopwatch.elapsed(), 3 * MS);
        stopwatch.rewind(9 * MS);
        assert_eq!(stopwatch.shown(), Duration::ZERO);
        assert_eq!(stopwatch.readings(), 2);
    }

    #[test]
    fn it_stops_at_the_longest_time_instead_of_overflowing() {
        let stopwatch = ManualStopwatch::stepping(Duration::MAX);

        assert_eq!(stopwatch.elapsed(), Duration::MAX);
        assert_eq!(stopwatch.elapsed(), Duration::MAX);
        stopwatch.advance(MS);
        assert_eq!(stopwatch.shown(), Duration::MAX);
    }

    #[test]
    fn clones_share_one_time() {
        let stopwatch = ManualStopwatch::stepping(MS);
        let clone = stopwatch.clone();

        let _ = stopwatch.elapsed();
        clone.advance(MS);

        assert_eq!(stopwatch.shown(), 2 * MS);
        assert_eq!(clone.readings(), 1);
    }
}
