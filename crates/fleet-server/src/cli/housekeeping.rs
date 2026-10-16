//! The server's periodic housekeeping (Plan.md P7.10, ADR-0016): its first
//! long-lived background task, which `serve` owns and stops at the first
//! signal.
//!
//! Every [`INTERVAL`], one run ([`Housekeeping::run_once`]):
//! 1. the rate limiters forget every client whose bucket is full again
//!    (`retain_recent`), so their memory stays bounded;
//! 2. the lockout's expired streaks are purged, in batches that stop at the
//!    shutdown; a failure is a `warn`, and the next run tries again;
//! 3. the login's expired challenges (mfa and setup tokens) are dropped from
//!    memory;
//! 4. the link flows that finished more than 15 minutes ago are dropped
//!    from memory (P9.4);
//! 5. one `info` line sums up the requests the limits and the lockout
//!    refused since the last run, only if they refused any. A refused
//!    request's own line is at `debug`, so a flood adds one line a minute to
//!    the log, not thousands a second.
//!
//! The first run comes one interval after the start. [`every`] holds the
//! timing, generic over its chore, so it's tested on paused time.

use core::time::Duration;

use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::app::challenges::Challenges;
use crate::app::link_flows::LinkFlows;
use crate::app::lockout::LockoutService;
use crate::http::error::chain;
use crate::http::rate_limit::RateLimits;

/// How often the housekeeping runs.
pub const INTERVAL: Duration = Duration::from_mins(1);

/// What the housekeeping looks after: the rate limits, the lockout, the
/// login's challenges and the link flows.
#[derive(Debug, Clone)]
pub struct Housekeeping {
    limits: RateLimits,
    lockout: LockoutService,
    challenges: Challenges,
    link_flows: LinkFlows,
}

impl Housekeeping {
    /// The housekeeping of `limits`, `lockout`, `challenges` and
    /// `link_flows`.
    #[must_use]
    pub const fn new(
        limits: RateLimits,
        lockout: LockoutService,
        challenges: Challenges,
        link_flows: LinkFlows,
    ) -> Self {
        Self {
            limits,
            lockout,
            challenges,
            link_flows,
        }
    }

    /// Runs every [`INTERVAL`] until `stop` is cancelled.
    pub async fn run(self, stop: CancellationToken) {
        every(INTERVAL, &stop, || self.run_once(&stop)).await;
    }

    /// One run; see the [module docs](self). The purge stops between its
    /// batches once `stop` is cancelled.
    pub async fn run_once(&self, stop: &CancellationToken) {
        self.limits.retain_recent();
        match self.lockout.purge_stale(stop).await {
            Ok(0) => {}
            Ok(deleted) => debug!(deleted, "expired login failure streaks were purged"),
            Err(error) => warn!(
                error = %chain(&error),
                "the purge of expired login failure streaks failed; the next run tries again"
            ),
        }
        match self.challenges.purge_expired() {
            0 => {}
            dropped => debug!(dropped, "expired login challenges were dropped"),
        }
        match self.link_flows.purge_finished() {
            0 => {}
            dropped => debug!(dropped, "finished link flows were dropped"),
        }
        let limits = self.limits.take_refusals();
        let lockout = self.lockout.take_refusals();
        if !limits.is_empty() || !lockout.is_empty() {
            info!(
                per_ip = limits.per_ip,
                login = limits.login,
                refresh = limits.refresh,
                register = limits.register,
                per_user = limits.per_user,
                grants_by_name = limits.grants_by_name,
                enroll = limits.enroll,
                connect = limits.connect,
                locked = lockout.locked,
                in_flight = lockout.in_flight,
                "requests were refused since the last report"
            );
        }
    }
}

/// Runs `chore` every `period`, the first time one period from now, until
/// `stop` is cancelled. A run is never cut short: `stop` is checked between
/// runs, and a chore that must stop sooner watches it itself.
pub async fn every<F, R>(period: Duration, stop: &CancellationToken, mut chore: F)
where
    F: FnMut() -> R,
    R: Future<Output = ()>,
{
    let mut ticks = interval_at(Instant::now() + period, period);
    // A run that took longer than a period isn't followed by a burst of
    // catch-up runs.
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            // Cancel-safe: waiting for the token loses nothing.
            () = stop.cancelled() => return,
            // Cancel-safe, as tokio documents `Interval::tick`.
            _ = ticks.tick() => chore().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// Counts the chore's runs.
    fn counter() -> Arc<AtomicU32> {
        Arc::new(AtomicU32::new(0))
    }

    /// Starts `every` with a counting chore on its own task.
    fn start(
        period: Duration,
        stop: &CancellationToken,
        runs: &Arc<AtomicU32>,
    ) -> tokio::task::JoinHandle<()> {
        let stop = stop.clone();
        let runs = Arc::clone(runs);
        tokio::spawn(async move {
            every(period, &stop, || {
                runs.fetch_add(1, Ordering::SeqCst);
                core::future::ready(())
            })
            .await;
        })
    }

    /// Waits for `task`, failing the test if it hasn't ended within 10 s of
    /// (paused) time: a broken `every` must fail, not hang.
    async fn within(task: tokio::task::JoinHandle<()>) {
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .expect("the task should end within 10 s")
            .unwrap();
    }

    /// Lets the spawned task run after time moved.
    async fn settle() {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_chore_runs_once_a_period_the_first_time_one_period_in() {
        let stop = CancellationToken::new();
        let runs = counter();
        let task = start(Duration::from_secs(60), &stop, &runs);
        settle().await;

        tokio::time::advance(Duration::from_secs(59)).await;
        settle().await;
        assert_eq!(runs.load(Ordering::SeqCst), 0, "not at the start");
        tokio::time::advance(Duration::from_secs(1)).await;
        settle().await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(60)).await;
        settle().await;
        assert_eq!(runs.load(Ordering::SeqCst), 2);

        stop.cancel();
        within(task).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancel_stops_it_between_runs_without_waiting_for_the_next() {
        let stop = CancellationToken::new();
        let runs = counter();
        let task = start(Duration::from_secs(60), &stop, &runs);
        tokio::time::advance(Duration::from_secs(30)).await;
        settle().await;

        stop.cancel();

        tokio::time::timeout(Duration::from_millis(1), task)
            .await
            .expect("it stops at once")
            .unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_isnt_cut_short_by_a_cancel() {
        let stop = CancellationToken::new();
        let finished = counter();
        let task = {
            let stop = stop.clone();
            let finished = Arc::clone(&finished);
            tokio::spawn(async move {
                let inner = stop.clone();
                every(Duration::from_secs(60), &stop, move || {
                    let stop = inner.clone();
                    let finished = Arc::clone(&finished);
                    async move {
                        stop.cancel();
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        finished.fetch_add(1, Ordering::SeqCst);
                    }
                })
                .await;
            })
        };
        settle().await;
        tokio::time::advance(Duration::from_secs(60)).await;
        settle().await;
        assert_eq!(finished.load(Ordering::SeqCst), 0, "the run is underway");

        tokio::time::advance(Duration::from_secs(5)).await;
        within(task).await;

        assert_eq!(finished.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn it_runs_once_a_minute() {
        assert_eq!(INTERVAL, Duration::from_secs(60));
    }
}
