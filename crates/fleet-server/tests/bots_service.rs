//! Tests for the bots' reads (Plan.md P10.5; ADR-0019, S41): each bot comes
//! with what its state reads as now, from its row, the agent its row names
//! and that agent's live stream. A bot without an agent reads Unplaced, with
//! why and its sticky state; a bot whose agent has no live stream reads
//! Unknown, with the agent's status, when it was last heard from and the
//! bot's last known state; a bot on a live agent reads its report on that
//! stream, or that none has come yet.
//!
//! Each test gets a fresh database from `#[sqlx::test]`. Time isn't paused:
//! sqlx's worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeDelta, Utc};
use fleet_core::agent::AgentStatus;
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::authz::Role;
use fleet_core::bot::{
    BotSnapshot, BotState, LastReport, LastState, PauseReason, ShownState, StickyState, UnplacedWhy,
};
use fleet_core::disconnect::ConflictKind;
use fleet_core::id::{AccountId, AgentId, BotId, UserId};
use fleet_core::mode::ModePreset;
use fleet_core::pki::CertFingerprint;
use fleet_core::placement::Unplaced;
use fleet_core::value::{AgentName, Username};
use fleet_server::app::bots::BotService;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::agents::{NewAgent, NewAgentCert};
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::bots::NewBot;
use fleet_server::ports::live_agents::{HeldBot, LiveAgents, LiveStream, PermissionEnd};
use fleet_server::ports::page::{Page, PageLimit};
use fleet_server::ports::store::{Store as _, StoreError};
use fleet_server::ports::users::{NewUser, PasswordHash};
use fleet_testkit::system::ManualClock;
use secrecy::SecretString;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

const T0: &str = "2027-01-15T08:00:00Z";
/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";

fn at(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

fn agent(n: u8) -> AgentId {
    AgentId::new_v7(at(T0), [n; 10]).unwrap()
}

/// Bot `n`'s ID: later bots are newer.
fn bot(n: u8) -> BotId {
    BotId::new_v7(at(T0) + TimeDelta::seconds(i64::from(n)), [n; 10]).unwrap()
}

/// The entry a test commits its setup with. A test-only name, never listed
/// in `audit_actions!`.
fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from("test.bots_setup").unwrap(),
        AuditOutcome::Success,
    )
}

fn paused() -> LastState {
    LastState::Paused(PauseReason::Conflict {
        kind: ConflictKind::DuplicateLogin,
    })
}

/// The live streams the test sets up.
#[derive(Debug, Default)]
struct Live {
    streams: Mutex<HashMap<AgentId, LiveStream>>,
    held: Mutex<HashMap<(AgentId, BotId), HeldBot>>,
}

impl LiveAgents for Live {
    fn end(&self, _agent: AgentId, _reason: PermissionEnd) -> bool {
        false
    }

    fn live(&self, agent: AgentId) -> Option<LiveStream> {
        self.streams.lock().unwrap().get(&agent).copied()
    }

    fn held(&self, agent: AgentId, bot: BotId) -> Option<HeldBot> {
        self.held.lock().unwrap().get(&(agent, bot)).cloned()
    }
}

struct Setup {
    db: Database,
    live: Arc<Live>,
    clock: ManualClock,
}

impl Setup {
    /// One user, and agents 1 and 2 with a certificate valid for 60 days,
    /// heard from last at T0.
    async fn new(options: SqliteConnectOptions) -> Self {
        let db = Database::connect(options, DatabaseOptions::default())
            .await
            .unwrap();
        let mut tx = db.write().await.unwrap();
        tx.users()
            .insert(&NewUser {
                id: owner(),
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
            tx.agents()
                .insert_cert(&NewAgentCert {
                    fingerprint: CertFingerprint::from_digest([n; 32]),
                    agent: agent(n),
                    not_after: at(T0) + TimeDelta::days(60),
                    created_at: at(T0),
                })
                .await
                .unwrap();
        }
        tx.commit(setup_entry()).await.unwrap();
        Self {
            db,
            live: Arc::new(Live::default()),
            clock: ManualClock::new(at(T0) + TimeDelta::hours(1)),
        }
    }

    fn service(&self, allow_offline: bool) -> BotService {
        BotService::new(
            Arc::new(self.clock.clone()),
            Arc::new(self.db.clone()),
            Arc::clone(&self.live) as Arc<dyn LiveAgents>,
            allow_offline,
        )
    }

    /// Adds the offline account `AfkBot<n>` and its bot `n`: no server,
    /// stopped, unassigned.
    async fn bot(&self, n: u8) -> BotId {
        let account = AccountId::new_v7(at(T0), [n; 10]).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.accounts()
            .insert(&McAccount {
                id: account,
                owner: owner(),
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
        bot(n)
    }

    /// Gives `bot` a server and sets it running, as E's endpoints will.
    async fn run(&self, bot: BotId) {
        let id = bot.as_uuid().hyphenated().to_string();
        sqlx::query!(
            "UPDATE bots SET server_address = 'play.example.com', desired_state = 'running' WHERE id = ?",
            id
        )
        .execute(self.db.write_pool())
        .await
        .unwrap();
    }

    /// Assigns `bot` to `agent`, with `last` as its stored last state.
    async fn assign(&self, bot: BotId, agent: AgentId, last: Option<LastReport>) {
        let mut tx = self.db.write().await.unwrap();
        assert!(tx.bots().assign(bot, agent).await.unwrap());
        if let Some(last) = last {
            assert!(tx.bots().record_state(bot, agent, &last).await.unwrap());
        }
        tx.commit(setup_entry()).await.unwrap();
    }

    /// Stores `last` for `bot`, which ends up unassigned.
    async fn rested(&self, bot: BotId, last: LastReport) {
        self.assign(bot, agent(1), Some(last)).await;
        let mut tx = self.db.write().await.unwrap();
        assert!(tx.bots().detach(bot, agent(1)).await.unwrap());
        tx.commit(setup_entry()).await.unwrap();
    }

    async fn disable(&self, agent: AgentId) {
        let id = agent.as_uuid().hyphenated().to_string();
        sqlx::query!("UPDATE agents SET enabled = 0 WHERE id = ?", id)
            .execute(self.db.write_pool())
            .await
            .unwrap();
    }

    async fn shown(&self, bot: BotId) -> ShownState {
        self.service(true).view(bot).await.unwrap().unwrap().shown
    }
}

fn owner() -> UserId {
    UserId::new_v7(at(T0), [1; 10]).unwrap()
}

fn last(state: LastState, minutes: i64) -> LastReport {
    LastReport {
        state,
        at: at(T0) + TimeDelta::minutes(minutes),
    }
}

// --- Without an agent ---------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_bot_without_an_agent_reads_unplaced_with_why(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;

    let shown = setup.shown(bot).await;

    assert_eq!(
        shown,
        ShownState::Unplaced {
            why: UnplacedWhy::Unqualified(Unplaced::NoServer),
            sticky: None,
        }
    );
}

#[sqlx::test(migrations = false)]
async fn a_qualifying_bot_without_an_agent_awaits_one_in_its_sticky_state(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.run(bot).await;
    setup.rested(bot, last(paused(), 5)).await;

    let shown = setup.shown(bot).await;

    assert_eq!(
        shown,
        ShownState::Unplaced {
            why: UnplacedWhy::AwaitingAgent,
            sticky: Some(StickyState::Paused(PauseReason::Conflict {
                kind: ConflictKind::DuplicateLogin
            })),
        }
    );
}

#[sqlx::test(migrations = false)]
async fn an_offline_bot_reads_unplaced_while_offline_accounts_are_off(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.run(bot).await;

    let view = setup.service(false).view(bot).await.unwrap().unwrap();

    assert_eq!(
        view.shown,
        ShownState::Unplaced {
            why: UnplacedWhy::Unqualified(Unplaced::OfflineNotAllowed),
            sticky: None,
        }
    );
}

// --- On an agent ------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_bot_on_an_agent_without_a_live_stream_reads_unknown_with_its_last_state(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.run(bot).await;
    setup
        .assign(bot, agent(1), Some(last(LastState::Online, 7)))
        .await;

    let shown = setup.shown(bot).await;

    assert_eq!(
        shown,
        ShownState::Unknown {
            agent: AgentStatus::Offline,
            agent_last_seen: None,
            last_known: Some(last(LastState::Online, 7)),
        }
    );
}

#[sqlx::test(migrations = false)]
async fn a_bot_on_a_disabled_agent_reads_unknown_even_while_its_stream_lives(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.assign(bot, agent(1), None).await;
    setup.disable(agent(1)).await;
    let seen = at(T0) + TimeDelta::minutes(30);
    setup.live.streams.lock().unwrap().insert(
        agent(1),
        LiveStream {
            connected_at: at(T0),
            last_seen: seen,
        },
    );

    let shown = setup.shown(bot).await;

    assert_eq!(
        shown,
        ShownState::Unknown {
            agent: AgentStatus::Disabled,
            agent_last_seen: Some(seen),
            last_known: None,
        }
    );
}

#[sqlx::test(migrations = false)]
async fn a_bot_on_a_live_agent_reads_its_report_on_that_stream(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.run(bot).await;
    setup.assign(bot, agent(1), None).await;
    let report = BotSnapshot {
        bot_id: bot,
        state: BotState::Stopped,
        since: at(T0),
        last_disconnect: None,
    };
    setup.live.streams.lock().unwrap().insert(
        agent(1),
        LiveStream {
            connected_at: at(T0),
            last_seen: at(T0),
        },
    );
    setup.live.held.lock().unwrap().insert(
        (agent(1), bot),
        HeldBot {
            since: at(T0),
            report: Some(report.clone()),
        },
    );

    let shown = setup.shown(bot).await;

    assert_eq!(shown, ShownState::Reported(report));
}

#[sqlx::test(migrations = false)]
async fn a_bot_on_a_live_agent_awaits_its_report_since_it_was_given(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let given = setup.bot(1).await;
    let welcomed = setup.bot(2).await;
    setup.assign(given, agent(1), None).await;
    setup.assign(welcomed, agent(1), None).await;
    let connected = at(T0) + TimeDelta::minutes(1);
    let held = at(T0) + TimeDelta::minutes(2);
    setup.live.streams.lock().unwrap().insert(
        agent(1),
        LiveStream {
            connected_at: connected,
            last_seen: connected,
        },
    );
    setup.live.held.lock().unwrap().insert(
        (agent(1), given),
        HeldBot {
            since: held,
            report: None,
        },
    );

    let given = setup.shown(given).await;
    let welcomed = setup.shown(welcomed).await;

    assert_eq!(given, ShownState::AwaitingReport { since: held });
    assert_eq!(
        welcomed,
        ShownState::AwaitingReport { since: connected },
        "not held yet: since its stream was welcomed"
    );
}

// --- Reading ----------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_bot_that_doesnt_exist_reads_as_none(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;

    let view = setup.service(true).view(bot(9)).await.unwrap();

    assert_eq!(view, None);
}

#[sqlx::test(migrations = false)]
async fn the_list_pages_the_bots_newest_first_each_with_its_state(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    for n in 1..=3 {
        setup.bot(n).await;
    }
    setup.assign(bot(2), agent(2), None).await;
    let service = setup.service(true);

    let first = service
        .list(Page {
            before: None,
            limit: PageLimit::new(2).unwrap(),
        })
        .await
        .unwrap();
    let second = service
        .list(Page {
            before: first.next,
            limit: PageLimit::new(2).unwrap(),
        })
        .await
        .unwrap();

    let ids = |items: &[fleet_server::app::bots::BotView]| {
        items
            .iter()
            .map(|view| view.record.bot.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&first.items), [bot(3), bot(2)]);
    assert_eq!(first.next, Some(bot(2)));
    assert_eq!(ids(&second.items), [bot(1)]);
    assert_eq!(second.next, None);
    assert!(matches!(
        first.items[1].shown,
        ShownState::Unknown {
            agent: AgentStatus::Offline,
            ..
        }
    ));
}

#[sqlx::test(migrations = false)]
async fn a_read_that_fails_is_an_error(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    let service = setup.service(true);
    setup.db.close().await;

    let view = service.view(bot).await;
    let list = service
        .list(Page {
            before: None,
            limit: PageLimit::new(2).unwrap(),
        })
        .await;

    assert!(
        matches!(view, Err(StoreError::Backend(_) | StoreError::Busy)),
        "{view:?}"
    );
    assert!(
        matches!(list, Err(StoreError::Backend(_) | StoreError::Busy)),
        "{list:?}"
    );
}
