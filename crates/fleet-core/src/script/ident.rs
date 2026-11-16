//! [`Ident`]: a name a mode's declaration gives something.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};

/// Why text isn't an [`Ident`]. It never holds the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IdentError {
    /// The name is empty.
    #[error("the name is empty")]
    Empty,
    /// The name is longer than [`Ident::MAX_LEN`] characters.
    #[error("the name is {chars} characters long; the limit is {max}", max = Ident::MAX_LEN)]
    TooLong {
        /// Its length in characters.
        chars: usize,
    },
    /// The name holds a character other than `a`–`z`, `0`–`9` and `_`, or
    /// doesn't start with a letter.
    #[error("the name holds a character it may not, at position {index}")]
    InvalidChar {
        /// The character's position, counted in characters.
        index: usize,
    },
}

/// A name a mode's declaration gives something: a declared message's or a
/// chat pattern's key, a value's or a capture's name.
///
/// It's 1 to [`Ident::MAX_LEN`] characters of `a`–`z`, `0`–`9` and `_`,
/// starting with a letter. That's a name a script can write after a dot
/// (`values.amount`) and as a map's key (`#{ price: 1 }`), and it holds
/// nothing that needs escaping in a log line or in JSON.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Ident(String);

impl Ident {
    /// The longest name, in characters.
    pub const MAX_LEN: usize = 32;

    /// Returns the name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for Ident {
    type Error = IdentError;

    fn try_from(text: &str) -> Result<Self, IdentError> {
        let chars = text.chars().count();
        if chars == 0 {
            return Err(IdentError::Empty);
        }
        if chars > Self::MAX_LEN {
            return Err(IdentError::TooLong { chars });
        }
        let invalid = text.chars().enumerate().find(|&(index, c)| {
            let letter = c.is_ascii_lowercase();
            !(letter || (index > 0 && (c.is_ascii_digit() || c == '_')))
        });
        match invalid {
            Some((index, _)) => Err(IdentError::InvalidChar { index }),
            None => Ok(Self(text.to_owned())),
        }
    }
}

impl TryFrom<String> for Ident {
    type Error = IdentError;

    fn try_from(text: String) -> Result<Self, IdentError> {
        Self::try_from(text.as_str())
    }
}

impl FromStr for Ident {
    type Err = IdentError;

    fn from_str(text: &str) -> Result<Self, IdentError> {
        Self::try_from(text)
    }
}

impl From<Ident> for String {
    fn from(ident: Ident) -> Self {
        ident.0
    }
}

impl fmt::Display for Ident {
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
    #[case::one_letter("a")]
    #[case::a_word("balance")]
    #[case::with_digits_and_underscores("sell_menu_2")]
    #[case::the_longest("abcdefghijklmnopqrstuvwxyz012345")]
    fn accepts_a_name(#[case] text: &str) {
        let ident = Ident::try_from(text).unwrap();

        assert_eq!(ident.as_str(), text);
        assert_eq!(ident.to_string(), text);
        assert_eq!(text.parse::<Ident>(), Ok(ident));
    }

    #[rstest]
    #[case::empty("", IdentError::Empty)]
    #[case::too_long("abcdefghijklmnopqrstuvwxyz0123456", IdentError::TooLong { chars: 33 })]
    #[case::starts_with_a_digit("1st", IdentError::InvalidChar { index: 0 })]
    #[case::starts_with_an_underscore("_x", IdentError::InvalidChar { index: 0 })]
    #[case::upper_case("Balance", IdentError::InvalidChar { index: 0 })]
    #[case::upper_case_later("baLance", IdentError::InvalidChar { index: 2 })]
    #[case::a_space("a b", IdentError::InvalidChar { index: 1 })]
    #[case::a_hyphen("a-b", IdentError::InvalidChar { index: 1 })]
    #[case::a_dot("a.b", IdentError::InvalidChar { index: 1 })]
    #[case::a_brace("a{b", IdentError::InvalidChar { index: 1 })]
    #[case::an_umlaut("gr\u{fc}n", IdentError::InvalidChar { index: 2 })]
    #[case::position_counts_characters("a\u{fc}\u{fc}-", IdentError::InvalidChar { index: 1 })]
    fn refuses_anything_else(#[case] text: &str, #[case] error: IdentError) {
        assert_eq!(Ident::try_from(text), Err(error));
    }

    #[test]
    fn length_comes_before_characters() {
        let text = "A".repeat(40);

        assert_eq!(
            Ident::try_from(text.as_str()),
            Err(IdentError::TooLong { chars: 40 })
        );
    }

    #[test]
    fn reads_and_writes_as_a_plain_string() {
        let ident = Ident::try_from("balance").unwrap();

        assert_eq!(serde_json::to_string(&ident).unwrap(), "\"balance\"");
        assert_eq!(serde_json::from_str::<Ident>("\"balance\"").unwrap(), ident);
        assert!(serde_json::from_str::<Ident>("\"Balance\"").is_err());
    }

    #[test]
    fn an_error_never_holds_the_text() {
        assert_eq!(
            IdentError::InvalidChar { index: 3 }.to_string(),
            "the name holds a character it may not, at position 3"
        );
        assert_eq!(
            IdentError::TooLong { chars: 40 }.to_string(),
            "the name is 40 characters long; the limit is 32"
        );
    }

    proptest! {
        #[test]
        fn never_panics_and_keeps_what_it_accepts(text in any::<String>()) {
            if let Ok(ident) = Ident::try_from(text.as_str()) {
                prop_assert_eq!(ident.as_str(), text.as_str());
                prop_assert!(text.len() <= Ident::MAX_LEN);
                prop_assert!(text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'));
            }
        }

        #[test]
        fn every_name_of_the_grammar_is_accepted(text in "[a-z][a-z0-9_]{0,31}") {
            prop_assert!(Ident::try_from(text.as_str()).is_ok());
        }
    }
}
