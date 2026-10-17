//! Tests for the assignments' service and releasing a disabled agent's bots
//! (Plan.md P10.5; ADR-0019, S32, S36, S40, S43, R25): each change of a
//! bot's agent commits with its own entry and no actor, a change that
//! changes nothing writes nothing, the latest report is stored with the
//! decision, and the host's release clears a disabled agent's bots and
//! records, refusing an enabled or unknown agent.
//!
//! Each test gets a fresh database from `#[sqlx::test]`. Time isn't paused:
//! sqlx's worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget, RecordedValue};
use fleet_core::authz::Role;
use fleet_core::bot::{FailReason, LastReport, LastState};
use fleet_core::id::{AccountId, AgentId, BotId, UserId};
use fleet_core::mode::ModePreset;
use fleet_core::placement::{DetachReason, UnassignReason, Unplaced};
use fleet_core::value::{AgentName, Username};
use fleet_server::app::agents::{AgentService, AssignmentService, ReleaseAgentError};
use fleet_server::grpc::registry::AgentRegistry;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::agents::NewAgent;
use fleet_server::ports::assignments::Assignments as _;
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditRecord, NewAuditEntry};
use fleet_server::ports::bots::NewBot;
use fleet_server::ports::store::Store as _;
use fleet_server::ports::users::{NewUser, PasswordHash};
use fleet_testkit::system::ManualClock;
use secrecy::SecretString;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// The time everything starts at, in milliseconds.
const T0: i64 = 1_800_000_000_000;
/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

fn agent(n: u8) -> AgentId {
    AgentId::new_v7(at(T0), [n; 10]).unwrap()
}

fn bot(n: u8) -> BotId {
    BotId::new_v7(at(T0 + i64::from(n)), [n; 10]).unwrap()
}

fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from("test.assignments_setup").unwrap(),
        AuditOutcome::Success,
    )
}

struct Setup {
    db: Database,
    owner: UserId,
    assignments: AssignmentService,
    agents: AgentService,
}

impl Setup {
    async fn new(options: SqliteConnectOptions) -> Self {
        let db = Database::connect(options, DatabaseOptions::default())
            .await
            .unwrap();
        let clock = ManualClock::new(at(T0 + 1_000));
        let owner = UserId::new_v7(at(T0), [1; 10]).unwrap();
        let mut tx = db.write().await.unwrap();
        tx.users()
            .insert(&NewUser {
                id: owner,
                username: Username::try_from("owner").unwrap(),
                password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
                role: Role::Owner,
                created_at: at(T0),
            })
            .await
            .unwrap();
        for n in 1..=2 {
            tx.agents()
                .insert(&NewAgent {
                    id: agent(n),
                    name: AgentName::try_from(format!("agent-{n}").as_str()).unwrap(),
                    created_at: at(T0),
                })
                .await
                .unwrap();
        }
        tx.commit(setup_entry()).await.unwrap();
        let assignments = AssignmentService::new(Arc::new(clock.clone()), Arc::new(db.clone()));
        let agents = AgentService::new(
            Arc::new(clock.clone()),
            Arc::new(db.clone()),
            Arc::new(AgentRegistry::new(Arc::new(clock))),
        );
        Self {
            db,
            owner,
            assignments,
            agents,
        }
    }

    /// Adds offline account `n` and its bot `n`.
    async fn bot(&self, n: u8) -> (AccountId, BotId) {
        let account = AccountId::new_v7(at(T0 + i64::from(n)), [n; 10]).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.accounts()
            .insert(&McAccount {
                id: account,
                owner: self.owner,
                name: format!("AfkBot{n}").parse().unwrap(),
                created_at: at(T0),
                credential: AccountCredential::Offline,
            })
            .await
            .unwrap();
        tx.bots()
            .insert(&NewBot {
                id: bot(n),
                account,
                mode: ModePreset::Afk.id(),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        (account, bot(n))
    }

    async fn disable(&self, n: u8) {
        let id = agent(n).to_string();
        sqlx::query!("UPDATE agents SET enabled = 0 WHERE id = ?", id)
            .execute(self.db.write_pool())
            .await
            .unwrap();
    }

    /// Every entry with `action`, oldest first.
    async fn entries(&self, action: &AuditAction) -> Vec<AuditRecord> {
        let mut entries: Vec<AuditRecord> = self
            .db
            .audit()
            .list(AuditPage {
                before: None,
                limit: AuditLimit::new(100).unwrap(),
            })
            .await
            .unwrap()
            .into_iter()
            .filter(|entry| &entry.action == action)
            .collect();
        entries.reverse();
        entries
    }

    async fn agent_of(&self, id: BotId) -> Option<AgentId> {
        self.db.bots().find(id).await.unwrap().unwrap().agent
    }
}

fn meta(entry: &AuditRecord, key: &str) -> Option<RecordedValue> {
    entry
        .metadata
        .entries()
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
}

fn text(value: &str) -> RecordedValue {
    RecordedValue::Text(value.to_owned())
}

/// Asserts `entry` targets bot `id`, with no actor and `outcome`.
fn on_bot(entry: &AuditRecord, id: BotId, outcome: AuditOutcome) {
    assert_eq!(entry.outcome, outcome);
    assert_eq!(entry.actor, None);
    assert_eq!(entry.ip, None);
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Bot(id)))
    );
}

// --- The service --------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn an_assignment_commits_one_entry_and_a_repeat_writes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (_, id) = setup.bot(1).await;

    let first = setup.assignments.assign(id, agent(1)).await.unwrap();
    let again = setup.assignments.assign(id, agent(2)).await.unwrap();

    assert!(first);
    assert!(!again);
    assert_eq!(setup.agent_of(id).await, Some(agent(1)));
    let entries = setup.entries(&AuditAction::BOT_ASSIGN).await;
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    on_bot(entry, id, AuditOutcome::Success);
    assert_eq!(meta(entry, "agent"), Some(text(&agent(1).to_string())));
}

#[sqlx::test(migrations = false)]
async fn an_unassignment_records_its_reason_and_the_latest_report_and_keeps_the_agent(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (_, id) = setup.bot(1).await;
    setup.assignments.assign(id, agent(1)).await.unwrap();
    let paused = LastReport {
        state: LastState::Failed(FailReason::Auth),
        at: at(T0 + 500),
    };

    let decided = setup
        .assignments
        .unassign(
            id,
            agent(1),
            UnassignReason::Unqualified(Unplaced::Pending),
            Some(paused),
        )
        .await
        .unwrap();
    let elsewhere = setup
        .assignments
        .unassign(id, agent(2), UnassignReason::Capacity, None)
        .await
        .unwrap();

    assert!(decided);
    assert!(!elsewhere, "its row names another agent");
    let row = setup.db.bots().find(id).await.unwrap().unwrap();
    assert_eq!(row.agent, Some(agent(1)), "kept until the agent lets it go");
    assert_eq!(row.last, Some(paused));
    let entries = setup.entries(&AuditAction::BOT_UNASSIGN).await;
    assert_eq!(entries.len(), 1);
    on_bot(&entries[0], id, AuditOutcome::Success);
    assert_eq!(
        meta(&entries[0], "agent"),
        Some(text(&agent(1).to_string()))
    );
    assert_eq!(meta(&entries[0], "reason"), Some(text("pending")));
}

#[sqlx::test(migrations = false)]
async fn a_detach_stores_the_latest_report_then_clears_and_a_repeat_writes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (_, id) = setup.bot(1).await;
    setup.assignments.assign(id, agent(1)).await.unwrap();
    let online = LastReport {
        state: LastState::Online,
        at: at(T0 + 700),
    };

    let cleared = setup
        .assignments
        .detach(id, agent(1), DetachReason::Removed, Some(online))
        .await
        .unwrap();
    let again = setup
        .assignments
        .detach(id, agent(1), DetachReason::Removed, None)
        .await
        .unwrap();

    assert!(cleared);
    assert!(!again);
    let row = setup.db.bots().find(id).await.unwrap().unwrap();
    assert_eq!((row.agent, row.last), (None, Some(online)));
    let entries = setup.entries(&AuditAction::BOT_DETACH).await;
    assert_eq!(entries.len(), 1);
    on_bot(&entries[0], id, AuditOutcome::Success);
    assert_eq!(
        meta(&entries[0], "agent"),
        Some(text(&agent(1).to_string()))
    );
    assert_eq!(meta(&entries[0], "reason"), Some(text("removed")));
}

#[sqlx::test(migrations = false)]
async fn a_detach_closes_a_deleted_bots_record(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (account, id) = setup.bot(1).await;
    setup.assignments.assign(id, agent(1)).await.unwrap();
    let mut tx = setup.db.write().await.unwrap();
    tx.accounts().delete(account, at(T0)).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();

    let closed = setup
        .assignments
        .detach(id, agent(1), DetachReason::NotRunning, None)
        .await
        .unwrap();

    assert!(closed);
    assert_eq!(setup.db.bots().fleet().await.unwrap().releases, []);
    let entries = setup.entries(&AuditAction::BOT_DETACH).await;
    assert_eq!(meta(&entries[0], "reason"), Some(text("not_running")));
}

#[sqlx::test(migrations = false)]
async fn a_stray_is_recorded_once_as_a_failure_naming_both_agents(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (_, id) = setup.bot(1).await;
    setup.assignments.assign(id, agent(1)).await.unwrap();

    let first = setup
        .assignments
        .stray(id, agent(2), Some(agent(1)))
        .await
        .unwrap();
    let again = setup
        .assignments
        .stray(id, agent(2), Some(agent(1)))
        .await
        .unwrap();
    let unknown = setup
        .assignments
        .stray(bot(9), agent(2), None)
        .await
        .unwrap();

    assert!(first);
    assert!(!again);
    assert!(!unknown);
    let entries = setup.entries(&AuditAction::BOT_STRAY).await;
    assert_eq!(entries.len(), 1);
    on_bot(&entries[0], id, AuditOutcome::Failure);
    assert_eq!(
        meta(&entries[0], "agent"),
        Some(text(&agent(2).to_string()))
    );
    assert_eq!(
        meta(&entries[0], "assigned_to"),
        Some(text(&agent(1).to_string()))
    );
    assert_eq!(setup.db.bots().fleet().await.unwrap().releases.len(), 1);
}

#[sqlx::test(migrations = false)]
async fn the_fleet_is_the_stores_read(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = Setup::new(options).await;
    setup.bot(1).await;

    let fleet = setup.assignments.fleet().await.unwrap();

    assert_eq!(fleet, setup.db.bots().fleet().await.unwrap());
    assert_eq!(fleet.bots.len(), 1);
}

// --- Releasing --------------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn releasing_a_disabled_agent_clears_its_bots_and_records_one_entry_each(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (_, first) = setup.bot(1).await;
    let (gone_account, gone) = setup.bot(2).await;
    for id in [first, gone] {
        setup.assignments.assign(id, agent(1)).await.unwrap();
    }
    let mut tx = setup.db.write().await.unwrap();
    tx.accounts().delete(gone_account, at(T0)).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    setup.disable(1).await;

    let released = setup.agents.release(agent(1)).await.unwrap();
    let again = setup.agents.release(agent(1)).await.unwrap();

    assert_eq!(released, [first, gone]);
    assert_eq!(again, []);
    assert_eq!(setup.agent_of(first).await, None);
    let detaches = setup.entries(&AuditAction::BOT_DETACH).await;
    assert_eq!(detaches.len(), 2);
    for (entry, id) in detaches.iter().zip([first, gone]) {
        on_bot(entry, id, AuditOutcome::Success);
        assert_eq!(meta(entry, "reason"), Some(text("agent_released")));
        assert_eq!(meta(entry, "agent"), Some(text(&agent(1).to_string())));
    }
    let releases = setup.entries(&AuditAction::AGENT_RELEASE).await;
    assert_eq!(releases.len(), 1, "nothing to release is no entry");
    let entry = &releases[0];
    assert_eq!(entry.actor, None);
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Agent(agent(1))))
    );
    assert_eq!(meta(entry, "count"), Some(RecordedValue::Int(2)));
    assert_eq!(meta(entry, "via"), Some(text("cli")));
}

#[sqlx::test(migrations = false)]
async fn an_enabled_or_unknown_agent_is_never_released(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (_, id) = setup.bot(1).await;
    setup.assignments.assign(id, agent(1)).await.unwrap();

    let enabled = setup.agents.release(agent(1)).await;
    let unknown = setup.agents.release(agent(9)).await;

    assert!(
        matches!(enabled, Err(ReleaseAgentError::Enabled)),
        "{enabled:?}"
    );
    assert!(
        matches!(unknown, Err(ReleaseAgentError::NotFound)),
        "{unknown:?}"
    );
    assert_eq!(setup.agent_of(id).await, Some(agent(1)));
    assert_eq!(setup.entries(&AuditAction::AGENT_RELEASE).await, []);
}
