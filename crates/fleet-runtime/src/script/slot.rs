//! [`ScriptSlot`]: what of a bot's script outlives a session, and
//! [`ScriptPrograms`], the compiled scripts a fleet's bots share
//! (Plan.md P15.16; ADR-0036, D5, D7).
//!
//! A script's runner lives as long as one session. What must outlive it
//! belongs to the bot:
//!
//! - **The instance,** with the script's state: kept across reconnects,
//!   world changes, deaths and respawns; gone when the bot is stopped and
//!   when its mode is changed or saved. A script that stopped stays
//!   stopped until then.
//! - **The limiter,** which outlives everything: a stop, a mode change, an
//!   actor that crashed. No sequence of events gets around a minimum.
//! - **The chat watch:** the windows are cleared at every disconnect and
//!   stop, and when each any-time pattern last matched is kept, so a
//!   reconnect never shortens its second.
//!
//! The supervisor keeps a bot's slot, as it keeps its chat bucket, and
//! hands a clone to each of its actors.

use core::num::NonZeroUsize;
use core::time::Duration;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

use chrono::{DateTime, Utc};
use fleet_core::script::{ActionLimiter, ChatWatch, Script, Stopwatch, limits};
use fleet_core::time;
use fleet_script::{CompileError, Instance, Program};
use tokio::sync::{Semaphore, SemaphorePermit};

use super::event::ScriptStatus;

/// A script as it stands in a bot's slot.
#[derive(Debug)]
enum Standing {
    /// An instance: running, or stopped with its error.
    Instance(Box<Instance>),
    /// The script doesn't compile here.
    Refused(CompileError),
    /// A hook panicked.
    Crashed,
}

/// One start of a script: from the bot's Start, or its mode's change, until
/// its Stop or the next change.
#[derive(Debug)]
struct Run {
    /// The script it runs, to tell a changed mode.
    script: Script,
    standing: Standing,
    /// Whether the bot has joined since this start.
    joined: bool,
}

impl Run {
    fn status(&self) -> ScriptStatus {
        match &self.standing {
            Standing::Instance(instance) => {
                instance.error().map_or(ScriptStatus::Running, |error| {
                    ScriptStatus::Stopped(error.clone())
                })
            }
            Standing::Refused(error) => ScriptStatus::Refused(error.clone()),
            Standing::Crashed => ScriptStatus::Crashed,
        }
    }
}

/// When the runtime wrote the lines about what a script was refused, for
/// their rate: as many a minute as a script writes itself.
#[derive(Debug, Default)]
struct Notes {
    lines: Vec<DateTime<Utc>>,
}

impl Notes {
    /// How long a line counts.
    const WINDOW: Duration = Duration::from_secs(60);

    /// Whether a line may be written at `now`; if so, it's counted.
    fn take(&mut self, now: DateTime<Utc>) -> bool {
        self.lines
            .retain(|line| time::elapsed(*line, now) < Self::WINDOW);
        let room =
            u32::try_from(self.lines.len()).is_ok_and(|lines| lines < limits::LOG_LINES_PER_MINUTE);
        if room {
            self.lines.push(now);
        }
        room
    }
}

#[derive(Debug, Default)]
struct Slot {
    limiter: ActionLimiter,
    watch: ChatWatch,
    notes: Notes,
    run: Option<Run>,
}

/// What of a bot's script outlives a session (Plan.md P15.16; ADR-0036,
/// D5, D7). Clones share one slot.
///
/// - **The instance,** with the script's state: kept across reconnects,
///   world changes, deaths and respawns; gone when the bot is stopped and
///   when its mode is changed or saved. A script that stopped stays
///   stopped until then.
/// - **The limiter,** which outlives everything, an actor that crashed
///   included: no sequence of events gets around a minimum.
/// - **The chat watch:** its windows are cleared at every disconnect and
///   stop; when each any-time pattern last matched is kept.
///
/// The supervisor keeps a bot's slot, as it keeps its chat bucket, and
/// hands a clone to each of the bot's actors.
///
/// Nothing waits on it: every call is a short, synchronous step, and a hook
/// is the longest of them, bounded at 5 ms (ADR-0040).
#[derive(Debug, Clone, Default)]
pub struct ScriptSlot {
    inner: Arc<Mutex<Slot>>,
}

impl ScriptSlot {
    /// How long after a window's last moment the runner wakes for it.
    const AFTER_A_WINDOW: Duration = Duration::from_millis(1);

    /// A slot with no script, and a limiter that has seen nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the slot.
    ///
    /// **A poisoned lock means a hook panicked,** since a hook is the only
    /// thing that runs code of any size under it. That instance can't be
    /// trusted any more, so the script stands as crashed from here on: it
    /// runs nothing until the bot's next Start or its mode's next change,
    /// instead of crashing the bot's actor again at every join.
    fn lock(&self) -> MutexGuard<'_, Slot> {
        match self.inner.lock() {
            Ok(slot) => slot,
            Err(poisoned) => {
                let mut slot = poisoned.into_inner();
                if let Some(run) = &mut slot.run {
                    run.standing = Standing::Crashed;
                }
                slot.watch.clear();
                self.inner.clear_poison();
                slot
            }
        }
    }

    /// Where the bot's script is in its life; `None` for a bot whose
    /// script hasn't started since its last Stop or mode change, or that
    /// runs a step mode.
    #[must_use]
    pub fn status(&self) -> Option<ScriptStatus> {
        self.lock().run.as_ref().map(Run::status)
    }

    /// The status of the run of `script`; `None` if the slot holds none of
    /// this very script, which then needs a start.
    pub(crate) fn status_of(&self, script: &Script) -> Option<ScriptStatus> {
        self.lock()
            .run
            .as_ref()
            .filter(|run| run.script == *script)
            .map(Run::status)
    }

    /// Starts `script` anew: an instance of `program` with an empty state,
    /// or the reason it can't run. Whatever ran before is gone.
    pub(crate) fn start(
        &self,
        script: &Script,
        program: Result<Arc<Program>, CompileError>,
        seed: u64,
        stopwatch: Arc<dyn Stopwatch>,
    ) -> ScriptStatus {
        let standing = match program {
            Ok(program) => Standing::Instance(Box::new(Instance::new(program, seed, stopwatch))),
            Err(error) => Standing::Refused(error),
        };
        let run = Run {
            script: script.clone(),
            standing,
            joined: false,
        };
        let status = run.status();
        let mut slot = self.lock();
        slot.watch.clear();
        slot.run = Some(run);
        status
    }

    /// `script` can't run because the engine itself failed on it: it
    /// stands as crashed until the bot's next Start or its mode's next
    /// change.
    pub(crate) fn crashed(&self, script: &Script) -> ScriptStatus {
        let mut slot = self.lock();
        slot.watch.clear();
        slot.run = Some(Run {
            script: script.clone(),
            standing: Standing::Crashed,
            joined: false,
        });
        ScriptStatus::Crashed
    }

    /// Ends the script's run: its instance and its state are gone, and its
    /// windows with them. The limiter stays.
    pub(crate) fn end_run(&self) {
        let mut slot = self.lock();
        slot.watch.clear();
        slot.run = None;
    }

    /// Whether this join is the first since the script's start; from now
    /// on the bot has joined.
    pub(crate) fn join(&self) -> bool {
        self.lock()
            .run
            .as_mut()
            .is_some_and(|run| !core::mem::replace(&mut run.joined, true))
    }

    /// Runs `call` on the script's instance with the bot's limiter; `None`
    /// if no instance stands in the slot.
    ///
    /// The instance gets a copy of the limiter and the slot takes the copy
    /// back afterwards: a hook that panics then loses what it alone took,
    /// never the bot's limits.
    pub(crate) fn turn<R>(
        &self,
        call: impl FnOnce(&mut Instance, &mut ActionLimiter) -> R,
    ) -> Option<R> {
        let mut slot = self.lock();
        let Slot { limiter, run, .. } = &mut *slot;
        let Standing::Instance(instance) = &mut run.as_mut()?.standing else {
            return None;
        };
        let mut copy = limiter.clone();
        let result = call(instance, &mut copy);
        *limiter = copy;
        Some(result)
    }

    /// Reads or changes the bot's chat watch.
    pub(crate) fn watch<R>(&self, call: impl FnOnce(&mut ChatWatch) -> R) -> R {
        call(&mut self.lock().watch)
    }

    /// When the runner must wake next: for a timer or the end of a
    /// `hold_for` of the script's, or for a window that has run out.
    ///
    /// A window is still open at its last millisecond, so the wake for it
    /// is the millisecond after: a wake at its end would find it open,
    /// and ask for the same wake again without end.
    pub(crate) fn next_wake(&self) -> Option<DateTime<Utc>> {
        let slot = self.lock();
        let script = slot.run.as_ref().and_then(|run| match &run.standing {
            Standing::Instance(instance) => instance.next_wake(),
            Standing::Refused(_) | Standing::Crashed => None,
        });
        let window = slot
            .watch
            .next_expiry()
            .map(|until| time::add(until, Self::AFTER_A_WINDOW));
        [script, window].into_iter().flatten().min()
    }

    /// Whether the runtime may write a line about what the script was
    /// refused at `now`; if so, it's counted.
    pub(crate) fn may_note(&self, now: DateTime<Utc>) -> bool {
        self.lock().notes.take(now)
    }
}

/// The compiled scripts a fleet's bots share: two hundred bots that run one
/// mode hold one program. Clones share one list.
///
/// A program stays for as long as an instance holds it and no longer: the
/// list holds none itself.
#[derive(Debug, Clone, Default)]
pub struct ScriptPrograms {
    known: Arc<Mutex<Vec<Weak<Program>>>>,
}

impl ScriptPrograms {
    /// A list that knows no program.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the list. Nothing here can leave it half-changed, so a
    /// poisoned lock is used as it is.
    fn lock(&self) -> MutexGuard<'_, Vec<Weak<Program>>> {
        self.known
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The program of `script`, if a bot of the fleet holds it.
    pub(crate) fn get(&self, script: &Script) -> Option<Arc<Program>> {
        self.lock()
            .iter()
            .filter_map(Weak::upgrade)
            .find(|program| program.script() == script)
    }

    /// Takes `program` into the list and returns the one to use: an equal
    /// one another bot put there meanwhile, or this one.
    pub(crate) fn share(&self, program: Program) -> Arc<Program> {
        let mut known = self.lock();
        known.retain(|program| program.strong_count() > 0);
        let same = known
            .iter()
            .filter_map(Weak::upgrade)
            .find(|other| other.script() == program.script());
        same.unwrap_or_else(|| {
            let program = Arc::new(program);
            known.push(Arc::downgrade(&program));
            program
        })
    }

    /// How many programs the fleet's bots hold right now.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock()
            .iter()
            .filter(|program| program.strong_count() > 0)
            .count()
    }

    /// Whether no bot holds a program.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// How many hooks of a fleet's scripts run at the same time: one fewer
/// than the runtime has async workers, and at least one (ADR-0043). Clones
/// share one turnstile.
///
/// A hook holds its worker for up to 5 ms. With every worker inside a
/// hook, whatever else is due on the runtime waits: the agent's stream
/// and its heartbeat, every step-mode bot, the supervisor. So a hook
/// takes a turn first. A script whose turn hasn't come isn't runnable,
/// which keeps the workers' queues short however many scripts are due:
/// what else is due waits for one hook at most, not for one of every
/// script.
///
/// The turns are handed out in the order they were asked for.
#[derive(Debug, Clone, Default)]
pub struct ScriptTurns {
    /// Made at the first turn, inside the runtime whose workers it counts.
    turns: Arc<OnceLock<Semaphore>>,
    /// A fixed number of turns instead of the runtime's, if one was set.
    at_once: Option<NonZeroUsize>,
}

impl ScriptTurns {
    /// A turnstile for the runtime its first turn is taken on.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A turnstile that lets `at_once` hooks run at the same time,
    /// whatever the runtime.
    #[must_use]
    pub fn of(at_once: NonZeroUsize) -> Self {
        Self {
            turns: Arc::default(),
            at_once: Some(at_once),
        }
    }

    /// How many hooks may run at once on a runtime of `workers` async
    /// workers: one fewer, so one worker is always free for what isn't a
    /// hook; one on a runtime with a single worker, where the hooks then
    /// run one after the other with everything else in between.
    const fn spare(workers: usize) -> usize {
        let spare = workers.saturating_sub(1);
        if spare == 0 { 1 } else { spare }
    }

    /// [`spare`](Self::spare) for the runtime this is called on.
    fn here() -> usize {
        Self::spare(
            tokio::runtime::Handle::try_current()
                .map_or(1, |runtime| runtime.metrics().num_workers()),
        )
    }

    /// Waits for a turn. The hook runs while the turn is held, and
    /// nothing is awaited meanwhile.
    pub(crate) async fn take(&self) -> Turn<'_> {
        let turns = self.turns.get_or_init(|| {
            let at_once = self.at_once.map_or_else(Self::here, NonZeroUsize::get);
            // More than tokio counts is as good as no bound.
            Semaphore::new(at_once.min(Semaphore::MAX_PERMITS))
        });
        // The turnstile is never closed, so a turn always comes; without
        // one the hook runs all the same.
        Turn {
            _permit: turns.acquire().await.ok(),
        }
    }
}

/// A hook's turn, given back when it's dropped.
#[derive(Debug)]
pub(crate) struct Turn<'a> {
    _permit: Option<SemaphorePermit<'a>>,
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use fleet_core::mode::HotbarSlot;
    use fleet_core::script::{
        BlockPosition, Blocks, BotFacts, HookEvent, Ident, Identifier, Input, Key, Limited,
        Position, Press, ScriptDraft, SendId, Snapshot,
    };
    use fleet_script::{Moment, Outcome, Presence, compile};
    use fleet_testkit::stopwatch::ManualStopwatch;

    use super::*;

    fn script(source: &str) -> Script {
        Script::try_from(ScriptDraft {
            api: 1,
            source: source.to_owned(),
            leash: 8,
            tool_guard: 2,
            block_guard: Vec::new(),
            messages: Vec::new(),
            patterns: Vec::new(),
            signs: Vec::new(),
        })
        .unwrap()
    }

    fn program(script: &Script) -> Result<Arc<Program>, CompileError> {
        compile(script).map(Arc::new)
    }

    fn still() -> Arc<dyn Stopwatch> {
        Arc::new(ManualStopwatch::standing())
    }

    fn at(ms: u64) -> DateTime<Utc> {
        time::add(
            DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            Duration::from_millis(ms),
        )
    }

    fn facts() -> Snapshot {
        let bot = BotFacts {
            health: 20.0,
            food: 20,
            position: Position::try_new(0.5, 64.0, 0.5).unwrap(),
            yaw: 0.0,
            pitch: 0.0,
            dimension: Identifier::try_from("overworld").unwrap(),
            riding: None,
            held: None,
            selected: HotbarSlot::FIRST,
            looking_at: None,
        };
        let blocks = Blocks::read(BlockPosition { x: 0, y: 64, z: 0 }, |_| None);
        Snapshot::new(bot, Vec::new(), blocks)
    }

    /// Runs `on_join` of the slot's script at `ms`.
    fn join(slot: &ScriptSlot, ms: u64) -> Option<Outcome> {
        let facts = facts();
        let menu = fleet_core::mc::MenuView::default();
        let moment = Moment {
            facts: &facts,
            now: at(ms),
            presence: Presence::InWorld,
            leash: None,
            menu: &menu,
            name: "AfkBot1",
        };
        slot.turn(|instance, limiter| {
            instance
                .handle(&HookEvent::Join { first: true }, &moment, limiter)
                .outcome
        })
    }

    const JUMPS: &str = "fn on_join(first) { keys.jump.tap(); }";
    const FAILS: &str = "fn on_join(first) { keys.jump.hodl(); }";

    #[test]
    fn an_empty_slot_has_no_script_and_runs_nothing() {
        let slot = ScriptSlot::new();

        assert_eq!(slot.status(), None);
        assert_eq!(slot.status_of(&script(JUMPS)), None);
        assert_eq!(join(&slot, 0), None);
        assert_eq!(slot.next_wake(), None);
        assert!(!slot.join());
    }

    #[test]
    fn a_started_script_runs_until_the_run_ends() {
        let slot = ScriptSlot::new();
        let jumps = script(JUMPS);

        let started = slot.start(&jumps, program(&jumps), 7, still());
        let ran = join(&slot, 0);
        slot.end_run();

        assert_eq!(started, ScriptStatus::Running);
        assert!(matches!(ran, Some(Outcome::Ran { .. })), "{ran:?}");
        assert_eq!(slot.status(), None);
        assert_eq!(join(&slot, 1_000), None);
    }

    #[test]
    fn the_slot_holds_one_script_and_tells_another_apart() {
        let slot = ScriptSlot::new();
        let jumps = script(JUMPS);
        slot.start(&jumps, program(&jumps), 7, still());

        assert_eq!(slot.status_of(&jumps), Some(ScriptStatus::Running));
        assert_eq!(slot.status_of(&script(FAILS)), None);
    }

    #[test]
    fn a_script_that_stopped_stays_stopped_with_its_error() {
        let slot = ScriptSlot::new();
        let fails = script(FAILS);
        slot.start(&fails, program(&fails), 7, still());

        let first = join(&slot, 0);
        let again = join(&slot, 5_000);

        assert!(matches!(first, Some(Outcome::Stopped(_))), "{first:?}");
        assert_eq!(again, Some(Outcome::Over));
        let Some(ScriptStatus::Stopped(error)) = slot.status() else {
            panic!("the script stands as stopped");
        };
        assert_eq!(error.at.map(|at| at.line), Some(1));
        assert_eq!(slot.status_of(&fails), slot.status());
    }

    #[test]
    fn a_script_that_doesnt_compile_stands_as_refused_and_runs_nothing() {
        let slot = ScriptSlot::new();
        let broken = script("fn on_join(first) { let keys = 1; }");

        let started = slot.start(&broken, program(&broken), 7, still());

        assert!(matches!(started, ScriptStatus::Refused(_)), "{started:?}");
        assert_eq!(slot.status(), Some(started));
        assert_eq!(join(&slot, 0), None);
    }

    #[test]
    fn only_the_first_join_since_a_start_is_the_first() {
        let slot = ScriptSlot::new();
        let jumps = script(JUMPS);
        slot.start(&jumps, program(&jumps), 7, still());

        let joins = [slot.join(), slot.join(), slot.join()];
        slot.start(&jumps, program(&jumps), 7, still());

        assert_eq!(joins, [true, false, false]);
        assert!(slot.join(), "a new start is a new first join");
    }

    /// The limiter is the bot's: a new start of the script, and a new
    /// script, find the jump taken half a second ago.
    #[test]
    fn the_limiter_outlives_the_run() {
        let slot = ScriptSlot::new();
        let jumps = script(JUMPS);
        slot.start(&jumps, program(&jumps), 7, still());
        join(&slot, 0);
        slot.end_run();
        slot.start(&jumps, program(&jumps), 8, still());

        let too_soon = slot.turn(|_, limiter| {
            limiter
                .check(
                    Limited::of_press(Input::Key(Key::Jump), Press::Tap),
                    at(100),
                )
                .is_err()
        });

        assert_eq!(too_soon, Some(true));
    }

    #[test]
    fn a_new_start_and_an_end_close_every_window() {
        let slot = ScriptSlot::new();
        let jumps = script(JUMPS);
        let open = |slot: &ScriptSlot| {
            slot.watch(|watch| {
                watch
                    .expect(
                        SendId(1),
                        Ident::try_from("balance").unwrap(),
                        Duration::from_secs(5),
                    )
                    .unwrap();
                watch.sent(SendId(1), at(0));
            });
        };

        open(&slot);
        let opened = slot.next_wake();
        slot.start(&jumps, program(&jumps), 7, still());
        let after_a_start = slot.watch(|watch| watch.is_idle());
        open(&slot);
        slot.end_run();

        assert_eq!(opened, Some(at(5_001)));
        assert!(after_a_start);
        assert!(slot.watch(|watch| watch.is_idle()));
    }

    #[test]
    fn the_next_wake_is_the_scripts_or_a_windows_whichever_is_first() {
        let slot = ScriptSlot::new();
        let timed = script("fn on_join(first) { timer.after(2000, \"t\"); }");
        slot.start(&timed, program(&timed), 7, still());
        join(&slot, 0);
        let timer_only = slot.next_wake();

        slot.watch(|watch| {
            watch
                .expect(
                    SendId(1),
                    Ident::try_from("balance").unwrap(),
                    Duration::from_millis(500),
                )
                .unwrap();
            watch.sent(SendId(1), at(0));
        });

        assert_eq!(timer_only, Some(at(2_000)));
        // The window is open for all of its 500 ms: the wake that finds it
        // run out is the millisecond after.
        assert_eq!(slot.next_wake(), Some(at(501)));
        let still_open = slot.watch(|watch| watch.expired(at(500)));
        let run_out = slot.watch(|watch| watch.expired(at(501)));
        assert_eq!((still_open.len(), run_out.len()), (0, 1));
        assert_eq!(slot.next_wake(), Some(at(2_000)));
    }

    /// A hook that panics is a bug of ours. The script stands as crashed
    /// from then on, and the bot's limits are what they were.
    #[test]
    fn a_hook_that_panics_leaves_the_script_crashed_and_the_limiter_whole() {
        let slot = ScriptSlot::new();
        let jumps = script(JUMPS);
        slot.start(&jumps, program(&jumps), 7, still());
        join(&slot, 0);

        let panicked = catch_unwind(AssertUnwindSafe(|| {
            slot.turn(|_, limiter| {
                // What the instance does while a hook runs: it takes the
                // limiter and hands it back at the end.
                drop(core::mem::take(limiter));
                panic!("a test panic inside a hook");
            })
        }));

        assert!(panicked.is_err());
        assert_eq!(slot.status(), Some(ScriptStatus::Crashed));
        assert_eq!(slot.status_of(&jumps), Some(ScriptStatus::Crashed));
        assert_eq!(join(&slot, 1_000), None);
        assert_eq!(slot.next_wake(), None);
        // A new start runs again, and still finds the jump of before.
        slot.start(&jumps, program(&jumps), 9, still());
        let too_soon = slot.turn(|_, limiter| {
            limiter
                .check(
                    Limited::of_press(Input::Key(Key::Jump), Press::Tap),
                    at(100),
                )
                .is_err()
        });
        assert_eq!(too_soon, Some(true));
    }

    #[test]
    fn the_runtimes_notes_are_as_many_a_minute_as_a_scripts_own_lines() {
        let slot = ScriptSlot::new();

        let taken = (0..25).filter(|_| slot.may_note(at(0))).count();
        let a_minute_later = slot.may_note(at(60_000));

        assert_eq!(taken, 20);
        assert!(a_minute_later);
    }

    #[test]
    fn bots_that_run_one_script_share_one_program() {
        let programs = ScriptPrograms::new();
        let jumps = script(JUMPS);
        assert!(programs.get(&jumps).is_none() && programs.is_empty());

        let first = programs.share(compile(&jumps).unwrap());
        let second = programs.share(compile(&jumps).unwrap());
        let other = programs.share(compile(&script(FAILS)).unwrap());

        assert!(Arc::ptr_eq(&first, &second));
        assert!(!Arc::ptr_eq(&first, &other));
        assert!(Arc::ptr_eq(&programs.get(&jumps).unwrap(), &first));
        assert_eq!(programs.len(), 2);
    }

    #[test]
    fn a_program_no_bot_holds_is_forgotten() {
        let programs = ScriptPrograms::new();
        let jumps = script(JUMPS);

        let held = programs.share(compile(&jumps).unwrap());
        drop(held);

        assert!(programs.get(&jumps).is_none());
        assert!(programs.is_empty());
        // The next one that's shared sweeps the dead entry out.
        let _again = programs.share(compile(&jumps).unwrap());
        assert_eq!(programs.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn no_more_hooks_take_a_turn_at_once_than_the_turnstile_lets() {
        let turns = ScriptTurns::of(NonZeroUsize::new(2).unwrap());

        let first = turns.take().await;
        let _second = turns.take().await;
        let third = tokio::time::timeout(Duration::from_secs(1), turns.take()).await;
        drop(first);
        let then = tokio::time::timeout(Duration::from_secs(1), turns.take()).await;

        assert!(third.is_err(), "two turns are out");
        assert!(then.is_ok(), "a turn that was given back is taken");
    }

    #[tokio::test(start_paused = true)]
    async fn the_turns_come_in_the_order_they_were_asked_for() {
        let turns = ScriptTurns::of(NonZeroUsize::MIN);
        let held = turns.take().await;
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut waiting = tokio::task::JoinSet::new();
        for n in 0..3 {
            let (turns, order) = (turns.clone(), Arc::clone(&order));
            waiting.spawn(async move {
                let _turn = turns.take().await;
                order.lock().unwrap().push(n);
            });
            // Each asks before the next one does.
            tokio::task::yield_now().await;
        }

        drop(held);
        while waiting.join_next().await.is_some() {}

        assert_eq!(*order.lock().unwrap(), [0, 1, 2]);
    }

    /// A test's runtime has one worker: one hook at a time.
    #[tokio::test(start_paused = true)]
    async fn a_runtime_with_one_worker_runs_one_hook_at_a_time() {
        let turns = ScriptTurns::new();

        let _first = turns.take().await;
        let second = tokio::time::timeout(Duration::from_secs(1), turns.take()).await;

        assert!(second.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn a_turnstile_set_to_the_largest_number_holds_no_hook_back() {
        let turns = ScriptTurns::of(NonZeroUsize::MAX);

        let mut held = Vec::new();
        for _ in 0..1_000 {
            held.push(turns.take().await);
        }

        assert_eq!(held.len(), 1_000);
    }

    /// One worker stays free for what isn't a hook; a single worker runs
    /// one hook at a time.
    #[test]
    fn one_fewer_hooks_run_at_once_than_the_runtime_has_workers() {
        let at_once: Vec<usize> = [0, 1, 2, 3, 4, 8, 64]
            .into_iter()
            .map(ScriptTurns::spare)
            .collect();

        assert_eq!(at_once, [1, 1, 1, 2, 3, 7, 63]);
    }
}
