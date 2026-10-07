//! [`McHostPool`] and [`HostThread`]: one OS thread per session, which is
//! where azalea runs (ADR-0008 §2, ADR-0011).
//!
//! A host thread runs a current-thread tokio runtime with a `LocalSet`, so the
//! `!Send` futures azalea needs run there. Jobs arrive over a bounded queue as
//! `Send` closures; each builds its future on the thread, which runs it as a
//! task of the thread's `JoinSet`, and the output goes back over a oneshot.
//!
//! The thread's tasks have no `CancellationToken`, a deliberate exception to
//! the rule in CLAUDE.md (ADR-0011): the `JoinSet` owns them, and the stop
//! signal, or the last handle being dropped, ends the loop, which drops the
//! `JoinSet` and the `LocalSet` and with them every task the session left.

use core::future::Future;
use core::num::NonZeroUsize;
use core::pin::Pin;
use core::time::Duration;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Instant;

use fleet_core::id::BotId;
use fleet_core::mc::{ConnectError, SessionError};
use tokio::runtime::{self, Runtime};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::{JoinSet, LocalSet};
use tokio::time;
use tracing::{debug, warn};

use crate::McConfig;

/// A job's work on the host thread: a future that may be `!Send`, as azalea's
/// are.
type LocalJob = Pin<Box<dyn Future<Output = ()>>>;

/// What travels over a host thread's queue: a `Send` closure that builds the
/// job's future on the host thread.
type Job = Box<dyn FnOnce() -> LocalJob + Send>;

/// Why a job didn't run, or didn't answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum JobError {
    /// The host thread's queue is full, so the job was dropped instead of
    /// waiting.
    #[error("the host thread's job queue is full")]
    QueueFull,
    /// The job didn't answer within the job timeout; the thread may hang.
    #[error("the host thread didn't answer in time")]
    TimedOut,
    /// The host thread has ended or is shutting down, or the job ended without
    /// an answer (it panicked).
    #[error("the host thread has ended")]
    Closed,
}

impl From<JobError> for SessionError {
    fn from(error: JobError) -> Self {
        match error {
            JobError::QueueFull => Self::QueueFull,
            JobError::TimedOut => Self::TimedOut,
            JobError::Closed => Self::Closed,
        }
    }
}

/// Why no host thread was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SpawnError {
    /// As many hung threads were abandoned as the limit allows, so the pool
    /// refuses new ones (ADR-0011).
    #[error("{abandoned} host threads were abandoned, and the limit is {limit}")]
    AbandonedLimit {
        /// How many threads were abandoned.
        abandoned: usize,
        /// `max_abandoned_threads`.
        limit: usize,
    },
    /// The thread's tokio runtime couldn't be built.
    #[error("the host thread's runtime couldn't be built: {0}")]
    Runtime(io::ErrorKind),
    /// The OS thread couldn't be started.
    #[error("the host thread couldn't be started: {0}")]
    Thread(io::ErrorKind),
}

impl From<SpawnError> for ConnectError {
    fn from(_error: SpawnError) -> Self {
        Self::HostUnavailable
    }
}

/// How [`HostThread::shutdown`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownOutcome {
    /// The thread ended within the shutdown timeout.
    Ended,
    /// The thread didn't end in time, so it was abandoned and counted.
    Abandoned,
}

/// Starts one host thread per session (Plan.md P3.2; ADR-0008 §2, ADR-0011),
/// and counts them.
///
/// Clones share the same threads and counters.
#[derive(Debug, Clone)]
pub struct McHostPool {
    shared: Arc<PoolShared>,
}

/// What a pool shares with its threads.
#[derive(Debug)]
struct PoolShared {
    job_queue: NonZeroUsize,
    job_timeout: Duration,
    shutdown_timeout: Duration,
    max_abandoned: NonZeroUsize,
    /// Host threads whose OS thread is still running, abandoned ones included.
    live: AtomicUsize,
    /// Threads abandoned so far. It only counts up.
    abandoned: AtomicUsize,
    /// Threads whose handles were all dropped before a shutdown decided how
    /// they ended. Settled by `spawn` and `abandoned_threads`.
    orphans: Mutex<Vec<Orphan>>,
}

/// A host thread whose last handle was dropped without a shutdown outcome.
/// Dropping the handles stops it, unless it hangs: if it hasn't ended by its
/// deadline, the shutdown timeout after the drop, it's counted as abandoned.
#[derive(Debug)]
struct Orphan {
    bot_id: BotId,
    name: String,
    /// Closes when the thread has ended.
    exit: watch::Receiver<()>,
    deadline: Instant,
}

impl PoolShared {
    /// Settles the orphans without blocking: one that has ended is
    /// forgotten, and one past its deadline is counted as abandoned.
    fn settle_orphans(&self) {
        let now = Instant::now();
        self.lock_orphans().retain(|orphan| {
            // The exit signal's sender is gone once the thread has ended.
            if orphan.exit.has_changed().is_err() {
                return false;
            }
            if now < orphan.deadline {
                return true;
            }
            let abandoned = self
                .abandoned
                .fetch_add(1, Ordering::SeqCst)
                .saturating_add(1);
            warn!(
                bot_id = %orphan.bot_id,
                thread = %orphan.name,
                abandoned,
                limit = self.max_abandoned.get(),
                "a host thread whose handles were dropped didn't end in time, so it was abandoned"
            );
            false
        });
    }

    /// Locks the orphans, poison-tolerantly: the list stays consistent if a
    /// holder panicked.
    fn lock_orphans(&self) -> MutexGuard<'_, Vec<Orphan>> {
        self.orphans.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl McHostPool {
    /// Creates a pool with the queue size, timeouts and abandoned-thread limit
    /// from `config`.
    #[must_use]
    pub fn new(config: &McConfig) -> Self {
        Self {
            shared: Arc::new(PoolShared {
                job_queue: config.job_queue,
                job_timeout: config.job_timeout,
                shutdown_timeout: config.thread_shutdown_timeout,
                max_abandoned: config.max_abandoned_threads,
                live: AtomicUsize::new(0),
                abandoned: AtomicUsize::new(0),
                orphans: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Starts the host thread for `bot_id`'s session. It's named `mc-` plus the
    /// last 12 hex digits of the `BotId`.
    ///
    /// # Errors
    /// - [`SpawnError::AbandonedLimit`] once `max_abandoned_threads` threads
    ///   were abandoned.
    /// - [`SpawnError::Runtime`] or [`SpawnError::Thread`] if the OS refuses
    ///   the runtime or the thread.
    pub fn spawn(&self, bot_id: BotId) -> Result<HostThread, SpawnError> {
        let abandoned = self.abandoned_threads();
        let limit = self.shared.max_abandoned.get();
        if abandoned >= limit {
            warn!(
                %bot_id,
                abandoned,
                limit,
                "refusing a new host thread: the abandoned-thread limit is reached"
            );
            return Err(SpawnError::AbandonedLimit { abandoned, limit });
        }

        // Built here, so an error comes back to the caller. If the thread
        // doesn't start, the guard shuts the runtime down without blocking
        // this (async) caller.
        let runtime = runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map(|runtime| RuntimeGuard(Some(runtime)))
            .map_err(|error| SpawnError::Runtime(error.kind()))?;
        let (jobs_tx, jobs_rx) = mpsc::channel(self.shared.job_queue.get());
        let (stop_tx, stop_rx) = oneshot::channel();
        let (exit_tx, exit_rx) = watch::channel(());
        // Counts the thread from here on. If it doesn't start, dropping the
        // closure drops the guard and takes the count back.
        let guard = LiveGuard::new(Arc::clone(&self.shared), exit_tx);
        let name = thread_name(bot_id);

        let thread = thread::Builder::new()
            .name(name.clone())
            .spawn(move || {
                // The first local, so it drops last: after the LocalSet and
                // the runtime, once nothing of the session is left.
                let _guard = guard;
                host_main(runtime, jobs_rx, stop_rx, bot_id);
            })
            .map_err(|error| SpawnError::Thread(error.kind()))?;
        // The thread is never joined: joining would block the caller's
        // runtime, and a hung thread can't be joined at all. Its exit signal
        // says when it has ended, so the `JoinHandle` is dropped (detached).
        drop(thread);
        debug!(%bot_id, thread = %name, "host thread started");

        Ok(HostThread {
            inner: Arc::new(HostInner {
                name,
                bot_id,
                jobs: jobs_tx,
                stop: Mutex::new(Some(stop_tx)),
                exit: exit_rx,
                state: Mutex::new(HostState::Running),
                pool: Arc::clone(&self.shared),
            }),
        })
    }

    /// Returns how many host threads are running, abandoned ones included.
    #[must_use]
    pub fn live_threads(&self) -> usize {
        self.shared.live.load(Ordering::SeqCst)
    }

    /// Returns how many hung host threads were abandoned so far. It never goes
    /// down, even when an abandoned thread ends after all.
    ///
    /// A thread whose handles were all dropped without a shutdown counts too,
    /// once it's still running the shutdown timeout after the drop.
    #[must_use]
    pub fn abandoned_threads(&self) -> usize {
        self.shared.settle_orphans();
        self.shared.abandoned.load(Ordering::SeqCst)
    }
}

/// One session's host thread. Clones control the same thread.
///
/// The thread ends on [`shutdown`](Self::shutdown), or once every clone is
/// dropped, so a session whose owner forgot to shut it down can't keep a bot
/// running.
#[derive(Clone)]
pub struct HostThread {
    inner: Arc<HostInner>,
}

/// What a host thread's handles share.
struct HostInner {
    name: String,
    bot_id: BotId,
    jobs: mpsc::Sender<Job>,
    /// Taken by the first shutdown. Dropping it, with the last handle, stops
    /// the thread too.
    stop: Mutex<Option<oneshot::Sender<()>>>,
    /// Closes when the thread has ended: its sender is dropped last.
    exit: watch::Receiver<()>,
    state: Mutex<HostState>,
    pool: Arc<PoolShared>,
}

/// Where a host thread is, as far as its handles know.
enum HostState {
    /// Running, or shutting down.
    Running,
    /// Ended within the shutdown timeout.
    Ended,
    /// Abandoned at shutdown, and counted.
    Abandoned,
}

impl HostState {
    /// The outcome of an earlier shutdown, if one has decided.
    const fn outcome(&self) -> Option<ShutdownOutcome> {
        match self {
            Self::Running => None,
            Self::Ended => Some(ShutdownOutcome::Ended),
            Self::Abandoned => Some(ShutdownOutcome::Abandoned),
        }
    }
}

impl core::fmt::Debug for HostThread {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HostThread")
            .field("name", &self.inner.name)
            .finish_non_exhaustive()
    }
}

impl HostThread {
    /// Returns the thread's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Runs `job` on the host thread and returns its output.
    ///
    /// The job is queued when `run` is called, not when the future is first
    /// polled. On the host thread, `job` builds a future that may be `!Send`,
    /// which runs in the thread's `LocalSet`. A job whose caller has given up
    /// by the time it starts is skipped.
    ///
    /// # Errors
    /// - [`JobError::QueueFull`] if the queue is full.
    /// - [`JobError::TimedOut`] if there's no answer within the job timeout.
    /// - [`JobError::Closed`] if the thread has ended or is shutting down, or
    ///   the job panicked.
    pub fn run<F, Fut, T>(
        &self,
        job: F,
    ) -> impl Future<Output = Result<T, JobError>> + Send + 'static
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = T> + 'static,
        T: Send + 'static,
    {
        let (answer_tx, answer_rx) = oneshot::channel();
        let queued = self.inner.queue(Box::new(move || -> LocalJob {
            Box::pin(async move {
                // The caller timed out or dropped its future: skip the work.
                if answer_tx.is_closed() {
                    return;
                }
                let output = job().await;
                // The caller may have given up meanwhile; then nobody waits.
                let _ = answer_tx.send(output);
            })
        }));
        let timeout = self.inner.pool.job_timeout;
        async move {
            queued?;
            match time::timeout(timeout, answer_rx).await {
                Ok(Ok(output)) => Ok(output),
                // The job was dropped without an answer: the thread ended, or
                // the job panicked.
                Ok(Err(_)) => Err(JobError::Closed),
                Err(_) => Err(JobError::TimedOut),
            }
        }
    }

    /// Ends the host thread: it stops taking jobs, drops the jobs still queued
    /// or running, and with them everything the session left on the thread.
    ///
    /// It waits up to the shutdown timeout. A thread that doesn't end by then
    /// hangs: it's abandoned (left to itself, never joined) and counted. Calling it
    /// again, from any clone, returns the first outcome at once.
    pub fn shutdown(&self) -> impl Future<Output = ShutdownOutcome> + Send + 'static {
        let inner = Arc::clone(&self.inner);
        async move {
            if let Some(outcome) = inner.lock_state().outcome() {
                return outcome;
            }
            if let Some(stop) = inner.lock_stop().take() {
                // The thread may have ended already; then nobody listens.
                let _ = stop.send(());
            }
            let ended = time::timeout(inner.pool.shutdown_timeout, wait_ended(inner.exit.clone()))
                .await
                .is_ok();
            inner.decide(ended)
        }
    }

    /// Resolves once the host thread has ended, however that happened.
    pub fn ended(&self) -> impl Future<Output = ()> + Send + 'static {
        wait_ended(self.inner.exit.clone())
    }
}

impl HostInner {
    /// Queues `job`, unless the thread is shutting down or its queue is full.
    fn queue(&self, job: Job) -> Result<(), JobError> {
        if self.lock_stop().is_none() {
            return Err(JobError::Closed);
        }
        self.jobs.try_send(job).map_err(|error| match error {
            TrySendError::Full(_) => JobError::QueueFull,
            TrySendError::Closed(_) => JobError::Closed,
        })
    }

    /// Records how shutdown ended, unless a concurrent shutdown already did:
    /// the first decision wins, so an abandoned thread is counted once.
    fn decide(&self, ended: bool) -> ShutdownOutcome {
        let mut state = self.lock_state();
        if let Some(outcome) = state.outcome() {
            return outcome;
        }
        if ended {
            *state = HostState::Ended;
            debug!(bot_id = %self.bot_id, thread = %self.name, "host thread shut down");
            ShutdownOutcome::Ended
        } else {
            *state = HostState::Abandoned;
            let abandoned = self
                .pool
                .abandoned
                .fetch_add(1, Ordering::SeqCst)
                .saturating_add(1);
            warn!(
                bot_id = %self.bot_id,
                thread = %self.name,
                abandoned,
                limit = self.pool.max_abandoned.get(),
                "the host thread didn't end in time, so it was abandoned"
            );
            ShutdownOutcome::Abandoned
        }
    }

    /// Locks the stop signal. The data stays consistent if a holder panicked,
    /// so a poisoned lock is used as it is.
    fn lock_stop(&self) -> MutexGuard<'_, Option<oneshot::Sender<()>>> {
        self.stop.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Locks the state, poison-tolerantly like [`lock_stop`](Self::lock_stop).
    fn lock_state(&self) -> MutexGuard<'_, HostState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for HostInner {
    /// The last handle is gone, and the stop signal and the job queue close
    /// with it, so the thread ends, unless it hangs. If no shutdown decided
    /// how it ended, the pool keeps it as an orphan, so a hang still counts
    /// against the abandoned-thread limit.
    fn drop(&mut self) {
        let undecided = self
            .state
            .get_mut()
            .map_or_else(
                |poisoned| poisoned.into_inner().outcome(),
                |state| state.outcome(),
            )
            .is_none();
        if undecided {
            let now = Instant::now();
            self.pool.lock_orphans().push(Orphan {
                bot_id: self.bot_id,
                name: core::mem::take(&mut self.name),
                exit: self.exit.clone(),
                deadline: now.checked_add(self.pool.shutdown_timeout).unwrap_or(now),
            });
        }
    }
}

/// Waits until the thread's exit signal closes. Its sender never sends; it's
/// dropped when the thread ends.
async fn wait_ended(mut exit: watch::Receiver<()>) {
    while exit.changed().await.is_ok() {}
}

/// Counts a host thread while it lives, and closes its exit signal when it
/// ends, even by a panic.
struct LiveGuard {
    pool: Arc<PoolShared>,
    /// Dropped after [`Drop::drop`] has run, so the exit signal comes after
    /// the count went down.
    _exit: watch::Sender<()>,
}

impl LiveGuard {
    fn new(pool: Arc<PoolShared>, exit: watch::Sender<()>) -> Self {
        pool.live.fetch_add(1, Ordering::SeqCst);
        Self { pool, _exit: exit }
    }
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.pool.live.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A runtime on its way to its host thread. Dropping a runtime blocks, which
/// panics on an async thread, so if the thread never starts, this shuts the
/// runtime down in the background instead.
struct RuntimeGuard(Option<Runtime>);

impl Drop for RuntimeGuard {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
    }
}

/// The host thread's body: runs the job loop in a `LocalSet` until it stops,
/// then drops the `LocalSet` (every task the session left, such as azalea's
/// runner and its World) and the runtime.
fn host_main(
    mut runtime: RuntimeGuard,
    jobs: mpsc::Receiver<Job>,
    stop: oneshot::Receiver<()>,
    bot_id: BotId,
) {
    let Some(runtime) = runtime.0.take() else {
        return;
    };
    let local = LocalSet::new();
    local.block_on(&runtime, host_loop(jobs, stop, bot_id));
    drop(local);
    drop(runtime);
    debug!(%bot_id, "host thread ended");
}

/// Runs jobs until the stop signal comes, or every handle is dropped.
async fn host_loop(mut jobs: mpsc::Receiver<Job>, mut stop: oneshot::Receiver<()>, bot_id: BotId) {
    let mut tasks = JoinSet::new();
    loop {
        // Every branch is cancel-safe: a oneshot receiver, `mpsc::recv` and
        // `JoinSet::join_next` lose nothing when another branch wins.
        tokio::select! {
            biased;
            // Sent by shutdown, or closed when every handle is dropped.
            _ = &mut stop => break,
            job = jobs.recv() => match job {
                Some(job) => {
                    tasks.spawn_local(job());
                }
                None => break,
            },
            Some(result) = tasks.join_next() => {
                if result.is_err_and(|error| error.is_panic()) {
                    warn!(%bot_id, "a job on the host thread panicked");
                }
            }
        }
    }
}

/// `mc-` plus the last 12 hex digits of the `BotId`: its random part, since
/// the leading timestamp would make truncated names collide. That's 15 bytes,
/// Linux's limit for thread names (ADR-0011).
fn thread_name(bot_id: BotId) -> String {
    format!("mc-{:012x}", bot_id.as_uuid().as_u128() & 0xFFFF_FFFF_FFFF)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::random_tail("018bcfe5-6800-7bab-abab-abababababab", "mc-abababababab")]
    #[case::leading_zeros("018bcfe5-6800-7bab-8000-00000000002a", "mc-00000000002a")]
    #[case::same_timestamp_other_tail("018bcfe5-6800-7bab-abab-0123456789ab", "mc-0123456789ab")]
    fn thread_name_is_mc_plus_the_last_12_hex_digits(#[case] bot_id: &str, #[case] name: &str) {
        assert_eq!(thread_name(bot_id.parse().unwrap()), name);
    }

    #[test]
    fn thread_names_fit_linuxs_15_byte_limit() {
        let name = thread_name("018bcfe5-6800-7bab-abab-abababababab".parse().unwrap());

        assert_eq!(name.len(), 15);
    }

    #[rstest]
    #[case::queue_full(JobError::QueueFull, SessionError::QueueFull)]
    #[case::timed_out(JobError::TimedOut, SessionError::TimedOut)]
    #[case::closed(JobError::Closed, SessionError::Closed)]
    fn job_errors_become_the_matching_session_errors(
        #[case] error: JobError,
        #[case] expected: SessionError,
    ) {
        assert_eq!(SessionError::from(error), expected);
    }

    #[rstest]
    #[case::limit(SpawnError::AbandonedLimit { abandoned: 3, limit: 3 })]
    #[case::runtime(SpawnError::Runtime(io::ErrorKind::OutOfMemory))]
    #[case::thread(SpawnError::Thread(io::ErrorKind::WouldBlock))]
    fn every_spawn_error_means_no_host_is_available(#[case] error: SpawnError) {
        assert_eq!(ConnectError::from(error), ConnectError::HostUnavailable);
    }

    #[rstest]
    #[case::queue_full(JobError::QueueFull.to_string(), "the host thread's job queue is full")]
    #[case::timed_out(JobError::TimedOut.to_string(), "the host thread didn't answer in time")]
    #[case::closed(JobError::Closed.to_string(), "the host thread has ended")]
    #[case::limit(
        SpawnError::AbandonedLimit { abandoned: 3, limit: 3 }.to_string(),
        "3 host threads were abandoned, and the limit is 3"
    )]
    #[case::runtime(
        SpawnError::Runtime(io::ErrorKind::OutOfMemory).to_string(),
        "the host thread's runtime couldn't be built: out of memory"
    )]
    #[case::thread(
        SpawnError::Thread(io::ErrorKind::WouldBlock).to_string(),
        "the host thread couldn't be started: operation would block"
    )]
    fn errors_have_fixed_messages(#[case] message: String, #[case] expected: &str) {
        assert_eq!(message, expected);
    }
}
