//! The invites (Plan.md P7.1, P7.9, §7.2, ADR-0016): how long one lives,
//! which state it's in, and how many a user may hold at once.
//!
//! Registration is invite-only. An invite is single-use, carries a fixed
//! role and expires [`InviteTtl`] after its creation; its expiry is stored
//! when it's made, so a later config change doesn't move it. The server
//! holds only the code's hash; [`invite_state`] decides what a stored invite
//! is at a given time. A creator holds at most [`MAX_PENDING_INVITES`]
//! pending ones.

use core::time::Duration;

use chrono::{DateTime, Utc};

use crate::time::add;

/// How many pending invites one user may hold at once: unused, unrevoked
/// and unexpired. The next one is refused until one is used, revoked or
/// expires.
pub const MAX_PENDING_INVITES: u32 = 20;

/// How long an invite lives (`[auth] invite_ttl_hours`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InviteTtl {
    hours: u32,
}

/// Why a lifetime was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("an invite's lifetime must be between {min} and {max} hours")]
pub struct InviteTtlError {
    /// [`InviteTtl::MIN_HOURS`].
    pub min: u32,
    /// [`InviteTtl::MAX_HOURS`].
    pub max: u32,
}

impl InviteTtl {
    /// The shortest lifetime.
    pub const MIN_HOURS: u32 = 1;
    /// The longest lifetime: 30 days.
    pub const MAX_HOURS: u32 = 720;

    /// Checks `hours` against its range.
    ///
    /// # Errors
    /// [`InviteTtlError`] outside
    /// [`InviteTtl::MIN_HOURS`]..=[`InviteTtl::MAX_HOURS`].
    pub const fn try_new(hours: u32) -> Result<Self, InviteTtlError> {
        if hours < Self::MIN_HOURS || hours > Self::MAX_HOURS {
            return Err(InviteTtlError {
                min: Self::MIN_HOURS,
                max: Self::MAX_HOURS,
            });
        }
        Ok(Self { hours })
    }

    /// The lifetime in hours.
    #[must_use]
    pub const fn hours(&self) -> u32 {
        self.hours
    }

    /// The lifetime.
    #[must_use]
    pub const fn duration(&self) -> Duration {
        // At most 720 hours, so the product can't overflow.
        Duration::from_secs(self.hours as u64 * 3600)
    }

    /// When an invite created at `created_at` expires.
    #[must_use]
    pub fn expires_at(&self, created_at: DateTime<Utc>) -> DateTime<Utc> {
        add(created_at, self.duration())
    }
}

impl Default for InviteTtl {
    /// §7.2's: 72 hours.
    fn default() -> Self {
        Self { hours: 72 }
    }
}

/// A stored invite, as the rule sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InviteFacts {
    /// When it expires.
    pub expires_at: DateTime<Utc>,
    /// When it was used to register, if it was.
    pub used_at: Option<DateTime<Utc>>,
    /// When it was revoked, if it was.
    pub revoked_at: Option<DateTime<Utc>>,
}

/// What an invite is at a given time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InviteState {
    /// It can still register a user.
    Pending,
    /// It registered a user.
    Used,
    /// It was revoked before it was used.
    Revoked,
    /// It wasn't used before it expired.
    Expired,
}

impl InviteState {
    /// Every state, in declaration order.
    pub const ALL: [Self; 4] = [Self::Pending, Self::Used, Self::Revoked, Self::Expired];

    /// The state's name: `pending`, `used`, `revoked` or `expired`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Used => "used",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
        }
    }
}

/// What the invite described by `facts` is at `now`: used, else revoked,
/// else expired from its exact millisecond on (`now >= expires_at`), else
/// pending. A used invite stays used and a revoked one revoked after it
/// would have expired, since that's what happened to it.
#[must_use]
pub fn invite_state(facts: &InviteFacts, now: DateTime<Utc>) -> InviteState {
    if facts.used_at.is_some() {
        InviteState::Used
    } else if facts.revoked_at.is_some() {
        InviteState::Revoked
    } else if now >= facts.expires_at {
        InviteState::Expired
    } else {
        InviteState::Pending
    }
}

/// Whether a creator holding `pending` pending invites has reached
/// [`MAX_PENDING_INVITES`], so a new one is refused.
#[must_use]
pub const fn pending_cap_reached(pending: u32) -> bool {
    pending >= MAX_PENDING_INVITES
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    /// The start of every test, in milliseconds.
    const T0: i64 = 1_800_000_000_000;
    /// 72 hours in milliseconds.
    const DAYS_3: i64 = 72 * 3_600_000;

    fn at(millis: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(millis).unwrap()
    }

    fn facts(used: Option<i64>, revoked: Option<i64>) -> InviteFacts {
        InviteFacts {
            expires_at: at(T0 + DAYS_3),
            used_at: used.map(|millis| at(T0 + millis)),
            revoked_at: revoked.map(|millis| at(T0 + millis)),
        }
    }

    #[test]
    fn the_default_lifetime_is_72_hours() {
        let ttl = InviteTtl::default();

        assert_eq!(ttl.hours(), 72);
        assert_eq!(ttl.duration(), Duration::from_hours(72));
    }

    #[rstest]
    #[case::the_shortest(1)]
    #[case::the_default(72)]
    #[case::thirty_days(720)]
    fn a_lifetime_in_range_is_kept(#[case] hours: u32) {
        let ttl = InviteTtl::try_new(hours).unwrap();

        assert_eq!(ttl.hours(), hours);
        assert_eq!(ttl.duration(), Duration::from_hours(u64::from(hours)));
    }

    #[rstest]
    #[case::zero(0)]
    #[case::one_past_thirty_days(721)]
    #[case::the_largest(u32::MAX)]
    fn a_lifetime_out_of_range_is_refused(#[case] hours: u32) {
        assert_eq!(
            InviteTtl::try_new(hours),
            Err(InviteTtlError { min: 1, max: 720 })
        );
    }

    #[test]
    fn the_refusal_names_the_range() {
        let error = InviteTtl::try_new(0).unwrap_err();

        assert_eq!(
            error.to_string(),
            "an invite's lifetime must be between 1 and 720 hours"
        );
    }

    #[test]
    fn an_invite_expires_its_lifetime_after_its_creation() {
        let ttl = InviteTtl::try_new(5).unwrap();

        assert_eq!(ttl.expires_at(at(T0)), at(T0 + 5 * 3_600_000));
    }

    #[test]
    fn an_expiry_past_chronos_range_saturates() {
        let ttl = InviteTtl::try_new(720).unwrap();

        assert_eq!(
            ttl.expires_at(DateTime::<Utc>::MAX_UTC),
            DateTime::<Utc>::MAX_UTC
        );
    }

    #[rstest]
    #[case::at_creation(0, InviteState::Pending)]
    #[case::one_millisecond_before_its_end(DAYS_3 - 1, InviteState::Pending)]
    #[case::exactly_at_its_end(DAYS_3, InviteState::Expired)]
    #[case::after_its_end(DAYS_3 + 1, InviteState::Expired)]
    #[case::clock_went_back(-60_000, InviteState::Pending)]
    fn an_unused_invite_is_pending_until_it_expires(
        #[case] millis: i64,
        #[case] expected: InviteState,
    ) {
        assert_eq!(invite_state(&facts(None, None), at(T0 + millis)), expected);
    }

    #[rstest]
    #[case::before_its_end(1_000)]
    #[case::after_its_end(DAYS_3 + 1)]
    fn a_used_invite_stays_used(#[case] millis: i64) {
        assert_eq!(
            invite_state(&facts(Some(500), None), at(T0 + millis)),
            InviteState::Used
        );
    }

    #[rstest]
    #[case::before_its_end(1_000)]
    #[case::after_its_end(DAYS_3 + 1)]
    fn a_revoked_invite_stays_revoked(#[case] millis: i64) {
        assert_eq!(
            invite_state(&facts(None, Some(500)), at(T0 + millis)),
            InviteState::Revoked
        );
    }

    #[test]
    fn used_wins_over_revoked() {
        // The table refuses such a row; the rule still has one answer.
        assert_eq!(
            invite_state(&facts(Some(500), Some(600)), at(T0 + 1_000)),
            InviteState::Used
        );
    }

    #[test]
    fn the_state_names_are_pinned() {
        let names = InviteState::ALL.map(InviteState::as_str);

        assert_eq!(names, ["pending", "used", "revoked", "expired"]);
    }

    #[rstest]
    #[case::none(0, false)]
    #[case::one_below(19, false)]
    #[case::at_the_cap(20, true)]
    #[case::past_it(21, true)]
    fn the_twenty_first_pending_invite_is_refused(#[case] pending: u32, #[case] reached: bool) {
        assert_eq!(MAX_PENDING_INVITES, 20);
        assert_eq!(pending_cap_reached(pending), reached);
    }
}
