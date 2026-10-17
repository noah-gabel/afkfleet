//! A welcomed stream's session requests (Plan.md P10.6; ADR-0019, S57,
//! R40): the queue its session works them from, so the stream's reads and
//! heartbeats never wait for a grant.
//!
//! - **One waiting request per bot,** in arrival order: a newer request for
//!   a bot takes its waiting one's place, and the older one is never
//!   answered (the agent stopped waiting for it; S3). Two requests are never
//!   merged: a fresh request answered with a token minted before it would
//!   fail the bot (`Failed(Auth)`).
//! - **A newer request aborts its bot's running one** (R40), at the older
//!   one's next await. Stopped there, it sends nothing; the token flight it
//!   awaited goes on, on the session-token service's own tracker, so
//!   nothing it stores is lost. One already past its last await (the
//!   fence's read, or a failed call it now denies) finishes and may still
//!   queue its answer: on this same stream, for the same bot, under its own
//!   older number, which the agent no longer waits for and drops (S3, S62).
//! - **At most [`GrantBounds::workers`] run at once,** on the queue's own
//!   `JoinSet`, which the session polls in its loop; at most
//!   [`GrantBounds::cap`] requests are held, waiting or running, and one more
//!   bot's request is [`Full`].
//! - **The stream's end** drops the waiting requests unanswered and aborts
//!   the running ones ([`GrantQueue::shutdown`]).

use std::collections::{HashMap, VecDeque};

use fleet_core::id::BotId;
use fleet_proto::control::{MAX_BOTS, SessionRequest};
use tokio::task::{AbortHandle, Id, JoinSet};
use tracing::error;

/// How many session requests of one stream are worked on at once (S57).
pub const GRANT_WORKERS: usize = 4;

/// How many session requests a stream holds and works on at once. Tests
/// lower them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantBounds {
    /// How many requests it holds, waiting or running: one per bot, so an
    /// agent's thousand bots fit.
    pub cap: usize,
    /// How many run at once.
    pub workers: usize,
}

impl Default for GrantBounds {
    /// [`MAX_BOTS`] held, [`GRANT_WORKERS`] at once.
    fn default() -> Self {
        Self {
            cap: MAX_BOTS,
            workers: GRANT_WORKERS,
        }
    }
}

/// The queue holds as many requests as it may: one more bot's request is
/// refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Full;

/// One stream's session requests; see the [module docs](self).
#[derive(Debug)]
pub(super) struct GrantQueue {
    bounds: GrantBounds,
    /// The waiting requests, at most one per bot, in arrival order.
    waiting: VecDeque<SessionRequest>,
    /// Each running request's task, by its bot.
    running: HashMap<BotId, AbortHandle>,
    workers: JoinSet<BotId>,
}

impl GrantQueue {
    /// An empty queue within `bounds`.
    pub(super) fn new(bounds: GrantBounds) -> Self {
        Self {
            bounds,
            waiting: VecDeque::new(),
            running: HashMap::new(),
            workers: JoinSet::new(),
        }
    }

    /// Queues `request`: it takes its bot's waiting request's place, and its
    /// bot's running request is aborted.
    ///
    /// # Errors
    /// [`Full`] when the queue holds as many requests as it may and none is
    /// its bot's.
    pub(super) fn push(&mut self, request: SessionRequest) -> Result<(), Full> {
        let bot = request.bot_id;
        if let Some(waiting) = self
            .waiting
            .iter_mut()
            .find(|waiting| waiting.bot_id == bot)
        {
            *waiting = request;
            return Ok(());
        }
        if let Some(running) = self.running.remove(&bot) {
            // Stopped at its next await: one already past its last await
            // may still queue its answer here, under its own number, which
            // the agent drops. Its token flight goes on, on the
            // session-token service's own tracker (R40).
            running.abort();
            self.waiting.push_back(request);
            return Ok(());
        }
        if self.waiting.len().saturating_add(self.running.len()) >= self.bounds.cap {
            return Err(Full);
        }
        self.waiting.push_back(request);
        Ok(())
    }

    /// The next request to work on, while fewer than the bound run.
    pub(super) fn next(&mut self) -> Option<SessionRequest> {
        if self.running.len() >= self.bounds.workers {
            return None;
        }
        self.waiting.pop_front()
    }

    /// Runs `work` for `bot`'s request on the queue's own tasks.
    pub(super) fn run<F>(&mut self, bot: BotId, work: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let task = self.workers.spawn(async move {
            work.await;
            bot
        });
        self.running.insert(bot, task);
    }

    /// Whether any request runs, or an aborted one still has to be reaped.
    pub(super) fn is_busy(&self) -> bool {
        !self.workers.is_empty()
    }

    /// Waits until a task ends: the bot of a request that ran to its end,
    /// or `None` for an aborted one, or without tasks. Cancel-safe, as
    /// `JoinSet::join_next_with_id` is.
    pub(super) async fn finished(&mut self) -> Option<BotId> {
        match self.workers.join_next_with_id().await? {
            Ok((id, bot)) => {
                self.ended(id, bot);
                Some(bot)
            }
            Err(failure) => {
                if failure.is_panic() {
                    error!(error = %failure, "a session request's grant panicked");
                }
                let id = failure.id();
                self.running.retain(|_, task| task.id() != id);
                None
            }
        }
    }

    /// Drops the waiting requests unanswered, aborts the running ones and
    /// waits for them.
    pub(super) async fn shutdown(&mut self) {
        self.waiting.clear();
        self.running.clear();
        self.workers.shutdown().await;
    }

    /// The task `id` ended: frees its bot's place if it still holds it.
    fn ended(&mut self, id: Id, bot: BotId) {
        if self.running.get(&bot).is_some_and(|task| task.id() == id) {
            self.running.remove(&bot);
        }
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU64;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use chrono::DateTime;
    use fleet_proto::control::SessionRequestId;
    use tokio::sync::oneshot;

    use super::*;

    fn bot(n: u8) -> BotId {
        BotId::new_v7(DateTime::UNIX_EPOCH, [n; 10]).unwrap()
    }

    fn request(number: u64, bot_n: u8) -> SessionRequest {
        SessionRequest {
            id: SessionRequestId::new(NonZeroU64::new(number).unwrap()),
            bot_id: bot(bot_n),
            fresh: false,
        }
    }

    fn queue(cap: usize, workers: usize) -> GrantQueue {
        GrantQueue::new(GrantBounds { cap, workers })
    }

    /// Work that waits for its gate and says when it was dropped unfinished.
    fn gated(dropped: &Arc<AtomicBool>) -> (oneshot::Sender<()>, impl Future<Output = ()> + use<>) {
        struct Drops(Arc<AtomicBool>, bool);
        impl Drop for Drops {
            fn drop(&mut self) {
                if !self.1 {
                    self.0.store(true, Ordering::Release);
                }
            }
        }
        let (open, gate) = oneshot::channel::<()>();
        let mut guard = Drops(Arc::clone(dropped), false);
        let work = async move {
            let _ = gate.await;
            guard.1 = true;
            drop(guard);
        };
        (open, work)
    }

    #[test]
    fn the_defaults_hold_every_bot_and_run_four_at_once() {
        assert_eq!(
            GrantBounds::default(),
            GrantBounds {
                cap: 1000,
                workers: 4
            }
        );
    }

    #[tokio::test]
    async fn one_request_waits_per_bot_and_a_newer_one_takes_its_place() {
        let mut queue = queue(10, 0);
        queue.push(request(1, 1)).unwrap();
        queue.push(request(2, 2)).unwrap();
        queue.push(request(3, 1)).unwrap();
        queue.bounds.workers = 10;

        let order: Vec<SessionRequest> = core::iter::from_fn(|| queue.next()).collect();

        assert_eq!(order, [request(3, 1), request(2, 2)]);
    }

    #[tokio::test]
    async fn at_most_the_bound_run_at_once() {
        let mut queue = queue(10, 2);
        for n in 1..=3 {
            queue.push(request(u64::from(n), n)).unwrap();
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let (first, work) = gated(&dropped);
        let next = queue.next().unwrap();
        queue.run(next.bot_id, work);
        let (_second, work) = gated(&dropped);
        let next = queue.next().unwrap();
        queue.run(next.bot_id, work);

        let third_before = queue.next();
        first.send(()).unwrap();
        let ended = tokio::time::timeout(BOUND, queue.finished()).await.unwrap();
        let third_after = queue.next();

        assert_eq!(third_before, None, "two run already");
        assert_eq!(ended, Some(bot(1)));
        assert_eq!(third_after, Some(request(3, 3)));
    }

    #[tokio::test]
    async fn a_newer_request_aborts_its_bots_running_one() {
        let mut queue = queue(10, 1);
        queue.push(request(1, 1)).unwrap();
        let dropped = Arc::new(AtomicBool::new(false));
        let (_gate, work) = gated(&dropped);
        let first = queue.next().unwrap();
        queue.run(first.bot_id, work);

        queue.push(request(2, 1)).unwrap();
        let reaped = tokio::time::timeout(BOUND, queue.finished()).await.unwrap();
        let next = queue.next();

        assert!(
            dropped.load(Ordering::Acquire),
            "the older one's work was dropped"
        );
        assert_eq!(reaped, None, "an aborted request never reports its bot");
        assert_eq!(next, Some(request(2, 1)), "the newer one runs in its place");
    }

    #[tokio::test]
    async fn a_full_queue_refuses_one_more_bot_but_takes_a_replacement() {
        let mut queue = queue(2, 1);
        queue.push(request(1, 1)).unwrap();
        queue.push(request(2, 2)).unwrap();
        let dropped = Arc::new(AtomicBool::new(false));
        let (_gate, work) = gated(&dropped);
        let running = queue.next().unwrap();
        queue.run(running.bot_id, work);

        let third = queue.push(request(3, 3));
        let replacement = queue.push(request(4, 2));
        let rerun = queue.push(request(5, 1));

        assert_eq!(third, Err(Full), "a running request counts too");
        assert_eq!(replacement, Ok(()));
        assert_eq!(rerun, Ok(()), "a running bot's newer request replaces it");
    }

    #[tokio::test]
    async fn the_shutdown_drops_the_waiting_and_aborts_the_running() {
        let mut queue = queue(10, 1);
        queue.push(request(1, 1)).unwrap();
        queue.push(request(2, 2)).unwrap();
        let dropped = Arc::new(AtomicBool::new(false));
        let (_gate, work) = gated(&dropped);
        let first = queue.next().unwrap();
        queue.run(first.bot_id, work);

        tokio::time::timeout(BOUND, queue.shutdown()).await.unwrap();
        queue.bounds.workers = 10;

        assert!(
            dropped.load(Ordering::Acquire),
            "the running work was dropped"
        );
        assert_eq!(queue.next(), None, "nothing waits");
        assert!(!queue.is_busy());
    }

    #[tokio::test]
    async fn a_request_that_ran_to_its_end_frees_its_place_once() {
        let mut queue = queue(10, 1);
        queue.push(request(1, 1)).unwrap();
        let first = queue.next().unwrap();
        queue.run(first.bot_id, async {});

        let ended = tokio::time::timeout(BOUND, queue.finished()).await.unwrap();

        assert_eq!(ended, Some(bot(1)));
        assert!(!queue.is_busy());
        assert_eq!(
            tokio::time::timeout(BOUND, queue.finished()).await.unwrap(),
            None,
            "no task left"
        );
    }

    /// How long a test waits for a task.
    const BOUND: core::time::Duration = core::time::Duration::from_secs(5);
}
