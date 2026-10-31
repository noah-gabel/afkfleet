//! What the retention tests share: a database with the production
//! settings, rows of every kind the retention deletes, many of them at once
//! by recursive inserts through sqlx's macros, and one batch of a delete
//! committed as the retention commits it.

use chrono::{DateTime, TimeDelta, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::authz::Role;
use fleet_core::id::{AccountId, AgentId, BotId, EnrollmentTokenId, InviteId, SessionId, UserId};
use fleet_core::mode::ModePreset;
use fleet_core::value::{AgentName, Username};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::agents::NewAgent;
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::bots::NewBot;
use fleet_server::ports::enrollment_tokens::NewEnrollmentToken;
use fleet_server::ports::invites::NewInvite;
use fleet_server::ports::retention::RetentionBatch;
use fleet_server::ports::sessions::NewSession;
use fleet_server::ports::store::Store;
use fleet_server::ports::tokens::TokenHash;
use fleet_server::ports::users::{NewUser, PasswordHash};
use secrecy::SecretString;
use sqlx::sqlite::SqliteConnectOptions;

/// The time everything starts at, in milliseconds.
pub(crate) const T0: i64 = 1_800_000_000_000;
/// A valid Argon2id PHC string (not a real hash).
pub(crate) const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// The batches' size: the retention's own.
pub(crate) use fleet_server::app::retention::BATCH;

pub(crate) fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

/// `at`'s stored text.
pub(crate) fn stored(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

pub(crate) fn days(n: i64) -> TimeDelta {
    TimeDelta::days(n)
}

pub(crate) fn millis(n: i64) -> TimeDelta {
    TimeDelta::milliseconds(n)
}

pub(crate) async fn database(options: SqliteConnectOptions) -> Database {
    Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap()
}

/// The entry a test commits its setup with. A test-only name, never listed
/// in `audit_actions!`.
pub(crate) fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from("test.retention_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// User `n`.
pub(crate) async fn add_user(db: &Database, n: u8) -> UserId {
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
pub(crate) async fn add_agent(db: &Database, n: u8) -> AgentId {
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
pub(crate) async fn add_bot(db: &Database, owner: UserId, n: u8) -> BotId {
    add_account_bot(db, owner, n).await.1
}

/// Offline account `n` of `owner` and its bot, both IDs.
pub(crate) async fn add_account_bot(db: &Database, owner: UserId, n: u8) -> (AccountId, BotId) {
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
    (account.id, bot.id)
}

/// `count` system lines of `bot`, received at `received`, inserted at once.
pub(crate) async fn lines(db: &Database, bot: BotId, count: u32, received: DateTime<Utc>) {
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
pub(crate) async fn users_lines(
    db: &Database,
    bot: BotId,
    user: UserId,
    count: u32,
    received: DateTime<Utc>,
) {
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
pub(crate) async fn line_ids(db: &Database, bot: BotId) -> Vec<i64> {
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
pub(crate) async fn entries(db: &Database, count: u32, dated: DateTime<Utc>) {
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
pub(crate) async fn entry_ids(db: &Database) -> Vec<i64> {
    sqlx::query_scalar!(r#"SELECT id AS "id!: i64" FROM audit_log ORDER BY id"#)
        .fetch_all(db.read_pool())
        .await
        .unwrap()
}

/// Session `n` of `user`, created at `created`, revoked when asked.
pub(crate) async fn add_session(
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
pub(crate) async fn rotations(db: &Database, session: SessionId, count: u32) {
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
pub(crate) async fn rotations_of(db: &Database, session: SessionId) -> i64 {
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
pub(crate) async fn session_ids(db: &Database) -> Vec<String> {
    sqlx::query_scalar!(r#"SELECT id AS "id!" FROM sessions ORDER BY id"#)
        .fetch_all(db.read_pool())
        .await
        .unwrap()
}

/// Invite `n` by `creator`, expiring at `expires`.
pub(crate) async fn add_invite(
    db: &Database,
    creator: UserId,
    n: u8,
    expires: DateTime<Utc>,
) -> InviteId {
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
pub(crate) async fn invite_ids(db: &Database) -> Vec<String> {
    sqlx::query_scalar!(r#"SELECT id AS "id!" FROM invites ORDER BY id"#)
        .fetch_all(db.read_pool())
        .await
        .unwrap()
}

/// Enrollment token `n`, expiring at `expires`.
pub(crate) async fn add_token(db: &Database, n: u8, expires: DateTime<Utc>) -> EnrollmentTokenId {
    let created = expires - TimeDelta::minutes(15);
    let id = EnrollmentTokenId::new_v7(created, [n; 10]).unwrap();
    let mut tx = db.write().await.unwrap();
    tx.enrollment_tokens()
        .insert(&NewEnrollmentToken {
            id,
            token_hash: TokenHash::from_bytes([n; 32]),
            created_at: created,
            expires_at: expires,
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    id
}

/// The enrollment tokens' IDs.
pub(crate) async fn token_ids(db: &Database) -> Vec<String> {
    sqlx::query_scalar!(r#"SELECT id AS "id!" FROM enrollment_tokens ORDER BY id"#)
        .fetch_all(db.read_pool())
        .await
        .unwrap()
}

/// One batch of `delete`, committed as the retention commits it.
pub(crate) async fn batch<F>(db: &Database, delete: F) -> u64
where
    F: for<'a> FnOnce(
        &'a mut dyn RetentionBatch,
    ) -> core::pin::Pin<Box<dyn Future<Output = u64> + Send + 'a>>,
{
    let mut batch = db.retention_batch().await.unwrap();
    let deleted = delete(batch.as_mut()).await;
    batch.commit().await.unwrap();
    deleted
}

/// Batches of `delete` until one comes back short: each batch's count.
pub(crate) async fn drain<F>(db: &Database, limit: u32, delete: F) -> Vec<u64>
where
    F: Fn(
            &mut dyn RetentionBatch,
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

/// Deletes every chat line, so the next case starts empty.
pub(crate) async fn clear_chat(db: &Database) {
    sqlx::query!("DELETE FROM chat_messages")
        .execute(db.write_pool())
        .await
        .unwrap();
}

/// A player's line of `bot` from `sender` saying `text`, received at
/// `received`.
pub(crate) async fn said(
    db: &Database,
    bot: BotId,
    sender: &str,
    text: &str,
    received: DateTime<Utc>,
) {
    let bot = bot.to_string();
    let received = stored(received);
    sqlx::query!(
        "INSERT INTO chat_messages (bot_id, direction, kind, sender_name, text, truncated, at)
         VALUES (?, 'in', 'chat', ?, ?, 0, ?)",
        bot,
        sender,
        text,
        received
    )
    .execute(db.write_pool())
    .await
    .unwrap();
}

/// `count` system lines of `bot` saying `text` and their number, received at
/// `received`.
pub(crate) async fn lines_saying(
    db: &Database,
    bot: BotId,
    count: u32,
    received: DateTime<Utc>,
    text: &str,
) {
    let bot = bot.to_string();
    let received = stored(received);
    sqlx::query!(
        "WITH RECURSIVE n(i) AS (SELECT 1 WHERE ?1 > 0 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
         INSERT INTO chat_messages (bot_id, direction, kind, text, truncated, at)
         SELECT ?2, 'in', 'system', ?4 || ' ' || i, 0, ?3 FROM n",
        count,
        bot,
        received,
        text
    )
    .execute(db.write_pool())
    .await
    .unwrap();
}

/// `count` audit entries dated `dated` whose metadata notes `note` and their
/// number.
pub(crate) async fn entries_noting(db: &Database, count: u32, dated: DateTime<Utc>, note: &str) {
    let dated = stored(dated);
    sqlx::query!(
        "WITH RECURSIVE n(i) AS (SELECT 1 WHERE ?1 > 0 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
         INSERT INTO audit_log (at, action, outcome, metadata_json)
         SELECT ?2, 'test.old_entry', 'success', json_object('note', ?3 || ' ' || i) FROM n",
        count,
        dated,
        note
    )
    .execute(db.write_pool())
    .await
    .unwrap();
}
