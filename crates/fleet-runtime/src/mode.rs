//! [`ModeRunner`]: runs a bot's mode in one session.

use core::ops::ControlFlow;

use fleet_core::mc::{SessionError, SessionHandle};
use fleet_core::mode::{GameAction, ModeDefinition, ModePlan, PlanTick, PlannedAction};
use rand::rngs::StdRng;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::chat::{ChatError, ModeChat};
use crate::clock::RuntimeClock;
use crate::failure_log::{FailureKind, FailureLog, warn_or_debug};

/// What a mode change does before the new mode starts: let go of the use
/// button and stop sneaking, so nothing of the old mode is left held down
/// (ADR-0013). The selected hotbar slot stays.
const LET_GO: [PlannedAction; 2] = [
    PlannedAction::Game(GameAction::HoldUse { on: false }),
    PlannedAction::Game(GameAction::Sneak { on: false }),
];

/// Runs a bot's mode in one session (Plan.md P4.4; ADR-0013).
///
/// It drives fleet-core's `ModePlan`: it runs the at-start steps at once,
/// then sleeps until the next step is due, runs it and sleeps again. Game
/// actions go to the session, chat goes through the bot's chat queue.
///
/// - **A failed step is skipped**, never fatal. The first failure of each
///   kind in the session logs at `warn`, later ones at `debug`. Mode chat
///   that finds the queue closed logs at `debug` only, since the queue
///   closes on a disconnect anyway.
/// - **A mode change** comes through the `watch`. The runner first lets go
///   (`HoldUse{on: false}`, `Sneak{on: false}`), then starts the new mode
///   with its at-start steps; the selected hotbar slot stays. An equal
///   definition changes nothing.
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
    failures: FailureLog,
}

impl<S: SessionHandle> ModeRunner<S> {
    /// Builds a runner for `session` that sends mode chat through `chat`,
    /// reads the time from `clock`, makes its random choices with `rng` and
    /// runs the mode that `mode` holds.
    #[must_use]
    pub fn new(
        session: S,
        chat: ModeChat,
        clock: RuntimeClock,
        rng: StdRng,
        mode: watch::Receiver<ModeDefinition>,
    ) -> Self {
        Self {
            session,
            chat,
            clock,
            rng,
            mode,
            failures: FailureLog::default(),
        }
    }

    /// Runs the mode until `cancel` is cancelled. It also ends by itself when
    /// the session has ended (a call fails with `SessionError::Closed`), or
    /// when the owner drops the mode's `watch` sender.
    ///
    /// A cancellation interrupts the actions of a tick; a mode change waits
    /// until they're done.
    pub async fn run(mut self, cancel: CancellationToken) {
        let mut definition = self.mode.borrow_and_update().clone();
        let (mut plan, first) = ModePlan::start(&definition, self.clock.now(), &mut self.rng);
        let mut next_due = first.next_due;
        if self.run_actions(first.actions, &cancel).await.is_break() {
            return;
        }
        loop {
            let deadline = next_due.and_then(|due| self.clock.deadline(due));
            let tick = tokio::select! {
                biased;
                // All three branches are cancel-safe: `cancelled` and
                // `changed` lose nothing when another branch wins, and the
                // sleep starts over from `next_due` on the next turn.
                () = cancel.cancelled() => return,
                changed = self.mode.changed() => {
                    // An error means the owner dropped the sender.
                    if changed.is_err() {
                        return;
                    }
                    let changed = self.mode.borrow_and_update().clone();
                    if changed == definition {
                        continue;
                    }
                    definition = changed;
                    let (new_plan, first) =
                        ModePlan::start(&definition, self.clock.now(), &mut self.rng);
                    plan = new_plan;
                    PlanTick {
                        actions: LET_GO.into_iter().chain(first.actions).collect(),
                        next_due: first.next_due,
                    }
                }
                () = sleep_until(deadline) => plan.tick(self.clock.now(), &mut self.rng),
            };
            next_due = tick.next_due;
            if self.run_actions(tick.actions, &cancel).await.is_break() {
                return;
            }
        }
    }

    /// Runs `actions` in order, unless `cancel` comes first. Breaks when the
    /// runner must stop: it was cancelled, or the session has ended.
    async fn run_actions(
        &mut self,
        actions: Vec<PlannedAction>,
        cancel: &CancellationToken,
    ) -> ControlFlow<()> {
        tokio::select! {
            biased;
            // Cancel-safe: dropping the actions drops at most one session
            // call, which the session still finishes or drops on its own.
            () = cancel.cancelled() => ControlFlow::Break(()),
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
