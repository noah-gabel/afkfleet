//! [`Open`]: a response value that may grow in a later version, read
//! tolerantly (ADR-0015).
//!
//! A newer server can send a value this version doesn't know, such as a new
//! [`ErrorCode`](crate::ErrorCode). Failing to read the whole response over
//! it would hide everything else in it from an older app, so an `Open<E>`
//! keeps such a value as text: cleaned with fleet-core's
//! `sanitize_untrusted` and cut at [`UNKNOWN_MAX_CHARS`] characters, like
//! every text a client reads. Only a value that isn't a string fails.
//!
//! The server can only build an `Open` from a known value, so it never sends
//! a made-up one. A DTO field holding one carries `#[ts(as = "E")]`: the
//! TypeScript type lists exactly the values this version knows, and app code
//! that branches on it always keeps a default branch.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::text::BoundedText;

/// The most characters an unknown value keeps.
pub const UNKNOWN_MAX_CHARS: usize = 64;

/// A closed set of values that a later version may extend. Its wire names
/// must equal its serde names; each implementation tests that.
pub trait OpenEnum: Copy + 'static {
    /// Every value, each exactly once.
    const ALL: &'static [Self];

    /// The value's wire name.
    fn as_str(self) -> &'static str;
}

/// A value of `E`, or one a newer version knows: see the
/// [module docs](self). It's a plain JSON string on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Open<E>(Value<E>);

/// What an [`Open`] holds.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Value<E> {
    /// A value this version knows.
    Known(E),
    /// A value only a newer version knows, cleaned and capped.
    Unknown(BoundedText<UNKNOWN_MAX_CHARS>),
}

impl<E: OpenEnum> Open<E> {
    /// The value, if this version knows it.
    #[must_use]
    pub fn known(&self) -> Option<E> {
        match &self.0 {
            Value::Known(value) => Some(*value),
            Value::Unknown(_) => None,
        }
    }

    /// The wire name: a known value's name, or the cleaned text of an
    /// unknown one.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match &self.0 {
            Value::Known(value) => value.as_str(),
            Value::Unknown(text) => text.as_str(),
        }
    }
}

impl<E> From<E> for Open<E> {
    fn from(value: E) -> Self {
        Self(Value::Known(value))
    }
}

impl<E: OpenEnum> Serialize for Open<E> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de, E: OpenEnum> Deserialize<'de> for Open<E> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        let value = E::ALL
            .iter()
            .find(|known| known.as_str() == raw)
            .map_or_else(
                || Value::Unknown(BoundedText::new(&raw)),
                |known| Value::Known(*known),
            );
        Ok(Self(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Color {
        Red,
        DarkBlue,
    }

    impl OpenEnum for Color {
        const ALL: &'static [Self] = &[Self::Red, Self::DarkBlue];

        fn as_str(self) -> &'static str {
            match self {
                Self::Red => "red",
                Self::DarkBlue => "dark_blue",
            }
        }
    }

    fn read(json: &str) -> Open<Color> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_known_value_reads_back_as_known() {
        let color = read(r#""dark_blue""#);

        assert_eq!(color.known(), Some(Color::DarkBlue));
        assert_eq!(color.as_str(), "dark_blue");
    }

    #[test]
    fn a_known_value_is_written_as_its_name() {
        let color = Open::from(Color::DarkBlue);

        assert_eq!(serde_json::to_string(&color).unwrap(), r#""dark_blue""#);
        assert_eq!(color.known(), Some(Color::DarkBlue));
    }

    #[test]
    fn an_unknown_value_reads_back_as_its_text() {
        let color = read(r#""purple""#);

        assert_eq!(color.known(), None);
        assert_eq!(color.as_str(), "purple");
        assert_eq!(serde_json::to_string(&color).unwrap(), r#""purple""#);
    }

    #[test]
    fn matching_is_exact() {
        assert_eq!(read(r#""Red""#).known(), None);
    }

    #[test]
    fn an_unknown_value_with_a_line_break_comes_back_sanitized() {
        let color = read(r#""pur\nple\u0007""#);

        assert_eq!(color.known(), None);
        assert_eq!(color.as_str(), "pur | ple");
    }

    #[test]
    fn a_10_kb_unknown_value_is_cut_to_64_characters() {
        let json = serde_json::to_string(&"x".repeat(10 * 1024)).unwrap();

        let color = read(&json);

        assert_eq!(color.as_str(), "x".repeat(UNKNOWN_MAX_CHARS));
    }

    #[test]
    fn a_value_that_isnt_a_string_fails_as_a_wrong_type() {
        let error = serde_json::from_str::<Open<Color>>("42").unwrap_err();

        assert!(error.is_data(), "{error}");
    }
}
