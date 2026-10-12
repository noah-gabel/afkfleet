//! Tests for the invites' repository (Plan.md P7.1, P7.9, ADR-0016): an
//! invite is found by ID and by its code's hash, on the read pool and in a
//! transaction; it's used once, revoked once, and never both; a creator's
//! pending invites are counted and revoked together; the list pages newest
//! first with the people's names; and the table refuses what its CHECKs and
//! foreign keys forbid.
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
use fleet_core::id::{InviteId, UserId};
use fleet_core::value::Username;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::invites::{Invite, NewInvite, RevokedInvite};
use fleet_server::ports::page::{Page, PageLimit};
use fleet_server::ports::store::{Store, StoreError};
use fleet_server::ports::tokens::TokenHash;
use fleet_server::ports::users::{NewUser, PasswordHash};
use secrecy::SecretString;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// The time everything starts at, in milliseconds.
const T0: i64 = 1_800_000_000_000;
/// An invite's default lifetime, in milliseconds.
const TTL: i64 = 72 * 3_600_000;
/// A stored time.
const STORED_AT: &str = "2027-01-15T08:00:00.000Z";
/// A stored time after [`STORED_AT`].
const LATER: &str = "2027-01-18T08:00:00.000Z";

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

fn hash(byte: u8) -> TokenHash {
    TokenHash::from_bytes([byte; 32])
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
        AuditAction::try_from("test.invite_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// Adds user `n` with `role`, and returns its ID.
async fn add_user(db: &Database, n: u8, role: Role) -> UserId {
    let id = UserId::new_v7(at(T0), [n; 10]).unwrap();
    let user = NewUser {
        id,
        username: Username::try_from(format!("user{n}").as_str()).unwrap(),
        password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
        role,
        created_at: at(T0),
    };
    let mut tx = db.write().await.unwrap();
    tx.users().insert(&user).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    id
}

/// An invite `n` by `creator` for `role`, created `millis` after `T0` and
/// living [`TTL`].
fn new_invite(n: u8, creator: UserId, role: Role, millis: i64) -> NewInvite {
    NewInvite {
        id: InviteId::new_v7(at(T0 + millis), [n; 10]).unwrap(),
        code_hash: hash(n),
        role,
        created_by: creator,
        created_at: at(T0 + millis),
        expires_at: at(T0 + millis + TTL),
    }
}

/// Inserts `invite` in its own audited transaction.
async fn insert(db: &Database, invite: &NewInvite) -> Result<(), StoreError> {
    let mut tx = db.write().await?;
    tx.invites().insert(invite).await?;
    tx.commit(setup_entry()).await?;
    Ok(())
}

/// The fields of an invite, for comparisons.
type Summary = (
    InviteId,
    [u8; 32],
    Role,
    UserId,
    DateTime<Utc>,
    DateTime<Utc>,
    Option<UserId>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
);

fn summary(invite: &Invite) -> Summary {
    (
        invite.id,
        *invite.code_hash.as_bytes(),
        invite.role,
        invite.created_by,
        invite.created_at,
        invite.expires_at,
        invite.used_by,
        invite.used_at,
        invite.revoked_at,
    )
}

fn pending(invite: &NewInvite) -> Summary {
    (
        invite.id,
        *invite.code_hash.as_bytes(),
        invite.role,
        invite.created_by,
        invite.created_at,
        invite.expires_at,
        None,
        None,
        None,
    )
}

async fn found(db: &Database, id: InviteId) -> Option<Summary> {
    db.invites().find(id).await.unwrap().as_ref().map(summary)
}

/// Uses `id` for `user` at `millis` in its own transaction.
async fn use_invite(db: &Database, id: InviteId, user: UserId, millis: i64) -> bool {
    let mut tx = db.write().await.unwrap();
    let marked = tx
        .invites()
        .use_invite(id, user, at(T0 + millis))
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    marked
}

/// Revokes `id` at `millis` in its own transaction.
async fn revoke(db: &Database, id: InviteId, millis: i64) -> bool {
    let mut tx = db.write().await.unwrap();
    let revoked = tx.invites().revoke(id, at(T0 + millis)).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    revoked
}

async fn count_pending(db: &Database, creator: UserId, millis: i64) -> u32 {
    let mut tx = db.write().await.unwrap();
    tx.invites()
        .count_pending_by(creator, at(T0 + millis))
        .await
        .unwrap()
}

fn page(before: Option<InviteId>, limit: u32) -> Page<InviteId> {
    Page {
        before,
        limit: PageLimit::new(limit).unwrap(),
    }
}

#[sqlx::test(migrations = false)]
async fn an_inserted_invite_is_found_by_id_and_by_code_hash(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let invite = new_invite(7, owner, Role::Admin, 0);

    insert(&db, &invite).await.unwrap();

    assert_eq!(found(&db, invite.id).await, Some(pending(&invite)));
    let by_hash = db.invites().find_by_code_hash(&hash(7)).await.unwrap();
    assert_eq!(by_hash.as_ref().map(summary), Some(pending(&invite)));
    let mut tx = db.write().await.unwrap();
    let in_tx = tx.invites().find(invite.id).await.unwrap();
    let by_hash_in_tx = tx.invites().find_by_code_hash(&hash(7)).await.unwrap();
    assert_eq!(in_tx.as_ref().map(summary), Some(pending(&invite)));
    assert_eq!(by_hash_in_tx.as_ref().map(summary), Some(pending(&invite)));
}

#[sqlx::test(migrations = false)]
async fn an_unknown_id_or_hash_finds_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    insert(&db, &new_invite(7, owner, Role::Member, 0))
        .await
        .unwrap();
    let unknown = InviteId::new_v7(at(T0), [9; 10]).unwrap();

    assert_eq!(found(&db, unknown).await, None);
    assert!(
        db.invites()
            .find_by_code_hash(&hash(9))
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrations = false)]
async fn an_invite_by_an_unknown_creator_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let unknown = UserId::new_v7(at(T0), [9; 10]).unwrap();

    let refused = insert(&db, &new_invite(7, unknown, Role::Member, 0)).await;

    assert!(
        matches!(refused, Err(StoreError::Backend(_))),
        "{refused:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_code_hash_stored_twice_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    insert(&db, &new_invite(7, owner, Role::Member, 0))
        .await
        .unwrap();
    let same_hash = NewInvite {
        id: InviteId::new_v7(at(T0), [8; 10]).unwrap(),
        ..new_invite(7, owner, Role::Member, 0)
    };

    let refused = insert(&db, &same_hash).await;

    assert!(
        matches!(refused, Err(StoreError::Backend(_))),
        "{refused:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_pending_invite_is_used_once(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let invite = new_invite(7, owner, Role::Member, 0);
    insert(&db, &invite).await.unwrap();
    let alice = add_user(&db, 2, Role::Member).await;
    let bob = add_user(&db, 3, Role::Member).await;

    let first = use_invite(&db, invite.id, alice, 1_000).await;
    let second = use_invite(&db, invite.id, bob, 2_000).await;

    assert!(first);
    assert!(!second);
    let stored = found(&db, invite.id).await.unwrap();
    assert_eq!((stored.6, stored.7), (Some(alice), Some(at(T0 + 1_000))));
}

#[sqlx::test(migrations = false)]
async fn an_invite_cant_be_used_from_its_expiry_on(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let invite = new_invite(7, owner, Role::Member, 0);
    insert(&db, &invite).await.unwrap();
    let alice = add_user(&db, 2, Role::Member).await;

    let at_expiry = use_invite(&db, invite.id, alice, TTL).await;
    let just_before = use_invite(&db, invite.id, alice, TTL - 1).await;

    assert!(!at_expiry);
    assert!(just_before);
}

#[sqlx::test(migrations = false)]
async fn a_revoked_invite_cant_be_used(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let invite = new_invite(7, owner, Role::Member, 0);
    insert(&db, &invite).await.unwrap();
    let alice = add_user(&db, 2, Role::Member).await;
    assert!(revoke(&db, invite.id, 500).await);

    let used = use_invite(&db, invite.id, alice, 1_000).await;

    assert!(!used);
    assert_eq!(found(&db, invite.id).await.unwrap().6, None);
}

#[sqlx::test(migrations = false)]
async fn a_user_uses_at_most_one_invite(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let first = new_invite(7, owner, Role::Member, 0);
    let second = new_invite(8, owner, Role::Member, 0);
    insert(&db, &first).await.unwrap();
    insert(&db, &second).await.unwrap();
    let alice = add_user(&db, 2, Role::Member).await;
    assert!(use_invite(&db, first.id, alice, 1_000).await);

    let mut tx = db.write().await.unwrap();
    let refused = tx
        .invites()
        .use_invite(second.id, alice, at(T0 + 2_000))
        .await;

    assert!(
        matches!(refused, Err(StoreError::Backend(_))),
        "{refused:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_pending_invite_is_revoked_once(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let invite = new_invite(7, owner, Role::Member, 0);
    insert(&db, &invite).await.unwrap();

    let first = revoke(&db, invite.id, 1_000).await;
    let second = revoke(&db, invite.id, 2_000).await;

    assert!(first);
    assert!(!second);
    assert_eq!(found(&db, invite.id).await.unwrap().8, Some(at(T0 + 1_000)));
}

#[sqlx::test(migrations = false)]
async fn a_used_or_expired_invite_cant_be_revoked(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let used = new_invite(7, owner, Role::Member, 0);
    let expired = new_invite(8, owner, Role::Member, 0);
    insert(&db, &used).await.unwrap();
    insert(&db, &expired).await.unwrap();
    let alice = add_user(&db, 2, Role::Member).await;
    assert!(use_invite(&db, used.id, alice, 1_000).await);

    let revoked_used = revoke(&db, used.id, 2_000).await;
    let revoked_expired = revoke(&db, expired.id, TTL).await;

    assert!(!revoked_used);
    assert!(!revoked_expired);
    assert_eq!(found(&db, used.id).await.unwrap().8, None);
    assert_eq!(found(&db, expired.id).await.unwrap().8, None);
}

#[sqlx::test(migrations = false)]
async fn only_a_creators_pending_invites_are_counted(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let admin = add_user(&db, 2, Role::Admin).await;
    let alice = add_user(&db, 3, Role::Member).await;
    let pending_one = new_invite(10, owner, Role::Member, 0);
    let pending_two = new_invite(11, owner, Role::Admin, 5_000);
    let used = new_invite(12, owner, Role::Member, 0);
    let revoked = new_invite(13, owner, Role::Member, 0);
    let expiring = new_invite(14, owner, Role::Member, -TTL + 10_000);
    let admins = new_invite(15, admin, Role::Member, 0);
    for invite in [
        &pending_one,
        &pending_two,
        &used,
        &revoked,
        &expiring,
        &admins,
    ] {
        insert(&db, invite).await.unwrap();
    }
    assert!(use_invite(&db, used.id, alice, 1_000).await);
    assert!(revoke(&db, revoked.id, 1_000).await);

    let before_the_expiry = count_pending(&db, owner, 9_999).await;
    let at_the_expiry = count_pending(&db, owner, 10_000).await;

    assert_eq!(before_the_expiry, 3);
    assert_eq!(at_the_expiry, 2);
    assert_eq!(count_pending(&db, admin, 0).await, 1);
    assert_eq!(count_pending(&db, alice, 0).await, 0);
}

#[sqlx::test(migrations = false)]
async fn revoking_a_creators_pending_invites_leaves_every_other_alone(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let admin = add_user(&db, 2, Role::Admin).await;
    let alice = add_user(&db, 3, Role::Member).await;
    let pending_one = new_invite(10, admin, Role::Member, 0);
    let pending_two = new_invite(11, admin, Role::Member, 1_000);
    let used = new_invite(12, admin, Role::Member, 0);
    let expired = new_invite(13, admin, Role::Member, -TTL);
    let owners = new_invite(14, owner, Role::Admin, 0);
    for invite in [&pending_one, &pending_two, &used, &expired, &owners] {
        insert(&db, invite).await.unwrap();
    }
    assert!(use_invite(&db, used.id, alice, 1_000).await);

    let mut tx = db.write().await.unwrap();
    let mut revoked = tx
        .invites()
        .revoke_pending_by(admin, at(T0 + 2_000))
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();

    revoked.sort_by_key(|invite| invite.id);
    assert_eq!(
        revoked,
        [
            RevokedInvite {
                id: pending_one.id,
                role: Role::Member
            },
            RevokedInvite {
                id: pending_two.id,
                role: Role::Member
            },
        ]
    );
    assert_eq!(
        found(&db, pending_one.id).await.unwrap().8,
        Some(at(T0 + 2_000))
    );
    assert_eq!(found(&db, used.id).await.unwrap().8, None);
    assert_eq!(found(&db, expired.id).await.unwrap().8, None);
    assert_eq!(found(&db, owners.id).await.unwrap().8, None);
}

#[sqlx::test(migrations = false)]
async fn the_list_is_newest_first_with_the_peoples_names(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let admin = add_user(&db, 2, Role::Admin).await;
    let alice = add_user(&db, 3, Role::Member).await;
    let older = new_invite(10, owner, Role::Admin, 0);
    let newer = new_invite(11, admin, Role::Member, 1_000);
    insert(&db, &older).await.unwrap();
    insert(&db, &newer).await.unwrap();
    assert!(use_invite(&db, older.id, alice, 2_000).await);

    let listed = db.invites().list(page(None, 50)).await.unwrap();

    let rows: Vec<_> = listed
        .iter()
        .map(|row| {
            (
                row.invite.id,
                row.created_by_name.as_str().to_owned(),
                row.used_by_name
                    .as_ref()
                    .map(|name| name.as_str().to_owned()),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (newer.id, "user2".to_owned(), None),
            (older.id, "user1".to_owned(), Some("user3".to_owned())),
        ]
    );
    assert_eq!(summary(&listed[1].invite).6, Some(alice));
}

#[sqlx::test(migrations = false)]
async fn the_list_pages_by_id_and_fetches_one_more(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let invites: Vec<_> = (0..5_u8)
        .map(|n| new_invite(10 + n, owner, Role::Member, i64::from(n) * 1_000))
        .collect();
    for invite in &invites {
        insert(&db, invite).await.unwrap();
    }

    let first = db.invites().list(page(None, 2)).await.unwrap();
    let after = db
        .invites()
        .list(page(Some(invites[3].id), 2))
        .await
        .unwrap();
    let last = db
        .invites()
        .list(page(Some(invites[1].id), 2))
        .await
        .unwrap();

    let ids = |rows: &[fleet_server::ports::invites::ListedInvite]| -> Vec<InviteId> {
        rows.iter().map(|row| row.invite.id).collect()
    };
    assert_eq!(ids(&first), [invites[4].id, invites[3].id, invites[2].id]);
    assert_eq!(ids(&after), [invites[2].id, invites[1].id, invites[0].id]);
    assert_eq!(ids(&last), [invites[0].id]);
}

// --- What the table refuses ----------------------------------------------------------

/// An `invites` row as raw column values, for inserts that bypass the
/// repository.
#[derive(Clone)]
struct RawInvite {
    id: String,
    code_hash: Vec<u8>,
    role: String,
    created_by: String,
    created_at: String,
    expires_at: String,
    used_by: Option<String>,
    used_at: Option<String>,
    revoked_at: Option<String>,
}

impl RawInvite {
    fn valid(creator: UserId, n: u8) -> Self {
        Self {
            id: InviteId::new_v7(at(T0), [n; 10]).unwrap().to_string(),
            code_hash: vec![n; 32],
            role: "member".to_owned(),
            created_by: creator.to_string(),
            created_at: STORED_AT.to_owned(),
            expires_at: LATER.to_owned(),
            used_by: None,
            used_at: None,
            revoked_at: None,
        }
    }

    async fn insert(&self, pool: &SqlitePool) -> Result<(), sqlx::Error> {
        let code_hash = self.code_hash.as_slice();
        sqlx::query!(
            "INSERT INTO invites (id, code_hash, role, created_by, created_at, expires_at,
                                  used_by, used_at, revoked_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            self.id,
            code_hash,
            self.role,
            self.created_by,
            self.created_at,
            self.expires_at,
            self.used_by,
            self.used_at,
            self.revoked_at
        )
        .execute(pool)
        .await
        .map(drop)
    }
}

/// Whether `result` is a CHECK constraint failure.
fn check_failed(result: &Result<(), sqlx::Error>) -> bool {
    result
        .as_ref()
        .err()
        .and_then(sqlx::Error::as_database_error)
        .is_some_and(|error| error.message().starts_with("CHECK constraint failed"))
}

#[sqlx::test(migrations = false)]
async fn valid_raw_rows_are_accepted(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let alice = add_user(&db, 2, Role::Member).await;
    let pool = db.write_pool();

    RawInvite::valid(owner, 1).insert(pool).await.unwrap();
    RawInvite {
        role: "admin".to_owned(),
        used_by: Some(alice.to_string()),
        used_at: Some(LATER.to_owned()),
        ..RawInvite::valid(owner, 2)
    }
    .insert(pool)
    .await
    .unwrap();
    RawInvite {
        revoked_at: Some(LATER.to_owned()),
        ..RawInvite::valid(owner, 3)
    }
    .insert(pool)
    .await
    .unwrap();
}

#[sqlx::test(migrations = false)]
async fn the_table_refuses_malformed_values(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let valid = RawInvite::valid(owner, 1);
    let cases: Vec<(&str, RawInvite)> = vec![
        (
            "a hash one byte short",
            RawInvite {
                code_hash: vec![1; 31],
                ..valid.clone()
            },
        ),
        (
            "a hash one byte long",
            RawInvite {
                code_hash: vec![1; 33],
                ..valid.clone()
            },
        ),
        (
            "an Owner invite",
            RawInvite {
                role: "owner".to_owned(),
                ..valid.clone()
            },
        ),
        (
            "a capitalized role",
            RawInvite {
                role: "Member".to_owned(),
                ..valid.clone()
            },
        ),
        (
            "an uppercase ID",
            RawInvite {
                id: valid.id.to_uppercase(),
                ..valid.clone()
            },
        ),
        (
            "created_at without milliseconds",
            RawInvite {
                created_at: "2027-01-15T08:00:00Z".to_owned(),
                ..valid.clone()
            },
        ),
        (
            "a malformed revoked_at",
            RawInvite {
                revoked_at: Some("2027-01-18".to_owned()),
                ..valid.clone()
            },
        ),
    ];

    for (case, row) in cases {
        let result = row.insert(db.write_pool()).await;

        assert!(check_failed(&result), "{case}: {result:?}");
    }
}

#[sqlx::test(migrations = false)]
async fn a_user_and_the_time_of_the_use_come_together(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let alice = add_user(&db, 2, Role::Member).await;
    let pool = db.write_pool();

    let user_without_time = RawInvite {
        used_by: Some(alice.to_string()),
        ..RawInvite::valid(owner, 1)
    }
    .insert(pool)
    .await;
    let time_without_user = RawInvite {
        used_at: Some(LATER.to_owned()),
        ..RawInvite::valid(owner, 2)
    }
    .insert(pool)
    .await;

    assert!(check_failed(&user_without_time), "{user_without_time:?}");
    assert!(check_failed(&time_without_user), "{time_without_user:?}");
}

#[sqlx::test(migrations = false)]
async fn an_invite_is_never_both_used_and_revoked(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let alice = add_user(&db, 2, Role::Member).await;

    let both = RawInvite {
        used_by: Some(alice.to_string()),
        used_at: Some(LATER.to_owned()),
        revoked_at: Some(LATER.to_owned()),
        ..RawInvite::valid(owner, 1)
    }
    .insert(db.write_pool())
    .await;

    assert!(check_failed(&both), "{both:?}");
}

#[sqlx::test(migrations = false)]
async fn an_invite_expires_after_its_creation(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let pool = db.write_pool();

    let at_creation = RawInvite {
        expires_at: STORED_AT.to_owned(),
        ..RawInvite::valid(owner, 1)
    }
    .insert(pool)
    .await;
    let before_creation = RawInvite {
        expires_at: "2027-01-14T08:00:00.000Z".to_owned(),
        ..RawInvite::valid(owner, 2)
    }
    .insert(pool)
    .await;

    assert!(check_failed(&at_creation), "{at_creation:?}");
    assert!(check_failed(&before_creation), "{before_creation:?}");
}

#[sqlx::test(migrations = false)]
async fn rows_naming_an_unknown_user_are_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1, Role::Owner).await;
    let unknown = UserId::new_v7(at(T0), [9; 10]).unwrap().to_string();

    let unknown_creator = RawInvite {
        created_by: unknown.clone(),
        ..RawInvite::valid(owner, 1)
    }
    .insert(db.write_pool())
    .await;
    let unknown_user = RawInvite {
        used_by: Some(unknown),
        used_at: Some(LATER.to_owned()),
        ..RawInvite::valid(owner, 2)
    }
    .insert(db.write_pool())
    .await;

    for result in [unknown_creator, unknown_user] {
        let error = result.unwrap_err();
        assert!(
            error
                .as_database_error()
                .is_some_and(|error| error.message().contains("FOREIGN KEY")),
            "{error:?}"
        );
    }
}
