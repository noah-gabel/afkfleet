//! [`ChatBucket`]: a bot's chat rate limit, at the rate its spec sets
//! (ADR-0020, S3).

use core::fmt;
use core::num::NonZeroU32;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use fleet_core::chat::ChatRate;
use governor::middleware::NoOpMiddleware;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter};

use crate::clock::TokioClock;

/// governor's limiter for one bot, on tokio's clock.
type Limiter = RateLimiter<NotKeyed, InMemoryState, TokioClock, NoOpMiddleware<std::time::Instant>>;

/// A bucket's rate, and the limiter that keeps it.
struct Shared {
    rate: ChatRate,
    limiter: Limiter,
}

/// A bot's chat rate limit: a token bucket that holds `burst` messages and
/// gains one per `interval`, at the [`ChatRate`] of the bot's spec (Plan.md
/// §7.4: one per 3 s, burst 3, by default).
///
/// It runs on tokio's clock, so paused test time controls it (Plan.md §8).
/// Clones share the same bucket. User chat and mode chat draw from it, and it
/// outlives the bot's sessions; the supervisor keeps it across actor restarts
/// (P4.7), so neither a reconnect nor a crash refills it. A spec with another
/// rate swaps it for every clone without refilling it: it keeps what it
/// held, up to the new burst (ADR-0020, S3; PR #59's review).
#[derive(Clone)]
pub struct ChatBucket {
    shared: Arc<Mutex<Shared>>,
}

impl ChatBucket {
    /// Builds a full bucket at `rate`.
    #[must_use]
    pub fn new(rate: ChatRate) -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared {
                rate,
                limiter: limiter(rate),
            })),
        }
    }

    /// The rate it keeps.
    #[must_use]
    pub fn rate(&self) -> ChatRate {
        self.lock().rate
    }

    /// Keeps `rate` from now on, for every clone. A change never refills the
    /// bucket: it keeps what it holds, up to the new burst (PR #59's review),
    /// and the same rate changes nothing.
    pub fn set_rate(&self, rate: ChatRate) {
        let mut shared = self.lock();
        if shared.rate == rate {
            return;
        }
        let left = remaining(&shared.limiter, shared.rate.burst());
        let limiter = limiter(rate);
        let spent = rate.burst().get().saturating_sub(left);
        if let Some(spent) = NonZeroU32::new(spent) {
            // A full new bucket always holds its burst, so this can't fail.
            let _ = limiter.check_n(spent);
        }
        *shared = Shared { rate, limiter };
    }

    /// Takes one message's token, if the bucket has one.
    pub(crate) fn try_take(&self) -> bool {
        self.lock().limiter.check().is_ok()
    }

    /// Locks the bucket, poison-tolerantly: a check never leaves it half
    /// changed. Never held across an await.
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// How many messages `limiter`, whose burst is `burst`, lets through now.
/// Measuring takes them, so it's only for a limiter about to be replaced.
fn remaining(limiter: &Limiter, burst: NonZeroU32) -> u32 {
    (1..=burst.get())
        .rev()
        .filter_map(NonZeroU32::new)
        .find(|&n| matches!(limiter.check_n(n), Ok(Ok(()))))
        .map_or(0, NonZeroU32::get)
}

/// A full limiter at `rate`.
fn limiter(rate: ChatRate) -> Limiter {
    let quota = Quota::with_period(rate.interval()).map_or_else(
        || Quota::per_second(rate.burst()),
        |quota| quota.allow_burst(rate.burst()),
    );
    RateLimiter::direct_with_clock(quota, TokioClock)
}

impl fmt::Debug for ChatBucket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatBucket").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use super::*;

    fn rate(interval_secs: u32, burst: u32) -> ChatRate {
        ChatRate::try_new(interval_secs, burst).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_bucket_lets_its_burst_through_then_one_per_interval() {
        let bucket = ChatBucket::new(rate(3, 3));

        assert_eq!(
            [bucket.try_take(), bucket.try_take(), bucket.try_take()],
            [true; 3]
        );
        assert!(!bucket.try_take());

        tokio::time::advance(Duration::from_millis(2_999)).await;
        assert!(!bucket.try_take());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(bucket.try_take());
        assert!(!bucket.try_take());
    }

    #[tokio::test(start_paused = true)]
    async fn clones_share_one_bucket() {
        let bucket = ChatBucket::new(rate(3, 1));
        let clone = bucket.clone();

        assert!(bucket.try_take());

        assert!(!clone.try_take());
    }

    #[tokio::test(start_paused = true)]
    async fn buckets_at_one_rate_are_separate() {
        let first = ChatBucket::new(rate(3, 1));
        let second = ChatBucket::new(rate(3, 1));

        assert!(first.try_take());

        assert!(second.try_take(), "the second bucket is still full");
    }

    /// An operator's new rate reaches every clone, keeping what the bucket
    /// held, up to the new burst: a change never refills it (ADR-0020, S3;
    /// PR #59's review).
    #[tokio::test(start_paused = true)]
    async fn a_new_rate_reaches_every_clone_keeping_what_is_left() {
        let bucket = ChatBucket::new(rate(60, 3));
        let clone = bucket.clone();
        assert_eq!([bucket.try_take(), bucket.try_take()], [true; 2]);

        clone.set_rate(rate(5, 2));

        assert_eq!(bucket.rate(), rate(5, 2));
        assert!(bucket.try_take(), "the one message left");
        assert!(!clone.try_take());
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(clone.try_take(), "one more at the new interval");
    }

    #[tokio::test(start_paused = true)]
    async fn a_bot_that_used_its_burst_gets_no_new_one_when_the_rate_changes() {
        let bucket = ChatBucket::new(rate(60, 2));
        assert_eq!([bucket.try_take(), bucket.try_take()], [true; 2]);

        bucket.set_rate(rate(60, 8));

        assert!(!bucket.try_take());
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_bucket_keeps_at_most_the_new_burst() {
        let bucket = ChatBucket::new(rate(60, 3));

        bucket.set_rate(rate(60, 2));

        assert_eq!([bucket.try_take(), bucket.try_take()], [true; 2]);
        assert!(!bucket.try_take());
    }

    #[tokio::test(start_paused = true)]
    async fn the_same_rate_keeps_the_bucket_as_it_is() {
        let bucket = ChatBucket::new(rate(60, 1));
        assert!(bucket.try_take());

        bucket.set_rate(rate(60, 1));

        assert!(!bucket.try_take(), "not refilled");
        assert_eq!(bucket.rate(), rate(60, 1));
    }

    #[test]
    fn debug_shows_no_internals() {
        let bucket = ChatBucket::new(rate(3, 3));

        assert_eq!(format!("{bucket:?}"), "ChatBucket { .. }");
    }
}
