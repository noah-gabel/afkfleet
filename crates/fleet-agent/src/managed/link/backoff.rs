//! [`Backoff`]: how long the agent waits between attempts on the control
//! stream (ADR-0019, S67).
//!
//! fleet-core's [`RetryPolicy`] with a 1 s base, `RECONNECT_DELAY_MAX`
//! (30 s) as its cap and a 60 s stable period: 1–2, 2–4, 4–8 and 8–16 s,
//! then 15–30 s. A slow retry always waits the plateau. The count starts
//! over only after a stream that stayed welcomed for the stable period, so
//! a server that welcomes and drops the agent at once doesn't make it
//! hammer. A `retry-after` is honoured, capped. No wait exceeds
//! `RECONNECT_DELAY_MAX`, which S45's placement grace counts on.
//!
//! The jitter is drawn from a seed the process got from the OS, through a
//! splitmix64 sequence, since the agent has no `rand` of its own.

use core::num::NonZeroU32;
use core::time::Duration;

use fleet_core::resilience::{RetryPolicy, RetryPolicyError};
use fleet_proto::control::RECONNECT_DELAY_MAX;

use super::ends::Next;

/// The first wait.
pub const BASE_DELAY: Duration = Duration::from_secs(1);

/// How long a stream must stay welcomed for the count to start over.
pub const STABLE_AFTER: Duration = Duration::from_secs(60);

/// The attempt a slow retry waits as: past every doubling.
const PLATEAU: NonZeroU32 = NonZeroU32::MAX;

/// The reconnect policy: [`BASE_DELAY`], [`RECONNECT_DELAY_MAX`],
/// [`STABLE_AFTER`].
///
/// # Errors
/// [`RetryPolicyError`], which these constants never cause; the run maps
/// it to a startup error.
pub fn policy() -> Result<RetryPolicy, RetryPolicyError> {
    RetryPolicy::try_new(BASE_DELAY, RECONNECT_DELAY_MAX, STABLE_AFTER)
}

/// The waits between attempts; see the [module docs](self).
#[derive(Debug, Clone)]
pub struct Backoff {
    policy: RetryPolicy,
    /// Attempts that ended since the count last started over.
    failures: u32,
    /// The splitmix64 state.
    seed: u64,
}

impl Backoff {
    /// The waits of `policy`, their jitter drawn from `seed`.
    #[must_use]
    pub const fn new(policy: RetryPolicy, seed: u64) -> Self {
        Self {
            policy,
            failures: 0,
            seed,
        }
    }

    /// The wait after an attempt that ended for `next` (never a stop),
    /// welcomed for `welcomed` if it was, the server asking for
    /// `retry_after`.
    pub fn after(
        &mut self,
        next: Next,
        welcomed: Option<Duration>,
        retry_after: Option<Duration>,
    ) -> Duration {
        if welcomed.is_some_and(|welcomed| welcomed >= STABLE_AFTER) {
            self.failures = 0;
        }
        self.failures = self.failures.saturating_add(1);
        let attempt = match next {
            Next::Slow => PLATEAU,
            Next::Backoff | Next::Stop(_) => NonZeroU32::new(self.failures).unwrap_or(PLATEAU),
        };
        let seed = self.draw();
        let wait = self.policy.seeded_delay(attempt, seed);
        let asked = retry_after.map_or(Duration::ZERO, |asked| asked.min(RECONNECT_DELAY_MAX));
        wait.max(asked).min(RECONNECT_DELAY_MAX)
    }

    /// The next seed of the sequence.
    fn draw(&mut self) -> u64 {
        // splitmix64 (Steele, Lea and Flood): a full-period sequence.
        self.seed = self.seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    const fn secs(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    fn backoff(seed: u64) -> Backoff {
        Backoff::new(policy().unwrap(), seed)
    }

    fn within(wait: Duration, lower: u64, upper: u64) -> bool {
        secs(lower) <= wait && wait <= secs(upper)
    }

    #[test]
    fn the_waits_double_to_their_plateau() {
        let mut backoff = backoff(1);

        let waits: Vec<Duration> = (0..7)
            .map(|_| backoff.after(Next::Backoff, None, None))
            .collect();

        for (wait, (lower, upper)) in waits.iter().zip([
            (1, 2),
            (2, 4),
            (4, 8),
            (8, 16),
            (15, 30),
            (15, 30),
            (15, 30),
        ]) {
            assert!(within(*wait, lower, upper), "{waits:?}");
        }
    }

    #[test]
    fn a_slow_retry_waits_the_plateau_from_the_first() {
        let mut backoff = backoff(2);

        let first = backoff.after(Next::Slow, None, None);

        assert!(within(first, 15, 30), "{first:?}");
    }

    #[rstest]
    #[case::stable(STABLE_AFTER, true)]
    #[case::longer(secs(3600), true)]
    #[case::not_quite(secs(59), false)]
    fn the_count_starts_over_only_after_a_stable_stream(
        #[case] welcomed: Duration,
        #[case] starts_over: bool,
    ) {
        let mut backoff = backoff(3);
        for _ in 0..5 {
            let _ = backoff.after(Next::Backoff, None, None);
        }

        let next = backoff.after(Next::Backoff, Some(welcomed), None);

        assert_eq!(within(next, 1, 2), starts_over, "{next:?}");
    }

    #[test]
    fn a_retry_after_is_honoured_up_to_the_cap() {
        let mut asked = backoff(4);
        let mut beyond = backoff(4);

        let honoured = asked.after(Next::Backoff, None, Some(secs(20)));
        let capped = beyond.after(Next::Backoff, None, Some(secs(1000)));

        assert!(within(honoured, 20, 20), "{honoured:?}");
        assert_eq!(capped, RECONNECT_DELAY_MAX);
    }

    #[test]
    fn the_same_seed_gives_the_same_waits_and_another_seed_others() {
        let waits = |seed| {
            let mut backoff = backoff(seed);
            (0..8)
                .map(|_| backoff.after(Next::Backoff, None, None))
                .collect::<Vec<_>>()
        };

        assert_eq!(waits(7), waits(7));
        assert_ne!(waits(7), waits(8));
    }

    #[test]
    fn no_wait_ever_exceeds_the_reconnect_maximum() {
        // A sweep over seeds, kinds, welcomes and retry-afters.
        for seed in 0..300_u64 {
            let mut backoff = backoff(seed);
            for step in 0..80_u64 {
                let next = if step % 7 == 0 {
                    Next::Slow
                } else {
                    Next::Backoff
                };
                let welcomed = (step % 11 == 0).then(|| secs(step));
                let retry_after = (step % 5 == 0).then(|| secs(step * 13));

                let wait = backoff.after(next, welcomed, retry_after);

                assert!(wait <= RECONNECT_DELAY_MAX, "{seed} {step}: {wait:?}");
                assert!(wait >= BASE_DELAY, "{seed} {step}: {wait:?}");
            }
        }
    }

    #[test]
    fn a_dead_server_is_noticed_and_redialled_within_the_servers_grace() {
        // S45: the server waits 90 s after its start for every running agent.
        let noticed =
            fleet_proto::control::KEEPALIVE_INTERVAL + fleet_proto::control::KEEPALIVE_TIMEOUT;
        let back = noticed + RECONNECT_DELAY_MAX + fleet_proto::control::WELCOME_TIMEOUT;

        assert!(back <= secs(90), "{back:?}");
    }
}
