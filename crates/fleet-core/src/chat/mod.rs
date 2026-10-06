//! Chat: messages the bots send, and messages they receive.
//!
//! [`ChatMessage`] is the only way to send text through a bot. It's validated
//! at the boundary, so nothing downstream can send control characters,
//! formatting codes or over-long text. [`IncomingChat`] is the other direction:
//! untrusted server text, sanitized so it's safe to store and display.

mod incoming;
mod message;

pub use incoming::{ChatKind, ChatSender, IncomingChat, IncomingChatError, PlayerChatKind};
pub use message::{ChatMessage, ChatMessageError};
