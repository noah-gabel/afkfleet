//! What an authenticated request needs (Plan.md P7.8, §7.2, ADR-0016):
//! whether a session's access token still counts ([`access_decision`]),
//! when its `last_used_at` is due for a write ([`touch_due`]), and whether
//! the password was entered again recently enough for a step-up action
//! ([`is_stepped_up`]).

use core::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};

use super::{SessionPolicy, SessionTimes};
use crate::time::elapsed;

/// How old a session's `last_used_at` must be before a request writes it
/// again: at most one write per session per minute.
pub const TOUCH_INTERVAL: Duration = Duration::from_mins(1);

/// How long after the password was entered again a step-up action is
/// allowed (§7.2).
pub const STEP_UP_WINDOW: Duration = Duration::from_mins(5);

/// What the server found for a presented access token: its session's facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessFacts {
    /// The session's anchors.
    pub times: SessionTimes,
    /// Whether the session was revoked.
    pub revoked: bool,
    /// Whether the session's user is disabled.
    pub user_disabled: bool,
}

/// Why a found session's access token doesn't count. The API answers every
/// one alike; the reason is only logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessRejection {
    /// The session was revoked.
    Revoked,
    /// The access token expired: its lifetime, or the session's absolute
    /// one, is over.
    Expired,
    /// The session's user is disabled.
    Disabled,
}

impl AccessRejection {
    /// The reason as the log names it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Revoked => "revoked",
            Self::Expired => "expired",
            Self::Disabled => "disabled",
        }
    }
}

/// Whether a found session's access token counts at `now`: not revoked,
/// not expired, and its user not disabled, checked in that order.
///
/// # Errors
/// The first [`AccessRejection`] that applies.
pub fn access_decision(
    policy: &SessionPolicy,
    facts: AccessFacts,
    now: DateTime<Utc>,
) -> Result<(), AccessRejection> {
    if facts.revoked {
        return Err(AccessRejection::Revoked);
    }
    if !policy.access_valid(facts.times, now) {
        return Err(AccessRejection::Expired);
    }
    if facts.user_disabled {
        return Err(AccessRejection::Disabled);
    }
    Ok(())
}

/// Whether a request at `now` should write `last_used_at`: when the stored
/// value is at least [`TOUCH_INTERVAL`] old. A value in the future (a clock
/// that went back) isn't due.
#[must_use]
pub fn touch_due(last_used_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    last_used_at <= now && elapsed(last_used_at, now) >= TOUCH_INTERVAL
}

/// The newest stored `last_used_at` that is due at `now`: the conditional
/// write's limit, so two requests that both found it due write it once.
#[must_use]
pub fn touch_stale_since(now: DateTime<Utc>) -> DateTime<Utc> {
    TimeDelta::from_std(TOUCH_INTERVAL)
        .ok()
        .and_then(|interval| now.checked_sub_signed(interval))
        .unwrap_or(DateTime::<Utc>::MIN_UTC)
}

/// Whether `stamp`, when the password was last entered again, allows a
/// step-up action at `now`: at most [`STEP_UP_WINDOW`] old. No stamp, or one
/// in the future (a clock that went back), doesn't count.
#[must_use]
pub fn is_stepped_up(stamp: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    stamp.is_some_and(|stamp| stamp <= now && elapsed(stamp, now) <= STEP_UP_WINDOW)
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

    fn facts() -> AccessFacts {
        AccessFacts {
            times: SessionTimes {
                created_at: at(T0),
                refreshed_at: at(T0),
            },
            revoked: false,
            user_disabled: false,
        }
    }

    fn decide(facts: AccessFacts, now: DateTime<Utc>) -> Result<(), AccessRejection> {
        access_decision(&SessionPolicy::default(), facts, now)
    }

    #[test]
    fn a_live_sessions_access_token_counts() {
        assert_eq!(decide(facts(), after(60, 0)), Ok(()));
    }

    #[test]
    fn an_access_token_expires_15_minutes_after_the_refresh() {
        assert_eq!(decide(facts(), after(15 * 60, -1)), Ok(()));
        assert_eq!(
            decide(facts(), after(15 * 60, 0)),
            Err(AccessRejection::Expired)
        );
    }

    #[test]
    fn a_revoked_session_is_refused_first() {
        let facts = AccessFacts {
            revoked: true,
            user_disabled: true,
            ..facts()
        };

        assert_eq!(
            decide(facts, after(16 * 60, 0)),
            Err(AccessRejection::Revoked)
        );
    }

    #[test]
    fn an_expired_token_is_refused_before_a_disabled_user() {
        let facts = AccessFacts {
            user_disabled: true,
            ..facts()
        };

        assert_eq!(
            decide(facts, after(16 * 60, 0)),
            Err(AccessRejection::Expired)
        );
    }

    #[test]
    fn a_disabled_users_token_is_refused() {
        let facts = AccessFacts {
            user_disabled: true,
            ..facts()
        };

        assert_eq!(decide(facts, after(60, 0)), Err(AccessRejection::Disabled));
    }

    #[test]
    fn the_reasons_have_their_names() {
        assert_eq!(AccessRejection::Revoked.as_str(), "revoked");
        assert_eq!(AccessRejection::Expired.as_str(), "expired");
        assert_eq!(AccessRejection::Disabled.as_str(), "disabled");
    }

    #[rstest]
    #[case::just_written(0, 0, false)]
    #[case::a_millisecond_short(60, -1, false)]
    #[case::exactly_a_minute(60, 0, true)]
    #[case::long_ago(3600, 0, true)]
    #[case::in_the_future(-10, 0, false)]
    fn the_touch_is_due_once_a_minute(#[case] secs: i64, #[case] millis: i64, #[case] due: bool) {
        assert_eq!(touch_due(at(T0), after(secs, millis)), due);
    }

    #[test]
    fn the_stale_limit_is_a_minute_before_now() {
        assert_eq!(touch_stale_since(after(120, 0)), after(60, 0));
        assert!(touch_due(touch_stale_since(after(120, 0)), after(120, 0)));
    }

    #[test]
    fn the_stale_limit_saturates_at_the_earliest_time() {
        assert_eq!(
            touch_stale_since(DateTime::<Utc>::MIN_UTC),
            DateTime::<Utc>::MIN_UTC
        );
    }

    #[rstest]
    #[case::just_now(0, 0, true)]
    #[case::exactly_5_minutes(300, 0, true)]
    #[case::a_millisecond_late(300, 1, false)]
    #[case::stamp_in_the_future(-1, 0, false)]
    fn a_step_up_counts_for_5_minutes(
        #[case] secs: i64,
        #[case] millis: i64,
        #[case] counts: bool,
    ) {
        assert_eq!(is_stepped_up(Some(at(T0)), after(secs, millis)), counts);
    }

    #[test]
    fn no_step_up_never_counts() {
        assert!(!is_stepped_up(None, at(T0)));
    }
}
