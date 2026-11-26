//! P15.19's load test (Plan.md P15.19, ADR-0043): 200 scripted bots in one
//! fleet, on async runtimes of one, two and four workers.
//!
//! **What it's for.** A script's hook runs on the async worker its bot's
//! runner happens to be on: up to 5 ms a hook, 20 ms a second for one
//! script (ADR-0040's amendment). The agent's stream to the server, its
//! heartbeat, every step-mode bot and every other script share those
//! workers. This test measures what a fleet of scripts does to them, with
//! the reference's examples alone and with a rising share of the bots on a
//! script that spends as much of its time as a script can.
//!
//! **What runs.** fleet-runtime's own `Fleet`, as the agent builds it: the
//! supervisor, an actor a bot, the mode runner with its script runner,
//! fleet-script's instances timed by the OS's clock. The bots' sessions are
//! fleet-testkit's fake, so what's measured is the runtime and the engine,
//! not Minecraft. A scripted mode can't reach a managed agent yet (the wire
//! carries none before Plan.md P15.20), so the agent's stream itself isn't
//! in it; the heartbeat here is what that stream's is to the runtime, a
//! task with a timer, and it asks the supervisor what the agent's own
//! heartbeat asks it ([`Fleet::snapshot_all`]).
//!
//! **What it measures,** in each phase, over [`MEASURE`]:
//! - how late that heartbeat's timer fires, and how long the supervisor
//!   takes to answer it;
//! - how late a step-mode bot's step comes (ten bots that jump every
//!   250 ms, the shortest interval a step has);
//! - how late a timer's hook begins, and how long a hook takes, read from
//!   the runner's own `debug` line a hook;
//! - the process's CPU time and memory, and how many scripts stopped.
//!
//! **Each runtime is measured twice:** with as many hooks at once as are
//! due, which is how the runner was first built, and with the hooks taking
//! turns, as it is now ([`fleet_runtime::ScriptTurns`]).
//!
//! It asserts only what must hold at any load: every bot stays online, and
//! no example ever stops.

use core::future::Future;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use fleet_agent::stopwatch::OsStopwatch;
use fleet_core::bot::{BotAccount, BotSpec, BotState, DesiredRunState};
use fleet_core::chat::{ChatFormats, ChatMessage, ChatRate, IncomingChat};
use fleet_core::disconnect::ConflictTexts;
use fleet_core::id::BotId;
use fleet_core::mc::{
    ConnectError, ConnectParams, Guards, Liveness, MinecraftConnector, Observation, SessionError,
    SessionEvent, SessionHandle,
};
use fleet_core::mode::{Action, GameAction, HotbarSlot, ModeDefinition, ModeDraft, Schedule, Step};
use fleet_core::resilience::{CircuitPolicy, RetryPolicy};
use fleet_core::script::{
    ActionLimiter, BlockPosition, Blocks, BotFacts, EntityFact, HeldItem, HookEvent, Identifier,
    LookingAt, Position, Script, ScriptDraft, Snapshot,
};
use fleet_runtime::{
    Fleet, FleetEventKind, FleetParts, OfflineCredentials, RuntimeConfig, ScriptEvent,
};
use fleet_script::{EXAMPLES, Instance, Moment, Outcome, Presence, compile};
use fleet_testkit::mc::{EmitOutcome, FakeConnector, FakeEvents, FakeSession, SessionController};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata};

use super::{Meter, percentile};

/// The scripted bots: fifty of each of the reference's four examples.
const SCRIPTED: usize = 200;
/// The step-mode bots, whose steps are timed.
const STEPPERS: usize = 10;
/// How often a stepper jumps: a step's shortest interval.
const STEP: Duration = Duration::from_millis(250);
/// The share of the scripted bots on the hot script in each phase, in
/// percent.
const SHARES: [usize; 5] = [0, 10, 25, 50, 100];
/// The async workers of each run's runtime.
const WORKERS: [usize; 3] = [1, 2, 4];
/// How long a phase's bots get to settle before it's measured.
const SETTLE: Duration = Duration::from_secs(2);
/// How long a phase is measured.
const MEASURE: Duration = Duration::from_secs(6);
/// How often the heartbeat beats.
const BEAT: Duration = Duration::from_millis(250);
/// How long one hook of the hot script should take when it runs alone:
/// under a hook's 5 ms with room for what running side by side adds
/// (three hooks at once took a third longer each, and at 3.4 ms alone
/// every one of them crossed the 5 ms and was stopped), and four of them
/// a second, or five in any second and a bit, as the second's budget is
/// judged, stay inside the second's 20 ms. It's about what a script can
/// spend of its time and still run on a busy agent.
const HOT_HOOK: Duration = Duration::from_micros(2_800);

/// The hot script: four hooks a second, each `rounds` times an array's
/// method that calls a function of the script's for thirty items, the
/// costliest kind of operation the engine has (ADR-0040's amendment, A1).
pub(super) fn hot(rounds: u64) -> ScriptDraft {
    let source = format!(
        "fn on_join(first) {{\n    timer.every(250, \"burn\");\n}}\n\nfn on_timer(name) {{\n    let items = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30];\n    let n = 0;\n    while n < {rounds} {{\n        items.map(Fn(\"same\"));\n        n += 1;\n    }}\n}}\n\nfn same(item) {{\n    item\n}}\n"
    );
    ScriptDraft {
        api: 1,
        source,
        leash: 8,
        tool_guard: 2,
        block_guard: Vec::new(),
        messages: Vec::new(),
        patterns: Vec::new(),
    }
}

/// How long one hook of the hot script with `rounds` takes here, timed on
/// an instance of its own with the OS's clock: the median of five timer
/// hooks, a quarter second of the script's time apart. `None` if the
/// script stopped, which it does past a hook's 5 ms.
fn hot_hook(rounds: u64) -> Option<Duration> {
    let script = Script::try_from(hot(rounds)).expect("a scripted mode's body");
    let program = Arc::new(compile(&script).expect("the hot script compiles"));
    let mut instance = Instance::new(program, 1, Arc::new(OsStopwatch::start()));
    let facts = seen().facts;
    let mut limiter = ActionLimiter::new();
    let moment = |quarter: u32| Moment {
        facts: &facts,
        now: anchor() + STEP * quarter,
        presence: Presence::InWorld,
        leash: None,
    };
    instance.handle(&HookEvent::Join { first: true }, &moment(0), &mut limiter);
    let mut took = Vec::new();
    for quarter in 1..=5 {
        let begun = Instant::now();
        let ran = instance.wake(&moment(quarter), &mut limiter);
        took.push(begun.elapsed());
        if !matches!(ran.outcome, Outcome::Ran { operations } if operations > 0) {
            return None;
        }
    }
    Some(percentile(&sorted(took), 50))
}

/// Finds the rounds that make the hot script's hook take [`HOT_HOOK`] on
/// this machine, in this build: a few rounds are timed, and a rule of
/// three brings them to the time, four times over. Rounds whose hook is
/// stopped are halved.
pub(super) fn calibrate() -> u64 {
    let mut rounds = 4_u64;
    let mut took = Duration::ZERO;
    for _ in 0..6 {
        match hot_hook(rounds) {
            Some(measured) => {
                took = measured;
                let wanted =
                    u128::from(rounds) * HOT_HOOK.as_micros() / measured.as_micros().max(1);
                rounds = u64::try_from(wanted).unwrap_or(rounds).clamp(1, 4_000);
            }
            None => rounds = (rounds / 2).max(1),
        }
    }
    // The last rounds as they really are, a little under the time for
    // sure.
    loop {
        match hot_hook(rounds) {
            Some(measured) if measured <= HOT_HOOK => {
                took = measured;
                break;
            }
            _ if rounds == 1 => break,
            _ => rounds = rounds * 19 / 20,
        }
    }
    println!(
        "LOAD scripted, the hot script: {rounds} rounds a hook take {took:.1?} here, alone; four hooks a second are {:.1?} of a second's 20 ms",
        took * 4
    );
    rounds
}

fn scripted(draft: ScriptDraft) -> ModeDefinition {
    ModeDefinition::scripted(Script::try_from(draft).expect("a scripted mode's body"))
}

/// The example bot `n` runs: fifty bots each.
fn example(n: usize) -> ModeDefinition {
    let example = EXAMPLES
        .get(n % EXAMPLES.len())
        .expect("an example for every bot");
    scripted(example.draft())
}

/// A mode that jumps every [`STEP`].
fn stepping() -> ModeDefinition {
    ModeDraft {
        steps: vec![Step {
            action: Action::Jump,
            schedule: Schedule::Every {
                interval: STEP,
                jitter: Duration::ZERO,
            },
            probability: 100,
        }],
    }
    .validate()
    .expect("a step mode")
}

fn anchor() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).expect("a time")
}

/// The ID of bot `n`.
fn bot(n: usize) -> BotId {
    let mut random = [0_u8; 10];
    let bytes = u64::try_from(n).expect("a small number").to_be_bytes();
    for (place, byte) in random.iter_mut().rev().zip(bytes.iter().rev()) {
        *place = *byte;
    }
    BotId::new_v7(anchor(), random).expect("a bot's ID")
}

fn spec(n: usize, mode: ModeDefinition) -> BotSpec {
    BotSpec {
        id: bot(n),
        account: BotAccount::Offline(format!("AfkBot{n}").parse().expect("a username")),
        server: "localhost".try_into().expect("a server"),
        mode,
        desired: DesiredRunState::Running,
        conflict_texts: ConflictTexts::default(),
        chat_rate: ChatRate::default(),
        chat_formats: ChatFormats::default(),
    }
}

/// What a bot's session shows it: a farm as each example expects one, with
/// a pickaxe in hand, twenty entities around and cobblestone in front.
pub(super) fn seen() -> Observation {
    let place = Position::try_new(0.5, 64.0, 0.5).expect("a position");
    let id = |name: &str| Identifier::try_from(name).expect("an identifier");
    let bot = BotFacts {
        health: 20.0,
        food: 20,
        position: place,
        yaw: 0.0,
        pitch: 20.0,
        dimension: id("overworld"),
        riding: Some(id("minecart")),
        held: Some(HeldItem {
            id: id("iron_pickaxe"),
            count: 1,
            durability: Some(200),
        }),
        selected: HotbarSlot::FIRST,
        looking_at: Some(LookingAt::Block {
            id: id("cobblestone"),
            at: BlockPosition { x: 2, y: 64, z: 0 },
        }),
    };
    let entities = (0..20_u8).map(|n| EntityFact {
        kind: id(if n % 4 == 0 { "minecart" } else { "zombie" }),
        position: Position::try_new(1.5 + f64::from(n) / 2.0, 64.0, 0.5).expect("a position"),
        occupied: false,
    });
    let stone = id("stone");
    let blocks = Blocks::read(BlockPosition { x: 0, y: 64, z: 0 }, |at| {
        (at.y < 64).then(|| stone.clone())
    });
    Observation {
        facts: Snapshot::new(bot, entities, blocks),
        anchor: Some(place),
        menu: fleet_core::mc::MenuView::default(),
    }
}

// --- The session, with its steps timed ---

/// When each stepper's jumps reached its session.
type Jumps = Arc<Mutex<HashMap<BotId, Vec<Instant>>>>;

fn lock<T>(shared: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The fake connector, with every jump's arrival noted.
#[derive(Debug, Clone)]
struct Timed {
    fake: FakeConnector,
    jumps: Jumps,
}

impl MinecraftConnector for Timed {
    type Session = TimedSession;
    type Events = FakeEvents;

    fn connect(
        &self,
        params: ConnectParams,
    ) -> impl Future<Output = Result<(Self::Session, Self::Events), ConnectError>> + Send {
        let bot = params.bot_id;
        let fake = self.fake.clone();
        let jumps = Arc::clone(&self.jumps);
        async move {
            let (session, events) = fake.connect(params).await?;
            Ok((
                TimedSession {
                    session,
                    bot,
                    jumps,
                },
                events,
            ))
        }
    }
}

#[derive(Debug, Clone)]
struct TimedSession {
    session: FakeSession,
    bot: BotId,
    jumps: Jumps,
}

impl SessionHandle for TimedSession {
    fn perform(&self, action: GameAction) -> impl Future<Output = Result<(), SessionError>> + Send {
        if action == GameAction::Jump {
            lock(&self.jumps)
                .entry(self.bot)
                .or_default()
                .push(Instant::now());
        }
        self.session.perform(action)
    }

    fn send_chat(
        &self,
        message: ChatMessage,
    ) -> impl Future<Output = Result<(), SessionError>> + Send {
        self.session.send_chat(message)
    }

    fn observe(&self) -> impl Future<Output = Result<Observation, SessionError>> + Send {
        self.session.observe()
    }

    fn set_guards(&self, guards: Guards) -> impl Future<Output = Result<(), SessionError>> + Send {
        self.session.set_guards(guards)
    }

    fn respawn(&self) -> impl Future<Output = Result<(), SessionError>> + Send {
        self.session.respawn()
    }

    fn menu(
        &self,
        command: fleet_core::mc::MenuCommand,
    ) -> impl Future<Output = Result<(), fleet_core::mc::MenuError>> + Send {
        self.session.menu(command)
    }

    fn disconnect(&self) -> impl Future<Output = ()> + Send {
        self.session.disconnect()
    }

    fn liveness(&self) -> Liveness {
        self.session.liveness()
    }
}

// --- The runner's own line a hook ---

/// One hook, as the runner's `debug` line says it ran.
#[derive(Debug, Clone, Copy, Default)]
struct Hooked {
    /// Whether it was a timer's hook.
    timer: bool,
    took: Duration,
    /// How late it began, for a timer's hook.
    late: Duration,
    /// How long it waited for its turn.
    waited: Duration,
    /// The operations it ran.
    operations: u64,
}

/// Collects the runner's line a hook from every thread it's set on, and
/// nothing else.
#[derive(Debug, Clone, Default)]
struct Hooks {
    seen: Arc<Mutex<Vec<Hooked>>>,
}

impl Hooks {
    /// The hooks since the last call.
    fn take(&self) -> Vec<Hooked> {
        core::mem::take(&mut *lock(&self.seen))
    }
}

impl Visit for Hooked {
    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "took_us" => self.took = Duration::from_micros(value),
            "late_us" => self.late = Duration::from_micros(value),
            "waited_us" => self.waited = Duration::from_micros(value),
            "operations" => self.operations = value,
            _ => {}
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "hook" {
            self.timer = value == "on_timer";
        }
    }

    fn record_debug(&mut self, _: &Field, _: &dyn core::fmt::Debug) {}
}

impl tracing::Subscriber for Hooks {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.is_event()
            && *metadata.level() == Level::DEBUG
            && metadata.fields().field("took_us").is_some()
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut hooked = Hooked::default();
        event.record(&mut hooked);
        lock(&self.seen).push(hooked);
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

// --- The heartbeat ---

/// One beat: how late its timer fired, and how long the supervisor took to
/// answer.
type Beats = Arc<Mutex<Vec<(Duration, Duration)>>>;

/// Beats every [`BEAT`] until `stop`: a timer, then the question the
/// agent's own heartbeat asks the fleet.
async fn beat(fleet: Fleet, beats: Beats, unanswered: Arc<AtomicUsize>, stop: CancellationToken) {
    let mut due = tokio::time::Instant::now() + BEAT;
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep_until(due) => {}
        }
        let now = tokio::time::Instant::now();
        let late = now.saturating_duration_since(due);
        if fleet.snapshot_all().await.is_err() {
            unanswered.fetch_add(1, Ordering::Relaxed);
        }
        lock(&beats).push((late, now.elapsed()));
        // A late beat isn't made up for.
        due = (due + BEAT).max(tokio::time::Instant::now());
    }
}

// --- The run ---

/// The scripts that stopped: how many for each reason, and the first
/// few errors as they read.
#[derive(Debug, Default)]
struct Stopped {
    counts: HashMap<&'static str, usize>,
    examples: Vec<String>,
}

/// What a run holds between its phases.
struct Run {
    fleet: Fleet,
    fake: FakeConnector,
    /// Each bot's session, by its place among the connects.
    sessions: Vec<SessionController>,
    jumps: Jumps,
    hooks: Hooks,
    beats: Beats,
    unanswered: Arc<AtomicUsize>,
    /// How many scripts stopped since the run began, by why, and why the
    /// first few did, in their own words.
    stopped: Arc<Mutex<Stopped>>,
    /// Whether an example stopped: none ever may.
    feeding: Arc<AtomicBool>,
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let waited = tokio::time::timeout(Duration::from_mins(2), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(waited.is_ok(), "{what}: not within two minutes");
}

fn sorted(mut times: Vec<Duration>) -> Vec<Duration> {
    times.sort_unstable();
    times
}

fn spread(what: &str, times: Vec<Duration>) -> String {
    let times = sorted(times);
    format!(
        "{what} p50 {:.1?}, p95 {:.1?}, max {:.1?} ({} of them)",
        percentile(&times, 50),
        percentile(&times, 95),
        times.last().copied().unwrap_or_default(),
        times.len(),
    )
}

impl Run {
    /// Builds the fleet on the current runtime and brings every bot
    /// online: the scripted ones on their examples, the steppers jumping.
    /// `at_once` is how many hooks may run at the same time; `None` is
    /// the runtime's own rule.
    async fn start(
        hooks: Hooks,
        at_once: Option<core::num::NonZeroUsize>,
        stop: &CancellationToken,
    ) -> Self {
        let fake = FakeConnector::new();
        let jumps = Jumps::default();
        let (fleet, supervisor) = Fleet::new(FleetParts {
            connector: Arc::new(Timed {
                fake: fake.clone(),
                jumps: Arc::clone(&jumps),
            }),
            credentials: Arc::new(OfflineCredentials),
            retry: RetryPolicy::try_new(
                Duration::from_secs(5),
                Duration::from_mins(5),
                Duration::from_mins(5),
            )
            .expect("a retry policy"),
            circuit: CircuitPolicy::try_new(
                core::num::NonZeroUsize::new(8).expect("eight"),
                Duration::from_mins(10),
                Duration::from_mins(15),
            )
            .expect("a circuit policy"),
            config: RuntimeConfig {
                max_bots: core::num::NonZeroUsize::new(SCRIPTED + STEPPERS).expect("the bots"),
                script_hooks_at_once: at_once,
                ..RuntimeConfig::default()
            },
            anchor: anchor(),
            seed: 1,
            stopwatch: Arc::new(OsStopwatch::start()),
        })
        .expect("a fleet");
        tokio::spawn(supervisor.run(stop.child_token()));

        // Every script's status, counted by why it stopped.
        let stopped: Arc<Mutex<Stopped>> = Arc::default();
        let mut events = fleet.subscribe();
        let counts = Arc::clone(&stopped);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        if let FleetEventKind::Script(ScriptEvent::Status(status)) = &event.kind
                            && let Some(why) = status.stop_label()
                        {
                            let mut stopped = lock(&counts);
                            *stopped.counts.entry(why).or_default() += 1;
                            if stopped.examples.len() < 3 {
                                stopped.examples.push(format!("{status:?}"));
                            }
                        }
                    }
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return,
                }
            }
        });

        for n in 0..SCRIPTED {
            fleet
                .apply(spec(n, example(n)), None)
                .await
                .expect("a bot the fleet takes");
        }
        for n in SCRIPTED..SCRIPTED + STEPPERS {
            fleet
                .apply(spec(n, stepping()), None)
                .await
                .expect("a bot the fleet takes");
        }
        let bots = SCRIPTED + STEPPERS;
        until("every bot's session", || fake.session_count() == bots).await;
        let mut sessions = Vec::with_capacity(bots);
        for index in 0..bots {
            let session = fake.session(index).await;
            session.show(seen());
            assert_eq!(session.emit(SessionEvent::Joined), EmitOutcome::Queued);
            sessions.push(session);
        }

        let beats = Beats::default();
        let unanswered = Arc::new(AtomicUsize::new(0));
        tokio::spawn(beat(
            fleet.clone(),
            Arc::clone(&beats),
            Arc::clone(&unanswered),
            stop.child_token(),
        ));
        let run = Self {
            fleet,
            fake,
            sessions,
            jumps,
            hooks,
            beats,
            unanswered,
            stopped,
            feeding: Arc::new(AtomicBool::new(true)),
        };
        run.all_online().await;
        run.feed(stop.child_token());
        run
    }

    /// Waits until every bot is online.
    async fn all_online(&self) {
        let online = Arc::new(AtomicUsize::new(0));
        let waited = tokio::time::timeout(Duration::from_mins(2), async {
            loop {
                let states = self.fleet.snapshot_all().await.unwrap_or_default();
                let count = states
                    .iter()
                    .filter(|bot| matches!(bot.state, BotState::Online { .. }))
                    .count();
                online.store(count, Ordering::Relaxed);
                if count == SCRIPTED + STEPPERS {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(
            waited.is_ok(),
            "{} of {} bots online within two minutes",
            online.load(Ordering::Relaxed),
            SCRIPTED + STEPPERS
        );
    }

    /// The session of bot `n`: the fake numbers its sessions by connect.
    fn session(&self, n: usize) -> Option<&SessionController> {
        let connects = self.fake.connects();
        let place = connects.iter().position(|params| params.bot_id == bot(n))?;
        self.sessions.get(place)
    }

    /// The server talks: three lines a second to every bot, one of them
    /// the reply the balance example reads inside its window. Only a bot
    /// whose script declares patterns is handed them.
    fn feed(&self, stop: CancellationToken) {
        let sessions = self.sessions.clone();
        let feeding = Arc::clone(&self.feeding);
        tokio::spawn(async move {
            let mut second = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    () = stop.cancelled() => return,
                    _ = second.tick() => {}
                }
                if !feeding.load(Ordering::Relaxed) {
                    continue;
                }
                for session in &sessions {
                    for line in [
                        "AfkBot7 joined the game",
                        "Balance: $1,250.50",
                        "[Server] Hello",
                    ] {
                        // A full queue drops chat, as fleet-mc's does.
                        let _ = session.emit(SessionEvent::Chat(IncomingChat::system(line)));
                    }
                }
            }
        });
    }

    /// Puts bot `n` on `mode`.
    async fn mode(&self, n: usize, mode: ModeDefinition) {
        self.fleet
            .apply(spec(n, mode), None)
            .await
            .expect("a mode the fleet takes");
    }

    /// Forgets what was measured so far.
    fn reset(&self) {
        let _ = self.hooks.take();
        lock(&self.beats).clear();
        lock(&self.jumps).clear();
        self.unanswered.store(0, Ordering::Relaxed);
    }

    /// How late each stepper's steps came: what lay between two jumps
    /// beyond their interval.
    fn steps_late(&self) -> Vec<Duration> {
        lock(&self.jumps)
            .values()
            .flat_map(|jumps| {
                jumps
                    .windows(2)
                    .filter_map(|pair| match pair {
                        [before, after] => Some(after.duration_since(*before).saturating_sub(STEP)),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Measures one phase and prints its report line.
    async fn measure(&self, turns: &str, workers: usize, share: usize, meter: &mut Meter) {
        let workers = format!("{workers} workers, {turns},");
        tokio::time::sleep(SETTLE).await;
        self.reset();
        let (cpu_before, _) = meter.read();
        let begun = Instant::now();
        tokio::time::sleep(MEASURE).await;
        let wall = begun.elapsed();
        let (cpu_after, memory) = meter.read();

        let hooks = self.hooks.take();
        let beats = lock(&self.beats).clone();
        let timers: Vec<Hooked> = hooks.iter().copied().filter(|hook| hook.timer).collect();
        let cpu = cpu_after.saturating_sub(cpu_before);
        let stopped = format!("{:?}", *lock(&self.stopped));
        println!(
            "LOAD scripted, {workers} {share} % hot: {}; {}",
            spread(
                "heartbeat late",
                beats.iter().map(|(late, _)| *late).collect()
            ),
            spread(
                "the supervisor's answer",
                beats.iter().map(|(_, took)| *took).collect()
            ),
        );
        println!(
            "LOAD scripted, {workers} {share} % hot: {}",
            spread("a step late", self.steps_late()),
        );
        println!(
            "LOAD scripted, {workers} {share} % hot: {}; {}",
            spread(
                "a timer's hook late",
                timers.iter().map(|hook| hook.late).collect()
            ),
            spread("a hook took", hooks.iter().map(|hook| hook.took).collect()),
        );
        println!(
            "LOAD scripted, {workers} {share} % hot: {}; the most operations in a hook: {}",
            spread(
                "a hook waited for its turn",
                hooks.iter().map(|hook| hook.waited).collect()
            ),
            hooks.iter().map(|hook| hook.operations).max().unwrap_or(0),
        );
        let hook_time: Duration = hooks.iter().map(|hook| hook.took).sum();
        println!(
            "LOAD scripted, {workers} {share} % hot: {} hooks/s, {:.0} ms of hooks a second; {:.0} % of one core; {} MiB held; {} beats unanswered; scripts stopped so far: {stopped}",
            // Whole hooks a second: a count over the phase's milliseconds.
            u128::try_from(hooks.len()).unwrap_or(u128::MAX) * 1000 / wall.as_millis().max(1),
            1000.0 * hook_time.as_secs_f64() / wall.as_secs_f64(),
            100.0 * cpu.as_secs_f64() / wall.as_secs_f64().max(0.001),
            memory / (1024 * 1024),
            self.unanswered.load(Ordering::Relaxed),
        );
    }
}

/// One run: a fleet on a runtime of `workers` async workers, through every
/// share of hot scripts. With `turns`, hooks take turns as the runtime
/// has it; without, as many run at once as are due, which is how the
/// runner was first built.
async fn run(workers: usize, turns: bool, rounds: u64, hooks: Hooks) {
    let stop = CancellationToken::new();
    let mut meter = Meter::start();
    let begun = Instant::now();
    let (at_once, turns) = if turns {
        (None, "hooks take turns")
    } else {
        (Some(core::num::NonZeroUsize::MAX), "no turns")
    };
    let run = Run::start(hooks, at_once, &stop).await;
    println!(
        "LOAD scripted, {workers} workers, {turns}: {SCRIPTED} scripted bots and {STEPPERS} step-mode bots online after {:.1?}",
        begun.elapsed()
    );
    let mut hot_bots = 0;
    for share in SHARES {
        let wanted = SCRIPTED * share / 100;
        for n in hot_bots..wanted {
            run.mode(n, scripted(hot(rounds))).await;
        }
        hot_bots = wanted;
        run.measure(turns, workers, share, &mut meter).await;
        // An example never stops, whatever runs beside it. A hot script
        // may: it's written to the edge of its budget.
        if share == 0 {
            let stopped = lock(&run.stopped);
            assert!(
                stopped.counts.is_empty(),
                "an example stopped: {:?}",
                *stopped
            );
        }
        let states = run.fleet.snapshot_all().await.unwrap_or_default();
        let online = states
            .iter()
            .filter(|bot| matches!(bot.state, BotState::Online { .. }))
            .count();
        assert_eq!(
            online,
            SCRIPTED + STEPPERS,
            "{share} % hot: every bot is online"
        );
    }
    assert!(run.session(0).is_some());

    stop.cancel();
    let report = run.fleet.shutdown(Duration::from_secs(10)).await;
    println!(
        "LOAD scripted, {workers} workers, {turns}: shut down, {}",
        if report.is_ok() {
            "in time"
        } else {
            "by its token"
        }
    );
}

#[test]
fn load_200_scripted_bots() {
    let rounds = calibrate();
    for (workers, turns) in WORKERS
        .into_iter()
        .flat_map(|workers| [(workers, false), (workers, true)])
    {
        let hooks = Hooks::default();
        let on_thread = hooks.clone();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .enable_all()
            // The runner's line a hook is read on every worker, and
            // nothing else of the log.
            .on_thread_start(move || {
                let guard = tracing::subscriber::set_default(on_thread.clone());
                // For the thread's whole life.
                core::mem::forget(guard);
            })
            .build()
            .expect("a runtime");
        runtime.block_on(run(workers, turns, rounds, hooks));
        drop(runtime);
    }
}
