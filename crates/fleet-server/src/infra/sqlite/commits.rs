//! The database's commit count (Plan.md P11.5; ADR-0020, R8, PR #66's
//! review): [`Store::commits`](crate::ports::store::Store::commits) wakes
//! every live events socket to check its session again.
//!
//! The count moves whenever a commit was asked for, however it ended: it
//! returned, it failed, or its future was dropped midway (a request's
//! timeout, a client gone). A dropped one may still land, since sqlx's
//! worker runs a `COMMIT` it was handed, and a socket woken before it lands
//! would read the old state. So a commit marks the count unsettled before
//! it starts and clears the mark once `COMMIT` returned; while the mark
//! stands, the write pool's `after_release` pings the returned connection,
//! which answers only after every statement it was handed, and moves the
//! count once more. A move too many costs a socket one check; one too few
//! would leave a revoked session's socket open.
//!
//! The mark is the generation of the commit that set it (ADR-0020, S38): a
//! commit's connection can come back, be settled and start the next commit
//! before the first commit's own future sees its `COMMIT` return, so a late
//! return clears only its own mark, never the next commit's.
//!
//! sqlx closes a returned connection without `after_release` when it's past
//! the pool's lifetime or the pool is closed (sqlx 0.9's
//! `Floating::return_to_pool`), so the write pool has no lifetime (S39); a
//! closed pool has no socket left to wake. A sqlx bump re-reads that
//! function.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlx::Connection as _;
use sqlx::sqlite::SqliteConnection;
use tokio::sync::watch;

/// The count and its mark; the database and the write pool's hook share it.
#[derive(Debug)]
pub(super) struct CommitCount {
    count: watch::Sender<u64>,
    /// The last generation a commit took.
    generations: AtomicU64,
    /// The generation of the commit whose `COMMIT` hasn't returned, 0 when
    /// none.
    unsettled: AtomicU64,
}

impl CommitCount {
    /// A count at zero, settled.
    pub(super) fn new() -> Self {
        Self {
            count: watch::Sender::new(0),
            generations: AtomicU64::new(0),
            unsettled: AtomicU64::new(0),
        }
    }

    /// A reader of the count.
    pub(super) fn subscribe(&self) -> watch::Receiver<u64> {
        self.count.subscribe()
    }

    /// One commit's turn: unsettled until its `COMMIT` returned, and the
    /// count moves when the turn is dropped. Its generation is never 0.
    pub(super) fn start(self: &Arc<Self>) -> Counted {
        let generation = self
            .generations
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1)
            .max(1);
        self.unsettled.store(generation, Ordering::SeqCst);
        Counted {
            count: Arc::clone(self),
            generation,
        }
    }

    /// The write connection is back in its pool: after a commit that didn't
    /// see its `COMMIT` return, waits until `connection` has run everything
    /// it was handed, then moves the count. Whether the pool keeps the
    /// connection: a failed ping closes it.
    ///
    /// # Errors
    /// The ping's error, which closes the connection; the count moves anyway.
    pub(super) async fn settle(
        &self,
        connection: &mut SqliteConnection,
    ) -> Result<bool, sqlx::Error> {
        if !self.take_unsettled() {
            return Ok(true);
        }
        let pinged = connection.ping().await;
        self.bump();
        pinged.map(|()| true)
    }

    /// Whether a commit's mark stood, clearing it: [`CommitCount::settle`]'s
    /// decision, apart from its ping.
    fn take_unsettled(&self) -> bool {
        self.unsettled.swap(0, Ordering::SeqCst) != 0
    }

    fn bump(&self) {
        self.count
            .send_modify(|count| *count = count.wrapping_add(1));
    }
}

/// One commit's turn ([`CommitCount::start`]).
#[derive(Debug)]
pub(super) struct Counted {
    count: Arc<CommitCount>,
    /// The mark it set.
    generation: u64,
}

impl Counted {
    /// The commit's `COMMIT` returned: it landed, or failed for good. Clears
    /// its own mark only: a later commit's stays.
    pub(super) fn returned(&self) {
        // A failure means the mark is another commit's, or already settled.
        let _ = self.count.unsettled.compare_exchange(
            self.generation,
            0,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }
}

impl Drop for Counted {
    /// Moves the count, however the commit ended.
    fn drop(&mut self) {
        self.count.bump();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::CommitCount;

    #[test]
    fn a_late_return_leaves_the_next_commits_mark() {
        // Commit A's connection came back, and was settled, before A saw its
        // COMMIT return; commit B then began on it.
        let count = Arc::new(CommitCount::new());
        let a = count.start();
        assert!(count.take_unsettled());
        let b = count.start();

        a.returned();

        assert!(count.take_unsettled(), "B's mark was cleared by A's return");
        drop(b);
    }

    #[test]
    fn a_returned_commit_leaves_no_mark() {
        let count = Arc::new(CommitCount::new());
        let commit = count.start();

        commit.returned();

        assert!(!count.take_unsettled());
    }

    #[test]
    fn a_commit_settled_before_its_return_is_settled_once() {
        let count = Arc::new(CommitCount::new());
        let commit = count.start();
        assert!(count.take_unsettled());

        let again = count.take_unsettled();
        commit.returned();

        assert!(!again, "a settled mark was taken twice");
        assert!(!count.take_unsettled());
    }

    #[test]
    fn a_commit_dropped_before_its_return_leaves_its_mark_for_the_pool() {
        let count = Arc::new(CommitCount::new());
        let before = *count.subscribe().borrow();

        drop(count.start());

        assert_eq!(*count.subscribe().borrow(), before + 1);
        assert!(count.take_unsettled());
    }
}
