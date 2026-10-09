//! Watching a running fleet until the agent has to stop.
//!
//! The watch ends when the fleet's supervisor ends on its own, when a signal
//! arrives, or when a sample of fleet-mc's diagnostics reaches the
//! abandoned-thread limit. Each sample also updates the diagnostics'
//! metrics.

use core::num::NonZeroUsize;

use tokio::task::JoinSet;
use tokio::time::MissedTickBehavior;

use crate::diagnostics::{self, HostDiagnostics, SAMPLE_PERIOD};
use crate::signals::{ShutdownSignals, Signal};

/// Why the agent stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reason {
    /// A signal asked the agent to stop.
    Signal(Signal),
    /// A bot couldn't be applied at startup; that's already logged.
    StartupFailed,
    /// fleet-mc abandoned `abandoned` hung threads, at least its `limit`.
    AbandonedLimit {
        /// The abandoned threads.
        abandoned: usize,
        /// `[runtime] max_abandoned_threads`.
        limit: usize,
    },
    /// The fleet's supervisor ended without being asked to. A panic's
    /// payload isn't kept: the panic hook has logged it, sanitized.
    SupervisorEnded {
        /// Whether its task panicked.
        panicked: bool,
    },
}

/// Watches the fleet: its supervisor's task in `supervisor`, the stop
/// signals, and the diagnostics, sampled at once and then every
/// [`SAMPLE_PERIOD`](crate::diagnostics::SAMPLE_PERIOD).
pub(crate) async fn watch<D: HostDiagnostics, S: ShutdownSignals>(
    supervisor: &mut JoinSet<()>,
    signals: &mut S,
    diagnostics: &D,
    limit: NonZeroUsize,
) -> Reason {
    let mut samples = tokio::time::interval(SAMPLE_PERIOD);
    // A late sample isn't made up for; the next one keeps the period.
    samples.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            // Every branch is cancel-safe: `join_next`, the signals' `recv`
            // and `tick` lose nothing when another one wins.
            Some(joined) = supervisor.join_next(), if !supervisor.is_empty() => {
                // The payload isn't kept: it's untrusted, and the panic
                // hook has logged it, sanitized.
                let panicked = joined.is_err_and(|error| error.is_panic());
                return Reason::SupervisorEnded { panicked };
            }
            signal = signals.recv() => return Reason::Signal(signal),
            _ = samples.tick() => {
                let sample = diagnostics.sample();
                diagnostics::record(&sample);
                if sample.abandoned_threads >= limit.get() {
                    return Reason::AbandonedLimit {
                        abandoned: sample.abandoned_threads,
                        limit: limit.get(),
                    };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;
    use std::sync::Mutex;

    use rstest::rstest;

    use super::*;
    use crate::diagnostics::HostSample;
    use crate::signals::testing::ChannelSignals;

    struct FakeDiagnostics(Mutex<HostSample>);

    impl FakeDiagnostics {
        fn abandoned(abandoned: usize) -> Self {
            Self(Mutex::new(HostSample {
                abandoned_threads: abandoned,
                ..HostSample::default()
            }))
        }
    }

    impl HostDiagnostics for FakeDiagnostics {
        fn sample(&self) -> HostSample {
            *self.0.lock().unwrap()
        }
    }

    const LIMIT: NonZeroUsize = NonZeroUsize::new(3).unwrap();

    fn running_supervisor() -> JoinSet<()> {
        let mut supervisor = JoinSet::new();
        supervisor.spawn(core::future::pending());
        supervisor
    }

    async fn watched(
        supervisor: &mut JoinSet<()>,
        diagnostics: &FakeDiagnostics,
    ) -> Option<Reason> {
        let (_sender, mut signals) = ChannelSignals::new();
        tokio::time::timeout(
            Duration::from_secs(60),
            watch(supervisor, &mut signals, diagnostics, LIMIT),
        )
        .await
        .ok()
    }

    #[tokio::test(start_paused = true)]
    async fn a_signal_ends_the_watch_with_that_signal() {
        let mut supervisor = running_supervisor();
        let (sender, mut signals) = ChannelSignals::new();
        sender.send(Signal::Interrupt).await.unwrap();

        let reason = tokio::time::timeout(
            Duration::from_secs(60),
            watch(
                &mut supervisor,
                &mut signals,
                &FakeDiagnostics::abandoned(0),
                LIMIT,
            ),
        )
        .await
        .ok();

        assert_eq!(reason, Some(Reason::Signal(Signal::Interrupt)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_supervisor_that_returns_ended_unasked() {
        let mut supervisor = JoinSet::new();
        supervisor.spawn(async {});

        let reason = watched(&mut supervisor, &FakeDiagnostics::abandoned(0)).await;

        assert_eq!(reason, Some(Reason::SupervisorEnded { panicked: false }));
    }

    #[tokio::test(start_paused = true)]
    async fn a_supervisor_that_panics_ended_unasked_with_a_panic() {
        let mut supervisor = JoinSet::new();
        supervisor.spawn(async { panic!("a supervisor bug") });

        let reason = watched(&mut supervisor, &FakeDiagnostics::abandoned(0)).await;

        assert_eq!(reason, Some(Reason::SupervisorEnded { panicked: true }));
    }

    #[rstest]
    #[case::at_the_limit(3)]
    #[case::above_the_limit(4)]
    #[tokio::test(start_paused = true)]
    async fn reaching_the_abandoned_limit_ends_the_watch_at_once(#[case] abandoned: usize) {
        let mut supervisor = running_supervisor();
        let started = tokio::time::Instant::now();

        let reason = watched(&mut supervisor, &FakeDiagnostics::abandoned(abandoned)).await;

        assert_eq!(
            reason,
            Some(Reason::AbandonedLimit {
                abandoned,
                limit: 3
            })
        );
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn below_the_abandoned_limit_the_watch_goes_on() {
        let mut supervisor = running_supervisor();

        let reason = watched(&mut supervisor, &FakeDiagnostics::abandoned(2)).await;

        assert_eq!(reason, None);
    }

    #[tokio::test(start_paused = true)]
    async fn the_limit_reached_later_ends_the_watch_at_the_next_sample() {
        let mut supervisor = running_supervisor();
        let diagnostics = FakeDiagnostics::abandoned(0);
        let started = tokio::time::Instant::now();

        let (reason, ()) = tokio::join!(watched(&mut supervisor, &diagnostics), async {
            tokio::time::sleep(Duration::from_secs(7)).await;
            diagnostics.0.lock().unwrap().abandoned_threads = 3;
        });

        assert_eq!(
            reason,
            Some(Reason::AbandonedLimit {
                abandoned: 3,
                limit: 3
            })
        );
        assert_eq!(started.elapsed(), Duration::from_secs(10));
    }
}
