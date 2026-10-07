//! [`FakeSession`], [`FakeEvents`] and the test's [`SessionController`]: one
//! fake Minecraft session, shared between the code under test and the test.

use core::future::Future;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use fleet_core::chat::ChatMessage;
use fleet_core::mc::{Liveness, SessionError, SessionEvent, SessionEvents, SessionHandle};
use fleet_core::mode::GameAction;
use tokio::sync::Notify;

/// Something the code under test did in a session, in the order it happened.
#[derive(Debug, Clone, PartialEq)]
pub enum Performed {
    /// [`SessionHandle::perform`] ran this action.
    Action(GameAction),
    /// [`SessionHandle::send_chat`] sent this message.
    Chat(ChatMessage),
    /// [`SessionHandle::respawn`] ran.
    Respawn,
    /// [`SessionHandle::disconnect`] tore the session down. It's logged once,
    /// however often it's called.
    Disconnect,
}

/// What [`SessionController::emit`] did with an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitOutcome {
    /// The event is queued for [`SessionEvents::next`].
    Queued,
    /// The queue was full, so the chat event was dropped and counted
    /// ([`SessionController::dropped_chat`]). Only chat is ever dropped.
    ChatDropped,
    /// A `Died` before the last one was answered with a respawn: dropped, as
    /// fleet-mc drops azalea's double death report (ADR-0008 §4).
    DuplicateDeath,
    /// The session already ended with a terminal event, or was torn down, so
    /// no more events are delivered.
    SessionEnded,
}

/// The state one fake session shares between its handle, its events and the
/// test's controller.
#[derive(Debug)]
struct Shared {
    /// How many events the queue holds before chat is dropped.
    capacity: usize,
    state: Mutex<State>,
    /// Wakes the one [`FakeEvents`] when an event is queued or the session is
    /// torn down. `notify_one` keeps a permit when nobody waits yet, so no
    /// wake-up is lost between checking the queue and waiting.
    events_ready: Notify,
}

#[derive(Debug, Default)]
struct State {
    queue: VecDeque<SessionEvent>,
    phase: Phase,
    /// `Died` was emitted and not yet answered with a respawn.
    death_pending: bool,
    hung: bool,
    failing_actions: Option<SessionError>,
    failing_chat: Option<SessionError>,
    frozen_tick: Option<Instant>,
    frozen_packet: Option<Instant>,
    dropped_chat: u64,
    log: Vec<Performed>,
}

/// Where a session is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Phase {
    /// Started, but `Joined` hasn't been emitted yet.
    #[default]
    Starting,
    /// `Joined` was emitted: the bot is in a world, and calls run.
    InWorld,
    /// A terminal event was emitted.
    Ended,
    /// [`SessionHandle::disconnect`] tore the session down.
    TornDown,
}

/// What [`FakeEvents::next`] finds when it looks.
enum Next {
    Event(SessionEvent),
    End,
    Wait,
}

impl State {
    /// Whether a handle call runs now, and the error it fails with if not.
    fn ready(&self) -> Result<(), SessionError> {
        // A torn-down session answers at once, even when it hung: fleet-mc
        // has abandoned the thread by then.
        if self.hung && self.phase != Phase::TornDown {
            return Err(SessionError::TimedOut);
        }
        match self.phase {
            Phase::Starting => Err(SessionError::NotInWorld),
            Phase::InWorld => Ok(()),
            Phase::Ended | Phase::TornDown => Err(SessionError::Closed),
        }
    }
}

impl Shared {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: Mutex::new(State::default()),
            events_ready: Notify::new(),
        }
    }

    /// Locks the state. A test that panicked while holding the lock leaves
    /// consistent data behind, so a poisoned lock is used as it is.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs one handle call: checks that the session can take it, then logs
    /// it.
    fn run(&self, call: Performed) -> Result<(), SessionError> {
        let mut state = self.lock();
        state.ready()?;
        if let (Performed::Action(_), Some(error)) = (&call, state.failing_actions) {
            return Err(error);
        }
        if let (Performed::Chat(_), Some(error)) = (&call, state.failing_chat) {
            return Err(error);
        }
        if matches!(call, Performed::Respawn) {
            state.death_pending = false;
        }
        state.log.push(call);
        Ok(())
    }

    fn tear_down(&self) {
        {
            let mut state = self.lock();
            if state.phase != Phase::TornDown {
                state.phase = Phase::TornDown;
                state.queue.clear();
                state.log.push(Performed::Disconnect);
            }
        }
        self.events_ready.notify_one();
    }

    fn next(&self) -> Next {
        let mut state = self.lock();
        if state.phase == Phase::TornDown {
            return Next::End;
        }
        match state.queue.pop_front() {
            Some(event) => Next::Event(event),
            None if state.phase == Phase::Ended => Next::End,
            None => Next::Wait,
        }
    }
}

/// Starts a fake session: its handle, its events and the test's controller.
pub(super) fn start(capacity: usize) -> (FakeSession, FakeEvents, SessionController) {
    let shared = Arc::new(Shared::new(capacity));
    (
        FakeSession {
            shared: Arc::clone(&shared),
        },
        FakeEvents {
            shared: Arc::clone(&shared),
        },
        SessionController { shared },
    )
}

/// tokio's clock as a std [`Instant`], so paused test time moves it.
fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

/// The fake [`SessionHandle`] that the code under test holds. Clones control
/// the same session.
///
/// A call fails with [`SessionError::Closed`] once the session is torn down
/// or has ended, with [`SessionError::TimedOut`] while it
/// [hangs](SessionController::hang), and with [`SessionError::NotInWorld`]
/// before `Joined` was emitted. Otherwise it's logged and succeeds.
#[derive(Debug, Clone)]
pub struct FakeSession {
    shared: Arc<Shared>,
}

impl SessionHandle for FakeSession {
    fn perform(&self, action: GameAction) -> impl Future<Output = Result<(), SessionError>> + Send {
        let shared = Arc::clone(&self.shared);
        async move { shared.run(Performed::Action(action)) }
    }

    fn send_chat(
        &self,
        message: ChatMessage,
    ) -> impl Future<Output = Result<(), SessionError>> + Send {
        let shared = Arc::clone(&self.shared);
        async move { shared.run(Performed::Chat(message)) }
    }

    fn respawn(&self) -> impl Future<Output = Result<(), SessionError>> + Send {
        let shared = Arc::clone(&self.shared);
        async move { shared.run(Performed::Respawn) }
    }

    fn disconnect(&self) -> impl Future<Output = ()> + Send {
        let shared = Arc::clone(&self.shared);
        async move { shared.tear_down() }
    }

    fn liveness(&self) -> Liveness {
        let state = self.shared.lock();
        let now = now();
        Liveness {
            last_tick: state.frozen_tick.unwrap_or(now),
            last_packet: state.frozen_packet.unwrap_or(now),
        }
    }
}

/// The fake [`SessionEvents`] of one session.
#[derive(Debug)]
pub struct FakeEvents {
    shared: Arc<Shared>,
}

impl SessionEvents for FakeEvents {
    /// Cancel-safe: an event leaves the queue only in the poll that returns
    /// it.
    fn next(&mut self) -> impl Future<Output = Option<SessionEvent>> + Send {
        let shared = Arc::clone(&self.shared);
        async move {
            loop {
                match shared.next() {
                    Next::Event(event) => return Some(event),
                    Next::End => return None,
                    Next::Wait => shared.events_ready.notified().await,
                }
            }
        }
    }
}

/// The test's side of one fake session. Clones control the same session.
#[derive(Debug, Clone)]
pub struct SessionController {
    shared: Arc<Shared>,
}

impl SessionController {
    /// Emits `event` to the session's [`FakeEvents`], keeping the port
    /// contract (see [`EmitOutcome`]).
    ///
    /// `Joined` puts the bot in the world, so handle calls run. A terminal
    /// event (`Disconnected` or `ConnectionFailed`) ends the session: later
    /// calls fail with [`SessionError::Closed`], and
    /// [`next`](SessionEvents::next) returns `None` once it has delivered
    /// the queued events.
    ///
    /// The outcome is `#[must_use]`, so a test can't miss an event that was
    /// dropped or arrived after the session ended.
    #[must_use]
    pub fn emit(&self, event: SessionEvent) -> EmitOutcome {
        let outcome = {
            let mut state = self.shared.lock();
            if matches!(state.phase, Phase::Ended | Phase::TornDown) {
                EmitOutcome::SessionEnded
            } else {
                match event {
                    SessionEvent::Chat(_) if state.queue.len() >= self.shared.capacity => {
                        state.dropped_chat = state.dropped_chat.saturating_add(1);
                        EmitOutcome::ChatDropped
                    }
                    SessionEvent::Died if state.death_pending => EmitOutcome::DuplicateDeath,
                    event => {
                        match event {
                            SessionEvent::Joined => state.phase = Phase::InWorld,
                            SessionEvent::Died => state.death_pending = true,
                            SessionEvent::Disconnected(_) | SessionEvent::ConnectionFailed(_) => {
                                state.phase = Phase::Ended;
                            }
                            SessionEvent::Chat(_) => {}
                        }
                        state.queue.push_back(event);
                        EmitOutcome::Queued
                    }
                }
            }
        };
        if outcome == EmitOutcome::Queued {
            self.shared.events_ready.notify_one();
        }
        outcome
    }

    /// Stops the tick stamp where it is now, as if the session hung. A stamp
    /// that's already frozen stays where it was.
    pub fn freeze_ticks(&self) {
        let mut state = self.shared.lock();
        state.frozen_tick = Some(state.frozen_tick.unwrap_or_else(now));
    }

    /// Stops the packet stamp where it is now, as if the server went silent.
    /// A stamp that's already frozen stays where it was.
    pub fn freeze_packets(&self) {
        let mut state = self.shared.lock();
        state.frozen_packet = Some(state.frozen_packet.unwrap_or_else(now));
    }

    /// Lets both stamps follow the clock again. A hung session stays frozen.
    pub fn unfreeze(&self) {
        let mut state = self.shared.lock();
        if !state.hung {
            state.frozen_tick = None;
            state.frozen_packet = None;
        }
    }

    /// Makes the session hang for good: both stamps freeze, and every
    /// [`SessionHandle`] call fails with [`SessionError::TimedOut`].
    /// [`SessionHandle::disconnect`] still finishes, as fleet-mc abandons a
    /// hung host thread.
    pub fn hang(&self) {
        let mut state = self.shared.lock();
        state.hung = true;
        state.frozen_tick = Some(state.frozen_tick.unwrap_or_else(now));
        state.frozen_packet = Some(state.frozen_packet.unwrap_or_else(now));
    }

    /// Makes [`SessionHandle::perform`] fail with `error` until
    /// [`succeed_actions`](Self::succeed_actions). Chat and respawns still
    /// work.
    pub fn fail_actions(&self, error: SessionError) {
        self.shared.lock().failing_actions = Some(error);
    }

    /// Lets [`SessionHandle::perform`] succeed again.
    pub fn succeed_actions(&self) {
        self.shared.lock().failing_actions = None;
    }

    /// Makes [`SessionHandle::send_chat`] fail with `error` until
    /// [`succeed_chat`](Self::succeed_chat), as when the session can't sign
    /// chat. Actions and respawns still work.
    pub fn fail_chat(&self, error: SessionError) {
        self.shared.lock().failing_chat = Some(error);
    }

    /// Lets [`SessionHandle::send_chat`] succeed again.
    pub fn succeed_chat(&self) {
        self.shared.lock().failing_chat = None;
    }

    /// Returns what the code under test did in this session, in order.
    #[must_use]
    pub fn log(&self) -> Vec<Performed> {
        self.shared.lock().log.clone()
    }

    /// Returns how many chat events were dropped because the queue was full.
    #[must_use]
    pub fn dropped_chat(&self) -> u64 {
        self.shared.lock().dropped_chat
    }

    /// Returns whether [`SessionHandle::disconnect`] has torn the session
    /// down.
    #[must_use]
    pub fn is_torn_down(&self) -> bool {
        self.shared.lock().phase == Phase::TornDown
    }
}
