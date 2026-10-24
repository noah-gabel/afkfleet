//! What the server's retention deletes, and from when (Plan.md P11.6;
//! ADR-0020, U19, U21, U22, S45, S46, R51).
//!
//! Each kept thing goes once its retention has passed: from the exact
//! millisecond of `its time + its retention` on (`now >= expiry`), as a
//! session's expiry already works. So a cut is a point in time, and what is
//! dated at or before it goes:
//! - **chat:** a line received at or before `now - [chat] retention_days`;
//! - **sessions:** one created at or before `now - refresh_token_absolute_days`
//!   (the running config's), revoked or not (ADR-0016);
//! - **the audit log:** an entry dated at or before `now - [audit]
//!   retention_days`, as a prefix by ID, so an entry dated later never goes
//!   early;
//! - **unused invites and enrollment tokens:** one whose expiry is at or
//!   before the audit log's cut (S46).
//!
//! Besides its age, a bot keeps at most [`ChatBound`] lines, its newest.

use core::time::Duration;

use chrono::{DateTime, Utc};

use crate::auth::SessionPolicy;
use crate::time::sub;

/// One day.
const DAY: Duration = Duration::from_hours(24);

/// How long chat is kept (`[chat] retention_days`, U21).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatRetention {
    days: u32,
}

impl ChatRetention {
    /// The shortest retention.
    pub const MIN_DAYS: u32 = 1;
    /// The longest retention: 30 days.
    pub const MAX_DAYS: u32 = 30;

    /// Checks `days` against its range.
    ///
    /// # Errors
    /// [`RetentionError`] outside
    /// [`ChatRetention::MIN_DAYS`]..=[`ChatRetention::MAX_DAYS`].
    pub const fn try_new(days: u32) -> Result<Self, RetentionError> {
        match RetentionError::check(days, Self::MIN_DAYS, Self::MAX_DAYS) {
            Ok(()) => Ok(Self { days }),
            Err(error) => Err(error),
        }
    }

    /// The retention in days.
    #[must_use]
    pub const fn days(&self) -> u32 {
        self.days
    }
}

impl Default for ChatRetention {
    /// Appendix A's 14 days.
    fn default() -> Self {
        Self { days: 14 }
    }
}

/// How many lines each bot keeps at most, its newest
/// (`[chat] max_lines_per_bot`, U19).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatBound {
    lines: u32,
}

impl ChatBound {
    /// The fewest lines a bot keeps.
    pub const MIN_LINES: u32 = 100;
    /// The most lines a bot keeps.
    pub const MAX_LINES: u32 = 200_000;

    /// Checks `lines` against its range.
    ///
    /// # Errors
    /// [`RetentionError`] outside
    /// [`ChatBound::MIN_LINES`]..=[`ChatBound::MAX_LINES`].
    pub const fn try_new(lines: u32) -> Result<Self, RetentionError> {
        match RetentionError::check(lines, Self::MIN_LINES, Self::MAX_LINES) {
            Ok(()) => Ok(Self { lines }),
            Err(error) => Err(error),
        }
    }

    /// The most lines a bot keeps.
    #[must_use]
    pub const fn lines(&self) -> u32 {
        self.lines
    }
}

impl Default for ChatBound {
    /// 50,000 lines (U19).
    fn default() -> Self {
        Self { lines: 50_000 }
    }
}

/// How long the audit log is kept (`[audit] retention_days`, U22): every
/// entry holds a user's IP address, so never longer than two years.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditRetention {
    days: u32,
}

impl AuditRetention {
    /// The shortest retention: a quarter, longer than the longest window any
    /// limit reads from the audit log (24 h) and than chat's longest.
    pub const MIN_DAYS: u32 = 90;
    /// The longest retention: two years.
    pub const MAX_DAYS: u32 = 730;

    /// Checks `days` against its range.
    ///
    /// # Errors
    /// [`RetentionError`] outside
    /// [`AuditRetention::MIN_DAYS`]..=[`AuditRetention::MAX_DAYS`].
    pub const fn try_new(days: u32) -> Result<Self, RetentionError> {
        match RetentionError::check(days, Self::MIN_DAYS, Self::MAX_DAYS) {
            Ok(()) => Ok(Self { days }),
            Err(error) => Err(error),
        }
    }

    /// The retention in days.
    #[must_use]
    pub const fn days(&self) -> u32 {
        self.days
    }
}

impl Default for AuditRetention {
    /// A year (U22).
    fn default() -> Self {
        Self { days: 365 }
    }
}

/// Why a retention setting was refused: it's outside its range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("must be between {min} and {max}")]
pub struct RetentionError {
    /// The lowest valid value.
    pub min: u32,
    /// The highest valid value.
    pub max: u32,
}

impl RetentionError {
    /// `Ok` when `value` is within `min..=max`.
    const fn check(value: u32, min: u32, max: u32) -> Result<(), Self> {
        if value < min || value > max {
            Err(Self { min, max })
        } else {
            Ok(())
        }
    }
}

/// What the retention keeps: chat's age and size, and the audit log's age.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// How long chat is kept.
    pub chat: ChatRetention,
    /// How many lines each bot keeps at most.
    pub bound: ChatBound,
    /// How long the audit log is kept.
    pub audit: AuditRetention,
}

impl RetentionPolicy {
    /// The cuts at `now`, with `sessions`' absolute lifetime for the
    /// sessions'; see the [module docs](self).
    #[must_use]
    pub fn cuts(&self, now: DateTime<Utc>, sessions: &SessionPolicy) -> Cuts {
        let days = |days: u32| DAY.saturating_mul(days);
        Cuts {
            chat: sub(now, days(self.chat.days())),
            sessions: sub(now, sessions.absolute()),
            audit: sub(now, days(self.audit.days())),
        }
    }
}

/// Where one run of the retention cuts: what is dated at or before a cut
/// goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cuts {
    /// Chat received at or before it goes.
    pub chat: DateTime<Utc>,
    /// Sessions created at or before it go, with their rotated tokens.
    pub sessions: DateTime<Utc>,
    /// Audit entries dated at or before it go, as a prefix by ID; and unused
    /// invites and enrollment tokens that expired at or before it.
    pub audit: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    #[test]
    fn the_defaults_are_the_users() {
        let policy = RetentionPolicy::default();

        assert_eq!(policy.chat.days(), 14);
        assert_eq!(policy.bound.lines(), 50_000);
        assert_eq!(policy.audit.days(), 365);
    }

    #[rstest]
    #[case::chat_below(ChatRetention::try_new(0).map(|kept| kept.days()), Err(RetentionError { min: 1, max: 30 }))]
    #[case::chat_lowest(ChatRetention::try_new(1).map(|kept| kept.days()), Ok(1))]
    #[case::chat_highest(ChatRetention::try_new(30).map(|kept| kept.days()), Ok(30))]
    #[case::chat_above(ChatRetention::try_new(31).map(|kept| kept.days()), Err(RetentionError { min: 1, max: 30 }))]
    #[case::bound_below(ChatBound::try_new(99).map(|kept| kept.lines()), Err(RetentionError { min: 100, max: 200_000 }))]
    #[case::bound_lowest(ChatBound::try_new(100).map(|kept| kept.lines()), Ok(100))]
    #[case::bound_highest(ChatBound::try_new(200_000).map(|kept| kept.lines()), Ok(200_000))]
    #[case::bound_above(ChatBound::try_new(200_001).map(|kept| kept.lines()), Err(RetentionError { min: 100, max: 200_000 }))]
    #[case::audit_below(AuditRetention::try_new(89).map(|kept| kept.days()), Err(RetentionError { min: 90, max: 730 }))]
    #[case::audit_lowest(AuditRetention::try_new(90).map(|kept| kept.days()), Ok(90))]
    #[case::audit_highest(AuditRetention::try_new(730).map(|kept| kept.days()), Ok(730))]
    #[case::audit_above(AuditRetention::try_new(731).map(|kept| kept.days()), Err(RetentionError { min: 90, max: 730 }))]
    fn each_setting_holds_its_range(
        #[case] made: Result<u32, RetentionError>,
        #[case] expected: Result<u32, RetentionError>,
    ) {
        assert_eq!(made, expected);
    }

    #[test]
    fn a_refused_setting_names_its_range() {
        assert_eq!(
            ChatRetention::try_new(0).unwrap_err().to_string(),
            "must be between 1 and 30"
        );
    }

    #[test]
    fn each_cut_is_its_retention_before_now() {
        let policy = RetentionPolicy {
            chat: ChatRetention::try_new(14).unwrap(),
            bound: ChatBound::default(),
            audit: AuditRetention::try_new(90).unwrap(),
        };
        let sessions = SessionPolicy::try_new(
            Duration::from_mins(15),
            Duration::from_hours(7 * 24),
            Duration::from_hours(30 * 24),
        )
        .unwrap();

        let cuts = policy.cuts(at("2027-05-31T12:00:00.250Z"), &sessions);

        assert_eq!(
            cuts,
            Cuts {
                chat: at("2027-05-17T12:00:00.250Z"),
                sessions: at("2027-05-01T12:00:00.250Z"),
                audit: at("2027-03-02T12:00:00.250Z"),
            }
        );
    }

    #[test]
    fn a_cut_near_the_earliest_time_saturates() {
        let cuts =
            RetentionPolicy::default().cuts(DateTime::<Utc>::MIN_UTC, &SessionPolicy::default());

        assert_eq!(cuts.chat, DateTime::<Utc>::MIN_UTC);
        assert_eq!(cuts.sessions, DateTime::<Utc>::MIN_UTC);
        assert_eq!(cuts.audit, DateTime::<Utc>::MIN_UTC);
    }
}
