//! Chat: messages the bots send, and messages they receive.
//!
//! [`ChatMessage`] is the only way to send text through a bot. It's validated
//! at the boundary, so nothing downstream can send control characters,
//! formatting codes or over-long text. [`IncomingChat`] is the other direction:
//! untrusted server text, sanitized so it's safe to store and display.
//! [`ChatRate`] is how fast a bot may chat, the server's setting, sent in
//! each bot's spec (ADR-0020, S3). [`ChatFormats`] are a Minecraft server's
//! chat formats, which read a display-only sender out of its system lines
//! (ADR-0020, S79, S80).

mod format;
mod incoming;
mod message;
mod rate;

pub use format::{ChatFormats, ChatFormatsError, FormatMatch, MAX_CHAT_FORMATS, READ_MAX};
pub use incoming::{ChatKind, ChatSender, IncomingChat, IncomingChatError, PlayerChatKind};
pub use message::{ChatMessage, ChatMessageError, Redacted};
pub use rate::{ChatRate, ChatRateError};
