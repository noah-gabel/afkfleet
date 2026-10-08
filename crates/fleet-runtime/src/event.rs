//! [`FleetEvent`]: what the runtime tells the app about its bots.

use chrono::{DateTime, Utc};
use fleet_core::bot::{BotNotification, BotSnapshot};
use fleet_core::chat::{ChatMessage, IncomingChat};
use fleet_core::id::BotId;

use crate::chat::{ChatFailure, ChatTicket};

/// Something that happened to one bot, published on the fleet's bounded
/// `broadcast` (ADR-0013). A subscriber that lags behind resyncs from the
/// snapshots (Plan.md §6 row 12).
///
/// Its `Debug` output can hold chat text, so it's logged only at `debug`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetEvent {
    /// The bot.
    pub bot_id: BotId,
    /// When it happened, by the runtime's clock.
    pub at: DateTime<Utc>,
    /// What happened.
    pub kind: FleetEventKind,
}

/// What happened in a [`FleetEvent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetEventKind {
    /// The bot's state changed; this is its new snapshot.
    StateChanged(BotSnapshot),
    /// The bot died. The actor respawns it.
    Died,
    /// The bot received a chat message, already sanitized.
    ChatReceived(IncomingChat),
    /// The session sent a user's chat message.
    ChatSent {
        /// The ticket `send` returned for it.
        ticket: ChatTicket,
    },
    /// A user's chat message was queued but not sent. It isn't retried.
    ChatFailed {
        /// The ticket `send` returned for it.
        ticket: ChatTicket,
        /// Why it wasn't sent.
        reason: ChatFailure,
    },
    /// The session sent a chat message of the bot's mode. Mode chat has no
    /// ticket, and one that isn't sent is only logged (ADR-0013).
    ModeChatSent {
        /// The message.
        message: ChatMessage,
    },
    /// The bot paused or failed and needs a human: `Resume` or `Reset`
    /// connects it again. It comes right after the `StateChanged` it belongs
    /// to (ADR-0013).
    Alert(BotNotification),
    /// The fleet removed the bot: its actor has ended, every teardown has
    /// finished, and its account is free. It's the bot's last event.
    ///
    /// A bot that was running publishes `StateChanged(Stopped)` first. A
    /// Paused, Failed or crash-looped bot doesn't, so its last published
    /// state stays: the server keeps it and restores it when it assigns the
    /// bot again (P10.5; ADR-0013).
    Removed,
}
