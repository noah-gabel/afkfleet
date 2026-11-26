//! [`FakeSession`], [`FakeEvents`] and the test's [`SessionController`]: one
//! fake Minecraft session, shared between the code under test and the test.

use core::future::Future;
use core::time::Duration;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use fleet_core::chat::ChatMessage;
use fleet_core::mc::{
    Guards, Liveness, MenuCommand, MenuError, Observation, SessionError, SessionEvent,
    SessionEvents, SessionHandle,
};
use fleet_core::mode::GameAction;
use fleet_core::script::{Input, MouseButton};
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
    /// [`SessionHandle::observe`] took a snapshot.
    Observed,
    /// [`SessionHandle::set_guards`] set these.
    Guards(Guards),
    /// [`SessionHandle::menu`] carried this out. A command the test made
    /// it refuse isn't logged: nothing was sent.
    Menu(MenuCommand),
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
    /// A `Died` before the last one was answered with a confirmed respawn:
    /// dropped, as fleet-mc drops azalea's double death report (ADR-0008 §4).
    DuplicateDeath,
    /// A `Respawned` no respawn asked for: a death the code under test took
    /// and a [`SessionHandle::respawn`] since (ADR-0020, S22). Dropped.
    UnrequestedRespawn,
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
    death: Death,
    /// Respawns asked for are confirmed only once this is false again.
    withholding: bool,
    hung: bool,
    failing_actions: Option<SessionError>,
    failing_performs: Option<SessionError>,
    failing_chat: Option<SessionError>,
    failing_respawn: Option<SessionError>,
    /// How long [`SessionHandle::disconnect`] takes after it has closed the
    /// session.
    disconnect_delay: Duration,
    frozen_tick: Option<Instant>,
    frozen_packet: Option<Instant>,
    dropped_chat: u64,
    log: Vec<Performed>,
    /// What [`SessionHandle::observe`] answers with, once the test has
    /// shown the session something.
    shown: Option<Observation>,
    /// The keys and buttons a scripted mode holds down, in the order they
    /// went down.
    held: Vec<Input>,
    /// A screen is open: from a `MenuOpened` the test emitted to its
    /// `MenuClosed`, or to a `MenuCommand::Close` the code under test sent.
    /// Nothing goes down meanwhile, as in fleet-mc (M8).
    screen_open: bool,
    /// What [`SessionHandle::menu`] answers with instead of carrying a
    /// command out, until the test lets it again.
    refusing_menu: Option<MenuError>,
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

/// The death the session reported, as fleet-mc's bridge keeps it
/// (ADR-0020, S22).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Death {
    /// None since the last confirmed respawn.
    #[default]
    None,
    /// `Died` is queued and not yet read.
    Queued,
    /// `Died` was read; `requested` once a respawn was asked for since.
    Read {
        /// Whether a respawn was asked for since.
        requested: bool,
    },
}

/// What [`FakeEvents::next`] finds when it looks.
enum Next {
    Event(SessionEvent),
    End,
    Wait,
}

impl State {
    /// A respawn was asked for: it counts only after its `Died` was read,
    /// and is confirmed at once unless held back. Returns whether it queued
    /// `Respawned`.
    fn respawn_asked(&mut self) -> bool {
        if let Death::Read { .. } = self.death {
            self.death = Death::Read { requested: true };
        }
        self.confirm()
    }

    /// Queues `Respawned` for a respawn asked for, unless held back or the
    /// session ended. Returns whether it did.
    fn confirm(&mut self) -> bool {
        let ended = matches!(self.phase, Phase::Ended | Phase::TornDown);
        if self.withholding || ended || self.death != (Death::Read { requested: true }) {
            return false;
        }
        self.death = Death::None;
        self.queue.push_back(SessionEvent::Respawned);
        true
    }

    /// Keeps what a scripted mode's `action` holds down, as fleet-mc's
    /// own record of it does: a press of an input that can be held puts it
    /// down, a release lets it go, and a tap holds nothing.
    fn hold(&mut self, action: GameAction) {
        let right = Input::Mouse(MouseButton::Right);
        if self.screen_open && matches!(action, GameAction::Press(_) | GameAction::HoldUntilUsed) {
            return;
        }
        match action {
            GameAction::Press(input) if input.can_be_held() => self.put_down(input),
            GameAction::HoldUntilUsed => self.put_down(right),
            GameAction::Release(input) => self.held.retain(|held| *held != input),
            GameAction::ReleaseKeys => self.held.retain(|held| matches!(held, Input::Mouse(_))),
            GameAction::ReleaseAll => self.held.clear(),
            _ => {}
        }
    }

    fn put_down(&mut self, input: Input) {
        if !self.held.contains(&input) {
            self.held.push(input);
        }
    }

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
    /// it. A respawn asked for after its `Died` was read is confirmed, as a
    /// cooperative server would, unless confirmations are held back.
    fn run(&self, call: Performed) -> Result<(), SessionError> {
        let confirmed = {
            let mut state = self.lock();
            state.ready()?;
            if let (Performed::Action(_) | Performed::Guards(_), Some(error)) =
                (&call, state.failing_actions)
            {
                return Err(error);
            }
            if let (Performed::Action(_), Some(error)) = (&call, state.failing_performs) {
                return Err(error);
            }
            if let (Performed::Chat(_), Some(error)) = (&call, state.failing_chat) {
                return Err(error);
            }
            if let (Performed::Respawn, Some(error)) = (&call, state.failing_respawn) {
                return Err(error);
            }
            let respawn = matches!(call, Performed::Respawn);
            if let Performed::Action(action) = &call {
                state.hold(*action);
            }
            state.log.push(call);
            respawn && state.respawn_asked()
        };
        if confirmed {
            self.events_ready.notify_one();
        }
        Ok(())
    }

    /// Takes a snapshot: checks that the session can answer, logs it, and
    /// answers with what the test last showed. A session shown nothing
    /// has a bot that isn't placed in its world yet.
    fn observe(&self) -> Result<Observation, SessionError> {
        let mut state = self.lock();
        state.ready()?;
        if let Some(error) = state.failing_actions {
            return Err(error);
        }
        let seen = state.shown.clone().ok_or(SessionError::NotInWorld)?;
        state.log.push(Performed::Observed);
        Ok(seen)
    }

    /// Runs one [`SessionHandle::menu`] call: checks that the session can
    /// take it, answers with the refusal the test set, or logs it.
    fn menu(&self, command: MenuCommand) -> Result<(), MenuError> {
        let mut state = self.lock();
        state.ready()?;
        if let Some(error) = state.failing_actions {
            return Err(error.into());
        }
        if let Some(refusal) = state.refusing_menu {
            return Err(refusal);
        }
        // The bot's own close ends the screen, as fleet-mc's does: keys go
        // down again.
        if matches!(command, MenuCommand::Close(_)) {
            state.screen_open = false;
        }
        state.log.push(Performed::Menu(command));
        Ok(())
    }

    /// Tears the session down and returns how long the teardown still takes.
    fn tear_down(&self) -> Duration {
        let delay = {
            let mut state = self.lock();
            if state.phase != Phase::TornDown {
                state.phase = Phase::TornDown;
                state.held.clear();
                state.queue.clear();
                state.log.push(Performed::Disconnect);
            }
            state.disconnect_delay
        };
        self.events_ready.notify_one();
        delay
    }

    fn next(&self) -> Next {
        let mut state = self.lock();
        if state.phase == Phase::TornDown {
            return Next::End;
        }
        match state.queue.pop_front() {
            Some(SessionEvent::Died) => {
                if state.death == Death::Queued {
                    state.death = Death::Read { requested: false };
                }
                Next::Event(SessionEvent::Died)
            }
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

    fn observe(&self) -> impl Future<Output = Result<Observation, SessionError>> + Send {
        let shared = Arc::clone(&self.shared);
        async move { shared.observe() }
    }

    fn set_guards(&self, guards: Guards) -> impl Future<Output = Result<(), SessionError>> + Send {
        let shared = Arc::clone(&self.shared);
        async move { shared.run(Performed::Guards(guards)) }
    }

    fn respawn(&self) -> impl Future<Output = Result<(), SessionError>> + Send {
        let shared = Arc::clone(&self.shared);
        async move { shared.run(Performed::Respawn) }
    }

    fn menu(&self, command: MenuCommand) -> impl Future<Output = Result<(), MenuError>> + Send {
        let shared = Arc::clone(&self.shared);
        async move { shared.menu(command) }
    }

    fn disconnect(&self) -> impl Future<Output = ()> + Send {
        let shared = Arc::clone(&self.shared);
        async move {
            let delay = shared.tear_down();
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }
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
                    SessionEvent::Died if state.death != Death::None => EmitOutcome::DuplicateDeath,
                    SessionEvent::Respawned => {
                        if state.death == (Death::Read { requested: true }) {
                            state.death = Death::None;
                            state.queue.push_back(SessionEvent::Respawned);
                            EmitOutcome::Queued
                        } else {
                            EmitOutcome::UnrequestedRespawn
                        }
                    }
                    event => {
                        match event {
                            SessionEvent::Joined => state.phase = Phase::InWorld,
                            SessionEvent::Died => state.death = Death::Queued,
                            SessionEvent::Disconnected(_) | SessionEvent::ConnectionFailed(_) => {
                                state.phase = Phase::Ended;
                                state.held.clear();
                            }
                            // As fleet-mc does: a world change lets go of
                            // everything, and a let-go of its own inputs
                            // (ADR-0042, G1, G6).
                            SessionEvent::WorldChanged => state.held.clear(),
                            SessionEvent::LetGo(reason) => {
                                let gone = reason.inputs();
                                state.held.retain(|held| !gone.contains(held));
                            }
                            // A screen lets go of everything, and nothing
                            // goes down while it's open (ADR-0036, M8).
                            SessionEvent::MenuOpened(_) => {
                                state.held.clear();
                                state.screen_open = true;
                            }
                            SessionEvent::MenuClosed(_) => state.screen_open = false,
                            SessionEvent::Chat(_)
                            | SessionEvent::Respawned
                            | SessionEvent::MenuUpdated(_)
                            | SessionEvent::Clicked(_)
                            | SessionEvent::SignOpened(_) => {}
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

    /// Shows the session `observation`: what [`SessionHandle::observe`]
    /// answers with from now on. Before the first, a bot that is in the
    /// world still sees nothing, and `observe` fails with
    /// [`SessionError::NotInWorld`].
    pub fn show(&self, observation: Observation) {
        self.shared.lock().shown = Some(observation);
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

    /// Makes [`SessionHandle::perform`], [`SessionHandle::set_guards`],
    /// [`SessionHandle::observe`] and [`SessionHandle::menu`] fail with
    /// `error` until
    /// [`succeed_actions`](Self::succeed_actions), as a session whose job
    /// queue is full or that doesn't answer. Chat and respawns still work.
    pub fn fail_actions(&self, error: SessionError) {
        self.shared.lock().failing_actions = Some(error);
    }

    /// Lets [`SessionHandle::perform`] succeed again.
    pub fn succeed_actions(&self) {
        self.shared.lock().failing_actions = None;
    }

    /// Makes [`SessionHandle::perform`] alone fail with `error` until
    /// [`succeed_performs`](Self::succeed_performs): the session still
    /// takes its guards and still says what the bot sees, which
    /// [`fail_actions`](Self::fail_actions) stops too.
    pub fn fail_performs(&self, error: SessionError) {
        self.shared.lock().failing_performs = Some(error);
    }

    /// Lets [`SessionHandle::perform`] succeed again, unless
    /// [`fail_actions`](Self::fail_actions) still holds it.
    pub fn succeed_performs(&self) {
        self.shared.lock().failing_performs = None;
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

    /// Makes [`SessionHandle::respawn`] fail with `error` until
    /// [`succeed_respawn`](Self::succeed_respawn). A failed respawn leaves
    /// the bot dead. Actions and chat still work.
    pub fn fail_respawn(&self, error: SessionError) {
        self.shared.lock().failing_respawn = Some(error);
    }

    /// Lets [`SessionHandle::respawn`] succeed again.
    pub fn succeed_respawn(&self) {
        self.shared.lock().failing_respawn = None;
    }

    /// Makes [`SessionHandle::menu`] answer every command with `error`
    /// until [`accept_menu`](Self::accept_menu), as fleet-mc does when the
    /// gate, the guard or the sign desk says no. A session that can't take
    /// the call at all still fails with its [`SessionError`] first.
    pub fn refuse_menu(&self, error: MenuError) {
        self.shared.lock().refusing_menu = Some(error);
    }

    /// Lets [`SessionHandle::menu`] carry commands out again.
    pub fn accept_menu(&self) {
        self.shared.lock().refusing_menu = None;
    }

    /// Holds back the confirmation of every respawn asked for from now on,
    /// as a server that's slow to respawn the bot or ignores the request
    /// (ADR-0020, S23): `respawn()` still succeeds, but no `Respawned`
    /// comes until [`confirm_respawns`](Self::confirm_respawns).
    pub fn withhold_respawns(&self) {
        self.shared.lock().withholding = true;
    }

    /// Confirms respawns again: one asked for while they were held back is
    /// confirmed at once, as a late load would.
    pub fn confirm_respawns(&self) {
        let confirmed = {
            let mut state = self.shared.lock();
            state.withholding = false;
            state.confirm()
        };
        if confirmed {
            self.shared.events_ready.notify_one();
        }
    }

    /// Makes every later [`SessionHandle::disconnect`] slow, the way fleet-mc
    /// closes a session first and then waits for its host thread: the session
    /// is torn down at once (calls fail with [`SessionError::Closed`] and
    /// [`next`](SessionEvents::next) returns `None`), and `disconnect`
    /// returns after `delay` on tokio's clock.
    pub fn delay_disconnect(&self, delay: Duration) {
        self.shared.lock().disconnect_delay = delay;
    }

    /// Returns what the code under test did in this session, in order.
    #[must_use]
    pub fn log(&self) -> Vec<Performed> {
        self.shared.lock().log.clone()
    }

    /// Returns the keys and buttons a scripted mode holds down in this
    /// session right now, in the order they went down: what its presses
    /// put down and no release, world change, let-go or end of the session
    /// has let go of since.
    #[must_use]
    pub fn held(&self) -> Vec<Input> {
        self.shared.lock().held.clone()
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
