//! [`McConfig`]: fleet-mc's tuning values.

use core::num::NonZeroUsize;
use core::time::Duration;

/// fleet-mc's tuning values. [`Default`] gives the ones ADR-0011 decided.
///
/// Only `max_abandoned_threads` is a config key so far: the agent reads it
/// from `[runtime] max_abandoned_threads` (P5.1). P5 adds keys for the others
/// if they're needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McConfig {
    /// How many jobs a host thread's queue holds. A call beyond that fails
    /// with [`JobError::QueueFull`](crate::JobError::QueueFull) instead of
    /// waiting.
    pub job_queue: NonZeroUsize,
    /// How long a call waits for its job's answer before it fails with
    /// [`JobError::TimedOut`](crate::JobError::TimedOut).
    pub job_timeout: Duration,
    /// How long [`HostThread::shutdown`](crate::HostThread::shutdown) waits for
    /// the thread to end before it abandons the thread.
    pub thread_shutdown_timeout: Duration,
    /// How many hung host threads may be abandoned. Once that many are, the
    /// pool refuses new threads, and the agent exits so Docker restarts it
    /// (P5.3).
    pub max_abandoned_threads: NonZeroUsize,
    /// How many events a session's bridge holds before chat is dropped.
    /// Lifecycle events are always delivered.
    pub event_capacity: NonZeroUsize,
    /// How long an online account's join call to the session server may take.
    /// azalea's HTTP client has no timeout of its own, so without this a
    /// stalled session server would hold the login until the connect timeout.
    pub session_join_timeout: Duration,
}

impl Default for McConfig {
    fn default() -> Self {
        Self {
            job_queue: non_zero(32),
            job_timeout: Duration::from_secs(5),
            thread_shutdown_timeout: Duration::from_secs(5),
            max_abandoned_threads: non_zero(3),
            event_capacity: non_zero(64),
            session_join_timeout: Duration::from_secs(10),
        }
    }
}

/// `n` as a `NonZeroUsize`, for the defaults above; `0` would become `1`.
const fn non_zero(n: usize) -> NonZeroUsize {
    match NonZeroUsize::new(n) {
        Some(n) => n,
        None => NonZeroUsize::MIN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_ones_adr_0011_decided() {
        let config = McConfig::default();

        assert_eq!(config.job_queue.get(), 32);
        assert_eq!(config.job_timeout, Duration::from_secs(5));
        assert_eq!(config.thread_shutdown_timeout, Duration::from_secs(5));
        assert_eq!(config.max_abandoned_threads.get(), 3);
        assert_eq!(config.event_capacity.get(), 64);
        assert_eq!(config.session_join_timeout, Duration::from_secs(10));
    }
}
