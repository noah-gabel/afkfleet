//! [`AuditAction`]: what an audit entry records, as a stable dotted name.

use core::fmt;
use std::borrow::Cow;

use super::AuditError;
use super::name::{self, Dots};

/// What happened, as a dotted name like `security.refresh_token_reuse`:
/// two or more parts of `a`–`z`, `0`–`9` and `_`, separated by dots, at most
/// [`AuditAction::MAX_LEN`] characters.
///
/// Code that records an entry uses only the constants each phase lists in
/// one `audit_actions!` block, and [`AuditAction::ALL`] holds them all, so
/// one test checks that every action is valid and that no two share a name.
/// Group B lists none yet; P7.13, P9 and P11 add theirs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AuditAction(Cow<'static, str>);

impl AuditAction {
    /// The longest name, in characters.
    pub const MAX_LEN: usize = 64;

    /// Returns the stable name stored in the `action` column.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Defines the listed actions as constants, and [`AuditAction::ALL`]. The
/// names aren't checked here: the list's test checks every one.
macro_rules! audit_actions {
    ($($(#[$doc:meta])* $constant:ident = $name:literal;)*) => {
        impl AuditAction {
            $(
                $(#[$doc])*
                pub const $constant: Self = Self(Cow::Borrowed($name));
            )*

            /// Every listed action, for the test that checks them all.
            pub const ALL: &'static [Self] = &[$(Self::$constant),*];
        }
    };
}

audit_actions! {}

/// For reading rows back. Code that records an entry uses the listed
/// constants instead.
impl TryFrom<&str> for AuditAction {
    type Error = AuditError;

    fn try_from(text: &str) -> Result<Self, AuditError> {
        name::check(text, Self::MAX_LEN, Dots::Required)?;
        Ok(Self(Cow::Owned(text.to_owned())))
    }
}

impl fmt::Display for AuditAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use rstest::rstest;

    use super::*;

    /// Why a list of actions isn't acceptable: an invalid name, or two
    /// actions with the same name, which would make the log ambiguous.
    fn list_problems(list: &[AuditAction]) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut problems = Vec::new();
        for action in list {
            if let Err(error) = AuditAction::try_from(action.as_str()) {
                problems.push(format!("{}: {error}", action.as_str()));
            }
            if !seen.insert(action.as_str()) {
                problems.push(format!("{}: listed twice", action.as_str()));
            }
        }
        problems
    }

    #[test]
    fn every_listed_action_is_valid_and_unique() {
        assert_eq!(list_problems(AuditAction::ALL), Vec::<String>::new());
    }

    #[test]
    fn the_list_check_finds_invalid_and_duplicate_names() {
        let list = [
            AuditAction(Cow::Borrowed("user.create")),
            AuditAction(Cow::Borrowed("User.Create")),
            AuditAction(Cow::Borrowed("user.create")),
        ];

        assert_eq!(
            list_problems(&list),
            [
                "User.Create: a name may only use a-z, 0-9 and _; character 0 isn't one of them",
                "user.create: listed twice",
            ]
        );
    }

    #[rstest]
    #[case::two_parts("user.create")]
    #[case::three_parts("security.refresh_token.reuse")]
    #[case::digits_and_underscores("p7_13.login_2fa")]
    fn valid_actions_read_back(#[case] text: &str) {
        let action = AuditAction::try_from(text).unwrap();

        assert_eq!(action.as_str(), text);
        assert_eq!(action.to_string(), text);
    }

    #[test]
    fn the_length_limit_is_64_characters() {
        let longest = format!("a.{}", "b".repeat(62));
        let too_long = format!("a.{}", "b".repeat(63));

        assert_eq!(
            AuditAction::try_from(longest.as_str()).unwrap().as_str(),
            longest
        );
        assert_eq!(
            AuditAction::try_from(too_long.as_str()),
            Err(AuditError::NameLength { len: 65, max: 64 })
        );
    }

    #[rstest]
    #[case::empty("", AuditError::NameLength { len: 0, max: 64 })]
    #[case::one_part("login", AuditError::ActionShape)]
    #[case::empty_part("user..create", AuditError::ActionShape)]
    #[case::leading_dot(".user", AuditError::ActionShape)]
    #[case::trailing_dot("user.", AuditError::ActionShape)]
    #[case::uppercase("user.Create", AuditError::NameChar { index: 5 })]
    #[case::space("user create", AuditError::NameChar { index: 4 })]
    #[case::dash("user.re-create", AuditError::NameChar { index: 7 })]
    #[case::non_ascii("user.créate", AuditError::NameChar { index: 7 })]
    fn invalid_actions_are_refused(#[case] text: &str, #[case] expected: AuditError) {
        assert_eq!(AuditAction::try_from(text), Err(expected));
    }
}
