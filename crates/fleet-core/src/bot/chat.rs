//! [`judge_chat`]: whether a user's chat line can go to a bot now, decided
//! before the server records or sends it (Plan.md P11.3; ADR-0020, S5).
//!
//! A line goes only to a bot that should run, whose agent's live stream
//! holds it, and that its agent last reported Online: anything else is
//! refused at once, with nothing written or recorded. A line, like a
//! lifecycle command, is never stored for later: it reaches the bot now or
//! never. The agent and the bot have the last word (a session can end on the
//! way, or a Minecraft server refuse what it can't sign); they answer the
//! send.

use super::{DesiredRunState, LastState, Reach};

/// What [`judge_chat`] reads about a bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatFacts {
    /// Whether the bot should run.
    pub desired: DesiredRunState,
    /// How far the server reaches it.
    pub reach: Reach,
    /// Its known state: its stream's report, or else the newer of the stored
    /// state and the server's memory.
    pub known: Option<LastState>,
}

/// Why a chat line can't go to a bot now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChatRefusal {
    /// The bot isn't online: it should be stopped, runs nowhere, or its
    /// stream reports it in another state.
    #[error("only an online bot can send chat")]
    NotOnline,
    /// The bot's agent has no live stream that holds it.
    #[error("the bot's agent can't be reached")]
    Unreachable,
}

/// Whether a chat line can go to the bot `facts` describe.
///
/// # Errors
/// [`ChatRefusal::NotOnline`] for a bot that should be stopped, runs on no
/// agent, or is held in any state but Online; [`ChatRefusal::Unreachable`]
/// for one whose agent has no live stream that holds it.
pub const fn judge_chat(facts: ChatFacts) -> Result<(), ChatRefusal> {
    if matches!(facts.desired, DesiredRunState::Stopped) {
        return Err(ChatRefusal::NotOnline);
    }
    match facts.reach {
        Reach::NoAgent => Err(ChatRefusal::NotOnline),
        Reach::NotHeld => Err(ChatRefusal::Unreachable),
        Reach::Held => match facts.known {
            Some(LastState::Online) => Ok(()),
            _ => Err(ChatRefusal::NotOnline),
        },
    }
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

    fn facts(desired: DesiredRunState, reach: Reach, known: Option<LastState>) -> ChatFacts {
        ChatFacts {
            desired,
            reach,
            known,
        }
    }

    /// S5's rows: no agent, desired Stopped, or held without an Online
    /// report is `NotOnline`; a bot its agent's stream doesn't hold is
    /// `Unreachable`.
    #[rstest]
    #[case::online(facts(RUNNING, Reach::Held, Some(LastState::Online)), Ok(()))]
    #[case::held_connecting(
        facts(RUNNING, Reach::Held, Some(LastState::Connecting)),
        Err(ChatRefusal::NotOnline)
    )]
    #[case::held_backing_off(
        facts(RUNNING, Reach::Held, Some(LastState::Backoff)),
        Err(ChatRefusal::NotOnline)
    )]
    #[case::held_paused(facts(RUNNING, Reach::Held, Some(PAUSED)), Err(ChatRefusal::NotOnline))]
    #[case::held_failed(facts(RUNNING, Reach::Held, Some(FAILED)), Err(ChatRefusal::NotOnline))]
    #[case::held_without_a_report(facts(RUNNING, Reach::Held, None), Err(ChatRefusal::NotOnline))]
    #[case::no_agent(facts(RUNNING, Reach::NoAgent, None), Err(ChatRefusal::NotOnline))]
    #[case::no_agent_but_a_stored_online(
        facts(RUNNING, Reach::NoAgent, Some(LastState::Online)),
        Err(ChatRefusal::NotOnline)
    )]
    #[case::not_held(
        facts(RUNNING, Reach::NotHeld, Some(LastState::Online)),
        Err(ChatRefusal::Unreachable)
    )]
    #[case::not_held_without_a_report(
        facts(RUNNING, Reach::NotHeld, None),
        Err(ChatRefusal::Unreachable)
    )]
    #[case::stopped_while_online(
        facts(STOPPED, Reach::Held, Some(LastState::Online)),
        Err(ChatRefusal::NotOnline)
    )]
    #[case::stopped_and_not_held(
        facts(STOPPED, Reach::NotHeld, Some(LastState::Online)),
        Err(ChatRefusal::NotOnline)
    )]
    fn a_chat_line_is_judged_as_s5_says(
        #[case] facts: ChatFacts,
        #[case] expected: Result<(), ChatRefusal>,
    ) {
        assert_eq!(judge_chat(facts), expected);
    }

    /// Every combination: a line goes exactly to a bot that should run, is
    /// held, and was last reported Online; a refusal is `Unreachable` only
    /// for a bot that should run and isn't held.
    #[test]
    fn a_line_goes_exactly_to_a_held_online_bot_that_should_run() {
        let known = LastState::ALL.into_iter().map(Some).chain([None]);
        for known in known {
            for desired in [RUNNING, STOPPED] {
                for reach in [Reach::NoAgent, Reach::NotHeld, Reach::Held] {
                    let facts = facts(desired, reach, known);
                    let goes = desired == RUNNING
                        && reach == Reach::Held
                        && known == Some(LastState::Online);
                    let unreachable = desired == RUNNING && reach == Reach::NotHeld;

                    let judged = judge_chat(facts);

                    match judged {
                        Ok(()) => assert!(goes, "{facts:?}"),
                        Err(ChatRefusal::Unreachable) => assert!(unreachable, "{facts:?}"),
                        Err(ChatRefusal::NotOnline) => {
                            assert!(!goes && !unreachable, "{facts:?}");
                        }
                    }
                }
            }
        }
    }

    #[rstest]
    #[case::not_online(ChatRefusal::NotOnline, "only an online bot can send chat")]
    #[case::unreachable(ChatRefusal::Unreachable, "the bot's agent can't be reached")]
    fn each_refusal_says_why(#[case] refusal: ChatRefusal, #[case] text: &str) {
        assert_eq!(refusal.to_string(), text);
    }
}
