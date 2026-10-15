//! [`AgentVersion`]: the version an agent reports.

use core::fmt;
use core::str::FromStr;

/// Why text isn't a valid [`AgentVersion`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AgentVersionError {
    /// The version isn't 1 to 64 characters long.
    #[error("an agent version has {min} to {max} characters, not {len}", min = AgentVersion::MIN_LEN, max = AgentVersion::MAX_LEN)]
    InvalidLength {
        /// The version's length in characters.
        len: usize,
    },
    /// The version contains a character other than ASCII letters, digits, `.`, `+` and `-`.
    #[error(
        "an agent version may only contain ASCII letters, digits, `.`, `+` and `-` (position {index})"
    )]
    InvalidChar {
        /// The character's position, counted in characters.
        index: usize,
    },
    /// The version doesn't start with a letter or digit.
    #[error("an agent version starts with a letter or digit")]
    InvalidStart,
}

/// The version an agent reports in its hello, e.g. `0.1.0` or
/// `1.2.0-rc.1+build.5` (Plan.md P10.1; ADR-0019). The server stores it with
/// the agent (`agents.version`) and shows it to Admins.
///
/// It has 1 to 64 characters from ASCII letters, digits, `.`, `+` and `-`,
/// enough for any semantic version, and starts with a letter or digit, so
/// it's safe in logs. Only the protocol version decides whether an agent may
/// connect; this one is for people.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentVersion(String);

impl AgentVersion {
    /// The shortest valid version.
    pub const MIN_LEN: usize = 1;
    /// The longest valid version.
    pub const MAX_LEN: usize = 64;

    /// Returns the version.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for AgentVersion {
    type Error = AgentVersionError;

    fn try_from(text: &str) -> Result<Self, AgentVersionError> {
        let len = text.chars().count();
        if !(Self::MIN_LEN..=Self::MAX_LEN).contains(&len) {
            return Err(AgentVersionError::InvalidLength { len });
        }
        if let Some(index) = text
            .chars()
            .position(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-')))
        {
            return Err(AgentVersionError::InvalidChar { index });
        }
        if !text.starts_with(|c: char| c.is_ascii_alphanumeric()) {
            return Err(AgentVersionError::InvalidStart);
        }
        Ok(Self(text.to_owned()))
    }
}

impl TryFrom<String> for AgentVersion {
    type Error = AgentVersionError;

    fn try_from(text: String) -> Result<Self, AgentVersionError> {
        Self::try_from(text.as_str())
    }
}

impl FromStr for AgentVersion {
    type Err = AgentVersionError;

    fn from_str(text: &str) -> Result<Self, AgentVersionError> {
        Self::try_from(text)
    }
}

impl From<AgentVersion> for String {
    fn from(version: AgentVersion) -> Self {
        version.0
    }
}

impl fmt::Display for AgentVersion {
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
    #[case::release("0.1.0")]
    #[case::shortest("1")]
    #[case::pre_release_and_build("1.2.0-rc.1+build.5")]
    #[case::letters("dev")]
    #[case::longest("1234567890123456789012345678901234567890123456789012345678901234")]
    fn accepts(#[case] input: &str) {
        assert_eq!(AgentVersion::try_from(input).unwrap().as_str(), input);
        assert_eq!(input.parse::<AgentVersion>().unwrap().to_string(), input);
    }

    #[rstest]
    #[case::empty("", AgentVersionError::InvalidLength { len: 0 })]
    #[case::too_long(
        "12345678901234567890123456789012345678901234567890123456789012345",
        AgentVersionError::InvalidLength { len: 65 }
    )]
    #[case::non_ascii("1.ä", AgentVersionError::InvalidChar { index: 2 })]
    #[case::space("0.1.0 beta", AgentVersionError::InvalidChar { index: 5 })]
    #[case::underscore("0_1", AgentVersionError::InvalidChar { index: 1 })]
    #[case::newline("0.1\n", AgentVersionError::InvalidChar { index: 3 })]
    #[case::leading_dot(".1", AgentVersionError::InvalidStart)]
    #[case::leading_plus("+1", AgentVersionError::InvalidStart)]
    #[case::leading_hyphen("-1", AgentVersionError::InvalidStart)]
    fn rejects(#[case] input: &str, #[case] error: AgentVersionError) {
        assert_eq!(AgentVersion::try_from(input), Err(error));
    }

    #[test]
    fn the_length_counts_characters() {
        let long = "ä".repeat(33);
        assert_eq!(
            AgentVersion::try_from(long.as_str()),
            Err(AgentVersionError::InvalidChar { index: 0 })
        );
        let too_long = "1".repeat(65);
        assert_eq!(
            AgentVersion::try_from(too_long),
            Err(AgentVersionError::InvalidLength { len: 65 })
        );
    }

    #[test]
    fn converts_back_into_its_text() {
        let version = AgentVersion::try_from("0.1.0").unwrap();
        assert_eq!(String::from(version), "0.1.0");
    }

    #[test]
    fn errors_have_fixed_messages() {
        assert_eq!(
            AgentVersionError::InvalidLength { len: 65 }.to_string(),
            "an agent version has 1 to 64 characters, not 65"
        );
        assert_eq!(
            AgentVersionError::InvalidChar { index: 5 }.to_string(),
            "an agent version may only contain ASCII letters, digits, `.`, `+` and `-` (position 5)"
        );
        assert_eq!(
            AgentVersionError::InvalidStart.to_string(),
            "an agent version starts with a letter or digit"
        );
    }

    proptest! {
        #[test]
        fn never_panics(text in any::<String>()) {
            let _ = AgentVersion::try_from(text.as_str());
        }

        #[test]
        fn valid_versions_round_trip(text in "[A-Za-z0-9][A-Za-z0-9.+-]{0,63}") {
            let version = AgentVersion::try_from(text.as_str()).unwrap();
            prop_assert_eq!(version.as_str(), text.as_str());
            prop_assert_eq!(AgentVersion::try_from(version.to_string()), Ok(version));
        }
    }
}
