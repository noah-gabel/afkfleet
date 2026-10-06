//! [`FakeConnector`]: starts fake sessions with scripted results.

use core::future::Future;
use core::pin::pin;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use fleet_core::mc::{ConnectError, ConnectParams, MinecraftConnector};
use tokio::sync::Notify;

use super::session::{self, FakeEvents, FakeSession, SessionController};

/// A fake [`MinecraftConnector`] (Plan.md P3.1, ADR-0011). Clones share the
/// same script, log and sessions.
///
/// Every [`connect`](MinecraftConnector::connect) is recorded. It returns the
/// next scripted result; with nothing scripted, it starts a session. The test
/// reaches each session through [`session`](Self::session) or
/// [`try_session`](Self::try_session), numbered from 0 in the order they
/// started.
#[derive(Debug, Clone)]
pub struct FakeConnector {
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    /// How many events each session's queue holds before chat is dropped.
    event_capacity: usize,
    state: Mutex<State>,
    /// Wakes [`FakeConnector::session`] when a session starts.
    session_started: Notify,
}

#[derive(Debug, Default)]
struct State {
    script: VecDeque<Result<(), ConnectError>>,
    connects: Vec<ConnectParams>,
    sessions: Vec<SessionController>,
}

impl Shared {
    /// Locks the state. A test that panicked while holding the lock leaves
    /// consistent data behind, so a poisoned lock is used as it is.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl FakeConnector {
    /// How many events a session's queue holds by default before chat is
    /// dropped: the size of fleet-mc's event bridge (ADR-0011).
    pub const DEFAULT_EVENT_CAPACITY: usize = 64;

    /// Creates a connector whose sessions queue up to
    /// [`DEFAULT_EVENT_CAPACITY`](Self::DEFAULT_EVENT_CAPACITY) events.
    #[must_use]
    pub fn new() -> Self {
        Self::with_event_capacity(Self::DEFAULT_EVENT_CAPACITY)
    }

    /// Creates a connector whose sessions queue up to `capacity` events before
    /// chat is dropped.
    #[must_use]
    pub fn with_event_capacity(capacity: usize) -> Self {
        Self {
            shared: Arc::new(Shared {
                event_capacity: capacity,
                state: Mutex::new(State::default()),
                session_started: Notify::new(),
            }),
        }
    }

    /// Scripts the result of a later connect: `Ok(())` starts a session, and
    /// an error is returned as it is. Results are used in the order they were
    /// pushed; once they run out, every connect starts a session.
    pub fn push_connect_result(&self, result: Result<(), ConnectError>) {
        self.shared.lock().script.push_back(result);
    }

    /// Returns the parameters of every connect so far, failed ones included.
    #[must_use]
    pub fn connects(&self) -> Vec<ConnectParams> {
        self.shared.lock().connects.clone()
    }

    /// Returns how many sessions have started.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.shared.lock().sessions.len()
    }

    /// Returns the controller of session `index` (from 0), if it has started.
    #[must_use]
    pub fn try_session(&self, index: usize) -> Option<SessionController> {
        self.shared.lock().sessions.get(index).cloned()
    }

    /// Waits until session `index` (from 0) has started and returns its
    /// controller. It waits forever if no such session starts, so a test
    /// that isn't sure wraps it in `tokio::time::timeout`.
    pub async fn session(&self, index: usize) -> SessionController {
        loop {
            // Register before looking, so a session that starts in between
            // still wakes this waiter.
            let mut started = pin!(self.shared.session_started.notified());
            started.as_mut().enable();
            if let Some(controller) = self.try_session(index) {
                return controller;
            }
            started.await;
        }
    }

    /// Records `params` and returns the next scripted result, starting a
    /// session unless the script says otherwise.
    fn start(&self, params: ConnectParams) -> Result<(FakeSession, FakeEvents), ConnectError> {
        let result = {
            let mut state = self.shared.lock();
            state.connects.push(params);
            if let Some(Err(error)) = state.script.pop_front() {
                Err(error)
            } else {
                let (session, events, controller) = session::start(self.shared.event_capacity);
                state.sessions.push(controller);
                Ok((session, events))
            }
        };
        if result.is_ok() {
            self.shared.session_started.notify_waiters();
        }
        result
    }
}

impl Default for FakeConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl MinecraftConnector for FakeConnector {
    type Session = FakeSession;
    type Events = FakeEvents;

    fn connect(
        &self,
        params: ConnectParams,
    ) -> impl Future<Output = Result<(Self::Session, Self::Events), ConnectError>> + Send {
        let connector = self.clone();
        async move { connector.start(params) }
    }
}
