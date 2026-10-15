//! [`AccountKind`] and [`AccountStatus`]: what a linked Minecraft account is
//! and where it stands, kept in `mc_accounts.kind`, `mc_accounts.status` and
//! `mc_accounts.status_reason` (Plan.md P9.1, P9.5, ADR-0018).
//!
//! A Microsoft account signs in through Microsoft and holds a refresh token
//! in the server's vault. An offline account is only a name, for
//! offline-mode servers in development (P9.7). Only a Microsoft account has
//! a status other than [`AccountStatus::Active`]: a new link waits for
//! approval, a refresh token Microsoft refuses needs the account linked
//! again, and an account Minecraft's side refuses ([`RefusalReason`]) gets no
//! session until it's linked again.
//!
//! A status is read back from its two columns with
//! [`AccountStatus::from_columns`]: only a refused account has a reason.
//!
//! # Linking (P9.4, P9.7, ADR-0018)
//! Whoever links an account owns it. A Member owns at most
//! `[accounts] max_per_member` accounts of any kind and status
//! ([`over_quota`]); Admins and the Owner have no quota. A new Microsoft
//! link is [`AccountStatus::Pending`] until someone above the linker approves
//! it, except the Owner's own ([`new_link_status`]). A user starts at most
//! `[rate_limits] link_flows_per_user_per_day` link flows in any
//! [`LINK_START_WINDOW`] ([`link_start_wait`]).

use core::fmt;
use core::num::NonZeroU32;
use core::str::FromStr;
use core::time::Duration;

use chrono::{DateTime, Utc};

use crate::authz::Role;
use crate::time::{add, elapsed};

/// Why a stored name isn't a known account kind, status or reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AccountError {
    /// The name isn't one of the [`AccountKind`] names.
    #[error("unknown account kind")]
    UnknownKind,
    /// The name isn't one of the [`AccountStatus`] names.
    #[error("unknown account status")]
    UnknownStatus,
    /// The name isn't one of the [`RefusalReason`] names.
    #[error("unknown refusal reason")]
    UnknownReason,
    /// A refused account without its reason.
    #[error("a refused account needs its reason")]
    MissingReason,
    /// A reason on an account that isn't refused.
    #[error("only a refused account has a reason")]
    UnexpectedReason,
}

/// How an account signs in to Minecraft, stored in `mc_accounts.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountKind {
    /// Through Microsoft: the server holds its refresh token.
    Microsoft,
    /// Without any sign-in, for offline-mode servers in development only.
    Offline,
}

impl AccountKind {
    /// Returns the stable name stored in the `kind` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Microsoft => "microsoft",
            Self::Offline => "offline",
        }
    }
}

impl FromStr for AccountKind {
    type Err = AccountError;

    fn from_str(name: &str) -> Result<Self, AccountError> {
        match name {
            "microsoft" => Ok(Self::Microsoft),
            "offline" => Ok(Self::Offline),
            _ => Err(AccountError::UnknownKind),
        }
    }
}

impl fmt::Display for AccountKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why Minecraft's side refused a linked account when the server started a
/// session for it (P9.5), stored in `mc_accounts.status_reason`. Linking it
/// again checks the account afresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefusalReason {
    /// The account doesn't own Minecraft (no entitlement at all).
    NotOwned,
    /// The account has no Minecraft Java profile.
    NoProfile,
    /// Xbox Live banned the account.
    XboxBanned,
    /// The Microsoft account has no Xbox profile.
    XboxNoXboxProfile,
    /// Xbox Live isn't available in the account's country.
    XboxCountryUnavailable,
    /// The account needs adult verification.
    XboxAdultVerification,
    /// A child's account that an adult must add to a Microsoft family.
    XboxChildAccount,
    /// Another Xbox Live refusal; its code is in the audit log only.
    XboxOther,
    /// The refresh token now leads to another Minecraft profile than the
    /// one linked.
    ProfileMismatch,
}

impl RefusalReason {
    /// Every reason, for the tests that check them all.
    pub const ALL: [Self; 9] = [
        Self::NotOwned,
        Self::NoProfile,
        Self::XboxBanned,
        Self::XboxNoXboxProfile,
        Self::XboxCountryUnavailable,
        Self::XboxAdultVerification,
        Self::XboxChildAccount,
        Self::XboxOther,
        Self::ProfileMismatch,
    ];

    /// Returns the stable name stored in the `status_reason` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotOwned => "not_owned",
            Self::NoProfile => "no_profile",
            Self::XboxBanned => "xbox_banned",
            Self::XboxNoXboxProfile => "xbox_no_xbox_profile",
            Self::XboxCountryUnavailable => "xbox_country_unavailable",
            Self::XboxAdultVerification => "xbox_adult_verification",
            Self::XboxChildAccount => "xbox_child_account",
            Self::XboxOther => "xbox_other",
            Self::ProfileMismatch => "profile_mismatch",
        }
    }
}

impl FromStr for RefusalReason {
    type Err = AccountError;

    fn from_str(name: &str) -> Result<Self, AccountError> {
        Self::ALL
            .into_iter()
            .find(|reason| reason.as_str() == name)
            .ok_or(AccountError::UnknownReason)
    }
}

impl fmt::Display for RefusalReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where an account stands, stored in `mc_accounts.status` and, for a
/// refused account, `mc_accounts.status_reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountStatus {
    /// Linked, and waiting for someone above the linker to approve it; no bot
    /// can use it yet.
    Pending,
    /// Usable.
    Active,
    /// Microsoft refused its refresh token: it must be linked again.
    ReauthRequired,
    /// Minecraft's side refused it: it must be linked again.
    Refused(RefusalReason),
}

impl AccountStatus {
    /// Returns the stable name stored in the `status` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::ReauthRequired => "reauth_required",
            Self::Refused(_) => "refused",
        }
    }

    /// Returns the reason stored in the `status_reason` column: only a
    /// refused account has one.
    #[must_use]
    pub const fn reason(self) -> Option<RefusalReason> {
        match self {
            Self::Refused(reason) => Some(reason),
            Self::Pending | Self::Active | Self::ReauthRequired => None,
        }
    }

    /// Reads a status back from its `status` and `status_reason` columns.
    ///
    /// # Errors
    /// - [`AccountError::UnknownStatus`] for an unknown status name.
    /// - [`AccountError::MissingReason`] for `refused` without a reason.
    /// - [`AccountError::UnknownReason`] for an unknown reason name.
    /// - [`AccountError::UnexpectedReason`] for a reason on any other
    ///   status.
    pub fn from_columns(status: &str, reason: Option<&str>) -> Result<Self, AccountError> {
        let without_reason = match status {
            "pending" => Self::Pending,
            "active" => Self::Active,
            "reauth_required" => Self::ReauthRequired,
            "refused" => {
                let reason = reason.ok_or(AccountError::MissingReason)?;
                return reason.parse().map(Self::Refused);
            }
            _ => return Err(AccountError::UnknownStatus),
        };
        match reason {
            None => Ok(without_reason),
            Some(_) => Err(AccountError::UnexpectedReason),
        }
    }
}

impl fmt::Display for AccountStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The window the daily cap on link flows counts starts in: a start counts
/// until it's this old.
pub const LINK_START_WINDOW: Duration = Duration::from_hours(24);

/// Whether a user with `role` who owns `owned` accounts, of any kind and
/// status, may not link another: only a Member has a quota,
/// `max_per_member`.
#[must_use]
pub const fn over_quota(role: Role, owned: u32, max_per_member: u32) -> bool {
    matches!(role, Role::Member) && owned >= max_per_member
}

/// The status a new Microsoft link of a user with `role` starts in: the
/// Owner's is active at once, everyone else's waits for approval (ADR-0018).
#[must_use]
pub const fn new_link_status(role: Role) -> AccountStatus {
    match role {
        Role::Owner => AccountStatus::Active,
        Role::Admin | Role::Member => AccountStatus::Pending,
    }
}

/// How long a user must wait at `now` before starting another link flow,
/// given the times of their counted `starts` and the `cap` on starts in a
/// [`LINK_START_WINDOW`]: `None` when they may start one now.
///
/// A start stops counting once it's [`LINK_START_WINDOW`] old. The wait
/// lasts until enough of the counted starts are that old to bring the user
/// below the cap, which is the oldest one whenever exactly `cap` count.
#[must_use]
pub fn link_start_wait(
    starts: &[DateTime<Utc>],
    cap: NonZeroU32,
    now: DateTime<Utc>,
) -> Option<Duration> {
    let mut counted: Vec<DateTime<Utc>> = starts
        .iter()
        .copied()
        .filter(|start| elapsed(*start, now) < LINK_START_WINDOW)
        .collect();
    let count = u32::try_from(counted.len()).unwrap_or(u32::MAX);
    let excess = count.checked_sub(cap.get())?;
    // `excess + 1` starts must age out: the last of them is the one at index
    // `excess`, oldest first.
    counted.sort_unstable();
    let index = usize::try_from(excess).unwrap_or(usize::MAX);
    let last_to_age_out = counted.get(index)?;
    let free_at = add(*last_to_age_out, LINK_START_WINDOW);
    Some(
        free_at
            .signed_duration_since(now)
            .to_std()
            .unwrap_or(Duration::ZERO),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::microsoft(AccountKind::Microsoft, "microsoft")]
    #[case::offline(AccountKind::Offline, "offline")]
    fn kind_names_are_stable_and_round_trip(#[case] kind: AccountKind, #[case] name: &str) {
        assert_eq!(kind.as_str(), name);
        assert_eq!(kind.to_string(), name);
        assert_eq!(name.parse::<AccountKind>(), Ok(kind));
    }

    #[rstest]
    #[case::wrong_case("Microsoft")]
    #[case::padded(" offline")]
    #[case::unknown("mojang")]
    #[case::empty("")]
    fn unknown_kind_names_are_rejected(#[case] name: &str) {
        assert_eq!(name.parse::<AccountKind>(), Err(AccountError::UnknownKind));
    }

    #[rstest]
    #[case::not_owned(RefusalReason::NotOwned, "not_owned")]
    #[case::no_profile(RefusalReason::NoProfile, "no_profile")]
    #[case::xbox_banned(RefusalReason::XboxBanned, "xbox_banned")]
    #[case::xbox_no_xbox_profile(RefusalReason::XboxNoXboxProfile, "xbox_no_xbox_profile")]
    #[case::xbox_country_unavailable(
        RefusalReason::XboxCountryUnavailable,
        "xbox_country_unavailable"
    )]
    #[case::xbox_adult_verification(
        RefusalReason::XboxAdultVerification,
        "xbox_adult_verification"
    )]
    #[case::xbox_child_account(RefusalReason::XboxChildAccount, "xbox_child_account")]
    #[case::xbox_other(RefusalReason::XboxOther, "xbox_other")]
    #[case::profile_mismatch(RefusalReason::ProfileMismatch, "profile_mismatch")]
    fn reason_names_are_stable_and_round_trip(#[case] reason: RefusalReason, #[case] name: &str) {
        assert_eq!(reason.as_str(), name);
        assert_eq!(reason.to_string(), name);
        assert_eq!(name.parse::<RefusalReason>(), Ok(reason));
    }

    #[test]
    fn every_reason_is_listed_once_with_its_own_name() {
        let names: HashSet<&str> = RefusalReason::ALL
            .iter()
            .map(|reason| reason.as_str())
            .collect();

        assert_eq!(names.len(), RefusalReason::ALL.len());
        assert!(!names.contains(""));
    }

    #[rstest]
    #[case::wrong_case("Not_Owned")]
    #[case::hyphen("not-owned")]
    #[case::status_name("refused")]
    #[case::empty("")]
    fn unknown_reason_names_are_rejected(#[case] name: &str) {
        assert_eq!(
            name.parse::<RefusalReason>(),
            Err(AccountError::UnknownReason)
        );
    }

    #[rstest]
    #[case::pending(AccountStatus::Pending, "pending", None)]
    #[case::active(AccountStatus::Active, "active", None)]
    #[case::reauth_required(AccountStatus::ReauthRequired, "reauth_required", None)]
    #[case::refused(
        AccountStatus::Refused(RefusalReason::NotOwned),
        "refused",
        Some("not_owned")
    )]
    #[case::refused_mismatch(
        AccountStatus::Refused(RefusalReason::ProfileMismatch),
        "refused",
        Some("profile_mismatch")
    )]
    fn statuses_are_stored_in_two_columns_and_read_back(
        #[case] status: AccountStatus,
        #[case] name: &str,
        #[case] reason: Option<&str>,
    ) {
        assert_eq!(status.as_str(), name);
        assert_eq!(status.to_string(), name);
        assert_eq!(status.reason().map(RefusalReason::as_str), reason);
        assert_eq!(AccountStatus::from_columns(name, reason), Ok(status));
    }

    #[test]
    fn every_reason_reads_back_as_refused() {
        for reason in RefusalReason::ALL {
            let status = AccountStatus::Refused(reason);

            assert_eq!(
                AccountStatus::from_columns(status.as_str(), Some(reason.as_str())),
                Ok(status)
            );
        }
    }

    #[rstest]
    #[case::wrong_case("Active", None, AccountError::UnknownStatus)]
    #[case::hyphen("reauth-required", None, AccountError::UnknownStatus)]
    #[case::padded(" active", None, AccountError::UnknownStatus)]
    #[case::unknown("frozen", None, AccountError::UnknownStatus)]
    #[case::empty("", None, AccountError::UnknownStatus)]
    #[case::unknown_with_a_reason("frozen", Some("not_owned"), AccountError::UnknownStatus)]
    #[case::refused_without_a_reason("refused", None, AccountError::MissingReason)]
    #[case::refused_with_an_unknown_reason("refused", Some("banned"), AccountError::UnknownReason)]
    #[case::refused_with_an_empty_reason("refused", Some(""), AccountError::UnknownReason)]
    #[case::active_with_a_reason("active", Some("not_owned"), AccountError::UnexpectedReason)]
    #[case::pending_with_a_reason("pending", Some("no_profile"), AccountError::UnexpectedReason)]
    #[case::reauth_with_a_reason(
        "reauth_required",
        Some("not_owned"),
        AccountError::UnexpectedReason
    )]
    #[case::active_with_an_unknown_reason("active", Some("x"), AccountError::UnexpectedReason)]
    fn columns_that_dont_describe_a_status_are_rejected(
        #[case] status: &str,
        #[case] reason: Option<&str>,
        #[case] error: AccountError,
    ) {
        assert_eq!(AccountStatus::from_columns(status, reason), Err(error));
    }

    #[test]
    fn errors_name_no_value() {
        assert_eq!(
            AccountError::MissingReason.to_string(),
            "a refused account needs its reason"
        );
        assert_eq!(
            AccountError::UnexpectedReason.to_string(),
            "only a refused account has a reason"
        );
        assert_eq!(
            AccountError::UnknownReason.to_string(),
            "unknown refusal reason"
        );
    }

    // Linking.

    /// The start of every link test, in milliseconds.
    const T0: i64 = 1_800_000_000_000;

    fn at(millis: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(millis).unwrap()
    }

    fn hours(n: i64) -> i64 {
        n * 3_600_000
    }

    fn cap(n: u32) -> NonZeroU32 {
        NonZeroU32::new(n).unwrap()
    }

    #[rstest]
    #[case::member_below(Role::Member, 2, 3, false)]
    #[case::member_at(Role::Member, 3, 3, true)]
    #[case::member_above(Role::Member, 4, 3, true)]
    #[case::member_none_of_one(Role::Member, 0, 1, false)]
    #[case::admin_far_above(Role::Admin, 50, 3, false)]
    #[case::owner_far_above(Role::Owner, 50, 3, false)]
    fn a_member_at_the_quota_is_over_it_admins_and_the_owner_never(
        #[case] role: Role,
        #[case] owned: u32,
        #[case] max: u32,
        #[case] over: bool,
    ) {
        assert_eq!(over_quota(role, owned, max), over);
    }

    #[rstest]
    #[case::member(Role::Member, AccountStatus::Pending)]
    #[case::admin(Role::Admin, AccountStatus::Pending)]
    #[case::owner(Role::Owner, AccountStatus::Active)]
    fn the_owners_link_is_active_everyone_elses_pending(
        #[case] role: Role,
        #[case] status: AccountStatus,
    ) {
        assert_eq!(new_link_status(role), status);
    }

    #[test]
    fn the_window_is_a_day() {
        assert_eq!(LINK_START_WINDOW, Duration::from_hours(24));
    }

    #[test]
    fn starts_below_the_cap_need_no_wait() {
        let starts = [at(T0), at(T0 + hours(1))];

        assert_eq!(link_start_wait(&starts, cap(3), at(T0 + hours(2))), None);
        assert_eq!(link_start_wait(&[], cap(1), at(T0)), None);
    }

    #[test]
    fn the_tenth_start_in_24_h_waits_until_one_ages_out() {
        // Ten starts an hour apart; the first one is 9 h old at `now`.
        let starts: Vec<_> = (0..10).map(|n| at(T0 + hours(n))).collect();
        let now = at(T0 + hours(9));

        assert_eq!(
            link_start_wait(&starts, cap(10), now),
            Some(Duration::from_hours(15))
        );
    }

    #[test]
    fn the_order_of_the_starts_doesnt_matter() {
        let starts = [at(T0 + hours(5)), at(T0), at(T0 + hours(2))];

        assert_eq!(
            link_start_wait(&starts, cap(3), at(T0 + hours(6))),
            Some(Duration::from_hours(18))
        );
    }

    #[test]
    fn a_start_exactly_a_day_old_no_longer_counts() {
        let starts = [at(T0), at(T0 + hours(1))];

        assert_eq!(link_start_wait(&starts, cap(2), at(T0 + hours(24))), None);
        assert_eq!(
            link_start_wait(&starts, cap(2), at(T0 + hours(24) - 1)),
            Some(Duration::from_millis(1))
        );
    }

    #[test]
    fn a_lowered_cap_waits_until_enough_age_out() {
        // Five starts an hour apart and a cap lowered to 2: four must age out
        // to leave one, so the wait ends when the fourth oldest is a day old.
        let starts: Vec<_> = (0..5).map(|n| at(T0 + hours(n))).collect();
        let now = at(T0 + hours(5));

        assert_eq!(
            link_start_wait(&starts, cap(2), now),
            Some(Duration::from_hours(22))
        );
    }

    #[test]
    fn starts_from_the_future_count_and_wait_their_whole_day() {
        // A clock that went backwards: the start counts until it's a day
        // old by the clock.
        let starts = [at(T0 + hours(1))];

        assert_eq!(
            link_start_wait(&starts, cap(1), at(T0)),
            Some(Duration::from_hours(25))
        );
    }
}
