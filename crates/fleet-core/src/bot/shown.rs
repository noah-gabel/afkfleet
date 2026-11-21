//! [`shown`]: what a bot's state reads as, computed when it's read (Plan.md
//! P10.5, §6 row 10; ADR-0019, S41).
//!
//! An agent goes stale when its stream ends, and nothing is written then:
//! its bots read [`ShownState::Unknown`] while their agent has no live
//! stream, from the agent's status ([`crate::agent::status`]) and when it was
//! last heard from. A bot on a live agent shows the report its current stream
//! sent, or that none has come yet. A bot with no agent rests: in its sticky
//! Paused or Failed state, or Stopped, with why it isn't placed. A scripted
//! bot that qualifies waits for an agent that runs its mode's script API
//! level: while agents are live and none runs it, that's why it waits
//! (ADR-0036, D13; ADR-0045).

use chrono::{DateTime, Utc};

use super::{BotSnapshot, LastReport, StickyState};
use crate::agent::AgentStatus;
use crate::placement::Unplaced;
use crate::script::ScriptApiLevel;

/// What a bot's state reads as, computed when it's read: nothing is written
/// when an agent goes quiet (S41).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShownState {
    /// Its agent is online and reported it on its current stream.
    Reported(BotSnapshot),
    /// Its agent is online, and its current stream hasn't reported it yet.
    AwaitingReport {
        /// Since when: the bot was given to the stream, or the stream was
        /// welcomed.
        since: DateTime<Utc>,
    },
    /// Its agent has no live stream: stale, expired, disabled, or the server
    /// just started.
    Unknown {
        /// The agent's status, which says what to do: wait while it's
        /// offline, or release its bots once it's expired or disabled, as
        /// neither comes back (enrolling mints a new agent).
        agent: AgentStatus,
        /// When the server last heard from the agent.
        agent_last_seen: Option<DateTime<Utc>>,
        /// The bot's last known state, and when the server received it.
        last_known: Option<LastReport>,
    },
    /// It has no agent, and runs nowhere.
    Unplaced {
        /// Why.
        why: UnplacedWhy,
        /// The Paused or Failed state it rests in, if its last one was.
        sticky: Option<StickyState>,
    },
}

/// Why a bot has no agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnplacedWhy {
    /// It doesn't qualify ([`crate::placement::placeable`]).
    Unqualified(Unplaced),
    /// It qualifies and waits for an agent with room.
    AwaitingAgent,
    /// It qualifies, its mode is a script, and the agents that are live
    /// all run a lower script API level than it needs: one must be updated
    /// (ADR-0036, D13).
    AwaitingNewerAgent,
}

/// What [`shown`] reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShownFacts {
    /// Whether the bot qualifies.
    pub qualifies: Result<(), Unplaced>,
    /// Its last known state: the newer of the stored one and the server's
    /// memory.
    pub last: Option<LastReport>,
    /// The agent its row assigns it to, if any.
    pub assigned: Option<AssignedTo>,
    /// The script API level its mode needs: `None` for a step mode.
    pub needs: Option<ScriptApiLevel>,
    /// The highest script API level a live agent runs: `None` while no
    /// agent is live.
    pub newest_live: Option<u32>,
}

/// The agent a bot is assigned to, as the read finds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignedTo {
    /// Its status ([`crate::agent::status`]).
    pub status: AgentStatus,
    /// When the server last heard from it.
    pub last_seen: Option<DateTime<Utc>>,
    /// Its live stream, if it has one.
    pub stream: Option<StreamFacts>,
}

/// What an agent's live stream knows of a bot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamFacts {
    /// When the stream was welcomed.
    pub connected_since: DateTime<Utc>,
    /// When the stream was given the bot, if it holds it.
    pub held_since: Option<DateTime<Utc>>,
    /// The bot's latest report on this stream, if any.
    pub report: Option<BotSnapshot>,
}

/// What a bot's state reads as, from `facts`: its agent's report on its
/// current stream, or that none came yet; [`ShownState::Unknown`] while its agent
/// has no live stream or is disabled or expired; [`ShownState::Unplaced`], resting
/// in its sticky state, when it has no agent.
#[must_use]
pub fn shown(facts: ShownFacts) -> ShownState {
    let ShownFacts {
        qualifies,
        last,
        assigned,
        needs,
        newest_live,
    } = facts;
    let Some(assigned) = assigned else {
        let why = match qualifies {
            Ok(()) => awaiting(needs, newest_live),
            Err(why) => UnplacedWhy::Unqualified(why),
        };
        // It runs nowhere: it rests in its sticky state, or Stopped.
        let sticky = last.and_then(|last| last.state.sticky());
        return ShownState::Unplaced { why, sticky };
    };
    match (assigned.status, assigned.stream) {
        (AgentStatus::Online, Some(stream)) => match stream.report {
            Some(report) => ShownState::Reported(report),
            None => ShownState::AwaitingReport {
                since: stream.held_since.unwrap_or(stream.connected_since),
            },
        },
        // An agent disabled or expired while its stream still lives counts as
        // gone, as the list of agents shows it.
        (status, _) => ShownState::Unknown {
            agent: status,
            agent_last_seen: assigned.last_seen,
            last_known: last,
        },
    }
}

/// Why a bot that qualifies waits: for a newer agent while the live ones are
/// all too old for its mode, for an agent with room otherwise.
fn awaiting(needs: Option<ScriptApiLevel>, newest_live: Option<u32>) -> UnplacedWhy {
    match (needs, newest_live) {
        (Some(level), Some(newest)) if !level.runs_on(newest) => UnplacedWhy::AwaitingNewerAgent,
        _ => UnplacedWhy::AwaitingAgent,
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::bot::{BotState, FailReason, LastState};
    use crate::id::BotId;

    const T0: &str = "2027-01-15T08:00:00Z";

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    fn snapshot() -> BotSnapshot {
        BotSnapshot {
            bot_id: BotId::new_v7(at(T0), [1; 10]).unwrap(),
            state: BotState::Stopped,
            since: at(T0),
            last_disconnect: None,
        }
    }

    fn last(state: LastState) -> LastReport {
        LastReport {
            state,
            at: at("2027-01-15T07:00:00Z"),
        }
    }

    fn stream(held: bool, report: Option<BotSnapshot>) -> StreamFacts {
        StreamFacts {
            connected_since: at("2027-01-15T07:30:00Z"),
            held_since: held.then(|| at("2027-01-15T07:45:00Z")),
            report,
        }
    }

    fn on(status: AgentStatus, stream: Option<StreamFacts>) -> AssignedTo {
        AssignedTo {
            status,
            last_seen: Some(at("2027-01-15T07:50:00Z")),
            stream,
        }
    }

    const FAILED: LastState = LastState::Failed(FailReason::Auth);

    #[rstest]
    #[case::reported(
        Some(on(AgentStatus::Online, Some(stream(true, Some(snapshot()))))),
        ShownState::Reported(snapshot())
    )]
    #[case::held_not_yet_reported(
        Some(on(AgentStatus::Online, Some(stream(true, None)))),
        ShownState::AwaitingReport { since: at("2027-01-15T07:45:00Z") }
    )]
    #[case::not_yet_held(
        Some(on(AgentStatus::Online, Some(stream(false, None)))),
        ShownState::AwaitingReport { since: at("2027-01-15T07:30:00Z") }
    )]
    #[case::online_without_a_stream(
        Some(on(AgentStatus::Online, None)),
        ShownState::Unknown {
            agent: AgentStatus::Online,
            agent_last_seen: Some(at("2027-01-15T07:50:00Z")),
            last_known: Some(last(FAILED)),
        }
    )]
    #[case::stale(
        Some(on(AgentStatus::Offline, None)),
        ShownState::Unknown {
            agent: AgentStatus::Offline,
            agent_last_seen: Some(at("2027-01-15T07:50:00Z")),
            last_known: Some(last(FAILED)),
        }
    )]
    #[case::expired(
        Some(on(AgentStatus::Expired, None)),
        ShownState::Unknown {
            agent: AgentStatus::Expired,
            agent_last_seen: Some(at("2027-01-15T07:50:00Z")),
            last_known: Some(last(FAILED)),
        }
    )]
    #[case::disabled_while_its_stream_still_lives(
        Some(on(AgentStatus::Disabled, Some(stream(true, Some(snapshot()))))),
        ShownState::Unknown {
            agent: AgentStatus::Disabled,
            agent_last_seen: Some(at("2027-01-15T07:50:00Z")),
            last_known: Some(last(FAILED)),
        }
    )]
    fn an_assigned_bot_reads_by_its_agents_status(
        #[case] assigned: Option<AssignedTo>,
        #[case] expected: ShownState,
    ) {
        let facts = ShownFacts {
            // An assigned bot reads by its agent whether it qualifies or not.
            qualifies: Err(Unplaced::Stopped),
            last: Some(last(FAILED)),
            assigned,
            needs: Some(ScriptApiLevel::V1),
            newest_live: Some(0),
        };

        assert_eq!(shown(facts), expected);
    }

    #[rstest]
    #[case::waiting(Ok(()), None, UnplacedWhy::AwaitingAgent, None)]
    #[case::unqualified(
        Err(Unplaced::Pending),
        Some(last(LastState::Online)),
        UnplacedWhy::Unqualified(Unplaced::Pending),
        None
    )]
    #[case::resting_failed(
        Ok(()),
        Some(last(FAILED)),
        UnplacedWhy::AwaitingAgent,
        Some(StickyState::Failed(FailReason::Auth))
    )]
    fn an_unassigned_bot_rests_with_why_it_isnt_placed(
        #[case] qualifies: Result<(), Unplaced>,
        #[case] last: Option<LastReport>,
        #[case] why: UnplacedWhy,
        #[case] sticky: Option<StickyState>,
    ) {
        let facts = ShownFacts {
            qualifies,
            last,
            assigned: None,
            needs: None,
            newest_live: Some(0),
        };

        assert_eq!(shown(facts), ShownState::Unplaced { why, sticky });
    }

    #[rstest]
    #[case::every_live_agent_too_old(
        Some(ScriptApiLevel::V1),
        Some(0),
        UnplacedWhy::AwaitingNewerAgent
    )]
    #[case::one_live_agent_runs_it(Some(ScriptApiLevel::V1), Some(1), UnplacedWhy::AwaitingAgent)]
    #[case::a_newer_live_agent_runs_it(
        Some(ScriptApiLevel::V1),
        Some(7),
        UnplacedWhy::AwaitingAgent
    )]
    #[case::no_agent_live(Some(ScriptApiLevel::V1), None, UnplacedWhy::AwaitingAgent)]
    #[case::a_step_mode(None, Some(0), UnplacedWhy::AwaitingAgent)]
    fn a_scripted_bot_waits_for_a_newer_agent_while_every_live_one_is_too_old(
        #[case] needs: Option<ScriptApiLevel>,
        #[case] newest_live: Option<u32>,
        #[case] why: UnplacedWhy,
    ) {
        let facts = ShownFacts {
            qualifies: Ok(()),
            last: None,
            assigned: None,
            needs,
            newest_live,
        };

        assert_eq!(shown(facts), ShownState::Unplaced { why, sticky: None });
    }

    #[test]
    fn a_bot_that_doesnt_qualify_says_why_whatever_its_mode_needs() {
        let facts = ShownFacts {
            qualifies: Err(Unplaced::Stopped),
            last: None,
            assigned: None,
            needs: Some(ScriptApiLevel::V1),
            newest_live: Some(0),
        };

        assert_eq!(
            shown(facts),
            ShownState::Unplaced {
                why: UnplacedWhy::Unqualified(Unplaced::Stopped),
                sticky: None
            }
        );
    }
}
