//! Chat: messages the bots send, and messages they receive.
//!
//! [`ChatMessage`] is the only way to send text through a bot. It's validated
//! at the boundary, so nothing downstream can send control characters,
//! formatting codes or over-long text.

mod message;

pub use message::{ChatMessage, ChatMessageError};
