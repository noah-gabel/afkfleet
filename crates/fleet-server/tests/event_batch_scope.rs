//! The scope of `UnauditedWrite::EventBatch` (Plan.md P10.8; ADR-0019, U17,
//! the user's request at the plan review): one batch holding chat and a
//! sticky crossing, over a database with rows in every table, changes
//! nothing but new `chat_messages` rows and the held bot's
//! `bots.last_state`, `last_state_reason` and `last_state_at`. No row of any
//! table is deleted, and no other row or column is touched.
//!
//! Every table is dumped whole with `SELECT *` by the `tables` module, which
//! the retention's scope test shares, so a column added later is compared
//! too; [`every_table_is_dumped`] fails when a table is added without being
//! dumped there. The database is a fresh one from `#[sqlx::test]`, on real
//! time (sqlx's worker threads, CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

#[path = "tables/mod.rs"]
mod tables;

use core::num::NonZeroUsize;
use core::time::Duration;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::authz::Role;
use fleet_core::bot::{LastReport, LastState, PauseReason};
use fleet_core::chat::{ChatMessage, IncomingChat, PlayerChatKind};
use fleet_core::disconnect::ConflictKind;
use fleet_core::id::{AccountId, AgentId, BotId, UserId};
use fleet_core::mode::ModePreset;
use fleet_core::value::{AgentName, Username};
use fleet_server::app::agents::ingest::{IngestBounds, IngestTimings, Tick, ingest};
use fleet_server::app::events::EventBus;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::agents::NewAgent;
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::bots::NewBot;
use fleet_server::ports::ingest::{Ingest, Report, Reported, StickyMarks, StickyWrite};
use fleet_server::ports::store::Store;
use fleet_server::ports::users::{NewUser, PasswordHash};
use secrecy::SecretString;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tables::{Ids, TABLES, dump, fill};

/// The time everything starts at, in milliseconds.
const T0: i64 = 1_800_000_000_000;
/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from("test.scope_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// The sticky crossings a test hands the writer.
#[derive(Debug, Default)]
struct Marks(Mutex<Vec<StickyWrite>>);

impl StickyMarks for Marks {
    fn any(&self) -> bool {
        !self.0.lock().unwrap().is_empty()
    }

    fn take(&self) -> Vec<StickyWrite> {
        core::mem::take(&mut *self.0.lock().unwrap())
    }

    fn remark(&self, bots: &[BotId]) {
        assert!(bots.is_empty(), "the batch failed: {bots:?}");
    }
}

/// A database with rows in every table: two users, agents 1 and 2, the
/// held bot 1 on agent 1 and bot 2 on agent 2, and one row of each other
/// table.
struct Filled {
    db: Database,
    agent: AgentId,
    held: BotId,
    other: BotId,
}

async fn user(db: &Database, n: u8) -> UserId {
    let id = UserId::new_v7(at(T0), [n; 10]).unwrap();
    let mut tx = db.write().await.unwrap();
    tx.users()
        .insert(&NewUser {
            id,
            username: Username::try_from(format!("user{n}").as_str()).unwrap(),
            password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
            role: Role::Member,
            created_at: at(T0),
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    id
}

async fn agent(db: &Database, n: u8) -> AgentId {
    let id = AgentId::new_v7(at(T0), [n; 10]).unwrap();
    let mut tx = db.write().await.unwrap();
    tx.agents()
        .insert(&NewAgent {
            id,
            name: AgentName::try_from(format!("agent-{n}").as_str()).unwrap(),
            created_at: at(T0),
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    id
}

async fn bot(db: &Database, owner: UserId, n: u8, agent: AgentId) -> (AccountId, BotId) {
    let account = AccountId::new_v7(at(T0 + i64::from(n)), [n; 10]).unwrap();
    let bot = BotId::new_v7(at(T0), [n; 10]).unwrap();
    let mut tx = db.write().await.unwrap();
    tx.accounts()
        .insert(&McAccount {
            id: account,
            owner,
            name: format!("AfkBot{n}").parse().unwrap(),
            created_at: at(T0),
            credential: AccountCredential::Offline,
        })
        .await
        .unwrap();
    tx.bots()
        .insert(&NewBot {
            id: bot,
            account,
            mode: ModePreset::Afk.id(),
        })
        .await
        .unwrap();
    assert!(tx.bots().assign(bot, agent).await.unwrap());
    tx.commit(setup_entry()).await.unwrap();
    (account, bot)
}

impl Filled {
    async fn new(options: SqliteConnectOptions) -> Self {
        let db = Database::connect(options, DatabaseOptions::default())
            .await
            .unwrap();
        let owner = user(&db, 1).await;
        let friend = user(&db, 2).await;
        let one = agent(&db, 1).await;
        let two = agent(&db, 2).await;
        let (account, held) = bot(&db, owner, 1, one).await;
        let (_, other) = bot(&db, owner, 2, two).await;
        // Another agent's bot, paused earlier: a batch that wrote its state
        // too would change its time alone.
        let mut tx = db.write().await.unwrap();
        assert!(
            tx.bots()
                .record_state(
                    other,
                    two,
                    &LastReport {
                        state: LastState::Paused(PauseReason::Conflict {
                            kind: ConflictKind::DuplicateLogin,
                        }),
                        at: at(T0),
                    },
                )
                .await
                .unwrap()
        );
        tx.commit(setup_entry()).await.unwrap();
        let filled = Self {
            db,
            agent: one,
            held,
            other,
        };
        filled.fill(owner, friend, account, one, two).await;
        filled
    }

    /// One row of each table no repository above wrote, through fixed SQL
    /// with bound IDs.
    async fn fill(
        &self,
        owner: UserId,
        friend: UserId,
        account: AccountId,
        one: AgentId,
        two: AgentId,
    ) {
        let ids = Ids {
            owner: owner.to_string(),
            friend: friend.to_string(),
            account: account.to_string(),
            one: one.to_string(),
            two: two.to_string(),
            held: self.held.to_string(),
        };
        fill(self.db.write_pool(), &ids).await;
    }
}

/// A bot's ID and its three `last_state*` columns, as stored.
type StoredState = (String, Option<String>, Option<String>, Option<String>);

/// Every row of `bots` with its last state left out, as `Debug` text, and
/// each bot's `last_state`, `last_state_reason` and `last_state_at`.
async fn bots_without_state(pool: &SqlitePool) -> (Vec<String>, Vec<StoredState>) {
    let rows = sqlx::query!("SELECT * FROM bots ORDER BY rowid")
        .fetch_all(pool)
        .await
        .unwrap();
    let states = rows
        .iter()
        .map(|row| {
            (
                row.id.clone(),
                row.last_state.clone(),
                row.last_state_reason.clone(),
                row.last_state_at.clone(),
            )
        })
        .collect();
    let masked = rows
        .into_iter()
        .map(|mut row| {
            row.last_state = None;
            row.last_state_reason = None;
            row.last_state_at = None;
            format!("{row:?}")
        })
        .collect();
    (masked, states)
}

#[sqlx::test(migrations = false)]
async fn every_table_is_dumped(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap();

    let tables: Vec<String> = sqlx::query_scalar!(
        r#"SELECT name AS "name!" FROM sqlite_schema
           WHERE type = 'table' AND name NOT LIKE 'sqlite\_%' ESCAPE '\'
                 AND name <> '_sqlx_migrations'
           ORDER BY name"#
    )
    .fetch_all(db.read_pool())
    .await
    .unwrap();

    assert_eq!(tables, TABLES, "a table isn't dumped by this test");
    assert_eq!(
        dump(db.read_pool())
            .await
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        TABLES
    );
}

#[sqlx::test(migrations = false)]
async fn an_event_batch_changes_only_chat_messages_and_the_held_bots_last_state(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let filled = Filled::new(options).await;
    let pool = filled.db.read_pool();
    let marks = Arc::new(Marks::default());
    let (front, mut writer) = ingest(
        Arc::new(filled.db.clone()),
        marks.clone(),
        EventBus::new(),
        IngestTimings {
            interval: Duration::from_millis(500),
            batch_rows: NonZeroUsize::new(10).unwrap(),
            wait: Duration::from_millis(250),
        },
        IngestBounds::default(),
    );
    let share = front.share();
    let report = |bot, reported| Report {
        agent: filled.agent,
        generation: 1,
        bot,
        received_at: at(T0 + 5),
        reported,
    };
    let before = dump(pool).await;
    let unwatched: Vec<_> = before
        .iter()
        .filter(|(_, rows)| rows.is_empty())
        .map(|(table, _)| table)
        .collect();
    assert!(unwatched.is_empty(), "no row to watch in {unwatched:?}");
    let (bots_before, states_before) = bots_without_state(pool).await;

    front
        .take(
            &share,
            report(
                filled.held,
                Reported::ChatReceived(IncomingChat::player(
                    PlayerChatKind::Whisper,
                    "Steve",
                    None,
                    "hi",
                )),
            ),
        )
        .unwrap();
    front
        .take(
            &share,
            report(
                filled.held,
                Reported::ModeChatSent(ChatMessage::try_from("/afk").unwrap()),
            ),
        )
        .unwrap();
    // Not the reporting agent's bot: skipped, and never touched.
    front
        .take(
            &share,
            report(
                filled.other,
                Reported::ChatReceived(IncomingChat::system("elsewhere")),
            ),
        )
        .unwrap();
    let paused = LastState::Paused(PauseReason::Conflict {
        kind: ConflictKind::DuplicateLogin,
    });
    marks.0.lock().unwrap().push(StickyWrite {
        bot: filled.held,
        agent: filled.agent,
        last: LastReport {
            state: paused,
            at: at(T0 + 5),
        },
    });
    let tick = writer.tick().await;
    let after = dump(pool).await;
    let (bots_after, states_after) = bots_without_state(pool).await;

    assert_eq!(
        tick,
        Tick::Written {
            stored: 2,
            skipped: 1,
            sticky: 1
        }
    );
    for table in TABLES {
        let (old, new) = (&before[table], &after[table]);
        match table {
            "chat_messages" => {
                assert_eq!(new.len(), old.len() + 2, "two new rows");
                assert_eq!(&new[..old.len()], &old[..], "no old row changed");
            }
            "bots" => assert_eq!(new.len(), old.len()),
            _ => assert_eq!(new, old, "{table} changed"),
        }
    }
    assert_eq!(
        bots_after, bots_before,
        "a column besides last_state* changed"
    );
    let held = filled.held.to_string();
    // All three columns of every bot: only the held one's may change.
    let changed: Vec<&StoredState> = states_after
        .iter()
        .zip(&states_before)
        .filter(|(new, old)| new != old)
        .map(|(new, _)| new)
        .collect();
    let expected: StoredState = (
        held,
        Some("paused".to_owned()),
        paused.reason().map(str::to_owned),
        Some("2027-01-15T08:00:00.005Z".to_owned()),
    );
    assert_eq!(changed, [&expected]);
}
