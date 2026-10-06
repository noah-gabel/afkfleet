//! [`IncomingChat`]: what a bot receives, made safe to store and display.

use core::fmt;
use core::str::FromStr;

use uuid::Uuid;

use crate::text::{self, LineBreaks, Sanitized};

/// Why parts can't form an [`IncomingChat`], or a kind name is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IncomingChatError {
    /// The name isn't one of the [`ChatKind`] names.
    #[error("unknown chat kind")]
    UnknownKind,
    /// A system message came with a sender; only player messages have one.
    #[error("a system message has no sender")]
    SenderOnSystemMessage,
    /// A player message came without a sender.
    #[error("a player message needs a sender")]
    MissingSender,
}

/// The kind of a message a player sent (from Minecraft's chat-type registry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlayerChatKind {
    /// Ordinary chat, including team chat and the echo of a whisper the bot sent.
    Chat,
    /// `/me`.
    Emote,
    /// A whisper to the bot (`/msg`, `/tell`).
    Whisper,
    /// `/say`.
    Announcement,
}

/// The kind of any received message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatKind {
    /// Ordinary chat.
    Chat,
    /// `/me`.
    Emote,
    /// A whisper to the bot.
    Whisper,
    /// `/say`.
    Announcement,
    /// A server message: joins and leaves, command output, `tellraw`, plugins.
    System,
}

impl ChatKind {
    /// Returns the stable name stored in the `kind` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Emote => "emote",
            Self::Whisper => "whisper",
            Self::Announcement => "announcement",
            Self::System => "system",
        }
    }
}

impl From<PlayerChatKind> for ChatKind {
    fn from(kind: PlayerChatKind) -> Self {
        match kind {
            PlayerChatKind::Chat => Self::Chat,
            PlayerChatKind::Emote => Self::Emote,
            PlayerChatKind::Whisper => Self::Whisper,
            PlayerChatKind::Announcement => Self::Announcement,
        }
    }
}

/// Returns the player kind behind `kind`, or `None` for a system message.
const fn player_kind(kind: ChatKind) -> Option<PlayerChatKind> {
    match kind {
        ChatKind::Chat => Some(PlayerChatKind::Chat),
        ChatKind::Emote => Some(PlayerChatKind::Emote),
        ChatKind::Whisper => Some(PlayerChatKind::Whisper),
        ChatKind::Announcement => Some(PlayerChatKind::Announcement),
        ChatKind::System => None,
    }
}

impl FromStr for ChatKind {
    type Err = IncomingChatError;

    fn from_str(name: &str) -> Result<Self, IncomingChatError> {
        match name {
            "chat" => Ok(Self::Chat),
            "emote" => Ok(Self::Emote),
            "whisper" => Ok(Self::Whisper),
            "announcement" => Ok(Self::Announcement),
            "system" => Ok(Self::System),
            _ => Err(IncomingChatError::UnknownKind),
        }
    }
}

impl fmt::Display for ChatKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Who sent a player message.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChatSender {
    name: String,
    uuid: Option<Uuid>,
}

impl ChatSender {
    /// The longest sender name kept, in characters.
    pub const MAX_NAME_LEN: usize = 64;

    /// Returns the sanitized display name. It can include a server's prefixes
    /// or nicknames, so it isn't necessarily a Minecraft username.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the player's UUID, which only ordinary chat carries.
    #[must_use]
    pub const fn uuid(&self) -> Option<Uuid> {
        self.uuid
    }
}

/// A received chat message, sanitized and safe to store and display as plain
/// text.
///
/// Minecraft text is untrusted. The text and the sender's name lose `§`
/// formatting codes, control characters (the text keeps `\n`), bidirectional
/// controls and invisible characters, and are capped in length (ADR-0010).
/// Only player messages have a sender: for system messages azalea guesses one
/// from the text, which anyone can fake with `tellraw` (ADR-0008 §7), so a
/// system message never carries one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IncomingChat {
    kind: ChatKind,
    sender: Option<ChatSender>,
    text: String,
    truncated: bool,
}

impl IncomingChat {
    /// The longest text kept, in characters.
    pub const MAX_TEXT_LEN: usize = 1024;

    /// Sanitizes a system message.
    #[must_use]
    pub fn system(raw_text: &str) -> Self {
        Self::sanitized(ChatKind::System, None, raw_text)
    }

    /// Sanitizes a message a player sent.
    #[must_use]
    pub fn player(
        kind: PlayerChatKind,
        sender_name: &str,
        sender_uuid: Option<Uuid>,
        raw_text: &str,
    ) -> Self {
        let sender = ChatSender {
            name: text::sanitize(sender_name, ChatSender::MAX_NAME_LEN, LineBreaks::Strip).text,
            uuid: sender_uuid,
        };
        Self::sanitized(kind.into(), Some(sender), raw_text)
    }

    /// Builds a message with a sanitized text; the sender is already sanitized.
    fn sanitized(kind: ChatKind, sender: Option<ChatSender>, raw_text: &str) -> Self {
        let Sanitized { text, truncated } =
            text::sanitize(raw_text, Self::MAX_TEXT_LEN, LineBreaks::Keep);
        Self {
            kind,
            sender,
            text,
            truncated,
        }
    }

    /// Rebuilds a message from its stored or transmitted parts.
    ///
    /// The text and the sender name are sanitized again (which leaves clean
    /// text unchanged), and `truncated` is kept.
    ///
    /// # Errors
    /// - [`IncomingChatError::SenderOnSystemMessage`] if a system message has a sender.
    /// - [`IncomingChatError::MissingSender`] if a player message has none.
    pub fn from_parts(
        kind: ChatKind,
        sender: Option<(&str, Option<Uuid>)>,
        text: &str,
        truncated: bool,
    ) -> Result<Self, IncomingChatError> {
        let mut chat = match (player_kind(kind), sender) {
            (None, None) => Self::system(text),
            (None, Some(_)) => return Err(IncomingChatError::SenderOnSystemMessage),
            (Some(_), None) => return Err(IncomingChatError::MissingSender),
            (Some(kind), Some((name, uuid))) => Self::player(kind, name, uuid, text),
        };
        chat.truncated |= truncated;
        Ok(chat)
    }

    /// Returns the kind.
    #[must_use]
    pub const fn kind(&self) -> ChatKind {
        self.kind
    }

    /// Returns the sender, which only player messages have.
    #[must_use]
    pub const fn sender(&self) -> Option<&ChatSender> {
        self.sender.as_ref()
    }

    /// Returns the sanitized text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the text was cut off at [`IncomingChat::MAX_TEXT_LEN`].
    #[must_use]
    pub const fn is_truncated(&self) -> bool {
        self.truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    const PLAYER_UUID: Uuid = Uuid::from_u128(0x0123_4567_89ab_4def_8123_4567_89ab_cdef);

    #[test]
    fn system_messages_are_sanitized_and_have_no_sender() {
        let chat = IncomingChat::system("§eAfkBot2 joined the game\u{202E}");

        assert_eq!(chat.kind(), ChatKind::System);
        assert_eq!(chat.sender(), None);
        assert_eq!(chat.text(), "AfkBot2 joined the game");
        assert!(!chat.is_truncated());
    }

    #[test]
    fn a_spoofed_sender_in_system_text_stays_text() {
        let chat = IncomingChat::system("<AfkBot7> I am not really AfkBot7");

        assert_eq!(chat.sender(), None);
        assert_eq!(chat.text(), "<AfkBot7> I am not really AfkBot7");
    }

    #[test]
    fn system_text_keeps_newlines() {
        let chat = IncomingChat::system("There are 2 players:\nAfkBot1, AfkBot2");
        assert_eq!(chat.text(), "There are 2 players:\nAfkBot1, AfkBot2");
    }

    #[test]
    fn player_messages_keep_kind_and_sender() {
        let chat = IncomingChat::player(
            PlayerChatKind::Chat,
            "AfkBot2",
            Some(PLAYER_UUID),
            "hello §cworld",
        );

        assert_eq!(chat.kind(), ChatKind::Chat);
        let sender = chat.sender().unwrap();
        assert_eq!(sender.name(), "AfkBot2");
        assert_eq!(sender.uuid(), Some(PLAYER_UUID));
        assert_eq!(chat.text(), "hello world");
    }

    #[rstest]
    #[case::bidi("\u{202E}AfkBot7", "AfkBot7")]
    #[case::zero_width_space("Afk\u{200B}Bot7", "AfkBot7")]
    #[case::soft_hyphen("Afk\u{00AD}Bot7", "AfkBot7")]
    #[case::byte_order_mark("\u{FEFF}AfkBot7", "AfkBot7")]
    #[case::formatting("§c[Admin] §fAfkBot7", "[Admin] AfkBot7")]
    #[case::newline("Afk\nBot7", "AfkBot7")]
    #[case::line_separator("Afk\u{2028}Bot7", "AfkBot7")]
    #[case::hangul_filler("AfkBot7\u{3164}", "AfkBot7")]
    #[case::hidden_tag_text("AfkBot7\u{E0068}\u{E0069}", "AfkBot7")]
    fn sender_names_are_sanitized(#[case] raw: &str, #[case] expected: &str) {
        let chat = IncomingChat::player(PlayerChatKind::Whisper, raw, None, "hi");
        assert_eq!(chat.sender().unwrap().name(), expected);
    }

    #[test]
    fn sender_names_are_capped() {
        let chat = IncomingChat::player(PlayerChatKind::Chat, &"n".repeat(70), None, "hi");
        assert_eq!(chat.sender().unwrap().name(), "n".repeat(64));
    }

    #[test]
    fn long_text_is_truncated_and_flagged() {
        let chat = IncomingChat::system(&"x".repeat(2000));

        assert_eq!(chat.text().chars().count(), IncomingChat::MAX_TEXT_LEN);
        assert!(chat.is_truncated());
    }

    #[rstest]
    #[case::chat(PlayerChatKind::Chat, ChatKind::Chat)]
    #[case::emote(PlayerChatKind::Emote, ChatKind::Emote)]
    #[case::whisper(PlayerChatKind::Whisper, ChatKind::Whisper)]
    #[case::announcement(PlayerChatKind::Announcement, ChatKind::Announcement)]
    fn player_kinds_map_to_chat_kinds(#[case] player: PlayerChatKind, #[case] kind: ChatKind) {
        assert_eq!(ChatKind::from(player), kind);
    }

    #[rstest]
    #[case::chat(ChatKind::Chat, "chat")]
    #[case::emote(ChatKind::Emote, "emote")]
    #[case::whisper(ChatKind::Whisper, "whisper")]
    #[case::announcement(ChatKind::Announcement, "announcement")]
    #[case::system(ChatKind::System, "system")]
    fn kind_names_are_stable_and_round_trip(#[case] kind: ChatKind, #[case] name: &str) {
        assert_eq!(kind.as_str(), name);
        assert_eq!(kind.to_string(), name);
        assert_eq!(name.parse::<ChatKind>(), Ok(kind));
    }

    #[rstest]
    #[case::unknown("shout")]
    #[case::wrong_case("Chat")]
    #[case::empty("")]
    fn unknown_kind_names_are_rejected(#[case] name: &str) {
        assert_eq!(
            name.parse::<ChatKind>(),
            Err(IncomingChatError::UnknownKind)
        );
    }

    #[test]
    fn from_parts_rebuilds_a_player_message_and_keeps_the_flag() {
        let chat =
            IncomingChat::from_parts(ChatKind::Emote, Some(("AfkBot2", None)), "waves", true)
                .unwrap();

        assert_eq!(chat.kind(), ChatKind::Emote);
        assert_eq!(chat.sender().unwrap().name(), "AfkBot2");
        assert_eq!(chat.text(), "waves");
        assert!(chat.is_truncated());
    }

    #[test]
    fn from_parts_sanitizes_again() {
        let chat = IncomingChat::from_parts(ChatKind::System, None, "a\u{202E}b§c", false).unwrap();

        assert_eq!(chat.text(), "ab");
        assert!(!chat.is_truncated());
    }

    #[test]
    fn from_parts_rejects_a_sender_on_a_system_message() {
        assert_eq!(
            IncomingChat::from_parts(ChatKind::System, Some(("Rcon", None)), "hi", false),
            Err(IncomingChatError::SenderOnSystemMessage)
        );
    }

    #[test]
    fn from_parts_rejects_a_player_message_without_a_sender() {
        assert_eq!(
            IncomingChat::from_parts(ChatKind::Whisper, None, "hi", false),
            Err(IncomingChatError::MissingSender)
        );
    }

    fn player_kind() -> impl Strategy<Value = PlayerChatKind> {
        prop_oneof![
            Just(PlayerChatKind::Chat),
            Just(PlayerChatKind::Emote),
            Just(PlayerChatKind::Whisper),
            Just(PlayerChatKind::Announcement),
        ]
    }

    proptest! {
        #[test]
        fn received_chat_is_always_clean(
            kind in player_kind(),
            name in any::<String>(),
            text in any::<String>(),
        ) {
            let chat = IncomingChat::player(kind, &name, None, &text);
            let sender = chat.sender().unwrap();

            prop_assert!(chat.text().chars().count() <= IncomingChat::MAX_TEXT_LEN);
            prop_assert!(sender.name().chars().count() <= ChatSender::MAX_NAME_LEN);
            prop_assert!(chat.text().chars().all(|c| c == '\n' || !c.is_control()));
            prop_assert!(sender.name().chars().all(|c| !c.is_control()));
            prop_assert!(!chat.text().contains('§') && !sender.name().contains('§'));
        }
    }
}
