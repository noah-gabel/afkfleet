//! One case's world: its folder, its log, its ledger, the server and the
//! Owner logged in to it, and what a case asks of them.

use std::sync::atomic::{AtomicU64, Ordering};

use fleet_core::audit::AuditAction;
use fleet_core::bot::{BotSnapshot, BotState};
use fleet_core::id::BotId;
use fleet_server::grpc::registry::ViewPhase;
use fleet_server::ports::live_agents::LiveAgents as _;
use tempfile::TempDir;
use tracing::subscriber::DefaultGuard;

use super::agent::Agent;
use super::client::Owner;
use super::ledger::Ledger;
use super::logs::Logs;
use super::server::{Server, ServerOptions};

/// A case's world; see the [module docs](self).
#[derive(Debug)]
pub(crate) struct World {
    pub(crate) logs: Logs,
    pub(crate) ledger: Ledger,
    pub(crate) server: Server,
    pub(crate) owner: Owner,
    seeds: AtomicU64,
    _guard: DefaultGuard,
    // Last, so the folder goes after everything that holds its files.
    dir: TempDir,
}

impl World {
    /// A server as `options` say, with its Owner logged in.
    pub(crate) async fn new(options: ServerOptions) -> Self {
        let (logs, guard) = Logs::capture();
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path(), options).await;
        let owner = Owner::log_in(&server).await;
        Self {
            logs,
            ledger: Ledger::default(),
            server,
            owner,
            seeds: AtomicU64::new(1),
            _guard: guard,
            dir,
        }
    }

    /// A new agent `name`, enrolled and not yet running.
    pub(crate) async fn agent(&self, name: &str) -> Agent {
        let seed = self.seeds.fetch_add(1, Ordering::Relaxed);
        Agent::enroll(&self.server, self.dir.path(), name, seed).await
    }

    /// `bot`'s latest report on `agent`'s live stream, which holds it
    /// assigned, and that stream's generation.
    pub(crate) fn held(&self, agent: &Agent, bot: BotId) -> Option<(u64, Option<BotSnapshot>)> {
        self.server
            .registry()
            .held(agent.id, bot)
            .map(|held| (held.generation, held.report))
    }

    /// Waits until `agent`'s live stream holds `bot` and reports it Online,
    /// on a generation after `after`; returns that generation.
    pub(crate) async fn online(&self, agent: &Agent, bot: BotId, after: u64) -> u64 {
        let mut generation = 0;
        self.logs
            .until(
                &format!("bot {bot} online on {}", agent.name),
                async || match self.held(agent, bot) {
                    Some((seen, Some(report)))
                        if seen > after && matches!(report.state, BotState::Online { .. }) =>
                    {
                        generation = seen;
                        true
                    }
                    _ => false,
                },
            )
            .await;
        generation
    }

    /// Waits until `agent` has a reconciled stream after generation `after`:
    /// its welcome's pass ran, so the placements count it from now on;
    /// returns the generation.
    pub(crate) async fn reconciled(&self, agent: &Agent, after: u64) -> u64 {
        let mut generation = 0;
        self.logs
            .until(&format!("{} reconciled", agent.name), async || {
                let view = self.server.registry().placement_view();
                match view.iter().find(|live| live.agent == agent.id) {
                    Some(live)
                        if live.generation > after
                            && matches!(live.phase, ViewPhase::Reconciled(_)) =>
                    {
                        generation = live.generation;
                        true
                    }
                    _ => false,
                }
            })
            .await;
        generation
    }

    /// Waits until the server has recorded `count` entries of `action`.
    pub(crate) async fn entries(&self, action: &AuditAction, count: usize) {
        self.logs
            .until(&format!("{count} × {action:?}"), async || {
                self.server.entries(action).await.len() >= count
            })
            .await;
    }

    /// What every case ends with: no account ran twice, no bot ran where
    /// the server didn't put it; then the server stops cleanly.
    pub(crate) async fn end(mut self, agents: Vec<Agent>) {
        self.ledger.assert_clean();
        assert_eq!(
            self.server.entries(&AuditAction::BOT_STRAY).await.len(),
            0,
            "no agent ran a bot the server didn't give it"
        );
        drop(agents);
        self.server.stop().await;
    }
}
