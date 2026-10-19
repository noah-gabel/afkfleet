//! [`ChatBucket`]: a bot's chat rate limit, at the rate its spec sets
//! (ADR-0020, S3).

use core::fmt;
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
/// rate swaps it for every clone, full: only an operator's change does that
/// (ADR-0020, S3).
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

    /// Keeps `rate` from now on: another rate gives every clone a full bucket
    /// at it, and the same rate changes nothing.
    pub fn set_rate(&self, rate: ChatRate) {
        let mut shared = self.lock();
        if shared.rate != rate {
            *shared = Shared {
                rate,
                limiter: limiter(rate),
            };
        }
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

    /// An operator's new rate reaches every clone, as a full bucket at it
    /// (ADR-0020, S3).
    #[tokio::test(start_paused = true)]
    async fn a_new_rate_gives_every_clone_a_full_bucket_at_it() {
        let bucket = ChatBucket::new(rate(60, 1));
        let clone = bucket.clone();
        assert!(bucket.try_take());

        clone.set_rate(rate(5, 2));

        assert_eq!(bucket.rate(), rate(5, 2));
        assert_eq!([bucket.try_take(), bucket.try_take()], [true; 2]);
        assert!(!clone.try_take());
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(clone.try_take());
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
