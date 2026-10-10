//! The audit log: [`NewAuditEntry`] to record, [`AuditRecord`] as it reads
//! back, and the two traits. Append-only: recording and listing are the
//! only operations ([`AuditWrites::record`] and [`AuditReads::list`]).

use core::fmt;
use std::net::IpAddr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::audit::{
    AuditAction, AuditMetadata, AuditOutcome, AuditTarget, RecordedMetadata, RecordedTarget,
};
use fleet_core::id::UserId;

use super::store::StoreError;

/// An audit entry's ID. With one writer, IDs follow the order of commits, so
/// the log is ordered and paged by ID, never by time (ADR-0015).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuditEntryId(i64);

impl AuditEntryId {
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

/// How many entries one page holds: 1 to [`AuditLimit::MAX`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditLimit(u8);

/// Why a page size isn't valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuditLimitError {
    /// It's 0 or more than [`AuditLimit::MAX`].
    #[error("a page holds 1 to {max} entries, not {limit}")]
    OutOfRange {
        /// The asked size.
        limit: u32,
        /// The limit.
        max: u8,
    },
}

impl AuditLimit {
    /// The largest page.
    pub const MAX: u8 = 100;

    /// Checks a page size.
    ///
    /// # Errors
    /// [`AuditLimitError::OutOfRange`] for 0 or more than [`AuditLimit::MAX`].
    pub fn new(limit: u32) -> Result<Self, AuditLimitError> {
        u8::try_from(limit)
            .ok()
            .filter(|limit| (1..=Self::MAX).contains(limit))
            .map(Self)
            .ok_or(AuditLimitError::OutOfRange {
                limit,
                max: Self::MAX,
            })
    }

    /// Returns the page size.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Which page of the log to list: the newest entries older than `before`
/// (all of them without a cursor), newest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditPage {
    /// Only entries with a smaller ID; `None` starts at the newest.
    pub before: Option<AuditEntryId>,
    /// How many entries at most.
    pub limit: AuditLimit,
}

/// An entry to record. Built with [`NewAuditEntry::new`] and the builder
/// methods; the audit service stamps `at` from the clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAuditEntry {
    pub(crate) at: DateTime<Utc>,
    pub(crate) actor: Option<UserId>,
    pub(crate) ip: Option<IpAddr>,
    pub(crate) action: AuditAction,
    pub(crate) target: Option<AuditTarget>,
    pub(crate) outcome: AuditOutcome,
    pub(crate) metadata: AuditMetadata,
}

impl NewAuditEntry {
    /// An entry with no actor, IP, target or metadata yet.
    #[must_use]
    pub fn new(at: DateTime<Utc>, action: AuditAction, outcome: AuditOutcome) -> Self {
        Self {
            at,
            actor: None,
            ip: None,
            action,
            target: None,
            outcome,
            metadata: AuditMetadata::new(),
        }
    }

    /// Sets who did it.
    #[must_use]
    pub fn actor(self, actor: UserId) -> Self {
        Self {
            actor: Some(actor),
            ..self
        }
    }

    /// Sets the client's IP address.
    #[must_use]
    pub fn ip(self, ip: IpAddr) -> Self {
        Self {
            ip: Some(ip),
            ..self
        }
    }

    /// Sets what it touched.
    #[must_use]
    pub fn target(self, target: AuditTarget) -> Self {
        Self {
            target: Some(target),
            ..self
        }
    }

    /// Sets the details.
    #[must_use]
    pub fn metadata(self, metadata: AuditMetadata) -> Self {
        Self { metadata, ..self }
    }
}

/// A recorded entry as it reads back: tolerant of a target type or a
/// metadata value only a newer version knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecord {
    /// The ID, in commit order.
    pub id: AuditEntryId,
    /// When it happened (informational: the order is the ID).
    pub at: DateTime<Utc>,
    /// Who did it, if a user did.
    pub actor: Option<UserId>,
    /// The client's canonical IP address, if known.
    pub ip: Option<IpAddr>,
    /// What happened.
    pub action: AuditAction,
    /// What it touched, if anything.
    pub target: Option<RecordedTarget>,
    /// How it ended.
    pub outcome: AuditOutcome,
    /// The details.
    pub metadata: RecordedMetadata,
}

/// Reading the audit log.
#[async_trait]
pub trait AuditReads: Send + Sync + fmt::Debug {
    /// One page of entries, newest first by ID.
    ///
    /// # Errors
    /// [`StoreError::Corrupt`] for a row that doesn't read back, naming its
    /// ID, and the other [`StoreError`]s.
    async fn list(&self, page: AuditPage) -> Result<Vec<AuditRecord>, StoreError>;
}

/// Recording in the audit log, inside a [`super::store::WriteTx`].
#[async_trait]
pub trait AuditWrites: Send {
    /// Records `entry` and returns its ID.
    ///
    /// # Errors
    /// [`StoreError`]. Never because of the metadata: `AuditMetadata` always
    /// fits its column.
    async fn record(&mut self, entry: &NewAuditEntry) -> Result<AuditEntryId, StoreError>;
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::smallest(1)]
    #[case::largest(100)]
    fn page_sizes_from_1_to_100_are_accepted(#[case] limit: u32) {
        assert_eq!(
            AuditLimit::new(limit).unwrap().get(),
            u8::try_from(limit).unwrap()
        );
    }

    #[rstest]
    #[case::zero(0)]
    #[case::one_too_many(101)]
    #[case::huge(u32::MAX)]
    fn other_page_sizes_are_refused(#[case] limit: u32) {
        assert_eq!(
            AuditLimit::new(limit),
            Err(AuditLimitError::OutOfRange { limit, max: 100 })
        );
    }
}
