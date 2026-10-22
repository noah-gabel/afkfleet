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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sqlx::Connection as _;
use sqlx::sqlite::SqliteConnection;
use tokio::sync::watch;

/// The count and its mark; the database and the write pool's hook share it.
#[derive(Debug)]
pub(super) struct CommitCount {
    count: watch::Sender<u64>,
    /// A commit started and its `COMMIT` hasn't returned.
    unsettled: AtomicBool,
}

impl CommitCount {
    /// A count at zero, settled.
    pub(super) fn new() -> Self {
        Self {
            count: watch::Sender::new(0),
            unsettled: AtomicBool::new(false),
        }
    }

    /// A reader of the count.
    pub(super) fn subscribe(&self) -> watch::Receiver<u64> {
        self.count.subscribe()
    }

    /// One commit's turn: unsettled until its `COMMIT` returned, and the
    /// count moves when the turn is dropped.
    pub(super) fn start(self: &Arc<Self>) -> Counted {
        self.unsettled.store(true, Ordering::SeqCst);
        Counted {
            count: Arc::clone(self),
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
        if !self.unsettled.swap(false, Ordering::SeqCst) {
            return Ok(true);
        }
        let pinged = connection.ping().await;
        self.bump();
        pinged.map(|()| true)
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
}

impl Counted {
    /// The commit's `COMMIT` returned: it landed, or failed for good.
    pub(super) fn returned(&self) {
        self.count.unsettled.store(false, Ordering::SeqCst);
    }
}

impl Drop for Counted {
    /// Moves the count, however the commit ended.
    fn drop(&mut self) {
        self.count.bump();
    }
}
