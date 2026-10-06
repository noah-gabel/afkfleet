//! [`McUsername`]: a Minecraft player name.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};

/// Why text isn't a valid [`McUsername`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum McUsernameError {
    /// The name isn't 3 to 16 characters long.
    #[error("a Minecraft name has {min} to {max} characters, not {len}", min = McUsername::MIN_LEN, max = McUsername::MAX_LEN)]
    InvalidLength {
        /// The name's length in characters.
        len: usize,
    },
    /// The name contains a character other than `A`–`Z`, `a`–`z`, `0`–`9` and `_`.
    #[error("a Minecraft name may only contain letters, digits and `_` (position {index})")]
    InvalidChar {
        /// The character's position, counted in characters.
        index: usize,
    },
}

/// A Minecraft player name: 3 to 16 characters from `[A-Za-z0-9_]`.
///
/// The case is kept as given, since Minecraft displays it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct McUsername(String);

impl McUsername {
    /// The shortest valid name.
    pub const MIN_LEN: usize = 3;
    /// The longest valid name.
    pub const MAX_LEN: usize = 16;

    /// Returns the name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for McUsername {
    type Error = McUsernameError;

    fn try_from(text: &str) -> Result<Self, McUsernameError> {
        let len = text.chars().count();
        if !(Self::MIN_LEN..=Self::MAX_LEN).contains(&len) {
            return Err(McUsernameError::InvalidLength { len });
        }
        if let Some(index) = text
            .chars()
            .position(|c| !(c.is_ascii_alphanumeric() || c == '_'))
        {
            return Err(McUsernameError::InvalidChar { index });
        }
        Ok(Self(text.to_owned()))
    }
}

impl TryFrom<String> for McUsername {
    type Error = McUsernameError;

    fn try_from(text: String) -> Result<Self, McUsernameError> {
        Self::try_from(text.as_str())
    }
}

impl FromStr for McUsername {
    type Err = McUsernameError;

    fn from_str(text: &str) -> Result<Self, McUsernameError> {
        Self::try_from(text)
    }
}

impl From<McUsername> for String {
    fn from(name: McUsername) -> Self {
        name.0
    }
}

impl fmt::Display for McUsername {
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
    #[case::typical("AfkBot1")]
    #[case::shortest("abc")]
    #[case::underscore("a_b")]
    #[case::longest("A234567890123456")]
    fn accepts(#[case] input: &str) {
        assert_eq!(McUsername::try_from(input).unwrap().as_str(), input);
    }

    #[rstest]
    #[case::empty("", McUsernameError::InvalidLength { len: 0 })]
    #[case::too_short("ab", McUsernameError::InvalidLength { len: 2 })]
    #[case::too_long("A2345678901234567", McUsernameError::InvalidLength { len: 17 })]
    #[case::hyphen("afk-bot", McUsernameError::InvalidChar { index: 3 })]
    #[case::space("afk bot", McUsernameError::InvalidChar { index: 3 })]
    #[case::umlaut("äfk", McUsernameError::InvalidChar { index: 0 })]
    #[case::newline("afk\n", McUsernameError::InvalidChar { index: 3 })]
    fn rejects(#[case] input: &str, #[case] error: McUsernameError) {
        assert_eq!(McUsername::try_from(input), Err(error));
    }

    #[test]
    fn deserialize_rejects_an_invalid_name() {
        assert!(serde_json::from_str::<McUsername>("\"a b\"").is_err());
    }

    proptest! {
        #[test]
        fn never_panics(text in any::<String>()) {
            let _ = McUsername::try_from(text.as_str());
        }

        #[test]
        fn valid_names_round_trip(text in "[A-Za-z0-9_]{3,16}") {
            let name = McUsername::try_from(text.as_str()).unwrap();
            prop_assert_eq!(name.to_string(), text);
        }
    }
}
