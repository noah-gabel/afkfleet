//! [`ScriptRunner`]: runs a bot's script in one session (Plan.md P15.16 to
//! P15.18, P15.33, P16.13; ADR-0036, D1, D5, D6, D9, M4, M8, R1; ADR-0037,
//! C4, C5, C7; ADR-0052).

use core::ops::ControlFlow;
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::chat::{ChatFormats, ChatMessage, IncomingChat};
use fleet_core::id::BotId;
use fleet_core::mc::{Guards, MenuCommand, MenuError, MenuView, SessionError, SessionHandle};
use fleet_core::mode::{GameAction, ModeDefinition};
use fleet_core::script::{
    ChatWatch, ClickOutcome, Hook, HookEvent, Ident, Input, Leash, Observed, Position, Refusal,
    Script, SendId, Snapshot, Stopwatch, candidate_line,
};
use fleet_core::time;
use fleet_script::{Action, Asked, Moment, Outcome, Presence, PressKind, Ran, Refused, compile};
use rand::Rng as _;
use rand::rngs::StdRng;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::event::{NotSent, ScriptEvent, ScriptLog, ScriptStatus};
use super::feed::{Fed, Life, ScriptInbox};
use super::slot::{ScriptPrograms, ScriptSlot, ScriptTurns};
use crate::chat::{ChatError, ChatFailure, ModeChat, SendOutcome};
use crate::clock::RuntimeClock;
use crate::event::{FleetEvent, FleetEventKind};
use crate::failure_log::{FailureKind, FailureLog, warn_or_debug};
use crate::metrics;

/// How long the runner waits before it asks a session again that couldn't
/// answer: for the bot's placement in its world, and for the mode's guards.
const RETRY: Duration = Duration::from_millis(100);

/// How long a timer waits for a session that was too busy to say what the
/// bot sees.
const BUSY: Duration = Duration::from_millis(250);

/// How many outcomes of a script's sends wait for the runner at most: more
/// than a chat queue holds.
const OUTCOMES: usize = 64;

/// The least time between two `on_menu_update` hooks: a game tick. An
/// update that comes sooner runs when it's over, once, however many came
/// (Plan.md P16.13).
const UPDATE_EVERY: Duration = Duration::from_millis(50);

/// How long the runner waits for an action's fate when the session took
/// longer than its own timeout to answer the send: the session's gate gives
/// every action sent its fate within 5 s, so a fate that hasn't come after
/// this never will, and the script hears `unanswered` instead.
const FATE_PATIENCE: Duration = Duration::from_secs(10);

/// What a mode runner needs beside its own parts to run a script in its
/// session (Plan.md P15.16).
///
/// The bot's actor makes one for every session: the slot and the programs
/// are the bot's and the fleet's, the rest is the session's own.
#[derive(Debug)]
pub struct ScriptParts {
    /// The bot, for its events.
    pub bot_id: BotId,
    /// What of the bot's script outlives the session.
    pub slot: ScriptSlot,
    /// The compiled scripts the fleet's bots share.
    pub programs: ScriptPrograms,
    /// The turnstile the fleet's hooks pass: how many run at once.
    pub turns: ScriptTurns,
    /// The session's events for the script.
    pub inbox: ScriptInbox,
    /// The fleet's events.
    pub events: broadcast::Sender<FleetEvent>,
    /// What a hook's time is read from: the OS's monotonic clock in an
    /// agent, a manual one in tests (ADR-0040's amendment).
    pub stopwatch: Arc<dyn Stopwatch>,
    /// The server's chat formats, which tell a player's line from the
    /// server's own (ADR-0037, C2): the bot's spec's, the current ones at
    /// every line, so a change reaches a running script at once (P15.34).
    pub formats: watch::Receiver<ChatFormats>,
    /// Cancelled to stop the mode in an orderly way while the session
    /// goes on living: `on_stop` runs, then everything is released.
    pub stop: CancellationToken,
    /// The bot's own player name, from the credentials it connected with:
    /// what `bot.name` reads (ADR-0036, M12).
    pub name: String,
    /// Whether the session may still hold what a script pressed. The
    /// runner sets it when a script runs in the session and clears it
    /// once the script's last `ReleaseAll` went through; whoever owns the
    /// runner's task reads it when the task has ended, however it ended,
    /// and releases what's left (ADR-0036, D9).
    pub held: Arc<AtomicBool>,
}

/// How a script's time in a session ended.
#[derive(Debug)]
pub(crate) enum Next {
    /// The session, the mode's owner or an orderly stop ended it.
    End,
    /// The bot's mode changed to this one.
    Changed(ModeDefinition),
}

/// Why the runner's loop ended.
#[derive(Debug)]
enum Ended {
    /// The session is over: cancelled, closed, or the mode's owner is gone.
    Session,
    /// An orderly stop.
    Stop,
    /// The bot's mode changed.
    Changed(ModeDefinition),
}

/// What the runner waited for.
enum Waited {
    Ended(Ended),
    Outcome(SendOutcome),
    Fed(Fed),
    Due,
}

/// What a hook is given of the game.
struct Seen {
    facts: Snapshot,
    /// The inventory, the screen and the sign editor.
    menu: MenuView,
    /// Where the bot's leash is anchored, if it's placed.
    anchor: Option<Position>,
    /// Whether the session answered with the bot in its world.
    placed: bool,
}

/// Why the session didn't say what the bot sees.
enum Unseen {
    /// The session has ended.
    Closed,
    /// The bot is between worlds.
    NotInWorld,
    /// The session was too busy, or didn't answer in time.
    Busy,
}

/// Which call on the instance a turn makes.
enum Call<'a> {
    Event(&'a HookEvent),
    /// A wake the script asked for at this time.
    Wake(DateTime<Utc>),
}

/// Runs a bot's script in one session, from the mode runner's task.
///
/// - **An event or a due timer takes a snapshot, runs the hook to its end
///   and performs what it asked for,** in order. The instance has put every
///   action past the bot's limiter already.
/// - **Time is tokio's:** the script's timers, its `hold_for`s and its
///   replies' windows are counted on the runtime's clock, so paused time
///   moves them. A hook's own time is read from the stopwatch.
/// - **The mode's guards are set before anything is pressed,** at every
///   join and every world change.
/// - **Every way out releases everything:** an orderly stop and a mode
///   change after `on_stop`; an error and a limit by the instance's own
///   last action; a death and a world change before their hooks. A session
///   that ended holds nothing by itself.
pub(crate) struct ScriptRunner<'a, S> {
    session: &'a S,
    chat: &'a ModeChat,
    clock: RuntimeClock,
    rng: &'a mut StdRng,
    mode: &'a mut watch::Receiver<ModeDefinition>,
    failures: &'a mut FailureLog,
    parts: &'a mut ScriptParts,
    definition: &'a ModeDefinition,
    script: &'a Script,
    cancel: &'a CancellationToken,
    /// Where the chat delivery reports what became of the script's sends.
    outcomes: mpsc::Receiver<SendOutcome>,
    report: mpsc::Sender<SendOutcome>,
    /// Whether the bot lives, as far as the session said.
    alive: bool,
    /// What the bot saw last.
    last: Option<Snapshot>,
    /// A timer's wake isn't tried again before this.
    not_before: Option<DateTime<Utc>>,
    /// Whether the last thing the session took was a `ReleaseAll`.
    released: bool,
    /// The script's action that waits for its fate, if one went out.
    awaiting: Option<Awaiting>,
    /// What the script hears in `on_click` next: an action the session
    /// didn't send after all.
    queued: Option<ClickOutcome>,
    /// When `on_menu_update` last ran.
    updated: Option<DateTime<Utc>>,
    /// Whether an update waits for its game tick to be over.
    update_waits: bool,
}

/// An action that went out and waits for its fate.
#[derive(Debug, Clone, Copy)]
struct Awaiting {
    /// The slot it clicked, if it clicked one.
    slot: Option<u16>,
    /// When it went out, or when the session stopped answering its send.
    since: DateTime<Utc>,
}

/// What a [`ScriptRunner`] borrows of its mode runner.
pub(crate) struct Borrowed<'a, S> {
    pub(crate) session: &'a S,
    pub(crate) chat: &'a ModeChat,
    pub(crate) clock: RuntimeClock,
    pub(crate) rng: &'a mut StdRng,
    pub(crate) mode: &'a mut watch::Receiver<ModeDefinition>,
    pub(crate) failures: &'a mut FailureLog,
    pub(crate) parts: &'a mut ScriptParts,
}

impl<'a, S: SessionHandle> ScriptRunner<'a, S> {
    /// A runner for `script`, the body of `definition`, that ends at once
    /// when `cancel` is cancelled.
    pub(crate) fn new(
        borrowed: Borrowed<'a, S>,
        definition: &'a ModeDefinition,
        script: &'a Script,
        cancel: &'a CancellationToken,
    ) -> Self {
        let Borrowed {
            session,
            chat,
            clock,
            rng,
            mode,
            failures,
            parts,
        } = borrowed;
        let (report, outcomes) = mpsc::channel(OUTCOMES);
        Self {
            session,
            chat,
            clock,
            rng,
            mode,
            failures,
            parts,
            definition,
            script,
            cancel,
            outcomes,
            report,
            alive: true,
            last: None,
            not_before: None,
            released: false,
            awaiting: None,
            queued: None,
            updated: None,
            update_waits: false,
        }
    }

    /// Runs the script until the session ends, the mode stops in an
    /// orderly way, or the bot's mode changes.
    pub(crate) async fn run(mut self) -> Next {
        self.parts.inbox.start(!self.script.patterns().is_empty());
        let status = self.prepare().await;
        if status == ScriptStatus::Running {
            // From here on the session may hold what the script presses.
            self.parts.held.store(true, Ordering::SeqCst);
        }
        self.publish(ScriptEvent::Status(status));
        let ended = self.live().await;
        let next = self.finish(ended).await;
        self.parts.inbox.stop();
        next
    }

    /// Makes sure the bot's slot holds this script: as it stands there
    /// from an earlier session, or started anew.
    async fn prepare(&mut self) -> ScriptStatus {
        if let Some(status) = self.parts.slot.status_of(self.script) {
            return status;
        }
        let program = if let Some(program) = self.parts.programs.get(self.script) {
            Ok(program)
        } else {
            let source = self.script.clone();
            // About half a millisecond of pure computation, kept off the
            // async workers (ADR-0036, D1). It needs no timeout: the
            // compiler's own limits bound it, a blocking call can't be
            // cancelled, and the runner isn't either while it waits here.
            // tokio's blocking threads have a worker's stack.
            match tokio::task::spawn_blocking(move || compile(&source)).await {
                Ok(Ok(program)) => Ok(self.parts.programs.share(program)),
                Ok(Err(error)) => Err(error),
                Err(_) => {
                    warn!("compiling the bot's script failed inside the engine; it won't run");
                    metrics::script_stopped("crashed");
                    return self.parts.slot.crashed(self.script);
                }
            }
        };
        let status = self.parts.slot.start(
            self.script,
            program,
            self.rng.next_u64(),
            Arc::clone(&self.parts.stopwatch),
        );
        match &status {
            ScriptStatus::Running => info!("the bot's script started"),
            ScriptStatus::Refused(error) => {
                info!(
                    line = error.at.map(|at| at.line),
                    column = error.at.map(|at| at.column),
                    "the bot's script isn't a program this agent takes; the bot stays idle"
                );
                debug!(%error, "why the script was refused");
                metrics::script_stopped("refused");
            }
            ScriptStatus::Stopped(_) | ScriptStatus::Crashed => {}
        }
        status
    }

    /// Whether the slot's script runs.
    fn runs(&self) -> bool {
        self.parts.slot.status() == Some(ScriptStatus::Running)
    }

    /// The session's life with the script: the guards, the join, then
    /// every event and every wake until something ends it.
    async fn live(&mut self) -> Ended {
        if let ControlFlow::Break(ended) = self.guard().await {
            return ended;
        }
        let first = self.parts.slot.join();
        if let ControlFlow::Break(ended) = self.placed_hook(HookEvent::Join { first }).await {
            return ended;
        }
        loop {
            // An action the session didn't send is heard before anything
            // else: the script may act again only after it.
            if let Some(outcome) = self.queued.take() {
                if let ControlFlow::Break(ended) = self.clicked(outcome).await {
                    return ended;
                }
                continue;
            }
            let waited = self.wait(self.deadline(), true).await;
            let flow = match waited {
                Waited::Ended(ended) => return ended,
                Waited::Outcome(outcome) => {
                    self.sent(outcome);
                    ControlFlow::Continue(())
                }
                Waited::Fed(Fed::Life(life)) => self.life(life).await,
                Waited::Fed(Fed::Chat(chat)) => self.line(&chat).await,
                Waited::Due => self.due().await,
            };
            if let ControlFlow::Break(ended) = flow {
                return ended;
            }
        }
    }

    /// When the runner must wake by itself next: the script's own next
    /// wake, an update's tick, or the end of a fate's patience.
    fn deadline(&self) -> Option<Instant> {
        let script = self
            .parts
            .slot
            .next_wake()
            .map(|wake| self.not_before.map_or(wake, |after| wake.max(after)));
        let update = self
            .update_waits
            .then(|| self.updated.map(|at| time::add(at, UPDATE_EVERY)))
            .flatten();
        let fate = self
            .awaiting
            .map(|awaiting| time::add(awaiting.since, FATE_PATIENCE));
        let wake = [script, update, fate].into_iter().flatten().min()?;
        self.clock.deadline(wake)
    }

    /// Waits for whatever comes first: the session's or the mode's end, a
    /// send's outcome, something from the feed, or `deadline`. Without
    /// `listen` only an end or the deadline counts, and what's fed waits.
    async fn wait(&mut self, deadline: Option<Instant>, listen: bool) -> Waited {
        loop {
            tokio::select! {
                biased;
                // Every branch is cancel-safe: the tokens and `changed`
                // lose nothing when another branch wins, `recv` and the
                // inbox hand something out only in the poll that returns
                // it, and the sleep starts over from the deadline.
                () = self.cancel.cancelled() => return Waited::Ended(Ended::Session),
                () = self.parts.stop.cancelled() => return Waited::Ended(Ended::Stop),
                changed = self.mode.changed() => {
                    // An error means the owner dropped the sender.
                    if changed.is_err() {
                        return Waited::Ended(Ended::Session);
                    }
                    let changed = self.mode.borrow_and_update().clone();
                    if changed != *self.definition {
                        return Waited::Ended(Ended::Changed(changed));
                    }
                }
                Some(outcome) = self.outcomes.recv(), if listen => return Waited::Outcome(outcome),
                fed = self.parts.inbox.next(), if listen => return Waited::Fed(fed),
                () = sleep_until(deadline) => return Waited::Due,
            }
        }
    }

    /// Waits `length`, unless the session or the mode ends first.
    async fn pause(&mut self, length: Duration) -> ControlFlow<Ended> {
        match self.wait(Instant::now().checked_add(length), false).await {
            Waited::Ended(ended) => ControlFlow::Break(ended),
            Waited::Outcome(_) | Waited::Fed(_) | Waited::Due => ControlFlow::Continue(()),
        }
    }

    /// Sets the mode's own guards, and doesn't go on before the session
    /// took them: a new session has the defaults, never the mode's.
    async fn guard(&mut self) -> ControlFlow<Ended> {
        let guards = Guards {
            leash: self.script.leash(),
            tool_guard: self.script.tool_guard(),
            block_guard: self.script.block_guard().clone(),
        };
        loop {
            // Like every session call, bounded by the session's own
            // timeout.
            match self.session.set_guards(guards.clone()).await {
                Ok(()) => return ControlFlow::Continue(()),
                Err(SessionError::Closed) => return ControlFlow::Break(Ended::Session),
                Err(error) => {
                    let level = self.failures.level(FailureKind::Session(error));
                    warn_or_debug!(
                        level,
                        %error,
                        "the mode's guards couldn't be set yet; the script presses nothing until they are"
                    );
                }
            }
            self.pause(RETRY).await?;
        }
    }

    /// Asks the session once what the bot sees.
    async fn see(&mut self) -> Result<Seen, Unseen> {
        match self.session.observe().await {
            Ok(observation) => {
                self.last = Some(observation.facts.clone());
                Ok(Seen {
                    facts: observation.facts,
                    menu: observation.menu,
                    anchor: observation.anchor,
                    placed: true,
                })
            }
            Err(SessionError::Closed) => Err(Unseen::Closed),
            Err(SessionError::NotInWorld) => Err(Unseen::NotInWorld),
            Err(
                SessionError::QueueFull | SessionError::TimedOut | SessionError::ChatUnavailable,
            ) => Err(Unseen::Busy),
        }
    }

    /// What the bot sees once the server has placed it in its world,
    /// asking again until then.
    async fn see_placed(&mut self) -> Result<Seen, Ended> {
        loop {
            match self.see().await {
                Ok(seen) => return Ok(seen),
                Err(Unseen::Closed) => return Err(Ended::Session),
                Err(Unseen::NotInWorld | Unseen::Busy) => {}
            }
            if let ControlFlow::Break(ended) = self.pause(RETRY).await {
                return Err(ended);
            }
        }
    }

    /// What a hook is given when the session can't say what the bot sees:
    /// what it saw last, or a bot nowhere.
    fn unplaced(&self) -> Seen {
        Seen {
            facts: self.last.clone().unwrap_or_else(Snapshot::nowhere),
            menu: MenuView::default(),
            anchor: None,
            placed: false,
        }
    }

    /// Whether the bot can act, for a hook that saw the world or didn't.
    const fn presence(&self, placed: bool) -> Presence {
        if !self.alive {
            Presence::Dead
        } else if placed {
            Presence::InWorld
        } else {
            Presence::NotInWorld
        }
    }

    /// Runs the hook of an event that put the bot somewhere: it waits
    /// until the server has placed the bot there.
    async fn placed_hook(&mut self, event: HookEvent) -> ControlFlow<Ended> {
        if !self.runs() {
            return ControlFlow::Continue(());
        }
        let seen = match self.see_placed().await {
            Ok(seen) => seen,
            Err(ended) => return ControlFlow::Break(ended),
        };
        let presence = self.presence(true);
        let ran = self.turn(&Call::Event(&event), &seen, presence).await;
        self.after(ran, false).await
    }

    /// Something of the bot's life came from the session.
    async fn life(&mut self, life: Life) -> ControlFlow<Ended> {
        match life {
            Life::Died => {
                self.alive = false;
                if !self.runs() {
                    return ControlFlow::Continue(());
                }
                let seen = match self.see().await {
                    Ok(seen) => seen,
                    Err(Unseen::Closed) => return ControlFlow::Break(Ended::Session),
                    Err(Unseen::NotInWorld | Unseen::Busy) => self.unplaced(),
                };
                let ran = self
                    .turn(&Call::Event(&HookEvent::Death), &seen, Presence::Dead)
                    .await;
                self.after(ran, false).await
            }
            Life::Respawned => {
                self.alive = true;
                self.placed_hook(HookEvent::Respawn).await
            }
            Life::WorldChanged => {
                // The session's guards may be a new bot's own again.
                self.guard().await?;
                self.placed_hook(HookEvent::WorldChange).await
            }
            Life::LetGo(reason) => {
                self.parts
                    .slot
                    .turn(|instance, _| instance.let_go(reason.inputs()));
                ControlFlow::Continue(())
            }
            Life::MenuOpened(_) => {
                self.update_waits = false;
                self.menu_hook(HookEvent::MenuOpen).await
            }
            Life::MenuUpdated(_) => {
                let now = self.clock.now();
                let soon = self
                    .updated
                    .is_some_and(|at| time::elapsed(at, now) < UPDATE_EVERY);
                if soon {
                    self.update_waits = true;
                    return ControlFlow::Continue(());
                }
                self.update().await
            }
            Life::MenuClosed(_) => {
                self.update_waits = false;
                self.menu_hook(HookEvent::MenuClose).await
            }
            Life::Clicked(result) => {
                if self.awaiting.is_none() {
                    // A fate the runner stopped waiting for: the script was
                    // told `unanswered` already.
                    debug!(
                        ?result,
                        "a fate came after the script stopped waiting for it"
                    );
                    return ControlFlow::Continue(());
                }
                let outcome = ClickOutcome::from(result);
                if let Err(reason) = outcome.answer {
                    self.log(ScriptLog::Menu {
                        asked: Asked::Click,
                        slot: outcome.slot,
                        reason,
                        sent: true,
                    });
                }
                self.clicked(outcome).await
            }
            Life::SignOpened(_) => self.menu_hook(HookEvent::SignOpen).await,
        }
    }

    /// Runs the hook of a screen's or a sign editor's event. A session too
    /// busy to say what the bot sees skips it, as a match's hook is; the
    /// next event, or the script's own timer, sees the screen as it is.
    async fn menu_hook(&mut self, event: HookEvent) -> ControlFlow<Ended> {
        if !self.runs() {
            return ControlFlow::Continue(());
        }
        let seen = match self.see().await {
            Ok(seen) => seen,
            Err(Unseen::Closed) => return ControlFlow::Break(Ended::Session),
            Err(Unseen::NotInWorld) => self.unplaced(),
            Err(Unseen::Busy) => {
                debug!(
                    hook = event.hook().name(),
                    "the session was too busy to say what the bot sees; the hook is skipped"
                );
                return ControlFlow::Continue(());
            }
        };
        let presence = self.presence(seen.placed);
        let ran = self.turn(&Call::Event(&event), &seen, presence).await;
        self.after(ran, false).await
    }

    /// `on_menu_update`, now: at most one a game tick.
    async fn update(&mut self) -> ControlFlow<Ended> {
        self.update_waits = false;
        self.updated = Some(self.clock.now());
        self.menu_hook(HookEvent::MenuUpdate).await
    }

    /// The script hears what became of its action: `on_click`. It's
    /// delivered even when the session can't say what the bot sees, so the
    /// script may act again.
    async fn clicked(&mut self, outcome: ClickOutcome) -> ControlFlow<Ended> {
        self.awaiting = None;
        if !self.runs() {
            return ControlFlow::Continue(());
        }
        let seen = match self.see().await {
            Ok(seen) => seen,
            Err(Unseen::Closed) => return ControlFlow::Break(Ended::Session),
            Err(Unseen::NotInWorld | Unseen::Busy) => self.unplaced(),
        };
        let presence = self.presence(seen.placed);
        let ran = self
            .turn(&Call::Event(&HookEvent::Click(outcome)), &seen, presence)
            .await;
        self.after(ran, false).await
    }

    /// A chat message came from the session: if it's a line of the
    /// server's own that a pattern of the mode's fits now, `on_chat` runs.
    async fn line(&mut self, chat: &IncomingChat) -> ControlFlow<Ended> {
        let dropped = self.parts.inbox.take_dropped();
        if dropped > 0 {
            metrics::script_chat_dropped("queue", dropped);
        }
        if !self.runs() {
            return ControlFlow::Continue(());
        }
        let now = self.clock.now();
        self.expire(now);
        // Borrowed for this call only, never across an await.
        let formats = self.parts.formats.borrow().clone();
        let Some(line) = candidate_line(chat, &formats) else {
            return ControlFlow::Continue(());
        };
        let patterns = self.script.patterns();
        let observed = self
            .parts
            .slot
            .watch(|watch| watch.observe(patterns, line, now));
        let found = match observed {
            Observed::Nothing => return ControlFlow::Continue(()),
            Observed::TooSoon(_) => {
                metrics::script_chat_dropped("gap", 1);
                return ControlFlow::Continue(());
            }
            Observed::Match(found) => found,
        };
        self.log(ScriptLog::matched(&found));
        let seen = match self.see().await {
            Ok(seen) => seen,
            Err(Unseen::Closed) => return ControlFlow::Break(Ended::Session),
            Err(Unseen::NotInWorld) => self.unplaced(),
            Err(Unseen::Busy) => {
                debug!(
                    "the session was too busy to say what the bot sees; the match's hook is skipped"
                );
                return ControlFlow::Continue(());
            }
        };
        let presence = self.presence(seen.placed);
        let ran = self
            .turn(&Call::Event(&HookEvent::Chat(found)), &seen, presence)
            .await;
        self.after(ran, false).await
    }

    /// The runner's own wake: windows that ran out, and whatever of the
    /// script's is due.
    async fn due(&mut self) -> ControlFlow<Ended> {
        let now = self.clock.now();
        self.expire(now);
        if let Some(awaiting) = self.awaiting
            && time::elapsed(awaiting.since, now) >= FATE_PATIENCE
        {
            self.log(ScriptLog::Menu {
                asked: Asked::Click,
                slot: awaiting.slot,
                reason: Refusal::Unanswered,
                sent: true,
            });
            return self
                .clicked(ClickOutcome {
                    slot: awaiting.slot,
                    answer: Err(Refusal::Unanswered),
                })
                .await;
        }
        if self.update_waits
            && self
                .updated
                .is_none_or(|at| time::elapsed(at, now) >= UPDATE_EVERY)
        {
            return self.update().await;
        }
        let due = self
            .parts
            .slot
            .turn(|instance, _| instance.next_wake())
            .flatten()
            .filter(|wake| *wake <= now);
        let Some(due) = due else {
            return ControlFlow::Continue(());
        };
        let seen = match self.see().await {
            Ok(seen) => seen,
            Err(Unseen::Closed) => return ControlFlow::Break(Ended::Session),
            // Between worlds the timers wait, as the instance knows.
            Err(Unseen::NotInWorld) => self.unplaced(),
            Err(Unseen::Busy) => {
                // The instance isn't told the bot left its world, which
                // would stop its timers: the wake is tried again.
                self.not_before = Some(time::add(now, BUSY));
                return ControlFlow::Continue(());
            }
        };
        self.not_before = None;
        let presence = self.presence(seen.placed);
        let ran = self.turn(&Call::Wake(due), &seen, presence).await;
        self.after(ran, false).await
    }

    /// Logs the windows that ran out by `now`.
    fn expire(&mut self, now: DateTime<Utc>) {
        for pattern in self.parts.slot.watch(|watch| watch.expired(now)) {
            self.log(ScriptLog::WindowExpired { pattern });
        }
    }

    /// The chat delivery says what became of a send: a command that was
    /// handed to the session opens the windows that waited for it, at the
    /// moment it was handed over; one that wasn't opens none.
    fn sent(&mut self, outcome: SendOutcome) {
        let SendOutcome {
            send,
            key,
            message,
            at,
            result,
        } = outcome;
        match result {
            Ok(()) => {
                self.log(ScriptLog::Sent { key, message });
                for pattern in self.parts.slot.watch(|watch| watch.sent(send, at)) {
                    self.log(ScriptLog::WindowOpened { pattern });
                }
            }
            Err(failure) => self.not_sent(send, key, NotSent::Failed(failure)),
        }
    }

    /// A send that wasn't sent: its windows are gone, and the log says so.
    fn not_sent(&mut self, send: SendId, key: Ident, reason: NotSent) {
        self.note(ScriptLog::NotSent { key, reason });
        for pattern in self.parts.slot.watch(|watch| watch.dropped(send)) {
            self.note(ScriptLog::WindowDropped { pattern });
        }
    }

    /// Runs one call on the instance at this moment and counts it; `None`
    /// if no instance stands in the slot, or the session ended while the
    /// hook waited for its turn.
    ///
    /// **A hook takes a turn first** ([`ScriptTurns`]): no more hooks run
    /// at once than the runtime can spare workers for. The snapshot was
    /// taken before the wait, so under a load that makes hooks wait it's
    /// as old as the wait; the session's own guards judge the game as it
    /// is, every tick.
    async fn turn(&mut self, call: &Call<'_>, seen: &Seen, presence: Presence) -> Option<Ran> {
        let asked = self.parts.stopwatch.elapsed();
        let turns = self.parts.turns.clone();
        let turn = tokio::select! {
            biased;
            // Cancel-safe: a turn that was being waited for is given up.
            () = self.cancel.cancelled() => return None,
            turn = turns.take() => turn,
        };
        let waited = self.parts.stopwatch.elapsed().saturating_sub(asked);
        let moment = Moment {
            facts: &seen.facts,
            now: self.clock.now(),
            presence,
            leash: seen
                .anchor
                .map(|anchor| Leash::new(self.script.leash(), anchor)),
            menu: &seen.menu,
            name: &self.parts.name,
        };
        // How long after the time it asked for a wake's hook begins.
        let (hook, late) = match call {
            Call::Event(event) => (event.hook(), Duration::ZERO),
            Call::Wake(due) => (Hook::Timer, time::elapsed(*due, moment.now)),
        };
        let before = self.parts.stopwatch.elapsed();
        let ran = self.parts.slot.turn(|instance, limiter| match call {
            Call::Event(event) => instance.handle(event, &moment, limiter),
            Call::Wake(_) => instance.wake(&moment, limiter),
        })?;
        let took = self.parts.stopwatch.elapsed().saturating_sub(before);
        metrics::script_hook(hook, took);
        let micros = |time: Duration| u64::try_from(time.as_micros()).unwrap_or(u64::MAX);
        debug!(
            hook = hook.name(),
            took_us = micros(took),
            late_us = micros(late),
            waited_us = micros(waited),
            operations = match ran.outcome {
                Outcome::Ran { operations } => operations,
                _ => 0,
            },
            actions = ran.actions.len(),
            "a hook of the bot's script ran"
        );
        // The hook held this worker for up to its 5 ms. Whoever else waits
        // for one gets it now, before this script's next hook and before
        // the next script's: the turn is given back only after that, so
        // the scripts that wait for it stay out of the workers' queues
        // meanwhile (ADR-0043).
        tokio::task::yield_now().await;
        drop(turn);
        Some(ran)
    }

    /// What follows a call on the instance: what it was refused is
    /// counted, a stop is reported, and its actions are performed.
    /// `stopping` is `on_stop`, whose message goes out at once.
    async fn after(&mut self, ran: Option<Ran>, stopping: bool) -> ControlFlow<Ended> {
        let Some(Ran {
            actions,
            outcome,
            refused,
            ..
        }) = ran
        else {
            return ControlFlow::Continue(());
        };
        self.refused(&refused);
        match outcome {
            Outcome::Stopped(error) => self.stopped(ScriptStatus::Stopped(error)),
            Outcome::Ended { error: Some(error) } => self.log(ScriptLog::StopFailed(error)),
            Outcome::Ended { error: None }
            | Outcome::Ran { .. }
            | Outcome::NotDefined
            | Outcome::Over => {}
        }
        self.perform(actions, stopping).await
    }

    /// Counts what a call was refused; what it was refused of its chat
    /// goes into the script's log too, at most 20 lines a minute (ADR-0037,
    /// C7), and every refusal in a screen or a sign editor, which the click
    /// limit bounds (Plan.md P16.13).
    fn refused(&mut self, refused: &[Refused]) {
        for entry in refused {
            metrics::script_refused(entry.reason, entry.times);
            match entry.asked {
                Asked::Send | Asked::Expect => self.note(ScriptLog::Refused(*entry)),
                Asked::Click | Asked::Screen | Asked::Sign => self.log(ScriptLog::Refused(*entry)),
                Asked::Press
                | Asked::Look
                | Asked::Hotbar
                | Asked::Vehicle
                | Asked::Timer
                | Asked::Log => {}
            }
        }
    }

    /// The script stopped: it failed or hit a limit. The bot stays online
    /// and idle, and the app hears why.
    fn stopped(&mut self, status: ScriptStatus) {
        if let ScriptStatus::Stopped(error) = &status {
            // The message is the script's own text, or Rhai's about it:
            // only at `debug`, like chat.
            info!(
                hook = error.hook.name(),
                line = error.at.map(|at| at.line),
                column = error.at.map(|at| at.column),
                reason = status.stop_label(),
                "the bot's script stopped; the bot stays online and idle"
            );
            debug!(%error, "why the script stopped");
        }
        if let Some(label) = status.stop_label() {
            metrics::script_stopped(label);
        }
        self.parts.slot.watch(ChatWatch::clear);
        self.publish(ScriptEvent::Status(status));
    }

    /// Performs what a hook asked for, in order. A failed action is
    /// skipped; a held input the session didn't take is the script's to
    /// know. Breaks when the session has ended.
    async fn perform(&mut self, actions: Vec<Action>, stopping: bool) -> ControlFlow<Ended> {
        // The sends of this hook that the chat queue refused at once.
        let mut unsent: Vec<SendId> = Vec::new();
        for action in actions {
            match action {
                Action::Log(line) => self.log(ScriptLog::Line(line)),
                Action::Send { id, key, message } => {
                    if stopping {
                        self.send_now(key, message).await?;
                    } else if let Err(error) =
                        self.chat
                            .send_script(id, key.clone(), message, &self.report)
                    {
                        unsent.push(id);
                        self.not_sent(id, key, NotSent::Refused(error));
                    }
                }
                Action::Expect {
                    send,
                    pattern,
                    window,
                } => {
                    if unsent.contains(&send) {
                        self.note(ScriptLog::WindowDropped { pattern });
                    } else {
                        // The instance has checked the length already.
                        let asked = self
                            .parts
                            .slot
                            .watch(|watch| watch.expect(send, pattern, window));
                        if let Err(error) = asked {
                            debug!(%error, "a reply's window wasn't taken");
                        }
                    }
                }
                Action::Menu(command) => self.menu(command).await?,
                input => self.act(&input).await?,
            }
        }
        ControlFlow::Continue(())
    }

    /// Does what a hook asked in a screen or a sign editor. The session
    /// decides it again at the moment of sending (ADR-0050); what it doesn't
    /// send is written into the log, and an action that wasn't sent is
    /// heard in `on_click` before anything else, so the script may act
    /// again.
    async fn menu(&mut self, command: MenuCommand) -> ControlFlow<Ended> {
        let (asked, slot) = match &command {
            MenuCommand::Act(action) => (Asked::Click, action.slot()),
            MenuCommand::Close(_) | MenuCommand::OpenInventory => (Asked::Screen, None),
            MenuCommand::AnswerSign { .. } => (Asked::Sign, None),
        };
        let action = asked == Asked::Click;
        // The command holds a sign's text, which is the script's own: only
        // at `debug`, like chat.
        debug!(
            ?command,
            "the script does something in a screen or a sign editor"
        );
        // Bounded by the session's own timeout.
        let error = match self.session.menu(command).await {
            Ok(()) => {
                if action {
                    self.awaiting = Some(Awaiting {
                        slot,
                        since: self.clock.now(),
                    });
                }
                return ControlFlow::Continue(());
            }
            Err(MenuError::Session(SessionError::Closed)) => {
                return ControlFlow::Break(Ended::Session);
            }
            Err(error) => error,
        };
        let reason = match error {
            MenuError::Refused(refusal) => Refusal::from(refusal),
            MenuError::Sign(refusal) => Refusal::from(refusal),
            MenuError::Session(SessionError::TimedOut) => {
                // It may still go out: an action waits for its fate, which
                // the session's gate gives it, or the runner's patience.
                let level = self
                    .failures
                    .level(FailureKind::Session(SessionError::TimedOut));
                warn_or_debug!(
                    level,
                    "the session didn't answer an action of the bot's script in time"
                );
                if action {
                    self.awaiting = Some(Awaiting {
                        slot,
                        since: self.clock.now(),
                    });
                } else {
                    self.log(ScriptLog::Menu {
                        asked,
                        slot,
                        reason: Refusal::Unanswered,
                        sent: true,
                    });
                }
                return ControlFlow::Continue(());
            }
            MenuError::Session(SessionError::NotInWorld) => Refusal::NotInWorld,
            // A session too busy to take it: as an action that came too
            // soon.
            MenuError::Session(
                SessionError::QueueFull | SessionError::ChatUnavailable | SessionError::Closed,
            ) => Refusal::TooSoon,
        };
        self.log(ScriptLog::Menu {
            asked,
            slot,
            reason,
            sent: false,
        });
        if action {
            self.queued = Some(ClickOutcome {
                slot,
                answer: Err(reason),
            });
        }
        ControlFlow::Continue(())
    }

    /// Performs one of a script's inputs in the session.
    async fn act(&mut self, action: &Action) -> ControlFlow<Ended> {
        let Some((game, held)) = game_action(action) else {
            return ControlFlow::Continue(());
        };
        match self.session.perform(game).await {
            Ok(()) => {
                self.released = game == GameAction::ReleaseAll;
                ControlFlow::Continue(())
            }
            Err(SessionError::Closed) => {
                debug!("the session has ended; the script's runner stops");
                ControlFlow::Break(Ended::Session)
            }
            Err(error) => {
                let level = self.failures.level(FailureKind::Session(error));
                warn_or_debug!(level, action = ?game, %error, "an action of the bot's script failed; skipped");
                if let Some(input) = held {
                    // The session doesn't hold it, so the script doesn't.
                    self.parts.slot.turn(|instance, _| instance.let_go([input]));
                }
                ControlFlow::Continue(())
            }
        }
    }

    /// Sends `on_stop`'s one message at once, by the bot's chat rate: the
    /// queue's turn would come after the session it's meant for.
    async fn send_now(&mut self, key: Ident, message: ChatMessage) -> ControlFlow<Ended> {
        if !self.chat.take_token() {
            self.note(ScriptLog::NotSent {
                key,
                reason: NotSent::Refused(ChatError::RateLimited),
            });
            return ControlFlow::Continue(());
        }
        // Bounded by the session's own timeout.
        match self.session.send_chat(message.clone()).await {
            Ok(()) => {
                self.publish_kind(FleetEventKind::ModeChatSent {
                    message: message.clone(),
                });
                self.log(ScriptLog::Sent { key, message });
                ControlFlow::Continue(())
            }
            Err(SessionError::Closed) => ControlFlow::Break(Ended::Session),
            Err(error) => {
                self.note(ScriptLog::NotSent {
                    key,
                    reason: NotSent::Failed(ChatFailure::from(error)),
                });
                ControlFlow::Continue(())
            }
        }
    }

    /// Ends the script's time in this session, as `ended` says.
    async fn finish(&mut self, ended: Ended) -> Next {
        match ended {
            Ended::Session => {
                // A fate still to come never will.
                self.awaiting = None;
                self.queued = None;
                // A session that ended holds nothing, and a new one starts
                // with nothing held. The state stays for the next join.
                self.parts.slot.turn(|instance, _| instance.disconnected());
                self.parts.slot.watch(ChatWatch::clear);
                Next::End
            }
            Ended::Stop => {
                self.orderly().await;
                Next::End
            }
            Ended::Changed(definition) => {
                self.orderly().await;
                Next::Changed(definition)
            }
        }
    }

    /// An orderly stop: `on_stop`, then everything is released, and the
    /// script's run is over.
    async fn orderly(&mut self) {
        let seen = match self.see().await {
            Ok(seen) => seen,
            Err(Unseen::Closed | Unseen::NotInWorld | Unseen::Busy) => self.unplaced(),
        };
        let presence = self.presence(seen.placed);
        let ran = self
            .turn(&Call::Event(&HookEvent::Stop), &seen, presence)
            .await;
        // The instance's last action is a `ReleaseAll`. If the session
        // didn't take it, the mark stays, and whoever owns this task
        // releases once more when it has ended.
        let open = self.after(ran, true).await.is_continue();
        self.parts.slot.end_run();
        if open && self.released {
            self.parts.held.store(false, Ordering::SeqCst);
        }
        info!("the bot's script ended");
    }

    /// Writes a line into the script's log.
    fn log(&self, entry: ScriptLog) {
        // A line is the script's own text: only at `debug`, like chat.
        debug!(?entry, "the bot's script logged");
        self.publish(ScriptEvent::Log(entry));
    }

    /// Writes a line about something the script was refused, if this
    /// minute has room for one more: a script that asks again and again
    /// can't flood its log.
    fn note(&self, entry: ScriptLog) {
        if self.parts.slot.may_note(self.clock.now()) {
            self.log(entry);
        } else {
            metrics::script_notes_dropped();
        }
    }

    fn publish(&self, event: ScriptEvent) {
        self.publish_kind(FleetEventKind::Script(event));
    }

    fn publish_kind(&self, kind: FleetEventKind) {
        let event = FleetEvent {
            bot_id: self.parts.bot_id,
            at: self.clock.now(),
            kind,
        };
        // An error only means nobody subscribes right now, so nobody
        // misses the event.
        let _ = self.parts.events.send(event);
    }
}

/// One of a script's inputs as the session's action, and the input it
/// holds down, if it holds one. `None` for what is no input: a message, a
/// window, a log line.
fn game_action(action: &Action) -> Option<(GameAction, Option<Input>)> {
    Some(match action {
        Action::ReleaseAll => (GameAction::ReleaseAll, None),
        Action::ReleaseKeys => (GameAction::ReleaseKeys, None),
        Action::Press { input, kind } => match kind {
            PressKind::Tap => (GameAction::Tap(*input), None),
            PressKind::Hold => (GameAction::Press(*input), Some(*input)),
            PressKind::HoldUntilUsed => (GameAction::HoldUntilUsed, Some(*input)),
        },
        Action::Release(input) => (GameAction::Release(*input), None),
        Action::LookAt { yaw, pitch } => (
            GameAction::Look {
                yaw: narrow(*yaw),
                pitch: narrow(*pitch),
            },
            None,
        ),
        Action::LookTurn { yaw, pitch } => (
            GameAction::Turn {
                yaw: narrow(*yaw),
                pitch: narrow(*pitch),
            },
            None,
        ),
        Action::LookAtPosition(at) => (GameAction::LookAtPosition(*at), None),
        Action::SelectHotbar(slot) => (GameAction::SelectHotbarSlot { slot: *slot }, None),
        // Only a minecart has a seat, and the instance refuses any other
        // kind (ADR-0042, G1).
        Action::Mount(_) => (GameAction::Mount, None),
        Action::Dismount => (GameAction::Dismount, None),
        Action::Send { .. } | Action::Expect { .. } | Action::Log(_) | Action::Menu(_) => {
            return None;
        }
    })
}

/// An angle as the session takes it. Written out and read back: the
/// nearest `f32`, without a cast that the lints here allow none of. An
/// angle too large for one isn't finite, and the session skips it.
fn narrow(angle: f64) -> f32 {
    angle.to_string().parse().unwrap_or(f32::NAN)
}

/// Sleeps until `deadline`, or forever without one.
async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => core::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use fleet_core::mode::HotbarSlot;
    use fleet_core::script::{Identifier, Key, MouseButton};

    use super::*;

    fn identifier(text: &str) -> Identifier {
        Identifier::try_from(text).unwrap()
    }

    #[test]
    fn every_input_of_a_script_is_the_sessions_action() {
        let forward = Input::Key(Key::Forward);
        let right = Input::Mouse(MouseButton::Right);
        let at = Position::try_new(1.0, 2.0, 3.0).unwrap();
        let slot = HotbarSlot::try_from(3).unwrap();
        let press = |input, kind| Action::Press { input, kind };

        assert_eq!(
            game_action(&Action::ReleaseAll),
            Some((GameAction::ReleaseAll, None))
        );
        assert_eq!(
            game_action(&Action::ReleaseKeys),
            Some((GameAction::ReleaseKeys, None))
        );
        assert_eq!(
            game_action(&press(forward, PressKind::Tap)),
            Some((GameAction::Tap(forward), None))
        );
        assert_eq!(
            game_action(&press(forward, PressKind::Hold)),
            Some((GameAction::Press(forward), Some(forward)))
        );
        assert_eq!(
            game_action(&press(right, PressKind::HoldUntilUsed)),
            Some((GameAction::HoldUntilUsed, Some(right)))
        );
        assert_eq!(
            game_action(&Action::Release(forward)),
            Some((GameAction::Release(forward), None))
        );
        assert_eq!(
            game_action(&Action::LookAt {
                yaw: 90.0,
                pitch: -12.5
            }),
            Some((
                GameAction::Look {
                    yaw: 90.0,
                    pitch: -12.5
                },
                None
            ))
        );
        assert_eq!(
            game_action(&Action::LookTurn {
                yaw: -45.0,
                pitch: 0.25
            }),
            Some((
                GameAction::Turn {
                    yaw: -45.0,
                    pitch: 0.25
                },
                None
            ))
        );
        assert_eq!(
            game_action(&Action::LookAtPosition(at)),
            Some((GameAction::LookAtPosition(at), None))
        );
        assert_eq!(
            game_action(&Action::SelectHotbar(slot)),
            Some((GameAction::SelectHotbarSlot { slot }, None))
        );
        assert_eq!(
            game_action(&Action::Mount(identifier("minecart"))),
            Some((GameAction::Mount, None))
        );
        assert_eq!(
            game_action(&Action::Dismount),
            Some((GameAction::Dismount, None))
        );
    }

    #[test]
    fn a_log_line_is_no_input() {
        assert_eq!(game_action(&Action::Log("hello".to_owned())), None);
    }

    #[test]
    fn an_angle_is_the_nearest_the_session_takes() {
        assert!((narrow(90.0) - 90.0).abs() < f32::EPSILON);
        assert!((narrow(-0.1) + 0.1).abs() < f32::EPSILON);
        assert!((narrow(179.999_999_999) - 180.0).abs() < f32::EPSILON);
        // Too large for the session's number: not finite, so it's skipped.
        assert!(!narrow(1e300).is_finite());
        assert!(narrow(f64::NAN).is_nan());
    }
}
