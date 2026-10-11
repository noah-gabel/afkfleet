//! The rules of sessions (Plan.md P7.3, §7.2, ADR-0006, ADR-0016): their
//! lifetimes ([`SessionPolicy`]), when a session's tokens expire, and what a
//! refresh comes to ([`refresh_decision`]).
//!
//! A session stores only two anchors ([`SessionTimes`]): when it was created
//! and when its current token pair was issued. Every expiry is computed from
//! them with the running config's lifetimes, so lowering a lifetime shortens
//! existing sessions too. A time is expired from the exact millisecond of its
//! expiry on (`now >= expiry`).

use core::time::Duration;

use chrono::{DateTime, Utc};

use crate::time::add;

/// A session's lifetimes (§7.2): the access token's, and the refresh
/// token's idle and absolute ones. Each is longer than zero, and the access
/// token's < idle ≤ absolute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionPolicy {
    access_ttl: Duration,
    idle: Duration,
    absolute: Duration,
}

/// Why lifetimes were refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SessionPolicyError {
    /// A lifetime is zero.
    #[error("a lifetime must be longer than zero")]
    Zero,
    /// The access token would live as long as an idle refresh token, or
    /// longer.
    #[error("the access token's lifetime must be shorter than the idle lifetime")]
    AccessNotShorterThanIdle,
    /// The idle lifetime would exceed the absolute one, so it would never
    /// matter.
    #[error("the idle lifetime can't be longer than the absolute lifetime")]
    IdleLongerThanAbsolute,
}

impl SessionPolicy {
    /// Checks the lifetimes.
    ///
    /// # Errors
    /// [`SessionPolicyError::Zero`],
    /// [`SessionPolicyError::AccessNotShorterThanIdle`] or
    /// [`SessionPolicyError::IdleLongerThanAbsolute`].
    pub fn try_new(
        access_ttl: Duration,
        idle: Duration,
        absolute: Duration,
    ) -> Result<Self, SessionPolicyError> {
        if access_ttl.is_zero() || idle.is_zero() || absolute.is_zero() {
            return Err(SessionPolicyError::Zero);
        }
        if access_ttl >= idle {
            return Err(SessionPolicyError::AccessNotShorterThanIdle);
        }
        if idle > absolute {
            return Err(SessionPolicyError::IdleLongerThanAbsolute);
        }
        Ok(Self {
            access_ttl,
            idle,
            absolute,
        })
    }

    /// How long an access token lives.
    #[must_use]
    pub const fn access_ttl(&self) -> Duration {
        self.access_ttl
    }

    /// How long a refresh token lives unused.
    #[must_use]
    pub const fn idle(&self) -> Duration {
        self.idle
    }

    /// How long a session lives at most, however often it's refreshed.
    #[must_use]
    pub const fn absolute(&self) -> Duration {
        self.absolute
    }

    /// When the session's absolute lifetime ends.
    #[must_use]
    pub fn absolute_expiry(&self, times: SessionTimes) -> DateTime<Utc> {
        add(times.created_at, self.absolute)
    }

    /// When the current access token expires: its lifetime after the last
    /// refresh, at the latest when the session's absolute lifetime ends.
    #[must_use]
    pub fn access_expiry(&self, times: SessionTimes) -> DateTime<Utc> {
        add(times.refreshed_at, self.access_ttl).min(self.absolute_expiry(times))
    }

    /// When the current refresh token expires: the idle lifetime after the
    /// last refresh, at the latest when the absolute lifetime ends.
    #[must_use]
    pub fn refresh_expiry(&self, times: SessionTimes) -> DateTime<Utc> {
        add(times.refreshed_at, self.idle).min(self.absolute_expiry(times))
    }

    /// Whether the current access token is still valid at `now`.
    #[must_use]
    pub fn access_valid(&self, times: SessionTimes, now: DateTime<Utc>) -> bool {
        now < self.access_expiry(times)
    }

    /// Whether the session can still be refreshed at `now`: neither idle
    /// nor past its absolute lifetime.
    #[must_use]
    pub fn refresh_valid(&self, times: SessionTimes, now: DateTime<Utc>) -> bool {
        now < self.refresh_expiry(times)
    }
}

impl Default for SessionPolicy {
    /// §7.2's lifetimes: 15 minutes, 7 days idle, 30 days absolute.
    fn default() -> Self {
        Self {
            access_ttl: Duration::from_mins(15),
            idle: Duration::from_hours(7 * 24),
            absolute: Duration::from_hours(30 * 24),
        }
    }
}

/// A session's two anchors: everything its expiry is computed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionTimes {
    /// When the session was created: the anchor of its absolute lifetime.
    pub created_at: DateTime<Utc>,
    /// When its current token pair was issued: the anchor of the access
    /// token's lifetime and of the idle lifetime.
    pub refreshed_at: DateTime<Utc>,
}

/// Which refresh token of a session was presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshMatch {
    /// The session's current refresh token.
    Current,
    /// One the session has already replaced.
    Rotated,
}

/// What the server found for a presented refresh token: its session's
/// facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefreshFacts {
    /// Which of the session's tokens it is.
    pub matched: RefreshMatch,
    /// The session's anchors.
    pub times: SessionTimes,
    /// Whether the session was revoked.
    pub revoked: bool,
    /// Whether the session's user is disabled.
    pub user_disabled: bool,
}

/// What a refresh comes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshDecision {
    /// Issue a new token pair and keep the presented token in the history.
    Rotate,
    /// An already replaced token of a live session: someone else holds a
    /// copy. Revoke the whole session and record it.
    Reuse,
    /// Refuse it: 401, nothing changes and nothing is recorded.
    Reject,
}

/// Decides a refresh (§7.2, ADR-0016), in this order:
/// 1. an unknown token (`None`) is rejected;
/// 2. so is a token of a revoked session;
/// 3. and one of an expired session (idle or absolute);
/// 4. an already replaced token of a live session is reuse, even when the
///    user is disabled;
/// 5. the current token of a disabled user is rejected;
/// 6. the current token of a live session rotates.
///
/// There's no grace window, and a replaced token of a revoked or expired
/// session is a plain rejection, so replays can't flood the audit log.
#[must_use]
pub fn refresh_decision(
    policy: &SessionPolicy,
    facts: Option<RefreshFacts>,
    now: DateTime<Utc>,
) -> RefreshDecision {
    let Some(facts) = facts else {
        return RefreshDecision::Reject;
    };
    if facts.revoked || !policy.refresh_valid(facts.times, now) {
        return RefreshDecision::Reject;
    }
    match facts.matched {
        RefreshMatch::Rotated => RefreshDecision::Reuse,
        RefreshMatch::Current if facts.user_disabled => RefreshDecision::Reject,
        RefreshMatch::Current => RefreshDecision::Rotate,
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    const MIN: u64 = 60;
    const DAY: u64 = 24 * 60 * MIN;

    fn at(millis: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(millis).unwrap()
    }

    /// The start of every test, in milliseconds.
    const T0: i64 = 1_800_000_000_000;

    /// `T0` plus `secs` seconds and `millis` milliseconds.
    fn after(secs: u64, millis: i64) -> DateTime<Utc> {
        at(T0 + i64::try_from(secs).unwrap() * 1000 + millis)
    }

    fn policy() -> SessionPolicy {
        SessionPolicy::default()
    }

    /// A session created at `T0` and last refreshed `refreshed` seconds
    /// later.
    fn times(refreshed: u64) -> SessionTimes {
        SessionTimes {
            created_at: at(T0),
            refreshed_at: after(refreshed, 0),
        }
    }

    // --- The policy ------------------------------------------------------------

    #[test]
    fn the_default_is_section_7_2s() {
        let policy = SessionPolicy::default();

        assert_eq!(policy.access_ttl(), Duration::from_secs(15 * MIN));
        assert_eq!(policy.idle(), Duration::from_secs(7 * DAY));
        assert_eq!(policy.absolute(), Duration::from_secs(30 * DAY));
        assert_eq!(
            SessionPolicy::try_new(policy.access_ttl(), policy.idle(), policy.absolute()),
            Ok(policy)
        );
    }

    #[rstest]
    #[case::zero_access(0, 7 * DAY, 30 * DAY, SessionPolicyError::Zero)]
    #[case::zero_idle(15 * MIN, 0, 30 * DAY, SessionPolicyError::Zero)]
    #[case::zero_absolute(15 * MIN, 7 * DAY, 0, SessionPolicyError::Zero)]
    #[case::access_as_long_as_idle(DAY, DAY, 30 * DAY, SessionPolicyError::AccessNotShorterThanIdle)]
    #[case::access_longer_than_idle(2 * DAY, DAY, 30 * DAY, SessionPolicyError::AccessNotShorterThanIdle)]
    #[case::idle_longer_than_absolute(15 * MIN, 31 * DAY, 30 * DAY, SessionPolicyError::IdleLongerThanAbsolute)]
    fn invalid_lifetimes_are_refused(
        #[case] access: u64,
        #[case] idle: u64,
        #[case] absolute: u64,
        #[case] error: SessionPolicyError,
    ) {
        assert_eq!(
            SessionPolicy::try_new(
                Duration::from_secs(access),
                Duration::from_secs(idle),
                Duration::from_secs(absolute)
            ),
            Err(error)
        );
    }

    #[test]
    fn idle_may_equal_absolute() {
        assert!(
            SessionPolicy::try_new(
                Duration::from_secs(MIN),
                Duration::from_secs(DAY),
                Duration::from_secs(DAY)
            )
            .is_ok()
        );
    }

    // --- Expiry ------------------------------------------------------------------

    #[test]
    fn an_access_token_lives_15_minutes_after_the_last_refresh() {
        let times = times(3600);

        assert_eq!(policy().access_expiry(times), after(3600 + 15 * MIN, 0));
        assert!(policy().access_valid(times, after(3600 + 15 * MIN, -1)));
        assert!(
            !policy().access_valid(times, after(3600 + 15 * MIN, 0)),
            "expired at the exact millisecond"
        );
    }

    #[test]
    fn a_refresh_token_lives_7_days_idle_after_the_last_refresh() {
        let times = times(DAY);

        assert_eq!(policy().refresh_expiry(times), after(8 * DAY, 0));
        assert!(policy().refresh_valid(times, after(8 * DAY, -1)));
        assert!(!policy().refresh_valid(times, after(8 * DAY, 0)));
    }

    #[test]
    fn the_absolute_lifetime_caps_the_idle_one() {
        // Refreshed on day 29: idle would end on day 36, the session on day 30.
        let times = times(29 * DAY);

        assert_eq!(policy().refresh_expiry(times), after(30 * DAY, 0));
        assert!(policy().refresh_valid(times, after(30 * DAY, -1)));
        assert!(!policy().refresh_valid(times, after(30 * DAY, 0)));
    }

    #[test]
    fn the_absolute_lifetime_caps_the_access_token() {
        // Refreshed 5 minutes before the session's end.
        let times = times(30 * DAY - 5 * MIN);

        assert_eq!(policy().access_expiry(times), after(30 * DAY, 0));
        assert!(!policy().access_valid(times, after(30 * DAY, 0)));
    }

    #[test]
    fn a_clock_that_went_back_doesnt_expire_anything() {
        let times = times(DAY);

        assert!(policy().access_valid(times, after(0, 0)));
        assert!(policy().refresh_valid(times, after(0, 0)));
    }

    #[test]
    fn expiry_saturates_instead_of_overflowing() {
        let times = SessionTimes {
            created_at: DateTime::<Utc>::MAX_UTC,
            refreshed_at: DateTime::<Utc>::MAX_UTC,
        };

        assert_eq!(policy().access_expiry(times), DateTime::<Utc>::MAX_UTC);
        assert_eq!(policy().refresh_expiry(times), DateTime::<Utc>::MAX_UTC);
    }

    // --- The refresh decision --------------------------------------------------

    fn facts(matched: RefreshMatch) -> RefreshFacts {
        RefreshFacts {
            matched,
            times: times(DAY),
            revoked: false,
            user_disabled: false,
        }
    }

    /// One day after `times(DAY)`'s refresh: well inside every lifetime.
    fn soon() -> DateTime<Utc> {
        after(2 * DAY, 0)
    }

    #[test]
    fn an_unknown_token_is_rejected() {
        assert_eq!(
            refresh_decision(&policy(), None, soon()),
            RefreshDecision::Reject
        );
    }

    #[test]
    fn the_current_token_of_a_live_session_rotates() {
        assert_eq!(
            refresh_decision(&policy(), Some(facts(RefreshMatch::Current)), soon()),
            RefreshDecision::Rotate
        );
    }

    #[test]
    fn a_replaced_token_of_a_live_session_is_reuse() {
        assert_eq!(
            refresh_decision(&policy(), Some(facts(RefreshMatch::Rotated)), soon()),
            RefreshDecision::Reuse
        );
    }

    #[rstest]
    #[case::current(RefreshMatch::Current)]
    #[case::replaced(RefreshMatch::Rotated)]
    fn any_token_of_a_revoked_session_is_rejected(#[case] matched: RefreshMatch) {
        let facts = RefreshFacts {
            revoked: true,
            ..facts(matched)
        };

        assert_eq!(
            refresh_decision(&policy(), Some(facts), soon()),
            RefreshDecision::Reject
        );
    }

    /// P7.12's idle expiry, through the rule: 7 days after the last refresh.
    #[rstest]
    #[case::current(RefreshMatch::Current)]
    #[case::replaced(RefreshMatch::Rotated)]
    fn any_token_of_an_idle_session_is_rejected(#[case] matched: RefreshMatch) {
        assert_eq!(
            refresh_decision(&policy(), Some(facts(matched)), after(8 * DAY, -1)),
            refresh_decision(&policy(), Some(facts(matched)), soon()),
            "still live a millisecond before"
        );
        assert_eq!(
            refresh_decision(&policy(), Some(facts(matched)), after(8 * DAY, 0)),
            RefreshDecision::Reject
        );
    }

    /// P7.12's absolute expiry, through the rule: 30 days after creation,
    /// even when refreshed a moment before.
    #[rstest]
    #[case::current(RefreshMatch::Current)]
    #[case::replaced(RefreshMatch::Rotated)]
    fn any_token_past_the_absolute_lifetime_is_rejected(#[case] matched: RefreshMatch) {
        let facts = RefreshFacts {
            times: times(30 * DAY - MIN),
            ..facts(matched)
        };

        assert_eq!(
            refresh_decision(&policy(), Some(facts), after(30 * DAY, 0)),
            RefreshDecision::Reject
        );
    }

    #[test]
    fn the_current_token_of_a_disabled_user_is_rejected() {
        let facts = RefreshFacts {
            user_disabled: true,
            ..facts(RefreshMatch::Current)
        };

        assert_eq!(
            refresh_decision(&policy(), Some(facts), soon()),
            RefreshDecision::Reject
        );
    }

    #[test]
    fn a_replaced_token_of_a_disabled_user_is_still_reuse() {
        let facts = RefreshFacts {
            user_disabled: true,
            ..facts(RefreshMatch::Rotated)
        };

        assert_eq!(
            refresh_decision(&policy(), Some(facts), soon()),
            RefreshDecision::Reuse
        );
    }
}
