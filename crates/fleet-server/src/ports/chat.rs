//! The bots' chat log, `chat_messages` (Plan.md P11.1, P10.8, P11.3;
//! ADR-0019, S70; ADR-0020, S6, S7, U3): [`NewChatMessage`] to append,
//! [`ChatRecord`] as it reads back, [`ListedChat`] as the history lists
//! it, and the two traits. Append-only: the
//! ingest appends what bots received and what their modes said inside its
//! batch's write transaction, a user's send appends their line inside its
//! own audited one ([`ChatWrites::append`]), P11.3 pages through a bot's
//! chat ([`ChatReads::page`]), and P11.6's retention deletes old rows. No
//! row changes once written: a user's line records what they asked the bot
//! to send, never whether it went out (S7).

use core::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::chat::{ChatMessage, IncomingChat};
use fleet_core::id::{AgentId, BotId, UserId};
use fleet_core::value::Username;

use super::page::Page;
use super::store::StoreError;

/// A chat row's ID. With one writer, IDs follow the order of commits, so a
/// bot's chat is ordered and paged by ID, never by time (ADR-0015). That's
/// nearly the order the server received the messages in, not quite: a
/// user's line commits at once, while received chat commits with the
/// ingest's next batch, so a line can get a lower ID than a message received
/// a moment before it (ADR-0020, U5; `0016_chat_messages.sql` says "the
/// order the server received the messages in" and stays as it is).
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

/// What a row holds: a message the bot received, one its mode sent, or one
/// a user asked it to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEntry {
    /// Received from the Minecraft server, cleaned and capped (S6).
    Received(IncomingChat),
    /// Sent by the bot's mode (ADR-0013's `ModeChatSent`).
    ModeSent(ChatMessage),
    /// A line a user asked the bot to send through the API (ADR-0020, S6):
    /// stored before it's sent, whatever then happens to it (S7).
    UserSent {
        /// The line, exactly as it was checked and sent.
        message: ChatMessage,
        /// Who sent it: always set when it's appended, and `None` once that
        /// user is deleted (U3).
        by: Option<UserId>,
    },
}

/// A message to append: what a bot's agent reported, or a user's line on its
/// way to the bot, stamped with the server's own time (S5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewChatMessage {
    /// The bot it belongs to.
    pub bot: BotId,
    /// The agent that reported it, or that a user's line goes to. The row is
    /// stored only while the bot's row still names this agent (S28, S59).
    pub agent: AgentId,
    /// When the server received it, or took the user's line, by its own
    /// clock.
    pub at: DateTime<Utc>,
    /// The message.
    pub entry: ChatEntry,
}

/// A stored message as it reads back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRecord {
    /// The ID, in commit order.
    pub id: ChatMessageId,
    /// The bot it belongs to.
    pub bot: BotId,
    /// When the server received it, or a user's line was sent
    /// (informational: the order is the ID, and neighbouring rows' times can
    /// disagree with it by about one ingest batch).
    pub at: DateTime<Utc>,
    /// The message.
    pub entry: ChatEntry,
}

/// A stored message as the history lists it: the record, and the username
/// of the user who sent a user's line, while that user exists (U3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedChat {
    /// The message.
    pub record: ChatRecord,
    /// The sender's username: a user's line whose sender still exists has
    /// one, every other line none.
    pub sent_by: Option<Username>,
}

/// Reading the chat log.
#[async_trait]
pub trait ChatReads: Send + Sync + fmt::Debug {
    /// One page of `bot`'s messages, newest first by ID: the messages
    /// before `page.before` (from the newest without a cursor), at most
    /// `page.limit.fetch()` of them, so the caller knows whether another
    /// page follows; a user's line with its sender's username.
    ///
    /// # Errors
    /// [`StoreError::Corrupt`] for a row that doesn't read back, naming its
    /// column and ID, and the other [`StoreError`]s.
    async fn page(
        &self,
        bot: BotId,
        page: Page<ChatMessageId>,
    ) -> Result<Vec<ListedChat>, StoreError>;
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
