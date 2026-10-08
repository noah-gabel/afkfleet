//! [`PanickyConnector`]: a connector whose connects or sessions panic on cue,
//! a stand-in for a bug in a port. fleet-testkit's fakes stay panic-free
//! (ADR-0013).
//!
//! A panic in `connect()` panics the bot's actor itself, since the actor
//! awaits the connect. A panic in a session's `perform` panics the mode
//! runner, and one in `disconnect` the teardown: the actor then ends with
//! `TaskCrashed`.

use core::future::Future;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use fleet_core::chat::ChatMessage;
use fleet_core::mc::{
    ConnectError, ConnectParams, Liveness, MinecraftConnector, SessionError, SessionHandle,
};
use fleet_core::mode::GameAction;
use fleet_testkit::mc::{FakeConnector, FakeEvents, FakeSession};

/// Where a [`PanickySession`] panics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanicOn {
    /// When it's asked to jump, which the mode runner does.
    Jump,
    /// In its teardown.
    Disconnect,
}

/// What the connector does wrong, shared by its clones.
#[derive(Debug, Default)]
struct Script {
    /// Every connect so far, the panicked ones included.
    attempts: usize,
    /// The connects (counted from 1) that panic.
    panicking: BTreeSet<usize>,
    /// Whether every connect from now on panics.
    panic_always: bool,
    /// Where the sessions it starts panic.
    sessions: Option<PanicOn>,
}

/// Wraps a [`FakeConnector`]: connects and sessions behave like the fake's
/// unless the script says they panic. Clones share the script.
#[derive(Debug, Clone)]
pub(crate) struct PanickyConnector {
    fake: FakeConnector,
    script: Arc<Mutex<Script>>,
}

impl PanickyConnector {
    pub(crate) fn new(fake: FakeConnector) -> Self {
        Self {
            fake,
            script: Arc::default(),
        }
    }

    /// Every connect so far, the panicked ones included.
    pub(crate) fn attempts(&self) -> usize {
        self.script.lock().unwrap().attempts
    }

    /// Makes connect number `attempt` (counted from 1) panic.
    pub(crate) fn panic_on_connect(&self, attempt: usize) {
        self.script.lock().unwrap().panicking.insert(attempt);
    }

    /// Makes every connect panic, or none if `on` is false.
    pub(crate) fn panic_always(&self, on: bool) {
        self.script.lock().unwrap().panic_always = on;
    }

    /// Makes the sessions it starts from now on panic at `panic_on`.
    pub(crate) fn sessions_panic_on(&self, panic_on: PanicOn) {
        self.script.lock().unwrap().sessions = Some(panic_on);
    }
}

impl MinecraftConnector for PanickyConnector {
    type Session = PanickySession;
    type Events = FakeEvents;

    fn connect(
        &self,
        params: ConnectParams,
    ) -> impl Future<Output = Result<(Self::Session, Self::Events), ConnectError>> + Send {
        let (panics, sessions) = {
            let mut script = self.script.lock().unwrap();
            script.attempts += 1;
            let attempt = script.attempts;
            (
                script.panic_always || script.panicking.contains(&attempt),
                script.sessions,
            )
        };
        let fake = self.fake.clone();
        async move {
            assert!(!panics, "a test panic in connect");
            let (session, events) = fake.connect(params).await?;
            Ok((PanickySession(session, sessions), events))
        }
    }
}

/// A [`FakeSession`] that panics where its connector's script said.
#[derive(Debug, Clone)]
pub(crate) struct PanickySession(FakeSession, Option<PanicOn>);

impl SessionHandle for PanickySession {
    fn perform(&self, action: GameAction) -> impl Future<Output = Result<(), SessionError>> + Send {
        let session = self.0.clone();
        let panics = self.1 == Some(PanicOn::Jump) && action == GameAction::Jump;
        async move {
            assert!(!panics, "a test panic in perform");
            session.perform(action).await
        }
    }

    fn send_chat(
        &self,
        message: ChatMessage,
    ) -> impl Future<Output = Result<(), SessionError>> + Send {
        self.0.send_chat(message)
    }

    fn respawn(&self) -> impl Future<Output = Result<(), SessionError>> + Send {
        self.0.respawn()
    }

    fn disconnect(&self) -> impl Future<Output = ()> + Send {
        let session = self.0.clone();
        let panics = self.1 == Some(PanicOn::Disconnect);
        async move {
            session.disconnect().await;
            assert!(!panics, "a test panic in disconnect");
        }
    }

    fn liveness(&self) -> Liveness {
        self.0.liveness()
    }
}
