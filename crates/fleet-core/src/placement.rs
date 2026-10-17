//! Which agent runs which bot (Plan.md P10.5; ADR-0019, S32–S45): the
//! scheduler's choice and the reconciler's diff, as one pure [`plan`].
//!
//! The server's reconciler collects the facts (the stored bots, the open
//! release records, each live agent's stream) and applies the [`Plan`]: its
//! writes first, each in its own audited transaction, then the messages. The
//! rules:
//! - **Never two agents on one account.** A bot is placed only while no live
//!   stream holds it, lists it in an unreconciled hello, or holds a bot of
//!   the same Minecraft account ([`McIdentity`]), no open release record
//!   names it or its account, and no other bot of its account is assigned. A
//!   bot leaves an agent only by that agent's word: its `Removed` of a bot
//!   the server released, or a hello that doesn't list it (S32, S44).
//! - **Assignments come only from the server's own state.** A hello never
//!   gains an agent a bot: a listed bot its row doesn't assign to that agent
//!   is left out and released, and its fence persisted ([`Stray`], S43).
//! - **Which bots run:** [`placeable`] (S33). A placed bot that stops
//!   qualifying is stopped first and unassigned once it rests or its bound
//!   passes, so a Paused report is never dropped.
//! - **Where:** live agents in enrollment order (their IDs), first fit; a
//!   placed bot is never moved to rebalance. An agent back with less
//!   capacity keeps the bots it runs first, then those not paused or failed,
//!   then the lowest IDs; the rest move under the same rules (S42).
//! - **When:** right after serve starts, new placements wait until every
//!   enabled agent has said hello, or a grace has passed ([`Facts::gate_open`],
//!   S45).
//! - **A bot whose facts don't read back** ([`Untouchable`]) is left alone:
//!   never placed, unassigned, moved or sent anything; it keeps its agent's
//!   slot and fences its account, and a welcome that lists it as its own waits
//!   until it reads back, since no full reconcile could keep it running.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::account::{AccountKind, AccountStatus};
use crate::bot::DesiredRunState;
use crate::id::{AgentId, BotId};
use crate::value::{AgentCapacity, McUsername, McUuid};

/// A Minecraft account as a player sees it: two bots with the same identity
/// can't run side by side, whichever account rows they belong to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum McIdentity {
    /// A Microsoft account: its profile's UUID.
    Microsoft(McUuid),
    /// An offline account: its name, ignoring ASCII case, as Minecraft
    /// compares names.
    Offline(OfflineName),
}

/// An offline account's name in ASCII lowercase; see [`McIdentity`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OfflineName(String);

impl OfflineName {
    /// The lowercased name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl McIdentity {
    /// The identity of the offline account `name`.
    #[must_use]
    pub fn offline(name: &McUsername) -> Self {
        Self::Offline(OfflineName(name.as_str().to_ascii_lowercase()))
    }
}

/// Why a bot isn't placed on an agent (S33).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unplaced {
    /// It has no server address.
    NoServer,
    /// Its desired state is Stopped.
    Stopped,
    /// Its account's owner is disabled.
    OwnerDisabled,
    /// Its account waits for approval.
    Pending,
    /// Microsoft refused its account's refresh token.
    ReauthRequired,
    /// Minecraft's side refused its account.
    Refused,
    /// It plays an offline account while `[accounts] allow_offline` is off.
    OfflineNotAllowed,
}

impl Unplaced {
    /// Every reason, for the tests that check them all.
    pub const ALL: [Self; 7] = [
        Self::NoServer,
        Self::Stopped,
        Self::OwnerDisabled,
        Self::Pending,
        Self::ReauthRequired,
        Self::Refused,
        Self::OfflineNotAllowed,
    ];

    /// The reason's stable name, for the audit log and the API.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoServer => "no_server",
            Self::Stopped => "stopped",
            Self::OwnerDisabled => "owner_disabled",
            Self::Pending => "pending",
            Self::ReauthRequired => "reauth_required",
            Self::Refused => "refused",
            Self::OfflineNotAllowed => "offline_not_allowed",
        }
    }
}

/// What [`placeable`] decides by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaceFacts {
    /// Whether the bot has a server address.
    pub has_server: bool,
    /// Whether it should run.
    pub desired: DesiredRunState,
    /// Whether its account's owner is enabled.
    pub owner_enabled: bool,
    /// Its account's kind.
    pub kind: AccountKind,
    /// Its account's status.
    pub status: AccountStatus,
    /// Whether `[accounts] allow_offline` is on.
    pub allow_offline: bool,
}

/// Whether a bot may run on an agent: a server address, desired Running, an
/// enabled owner, and an active Microsoft account or an offline one while
/// offline accounts are allowed. The checks run in that order.
///
/// # Errors
/// The first [`Unplaced`] reason that applies.
pub const fn placeable(facts: &PlaceFacts) -> Result<(), Unplaced> {
    if !facts.has_server {
        return Err(Unplaced::NoServer);
    }
    if matches!(facts.desired, DesiredRunState::Stopped) {
        return Err(Unplaced::Stopped);
    }
    if !facts.owner_enabled {
        return Err(Unplaced::OwnerDisabled);
    }
    match facts.kind {
        // An offline account is always active (P9.7).
        AccountKind::Offline if facts.allow_offline => Ok(()),
        AccountKind::Offline => Err(Unplaced::OfflineNotAllowed),
        AccountKind::Microsoft => match facts.status {
            AccountStatus::Active => Ok(()),
            AccountStatus::Pending => Err(Unplaced::Pending),
            AccountStatus::ReauthRequired => Err(Unplaced::ReauthRequired),
            AccountStatus::Refused(_) => Err(Unplaced::Refused),
        },
    }
}

/// A stored bot, as the planner sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotFacts {
    /// The bot.
    pub id: BotId,
    /// Its account, as a player sees it.
    pub identity: McIdentity,
    /// [`placeable`]'s answer for it.
    pub qualifies: Result<(), Unplaced>,
    /// The agent its row assigns it to.
    pub agent: Option<AgentId>,
    /// Whether its last known state is Paused or Failed.
    pub sticky: bool,
}

/// An open release record: an agent that may still run a bot whose row no
/// longer says so (a deleted account's bot, S39, or a hello's stray, S43).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseFacts {
    /// The bot.
    pub bot: BotId,
    /// The agent that may still run it.
    pub agent: AgentId,
    /// Its account, as a player sees it.
    pub identity: McIdentity,
}

/// A live agent's stream, as the planner sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFacts {
    /// The agent.
    pub id: AgentId,
    /// The stream's generation, which the plan carries back.
    pub generation: u64,
    /// Whether the agent is enabled and its stream not ended: only such an
    /// agent gets a plan. Every stream's bots fence the others either way.
    pub plannable: bool,
    /// The capacity its hello reported.
    pub capacity: AgentCapacity,
    /// What the stream holds.
    pub phase: Phase,
}

/// Where a live stream is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Welcomed, and its hello's bots not reconciled yet.
    Welcomed {
        /// The bots its hello says it runs.
        listed: Vec<Listed>,
    },
    /// Reconciled: the bots the server let it hold.
    Reconciled {
        /// The holds.
        holds: Vec<HoldFacts>,
    },
}

/// A bot a hello listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Listed {
    /// The bot.
    pub bot: BotId,
    /// Whether its reported state rests: Stopped, Paused or Failed.
    pub rests: bool,
}

/// A bot a reconciled stream holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoldFacts {
    /// The bot.
    pub bot: BotId,
    /// How.
    pub held: Held,
}

/// How a stream holds a bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// Assigned to it.
    Assigned {
        /// Whether the server is stopping it before it unassigns it.
        stopping: Option<Stopping>,
        /// Whether its latest report on this stream rests.
        rests: bool,
    },
    /// Unassigned: only its `Removed` is still expected.
    Releasing,
}

/// Why the server is stopping an assigned bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopping {
    /// It stopped qualifying; it's unassigned when it rests or `overdue`.
    Unqualified {
        /// Whether its bound has passed.
        overdue: bool,
    },
    /// It moves off an agent with less capacity; it's unassigned when it
    /// rests, with no bound.
    Move,
}

/// A bot whose stored facts don't read back: it's never placed, unassigned,
/// moved or sent anything until they do; it keeps its agent's slot and
/// fences its account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Untouchable {
    /// The bot.
    pub bot: BotId,
    /// The agent its row, or its release record, names.
    pub agent: Option<AgentId>,
    /// Its account as a player sees it, when that much reads back.
    pub identity: Option<McIdentity>,
}

/// Everything [`plan`] decides by.
#[derive(Debug, Clone, Copy)]
pub struct Facts<'a> {
    /// Every stored bot that reads back.
    pub bots: &'a [BotFacts],
    /// Every open release record that reads back.
    pub releases: &'a [ReleaseFacts],
    /// Every bot, or release record, that doesn't.
    pub untouchable: &'a [Untouchable],
    /// Every live stream.
    pub agents: &'a [AgentFacts],
    /// Whether new placements may happen (S45).
    pub gate_open: bool,
}

/// What to do: the writes, then each agent's messages.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Assignments and records to clear: the agent doesn't run the bot.
    pub detach: Vec<Detach>,
    /// Fences to persist for bots a hello listed that aren't its (S43).
    pub stray: Vec<Stray>,
    /// Each plannable agent's part, in agent order.
    pub agents: Vec<AgentPlan>,
}

/// An assignment or a release record cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detach {
    /// The bot.
    pub bot: BotId,
    /// The agent that no longer runs it.
    pub agent: AgentId,
    /// Why.
    pub reason: DetachReason,
}

/// Why an agent no longer runs a bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DetachReason {
    /// The agent sent its `Removed`.
    Removed,
    /// The agent's hello didn't list it.
    NotRunning,
    /// The server's host released the disabled agent's bots.
    AgentReleased,
}

impl DetachReason {
    /// Every reason, for the tests that check them all.
    pub const ALL: [Self; 3] = [Self::Removed, Self::NotRunning, Self::AgentReleased];

    /// The reason's stable name, for the audit log.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Removed => "removed",
            Self::NotRunning => "not_running",
            Self::AgentReleased => "agent_released",
        }
    }
}

/// A fence to persist: `agent`'s hello listed `bot`, whose row doesn't
/// assign it there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stray {
    /// The bot.
    pub bot: BotId,
    /// The agent that listed it.
    pub agent: AgentId,
}

/// One agent's part of a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPlan {
    /// The agent.
    pub agent: AgentId,
    /// The stream generation the part is for.
    pub generation: u64,
    /// Whether it's the stream's welcome: one full reconcile of `run`.
    pub full: bool,
    /// The bots the agent should run, in bot order.
    pub run: Vec<Run>,
    /// The bots it should let go of, in bot order.
    pub release: Vec<Let>,
    /// Bots assigned to it that it never got and that aren't sent again:
    /// its next hello settles them. In bot order.
    pub undelivered: Vec<Undelivered>,
}

/// A bot assigned to an agent that never got it, and why it isn't sent
/// again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Undelivered {
    /// The bot.
    pub bot: BotId,
    /// Why.
    pub why: UndeliveredWhy,
}

/// Why an assignment its agent never got isn't sent again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndeliveredWhy {
    /// The bot no longer qualifies.
    Unqualified(Unplaced),
    /// The agent has no room left for it.
    NoRoom,
}

impl UndeliveredWhy {
    /// The reason's stable name, for the logs: an [`Unplaced`] name, or
    /// `no_room`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unqualified(why) => why.as_str(),
            Self::NoRoom => "no_room",
        }
    }
}

/// A bot an agent should run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    /// The bot.
    pub bot: BotId,
    /// What its spec says: Stopped while it's being stopped.
    pub desired: DesiredRunState,
    /// Why it's being stopped, if it is.
    pub stopping: Option<StopWhy>,
    /// Whether the server assigns it now: its row gets the agent.
    pub placed: bool,
}

/// Why a bot is being stopped before it's unassigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopWhy {
    /// It stopped qualifying.
    Unqualified,
    /// It moves off the agent.
    Move,
}

/// A bot an agent should let go of: it's left out of a welcome's reconcile,
/// or sent an unassign, and held releasing until its `Removed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Let {
    /// The bot.
    pub bot: BotId,
    /// Why the server unassigns it, when its row still names the agent: the
    /// decision is recorded then.
    pub unassign: Option<UnassignReason>,
    /// Whether an unassign is sent (outside a welcome).
    pub send: bool,
}

/// Why the server unassigns a bot from its agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnassignReason {
    /// It stopped qualifying.
    Unqualified(Unplaced),
    /// Its agent came back with less capacity.
    Capacity,
}

impl UnassignReason {
    /// The reason's stable name, for the audit log: an [`Unplaced`] name,
    /// or `capacity`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unqualified(why) => why.as_str(),
            Self::Capacity => "capacity",
        }
    }
}

/// The plan for `facts`; see the [module docs](self). It sorts what it
/// reads, so the order of the input never changes it.
#[must_use]
pub fn plan(facts: &Facts<'_>) -> Plan {
    let mut planner = Planner::new(facts);
    // Each agent's part is kept in agent order, whatever the input's.
    for agent in facts.agents {
        if !agent.plannable {
            continue;
        }
        match &agent.phase {
            Phase::Welcomed { listed } => planner.welcome(agent, listed),
            Phase::Reconciled { holds } => planner.steady(agent, holds),
        }
    }
    if facts.gate_open {
        planner.place_waiting();
    }
    planner.finish()
}

/// The bots a stream holds or lists.
fn stream_bots(agent: &AgentFacts) -> Vec<BotId> {
    match &agent.phase {
        Phase::Welcomed { listed } => listed.iter().map(|listed| listed.bot).collect(),
        Phase::Reconciled { holds } => holds.iter().map(|hold| hold.bot).collect(),
    }
}

/// A plan being built.
struct Planner<'a> {
    bots: BTreeMap<BotId, &'a BotFacts>,
    /// Each untouchable bot, with the agent its row names.
    untouchable: BTreeMap<BotId, Option<AgentId>>,
    records: BTreeSet<(BotId, AgentId)>,
    /// Bots any live stream holds or lists, and bots with an open record.
    fenced_bots: HashSet<BotId>,
    /// Accounts no new placement may take.
    fenced_accounts: HashSet<McIdentity>,
    /// Bots whose row loses its agent in this plan.
    detached: HashSet<BotId>,
    /// Each plannable agent's part, and how many bots it may still be given.
    parts: BTreeMap<AgentId, (AgentPlan, usize)>,
    plan: Plan,
}

impl<'a> Planner<'a> {
    fn new(facts: &Facts<'a>) -> Self {
        let bots: BTreeMap<BotId, &'a BotFacts> =
            facts.bots.iter().map(|bot| (bot.id, bot)).collect();
        let records = facts
            .releases
            .iter()
            .map(|record| (record.bot, record.agent))
            .collect();
        let mut fenced_bots: HashSet<BotId> =
            facts.releases.iter().map(|record| record.bot).collect();
        let mut fenced_accounts: HashSet<McIdentity> = facts
            .releases
            .iter()
            .map(|record| record.identity.clone())
            .collect();
        for bot in facts.agents.iter().flat_map(stream_bots) {
            fenced_bots.insert(bot);
            if let Some(facts) = bots.get(&bot) {
                fenced_accounts.insert(facts.identity.clone());
            }
        }
        for frozen in facts.untouchable {
            fenced_bots.insert(frozen.bot);
            if let Some(identity) = &frozen.identity {
                fenced_accounts.insert(identity.clone());
            }
        }
        Self {
            bots,
            untouchable: facts
                .untouchable
                .iter()
                .map(|frozen| (frozen.bot, frozen.agent))
                .collect(),
            records,
            fenced_bots,
            fenced_accounts,
            detached: HashSet::new(),
            parts: BTreeMap::new(),
            plan: Plan::default(),
        }
    }

    /// A welcomed stream's part: one full reconcile.
    fn welcome(&mut self, agent: &AgentFacts, listed: &[Listed]) {
        let listed: BTreeMap<BotId, bool> = listed
            .iter()
            .map(|listed| (listed.bot, listed.rests))
            .collect();
        // Its full reconcile can't keep a bot of its own that doesn't read
        // back, and leaving it out would remove it: the welcome waits.
        if listed
            .keys()
            .any(|bot| self.untouchable.get(bot) == Some(&Some(agent.id)))
        {
            return;
        }
        let frozen = self.frozen_on(agent.id, |bot| listed.contains_key(&bot));
        let mut part = AgentPlan {
            agent: agent.id,
            generation: agent.generation,
            full: true,
            run: Vec::new(),
            release: Vec::new(),
            undelivered: Vec::new(),
        };
        let foreign = self.let_foreign_go(agent.id, &listed, &mut part);
        // Its records of bots it no longer runs close.
        let closed: Vec<BotId> = self
            .records
            .iter()
            .filter(|&&(bot, holder)| holder == agent.id && !listed.contains_key(&bot))
            .map(|&(bot, _)| bot)
            .collect();
        for bot in closed {
            self.plan.detach.push(Detach {
                bot,
                agent: agent.id,
                reason: DetachReason::NotRunning,
            });
        }
        // Its own bots: the ones it runs first, then those not paused or
        // failed, then the lowest IDs (S42).
        let mut own: Vec<&BotFacts> = self
            .bots
            .values()
            .copied()
            .filter(|facts| facts.agent == Some(agent.id))
            .collect();
        own.sort_by_key(|facts| (!listed.contains_key(&facts.id), facts.sticky, facts.id));
        let capacity = usize::from(agent.capacity.get());
        let listed_room = capacity.saturating_sub(foreign);
        // What it may run besides what it runs now: the runtime counts every
        // bot it knows until its `Removed`.
        let new_room = capacity.saturating_sub(listed.len()).saturating_sub(frozen);
        let mut kept_listed = 0_usize;
        let mut added = 0_usize;
        for facts in own {
            let bot = facts.id;
            match (listed.get(&bot).copied(), facts.qualifies) {
                (Some(rests), Ok(())) => {
                    if kept_listed < listed_room {
                        kept_listed = kept_listed.saturating_add(1);
                        part.run.push(running(bot, false));
                    } else if rests {
                        part.release.push(Let {
                            bot,
                            unassign: Some(UnassignReason::Capacity),
                            send: false,
                        });
                    } else {
                        part.run.push(stopping(bot, StopWhy::Move));
                    }
                }
                (Some(rests), Err(why)) => {
                    // A bot without a server can't get a spec to stop with.
                    if rests || why == Unplaced::NoServer {
                        part.release.push(Let {
                            bot,
                            unassign: Some(UnassignReason::Unqualified(why)),
                            send: false,
                        });
                    } else {
                        part.run.push(stopping(bot, StopWhy::Unqualified));
                    }
                }
                (None, Ok(())) if added < new_room => {
                    added = added.saturating_add(1);
                    part.run.push(running(bot, false));
                }
                // It doesn't run it: the hello proves it, so it's freed at
                // once (S32), and placed again if it may be.
                (None, _) => self.detach_not_running(bot, agent.id),
            }
        }
        let free = new_room.saturating_sub(added);
        self.parts.insert(agent.id, (part, free));
    }

    /// Lets go of the listed bots `agent`'s row doesn't give it, in `part`:
    /// they're left out, and their fence is persisted unless it is already,
    /// or nothing says which account. How many there were.
    fn let_foreign_go(
        &mut self,
        agent: AgentId,
        listed: &BTreeMap<BotId, bool>,
        part: &mut AgentPlan,
    ) -> usize {
        let mut foreign = 0_usize;
        for &bot in listed.keys() {
            if self.bots.get(&bot).and_then(|facts| facts.agent) == Some(agent) {
                continue;
            }
            foreign = foreign.saturating_add(1);
            part.release.push(Let {
                bot,
                unassign: None,
                send: false,
            });
            let own_record = self.records.contains(&(bot, agent));
            let other_record = self
                .records
                .iter()
                .any(|&(recorded, holder)| recorded == bot && holder != agent);
            if !own_record && (self.bots.contains_key(&bot) || other_record) {
                self.plan.stray.push(Stray { bot, agent });
            }
        }
        foreign
    }

    /// A reconciled stream's part.
    fn steady(&mut self, agent: &AgentFacts, holds: &[HoldFacts]) {
        let holds: BTreeMap<BotId, Held> = holds.iter().map(|hold| (hold.bot, hold.held)).collect();
        let mut part = AgentPlan {
            agent: agent.id,
            generation: agent.generation,
            full: false,
            run: Vec::new(),
            release: Vec::new(),
            undelivered: Vec::new(),
        };
        for (&bot, &held) in &holds {
            let Held::Assigned {
                stopping: stop,
                rests,
            } = held
            else {
                // Releasing: only its `Removed` is still expected.
                continue;
            };
            if self.untouchable.contains_key(&bot) {
                // Left alone until it reads back.
                continue;
            }
            let row = self
                .bots
                .get(&bot)
                .filter(|facts| facts.agent == Some(agent.id));
            let Some(facts) = row else {
                // Its row is gone (a deleted account) or names another agent.
                part.release.push(Let {
                    bot,
                    unassign: None,
                    send: true,
                });
                continue;
            };
            match (facts.qualifies, stop) {
                (Ok(()), Some(Stopping::Move)) if rests => part.release.push(Let {
                    bot,
                    unassign: Some(UnassignReason::Capacity),
                    send: true,
                }),
                (Ok(()), Some(Stopping::Move)) => part.run.push(stopping(bot, StopWhy::Move)),
                (Ok(()), _) => part.run.push(running(bot, false)),
                (Err(why), _) => {
                    let overdue = matches!(stop, Some(Stopping::Unqualified { overdue: true }));
                    if rests || overdue {
                        part.release.push(Let {
                            bot,
                            unassign: Some(UnassignReason::Unqualified(why)),
                            send: true,
                        });
                    } else {
                        part.run.push(stopping(bot, StopWhy::Unqualified));
                    }
                }
            }
        }
        let frozen = self.frozen_on(agent.id, |bot| holds.contains_key(&bot));
        let room = usize::from(agent.capacity.get())
            .saturating_sub(holds.len())
            .saturating_sub(frozen);
        // Its own bots it doesn't hold: assignments whose message couldn't
        // be sent. One that qualifies is sent again while there's room; the
        // rest wait for the agent's next hello, which settles them.
        let mut resent = 0_usize;
        for facts in self.bots.values() {
            if facts.agent != Some(agent.id) || holds.contains_key(&facts.id) {
                continue;
            }
            let why = match facts.qualifies {
                Ok(()) if resent < room => {
                    resent = resent.saturating_add(1);
                    part.run.push(running(facts.id, false));
                    continue;
                }
                Ok(()) => UndeliveredWhy::NoRoom,
                Err(why) => UndeliveredWhy::Unqualified(why),
            };
            part.undelivered.push(Undelivered { bot: facts.id, why });
        }
        let free = room.saturating_sub(resent);
        self.parts.insert(agent.id, (part, free));
    }

    /// How many untouchable bots name `agent` without being `counted`
    /// already: each keeps a slot.
    fn frozen_on(&self, agent: AgentId, counted: impl Fn(BotId) -> bool) -> usize {
        self.untouchable
            .iter()
            .filter(|&(&bot, &named)| named == Some(agent) && !counted(bot))
            .count()
    }

    fn detach_not_running(&mut self, bot: BotId, agent: AgentId) {
        self.detached.insert(bot);
        self.plan.detach.push(Detach {
            bot,
            agent,
            reason: DetachReason::NotRunning,
        });
    }

    /// Places every waiting bot that may run, first fit in agent order.
    fn place_waiting(&mut self) {
        // An account assigned anywhere stays there, stale agents included.
        for facts in self.bots.values() {
            if facts.agent.is_some() && !self.detached.contains(&facts.id) {
                self.fenced_accounts.insert(facts.identity.clone());
            }
        }
        let mut waiting: Vec<&BotFacts> = self
            .bots
            .values()
            .copied()
            .filter(|facts| facts.agent.is_none() || self.detached.contains(&facts.id))
            .filter(|facts| facts.qualifies.is_ok() && !self.fenced_bots.contains(&facts.id))
            .collect();
        waiting.sort_by_key(|facts| (facts.sticky, facts.id));
        for facts in waiting {
            if self.fenced_accounts.contains(&facts.identity) {
                continue;
            }
            let Some((part, free)) = self.parts.values_mut().find(|(_, free)| *free > 0) else {
                return;
            };
            *free = free.saturating_sub(1);
            part.run.push(running(facts.id, true));
            self.fenced_accounts.insert(facts.identity.clone());
        }
    }

    fn finish(mut self) -> Plan {
        for (mut part, _) in self.parts.into_values() {
            part.run.sort_by_key(|run| run.bot);
            part.release.sort_by_key(|release| release.bot);
            part.undelivered.sort_by_key(|undelivered| undelivered.bot);
            self.plan.agents.push(part);
        }
        self.plan
            .detach
            .sort_by_key(|detach| (detach.bot, detach.agent));
        self.plan
            .stray
            .sort_by_key(|stray| (stray.bot, stray.agent));
        self.plan
    }
}

/// `bot` running, `placed` now or already its agent's.
const fn running(bot: BotId, placed: bool) -> Run {
    Run {
        bot,
        desired: DesiredRunState::Running,
        stopping: None,
        placed,
    }
}

/// `bot` being stopped for `why`.
const fn stopping(bot: BotId, why: StopWhy) -> Run {
    Run {
        bot,
        desired: DesiredRunState::Stopped,
        stopping: Some(why),
        placed: false,
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rstest::rstest;
    use uuid::Uuid;

    use super::*;
    use crate::account::RefusalReason;

    const T0: &str = "2027-01-15T08:00:00Z";

    fn agent(n: u8) -> AgentId {
        AgentId::new_v7(T0.parse().unwrap(), [n; 10]).unwrap()
    }

    fn bot(n: u8) -> BotId {
        BotId::new_v7(T0.parse().unwrap(), [n; 10]).unwrap()
    }

    fn ms(n: u8) -> McIdentity {
        McIdentity::Microsoft(McUuid::try_from(Uuid::from_u128(u128::from(n) + 1)).unwrap())
    }

    fn off(name: &str) -> McIdentity {
        McIdentity::offline(&name.parse().unwrap())
    }

    fn cap(n: u32) -> AgentCapacity {
        AgentCapacity::try_from(n).unwrap()
    }

    /// Bot `n` of the Microsoft account `n`: placeable, unassigned.
    fn waiting(n: u8) -> BotFacts {
        BotFacts {
            id: bot(n),
            identity: ms(n),
            qualifies: Ok(()),
            agent: None,
            sticky: false,
        }
    }

    /// Bot `n` assigned to agent `a`.
    fn on(n: u8, a: u8) -> BotFacts {
        BotFacts {
            agent: Some(agent(a)),
            ..waiting(n)
        }
    }

    fn welcomed(a: u8, capacity: u32, listed: &[(u8, bool)]) -> AgentFacts {
        AgentFacts {
            id: agent(a),
            generation: u64::from(a),
            plannable: true,
            capacity: cap(capacity),
            phase: Phase::Welcomed {
                listed: listed
                    .iter()
                    .map(|&(n, rests)| Listed { bot: bot(n), rests })
                    .collect(),
            },
        }
    }

    fn reconciled(a: u8, capacity: u32, holds: &[(u8, Held)]) -> AgentFacts {
        AgentFacts {
            id: agent(a),
            generation: u64::from(a),
            plannable: true,
            capacity: cap(capacity),
            phase: Phase::Reconciled {
                holds: holds
                    .iter()
                    .map(|&(n, held)| HoldFacts { bot: bot(n), held })
                    .collect(),
            },
        }
    }

    const ASSIGNED: Held = Held::Assigned {
        stopping: None,
        rests: false,
    };

    fn plan_of(bots: &[BotFacts], releases: &[ReleaseFacts], agents: &[AgentFacts]) -> Plan {
        plan(&Facts {
            bots,
            releases,
            untouchable: &[],
            agents,
            gate_open: true,
        })
    }

    /// The plan with `untouchable` bots besides.
    fn plan_with(bots: &[BotFacts], untouchable: &[Untouchable], agents: &[AgentFacts]) -> Plan {
        plan(&Facts {
            bots,
            releases: &[],
            untouchable,
            agents,
            gate_open: true,
        })
    }

    /// Bot `n` of Microsoft account `n`, whose row doesn't read back,
    /// naming agent `a`.
    fn frozen(n: u8, a: Option<u8>) -> Untouchable {
        Untouchable {
            bot: bot(n),
            agent: a.map(agent),
            identity: Some(ms(n)),
        }
    }

    /// Agent `a`'s part of `plan`.
    fn part(plan: &Plan, a: u8) -> &AgentPlan {
        plan.agents
            .iter()
            .find(|part| part.agent == agent(a))
            .unwrap()
    }

    fn keep(n: u8) -> Run {
        Run {
            bot: bot(n),
            desired: DesiredRunState::Running,
            stopping: None,
            placed: false,
        }
    }

    fn place(n: u8) -> Run {
        Run {
            placed: true,
            ..keep(n)
        }
    }

    fn stop(n: u8, why: StopWhy) -> Run {
        Run {
            desired: DesiredRunState::Stopped,
            stopping: Some(why),
            ..keep(n)
        }
    }

    fn left(n: u8, unassign: Option<UnassignReason>, send: bool) -> Let {
        Let {
            bot: bot(n),
            unassign,
            send,
        }
    }

    fn detach(n: u8, a: u8) -> Detach {
        Detach {
            bot: bot(n),
            agent: agent(a),
            reason: DetachReason::NotRunning,
        }
    }

    // --- Which bots qualify ---------------------------------------------------------------

    fn facts() -> PlaceFacts {
        PlaceFacts {
            has_server: true,
            desired: DesiredRunState::Running,
            owner_enabled: true,
            kind: AccountKind::Microsoft,
            status: AccountStatus::Active,
            allow_offline: false,
        }
    }

    #[rstest]
    #[case::an_active_microsoft_account(facts(), Ok(()))]
    #[case::an_allowed_offline_account(
        PlaceFacts { kind: AccountKind::Offline, allow_offline: true, ..facts() },
        Ok(())
    )]
    #[case::no_server(PlaceFacts { has_server: false, ..facts() }, Err(Unplaced::NoServer))]
    #[case::stopped(
        PlaceFacts { desired: DesiredRunState::Stopped, ..facts() },
        Err(Unplaced::Stopped)
    )]
    #[case::owner_disabled(
        PlaceFacts { owner_enabled: false, ..facts() },
        Err(Unplaced::OwnerDisabled)
    )]
    #[case::pending(PlaceFacts { status: AccountStatus::Pending, ..facts() }, Err(Unplaced::Pending))]
    #[case::reauth_required(
        PlaceFacts { status: AccountStatus::ReauthRequired, ..facts() },
        Err(Unplaced::ReauthRequired)
    )]
    #[case::refused(
        PlaceFacts { status: AccountStatus::Refused(RefusalReason::NotOwned), ..facts() },
        Err(Unplaced::Refused)
    )]
    #[case::offline_not_allowed(
        PlaceFacts { kind: AccountKind::Offline, ..facts() },
        Err(Unplaced::OfflineNotAllowed)
    )]
    #[case::the_server_first(
        PlaceFacts {
            has_server: false,
            desired: DesiredRunState::Stopped,
            owner_enabled: false,
            status: AccountStatus::Pending,
            ..facts()
        },
        Err(Unplaced::NoServer)
    )]
    #[case::then_the_desired_state(
        PlaceFacts {
            desired: DesiredRunState::Stopped,
            owner_enabled: false,
            status: AccountStatus::Pending,
            ..facts()
        },
        Err(Unplaced::Stopped)
    )]
    #[case::then_the_owner(
        PlaceFacts { owner_enabled: false, status: AccountStatus::Pending, ..facts() },
        Err(Unplaced::OwnerDisabled)
    )]
    fn placeable_checks_the_server_the_desired_state_the_owner_then_the_account(
        #[case] facts: PlaceFacts,
        #[case] expected: Result<(), Unplaced>,
    ) {
        assert_eq!(placeable(&facts), expected);
    }

    #[test]
    fn every_reason_has_its_own_stable_name() {
        let names: Vec<_> = Unplaced::ALL.map(Unplaced::as_str).into();
        assert_eq!(
            names,
            [
                "no_server",
                "stopped",
                "owner_disabled",
                "pending",
                "reauth_required",
                "refused",
                "offline_not_allowed"
            ]
        );
        assert_eq!(
            DetachReason::ALL.map(DetachReason::as_str),
            ["removed", "not_running", "agent_released"]
        );
        assert_eq!(UnassignReason::Capacity.as_str(), "capacity");
        assert_eq!(
            UnassignReason::Unqualified(Unplaced::Pending).as_str(),
            "pending"
        );
    }

    #[test]
    fn an_offline_identity_ignores_case_and_differs_from_others() {
        assert_eq!(off("AfkBot1"), off("afkBOT1"));
        assert_ne!(off("AfkBot1"), off("AfkBot2"));
        let McIdentity::Offline(name) = off("AfkBot1") else {
            panic!("offline");
        };
        assert_eq!(name.as_str(), "afkbot1");
        assert_ne!(ms(1), ms(2));
    }

    // --- A welcome --------------------------------------------------------------------------

    #[test]
    fn a_welcome_always_gets_a_full_reconcile_even_empty() {
        let plan = plan_of(&[], &[], &[welcomed(1, 5, &[])]);

        assert_eq!(
            plan.agents,
            [AgentPlan {
                agent: agent(1),
                generation: 1,
                full: true,
                run: vec![],
                release: vec![],
                undelivered: vec![],
            }]
        );
    }

    #[test]
    fn a_welcome_keeps_its_listed_bots_and_gives_back_the_ones_it_lost() {
        // Bot 1 it runs; bot 2 it lost in a restart: both are its own.
        let plan = plan_of(&[on(1, 1), on(2, 1)], &[], &[welcomed(1, 5, &[(1, false)])]);

        assert_eq!(part(&plan, 1).run, [keep(1), keep(2)]);
        assert_eq!(plan.detach, []);
    }

    #[test]
    fn a_welcome_places_waiting_bots_within_what_it_may_add() {
        // Capacity 3, two listed: room for one bot it doesn't run yet.
        let plan = plan_of(
            &[on(1, 1), on(2, 1), waiting(3), waiting(4)],
            &[],
            &[welcomed(1, 3, &[(1, false), (2, false)])],
        );

        assert_eq!(part(&plan, 1).run, [keep(1), keep(2), place(3)]);
    }

    #[test]
    fn a_listed_bot_its_row_doesnt_assign_there_is_left_out_and_its_fence_persisted() {
        // Bot 1 belongs to agent 2's row; bot 2 to nobody; bot 9 has no row.
        let plan = plan_of(
            &[on(1, 2), waiting(2)],
            &[],
            &[welcomed(1, 5, &[(1, false), (2, false), (9, false)])],
        );

        let part = part(&plan, 1);
        assert_eq!(part.run, []);
        assert_eq!(
            part.release,
            [
                left(1, None, false),
                left(2, None, false),
                left(9, None, false)
            ]
        );
        assert_eq!(
            plan.stray,
            [
                Stray {
                    bot: bot(1),
                    agent: agent(1)
                },
                Stray {
                    bot: bot(2),
                    agent: agent(1)
                },
            ]
        );
    }

    #[test]
    fn a_listed_bot_with_another_agents_record_is_strayed_and_with_its_own_it_isnt() {
        let releases = [
            ReleaseFacts {
                bot: bot(1),
                agent: agent(2),
                identity: ms(1),
            },
            ReleaseFacts {
                bot: bot(2),
                agent: agent(1),
                identity: ms(2),
            },
        ];

        let plan = plan_of(&[], &releases, &[welcomed(1, 5, &[(1, false), (2, false)])]);

        assert_eq!(
            plan.stray,
            [Stray {
                bot: bot(1),
                agent: agent(1)
            }]
        );
        assert_eq!(
            part(&plan, 1).release,
            [left(1, None, false), left(2, None, false)]
        );
    }

    #[test]
    fn a_welcome_closes_its_records_of_bots_it_doesnt_list() {
        let releases = [ReleaseFacts {
            bot: bot(7),
            agent: agent(1),
            identity: ms(7),
        }];

        let plan = plan_of(&[], &releases, &[welcomed(1, 5, &[])]);

        assert_eq!(plan.detach, [detach(7, 1)]);
    }

    #[test]
    fn an_unqualified_bot_at_a_welcome_rests_out_stops_or_is_detached() {
        let unqualified = |n, a| BotFacts {
            qualifies: Err(Unplaced::Stopped),
            ..on(n, a)
        };
        // 1 listed and resting, 2 listed and online, 3 not listed.
        let plan = plan_of(
            &[unqualified(1, 1), unqualified(2, 1), unqualified(3, 1)],
            &[],
            &[welcomed(1, 5, &[(1, true), (2, false)])],
        );

        let part = part(&plan, 1);
        assert_eq!(part.run, [stop(2, StopWhy::Unqualified)]);
        assert_eq!(
            part.release,
            [left(
                1,
                Some(UnassignReason::Unqualified(Unplaced::Stopped)),
                false
            )]
        );
        assert_eq!(plan.detach, [detach(3, 1)]);
    }

    #[test]
    fn a_listed_bot_without_a_server_is_left_out_whether_it_rests_or_not() {
        let no_server = BotFacts {
            qualifies: Err(Unplaced::NoServer),
            ..on(1, 1)
        };

        let plan = plan_of(&[no_server], &[], &[welcomed(1, 5, &[(1, false)])]);

        assert_eq!(
            part(&plan, 1).release,
            [left(
                1,
                Some(UnassignReason::Unqualified(Unplaced::NoServer)),
                false
            )]
        );
    }

    #[test]
    fn an_agent_back_with_less_capacity_keeps_running_then_not_sticky_then_lowest_ids() {
        let sticky = |n| BotFacts {
            sticky: true,
            ..on(n, 1)
        };
        // Capacity 2. Listed: 3 (sticky), 4, 5 (resting). Not listed: 1, 2.
        let plan = plan_of(
            &[on(1, 1), on(2, 1), sticky(3), on(4, 1), on(5, 1)],
            &[],
            &[welcomed(1, 2, &[(3, false), (4, false), (5, true)])],
        );

        let part = part(&plan, 1);
        // 4 and 5 stay (running, not sticky); 3 moves: online, so stopped
        // first. 1 and 2 aren't running: detached at once, with nowhere to go.
        assert_eq!(part.run, [stop(3, StopWhy::Move), keep(4), keep(5)]);
        assert_eq!(part.release, []);
        assert_eq!(plan.detach, [detach(1, 1), detach(2, 1)]);
    }

    #[test]
    fn a_resting_surplus_bot_is_left_out_and_unassigned_for_capacity() {
        let plan = plan_of(
            &[on(1, 1), on(2, 1)],
            &[],
            &[welcomed(1, 1, &[(1, false), (2, true)])],
        );

        let part = part(&plan, 1);
        assert_eq!(part.run, [keep(1)]);
        assert_eq!(
            part.release,
            [left(2, Some(UnassignReason::Capacity), false)]
        );
    }

    #[test]
    fn surplus_bots_not_running_move_to_the_next_agent_in_the_same_plan() {
        let plan = plan_of(
            &[on(1, 1), on(2, 1)],
            &[],
            &[welcomed(1, 1, &[]), reconciled(2, 5, &[])],
        );

        assert_eq!(part(&plan, 1).run, [keep(1)]);
        assert_eq!(plan.detach, [detach(2, 1)]);
        assert_eq!(part(&plan, 2).run, [place(2)]);
    }

    // --- A reconciled stream -----------------------------------------------------------------

    #[test]
    fn a_held_bot_that_qualifies_keeps_running() {
        let plan = plan_of(&[on(1, 1)], &[], &[reconciled(1, 5, &[(1, ASSIGNED)])]);

        assert_eq!(part(&plan, 1).run, [keep(1)]);
        assert!(!part(&plan, 1).full);
    }

    #[test]
    fn a_held_bot_whose_row_is_gone_or_elsewhere_gets_an_unassign() {
        let plan = plan_of(
            &[on(2, 9)],
            &[],
            &[reconciled(1, 5, &[(1, ASSIGNED), (2, ASSIGNED)])],
        );

        assert_eq!(
            part(&plan, 1).release,
            [left(1, None, true), left(2, None, true)]
        );
    }

    #[rstest]
    #[case::online(None, false, Some(stop(1, StopWhy::Unqualified)), None)]
    #[case::stopping(
        Some(Stopping::Unqualified { overdue: false }),
        false,
        Some(stop(1, StopWhy::Unqualified)),
        None
    )]
    #[case::at_rest(
        Some(Stopping::Unqualified { overdue: false }),
        true,
        None,
        Some(left(1, Some(UnassignReason::Unqualified(Unplaced::Pending)), true))
    )]
    #[case::resting_already(
        None,
        true,
        None,
        Some(left(1, Some(UnassignReason::Unqualified(Unplaced::Pending)), true))
    )]
    #[case::overdue(
        Some(Stopping::Unqualified { overdue: true }),
        false,
        None,
        Some(left(1, Some(UnassignReason::Unqualified(Unplaced::Pending)), true))
    )]
    fn a_held_bot_that_stops_qualifying_is_stopped_then_unassigned_at_rest_or_its_bound(
        #[case] stopping: Option<Stopping>,
        #[case] rests: bool,
        #[case] run: Option<Run>,
        #[case] release: Option<Let>,
    ) {
        let pending = BotFacts {
            qualifies: Err(Unplaced::Pending),
            ..on(1, 1)
        };

        let plan = plan_of(
            &[pending],
            &[],
            &[reconciled(1, 5, &[(1, Held::Assigned { stopping, rests })])],
        );

        let part = part(&plan, 1);
        assert_eq!(part.run, run.into_iter().collect::<Vec<_>>());
        assert_eq!(part.release, release.into_iter().collect::<Vec<_>>());
    }

    #[test]
    fn a_bot_that_qualifies_again_while_stopping_runs_again() {
        let plan = plan_of(
            &[on(1, 1)],
            &[],
            &[reconciled(
                1,
                5,
                &[(
                    1,
                    Held::Assigned {
                        stopping: Some(Stopping::Unqualified { overdue: true }),
                        rests: true,
                    },
                )],
            )],
        );

        assert_eq!(part(&plan, 1).run, [keep(1)]);
    }

    #[rstest]
    #[case::still_online(false, Some(stop(1, StopWhy::Move)), None)]
    #[case::at_rest(true, None, Some(left(1, Some(UnassignReason::Capacity), true)))]
    fn a_moving_bot_is_unassigned_only_once_it_rests(
        #[case] rests: bool,
        #[case] run: Option<Run>,
        #[case] release: Option<Let>,
    ) {
        let plan = plan_of(
            &[on(1, 1)],
            &[],
            &[reconciled(
                1,
                5,
                &[(
                    1,
                    Held::Assigned {
                        stopping: Some(Stopping::Move),
                        rests,
                    },
                )],
            )],
        );

        let part = part(&plan, 1);
        assert_eq!(part.run, run.into_iter().collect::<Vec<_>>());
        assert_eq!(part.release, release.into_iter().collect::<Vec<_>>());
    }

    #[test]
    fn a_releasing_bot_waits_for_its_removed() {
        let plan = plan_of(
            &[on(1, 1)],
            &[],
            &[reconciled(1, 5, &[(1, Held::Releasing)])],
        );

        let part = part(&plan, 1);
        assert_eq!((part.run.len(), part.release.len()), (0, 0));
        assert_eq!(plan.detach, []);
    }

    // --- Placing --------------------------------------------------------------------------

    #[test]
    fn waiting_bots_fill_live_agents_in_enrollment_order_not_sticky_first() {
        let sticky = |n| BotFacts {
            sticky: true,
            ..waiting(n)
        };
        let plan = plan_of(
            &[sticky(1), waiting(2), waiting(3)],
            &[],
            // Agent 2 has more room and connected first: it doesn't matter.
            &[
                reconciled(3, 1, &[]),
                reconciled(2, 1, &[]),
                reconciled(4, 9, &[]),
            ],
        );

        assert_eq!(part(&plan, 2).run, [place(2)]);
        assert_eq!(part(&plan, 3).run, [place(3)]);
        assert_eq!(part(&plan, 4).run, [place(1)]);
    }

    #[test]
    fn free_capacity_counts_assigned_and_releasing_holds() {
        let plan = plan_of(
            &[on(1, 1), on(2, 1), waiting(3), waiting(4)],
            &[],
            &[
                reconciled(1, 3, &[(1, ASSIGNED), (2, Held::Releasing)]),
                reconciled(2, 5, &[]),
            ],
        );

        assert_eq!(part(&plan, 1).run, [keep(1), place(3)]);
        assert_eq!(part(&plan, 2).run, [place(4)]);
    }

    #[test]
    fn a_stale_agents_bots_stay_its_own_and_are_never_placed_elsewhere() {
        // Agent 9 has no live stream.
        let plan = plan_of(&[on(1, 9)], &[], &[reconciled(1, 5, &[])]);

        assert_eq!(part(&plan, 1).run, []);
        assert_eq!(plan.detach, []);
    }

    #[test]
    fn an_unqualified_waiting_bot_is_never_placed() {
        let unqualified = BotFacts {
            qualifies: Err(Unplaced::OfflineNotAllowed),
            ..waiting(1)
        };

        let plan = plan_of(&[unqualified], &[], &[reconciled(1, 5, &[])]);

        assert_eq!(part(&plan, 1).run, []);
    }

    #[test]
    fn an_agent_that_isnt_plannable_gets_nothing_but_its_bots_fence_the_others() {
        let mut disabled = reconciled(1, 5, &[(1, ASSIGNED)]);
        disabled.plannable = false;
        // Bot 2 plays the same account as the bot agent 1 holds.
        let same_account = BotFacts {
            identity: ms(1),
            ..waiting(2)
        };

        let plan = plan_of(
            &[waiting(1), same_account],
            &[],
            &[disabled, reconciled(2, 5, &[])],
        );

        assert_eq!(plan.agents.len(), 1);
        assert_eq!(part(&plan, 2).run, []);
    }

    #[test]
    fn a_hellos_listing_fences_its_bots_from_every_other_agent() {
        let plan = plan_of(
            &[waiting(1)],
            &[],
            &[reconciled(1, 5, &[]), welcomed(2, 5, &[(1, false)])],
        );

        assert_eq!(part(&plan, 1).run, []);
    }

    #[rstest]
    #[case::the_same_profile(ms(1), ms(1))]
    #[case::an_offline_name_in_another_case(off("AfkBot1"), off("afkbot1"))]
    fn an_open_record_fences_its_account_from_every_agent(
        #[case] recorded: McIdentity,
        #[case] relinked: McIdentity,
    ) {
        let releases = [ReleaseFacts {
            bot: bot(1),
            agent: agent(9),
            identity: recorded,
        }];
        let new_bot = BotFacts {
            identity: relinked,
            ..waiting(2)
        };

        let plan = plan_of(
            &[new_bot],
            &releases,
            &[reconciled(1, 5, &[]), reconciled(9, 5, &[])],
        );

        assert_eq!(part(&plan, 1).run, []);
        assert_eq!(part(&plan, 9).run, []);
    }

    #[test]
    fn a_microsoft_account_and_an_offline_one_never_fence_each_other() {
        let releases = [ReleaseFacts {
            bot: bot(1),
            agent: agent(9),
            identity: off("AfkBot1"),
        }];

        let plan = plan_of(&[waiting(2)], &releases, &[reconciled(1, 5, &[])]);

        assert_eq!(part(&plan, 1).run, [place(2)]);
    }

    #[test]
    fn an_account_assigned_anywhere_fences_another_bot_of_it() {
        let twin = BotFacts {
            identity: ms(1),
            ..waiting(2)
        };

        let plan = plan_of(&[on(1, 9), twin], &[], &[reconciled(1, 5, &[])]);

        assert_eq!(part(&plan, 1).run, []);
    }

    #[test]
    fn two_waiting_bots_of_one_account_are_never_placed_together() {
        let twin = BotFacts {
            identity: ms(1),
            ..waiting(2)
        };

        let plan = plan_of(&[waiting(1), twin], &[], &[reconciled(1, 5, &[])]);

        assert_eq!(part(&plan, 1).run, [place(1)]);
    }

    #[test]
    fn a_closed_gate_places_nothing_but_still_sends_what_is_stored() {
        let plan = plan(&Facts {
            bots: &[on(1, 1), waiting(2)],
            releases: &[],
            untouchable: &[],
            agents: &[welcomed(1, 5, &[])],
            gate_open: false,
        });

        assert_eq!(part(&plan, 1).run, [keep(1)]);
    }

    // --- Assignments an agent never got --------------------------------------------------

    #[test]
    fn an_assignment_its_live_agent_doesnt_hold_is_sent_again_and_keeps_its_slot() {
        let plan = plan_of(
            &[on(1, 1), waiting(2)],
            &[],
            &[reconciled(1, 1, &[]), reconciled(2, 5, &[])],
        );

        assert_eq!(part(&plan, 1).run, [keep(1)]);
        assert_eq!(
            part(&plan, 2).run,
            [place(2)],
            "agent-1's one slot is taken"
        );
    }

    #[test]
    fn an_undelivered_assignment_that_no_longer_qualifies_is_recorded_not_sent() {
        let stopped = BotFacts {
            qualifies: Err(Unplaced::Stopped),
            ..on(1, 1)
        };

        let plan = plan_of(&[stopped], &[], &[reconciled(1, 5, &[])]);

        assert_eq!(part(&plan, 1).run, []);
        assert_eq!(
            part(&plan, 1).undelivered,
            [Undelivered {
                bot: bot(1),
                why: UndeliveredWhy::Unqualified(Unplaced::Stopped),
            }]
        );
    }

    #[test]
    fn an_undelivered_assignment_beyond_its_agents_room_is_recorded_not_sent() {
        let plan = plan_of(
            &[on(1, 1), on(2, 1), on(3, 1)],
            &[],
            &[reconciled(1, 2, &[(1, ASSIGNED)])],
        );

        assert_eq!(part(&plan, 1).run, [keep(1), keep(2)]);
        assert_eq!(
            part(&plan, 1).undelivered,
            [Undelivered {
                bot: bot(3),
                why: UndeliveredWhy::NoRoom,
            }]
        );
    }

    // --- Bots that don't read back --------------------------------------------------------

    #[test]
    fn an_untouchable_bot_its_agent_holds_is_left_alone_and_keeps_its_slot() {
        let plan = plan_with(
            &[waiting(2)],
            &[frozen(1, Some(1))],
            &[reconciled(1, 1, &[(1, ASSIGNED)]), reconciled(2, 5, &[])],
        );

        assert_eq!(part(&plan, 1).release, [], "never unassigned");
        assert_eq!(part(&plan, 1).run, []);
        assert_eq!(
            part(&plan, 2).run,
            [place(2)],
            "its agent's one slot is taken"
        );
    }

    #[test]
    fn an_untouchable_bot_is_never_placed_and_fences_its_account() {
        let twin = BotFacts {
            identity: ms(1),
            ..waiting(2)
        };

        let plan = plan_with(
            &[twin, waiting(3)],
            &[frozen(1, None)],
            &[reconciled(1, 5, &[])],
        );

        assert_eq!(part(&plan, 1).run, [place(3)]);
    }

    #[test]
    fn a_welcome_listing_its_own_untouchable_bot_waits_until_it_reads_back() {
        let plan = plan_with(
            &[on(2, 1), waiting(3)],
            &[frozen(1, Some(1))],
            &[welcomed(1, 5, &[(1, false), (2, false)])],
        );

        assert!(
            plan.agents.iter().all(|part| part.agent != agent(1)),
            "no full reconcile can keep a bot whose spec doesn't read back"
        );
        assert_eq!(plan.detach, []);
    }

    #[test]
    fn a_welcome_without_its_own_untouchable_bot_keeps_it_assigned_and_counts_it() {
        let plan = plan_with(
            &[waiting(2), waiting(3)],
            &[frozen(1, Some(1))],
            &[welcomed(1, 2, &[])],
        );

        assert_eq!(plan.detach, [], "never moved");
        assert_eq!(
            part(&plan, 1).run,
            [place(2)],
            "one of its two slots is taken"
        );
    }

    #[test]
    fn another_agents_untouchable_bot_in_a_hello_is_let_go() {
        let plan = plan_with(&[], &[frozen(1, Some(2))], &[welcomed(1, 5, &[(1, false)])]);

        assert_eq!(part(&plan, 1).release, [left(1, None, false)]);
    }

    // --- Properties -----------------------------------------------------------------------

    /// A scenario of up to 8 bots over 4 accounts and up to 3 agents.
    fn scenario() -> impl Strategy<Value = (Vec<BotFacts>, Vec<ReleaseFacts>, Vec<AgentFacts>)> {
        let bots = proptest::collection::vec(
            (
                0..4_u8,
                any::<bool>(),
                prop::option::of(1..5_u8),
                any::<bool>(),
            ),
            0..8,
        );
        let releases = proptest::collection::vec((10..14_u8, 0..4_u8, 1..5_u8), 0..3);
        let agents = proptest::collection::vec(
            (
                any::<bool>(),
                1..4_u32,
                any::<bool>(),
                proptest::collection::vec((0..14_u8, 0..4_u8), 0..5),
            ),
            0..4,
        );
        (bots, releases, agents).prop_map(|(bots, releases, agents)| {
            let bots = bots
                .into_iter()
                .enumerate()
                .map(|(n, (account, qualifies, assigned, sticky))| BotFacts {
                    id: bot(u8::try_from(n).unwrap()),
                    identity: ms(account),
                    qualifies: if qualifies {
                        Ok(())
                    } else {
                        Err(Unplaced::Stopped)
                    },
                    agent: assigned.map(agent),
                    sticky,
                })
                .collect();
            let releases = releases
                .into_iter()
                .map(|(n, account, a)| ReleaseFacts {
                    bot: bot(n),
                    agent: agent(a),
                    identity: ms(account),
                })
                .collect();
            let agents = agents
                .into_iter()
                .enumerate()
                .map(|(n, (welcome, capacity, plannable, items))| {
                    let id = u8::try_from(n + 1).unwrap();
                    let mut seen = BTreeSet::new();
                    let items: Vec<_> =
                        items.into_iter().filter(|(b, _)| seen.insert(*b)).collect();
                    let mut facts = if welcome {
                        welcomed(
                            id,
                            capacity,
                            &items
                                .iter()
                                .map(|&(b, s)| (b, s % 2 == 0))
                                .collect::<Vec<_>>(),
                        )
                    } else {
                        let holds: Vec<_> = items
                            .iter()
                            .map(|&(b, s)| {
                                let held = match s {
                                    0 => Held::Releasing,
                                    1 => ASSIGNED,
                                    2 => Held::Assigned {
                                        stopping: Some(Stopping::Move),
                                        rests: false,
                                    },
                                    _ => Held::Assigned {
                                        stopping: Some(Stopping::Unqualified { overdue: true }),
                                        rests: true,
                                    },
                                };
                                (b, held)
                            })
                            .collect();
                        reconciled(id, capacity, &holds)
                    };
                    facts.plannable = plannable;
                    facts
                })
                .collect();
            (bots, releases, agents)
        })
    }

    /// Every bot each agent holds or lists before the plan.
    fn held_before(agents: &[AgentFacts]) -> Vec<(AgentId, BotId)> {
        agents
            .iter()
            .flat_map(|agent| {
                let bots: Vec<BotId> = match &agent.phase {
                    Phase::Welcomed { listed } => listed.iter().map(|listed| listed.bot).collect(),
                    Phase::Reconciled { holds } => holds.iter().map(|hold| hold.bot).collect(),
                };
                bots.into_iter().map(move |bot| (agent.id, bot))
            })
            .collect()
    }

    proptest! {
        #[test]
        fn a_placement_never_crosses_a_fence_and_never_overfills(
            (bots, releases, agents) in scenario(),
        ) {
            let plan = plan_of(&bots, &releases, &agents);
            let identity = |id: BotId| bots.iter().find(|bot| bot.id == id).map(|bot| bot.identity.clone());
            let held = held_before(&agents);
            let mut placed_identities = HashSet::new();
            for part in &plan.agents {
                let agent = agents.iter().find(|agent| agent.id == part.agent).unwrap();
                prop_assert!(agent.plannable, "only plannable agents get a part");
                let new: Vec<&Run> = part.run.iter().filter(|run| run.placed).collect();
                for run in &new {
                    let facts = bots.iter().find(|bot| bot.id == run.bot).unwrap();
                    prop_assert!(
                        facts.agent.is_none() || plan.detach.iter().any(|detach| detach.bot == run.bot),
                        "placed only from no agent, or detached in the same plan"
                    );
                    prop_assert!(facts.qualifies.is_ok());
                    prop_assert!(
                        !held.iter().any(|(_, bot)| *bot == run.bot),
                        "a held or listed bot is never placed"
                    );
                    prop_assert!(
                        !releases.iter().any(|record| record.bot == run.bot || Some(record.identity.clone()) == identity(run.bot)),
                        "an open record fences the bot and its account"
                    );
                    prop_assert!(
                        placed_identities.insert(facts.identity.clone()),
                        "one account is placed once"
                    );
                }
                let occupied = match &agent.phase {
                    Phase::Welcomed { listed } => listed.len()
                        + part.run.iter().filter(|run| !listed.iter().any(|listed| listed.bot == run.bot)).count(),
                    Phase::Reconciled { holds } => holds.len()
                        + part.run.iter().filter(|run| !holds.iter().any(|hold| hold.bot == run.bot)).count(),
                };
                let before = match &agent.phase {
                    Phase::Welcomed { listed } => listed.len(),
                    Phase::Reconciled { holds } => holds.len(),
                };
                prop_assert!(
                    occupied <= before.max(usize::from(agent.capacity.get())),
                    "an agent gets nothing beyond its capacity"
                );
            }
        }

        #[test]
        fn the_plan_never_depends_on_the_input_order(
            (bots, releases, agents) in scenario(),
        ) {
            let forward = plan_of(&bots, &releases, &agents);
            let mut reversed_bots = bots.clone();
            reversed_bots.reverse();
            let mut reversed_releases = releases.clone();
            reversed_releases.reverse();
            let mut reversed_agents = agents.clone();
            reversed_agents.reverse();

            prop_assert_eq!(forward, plan_of(&reversed_bots, &reversed_releases, &reversed_agents));
        }
    }
}
