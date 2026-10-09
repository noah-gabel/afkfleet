//! Ending a run: the fleet's shutdown, with one deadline (ADR-0014).
//!
//! The deadline is the shutdown timeout plus the reply timeout, counted from
//! the start of the shutdown; nothing waits past it.
//!
//! 1. `Fleet::shutdown(shutdown_timeout)` stops every bot and returns the
//!    report.
//! 2. If the fleet doesn't confirm (`Busy`, `ShuttingDown`, `TimedOut`), the
//!    supervisor's token is cancelled, which shuts the fleet down too, only
//!    without a report. `TimedOut` has used up the deadline already.
//! 3. The supervisor's task must then end by the deadline. If it doesn't, or
//!    if it panics, the run ends with [`Exit::SupervisorFailed`].

use core::pin::{Pin, pin};
use core::time::Duration;

use fleet_runtime::{Fleet, FleetError, RuntimeConfig, ShutdownReport};
use tokio::task::JoinSet;
use tokio::time::Sleep;
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use super::watch::Reason;
use super::{Exit, Outcome};

/// How long a shutdown may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Timeouts {
    /// How long the bots may take to stop before they're aborted.
    pub(crate) shutdown: Duration,
    /// How long the supervisor may take to answer on top of that.
    pub(crate) reply: Duration,
}

impl Timeouts {
    /// The runtime's shutdown and reply timeouts.
    pub(crate) const fn of(config: &RuntimeConfig) -> Self {
        Self {
            shutdown: config.shutdown_timeout,
            reply: config.reply_timeout,
        }
    }
}

/// Ends the run for `reason`: logs it, and shuts the fleet down unless its
/// supervisor has already ended.
pub(crate) async fn finish(
    reason: Reason,
    fleet: &Fleet,
    supervisor: &mut JoinSet<()>,
    cancel: &CancellationToken,
    timeouts: Timeouts,
) -> Outcome {
    let exit = match reason {
        Reason::SupervisorEnded { panicked } => {
            error!(panicked, "the fleet's supervisor ended unasked");
            cancel.cancel();
            return SUPERVISOR_FAILED;
        }
        Reason::AbandonedLimit { abandoned, limit } => {
            error!(
                abandoned,
                limit, "the abandoned-thread limit is reached; shutting down"
            );
            Exit::AbandonedLimit
        }
        Reason::StartupFailed => Exit::StartupFailed,
    };
    // `sleep` saturates far in the future instead of overflowing.
    let deadline = pin!(tokio::time::sleep(
        timeouts.shutdown.saturating_add(timeouts.reply)
    ));
    // `shutdown` itself waits until the deadline at most.
    let report = match fleet.shutdown(timeouts.shutdown).await {
        Ok(report) => Some(report),
        Err(error) => {
            warn!(
                %error,
                "the fleet didn't confirm the shutdown; cancelling its supervisor"
            );
            cancel.cancel();
            if error == FleetError::TimedOut {
                // The deadline has passed.
                error!("the fleet's supervisor didn't end when asked");
                return SUPERVISOR_FAILED;
            }
            None
        }
    };
    join_by(supervisor, deadline, exit, report).await
}

/// The outcome when the supervisor ended unasked or didn't end when asked.
const SUPERVISOR_FAILED: Outcome = Outcome {
    exit: Exit::SupervisorFailed,
    report: None,
};

/// Waits for the supervisor's task until `deadline`. If it ends in time,
/// the run ends with `exit` and `report`.
async fn join_by(
    supervisor: &mut JoinSet<()>,
    mut deadline: Pin<&mut Sleep>,
    exit: Exit,
    report: Option<ShutdownReport>,
) -> Outcome {
    let joined = tokio::select! {
        biased;
        // Both branches are cancel-safe: `join_next` loses nothing, and the
        // pinned deadline keeps its time.
        joined = supervisor.join_next() => Ok(joined),
        () = &mut deadline => Err(()),
    };
    match joined {
        // `None`: the supervisor's task was already joined.
        Ok(None | Some(Ok(()))) => Outcome { exit, report },
        Ok(Some(Err(failure))) => {
            // The payload isn't kept: it's untrusted, and the panic hook has
            // logged it, sanitized.
            error!(
                panicked = failure.is_panic(),
                "the fleet's supervisor failed during the shutdown"
            );
            SUPERVISOR_FAILED
        }
        Err(()) => {
            error!("the fleet's supervisor didn't end when asked");
            SUPERVISOR_FAILED
        }
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroUsize;
    use std::sync::Arc;

    use chrono::DateTime;
    use fleet_core::bot::{BotAccount, BotSpec, DesiredRunState};
    use fleet_core::disconnect::ConflictTexts;
    use fleet_core::mc::SessionEvent;
    use fleet_core::mode::ModePreset;
    use fleet_core::resilience::{CircuitPolicy, RetryPolicy};
    use fleet_runtime::{FleetParts, OfflineCredentials, ShutdownReport, Supervisor};
    use fleet_testkit::mc::{EmitOutcome, FakeConnector};
    use serde_json::Value;
    use tokio::time::Instant;

    use super::*;
    use crate::telemetry::capture::{Capture, json_on_this_thread};

    const fn secs(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    /// The defaults: 10 s to shut down, 5 s to reply.
    const TIMEOUTS: Timeouts = Timeouts {
        shutdown: secs(10),
        reply: secs(5),
    };

    /// The deadline of every shutdown with [`TIMEOUTS`].
    const DEADLINE: Duration = secs(15);

    const LIMIT: Reason = Reason::AbandonedLimit {
        abandoned: 3,
        limit: 3,
    };

    type Held = Supervisor<FakeConnector, OfflineCredentials>;

    struct Setup {
        fleet: Fleet,
        supervisor: Option<Held>,
        fake: FakeConnector,
        cancel: CancellationToken,
        tasks: JoinSet<()>,
    }

    impl Setup {
        /// A fleet whose supervisor queue holds `queue` calls; its
        /// supervisor isn't running yet.
        fn new(queue: usize) -> Self {
            let fake = FakeConnector::new();
            let config = RuntimeConfig {
                supervisor_queue: NonZeroUsize::new(queue).unwrap(),
                ..RuntimeConfig::default()
            };
            let (fleet, supervisor) = Fleet::new(FleetParts {
                connector: Arc::new(fake.clone()),
                credentials: Arc::new(OfflineCredentials),
                retry: RetryPolicy::try_new(secs(5), secs(300), secs(300)).unwrap(),
                circuit: CircuitPolicy::try_new(
                    NonZeroUsize::new(8).unwrap(),
                    secs(600),
                    secs(900),
                )
                .unwrap(),
                config,
                anchor: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
                seed: 1,
            })
            .unwrap();
            Self {
                fleet,
                supervisor: Some(supervisor),
                fake,
                cancel: CancellationToken::new(),
                tasks: JoinSet::new(),
            }
        }

        /// The supervisor runs as it does in the agent.
        fn run_supervisor(&mut self) -> JoinSet<()> {
            let supervisor = self.supervisor.take().unwrap();
            let mut set = JoinSet::new();
            set.spawn(supervisor.run(self.cancel.child_token()));
            set
        }

        /// A task holds the supervisor without running it, so no call is
        /// answered, and ends when the token is cancelled.
        fn hold_until_cancelled(&mut self) -> JoinSet<()> {
            let supervisor = self.supervisor.take().unwrap();
            let cancel = self.cancel.clone();
            let mut set = JoinSet::new();
            set.spawn(async move {
                let _held = supervisor;
                cancel.cancelled().await;
            });
            set
        }

        /// A task holds the supervisor and never ends.
        fn hold_forever(&mut self) -> JoinSet<()> {
            let supervisor = self.supervisor.take().unwrap();
            let mut set = JoinSet::new();
            set.spawn(async move {
                let _held = supervisor;
                core::future::pending::<()>().await;
            });
            set
        }

        /// A task holds the supervisor and panics once the token is
        /// cancelled.
        fn hold_then_panic(&mut self) -> JoinSet<()> {
            let supervisor = self.supervisor.take().unwrap();
            let cancel = self.cancel.clone();
            let mut set = JoinSet::new();
            set.spawn(async move {
                let _held = supervisor;
                cancel.cancelled().await;
                panic!("untrusted panic payload");
            });
            set
        }

        /// The supervisor is gone; a task stands in for it and ends when the
        /// token is cancelled.
        fn drop_supervisor(&mut self) -> JoinSet<()> {
            drop(self.supervisor.take());
            let cancel = self.cancel.clone();
            let mut set = JoinSet::new();
            set.spawn(async move { cancel.cancelled().await });
            set
        }

        /// Fills the one-call queue with a call nobody answers.
        async fn fill_the_queue(&mut self) {
            let fleet = self.fleet.clone();
            self.tasks.spawn(async move {
                let _ = fleet.snapshot_all().await;
            });
            settle().await;
        }

        /// Runs one bot and joins it.
        async fn one_bot_online(&self) {
            let spec = BotSpec {
                id: "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap(),
                account: BotAccount::Offline("AfkBot1".parse().unwrap()),
                server: "localhost".try_into().unwrap(),
                mode: ModePreset::Afk.definition(),
                desired: DesiredRunState::Running,
                conflict_texts: ConflictTexts::default(),
            };
            self.fleet.apply(spec, None).await.unwrap();
            settle().await;
            let session = tokio::time::timeout(secs(1), self.fake.session(0))
                .await
                .unwrap();
            assert_eq!(session.emit(SessionEvent::Joined), EmitOutcome::Queued);
            settle().await;
        }

        /// Runs `finish` and returns its outcome with the time it took.
        async fn finish(
            &self,
            reason: Reason,
            supervisor: &mut JoinSet<()>,
        ) -> (Outcome, Duration) {
            let started = Instant::now();
            let outcome = finish(reason, &self.fleet, supervisor, &self.cancel, TIMEOUTS).await;
            (outcome, started.elapsed())
        }
    }

    async fn settle() {
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
    }

    fn levels_of(capture: &Capture, message: &str) -> Vec<String> {
        capture
            .lines_with(message)
            .iter()
            .map(|line| line["level"].as_str().unwrap().to_owned())
            .collect()
    }

    const DIDNT_CONFIRM: &str = "the fleet didn't confirm the shutdown; cancelling its supervisor";
    const DIDNT_END: &str = "the fleet's supervisor didn't end when asked";
    const FAILED: &str = "the fleet's supervisor failed during the shutdown";

    #[tokio::test(start_paused = true)]
    async fn a_confirmed_shutdown_ends_with_the_reasons_exit_and_the_report() {
        let (capture, _guard) = json_on_this_thread();
        let mut setup = Setup::new(64);
        let mut supervisor = setup.run_supervisor();
        setup.one_bot_online().await;

        let (outcome, took) = setup.finish(LIMIT, &mut supervisor).await;

        assert_eq!(
            outcome,
            Outcome {
                exit: Exit::AbandonedLimit,
                report: Some(ShutdownReport {
                    stopped: 1,
                    aborted: 0,
                    crashed: 0
                })
            }
        );
        assert_eq!(took, Duration::ZERO);
        assert!(setup.fake.try_session(0).unwrap().is_torn_down());
        assert!(supervisor.is_empty());
        let limit = capture.lines_with("the abandoned-thread limit is reached; shutting down");
        assert_eq!(limit.len(), 1, "{}", capture.text());
        assert_eq!(limit[0]["level"], "ERROR");
        assert_eq!(limit[0]["abandoned"], 3);
        assert_eq!(limit[0]["limit"], 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_shutdown_that_times_out_exits_supervisor_failed_at_the_deadline() {
        let (capture, _guard) = json_on_this_thread();
        let mut setup = Setup::new(64);
        let mut supervisor = setup.hold_until_cancelled();

        let (outcome, took) = setup.finish(LIMIT, &mut supervisor).await;

        assert_eq!(
            outcome,
            Outcome {
                exit: Exit::SupervisorFailed,
                report: None
            }
        );
        assert_eq!(took, DEADLINE);
        assert!(setup.cancel.is_cancelled());
        let warned = capture.lines_with(DIDNT_CONFIRM);
        assert_eq!(warned.len(), 1, "{}", capture.text());
        assert_eq!(warned[0]["level"], "WARN");
        assert_eq!(warned[0]["error"], "the fleet didn't answer in time");
        assert_eq!(levels_of(&capture, DIDNT_END), ["ERROR"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_busy_fleet_is_cancelled_and_its_supervisor_ending_keeps_the_reasons_exit() {
        let (capture, _guard) = json_on_this_thread();
        let mut setup = Setup::new(1);
        let mut supervisor = setup.hold_until_cancelled();
        setup.fill_the_queue().await;

        let (outcome, took) = setup.finish(LIMIT, &mut supervisor).await;

        assert_eq!(
            outcome,
            Outcome {
                exit: Exit::AbandonedLimit,
                report: None
            }
        );
        assert_eq!(took, Duration::ZERO);
        assert!(setup.cancel.is_cancelled());
        let warned = capture.lines_with(DIDNT_CONFIRM);
        assert_eq!(warned.len(), 1, "{}", capture.text());
        assert_eq!(warned[0]["error"], "the fleet is busy; try again");
        assert_eq!(levels_of(&capture, DIDNT_END), Vec::<String>::new());
    }

    #[tokio::test(start_paused = true)]
    async fn a_busy_fleet_whose_supervisor_hangs_exits_supervisor_failed_at_the_deadline() {
        let (capture, _guard) = json_on_this_thread();
        let mut setup = Setup::new(1);
        let mut supervisor = setup.hold_forever();
        setup.fill_the_queue().await;

        let (outcome, took) = setup.finish(LIMIT, &mut supervisor).await;

        assert_eq!(
            outcome,
            Outcome {
                exit: Exit::SupervisorFailed,
                report: None
            }
        );
        assert_eq!(took, DEADLINE);
        assert_eq!(levels_of(&capture, DIDNT_CONFIRM), ["WARN"]);
        assert_eq!(levels_of(&capture, DIDNT_END), ["ERROR"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_fleet_already_shutting_down_is_cancelled_and_keeps_the_reasons_exit() {
        let (capture, _guard) = json_on_this_thread();
        let mut setup = Setup::new(64);
        let mut supervisor = setup.drop_supervisor();

        let (outcome, took) = setup.finish(LIMIT, &mut supervisor).await;

        assert_eq!(
            outcome,
            Outcome {
                exit: Exit::AbandonedLimit,
                report: None
            }
        );
        assert_eq!(took, Duration::ZERO);
        let warned = capture.lines_with(DIDNT_CONFIRM);
        assert_eq!(warned.len(), 1, "{}", capture.text());
        assert_eq!(warned[0]["error"], "the fleet is shutting down");
    }

    #[tokio::test(start_paused = true)]
    async fn a_supervisor_that_panics_during_the_shutdown_exits_supervisor_failed_without_its_payload()
     {
        let (capture, _guard) = json_on_this_thread();
        let mut setup = Setup::new(1);
        let mut supervisor = setup.hold_then_panic();
        setup.fill_the_queue().await;

        let (outcome, took) = setup.finish(LIMIT, &mut supervisor).await;

        assert_eq!(
            outcome,
            Outcome {
                exit: Exit::SupervisorFailed,
                report: None
            }
        );
        assert_eq!(took, Duration::ZERO);
        let failed = capture.lines_with(FAILED);
        assert_eq!(failed.len(), 1, "{}", capture.text());
        assert_eq!(failed[0]["level"], "ERROR");
        assert_eq!(failed[0]["panicked"], true);
        assert!(!capture.text().contains("untrusted"), "{}", capture.text());
    }

    #[tokio::test(start_paused = true)]
    async fn a_startup_failure_shuts_the_fleet_down_and_exits_startup_failed() {
        let mut setup = Setup::new(64);
        let mut supervisor = setup.run_supervisor();
        setup.one_bot_online().await;

        let (outcome, took) = setup.finish(Reason::StartupFailed, &mut supervisor).await;

        assert_eq!(outcome.exit, Exit::StartupFailed);
        assert_eq!(
            outcome.report,
            Some(ShutdownReport {
                stopped: 1,
                aborted: 0,
                crashed: 0
            })
        );
        assert_eq!(took, Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn a_startup_failure_with_a_hung_supervisor_exits_supervisor_failed() {
        let mut setup = Setup::new(1);
        let mut supervisor = setup.hold_forever();
        setup.fill_the_queue().await;

        let (outcome, took) = setup.finish(Reason::StartupFailed, &mut supervisor).await;

        assert_eq!(outcome.exit, Exit::SupervisorFailed);
        assert_eq!(took, DEADLINE);
    }

    #[rstest::rstest]
    #[case::returned(false)]
    #[case::panicked(true)]
    #[tokio::test(start_paused = true)]
    async fn a_supervisor_that_ended_unasked_exits_supervisor_failed_at_once(
        #[case] panicked: bool,
    ) {
        let (capture, _guard) = json_on_this_thread();
        let mut setup = Setup::new(64);
        let mut supervisor = setup.drop_supervisor();

        let (outcome, took) = setup
            .finish(Reason::SupervisorEnded { panicked }, &mut supervisor)
            .await;

        assert_eq!(
            outcome,
            Outcome {
                exit: Exit::SupervisorFailed,
                report: None
            }
        );
        assert_eq!(took, Duration::ZERO);
        assert!(setup.cancel.is_cancelled());
        let ended = capture.lines_with("the fleet's supervisor ended unasked");
        assert_eq!(ended.len(), 1, "{}", capture.text());
        assert_eq!(ended[0]["level"], "ERROR");
        assert_eq!(ended[0]["panicked"], panicked);
        assert_eq!(capture.lines_with(DIDNT_CONFIRM), Vec::<Value>::new());
    }
}
