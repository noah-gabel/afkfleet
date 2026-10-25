//! What the scope tests of the writes that commit without an audit entry
//! share (ADR-0019, U17; ADR-0020, U20, R57): every table the migrations
//! create, one row of each table no repository writes, and a dump of every
//! row of every table.
//!
//! Every table is dumped whole with `SELECT *`, so a column added later is
//! compared too; `event_batch_scope`'s `every_table_is_dumped` fails when a
//! table is added without being dumped here. The list of tables is fixed,
//! one query each: a dump that built its SQL from the schema's names would
//! break rule 8 (never SQL from strings). It's included by `#[path]` in
//! each test binary that uses it.

use std::collections::BTreeMap;

use sqlx::SqlitePool;

/// Every table the migrations create; `event_batch_scope`'s
/// `every_table_is_dumped` keeps it whole.
pub(crate) const TABLES: [&str; 17] = [
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

/// One row of each table no repository wrote, through fixed SQL with
/// bound IDs: about users and their sign-in, then about accounts, agents
/// and bots.
pub(crate) async fn fill(pool: &SqlitePool, ids: &Ids) {
    fill_users(pool, ids).await;
    fill_fleet(pool, ids).await;
}

/// The IDs [`fill`]'s rows refer to, as stored.
pub(crate) struct Ids {
    pub(crate) owner: String,
    pub(crate) friend: String,
    pub(crate) account: String,
    pub(crate) one: String,
    pub(crate) two: String,
    pub(crate) held: String,
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
pub(crate) async fn dump(pool: &SqlitePool) -> BTreeMap<&'static str, Vec<String>> {
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
