//! Tests for the queries behind the user's own routes (Plan.md P7.6,
//! ADR-0016): the wrong 2FA codes since the previous login, the unused
//! recovery codes, a password change, the live sessions in pages, and
//! ending a session's step-up; and the link starts behind the link flows'
//! daily cap (P9.4).
//!
//! Each test gets a fresh database from `#[sqlx::test]` and opens it with
//! `Database::connect`, the production settings. Time isn't paused: sqlx's
//! worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::authz::Role;
use fleet_core::id::{SessionId, UserId};
use fleet_core::value::Username;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::audit::{FailedCodeAttempts, NewAuditEntry};
use fleet_server::ports::page::{Page, PageLimit};
use fleet_server::ports::recovery_codes::RecoveryCodeHash;
use fleet_server::ports::sessions::{LiveSince, NewSession};
use fleet_server::ports::store::Store;
use fleet_server::ports::tokens::TokenHash;
use fleet_server::ports::users::{NewUser, PasswordHash};
use secrecy::SecretString;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// Another one.
const OTHER_PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$b3RoZXJzYWx0$b3RoZXJoYXNo";
/// The time everything starts at, in milliseconds.
const T0: i64 = 1_800_000_000_000;

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

async fn database(options: SqliteConnectOptions) -> Database {
    Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap()
}

fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from("test.account_setup").unwrap(),
        AuditOutcome::Success,
    )
}

async fn add_user(db: &Database, n: u8) -> UserId {
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

/// Records an `auth.login` entry for `user` at `millis`: a success, or a
/// failure at `step`.
async fn login_entry(db: &Database, user: UserId, millis: i64, failure: Option<&str>) {
    let outcome = if failure.is_some() {
        AuditOutcome::Failure
    } else {
        AuditOutcome::Success
    };
    let mut entry = NewAuditEntry::new(at(T0 + millis), AuditAction::AUTH_LOGIN, outcome)
        .target(AuditTarget::User(user));
    if let Some(step) = failure {
        entry = entry.metadata(
            AuditMetadata::new()
                .text("step", step)
                .unwrap()
                .text("reason", "wrong_code")
                .unwrap(),
        );
    }
    db.write().await.unwrap().commit(entry).await.unwrap();
}

/// Records an `auth.login` entry of the login step `step` (a version 7
/// UUID's text) for `user` at `millis`: a success, or a wrong code.
async fn step_entry(db: &Database, user: UserId, millis: i64, step: &str, success: bool) {
    let outcome = if success {
        AuditOutcome::Success
    } else {
        AuditOutcome::Failure
    };
    let metadata = if success {
        AuditMetadata::new().text("login_step_id", step).unwrap()
    } else {
        AuditMetadata::new()
            .text("step", "code")
            .unwrap()
            .text("reason", "wrong_code")
            .unwrap()
            .text("login_step_id", step)
            .unwrap()
    };
    let entry = NewAuditEntry::new(at(T0 + millis), AuditAction::AUTH_LOGIN, outcome)
        .target(AuditTarget::User(user))
        .metadata(metadata);
    db.write().await.unwrap().commit(entry).await.unwrap();
}

async fn attempts(db: &Database, user: UserId) -> FailedCodeAttempts {
    db.audit().failed_code_attempts(user).await.unwrap()
}

#[sqlx::test(migrations = false)]
async fn a_user_without_failures_has_none(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    login_entry(&db, user, 1_000, None).await;

    assert_eq!(
        attempts(&db, user).await,
        FailedCodeAttempts {
            count: 0,
            last_at: None
        }
    );
}

#[sqlx::test(migrations = false)]
async fn the_wrong_codes_since_the_previous_login_count(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let other = add_user(&db, 2).await;
    login_entry(&db, user, 1_000, Some("code")).await;
    login_entry(&db, user, 2_000, None).await;
    login_entry(&db, user, 3_000, Some("code")).await;
    login_entry(&db, user, 4_000, Some("password")).await;
    login_entry(&db, other, 5_000, Some("code")).await;
    login_entry(&db, user, 6_000, Some("code")).await;
    login_entry(&db, user, 7_000, None).await;
    login_entry(&db, user, 8_000, Some("code")).await;

    assert_eq!(
        attempts(&db, user).await,
        FailedCodeAttempts {
            count: 3,
            last_at: Some(at(T0 + 8_000))
        }
    );
}

#[sqlx::test(migrations = false)]
async fn before_a_second_login_every_wrong_code_counts(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    login_entry(&db, user, 1_000, Some("code")).await;
    login_entry(&db, user, 2_000, Some("code")).await;
    login_entry(&db, user, 3_000, None).await;

    assert_eq!(
        attempts(&db, user).await,
        FailedCodeAttempts {
            count: 2,
            last_at: Some(at(T0 + 2_000))
        }
    );
}

#[sqlx::test(migrations = false)]
async fn the_newest_logins_own_wrong_codes_dont_count_but_every_other_does(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    const PREVIOUS: &str = "018bcfe5-6800-7aaa-8aaa-aaaaaaaaaaaa";
    const ABANDONED: &str = "018bcfe5-6800-7bbb-8bbb-bbbbbbbbbbbb";
    const NEWEST: &str = "018bcfe5-6800-7ccc-8ccc-cccccccccccc";
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    step_entry(&db, user, 1_000, PREVIOUS, true).await;
    step_entry(&db, user, 2_000, ABANDONED, false).await;
    login_entry(&db, user, 3_000, Some("code")).await;
    step_entry(&db, user, 4_000, NEWEST, false).await;
    step_entry(&db, user, 5_000, NEWEST, false).await;
    step_entry(&db, user, 6_000, NEWEST, true).await;

    assert_eq!(
        attempts(&db, user).await,
        FailedCodeAttempts {
            count: 2,
            last_at: Some(at(T0 + 3_000))
        }
    );
}

#[sqlx::test(migrations = false)]
async fn only_unused_recovery_codes_of_the_user_count(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let other = add_user(&db, 2).await;
    let hash = |byte| RecoveryCodeHash::from_bytes([byte; 32]);
    let mut tx = db.write().await.unwrap();
    tx.mfa()
        .replace_codes(user, &[hash(1), hash(2), hash(3)])
        .await
        .unwrap();
    tx.mfa().replace_codes(other, &[hash(4)]).await.unwrap();
    assert!(tx.mfa().use_code(user, &hash(2), at(T0)).await.unwrap());
    tx.commit(setup_entry()).await.unwrap();
    let none = add_user(&db, 3).await;

    assert_eq!(db.mfa().count_unused_codes(user).await.unwrap(), 2);
    assert_eq!(db.mfa().count_unused_codes(other).await.unwrap(), 1);
    assert_eq!(db.mfa().count_unused_codes(none).await.unwrap(), 0);
}

#[sqlx::test(migrations = false)]
async fn a_password_change_writes_the_hash_and_its_time(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let unknown = UserId::new_v7(at(T0), [9; 10]).unwrap();
    let hash = PasswordHash::new(SecretString::from(OTHER_PHC)).unwrap();

    let mut tx = db.write().await.unwrap();
    let changed = tx
        .users()
        .change_password(user, &hash, at(T0 + 5_000))
        .await
        .unwrap();
    let missing = tx
        .users()
        .change_password(unknown, &hash, at(T0 + 5_000))
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert!(changed);
    assert!(!missing);
    let stored = db.users().get(user).await.unwrap().unwrap();
    assert_eq!(stored.password_hash.expose_secret(), OTHER_PHC);
    assert_eq!(stored.password_changed_at, at(T0 + 5_000));
}

/// Adds a session `n` of `user` created at `millis`.
async fn add_session(db: &Database, user: UserId, n: u8, millis: i64) -> SessionId {
    let id = SessionId::new_v7(at(T0 + millis), [n; 10]).unwrap();
    let mut tx = db.write().await.unwrap();
    tx.sessions()
        .insert(&NewSession {
            id,
            user_id: user,
            access_hash: TokenHash::from_bytes([n; 32]),
            refresh_hash: TokenHash::from_bytes([n.wrapping_add(100); 32]),
            created_at: at(T0 + millis),
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    id
}

fn page(before: Option<SessionId>, limit: u32) -> Page<SessionId> {
    Page {
        before,
        limit: PageLimit::new(limit).unwrap(),
    }
}

#[sqlx::test(migrations = false)]
async fn only_live_sessions_of_the_user_are_listed_newest_first(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let other = add_user(&db, 2).await;
    let too_old = add_session(&db, user, 1, 0).await;
    let idle = add_session(&db, user, 2, 10_000).await;
    let live = add_session(&db, user, 3, 20_000).await;
    let revoked = add_session(&db, user, 4, 30_000).await;
    let newest = add_session(&db, user, 5, 40_000).await;
    add_session(&db, other, 6, 40_000).await;
    let mut tx = db.write().await.unwrap();
    assert!(
        tx.sessions()
            .revoke(revoked, at(T0 + 50_000))
            .await
            .unwrap()
    );
    tx.commit(setup_entry()).await.unwrap();
    // `too_old` was created at the absolute cutoff and `idle` refreshed at
    // the idle cutoff: both are over from their exact millisecond on.
    let cutoffs = LiveSince {
        refreshed_after: at(T0 + 10_000),
        created_after: at(T0),
    };

    let listed = db
        .sessions()
        .list_live(user, cutoffs, page(None, 50))
        .await
        .unwrap();

    let ids: Vec<SessionId> = listed.iter().map(|session| session.id).collect();
    assert_eq!(ids, [newest, live]);
    assert!(!ids.contains(&too_old) && !ids.contains(&idle));
}

#[sqlx::test(migrations = false)]
async fn live_sessions_page_by_id_and_fetch_one_more(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let mut sessions = Vec::new();
    for n in 1..=3_u8 {
        sessions.push(add_session(&db, user, n, i64::from(n) * 1_000).await);
    }
    let cutoffs = LiveSince {
        refreshed_after: at(0),
        created_after: at(0),
    };

    let first = db
        .sessions()
        .list_live(user, cutoffs, page(None, 1))
        .await
        .unwrap();
    let rest = db
        .sessions()
        .list_live(user, cutoffs, page(Some(sessions[2]), 1))
        .await
        .unwrap();

    let ids = |list: &[fleet_server::ports::sessions::Session]| -> Vec<SessionId> {
        list.iter().map(|session| session.id).collect()
    };
    assert_eq!(ids(&first), [sessions[2], sessions[1]]);
    assert_eq!(ids(&rest), [sessions[1], sessions[0]]);
}

#[sqlx::test(migrations = false)]
async fn a_cleared_step_up_is_gone(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let session = add_session(&db, user, 1, 0).await;
    let unknown = SessionId::new_v7(at(T0), [9; 10]).unwrap();
    let mut tx = db.write().await.unwrap();
    assert!(tx.sessions().stamp_step_up(session, at(T0)).await.unwrap());

    let cleared = tx.sessions().clear_step_up(session).await.unwrap();
    let missing = tx.sessions().clear_step_up(unknown).await.unwrap();
    let after = tx.sessions().find(session).await.unwrap().unwrap();

    assert!(cleared);
    assert!(!missing);
    assert_eq!(after.session.stepped_up_at, None);
}

// --- The link starts the daily cap counts (P9.4) ---------------------------

/// Records an `action` entry for `target` at `millis` with `outcome`.
async fn entry(
    db: &Database,
    action: AuditAction,
    target: UserId,
    millis: i64,
    outcome: AuditOutcome,
) {
    let entry =
        NewAuditEntry::new(at(T0 + millis), action, outcome).target(AuditTarget::User(target));
    db.write().await.unwrap().commit(entry).await.unwrap();
}

#[sqlx::test(migrations = false)]
async fn only_one_users_successes_of_one_action_after_the_time_are_listed_in_commit_order(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;
    let other = add_user(&db, 2).await;
    let start = || AuditAction::ACCOUNT_START_LINK;
    // Before and exactly at the time: not listed.
    entry(&db, start(), user, 1_000, AuditOutcome::Success).await;
    entry(&db, start(), user, 2_000, AuditOutcome::Success).await;
    // Listed, in commit order.
    entry(&db, start(), user, 5_000, AuditOutcome::Success).await;
    entry(&db, start(), user, 3_000, AuditOutcome::Success).await;
    // Another outcome, action or user: not listed.
    entry(&db, start(), user, 4_000, AuditOutcome::Failure).await;
    entry(
        &db,
        AuditAction::AUTH_LOGIN,
        user,
        4_000,
        AuditOutcome::Success,
    )
    .await;
    entry(&db, start(), other, 4_000, AuditOutcome::Success).await;
    let since = at(T0 + 2_000);

    let listed = db
        .audit()
        .user_successes_since(user, &start(), since)
        .await
        .unwrap();
    let mut tx = db.write().await.unwrap();
    let in_tx = tx
        .audit()
        .user_successes_since(user, &start(), since)
        .await
        .unwrap();

    assert_eq!(listed, [at(T0 + 5_000), at(T0 + 3_000)]);
    assert_eq!(in_tx, listed);
    assert_eq!(
        db.audit()
            .user_successes_since(other, &start(), since)
            .await
            .unwrap(),
        [at(T0 + 4_000)]
    );
}

#[sqlx::test(migrations = false)]
async fn a_user_without_starts_has_none(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let user = add_user(&db, 1).await;

    let listed = db
        .audit()
        .user_successes_since(user, &AuditAction::ACCOUNT_START_LINK, at(T0))
        .await
        .unwrap();

    assert_eq!(listed, Vec::<DateTime<Utc>>::new());
}
