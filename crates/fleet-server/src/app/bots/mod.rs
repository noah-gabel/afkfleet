//! [`BotService`]: the bots as each viewer reads them, what a user changes
//! on one, and the chat lines a user sends one (Plan.md P10.5, P11.2,
//! P11.3; ADR-0019, S41, S46–S56; ADR-0020, S4–S8).
//!
//! **Reads.** A bot's state is computed when it's read, never stored as
//! such: from its row (placeable or not, and its last stored state), the
//! agent its row names (that agent's status, from fleet-core's rule over its
//! row, its latest unrevoked certificate and its live stream, and when the
//! server last heard from it), what that agent's live stream reported, and
//! the latest report the server keeps in memory, through fleet-core's
//! [`shown`]. A stale agent's bots read Unknown; nothing is written when an
//! agent goes quiet. A viewer reads only the bots of the accounts they may
//! see, with their own grant; the caller asks `authorize()`.
//!
//! **The chat history** ([`history`]) pages a bot's chat for a viewer who
//! may see the bot (ADR-0020, S6, S9, U3).
//!
//! **Writes** ([`settings`], [`lifecycle`], [`chat`]) each run in one short
//! transaction that reads the acting user, the bot and its account again and
//! asks `authorize()` again on what it read, write only what changed, never
//! the bot's agent (the reconciler alone assigns, S36), and commit with their
//! audit entry; a write that changes nothing writes and records nothing. The
//! reconciler is told after each commit (`Change::Store`).

pub mod chat;
pub mod history;
pub mod lifecycle;
pub mod settings;

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::agent::{AgentStatus, status};
use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::authz::{Actor, AuthzError, CommandAllowlist, Permission, authorize};
use fleet_core::bot::{
    AssignedTo, CommandRefusal, LastReport, ShownFacts, ShownState, StreamFacts, shown,
};
use fleet_core::id::{AgentId, BotId};
use fleet_core::placement::{PlaceFacts, placeable};
use fleet_core::system::{Clock, SecureRandom};
use fleet_core::value::AgentName;
use tracing::{debug, error};

pub use chat::{ChatOutcome, ChatRoute, ChatSendDone, ChatSendError, ChatWait};
pub use history::{ChatHistory, ChatHistoryError};
pub use lifecycle::{BotAction, Controlled};
pub use settings::Configured;

use super::agents::last_seen;
use super::audit::AuditService;
use super::events::EventBus;
use super::users::{ActingUser, recheck};
use crate::ports::accounts::ListedAccount;
use crate::ports::agents::ListedAgent;
use crate::ports::audit::NewAuditEntry;
use crate::ports::bots::{Bot, BotRecord, ListedBot};
use crate::ports::live_agents::LiveAgents;
use crate::ports::page::{Page, Paged};
use crate::ports::reconcile::{NoReconcile, Reconcile};
use crate::ports::store::{Store, StoreError, WriteTx};

/// The agents one read found, each read once.
type Agents = HashMap<AgentId, Option<ListedAgent>>;

/// A bot as one viewer reads it; see the [module docs](self).
#[derive(Debug, Clone, PartialEq)]
pub struct BotView {
    /// Its row, account, owner and mode.
    pub record: BotRecord,
    /// Its account as the viewer sees it, with their grant on it.
    pub account: ListedAccount,
    /// What its state reads as now.
    pub shown: ShownState,
    /// The agent its row assigns it to, as the agents' list shows it.
    pub agent: Option<BotAgentView>,
}

/// The agent a bot is assigned to, as the read found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotAgentView {
    /// Its ID.
    pub id: AgentId,
    /// Its name.
    pub name: AgentName,
    /// Its status ([`fleet_core::agent::status`]).
    pub status: AgentStatus,
}

/// Why a list of bots couldn't be read.
#[derive(Debug, thiserror::Error)]
pub enum ListBotsError {
    /// The database listed a bot `authorize()` doesn't let the viewer see: a
    /// bug in the query, answered as an internal error rather than shown.
    #[error("the database listed a bot the viewer may not see")]
    Disagreement,
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Why a bot write wasn't made.
#[derive(Debug, thiserror::Error)]
pub enum BotWriteError {
    /// The acting session ended, or its user was disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// The bot doesn't exist.
    #[error("the bot doesn't exist")]
    NotFound,
    /// `authorize()` refused it on the fresh facts.
    #[error("the write isn't allowed")]
    Denied(#[source] AuthzError),
    /// The mode doesn't exist, or the user may not see it: one answer, so
    /// modes can't be probed.
    #[error("the mode isn't one the user may use")]
    UnusableMode,
    /// A restart, reset or resume that can't act on the bot now.
    #[error(transparent)]
    Refused(CommandRefusal),
    /// The command couldn't be written, which a command never is.
    #[error("the command can't be written")]
    Unsendable,
    /// An audit entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What a list logs when the database and `authorize()` disagree.
const DISAGREEMENT: &str = "the database listed a bot authorize() refuses; nothing was shown";

/// The bots' use cases; see the [module docs](self).
#[derive(Debug, Clone)]
pub struct BotService {
    clock: Arc<dyn Clock>,
    random: Arc<dyn SecureRandom>,
    store: Arc<dyn Store>,
    live: Arc<dyn LiveAgents>,
    events: EventBus,
    audit: AuditService,
    allow_offline: bool,
    allowlist: CommandAllowlist,
    reconcile: Arc<dyn Reconcile>,
    chat_wait: ChatWait,
}

impl BotService {
    /// The service over `store`, reading the time from `clock`, the live
    /// streams from `live` and their agents' answers to chat from `events`,
    /// and minting chat requests' IDs from `random`; offline accounts' bots
    /// qualify only when `allow_offline`. Every command needs Manage in a
    /// mode and in chat until [`BotService::with_allowlist`] lists it, and a
    /// chat line's wait is [`ChatWait::default`] until
    /// [`BotService::with_chat_wait`] sets it.
    #[must_use]
    pub fn new(
        clock: Arc<dyn Clock>,
        random: Arc<dyn SecureRandom>,
        store: Arc<dyn Store>,
        live: Arc<dyn LiveAgents>,
        events: EventBus,
        allow_offline: bool,
    ) -> Self {
        let audit = AuditService::new(Arc::clone(&clock), Arc::clone(&store));
        Self {
            clock,
            random,
            store,
            live,
            events,
            audit,
            allow_offline,
            allowlist: CommandAllowlist::default(),
            reconcile: Arc::new(NoReconcile),
            chat_wait: ChatWait::default(),
        }
    }

    /// The service, with the commands Control holders may send through a
    /// mode or in chat (`[chat] command_allowlist`).
    #[must_use]
    pub fn with_allowlist(self, allowlist: CommandAllowlist) -> Self {
        Self { allowlist, ..self }
    }

    /// The service, telling `reconcile` to look again after each write.
    #[must_use]
    pub fn with_reconcile(self, reconcile: Arc<dyn Reconcile>) -> Self {
        Self { reconcile, ..self }
    }

    /// The service, waiting for a chat line's answer as `chat_wait` says:
    /// serve's is under its request timeout.
    #[must_use]
    pub fn with_chat_wait(self, chat_wait: ChatWait) -> Self {
        Self { chat_wait, ..self }
    }

    /// The bot `id` as `viewer` reads it, whether or not they may: the
    /// caller asks `authorize()` with its account's context.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn view(&self, viewer: &Actor, id: BotId) -> Result<Option<BotView>, StoreError> {
        let Some(listed) = self.store.bots().listed(id, viewer.id).await? else {
            return Ok(None);
        };
        let view = self.read(listed, &mut Agents::new()).await?;
        Ok(Some(view))
    }

    /// The bots `viewer` may see on `page`, newest first, and the next
    /// page's cursor; every one is checked with `authorize()`.
    ///
    /// # Errors
    /// [`ListBotsError::Disagreement`], or [`ListBotsError::Store`].
    pub async fn list(
        &self,
        viewer: &Actor,
        page: Page<BotId>,
    ) -> Result<Paged<BotView, BotId>, ListBotsError> {
        let rows = self.store.bots().visible(*viewer, page).await?;
        if let Some(row) = rows
            .iter()
            .find(|row| authorize(viewer, Permission::ViewBot, &row.account.context()).is_err())
        {
            error!(bot_id = %row.record.bot.id, "{DISAGREEMENT}");
            return Err(ListBotsError::Disagreement);
        }
        let rows = Paged::split(rows, page.limit, |row| row.record.bot.id);
        let mut agents = Agents::new();
        let mut items = Vec::with_capacity(rows.items.len());
        for row in rows.items {
            items.push(self.read(row, &mut agents).await?);
        }
        Ok(Paged {
            items,
            next: rows.next,
        })
    }

    /// The newest last state the server knows of `bot`, whose row stores
    /// `stored`: that, or the latest report it keeps in memory, as the
    /// reconciler restores it (ADR-0019, S40, R35).
    fn last_known(&self, bot: BotId, stored: Option<LastReport>) -> Option<LastReport> {
        let kept = self.live.latest(bot).map(|report| report.last());
        [stored, kept]
            .into_iter()
            .flatten()
            .max_by_key(|last| last.at)
    }

    /// `listed` with what its state reads as now; its agent comes from
    /// `agents`, or is read and kept there.
    async fn read(&self, listed: ListedBot, agents: &mut Agents) -> Result<BotView, StoreError> {
        let ListedBot { record, account } = listed;
        let now = self.clock.now();
        let (assigned, agent) = match record.bot.agent {
            None => (None, None),
            Some(agent) => {
                let listed = if let Some(listed) = agents.get(&agent) {
                    listed.clone()
                } else {
                    let listed = self.store.agents().listed(agent).await?;
                    agents.insert(agent, listed.clone());
                    listed
                };
                // The foreign key keeps every bot's agent.
                match listed {
                    Some(listed) => {
                        let assigned = self.assigned_to(&listed, record.bot.id, now);
                        let agent = BotAgentView {
                            id: listed.agent.id,
                            name: listed.agent.name.clone(),
                            status: assigned.status,
                        };
                        (Some(assigned), Some(agent))
                    }
                    None => (None, None),
                }
            }
        };
        let qualifies = placeable(&PlaceFacts {
            has_server: record.bot.server.is_some(),
            desired: record.bot.desired,
            owner_enabled: record.owner_enabled,
            kind: record.kind,
            status: record.status,
            allow_offline: self.allow_offline,
        });
        let shown = shown(ShownFacts {
            qualifies,
            last: self.last_known(record.bot.id, record.bot.last),
            assigned,
        });
        Ok(BotView {
            record,
            account,
            shown,
            agent,
        })
    }

    /// A write's audit entry: `action` by `by` on `bot`, with `metadata`.
    fn entry(
        &self,
        by: &ActingUser,
        action: AuditAction,
        bot: BotId,
        metadata: AuditMetadata,
    ) -> NewAuditEntry {
        self.audit
            .entry(action, AuditOutcome::Success)
            .actor(by.actor.id)
            .ip(by.ip)
            .target(AuditTarget::Bot(bot))
            .metadata(metadata)
    }

    /// `listed`, the agent `bot` is assigned to, as the bot's read finds it
    /// at `now`.
    fn assigned_to(&self, listed: &ListedAgent, bot: BotId, now: DateTime<Utc>) -> AssignedTo {
        let agent = listed.agent.id;
        let live = self.live.live(agent);
        let stream = live.map(|live| {
            let held = self.live.held(agent, bot);
            StreamFacts {
                connected_since: live.connected_at,
                held_since: held.as_ref().map(|held| held.since),
                report: held.and_then(|held| held.report),
            }
        });
        AssignedTo {
            status: status(listed.agent.enabled, listed.not_after, live.is_some(), now),
            last_seen: last_seen(listed.agent.last_seen_at, live),
            stream,
        }
    }
}

/// Reads the acting user `by` again, then the bot `id` and its account as
/// they see it now, inside `tx`: what every write decides on.
async fn reread(
    tx: &mut dyn WriteTx,
    by: &ActingUser,
    id: BotId,
) -> Result<(Actor, Bot, ListedAccount), BotWriteError> {
    let Some(acting) = recheck(tx, by).await? else {
        debug!(user_id = %by.actor.id, bot_id = %id, "a bot write was refused: the session is gone");
        return Err(BotWriteError::SessionGone);
    };
    let Some(bot) = tx.bots().find(id).await? else {
        return Err(BotWriteError::NotFound);
    };
    let Some(account) = tx.accounts().listed(bot.account, acting.actor.id).await? else {
        return Err(BotWriteError::NotFound);
    };
    Ok((acting.actor, bot, account))
}
