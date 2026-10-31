//! [`AuditService`]: the audit log's use cases (Plan.md P6.8, ADR-0015).

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::system::Clock;
use tracing::warn;

use crate::ports::audit::{
    AuditEntryId, AuditLimit, AuditPage, AuditRecord, AuditTargetFilter, NewAuditEntry, TABLE,
};
use crate::ports::page::Paged;
use crate::ports::store::{Store, StoreError};

/// The most entries the service remembers having warned about; past it, a
/// new one warns at every read, so none goes silent and memory stays
/// bounded (ADR-0020, S52).
pub const WARNED_MAX: usize = 1024;

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
    /// The entries that don't read back and warned already.
    warned: Arc<Mutex<Warned>>,
}

impl AuditService {
    /// A service over `store`, stamping with `clock`.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, store: Arc<dyn Store>) -> Self {
        Self {
            clock,
            store,
            warned: Arc::default(),
        }
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

    /// One page of the log, newest first by ID, with the next page's cursor
    /// when an older entry follows (ADR-0020, R64): when the page is full,
    /// one more read of a single entry below its last decides. The port's
    /// list stays as it is, the user's Phase 6 choice (ADR-0015).
    ///
    /// A row that doesn't read back below the cursor no longer fails the
    /// page (S52): the page is read again one entry at a time, and that row
    /// is listed by its ID alone in its place, with one `warn` per entry for
    /// the process, naming the column, never a value; the probe takes such
    /// a row as an older entry.
    ///
    /// # Errors
    /// As [`Self::list`], but for a row that doesn't read back below the
    /// cursor.
    pub async fn page(
        &self,
        page: AuditPage,
    ) -> Result<Paged<ListedAuditEntry, AuditEntryId>, StoreError> {
        self.listed(None, page).await
    }

    /// One page of the entries about `filter`'s target, as [`Self::page`]
    /// lists the whole log: the probe and the reading one at a time stay
    /// under the filter, so another target's row is never named and never
    /// counts as an older entry (ADR-0020, S89).
    ///
    /// # Errors
    /// As [`Self::page`].
    pub async fn page_for_target(
        &self,
        filter: &AuditTargetFilter,
        page: AuditPage,
    ) -> Result<Paged<ListedAuditEntry, AuditEntryId>, StoreError> {
        self.listed(Some(filter), page).await
    }

    /// One page of the log, or of `filter`'s target, with its `next`.
    async fn listed(
        &self,
        filter: Option<&AuditTargetFilter>,
        page: AuditPage,
    ) -> Result<Paged<ListedAuditEntry, AuditEntryId>, StoreError> {
        let items = match self.read(filter, page).await {
            Ok(records) => records
                .into_iter()
                .map(ListedAuditEntry::Readable)
                .collect(),
            Err(error) if unreadable_below(&error, page.before).is_some() => {
                self.one_by_one(filter, page).await?
            }
            Err(error) => return Err(error),
        };
        let full = items.len() >= usize::from(page.limit.get());
        let next = match items.last() {
            Some(last) if full => self
                .older_than(filter, last.id())
                .await?
                .then_some(last.id()),
            _ => None,
        };
        Ok(Paged { items, next })
    }

    /// One read of the log, or of `filter`'s target.
    async fn read(
        &self,
        filter: Option<&AuditTargetFilter>,
        page: AuditPage,
    ) -> Result<Vec<AuditRecord>, StoreError> {
        match filter {
            Some(filter) => self.store.audit().list_for_target(filter, page).await,
            None => self.list(page).await,
        }
    }

    /// Whether an entry older than `id` follows, under `filter`: one read of
    /// a single entry, where a row that doesn't read back counts too (R64,
    /// S52).
    async fn older_than(
        &self,
        filter: Option<&AuditTargetFilter>,
        id: AuditEntryId,
    ) -> Result<bool, StoreError> {
        let older = AuditPage {
            before: Some(id),
            limit: AuditLimit::ONE,
        };
        match self.read(filter, older).await {
            Ok(older) => Ok(!older.is_empty()),
            Err(error) if unreadable_below(&error, Some(id)).is_some() => Ok(true),
            Err(error) => Err(error),
        }
    }

    /// `page` read again one entry at a time, each row that doesn't read
    /// back listed by its ID, with one warning, and stepped past: every read
    /// is below the last entry listed, so it always ends.
    async fn one_by_one(
        &self,
        filter: Option<&AuditTargetFilter>,
        page: AuditPage,
    ) -> Result<Vec<ListedAuditEntry>, StoreError> {
        let limit = usize::from(page.limit.get());
        let mut items = Vec::with_capacity(limit);
        let mut before = page.before;
        while items.len() < limit {
            let one = AuditPage {
                before,
                limit: AuditLimit::ONE,
            };
            let entry = match self.read(filter, one).await {
                Ok(mut one) => match one.pop() {
                    Some(record) => ListedAuditEntry::Readable(record),
                    None => break,
                },
                Err(error) => {
                    let Some((id, column)) = unreadable_below(&error, before) else {
                        return Err(error);
                    };
                    self.warn_once(id, column);
                    ListedAuditEntry::Unreadable(id)
                }
            };
            before = Some(entry.id());
            items.push(entry);
        }
        Ok(items)
    }

    /// One `warn` for the entry `id`, whose `column` doesn't read back, the
    /// first time this process lists it.
    fn warn_once(&self, id: AuditEntryId, column: &'static str) {
        let first = self
            .warned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .first(id);
        if first {
            warn!(
                audit_entry = id.get(),
                column,
                "an audit entry doesn't read back: the audit log lists it as unreadable; \
                 a value in its row was changed outside the server or by another version"
            );
        }
    }
}

/// The entry `error` names, and its column, when it's a row of the audit
/// log that doesn't read back below the cursor `before` (any row when
/// there's none); `None` for every other error, which stays the store's.
fn unreadable_below(
    error: &StoreError,
    before: Option<AuditEntryId>,
) -> Option<(AuditEntryId, &'static str)> {
    match error {
        StoreError::Corrupt {
            table,
            column,
            rowid,
        } if *table == TABLE
            && *rowid >= 1
            && before.is_none_or(|before| *rowid < before.get()) =>
        {
            Some((AuditEntryId::new(*rowid), column))
        }
        _ => None,
    }
}

/// The entries that warned already, so each warns once per process: at
/// most [`WARNED_MAX`] are kept.
#[derive(Debug, Default)]
struct Warned {
    ids: BTreeSet<AuditEntryId>,
}

impl Warned {
    /// Whether `id` warns now: the first time, and every time once
    /// [`WARNED_MAX`] others are kept.
    fn first(&mut self, id: AuditEntryId) -> bool {
        if self.ids.contains(&id) {
            return false;
        }
        if self.ids.len() < WARNED_MAX {
            self.ids.insert(id);
        }
        true
    }
}

/// One entry of a page of the log: as it reads, or, for a row whose values
/// don't read back, its ID alone (ADR-0020, S52).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListedAuditEntry {
    /// The entry as stored.
    Readable(AuditRecord),
    /// A stored row that doesn't read back: changed outside the server, or
    /// by another version.
    Unreadable(AuditEntryId),
}

impl ListedAuditEntry {
    /// The entry's ID.
    #[must_use]
    pub const fn id(&self) -> AuditEntryId {
        match self {
            Self::Readable(record) => record.id,
            Self::Unreadable(id) => *id,
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn corrupt(table: &'static str, rowid: i64) -> StoreError {
        StoreError::Corrupt {
            table,
            column: "at",
            rowid,
        }
    }

    #[rstest]
    #[case::below_the_cursor(corrupt(TABLE, 4), Some(5), Some(4))]
    #[case::without_a_cursor(corrupt(TABLE, 9), None, Some(9))]
    #[case::at_the_cursor(corrupt(TABLE, 5), Some(5), None)]
    #[case::above_the_cursor(corrupt(TABLE, 6), Some(5), None)]
    #[case::no_row(corrupt(TABLE, 0), None, None)]
    #[case::another_table(corrupt("users", 4), Some(5), None)]
    #[case::busy(StoreError::Busy, Some(5), None)]
    #[case::unstorable(StoreError::Unstorable { table: TABLE, column: "at" }, Some(5), None)]
    fn only_an_audit_row_below_the_cursor_is_unreadable(
        #[case] error: StoreError,
        #[case] before: Option<i64>,
        #[case] expected: Option<i64>,
    ) {
        let found = unreadable_below(&error, before.map(AuditEntryId::new));

        assert_eq!(found.map(|(id, _)| id.get()), expected);
        assert!(found.is_none_or(|(_, column)| column == "at"));
    }

    #[test]
    fn each_entry_warns_once_and_past_the_kept_ids_a_new_one_still_warns() {
        let mut warned = Warned::default();
        let kept: Vec<bool> = (1..=i64::try_from(WARNED_MAX).unwrap())
            .map(|id| warned.first(AuditEntryId::new(id)))
            .collect();

        let again = warned.first(AuditEntryId::new(1));
        let past = warned.first(AuditEntryId::new(5_000));
        let past_again = warned.first(AuditEntryId::new(5_000));

        assert!(kept.iter().all(|first| *first));
        assert!(!again, "a kept entry warns once");
        assert!(past && past_again, "past the bound, every read warns");
        assert_eq!(warned.ids.len(), WARNED_MAX);
    }
}
