//! What a `Connect` stream hands the server's ingest (Plan.md P10.8;
//! ADR-0019, S28, S70, U16, U17): the reports the registry judged to be
//! about a bot the stream holds assigned on a reconciled stream that isn't
//! ended, and nothing else.
//!
//! - [`Ingest`]: takes a report and never waits. Chat goes into a bounded
//!   queue, each stream at most its [`StreamShare`] of it; the rest is
//!   published on the bus at once.
//! - [`StickyMarks`]: the bots whose reported state entered or left Paused
//!   or Failed (U16), which the ingest's batch writes, fenced: taken only
//!   while the report is still the bot's latest, from the live stream that
//!   holds it assigned.

use core::fmt;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::bot::{BotNotification, BotSnapshot, LastReport};
use fleet_core::chat::{ChatMessage, IncomingChat};
use fleet_core::id::{AgentId, BotId, ChatRequestId};
use fleet_proto::control::ChatFailedReason;
use tokio::sync::Semaphore;

/// A report the registry passed to the ingest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The stream's agent.
    pub agent: AgentId,
    /// The stream's generation.
    pub generation: u64,
    /// The bot it's about, which the stream holds assigned.
    pub bot: BotId,
    /// When the server received it, by its own clock (S5).
    pub received_at: DateTime<Utc>,
    /// What it reports.
    pub reported: Reported,
}

/// What a report says. A `Removed` is the reconciler's alone, never the
/// ingest's, so it can't be said here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reported {
    /// The bot's new state.
    Status(BotSnapshot),
    /// The bot died and respawns.
    Died,
    /// An alert for a human, after a Paused or Failed state.
    Alert(BotNotification),
    /// A chat message the bot received, cleaned (S6).
    ChatReceived(IncomingChat),
    /// A chat message the bot's mode sent.
    ModeChatSent(ChatMessage),
    /// The server's chat message `request` was sent (P11.3).
    ChatSent(ChatRequestId),
    /// The server's chat message `request` wasn't sent, and why (P11.3).
    ChatFailed(ChatRequestId, ChatFailedReason),
}

/// One stream's share of the ingest's chat queue: at most its permits of
/// its rows are queued or being written at once, so one flooding agent
/// can't crowd out the others (S70). Clones share it.
#[derive(Debug, Clone)]
pub struct StreamShare(Arc<Semaphore>);

impl StreamShare {
    /// A share of at most `rows` rows in flight, capped at what a semaphore
    /// holds.
    #[must_use]
    pub fn new(rows: usize) -> Self {
        Self(Arc::new(Semaphore::new(rows.min(Semaphore::MAX_PERMITS))))
    }

    /// The semaphore its rows' permits come from.
    #[must_use]
    pub fn semaphore(&self) -> &Arc<Semaphore> {
        &self.0
    }
}

/// Why the ingest didn't take a report: only a chat row is ever refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NotTaken {
    /// The stream has its share of rows in flight already.
    #[error("the stream's share of the chat queue is full")]
    StreamFull,
    /// The chat queue is full.
    #[error("the chat queue is full")]
    QueueFull,
    /// The ingest's writer has ended.
    #[error("the ingest is closed")]
    Closed,
}

/// The server's ingest of what agents report; see the [module docs](self).
pub trait Ingest: Send + Sync + fmt::Debug {
    /// A new stream's share of the chat queue.
    fn share(&self) -> StreamShare;

    /// Takes `report`, which the registry judged held: chat is queued for
    /// the next batch, anything else is published at once. Never waits.
    ///
    /// # Errors
    /// [`NotTaken`] for a chat row that doesn't fit; it's dropped, and the
    /// caller counts it.
    fn take(&self, share: &StreamShare, report: Report) -> Result<(), NotTaken>;
}

/// An ingest that takes everything and keeps nothing: for a control
/// service whose reports go nowhere (the enrollment's tests, a port
/// without the batch writer).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoIngest;

impl Ingest for NoIngest {
    fn share(&self) -> StreamShare {
        StreamShare::new(1)
    }

    fn take(&self, _share: &StreamShare, _report: Report) -> Result<(), NotTaken> {
        Ok(())
    }
}

/// A bot's latest report for its row's `last_state`, taken from the stream
/// that holds it assigned (U16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StickyWrite {
    /// The bot.
    pub bot: BotId,
    /// The agent whose live stream holds it assigned; the row is written
    /// only while it names this agent.
    pub agent: AgentId,
    /// The report.
    pub last: LastReport,
}

/// The bots whose reported state crossed into or out of Paused or Failed,
/// or changed its reason, since their row last stored it (U16).
pub trait StickyMarks: Send + Sync + fmt::Debug {
    /// Whether any bot is marked: a batch writes only when something waits.
    fn any(&self) -> bool;

    /// Takes every mark, each with its bot's latest report, fenced: only a
    /// report that is still the bot's latest, from the live, unended stream
    /// of its agent and generation that holds the bot assigned. The batch
    /// calls it inside its open write transaction, so no clear of the bot's
    /// state (S48) commits between this and the batch's commit. Marks that
    /// don't pass are dropped.
    fn take(&self) -> Vec<StickyWrite>;

    /// Marks `bots` again: the batch that took them didn't commit.
    fn remark(&self, bots: &[BotId]);
}

/// Marks that are never set: for a writer with no live streams.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoStickyMarks;

impl StickyMarks for NoStickyMarks {
    fn any(&self) -> bool {
        false
    }

    fn take(&self) -> Vec<StickyWrite> {
        Vec::new()
    }

    fn remark(&self, _bots: &[BotId]) {}
}
