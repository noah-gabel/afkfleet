//! The bots' sessions across every agent run of a case (ADR-0019, S72):
//! each run's connector is a [`Connector`] over its own fake, and every
//! session it starts is booked here. A session that starts while an
//! earlier one of the same account is still live, on any agent and in any
//! run, is a violation: two agents, or two runs, playing one account.
//!
//! A session is live until it's torn down, or until the case ended it with
//! [`Ledger::end`] (a kick, a refused login); a crashed run's sessions die
//! with it ([`Ledger::dead`]). A new session is joined at once, so its bot
//! goes Online, unless the case held its bot's next one ([`Ledger::hold`]).

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use fleet_core::disconnect::DisconnectReason;
use fleet_core::id::BotId;
use fleet_core::mc::{
    ConnectError, ConnectParams, MinecraftConnector, SessionCredentials, SessionEvent,
};
use fleet_testkit::mc::{EmitOutcome, FakeConnector, FakeEvents, FakeSession, SessionController};

/// The case's ledger; clones share it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Ledger(Arc<Mutex<Book>>);

#[derive(Debug, Default)]
struct Book {
    runs: Vec<Run>,
    /// The bots whose next session isn't joined.
    held: BTreeSet<BotId>,
    violations: Vec<String>,
}

#[derive(Debug)]
struct Run {
    agent: String,
    dead: bool,
    sessions: Vec<Session>,
}

#[derive(Debug)]
struct Session {
    account: String,
    bot: BotId,
    ended: bool,
    controller: SessionController,
}

impl Session {
    fn live(&self) -> bool {
        !self.ended && !self.controller.is_torn_down()
    }
}

impl Ledger {
    /// A connector for a new run of `agent`.
    pub(crate) fn run(&self, agent: &str) -> Connector {
        let mut book = self.0.lock().unwrap();
        book.runs.push(Run {
            agent: agent.to_owned(),
            dead: false,
            sessions: Vec::new(),
        });
        Connector {
            ledger: self.clone(),
            run: book.runs.len() - 1,
            fake: FakeConnector::new(),
        }
    }

    /// `run` crashed: its sessions died with it.
    pub(crate) fn dead(&self, run: usize) {
        self.0.lock().unwrap().runs[run].dead = true;
    }

    /// `bot`'s next session isn't joined.
    pub(crate) fn hold(&self, bot: BotId) {
        self.0.lock().unwrap().held.insert(bot);
    }

    /// Ends `bot`'s live session with `reason`, as its server would.
    pub(crate) fn end(&self, bot: BotId, reason: DisconnectReason) {
        let mut book = self.0.lock().unwrap();
        let session = book
            .runs
            .iter_mut()
            .filter(|run| !run.dead)
            .flat_map(|run| run.sessions.iter_mut())
            .rfind(|session| session.bot == bot && session.live())
            .expect("the bot has a live session");
        session.ended = true;
        assert_eq!(
            session.controller.emit(SessionEvent::Disconnected(reason)),
            EmitOutcome::Queued
        );
    }

    /// Whether `bot` has a live session in a run that didn't crash.
    pub(crate) fn live(&self, bot: BotId) -> bool {
        let book = self.0.lock().unwrap();
        book.runs
            .iter()
            .filter(|run| !run.dead)
            .flat_map(|run| &run.sessions)
            .any(|session| session.bot == bot && session.live())
    }

    /// How many sessions `bot` started, in every run.
    pub(crate) fn sessions(&self, bot: BotId) -> usize {
        let book = self.0.lock().unwrap();
        book.runs
            .iter()
            .flat_map(|run| &run.sessions)
            .filter(|session| session.bot == bot)
            .count()
    }

    /// How many sessions `bot` started in runs of `agent`.
    pub(crate) fn sessions_on(&self, bot: BotId, agent: &str) -> usize {
        let book = self.0.lock().unwrap();
        book.runs
            .iter()
            .filter(|run| run.agent == agent)
            .flat_map(|run| &run.sessions)
            .filter(|session| session.bot == bot)
            .count()
    }

    /// Fails the case if any account ever had two live sessions.
    pub(crate) fn assert_clean(&self) {
        let book = self.0.lock().unwrap();
        assert!(
            book.violations.is_empty(),
            "an account ran twice: {:#?}",
            book.violations
        );
    }

    /// Books the session `controller` of `bot` on `account`, started by
    /// `run`.
    fn started(&self, run: usize, account: String, bot: BotId, controller: &SessionController) {
        let mut book = self.0.lock().unwrap();
        let starting = book.runs[run].agent.clone();
        let twice: Vec<String> = book
            .runs
            .iter()
            .enumerate()
            .filter(|(_, earlier)| !earlier.dead)
            .flat_map(|(index, earlier)| {
                earlier
                    .sessions
                    .iter()
                    .map(move |session| (index, earlier, session))
            })
            .filter(|(_, _, session)| session.account == account && session.live())
            .map(|(index, earlier, session)| {
                format!(
                    "{account}: bot {bot} started on {starting} (run {run}) while bot {} lived on {} (run {index})",
                    session.bot, earlier.agent,
                )
            })
            .collect();
        book.violations.extend(twice);
        let joined = !book.held.remove(&bot);
        book.runs[run].sessions.push(Session {
            account,
            bot,
            ended: false,
            controller: controller.clone(),
        });
        if joined {
            assert_eq!(controller.emit(SessionEvent::Joined), EmitOutcome::Queued);
        }
    }
}

/// One agent run's connector: its own fake, booked in the ledger.
#[derive(Debug, Clone)]
pub(crate) struct Connector {
    ledger: Ledger,
    run: usize,
    fake: FakeConnector,
}

impl Connector {
    /// The run's number in the ledger.
    pub(crate) fn run(&self) -> usize {
        self.run
    }

    /// The run's fake connector.
    pub(crate) fn fake(&self) -> &FakeConnector {
        &self.fake
    }
}

impl MinecraftConnector for Connector {
    type Session = FakeSession;
    type Events = FakeEvents;

    fn connect(
        &self,
        params: ConnectParams,
    ) -> impl Future<Output = Result<(Self::Session, Self::Events), ConnectError>> + Send {
        let this = self.clone();
        async move {
            let bot = params.bot_id;
            let account = match &params.credentials {
                SessionCredentials::Offline { username }
                | SessionCredentials::Online { username, .. } => username.to_string(),
            };
            let started = this.fake.connect(params).await;
            if started.is_ok() {
                // The fake starts its session before it answers, on this
                // thread: the newest one is this connect's.
                let newest = this.fake.session_count() - 1;
                let controller = this.fake.try_session(newest).unwrap();
                this.ledger.started(this.run, account, bot, &controller);
            }
            started
        }
    }
}
