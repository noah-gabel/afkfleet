//! [`CircuitBreaker`]: stops hammering a server that keeps failing.

use core::num::NonZeroUsize;
use core::time::Duration;

use chrono::{DateTime, Utc};

use super::FailureWindow;
use crate::time;

/// Why a [`CircuitPolicy`] can't be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CircuitPolicyError {
    /// The failure window is zero.
    #[error("the failure window must be longer than zero")]
    ZeroWindow,
    /// The cool-down is zero.
    #[error("the cool-down must be longer than zero")]
    ZeroCooldown,
}

/// When a [`CircuitBreaker`] opens, and for how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CircuitPolicy {
    threshold: NonZeroUsize,
    window: Duration,
    cooldown: Duration,
}

impl CircuitPolicy {
    /// Builds a policy: open after `threshold` failures within `window`, and
    /// stay open for `cooldown`.
    ///
    /// # Errors
    /// - [`CircuitPolicyError::ZeroWindow`] if `window` is zero.
    /// - [`CircuitPolicyError::ZeroCooldown`] if `cooldown` is zero.
    pub fn try_new(
        threshold: NonZeroUsize,
        window: Duration,
        cooldown: Duration,
    ) -> Result<Self, CircuitPolicyError> {
        if window.is_zero() {
            return Err(CircuitPolicyError::ZeroWindow);
        }
        if cooldown.is_zero() {
            return Err(CircuitPolicyError::ZeroCooldown);
        }
        Ok(Self {
            threshold,
            window,
            cooldown,
        })
    }
}

/// The breaker's state at a point in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Attempts are allowed.
    Closed,
    /// No attempts until `until`.
    Open {
        /// When the cool-down ends.
        until: DateTime<Utc>,
    },
    /// The cool-down is over: one attempt is allowed, and its outcome closes
    /// or re-opens the breaker.
    HalfOpen,
}

/// A circuit breaker for one bot (Plan.md §6, row 4; ADR-0010).
///
/// It opens after too many failures within the policy's window and then lets
/// no attempt through until the cool-down is over. After that it's half open:
/// the next attempt decides. A success closes it; a failure opens it again for
/// a fresh cool-down. The caller reports outcomes; the breaker never waits
/// for a result, so it can't get stuck half open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CircuitBreaker {
    policy: CircuitPolicy,
    failures: FailureWindow,
    open_until: Option<DateTime<Utc>>,
}

impl CircuitBreaker {
    /// Creates a closed breaker.
    #[must_use]
    pub fn new(policy: CircuitPolicy) -> Self {
        Self {
            policy,
            failures: FailureWindow::new(policy.threshold, policy.window),
            open_until: None,
        }
    }

    /// Records a failed attempt at `now`.
    pub fn record_failure(&mut self, now: DateTime<Utc>) {
        // While open, a failure (normally the half-open attempt) starts a fresh
        // cool-down; while closed, it counts towards the threshold.
        if self.open_until.is_some() || self.failures.record(now) {
            self.failures.clear();
            self.open_until = Some(time::add(now, self.policy.cooldown));
        }
    }

    /// Records a successful attempt: the breaker closes and forgets earlier
    /// failures.
    pub fn record_success(&mut self) {
        self.failures.clear();
        self.open_until = None;
    }

    /// Whether an attempt may start at `now`.
    #[must_use]
    pub fn permits(&self, now: DateTime<Utc>) -> bool {
        self.open_until.is_none_or(|until| now >= until)
    }

    /// Returns when the current cool-down ends, if the breaker has opened and
    /// hasn't closed since.
    #[must_use]
    pub const fn open_until(&self) -> Option<DateTime<Utc>> {
        self.open_until
    }

    /// Returns the state at `now`.
    #[must_use]
    pub fn state(&self, now: DateTime<Utc>) -> CircuitState {
        match self.open_until {
            None => CircuitState::Closed,
            Some(until) if now < until => CircuitState::Open { until },
            Some(_) => CircuitState::HalfOpen,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    /// 3 failures within 600 s open the breaker for 900 s.
    fn policy() -> CircuitPolicy {
        CircuitPolicy::try_new(
            NonZeroUsize::new(3).unwrap(),
            Duration::from_mins(10),
            Duration::from_mins(15),
        )
        .unwrap()
    }

    /// A breaker opened by failures at 0, 1 and 2 s, so open until 902 s.
    fn opened() -> CircuitBreaker {
        let mut breaker = CircuitBreaker::new(policy());
        for second in 0..3 {
            breaker.record_failure(at(second));
        }
        breaker
    }

    #[test]
    fn try_new_rejects_a_zero_window() {
        assert_eq!(
            CircuitPolicy::try_new(NonZeroUsize::MIN, Duration::ZERO, Duration::from_secs(1)),
            Err(CircuitPolicyError::ZeroWindow)
        );
    }

    #[test]
    fn try_new_rejects_a_zero_cooldown() {
        assert_eq!(
            CircuitPolicy::try_new(NonZeroUsize::MIN, Duration::from_secs(1), Duration::ZERO),
            Err(CircuitPolicyError::ZeroCooldown)
        );
    }

    #[test]
    fn starts_closed() {
        let breaker = CircuitBreaker::new(policy());

        assert!(breaker.permits(at(0)));
        assert_eq!(breaker.state(at(0)), CircuitState::Closed);
        assert_eq!(breaker.open_until(), None);
    }

    #[test]
    fn stays_closed_below_the_threshold() {
        let mut breaker = CircuitBreaker::new(policy());
        breaker.record_failure(at(0));
        breaker.record_failure(at(1));

        assert!(breaker.permits(at(2)));
    }

    #[test]
    fn opens_at_the_threshold_for_the_cooldown() {
        let breaker = opened();

        assert_eq!(breaker.open_until(), Some(at(902)));
        assert_eq!(
            breaker.state(at(500)),
            CircuitState::Open { until: at(902) }
        );
        assert!(!breaker.permits(at(2)));
        assert!(!breaker.permits(at(901)));
    }

    #[test]
    fn half_opens_when_the_cooldown_ends() {
        let breaker = opened();

        assert!(breaker.permits(at(902)));
        assert_eq!(breaker.state(at(902)), CircuitState::HalfOpen);
    }

    #[test]
    fn closes_after_a_successful_attempt() {
        let mut breaker = opened();
        breaker.record_success();

        assert_eq!(breaker.state(at(903)), CircuitState::Closed);
        assert_eq!(breaker.open_until(), None);
    }

    #[test]
    fn reopens_after_a_failed_half_open_attempt() {
        let mut breaker = opened();
        breaker.record_failure(at(1000));

        assert_eq!(
            breaker.state(at(1000)),
            CircuitState::Open { until: at(1900) }
        );
        assert!(!breaker.permits(at(1899)));
    }

    #[test]
    fn success_forgets_earlier_failures() {
        let mut breaker = CircuitBreaker::new(policy());
        breaker.record_failure(at(0));
        breaker.record_failure(at(1));
        breaker.record_success();
        breaker.record_failure(at(2));
        breaker.record_failure(at(3));

        assert!(breaker.permits(at(4)));
    }

    #[test]
    fn failures_spread_over_more_than_the_window_dont_open_it() {
        let mut breaker = CircuitBreaker::new(policy());
        breaker.record_failure(at(0));
        breaker.record_failure(at(400));
        breaker.record_failure(at(800));

        assert!(breaker.permits(at(800)));
    }

    #[derive(Debug, Clone)]
    enum Op {
        Advance(u16),
        Failure,
        Success,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0..=1_200_u16).prop_map(Op::Advance),
            Just(Op::Failure),
            Just(Op::Success),
        ]
    }

    proptest! {
        #[test]
        fn never_permits_while_open(ops in proptest::collection::vec(op(), 0..200)) {
            let mut breaker = CircuitBreaker::new(policy());
            let mut now = at(0);
            for op in ops {
                match op {
                    Op::Advance(secs) => now = time::add(now, Duration::from_secs(secs.into())),
                    Op::Failure => breaker.record_failure(now),
                    Op::Success => breaker.record_success(),
                }
                let open = breaker.open_until().is_some_and(|until| now < until);
                prop_assert_eq!(breaker.permits(now), !open);
                prop_assert_eq!(
                    matches!(breaker.state(now), CircuitState::Open { .. }),
                    open
                );
            }
        }
    }
}
