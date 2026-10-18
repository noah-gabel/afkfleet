//! Tests for the bots' chat log, `chat_messages` (Plan.md P11.1, P10.8,
//! Appendix D; ADR-0019, S5, S6, S70): a received message and a mode's
//! message read back as they were appended; a message is stored only while
//! its bot's row names the agent that reported it, and is skipped otherwise
//! without failing the transaction; a bot's messages page newest first by
//! ID; a bot's chat goes with its account; the table refuses what its CHECKs
//! forbid; and a stored value that doesn't read back is corrupt.
//!
//! Each test gets a fresh database from `#[sqlx::test]`. Time isn't paused:
//! sqlx's worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::authz::Role;
use fleet_core::chat::{ChatKind, ChatMessage, IncomingChat, PlayerChatKind};
use fleet_core::id::{AccountId, AgentId, BotId, UserId};
use fleet_core::mode::ModePreset;
use fleet_core::value::{AgentName, Username};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::agents::NewAgent;
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::bots::NewBot;
use fleet_server::ports::chat::{ChatEntry, ChatMessageId, ChatRecord, NewChatMessage};
use fleet_server::ports::page::{Page, PageLimit};
use fleet_server::ports::store::{Store, StoreError};
use fleet_server::ports::users::{NewUser, PasswordHash};
use secrecy::SecretString;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// The time everything starts at, in milliseconds.
const T0: i64 = 1_800_000_000_000;
/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// A player's profile UUID (a well-known example, not a real account's).
const PLAYER: &str = "069a79f4-44e9-4726-a5be-fca90e38aaf5";

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
        AuditAction::try_from("test.chat_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// The owner of every account.
async fn add_user(db: &Database) -> UserId {
    let id = UserId::new_v7(at(T0), [1; 10]).unwrap();
    let user = NewUser {
        id,
        username: Username::try_from("user1").unwrap(),
        password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
        role: Role::Member,
        created_at: at(T0),
    };
    let mut tx = db.write().await.unwrap();
    tx.users().insert(&user).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    id
}

/// Enabled agent `n`.
async fn add_agent(db: &Database, n: u8) -> AgentId {
    let agent = AgentId::new_v7(at(T0), [n; 10]).unwrap();
    let mut tx = db.write().await.unwrap();
    tx.agents()
        .insert(&NewAgent {
            id: agent,
            name: AgentName::try_from(format!("agent-{n}").as_str()).unwrap(),
            created_at: at(T0),
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    agent
}

/// Offline account `n` of `owner` with its bot `n`, assigned to `agent`
/// when one is given.
async fn add_bot(
    db: &Database,
    owner: UserId,
    n: u8,
    agent: Option<AgentId>,
) -> (AccountId, BotId) {
    let account = McAccount {
        id: AccountId::new_v7(at(T0 + i64::from(n)), [n; 10]).unwrap(),
        owner,
        name: format!("AfkBot{n}").parse().unwrap(),
        created_at: at(T0 + i64::from(n)),
        credential: AccountCredential::Offline,
    };
    let bot = NewBot {
        id: BotId::new_v7(at(T0 + i64::from(n)), [n; 10]).unwrap(),
        account: account.id,
        mode: ModePreset::Afk.id(),
    };
    let mut tx = db.write().await.unwrap();
    tx.accounts().insert(&account).await.unwrap();
    tx.bots().insert(&bot).await.unwrap();
    if let Some(agent) = agent {
        assert!(tx.bots().assign(bot.id, agent).await.unwrap());
    }
    tx.commit(setup_entry()).await.unwrap();
    (account.id, bot.id)
}

/// A database with one agent and one bot assigned to it.
async fn with_bot(options: SqliteConnectOptions) -> (Database, AgentId, BotId) {
    let db = database(options).await;
    let owner = add_user(&db).await;
    let agent = add_agent(&db, 1).await;
    let (_, bot) = add_bot(&db, owner, 1, Some(agent)).await;
    (db, agent, bot)
}

fn whisper(text: &str) -> ChatEntry {
    ChatEntry::Received(IncomingChat::player(
        PlayerChatKind::Whisper,
        "Steve",
        Some(PLAYER.parse().unwrap()),
        text,
    ))
}

fn system(text: &str) -> ChatEntry {
    ChatEntry::Received(IncomingChat::system(text))
}

fn mode_chat(text: &str) -> ChatEntry {
    ChatEntry::ModeSent(ChatMessage::try_from(text).unwrap())
}

fn message(bot: BotId, agent: AgentId, millis: i64, entry: ChatEntry) -> NewChatMessage {
    NewChatMessage {
        bot,
        agent,
        at: at(millis),
        entry,
    }
}

/// Appends each message in one transaction, committed with the setup entry;
/// what each append answered.
async fn append_all(db: &Database, messages: &[NewChatMessage]) -> Vec<Option<ChatMessageId>> {
    let mut tx = db.write().await.unwrap();
    let mut ids = Vec::new();
    for message in messages {
        ids.push(tx.chat().append(message).await.unwrap());
    }
    tx.commit(setup_entry()).await.unwrap();
    ids
}

fn page(before: Option<ChatMessageId>, limit: u32) -> Page<ChatMessageId> {
    Page {
        before,
        limit: PageLimit::new(limit).unwrap(),
    }
}

/// Every message of `bot`, newest first.
async fn all(db: &Database, bot: BotId) -> Vec<ChatRecord> {
    db.chat().page(bot, page(None, 100)).await.unwrap()
}

#[sqlx::test(migrations = false)]
async fn a_received_message_reads_back_with_its_kind_sender_and_cut(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, agent, bot) = with_bot(options).await;
    let long = "x".repeat(IncomingChat::MAX_TEXT_LEN + 1);
    let sent = [
        message(bot, agent, T0 + 1, whisper("hello there")),
        message(bot, agent, T0 + 2, system("Steve joined the game")),
        message(bot, agent, T0 + 3, whisper(&long)),
        message(
            bot,
            agent,
            T0 + 4,
            ChatEntry::Received(IncomingChat::player(
                PlayerChatKind::Emote,
                "Alex",
                None,
                "waves",
            )),
        ),
    ];

    let ids = append_all(&db, &sent).await;
    let read = all(&db, bot).await;

    assert!(ids.iter().all(Option::is_some), "{ids:?}");
    let expected: Vec<ChatRecord> = sent
        .iter()
        .zip(&ids)
        .rev()
        .map(|(sent, id)| ChatRecord {
            id: id.unwrap(),
            bot,
            at: sent.at,
            entry: sent.entry.clone(),
        })
        .collect();
    assert_eq!(read, expected);
    let ChatEntry::Received(cut) = &read.get(1).unwrap().entry else {
        panic!("not received");
    };
    assert!(cut.is_truncated());
    assert_eq!(cut.kind(), ChatKind::Whisper);
}

#[sqlx::test(migrations = false)]
async fn mode_chat_reads_back_as_outgoing_without_a_sender(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, agent, bot) = with_bot(options).await;

    let ids = append_all(&db, &[message(bot, agent, T0 + 1, mode_chat("/afk"))]).await;
    let stored = sqlx::query!(
        r#"SELECT direction, kind, sender_name, sender_uuid, truncated AS "truncated: i64"
           FROM chat_messages"#
    )
    .fetch_one(db.read_pool())
    .await
    .unwrap();

    assert_eq!(
        all(&db, bot).await,
        vec![ChatRecord {
            id: ids.first().unwrap().unwrap(),
            bot,
            at: at(T0 + 1),
            entry: mode_chat("/afk"),
        }]
    );
    assert_eq!(
        (
            stored.direction.as_str(),
            stored.kind.as_str(),
            stored.sender_name,
            stored.sender_uuid,
            stored.truncated
        ),
        ("out", "mode", None, None, 0)
    );
}

#[sqlx::test(migrations = false)]
async fn received_chat_is_stored_with_its_direction_and_kind_names(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, agent, bot) = with_bot(options).await;

    append_all(
        &db,
        &[
            message(bot, agent, T0 + 1, whisper("hi")),
            message(bot, agent, T0 + 2, system("restart in 5 minutes")),
        ],
    )
    .await;
    let stored = sqlx::query!(
        r#"SELECT direction, kind, sender_name, sender_uuid, text, at FROM chat_messages ORDER BY id"#
    )
    .fetch_all(db.read_pool())
    .await
    .unwrap();

    let rows: Vec<_> = stored
        .iter()
        .map(|row| {
            (
                row.direction.as_str(),
                row.kind.as_str(),
                row.sender_name.as_deref(),
                row.sender_uuid.as_deref(),
                row.text.as_str(),
                row.at.as_str(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "in",
                "whisper",
                Some("Steve"),
                Some(PLAYER),
                "hi",
                "2027-01-15T08:00:00.001Z"
            ),
            (
                "in",
                "system",
                None,
                None,
                "restart in 5 minutes",
                "2027-01-15T08:00:00.002Z"
            ),
        ]
    );
}

#[sqlx::test(migrations = false)]
async fn ids_follow_the_order_of_appends_across_transactions(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, agent, bot) = with_bot(options).await;

    let first = append_all(
        &db,
        &[
            message(bot, agent, T0 + 5, whisper("one")),
            message(bot, agent, T0 + 1, whisper("two")),
        ],
    )
    .await;
    let second = append_all(&db, &[message(bot, agent, T0, whisper("three"))]).await;

    let ids: Vec<i64> = first
        .iter()
        .chain(&second)
        .map(|id| id.unwrap().get())
        .collect();
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
    let texts: Vec<String> = all(&db, bot)
        .await
        .into_iter()
        .map(|record| match record.entry {
            ChatEntry::Received(chat) => chat.text().to_owned(),
            ChatEntry::ModeSent(chat) => chat.as_str().to_owned(),
        })
        .collect();
    assert_eq!(texts, ["three", "two", "one"]);
}

#[sqlx::test(migrations = false)]
async fn a_message_for_a_bot_thats_gone_is_skipped_and_the_batch_commits(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, agent, bot) = with_bot(options).await;
    let unknown = BotId::new_v7(at(T0), [9; 10]).unwrap();

    let ids = append_all(
        &db,
        &[
            message(unknown, agent, T0 + 1, whisper("lost")),
            message(bot, agent, T0 + 2, whisper("kept")),
        ],
    )
    .await;

    assert_eq!(ids.first(), Some(&None));
    assert!(ids.get(1).unwrap().is_some());
    assert_eq!(all(&db, bot).await.len(), 1);
    assert_eq!(all(&db, unknown).await, Vec::new());
}

#[sqlx::test(migrations = false)]
async fn a_message_for_a_bot_whose_row_doesnt_name_the_agent_is_skipped(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db).await;
    let agent = add_agent(&db, 1).await;
    let other = add_agent(&db, 2).await;
    let (_, elsewhere) = add_bot(&db, owner, 1, Some(other)).await;
    let (_, unassigned) = add_bot(&db, owner, 2, None).await;

    let ids = append_all(
        &db,
        &[
            message(elsewhere, agent, T0 + 1, whisper("assigned elsewhere")),
            message(unassigned, agent, T0 + 2, whisper("unassigned")),
            message(elsewhere, other, T0 + 3, whisper("its own agent")),
        ],
    )
    .await;

    assert_eq!(ids.first(), Some(&None));
    assert_eq!(ids.get(1), Some(&None));
    assert!(ids.get(2).unwrap().is_some());
    assert_eq!(all(&db, elsewhere).await.len(), 1);
    assert_eq!(all(&db, unassigned).await, Vec::new());
}

#[sqlx::test(migrations = false)]
async fn pages_are_newest_first_per_bot_and_before_is_exclusive(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db).await;
    let agent = add_agent(&db, 1).await;
    let (_, bot) = add_bot(&db, owner, 1, Some(agent)).await;
    let (_, other) = add_bot(&db, owner, 2, Some(agent)).await;
    let mut sent = Vec::new();
    for n in 0..5 {
        sent.push(message(bot, agent, T0 + n, whisper(&format!("mine {n}"))));
        sent.push(message(
            other,
            agent,
            T0 + n,
            whisper(&format!("other {n}")),
        ));
    }
    let ids = append_all(&db, &sent).await;
    let mine: Vec<ChatMessageId> = ids.iter().step_by(2).map(|id| id.unwrap()).collect();

    let first = db.chat().page(bot, page(None, 2)).await.unwrap();
    let cursor = first.get(1).unwrap().id;
    let second = db.chat().page(bot, page(Some(cursor), 2)).await.unwrap();
    let last = db
        .chat()
        .page(bot, page(Some(*mine.first().unwrap()), 2))
        .await
        .unwrap();

    let ids_of = |records: &[ChatRecord]| records.iter().map(|r| r.id).collect::<Vec<_>>();
    // A page holds one more row than its limit, so the caller knows whether
    // another page follows.
    assert_eq!(
        ids_of(&first),
        vec![mine[4], mine[3], mine[2]],
        "the newest first, limit + 1"
    );
    assert_eq!(ids_of(&second), vec![mine[2], mine[1], mine[0]]);
    assert!(last.is_empty(), "nothing before the oldest");
    assert!(first.iter().chain(&second).all(|record| record.bot == bot));
}

#[sqlx::test(migrations = false)]
async fn a_bots_chat_is_deleted_with_its_account(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db).await;
    let agent = add_agent(&db, 1).await;
    let (account, bot) = add_bot(&db, owner, 1, Some(agent)).await;
    let (_, kept) = add_bot(&db, owner, 2, Some(agent)).await;
    append_all(
        &db,
        &[
            message(bot, agent, T0 + 1, whisper("going")),
            message(kept, agent, T0 + 2, whisper("staying")),
        ],
    )
    .await;

    let mut tx = db.write().await.unwrap();
    tx.accounts().delete(account, at(T0 + 3)).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();

    let left = sqlx::query_scalar!(r#"SELECT count(*) AS "count!: i64" FROM chat_messages"#)
        .fetch_one(db.read_pool())
        .await
        .unwrap();
    assert_eq!(left, 1);
    assert_eq!(all(&db, kept).await.len(), 1);
}

#[sqlx::test(migrations = false)]
async fn a_dropped_transaction_stores_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, agent, bot) = with_bot(options).await;

    let mut tx = db.write().await.unwrap();
    let id = tx
        .chat()
        .append(&message(bot, agent, T0 + 1, whisper("never")))
        .await
        .unwrap();
    drop(tx);

    assert!(id.is_some());
    assert_eq!(all(&db, bot).await, Vec::new());
}

#[sqlx::test(migrations = false)]
async fn a_time_outside_the_years_0_to_9999_is_unstorable(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, agent, bot) = with_bot(options).await;
    let far = DateTime::<Utc>::from_timestamp(253_402_300_800, 0).unwrap();

    let mut tx = db.write().await.unwrap();
    let result = tx
        .chat()
        .append(&NewChatMessage {
            at: far,
            ..message(bot, agent, T0, whisper("later"))
        })
        .await;

    assert!(
        matches!(
            result,
            Err(StoreError::Unstorable {
                table: "chat_messages",
                column: "at"
            })
        ),
        "{result:?}"
    );
}

/// A raw row, as a test writes it past the repository.
#[derive(Debug, Clone, Copy)]
struct Raw<'a> {
    direction: &'a str,
    kind: &'a str,
    sender_name: Option<&'a str>,
    sender_uuid: Option<&'a str>,
    text: &'a str,
    truncated: i64,
    at: &'a str,
}

/// A system message received at T0, which the table accepts.
const FINE: Raw<'static> = Raw {
    direction: "in",
    kind: "system",
    sender_name: None,
    sender_uuid: None,
    text: "text",
    truncated: 0,
    at: "2027-01-15T08:00:00.000Z",
};

/// Inserts `row` for `bot` through the write pool.
async fn insert_raw(db: &Database, bot: &str, row: Raw<'_>) -> Result<(), sqlx::Error> {
    let Raw {
        direction,
        kind,
        sender_name,
        sender_uuid,
        text,
        truncated,
        at,
    } = row;
    sqlx::query!(
        "INSERT INTO chat_messages
             (bot_id, direction, kind, sender_name, sender_uuid, text, truncated, at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        bot,
        direction,
        kind,
        sender_name,
        sender_uuid,
        text,
        truncated,
        at
    )
    .execute(db.write_pool())
    .await
    .map(drop)
}

#[sqlx::test(migrations = false)]
async fn the_table_refuses_what_its_checks_forbid(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (db, _, bot) = with_bot(options).await;
    let bot = bot.to_string();
    let cases = [
        (
            "an unknown direction",
            Raw {
                direction: "sideways",
                ..FINE
            },
        ),
        (
            "a UUID in another form",
            Raw {
                kind: "chat",
                sender_name: Some("Steve"),
                sender_uuid: Some("069A79F4-44E9-4726-A5BE-FCA90E38AAF5"),
                ..FINE
            },
        ),
        (
            "a UUID without a name",
            Raw {
                kind: "chat",
                sender_uuid: Some(PLAYER),
                ..FINE
            },
        ),
        (
            "an outgoing message with a sender",
            Raw {
                direction: "out",
                kind: "mode",
                sender_name: Some("Steve"),
                ..FINE
            },
        ),
        (
            "an outgoing message cut",
            Raw {
                direction: "out",
                kind: "mode",
                truncated: 1,
                ..FINE
            },
        ),
        (
            "a cut that isn't 0 or 1",
            Raw {
                truncated: 2,
                ..FINE
            },
        ),
        (
            "a time in another form",
            Raw {
                at: "2027-01-15 08:00:00",
                ..FINE
            },
        ),
        (
            "a time without milliseconds",
            Raw {
                at: "2027-01-15T08:00:00Z",
                ..FINE
            },
        ),
    ];

    for (case, row) in cases {
        let result = insert_raw(&db, &bot, row).await;
        assert!(result.is_err(), "{case} was stored");
    }
    let unknown_bot = insert_raw(&db, "018bcfe5-6800-7cdc-8dcd-cdcdcdcdcdcd", FINE).await;
    assert!(unknown_bot.is_err(), "a row for an unknown bot was stored");
    let fine = insert_raw(&db, &bot, FINE).await;
    assert!(fine.is_ok(), "{fine:?}");
}

#[sqlx::test(migrations = false)]
async fn a_row_that_doesnt_read_back_is_corrupt_naming_its_column(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db).await;
    let agent = add_agent(&db, 1).await;
    let cases = [
        (
            Raw {
                kind: "shout",
                ..FINE
            },
            "kind",
        ),
        (
            Raw {
                sender_name: Some("Steve"),
                ..FINE
            },
            "sender_name",
        ),
        (
            Raw {
                kind: "chat",
                ..FINE
            },
            "sender_name",
        ),
        (
            Raw {
                direction: "out",
                kind: "shout",
                ..FINE
            },
            "kind",
        ),
        (
            Raw {
                direction: "out",
                kind: "mode",
                text: " not trimmed",
                ..FINE
            },
            "text",
        ),
    ];

    for (n, (row, column)) in (10_u8..).zip(cases) {
        let (_, bot) = add_bot(&db, owner, n, Some(agent)).await;
        insert_raw(&db, &bot.to_string(), row).await.unwrap();

        let result = db.chat().page(bot, page(None, 10)).await;

        assert!(
            matches!(
                result,
                Err(StoreError::Corrupt { table: "chat_messages", column: found, .. }) if found == column
            ),
            "{row:?}: {result:?}"
        );
    }
}
