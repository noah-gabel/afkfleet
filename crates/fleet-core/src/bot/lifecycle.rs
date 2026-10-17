//! [`judge_command`]: what a user's one-shot lifecycle command does to a bot, decided
//! before the server acts on it (Plan.md P11.2; ADR-0019, S48–S50).
//!
//! Start and stop aren't commands: they only write the bot's desired state,
//! which the reconciler then carries out, so they always act. Restart, reset
//! and resume act once on the bot's current run. They go to the agent whose
//! live stream holds the bot, and are refused when no stream does, so a
//! command can never fire later, after an outage, over a human who came back
//! meanwhile (S48). One exception: resuming a Paused bot, or resetting a
//! Failed one, that runs nowhere clears its stored state instead, so its next
//! placement starts it.
//!
//! The bot's state machine has the last word ([`super::transition`]): the
//! judge only refuses what can't mean anything, so a user hears why at once
//! instead of nothing happening.

use super::{DesiredRunState, LastState};

/// A user's one-shot lifecycle command for a bot: what the bot's actor is
/// told to do with its current run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lifecycle {
    /// Stop the bot's run and start it again. Nothing happens while the bot
    /// should be stopped, or is Paused or Failed.
    Restart,
    /// Try a Failed bot again.
    Reset,
    /// Connect a Paused bot again.
    Resume,
}

impl Lifecycle {
    /// Every command, for the tests that check them all.
    pub const ALL: [Self; 3] = [Self::Restart, Self::Reset, Self::Resume];
}

/// How far the server reaches a bot right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Its row names no agent: it runs nowhere.
    NoAgent,
    /// Its row names an agent, but no live stream of it holds the bot
    /// assigned: the agent is gone, or its stream isn't reconciled yet.
    NotHeld,
    /// The live stream of its agent holds it assigned.
    Held,
}

/// What [`judge_command`] reads about a bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandFacts {
    /// Whether the bot should run.
    pub desired: DesiredRunState,
    /// How far the server reaches it.
    pub reach: Reach,
    /// Its known state: its stream's report, or else the newer of the stored
    /// state and the server's memory.
    pub known: Option<LastState>,
}

/// What the server does for a command [`judge_command`] lets through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandEffect {
    /// Send the command to the live stream that holds the bot.
    Send,
    /// Clear the bot's stored Paused or Failed state: it runs nowhere, so its
    /// next placement starts it instead of restoring it.
    ClearSticky,
}

/// Why a command can't act on a bot now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CommandRefusal {
    /// A restart of a bot that isn't running: it should be stopped, runs
    /// nowhere, or is Paused or Failed.
    #[error("only a running bot can be restarted")]
    NotRunning,
    /// A resume of a bot that isn't Paused.
    #[error("only a paused bot can be resumed")]
    NotPaused,
    /// A reset of a bot that hasn't Failed.
    #[error("only a failed bot can be reset")]
    NotFailed,
    /// The bot's agent has no live stream that holds it.
    #[error("the bot's agent can't be reached")]
    Unreachable,
}

/// What `command` does to the bot `facts` describe: sent to its stream,
/// its stored sticky state cleared, or refused with why.
///
/// # Errors
/// [`CommandRefusal`] when the command can't mean anything for the bot now:
/// a restart of a bot that isn't running, a resume of one that isn't Paused,
/// a reset of one that hasn't Failed, or any of them while the bot's agent
/// has no live stream that holds it.
pub const fn judge_command(
    command: Lifecycle,
    facts: CommandFacts,
) -> Result<CommandEffect, CommandRefusal> {
    match command {
        Lifecycle::Restart => restart(facts),
        Lifecycle::Resume => leave_sticky(facts, is_paused(facts.known), CommandRefusal::NotPaused),
        Lifecycle::Reset => leave_sticky(facts, is_failed(facts.known), CommandRefusal::NotFailed),
    }
}

/// A restart acts only on a run its stream holds: a bot that should run,
/// isn't Paused or Failed, and whose stream holds it.
const fn restart(facts: CommandFacts) -> Result<CommandEffect, CommandRefusal> {
    if matches!(facts.desired, DesiredRunState::Stopped) {
        return Err(CommandRefusal::NotRunning);
    }
    match facts.reach {
        Reach::NoAgent => Err(CommandRefusal::NotRunning),
        Reach::NotHeld => Err(CommandRefusal::Unreachable),
        Reach::Held => match facts.known {
            Some(LastState::Paused(_) | LastState::Failed(_)) => Err(CommandRefusal::NotRunning),
            _ => Ok(CommandEffect::Send),
        },
    }
}

/// A resume or a reset acts only on its own sticky state (`matches`): sent
/// to the stream that holds the bot, or cleared where it runs nowhere.
const fn leave_sticky(
    facts: CommandFacts,
    matches: bool,
    refusal: CommandRefusal,
) -> Result<CommandEffect, CommandRefusal> {
    match facts.reach {
        Reach::NotHeld => Err(CommandRefusal::Unreachable),
        Reach::NoAgent if matches => Ok(CommandEffect::ClearSticky),
        Reach::Held if matches => Ok(CommandEffect::Send),
        Reach::NoAgent | Reach::Held => Err(refusal),
    }
}

const fn is_paused(known: Option<LastState>) -> bool {
    matches!(known, Some(LastState::Paused(_)))
}

const fn is_failed(known: Option<LastState>) -> bool {
    matches!(known, Some(LastState::Failed(_)))
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::bot::{FailReason, PauseReason};
    use crate::disconnect::{ConflictKind, PermanentKind};

    const PAUSED: LastState = LastState::Paused(PauseReason::Conflict {
        kind: ConflictKind::DuplicateLogin,
    });
    const FAILED: LastState = LastState::Failed(FailReason::Permanent {
        kind: PermanentKind::Banned,
    });
    const RUNNING: DesiredRunState = DesiredRunState::Running;
    const STOPPED: DesiredRunState = DesiredRunState::Stopped;

    fn facts(desired: DesiredRunState, reach: Reach, known: Option<LastState>) -> CommandFacts {
        CommandFacts {
            desired,
            reach,
            known,
        }
    }

    /// S50's table, one case per cell that differs.
    #[rstest]
    // Restart: only a held bot that should run and isn't sticky.
    #[case::restart_online(
        Lifecycle::Restart,
        RUNNING,
        Reach::Held,
        Some(LastState::Online),
        Ok(CommandEffect::Send)
    )]
    #[case::restart_connecting(
        Lifecycle::Restart,
        RUNNING,
        Reach::Held,
        Some(LastState::Connecting),
        Ok(CommandEffect::Send)
    )]
    #[case::restart_backoff(
        Lifecycle::Restart,
        RUNNING,
        Reach::Held,
        Some(LastState::Backoff),
        Ok(CommandEffect::Send)
    )]
    #[case::restart_reported_stopped(
        Lifecycle::Restart,
        RUNNING,
        Reach::Held,
        Some(LastState::Stopped),
        Ok(CommandEffect::Send)
    )]
    #[case::restart_held_nothing_known(
        Lifecycle::Restart,
        RUNNING,
        Reach::Held,
        None,
        Ok(CommandEffect::Send)
    )]
    #[case::restart_paused(
        Lifecycle::Restart,
        RUNNING,
        Reach::Held,
        Some(PAUSED),
        Err(CommandRefusal::NotRunning)
    )]
    #[case::restart_failed(
        Lifecycle::Restart,
        RUNNING,
        Reach::Held,
        Some(FAILED),
        Err(CommandRefusal::NotRunning)
    )]
    #[case::restart_desired_stopped(
        Lifecycle::Restart,
        STOPPED,
        Reach::Held,
        Some(LastState::Online),
        Err(CommandRefusal::NotRunning)
    )]
    #[case::restart_desired_stopped_unheld(
        Lifecycle::Restart,
        STOPPED,
        Reach::NotHeld,
        Some(LastState::Online),
        Err(CommandRefusal::NotRunning)
    )]
    #[case::restart_no_agent(
        Lifecycle::Restart,
        RUNNING,
        Reach::NoAgent,
        None,
        Err(CommandRefusal::NotRunning)
    )]
    #[case::restart_no_agent_sticky(
        Lifecycle::Restart,
        RUNNING,
        Reach::NoAgent,
        Some(PAUSED),
        Err(CommandRefusal::NotRunning)
    )]
    #[case::restart_not_held(
        Lifecycle::Restart,
        RUNNING,
        Reach::NotHeld,
        Some(LastState::Online),
        Err(CommandRefusal::Unreachable)
    )]
    #[case::restart_not_held_sticky(
        Lifecycle::Restart,
        RUNNING,
        Reach::NotHeld,
        Some(PAUSED),
        Err(CommandRefusal::Unreachable)
    )]
    // Resume: a held Paused bot is sent, an unplaced one cleared.
    #[case::resume_held_paused(
        Lifecycle::Resume,
        RUNNING,
        Reach::Held,
        Some(PAUSED),
        Ok(CommandEffect::Send)
    )]
    #[case::resume_held_paused_desired_stopped(
        Lifecycle::Resume,
        STOPPED,
        Reach::Held,
        Some(PAUSED),
        Ok(CommandEffect::Send)
    )]
    #[case::resume_held_online(
        Lifecycle::Resume,
        RUNNING,
        Reach::Held,
        Some(LastState::Online),
        Err(CommandRefusal::NotPaused)
    )]
    #[case::resume_held_failed(
        Lifecycle::Resume,
        RUNNING,
        Reach::Held,
        Some(FAILED),
        Err(CommandRefusal::NotPaused)
    )]
    #[case::resume_held_nothing_known(
        Lifecycle::Resume,
        RUNNING,
        Reach::Held,
        None,
        Err(CommandRefusal::NotPaused)
    )]
    #[case::resume_not_held_paused(
        Lifecycle::Resume,
        RUNNING,
        Reach::NotHeld,
        Some(PAUSED),
        Err(CommandRefusal::Unreachable)
    )]
    #[case::resume_not_held_online(
        Lifecycle::Resume,
        RUNNING,
        Reach::NotHeld,
        Some(LastState::Online),
        Err(CommandRefusal::Unreachable)
    )]
    #[case::resume_no_agent_paused(
        Lifecycle::Resume,
        RUNNING,
        Reach::NoAgent,
        Some(PAUSED),
        Ok(CommandEffect::ClearSticky)
    )]
    #[case::resume_no_agent_paused_desired_stopped(
        Lifecycle::Resume,
        STOPPED,
        Reach::NoAgent,
        Some(PAUSED),
        Ok(CommandEffect::ClearSticky)
    )]
    #[case::resume_no_agent_failed(
        Lifecycle::Resume,
        RUNNING,
        Reach::NoAgent,
        Some(FAILED),
        Err(CommandRefusal::NotPaused)
    )]
    #[case::resume_no_agent_stopped(
        Lifecycle::Resume,
        RUNNING,
        Reach::NoAgent,
        Some(LastState::Stopped),
        Err(CommandRefusal::NotPaused)
    )]
    #[case::resume_no_agent_nothing_known(
        Lifecycle::Resume,
        RUNNING,
        Reach::NoAgent,
        None,
        Err(CommandRefusal::NotPaused)
    )]
    // Reset: the same, for Failed.
    #[case::reset_held_failed(
        Lifecycle::Reset,
        RUNNING,
        Reach::Held,
        Some(FAILED),
        Ok(CommandEffect::Send)
    )]
    #[case::reset_held_failed_desired_stopped(
        Lifecycle::Reset,
        STOPPED,
        Reach::Held,
        Some(FAILED),
        Ok(CommandEffect::Send)
    )]
    #[case::reset_held_online(
        Lifecycle::Reset,
        RUNNING,
        Reach::Held,
        Some(LastState::Online),
        Err(CommandRefusal::NotFailed)
    )]
    #[case::reset_held_paused(
        Lifecycle::Reset,
        RUNNING,
        Reach::Held,
        Some(PAUSED),
        Err(CommandRefusal::NotFailed)
    )]
    #[case::reset_held_nothing_known(
        Lifecycle::Reset,
        RUNNING,
        Reach::Held,
        None,
        Err(CommandRefusal::NotFailed)
    )]
    #[case::reset_not_held_failed(
        Lifecycle::Reset,
        RUNNING,
        Reach::NotHeld,
        Some(FAILED),
        Err(CommandRefusal::Unreachable)
    )]
    #[case::reset_no_agent_failed(
        Lifecycle::Reset,
        RUNNING,
        Reach::NoAgent,
        Some(FAILED),
        Ok(CommandEffect::ClearSticky)
    )]
    #[case::reset_no_agent_failed_desired_stopped(
        Lifecycle::Reset,
        STOPPED,
        Reach::NoAgent,
        Some(FAILED),
        Ok(CommandEffect::ClearSticky)
    )]
    #[case::reset_no_agent_paused(
        Lifecycle::Reset,
        RUNNING,
        Reach::NoAgent,
        Some(PAUSED),
        Err(CommandRefusal::NotFailed)
    )]
    #[case::reset_no_agent_nothing_known(
        Lifecycle::Reset,
        RUNNING,
        Reach::NoAgent,
        None,
        Err(CommandRefusal::NotFailed)
    )]
    fn a_command_is_judged_as_s50s_table_says(
        #[case] command: Lifecycle,
        #[case] desired: DesiredRunState,
        #[case] reach: Reach,
        #[case] known: Option<LastState>,
        #[case] expected: Result<CommandEffect, CommandRefusal>,
    ) {
        assert_eq!(
            judge_command(command, facts(desired, reach, known)),
            expected
        );
    }

    /// Every combination: nothing is ever sent to a stream that doesn't hold
    /// the bot, and nothing stored is cleared while an agent may run it.
    #[test]
    fn a_command_is_sent_only_to_a_holding_stream_and_clears_only_a_bot_that_runs_nowhere() {
        let known = core::iter::once(None).chain(LastState::ALL.into_iter().map(Some));
        for command in Lifecycle::ALL {
            for desired in [RUNNING, STOPPED] {
                for reach in [Reach::NoAgent, Reach::NotHeld, Reach::Held] {
                    for state in known.clone() {
                        let judged = judge_command(command, facts(desired, reach, state));
                        if judged == Ok(CommandEffect::Send) {
                            assert_eq!(reach, Reach::Held, "{command:?} {desired:?} {state:?}");
                        }
                        if judged == Ok(CommandEffect::ClearSticky) {
                            assert_eq!(reach, Reach::NoAgent, "{command:?} {desired:?} {state:?}");
                            assert!(
                                state.and_then(LastState::sticky).is_some(),
                                "{command:?} cleared a state that isn't sticky: {state:?}"
                            );
                        }
                        if reach == Reach::NotHeld && desired == RUNNING {
                            assert_eq!(
                                judged,
                                Err(CommandRefusal::Unreachable),
                                "{command:?} {state:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// Each command leaves only its own sticky state: a resume never touches
    /// a Failed bot and a reset never a Paused one, wherever they are.
    #[test]
    fn resume_acts_only_on_paused_and_reset_only_on_failed() {
        for reach in [Reach::NoAgent, Reach::Held] {
            for state in LastState::ALL {
                let resumed = judge_command(Lifecycle::Resume, facts(RUNNING, reach, Some(state)));
                let reset = judge_command(Lifecycle::Reset, facts(RUNNING, reach, Some(state)));
                assert_eq!(
                    resumed.is_ok(),
                    matches!(state, LastState::Paused(_)),
                    "{reach:?} {state:?}"
                );
                assert_eq!(
                    reset.is_ok(),
                    matches!(state, LastState::Failed(_)),
                    "{reach:?} {state:?}"
                );
            }
        }
    }

    #[test]
    fn every_refusal_has_its_fixed_text() {
        let texts: Vec<String> = [
            CommandRefusal::NotRunning,
            CommandRefusal::NotPaused,
            CommandRefusal::NotFailed,
            CommandRefusal::Unreachable,
        ]
        .iter()
        .map(ToString::to_string)
        .collect();

        assert_eq!(
            texts,
            [
                "only a running bot can be restarted",
                "only a paused bot can be resumed",
                "only a failed bot can be reset",
                "the bot's agent can't be reached",
            ]
        );
    }
}
