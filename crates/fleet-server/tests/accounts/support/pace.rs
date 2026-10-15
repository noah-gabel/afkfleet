//! [`FakePace`]: the [`Pace`] for the link flows' tests (ADR-0018). Each
//! wait is recorded and moves the test's [`ManualClock`] forward by its
//! duration at once, so a test sees exactly how long a flow waited, and the
//! code's expiry passes on the clock, without any real waiting.
//!
//! [`FakePace::hold_next`] holds the next wait at a [`PaceGate`] until the
//! test releases it, for the tests that stop a flow while it waits; a held
//! wait whose gate is dropped never ends.

use core::time::Duration;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use fleet_server::ports::pace::Pace;
use fleet_testkit::system::ManualClock;
use tokio::sync::oneshot;

/// How long [`PaceGate::entered`] waits before failing the test.
const ENTER_BOUND: Duration = Duration::from_secs(5);

/// The fake; see the [module docs](self).
#[derive(Debug)]
pub(crate) struct FakePace {
    clock: Arc<ManualClock>,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    waits: Vec<Duration>,
    held: VecDeque<Held>,
}

/// The fake's end of a gate.
#[derive(Debug)]
struct Held {
    entered: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

/// The test's end of a gate: it learns when the held wait began, and ends
/// it.
#[derive(Debug)]
pub(crate) struct PaceGate {
    entered: Option<oneshot::Receiver<()>>,
    release: oneshot::Sender<()>,
}

impl PaceGate {
    /// Waits until the held wait began; fails the test after 5 seconds.
    pub(crate) async fn entered(&mut self) {
        if let Some(entered) = self.entered.take() {
            tokio::time::timeout(ENTER_BOUND, entered)
                .await
                .expect("the held wait began in time")
                .expect("the fake kept the gate");
        }
    }

    /// Ends the held wait: the clock moves by its duration.
    pub(crate) fn release(self) {
        // The wait may be gone already, dropped by a cancelled flow.
        let _ = self.release.send(());
    }
}

impl FakePace {
    pub(crate) fn new(clock: Arc<ManualClock>) -> Self {
        Self {
            clock,
            state: Mutex::default(),
        }
    }

    /// Every wait so far, held ones included, in order.
    pub(crate) fn waits(&self) -> Vec<Duration> {
        self.lock().waits.clone()
    }

    /// Holds the next wait not yet held at the returned gate.
    pub(crate) fn hold_next(&self) -> PaceGate {
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        self.lock().held.push_back(Held {
            entered: entered_tx,
            release: release_rx,
        });
        PaceGate {
            entered: Some(entered_rx),
            release: release_tx,
        }
    }

    /// The state, poison-tolerantly.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait]
impl Pace for FakePace {
    async fn wait(&self, duration: Duration) {
        let held = {
            let mut state = self.lock();
            state.waits.push(duration);
            state.held.pop_front()
        };
        if let Some(held) = held {
            // The test may not wait for the wait; then nobody listens.
            let _ = held.entered.send(());
            if held.release.await.is_err() {
                std::future::pending::<()>().await;
            }
        }
        self.clock.advance(duration);
        // Let the other tasks run, as a real wait would.
        tokio::task::yield_now().await;
    }
}
