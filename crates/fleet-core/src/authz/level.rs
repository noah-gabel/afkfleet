//! [`Role`], [`GrantLevel`] and [`ModeVisibility`]: the names authorization
//! keeps in SQL columns.

use core::fmt;
use core::str::FromStr;

/// Why a stored name isn't a known role, grant level or mode visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LevelError {
    /// The name isn't one of the [`Role`] names.
    #[error("unknown role")]
    UnknownRole,
    /// The name isn't one of the [`GrantLevel`] names.
    #[error("unknown grant level")]
    UnknownGrantLevel,
    /// The name isn't one of the [`ModeVisibility`] names.
    #[error("unknown mode visibility")]
    UnknownModeVisibility,
}

/// A user's global role, stored in `users.role`.
///
/// Roles are ordered by what they may do: `Member < Admin < Owner`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    /// Manages their own Minecraft accounts and the ones shared with them.
    Member,
    /// Also manages Members, Member invites and agents, and has implicit
    /// Manage on Members' accounts, but not on the Owner's or other Admins'.
    Admin,
    /// Exactly one user. Manages everything, including Admins.
    Owner,
}

impl Role {
    /// Returns the stable name stored in the `role` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Admin => "admin",
            Self::Owner => "owner",
        }
    }
}

impl FromStr for Role {
    type Err = LevelError;

    fn from_str(name: &str) -> Result<Self, LevelError> {
        match name {
            "member" => Ok(Self::Member),
            "admin" => Ok(Self::Admin),
            "owner" => Ok(Self::Owner),
            _ => Err(LevelError::UnknownRole),
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A user's level on one Minecraft account and its bot, stored in
/// `account_grants.level`.
///
/// Levels are ordered, and each includes the ones below it:
/// `View < Control < Manage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GrantLevel {
    /// See the bot's status and chat history.
    View,
    /// Start and stop the bot, change its mode, send chat and allowlisted
    /// commands.
    Control,
    /// Send any command, change the server, share, re-link and delete the
    /// account.
    Manage,
}

impl GrantLevel {
    /// Returns the stable name stored in the `level` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Control => "control",
            Self::Manage => "manage",
        }
    }
}

impl FromStr for GrantLevel {
    type Err = LevelError;

    fn from_str(name: &str) -> Result<Self, LevelError> {
        match name {
            "view" => Ok(Self::View),
            "control" => Ok(Self::Control),
            "manage" => Ok(Self::Manage),
            _ => Err(LevelError::UnknownGrantLevel),
        }
    }
}

impl fmt::Display for GrantLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Who may see a mode, stored in `modes.visibility`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModeVisibility {
    /// Its owner, and whoever manages the owner's things.
    Private,
    /// Everyone.
    Shared,
}

impl ModeVisibility {
    /// Returns the stable name stored in the `visibility` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Shared => "shared",
        }
    }
}

impl FromStr for ModeVisibility {
    type Err = LevelError;

    fn from_str(name: &str) -> Result<Self, LevelError> {
        match name {
            "private" => Ok(Self::Private),
            "shared" => Ok(Self::Shared),
            _ => Err(LevelError::UnknownModeVisibility),
        }
    }
}

impl fmt::Display for ModeVisibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::member(Role::Member, "member")]
    #[case::admin(Role::Admin, "admin")]
    #[case::owner(Role::Owner, "owner")]
    fn role_names_are_stable_and_round_trip(#[case] role: Role, #[case] name: &str) {
        assert_eq!(role.as_str(), name);
        assert_eq!(role.to_string(), name);
        assert_eq!(name.parse::<Role>(), Ok(role));
    }

    #[rstest]
    #[case::wrong_case("Admin")]
    #[case::unknown("superuser")]
    #[case::padded(" admin")]
    #[case::empty("")]
    fn unknown_role_names_are_rejected(#[case] name: &str) {
        assert_eq!(name.parse::<Role>(), Err(LevelError::UnknownRole));
    }

    #[rstest]
    #[case::view(GrantLevel::View, "view")]
    #[case::control(GrantLevel::Control, "control")]
    #[case::manage(GrantLevel::Manage, "manage")]
    fn grant_level_names_are_stable_and_round_trip(#[case] level: GrantLevel, #[case] name: &str) {
        assert_eq!(level.as_str(), name);
        assert_eq!(level.to_string(), name);
        assert_eq!(name.parse::<GrantLevel>(), Ok(level));
    }

    #[rstest]
    #[case::wrong_case("Manage")]
    #[case::unknown("owner")]
    #[case::empty("")]
    fn unknown_grant_level_names_are_rejected(#[case] name: &str) {
        assert_eq!(
            name.parse::<GrantLevel>(),
            Err(LevelError::UnknownGrantLevel)
        );
    }

    #[rstest]
    #[case::private(ModeVisibility::Private, "private")]
    #[case::shared(ModeVisibility::Shared, "shared")]
    fn mode_visibility_names_are_stable_and_round_trip(
        #[case] visibility: ModeVisibility,
        #[case] name: &str,
    ) {
        assert_eq!(visibility.as_str(), name);
        assert_eq!(visibility.to_string(), name);
        assert_eq!(name.parse::<ModeVisibility>(), Ok(visibility));
    }

    #[rstest]
    #[case::wrong_case("Shared")]
    #[case::unknown("public")]
    #[case::empty("")]
    fn unknown_mode_visibility_names_are_rejected(#[case] name: &str) {
        assert_eq!(
            name.parse::<ModeVisibility>(),
            Err(LevelError::UnknownModeVisibility)
        );
    }

    #[test]
    fn roles_are_ordered_by_power() {
        assert!(Role::Member < Role::Admin);
        assert!(Role::Admin < Role::Owner);
    }

    #[test]
    fn grant_levels_are_ordered_view_control_manage() {
        assert!(GrantLevel::View < GrantLevel::Control);
        assert!(GrantLevel::Control < GrantLevel::Manage);
    }
}
