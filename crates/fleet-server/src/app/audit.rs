//! [`AuditService`]: the audit log's use cases (Plan.md P6.8, ADR-0015).

use std::sync::Arc;

use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::system::Clock;

use crate::ports::audit::{AuditEntryId, AuditPage, AuditRecord, NewAuditEntry};
use crate::ports::store::{Store, StoreError};

/// Stamps audit entries with the clock, records the entries that come
/// without a change, and lists the log.
///
/// A use case that changes state builds its entry here and passes it to
/// `WriteTx::commit`, so the change and its entry commit together, and every
/// timestamp comes from one clock. [`AuditService::record`] is for an attempt
/// that changed nothing: a failure or a denial whose change was rolled back.
#[derive(Debug, Clone)]
pub struct AuditService {
    clock: Arc<dyn Clock>,
    store: Arc<dyn Store>,
}

impl AuditService {
    /// A service over `store`, stamping with `clock`.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, store: Arc<dyn Store>) -> Self {
        Self { clock, store }
    }

    /// An entry stamped with the clock's current time (milliseconds), with
    /// no actor, IP, target or metadata yet; the builder methods add them.
    #[must_use]
    pub fn entry(&self, action: AuditAction, outcome: AuditOutcome) -> NewAuditEntry {
        NewAuditEntry::new(self.clock.now(), action, outcome)
    }

    /// Records `entry` in its own short write transaction.
    ///
    /// # Errors
    /// [`StoreError::Busy`] when the write connection stays taken for the
    /// acquire timeout, and the other [`StoreError`]s.
    pub async fn record(&self, entry: NewAuditEntry) -> Result<AuditEntryId, StoreError> {
        self.store.write().await?.commit(entry).await
    }

    /// One page of the log, newest first by ID, through the read
    /// connections.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for a row that
    /// doesn't read back.
    pub async fn list(&self, page: AuditPage) -> Result<Vec<AuditRecord>, StoreError> {
        self.store.audit().list(page).await
    }
}
