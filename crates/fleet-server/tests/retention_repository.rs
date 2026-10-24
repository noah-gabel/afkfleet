//! Tests for the retention's deletes and cuts (Plan.md P11.6; ADR-0020,
//! S45, S46, R50, R51): each delete takes at most its limit, the oldest
//! first, and exactly what is due: what is at or before its cut, never a
//! row a millisecond newer; a bot's size cut keeps exactly its newest lines,
//! users' lines among them; the audit log goes as a prefix by ID, so an
//! entry after a clock stepped backwards keeps the entries after it; an
//! expired session's replaced tokens go before it, in batches; used invites
//! and enrollment tokens stay; and no delete touches another table.
//!
//! Each test gets a fresh database from `#[sqlx::test]`. Many rows are
//! inserted at once by recursive inserts through sqlx's macros. Time isn't
//! paused: sqlx's worker threads would make paused timeouts fire early
//! (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use chrono::{DateTime, TimeDelta, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::authz::Role;
use fleet_core::id::{AccountId, AgentId, BotId, EnrollmentTokenId, InviteId, SessionId, UserId};
use fleet_core::mode::ModePreset;
use fleet_core::value::{AgentName, Username};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::agents::NewAgent;
use fleet_server::ports::audit::{AuditEntryId, NewAuditEntry};
use fleet_server::ports::bots::NewBot;
use fleet_server::ports::chat::ChatMessageId;
use fleet_server::ports::enrollment_tokens::NewEnrollmentToken;
use fleet_server::ports::invites::NewInvite;
use fleet_server::ports::retention::RetentionWrites;
use fleet_server::ports::sessions::NewSession;
use fleet_server::ports::store::{Store, UnauditedWrite};
use fleet_server::ports::tokens::TokenHash;
use fleet_server::ports::users::{NewUser, PasswordHash};
use secrecy::SecretString;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// The time everything starts at, in milliseconds.
const T0: i64 = 1_800_000_000_000;
/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// The batches' size, as the retention's.
const BATCH: u32 = 1000;

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

/// `at`'s stored text.
fn stored(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

fn days(n: i64) -> TimeDelta {
    TimeDelta::days(n)
}

fn millis(n: i64) -> TimeDelta {
    TimeDelta::milliseconds(n)
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
        AuditAction::try_from("test.retention_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// User `n`.
async fn add_user(db: &Database, n: u8) -> UserId {
    let id = UserId::new_v7(at(T0), [n; 10]).unwrap();
    let user = NewUser {
        id,
        username: Username::try_from(format!("user{n}").as_str()).unwrap(),
        password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
        role: Role::Member,
        created_at: at(T0),
    };
    let mut tx = db.write().await.unwrap();
    tx.users().insert(&user).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    id
}

/// Agent `n`.
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

/// Offline account `n` of `owner`, with its bot.
async fn add_bot(db: &Database, owner: UserId, n: u8) -> BotId {
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
    tx.commit(setup_entry()).await.unwrap();
    bot.id
}

/// `count` system lines of `bot`, received at `received`, inserted at once.
async fn lines(db: &Database, bot: BotId, count: u32, received: DateTime<Utc>) {
    let bot = bot.to_string();
    let received = stored(received);
    sqlx::query!(
        "WITH RECURSIVE n(i) AS (SELECT 1 WHERE ?1 > 0 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
         INSERT INTO chat_messages (bot_id, direction, kind, text, truncated, at)
         SELECT ?2, 'in', 'system', 'line ' || i, 0, ?3 FROM n",
        count,
        bot,
        received
    )
    .execute(db.write_pool())
    .await
    .unwrap();
}

/// `count` lines `user` asked `bot` to send, received at `received`.
async fn users_lines(db: &Database, bot: BotId, user: UserId, count: u32, received: DateTime<Utc>) {
    let (bot, user) = (bot.to_string(), user.to_string());
    let received = stored(received);
    sqlx::query!(
        "WITH RECURSIVE n(i) AS (SELECT 1 WHERE ?1 > 0 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
         INSERT INTO chat_messages (bot_id, direction, kind, text, truncated, at, sent_by)
         SELECT ?2, 'out', 'user', 'asked ' || i, 0, ?3, ?4 FROM n",
        count,
        bot,
        received,
        user
    )
    .execute(db.write_pool())
    .await
    .unwrap();
}

/// The IDs of `bot`'s lines, oldest first.
async fn line_ids(db: &Database, bot: BotId) -> Vec<i64> {
    let bot = bot.to_string();
    sqlx::query_scalar!(
        r#"SELECT id AS "id!: i64" FROM chat_messages WHERE bot_id = ? ORDER BY id"#,
        bot
    )
    .fetch_all(db.read_pool())
    .await
    .unwrap()
}

/// `count` audit entries dated `dated`, recorded at once.
async fn entries(db: &Database, count: u32, dated: DateTime<Utc>) {
    let dated = stored(dated);
    sqlx::query!(
        "WITH RECURSIVE n(i) AS (SELECT 1 WHERE ?1 > 0 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
         INSERT INTO audit_log (at, action, outcome) SELECT ?2, 'test.old_entry', 'success' FROM n",
        count,
        dated
    )
    .execute(db.write_pool())
    .await
    .unwrap();
}

/// The audit log's IDs, oldest first.
async fn entry_ids(db: &Database) -> Vec<i64> {
    sqlx::query_scalar!(r#"SELECT id AS "id!: i64" FROM audit_log ORDER BY id"#)
        .fetch_all(db.read_pool())
        .await
        .unwrap()
}

/// Session `n` of `user`, created at `created`, revoked when asked.
async fn add_session(
    db: &Database,
    user: UserId,
    n: u8,
    created: DateTime<Utc>,
    revoked: bool,
) -> SessionId {
    let id = SessionId::new_v7(created, [n; 10]).unwrap();
    let mut tx = db.write().await.unwrap();
    tx.sessions()
        .insert(&NewSession {
            id,
            user_id: user,
            access_hash: TokenHash::from_bytes([n; 32]),
            refresh_hash: TokenHash::from_bytes([n.wrapping_add(100); 32]),
            created_at: created,
        })
        .await
        .unwrap();
    if revoked {
        assert!(tx.sessions().revoke(id, created).await.unwrap());
    }
    tx.commit(setup_entry()).await.unwrap();
    id
}

/// `count` replaced refresh tokens of `session`, inserted at once.
async fn rotations(db: &Database, session: SessionId, count: u32) {
    let session = session.to_string();
    let rotated = stored(at(T0));
    sqlx::query!(
        "WITH RECURSIVE n(i) AS (SELECT 1 WHERE ?1 > 0 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
         INSERT INTO rotated_refresh_tokens (hash, session_id, rotated_at)
         SELECT randomblob(32), ?2, ?3 FROM n",
        count,
        session,
        rotated
    )
    .execute(db.write_pool())
    .await
    .unwrap();
}

/// How many replaced tokens `session` has.
async fn rotations_of(db: &Database, session: SessionId) -> i64 {
    let session = session.to_string();
    sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!: i64" FROM rotated_refresh_tokens WHERE session_id = ?"#,
        session
    )
    .fetch_one(db.read_pool())
    .await
    .unwrap()
}

/// The sessions' IDs.
async fn session_ids(db: &Database) -> Vec<String> {
    sqlx::query_scalar!(r#"SELECT id AS "id!" FROM sessions ORDER BY id"#)
        .fetch_all(db.read_pool())
        .await
        .unwrap()
}

/// Invite `n` by `creator`, expiring at `expires`.
async fn add_invite(db: &Database, creator: UserId, n: u8, expires: DateTime<Utc>) -> InviteId {
    let created = expires - days(1);
    let id = InviteId::new_v7(created, [n; 10]).unwrap();
    let mut tx = db.write().await.unwrap();
    tx.invites()
        .insert(&NewInvite {
            id,
            code_hash: TokenHash::from_bytes([n; 32]),
            role: Role::Member,
            created_by: creator,
            created_at: created,
            expires_at: expires,
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    id
}

/// The invites' IDs.
async fn invite_ids(db: &Database) -> Vec<String> {
    sqlx::query_scalar!(r#"SELECT id AS "id!" FROM invites ORDER BY id"#)
        .fetch_all(db.read_pool())
        .await
        .unwrap()
}

/// Enrollment token `n`, expiring at `expires`.
async fn add_token(db: &Database, n: u8, expires: DateTime<Utc>) -> EnrollmentTokenId {
    let created = expires - TimeDelta::minutes(15);
    let id = EnrollmentTokenId::new_v7(created, [n; 10]).unwrap();
    let mut tx = db.write().await.unwrap();
    tx.enrollment_tokens()
        .insert(&NewEnrollmentToken {
            id,
            token_hash: TokenHash::from_bytes([n; 32]),
            created_by: None,
            created_at: created,
            expires_at: expires,
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    id
}

/// The enrollment tokens' IDs.
async fn token_ids(db: &Database) -> Vec<String> {
    sqlx::query_scalar!(r#"SELECT id AS "id!" FROM enrollment_tokens ORDER BY id"#)
        .fetch_all(db.read_pool())
        .await
        .unwrap()
}

/// One batch of `delete`, committed as the retention commits it.
async fn batch<F>(db: &Database, delete: F) -> u64
where
    F: for<'a> FnOnce(
        &'a mut dyn RetentionWrites,
    ) -> core::pin::Pin<Box<dyn Future<Output = u64> + Send + 'a>>,
{
    let mut tx = db.write().await.unwrap();
    let deleted = delete(tx.retention()).await;
    tx.commit_unaudited(UnauditedWrite::RetentionPurge)
        .await
        .unwrap();
    deleted
}

/// Batches of `delete` until one comes back short: each batch's count.
async fn drain<F>(db: &Database, limit: u32, delete: F) -> Vec<u64>
where
    F: Fn(
            &mut dyn RetentionWrites,
            u32,
        ) -> core::pin::Pin<Box<dyn Future<Output = u64> + Send + '_>>
        + Copy,
{
    let mut counts = Vec::new();
    loop {
        let deleted = batch(db, move |writes| delete(writes, limit)).await;
        counts.push(deleted);
        if deleted < u64::from(limit) {
            return counts;
        }
    }
}

// --- Chat by age -------------------------------------------------------------

/// The age cut over `due` lines at the cut and 5 a millisecond after it, for
/// each `due` in the table, each on a bot of its own with nothing else
/// stored: batches of 1000, the last one short, every due line gone and
/// nothing newer. (A table in one test: rstest's cases can't take
/// `sqlx::test`'s database.)
#[sqlx::test(migrations = false)]
async fn chat_at_or_before_its_cut_goes_in_batches_and_nothing_newer(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let cut = at(T0) + days(1);
    let table: [(u32, &[u64]); 6] = [
        (0, &[0]),
        (1, &[1]),
        (999, &[999]),
        (1000, &[1000, 0]),
        (1001, &[1000, 1]),
        (2001, &[1000, 1000, 1]),
    ];

    for (n, (due, expected)) in (1_u8..).zip(table) {
        let bot = add_bot(&db, owner, n).await;
        lines(&db, bot, due, cut).await;
        lines(&db, bot, 5, cut + millis(1)).await;

        let counts = drain(&db, BATCH, |writes, limit| {
            Box::pin(async move { writes.delete_chat_received_by(cut, limit).await.unwrap() })
        })
        .await;

        assert_eq!(counts, expected, "{due} due");
        assert_eq!(line_ids(&db, bot).await.len(), 5, "{due} due");
        clear_chat(&db).await;
    }
    db.close().await;
}

/// Deletes every chat line, so the next case starts empty.
async fn clear_chat(db: &Database) {
    sqlx::query!("DELETE FROM chat_messages")
        .execute(db.write_pool())
        .await
        .unwrap();
}

/// Of three lines dated a millisecond apart around the cut, only the one at
/// it and the one before go, oldest first.
#[sqlx::test(migrations = false)]
async fn the_age_cut_takes_the_oldest_first(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let bot = add_bot(&db, owner, 1).await;
    let cut = at(T0) + days(1);
    lines(&db, bot, 1, cut + millis(1)).await;
    lines(&db, bot, 1, cut).await;
    lines(&db, bot, 1, cut - millis(1)).await;
    let ids = line_ids(&db, bot).await;

    let first = batch(&db, |writes| {
        Box::pin(async move { writes.delete_chat_received_by(cut, 1).await.unwrap() })
    })
    .await;
    let after_first = line_ids(&db, bot).await;
    let rest = batch(&db, |writes| {
        Box::pin(async move { writes.delete_chat_received_by(cut, 5).await.unwrap() })
    })
    .await;

    assert_eq!((first, rest), (1, 1));
    assert_eq!(
        after_first,
        [ids[0], ids[1]],
        "the oldest, a millisecond before the cut, went first"
    );
    assert_eq!(line_ids(&db, bot).await, [ids[0]], "the newer one stays");
    db.close().await;
}

// --- Chat by size --------------------------------------------------------------

/// A bot's cut, with 1000 kept, for each size in the table, a pair of bots
/// of its own: none at 999 or 1000 lines; at 2500, the line just past its
/// newest 1000, so the 1500 oldest go, users' lines among them, and the
/// other bot's lines stay.
#[sqlx::test(migrations = false)]
async fn the_size_cut_keeps_exactly_a_bots_newest_lines(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let table: [(u32, Option<usize>); 3] = [(999, None), (1000, None), (2500, Some(1500))];

    for (n, (held, over)) in (1_u8..).step_by(2).zip(table) {
        let bot = add_bot(&db, owner, n).await;
        let other = add_bot(&db, owner, n + 1).await;
        let half = held / 2;
        lines(&db, bot, half, at(T0)).await;
        users_lines(&db, bot, owner, held - half, at(T0)).await;
        lines(&db, other, 3, at(T0)).await;
        let ids = line_ids(&db, bot).await;

        let cut = db.retention().chat_cut(bot, 1000).await.unwrap();
        if let Some(cut) = cut {
            drain(&db, BATCH, |writes, limit| {
                Box::pin(async move { writes.delete_chat_through(bot, cut, limit).await.unwrap() })
            })
            .await;
        }

        match over {
            None => assert_eq!(cut, None, "{held} held"),
            Some(over) => {
                assert_eq!(cut, Some(ChatMessageId::new(ids[over - 1])));
                assert_eq!(line_ids(&db, bot).await, ids[over..]);
            }
        }
        assert_eq!(line_ids(&db, other).await.len(), 3, "{held} held");
    }
    db.close().await;
}

/// The size cut's deletes in batches: 2001 lines through the cut go as 1000,
/// 1000 and 1; a line written after the cut was read, newer, stays.
#[sqlx::test(migrations = false)]
async fn the_size_cut_goes_in_batches_and_spares_lines_written_after_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let bot = add_bot(&db, owner, 1).await;
    lines(&db, bot, 2101, at(T0)).await;
    let cut = db.retention().chat_cut(bot, 100).await.unwrap().unwrap();
    lines(&db, bot, 1, at(T0)).await;

    let counts = drain(&db, BATCH, |writes, limit| {
        Box::pin(async move { writes.delete_chat_through(bot, cut, limit).await.unwrap() })
    })
    .await;

    assert_eq!(counts, [1000, 1000, 1]);
    assert_eq!(line_ids(&db, bot).await.len(), 101);
    db.close().await;
}

// --- The audit log -------------------------------------------------------------

/// Entries dated old, old, new, old again (a clock stepped backwards) and
/// new: the cut is the second, so the out-of-order old entry stays with
/// everything after the first new one.
#[sqlx::test(migrations = false)]
async fn the_audit_log_goes_as_a_prefix_so_a_clock_step_back_keeps_entries(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let cut = at(T0) + days(100);
    for dated in [
        cut,
        cut - millis(1),
        cut + millis(1),
        cut - days(1),
        cut + days(1),
    ] {
        entries(&db, 1, dated).await;
    }
    let ids = entry_ids(&db).await;

    let through = db.retention().audit_cut(cut).await.unwrap().unwrap();
    let counts = drain(&db, BATCH, |writes, limit| {
        Box::pin(async move { writes.delete_audit_through(through, limit).await.unwrap() })
    })
    .await;

    assert_eq!(through, AuditEntryId::new(ids[1]));
    assert_eq!(counts, [2]);
    assert_eq!(entry_ids(&db).await, ids[2..]);
    db.close().await;
}

/// The audit cut: below the first entry when that one is newer; the last
/// entry when every one is due; none for an empty log.
#[sqlx::test(migrations = false)]
async fn the_audit_cut_is_below_a_newer_first_entry_and_the_last_when_all_are_due(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let cut = at(T0) + days(100);

    let empty = db.retention().audit_cut(cut).await.unwrap();
    entries(&db, 2, cut + millis(1)).await;
    let ids = entry_ids(&db).await;
    let all_kept = db.retention().audit_cut(cut).await.unwrap();
    let all_due = db.retention().audit_cut(cut + days(1)).await.unwrap();

    assert_eq!(empty, None);
    assert_eq!(all_kept, Some(AuditEntryId::new(ids[0] - 1)));
    assert_eq!(all_due, Some(AuditEntryId::new(ids[1])));
    db.close().await;
}

/// 2001 due entries go as 1000, 1000 and 1, oldest first.
#[sqlx::test(migrations = false)]
async fn audit_entries_go_in_batches(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let cut = at(T0) + days(100);
    entries(&db, 2001, cut).await;
    entries(&db, 3, cut + millis(1)).await;
    let through = db.retention().audit_cut(cut).await.unwrap().unwrap();

    let counts = drain(&db, BATCH, |writes, limit| {
        Box::pin(async move { writes.delete_audit_through(through, limit).await.unwrap() })
    })
    .await;

    assert_eq!(counts, [1000, 1000, 1]);
    assert_eq!(entry_ids(&db).await.len(), 3);
    db.close().await;
}

// --- Sessions ------------------------------------------------------------------

/// An expired session with 2500 replaced tokens: they go in batches of 1000,
/// none longer, then the session in one batch with nothing left to cascade.
#[sqlx::test(migrations = false)]
async fn an_expired_sessions_replaced_tokens_go_first_in_batches(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let cut = at(T0) + days(30);
    let expired = add_session(&db, user, 1, cut, false).await;
    rotations(&db, expired, 2500).await;

    let tokens = drain(&db, BATCH, |writes, limit| {
        Box::pin(async move { writes.delete_expired_rotations(cut, limit).await.unwrap() })
    })
    .await;
    let left = rotations_of(&db, expired).await;
    let sessions = drain(&db, BATCH, |writes, limit| {
        Box::pin(async move { writes.delete_expired_sessions(cut, limit).await.unwrap() })
    })
    .await;

    assert_eq!(tokens, [1000, 1000, 500]);
    assert_eq!(left, 0);
    assert_eq!(sessions, [1]);
    assert_eq!(session_ids(&db).await, Vec::<String>::new());
    db.close().await;
}

/// Sessions created at the cut and before go, revoked or not, with their
/// replaced tokens; one a millisecond newer stays with its own.
#[sqlx::test(migrations = false)]
async fn sessions_created_at_or_before_the_cut_go_revoked_or_not(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let cut = at(T0) + days(30);
    let live = add_session(&db, user, 1, cut, false).await;
    let revoked = add_session(&db, user, 2, cut - days(1), true).await;
    let newer = add_session(&db, user, 3, cut + millis(1), false).await;
    for session in [live, revoked, newer] {
        rotations(&db, session, 3).await;
    }

    let tokens = drain(&db, 2, |writes, limit| {
        Box::pin(async move { writes.delete_expired_rotations(cut, limit).await.unwrap() })
    })
    .await;
    let sessions = drain(&db, 1, |writes, limit| {
        Box::pin(async move { writes.delete_expired_sessions(cut, limit).await.unwrap() })
    })
    .await;

    assert_eq!(tokens, [2, 2, 2, 0]);
    assert_eq!(sessions, [1, 1, 0]);
    assert_eq!(session_ids(&db).await, [newer.to_string()]);
    assert_eq!(rotations_of(&db, newer).await, 3);
    db.close().await;
}

// --- Invites and enrollment tokens -------------------------------------------

/// Unused invites that expired at the cut or before go, a revoked one too;
/// one expiring a millisecond later, a used one however old, stay.
#[sqlx::test(migrations = false)]
async fn unused_invites_expired_by_the_cut_go_and_used_ones_stay(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let creator = add_user(&db, 1).await;
    let invited = add_user(&db, 2).await;
    let cut = at(T0) + days(400);
    let expired = add_invite(&db, creator, 1, cut).await;
    let revoked = add_invite(&db, creator, 2, cut - days(10)).await;
    let used = add_invite(&db, creator, 3, cut - days(20)).await;
    let newer = add_invite(&db, creator, 4, cut + millis(1)).await;
    let mut tx = db.write().await.unwrap();
    assert!(tx.invites().revoke(revoked, cut - days(11)).await.unwrap());
    tx.commit(setup_entry()).await.unwrap();
    let (used_text, invited_text, used_at) = (
        used.to_string(),
        invited.to_string(),
        stored(cut - days(21)),
    );
    sqlx::query!(
        "UPDATE invites SET used_by = ?, used_at = ? WHERE id = ?",
        invited_text,
        used_at,
        used_text
    )
    .execute(db.write_pool())
    .await
    .unwrap();

    let counts = drain(&db, 1, |writes, limit| {
        Box::pin(async move { writes.delete_unused_invites(cut, limit).await.unwrap() })
    })
    .await;

    assert_eq!(counts, [1, 1, 0]);
    let mut kept = vec![used.to_string(), newer.to_string()];
    kept.sort();
    assert_eq!(invite_ids(&db).await, kept);
    assert!(!kept.contains(&expired.to_string()));
    db.close().await;
}

/// Unused enrollment tokens that expired at the cut or before go; one
/// expiring a millisecond later, and a used one with its agent, stay.
#[sqlx::test(migrations = false)]
async fn unused_enrollment_tokens_expired_by_the_cut_go_and_used_ones_stay(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let agent = add_agent(&db, 1).await;
    let cut = at(T0) + days(400);
    add_token(&db, 1, cut).await;
    add_token(&db, 2, cut - days(5)).await;
    let used = add_token(&db, 3, cut - days(9)).await;
    let newer = add_token(&db, 4, cut + millis(1)).await;
    let (used_text, agent_text, used_at) = (
        used.to_string(),
        agent.to_string(),
        stored(cut - days(9) - TimeDelta::minutes(1)),
    );
    sqlx::query!(
        "UPDATE enrollment_tokens SET used_at = ?, agent_id = ? WHERE id = ?",
        used_at,
        agent_text,
        used_text
    )
    .execute(db.write_pool())
    .await
    .unwrap();

    let counts = drain(&db, 5, |writes, limit| {
        Box::pin(async move {
            writes
                .delete_unused_enrollment_tokens(cut, limit)
                .await
                .unwrap()
        })
    })
    .await;

    assert_eq!(counts, [2]);
    let mut kept = vec![used.to_string(), newer.to_string()];
    kept.sort();
    assert_eq!(token_ids(&db).await, kept);
    db.close().await;
}

// --- Only its own table ----------------------------------------------------------

/// Every table's row count.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Counts {
    users: i64,
    audit_log: i64,
    sessions: i64,
    rotated_refresh_tokens: i64,
    login_attempts: i64,
    user_mfa: i64,
    recovery_codes: i64,
    invites: i64,
    mc_accounts: i64,
    account_grants: i64,
    agents: i64,
    agent_certs: i64,
    enrollment_tokens: i64,
    modes: i64,
    bots: i64,
    releasing_bots: i64,
    chat_messages: i64,
}

async fn counts(db: &Database) -> Counts {
    sqlx::query_as!(
        Counts,
        r#"SELECT
            (SELECT COUNT(*) FROM users) AS "users!: i64",
            (SELECT COUNT(*) FROM audit_log) AS "audit_log!: i64",
            (SELECT COUNT(*) FROM sessions) AS "sessions!: i64",
            (SELECT COUNT(*) FROM rotated_refresh_tokens) AS "rotated_refresh_tokens!: i64",
            (SELECT COUNT(*) FROM login_attempts) AS "login_attempts!: i64",
            (SELECT COUNT(*) FROM user_mfa) AS "user_mfa!: i64",
            (SELECT COUNT(*) FROM recovery_codes) AS "recovery_codes!: i64",
            (SELECT COUNT(*) FROM invites) AS "invites!: i64",
            (SELECT COUNT(*) FROM mc_accounts) AS "mc_accounts!: i64",
            (SELECT COUNT(*) FROM account_grants) AS "account_grants!: i64",
            (SELECT COUNT(*) FROM agents) AS "agents!: i64",
            (SELECT COUNT(*) FROM agent_certs) AS "agent_certs!: i64",
            (SELECT COUNT(*) FROM enrollment_tokens) AS "enrollment_tokens!: i64",
            (SELECT COUNT(*) FROM modes) AS "modes!: i64",
            (SELECT COUNT(*) FROM bots) AS "bots!: i64",
            (SELECT COUNT(*) FROM releasing_bots) AS "releasing_bots!: i64",
            (SELECT COUNT(*) FROM chat_messages) AS "chat_messages!: i64""#
    )
    .fetch_one(db.read_pool())
    .await
    .unwrap()
}

/// One batch of each delete over a database holding something due of every
/// kind: each changes only its own table's count, by what it deleted.
#[sqlx::test(migrations = false)]
async fn each_delete_touches_only_its_own_table(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let agent = add_agent(&db, 1).await;
    let bot = add_bot(&db, user, 1).await;
    let cut = at(T0) + days(400);
    lines(&db, bot, 3, cut).await;
    lines(&db, bot, 3, cut + days(1)).await;
    let session = add_session(&db, user, 1, cut, false).await;
    rotations(&db, session, 2).await;
    entries(&db, 2, cut).await;
    add_invite(&db, user, 1, cut).await;
    add_token(&db, 1, cut).await;
    let _ = agent;
    // The newest 2 of 6 stay: lines 1 to 4 are past the bound.
    let chat_cut = db.retention().chat_cut(bot, 2).await.unwrap().unwrap();
    let audit_cut = db.retention().audit_cut(cut).await.unwrap().unwrap();
    let before = counts(&db).await;

    let mut seen = Vec::new();
    let mut last = before.clone();
    for kind in Kind::ALL {
        let deleted = batch(&db, |writes| {
            Box::pin(async move {
                match kind {
                    Kind::Rotations => writes.delete_expired_rotations(cut, BATCH).await,
                    Kind::Sessions => writes.delete_expired_sessions(cut, BATCH).await,
                    // Lines 1 and 2: the size cut finds 3 and 4 left.
                    Kind::ChatByAge => writes.delete_chat_received_by(cut, 2).await,
                    Kind::ChatBySize => writes.delete_chat_through(bot, chat_cut, BATCH).await,
                    Kind::Audit => writes.delete_audit_through(audit_cut, BATCH).await,
                    Kind::Invites => writes.delete_unused_invites(cut, BATCH).await,
                    Kind::Tokens => writes.delete_unused_enrollment_tokens(cut, BATCH).await,
                }
                .unwrap()
            })
        })
        .await;
        let now = counts(&db).await;
        let gone = i64::try_from(deleted).unwrap();
        let mut expected = last.clone();
        match kind {
            Kind::Rotations => expected.rotated_refresh_tokens -= gone,
            Kind::Sessions => expected.sessions -= gone,
            Kind::ChatByAge | Kind::ChatBySize => expected.chat_messages -= gone,
            Kind::Audit => expected.audit_log -= gone,
            Kind::Invites => expected.invites -= gone,
            Kind::Tokens => expected.enrollment_tokens -= gone,
        }
        assert_eq!(now, expected, "{kind:?}");
        seen.push((kind, deleted));
        last = now;
    }

    assert_eq!(
        seen,
        [
            (Kind::Rotations, 2),
            (Kind::Sessions, 1),
            (Kind::ChatByAge, 2),
            (Kind::ChatBySize, 2),
            (Kind::Audit, u64::try_from(before.audit_log).unwrap()),
            (Kind::Invites, 1),
            (Kind::Tokens, 1),
        ]
    );
    db.close().await;
}

/// The kinds of delete, in the retention's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Rotations,
    Sessions,
    ChatByAge,
    ChatBySize,
    Audit,
    Invites,
    Tokens,
}

impl Kind {
    const ALL: [Self; 7] = [
        Self::Rotations,
        Self::Sessions,
        Self::ChatByAge,
        Self::ChatBySize,
        Self::Audit,
        Self::Invites,
        Self::Tokens,
    ];
}
