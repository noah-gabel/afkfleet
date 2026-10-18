//! The server's ingest of what agents report (Plan.md P10.8; ADR-0019, S28,
//! S70, U16, U17): chat and sticky states written in batches, everything
//! published on the bus.
//!
//! - **The front** ([`IngestService`], the [`Ingest`] port): it takes only
//!   what the registry judged held, and never waits. A status, a death, an
//!   alert or a chat outcome is published at once and never queued, so chat
//!   can't drop or delay them. A chat row goes into one bounded queue of
//!   [`INGEST_QUEUE`] rows, each stream at most [`STREAM_ROWS`] of them in
//!   flight through its [`StreamShare`], so one flooding agent can't crowd
//!   out the others; a row that doesn't fit is dropped, and the session
//!   counts it.
//! - **The writer** ([`IngestWriter`]): one task, owned and watched by
//!   serve. Every [`INGEST_INTERVAL`] it writes what waits in one short
//!   transaction, and an idle tick writes nothing: the wait for the write
//!   connection bounded by [`INGEST_WAIT`], as a session's touch is; at most
//!   [`BATCH_ROWS`] chat rows, each stored only while its bot's row names
//!   the agent that reported it; the sticky crossings the registry marked
//!   (U16), taken inside the transaction and written only while the row
//!   names the agent and nothing later is stored; then
//!   `commit_unaudited(EventBatch)` (U17), only when a row or a state was
//!   written: a tick whose marks the fence refused, with no chat, is idle,
//!   and a batch that stored nothing rolls back. Stored chat is published
//!   with its row's ID once the batch committed.
//! - **Failures:** a database that's busy keeps the batch, which is written
//!   first at the next tick; any other failure drops the batch, logged once
//!   per run of failures, and the recovery logs how many rows were lost.
//!   The sticky marks of a batch that didn't commit are set again.
//! - **The stop:** the writer's stop comes once every stream has ended, so
//!   nothing is admitted after it; then it writes what waits, each batch's
//!   wait bounded, and a batch that finds the database busy is dropped with
//!   one `warn`, so the stop stays within the database's close.
//!
//! It logs counts and IDs, never a chat message's text or sender.

use core::fmt;
use core::num::NonZeroUsize;
use core::time::Duration;
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, mpsc};
use tokio::time::{Instant, MissedTickBehavior, interval_at, timeout};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use crate::app::events::{EventBus, LiveEvent, LiveEventKind};
use crate::ports::chat::{ChatEntry, ChatMessageId, NewChatMessage};
use crate::ports::ingest::{Ingest, NotTaken, Report, Reported, StickyMarks, StreamShare};
use crate::ports::store::{Store, StoreError, UnauditedWrite};

/// How often the writer writes what waits (P10.8's "roughly every 500 ms").
pub const INGEST_INTERVAL: Duration = Duration::from_millis(500);

/// The most chat rows one batch writes, as the lockout purge's batches.
pub const BATCH_ROWS: NonZeroUsize = NonZeroUsize::new(1000).unwrap();

/// How many chat rows wait at most, from every stream.
pub const INGEST_QUEUE: NonZeroUsize = NonZeroUsize::new(4096).unwrap();

/// How many of one stream's chat rows wait or are written at most: a
/// 1000-bot agent's server-wide broadcast, one line per bot, fits.
pub const STREAM_ROWS: NonZeroUsize = NonZeroUsize::new(1024).unwrap();

/// How long a batch waits for the write connection, as a session's touch
/// does (ADR-0016).
pub const INGEST_WAIT: Duration = Duration::from_millis(250);

/// How often and how much the writer writes. Tests lower them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestTimings {
    /// Between two batches.
    pub interval: Duration,
    /// The most chat rows in one batch.
    pub batch_rows: NonZeroUsize,
    /// The wait for the write connection.
    pub wait: Duration,
}

impl Default for IngestTimings {
    fn default() -> Self {
        Self {
            interval: INGEST_INTERVAL,
            batch_rows: BATCH_ROWS,
            wait: INGEST_WAIT,
        }
    }
}

/// How many chat rows wait at most, in all and per stream. Tests lower them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestBounds {
    /// In all.
    pub queue: NonZeroUsize,
    /// Per stream, queued or being written.
    pub stream: NonZeroUsize,
}

impl Default for IngestBounds {
    fn default() -> Self {
        Self {
            queue: INGEST_QUEUE,
            stream: STREAM_ROWS,
        }
    }
}

/// The ingest's front over `bus`, and its writer over `store` and `marks`;
/// see the [module docs](self).
#[must_use]
pub fn ingest(
    store: Arc<dyn Store>,
    marks: Arc<dyn StickyMarks>,
    bus: EventBus,
    timings: IngestTimings,
    bounds: IngestBounds,
) -> (IngestService, IngestWriter) {
    let (rows, queued) = mpsc::channel(bounds.queue.get());
    (
        IngestService {
            rows,
            bus: bus.clone(),
            stream: bounds.stream,
        },
        IngestWriter {
            queued,
            kept: Vec::new(),
            store,
            marks,
            bus,
            timings,
            lost: None,
        },
    )
}

/// A chat row waiting for its batch, holding its place in its stream's
/// share until it's written or dropped.
struct Row {
    message: NewChatMessage,
    generation: u64,
    _place: OwnedSemaphorePermit,
}

/// The ingest's front; clones share its queue. See the [module docs](self).
#[derive(Clone)]
pub struct IngestService {
    rows: mpsc::Sender<Row>,
    bus: EventBus,
    stream: NonZeroUsize,
}

impl fmt::Debug for IngestService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IngestService")
            .field(
                "waiting",
                &(self.rows.max_capacity() - self.rows.capacity()),
            )
            .finish_non_exhaustive()
    }
}

impl Ingest for IngestService {
    fn share(&self) -> StreamShare {
        StreamShare::new(self.stream.get())
    }

    fn take(&self, share: &StreamShare, report: Report) -> Result<(), NotTaken> {
        let Report {
            agent,
            generation,
            bot,
            received_at,
            reported,
        } = report;
        let publish = |kind| {
            self.bus.publish(LiveEvent {
                bot,
                agent,
                generation,
                at: received_at,
                kind,
            });
            Ok(())
        };
        let entry = match reported {
            Reported::Status(snapshot) => return publish(LiveEventKind::Status(snapshot)),
            Reported::Died => return publish(LiveEventKind::Died),
            Reported::Alert(notification) => return publish(LiveEventKind::Alert(notification)),
            Reported::ChatSent(request) => return publish(LiveEventKind::ChatSent(request)),
            Reported::ChatFailed(request, reason) => {
                return publish(LiveEventKind::ChatFailed(request, reason));
            }
            Reported::ChatReceived(chat) => ChatEntry::Received(chat),
            Reported::ModeChatSent(message) => ChatEntry::ModeSent(message),
        };
        let place = Arc::clone(share.semaphore())
            .try_acquire_owned()
            .map_err(|_| NotTaken::StreamFull)?;
        let row = Row {
            message: NewChatMessage {
                bot,
                agent,
                at: received_at,
                entry,
            },
            generation,
            _place: place,
        };
        self.rows.try_send(row).map_err(|refused| match refused {
            mpsc::error::TrySendError::Full(_) => NotTaken::QueueFull,
            mpsc::error::TrySendError::Closed(_) => NotTaken::Closed,
        })
    }
}

/// What one tick did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tick {
    /// Nothing waited; nothing was written.
    Idle,
    /// A batch committed.
    Written {
        /// Chat rows stored.
        stored: usize,
        /// Chat rows skipped: their bot was gone or no longer the agent's.
        skipped: usize,
        /// Sticky states written.
        sticky: usize,
    },
    /// The database was busy; the batch waits for the next tick.
    Kept {
        /// Its chat rows.
        rows: usize,
    },
    /// The batch failed and was dropped.
    Dropped {
        /// Its chat rows.
        rows: usize,
    },
}

/// Why a batch didn't commit.
enum Failure {
    /// The database was busy: the batch is kept.
    Busy,
    /// Anything else: the batch is dropped.
    Failed(StoreError),
}

impl From<StoreError> for Failure {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Busy => Self::Busy,
            other => Self::Failed(other),
        }
    }
}

/// What a batch stored; it committed only when it stored something.
struct Committed {
    /// Each row's ID, or none when it was skipped.
    ids: Vec<Option<ChatMessageId>>,
    /// The marks the fence let through.
    taken: usize,
    /// The sticky states written.
    sticky: usize,
}

/// What the stop logs when its last batch finds the database busy.
const LAST_BATCH_BUSY: &str =
    "the ingest's last batch found the database busy; its chat messages are lost";

/// What the stop logs when its last batch, with no chat waiting, finds the
/// database busy: the streams' ends recorded the bots' states (S40).
const LAST_BATCH_BUSY_NO_CHAT: &str = "the ingest's last batch found the database busy; no chat message waited, and the bots' states stay as their streams' ends recorded them";

/// The ingest's writer; see the [module docs](self).
pub struct IngestWriter {
    queued: mpsc::Receiver<Row>,
    /// The batch a busy database kept, written first.
    kept: Vec<Row>,
    store: Arc<dyn Store>,
    marks: Arc<dyn StickyMarks>,
    bus: EventBus,
    timings: IngestTimings,
    /// How many rows were lost since batches started failing; none while
    /// they don't.
    lost: Option<u64>,
}

impl fmt::Debug for IngestWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IngestWriter")
            .field("waiting", &self.queued.len())
            .field("kept", &self.kept.len())
            .finish_non_exhaustive()
    }
}

impl IngestWriter {
    /// Writes what waits: one batch, or nothing when nothing waits.
    pub async fn tick(&mut self) -> Tick {
        let mut batch = core::mem::take(&mut self.kept);
        while batch.len() < self.timings.batch_rows.get() {
            match self.queued.try_recv() {
                Ok(row) => batch.push(row),
                Err(_) => break,
            }
        }
        if batch.is_empty() && !self.marks.any() {
            return Tick::Idle;
        }
        let rows = batch.len();
        match self.write(&batch).await {
            // No chat, and the fence refused every mark: nothing was written.
            Ok(Committed { taken: 0, .. }) if batch.is_empty() => Tick::Idle,
            Ok(Committed { ids, sticky, .. }) => {
                if let Some(lost) = self.lost.take() {
                    warn!(
                        lost,
                        "the ingest writes again; chat messages were lost meanwhile"
                    );
                }
                let skipped = ids.iter().filter(|id| id.is_none()).count();
                for (row, id) in batch.into_iter().zip(ids) {
                    let Some(id) = id else { continue };
                    self.bus.publish(LiveEvent {
                        bot: row.message.bot,
                        agent: row.message.agent,
                        generation: row.generation,
                        at: row.message.at,
                        kind: LiveEventKind::Chat {
                            id,
                            entry: row.message.entry,
                        },
                    });
                }
                debug!(
                    stored = rows - skipped,
                    skipped, sticky, "the ingest wrote a batch"
                );
                Tick::Written {
                    stored: rows - skipped,
                    skipped,
                    sticky,
                }
            }
            Err(Failure::Busy) => {
                debug!(rows, "the database is busy; the ingest's batch waits");
                self.kept = batch;
                Tick::Kept { rows }
            }
            Err(Failure::Failed(failure)) => {
                let lost = u64::try_from(rows).unwrap_or(u64::MAX);
                if self.lost.is_none() {
                    error!(error = %failure, rows, "the ingest couldn't write a batch; its chat messages are lost");
                } else {
                    debug!(error = %failure, rows, "the ingest couldn't write a batch; its chat messages are lost");
                }
                self.lost = Some(self.lost.unwrap_or(0).saturating_add(lost));
                Tick::Dropped { rows }
            }
        }
    }

    /// `batch` and the marked sticky states in one transaction, committed
    /// without an audit entry (U17). The marks taken are set again when it
    /// doesn't commit.
    async fn write(&self, batch: &[Row]) -> Result<Committed, Failure> {
        // Only the wait for the write connection is bounded: once the
        // transaction is open, its statements and its commit run to their
        // end, bounded by SQLite's busy timeout (ADR-0016's touch).
        let mut tx = match timeout(self.timings.wait, self.store.write()).await {
            Err(_elapsed) => return Err(Failure::Busy),
            Ok(opened) => opened?,
        };
        // Taken inside the open transaction, which holds every other writer
        // off until it commits: no clear of a bot's state lands in between
        // (U16, S48).
        let marked = self.marks.take();
        let written = async {
            let mut ids = Vec::with_capacity(batch.len());
            for row in batch {
                ids.push(tx.chat().append(&row.message).await?);
            }
            let mut sticky = 0;
            for write in &marked {
                if tx
                    .bots()
                    .record_state(write.bot, write.agent, &write.last)
                    .await?
                {
                    sticky += 1;
                }
            }
            // A commit only when rows were written (U17): when the fence
            // refused every mark and no row was stored, dropping the
            // transaction rolls it back.
            if sticky > 0 || ids.iter().any(Option::is_some) {
                tx.commit_unaudited(UnauditedWrite::EventBatch).await?;
            }
            Ok::<_, StoreError>(Committed {
                ids,
                taken: marked.len(),
                sticky,
            })
        }
        .await;
        written.map_err(|failure| {
            let bots: Vec<_> = marked.iter().map(|write| write.bot).collect();
            self.marks.remark(&bots);
            Failure::from(failure)
        })
    }

    /// Writes a batch every interval until `stop`, then what still waits.
    pub async fn run(mut self, stop: CancellationToken) {
        let interval = self.timings.interval;
        let mut ticks = interval_at(Instant::now() + interval, interval);
        // A batch that took longer than an interval isn't followed by a burst.
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                // Cancel-safe: waiting for the token loses nothing.
                () = stop.cancelled() => break,
                // Cancel-safe, as tokio documents `Interval::tick`; the
                // batch itself runs in the branch, never cut off.
                _ = ticks.tick() => {
                    self.tick().await;
                }
            }
        }
        self.flush().await;
    }

    /// The stop's last batches: what still waits, each batch's wait bounded.
    /// A batch that finds the database busy is dropped, with what's left,
    /// and one `warn`.
    async fn flush(&mut self) {
        // Every row waiting now, a batch at a time, and one more for the
        // marks: nothing more arrives once the streams have ended.
        let batches = (self.kept.len() + self.queued.len())
            .div_ceil(self.timings.batch_rows.get())
            .saturating_add(1);
        for _ in 0..batches {
            match self.tick().await {
                Tick::Idle => return,
                Tick::Written { .. } | Tick::Dropped { .. } => {}
                Tick::Kept { rows } => {
                    let left = rows + self.queued.len();
                    self.kept.clear();
                    while self.queued.try_recv().is_ok() {}
                    if left == 0 {
                        warn!(rows = left, "{LAST_BATCH_BUSY_NO_CHAT}");
                    } else {
                        warn!(rows = left, "{LAST_BATCH_BUSY}");
                    }
                    return;
                }
            }
        }
    }
}
