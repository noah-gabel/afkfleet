//! Tests for `McHostPool` and `HostThread`: named threads, jobs, overload,
//! shutdown, abandoning hung threads and the abandoned-thread limit
//! (Plan.md P3.2; ADR-0008 §2, ADR-0011).
//!
//! These tests wait for real OS threads, which tokio's paused clock would
//! race, so they run in real time (ADR-0011). Every wait has an upper bound
//! that only runs out when a test fails, and none of them sleeps. A hung
//! thread is a job blocked on a std `sync_channel`, released at the end.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::future::Future;
use core::num::NonZeroUsize;
use core::time::Duration;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;

use fleet_core::id::BotId;
use fleet_mc::{HostThread, JobError, McConfig, McHostPool, ShutdownOutcome, SpawnError};
use tokio::sync::oneshot;

const BOT: &str = "018bcfe5-6800-7bab-abab-abababababab";
const OTHER_BOT: &str = "018bcfe5-6800-7bab-8bab-0123456789ab";

/// The upper bound for waits that only run out when a test fails.
const WITHIN: Duration = Duration::from_secs(10);
/// A pool timeout that a test expects to run out.
const SHORT: Duration = Duration::from_millis(100);

fn bot(id: &str) -> BotId {
    id.parse().unwrap()
}

/// A pool with the default config, changed by `change`.
fn pool_with(change: impl FnOnce(&mut McConfig)) -> McHostPool {
    let mut config = McConfig::default();
    change(&mut config);
    McHostPool::new(&config)
}

fn pool() -> McHostPool {
    pool_with(|_| {})
}

/// Awaits `future`, failing the test if it takes longer than [`WITHIN`].
async fn within<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(WITHIN, future)
        .await
        .expect("a host thread didn't answer within the test's bound")
}

/// A host thread blocked by a job; [`Hang::release`] unblocks it. Dropping it
/// unblocks the thread too, so a failing test doesn't leave it hanging.
struct Hang {
    release: std_mpsc::SyncSender<()>,
}

impl Hang {
    fn release(self) {
        self.release.send(()).unwrap();
    }
}

/// Blocks `host` with a job that waits on a std channel, and returns once
/// the job runs.
async fn hang(host: &HostThread) -> Hang {
    let (release_tx, release_rx) = std_mpsc::sync_channel::<()>(1);
    let (started_tx, started_rx) = oneshot::channel();
    let job = host.run(move || async move {
        started_tx.send(()).unwrap();
        // Blocks the whole host thread, not just this task.
        let _ = release_rx.recv();
    });
    within(started_rx).await.unwrap();
    // The job has started, so dropping its answer no longer skips it.
    drop(job);
    Hang {
        release: release_tx,
    }
}

// --- Threads and jobs ---

#[tokio::test]
async fn host_thread_is_named_after_its_bot() {
    let host = pool().spawn(bot(BOT)).unwrap();

    let name = within(host.run(|| async { std::thread::current().name().map(str::to_owned) }))
        .await
        .unwrap();

    assert_eq!(name.as_deref(), Some("mc-abababababab"));
    assert_eq!(host.name(), "mc-abababababab");
    within(host.shutdown()).await;
}

#[tokio::test]
async fn jobs_run_on_the_host_thread_inside_a_local_set() {
    let host = pool().spawn(bot(BOT)).unwrap();
    let test_thread = std::thread::current().id();

    let (thread, sum) = within(host.run(|| async {
        // An `Rc` held across an await makes the future `!Send`, and
        // `spawn_local` only works inside a `LocalSet`.
        let shared = Rc::new(20);
        let task = tokio::task::spawn_local({
            let shared = Rc::clone(&shared);
            async move { *shared + 1 }
        });
        let local = task.await.unwrap();
        (std::thread::current().id(), *shared + local)
    }))
    .await
    .unwrap();

    assert_ne!(thread, test_thread);
    assert_eq!(sum, 41);
    within(host.shutdown()).await;
}

#[tokio::test]
async fn live_threads_counts_running_host_threads() {
    let pool = pool();
    assert_eq!(pool.live_threads(), 0);

    let first = pool.spawn(bot(BOT)).unwrap();
    let second = pool.spawn(bot(OTHER_BOT)).unwrap();
    assert_eq!(pool.live_threads(), 2);

    within(first.shutdown()).await;
    assert_eq!(pool.live_threads(), 1);
    within(second.shutdown()).await;
    assert_eq!(pool.live_threads(), 0);
}

#[tokio::test]
async fn panicking_job_returns_closed_and_the_thread_keeps_serving() {
    let host = pool().spawn(bot(BOT)).unwrap();

    let panicked = within(host.run::<_, _, u8>(|| async { panic!("injected job panic") })).await;
    let next = within(host.run(|| async { 7 })).await;

    assert_eq!(panicked, Err(JobError::Closed));
    assert_eq!(next, Ok(7));
    within(host.shutdown()).await;
}

// --- Overload and timeouts ---

#[tokio::test]
async fn full_job_queue_returns_queue_full() {
    let pool = pool_with(|config| config.job_queue = NonZeroUsize::MIN);
    let host = pool.spawn(bot(BOT)).unwrap();
    let hang = hang(&host).await;
    let queued = host.run(|| async { "queued" });

    let overflow = host.run(|| async { "overflow" }).await;

    assert_eq!(overflow, Err(JobError::QueueFull));
    hang.release();
    assert_eq!(within(queued).await, Ok("queued"));
    within(host.shutdown()).await;
}

#[tokio::test]
async fn job_without_an_answer_in_time_returns_timed_out() {
    let pool = pool_with(|config| config.job_timeout = SHORT);
    let host = pool.spawn(bot(BOT)).unwrap();
    let hang = hang(&host).await;

    let answer = within(host.run(|| async {})).await;

    assert_eq!(answer, Err(JobError::TimedOut));
    hang.release();
    within(host.shutdown()).await;
}

#[tokio::test]
async fn job_whose_caller_gave_up_is_skipped() {
    let host = pool().spawn(bot(BOT)).unwrap();
    let hang = hang(&host).await;
    let ran = Arc::new(AtomicBool::new(false));
    let given_up = host.run({
        let ran = Arc::clone(&ran);
        move || async move { ran.store(true, Ordering::SeqCst) }
    });

    drop(given_up);
    hang.release();
    // Jobs run in order, so the given-up job came up before this one.
    within(host.run(|| async {})).await.unwrap();

    assert!(!ran.load(Ordering::SeqCst));
    within(host.shutdown()).await;
}

// --- Shutdown ---

#[tokio::test]
async fn shutdown_ends_an_idle_thread() {
    let pool = pool();
    let host = pool.spawn(bot(BOT)).unwrap();

    let outcome = within(host.shutdown()).await;

    assert_eq!(outcome, ShutdownOutcome::Ended);
    within(host.ended()).await;
    assert_eq!(pool.live_threads(), 0);
    assert_eq!(pool.abandoned_threads(), 0);
}

#[tokio::test]
async fn run_after_shutdown_returns_closed() {
    let host = pool().spawn(bot(BOT)).unwrap();
    let clone = host.clone();
    within(host.shutdown()).await;

    let answer = within(clone.run(|| async {})).await;

    assert_eq!(answer, Err(JobError::Closed));
}

#[tokio::test]
async fn dropping_every_handle_ends_the_thread() {
    let pool = pool();
    let host = pool.spawn(bot(BOT)).unwrap();
    let clone = host.clone();
    let ended = host.ended();

    drop(host);
    // One handle is left, so the thread still serves.
    within(clone.run(|| async {})).await.unwrap();
    drop(clone);

    within(ended).await;
    assert_eq!(pool.live_threads(), 0);
    assert_eq!(pool.abandoned_threads(), 0);
}

#[tokio::test]
async fn second_shutdown_of_an_ended_thread_returns_ended() {
    let host = pool().spawn(bot(BOT)).unwrap();
    let clone = host.clone();
    within(host.shutdown()).await;

    let again = within(clone.shutdown()).await;

    assert_eq!(again, ShutdownOutcome::Ended);
}

// --- Hung threads ---

#[tokio::test]
async fn hung_thread_is_abandoned_on_shutdown_and_counted() {
    let pool = pool_with(|config| config.thread_shutdown_timeout = SHORT);
    let host = pool.spawn(bot(BOT)).unwrap();
    let hang = hang(&host).await;

    let outcome = within(host.shutdown()).await;

    assert_eq!(outcome, ShutdownOutcome::Abandoned);
    assert_eq!(pool.abandoned_threads(), 1);
    // The OS thread is still blocked: it was detached, not ended.
    assert_eq!(pool.live_threads(), 1);
    hang.release();
}

#[tokio::test]
async fn abandoned_count_stays_when_the_thread_ends_later() {
    let pool = pool_with(|config| config.thread_shutdown_timeout = SHORT);
    let host = pool.spawn(bot(BOT)).unwrap();
    let hang = hang(&host).await;
    within(host.shutdown()).await;

    hang.release();
    within(host.ended()).await;

    assert_eq!(pool.live_threads(), 0);
    assert_eq!(pool.abandoned_threads(), 1);
}

#[tokio::test]
async fn second_shutdown_of_an_abandoned_thread_counts_it_once() {
    let pool = pool_with(|config| config.thread_shutdown_timeout = SHORT);
    let host = pool.spawn(bot(BOT)).unwrap();
    let clone = host.clone();
    let hang = hang(&host).await;
    within(host.shutdown()).await;

    let again = within(clone.shutdown()).await;

    assert_eq!(again, ShutdownOutcome::Abandoned);
    assert_eq!(pool.abandoned_threads(), 1);
    hang.release();
}

#[tokio::test]
async fn pool_refuses_new_threads_once_the_abandoned_limit_is_reached() {
    let pool = pool_with(|config| {
        config.thread_shutdown_timeout = SHORT;
        config.max_abandoned_threads = NonZeroUsize::MIN;
    });
    let host = pool.spawn(bot(BOT)).unwrap();
    let hang = hang(&host).await;
    within(host.shutdown()).await;

    let refused = pool.spawn(bot(OTHER_BOT));

    assert_eq!(
        refused.unwrap_err(),
        SpawnError::AbandonedLimit {
            abandoned: 1,
            limit: 1
        }
    );
    hang.release();
}

#[tokio::test]
async fn pool_starts_threads_below_the_abandoned_limit() {
    let pool = pool_with(|config| {
        config.thread_shutdown_timeout = SHORT;
        config.max_abandoned_threads = NonZeroUsize::new(2).unwrap();
    });
    let host = pool.spawn(bot(BOT)).unwrap();
    let hang = hang(&host).await;
    within(host.shutdown()).await;

    let next = pool.spawn(bot(OTHER_BOT)).unwrap();

    assert_eq!(within(next.run(|| async { 1 })).await, Ok(1));
    within(next.shutdown()).await;
    hang.release();
}
