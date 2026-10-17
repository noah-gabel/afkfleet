//! [`BotService`]: the bots as they read, each with what its state reads as
//! now (Plan.md P10.5, P11.2; ADR-0019, S41). Read-only: E's endpoints call
//! it, and authorize first.
//!
//! A bot's state is computed when it's read, never stored as such: from its
//! row (placeable or not, and its last stored state), the agent its row
//! names (that agent's status, from fleet-core's rule over its row, its
//! latest unrevoked certificate and its live stream, and when the server
//! last heard from it) and what that agent's live stream reported, through
//! fleet-core's [`shown`]. A stale agent's
//! bots read Unknown; nothing is written when an agent goes quiet.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::agent::status;
use fleet_core::bot::{AssignedTo, ShownFacts, ShownState, StreamFacts, shown};
use fleet_core::id::{AgentId, BotId};
use fleet_core::placement::{PlaceFacts, placeable};
use fleet_core::system::Clock;

use super::agents::last_seen;
use crate::ports::agents::ListedAgent;
use crate::ports::bots::BotRecord;
use crate::ports::live_agents::LiveAgents;
use crate::ports::page::{Page, Paged};
use crate::ports::store::{Store, StoreError};

/// The agents one read found, each read once.
type Agents = HashMap<AgentId, Option<ListedAgent>>;

/// A bot as it reads; see the [module docs](self).
#[derive(Debug, Clone, PartialEq)]
pub struct BotView {
    /// Its row, account, owner and mode.
    pub record: BotRecord,
    /// What its state reads as now.
    pub shown: ShownState,
}

/// The bots' reads; see the [module docs](self).
#[derive(Debug, Clone)]
pub struct BotService {
    clock: Arc<dyn Clock>,
    store: Arc<dyn Store>,
    live: Arc<dyn LiveAgents>,
    allow_offline: bool,
}

impl BotService {
    /// The service over `store`, reading the time from `clock` and the live
    /// streams from `live`; offline accounts' bots qualify only when
    /// `allow_offline`.
    #[must_use]
    pub fn new(
        clock: Arc<dyn Clock>,
        store: Arc<dyn Store>,
        live: Arc<dyn LiveAgents>,
        allow_offline: bool,
    ) -> Self {
        Self {
            clock,
            store,
            live,
            allow_offline,
        }
    }

    /// The bot `id`, if it exists.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn view(&self, id: BotId) -> Result<Option<BotView>, StoreError> {
        let Some(record) = self.store.bots().record(id).await? else {
            return Ok(None);
        };
        let view = self.read(record, &mut Agents::new()).await?;
        Ok(Some(view))
    }

    /// The bots of `page`, newest first, and the next page's cursor.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn list(&self, page: Page<BotId>) -> Result<Paged<BotView, BotId>, StoreError> {
        let rows = self.store.bots().records(page).await?;
        let records = Paged::split(rows, page.limit, |record| record.bot.id);
        let mut agents = Agents::new();
        let mut items = Vec::with_capacity(records.items.len());
        for record in records.items {
            items.push(self.read(record, &mut agents).await?);
        }
        Ok(Paged {
            items,
            next: records.next,
        })
    }

    /// `record` with what its state reads as now; its agent comes from
    /// `agents`, or is read and kept there.
    async fn read(&self, record: BotRecord, agents: &mut Agents) -> Result<BotView, StoreError> {
        let now = self.clock.now();
        let assigned = match record.bot.agent {
            None => None,
            Some(agent) => {
                let listed = if let Some(listed) = agents.get(&agent) {
                    listed.clone()
                } else {
                    let listed = self.store.agents().listed(agent).await?;
                    agents.insert(agent, listed.clone());
                    listed
                };
                // The foreign key keeps every bot's agent.
                listed.map(|listed| self.assigned_to(&listed, record.bot.id, now))
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
            last: record.bot.last,
            assigned,
        });
        Ok(BotView { record, shown })
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
