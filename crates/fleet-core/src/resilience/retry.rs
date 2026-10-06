//! [`RetryPolicy`]: how long to wait before the next connection attempt.

use core::num::NonZeroU32;
use core::time::Duration;

use backon::{BackoffBuilder, ExponentialBuilder};
use chrono::{DateTime, Utc};
use rand::Rng;

use crate::time;

/// Why a [`RetryPolicy`] can't be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RetryPolicyError {
    /// The base delay is zero.
    #[error("the base delay must be longer than zero")]
    ZeroBase,
    /// The maximum delay is less than twice the base delay.
    #[error("the maximum delay must be at least twice the base delay")]
    MaxTooSmall,
    /// The stable-online period is zero.
    #[error("the stable-online period must be longer than zero")]
    ZeroStableAfter,
}

/// Exponential backoff with jitter, and the stable-online period after which
/// the attempt counter starts over (Plan.md §6).
///
/// The delay before attempt `n` starts at the base delay and doubles with
/// every attempt. backon computes it and adds a random jitter of up to 100 %
/// *after* capping it, so backon gets half the configured maximum: every delay
/// then stays within the maximum (ADR-0010). With the defaults (5 s, 300 s) the
/// first delay is 5–10 s and later ones are 150–300 s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    base: Duration,
    max: Duration,
    stable_after: Duration,
}

/// From this attempt on, the delay has reached its plateau for any policy:
/// doubling 63 times covers every `Duration`.
const MAX_STEPS: u32 = 64;

impl RetryPolicy {
    /// How much the delay grows from one attempt to the next.
    pub const FACTOR: f32 = 2.0;

    /// Builds a policy.
    ///
    /// # Errors
    /// - [`RetryPolicyError::ZeroBase`] if `base` is zero.
    /// - [`RetryPolicyError::MaxTooSmall`] if `max` is less than twice `base`.
    /// - [`RetryPolicyError::ZeroStableAfter`] if `stable_after` is zero.
    pub fn try_new(
        base: Duration,
        max: Duration,
        stable_after: Duration,
    ) -> Result<Self, RetryPolicyError> {
        if base.is_zero() {
            return Err(RetryPolicyError::ZeroBase);
        }
        if base.checked_mul(2).is_none_or(|twice| max < twice) {
            return Err(RetryPolicyError::MaxTooSmall);
        }
        if stable_after.is_zero() {
            return Err(RetryPolicyError::ZeroStableAfter);
        }
        Ok(Self {
            base,
            max,
            stable_after,
        })
    }

    /// Returns the base delay.
    #[must_use]
    pub const fn base(&self) -> Duration {
        self.base
    }

    /// Returns the maximum delay.
    #[must_use]
    pub const fn max(&self) -> Duration {
        self.max
    }

    /// Returns the stable-online period.
    #[must_use]
    pub const fn stable_after(&self) -> Duration {
        self.stable_after
    }

    /// Returns the delay before attempt `attempt` (the first attempt is 1).
    ///
    /// The jitter is seeded from `rng`, so a seeded RNG gives the same delay
    /// every time. The result always lies within [`RetryPolicy::bounds`].
    #[must_use]
    pub fn delay<R: Rng + ?Sized>(&self, attempt: NonZeroU32, rng: &mut R) -> Duration {
        let jittered = self.nth_delay(
            self.builder()
                .with_jitter()
                .with_jitter_seed(rng.next_u64()),
            attempt,
        );
        // backon computes in f32; this only absorbs its rounding.
        let (lower, upper) = self.bounds(attempt);
        jittered.max(lower).min(upper)
    }

    /// Returns the shortest and the longest delay before attempt `attempt`:
    /// the delay without jitter, and twice that, but no more than the maximum.
    #[must_use]
    pub fn bounds(&self, attempt: NonZeroU32) -> (Duration, Duration) {
        let lower = self.nth_delay(self.builder(), attempt);
        (lower, lower.saturating_mul(2).min(self.max))
    }

    /// Whether a bot that has been online since `online_since` counts as
    /// stable at `now`, so its attempt counter starts over.
    #[must_use]
    pub fn is_stable(&self, online_since: DateTime<Utc>, now: DateTime<Utc>) -> bool {
        time::elapsed(online_since, now) >= self.stable_after
    }

    /// The backoff schedule without jitter. backon adds the jitter after
    /// capping, so it gets half the maximum.
    fn builder(&self) -> ExponentialBuilder {
        ExponentialBuilder::new()
            .with_min_delay(self.base)
            .with_max_delay(self.max / 2)
            .with_factor(Self::FACTOR)
            .without_max_times()
    }

    /// The delay `builder` produces for `attempt`.
    fn nth_delay(&self, builder: ExponentialBuilder, attempt: NonZeroU32) -> Duration {
        let index = attempt.get().min(MAX_STEPS) - 1;
        builder
            .build()
            .nth(usize::try_from(index).unwrap_or(0))
            // Unreachable: without a maximum number of attempts backon never ends.
            .unwrap_or(self.max / 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use rstest::rstest;

    const fn secs(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    fn attempt(n: u32) -> NonZeroU32 {
        NonZeroU32::new(n).unwrap()
    }

    fn defaults() -> RetryPolicy {
        RetryPolicy::try_new(secs(5), secs(300), secs(300)).unwrap()
    }

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    #[rstest]
    #[case::zero_base(secs(0), secs(300), secs(300), RetryPolicyError::ZeroBase)]
    #[case::max_below_twice_the_base(secs(5), secs(9), secs(300), RetryPolicyError::MaxTooSmall)]
    #[case::twice_the_base_overflows(
        Duration::MAX,
        Duration::MAX,
        secs(300),
        RetryPolicyError::MaxTooSmall
    )]
    #[case::zero_stable_after(secs(5), secs(300), secs(0), RetryPolicyError::ZeroStableAfter)]
    fn try_new_rejects(
        #[case] base: Duration,
        #[case] max: Duration,
        #[case] stable_after: Duration,
        #[case] error: RetryPolicyError,
    ) {
        assert_eq!(RetryPolicy::try_new(base, max, stable_after), Err(error));
    }

    #[test]
    fn try_new_accepts_a_max_of_exactly_twice_the_base() {
        let policy = RetryPolicy::try_new(secs(5), secs(10), secs(1)).unwrap();
        assert_eq!(
            (policy.base(), policy.max(), policy.stable_after()),
            (secs(5), secs(10), secs(1))
        );
    }

    #[rstest]
    #[case::first(1, secs(5), secs(10))]
    #[case::second(2, secs(10), secs(20))]
    #[case::fifth(5, secs(80), secs(160))]
    #[case::capped(6, secs(150), secs(300))]
    #[case::stays_capped(1000, secs(150), secs(300))]
    #[case::last_attempt(u32::MAX, secs(150), secs(300))]
    fn delays_grow_then_plateau(#[case] n: u32, #[case] lower: Duration, #[case] upper: Duration) {
        assert_eq!(defaults().bounds(attempt(n)), (lower, upper));
    }

    #[rstest]
    #[case::first(1)]
    #[case::fifth(5)]
    #[case::plateau(6)]
    #[case::last_attempt(u32::MAX)]
    fn delay_is_within_its_bounds(#[case] n: u32) {
        let policy = defaults();
        let mut rng = StdRng::seed_from_u64(7);

        let (lower, upper) = policy.bounds(attempt(n));
        let delay = policy.delay(attempt(n), &mut rng);
        assert!(
            lower <= delay && delay <= upper,
            "{delay:?} not in {lower:?}..={upper:?}"
        );
    }

    #[test]
    fn delay_is_jittered() {
        let policy = defaults();
        let mut rng = StdRng::seed_from_u64(1);

        let delays: Vec<_> = (0..20)
            .map(|_| policy.delay(attempt(6), &mut rng))
            .collect();
        assert!(
            delays.iter().any(|&d| d != delays[0]),
            "no jitter in {delays:?}"
        );
    }

    #[test]
    fn the_same_seed_gives_the_same_delay() {
        let policy = defaults();
        let first = policy.delay(attempt(3), &mut StdRng::seed_from_u64(42));
        let second = policy.delay(attempt(3), &mut StdRng::seed_from_u64(42));
        assert_eq!(first, second);
    }

    #[rstest]
    #[case::not_yet(299, false)]
    #[case::exactly_at_the_period(300, true)]
    #[case::later(1000, true)]
    #[case::clock_went_backwards(-10, false)]
    fn is_stable_after_the_period(#[case] online_for: i64, #[case] stable: bool) {
        assert_eq!(
            defaults().is_stable(at(1_000), at(1_000 + online_for)),
            stable
        );
    }

    proptest! {
        #[test]
        fn every_delay_is_within_bounds_and_the_maximum(
            base_ms in 1..=60_000_u64,
            extra_ms in 0..=600_000_u64,
            n in 1..=u32::MAX,
            seed in any::<u64>(),
        ) {
            let base = Duration::from_millis(base_ms);
            let max = Duration::from_millis(base_ms * 2 + extra_ms);
            let policy = RetryPolicy::try_new(base, max, secs(300)).unwrap();

            let (lower, upper) = policy.bounds(attempt(n));
            let delay = policy.delay(attempt(n), &mut StdRng::seed_from_u64(seed));
            prop_assert!(lower <= delay && delay <= upper);
            prop_assert!(delay <= max);
            prop_assert!(lower >= base);
        }
    }
}
