//! The watchdog's check: has an online session stalled? (Plan.md P4.6;
//! ADR-0008 §5, ADR-0010.)

use core::time::Duration;
use std::time::Instant;

use fleet_core::mc::Liveness;

/// How an online session stalled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stall {
    /// No tick for the watchdog timeout: the session hung.
    Tick,
    /// No packet for the liveness timeout: the server froze or the link
    /// died. Ticks are client-side and keep going then.
    Packet,
}

/// Compares `liveness` with `now`: a stamp is stale when it's at least its
/// timeout old. When both are stale, the tick stall wins, because a hung
/// host thread stops both (ADR-0010).
///
/// `now` is tokio's clock as a std `Instant`, never `Instant::now()` or
/// `elapsed()`, so paused time controls the watchdog (ADR-0013). A stamp
/// later than `now` counts as fresh.
#[must_use]
pub(super) fn stall(
    liveness: Liveness,
    now: Instant,
    tick_timeout: Duration,
    packet_timeout: Duration,
) -> Option<Stall> {
    let stale = |stamp: Instant, timeout: Duration| now.saturating_duration_since(stamp) >= timeout;
    if stale(liveness.last_tick, tick_timeout) {
        Some(Stall::Tick)
    } else if stale(liveness.last_packet, packet_timeout) {
        Some(Stall::Packet)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    const TIMEOUT: Duration = Duration::from_secs(30);

    /// `age` before `now`.
    fn ago(now: Instant, age: Duration) -> Instant {
        now.checked_sub(age).unwrap()
    }

    /// Stamps a tick and a packet `tick_age` and `packet_age` before `now`.
    fn stamps(now: Instant, tick_age: Duration, packet_age: Duration) -> Liveness {
        Liveness {
            last_tick: ago(now, tick_age),
            last_packet: ago(now, packet_age),
        }
    }

    fn now() -> Instant {
        tokio::time::Instant::now().into_std() + Duration::from_secs(3_600)
    }

    #[rstest]
    #[case::fresh(Duration::ZERO, Duration::ZERO, None)]
    #[case::tick_just_fresh(Duration::from_millis(29_999), Duration::ZERO, None)]
    #[case::tick_exactly_stale(TIMEOUT, Duration::ZERO, Some(Stall::Tick))]
    #[case::packet_just_fresh(Duration::ZERO, Duration::from_millis(29_999), None)]
    #[case::packet_exactly_stale(Duration::ZERO, TIMEOUT, Some(Stall::Packet))]
    #[case::both_stale(TIMEOUT, Duration::from_secs(60), Some(Stall::Tick))]
    fn a_stamp_is_stale_from_its_timeout_on_and_the_tick_stall_wins(
        #[case] tick_age: Duration,
        #[case] packet_age: Duration,
        #[case] expected: Option<Stall>,
    ) {
        let now = now();

        let found = stall(stamps(now, tick_age, packet_age), now, TIMEOUT, TIMEOUT);

        assert_eq!(found, expected);
    }

    #[test]
    fn each_stamp_has_its_own_timeout() {
        let now = now();
        let liveness = stamps(now, Duration::from_secs(20), Duration::from_secs(20));

        assert_eq!(
            stall(
                liveness,
                now,
                Duration::from_secs(30),
                Duration::from_secs(10)
            ),
            Some(Stall::Packet)
        );
        assert_eq!(
            stall(
                liveness,
                now,
                Duration::from_secs(10),
                Duration::from_secs(30)
            ),
            Some(Stall::Tick)
        );
    }

    #[test]
    fn stamps_later_than_now_are_fresh() {
        let now = now();
        let later = now + Duration::from_secs(5);
        let liveness = Liveness {
            last_tick: later,
            last_packet: later,
        };

        assert_eq!(stall(liveness, now, TIMEOUT, TIMEOUT), None);
    }
}
