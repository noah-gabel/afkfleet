//! What the server's retention task deletes, and the reads that find where
//! it cuts (Plan.md P11.6; ADR-0020, S45, S46, R50, R51).
//!
//! The deletes have a port of their own, so the audit log's port stays
//! append-only for every other caller (P6.4's note): only the retention
//! task deletes from it, and it commits each batch through
//! `UnauditedWrite::RetentionPurge` (U20). Each delete takes at most `limit`
//! rows, the oldest first, and answers how many it deleted; a cut is a point
//! in time or an ID, and what is at or before it goes (fleet-core's
//! `retention` module).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::id::BotId;

use crate::ports::audit::AuditEntryId;
use crate::ports::chat::ChatMessageId;
use crate::ports::store::StoreError;

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

    /// The highest ID that, with every entry below it, is dated at or before
    /// `before`: the prefix of the audit log that goes, so an entry dated
    /// later (a clock stepped backwards) keeps every entry after it. Below
    /// the first entry's ID when that one is newer; `None` for an empty log.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Unstorable`] for a time
    /// outside the years 0 to 9999.
    async fn audit_cut(&self, before: DateTime<Utc>) -> Result<Option<AuditEntryId>, StoreError>;
}

/// The retention's deletes, inside a write transaction: each takes at most
/// `limit` rows and answers how many it deleted.
#[async_trait]
pub trait RetentionWrites: Send {
    /// Deletes the replaced refresh tokens of sessions created at or before
    /// `created_by`, before the sessions themselves, so no session's cascade
    /// makes a batch long.
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

    /// Deletes audit entries with an ID at or below `cut`.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn delete_audit_through(
        &mut self,
        cut: AuditEntryId,
        limit: u32,
    ) -> Result<u64, StoreError>;

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
}
