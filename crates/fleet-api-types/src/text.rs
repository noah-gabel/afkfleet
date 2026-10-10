//! [`BoundedText`]: text in a response, cleaned and capped (ADR-0015).
//!
//! A client reads every text a server sends, and the server may be buggy or
//! hostile. So response text never reaches a client's output as it arrived:
//! it goes through fleet-core's `sanitize_untrusted` (no control characters,
//! formatting codes, bidirectional controls or invisible characters; a line
//! break becomes ` | `) and is cut at `MAX` characters. Reading never fails
//! on the content, only on a value that isn't a string.
//!
//! The server builds its texts the same way, and its tests check that every
//! text it builds fits uncut, so writing never changes anything real.
//!
//! `BoundedText` has no TypeScript type of its own: every DTO field holding
//! one carries `#[ts(type = "string")]`, and a field without it doesn't
//! compile.

use core::fmt;

use fleet_core::text::sanitize_untrusted;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Text of at most `MAX` characters, cleaned with fleet-core's
/// `sanitize_untrusted`. It's a plain JSON string on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BoundedText<const MAX: usize>(String);

impl<const MAX: usize> BoundedText<MAX> {
    /// Cleans `text` and keeps at most `MAX` characters of it.
    #[must_use]
    pub fn new(text: &str) -> Self {
        Self(sanitize_untrusted(text, MAX).text)
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<const MAX: usize> fmt::Display for BoundedText<MAX> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<const MAX: usize> Serialize for BoundedText<MAX> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de, const MAX: usize> Deserialize<'de> for BoundedText<MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::new(&raw))
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    type Short = BoundedText<5>;

    #[test]
    fn plain_text_is_kept() {
        assert_eq!(Short::new("hello").as_str(), "hello");
    }

    #[rstest]
    #[case::control_character("a\u{7}bc", "abc")]
    #[case::formatting_code("a§cbc", "abc")]
    #[case::bidi_control("a\u{202E}bc", "abc")]
    #[case::invisible("a\u{200B}bc", "abc")]
    fn unsafe_characters_are_stripped(#[case] raw: &str, #[case] expected: &str) {
        assert_eq!(Short::new(raw).as_str(), expected);
    }

    #[test]
    fn a_line_break_becomes_a_separator() {
        assert_eq!(BoundedText::<16>::new("a\nb").as_str(), "a | b");
    }

    #[test]
    fn text_is_cut_at_the_cap() {
        assert_eq!(Short::new("abcdefgh").as_str(), "abcde");
    }

    #[test]
    fn the_cap_counts_characters_not_bytes() {
        assert_eq!(BoundedText::<3>::new("äöüß").as_str(), "äöü");
    }

    #[test]
    fn building_from_built_text_changes_nothing() {
        let once = Short::new("a\u{7}b\ncdefgh");

        assert_eq!(Short::new(once.as_str()), once);
    }

    #[test]
    fn it_is_a_plain_json_string() {
        let text = Short::new("hi");

        assert_eq!(serde_json::to_string(&text).unwrap(), r#""hi""#);
        assert_eq!(text.to_string(), "hi");
    }

    #[test]
    fn reading_cleans_and_caps_instead_of_failing() {
        let text: Short = serde_json::from_str(r#""a\u0007b\ncdefgh""#).unwrap();

        assert_eq!(text.as_str(), "ab | ");
    }

    #[rstest]
    #[case::number("42")]
    #[case::null("null")]
    #[case::object("{}")]
    fn reading_a_value_that_isnt_a_string_fails(#[case] json: &str) {
        assert!(serde_json::from_str::<Short>(json).is_err());
    }
}
