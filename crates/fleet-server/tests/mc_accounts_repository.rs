//! Tests for the linked Minecraft accounts' repository (Plan.md P9.1, P9.5,
//! ADR-0018): an account is found on the read pool and in a transaction, a
//! Minecraft account is linked at most once (a Microsoft account by its
//! UUID, pending ones included; an offline account by its name in any
//! case), the table refuses what its CHECKs and foreign key forbid, a
//! status is written and read with its reason, and a rename is a
//! compare-and-set on the old name. The Member quota counts an owner's
//! accounts, and a refused insert leaves its transaction usable.
//!
//! Each test gets a fresh database from `#[sqlx::test]` and opens it with
//! `Database::connect`, the production settings. Time isn't paused: sqlx's
//! worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use chrono::{DateTime, Utc};
use fleet_core::account::{AccountStatus, RefusalReason};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::authz::Role;
use fleet_core::id::{AccountId, UserId};
use fleet_core::value::{McUsername, Username};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::accounts::{AccountCredential, InsertAccountError, McAccount};
use fleet_server::ports::audit::{AuditLimit, AuditPage, NewAuditEntry};
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
/// A Minecraft profile's UUID.
const PROFILE: &str = "069a79f4-44e9-4726-a5be-fca90e38aaf5";
/// Another one.
const OTHER_PROFILE: &str = "853c80ef-3c37-49fd-aa49-938b674adae6";
/// A third one.
const THIRD_PROFILE: &str = "6f5a3c2e-8d1b-4c7a-9e2f-0a1b2c3d4e5f";
/// The length of a version 1 envelope around a 16-byte token: the shortest
/// envelope the table takes is 46 bytes, one byte of token.
const ENVELOPE_LEN: usize = 61;

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
        AuditAction::try_from("test.accounts_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// Adds user `n`, and returns its ID.
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

/// Account `n` of `owner`, named `name`, signing in by `credential`.
fn account(owner: UserId, n: u8, name: &str, credential: AccountCredential) -> McAccount {
    McAccount {
        id: AccountId::new_v7(at(T0 + i64::from(n)), [n; 10]).unwrap(),
        owner,
        name: name.parse().unwrap(),
        created_at: at(T0 + i64::from(n)),
        credential,
    }
}

/// A Microsoft account's credential for `profile`, its envelope all `byte`.
fn microsoft(profile: &str, byte: u8, status: AccountStatus) -> AccountCredential {
    AccountCredential::Microsoft {
        uuid: profile.parse().unwrap(),
        refresh_envelope: vec![byte; ENVELOPE_LEN],
        status,
    }
}

/// Inserts `account` in its own audited transaction.
async fn insert(db: &Database, account: &McAccount) -> Result<(), InsertAccountError> {
    let mut tx = db.write().await?;
    tx.accounts().insert(account).await?;
    tx.commit(setup_entry()).await?;
    Ok(())
}

async fn found(db: &Database, id: AccountId) -> Option<McAccount> {
    db.accounts().find(id).await.unwrap()
}

/// The raw `status` and `status_reason` of the account `id`.
async fn status_columns(pool: &SqlitePool, id: AccountId) -> (String, Option<String>) {
    let id = id.to_string();
    let row = sqlx::query!(
        r#"SELECT status AS "status!", status_reason FROM mc_accounts WHERE id = ?"#,
        id
    )
    .fetch_one(pool)
    .await
    .unwrap();
    (row.status, row.status_reason)
}

fn name(text: &str) -> McUsername {
    text.parse().unwrap()
}

async fn rows(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar!(r#"SELECT count(*) AS "count!: i64" FROM mc_accounts"#)
        .fetch_one(pool)
        .await
        .unwrap()
}

// --- Finding and inserting ------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_microsoft_and_an_offline_account_are_found_on_the_read_pool_and_in_a_transaction(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let online = account(
        owner,
        1,
        "AfkBot1",
        microsoft(PROFILE, 0x11, AccountStatus::Pending),
    );
    let offline = account(owner, 2, "OffBot", AccountCredential::Offline);

    insert(&db, &online).await.unwrap();
    insert(&db, &offline).await.unwrap();

    assert_eq!(found(&db, online.id).await.as_ref(), Some(&online));
    assert_eq!(found(&db, offline.id).await.as_ref(), Some(&offline));
    let mut tx = db.write().await.unwrap();
    assert_eq!(tx.accounts().find(online.id).await.unwrap(), Some(online));
    assert_eq!(tx.accounts().find(offline.id).await.unwrap(), Some(offline));
}

#[sqlx::test(migrations = false)]
async fn the_name_keeps_its_case_and_each_status_reads_back(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let statuses = [
        (OTHER_PROFILE, AccountStatus::Active),
        (PROFILE, AccountStatus::ReauthRequired),
    ];

    for (n, (profile, status)) in (1..).zip(statuses) {
        let stored = account(owner, n, "MiXeD_Case", microsoft(profile, n, status));
        insert(&db, &stored).await.unwrap();

        let read = found(&db, stored.id).await.unwrap();
        assert_eq!(read.name.as_str(), "MiXeD_Case");
        assert_eq!(read.status(), status);
    }
}

#[sqlx::test(migrations = false)]
async fn an_unknown_account_isnt_found(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = database(options).await;
    let unknown = AccountId::new_v7(at(T0), [9; 10]).unwrap();

    assert!(found(&db, unknown).await.is_none());
    let mut tx = db.write().await.unwrap();
    assert!(tx.accounts().find(unknown).await.unwrap().is_none());
}

#[sqlx::test(migrations = false)]
async fn a_linked_microsoft_account_is_refused_even_while_pending_and_nothing_is_added(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let first = add_user(&db, 1).await;
    let second = add_user(&db, 2).await;
    let pending = account(
        first,
        1,
        "AfkBot1",
        microsoft(PROFILE, 1, AccountStatus::Pending),
    );
    insert(&db, &pending).await.unwrap();

    let again = insert(
        &db,
        &account(
            second,
            2,
            "Renamed",
            microsoft(PROFILE, 2, AccountStatus::Active),
        ),
    )
    .await;

    assert!(matches!(again, Err(InsertAccountError::Taken)), "{again:?}");
    assert_eq!(rows(db.read_pool()).await, 1);
}

#[sqlx::test(migrations = false)]
async fn an_offline_name_is_refused_in_any_case(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    insert(
        &db,
        &account(owner, 1, "OffBot", AccountCredential::Offline),
    )
    .await
    .unwrap();

    let again = insert(
        &db,
        &account(owner, 2, "offbot", AccountCredential::Offline),
    )
    .await;

    assert!(matches!(again, Err(InsertAccountError::Taken)), "{again:?}");
    assert_eq!(rows(db.read_pool()).await, 1);
}

#[sqlx::test(migrations = false)]
async fn an_offline_account_may_share_a_microsoft_accounts_name(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;

    insert(
        &db,
        &account(
            owner,
            1,
            "AfkBot1",
            microsoft(PROFILE, 1, AccountStatus::Active),
        ),
    )
    .await
    .unwrap();
    insert(
        &db,
        &account(owner, 2, "afkbot1", AccountCredential::Offline),
    )
    .await
    .unwrap();

    assert_eq!(rows(db.read_pool()).await, 2);
}

#[sqlx::test(migrations = false)]
async fn an_account_of_an_unknown_owner_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let unknown = UserId::new_v7(at(T0), [9; 10]).unwrap();

    let refused = insert(
        &db,
        &account(unknown, 1, "AfkBot1", AccountCredential::Offline),
    )
    .await;

    assert!(
        matches!(
            refused,
            Err(InsertAccountError::Store(StoreError::Backend(_)))
        ),
        "{refused:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_second_account_with_the_same_id_is_a_database_error(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let stored = account(owner, 1, "OffBot", AccountCredential::Offline);
    insert(&db, &stored).await.unwrap();

    let again = insert(
        &db,
        &McAccount {
            name: "OtherBot".parse().unwrap(),
            ..stored
        },
    )
    .await;

    assert!(
        matches!(
            again,
            Err(InsertAccountError::Store(StoreError::Backend(_)))
        ),
        "{again:?}"
    );
}

// --- Counting and a refused insert (P9.4) ------------------------------------------------

#[sqlx::test(migrations = false)]
async fn the_owned_count_counts_every_status_and_kind_of_one_owner(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let other = add_user(&db, 2).await;
    let nobody = add_user(&db, 3).await;
    let owned = [
        account(
            owner,
            1,
            "Pending1",
            microsoft(PROFILE, 1, AccountStatus::Pending),
        ),
        account(
            owner,
            2,
            "Active1",
            microsoft(OTHER_PROFILE, 2, AccountStatus::Active),
        ),
        account(
            owner,
            3,
            "Reauth1",
            microsoft(THIRD_PROFILE, 3, AccountStatus::ReauthRequired),
        ),
        account(
            owner,
            4,
            "Refused1",
            microsoft(
                "0f0e0d0c-0b0a-4908-8706-050403020100",
                4,
                AccountStatus::Refused(RefusalReason::NotOwned),
            ),
        ),
        account(owner, 5, "Offline1", AccountCredential::Offline),
    ];
    for one in &owned {
        insert(&db, one).await.unwrap();
    }
    insert(
        &db,
        &account(other, 6, "Theirs", AccountCredential::Offline),
    )
    .await
    .unwrap();

    assert_eq!(db.accounts().count_owned_by(owner).await.unwrap(), 5);
    assert_eq!(db.accounts().count_owned_by(other).await.unwrap(), 1);
    assert_eq!(db.accounts().count_owned_by(nobody).await.unwrap(), 0);
    let mut tx = db.write().await.unwrap();
    assert_eq!(tx.accounts().count_owned_by(owner).await.unwrap(), 5);
    assert_eq!(tx.accounts().count_owned_by(nobody).await.unwrap(), 0);
}

#[sqlx::test(migrations = false)]
async fn the_offline_count_counts_only_offline_accounts_of_every_owner(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let first = add_user(&db, 1).await;
    let second = add_user(&db, 2).await;
    assert_eq!(db.accounts().count_offline().await.unwrap(), 0);
    insert(
        &db,
        &account(first, 1, "OffBot1", AccountCredential::Offline),
    )
    .await
    .unwrap();
    insert(
        &db,
        &account(second, 2, "OffBot2", AccountCredential::Offline),
    )
    .await
    .unwrap();
    insert(
        &db,
        &account(
            first,
            3,
            "AfkBot1",
            microsoft(PROFILE, 3, AccountStatus::Active),
        ),
    )
    .await
    .unwrap();

    let count = db.accounts().count_offline().await.unwrap();

    assert_eq!(count, 2);
}

#[sqlx::test(migrations = false)]
async fn the_count_in_a_transaction_sees_its_own_insert(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let mut tx = db.write().await.unwrap();

    tx.accounts()
        .insert(&account(owner, 1, "OffBot", AccountCredential::Offline))
        .await
        .unwrap();

    assert_eq!(tx.accounts().count_owned_by(owner).await.unwrap(), 1);
    assert_eq!(db.accounts().count_owned_by(owner).await.unwrap(), 0);
}

#[sqlx::test(migrations = false)]
async fn a_taken_insert_leaves_the_transaction_usable_for_its_entry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    // The link flow commits its Failure entry in the transaction whose
    // insert was refused (ADR-0018): SQLite backs out only that statement.
    let db = database(options).await;
    let first = add_user(&db, 1).await;
    let second = add_user(&db, 2).await;
    insert(
        &db,
        &account(
            first,
            1,
            "AfkBot1",
            microsoft(PROFILE, 1, AccountStatus::Pending),
        ),
    )
    .await
    .unwrap();
    let failure = NewAuditEntry::new(
        at(T0),
        AuditAction::try_from("test.accounts_refused").unwrap(),
        AuditOutcome::Failure,
    );

    let mut tx = db.write().await.unwrap();
    let again = tx
        .accounts()
        .insert(&account(
            second,
            2,
            "AfkBot2",
            microsoft(PROFILE, 2, AccountStatus::Pending),
        ))
        .await;
    tx.commit(failure).await.unwrap();

    assert!(matches!(again, Err(InsertAccountError::Taken)), "{again:?}");
    assert_eq!(rows(db.read_pool()).await, 1);
    let newest = db
        .audit()
        .list(AuditPage {
            before: None,
            limit: AuditLimit::new(1).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(newest[0].action.as_str(), "test.accounts_refused");
    assert_eq!(newest[0].outcome, AuditOutcome::Failure);
}

// --- Statuses and names (P9.5) ------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_refused_account_is_stored_and_read_back_with_its_reason(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let refused = account(
        owner,
        1,
        "AfkBot1",
        microsoft(
            PROFILE,
            1,
            AccountStatus::Refused(RefusalReason::XboxBanned),
        ),
    );

    insert(&db, &refused).await.unwrap();

    assert_eq!(found(&db, refused.id).await.as_ref(), Some(&refused));
    assert_eq!(
        status_columns(db.read_pool(), refused.id).await,
        ("refused".to_owned(), Some("xbox_banned".to_owned()))
    );
}

#[sqlx::test(migrations = false)]
async fn set_status_writes_the_status_and_its_reason_together(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let stored = account(
        owner,
        1,
        "AfkBot1",
        microsoft(PROFILE, 1, AccountStatus::Active),
    );
    insert(&db, &stored).await.unwrap();
    let steps = [
        (
            AccountStatus::Refused(RefusalReason::NotOwned),
            ("refused", Some("not_owned")),
        ),
        (AccountStatus::ReauthRequired, ("reauth_required", None)),
        (AccountStatus::Active, ("active", None)),
    ];

    for (status, (column, reason)) in steps {
        let mut tx = db.write().await.unwrap();
        let changed = tx.accounts().set_status(stored.id, status).await.unwrap();
        tx.commit(setup_entry()).await.unwrap();

        assert!(changed, "{status:?}");
        assert_eq!(found(&db, stored.id).await.unwrap().status(), status);
        assert_eq!(
            status_columns(db.read_pool(), stored.id).await,
            (column.to_owned(), reason.map(str::to_owned))
        );
    }
}

/// Passes against the stub (which changes nothing); shown red by dropping
/// the update's `kind = 'microsoft'` (ADR-0018, group C).
#[sqlx::test(migrations = false)]
async fn set_status_changes_no_offline_or_missing_account(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let offline = account(owner, 1, "OffBot", AccountCredential::Offline);
    insert(&db, &offline).await.unwrap();
    let missing = AccountId::new_v7(at(T0), [9; 10]).unwrap();

    let mut tx = db.write().await.unwrap();
    let offline_changed = tx
        .accounts()
        .set_status(offline.id, AccountStatus::Refused(RefusalReason::NoProfile))
        .await
        .unwrap();
    let missing_changed = tx
        .accounts()
        .set_status(missing, AccountStatus::ReauthRequired)
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert!(!offline_changed);
    assert!(!missing_changed);
    assert_eq!(
        status_columns(db.read_pool(), offline.id).await,
        ("active".to_owned(), None)
    );
    assert_eq!(found(&db, offline.id).await, Some(offline));
}

#[sqlx::test(migrations = false)]
async fn a_rename_happens_only_while_the_name_is_exactly_the_old_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let stored = account(
        owner,
        1,
        "AfkBot1",
        microsoft(PROFILE, 1, AccountStatus::Active),
    );
    insert(&db, &stored).await.unwrap();
    let missing = AccountId::new_v7(at(T0), [9; 10]).unwrap();

    let mut tx = db.write().await.unwrap();
    let other_case = tx
        .accounts()
        .rename(stored.id, &name("afkbot1"), &name("Wrong"))
        .await
        .unwrap();
    let of_missing = tx
        .accounts()
        .rename(missing, &name("AfkBot1"), &name("Wrong"))
        .await
        .unwrap();
    let renamed = tx
        .accounts()
        .rename(stored.id, &name("AfkBot1"), &name("afkBOT_2"))
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();

    assert!(!other_case);
    assert!(!of_missing);
    assert!(renamed);
    let read = found(&db, stored.id).await.unwrap();
    assert_eq!(read.name.as_str(), "afkBOT_2");
    assert_eq!(read.credential, stored.credential);
}

// --- The table's own rules --------------------------------------------------------------

/// An `mc_accounts` row as raw column values, for inserts that bypass the
/// repository.
#[derive(Clone)]
struct RawAccount {
    id: String,
    owner_user_id: String,
    kind: String,
    mc_uuid: Option<String>,
    mc_name: String,
    refresh_envelope: Option<Vec<u8>>,
    status: String,
    status_reason: Option<String>,
    created_at: String,
}

impl RawAccount {
    fn microsoft(owner: UserId, n: u8) -> Self {
        Self {
            id: AccountId::new_v7(at(T0), [n; 10]).unwrap().to_string(),
            owner_user_id: owner.to_string(),
            kind: "microsoft".to_owned(),
            mc_uuid: Some(PROFILE.to_owned()),
            mc_name: "AfkBot1".to_owned(),
            refresh_envelope: Some(vec![1; ENVELOPE_LEN]),
            status: "active".to_owned(),
            status_reason: None,
            created_at: STORED_AT.to_owned(),
        }
    }

    fn offline(owner: UserId, n: u8) -> Self {
        Self {
            kind: "offline".to_owned(),
            mc_uuid: None,
            mc_name: "OffBot".to_owned(),
            refresh_envelope: None,
            ..Self::microsoft(owner, n)
        }
    }

    async fn insert(&self, pool: &SqlitePool) -> Result<(), sqlx::Error> {
        let envelope = self.refresh_envelope.as_deref();
        sqlx::query!(
            "INSERT INTO mc_accounts
                 (id, owner_user_id, kind, mc_uuid, mc_name, refresh_envelope, status,
                  status_reason, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            self.id,
            self.owner_user_id,
            self.kind,
            self.mc_uuid,
            self.mc_name,
            envelope,
            self.status,
            self.status_reason,
            self.created_at
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
    let owner = add_user(&db, 1).await;

    RawAccount::microsoft(owner, 1)
        .insert(db.write_pool())
        .await
        .unwrap();
    RawAccount::offline(owner, 2)
        .insert(db.write_pool())
        .await
        .unwrap();
    RawAccount {
        refresh_envelope: Some(vec![1; 46]),
        mc_uuid: Some(OTHER_PROFILE.to_owned()),
        status: "pending".to_owned(),
        ..RawAccount::microsoft(owner, 3)
    }
    .insert(db.write_pool())
    .await
    .unwrap();
}

#[sqlx::test(migrations = false)]
async fn the_table_refuses_malformed_values(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let online = RawAccount::microsoft(owner, 1);
    let offline = RawAccount::offline(owner, 2);
    let cases: Vec<(&str, RawAccount)> = vec![
        (
            "a Microsoft account without a UUID",
            RawAccount {
                mc_uuid: None,
                ..online.clone()
            },
        ),
        (
            "a Microsoft account without an envelope",
            RawAccount {
                refresh_envelope: None,
                ..online.clone()
            },
        ),
        (
            "an offline account with a UUID",
            RawAccount {
                mc_uuid: Some(PROFILE.to_owned()),
                ..offline.clone()
            },
        ),
        (
            "an offline account with an envelope",
            RawAccount {
                refresh_envelope: Some(vec![1; ENVELOPE_LEN]),
                ..offline.clone()
            },
        ),
        (
            "a pending offline account",
            RawAccount {
                status: "pending".to_owned(),
                ..offline.clone()
            },
        ),
        (
            "an envelope without a token (45 bytes)",
            RawAccount {
                refresh_envelope: Some(vec![1; 45]),
                ..online.clone()
            },
        ),
        (
            "an uppercase UUID",
            RawAccount {
                mc_uuid: Some(PROFILE.to_uppercase()),
                ..online.clone()
            },
        ),
        (
            "a UUID without hyphens",
            RawAccount {
                mc_uuid: Some(PROFILE.replace('-', "")),
                ..online.clone()
            },
        ),
        (
            "an unknown kind",
            RawAccount {
                kind: "mojang".to_owned(),
                ..online.clone()
            },
        ),
        (
            "an uppercase ID",
            RawAccount {
                id: online.id.to_uppercase(),
                ..online.clone()
            },
        ),
        (
            "created_at without milliseconds",
            RawAccount {
                created_at: "2027-01-15T08:00:00Z".to_owned(),
                ..online.clone()
            },
        ),
    ];

    for (case, row) in cases {
        let result = row.insert(db.write_pool()).await;

        assert!(check_failed(&result), "{case}: {result:?}");
    }
    assert_eq!(rows(db.read_pool()).await, 0);
}

#[sqlx::test(migrations = false)]
async fn a_row_of_an_unknown_owner_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let unknown = UserId::new_v7(at(T0), [9; 10]).unwrap();

    let error = RawAccount::offline(unknown, 1)
        .insert(db.write_pool())
        .await
        .unwrap_err();

    assert!(
        error
            .as_database_error()
            .is_some_and(|error| error.message().contains("FOREIGN KEY")),
        "{error:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_stored_value_that_doesnt_read_back_is_corrupt(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let cases = [
        (
            "status",
            RawAccount {
                status: "frozen".to_owned(),
                ..RawAccount::microsoft(owner, 1)
            },
        ),
        (
            "mc_name",
            RawAccount {
                mc_name: "no spaces".to_owned(),
                ..RawAccount::offline(owner, 2)
            },
        ),
        (
            "mc_uuid",
            RawAccount {
                mc_uuid: Some("00000000-0000-0000-0000-000000000000".to_owned()),
                ..RawAccount::microsoft(owner, 3)
            },
        ),
    ];

    for (column, row) in cases {
        row.insert(db.write_pool()).await.unwrap();
        let id: AccountId = row.id.parse().unwrap();

        let read = db.accounts().find(id).await;

        assert!(
            matches!(read, Err(StoreError::Corrupt { table: "mc_accounts", column: c, .. }) if c == column),
            "{column}: {read:?}"
        );
    }
}

#[sqlx::test(migrations = false)]
async fn a_reason_that_doesnt_fit_its_status_is_corrupt(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let owner = add_user(&db, 1).await;
    let cases = [
        (
            "refused without a reason",
            RawAccount {
                status: "refused".to_owned(),
                ..RawAccount::microsoft(owner, 1)
            },
        ),
        (
            "refused with an unknown reason",
            RawAccount {
                status: "refused".to_owned(),
                status_reason: Some("banned".to_owned()),
                mc_uuid: Some(OTHER_PROFILE.to_owned()),
                ..RawAccount::microsoft(owner, 2)
            },
        ),
        (
            "active with a reason",
            RawAccount {
                status_reason: Some("not_owned".to_owned()),
                mc_uuid: Some(THIRD_PROFILE.to_owned()),
                ..RawAccount::microsoft(owner, 3)
            },
        ),
        (
            "an offline account with a reason",
            RawAccount {
                status_reason: Some("not_owned".to_owned()),
                ..RawAccount::offline(owner, 4)
            },
        ),
    ];

    for (case, row) in cases {
        row.insert(db.write_pool()).await.unwrap();
        let id: AccountId = row.id.parse().unwrap();

        let read = db.accounts().find(id).await;

        assert!(
            matches!(
                read,
                Err(StoreError::Corrupt {
                    table: "mc_accounts",
                    column: "status_reason",
                    ..
                })
            ),
            "{case}: {read:?}"
        );
    }
}
