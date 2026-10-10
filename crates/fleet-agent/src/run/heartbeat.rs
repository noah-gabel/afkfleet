//! The agent's heartbeat while it runs (Plan.md P5.5, ADR-0014).
//!
//! Once the agent runs, one task beats at once and then every
//! [`PERIOD`](crate::heartbeat::PERIOD): it asks the fleet for a snapshot of
//! every bot and, only when the supervisor answers, touches the heartbeat.
//! `Busy`, `TimedOut` and `ShuttingDown` skip the beat. A hung supervisor
//! leaves each timed-out call in its queue, and once the queue is full every
//! call answers `Busy` at once, so only an answer proves the fleet works.
//! The run cancels the task when its shutdown starts.
//!
//! Each kind of failure is logged once per streak: a `warn` when it starts
//! and an `info` when it ends.

use std::io;
use std::path::{Path, PathBuf};

use fleet_runtime::{Fleet, FleetError};
use tokio::time::{Interval, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::heartbeat::{Heartbeat, PERIOD};

/// Beats until `cancel` fires. `file` names the heartbeat in the log.
pub(crate) async fn beat<H: Heartbeat>(
    fleet: Fleet,
    heartbeat: H,
    file: PathBuf,
    cancel: CancellationToken,
) {
    let mut beats = tokio::time::interval(PERIOD);
    // A late beat isn't made up for; the next one keeps the period.
    beats.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut streaks = Streaks::default();
    loop {
        tokio::select! {
            biased;
            // Both branches are cancel-safe: a beat dropped midway loses
            // nothing but itself, and a touch already handed to the blocking
            // pool finishes on its own.
            () = cancel.cancelled() => return,
            () = beat_once(&mut beats, &fleet, &heartbeat, &file, &mut streaks) => {}
        }
    }
}

/// Waits for the next beat, then touches the heartbeat if the fleet
/// answers. The touch ends before the next beat can start.
async fn beat_once<H: Heartbeat>(
    beats: &mut Interval,
    fleet: &Fleet,
    heartbeat: &H,
    file: &Path,
    streaks: &mut Streaks,
) {
    beats.tick().await;
    // Every call answers within the reply timeout, so this needs none.
    if let Err(error) = fleet.snapshot_all().await {
        streaks.fleet_silent(error);
        return;
    }
    streaks.fleet_answered();
    match heartbeat.touch().await {
        Ok(()) => streaks.touched(file),
        Err(error) => streaks.touch_failed(file, &error),
    }
}

/// The failure streaks, each logged once when it starts and once when it
/// ends.
#[derive(Debug, Default)]
struct Streaks {
    /// The fleet doesn't answer.
    fleet_silent: bool,
    /// The heartbeat can't be touched.
    touch_failing: bool,
}

impl Streaks {
    fn fleet_silent(&mut self, error: FleetError) {
        if !self.fleet_silent {
            warn!(%error, "the fleet didn't answer the heartbeat");
            self.fleet_silent = true;
        }
    }

    fn fleet_answered(&mut self) {
        if self.fleet_silent {
            info!("the fleet answers the heartbeat again");
            self.fleet_silent = false;
        }
    }

    fn touch_failed(&mut self, file: &Path, error: &io::Error) {
        if !self.touch_failing {
            warn!(
                file = %file.display(),
                %error,
                "the heartbeat file can't be touched"
            );
            self.touch_failing = true;
        }
    }

    fn touched(&mut self, file: &Path) {
        if self.touch_failing {
            info!(file = %file.display(), "the heartbeat file is touched again");
            self.touch_failing = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use fleet_runtime::FleetError;
    use tokio::sync::oneshot;
    use tokio::task::JoinSet;
    use tokio::time::Instant;

    use super::*;
    use crate::run::testing::{TestSupervisor, advance, fleet, secs, settle};
    use crate::telemetry::capture::json_on_this_thread;

    /// The heartbeat's name in the log.
    const FILE: &str = "/beats/agent.alive";

    const SILENT: &str = "the fleet didn't answer the heartbeat";
    const ANSWERS_AGAIN: &str = "the fleet answers the heartbeat again";
    const CANT_TOUCH: &str = "the heartbeat file can't be touched";
    const TOUCHED_AGAIN: &str = "the heartbeat file is touched again";

    /// Counts its beats; each touch fails while it's set to fail. Clones
    /// share both.
    #[derive(Debug, Clone, Default)]
    struct Counting {
        beats: Arc<AtomicUsize>,
        failing: Arc<AtomicBool>,
    }

    impl Counting {
        fn beats(&self) -> usize {
            self.beats.load(Ordering::SeqCst)
        }

        fn fail(&self, failing: bool) {
            self.failing.store(failing, Ordering::SeqCst);
        }
    }

    impl Heartbeat for Counting {
        fn touch(&self) -> impl Future<Output = io::Result<()>> + Send {
            let touched = if self.failing.load(Ordering::SeqCst) {
                Err(io::Error::other("the disk is full"))
            } else {
                self.beats.fetch_add(1, Ordering::SeqCst);
                Ok(())
            };
            core::future::ready(touched)
        }
    }

    /// The heartbeat's task and the token that stops it.
    struct Beating {
        task: JoinSet<()>,
        cancel: CancellationToken,
    }

    /// Starts beating `heartbeat` for `fleet`.
    fn start(fleet: &Fleet, heartbeat: &Counting) -> Beating {
        let cancel = CancellationToken::new();
        let mut task = JoinSet::new();
        task.spawn(beat(
            fleet.clone(),
            heartbeat.clone(),
            PathBuf::from(FILE),
            cancel.clone(),
        ));
        Beating { task, cancel }
    }

    /// Runs `supervisor` in a task of its own.
    fn run(supervisor: TestSupervisor) -> JoinSet<()> {
        let mut task = JoinSet::new();
        task.spawn(supervisor.run(CancellationToken::new()));
        task
    }

    /// Holds `supervisor` without running it, so no call is answered.
    fn hold(supervisor: TestSupervisor) -> JoinSet<()> {
        let mut task = JoinSet::new();
        task.spawn(async move {
            let _held = supervisor;
            core::future::pending::<()>().await;
        });
        task
    }

    #[tokio::test(start_paused = true)]
    async fn a_fleet_that_answers_gets_a_beat_at_once_and_then_every_ten_seconds() {
        let (fleet, supervisor, _fake) = fleet(64);
        let _supervisor = run(supervisor);
        let heartbeat = Counting::default();

        let _beating = start(&fleet, &heartbeat);
        settle().await;

        assert_eq!(heartbeat.beats(), 1);
        advance(secs(9)).await;
        assert_eq!(heartbeat.beats(), 1);
        advance(secs(1)).await;
        assert_eq!(heartbeat.beats(), 2);
        advance(secs(10)).await;
        assert_eq!(heartbeat.beats(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_supervisor_never_gets_a_beat_even_once_every_call_answers_busy() {
        let (capture, _guard) = json_on_this_thread();
        let (fleet, supervisor, _fake) = fleet(64);
        let _held = hold(supervisor);
        let heartbeat = Counting::default();

        let _beating = start(&fleet, &heartbeat);
        settle().await;
        // 70 more beats: the first 64 time out and stay in the queue.
        for _ in 0..70 {
            advance(secs(10)).await;
        }

        assert_eq!(heartbeat.beats(), 0);
        // The queue is full of timed-out calls, so every call is `Busy` now,
        // at once: the case that would hide the hang.
        let started = Instant::now();
        let answer = fleet.snapshot_all().await;
        assert!(matches!(answer, Err(FleetError::Busy)), "{answer:?}");
        assert_eq!(started.elapsed(), Duration::ZERO);
        let warned = capture.lines_with(SILENT).unwrap();
        assert_eq!(warned.len(), 1, "{}", capture.text());
        assert_eq!(warned[0]["level"], "WARN");
        assert_eq!(warned[0]["error"], "the fleet didn't answer in time");
    }

    #[tokio::test(start_paused = true)]
    async fn a_supervisor_that_starts_answering_gets_its_beats_back() {
        let (capture, _guard) = json_on_this_thread();
        let (fleet, supervisor, _fake) = fleet(64);
        let (release, released) = oneshot::channel::<()>();
        let mut held = JoinSet::new();
        held.spawn(async move {
            let _ = released.await;
            supervisor.run(CancellationToken::new()).await;
        });
        let heartbeat = Counting::default();
        let _beating = start(&fleet, &heartbeat);
        settle().await;
        // The first beat times out at 5 s; the second waits from 10 s on.
        advance(secs(10)).await;
        assert_eq!(heartbeat.beats(), 0);

        release.send(()).unwrap();
        settle().await;

        assert_eq!(heartbeat.beats(), 1, "{}", capture.text());
        assert_eq!(
            capture.lines_with(SILENT).unwrap().len(),
            1,
            "{}",
            capture.text()
        );
        let back = capture.lines_with(ANSWERS_AGAIN).unwrap();
        assert_eq!(back.len(), 1, "{}", capture.text());
        assert_eq!(back[0]["level"], "INFO");
    }

    #[tokio::test(start_paused = true)]
    async fn a_file_that_cant_be_touched_warns_once_until_it_can_be_again() {
        let (capture, _guard) = json_on_this_thread();
        let (fleet, supervisor, _fake) = fleet(64);
        let _supervisor = run(supervisor);
        let heartbeat = Counting::default();
        heartbeat.fail(true);

        let _beating = start(&fleet, &heartbeat);
        settle().await;
        advance(secs(10)).await;

        assert_eq!(heartbeat.beats(), 0);
        let warned = capture.lines_with(CANT_TOUCH).unwrap();
        assert_eq!(warned.len(), 1, "{}", capture.text());
        assert_eq!(warned[0]["level"], "WARN");
        assert_eq!(warned[0]["file"], FILE);
        assert_eq!(warned[0]["error"], "the disk is full");
        assert_eq!(capture.lines_with(TOUCHED_AGAIN).unwrap().len(), 0);

        heartbeat.fail(false);
        advance(secs(10)).await;

        assert_eq!(heartbeat.beats(), 1);
        assert_eq!(capture.lines_with(CANT_TOUCH).unwrap().len(), 1);
        let back = capture.lines_with(TOUCHED_AGAIN).unwrap();
        assert_eq!(back.len(), 1, "{}", capture.text());
        assert_eq!(back[0]["level"], "INFO");
        assert_eq!(back[0]["file"], FILE);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_ends_the_heartbeat_even_while_it_waits_for_the_fleet() {
        let (fleet, supervisor, _fake) = fleet(64);
        let _held = hold(supervisor);
        let heartbeat = Counting::default();
        let mut beating = start(&fleet, &heartbeat);
        settle().await;
        // The first beat waits for its answer until 5 s.
        advance(secs(1)).await;

        beating.cancel.cancel();
        settle().await;

        let joined = beating.task.try_join_next();
        assert!(matches!(joined, Some(Ok(()))), "{joined:?}");
    }
}
