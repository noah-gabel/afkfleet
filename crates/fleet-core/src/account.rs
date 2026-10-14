//! [`AccountKind`] and [`AccountStatus`]: what a linked Minecraft account is
//! and where it stands, kept in `mc_accounts.kind` and `mc_accounts.status`
//! (Plan.md P9.1, ADR-0018).
//!
//! A Microsoft account signs in through Microsoft and holds a refresh token
//! in the server's vault. An offline account is only a name, for
//! offline-mode servers in development (P9.7). Only a Microsoft account has
//! a status other than [`AccountStatus::Active`]: a new link waits for
//! approval, and a refresh token Microsoft refuses needs the account linked
//! again.

use core::fmt;
use core::str::FromStr;

/// Why a stored name isn't a known account kind or status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AccountError {
    /// The name isn't one of the [`AccountKind`] names.
    #[error("unknown account kind")]
    UnknownKind,
    /// The name isn't one of the [`AccountStatus`] names.
    #[error("unknown account status")]
    UnknownStatus,
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

/// Where an account stands, stored in `mc_accounts.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountStatus {
    /// Linked, and waiting for someone above the linker to approve it; no bot
    /// can use it yet.
    Pending,
    /// Usable.
    Active,
    /// Microsoft refused its refresh token: it must be linked again.
    ReauthRequired,
}

impl AccountStatus {
    /// Returns the stable name stored in the `status` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::ReauthRequired => "reauth_required",
        }
    }
}

impl FromStr for AccountStatus {
    type Err = AccountError;

    fn from_str(name: &str) -> Result<Self, AccountError> {
        match name {
            "pending" => Ok(Self::Pending),
            "active" => Ok(Self::Active),
            "reauth_required" => Ok(Self::ReauthRequired),
            _ => Err(AccountError::UnknownStatus),
        }
    }
}

impl fmt::Display for AccountStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
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
    #[case::pending(AccountStatus::Pending, "pending")]
    #[case::active(AccountStatus::Active, "active")]
    #[case::reauth_required(AccountStatus::ReauthRequired, "reauth_required")]
    fn status_names_are_stable_and_round_trip(#[case] status: AccountStatus, #[case] name: &str) {
        assert_eq!(status.as_str(), name);
        assert_eq!(status.to_string(), name);
        assert_eq!(name.parse::<AccountStatus>(), Ok(status));
    }

    #[rstest]
    #[case::wrong_case("Active")]
    #[case::hyphen("reauth-required")]
    #[case::padded(" active")]
    #[case::unknown("frozen")]
    #[case::empty("")]
    fn unknown_status_names_are_rejected(#[case] name: &str) {
        assert_eq!(
            name.parse::<AccountStatus>(),
            Err(AccountError::UnknownStatus)
        );
    }
}
