//! What the server's retention task deletes, and the reads that find where
//! it cuts (Plan.md P11.6; ADR-0020, S45, S46, R50, R51, R57).
//!
//! The deletes have a transaction of their own, [`RetentionBatch`], which
//! only [`Store::retention_batch`](crate::ports::store::Store::retention_batch)
//! starts: it holds the seven deletes and a commit without an audit entry,
//! and nothing else, so no other write can delete through it and it can
//! write nothing but its deletes. The audit log's port stays append-only for
//! every other caller (P6.4's note), and only the retention task opens a
//! batch (a test reads the server's sources for it). Each delete takes at
//! most `limit` rows and answers how many it deleted; a cut is a point in
//! time or an ID, and what is at or before it goes (fleet-core's `retention`
//! module).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::id::BotId;

use crate::ports::audit::AuditEntryId;
use crate::ports::chat::ChatMessageId;
use crate::ports::store::StoreError;

/// Where the audit log's prefix ends (R51, R60).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditCut {
    /// The highest ID that, with every entry below it, is dated at or before
    /// the cut: the prefix that goes, so an entry dated later (a clock
    /// stepped backwards) keeps every entry after it. Below the first
    /// entry's ID when that one is newer.
    pub through: AuditEntryId,
    /// The first entry dated after the cut, where the prefix stops; `None`
    /// when every entry is due.
    pub first_kept: Option<KeptEntry>,
}

/// The audit entry a prefix stops at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeptEntry {
    /// Its ID.
    pub id: AuditEntryId,
    /// Its date: one in the future, after a clock stepped back, keeps every
    /// later entry until then.
    pub at: DateTime<Utc>,
}

/// What one batch of the audit log's prefix deleted (R59).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AuditPurged {
    /// How many entries.
    pub count: u64,
    /// The highest ID among them, which the log no longer holds once the
    /// batch committed; `None` when it deleted none.
    pub through: Option<AuditEntryId>,
}

/// Where the retention cuts, read through the read connections.
#[async_trait]
pub trait RetentionReads: Send + Sync {
    /// Every bot, for the size bound's walk.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for an ID that
    /// doesn't read back.
    async fn bots(&self) -> Result<Vec<BotId>, StoreError>;

    /// The ID of `bot`'s line just past its newest `keep`: it and every line
    /// of the bot below it are over the bound. `None` when the bot holds
    /// `keep` lines or fewer.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn chat_cut(&self, bot: BotId, keep: u32) -> Result<Option<ChatMessageId>, StoreError>;

    /// Where the audit log's prefix that goes ends: see [`AuditCut`]. `None`
    /// for an empty log.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Unstorable`] for a time
    /// outside the years 0 to 9999, and [`StoreError::Corrupt`] for a first
    /// kept entry's date that doesn't read back.
    async fn audit_cut(&self, before: DateTime<Utc>) -> Result<Option<AuditCut>, StoreError>;
}

/// One batch of the server's retention task (P11.6; ADR-0020, U20, S45,
/// S46, R57): its own write transaction on the one write connection, which
/// holds only the deletes below, each taking at most `limit` rows and
/// answering how many it deleted. The task deletes at most 1000 rows of one
/// table a batch, never inserting or updating anything:
/// - the replaced refresh tokens of sessions whose `created_at` plus the
///   current absolute lifetime has passed, then those sessions;
/// - chat lines older than `[chat] retention_days` or past a bot's bound;
/// - audit entries as a prefix by ID, below the first one dated after the
///   audit cut;
/// - unused invites and enrollment tokens that expired at or before it.
///
/// It commits without an audit entry ([`RetentionBatch::commit`]), every
/// 60 s at most, each batch its own short transaction: an entry per batch
/// would say only that rows the policy removes were removed, and would
/// follow the ingest's volume. One `retention.purge` entry, counts and
/// cut-offs and never text, sums up every hour that deleted anything, and
/// the stop. Dropping a batch without its commit rolls everything back.
#[async_trait]
pub trait RetentionBatch: Send {
    /// Deletes the replaced refresh tokens of sessions created at or before
    /// `created_by`, in no set order, before the sessions themselves, so no
    /// session's cascade makes a batch long.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Unstorable`].
    async fn delete_expired_rotations(
        &mut self,
        created_by: DateTime<Utc>,
        limit: u32,
    ) -> Result<u64, StoreError>;

    /// Deletes sessions created at or before `created_by`, revoked or not
    /// (ADR-0016); a replaced token still left goes with its session.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Unstorable`].
    async fn delete_expired_sessions(
        &mut self,
        created_by: DateTime<Utc>,
        limit: u32,
    ) -> Result<u64, StoreError>;

    /// Deletes chat lines received at or before `received_by`, of any kind
    /// and any bot.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Unstorable`].
    async fn delete_chat_received_by(
        &mut self,
        received_by: DateTime<Utc>,
        limit: u32,
    ) -> Result<u64, StoreError>;

    /// Deletes `bot`'s chat lines with an ID at or below `cut`.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn delete_chat_through(
        &mut self,
        bot: BotId,
        cut: ChatMessageId,
        limit: u32,
    ) -> Result<u64, StoreError>;

    /// Deletes audit entries with an ID at or below `cut`, the lowest
    /// first, and answers how many and the highest ID among them.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn delete_audit_through(
        &mut self,
        cut: AuditEntryId,
        limit: u32,
    ) -> Result<AuditPurged, StoreError>;

    /// Deletes unused invites (revoked ones included) that expired at or
    /// before `expired_by`; a used invite is never deleted (S46).
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Unstorable`].
    async fn delete_unused_invites(
        &mut self,
        expired_by: DateTime<Utc>,
        limit: u32,
    ) -> Result<u64, StoreError>;

    /// Deletes unused enrollment tokens that expired at or before
    /// `expired_by`; a used token, which names its agent, is never deleted
    /// (S46).
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Unstorable`].
    async fn delete_unused_enrollment_tokens(
        &mut self,
        expired_by: DateTime<Utc>,
        limit: u32,
    ) -> Result<u64, StoreError>;

    /// Commits the batch without an audit entry: this transaction is the
    /// exemption (ADR-0020, U20, R57). One `retention.purge` entry an hour
    /// sums the batches up instead. It moves the store's commit count like
    /// any other commit.
    ///
    /// # Errors
    /// [`StoreError`] when the commit fails; nothing is committed then.
    async fn commit(self: Box<Self>) -> Result<(), StoreError>;
}
