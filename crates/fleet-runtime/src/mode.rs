//! [`ModeRunner`]: runs a bot's mode in one session.

use core::ops::ControlFlow;

use fleet_core::mc::{SessionError, SessionHandle};
use fleet_core::mode::{GameAction, ModeBody, ModeDefinition, ModePlan, PlanTick, PlannedAction};
use fleet_core::script::Script;
use rand::rngs::StdRng;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::chat::{ChatError, ModeChat};
use crate::clock::RuntimeClock;
use crate::failure_log::{FailureKind, FailureLog, warn_or_debug};
use crate::script::{Borrowed, Next, ScriptParts, ScriptRunner};

/// What a mode change does before the new mode starts: let go of the use
/// button and stop sneaking, so nothing of the old mode is left held down
/// (ADR-0013). The selected hotbar slot stays.
const LET_GO: [PlannedAction; 2] = [
    PlannedAction::Game(GameAction::HoldUse { on: false }),
    PlannedAction::Game(GameAction::Sneak { on: false }),
];

/// Runs a bot's mode in one session (Plan.md P4.4, P15.16; ADR-0013).
///
/// **A step mode.** It drives fleet-core's `ModePlan`: it runs the at-start
/// steps at once, then sleeps until the next step is due, runs it and
/// sleeps again. Game actions go to the session, chat goes through the
/// bot's chat queue.
///
/// - **A failed step is skipped**, never fatal. The first failure of each
///   kind in the session logs at `warn`, later ones at `debug`. Mode chat
///   that finds the queue closed logs at `debug` only, since the queue
///   closes on a disconnect anyway.
/// - **A mode change** comes through the `watch`. The runner first lets go
///   (`HoldUse{on: false}`, `Sneak{on: false}`), then starts the new mode
///   with its at-start steps; the selected hotbar slot stays. An equal
///   definition changes nothing.
/// - **A respawn the server confirmed** comes through the actor's count.
///   For a mode with on-respawn steps the runner lets go, then runs them,
///   an on-respawn chat skipped within 30 s of the last one; a mode without
///   any does nothing, exactly as before, and the repeating steps keep
///   their times (ADR-0020, S24).
///
/// **A scripted mode** runs in the same task, through its script runner
/// (the `script` module), once the runner was given what a script needs
/// ([`scripted`](Self::scripted)). A change from a script to another mode
/// ends the script in an orderly way first: `on_stop`, then everything it
/// holds is released. Without those parts a scripted mode does nothing
/// here, as a mode without steps.
///
/// One runner lives as long as its session is Online. The bot's actor starts
/// it after it opened the chat queue, and cancels it before it closes the
/// queue (ADR-0013).
#[derive(Debug)]
pub struct ModeRunner<S> {
    session: S,
    chat: ModeChat,
    clock: RuntimeClock,
    rng: StdRng,
    mode: watch::Receiver<ModeDefinition>,
    respawns: watch::Receiver<u64>,
    failures: FailureLog,
    script: Option<ScriptParts>,
}

impl<S: SessionHandle> ModeRunner<S> {
    /// Builds a runner for `session` that sends mode chat through `chat`,
    /// reads the time from `clock`, makes its random choices with `rng`,
    /// runs the mode that `mode` holds, and runs its on-respawn steps each
    /// time `respawns` changes: the actor counts there the respawns the
    /// server confirmed (ADR-0020, S23, S24). A change before the receiver
    /// was made never reaches the runner.
    #[must_use]
    pub fn new(
        session: S,
        chat: ModeChat,
        clock: RuntimeClock,
        rng: StdRng,
        mode: watch::Receiver<ModeDefinition>,
        respawns: watch::Receiver<u64>,
    ) -> Self {
        Self {
            session,
            chat,
            clock,
            rng,
            mode,
            respawns,
            failures: FailureLog::default(),
            script: None,
        }
    }

    /// Gives the runner what it needs to run a scripted mode in its
    /// session: the bot's slot, the session's feed, where its events go
    /// and what its hooks are timed by. Its `stop` token also ends a step
    /// mode, at once.
    #[must_use]
    pub fn scripted(mut self, parts: ScriptParts) -> Self {
        self.script = Some(parts);
        self
    }

    /// Runs the mode until `cancel` is cancelled. It also ends by itself when
    /// the session has ended (a call fails with `SessionError::Closed`),
    /// when the owner drops the mode's `watch` sender, or when the owner
    /// asks for an orderly stop.
    ///
    /// A cancellation interrupts the actions of a tick; a mode change waits
    /// until they're done.
    pub async fn run(mut self, cancel: CancellationToken) {
        let mut definition = self.mode.borrow_and_update().clone();
        let mut after_script = false;
        loop {
            let next = match definition.body() {
                ModeBody::Steps(_) => {
                    if after_script {
                        // A respawn that came while a script ran was the
                        // script's to answer, not these steps'.
                        self.respawns.borrow_and_update();
                    }
                    self.run_steps(&definition, &cancel).await
                }
                ModeBody::Script(script) => {
                    after_script = true;
                    self.run_script(&definition, script, &cancel).await
                }
            };
            match next {
                Next::End => return,
                Next::Changed(changed) => definition = changed,
            }
        }
    }

    /// Runs the scripted mode `definition`, whose body is `script`, until
    /// it ends or the bot's mode changes.
    async fn run_script(
        &mut self,
        definition: &ModeDefinition,
        script: &Script,
        cancel: &CancellationToken,
    ) -> Next {
        let Some(parts) = self.script.as_mut() else {
            debug!("the runner has nothing to run a script with; the mode does nothing here");
            return self.idle(definition, cancel).await;
        };
        let borrowed = Borrowed {
            session: &self.session,
            chat: &self.chat,
            clock: self.clock,
            rng: &mut self.rng,
            mode: &mut self.mode,
            failures: &mut self.failures,
            parts,
        };
        ScriptRunner::new(borrowed, definition, script, cancel)
            .run()
            .await
    }

    /// Waits until the bot's mode is another one than `definition`, or
    /// the runner ends.
    async fn idle(&mut self, definition: &ModeDefinition, cancel: &CancellationToken) -> Next {
        loop {
            tokio::select! {
                biased;
                // Both branches are cancel-safe: neither loses anything
                // when the other one wins.
                () = cancel.cancelled() => return Next::End,
                changed = self.mode.changed() => {
                    // An error means the owner dropped the sender.
                    if changed.is_err() {
                        return Next::End;
                    }
                    let changed = self.mode.borrow_and_update().clone();
                    if changed != *definition {
                        return Next::Changed(changed);
                    }
                }
            }
        }
    }

    /// Runs the step mode `start`, and every step mode the bot's mode
    /// changes to, until the runner ends or the mode becomes a script.
    async fn run_steps(&mut self, start: &ModeDefinition, cancel: &CancellationToken) -> Next {
        let mut definition = start.clone();
        let stop = self.script.as_ref().map(|parts| parts.stop.clone());
        let (mut plan, first) = ModePlan::start(&definition, self.clock.now(), &mut self.rng);
        let mut next_due = first.next_due;
        if self
            .run_actions(first.actions, cancel, stop.as_ref())
            .await
            .is_break()
        {
            return Next::End;
        }
        loop {
            let deadline = next_due.and_then(|due| self.clock.deadline(due));
            let tick = tokio::select! {
                biased;
                // Every branch is cancel-safe: `cancelled` and both
                // `changed` lose nothing when another branch wins, and the
                // sleep starts over from `next_due` on the next turn. A
                // mode change goes before a respawn, so a changed mode's
                // on-respawn steps never run once the change is known.
                () = cancel.cancelled() => return Next::End,
                // An orderly stop: a step mode has nothing to end with.
                () = stopped(stop.as_ref()) => return Next::End,
                changed = self.mode.changed() => {
                    // An error means the owner dropped the sender.
                    if changed.is_err() {
                        return Next::End;
                    }
                    let changed = self.mode.borrow_and_update().clone();
                    if changed == definition {
                        continue;
                    }
                    if matches!(changed.body(), ModeBody::Script(_)) {
                        // A script takes over: nothing of these steps stays
                        // held down under it.
                        let let_go = LET_GO.into_iter().collect();
                        if self.run_actions(let_go, cancel, stop.as_ref()).await.is_break() {
                            return Next::End;
                        }
                        return Next::Changed(changed);
                    }
                    definition = changed;
                    // Keeps the on-respawn chat's 30 s memory (S24).
                    let first = plan.switch(&definition, self.clock.now(), &mut self.rng);
                    PlanTick {
                        actions: LET_GO.into_iter().chain(first.actions).collect(),
                        next_due: first.next_due,
                    }
                }
                respawned = self.respawns.changed() => {
                    if respawned.is_err() {
                        return Next::End;
                    }
                    // `changed` marked the value seen, so several respawns
                    // since the last turn run the steps once.
                    let Some(respawn) = plan.respawned(self.clock.now(), &mut self.rng) else {
                        // A mode without on-respawn steps: nothing, as before.
                        continue;
                    };
                    if respawn.chat_skipped {
                        debug!("the on-respawn chat was skipped: the last one went out less than 30 s before");
                    }
                    PlanTick {
                        actions: LET_GO.into_iter().chain(respawn.actions).collect(),
                        next_due: plan.next_due(),
                    }
                }
                () = sleep_until(deadline) => plan.tick(self.clock.now(), &mut self.rng),
            };
            next_due = tick.next_due;
            if self
                .run_actions(tick.actions, cancel, stop.as_ref())
                .await
                .is_break()
            {
                return Next::End;
            }
        }
    }

    /// Runs `actions` in order, unless `cancel` or an orderly `stop` comes
    /// first. Breaks when the runner must stop: it was cancelled or
    /// stopped, or the session has ended.
    async fn run_actions(
        &mut self,
        actions: Vec<PlannedAction>,
        cancel: &CancellationToken,
        stop: Option<&CancellationToken>,
    ) -> ControlFlow<()> {
        tokio::select! {
            biased;
            // Cancel-safe: dropping the actions drops at most one session
            // call, which the session still finishes or drops on its own.
            () = cancel.cancelled() => ControlFlow::Break(()),
            () = stopped(stop) => ControlFlow::Break(()),
            flow = self.perform_all(actions) => flow,
        }
    }

    /// Runs each action and skips the ones that fail.
    async fn perform_all(&mut self, actions: Vec<PlannedAction>) -> ControlFlow<()> {
        for action in actions {
            match action {
                PlannedAction::Game(action) => match self.session.perform(action).await {
                    Ok(()) => {}
                    Err(SessionError::Closed) => {
                        debug!("the session has ended; the mode stops");
                        return ControlFlow::Break(());
                    }
                    Err(error) => {
                        let level = self.failures.level(FailureKind::Session(error));
                        warn_or_debug!(level, ?action, %error, "a mode action failed; skipped");
                    }
                },
                PlannedAction::Chat(message) => match self.chat.send(message) {
                    Ok(()) => {}
                    // The queue closes when the session goes, so this is no
                    // fault worth a warning (ADR-0013).
                    Err(error @ ChatError::NotOnline) => {
                        debug!(%error, "mode chat skipped");
                    }
                    Err(error) => {
                        let level = self.failures.level(FailureKind::Chat(error));
                        warn_or_debug!(level, %error, "mode chat skipped");
                    }
                },
            }
        }
        ControlFlow::Continue(())
    }
}

/// Sleeps until `deadline`, or forever without one: a mode with no repeating
/// step waits only for a change or a cancellation.
async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => core::future::pending().await,
    }
}

/// Waits for an orderly stop, or forever for a runner that can't be asked
/// for one.
async fn stopped(stop: Option<&CancellationToken>) {
    match stop {
        Some(stop) => stop.cancelled().await,
        None => core::future::pending().await,
    }
}
