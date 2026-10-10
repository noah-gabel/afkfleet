//! The chaos property test (Plan.md P4.8; ADR-0013). Each case runs a
//! fleet of 1–3 bots on paused time through up to 30 random faults and
//! `Fleet` calls, lets it settle, and checks:
//! - **No panic escapes.** The actors' panics stay inside the supervisor,
//!   whose task ends cleanly at the final shutdown.
//! - **Bots heal.** A bot that should run and saw only transient faults and
//!   at most 5 crashes ends Online; with 6 or more crashes it may end
//!   `Failed(CrashLoop)` instead.
//! - **No reconnect storms.** A new run starts only after a deliberate call;
//!   every other attempt waits at least `RetryPolicy::bounds(n).0` after
//!   its Backoff; a Backoff keeps the attempt it came from, or is
//!   `Backoff{1}` after a stable Online; every connect follows its own
//!   published `Connecting`, so a bot never connects while Paused or
//!   Failed.
//! - **The API always answers,** at once and never with `TimedOut`. Besides
//!   the calls the steps make, every bot gets a `send_chat` probe every
//!   5 s, so calls also meet an actor that is ending after a crash, which
//!   random steps rarely do (found in P4.8).
//! - **The metrics agree:** the bots gauge with `snapshot_all`, and with 0
//!   after the shutdown; the reconnect counter with the published
//!   `Connecting`s.
//!
//! A fault counts for a bot only once it has landed: the event was queued,
//! the scripted answer used, or the panic fired. A call made in the same
//! instant as a crash of its bot may have been lost (ADR-0013), so it
//! doesn't bring the bot back into the healing class.
//!
//! It runs a fixed 500 cases, as an exception to `PROPTEST_CASES`, and
//! fails if any coverage target was never reached: a crash loop, a
//! duplicate-login pause, the fresh-token retry, a sticky re-add, a
//! teardown still running at a connect, a trip of each kind, and a probe
//! while a crashed session tears down.

use core::num::NonZeroU32;
use core::time::Duration;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use fleet_core::bot::{
    BotAccount, BotSnapshot, BotSpec, BotState, DesiredRunState, FailReason, StickyState,
};
use fleet_core::disconnect::{ConflictTexts, ConnectFailure, DisconnectReason};
use fleet_core::id::BotId;
use fleet_core::mc::SessionEvent;
use fleet_core::mode::{Action, ModeDefinition, Schedule, Step as ModeStep};
use fleet_core::resilience::RetryPolicy;
use fleet_runtime::{
    ChatTicket, Fleet, FleetError, FleetEvent, FleetEventKind, FleetParts, RuntimeConfig,
    SendChatError,
};
use fleet_testkit::mc::{EmitOutcome, FakeConnector, SessionController};
use proptest::collection::vec;
use proptest::prelude::{Just, Strategy, any};
use proptest::strategy::{BoxedStrategy, Union};
use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};
use tokio::sync::broadcast::{self, error::TryRecvError};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::credentials::{Answer, ScriptedCredentials};
use crate::harness::{BREAKER_COOLDOWN, anchor, circuit, message, mode, ms, n, policy, secs};
use crate::panicky::{CrashAt, PanickyConnector};
use crate::recorder::{Recorder, bots};

/// The number of cases, whatever `PROPTEST_CASES` says (ADR-0013).
const CASES: u32 = 500;

const BOT_IDS: [&str; 3] = [
    "018bcfe5-6800-7bab-abab-abababababab",
    "018bcfe5-6800-7cdc-8dcd-cdcdcdcdcdcd",
    "018bcfe5-6800-7efe-9efe-efefefefefef",
];
const NAMES: [&str; 3] = ["AfkBot1", "AfkBot2", "AfkBot3"];
const SERVERS: [&str; 2] = ["192.0.2.10", "example.com"];

/// A crash budget per bot and case: some cases reach the crash loop.
const MAX_CRASHES: usize = 8;
/// Crashes that can't trip the restart window (6 within 10 min).
const SAFE_CRASHES: usize = 5;
/// The longest slow teardown.
const MAX_TEARDOWN_SECS: u64 = 20;
/// How often every bot gets a probe, during the pauses and the settling.
const PROBE_EVERY: Duration = secs(5);

const SHUTDOWN_KEY: &str = "multiplayer.disconnect.server_shutdown";
const IDLING_KEY: &str = "multiplayer.disconnect.idling";
const BANNED_KEY: &str = "multiplayer.disconnect.banned";
const DUPLICATE_KEY: &str = "multiplayer.disconnect.duplicate_login";
const UNVERIFIED_KEY: &str = "multiplayer.disconnect.unverified_username";

const CRASH_LOOP: BotState = BotState::Failed {
    reason: FailReason::CrashLoop,
};

// --- The cases ---

/// One step of a case: something happens to one bot, then time passes.
#[derive(Debug, Clone, Copy)]
struct Step {
    /// The bot, modulo the case's bot count.
    bot: usize,
    op: Op,
    /// How long to wait afterwards, in seconds.
    pause: u64,
}

/// What can happen to a bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// A kick the bot recovers from, with this translation key (or none).
    TransientKick(Option<&'static str>),
    /// A kick that fails the bot until a Reset.
    PermanentKick,
    /// A human logged into the account: the bot pauses until a Resume.
    DuplicateLogin,
    /// The bot's next session fails to connect.
    ConnectFails(ConnectFailure),
    /// The bot's next connect finds no host thread.
    HostUnavailable,
    /// The bot's next session never joins; the test fails it after the
    /// connect timeout, as fleet-mc would.
    NeverJoins,
    /// The server rejects the bot's next session as unverified.
    AuthRejected,
    /// The bot's next session request gets this answer.
    Credentials(Answer),
    /// The bot's session stalls.
    Stall(Stall),
    /// The bot's teardowns take this long, from now on.
    SlowTeardown(u64),
    /// A panic in the bot's next connect, perform or teardown.
    Panic(CrashAt),
    /// The bot's next connects panic, and its session is kicked so they
    /// come one backoff apart.
    CrashBurst(usize),
    /// A changed spec, applied.
    Spec(SpecChange),
    Restart,
    Reset,
    Resume,
    SendChat,
    Snapshot,
    SnapshotAll,
    /// Removes the bot and adds it again, as the server moves a bot (P10.5).
    RemoveReadd,
}

/// What a spec update changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpecChange {
    /// `afk` and a cheap custom mode, in turn: no reconnect.
    Mode,
    /// Two placeholder servers, in turn: a deliberate new run.
    Server,
    /// Running and Stopped, in turn.
    Desired,
}

/// How a session stalls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stall {
    /// It hangs: no tick, no packet, and every call times out.
    Hang,
    /// The link dies: ticks go on, packets stop.
    Packets,
    /// It stops ticking.
    Ticks,
}

#[derive(Debug, Clone)]
struct Case {
    bots: usize,
    seed: u64,
    steps: Vec<Step>,
}

fn weighted(arms: Vec<(u32, BoxedStrategy<Op>)>) -> BoxedStrategy<Op> {
    Union::new_weighted(arms).boxed()
}

fn just(op: Op) -> BoxedStrategy<Op> {
    Just(op).boxed()
}

/// Every op, weighted so each coverage target is reached in several percent
/// of the cases.
fn op() -> BoxedStrategy<Op> {
    weighted(vec![
        (
            8,
            Union::new([Just(Some(SHUTDOWN_KEY)), Just(Some(IDLING_KEY)), Just(None)])
                .prop_map(Op::TransientKick)
                .boxed(),
        ),
        (2, just(Op::PermanentKick)),
        (3, just(Op::DuplicateLogin)),
        (
            4,
            Union::new([
                Just(ConnectFailure::Refused),
                Just(ConnectFailure::Unresolvable),
                Just(ConnectFailure::Other),
            ])
            .prop_map(Op::ConnectFails)
            .boxed(),
        ),
        (3, just(Op::HostUnavailable)),
        (2, just(Op::NeverJoins)),
        (3, just(Op::AuthRejected)),
        (3, just(Op::Credentials(Answer::Refuse { retryable: true }))),
        (
            1,
            just(Op::Credentials(Answer::Refuse { retryable: false })),
        ),
        (2, just(Op::Credentials(Answer::Never))),
        (3, just(Op::Stall(Stall::Hang))),
        (3, just(Op::Stall(Stall::Packets))),
        (2, just(Op::Stall(Stall::Ticks))),
        (
            6,
            (0..=MAX_TEARDOWN_SECS).prop_map(Op::SlowTeardown).boxed(),
        ),
        (3, just(Op::Panic(CrashAt::Connect))),
        (6, just(Op::Panic(CrashAt::Perform))),
        (3, just(Op::Panic(CrashAt::Teardown))),
        (3, (1..=MAX_CRASHES).prop_map(Op::CrashBurst).boxed()),
        (2, just(Op::Spec(SpecChange::Mode))),
        (3, just(Op::Spec(SpecChange::Server))),
        (3, just(Op::Spec(SpecChange::Desired))),
        (3, just(Op::Restart)),
        (3, just(Op::Reset)),
        (3, just(Op::Resume)),
        (2, just(Op::SendChat)),
        (1, just(Op::Snapshot)),
        (1, just(Op::SnapshotAll)),
        (3, just(Op::RemoveReadd)),
    ])
}

fn case() -> impl Strategy<Value = Case> {
    (
        1..=3_usize,
        any::<u64>(),
        vec((0..3_usize, op(), 0..=90_u64), 1..=30),
    )
        .prop_map(|(bots, seed, steps)| Case {
            bots,
            seed,
            steps: steps
                .into_iter()
                .map(|(bot, op, pause)| Step { bot, op, pause })
                .collect(),
        })
}

// --- What the test plays: the server side and the model ---

/// What a bot's next session does instead of joining.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Join {
    Fail(ConnectFailure),
    AuthReject,
    Never,
}

/// The test's server side, shared with its task: it joins every new
/// session, unless the bot's script says otherwise.
#[derive(Debug, Default)]
struct Server {
    /// What each bot's next sessions do instead of joining.
    next: BTreeMap<BotId, VecDeque<Join>>,
    /// Each bot's latest session.
    sessions: BTreeMap<BotId, SessionController>,
    /// How long each bot's teardowns take.
    delays: BTreeMap<BotId, Duration>,
    /// The bots whose session an auth rejection reached, in order.
    rejected: Vec<BotId>,
}

/// Runs the server side: each session that starts joins, or fails as its
/// bot's script says.
async fn serve(
    fake: FakeConnector,
    connector: PanickyConnector,
    server: Arc<Mutex<Server>>,
    connect_timeout: Duration,
) {
    let mut timers = JoinSet::new();
    let mut index = 0;
    loop {
        let controller = fake.session(index).await;
        index += 1;
        let Some(bot) = connector.bot_of_session(index - 1) else {
            continue;
        };
        let (join, delay) = {
            let mut server = server.lock().unwrap();
            server.sessions.insert(bot, controller.clone());
            let join = server.next.get_mut(&bot).and_then(VecDeque::pop_front);
            (join, server.delays.get(&bot).copied())
        };
        if let Some(delay) = delay {
            controller.delay_disconnect(delay);
        }
        match join {
            None => {
                let _ = controller.emit(SessionEvent::Joined);
            }
            Some(Join::Fail(failure)) => {
                let _ = controller.emit(SessionEvent::ConnectionFailed(failure));
            }
            Some(Join::AuthReject) => {
                let rejected = SessionEvent::Disconnected(DisconnectReason::kicked(
                    Some(UNVERIFIED_KEY),
                    "Failed to verify username",
                ));
                if controller.emit(rejected) == EmitOutcome::Queued {
                    server.lock().unwrap().rejected.push(bot);
                }
            }
            Some(Join::Never) => {
                timers.spawn(async move {
                    tokio::time::sleep(connect_timeout).await;
                    let _ =
                        controller.emit(SessionEvent::ConnectionFailed(ConnectFailure::TimedOut));
                });
            }
        }
    }
}

/// Why a bot left the class that must end Online.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exclusion {
    /// A duplicate login landed: until a Resume.
    Paused,
    /// A permanent kick, an auth rejection or refused credentials landed:
    /// until a Reset.
    Failed,
}

/// A Reset or Resume answered `Ok`, decided once its instant has passed.
#[derive(Debug, Clone, Copy)]
struct Recall {
    bot: usize,
    at: Instant,
    exclusion: Exclusion,
}

/// What the test knows about one bot.
#[derive(Debug)]
struct Bot {
    id: BotId,
    spec: BotSpec,
    exclusion: Option<Exclusion>,
    /// Its last published state; a new or re-added bot starts Stopped.
    last: BotState,
    /// Its last published state before its latest `Removed`, which the
    /// server stores and restores (P10.5).
    removed_from: Option<BotState>,
    /// Its last published Backoff, and when.
    backoff: Option<(NonZeroU32, chrono::DateTime<chrono::Utc>)>,
    /// Whether a deliberate call reached it since its last `Connecting`.
    deliberate: bool,
    /// Crashes scripted for it, at most `MAX_CRASHES`.
    scripted_crashes: usize,
    /// Whether it's being removed: from the `remove` until the re-add.
    removing: bool,
    /// Sessions the test stalled.
    stalls: Vec<SessionController>,
}

/// What the cases must reach, each in some of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Target {
    CrashLoop,
    DuplicateLoginPause,
    FreshTokenRetry,
    StickyReadd,
    TeardownAtConnect,
    TickTrip,
    PacketTrip,
    ProbeInCrashTeardown,
}

impl Target {
    const ALL: [Self; 8] = [
        Self::CrashLoop,
        Self::DuplicateLoginPause,
        Self::FreshTokenRetry,
        Self::StickyReadd,
        Self::TeardownAtConnect,
        Self::TickTrip,
        Self::PacketTrip,
        Self::ProbeInCrashTeardown,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::CrashLoop => "crash loop",
            Self::DuplicateLoginPause => "duplicate-login pause",
            Self::FreshTokenRetry => "fresh-token retry",
            Self::StickyReadd => "sticky re-add",
            Self::TeardownAtConnect => "teardown running at a connect",
            Self::TickTrip => "tick trip",
            Self::PacketTrip => "packet trip",
            Self::ProbeInCrashTeardown => "probe while a crashed session tears down",
        }
    }
}

/// The coverage targets over every case.
#[derive(Debug, Default)]
struct Tally {
    cases: u32,
    reached: BTreeMap<Target, u32>,
}

impl Tally {
    fn add(&mut self, reached: &BTreeSet<Target>) {
        self.cases += 1;
        for target in reached {
            *self.reached.entry(*target).or_default() += 1;
        }
    }

    fn count(&self, target: Target) -> u32 {
        self.reached.get(&target).copied().unwrap_or(0)
    }
}

// --- One case ---

/// A case's fleet, its fakes and the test's model of it.
struct Chaos<'r> {
    recorder: &'r Recorder,
    retry: RetryPolicy,
    config: RuntimeConfig,
    fleet: Fleet,
    connector: PanickyConnector,
    credentials: ScriptedCredentials,
    server: Arc<Mutex<Server>>,
    events: broadcast::Receiver<FleetEvent>,
    supervisor: JoinHandle<()>,
    serving: JoinHandle<()>,
    bots: Vec<Bot>,
    /// The scripted answers of the credentials already looked at.
    used_seen: usize,
    /// Resets and Resumes waiting for their instant to pass.
    recalls: Vec<Recall>,
    /// Published `Connecting`s with attempt > 1 or `auth_retried`.
    reconnects: u64,
    reached: BTreeSet<Target>,
}

/// A mode with a jump every 5 s and chat every 60 s, so mode changes,
/// mode chat and perform panics all happen, with few wakeups.
fn chaos_mode() -> ModeDefinition {
    mode(vec![
        ModeStep {
            action: Action::Jump,
            schedule: Schedule::Every {
                interval: secs(5),
                jitter: Duration::ZERO,
            },
            probability: 100,
        },
        ModeStep {
            action: Action::SendChat {
                message: message("hello"),
            },
            schedule: Schedule::Every {
                interval: secs(60),
                jitter: Duration::ZERO,
            },
            probability: 100,
        },
    ])
}

fn bot_spec(index: usize) -> BotSpec {
    BotSpec {
        id: BOT_IDS[index].parse().unwrap(),
        account: BotAccount::Offline(NAMES[index].parse().unwrap()),
        server: SERVERS[0].try_into().unwrap(),
        mode: ModeDefinition::afk(),
        desired: DesiredRunState::Running,
        conflict_texts: ConflictTexts::default(),
    }
}

/// Awaits a `Fleet` call and checks that it answered before the clock
/// moved: a supervisor that answers never lets paused time run on.
async fn at_once<T>(call: impl Future<Output = T>) -> T {
    let before = Instant::now();
    let answer = call.await;
    assert_eq!(Instant::now(), before, "a Fleet call took time to answer");
    answer
}

/// Checks that a call answered, neither timed out nor refused as shutting
/// down, and that it's one of `allowed`.
fn answered<T: core::fmt::Debug>(
    answer: Result<T, FleetError>,
    allowed: &[FleetError],
) -> Result<T, FleetError> {
    if let Err(error) = &answer {
        assert!(allowed.contains(error), "the Fleet answered {answer:?}");
    }
    answer
}

/// Checks a `send_chat` answer: anything but a timeout, a shutdown or an
/// unknown bot, since the bot may well be offline or rate-limited.
fn check_chat(answer: Result<ChatTicket, SendChatError>) {
    assert!(
        !matches!(
            answer,
            Err(SendChatError::Fleet(
                FleetError::TimedOut | FleetError::ShuttingDown | FleetError::UnknownBot
            ))
        ),
        "send_chat answered {answer:?}"
    );
}

/// The `state` label the bots gauge uses.
const fn label(state: BotState) -> &'static str {
    match state {
        BotState::Stopped => "stopped",
        BotState::AwaitingSession { .. } => "awaiting_session",
        BotState::Connecting { .. } => "connecting",
        BotState::Online { .. } => "online",
        BotState::Backoff { .. } => "backoff",
        BotState::Paused { .. } => "paused",
        BotState::Failed { .. } => "failed",
        BotState::Stopping { .. } => "stopping",
    }
}

/// A state the bot stays in on its own.
const fn is_final(state: BotState) -> bool {
    matches!(
        state,
        BotState::Stopped
            | BotState::Online { .. }
            | BotState::Paused { .. }
            | BotState::Failed { .. }
    )
}

impl<'r> Chaos<'r> {
    async fn start(case: &Case, recorder: &'r Recorder) -> Self {
        let config = RuntimeConfig {
            // Every event must reach the test: a `Lagged` fails the case.
            event_buffer: core::num::NonZeroUsize::new(65_536).unwrap(),
            ..RuntimeConfig::default()
        };
        let fake = FakeConnector::new();
        let connector = PanickyConnector::new(fake.clone());
        let credentials = ScriptedCredentials::new();
        let retry = policy();
        let (fleet, supervisor) = Fleet::new(FleetParts {
            connector: Arc::new(connector.clone()),
            credentials: Arc::new(credentials.clone()),
            retry,
            circuit: circuit(8),
            config,
            anchor: anchor(),
            seed: case.seed,
        })
        .unwrap();
        connector.observe(fleet.subscribe());
        let events = fleet.subscribe();
        let supervisor = tokio::spawn(supervisor.run(CancellationToken::new()));
        let server = Arc::new(Mutex::new(Server::default()));
        let serving = tokio::spawn(serve(
            fake,
            connector.clone(),
            Arc::clone(&server),
            config.connect_timeout,
        ));
        let mut chaos = Self {
            recorder,
            retry,
            config,
            fleet,
            connector,
            credentials,
            server,
            events,
            supervisor,
            serving,
            bots: Vec::new(),
            used_seen: 0,
            recalls: Vec::new(),
            reconnects: 0,
            reached: BTreeSet::new(),
        };
        for index in 0..case.bots {
            let spec = bot_spec(index);
            // An empty script, so the connector logs the bot's connects.
            chaos.connector.script(spec.id, |_| {});
            chaos.bots.push(Bot {
                id: spec.id,
                spec: spec.clone(),
                exclusion: None,
                last: BotState::Stopped,
                removed_from: None,
                backoff: None,
                deliberate: true,
                scripted_crashes: 0,
                removing: false,
                stalls: Vec::new(),
            });
            let answer = answered(at_once(chaos.fleet.apply(spec, None)).await, &[]);
            assert_eq!(answer, Ok(()));
        }
        chaos
    }

    // --- Watching the fleet ---

    /// Takes in what happened since the last call: the published events
    /// and the faults that landed.
    fn absorb(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(event) => self.observe(event),
                Err(TryRecvError::Lagged(missed)) => {
                    panic!("the test's subscriber lagged by {missed} events")
                }
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
        let rejected = core::mem::take(&mut self.server.lock().unwrap().rejected);
        for bot in rejected {
            self.exclude_id(bot, Exclusion::Failed);
        }
        let used = self.credentials.used();
        for used in &used[self.used_seen..] {
            if used.answer == (Answer::Refuse { retryable: false }) {
                self.exclude_id(used.bot, Exclusion::Failed);
            }
        }
        self.used_seen = used.len();
    }

    fn index_of(&self, id: BotId) -> Option<usize> {
        self.bots.iter().position(|bot| bot.id == id)
    }

    fn exclude_id(&mut self, id: BotId, exclusion: Exclusion) {
        if let Some(index) = self.index_of(id) {
            self.bots[index].exclusion = Some(exclusion);
        }
    }

    fn observe(&mut self, event: FleetEvent) {
        let Some(index) = self.index_of(event.bot_id) else {
            return;
        };
        match event.kind {
            FleetEventKind::StateChanged(snapshot) => self.changed(index, &snapshot),
            FleetEventKind::Removed => {
                // A re-added bot's snapshot starts Stopped again.
                let bot = &mut self.bots[index];
                bot.removed_from = Some(bot.last);
                bot.last = BotState::Stopped;
                bot.backoff = None;
            }
            _ => {}
        }
    }

    /// Checks one published state against the storm rules.
    fn changed(&mut self, index: usize, snapshot: &BotSnapshot) {
        let state = snapshot.state;
        let at = snapshot.since;
        let bot = &mut self.bots[index];
        let previous = bot.last;
        let id = bot.id;
        match state {
            BotState::AwaitingSession {
                attempt,
                fresh: true,
            } => {
                let rejected = BotState::Connecting {
                    attempt,
                    auth_retried: false,
                };
                assert_eq!(
                    previous, rejected,
                    "{id}: a fresh-token request must follow an auth rejection while Connecting"
                );
            }
            BotState::AwaitingSession { attempt, .. } if attempt.get() == 1 => {
                assert!(
                    bot.deliberate,
                    "{id}: a new run without a deliberate call, after {previous:?}"
                );
            }
            BotState::AwaitingSession { attempt, .. } => {
                let before = n(attempt.get() - 1);
                assert_eq!(
                    previous,
                    BotState::Backoff { attempt: before },
                    "{id}: attempt {attempt} must follow Backoff{{{before}}}"
                );
                let (backed_off, since) = bot.backoff.expect("a published Backoff");
                assert_eq!(backed_off, before);
                let waited = at.signed_duration_since(since).to_std().unwrap();
                let (lower, _) = self.retry.bounds(before);
                assert!(
                    waited >= lower,
                    "{id}: attempt {attempt} came {waited:?} after Backoff{{{before}}}, below {lower:?}"
                );
            }
            BotState::Connecting {
                attempt,
                auth_retried,
            } => {
                assert_eq!(
                    previous,
                    BotState::AwaitingSession {
                        attempt,
                        fresh: auth_retried
                    },
                    "{id}: Connecting must follow its own session request"
                );
                bot.deliberate = false;
                if attempt.get() > 1 || auth_retried {
                    self.reconnects += 1;
                }
                if auth_retried {
                    self.reached.insert(Target::FreshTokenRetry);
                }
            }
            BotState::Backoff { attempt } => {
                let kept = match previous {
                    BotState::AwaitingSession { attempt: from, .. }
                    | BotState::Connecting { attempt: from, .. }
                    | BotState::Online { attempt: from, .. } => from == attempt,
                    _ => false,
                };
                let stable = matches!(previous, BotState::Online { since, .. }
                    if attempt.get() == 1 && self.retry.is_stable(since, at));
                assert!(
                    kept || stable,
                    "{id}: Backoff{{{attempt}}} after {previous:?}"
                );
                bot.backoff = Some((attempt, at));
            }
            BotState::Paused { .. } => {
                self.reached.insert(Target::DuplicateLoginPause);
            }
            CRASH_LOOP => {
                self.reached.insert(Target::CrashLoop);
            }
            _ => {}
        }
        bot.last = state;
    }

    /// Decides the Resets and Resumes of the last step: one made in the
    /// same instant as a crash of its bot may have been lost.
    fn recall(&mut self) {
        let crashes = self.connector.crashes();
        for recall in core::mem::take(&mut self.recalls) {
            let bot = &mut self.bots[recall.bot];
            let lost = crashes
                .iter()
                .any(|crash| crash.bot == bot.id && crash.at == recall.at);
            if !lost && bot.exclusion == Some(recall.exclusion) {
                bot.exclusion = None;
            }
        }
    }

    fn check_connects(&self) {
        let violations = self.connector.violations();
        assert!(violations.is_empty(), "{violations:#?}");
    }

    /// The bot's latest session, if it hasn't ended.
    fn live(&self, index: usize) -> Option<SessionController> {
        self.server
            .lock()
            .unwrap()
            .sessions
            .get(&self.bots[index].id)
            .filter(|session| !session.is_torn_down())
            .cloned()
    }

    fn crashes_of(&self, index: usize) -> usize {
        let id = self.bots[index].id;
        self.connector
            .crashes()
            .iter()
            .filter(|crash| crash.bot == id)
            .count()
    }

    // --- Steps ---

    async fn step(&mut self, step: Step) {
        self.absorb();
        let index = step.bot % self.bots.len();
        self.apply(index, step.op).await;
        // At least a millisecond, so every task runs until it waits.
        let mut left = secs(step.pause).max(ms(1));
        while left > PROBE_EVERY {
            tokio::time::sleep(PROBE_EVERY).await;
            left -= PROBE_EVERY;
            self.probe().await;
        }
        tokio::time::sleep(left).await;
        self.recall();
        self.absorb();
        self.check_connects();
        self.probe().await;
    }

    /// Sends each bot one chat message, as a check that the API answers at
    /// once whatever the bot is doing: also while its actor ends after a
    /// crash, which a call at a random step rarely meets (found in P4.8).
    async fn probe(&mut self) {
        for index in 0..self.bots.len() {
            if self.bots[index].removing {
                continue;
            }
            let id = self.bots[index].id;
            if self.connector.in_crash_teardown(id) {
                self.reached.insert(Target::ProbeInCrashTeardown);
            }
            check_chat(at_once(self.fleet.send_chat(id, message("probe"))).await);
        }
    }

    async fn apply(&mut self, index: usize, op: Op) {
        let id = self.bots[index].id;
        match op {
            Op::Spec(change) => self.change_spec(index, change).await,
            Op::Restart => {
                let answer = answered(at_once(self.fleet.restart(id)).await, &[FleetError::Busy]);
                if answer.is_ok() {
                    self.bots[index].deliberate = true;
                }
            }
            Op::Reset => self.lifecycle(index, Exclusion::Failed).await,
            Op::Resume => self.lifecycle(index, Exclusion::Paused).await,
            Op::SendChat => check_chat(at_once(self.fleet.send_chat(id, message("hello"))).await),
            Op::Snapshot => {
                let answer = answered(at_once(self.fleet.snapshot(id)).await, &[]);
                assert!(answer.is_ok());
            }
            Op::SnapshotAll => {
                let answer = answered(at_once(self.fleet.snapshot_all()).await, &[]);
                assert_eq!(answer.map(|all| all.len()), Ok(self.bots.len()));
            }
            Op::RemoveReadd => self.move_bot(index).await,
            fault => self.fault(index, fault),
        }
    }

    /// Injects a fault; the calls are `apply`'s.
    fn fault(&mut self, index: usize, op: Op) {
        let id = self.bots[index].id;
        match op {
            Op::TransientKick(key) => {
                if let Some(session) = self.live(index) {
                    let kicked = DisconnectReason::kicked(key, "Kicked");
                    let _ = session.emit(SessionEvent::Disconnected(kicked));
                }
            }
            Op::PermanentKick => self.kick(index, BANNED_KEY, Exclusion::Failed),
            Op::DuplicateLogin => self.kick(index, DUPLICATE_KEY, Exclusion::Paused),
            Op::ConnectFails(failure) => self.join(id, Join::Fail(failure)),
            Op::NeverJoins => self.join(id, Join::Never),
            Op::AuthRejected => self.join(id, Join::AuthReject),
            Op::HostUnavailable => self.connector.script(id, |own| own.host_unavailable += 1),
            Op::Credentials(answer) => self.credentials.push(id, answer),
            Op::Stall(stall) => {
                if let Some(session) = self.live(index) {
                    match stall {
                        Stall::Hang => session.hang(),
                        Stall::Packets => session.freeze_packets(),
                        Stall::Ticks => session.freeze_ticks(),
                    }
                    self.bots[index].stalls.push(session);
                }
            }
            Op::SlowTeardown(seconds) => {
                self.server.lock().unwrap().delays.insert(id, secs(seconds));
                if let Some(session) = self.live(index) {
                    session.delay_disconnect(secs(seconds));
                }
            }
            Op::Panic(place) => self.panic(index, place),
            Op::CrashBurst(count) => self.crash_burst(index, count),
            Op::Spec(_)
            | Op::Restart
            | Op::Reset
            | Op::Resume
            | Op::SendChat
            | Op::Snapshot
            | Op::SnapshotAll
            | Op::RemoveReadd => {}
        }
    }

    /// Scripts up to `count` panics on the next connects of the bot, within
    /// its crash budget, and kicks its session so they come.
    fn crash_burst(&mut self, index: usize, count: usize) {
        let bot = &mut self.bots[index];
        let count = count.min(MAX_CRASHES - bot.scripted_crashes);
        if count == 0 {
            return;
        }
        bot.scripted_crashes += count;
        let id = bot.id;
        self.connector.script(id, |own| own.connect_panics += count);
        if let Some(session) = self.live(index) {
            let kicked = DisconnectReason::kicked(Some(SHUTDOWN_KEY), "Kicked");
            let _ = session.emit(SessionEvent::Disconnected(kicked));
        }
    }

    /// Changes one thing in the spec of the bot. A new server, or a bot that
    /// should run again, starts a deliberate new run.
    async fn change_spec(&mut self, index: usize, change: SpecChange) {
        let mut spec = self.bots[index].spec.clone();
        match change {
            SpecChange::Mode => {
                spec.mode = if spec.mode == ModeDefinition::afk() {
                    chaos_mode()
                } else {
                    ModeDefinition::afk()
                };
            }
            SpecChange::Server => {
                let other = if spec.server == SERVERS[0].try_into().unwrap() {
                    SERVERS[1]
                } else {
                    SERVERS[0]
                };
                spec.server = other.try_into().unwrap();
            }
            SpecChange::Desired => {
                spec.desired = match spec.desired {
                    DesiredRunState::Running => DesiredRunState::Stopped,
                    DesiredRunState::Stopped => DesiredRunState::Running,
                };
            }
        }
        let new_run = change != SpecChange::Mode;
        if self.update(index, spec).await.is_ok() && new_run {
            self.bots[index].deliberate = true;
        }
    }

    fn kick(&mut self, index: usize, key: &str, exclusion: Exclusion) {
        if let Some(session) = self.live(index) {
            let kicked = SessionEvent::Disconnected(DisconnectReason::kicked(Some(key), "Kicked"));
            if session.emit(kicked) == EmitOutcome::Queued {
                self.bots[index].exclusion = Some(exclusion);
            }
        }
    }

    fn join(&self, id: BotId, join: Join) {
        self.server
            .lock()
            .unwrap()
            .next
            .entry(id)
            .or_default()
            .push_back(join);
    }

    fn panic(&mut self, index: usize, place: CrashAt) {
        let id = self.bots[index].id;
        if self.bots[index].scripted_crashes >= MAX_CRASHES {
            return;
        }
        let pending = self.connector.pending(id);
        let added = match place {
            CrashAt::Connect => {
                self.connector.script(id, |own| own.connect_panics += 1);
                true
            }
            CrashAt::Perform if !pending.perform_panics => {
                self.connector.script(id, |own| own.perform_panics = true);
                true
            }
            CrashAt::Teardown if !pending.teardown_panics => {
                self.connector.script(id, |own| own.teardown_panics = true);
                true
            }
            CrashAt::Perform | CrashAt::Teardown => false,
        };
        if added {
            self.bots[index].scripted_crashes += 1;
        }
    }

    /// Applies `spec` to the bot; `Busy` is the only refusal allowed.
    async fn update(&mut self, index: usize, spec: BotSpec) -> Result<(), FleetError> {
        let answer = answered(
            at_once(self.fleet.apply(spec.clone(), None)).await,
            &[FleetError::Busy],
        );
        if answer.is_ok() {
            self.bots[index].spec = spec;
        }
        answer
    }

    /// A Reset (for a Failed bot) or a Resume (for a Paused one).
    async fn lifecycle(&mut self, index: usize, exclusion: Exclusion) {
        let id = self.bots[index].id;
        let at = Instant::now();
        let answer = match exclusion {
            Exclusion::Failed => at_once(self.fleet.reset(id)).await,
            Exclusion::Paused => at_once(self.fleet.resume(id)).await,
        };
        let answer = answered(answer, &[FleetError::Busy]);
        if answer.is_ok() {
            self.bots[index].deliberate = true;
            self.recalls.push(Recall {
                bot: index,
                at,
                exclusion,
            });
        }
    }

    /// Removes the bot and adds it again as the server moves a bot (P10.5):
    /// `apply` is retried on `Busy` until `Removed` has gone out, with the
    /// last published state as the restore when it's Paused or Failed.
    async fn move_bot(&mut self, index: usize) {
        let id = self.bots[index].id;
        self.bots[index].removed_from = None;
        let removed = answered(at_once(self.fleet.remove(id)).await, &[FleetError::Busy]);
        if removed.is_err() {
            return;
        }
        self.bots[index].removing = true;
        let started = Instant::now();
        loop {
            self.absorb();
            let bot = &self.bots[index];
            let restore = match bot.removed_from.unwrap_or(bot.last) {
                BotState::Paused { reason } => Some(StickyState::Paused(reason)),
                BotState::Failed { reason } => Some(StickyState::Failed(reason)),
                _ => None,
            };
            let spec = self.bots[index].spec.clone();
            let answer = answered(
                at_once(self.fleet.apply(spec, restore)).await,
                &[FleetError::Busy],
            );
            if answer.is_ok() {
                let bot = &mut self.bots[index];
                bot.exclusion = restore.map(|sticky| match sticky {
                    StickyState::Paused(_) => Exclusion::Paused,
                    StickyState::Failed(_) => Exclusion::Failed,
                });
                bot.deliberate = true;
                bot.removing = false;
                if restore.is_some() {
                    self.reached.insert(Target::StickyReadd);
                }
                return;
            }
            assert!(
                started.elapsed() < secs(MAX_TEARDOWN_SECS + 30),
                "{id} wasn't removed in time"
            );
            tokio::time::sleep(secs(1)).await;
        }
    }

    // --- Settling and the end ---

    /// The longest the fleet may take to settle: each fault still scripted
    /// may cost a backoff, a breaker cool-down and the timeouts.
    fn settle_cap(&self) -> Duration {
        let server = self.server.lock().unwrap();
        let pending = self
            .bots
            .iter()
            .map(|bot| {
                let own = self.connector.pending(bot.id);
                own.connect_panics
                    + own.host_unavailable
                    + self.credentials.pending(bot.id)
                    + server.next.get(&bot.id).map_or(0, VecDeque::len)
            })
            .max()
            .unwrap_or(0);
        let per_fault = self.retry.max()
            + BREAKER_COOLDOWN
            + self.config.session_request_timeout
            + self.config.connect_timeout
            + self.config.watchdog_timeout
            + secs(MAX_TEARDOWN_SECS);
        per_fault * u32::try_from(pending + 3).unwrap()
    }

    /// Whether every bot is in a state it stays in, with no stalled session
    /// left and no perform panic waiting on an Online bot.
    fn settled(&self, snapshots: &[BotSnapshot]) -> bool {
        snapshots.iter().all(|snapshot| {
            let Some(index) = self.index_of(snapshot.bot_id) else {
                return false;
            };
            let bot = &self.bots[index];
            is_final(snapshot.state)
                && bot.stalls.iter().all(SessionController::is_torn_down)
                && !(matches!(snapshot.state, BotState::Online { .. })
                    && self.connector.pending(bot.id).perform_panics)
        })
    }

    /// Stops injecting faults and waits until every bot has settled.
    async fn settle(&mut self) -> Vec<BotSnapshot> {
        let cap = self.settle_cap();
        let started = Instant::now();
        loop {
            self.recall();
            self.absorb();
            self.check_connects();
            self.probe().await;
            let snapshots = answered(at_once(self.fleet.snapshot_all()).await, &[]).unwrap();
            // A bot may have moved on while the probes and the snapshots
            // were awaited, e.g. a retry due in the instant the loop woke
            // up. The actor publishes each snapshot and its event in one
            // synchronous step, so absorbing now takes in every state the
            // snapshots show, before they're judged.
            self.absorb();
            if self.settled(&snapshots) {
                return snapshots;
            }
            assert!(
                started.elapsed() < cap,
                "the fleet didn't settle within {cap:?}: {snapshots:#?}"
            );
            tokio::time::sleep(PROBE_EVERY).await;
        }
    }

    /// Checks each bot's end state.
    fn check_end(&self, snapshots: &[BotSnapshot]) {
        for snapshot in snapshots {
            let index = self.index_of(snapshot.bot_id).unwrap();
            let bot = &self.bots[index];
            let crashes = self.crashes_of(index);
            let crash_looped = crashes > SAFE_CRASHES && snapshot.state == CRASH_LOOP;
            let fine = if bot.exclusion.is_some() {
                is_final(snapshot.state)
            } else {
                match bot.spec.desired {
                    DesiredRunState::Running => {
                        matches!(snapshot.state, BotState::Online { .. }) || crash_looped
                    }
                    DesiredRunState::Stopped => snapshot.state == BotState::Stopped || crash_looped,
                }
            };
            assert!(
                fine,
                "{}: ended {:?} with desired {:?}, {} crashes, exclusion {:?}",
                bot.id, snapshot.state, bot.spec.desired, crashes, bot.exclusion
            );
        }
    }

    /// Checks the bots gauge against the snapshots.
    fn check_gauge(&self, snapshots: &[BotSnapshot]) {
        let mut counted: BTreeMap<String, f64> = BTreeMap::new();
        for snapshot in snapshots {
            *counted.entry(label(snapshot.state).to_owned()).or_default() += 1.0;
        }
        assert_eq!(self.recorder.bots(), counted);
    }

    async fn finish(mut self) -> BTreeSet<Target> {
        let snapshots = self.settle().await;
        self.check_end(&snapshots);
        self.check_gauge(&snapshots);
        assert_eq!(self.recorder.reconnects(), self.reconnects);
        if self.recorder.trips("tick") > 0 {
            self.reached.insert(Target::TickTrip);
        }
        if self.recorder.trips("packet") > 0 {
            self.reached.insert(Target::PacketTrip);
        }
        if self
            .connector
            .log()
            .iter()
            .any(|attempt| attempt.teardown_running)
        {
            self.reached.insert(Target::TeardownAtConnect);
        }

        let timeout = self.config.shutdown_timeout;
        let started = Instant::now();
        let report = self.fleet.shutdown(timeout).await;
        assert!(report.is_ok(), "the shutdown answered {report:?}");
        assert!(started.elapsed() <= timeout + self.config.reply_timeout);
        self.serving.abort();
        let ended = tokio::time::timeout(secs(60), self.supervisor).await;
        assert!(
            matches!(ended, Ok(Ok(()))),
            "the supervisor task ended with {ended:?}"
        );
        assert_eq!(self.recorder.bots(), bots([]));
        assert_eq!(
            self.fleet.snapshot_all().await,
            Err(FleetError::ShuttingDown)
        );
        self.reached
    }
}

/// Runs one case on its own paused runtime, with its own recorder.
fn run_case(case: &Case) -> BTreeSet<Target> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap();
    let recorder = Recorder::default();
    let _metrics = recorder.install();
    runtime.block_on(async {
        let mut chaos = Chaos::start(case, &recorder).await;
        for step in &case.steps {
            chaos.step(*step).await;
        }
        chaos.finish().await
    })
}

/// The regression seeds proptest replays first; they don't count toward
/// the cases.
fn persisted_seeds() -> u32 {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fleet/chaos.proptest-regressions"
    );
    std::fs::read_to_string(path).map_or(0, |seeds| {
        u32::try_from(seeds.lines().filter(|line| line.starts_with("cc ")).count()).unwrap()
    })
}

#[test]
fn random_faults_and_calls_keep_every_fleet_invariant() {
    let config = Config {
        source_file: Some(file!()),
        // Next to this file, as `chaos.proptest-regressions`.
        failure_persistence: Some(Box::new(FileFailurePersistence::WithSource(
            "proptest-regressions",
        ))),
        ..Config::with_cases(CASES)
    };
    let mut runner = TestRunner::new(config);
    let tally = RefCell::new(Tally::default());

    let result = runner.run(&case(), |case| {
        let reached = run_case(&case);
        tally.borrow_mut().add(&reached);
        Ok(())
    });

    if let Err(error) = result {
        panic!("{error}");
    }
    let tally = tally.into_inner();
    println!("chaos: {} cases", tally.cases);
    for target in Target::ALL {
        println!("chaos: {} in {} cases", target.name(), tally.count(target));
    }
    assert_eq!(tally.cases, CASES + persisted_seeds());
    for target in Target::ALL {
        assert!(
            tally.count(target) > 0,
            "no case reached: {}",
            target.name()
        );
    }
}
