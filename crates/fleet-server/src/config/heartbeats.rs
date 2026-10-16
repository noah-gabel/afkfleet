//! [`Heartbeats`]: how often an agent beats on its stream, and how long the
//! server waits for a sign of life before it gives up on it (Plan.md P10.4,
//! Appendix A; ADR-0019, S23).
//!
//! The same two numbers set HTTP/2's keepalive on the gRPC port: a ping
//! after an interval without anything read, and the connection closed when
//! it isn't answered by the timeout. A silent stream, a silent connection
//! and a stale agent therefore all end at `heartbeat_timeout_secs`.

use core::time::Duration;

use super::Bounds;

/// The heartbeat settings of `[grpc]`: how often an agent beats, and how
/// long the server waits for a sign of life. They also set the gRPC port's
/// HTTP/2 keepalive, so a silent stream and a silent connection both end at
/// the timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Heartbeats {
    interval: Duration,
    timeout: Duration,
}

/// Why two numbers aren't [`Heartbeats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HeartbeatsError {
    /// The interval is outside [`Heartbeats::INTERVAL_SECS`].
    #[error("the heartbeat interval is outside its range")]
    IntervalOutOfRange,
    /// The timeout is outside [`Heartbeats::TIMEOUT_SECS`].
    #[error("the heartbeat timeout is outside its range")]
    TimeoutOutOfRange,
    /// The interval is more than half the timeout, so one late beat would
    /// end a stream.
    #[error("the heartbeat interval is more than half the timeout")]
    MoreThanHalf,
}

impl Heartbeats {
    /// `heartbeat_interval_secs`: 1 to 60. Default 10. It lies within what
    /// `Welcome` carries on the wire (1 s to 5 minutes).
    pub const INTERVAL_SECS: Bounds = Bounds { min: 1, max: 60 };
    /// `heartbeat_timeout_secs`: 2 to 300, at least twice the interval.
    /// Default 30.
    pub const TIMEOUT_SECS: Bounds = Bounds { min: 2, max: 300 };

    /// The settings of `interval` and `timeout` seconds.
    ///
    /// # Errors
    /// [`HeartbeatsError`] for a value outside its range, or an interval of
    /// more than half the timeout.
    pub fn try_from_secs(interval: u32, timeout: u32) -> Result<Self, HeartbeatsError> {
        let within = |value: u32, bounds: Bounds| (bounds.min..=bounds.max).contains(&value);
        if !within(interval, Self::INTERVAL_SECS) {
            return Err(HeartbeatsError::IntervalOutOfRange);
        }
        if !within(timeout, Self::TIMEOUT_SECS) {
            return Err(HeartbeatsError::TimeoutOutOfRange);
        }
        if u64::from(interval) * 2 > u64::from(timeout) {
            return Err(HeartbeatsError::MoreThanHalf);
        }
        Ok(Self {
            interval: Duration::from_secs(u64::from(interval)),
            timeout: Duration::from_secs(u64::from(timeout)),
        })
    }

    /// How often the agent beats: what `Welcome` tells it.
    #[must_use]
    pub const fn interval(&self) -> Duration {
        self.interval
    }

    /// How long the server waits for any message from the agent.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// When the port's HTTP/2 keepalive pings a connection it read nothing
    /// from: after [`interval`](Self::interval).
    #[must_use]
    pub const fn keepalive_interval(&self) -> Duration {
        self.interval
    }

    /// How long the keepalive waits for its ping's answer, so a silent
    /// connection closes at [`timeout`](Self::timeout).
    #[must_use]
    pub const fn keepalive_timeout(&self) -> Duration {
        self.timeout.saturating_sub(self.interval)
    }
}

impl Default for Heartbeats {
    /// Appendix A's 10 and 30 seconds.
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(10),
            timeout: Duration::from_secs(30),
        }
    }
}

#[cfg(test)]
mod tests {
    use fleet_proto::control::Welcome;
    use rstest::rstest;

    use super::*;

    #[test]
    fn the_keepalive_ends_exactly_at_the_heartbeat_timeout() {
        for (interval, timeout) in [(1, 2), (10, 30), (60, 300)] {
            let heartbeats = Heartbeats::try_from_secs(interval, timeout).unwrap();

            assert_eq!(heartbeats.keepalive_interval(), heartbeats.interval());
            assert_eq!(
                heartbeats.keepalive_interval() + heartbeats.keepalive_timeout(),
                heartbeats.timeout()
            );
        }
    }

    #[test]
    fn the_interval_range_lies_within_the_wires() {
        let lowest = Duration::from_secs(u64::from(Heartbeats::INTERVAL_SECS.min));
        let highest = Duration::from_secs(u64::from(Heartbeats::INTERVAL_SECS.max));

        assert!(lowest >= Welcome::MIN_HEARTBEAT_INTERVAL);
        assert!(highest <= Welcome::MAX_HEARTBEAT_INTERVAL);
    }

    #[rstest]
    #[case::interval_zero(0, 30, HeartbeatsError::IntervalOutOfRange)]
    #[case::interval_too_long(61, 300, HeartbeatsError::IntervalOutOfRange)]
    #[case::timeout_too_short(1, 1, HeartbeatsError::TimeoutOutOfRange)]
    #[case::timeout_too_long(10, 301, HeartbeatsError::TimeoutOutOfRange)]
    #[case::more_than_half(16, 30, HeartbeatsError::MoreThanHalf)]
    fn settings_out_of_their_rules_are_refused(
        #[case] interval: u32,
        #[case] timeout: u32,
        #[case] error: HeartbeatsError,
    ) {
        assert_eq!(Heartbeats::try_from_secs(interval, timeout), Err(error));
    }

    #[test]
    fn the_defaults_are_appendix_as() {
        assert_eq!(
            Heartbeats::default(),
            Heartbeats::try_from_secs(10, 30).unwrap()
        );
        assert_eq!(Heartbeats::default().interval(), Duration::from_secs(10));
    }
}
