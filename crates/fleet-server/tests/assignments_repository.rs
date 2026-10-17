//! Tests for the bots' assignments and release records in the repository
//! (Plan.md P10.5; ADR-0019, S32, S36, S39, S40, S43, R30): a bot is
//! assigned only from no agent and only to an enabled one; its last state is
//! stored only for its own agent and never moves backwards; an agent's word
//! clears its assignment and its record; deleting an assigned bot's account
//! leaves a record that fences its Minecraft account; a hello's stray gets a
//! record from the bot's row or another agent's record; the host's release
//! clears every assignment and record of an agent; and the reconciler's read
//! of everything agrees with itself.
//!
//! Each test gets a fresh database from `#[sqlx::test]`. Time isn't paused:
//! sqlx's worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use fleet_core::account::{AccountKind, AccountStatus};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::authz::Role;
use fleet_core::bot::{BotAccount, FailReason, LastReport, LastState};
use fleet_core::id::{AccountId, AgentId, BotId, UserId};
use fleet_core::mode::ModePreset;
use fleet_core::placement::McIdentity;
use fleet_core::value::{AgentName, McUuid, Username};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::accounts::{AccountCredential, DeletedAccount, McAccount};
use fleet_server::ports::agents::NewAgent;
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::bots::{NewBot, ReleaseRecord, UnreadableBot};
use fleet_server::ports::page::{Page, PageLimit};
use fleet_server::ports::store::Store;
use fleet_server::ports::users::{NewUser, PasswordHash};
use secrecy::SecretString;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// The time everything starts at, in milliseconds.
const T0: i64 = 1_800_000_000_000;
/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// A Minecraft profile's UUID (a well-known example, not a real account's).
const PROFILE: &str = "069a79f4-44e9-4726-a5be-fca90e38aaf5";

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

async fn database(options: SqliteConnectOptions) -> Database {
    Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap()
}

/// The entry a test commits its setup with. A test-only name, never listed
/// in `audit_actions!`.
fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from("test.assignments_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// The setup: one user, agents 1 to 3 and the bots the test adds.
struct Setup {
    db: Database,
    owner: UserId,
}

impl Setup {
    async fn new(options: SqliteConnectOptions) -> Self {
        let db = database(options).await;
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
        for n in 1..=3 {
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
        Self { db, owner }
    }

    /// Adds account `n` (offline `AfkBot<n>`, or Microsoft `PROFILE` when
    /// `microsoft`) and its bot `n`.
    async fn bot(&self, n: u8, microsoft: bool) -> (AccountId, BotId) {
        let account = AccountId::new_v7(at(T0 + i64::from(n)), [n; 10]).unwrap();
        let credential = if microsoft {
            AccountCredential::Microsoft {
                uuid: PROFILE.parse().unwrap(),
                refresh_envelope: vec![n; 61],
                status: AccountStatus::Active,
            }
        } else {
            AccountCredential::Offline
        };
        let mut tx = self.db.write().await.unwrap();
        tx.accounts()
            .insert(&McAccount {
                id: account,
                owner: self.owner,
                name: format!("AfkBot{n}").parse().unwrap(),
                created_at: at(T0),
                credential,
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

    /// Runs `write` in one transaction and commits it.
    async fn write<T>(
        &self,
        write: impl AsyncFnOnce(&mut dyn fleet_server::ports::store::WriteTx) -> T,
    ) -> T {
        let mut tx = self.db.write().await.unwrap();
        let result = write(tx.as_mut()).await;
        tx.commit(setup_entry()).await.unwrap();
        result
    }

    async fn agent_of(&self, bot: BotId) -> Option<AgentId> {
        self.db.bots().find(bot).await.unwrap().unwrap().agent
    }

    async fn disable(&self, n: u8) {
        let id = agent(n).to_string();
        sqlx::query!("UPDATE agents SET enabled = 0 WHERE id = ?", id)
            .execute(self.db.write_pool())
            .await
            .unwrap();
    }
}

fn agent(n: u8) -> AgentId {
    AgentId::new_v7(at(T0), [n; 10]).unwrap()
}

fn bot(n: u8) -> BotId {
    BotId::new_v7(at(T0 + i64::from(n)), [n; 10]).unwrap()
}

fn report(state: LastState, millis: i64) -> LastReport {
    LastReport {
        state,
        at: at(millis),
    }
}

// --- Assigning ----------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_bot_is_assigned_only_from_no_agent_and_only_to_an_enabled_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (_, first) = setup.bot(1, false).await;
    let (_, second) = setup.bot(2, false).await;
    setup.disable(3).await;

    let assigned = setup
        .write(async |tx| tx.bots().assign(first, agent(1)).await.unwrap())
        .await;
    let again = setup
        .write(async |tx| tx.bots().assign(first, agent(2)).await.unwrap())
        .await;
    let to_disabled = setup
        .write(async |tx| tx.bots().assign(second, agent(3)).await.unwrap())
        .await;
    let unknown = setup
        .write(async |tx| tx.bots().assign(bot(9), agent(1)).await.unwrap())
        .await;

    assert!(assigned);
    assert!(!again, "a bot with an agent keeps it");
    assert!(!to_disabled, "a disabled agent gets nothing");
    assert!(!unknown);
    assert_eq!(setup.agent_of(first).await, Some(agent(1)));
    assert_eq!(setup.agent_of(second).await, None);
}

// --- The last state ------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn the_last_state_is_stored_only_for_its_agent_and_never_backwards(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (_, id) = setup.bot(1, false).await;
    setup
        .write(async |tx| tx.bots().assign(id, agent(1)).await.unwrap())
        .await;
    let paused = report(LastState::Failed(FailReason::Auth), T0 + 2_000);

    let stored = setup
        .write(async |tx| tx.bots().record_state(id, agent(1), &paused).await.unwrap())
        .await;
    let older = setup
        .write(async |tx| {
            tx.bots()
                .record_state(id, agent(1), &report(LastState::Online, T0 + 1_000))
                .await
                .unwrap()
        })
        .await;
    let other_agent = setup
        .write(async |tx| {
            tx.bots()
                .record_state(id, agent(2), &report(LastState::Online, T0 + 3_000))
                .await
                .unwrap()
        })
        .await;

    assert!(stored);
    assert!(!older, "an older report never replaces a newer one");
    assert!(
        !other_agent,
        "only the bot's own agent's reports are stored"
    );
    assert_eq!(
        setup.db.bots().find(id).await.unwrap().unwrap().last,
        Some(paused)
    );
}

#[sqlx::test(migrations = false)]
async fn an_unassigned_bots_state_is_never_stored(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (_, id) = setup.bot(1, false).await;

    let stored = setup
        .write(async |tx| {
            tx.bots()
                .record_state(id, agent(1), &report(LastState::Online, T0))
                .await
                .unwrap()
        })
        .await;

    assert!(!stored);
    assert_eq!(setup.db.bots().find(id).await.unwrap().unwrap().last, None);
}

// --- Detaching -----------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn detaching_clears_only_that_agents_assignment(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (_, id) = setup.bot(1, false).await;
    setup
        .write(async |tx| tx.bots().assign(id, agent(1)).await.unwrap())
        .await;

    let other = setup
        .write(async |tx| tx.bots().detach(id, agent(2)).await.unwrap())
        .await;
    let own = setup
        .write(async |tx| tx.bots().detach(id, agent(1)).await.unwrap())
        .await;
    let again = setup
        .write(async |tx| tx.bots().detach(id, agent(1)).await.unwrap())
        .await;

    assert!(!other);
    assert!(own);
    assert!(!again);
    assert_eq!(setup.agent_of(id).await, None);
}

// --- Release records -----------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn deleting_an_assigned_bots_account_leaves_a_record_of_its_minecraft_account(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (microsoft, ms_bot) = setup.bot(1, true).await;
    let (offline, off_bot) = setup.bot(2, false).await;
    let (unassigned, _) = setup.bot(3, false).await;
    setup
        .write(async |tx| {
            tx.bots().assign(ms_bot, agent(1)).await.unwrap();
            tx.bots().assign(off_bot, agent(2)).await.unwrap();
        })
        .await;

    let deleted = setup
        .write(async |tx| {
            [
                tx.accounts().delete(microsoft, at(T0 + 9)).await.unwrap(),
                tx.accounts().delete(offline, at(T0 + 9)).await.unwrap(),
                tx.accounts().delete(unassigned, at(T0 + 9)).await.unwrap(),
            ]
        })
        .await;

    assert_eq!(
        deleted,
        [
            Some(DeletedAccount {
                bot: Some(ms_bot),
                agent: Some(agent(1))
            }),
            Some(DeletedAccount {
                bot: Some(off_bot),
                agent: Some(agent(2))
            }),
            Some(DeletedAccount {
                bot: Some(bot(3)),
                agent: None
            }),
        ]
    );
    let fleet = setup.db.bots().fleet().await.unwrap();
    assert_eq!(
        fleet.releases,
        [
            ReleaseRecord {
                bot: ms_bot,
                agent: agent(1),
                identity: McIdentity::Microsoft(PROFILE.parse::<McUuid>().unwrap()),
            },
            ReleaseRecord {
                bot: off_bot,
                agent: agent(2),
                identity: McIdentity::offline(&"afkbot2".parse().unwrap()),
            },
        ]
    );
    assert_eq!(fleet.bots, []);
}

#[sqlx::test(migrations = false)]
async fn detaching_closes_that_agents_record(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (account, id) = setup.bot(1, false).await;
    setup
        .write(async |tx| {
            tx.bots().assign(id, agent(1)).await.unwrap();
            tx.accounts().delete(account, at(T0)).await.unwrap();
        })
        .await;

    let other = setup
        .write(async |tx| tx.bots().detach(id, agent(2)).await.unwrap())
        .await;
    let own = setup
        .write(async |tx| tx.bots().detach(id, agent(1)).await.unwrap())
        .await;

    assert!(!other);
    assert!(own);
    assert_eq!(setup.db.bots().fleet().await.unwrap().releases, []);
}

#[sqlx::test(migrations = false)]
async fn a_stray_gets_a_record_from_its_row_or_another_agents_record_and_only_once(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (_, listed) = setup.bot(1, false).await;
    let (gone_account, gone) = setup.bot(2, true).await;
    setup
        .write(async |tx| {
            tx.bots().assign(gone, agent(1)).await.unwrap();
            tx.accounts().delete(gone_account, at(T0)).await.unwrap();
        })
        .await;

    let from_row = setup
        .write(async |tx| tx.bots().stray(listed, agent(2), at(T0)).await.unwrap())
        .await;
    let twice = setup
        .write(async |tx| tx.bots().stray(listed, agent(2), at(T0)).await.unwrap())
        .await;
    let from_record = setup
        .write(async |tx| tx.bots().stray(gone, agent(3), at(T0)).await.unwrap())
        .await;
    let unknown = setup
        .write(async |tx| tx.bots().stray(bot(9), agent(2), at(T0)).await.unwrap())
        .await;

    assert!(from_row);
    assert!(!twice);
    assert!(from_record);
    assert!(!unknown, "nothing says which account an unknown bot plays");
    let records: BTreeSet<(BotId, AgentId)> = setup
        .db
        .bots()
        .fleet()
        .await
        .unwrap()
        .releases
        .into_iter()
        .map(|record| (record.bot, record.agent))
        .collect();
    assert_eq!(
        records,
        BTreeSet::from([(listed, agent(2)), (gone, agent(1)), (gone, agent(3))])
    );
}

#[sqlx::test(migrations = false)]
async fn releasing_an_agent_clears_its_assignments_and_records_and_nothing_else(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (_, first) = setup.bot(1, false).await;
    let (_, second) = setup.bot(2, false).await;
    let (gone_account, gone) = setup.bot(3, false).await;
    let (_, others) = setup.bot(4, false).await;
    setup
        .write(async |tx| {
            for id in [first, second, gone] {
                tx.bots().assign(id, agent(1)).await.unwrap();
            }
            tx.bots().assign(others, agent(2)).await.unwrap();
            tx.accounts().delete(gone_account, at(T0)).await.unwrap();
        })
        .await;

    let released = setup
        .write(async |tx| tx.bots().release_all(agent(1)).await.unwrap())
        .await;
    let again = setup
        .write(async |tx| tx.bots().release_all(agent(1)).await.unwrap())
        .await;

    assert_eq!(released, [first, second, gone]);
    assert_eq!(again, []);
    assert_eq!(setup.agent_of(first).await, None);
    assert_eq!(setup.agent_of(others).await, Some(agent(2)));
    assert_eq!(setup.db.bots().fleet().await.unwrap().releases, []);
}

// --- Reading ---------------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_record_reads_the_bots_account_owner_and_mode_and_the_fleet_its_agents(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (microsoft, ms_bot) = setup.bot(1, true).await;
    let (_, off_bot) = setup.bot(2, false).await;
    setup.disable(3).await;

    let ms = setup.db.bots().record(ms_bot).await.unwrap().unwrap();
    let off = setup.db.bots().record(off_bot).await.unwrap().unwrap();
    let fleet = setup.db.bots().fleet().await.unwrap();

    assert_eq!(ms.account, BotAccount::Online(microsoft));
    assert_eq!(
        ms.identity,
        McIdentity::Microsoft(PROFILE.parse::<McUuid>().unwrap())
    );
    assert_eq!(
        (ms.kind, ms.status, ms.owner_enabled),
        (AccountKind::Microsoft, AccountStatus::Active, true)
    );
    assert_eq!(ms.mode, ModePreset::Afk.definition());
    assert_eq!(off.account, BotAccount::Offline("AfkBot2".parse().unwrap()));
    assert_eq!(
        off.identity,
        McIdentity::offline(&"AFKBOT2".parse().unwrap())
    );
    assert_eq!(off.kind, AccountKind::Offline);
    assert_eq!(setup.db.bots().record(bot(9)).await.unwrap(), None);
    assert_eq!(fleet.bots, [ms, off]);
    assert_eq!(fleet.enabled_agents, BTreeSet::from([agent(1), agent(2)]));
}

#[sqlx::test(migrations = false)]
async fn a_disabled_owner_reads_as_such(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = Setup::new(options).await;
    let (_, id) = setup.bot(1, false).await;
    let owner = setup.owner.to_string();
    sqlx::query!("UPDATE users SET disabled = 1 WHERE id = ?", owner)
        .execute(setup.db.write_pool())
        .await
        .unwrap();

    let record = setup.db.bots().record(id).await.unwrap().unwrap();

    assert!(!record.owner_enabled);
}

#[sqlx::test(migrations = false)]
async fn the_records_page_newest_first(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = Setup::new(options).await;
    for n in 1..=3 {
        setup.bot(n, false).await;
    }

    let first = setup
        .db
        .bots()
        .records(Page {
            before: None,
            limit: PageLimit::new(2).unwrap(),
        })
        .await
        .unwrap();
    let rest = setup
        .db
        .bots()
        .records(Page {
            before: Some(bot(2)),
            limit: PageLimit::new(2).unwrap(),
        })
        .await
        .unwrap();

    let ids = |records: &[fleet_server::ports::bots::BotRecord]| {
        records
            .iter()
            .map(|record| record.bot.id)
            .collect::<Vec<_>>()
    };
    // One past the limit when another page follows.
    assert_eq!(ids(&first), [bot(3), bot(2), bot(1)]);
    assert_eq!(ids(&rest), [bot(1)]);
}

#[sqlx::test(migrations = false)]
async fn a_bot_whose_mode_isnt_valid_reads_as_unreadable(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (_, bot) = setup.bot(1, false).await;
    let afk = ModePreset::Afk.id().to_string();
    sqlx::query!(
        r#"UPDATE modes SET definition_json = '{"steps":[{"action":{"type":"nope"}}]}' WHERE id = ?"#,
        afk
    )
    .execute(setup.db.write_pool())
    .await
    .unwrap();

    let fleet = setup.db.bots().fleet().await.unwrap();

    assert_eq!(fleet.bots, []);
    let unreadable = fleet.unreadable.first().unwrap();
    assert_eq!((unreadable.bot, unreadable.table), (bot, "modes"));
}

#[sqlx::test(migrations = false)]
async fn a_bot_whose_row_doesnt_read_back_is_unreadable_and_the_others_read(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (_, broken) = setup.bot(1, false).await;
    let (_, fine) = setup.bot(2, false).await;
    setup
        .write(async |tx| tx.bots().assign(broken, agent(1)).await.unwrap())
        .await;
    let id = broken.to_string();
    sqlx::query!(
        "UPDATE bots SET server_address = 'not a server address' WHERE id = ?",
        id
    )
    .execute(setup.db.write_pool())
    .await
    .unwrap();

    let fleet = setup.db.bots().fleet().await.unwrap();

    assert_eq!(
        fleet
            .bots
            .iter()
            .map(|record| record.bot.id)
            .collect::<Vec<_>>(),
        [fine]
    );
    assert_eq!(
        fleet.unreadable,
        [UnreadableBot {
            bot: broken,
            agent: Some(agent(1)),
            identity: Some(McIdentity::offline(&"AfkBot1".parse().unwrap())),
            table: "bots",
            column: "server_address",
            rowid: fleet.unreadable[0].rowid,
        }]
    );
}

#[sqlx::test(migrations = false)]
async fn a_release_record_that_doesnt_read_back_is_unreadable(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (account, bot) = setup.bot(1, false).await;
    setup
        .write(async |tx| tx.bots().assign(bot, agent(2)).await.unwrap())
        .await;
    setup
        .write(async |tx| tx.accounts().delete(account, at(T0)).await.unwrap())
        .await;
    let id = bot.to_string();
    sqlx::query!(
        "UPDATE releasing_bots SET mc_name = '' WHERE bot_id = ?",
        id
    )
    .execute(setup.db.write_pool())
    .await
    .unwrap();

    let fleet = setup.db.bots().fleet().await.unwrap();

    assert_eq!(fleet.releases, []);
    let unreadable = fleet.unreadable.first().unwrap();
    assert_eq!(
        (unreadable.bot, unreadable.agent, &unreadable.identity),
        (bot, Some(agent(2)), &None)
    );
    assert_eq!(unreadable.table, "releasing_bots");
}

#[sqlx::test(migrations = false)]
async fn the_agents_list_counts_their_bots_and_open_records(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let (_, first) = setup.bot(1, false).await;
    let (_, second) = setup.bot(2, false).await;
    let (gone_account, gone) = setup.bot(3, false).await;
    let (_, other) = setup.bot(4, false).await;
    setup
        .write(async |tx| {
            for id in [first, second, gone] {
                tx.bots().assign(id, agent(1)).await.unwrap();
            }
            tx.bots().assign(other, agent(2)).await.unwrap();
            tx.accounts().delete(gone_account, at(T0)).await.unwrap();
        })
        .await;

    let listed = setup
        .db
        .agents()
        .list(Page {
            before: None,
            limit: PageLimit::new(10).unwrap(),
        })
        .await
        .unwrap();
    let counts: Vec<(AgentId, u32)> = listed
        .iter()
        .map(|listed| (listed.agent.id, listed.bots))
        .collect();

    // Two bots and the deleted one's record; one bot; none.
    assert_eq!(counts, [(agent(3), 0), (agent(2), 1), (agent(1), 3)]);
    let one = setup.db.agents().listed(agent(1)).await.unwrap().unwrap();
    assert_eq!(one.bots, 3);
}
