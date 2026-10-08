//! [`ChatBucket`]: a bot's chat rate limit.

use core::fmt;
use core::num::NonZeroU32;
use core::time::Duration;
use std::sync::Arc;

use governor::middleware::NoOpMiddleware;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter};

use crate::clock::TokioClock;

/// governor's limiter for one bot, on tokio's clock.
type Limiter = RateLimiter<NotKeyed, InMemoryState, TokioClock, NoOpMiddleware<std::time::Instant>>;

/// Why a [`ChatBucket`] can't be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChatBucketError {
    /// The interval is zero, which would mean no limit at all.
    #[error("the chat interval is zero")]
    ZeroInterval,
}

/// A bot's chat rate limit: a token bucket that holds `burst` messages and
/// gains one per `interval` (Plan.md §7.4: one per 3 s, burst 3).
///
/// It runs on tokio's clock, so paused test time controls it (Plan.md §8).
/// Clones share the same bucket. User chat and mode chat draw from it, and it
/// outlives the bot's sessions; the supervisor keeps it across actor restarts
/// (P4.7), so neither a reconnect nor a crash refills it.
#[derive(Clone)]
pub struct ChatBucket {
    limiter: Arc<Limiter>,
}

impl ChatBucket {
    /// Builds a full bucket.
    ///
    /// # Errors
    /// [`ChatBucketError::ZeroInterval`] if `interval` is zero.
    pub fn new(interval: Duration, burst: NonZeroU32) -> Result<Self, ChatBucketError> {
        let quota = Quota::with_period(interval)
            .ok_or(ChatBucketError::ZeroInterval)?
            .allow_burst(burst);
        Ok(Self {
            limiter: Arc::new(RateLimiter::direct_with_clock(quota, TokioClock)),
        })
    }

    /// Takes one message's token, if the bucket has one.
    pub(crate) fn try_take(&self) -> bool {
        self.limiter.check().is_ok()
    }
}

impl fmt::Debug for ChatBucket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatBucket").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn burst(n: u32) -> NonZeroU32 {
        NonZeroU32::new(n).unwrap()
    }

    #[test]
    fn a_zero_interval_is_refused() {
        assert_eq!(
            ChatBucket::new(Duration::ZERO, burst(3)).err(),
            Some(ChatBucketError::ZeroInterval)
        );
        assert_eq!(
            ChatBucketError::ZeroInterval.to_string(),
            "the chat interval is zero"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_bucket_lets_its_burst_through_then_one_per_interval() {
        let bucket = ChatBucket::new(Duration::from_secs(3), burst(3)).unwrap();

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
        let bucket = ChatBucket::new(Duration::from_secs(3), burst(1)).unwrap();
        let clone = bucket.clone();

        assert!(bucket.try_take());

        assert!(!clone.try_take());
    }

    #[test]
    fn debug_shows_no_internals() {
        let bucket = ChatBucket::new(Duration::from_secs(3), burst(3)).unwrap();

        assert_eq!(format!("{bucket:?}"), "ChatBucket { .. }");
    }
}
