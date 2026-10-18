//! The scope of `UnauditedWrite::EventBatch` (Plan.md P10.8; ADR-0019, U17,
//! the user's request at the plan review): one batch holding chat and a
//! sticky crossing, over a database with rows in every table, changes
//! nothing but new `chat_messages` rows and the held bot's
//! `bots.last_state`, `last_state_reason` and `last_state_at`. No row of any
//! table is deleted, and no other row or column is touched.
//!
//! Every table is dumped whole with `SELECT *`, so a column added later is
//! compared too; [`every_table_is_dumped`] fails when a table is added
//! without being dumped here. The list of tables is fixed, one query each:
//! a dump that built its SQL from the schema's names would break rule 8
//! (never SQL from strings), and the fixed list costs nothing that test
//! doesn't catch. The database is a fresh one from
//! `#[sqlx::test]`, on real time (sqlx's worker threads, CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::num::NonZeroUsize;
use core::time::Duration;
use std::collections::BTreeMap;
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

/// The time everything starts at, in milliseconds.
const T0: i64 = 1_800_000_000_000;
/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// Every table the migrations create; [`every_table_is_dumped`] keeps it
/// whole.
const TABLES: [&str; 17] = [
    "account_grants",
    "agent_certs",
    "agents",
    "audit_log",
    "bots",
    "chat_messages",
    "enrollment_tokens",
    "invites",
    "login_attempts",
    "mc_accounts",
    "modes",
    "recovery_codes",
    "releasing_bots",
    "rotated_refresh_tokens",
    "sessions",
    "user_mfa",
    "users",
];

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
        fill_users(self.db.write_pool(), &ids).await;
        fill_fleet(self.db.write_pool(), &ids).await;
    }
}

/// The IDs [`Filled::fill`]'s rows refer to, as stored.
struct Ids {
    owner: String,
    friend: String,
    account: String,
    one: String,
    two: String,
    held: String,
}

const SESSION: &str = "018bcfe5-6800-7cdc-8dcd-cdcdcdcd0001";
const INVITE: &str = "018bcfe5-6800-7cdc-8dcd-cdcdcdcd0002";
const TOKEN: &str = "018bcfe5-6800-7cdc-8dcd-cdcdcdcd0003";
const GONE: &str = "018bcfe5-6800-7cdc-8dcd-cdcdcdcd0004";
const WHEN: &str = "2027-01-15T08:00:00.000Z";
const LATER: &str = "2027-03-15T08:00:00.000Z";

/// A row of each table about users and their sign-in.
async fn fill_users(pool: &SqlitePool, ids: &Ids) {
    sqlx::query!(
        "INSERT INTO sessions
             (id, user_id, access_hash, refresh_hash, created_at, refreshed_at, last_used_at)
         VALUES (?1, ?2, zeroblob(32), zeroblob(32), ?3, ?3, ?3)",
        SESSION,
        ids.owner,
        WHEN
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query!(
        "INSERT INTO rotated_refresh_tokens (hash, session_id, rotated_at)
         VALUES (zeroblob(32), ?1, ?2)",
        SESSION,
        WHEN
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query!(
        "INSERT INTO login_attempts (username_key, failures, last_failure_at)
         VALUES (zeroblob(32), 1, ?1)",
        WHEN
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query!(
        "INSERT INTO user_mfa (user_id, secret_envelope, last_used_step, confirmed_at)
         VALUES (?1, zeroblob(65), 0, ?2)",
        ids.owner,
        WHEN
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query!(
        "INSERT INTO recovery_codes (hash, user_id) VALUES (zeroblob(32), ?1)",
        ids.owner
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query!(
        "INSERT INTO invites (id, code_hash, role, created_by, created_at, expires_at)
         VALUES (?1, zeroblob(32), 'member', ?2, ?3, ?4)",
        INVITE,
        ids.owner,
        WHEN,
        LATER
    )
    .execute(pool)
    .await
    .unwrap();
}

/// A row of each table about accounts, agents and bots.
async fn fill_fleet(pool: &SqlitePool, ids: &Ids) {
    sqlx::query!(
        "INSERT INTO account_grants (account_id, user_id, level, granted_by, granted_at)
         VALUES (?1, ?2, 'view', ?3, ?4)",
        ids.account,
        ids.friend,
        ids.owner,
        WHEN
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query!(
        "INSERT INTO agent_certs (fingerprint_sha256, agent_id, not_after, created_at)
         VALUES (zeroblob(32), ?1, ?2, ?3)",
        ids.one,
        LATER,
        WHEN
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query!(
        "INSERT INTO enrollment_tokens
             (id, token_hash, created_by, created_at, expires_at, used_at, agent_id)
         VALUES (?1, zeroblob(32), ?2, ?3, ?4, ?3, ?5)",
        TOKEN,
        ids.owner,
        WHEN,
        LATER,
        ids.two
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query!(
        "INSERT INTO releasing_bots
             (bot_id, agent_id, account_id, kind, mc_name, created_at)
         VALUES (?1, ?2, ?1, 'offline', 'gone', ?3)",
        GONE,
        ids.two,
        WHEN
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query!(
        "INSERT INTO chat_messages (bot_id, direction, kind, text, truncated, at)
         VALUES (?1, 'in', 'system', 'an older message', 0, ?2)",
        ids.held,
        WHEN
    )
    .execute(pool)
    .await
    .unwrap();
}

/// Every row of every table, each as its `Debug` text, by table.
async fn dump(pool: &SqlitePool) -> BTreeMap<&'static str, Vec<String>> {
    macro_rules! rows {
        ($query:literal) => {
            sqlx::query!($query)
                .fetch_all(pool)
                .await
                .unwrap()
                .iter()
                .map(|row| format!("{row:?}"))
                .collect::<Vec<_>>()
        };
    }
    BTreeMap::from([
        (
            "account_grants",
            rows!("SELECT * FROM account_grants ORDER BY rowid"),
        ),
        (
            "agent_certs",
            rows!("SELECT * FROM agent_certs ORDER BY rowid"),
        ),
        ("agents", rows!("SELECT * FROM agents ORDER BY rowid")),
        ("audit_log", rows!("SELECT * FROM audit_log ORDER BY rowid")),
        ("bots", rows!("SELECT * FROM bots ORDER BY rowid")),
        (
            "chat_messages",
            rows!("SELECT * FROM chat_messages ORDER BY rowid"),
        ),
        (
            "enrollment_tokens",
            rows!("SELECT * FROM enrollment_tokens ORDER BY rowid"),
        ),
        ("invites", rows!("SELECT * FROM invites ORDER BY rowid")),
        (
            "login_attempts",
            rows!("SELECT * FROM login_attempts ORDER BY rowid"),
        ),
        (
            "mc_accounts",
            rows!("SELECT * FROM mc_accounts ORDER BY rowid"),
        ),
        ("modes", rows!("SELECT * FROM modes ORDER BY rowid")),
        (
            "recovery_codes",
            rows!("SELECT * FROM recovery_codes ORDER BY rowid"),
        ),
        (
            "releasing_bots",
            rows!("SELECT * FROM releasing_bots ORDER BY rowid"),
        ),
        (
            "rotated_refresh_tokens",
            rows!("SELECT * FROM rotated_refresh_tokens ORDER BY rowid"),
        ),
        ("sessions", rows!("SELECT * FROM sessions ORDER BY rowid")),
        ("user_mfa", rows!("SELECT * FROM user_mfa ORDER BY rowid")),
        ("users", rows!("SELECT * FROM users ORDER BY rowid")),
    ])
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
