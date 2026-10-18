//! Watching a running fleet until the agent has to stop.
//!
//! The watch ends when the fleet's supervisor ends on its own, when a signal
//! arrives, or when a sample of fleet-mc's diagnostics reaches the
//! abandoned-thread limit; for a managed agent also when the link ends,
//! refused by the server or unasked, or when the conductor ends (ADR-0019,
//! S65, S67). A signal wins over a refusal that comes at the same moment.
//! Each sample also updates the diagnostics' metrics.

use core::num::NonZeroUsize;

use tokio::task::JoinSet;
use tokio::time::MissedTickBehavior;

use crate::diagnostics::{self, HostDiagnostics, SAMPLE_PERIOD};
use crate::managed::link::LinkEnd;
use crate::managed::link::ends::Refusal;
use crate::signals::{ShutdownSignals, Signal};

/// A managed agent's own tasks, beside the supervisor.
#[derive(Debug)]
pub(crate) struct ManagedTasks {
    /// The conductor's.
    pub(crate) conductor: JoinSet<()>,
    /// The link's.
    pub(crate) link: JoinSet<LinkEnd>,
}

/// A managed agent's task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Task {
    /// The conductor.
    Conductor,
    /// The link.
    Link,
}

impl Task {
    /// Its name, for a log line.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Conductor => "conductor",
            Self::Link => "link",
        }
    }
}

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
    /// The server refused this agent for good (S67): every bot stops, and
    /// the agent exits 5.
    Refused(Refusal),
    /// A managed agent's own task ended without being asked to.
    TaskEnded {
        /// Which.
        task: Task,
        /// Whether it panicked.
        panicked: bool,
    },
}

/// Watches the fleet: its supervisor's task in `supervisor`, a managed
/// agent's `managed` tasks, the stop signals, and the diagnostics, sampled
/// at once and then every
/// [`SAMPLE_PERIOD`](crate::diagnostics::SAMPLE_PERIOD).
pub(crate) async fn watch<D: HostDiagnostics, S: ShutdownSignals>(
    supervisor: &mut JoinSet<()>,
    mut managed: Option<&mut ManagedTasks>,
    signals: &mut S,
    diagnostics: &D,
    limit: NonZeroUsize,
) -> Reason {
    let mut samples = tokio::time::interval(SAMPLE_PERIOD);
    // A late sample isn't made up for; the next one keeps the period.
    samples.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        let (link, conductor) = match managed.as_deref_mut() {
            Some(tasks) => (Some(&mut tasks.link), Some(&mut tasks.conductor)),
            None => (None, None),
        };
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
            Some(joined) = joined(link) => {
                return match joined {
                    Ok(LinkEnd::Refused(refusal)) => Reason::Refused(refusal),
                    Ok(LinkEnd::Stopped) => Reason::TaskEnded { task: Task::Link, panicked: false },
                    Err(error) => Reason::TaskEnded { task: Task::Link, panicked: error.is_panic() },
                };
            }
            Some(joined) = joined(conductor) => {
                let panicked = joined.is_err_and(|error| error.is_panic());
                return Reason::TaskEnded { task: Task::Conductor, panicked };
            }
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

/// The next of `tasks` to end; never, without tasks or once they're all
/// joined.
async fn joined<T: 'static>(
    tasks: Option<&mut JoinSet<T>>,
) -> Option<Result<T, tokio::task::JoinError>> {
    match tasks {
        Some(tasks) if !tasks.is_empty() => tasks.join_next().await,
        Some(_) | None => core::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use core::future::Future;
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
            watch(supervisor, None, &mut signals, diagnostics, LIMIT),
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
                None,
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

    // --- A managed agent's tasks (ADR-0019, S65, S67) ---

    /// A conductor that runs on, and a link that ends with `link`.
    fn managed(link: impl Future<Output = LinkEnd> + Send + 'static) -> ManagedTasks {
        let mut conductor = JoinSet::new();
        conductor.spawn(core::future::pending());
        let mut tasks = JoinSet::new();
        tasks.spawn(link);
        ManagedTasks {
            conductor,
            link: tasks,
        }
    }

    async fn watched_managed(
        tasks: &mut ManagedTasks,
        signals: &mut ChannelSignals,
    ) -> Option<Reason> {
        let mut supervisor = running_supervisor();
        tokio::time::timeout(
            Duration::from_secs(60),
            watch(
                &mut supervisor,
                Some(tasks),
                signals,
                &FakeDiagnostics::abandoned(0),
                LIMIT,
            ),
        )
        .await
        .ok()
    }

    #[tokio::test(start_paused = true)]
    async fn the_servers_refusal_ends_the_watch_with_it() {
        let mut tasks = managed(async { LinkEnd::Refused(Refusal::AgentDisabled) });
        let (_sender, mut signals) = ChannelSignals::new();

        let reason = watched_managed(&mut tasks, &mut signals).await;

        assert_eq!(reason, Some(Reason::Refused(Refusal::AgentDisabled)));
    }

    #[rstest]
    #[case::stopped(false)]
    #[case::panicked(true)]
    #[tokio::test(start_paused = true)]
    async fn a_link_that_ends_unasked_ends_the_watch(#[case] panics: bool) {
        let mut tasks = managed(async move {
            assert!(!panics, "a link bug");
            LinkEnd::Stopped
        });
        let (_sender, mut signals) = ChannelSignals::new();

        let reason = watched_managed(&mut tasks, &mut signals).await;

        assert_eq!(
            reason,
            Some(Reason::TaskEnded {
                task: Task::Link,
                panicked: panics
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_conductor_that_ends_unasked_ends_the_watch() {
        let mut tasks = managed(core::future::pending());
        tasks.conductor = JoinSet::new();
        tasks.conductor.spawn(async {});
        let (_sender, mut signals) = ChannelSignals::new();

        let reason = watched_managed(&mut tasks, &mut signals).await;

        assert_eq!(
            reason,
            Some(Reason::TaskEnded {
                task: Task::Conductor,
                panicked: false
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_signal_wins_over_a_refusal_at_the_same_moment() {
        let mut tasks = managed(async { LinkEnd::Refused(Refusal::Unauthenticated) });
        tokio::task::yield_now().await;
        let (sender, mut signals) = ChannelSignals::new();
        sender.send(Signal::Terminate).await.unwrap();

        let reason = watched_managed(&mut tasks, &mut signals).await;

        assert_eq!(reason, Some(Reason::Signal(Signal::Terminate)));
    }

    #[test]
    fn each_task_has_its_name() {
        assert_eq!(
            [Task::Conductor.as_str(), Task::Link.as_str()],
            ["conductor", "link"]
        );
    }
}
