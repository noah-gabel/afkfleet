//! [`Username`]: the name a person logs in to the app with.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};

/// Why text isn't a valid [`Username`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UsernameError {
    /// The name isn't 3 to 32 characters long.
    #[error("a username has {min} to {max} characters, not {len}", min = Username::MIN_LEN, max = Username::MAX_LEN)]
    InvalidLength {
        /// The name's length in characters.
        len: usize,
    },
    /// The name contains a character other than ASCII letters, digits, `_`, `.` and `-`.
    #[error(
        "a username may only contain ASCII letters, digits, `_`, `.` and `-` (position {index})"
    )]
    InvalidChar {
        /// The character's position, counted in characters.
        index: usize,
    },
    /// The name doesn't start with a letter or digit.
    #[error("a username starts with a letter or digit")]
    InvalidStart,
}

/// The name a person logs in to the app with.
///
/// It has 3 to 32 characters from ASCII letters, digits, `_`, `.` and `-`,
/// starts with a letter or digit, and is stored in lowercase, so `Alice` and
/// `alice` are the same user. Restricting it to ASCII rules out look-alike
/// names built from other scripts (ADR-0010).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Username(String);

impl Username {
    /// The shortest valid name.
    pub const MIN_LEN: usize = 3;
    /// The longest valid name.
    pub const MAX_LEN: usize = 32;

    /// Returns the normalized (lowercase) name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for Username {
    type Error = UsernameError;

    fn try_from(text: &str) -> Result<Self, UsernameError> {
        let len = text.chars().count();
        if !(Self::MIN_LEN..=Self::MAX_LEN).contains(&len) {
            return Err(UsernameError::InvalidLength { len });
        }
        if let Some(index) = text
            .chars()
            .position(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')))
        {
            return Err(UsernameError::InvalidChar { index });
        }
        if !text.starts_with(|c: char| c.is_ascii_alphanumeric()) {
            return Err(UsernameError::InvalidStart);
        }
        Ok(Self(text.to_ascii_lowercase()))
    }
}

impl TryFrom<String> for Username {
    type Error = UsernameError;

    fn try_from(text: String) -> Result<Self, UsernameError> {
        Self::try_from(text.as_str())
    }
}

impl FromStr for Username {
    type Err = UsernameError;

    fn from_str(text: &str) -> Result<Self, UsernameError> {
        Self::try_from(text)
    }
}

impl From<Username> for String {
    fn from(name: Username) -> Self {
        name.0
    }
}

impl fmt::Display for Username {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    #[rstest]
    #[case::lowercase("alice", "alice")]
    #[case::normalized("Alice", "alice")]
    #[case::all_allowed_punctuation("Bob.Smith-2_x", "bob.smith-2_x")]
    #[case::digit_first("2fast", "2fast")]
    #[case::shortest("abc", "abc")]
    #[case::longest("a2345678901234567890123456789012", "a2345678901234567890123456789012")]
    fn accepts_and_lowercases(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(Username::try_from(input).unwrap().as_str(), expected);
    }

    #[test]
    fn different_case_is_the_same_user() {
        assert_eq!(Username::try_from("ALICE"), Username::try_from("alice"));
    }

    #[rstest]
    #[case::empty("", UsernameError::InvalidLength { len: 0 })]
    #[case::too_short("ab", UsernameError::InvalidLength { len: 2 })]
    #[case::too_long("a23456789012345678901234567890123", UsernameError::InvalidLength { len: 33 })]
    #[case::space("al ice", UsernameError::InvalidChar { index: 2 })]
    #[case::non_ascii_letter("alïce", UsernameError::InvalidChar { index: 2 })]
    #[case::cyrillic_look_alike("\u{0430}lice", UsernameError::InvalidChar { index: 0 })]
    #[case::at_sign("alice@home", UsernameError::InvalidChar { index: 5 })]
    #[case::leading_underscore("_alice", UsernameError::InvalidStart)]
    #[case::leading_dot(".bob", UsernameError::InvalidStart)]
    #[case::leading_hyphen("-bob", UsernameError::InvalidStart)]
    fn rejects(#[case] input: &str, #[case] error: UsernameError) {
        assert_eq!(Username::try_from(input), Err(error));
    }

    #[test]
    fn deserialize_rejects_an_invalid_name() {
        assert!(serde_json::from_str::<Username>("\"_x\"").is_err());
    }

    proptest! {
        #[test]
        fn never_panics(text in any::<String>()) {
            let _ = Username::try_from(text.as_str());
        }

        #[test]
        fn valid_names_round_trip(text in "[A-Za-z0-9][A-Za-z0-9_.-]{2,31}") {
            let name = Username::try_from(text.as_str()).unwrap();
            prop_assert_eq!(name.as_str(), text.to_ascii_lowercase());
            prop_assert_eq!(Username::try_from(name.to_string()), Ok(name));
        }
    }
}
