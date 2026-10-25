//! Tests for the grants' repository (Plan.md P9.1, P9.6, ADR-0010,
//! ADR-0018): a grant is inserted once per account and user, found in a
//! transaction, listed with its people's names a page at a time, rewritten
//! in its one row and deleted; it goes with its account; and the table
//! refuses what its CHECKs and foreign keys forbid.
//!
//! Each test gets a fresh database from `#[sqlx::test]` and opens it with
//! `Database::connect`, the production settings. Time isn't paused: sqlx's
//! worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use chrono::{DateTime, Utc};
use fleet_core::account::AccountStatus;
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::authz::{GrantLevel, Role};
use fleet_core::id::{AccountId, UserId};
use fleet_core::value::Username;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::grants::{AccountGrant, HeldGrant, InsertGrantError};
use fleet_server::ports::page::{Page, PageLimit};
use fleet_server::ports::store::{Store, StoreError};
use fleet_server::ports::users::{NewUser, PasswordHash};
use secrecy::SecretString;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// The time everything starts at, in milliseconds.
const T0: i64 = 1_800_000_000_000;
/// A stored time.
const STORED_AT: &str = "2027-01-15T08:00:00.000Z";

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
        AuditAction::try_from("test.grants_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// Adds the Member `user{n}`, created `n` ms after T0 so later users sort
/// newer, and returns its ID.
async fn add_user(db: &Database, n: u8) -> UserId {
    let id = UserId::new_v7(at(T0 + i64::from(n)), [n; 10]).unwrap();
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

/// Adds the offline account `n` of `owner`, and returns its ID.
async fn add_account(db: &Database, owner: UserId, n: u8) -> AccountId {
    let account = McAccount {
        id: AccountId::new_v7(at(T0 + i64::from(n)), [n; 10]).unwrap(),
        owner,
        name: format!("OffBot{n}").parse().unwrap(),
        created_at: at(T0),
        credential: AccountCredential::Offline,
    };
    let mut tx = db.write().await.unwrap();
    tx.accounts().insert(&account).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();
    account.id
}

fn grant(account: AccountId, user: UserId, level: GrantLevel, by: UserId) -> AccountGrant {
    AccountGrant {
        account,
        user,
        level,
        granted_by: by,
        granted_at: at(T0 + 1000),
    }
}

/// Inserts `grant` in its own audited transaction.
async fn insert(db: &Database, grant: &AccountGrant) -> Result<(), InsertGrantError> {
    let mut tx = db.write().await?;
    tx.grants().insert(grant).await?;
    tx.commit(setup_entry()).await?;
    Ok(())
}

async fn find(db: &Database, account: AccountId, user: UserId) -> Option<AccountGrant> {
    let mut tx = db.write().await.unwrap();
    tx.grants().find(account, user).await.unwrap()
}

fn page(before: Option<UserId>, limit: u32) -> Page<UserId> {
    Page {
        before,
        limit: PageLimit::new(limit).unwrap(),
    }
}

async fn rows(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar!(r#"SELECT count(*) AS "count!: i64" FROM account_grants"#)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// An owner, their account, and two more users.
struct Cast {
    db: Database,
    owner: UserId,
    account: AccountId,
    friend: UserId,
    other: UserId,
}

async fn cast(options: SqliteConnectOptions) -> Cast {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let friend = add_user(&db, 2).await;
    let other = add_user(&db, 3).await;
    let account = add_account(&db, owner, 1).await;
    Cast {
        db,
        owner,
        account,
        friend,
        other,
    }
}

// --- Inserting, finding and listing -----------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_grant_is_found_in_a_transaction_and_listed_with_both_names(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let c = cast(options).await;
    let stored = grant(c.account, c.friend, GrantLevel::Control, c.owner);

    insert(&c.db, &stored).await.unwrap();

    assert_eq!(find(&c.db, c.account, c.friend).await, Some(stored));
    let listed = c.db.grants().list(c.account, page(None, 50)).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].grant, stored);
    assert_eq!(listed[0].user_name.as_str(), "user2");
    assert_eq!(listed[0].granted_by_name.as_str(), "user1");
}

#[sqlx::test(migrations = false)]
async fn a_missing_grant_isnt_found_and_an_unshared_account_lists_none(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let c = cast(options).await;
    let elsewhere = add_account(&c.db, c.owner, 2).await;
    let unshared = add_account(&c.db, c.owner, 3).await;
    insert(&c.db, &grant(c.account, c.other, GrantLevel::View, c.owner))
        .await
        .unwrap();
    insert(
        &c.db,
        &grant(elsewhere, c.friend, GrantLevel::View, c.owner),
    )
    .await
    .unwrap();

    assert_eq!(find(&c.db, c.account, c.friend).await, None);
    let listed = c.db.grants().list(unshared, page(None, 50)).await.unwrap();
    assert_eq!(listed, Vec::new());
}

#[sqlx::test(migrations = false)]
async fn a_second_grant_to_the_same_user_is_refused_and_adds_no_row(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let c = cast(options).await;
    insert(
        &c.db,
        &grant(c.account, c.friend, GrantLevel::View, c.owner),
    )
    .await
    .unwrap();

    let again = insert(
        &c.db,
        &grant(c.account, c.friend, GrantLevel::Manage, c.other),
    )
    .await;

    assert!(matches!(again, Err(InsertGrantError::Exists)), "{again:?}");
    assert_eq!(rows(c.db.read_pool()).await, 1);
    let kept = find(&c.db, c.account, c.friend).await.unwrap();
    assert_eq!(kept.level, GrantLevel::View);
}

#[sqlx::test(migrations = false)]
async fn a_refused_grant_leaves_the_transaction_usable_for_its_entry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let c = cast(options).await;
    insert(
        &c.db,
        &grant(c.account, c.friend, GrantLevel::View, c.owner),
    )
    .await
    .unwrap();
    let mut tx = c.db.write().await.unwrap();

    let again = tx
        .grants()
        .insert(&grant(c.account, c.friend, GrantLevel::Manage, c.other))
        .await;

    assert!(matches!(again, Err(InsertGrantError::Exists)), "{again:?}");
    tx.commit(setup_entry()).await.unwrap();
    assert_eq!(rows(c.db.read_pool()).await, 1);
}

#[sqlx::test(migrations = false)]
async fn the_list_pages_by_grantee_newest_first_and_shows_only_its_account(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let c = cast(options).await;
    let fourth = add_user(&c.db, 4).await;
    let elsewhere = add_account(&c.db, c.owner, 2).await;
    for user in [c.friend, c.other, fourth] {
        insert(&c.db, &grant(c.account, user, GrantLevel::View, c.owner))
            .await
            .unwrap();
    }
    insert(
        &c.db,
        &grant(elsewhere, c.friend, GrantLevel::Manage, c.owner),
    )
    .await
    .unwrap();

    let first = c.db.grants().list(c.account, page(None, 2)).await.unwrap();
    let rest =
        c.db.grants()
            .list(c.account, page(Some(c.other), 2))
            .await
            .unwrap();

    let users = |listed: &[fleet_server::ports::grants::ListedGrant]| {
        listed.iter().map(|g| g.grant.user).collect::<Vec<_>>()
    };
    // A repository fetches one row more than the page holds (PageLimit::fetch).
    assert_eq!(users(&first), [fourth, c.other, c.friend]);
    assert_eq!(users(&rest), [c.friend]);
    assert!(rest.iter().all(|g| g.grant.account == c.account));
}

/// The grants one user holds, newest account first, each with its
/// account's name, its owner as they are now and who set it; one row more
/// than the page; another user's grants never among them (ADR-0020, S50).
#[sqlx::test(migrations = false)]
async fn the_grants_a_user_holds_page_newest_account_first_with_owner_and_granter(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let c = cast(options).await;
    let second = add_account(&c.db, c.owner, 2).await;
    let third = add_account(&c.db, c.other, 3).await;
    for account in [c.account, second] {
        insert(&c.db, &grant(account, c.friend, GrantLevel::View, c.owner))
            .await
            .unwrap();
    }
    insert(&c.db, &grant(third, c.friend, GrantLevel::Control, c.other))
        .await
        .unwrap();
    insert(
        &c.db,
        &grant(c.account, c.other, GrantLevel::Manage, c.owner),
    )
    .await
    .unwrap();

    let first =
        c.db.grants()
            .held_by(c.friend, page_of_accounts(None, 2))
            .await
            .unwrap();
    let rest =
        c.db.grants()
            .held_by(c.friend, page_of_accounts(Some(second), 2))
            .await
            .unwrap();

    let accounts = |held: &[HeldGrant]| held.iter().map(|g| g.grant.account).collect::<Vec<_>>();
    // A repository fetches one row more than the page holds.
    assert_eq!(accounts(&first), [third, second, c.account]);
    assert_eq!(accounts(&rest), [c.account]);
    let newest = &first[0];
    assert_eq!(newest.grant.user, c.friend);
    assert_eq!(newest.grant.level, GrantLevel::Control);
    assert_eq!(newest.account_name.as_str(), "OffBot3");
    assert_eq!(newest.owner.id, c.other);
    assert_eq!(newest.owner.role, Role::Member);
    assert_eq!(newest.owner_name.as_str(), "user3");
    assert_eq!(newest.grant.granted_by, c.other);
    assert_eq!(newest.granted_by_name.as_str(), "user3");
    assert!(first.iter().all(|g| g.grant.user == c.friend));
}

fn page_of_accounts(before: Option<AccountId>, limit: u32) -> Page<AccountId> {
    Page {
        before,
        limit: PageLimit::new(limit).unwrap(),
    }
}

// --- Changing and revoking --------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn changing_a_grant_rewrites_its_one_row(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let c = cast(options).await;
    insert(
        &c.db,
        &grant(c.account, c.friend, GrantLevel::View, c.owner),
    )
    .await
    .unwrap();
    let mut tx = c.db.write().await.unwrap();

    let changed = tx
        .grants()
        .set_level(
            c.account,
            c.friend,
            GrantLevel::Manage,
            c.other,
            at(T0 + 5000),
        )
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert!(changed);
    assert_eq!(rows(c.db.read_pool()).await, 1);
    assert_eq!(
        find(&c.db, c.account, c.friend).await,
        Some(AccountGrant {
            account: c.account,
            user: c.friend,
            level: GrantLevel::Manage,
            granted_by: c.other,
            granted_at: at(T0 + 5000),
        })
    );
}

#[sqlx::test(migrations = false)]
async fn a_revoked_grant_is_gone_and_the_others_stay(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let c = cast(options).await;
    insert(
        &c.db,
        &grant(c.account, c.friend, GrantLevel::View, c.owner),
    )
    .await
    .unwrap();
    insert(&c.db, &grant(c.account, c.other, GrantLevel::View, c.owner))
        .await
        .unwrap();
    let mut tx = c.db.write().await.unwrap();

    let deleted = tx.grants().delete(c.account, c.friend).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert!(deleted);
    assert_eq!(find(&c.db, c.account, c.friend).await, None);
    assert!(find(&c.db, c.account, c.other).await.is_some());
}

#[sqlx::test(migrations = false)]
async fn changing_or_revoking_a_missing_grant_changes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let c = cast(options).await;
    insert(&c.db, &grant(c.account, c.other, GrantLevel::View, c.owner))
        .await
        .unwrap();
    let mut tx = c.db.write().await.unwrap();

    let changed = tx
        .grants()
        .set_level(c.account, c.friend, GrantLevel::Manage, c.owner, at(T0))
        .await
        .unwrap();
    let deleted = tx.grants().delete(c.account, c.friend).await.unwrap();

    assert!(!changed);
    assert!(!deleted);
    drop(tx);
    let kept = find(&c.db, c.account, c.other).await.unwrap();
    assert_eq!(kept.level, GrantLevel::View);
}

// --- The table's own rules --------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn the_grants_go_with_their_account(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let c = cast(options).await;
    let kept = add_account(&c.db, c.owner, 2).await;
    insert(
        &c.db,
        &grant(c.account, c.friend, GrantLevel::View, c.owner),
    )
    .await
    .unwrap();
    insert(&c.db, &grant(kept, c.friend, GrantLevel::View, c.owner))
        .await
        .unwrap();
    let id = c.account.to_string();

    sqlx::query!("DELETE FROM mc_accounts WHERE id = ?", id)
        .execute(c.db.write_pool())
        .await
        .unwrap();

    assert_eq!(rows(c.db.read_pool()).await, 1);
    assert!(find(&c.db, kept, c.friend).await.is_some());
}

/// An `account_grants` row as raw column values, for inserts that bypass
/// the repository.
#[derive(Clone)]
struct RawGrant {
    account_id: String,
    user_id: String,
    level: String,
    granted_by: String,
    granted_at: String,
}

impl RawGrant {
    fn new(account: AccountId, user: UserId, by: UserId) -> Self {
        Self {
            account_id: account.to_string(),
            user_id: user.to_string(),
            level: "view".to_owned(),
            granted_by: by.to_string(),
            granted_at: STORED_AT.to_owned(),
        }
    }

    async fn insert(&self, pool: &SqlitePool) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "INSERT INTO account_grants (account_id, user_id, level, granted_by, granted_at)
             VALUES (?, ?, ?, ?, ?)",
            self.account_id,
            self.user_id,
            self.level,
            self.granted_by,
            self.granted_at
        )
        .execute(pool)
        .await
        .map(drop)
    }
}

/// Whether `result` failed with a database error whose message starts with
/// `prefix`.
fn failed_with(result: &Result<(), sqlx::Error>, prefix: &str) -> bool {
    result
        .as_ref()
        .err()
        .and_then(sqlx::Error::as_database_error)
        .is_some_and(|error| error.message().starts_with(prefix))
}

#[sqlx::test(migrations = false)]
async fn a_valid_raw_row_is_accepted(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let c = cast(options).await;

    for (n, level) in ["view", "control", "manage"].into_iter().enumerate() {
        let user = add_user(&c.db, 10 + u8::try_from(n).unwrap()).await;
        RawGrant {
            level: level.to_owned(),
            ..RawGrant::new(c.account, user, c.owner)
        }
        .insert(c.db.write_pool())
        .await
        .unwrap();
    }
}

#[sqlx::test(migrations = false)]
async fn the_table_refuses_malformed_values(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let c = cast(options).await;
    let valid = RawGrant::new(c.account, c.friend, c.owner);
    let cases: Vec<(&str, RawGrant)> = vec![
        (
            "an unknown level",
            RawGrant {
                level: "owner".to_owned(),
                ..valid.clone()
            },
        ),
        (
            "a level in capitals",
            RawGrant {
                level: "Manage".to_owned(),
                ..valid.clone()
            },
        ),
        (
            "a grant to its own granter",
            RawGrant {
                granted_by: c.friend.to_string(),
                ..valid.clone()
            },
        ),
        (
            "an uppercase account ID",
            RawGrant {
                account_id: valid.account_id.to_uppercase(),
                ..valid.clone()
            },
        ),
        (
            "granted_at without milliseconds",
            RawGrant {
                granted_at: "2027-01-15T08:00:00Z".to_owned(),
                ..valid.clone()
            },
        ),
    ];

    for (case, row) in cases {
        let result = row.insert(c.db.write_pool()).await;

        assert!(
            failed_with(&result, "CHECK constraint failed"),
            "{case}: {result:?}"
        );
    }
    assert_eq!(rows(c.db.read_pool()).await, 0);
}

#[sqlx::test(migrations = false)]
async fn a_grant_on_an_unknown_account_or_to_an_unknown_user_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let c = cast(options).await;
    let unknown_user = UserId::new_v7(at(T0), [99; 10]).unwrap();
    let unknown_account = AccountId::new_v7(at(T0), [99; 10]).unwrap();

    let cases = [
        RawGrant::new(unknown_account, c.friend, c.owner),
        RawGrant::new(c.account, unknown_user, c.owner),
        RawGrant::new(c.account, c.friend, unknown_user),
    ];
    for row in cases {
        let result = row.insert(c.db.write_pool()).await;

        assert!(failed_with(&result, "FOREIGN KEY"), "{result:?}");
    }
    assert_eq!(rows(c.db.read_pool()).await, 0);
}

#[sqlx::test(migrations = false)]
async fn a_stored_time_that_doesnt_read_back_is_corrupt(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let c = cast(options).await;
    // The CHECK pins the shape, not the calendar.
    RawGrant {
        granted_at: "2027-13-45T08:00:00.000Z".to_owned(),
        ..RawGrant::new(c.account, c.friend, c.owner)
    }
    .insert(c.db.write_pool())
    .await
    .unwrap();

    let listed = c.db.grants().list(c.account, page(None, 50)).await;
    let mut tx = c.db.write().await.unwrap();
    let found = tx.grants().find(c.account, c.friend).await;

    for read in [listed.map(drop), found.map(drop)] {
        assert!(
            matches!(
                read,
                Err(StoreError::Corrupt {
                    table: "account_grants",
                    column: "granted_at",
                    ..
                })
            ),
            "{read:?}"
        );
    }
}

#[sqlx::test(migrations = false)]
async fn a_grant_on_a_pending_or_refused_account_is_stored_like_any_other(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    // The repository knows nothing of statuses; the service decides (group E).
    let c = cast(options).await;
    let pending = McAccount {
        id: AccountId::new_v7(at(T0 + 9), [9; 10]).unwrap(),
        owner: c.owner,
        name: "AfkBot1".parse().unwrap(),
        created_at: at(T0),
        credential: AccountCredential::Microsoft {
            uuid: "069a79f4-44e9-4726-a5be-fca90e38aaf5".parse().unwrap(),
            refresh_envelope: vec![1; 61],
            status: AccountStatus::Pending,
        },
    };
    let mut tx = c.db.write().await.unwrap();
    tx.accounts().insert(&pending).await.unwrap();
    tx.commit(setup_entry()).await.unwrap();

    insert(
        &c.db,
        &grant(pending.id, c.friend, GrantLevel::View, c.owner),
    )
    .await
    .unwrap();

    assert!(find(&c.db, pending.id, c.friend).await.is_some());
}

#[sqlx::test(migrations = false)]
async fn a_time_past_the_year_9999_cant_be_stored(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let c = cast(options).await;
    insert(&c.db, &grant(c.account, c.other, GrantLevel::View, c.owner))
        .await
        .unwrap();
    let far = DateTime::from_timestamp(253_402_300_800, 0).unwrap();
    let mut tx = c.db.write().await.unwrap();

    let inserted = tx
        .grants()
        .insert(&AccountGrant {
            granted_at: far,
            ..grant(c.account, c.friend, GrantLevel::View, c.owner)
        })
        .await;
    let changed = tx
        .grants()
        .set_level(c.account, c.other, GrantLevel::Manage, c.owner, far)
        .await;

    let unstorable = |error: &StoreError| {
        matches!(
            error,
            StoreError::Unstorable {
                table: "account_grants",
                column: "granted_at"
            }
        )
    };
    assert!(
        matches!(&inserted, Err(InsertGrantError::Store(error)) if unstorable(error)),
        "{inserted:?}"
    );
    assert!(
        matches!(&changed, Err(error) if unstorable(error)),
        "{changed:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_grantees_name_that_doesnt_read_back_is_corrupt(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let c = cast(options).await;
    insert(
        &c.db,
        &grant(c.account, c.friend, GrantLevel::View, c.owner),
    )
    .await
    .unwrap();
    let friend = c.friend.to_string();
    // Lowercase, so the table takes it, but not a valid Username.
    sqlx::query!(
        "UPDATE users SET username = 'no spaces' WHERE id = ?",
        friend
    )
    .execute(c.db.write_pool())
    .await
    .unwrap();

    let listed = c.db.grants().list(c.account, page(None, 50)).await;

    assert!(
        matches!(
            listed,
            Err(StoreError::Corrupt {
                table: "users",
                column: "username",
                ..
            })
        ),
        "{listed:?}"
    );
}
