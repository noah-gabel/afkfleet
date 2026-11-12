//! [`UsernamePrefix`]: the start of a username, as someone types it (Plan.md
//! P11.11; ADR-0020, U110, S127).

use core::fmt;

use super::username::Username;

/// Why text can't be the start of a username.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UsernamePrefixError {
    /// It's empty, or longer than a username can be.
    #[error("the start of a username has {min} to {max} characters, not {len}", min = UsernamePrefix::MIN_LEN, max = UsernamePrefix::MAX_LEN)]
    InvalidLength {
        /// Its length in characters.
        len: usize,
    },
    /// It holds a character no username holds.
    #[error(
        "a username may only contain ASCII letters, digits, `_`, `.` and `-` (position {index})"
    )]
    InvalidChar {
        /// The character's position, counted in characters.
        index: usize,
    },
    /// It doesn't start with a letter or digit, as every username does.
    #[error("a username starts with a letter or digit")]
    InvalidStart,
}

/// The start of a username: 1 to 32 of the characters a username holds,
/// beginning as one does, in lowercase, as usernames are stored.
///
/// What someone typed may be a password in the wrong field, so `Debug` never
/// shows it and there's no `Display`.
#[derive(Clone, PartialEq, Eq)]
pub struct UsernamePrefix(String);

impl UsernamePrefix {
    /// The shortest: one character is enough to start suggesting.
    pub const MIN_LEN: usize = 1;
    /// The longest: a whole username.
    pub const MAX_LEN: usize = Username::MAX_LEN;

    /// The prefix, in lowercase.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first text after every name starting with this: its last character
    /// moved up by one. The names lie in `as_str()..upper_bound()`, a range a
    /// query can bind, in which `_` and the like mean only themselves.
    #[must_use]
    pub fn upper_bound(&self) -> String {
        let mut bound = self.0.clone();
        // It's never empty, and every character is ASCII, so this moves one up.
        if let Some(moved) = bound
            .pop()
            .and_then(|last| char::from_u32(u32::from(last) + 1))
        {
            bound.push(moved);
        }
        bound
    }
}

/// Redacted: see the type's docs.
impl fmt::Debug for UsernamePrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UsernamePrefix([redacted])")
    }
}

impl TryFrom<&str> for UsernamePrefix {
    type Error = UsernamePrefixError;

    fn try_from(text: &str) -> Result<Self, UsernamePrefixError> {
        let len = text.chars().count();
        if !(Self::MIN_LEN..=Self::MAX_LEN).contains(&len) {
            return Err(UsernamePrefixError::InvalidLength { len });
        }
        if let Some(index) = text
            .chars()
            .position(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')))
        {
            return Err(UsernamePrefixError::InvalidChar { index });
        }
        if !text.starts_with(|c: char| c.is_ascii_alphanumeric()) {
            return Err(UsernamePrefixError::InvalidStart);
        }
        Ok(Self(text.to_ascii_lowercase()))
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::one_letter("a", "a")]
    #[case::one_digit("7", "7")]
    #[case::lowercased("USER", "user")]
    #[case::punctuation_after_the_start("a_B.c-", "a_b.c-")]
    #[case::a_whole_name("a2345678901234567890123456789012", "a2345678901234567890123456789012")]
    fn accepts_and_lowercases(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(UsernamePrefix::try_from(input).unwrap().as_str(), expected);
    }

    #[rstest]
    #[case::empty("", UsernamePrefixError::InvalidLength { len: 0 })]
    #[case::too_long("a23456789012345678901234567890123", UsernamePrefixError::InvalidLength { len: 33 })]
    #[case::space("a b", UsernamePrefixError::InvalidChar { index: 1 })]
    #[case::pattern_percent("a%", UsernamePrefixError::InvalidChar { index: 1 })]
    #[case::non_ascii("\u{e4}a", UsernamePrefixError::InvalidChar { index: 0 })]
    #[case::leading_underscore("_a", UsernamePrefixError::InvalidStart)]
    #[case::leading_dot(".", UsernamePrefixError::InvalidStart)]
    #[case::leading_hyphen("-a", UsernamePrefixError::InvalidStart)]
    fn rejects(#[case] input: &str, #[case] error: UsernamePrefixError) {
        assert_eq!(UsernamePrefix::try_from(input), Err(error));
    }

    #[rstest]
    #[case::a_letter("user", "uses")]
    #[case::the_last_letter("az", "a{")]
    #[case::a_digit("a9", "a:")]
    #[case::an_underscore("a_", "a`")]
    #[case::a_dot("a.", "a/")]
    #[case::a_hyphen("a-", "a.")]
    #[case::one_character("z", "{")]
    fn its_upper_bound_moves_the_last_character_up(#[case] input: &str, #[case] bound: &str) {
        assert_eq!(
            UsernamePrefix::try_from(input).unwrap().upper_bound(),
            bound
        );
    }

    #[test]
    fn debug_never_shows_what_was_typed() {
        let typed = UsernamePrefix::try_from("hunter2").unwrap();

        assert_eq!(format!("{typed:?}"), "UsernamePrefix([redacted])");
    }

    proptest! {
        #[test]
        fn never_panics(text in any::<String>()) {
            let _ = UsernamePrefix::try_from(text.as_str());
        }

        /// Every start of a valid username is a prefix, and that username lies
        /// in its range.
        #[test]
        fn every_start_of_a_username_finds_it(
            text in "[A-Za-z0-9][A-Za-z0-9_.-]{2,31}",
            cut in 1_usize..=32,
        ) {
            let name = Username::try_from(text.as_str()).unwrap();
            let end = cut.min(name.as_str().len());
            let start: String = name.as_str().chars().take(end).collect();
            let prefix = UsernamePrefix::try_from(start.as_str()).unwrap();

            prop_assert!(name.as_str() >= prefix.as_str());
            prop_assert!(name.as_str() < prefix.upper_bound().as_str());
        }

        /// A name that doesn't start with the prefix lies outside its range.
        #[test]
        fn no_other_name_is_in_its_range(
            start in "[a-z0-9][a-z0-9_.-]{0,3}",
            name in "[a-z0-9][a-z0-9_.-]{2,6}",
        ) {
            prop_assume!(!name.starts_with(start.as_str()));
            let prefix = UsernamePrefix::try_from(start.as_str()).unwrap();

            prop_assert!(!(name.as_str() >= prefix.as_str() && name.as_str() < prefix.upper_bound().as_str()));
        }
    }
}
