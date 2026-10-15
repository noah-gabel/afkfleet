//! Tests for the users' repository as the admins use it (Plan.md P7.9,
//! ADR-0016): the list, newest first in pages, with each user's 2FA state
//! and last use; one user as the list shows them; and the role and
//! `disabled` writes.
//!
//! Each test gets a fresh database from `#[sqlx::test]` and opens it with
//! `Database::connect`, the production settings. Time isn't paused: sqlx's
//! worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::authz::Role;
use fleet_core::id::{SessionId, UserId};
use fleet_core::value::Username;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::mfa::UserMfa;
use fleet_server::ports::page::{Page, PageLimit};
use fleet_server::ports::sessions::NewSession;
use fleet_server::ports::store::{Store, StoreError};
use fleet_server::ports::tokens::TokenHash;
use fleet_server::ports::users::{ListedUser, NewUser, PasswordHash};
use secrecy::SecretString;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
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

/// The entry a test commits its setup with. A test-only name, never listed
/// in `audit_actions!`.
fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from("test.users_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// Adds user `n` with `role`, created `n` seconds after `T0`.
async fn add_user(db: &Database, n: u8, role: Role) -> UserId {
    let created = at(T0 + i64::from(n) * 1_000);
    let id = UserId::new_v7(created, [n; 10]).unwrap();
    let user = NewUser {
        id,
        username: Username::try_from(format!("user{n}").as_str()).unwrap(),
        password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
        role,
        created_at: created,
    };
    let mut tx = db.write().await.unwrap();
    tx.users().insert(&user).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    id
}

/// Adds a session of `user` created at `millis`, its `last_used_at` too.
async fn add_session(db: &Database, user: UserId, n: u8, millis: i64) {
    let session = NewSession {
        id: SessionId::new_v7(at(millis), [n; 10]).unwrap(),
        user_id: user,
        access_hash: TokenHash::from_bytes([n; 32]),
        refresh_hash: TokenHash::from_bytes([n.wrapping_add(100); 32]),
        created_at: at(millis),
    };
    let mut tx = db.write().await.unwrap();
    tx.sessions().insert(&session).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
}

/// Gives `user` a confirmed authenticator.
async fn enroll(db: &Database, user: UserId) {
    let mfa = UserMfa {
        user_id: user,
        secret_envelope: vec![1; 65],
        last_used_step: 0,
        confirmed_at: at(T0),
    };
    let mut tx = db.write().await.unwrap();
    tx.mfa().insert(&mfa).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
}

fn page(before: Option<UserId>, limit: u32) -> Page<UserId> {
    Page {
        before,
        limit: PageLimit::new(limit).unwrap(),
    }
}

fn ids(users: &[ListedUser]) -> Vec<UserId> {
    users.iter().map(|user| user.id).collect()
}

#[sqlx::test(migrations = false)]
async fn the_list_shows_each_users_facts_newest_first(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let alice = add_user(&db, 2, Role::Member).await;
    enroll(&db, owner).await;
    add_session(&db, owner, 1, T0 + 10_000).await;
    add_session(&db, owner, 2, T0 + 20_000).await;

    let listed = db.users().list(page(None, 50)).await.unwrap();

    assert_eq!(
        listed,
        [
            ListedUser {
                id: alice,
                username: Username::try_from("user2").unwrap(),
                role: Role::Member,
                disabled: false,
                created_at: at(T0 + 2_000),
                mfa_enrolled: false,
                last_seen_at: None,
            },
            ListedUser {
                id: owner,
                username: Username::try_from("user1").unwrap(),
                role: Role::Owner,
                disabled: false,
                created_at: at(T0 + 1_000),
                mfa_enrolled: true,
                last_seen_at: Some(at(T0 + 20_000)),
            },
        ]
    );
}

#[sqlx::test(migrations = false)]
async fn the_list_pages_by_id_and_fetches_one_more(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let mut users = Vec::new();
    for n in 1..=5 {
        users.push(add_user(&db, n, Role::Member).await);
    }

    let first = db.users().list(page(None, 2)).await.unwrap();
    let after = db.users().list(page(Some(users[3]), 2)).await.unwrap();
    let last = db.users().list(page(Some(users[1]), 2)).await.unwrap();

    assert_eq!(ids(&first), [users[4], users[3], users[2]]);
    assert_eq!(ids(&after), [users[2], users[1], users[0]]);
    assert_eq!(ids(&last), [users[0]]);
}

#[sqlx::test(migrations = false)]
async fn one_user_reads_as_the_list_shows_them(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let alice = add_user(&db, 2, Role::Admin).await;
    enroll(&db, alice).await;
    add_session(&db, alice, 1, T0 + 30_000).await;
    let unknown = UserId::new_v7(at(T0), [9; 10]).unwrap();

    let listed = db.users().listed(alice).await.unwrap().unwrap();

    assert_eq!(listed.role, Role::Admin);
    assert!(listed.mfa_enrolled);
    assert_eq!(listed.last_seen_at, Some(at(T0 + 30_000)));
    assert_eq!(db.users().listed(unknown).await.unwrap(), None);
}

#[sqlx::test(migrations = false)]
async fn a_user_is_found_by_username_in_a_transaction(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    add_user(&db, 1, Role::Owner).await;
    let bob = add_user(&db, 2, Role::Member).await;
    let name = |text: &str| Username::try_from(text).unwrap();

    let mut tx = db.write().await.unwrap();
    let found = tx.users().find_by_username(&name("user2")).await.unwrap();
    let in_capitals = tx.users().find_by_username(&name("USER2")).await.unwrap();
    let unknown = tx.users().find_by_username(&name("nobody")).await.unwrap();

    let found = found.unwrap();
    assert_eq!(found.id, bob);
    assert_eq!(found.role, Role::Member);
    assert_eq!(in_capitals.map(|user| user.id), Some(bob));
    assert!(unknown.is_none());
}

#[sqlx::test(migrations = false)]
async fn a_role_is_changed_and_read_back(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let alice = add_user(&db, 2, Role::Member).await;
    let unknown = UserId::new_v7(at(T0), [9; 10]).unwrap();

    let mut tx = db.write().await.unwrap();
    let changed = tx.users().set_role(alice, Role::Admin).await.unwrap();
    let missing = tx.users().set_role(unknown, Role::Admin).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert!(changed);
    assert!(!missing);
    assert_eq!(
        db.users().get(alice).await.unwrap().unwrap().role,
        Role::Admin
    );
}

#[sqlx::test(migrations = false)]
async fn a_second_owner_is_refused_by_the_index(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    add_user(&db, 1, Role::Owner).await;
    let alice = add_user(&db, 2, Role::Member).await;

    let mut tx = db.write().await.unwrap();
    let refused = tx.users().set_role(alice, Role::Owner).await;

    assert!(
        matches!(refused, Err(StoreError::Backend(_))),
        "{refused:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_user_is_disabled_and_enabled(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let alice = add_user(&db, 2, Role::Member).await;
    let unknown = UserId::new_v7(at(T0), [9; 10]).unwrap();

    let mut tx = db.write().await.unwrap();
    let disabled = tx.users().set_disabled(alice, true).await.unwrap();
    let missing = tx.users().set_disabled(unknown, true).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    let was_disabled = db.users().get(alice).await.unwrap().unwrap().disabled;
    let mut tx = db.write().await.unwrap();
    let enabled = tx.users().set_disabled(alice, false).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert!(disabled);
    assert!(!missing);
    assert!(was_disabled);
    assert!(enabled);
    assert!(!db.users().get(alice).await.unwrap().unwrap().disabled);
}
