//! The per-username lockout (Plan.md P7.10, §7.4, ADR-0016): when a
//! username's failed logins lock it, for how long, and when its failure
//! streak is forgotten.
//!
//! - **The threshold's failure locks** ([`LockoutPolicy`], 5 by default):
//!   for [`FIRST_LOCK`], and each further failure doubles the lock, capped
//!   at [`MAX_LOCK`].
//! - **A streak expires** [`FAILURE_MEMORY`] after its last failure, once no
//!   lock runs: the next failure starts a new one. The server also clears
//!   it at a successful login.
//! - **While a lock runs** ([`check_lock`]), a login is refused before any
//!   password check, and the refusal isn't a failure, so it can't extend
//!   the lock.
//!
//! The rule treats every name alike: the server applies it to names that
//! belong to no user exactly as to real ones, so the lockout can't tell
//! anyone which names exist. A lock ends from its exact millisecond on.

use core::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};

use crate::time::{add, elapsed};

/// How long the threshold's failure locks a username.
pub const FIRST_LOCK: Duration = Duration::from_mins(1);

/// The longest lock: each failure past the threshold doubles the lock up to
/// this.
pub const MAX_LOCK: Duration = Duration::from_hours(1);

/// How long after its last failure a streak is forgotten, once no lock
/// runs.
pub const FAILURE_MEMORY: Duration = Duration::from_hours(24);

/// How many failures lock a username (`[auth] lockout_threshold`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockoutPolicy {
    threshold: u32,
}

/// Why a threshold was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the lockout threshold must be between {min} and {max}")]
pub struct LockoutPolicyError {
    /// [`LockoutPolicy::MIN_THRESHOLD`].
    pub min: u32,
    /// [`LockoutPolicy::MAX_THRESHOLD`].
    pub max: u32,
}

impl LockoutPolicy {
    /// The lowest threshold: fewer would lock people who mistype twice.
    pub const MIN_THRESHOLD: u32 = 3;
    /// The highest threshold.
    pub const MAX_THRESHOLD: u32 = 20;

    /// Checks `threshold` against its range.
    ///
    /// # Errors
    /// [`LockoutPolicyError`] outside
    /// [`LockoutPolicy::MIN_THRESHOLD`]..=[`LockoutPolicy::MAX_THRESHOLD`].
    pub const fn try_new(threshold: u32) -> Result<Self, LockoutPolicyError> {
        if threshold < Self::MIN_THRESHOLD || threshold > Self::MAX_THRESHOLD {
            return Err(LockoutPolicyError {
                min: Self::MIN_THRESHOLD,
                max: Self::MAX_THRESHOLD,
            });
        }
        Ok(Self { threshold })
    }

    /// How many failures lock a username.
    #[must_use]
    pub const fn threshold(&self) -> u32 {
        self.threshold
    }

    /// How long the `failures`-th failure of a streak locks its username:
    /// none below the threshold, [`FIRST_LOCK`] at it, doubling with each
    /// further failure up to [`MAX_LOCK`].
    #[must_use]
    pub fn lock_for(&self, failures: u32) -> Option<Duration> {
        let doublings = failures.checked_sub(self.threshold)?;
        let lock = 2_u32
            .checked_pow(doublings)
            .and_then(|factor| FIRST_LOCK.checked_mul(factor))
            .map_or(MAX_LOCK, |lock| lock.min(MAX_LOCK));
        Some(lock)
    }
}

impl Default for LockoutPolicy {
    /// §7.4's: 5 failures.
    fn default() -> Self {
        Self { threshold: 5 }
    }
}

/// A username's failure streak, as the rule sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailureStreak {
    /// The failures of the streak, at least 1.
    pub failures: u32,
    /// The streak's last failure.
    pub last_failure_at: DateTime<Utc>,
    /// When the running lock ends, once the streak reached the threshold.
    pub locked_until: Option<DateTime<Utc>>,
}

/// A running lock: a login is refused until it ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Locked {
    /// When it ends.
    pub until: DateTime<Utc>,
    /// How long it still runs.
    pub remaining: Duration,
}

/// What a failure made of a streak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailureOutcome {
    /// The streak with this failure, to store.
    pub streak: FailureStreak,
    /// When the lock this failure started ends, if it started one.
    pub lock_started: Option<DateTime<Utc>>,
}

/// Whether a login for `streak`'s username may be checked at `now`.
///
/// # Errors
/// [`Locked`] while its lock runs: until `locked_until`, exclusive.
pub fn check_lock(streak: Option<&FailureStreak>, now: DateTime<Utc>) -> Result<(), Locked> {
    match streak.and_then(|streak| streak.locked_until) {
        Some(until) if until > now => Err(Locked {
            until,
            remaining: elapsed(now, until),
        }),
        _ => Ok(()),
    }
}

/// Whether `streak` is forgotten at `now`: its last failure is at least
/// [`FAILURE_MEMORY`] old, and no lock runs. A last failure in the future (a
/// clock that went back) isn't old.
#[must_use]
pub fn is_expired(streak: &FailureStreak, now: DateTime<Utc>) -> bool {
    streak.last_failure_at <= now
        && elapsed(streak.last_failure_at, now) >= FAILURE_MEMORY
        && check_lock(Some(streak), now).is_ok()
}

/// The latest last failure a streak can have and be expired at `now`, for
/// the purge, which also skips streaks with a running lock.
#[must_use]
pub fn expired_failed_by(now: DateTime<Utc>) -> DateTime<Utc> {
    TimeDelta::from_std(FAILURE_MEMORY)
        .ok()
        .and_then(|memory| now.checked_sub_signed(memory))
        .unwrap_or(DateTime::<Utc>::MIN_UTC)
}

/// What a failed login at `now` makes of `streak`: an expired streak counts
/// as none, so the failure starts a new one; the count saturates. From the
/// threshold on, the failure starts a lock of
/// [`LockoutPolicy::lock_for`].
#[must_use]
pub fn after_failure(
    policy: &LockoutPolicy,
    streak: Option<&FailureStreak>,
    now: DateTime<Utc>,
) -> FailureOutcome {
    let failures = streak
        .filter(|streak| !is_expired(streak, now))
        .map_or(1, |streak| streak.failures.saturating_add(1));
    let lock_started = policy.lock_for(failures).map(|lock| add(now, lock));
    FailureOutcome {
        streak: FailureStreak {
            failures,
            last_failure_at: now,
            locked_until: lock_started,
        },
        lock_started,
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    /// The start of every test, in milliseconds.
    const T0: i64 = 1_800_000_000_000;

    fn at(millis: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(millis).unwrap()
    }

    /// `T0` plus `secs` seconds and `millis` milliseconds.
    fn after(secs: i64, millis: i64) -> DateTime<Utc> {
        at(T0 + secs * 1000 + millis)
    }

    fn streak(
        failures: u32,
        last: DateTime<Utc>,
        locked_until: Option<DateTime<Utc>>,
    ) -> FailureStreak {
        FailureStreak {
            failures,
            last_failure_at: last,
            locked_until,
        }
    }

    /// `n` failures one second apart from `T0`, each recorded right after
    /// the previous one; the outcome of the last.
    fn failures(policy: LockoutPolicy, n: u32) -> FailureOutcome {
        let mut current: Option<FailureStreak> = None;
        let mut outcome = None;
        for i in 0..n {
            let now = after(i64::from(i), 0);
            let next = after_failure(&policy, current.as_ref(), now);
            current = Some(next.streak);
            outcome = Some(next);
        }
        outcome.unwrap()
    }

    #[test]
    fn the_default_threshold_is_five() {
        assert_eq!(LockoutPolicy::default().threshold(), 5);
        assert_eq!(LockoutPolicy::try_new(5), Ok(LockoutPolicy::default()));
    }

    #[rstest]
    #[case::below(2, false)]
    #[case::lowest(3, true)]
    #[case::highest(20, true)]
    #[case::above(21, false)]
    #[case::zero(0, false)]
    fn the_threshold_is_3_to_20(#[case] threshold: u32, #[case] valid: bool) {
        let result = LockoutPolicy::try_new(threshold);

        assert_eq!(result.is_ok(), valid, "{result:?}");
        if !valid {
            assert_eq!(result, Err(LockoutPolicyError { min: 3, max: 20 }));
        }
    }

    #[rstest]
    #[case::first(1, None)]
    #[case::fourth(4, None)]
    #[case::fifth(5, Some(60))]
    #[case::sixth(6, Some(120))]
    #[case::seventh(7, Some(240))]
    #[case::eighth(8, Some(480))]
    #[case::ninth(9, Some(960))]
    #[case::tenth(10, Some(1920))]
    #[case::eleventh_is_capped(11, Some(3600))]
    #[case::twelfth(12, Some(3600))]
    #[case::far_past_the_cap(40, Some(3600))]
    #[case::the_most(u32::MAX, Some(3600))]
    fn the_lock_doubles_from_a_minute_up_to_an_hour(
        #[case] failures: u32,
        #[case] seconds: Option<u64>,
    ) {
        assert_eq!(
            LockoutPolicy::default().lock_for(failures),
            seconds.map(Duration::from_secs)
        );
    }

    #[test]
    fn the_threshold_moves_the_first_lock() {
        let policy = LockoutPolicy::try_new(3).unwrap();

        assert_eq!(policy.lock_for(2), None);
        assert_eq!(policy.lock_for(3), Some(FIRST_LOCK));
        assert_eq!(policy.lock_for(4), Some(FIRST_LOCK * 2));
    }

    #[test]
    fn a_first_failure_starts_a_streak_without_a_lock() {
        let outcome = after_failure(&LockoutPolicy::default(), None, at(T0));

        assert_eq!(
            outcome,
            FailureOutcome {
                streak: streak(1, at(T0), None),
                lock_started: None,
            }
        );
    }

    #[test]
    fn four_failures_dont_lock() {
        let outcome = failures(LockoutPolicy::default(), 4);

        assert_eq!(outcome.streak.failures, 4);
        assert_eq!(outcome.streak.locked_until, None);
        assert_eq!(outcome.lock_started, None);
    }

    #[test]
    fn the_fifth_failure_locks_for_a_minute_from_itself() {
        let outcome = failures(LockoutPolicy::default(), 5);

        let fifth = after(4, 0);
        assert_eq!(outcome.streak, streak(5, fifth, Some(after(64, 0))));
        assert_eq!(outcome.lock_started, Some(after(64, 0)));
    }

    #[test]
    fn each_failure_past_the_threshold_starts_a_doubled_lock() {
        let policy = LockoutPolicy::default();
        let locked = streak(5, at(T0), Some(after(60, 0)));

        let outcome = after_failure(&policy, Some(&locked), after(60, 0));

        assert_eq!(outcome.streak, streak(6, after(60, 0), Some(after(180, 0))));
        assert_eq!(outcome.lock_started, Some(after(180, 0)));
    }

    #[test]
    fn a_streak_a_millisecond_short_of_a_day_counts_on() {
        let old = streak(4, at(T0), None);

        let outcome = after_failure(&LockoutPolicy::default(), Some(&old), after(86_400, -1));

        assert_eq!(outcome.streak.failures, 5);
        assert!(outcome.lock_started.is_some());
    }

    #[test]
    fn a_streak_a_day_old_is_forgotten_and_the_failure_starts_a_new_one() {
        let old = streak(9, at(T0), Some(after(960, 0)));

        let outcome = after_failure(&LockoutPolicy::default(), Some(&old), after(86_400, 0));

        assert_eq!(
            outcome,
            FailureOutcome {
                streak: streak(1, after(86_400, 0), None),
                lock_started: None,
            }
        );
    }

    #[test]
    fn the_count_saturates() {
        let most = streak(u32::MAX, at(T0), Some(after(60, 0)));

        let outcome = after_failure(&LockoutPolicy::default(), Some(&most), after(60, 0));

        assert_eq!(outcome.streak.failures, u32::MAX);
        assert_eq!(outcome.streak.locked_until, Some(after(3660, 0)));
    }

    #[test]
    fn no_streak_is_never_locked() {
        assert_eq!(check_lock(None, at(T0)), Ok(()));
    }

    #[test]
    fn a_streak_below_the_threshold_isnt_locked() {
        assert_eq!(check_lock(Some(&streak(4, at(T0), None)), at(T0)), Ok(()));
    }

    #[rstest]
    #[case::at_its_start(0, 0, Some(60_000))]
    #[case::a_millisecond_before_its_end(59, 999, Some(1))]
    #[case::exactly_at_its_end(60, 0, None)]
    #[case::after_its_end(61, 0, None)]
    fn a_lock_runs_until_its_end_exclusive(
        #[case] secs: i64,
        #[case] millis: i64,
        #[case] remaining_ms: Option<u64>,
    ) {
        let locked = streak(5, at(T0), Some(after(60, 0)));

        let checked = check_lock(Some(&locked), after(secs, millis));

        assert_eq!(
            checked,
            match remaining_ms {
                Some(ms) => Err(Locked {
                    until: after(60, 0),
                    remaining: Duration::from_millis(ms),
                }),
                None => Ok(()),
            }
        );
    }

    #[test]
    fn a_clock_that_went_back_keeps_the_lock_for_its_full_rest() {
        let locked = streak(5, at(T0), Some(after(60, 0)));

        let checked = check_lock(Some(&locked), after(-30, 0));

        assert_eq!(
            checked,
            Err(Locked {
                until: after(60, 0),
                remaining: Duration::from_secs(90),
            })
        );
    }

    #[rstest]
    #[case::fresh(0, false)]
    #[case::a_millisecond_short(86_400_000 - 1, false)]
    #[case::exactly_a_day(86_400_000, true)]
    #[case::long_ago(10 * 86_400_000, true)]
    #[case::in_the_future(-1000, false)]
    fn a_streak_expires_a_day_after_its_last_failure(#[case] age_ms: i64, #[case] expired: bool) {
        let old = streak(2, at(T0), None);

        assert_eq!(is_expired(&old, at(T0 + age_ms)), expired);
    }

    #[test]
    fn a_streak_with_a_running_lock_never_expires() {
        let locked = streak(9, at(T0), Some(after(2 * 86_400, 0)));

        assert!(!is_expired(&locked, after(86_400, 0)));
    }

    #[test]
    fn the_purge_cutoff_is_a_day_before_now_and_saturates() {
        assert_eq!(expired_failed_by(after(86_400, 0)), at(T0));
        assert!(is_expired(
            &streak(1, expired_failed_by(at(T0)), None),
            at(T0)
        ));
        assert_eq!(
            expired_failed_by(DateTime::<Utc>::MIN_UTC),
            DateTime::<Utc>::MIN_UTC
        );
    }
}
