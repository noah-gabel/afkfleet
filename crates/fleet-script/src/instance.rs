//! [`Instance`]: one bot's running script (P15.7; ADR-0036, D1, D3, D5,
//! D6).

use core::fmt;
use core::time::Duration;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};

use chrono::{DateTime, Utc};
use fleet_core::mc::MenuView;
use fleet_core::script::menu::ScreenKind;
use fleet_core::script::{
    ActionLimiter, Captured, ClickOutcome, Hook, HookEvent, Input, Leash, Refusal, Snapshot,
    Stopwatch, TimerName, limits,
};
use rhai::{CallFnOptions, Dynamic, Engine, EvalAltResult, FLOAT, Map, Position, Scope};

use crate::action::{Action, Refused, note};
use crate::api;
use crate::budget::SecondBudget;
use crate::compile::{Program, message, position};
use crate::engine::{self, STATE};
use crate::error::{Limit, Misuse, ScriptError, ScriptErrorKind};
use crate::state::{self, Checked, Fault};
use crate::turn::{SharedTurn, Turn, lock};

/// Whether a bot can act right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Presence {
    /// It's in a world and alive.
    InWorld,
    /// It's dead: it can't act until it's back, and its timers wait.
    Dead,
    /// It's between worlds, or its session is ending.
    NotInWorld,
}

/// The moment a hook runs at: what it sees, and what the limits below it
/// are judged by.
///
/// The instance holds no clock and reads no world: its caller says what
/// time it is, hands it a snapshot taken right before the hook, and says
/// whether the bot can act.
#[derive(Debug, Clone, Copy)]
pub struct Moment<'a> {
    /// What the hook sees of the game. It doesn't change while the hook
    /// runs; what the hook's own actions change shows in the next one.
    pub facts: &'a Snapshot,
    /// The time. A hook's timers and holds count from it.
    pub now: DateTime<Utc>,
    /// Whether the bot can act.
    pub presence: Presence,
    /// The bot's leash, if its anchor is known: beyond it, a direction key
    /// that would lead further is refused when it's pressed. The session
    /// below holds the bot to it every tick, with or without this.
    pub leash: Option<Leash>,
    /// The bot's inventory, the screen it has open and the sign editor
    /// waiting for an answer, taken with the facts (API level 2).
    pub menu: &'a MenuView,
    /// The bot's own player name, as its account has it: `bot.name` (API
    /// level 2; ADR-0036, M12).
    pub name: &'a str,
}

/// How a call on an instance ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The hook ran to its end; a wake ran one timer that was due, or
    /// found none.
    Ran {
        /// How many operations it ran, of a hook's and a second's budget.
        operations: u64,
    },
    /// The script defines no such hook. What the event lets go of was let
    /// go of all the same.
    NotDefined,
    /// The script failed or hit a limit, now. Everything is released, and
    /// it runs nothing more: not at the next join either, only as a new
    /// instance.
    Stopped(ScriptError),
    /// The script ended in an orderly way, after its `on_stop`. An error
    /// in `on_stop` is reported and changes nothing.
    Ended {
        /// What went wrong in `on_stop`, if something did.
        error: Option<ScriptError>,
    },
    /// The script had stopped or ended before: nothing ran.
    Over,
}

/// What a call on an instance did.
#[derive(Debug, Clone, PartialEq)]
pub struct Ran {
    /// What the script asked for, in order, to be performed now.
    pub actions: Vec<Action>,
    /// When the instance wants [`Instance::wake`] called next: a timer is
    /// due, or a `hold_for` ends. Never before this call's moment. `None`
    /// when nothing is waiting.
    pub next_wake: Option<DateTime<Utc>>,
    /// How it ended.
    pub outcome: Outcome,
    /// What the script asked for in this call and wasn't given: each kind
    /// and reason once, with how often. The script got every one as a
    /// result; this is for its log and the runtime's counters.
    pub refused: Vec<Refused>,
}

/// Where an instance is in its life.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Running,
    Stopped(ScriptError),
    Ended,
}

/// How much of its thread's stack a hook may take: half of what a thread
/// of the async runtime has.
///
/// Rhai's call limit and nesting limit bound the stack too, but by a
/// number of levels, and a level's size depends on the build: measured,
/// the deepest script those limits allow takes 0.3 MiB in a release build
/// and 1.8 MiB in a debug build (ADR-0040). A stack that ran out would
/// end the process and every bot of the agent with it, so the instance
/// measures instead of counting.
const STACK_BUDGET: usize = 1024 * 1024;

/// What the instance's own guards end a hook for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// The state isn't a small map of plain data, or was used while a call
    /// on it ran.
    State(Fault),
    /// The hook took more of the stack than it may.
    Stack,
    /// The hook ran longer than it may.
    Time,
}

impl Ended {
    /// [`Ended::Stack`] as a number; a state's fault is its own, from 1.
    const STACK: u8 = 200;
    /// [`Ended::Time`] as a number.
    const TIME: u8 = 201;

    /// The reason as a number that's never 0, to keep in an atomic.
    fn code(self) -> u8 {
        match self {
            Self::State(fault) => fault.code(),
            Self::Stack => Self::STACK,
            Self::Time => Self::TIME,
        }
    }

    /// The reason `code` stands for; `None` for 0, which is "none".
    fn from_code(code: u8) -> Option<Self> {
        match code {
            Self::STACK => Some(Self::Stack),
            Self::TIME => Some(Self::Time),
            _ => Fault::from_code(code).map(Self::State),
        }
    }

    /// The reason as a script's error. `time_by_second` says whether the
    /// hook's time was cut to what was left of the second.
    const fn kind(self, time_by_second: bool) -> ScriptErrorKind {
        match self {
            Self::State(Fault::NotAMap) => ScriptErrorKind::StateNotAMap,
            Self::State(Fault::NotPlain) => ScriptErrorKind::StateNotPlain,
            Self::State(Fault::Entries) => ScriptErrorKind::Limit(Limit::StateEntries),
            Self::State(Fault::Bytes) => ScriptErrorKind::Limit(Limit::StateBytes),
            Self::State(Fault::InUse) => ScriptErrorKind::StateInUse,
            Self::Stack => ScriptErrorKind::Limit(Limit::Stack),
            Self::Time if time_by_second => ScriptErrorKind::Limit(Limit::TimePerSecond),
            Self::Time => ScriptErrorKind::Limit(Limit::HookTime),
        }
    }
}

/// What an instance's engine and the instance share about the hook that's
/// running.
#[derive(Debug)]
struct Running {
    /// The operations the running hook has run.
    operations: AtomicU64,
    state: state::Cell,
    /// What ended the running hook, as [`Ended::code`]; 0 for nothing.
    ///
    /// **Once set, every further operation ends the hook again.** Rhai's
    /// array functions wrap whatever error a function they call raises,
    /// and a script can catch the wrapped one; it can't catch its way past
    /// this, because the next thing it does ends it once more.
    ended: AtomicU8,
    /// Where the stack was when the running hook started.
    stack_top: AtomicUsize,
    /// How much of the stack a hook may take.
    stack_budget: usize,
    /// What the running hook's time is read from.
    stopwatch: Arc<dyn Stopwatch>,
    /// The stopwatch's last reading, in nanoseconds.
    time_read: AtomicU64,
    /// The time the running hook has taken so far, in nanoseconds.
    time_taken: AtomicU64,
    /// The time the running hook may take, in nanoseconds.
    time_allowed: AtomicU64,
}

/// `time` in nanoseconds; the longest a number holds, 584 years, for more.
fn nanos(time: Duration) -> u64 {
    u64::try_from(time.as_nanos()).unwrap_or(u64::MAX)
}

impl Running {
    /// Ends the running hook for `ended`, now and at every operation
    /// after: the token Rhai's progress callback returns to stop.
    fn end(&self, ended: Ended) -> Dynamic {
        self.ended.store(ended.code(), Ordering::Relaxed);
        Dynamic::from(ended)
    }

    /// What ended the running hook, if something did.
    fn ended_for(&self) -> Option<Ended> {
        Ended::from_code(self.ended.load(Ordering::Relaxed))
    }

    /// Starts the running hook's time: it may take `allowed`.
    fn start_time(&self, allowed: Duration) {
        self.time_read
            .store(nanos(self.stopwatch.elapsed()), Ordering::Relaxed);
        self.time_taken.store(0, Ordering::Relaxed);
        self.time_allowed.store(nanos(allowed), Ordering::Relaxed);
    }

    /// Reads the stopwatch and counts the time since its last reading
    /// towards the running hook's, at most [`limits::LONGEST_STRETCH`] of
    /// it: a longer gap is the host's, not the script's. A stopwatch that
    /// went back counts nothing. Returns whether the hook has now run
    /// longer than it may.
    fn count_time(&self) -> bool {
        let now = nanos(self.stopwatch.elapsed());
        let last = self.time_read.swap(now, Ordering::Relaxed);
        let stretch = now.saturating_sub(last).min(nanos(limits::LONGEST_STRETCH));
        let taken = self
            .time_taken
            .load(Ordering::Relaxed)
            .saturating_add(stretch);
        self.time_taken.store(taken, Ordering::Relaxed);
        taken > self.time_allowed.load(Ordering::Relaxed)
    }

    /// The time the running hook has taken, as counted.
    fn time_taken(&self) -> Duration {
        Duration::from_nanos(self.time_taken.load(Ordering::Relaxed))
    }

    /// Notes where the stack is as the hook starts.
    fn mark_stack(&self) {
        let here = 0_u8;
        self.stack_top
            .store((&raw const here).addr(), Ordering::Relaxed);
    }

    /// How much of the stack the running hook takes right now: how far a
    /// local of this function is from where the hook started.
    fn stack_used(&self) -> usize {
        let here = 0_u8;
        self.stack_top
            .load(Ordering::Relaxed)
            .abs_diff((&raw const here).addr())
    }
}

/// How a hook's run ended.
enum Run {
    /// The script defines no such hook.
    NotDefined,
    /// It ran to its end.
    Done {
        operations: u64,
        actions: Vec<Action>,
    },
    /// It failed or hit a limit.
    Failed(ScriptError),
}

/// One bot's running script: its state, its timers, what it holds down,
/// and its budgets.
///
/// A script is a set of hooks. The instance runs one when its caller
/// hands it the event ([`handle`](Self::handle)) and runs `on_timer` when
/// its caller wakes it at the time it asked for ([`wake`](Self::wake)).
/// A hook runs to its end at once and never waits; between hooks, time
/// passes outside.
///
/// **What bounds a hook:** [`OPERATIONS_PER_HOOK`] operations, and
/// [`OPERATIONS_PER_SECOND`] over all of a second's hooks; its time, read
/// from the instance's [`Stopwatch`] every few operations, a hook and a
/// second ([`limits::HOOK_TIME`], [`limits::TIME_PER_SECOND`]); Rhai's limits
/// for calls, texts, arrays, maps and variables; and the state's own, as
/// a small map of plain data, checked every
/// [`STATE_CHECK_EVERY_OPERATIONS`] operations while the hook runs and
/// after it (ADR-0036, D3, D5).
///
/// **When a script fails or hits a limit, it stops:** the one action left
/// is [`Action::ReleaseAll`], its timers and state are gone, and it never
/// runs again. A new instance is a new start.
///
/// **Each instance has an engine of its own,** over packages and API
/// functions that are shared, so one instance's budget, state check and
/// game objects are never another's (ADR-0040). An instance can move to
/// another thread between calls.
///
/// [`OPERATIONS_PER_HOOK`]: limits::OPERATIONS_PER_HOOK
/// [`OPERATIONS_PER_SECOND`]: limits::OPERATIONS_PER_SECOND
/// [`STATE_CHECK_EVERY_OPERATIONS`]: limits::STATE_CHECK_EVERY_OPERATIONS
pub struct Instance {
    engine: Engine,
    program: Arc<Program>,
    turn: SharedTurn,
    running: Arc<Running>,
    budget: SecondBudget,
    status: Status,
    /// Whether the bot could act when the instance last heard.
    presence: Presence,
    /// What the hooks of the call that's running were refused.
    refused: Vec<Refused>,
}

impl fmt::Debug for Instance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Instance")
            .field("status", &self.status)
            .field("presence", &self.presence)
            .finish_non_exhaustive()
    }
}

impl Instance {
    /// A new start of `program`: an empty state, no timer, nothing held.
    /// `seed` is what its random numbers come from; two instances with
    /// one seed draw the same. `stopwatch` is what its hooks' time is
    /// read from, every few operations while one runs.
    #[must_use]
    pub fn new(program: Arc<Program>, seed: u64, stopwatch: Arc<dyn Stopwatch>) -> Self {
        Self::with_stack_budget(program, seed, stopwatch, STACK_BUDGET)
    }

    /// [`new`](Self::new), with how much of the stack a hook may take.
    fn with_stack_budget(
        program: Arc<Program>,
        seed: u64,
        stopwatch: Arc<dyn Stopwatch>,
        stack_budget: usize,
    ) -> Self {
        let turn: SharedTurn =
            Arc::new(std::sync::Mutex::new(Turn::new(Arc::clone(&program), seed)));
        let running = Arc::new(Running {
            operations: AtomicU64::new(0),
            state: state::empty(),
            ended: AtomicU8::new(0),
            stack_top: AtomicUsize::new(0),
            stack_budget,
            stopwatch,
            time_read: AtomicU64::new(0),
            time_taken: AtomicU64::new(0),
            time_allowed: AtomicU64::new(u64::MAX),
        });

        let resolver = {
            let turn = SharedTurn::clone(&turn);
            let running = Arc::clone(&running);
            Box::new(move |name: &str| {
                if name != STATE {
                    return Ok(api::object(name, &turn));
                }
                // A call on the state is running and holds it. Rhai would
                // wait for it and then fail in a way a script can catch;
                // this ends the hook at once instead.
                if state::in_use(&running.state) {
                    let token = running.end(Ended::State(Fault::InUse));
                    return Err(EvalAltResult::ErrorTerminated(token, Position::NONE).into());
                }
                Ok(Some(state::shared(&running.state)))
            })
        };
        let progress = {
            let running = Arc::clone(&running);
            move |operations: u64| {
                running.operations.store(operations, Ordering::Relaxed);
                // Ended already: whatever caught that is ended again.
                if let Some(ended) = running.ended_for() {
                    return Some(Dynamic::from(ended));
                }
                if running.stack_used() > running.stack_budget {
                    return Some(running.end(Ended::Stack));
                }
                if operations.is_multiple_of(limits::TIME_CHECK_EVERY_OPERATIONS)
                    && running.count_time()
                {
                    return Some(running.end(Ended::Time));
                }
                if !operations.is_multiple_of(limits::STATE_CHECK_EVERY_OPERATIONS) {
                    return None;
                }
                match state::check(&running.state) {
                    Checked::Faulty(fault) => Some(running.end(Ended::State(fault))),
                    Checked::Fine(_) | Checked::Busy => None,
                }
            }
        };
        Self {
            engine: engine::runner(program.script().api(), resolver, progress),
            program,
            turn,
            running,
            budget: SecondBudget::default(),
            status: Status::Running,
            presence: Presence::NotInWorld,
            refused: Vec::new(),
        }
    }

    /// The program this is an instance of.
    #[must_use]
    pub fn program(&self) -> &Program {
        &self.program
    }

    /// Hands the script an event: its hook runs to its end.
    ///
    /// Before `on_join`, `on_world_change`, `on_death` and `on_respawn`
    /// everything the script holds is let go, whether it defines the hook
    /// or not: each puts the bot somewhere new. `limiter` is the bot's
    /// own, which outlives the instance; every action in the answer has
    /// passed it.
    ///
    /// [`HookEvent::Stop`] ends the script in an orderly way: `on_stop`
    /// runs, where only one declared message and releases are taken, then
    /// everything is released.
    ///
    /// **A screen that opens** ([`HookEvent::MenuOpen`]) let go of every key
    /// and button below the script (ADR-0036, M8), so the script hears it as
    /// a let-go first, whatever its level. A script that defines
    /// `on_menu_open` then handles the screen. One that doesn't, at level 1
    /// or 2, has a server's screen closed at once and what it held pressed
    /// again, past the same limits as any press: it never stays behind a
    /// screen it has no hook for. The bot's own inventory only a script
    /// opens, and it's never closed for it (ADR-0052).
    ///
    /// [`HookEvent::Click`] tells the script that its action's fate came,
    /// or that it wasn't sent after all: the next action may go, whether it
    /// defines `on_click` or not.
    pub fn handle(
        &mut self,
        event: &HookEvent,
        moment: &Moment<'_>,
        limiter: &mut ActionLimiter,
    ) -> Ran {
        if self.status != Status::Running {
            return Self::over();
        }
        self.presence = moment.presence;
        let hook = event.hook();
        if hook == Hook::Stop {
            return self.end(moment, limiter);
        }
        let mut actions = Vec::new();
        if hook.releases_first() {
            lock(&self.turn).holds.release_all();
            actions.push(Action::ReleaseAll);
        }
        match event {
            HookEvent::MenuOpen => {
                let held = self.let_go_for_a_screen();
                if !self.program.defines(Hook::MenuOpen) {
                    return self.screen_without_a_hook(moment, limiter, &held);
                }
            }
            HookEvent::Click(_) => lock(&self.turn).heard_fate(),
            _ => {}
        }
        let outcome = match self.run(event, moment, limiter, false) {
            Run::NotDefined => Outcome::NotDefined,
            Run::Done {
                operations,
                actions: asked,
            } => {
                actions.extend(asked);
                Outcome::Ran { operations }
            }
            Run::Failed(error) => return self.fail(error),
        };
        Ran {
            actions,
            next_wake: self.wake_after(moment.now),
            outcome,
            refused: core::mem::take(&mut self.refused),
        }
    }

    /// Wakes the script at the time it asked for: inputs whose `hold_for`
    /// is over are let go, and `on_timer` runs for **one** timer that's
    /// due, the one that was due first.
    ///
    /// **One hook a wake.** While another timer is due, the answer asks to
    /// be woken again at once ([`Ran::next_wake`] is this moment), so the
    /// timers still run in the order they were due, each as a hook of its
    /// own. Whoever runs the script gets its thread back between two
    /// hooks, and no call on an instance takes longer than one hook may
    /// (ADR-0043).
    ///
    /// Timers wait while the bot is dead or between worlds; a repeating
    /// one that was due several times runs once.
    pub fn wake(&mut self, moment: &Moment<'_>, limiter: &mut ActionLimiter) -> Ran {
        if self.status != Status::Running {
            return Self::over();
        }
        self.presence = moment.presence;
        let mut actions: Vec<Action> = lock(&self.turn)
            .holds
            .due(moment.now)
            .into_iter()
            .map(Action::Release)
            .collect();
        let mut total = 0_u64;
        // Timers wait while the bot is dead or between worlds. One a
        // wake: the next one that's due makes `next_wake` this moment.
        if let Some(name) = self.due_timer(moment) {
            match self.run(&HookEvent::Timer(name), moment, limiter, false) {
                Run::NotDefined => {}
                Run::Done {
                    operations,
                    actions: asked,
                } => {
                    total = operations;
                    actions.extend(asked);
                }
                Run::Failed(error) => return self.fail(error),
            }
        }
        Ran {
            actions,
            next_wake: self.wake_after(moment.now),
            outcome: Outcome::Ran { operations: total },
            refused: core::mem::take(&mut self.refused),
        }
    }

    /// When the script wants to be woken next: its next timer, or the end
    /// of a `hold_for`. Timers don't count while the bot is dead or
    /// between worlds, as the instance last heard.
    #[must_use]
    pub fn next_wake(&self) -> Option<DateTime<Utc>> {
        if self.status != Status::Running {
            return None;
        }
        let turn = lock(&self.turn);
        let timers = (self.presence == Presence::InWorld)
            .then(|| turn.timers.next_due())
            .flatten();
        [turn.holds.next_due(), timers].into_iter().flatten().min()
    }

    /// Lets go of everything the script holds, as the session did when a
    /// screen opened: what was held, with when each was to be let go, and
    /// whether the right button was held until used.
    fn let_go_for_a_screen(&self) -> Held {
        let mut turn = lock(&self.turn);
        let holds = turn.holds.holds().collect();
        let until_used = turn.held_until_used();
        turn.holds.release_all();
        Held { holds, until_used }
    }

    /// A screen opened for a script with no `on_menu_open`: a server's is
    /// closed at once, and what the script held is pressed again.
    fn screen_without_a_hook(
        &mut self,
        moment: &Moment<'_>,
        limiter: &mut ActionLimiter,
        held: &Held,
    ) -> Ran {
        let servers = moment
            .menu
            .screen
            .as_ref()
            .is_some_and(|screen| screen.kind() != ScreenKind::Inventory);
        let mut turn = lock(&self.turn);
        turn.begin(moment, core::mem::take(limiter), false);
        if servers {
            // A close is never refused while a screen is open.
            let _ = turn.close_screen();
            turn.press_again(&held.holds, held.until_used);
        }
        let (actions, back, refused) = turn.end();
        drop(turn);
        *limiter = back;
        for Refused {
            asked,
            reason,
            times,
        } in refused
        {
            note(&mut self.refused, asked, reason, times);
        }
        Ran {
            actions,
            next_wake: self.wake_after(moment.now),
            outcome: Outcome::NotDefined,
            refused: core::mem::take(&mut self.refused),
        }
    }

    /// Whether an action the script sent onto a screen still waits for its
    /// fate: until then, the next one is `click_pending`.
    #[must_use]
    pub fn awaits_fate(&self) -> bool {
        lock(&self.turn).awaits_fate()
    }

    /// Tells the script that `inputs` were let go from below it: by the
    /// leash, by the tool guard, or because the item was used up.
    pub fn let_go(&mut self, inputs: impl IntoIterator<Item = Input>) {
        let mut turn = lock(&self.turn);
        for input in inputs {
            turn.holds.release(input);
        }
    }

    /// The bot's session ended: nothing is held any more, and the timers
    /// are cleared, so `on_join` starts them again. The state stays.
    pub fn disconnected(&mut self) {
        self.presence = Presence::NotInWorld;
        let mut turn = lock(&self.turn);
        turn.holds.release_all();
        turn.timers.clear();
        // A fate that didn't come before the session ended never comes.
        turn.heard_fate();
    }

    /// Why the script stopped, if it failed or hit a limit.
    #[must_use]
    pub const fn error(&self) -> Option<&ScriptError> {
        match &self.status {
            Status::Stopped(error) => Some(error),
            Status::Running | Status::Ended => None,
        }
    }

    /// Whether the script holds `input` down.
    #[must_use]
    pub fn is_held(&self, input: Input) -> bool {
        lock(&self.turn).is_held(input)
    }

    /// How many timers the script has set.
    #[must_use]
    pub fn timers(&self) -> usize {
        lock(&self.turn).timers.len()
    }

    /// How much the state holds: its entries, nested ones counted, and
    /// its bytes.
    #[must_use]
    pub fn state_size(&self) -> (usize, usize) {
        match state::check(&self.running.state) {
            Checked::Fine(size) => (size.entries, size.bytes),
            Checked::Busy | Checked::Faulty(_) => (0, 0),
        }
    }

    /// The next timer that's due at `moment`, if the bot can act then.
    fn due_timer(&self, moment: &Moment<'_>) -> Option<TimerName> {
        (moment.presence == Presence::InWorld)
            .then(|| lock(&self.turn).timers.take_due(moment.now))
            .flatten()
    }

    /// The answer of a script that's over.
    const fn over() -> Ran {
        Ran {
            actions: Vec::new(),
            next_wake: None,
            outcome: Outcome::Over,
            refused: Vec::new(),
        }
    }

    /// [`next_wake`](Self::next_wake), never before `now`.
    fn wake_after(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.next_wake().map(|due| due.max(now))
    }

    /// Lets go of everything and forgets the timers and the state.
    fn release_everything(&mut self) {
        let mut turn = lock(&self.turn);
        turn.holds.release_all();
        turn.timers.clear();
        drop(turn);
        state::clear(&self.running.state);
    }

    /// Stops the script for `error`.
    fn fail(&mut self, error: ScriptError) -> Ran {
        self.release_everything();
        self.status = Status::Stopped(error.clone());
        Ran {
            actions: vec![Action::ReleaseAll],
            next_wake: None,
            outcome: Outcome::Stopped(error),
            refused: core::mem::take(&mut self.refused),
        }
    }

    /// Ends the script in an orderly way: `on_stop`, then everything is
    /// released.
    fn end(&mut self, moment: &Moment<'_>, limiter: &mut ActionLimiter) -> Ran {
        let (mut actions, error) = match self.run(&HookEvent::Stop, moment, limiter, true) {
            Run::NotDefined => (Vec::new(), None),
            Run::Done { actions, .. } => (actions, None),
            // What a failed `on_stop` asked for is dropped, like any
            // failed hook's.
            Run::Failed(error) => (Vec::new(), Some(error)),
        };
        self.release_everything();
        self.status = Status::Ended;
        actions.push(Action::ReleaseAll);
        Ran {
            actions,
            next_wake: None,
            outcome: Outcome::Ended { error },
            refused: core::mem::take(&mut self.refused),
        }
    }

    /// Runs `event`'s hook, if the script defines it.
    fn run(
        &mut self,
        event: &HookEvent,
        moment: &Moment<'_>,
        limiter: &mut ActionLimiter,
        stopping: bool,
    ) -> Run {
        let hook = event.hook();
        if !self.program.defines(hook) {
            return Run::NotDefined;
        }
        let limit = |limit| ScriptError {
            kind: ScriptErrorKind::Limit(limit),
            hook,
            at: None,
        };
        // Nothing left of this second: the hook isn't run at all. A
        // budget of 0 is never handed to Rhai, which takes it for none.
        let allowance = match self.budget.allowance(moment.now) {
            Ok(allowance) => allowance,
            Err(spent) => return Run::Failed(limit(spent)),
        };
        self.engine.set_max_operations(allowance.operations.get());
        self.running.operations.store(0, Ordering::Relaxed);
        self.running.ended.store(0, Ordering::Relaxed);
        self.running.mark_stack();
        self.running.start_time(allowance.time);
        lock(&self.turn).begin(moment, core::mem::take(limiter), stopping);

        let returned = call(&self.engine, self.program.ast(), event);

        let (actions, back, refused) = lock(&self.turn).end();
        *limiter = back;
        // Told to the runtime whether the hook goes on to fail or not.
        for Refused {
            asked,
            reason,
            times,
        } in refused
        {
            note(&mut self.refused, asked, reason, times);
        }
        // A hook that ran out of its budget counted one operation more
        // than it was allowed.
        let operations = self
            .running
            .operations
            .load(Ordering::Relaxed)
            .min(allowance.operations.get());
        // The time since the hook's last reading counts too.
        let overtime = self.running.count_time();
        self.budget
            .spend(moment.now, operations, self.running.time_taken());

        // One of the instance's own guards ended the hook: that's why it
        // stopped, whatever the script made of the error on its way out.
        if let Some(ended) = self.running.ended_for() {
            return Run::Failed(ScriptError {
                kind: ended.kind(allowance.time_by_second),
                hook,
                at: None,
            });
        }
        if let Err(error) = returned {
            return Run::Failed(failure(&error, hook, allowance.by_second));
        }
        if overtime {
            return Run::Failed(ScriptError {
                kind: Ended::Time.kind(allowance.time_by_second),
                hook,
                at: None,
            });
        }
        // And once more after the hook, for what it did since the last
        // check while it ran.
        match state::check(&self.running.state) {
            Checked::Faulty(fault) => Run::Failed(ScriptError {
                kind: Ended::State(fault).kind(false),
                hook,
                at: None,
            }),
            Checked::Fine(_) | Checked::Busy => Run::Done {
                operations,
                actions,
            },
        }
    }
}

/// Calls `event`'s hook in `ast` with what it's told, never running the
/// script's top level.
fn call(
    engine: &Engine,
    ast: &rhai::AST,
    event: &HookEvent,
) -> Result<Dynamic, Box<EvalAltResult>> {
    let options = CallFnOptions::new().eval_ast(false);
    let mut scope = Scope::new();
    let name = event.hook().name();
    match event {
        HookEvent::Join { first } => {
            engine.call_fn_with_options(options, &mut scope, ast, name, (*first,))
        }
        HookEvent::Timer(timer) => engine.call_fn_with_options(
            options,
            &mut scope,
            ast,
            name,
            (timer.as_str().to_owned(),),
        ),
        HookEvent::Chat(found) => {
            let values: Map = found
                .values
                .iter()
                .map(|(name, value)| (name.as_str().into(), captured(value)))
                .collect();
            engine.call_fn_with_options(
                options,
                &mut scope,
                ast,
                name,
                (found.key.as_str().to_owned(), values),
            )
        }
        HookEvent::Click(outcome) => {
            engine.call_fn_with_options(options, &mut scope, ast, name, (clicked(*outcome),))
        }
        HookEvent::WorldChange
        | HookEvent::Death
        | HookEvent::Respawn
        | HookEvent::Stop
        | HookEvent::MenuOpen
        | HookEvent::MenuUpdate
        | HookEvent::MenuClose
        | HookEvent::SignOpen => engine.call_fn_with_options(options, &mut scope, ast, name, ()),
    }
}

/// What a script held when a screen let go of it.
struct Held {
    holds: Vec<(Input, Option<DateTime<Utc>>)>,
    until_used: bool,
}

/// What `on_click(result)` is told: `ok`, `reason` (empty for `ok`), and
/// `slot`, `()` for a button, a trade or a name.
fn clicked(outcome: ClickOutcome) -> Map {
    let mut map = Map::new();
    map.insert("ok".into(), Dynamic::from_bool(outcome.answer.is_ok()));
    map.insert(
        "reason".into(),
        Dynamic::from(outcome.answer.err().map_or("", Refusal::as_str)),
    );
    map.insert(
        "slot".into(),
        outcome
            .slot
            .map_or(Dynamic::UNIT, |slot| Dynamic::from_int(i64::from(slot))),
    );
    map
}

/// A captured value as a script reads it: a number, or one of the
/// pattern's own words.
fn captured(value: &Captured) -> Dynamic {
    match value {
        Captured::Integer(number) => Dynamic::from_int(*number),
        Captured::Amount(number) => Dynamic::from_float(FLOAT::from(*number)),
        Captured::Word(word) => Dynamic::from(word.as_str().to_owned()),
    }
}

/// Rhai's error as ours. `by_second` says whether the hook's operations
/// were cut to what was left of the second.
fn failure(error: &EvalAltResult, hook: Hook, by_second: bool) -> ScriptError {
    // The error itself, out of the calls it happened in.
    let inner = error.unwrap_inner();
    let limit = |limit| (ScriptErrorKind::Limit(limit), None);
    let failed = || {
        // Rhai's text ends with the place, which the error carries apart.
        let text = inner.to_string();
        let place = format!(" ({})", inner.position());
        let text = text.strip_suffix(&place).unwrap_or(&text);
        let at = position(inner.position()).or_else(|| position(error.position()));
        (
            ScriptErrorKind::Failed {
                message: message(text, ScriptError::MAX_MESSAGE_CHARS),
            },
            at,
        )
    };
    let (kind, at) = match inner {
        EvalAltResult::ErrorTooManyOperations(_) if by_second => limit(Limit::OperationsPerSecond),
        EvalAltResult::ErrorTooManyOperations(_) => limit(Limit::OperationsPerHook),
        EvalAltResult::ErrorStackOverflow(_) => limit(Limit::CallDepth),
        EvalAltResult::ErrorTooManyVariables(_) => limit(Limit::Variables),
        EvalAltResult::ErrorDataTooLarge(what, _) if what.starts_with("Length of string") => {
            limit(Limit::TextBytes)
        }
        EvalAltResult::ErrorDataTooLarge(what, _) if what.starts_with("Size of array") => {
            limit(Limit::ArrayItems)
        }
        EvalAltResult::ErrorDataTooLarge(what, _) if what.starts_with("Size of object map") => {
            limit(Limit::MapEntries)
        }
        EvalAltResult::ErrorRuntime(value, at) => match value.clone().try_cast::<Misuse>() {
            Some(misuse) => (
                ScriptErrorKind::Misuse(misuse),
                position(*at).or_else(|| position(error.position())),
            ),
            None => failed(),
        },
        _ => failed(),
    };
    ScriptError { kind, hook, at }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use fleet_core::mode::HotbarSlot;
    use fleet_core::script::{
        BlockPosition, Blocks, BotFacts, Identifier, Position as Place, Script, ScriptDraft,
    };

    use super::*;
    use crate::compile::compile;

    fn program(source: &str) -> Arc<Program> {
        let script = Script::try_from(ScriptDraft {
            api: 1,
            source: source.to_owned(),
            leash: 8,
            tool_guard: 2,
            block_guard: Vec::new(),
            messages: Vec::new(),
            patterns: Vec::new(),
            signs: Vec::new(),
        })
        .unwrap();
        Arc::new(compile(&script).unwrap())
    }

    fn facts() -> Snapshot {
        let bot = BotFacts {
            health: 20.0,
            food: 20,
            position: Place::try_new(0.5, 64.0, 0.5).unwrap(),
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

    /// Runs `on_join` of `source` once, with `stack_budget` bytes of stack
    /// for the hook.
    fn join(source: &str, stack_budget: usize) -> (Ran, Instance) {
        let facts = facts();
        let menu = MenuView::default();
        let mut instance = Instance::with_stack_budget(
            program(source),
            7,
            Arc::new(fleet_testkit::stopwatch::ManualStopwatch::standing()),
            stack_budget,
        );
        let ran = instance.handle(
            &HookEvent::Join { first: true },
            &Moment {
                facts: &facts,
                now: Utc.with_ymd_and_hms(2026, 11, 17, 12, 0, 0).unwrap(),
                presence: Presence::InWorld,
                leash: None,
                menu: &menu,
                name: "AfkBot1",
            },
            &mut ActionLimiter::new(),
        );
        (ran, instance)
    }

    /// A recursion of ten calls, which no limit of Rhai's ends.
    const TEN_DEEP: &str =
        "fn on_join(first) { down(10); }\nfn down(n) { if n > 1 { down(n - 1); } }";

    const fn limit(limit: Limit) -> Outcome {
        Outcome::Stopped(ScriptError {
            kind: ScriptErrorKind::Limit(limit),
            hook: Hook::Join,
            at: None,
        })
    }

    #[test]
    fn a_hook_that_takes_more_of_the_stack_than_it_may_is_ended() {
        // Ten calls deep takes tens of KiB in any build: more than 4 KiB,
        // far less than the real budget.
        let (small, stopped) = join(TEN_DEEP, 4 * 1024);
        let (real, running) = join(TEN_DEEP, STACK_BUDGET);

        assert_eq!(small.outcome, limit(Limit::Stack));
        assert_eq!(small.actions, [Action::ReleaseAll]);
        assert_eq!(
            stopped.error().map(|error| &error.kind),
            Some(&ScriptErrorKind::Limit(Limit::Stack))
        );
        assert!(matches!(real.outcome, Outcome::Ran { .. }), "{real:?}");
        assert_eq!(running.error(), None);
    }

    #[test]
    fn a_hook_that_stays_shallow_is_never_ended_for_the_stack() {
        // A long loop at one level: the stack doesn't grow with the work.
        let (ran, _) = join(
            "fn on_join(first) { let n = 0; while n < 1000 { n += 1; } log.info(`${n}`); }",
            64 * 1024,
        );

        assert!(matches!(ran.outcome, Outcome::Ran { .. }), "{ran:?}");
    }

    #[test]
    fn a_script_cant_catch_its_way_past_the_stack_guard() {
        // The deep call runs inside a function an array's method calls,
        // which wraps its error into one a script can catch. Caught, the
        // next thing the script does ends it again.
        let source = "fn on_join(first) {\n  try { [1].for_each(Fn(\"deep\")); } catch { log.info(\"caught\"); }\n  log.info(\"went on\");\n}\nfn deep() { down(10); }\nfn down(n) { if n > 1 { down(n - 1); } }";

        let (ran, _) = join(source, 4 * 1024);

        assert_eq!(ran.outcome, limit(Limit::Stack));
        assert_eq!(ran.actions, [Action::ReleaseAll]);
    }

    #[test]
    fn the_stack_is_measured_from_each_hooks_own_start() {
        // Two hooks, the second called from deeper in this test's own
        // stack: each is judged by what it takes itself.
        fn deeper(instance: &mut Instance, facts: &Snapshot, levels: u32) -> Ran {
            let padding = [0_u8; 2048];
            if levels > 0 {
                let ran = deeper(instance, facts, levels - 1);
                assert_eq!(padding.len(), 2048);
                return ran;
            }
            instance.handle(
                &HookEvent::Join { first: false },
                &Moment {
                    facts,
                    now: Utc.with_ymd_and_hms(2026, 11, 17, 12, 0, 5).unwrap(),
                    presence: Presence::InWorld,
                    leash: None,
                    menu: &MenuView::default(),
                    name: "AfkBot1",
                },
                &mut ActionLimiter::new(),
            )
        }
        let facts = facts();
        let (first, mut instance) = join("fn on_join(first) { let a = 1; }", 64 * 1024);

        let second = deeper(&mut instance, &facts, 64);

        assert!(matches!(first.outcome, Outcome::Ran { .. }), "{first:?}");
        assert!(matches!(second.outcome, Outcome::Ran { .. }), "{second:?}");
    }

    #[test]
    fn every_reason_the_guards_end_a_hook_for_reads_back_from_its_number() {
        let reasons = [
            Ended::State(Fault::NotAMap),
            Ended::State(Fault::NotPlain),
            Ended::State(Fault::Entries),
            Ended::State(Fault::Bytes),
            Ended::State(Fault::InUse),
            Ended::Stack,
            Ended::Time,
        ];

        for reason in reasons {
            assert_ne!(reason.code(), 0);
            assert_eq!(Ended::from_code(reason.code()), Some(reason));
        }
        assert_eq!(Ended::from_code(0), None);
        assert_eq!(
            reasons.map(|reason| reason.kind(false)),
            [
                ScriptErrorKind::StateNotAMap,
                ScriptErrorKind::StateNotPlain,
                ScriptErrorKind::Limit(Limit::StateEntries),
                ScriptErrorKind::Limit(Limit::StateBytes),
                ScriptErrorKind::StateInUse,
                ScriptErrorKind::Limit(Limit::Stack),
                ScriptErrorKind::Limit(Limit::HookTime),
            ]
        );
        assert_eq!(
            Ended::Time.kind(true),
            ScriptErrorKind::Limit(Limit::TimePerSecond)
        );
        assert_eq!(
            Ended::Stack.kind(true),
            ScriptErrorKind::Limit(Limit::Stack)
        );
    }

    #[test]
    fn the_budget_is_half_of_a_runtime_threads_stack() {
        assert_eq!(STACK_BUDGET, 1024 * 1024);
    }

    #[test]
    fn an_instance_says_little_about_itself_in_debug_output() {
        let (_, instance) = join("fn on_join(first) { state.secret = 1; }", STACK_BUDGET);

        let shown = format!("{instance:?}");

        assert!(shown.starts_with("Instance { status: Running"), "{shown}");
        assert!(!shown.contains("secret"));
    }
}
