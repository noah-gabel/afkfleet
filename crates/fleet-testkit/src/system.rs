//! [`ManualClock`] and [`SeededRandom`]: test doubles for fleet-core's
//! [`Clock`] and [`SecureRandom`] ports (ADR-0015).

use core::time::Duration;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, SubsecRound, Utc};
use fleet_core::system::{Clock, RandomError, SecureRandom};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// A [`Clock`] that only moves when the test moves it (Plan.md P6, ADR-0015).
/// Clones share the same time.
///
/// Like every [`Clock`], it reports whole milliseconds: [`ManualClock::new`],
/// [`ManualClock::set`] and [`ManualClock::advance`] all truncate.
#[derive(Debug, Clone)]
pub struct ManualClock {
    now: Arc<Mutex<DateTime<Utc>>>,
}

impl ManualClock {
    /// Creates a clock that reads `start`, truncated to milliseconds.
    #[must_use]
    pub fn new(start: DateTime<Utc>) -> Self {
        Self {
            now: Arc::new(Mutex::new(start.trunc_subsecs(3))),
        }
    }

    /// Jumps to `at`, truncated to milliseconds, forwards or backwards.
    pub fn set(&self, at: DateTime<Utc>) {
        *self.lock() = at.trunc_subsecs(3);
    }

    /// Moves the time forward by `by`, truncated to milliseconds, and stops
    /// at the latest representable time instead of overflowing.
    pub fn advance(&self, by: Duration) {
        let mut now = self.lock();
        *now = fleet_core::time::add(*now, by).trunc_subsecs(3);
    }

    /// Locks the time. A test that panicked while holding the lock leaves
    /// consistent data behind, so a poisoned lock is used as it is.
    fn lock(&self) -> MutexGuard<'_, DateTime<Utc>> {
        self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self.lock()
    }
}

/// A seeded, deterministic [`SecureRandom`] (Plan.md P6, ADR-0015). Clones
/// share one stream.
///
/// **It isn't secure at all**: anyone who knows the seed knows every byte.
/// It exists only here, in a crate that is never more than a
/// dev-dependency (`just testkit-check` enforces that), so it can't be
/// linked into a binary.
///
/// Tests compare its output with itself ("the same seed gives the same
/// bytes") and never hard-code it: rand doesn't promise `StdRng`'s output
/// across versions.
#[derive(Debug, Clone)]
pub struct SeededRandom {
    state: Arc<Mutex<RandomState>>,
}

#[derive(Debug)]
struct RandomState {
    rng: StdRng,
    failures: VecDeque<RandomError>,
}

impl SeededRandom {
    /// Creates a source whose bytes depend only on `seed`.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(RandomState {
                rng: StdRng::seed_from_u64(seed),
                failures: VecDeque::new(),
            })),
        }
    }

    /// Makes a later [`SecureRandom::fill`] fail with `error`. Queued
    /// failures are used in order, one per fill, and a failed fill doesn't
    /// move the stream.
    pub fn fail_next(&self, error: RandomError) {
        self.lock().failures.push_back(error);
    }

    /// Locks the state. A test that panicked while holding the lock leaves
    /// consistent data behind, so a poisoned lock is used as it is.
    fn lock(&self) -> MutexGuard<'_, RandomState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl SecureRandom for SeededRandom {
    fn fill(&self, dest: &mut [u8]) -> Result<(), RandomError> {
        let mut state = self.lock();
        if let Some(error) = state.failures.pop_front() {
            return Err(error);
        }
        state.rng.fill_bytes(dest);
        Ok(())
    }
}
