//! The bots' chat log, `chat_messages` (Plan.md P11.1, P10.8; ADR-0019,
//! S70): [`NewChatMessage`] to append, [`ChatRecord`] as it reads back, and
//! the two traits. Append-only: the ingest appends inside its batch's write
//! transaction ([`ChatWrites::append`]), P11.3 pages through a bot's chat
//! ([`ChatReads::page`]), and P11.6's retention deletes old rows.

use core::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::chat::{ChatMessage, IncomingChat};
use fleet_core::id::{AgentId, BotId};

use super::page::Page;
use super::store::StoreError;

/// A chat row's ID. With one writer, IDs follow the order of commits, which
/// is the order the server received the messages in, so a bot's chat is
/// ordered and paged by ID, never by time (ADR-0015).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChatMessageId(i64);

impl ChatMessageId {
    /// Wraps a stored ID, or a cursor a client sent back.
    #[must_use]
    pub const fn new(id: i64) -> Self {
        Self(id)
    }

    /// Returns the stored ID.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

/// What a row holds: a message the bot received, or one its mode sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEntry {
    /// Received from the Minecraft server, cleaned and capped (S6).
    Received(IncomingChat),
    /// Sent by the bot's mode (ADR-0013's `ModeChatSent`).
    ModeSent(ChatMessage),
}

/// A message to append: what a bot's agent reported, stamped with the
/// server's time of receipt (S5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewChatMessage {
    /// The bot it belongs to.
    pub bot: BotId,
    /// The agent that reported it. The row is stored only while the bot's
    /// row still names this agent (S28, S59).
    pub agent: AgentId,
    /// When the server received it, by its own clock.
    pub at: DateTime<Utc>,
    /// The message.
    pub entry: ChatEntry,
}

/// A stored message as it reads back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRecord {
    /// The ID, in receipt order.
    pub id: ChatMessageId,
    /// The bot it belongs to.
    pub bot: BotId,
    /// When the server received it (informational: the order is the ID).
    pub at: DateTime<Utc>,
    /// The message.
    pub entry: ChatEntry,
}

/// Reading the chat log.
#[async_trait]
pub trait ChatReads: Send + Sync + fmt::Debug {
    /// One page of `bot`'s messages, newest first by ID: the messages
    /// before `page.before` (from the newest without a cursor), at most
    /// `page.limit.fetch()` of them, so the caller knows whether another
    /// page follows.
    ///
    /// # Errors
    /// [`StoreError::Corrupt`] for a row that doesn't read back, naming its
    /// column and ID, and the other [`StoreError`]s.
    async fn page(
        &self,
        bot: BotId,
        page: Page<ChatMessageId>,
    ) -> Result<Vec<ChatRecord>, StoreError>;
}

/// Appending to the chat log, inside a [`super::store::WriteTx`].
#[async_trait]
pub trait ChatWrites: Send {
    /// Appends `message` and returns its ID, or `None` when the bot's row is
    /// gone or no longer names `message.agent`: nothing is stored then, and
    /// the transaction goes on, so a batch never fails on a bot deleted or
    /// moved since its message arrived.
    ///
    /// # Errors
    /// [`StoreError::Unstorable`] for a time outside the years 0 to 9999,
    /// and the other [`StoreError`]s.
    async fn append(
        &mut self,
        message: &NewChatMessage,
    ) -> Result<Option<ChatMessageId>, StoreError>;
}
