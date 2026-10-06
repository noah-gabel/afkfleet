//! [`ChatMessage`]: validated text a bot may send.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::text;

/// Why text isn't a valid [`ChatMessage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChatMessageError {
    /// The message is empty or only whitespace.
    #[error("the message is empty")]
    Empty,
    /// The message is longer than [`ChatMessage::MAX_LEN`] UTF-16 code units.
    #[error("the message is {units} UTF-16 code units long; the limit is {max}", max = ChatMessage::MAX_LEN)]
    TooLong {
        /// The trimmed message's length in UTF-16 code units.
        units: usize,
    },
    /// The message contains a control character, `§` or a bidirectional control.
    #[error("the message contains a forbidden character at position {index}")]
    ForbiddenChar {
        /// The character's position (counted in characters) in the trimmed message.
        index: usize,
    },
}

/// Text a bot may send as chat or as a `/command`.
///
/// It's trimmed, 1 to [`ChatMessage::MAX_LEN`] UTF-16 code units long, and
/// contains no control characters, no `§` and no bidirectional controls.
/// Minecraft measures the limit as a Java string length, so an emoji counts
/// twice (ADR-0010). azalea would otherwise drop such characters and truncate
/// the text silently (ADR-0008 §7).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ChatMessage(String);

impl ChatMessage {
    /// The maximum length in UTF-16 code units, the same as Minecraft's.
    pub const MAX_LEN: usize = 256;

    /// Returns the message text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the message is a command, i.e. starts with `/`.
    #[must_use]
    pub fn is_command(&self) -> bool {
        self.0.starts_with('/')
    }

    /// Returns the command name for allowlist checks, or `None` if the message
    /// isn't a command.
    ///
    /// The name is the text after the `/` up to the first whitespace, compared
    /// as is: `/spawn home` gives `spawn`, `/minecraft:op` gives `minecraft:op`,
    /// and a lone `/` gives an empty name.
    #[must_use]
    pub fn command_name(&self) -> Option<&str> {
        let command = self.0.strip_prefix('/')?;
        Some(
            command
                .split_once(char::is_whitespace)
                .map_or(command, |(name, _)| name),
        )
    }
}

impl TryFrom<&str> for ChatMessage {
    type Error = ChatMessageError;

    fn try_from(text: &str) -> Result<Self, ChatMessageError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(ChatMessageError::Empty);
        }
        if let Some(index) = trimmed.chars().position(text::is_forbidden_in_message) {
            return Err(ChatMessageError::ForbiddenChar { index });
        }
        let units = text::utf16_len(trimmed);
        if units > Self::MAX_LEN {
            return Err(ChatMessageError::TooLong { units });
        }
        Ok(Self(trimmed.to_owned()))
    }
}

impl TryFrom<String> for ChatMessage {
    type Error = ChatMessageError;

    fn try_from(text: String) -> Result<Self, ChatMessageError> {
        Self::try_from(text.as_str())
    }
}

impl FromStr for ChatMessage {
    type Err = ChatMessageError;

    fn from_str(text: &str) -> Result<Self, ChatMessageError> {
        Self::try_from(text)
    }
}

impl From<ChatMessage> for String {
    fn from(message: ChatMessage) -> Self {
        message.0
    }
}

impl fmt::Display for ChatMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    fn message(text: &str) -> ChatMessage {
        ChatMessage::try_from(text).unwrap()
    }

    #[rstest]
    #[case::plain("hello", "hello")]
    #[case::trimmed("  hello world \n", "hello world")]
    #[case::command("/spawn", "/spawn")]
    #[case::umlauts_and_emoji("grüß dich 😀", "grüß dich 😀")]
    fn accepts_and_trims(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(message(input).as_str(), expected);
    }

    #[rstest]
    #[case::empty("")]
    #[case::only_spaces("   ")]
    #[case::only_newlines("\n\n")]
    fn rejects_empty(#[case] input: &str) {
        assert_eq!(ChatMessage::try_from(input), Err(ChatMessageError::Empty));
    }

    #[rstest]
    #[case::newline("hi\nthere", 2)]
    #[case::tab("a\tb", 1)]
    #[case::bell("ab\u{7}", 2)]
    #[case::delete("\u{7F}x", 0)]
    #[case::c1_control("x\u{85}y", 1)]
    #[case::section_sign("§cred", 0)]
    #[case::right_to_left_override("abc\u{202E}def", 3)]
    #[case::isolate("a\u{2066}b", 1)]
    #[case::position_counts_chars_not_bytes("üü\nx", 2)]
    fn rejects_forbidden_chars_with_their_position(#[case] input: &str, #[case] index: usize) {
        assert_eq!(
            ChatMessage::try_from(input),
            Err(ChatMessageError::ForbiddenChar { index })
        );
    }

    #[test]
    fn accepts_exactly_256_ascii_chars() {
        assert!(ChatMessage::try_from("x".repeat(256).as_str()).is_ok());
    }

    #[test]
    fn rejects_257_ascii_chars() {
        assert_eq!(
            ChatMessage::try_from("x".repeat(257).as_str()),
            Err(ChatMessageError::TooLong { units: 257 })
        );
    }

    #[test]
    fn counts_emoji_as_two_units() {
        assert!(ChatMessage::try_from("😀".repeat(128).as_str()).is_ok());
        assert_eq!(
            ChatMessage::try_from("😀".repeat(129).as_str()),
            Err(ChatMessageError::TooLong { units: 258 })
        );
    }

    #[test]
    fn rejects_255_chars_plus_one_emoji() {
        let text = format!("{}😀", "x".repeat(255));
        assert_eq!(
            ChatMessage::try_from(text.as_str()),
            Err(ChatMessageError::TooLong { units: 257 })
        );
    }

    #[rstest]
    #[case::plain_chat("hello", None)]
    #[case::bare_command("/spawn", Some("spawn"))]
    #[case::command_with_args("/spawn home now", Some("spawn"))]
    #[case::namespaced("/minecraft:op AfkBot1", Some("minecraft:op"))]
    #[case::worldedit_double_slash("//wand", Some("/wand"))]
    #[case::lone_slash("/", Some(""))]
    #[case::space_after_slash("/ spawn", Some(""))]
    #[case::case_is_kept("/Spawn", Some("Spawn"))]
    #[case::slash_later_in_text("not /a command", None)]
    fn finds_the_command_name(#[case] input: &str, #[case] expected: Option<&str>) {
        let message = message(input);
        assert_eq!(message.command_name(), expected);
        assert_eq!(message.is_command(), expected.is_some());
    }

    #[test]
    fn deserialize_rejects_invalid_text() {
        assert!(serde_json::from_str::<ChatMessage>("\"a\\nb\"").is_err());
    }

    #[test]
    fn serializes_as_a_plain_string() {
        let json = serde_json::to_string(&message("/spawn")).unwrap();
        assert_eq!(json, "\"/spawn\"");
        assert_eq!(
            serde_json::from_str::<ChatMessage>(&json).unwrap(),
            message("/spawn")
        );
    }

    proptest! {
        #[test]
        fn never_panics(text in any::<String>()) {
            let _ = ChatMessage::try_from(text.as_str());
        }

        #[test]
        fn valid_messages_round_trip(text in "[ -~äöü😀]{1,300}") {
            if let Ok(message) = ChatMessage::try_from(text.as_str()) {
                prop_assert!(crate::text::utf16_len(message.as_str()) <= ChatMessage::MAX_LEN);
                prop_assert_eq!(ChatMessage::try_from(message.to_string()), Ok(message));
            }
        }
    }
}
