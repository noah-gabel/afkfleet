//! Tests for the linked Minecraft accounts' repository (Plan.md P9.1,
//! ADR-0018): an account is found on the read pool and in a transaction, a
//! Minecraft account is linked at most once (a Microsoft account by its
//! UUID, pending ones included; an offline account by its name in any
//! case), and the table refuses what its CHECKs and foreign key forbid.
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
use fleet_core::authz::Role;
use fleet_core::id::{AccountId, UserId};
use fleet_core::value::Username;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::accounts::{AccountCredential, InsertAccountError, McAccount};
use fleet_server::ports::audit::NewAuditEntry;
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
                 (id, owner_user_id, kind, mc_uuid, mc_name, refresh_envelope, status, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            self.id,
            self.owner_user_id,
            self.kind,
            self.mc_uuid,
            self.mc_name,
            envelope,
            self.status,
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
