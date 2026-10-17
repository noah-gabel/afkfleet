//! The five lifecycle calls (Plan.md P11.2; ADR-0019, S48–S51, S53).
//!
//! **Start** and **stop** only write whether the bot should run, which the
//! reconciler then carries out, whatever its agent last reported: so they
//! always act, and stopping a Paused or Failed bot frees its agent's slot
//! while it keeps its state (S51). **Restart**, and **resume** and **reset**
//! of a bot that runs on an agent, are one-shot commands: recorded first,
//! then sent to the live stream that holds the bot, never stored or replayed,
//! so none can fire later over a human who came back (S48). **Resume** and
//! **reset** of a bot that runs nowhere clear its stored Paused or Failed
//! state instead, and the latest report the server keeps, in their
//! transaction, so its next placement starts it. fleet-core's
//! [`judge_command`] decides which.

use fleet_core::audit::{AuditAction, AuditError, AuditMetadata};
use fleet_core::authz::{Permission, authorize};
use fleet_core::bot::{
    CommandEffect, CommandFacts, CommandRefusal, DesiredRunState, LastState, Lifecycle, Reach,
    judge_command,
};
use fleet_core::id::BotId;
use tracing::info;

use super::{BotService, BotWriteError, reread};
use crate::app::users::ActingUser;
use crate::ports::bots::Bot;
use crate::ports::live_agents::CommandRefused;
use crate::ports::reconcile::Change;
use crate::ports::store::WriteTx;

/// A user's lifecycle call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotAction {
    /// The bot should run.
    Start,
    /// The bot should be stopped.
    Stop,
    /// Restart, reset or resume its current run.
    Command(Lifecycle),
}

/// What a lifecycle call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Controlled {
    /// Its desired state changed, with its audit entry.
    Changed,
    /// Its desired state says so already: nothing was written.
    Unchanged,
    /// The command was recorded and sent to the stream that holds the bot.
    Sent,
    /// The bot ran nowhere, and its stored sticky state was cleared.
    Cleared,
}

impl BotService {
    /// Carries out `action` on the bot `id`, for `by` (`ControlBot`); see the
    /// [module docs](self).
    ///
    /// # Errors
    /// [`BotWriteError::SessionGone`], [`BotWriteError::NotFound`],
    /// [`BotWriteError::Denied`], [`BotWriteError::Refused`] for a command
    /// that can't act now (after its entry was recorded, when its stream
    /// ended meanwhile), and the server's failures.
    pub async fn control(
        &self,
        by: &ActingUser,
        id: BotId,
        action: BotAction,
    ) -> Result<Controlled, BotWriteError> {
        let mut tx = self.store.write().await?;
        let (actor, bot, account) = reread(tx.as_mut(), by, id).await?;
        authorize(&actor, Permission::ControlBot, &account.context())
            .map_err(BotWriteError::Denied)?;
        match action {
            BotAction::Start => {
                self.desire(
                    tx,
                    by,
                    &bot,
                    DesiredRunState::Running,
                    AuditAction::BOT_START,
                )
                .await
            }
            BotAction::Stop => {
                self.desire(
                    tx,
                    by,
                    &bot,
                    DesiredRunState::Stopped,
                    AuditAction::BOT_STOP,
                )
                .await
            }
            BotAction::Command(command) => self.command(tx, by, &bot, command).await,
        }
    }

    /// Writes whether `bot` should run, with `action`'s entry.
    async fn desire(
        &self,
        mut tx: Box<dyn WriteTx>,
        by: &ActingUser,
        bot: &Bot,
        desired: DesiredRunState,
        action: AuditAction,
    ) -> Result<Controlled, BotWriteError> {
        if !tx.bots().set_desired(bot.id, desired).await? {
            // It says so already; dropping the transaction writes nothing.
            return Ok(Controlled::Unchanged);
        }
        let (_, known) = self.reach(bot);
        let metadata = acted_on(known)?;
        tx.commit(self.entry(by, action, bot.id, metadata)).await?;
        self.reconcile.changed(Change::Store);
        info!(user_id = %by.actor.id, bot_id = %bot.id, desired = desired.as_str(), "a bot's desired state was set");
        Ok(Controlled::Changed)
    }

    /// Restarts, resets or resumes `bot`'s run: recorded, then sent to the
    /// stream that holds it, or its stored sticky state cleared where it
    /// runs nowhere; see the [module docs](self).
    async fn command(
        &self,
        mut tx: Box<dyn WriteTx>,
        by: &ActingUser,
        bot: &Bot,
        command: Lifecycle,
    ) -> Result<Controlled, BotWriteError> {
        let (reach, known) = self.reach(bot);
        let facts = CommandFacts {
            desired: bot.desired,
            reach,
            known,
        };
        // A refusal drops the transaction: nothing is written or recorded.
        let effect = judge_command(command, facts).map_err(BotWriteError::Refused)?;
        let action = match command {
            Lifecycle::Restart => AuditAction::BOT_RESTART,
            Lifecycle::Reset => AuditAction::BOT_RESET,
            Lifecycle::Resume => AuditAction::BOT_RESUME,
        };
        match effect {
            CommandEffect::Send => {
                let metadata = effected(command, known, "sent")?;
                // Recorded before it's sent: nothing reaches an agent
                // unaudited, and a refusal after the commit keeps its entry.
                tx.commit(self.entry(by, action, bot.id, metadata)).await?;
                self.reconcile.changed(Change::Store);
                let Some(agent) = bot.agent else {
                    return Err(BotWriteError::Refused(CommandRefusal::Unreachable));
                };
                match self.live.command(agent, bot.id, command) {
                    Ok(()) => {
                        info!(user_id = %by.actor.id, bot_id = %bot.id, agent_id = %agent, command = ?command, "a command was sent to a bot's agent");
                        Ok(Controlled::Sent)
                    }
                    Err(
                        CommandRefused::NotHeld
                        | CommandRefused::NotConnected
                        | CommandRefused::QueueFull,
                    ) => Err(BotWriteError::Refused(CommandRefusal::Unreachable)),
                    Err(CommandRefused::Unsendable) => Err(BotWriteError::Unsendable),
                }
            }
            CommandEffect::ClearSticky => {
                let metadata = effected(command, known, "cleared")?;
                if let Some(last) = bot.last.filter(|last| last.state.sticky().is_some()) {
                    tx.bots().clear_sticky(bot.id, &last).await?;
                }
                // Taken before the commit, so no placement after it restores
                // what this clears; put back when the commit fails (S48).
                let taken = self.live.take_latest(bot.id);
                if let Err(error) = tx.commit(self.entry(by, action, bot.id, metadata)).await {
                    if let Some(taken) = taken {
                        self.live.put_back_latest(taken);
                    }
                    return Err(error.into());
                }
                self.reconcile.changed(Change::Store);
                info!(user_id = %by.actor.id, bot_id = %bot.id, command = ?command, "a resting bot's sticky state was cleared");
                Ok(Controlled::Cleared)
            }
        }
    }

    /// How far the server reaches `bot`, and its known state: its stream's
    /// report when that stream holds it, otherwise the newer of its stored
    /// state and the report the server keeps (S50, R35).
    fn reach(&self, bot: &Bot) -> (Reach, Option<LastState>) {
        let stored = self.last_known(bot.id, bot.last).map(|last| last.state);
        match bot.agent {
            None => (Reach::NoAgent, stored),
            Some(agent) => match self.live.held(agent, bot.id) {
                Some(held) => (
                    Reach::Held,
                    held.report
                        .map(|report| LastState::from(report.state))
                        .or(stored),
                ),
                None => (Reach::NotHeld, stored),
            },
        }
    }
}

/// The metadata of a call that acted on a bot in `known`: its state, and
/// its reason when it's paused or failed; nothing when no state is known.
fn acted_on(known: Option<LastState>) -> Result<AuditMetadata, AuditError> {
    let Some(state) = known else {
        return Ok(AuditMetadata::new());
    };
    let metadata = AuditMetadata::new().text("state", state.as_str())?;
    match state.reason() {
        Some(reason) => metadata.text("state_reason", reason),
        None => Ok(metadata),
    }
}

/// A command's metadata: [`acted_on`], and for a reset or a resume, which
/// can be sent or clear, its `effect`.
fn effected(
    command: Lifecycle,
    known: Option<LastState>,
    effect: &str,
) -> Result<AuditMetadata, AuditError> {
    let metadata = acted_on(known)?;
    match command {
        Lifecycle::Restart => Ok(metadata),
        Lifecycle::Reset | Lifecycle::Resume => metadata.text("effect", effect),
    }
}
