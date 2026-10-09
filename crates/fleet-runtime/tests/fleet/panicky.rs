//! [`PanickyConnector`]: a connector whose connects or sessions panic on cue,
//! a stand-in for a bug in a port. fleet-testkit's fakes stay panic-free
//! (ADR-0013).
//!
//! A panic in `connect()` panics the bot's actor itself, since the actor
//! awaits the connect. A panic in a session's `perform` panics the mode
//! runner, and one in `disconnect` the teardown: the actor then ends with
//! `TaskCrashed`.
//!
//! The supervisor tests script panics for the whole fleet. The chaos test
//! (P4.8) scripts them per bot, together with refused connects, and reads
//! the connector's log: every connect and every panic, and the rule
//! violations it saw. It also hands the connector its own event receiver:
//! each connect must follow the bot's own published `Connecting`, so a
//! bot never connects while its last published state is Paused or Failed.

use core::future::Future;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use fleet_core::bot::BotState;
use fleet_core::chat::ChatMessage;
use fleet_core::id::BotId;
use fleet_core::mc::{
    ConnectError, ConnectParams, Liveness, MinecraftConnector, SessionError, SessionHandle,
};
use fleet_core::mode::GameAction;
use fleet_runtime::{FleetEvent, FleetEventKind};
use fleet_testkit::mc::{FakeConnector, FakeEvents, FakeSession};
use tokio::sync::broadcast::{self, error::TryRecvError};
use tokio::time::Instant;

/// Where a [`PanickySession`] panics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PanicOn {
    /// When it's asked to jump, which the mode runner does.
    Jump,
    /// In its teardown.
    Disconnect,
}

/// Where a panic the connector caused happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CrashAt {
    /// In `connect()`: the actor itself.
    Connect,
    /// In a session's `perform`: the mode runner.
    Perform,
    /// In a session's `disconnect`: a teardown.
    Teardown,
}

/// A panic the connector caused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Crash {
    pub(crate) bot: BotId,
    pub(crate) at: Instant,
    pub(crate) place: CrashAt,
}

/// One connect, as the connector saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Attempt {
    pub(crate) bot: BotId,
    pub(crate) at: Instant,
    /// The fake's session index, if a session started.
    pub(crate) session: Option<usize>,
    /// Whether a teardown of the same bot was still running.
    pub(crate) teardown_running: bool,
}

/// What one bot's next connects and sessions do wrong.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BotScript {
    /// Its next connects that panic.
    pub(crate) connect_panics: usize,
    /// Its next connects that find no host thread, after those that panic.
    pub(crate) host_unavailable: usize,
    /// Whether its session's next `perform`, of any action, panics.
    pub(crate) perform_panics: bool,
    /// Whether its session's next `disconnect` panics.
    pub(crate) teardown_panics: bool,
}

/// What the connector does wrong and what it saw, shared by its clones.
#[derive(Debug, Default)]
struct Script {
    /// Every connect so far, the panicked ones included.
    attempts: usize,
    /// The connects (counted from 1) that panic.
    panicking: BTreeSet<usize>,
    /// Whether every connect from now on panics.
    panic_always: bool,
    /// Where the sessions it starts panic.
    sessions: BTreeSet<PanicOn>,
    /// What each bot does wrong.
    bots: BTreeMap<BotId, BotScript>,
    /// Every connect of a bot with a script, in order.
    log: Vec<Attempt>,
    /// Every panic the bots' scripts caused, in order.
    crashes: Vec<Crash>,
    /// The fleet's events, if the connector checks connects against them.
    events: Option<broadcast::Receiver<FleetEvent>>,
    /// The bots whose last published state is `Connecting`, with no connect
    /// since.
    connecting: BTreeSet<BotId>,
    /// Each bot's last published state.
    published: BTreeMap<BotId, BotState>,
    /// How many teardowns of each bot are running.
    teardowns: BTreeMap<BotId, usize>,
    /// How many of them tear down a session whose `perform` panicked: the
    /// bot's actor waits for them before it ends.
    crash_teardowns: BTreeMap<BotId, usize>,
    /// What broke the connect rules, for the test to fail on.
    violations: Vec<String>,
}

impl Script {
    /// Reads the events published so far, so `published` and `connecting`
    /// are current.
    fn catch_up(&mut self) {
        let Some(events) = self.events.as_mut() else {
            return;
        };
        loop {
            match events.try_recv() {
                Ok(event) => {
                    if let FleetEventKind::StateChanged(snapshot) = event.kind {
                        if matches!(snapshot.state, BotState::Connecting { .. }) {
                            self.connecting.insert(event.bot_id);
                        } else {
                            self.connecting.remove(&event.bot_id);
                        }
                        self.published.insert(event.bot_id, snapshot.state);
                    }
                }
                Err(TryRecvError::Lagged(missed)) => {
                    self.violations.push(format!(
                        "the connector's receiver lagged by {missed} events"
                    ));
                }
                Err(TryRecvError::Empty | TryRecvError::Closed) => return,
            }
        }
    }
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

    /// Makes the sessions it starts from now on panic at `panic_on` too.
    pub(crate) fn sessions_panic_on(&self, panic_on: PanicOn) {
        self.script.lock().unwrap().sessions.insert(panic_on);
    }

    /// Changes what `bot`'s next connects and sessions do wrong.
    pub(crate) fn script(&self, bot: BotId, change: impl FnOnce(&mut BotScript)) {
        change(self.script.lock().unwrap().bots.entry(bot).or_default());
    }

    /// What `bot`'s next connects and sessions still do wrong.
    pub(crate) fn pending(&self, bot: BotId) -> BotScript {
        self.script
            .lock()
            .unwrap()
            .bots
            .get(&bot)
            .copied()
            .unwrap_or_default()
    }

    /// Checks every connect from now on against `events`: it must follow
    /// the bot's own published `Connecting`.
    pub(crate) fn observe(&self, events: broadcast::Receiver<FleetEvent>) {
        self.script.lock().unwrap().events = Some(events);
    }

    /// Every connect of a bot with a script, in order.
    pub(crate) fn log(&self) -> Vec<Attempt> {
        self.script.lock().unwrap().log.clone()
    }

    /// Every panic the bots' scripts caused, in order.
    pub(crate) fn crashes(&self) -> Vec<Crash> {
        self.script.lock().unwrap().crashes.clone()
    }

    /// What broke the connect rules so far.
    pub(crate) fn violations(&self) -> Vec<String> {
        let mut script = self.script.lock().unwrap();
        script.catch_up();
        script.violations.clone()
    }

    /// Whether a session of `bot` whose `perform` panicked is still inside
    /// its `disconnect()`, so the bot's actor is still ending.
    pub(crate) fn in_crash_teardown(&self, bot: BotId) -> bool {
        self.script
            .lock()
            .unwrap()
            .crash_teardowns
            .get(&bot)
            .is_some_and(|running| *running > 0)
    }

    /// The bot whose connect started the fake's session `index`.
    pub(crate) fn bot_of_session(&self, index: usize) -> Option<BotId> {
        self.script
            .lock()
            .unwrap()
            .log
            .iter()
            .find(|attempt| attempt.session == Some(index))
            .map(|attempt| attempt.bot)
    }
}

impl MinecraftConnector for PanickyConnector {
    type Session = PanickySession;
    type Events = FakeEvents;

    fn connect(
        &self,
        params: ConnectParams,
    ) -> impl Future<Output = Result<(Self::Session, Self::Events), ConnectError>> + Send {
        let bot = params.bot_id;
        let (panics, unavailable, sessions, scripted) = {
            let mut script = self.script.lock().unwrap();
            script.attempts += 1;
            let attempt = script.attempts;
            let global = script.panic_always || script.panicking.contains(&attempt);
            let sessions = script.sessions.clone();
            if script.events.is_some() {
                script.catch_up();
                if !script.connecting.remove(&bot) {
                    let last = script.published.get(&bot).copied();
                    script.violations.push(format!(
                        "{bot} connected without its own Connecting; its last published state is {last:?}"
                    ));
                }
            }
            let at = Instant::now();
            let teardown_running = script
                .teardowns
                .get(&bot)
                .is_some_and(|running| *running > 0);
            match script.bots.get_mut(&bot) {
                Some(own) => {
                    let panics = own.connect_panics > 0;
                    let unavailable = !panics && own.host_unavailable > 0;
                    if panics {
                        own.connect_panics -= 1;
                        script.crashes.push(Crash {
                            bot,
                            at,
                            place: CrashAt::Connect,
                        });
                    } else if unavailable {
                        own.host_unavailable -= 1;
                    }
                    script.log.push(Attempt {
                        bot,
                        at,
                        session: None,
                        teardown_running,
                    });
                    (global || panics, unavailable, sessions, true)
                }
                None => (global, false, sessions, false),
            }
        };
        let fake = self.fake.clone();
        let shared = Arc::clone(&self.script);
        async move {
            assert!(!panics, "a test panic in connect");
            if unavailable {
                return Err(ConnectError::HostUnavailable);
            }
            let (session, events) = fake.connect(params).await?;
            if scripted {
                // The fake started the session synchronously, so it's the
                // latest one.
                let index = fake.session_count() - 1;
                let mut script = shared.lock().unwrap();
                if let Some(attempt) = script.log.iter_mut().rev().find(|a| a.bot == bot) {
                    attempt.session = Some(index);
                }
            }
            Ok((
                PanickySession {
                    session,
                    panics: sessions,
                    bot,
                    script: shared,
                    perform_panicked: Arc::default(),
                },
                events,
            ))
        }
    }
}

/// A [`FakeSession`] that panics where its connector's script said.
#[derive(Debug, Clone)]
pub(crate) struct PanickySession {
    session: FakeSession,
    /// Where it panics, from the fleet-wide script.
    panics: BTreeSet<PanicOn>,
    bot: BotId,
    script: Arc<Mutex<Script>>,
    /// Whether one of its `perform`s panicked; its clones share it.
    perform_panicked: Arc<AtomicBool>,
}

impl SessionHandle for PanickySession {
    fn perform(&self, action: GameAction) -> impl Future<Output = Result<(), SessionError>> + Send {
        let session = self.session.clone();
        let panics = {
            let mut script = self.script.lock().unwrap();
            let own = script
                .bots
                .get_mut(&self.bot)
                .is_some_and(|own| core::mem::take(&mut own.perform_panics));
            if own {
                script.crashes.push(Crash {
                    bot: self.bot,
                    at: Instant::now(),
                    place: CrashAt::Perform,
                });
            }
            own || (self.panics.contains(&PanicOn::Jump) && action == GameAction::Jump)
        };
        if panics {
            self.perform_panicked.store(true, Ordering::SeqCst);
        }
        async move {
            assert!(!panics, "a test panic in perform");
            session.perform(action).await
        }
    }

    fn send_chat(
        &self,
        message: ChatMessage,
    ) -> impl Future<Output = Result<(), SessionError>> + Send {
        self.session.send_chat(message)
    }

    fn respawn(&self) -> impl Future<Output = Result<(), SessionError>> + Send {
        self.session.respawn()
    }

    fn disconnect(&self) -> impl Future<Output = ()> + Send {
        let session = self.session.clone();
        let bot = self.bot;
        let shared = Arc::clone(&self.script);
        let crashed = self.perform_panicked.load(Ordering::SeqCst);
        let (own, global) = {
            let mut script = shared.lock().unwrap();
            *script.teardowns.entry(bot).or_default() += 1;
            if crashed {
                *script.crash_teardowns.entry(bot).or_default() += 1;
            }
            let own = script
                .bots
                .get_mut(&bot)
                .is_some_and(|own| core::mem::take(&mut own.teardown_panics));
            (own, self.panics.contains(&PanicOn::Disconnect))
        };
        async move {
            session.disconnect().await;
            {
                let mut script = shared.lock().unwrap();
                if let Some(running) = script.teardowns.get_mut(&bot) {
                    *running -= 1;
                }
                if crashed && let Some(running) = script.crash_teardowns.get_mut(&bot) {
                    *running -= 1;
                }
                if own {
                    script.crashes.push(Crash {
                        bot,
                        at: Instant::now(),
                        place: CrashAt::Teardown,
                    });
                }
            }
            assert!(!(own || global), "a test panic in disconnect");
        }
    }

    fn liveness(&self) -> Liveness {
        self.session.liveness()
    }
}
