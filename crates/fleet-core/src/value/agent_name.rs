//! [`AgentName`]: the label an agent goes by.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};

/// Why text isn't a valid [`AgentName`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AgentNameError {
    /// The name isn't 1 to 64 characters long.
    #[error("an agent name has {min} to {max} characters, not {len}", min = AgentName::MIN_LEN, max = AgentName::MAX_LEN)]
    InvalidLength {
        /// The name's length in characters.
        len: usize,
    },
    /// The name contains a character other than ASCII letters, digits, `-`, `_` and `.`.
    #[error(
        "an agent name may only contain ASCII letters, digits, `-`, `_` and `.` (position {index})"
    )]
    InvalidChar {
        /// The character's position, counted in characters.
        index: usize,
    },
    /// The name doesn't start with a letter or digit.
    #[error("an agent name starts with a letter or digit")]
    InvalidStart,
}

/// The label an agent goes by: in its logs, and later in the app
/// (`agents.name`, P10).
///
/// It has 1 to 64 characters from ASCII letters, digits, `-`, `_` and `.`,
/// and starts with a letter or digit, so it's safe in logs and file names:
/// `.`, `..` and hidden-file names like `.agent` are refused. The case is
/// kept as given.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentName(String);

impl AgentName {
    /// The shortest valid name.
    pub const MIN_LEN: usize = 1;
    /// The longest valid name.
    pub const MAX_LEN: usize = 64;

    /// Returns the name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for AgentName {
    type Error = AgentNameError;

    fn try_from(text: &str) -> Result<Self, AgentNameError> {
        let len = text.chars().count();
        if !(Self::MIN_LEN..=Self::MAX_LEN).contains(&len) {
            return Err(AgentNameError::InvalidLength { len });
        }
        if let Some(index) = text
            .chars()
            .position(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        {
            return Err(AgentNameError::InvalidChar { index });
        }
        if !text.starts_with(|c: char| c.is_ascii_alphanumeric()) {
            return Err(AgentNameError::InvalidStart);
        }
        Ok(Self(text.to_owned()))
    }
}

impl TryFrom<String> for AgentName {
    type Error = AgentNameError;

    fn try_from(text: String) -> Result<Self, AgentNameError> {
        Self::try_from(text.as_str())
    }
}

impl FromStr for AgentName {
    type Err = AgentNameError;

    fn from_str(text: &str) -> Result<Self, AgentNameError> {
        Self::try_from(text)
    }
}

impl From<AgentName> for String {
    fn from(name: AgentName) -> Self {
        name.0
    }
}

impl fmt::Display for AgentName {
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
    #[case::typical("agent-1")]
    #[case::shortest_letter("a")]
    #[case::shortest_digit("1")]
    #[case::all_allowed_punctuation("eu.agent_2-b")]
    #[case::case_kept("Agent-EU")]
    #[case::longest("a234567890123456789012345678901234567890123456789012345678901234")]
    fn accepts(#[case] input: &str) {
        assert_eq!(AgentName::try_from(input).unwrap().as_str(), input);
    }

    #[rstest]
    #[case::empty("", AgentNameError::InvalidLength { len: 0 })]
    #[case::too_long(
        "a2345678901234567890123456789012345678901234567890123456789012345",
        AgentNameError::InvalidLength { len: 65 }
    )]
    #[case::space("agent 1", AgentNameError::InvalidChar { index: 5 })]
    #[case::slash("eu/agent", AgentNameError::InvalidChar { index: 2 })]
    #[case::newline("a\n", AgentNameError::InvalidChar { index: 1 })]
    #[case::non_ascii_first("äx", AgentNameError::InvalidChar { index: 0 })]
    #[case::dot(".", AgentNameError::InvalidStart)]
    #[case::dot_dot("..", AgentNameError::InvalidStart)]
    #[case::hidden_file(".agent", AgentNameError::InvalidStart)]
    #[case::leading_hyphen("-a", AgentNameError::InvalidStart)]
    #[case::leading_underscore("_a", AgentNameError::InvalidStart)]
    fn rejects(#[case] input: &str, #[case] error: AgentNameError) {
        assert_eq!(AgentName::try_from(input), Err(error));
    }

    #[test]
    fn errors_have_fixed_messages() {
        assert_eq!(
            AgentNameError::InvalidLength { len: 65 }.to_string(),
            "an agent name has 1 to 64 characters, not 65"
        );
        assert_eq!(
            AgentNameError::InvalidChar { index: 5 }.to_string(),
            "an agent name may only contain ASCII letters, digits, `-`, `_` and `.` (position 5)"
        );
        assert_eq!(
            AgentNameError::InvalidStart.to_string(),
            "an agent name starts with a letter or digit"
        );
    }

    #[test]
    fn deserialize_goes_through_the_constructor() {
        assert_eq!(
            serde_json::from_str::<AgentName>("\"agent-1\"")
                .unwrap()
                .as_str(),
            "agent-1"
        );
        assert!(serde_json::from_str::<AgentName>("\"_x\"").is_err());
        assert_eq!(
            serde_json::to_string(&AgentName::try_from("agent-1").unwrap()).unwrap(),
            "\"agent-1\""
        );
    }

    proptest! {
        #[test]
        fn never_panics(text in any::<String>()) {
            let _ = AgentName::try_from(text.as_str());
        }

        #[test]
        fn valid_names_round_trip(text in "[A-Za-z0-9][A-Za-z0-9_.-]{0,63}") {
            let name = AgentName::try_from(text.as_str()).unwrap();
            prop_assert_eq!(name.as_str(), text.as_str());
            prop_assert_eq!(AgentName::try_from(name.to_string()), Ok(name));
        }
    }
}
