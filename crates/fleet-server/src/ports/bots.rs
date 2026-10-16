//! The bots (Plan.md P11.1, P10.5, Appendix D; ADR-0019, S32–S40): one per
//! linked Minecraft account.
//!
//! A bot is created with its account, in the same transaction, on every path
//! that creates one, and deleted with it. Its row says what the bot should do
//! (the server it joins, its mode, whether it should run), which agent the
//! server assigned it to, and the last state an agent reported for it.

use core::fmt;

use async_trait::async_trait;
use fleet_core::bot::{DesiredRunState, LastReport};
use fleet_core::id::{AccountId, AgentId, BotId, ModeId};
use fleet_core::value::ServerAddress;

use super::store::StoreError;

/// A stored bot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bot {
    /// The ID.
    pub id: BotId,
    /// Its account; it never changes.
    pub account: AccountId,
    /// The server it joins; `None` until someone sets one, and a bot without
    /// one is never assigned.
    pub server: Option<ServerAddress>,
    /// What it does online.
    pub mode: ModeId,
    /// Whether it should run.
    pub desired: DesiredRunState,
    /// The agent the server assigned it to, if any.
    pub agent: Option<AgentId>,
    /// The last state an agent reported for it, and when the server
    /// received it; `None` until one is stored.
    pub last: Option<LastReport>,
}

/// A bot to create with its account: no server yet, stopped, unassigned,
/// with no state reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewBot {
    /// The ID, minted with its account's.
    pub id: BotId,
    /// Its account.
    pub account: AccountId,
    /// Its mode: the built-in `afk` for every new bot.
    pub mode: ModeId,
}

/// Reading bots through the read connections.
#[async_trait]
pub trait BotReads: Send + Sync + fmt::Debug {
    /// The bot `id`.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for a row that
    /// doesn't read back.
    async fn find(&self, id: BotId) -> Result<Option<Bot>, StoreError>;

    /// The bot of the account `account`.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn of_account(&self, account: AccountId) -> Result<Option<Bot>, StoreError>;
}

/// Writing bots inside a [`super::store::WriteTx`].
#[async_trait]
pub trait BotWrites: Send {
    /// Inserts `bot`.
    ///
    /// # Errors
    /// [`StoreError`]: an account that doesn't exist or has a bot already, a
    /// mode that doesn't exist, or an ID that does.
    async fn insert(&mut self, bot: &NewBot) -> Result<(), StoreError>;

    /// The bot `id`, read in this transaction.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn find(&mut self, id: BotId) -> Result<Option<Bot>, StoreError>;
}
