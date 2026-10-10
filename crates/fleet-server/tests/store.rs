//! Tests for the store: users, the audit log and write transactions, on a
//! fresh database per test with the production connection settings
//! (Plan.md P6.4, ADR-0015).
//!
//! `#[sqlx::test]` hands each test the connect options of its own database
//! file, and the test opens it with `Database::connect`, which applies the
//! same `connect_options` as the server (`trusted_schema=OFF`, `query_only`
//! on reads, foreign keys) and runs the migrations. Time isn't paused: sqlx's
//! worker threads would make paused timeouts fire early.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::net::IpAddr;

use chrono::{DateTime, TimeZone, Utc};
use fleet_core::audit::{
    AuditAction, AuditMetadata, AuditOutcome, AuditTarget, RecordedMetadata, RecordedTarget,
    RecordedValue, TargetKind,
};
use fleet_core::authz::Role;
use fleet_core::id::{AccountId, AgentId, BotId, ModeId, UserId};
use fleet_core::value::Username;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::audit::{AuditEntryId, AuditLimit, AuditPage, AuditRecord, NewAuditEntry};
use fleet_server::ports::store::{Store, StoreError};
use fleet_server::ports::users::{InsertUserError, NewUser, PasswordHash, User};
use secrecy::SecretString;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use uuid::Uuid;

/// The acquire timeout of the tests that wait for it. It also bounds opening
/// the database (the first connections and the migrations), so it leaves room
/// for a slow CI runner: 50 ms once timed out `open()` itself on Windows.
const SHORT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(1);

/// A valid Argon2id PHC string (not a real hash).
const HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// A stored time.
const STORED_AT: &str = "2026-10-10T12:00:00.123Z";

async fn connect(options: SqliteConnectOptions) -> Database {
    Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap()
}

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

fn user_id(n: u8) -> UserId {
    UserId::new_v7(at(1_800_000_000_000), [n; 10]).unwrap()
}

fn new_user(n: u8, name: &str, role: Role) -> NewUser {
    NewUser {
        id: user_id(n),
        username: Username::try_from(name).unwrap(),
        password_hash: PasswordHash::new(SecretString::from(HASH)).unwrap(),
        role,
        created_at: at(1_800_000_000_000 + i64::from(n)),
    }
}

fn action(name: &str) -> AuditAction {
    AuditAction::try_from(name).unwrap()
}

fn entry(name: &str) -> NewAuditEntry {
    NewAuditEntry::new(at(1_800_000_000_500), action(name), AuditOutcome::Success)
}

fn first_page() -> AuditPage {
    AuditPage {
        before: None,
        limit: AuditLimit::new(100).unwrap(),
    }
}

/// Inserts `user` in its own write transaction, audited.
async fn insert(db: &Database, user: &NewUser) -> Result<(), InsertUserError> {
    let mut tx = db.write().await?;
    tx.users().insert(user).await?;
    tx.commit(entry("user.create")).await?;
    Ok(())
}

/// The fields of a user, without the hash, for comparisons.
fn summary(
    user: &User,
) -> (
    UserId,
    String,
    String,
    Role,
    bool,
    DateTime<Utc>,
    DateTime<Utc>,
) {
    (
        user.id,
        user.username.as_str().to_owned(),
        user.password_hash.expose_secret().to_owned(),
        user.role,
        user.disabled,
        user.created_at,
        user.password_changed_at,
    )
}

/// Inserts a user row directly, bypassing the repository.
async fn raw_user(
    pool: &SqlitePool,
    id: &str,
    username: &str,
    hash: &str,
    role: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO users (id, username, password_hash, role, disabled, created_at, password_changed_at) VALUES (?, ?, ?, ?, 0, ?, ?)",
        id,
        username,
        hash,
        role,
        STORED_AT,
        STORED_AT
    )
    .execute(pool)
    .await
    .map(|_| ())
}

/// Inserts an audit row directly, bypassing the repository.
async fn raw_entry(
    pool: &SqlitePool,
    action: &str,
    target: (Option<&str>, Option<&str>),
    outcome: &str,
    metadata: Option<&str>,
) -> Result<i64, sqlx::Error> {
    let (target_type, target_id) = target;
    sqlx::query!(
        "INSERT INTO audit_log (at, actor_user_id, actor_ip, action, target_type, target_id, outcome, metadata_json) VALUES (?, NULL, NULL, ?, ?, ?, ?, ?)",
        STORED_AT,
        action,
        target_type,
        target_id,
        outcome,
        metadata
    )
    .execute(pool)
    .await
    .map(|done| done.last_insert_rowid())
}

fn assert_refused(result: &Result<impl core::fmt::Debug, sqlx::Error>) {
    assert!(
        matches!(result, Err(sqlx::Error::Database(_))),
        "expected the database to refuse it: {result:?}"
    );
}

// --- Users -------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn an_inserted_user_is_found_by_id_and_by_username(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;
    let user = new_user(1, "AfkOwner", Role::Owner);

    insert(&db, &user).await.unwrap();

    let by_id = db.users().get(user.id).await.unwrap().unwrap();
    let by_name = db
        .users()
        .find_by_username(&Username::try_from("afkowner").unwrap())
        .await
        .unwrap()
        .unwrap();
    let expected = (
        user.id,
        "afkowner".to_owned(),
        HASH.to_owned(),
        Role::Owner,
        false,
        user.created_at,
        user.created_at,
    );
    assert_eq!(summary(&by_id), expected);
    assert_eq!(summary(&by_name), expected);
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn an_unknown_user_is_none(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = connect(options).await;

    assert!(db.users().get(user_id(9)).await.unwrap().is_none());
    assert!(
        db.users()
            .find_by_username(&Username::try_from("nobody").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn a_taken_username_is_refused_and_adds_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;
    insert(&db, &new_user(1, "afkbot1", Role::Member))
        .await
        .unwrap();

    let result = insert(&db, &new_user(2, "AfkBot1", Role::Member)).await;

    assert!(
        matches!(result, Err(InsertUserError::UsernameTaken)),
        "{result:?}"
    );
    assert!(db.users().get(user_id(2)).await.unwrap().is_none());
    assert_eq!(db.audit().list(first_page()).await.unwrap().len(), 1);
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn a_second_owner_is_refused(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = connect(options).await;
    insert(&db, &new_user(1, "first", Role::Owner))
        .await
        .unwrap();

    let result = insert(&db, &new_user(2, "second", Role::Owner)).await;

    assert!(
        matches!(result, Err(InsertUserError::OwnerExists)),
        "{result:?}"
    );
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn admins_and_members_have_no_such_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;

    for (n, (name, role)) in [
        ("admin1", Role::Admin),
        ("admin2", Role::Admin),
        ("member1", Role::Member),
        ("member2", Role::Member),
    ]
    .into_iter()
    .enumerate()
    {
        insert(&db, &new_user(u8::try_from(n).unwrap(), name, role))
            .await
            .unwrap();
    }
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn a_duplicate_id_is_a_store_error(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = connect(options).await;
    insert(&db, &new_user(1, "first", Role::Member))
        .await
        .unwrap();

    let result = insert(&db, &new_user(1, "second", Role::Member)).await;

    assert!(
        matches!(result, Err(InsertUserError::Store(StoreError::Backend(_)))),
        "{result:?}"
    );
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn every_role_is_stored_and_an_unknown_one_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;
    let pool = db.write_pool();

    for (n, role) in [Role::Member, Role::Admin, Role::Owner]
        .into_iter()
        .enumerate()
    {
        let id = user_id(u8::try_from(n).unwrap()).to_string();
        raw_user(pool, &id, &format!("user{n}"), HASH, role.as_str())
            .await
            .unwrap();
    }

    assert_refused(&raw_user(pool, &user_id(9).to_string(), "user9", HASH, "superuser").await);
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn an_uppercase_username_is_refused(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = connect(options).await;

    assert_refused(
        &raw_user(
            db.write_pool(),
            &user_id(1).to_string(),
            "Alice",
            HASH,
            "member",
        )
        .await,
    );
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn only_canonical_ids_are_stored(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = connect(options).await;
    let pool = db.write_pool();
    let id = user_id(1).to_string();

    assert_refused(&raw_user(pool, &id.to_uppercase(), "a1", HASH, "member").await);
    assert_refused(
        &raw_user(
            pool,
            "------------------------------------",
            "a2",
            HASH,
            "member",
        )
        .await,
    );
    assert_refused(&raw_user(pool, &id.replace('-', "x"), "a3", HASH, "member").await);
    let blob_id = user_id(1).as_uuid().as_bytes().to_vec();
    let blob = sqlx::query!(
        "INSERT INTO users (id, username, password_hash, role, disabled, created_at, password_changed_at) VALUES (?, 'a4', ?, 'member', 0, ?, ?)",
        blob_id,
        HASH,
        STORED_AT,
        STORED_AT
    )
    .execute(pool)
    .await;
    assert_refused(&blob);
    raw_user(pool, &id, "a5", HASH, "member").await.unwrap();
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn only_phc_strings_are_stored_as_hashes(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;
    let pool = db.write_pool();

    assert_refused(
        &raw_user(
            pool,
            &user_id(1).to_string(),
            "a1",
            "correct horse",
            "member",
        )
        .await,
    );
    assert_refused(&raw_user(pool, &user_id(2).to_string(), "a2", "", "member").await);
    assert_refused(&raw_user(pool, &user_id(3).to_string(), "a3", "$Argon2$x", "member").await);
    raw_user(pool, &user_id(4).to_string(), "a4", HASH, "member")
        .await
        .unwrap();
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn a_stored_id_that_isnt_v7_is_corrupt(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;
    raw_user(
        db.write_pool(),
        "550e8400-e29b-41d4-a716-446655440000",
        "oldid",
        HASH,
        "member",
    )
    .await
    .unwrap();

    let result = db
        .users()
        .find_by_username(&Username::try_from("oldid").unwrap())
        .await;

    assert!(
        matches!(
            result,
            Err(StoreError::Corrupt {
                table: "users",
                column: "id",
                rowid: 1
            })
        ),
        "{result:?}"
    );
    db.close().await;
}

// --- Write transactions ------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_dropped_transaction_leaves_neither_the_change_nor_its_entry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;
    let user = new_user(1, "afkbot1", Role::Member);

    let mut tx = db.write().await.unwrap();
    tx.users().insert(&user).await.unwrap();
    tx.audit().record(&entry("user.create")).await.unwrap();
    drop(tx);

    assert!(db.users().get(user.id).await.unwrap().is_none());
    assert_eq!(db.audit().list(first_page()).await.unwrap(), Vec::new());
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn commit_writes_the_change_and_its_entry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;
    let user = new_user(1, "afkbot1", Role::Member);

    let mut tx = db.write().await.unwrap();
    tx.users().insert(&user).await.unwrap();
    let id = tx
        .commit(entry("user.create").actor(user.id))
        .await
        .unwrap();

    assert!(db.users().get(user.id).await.unwrap().is_some());
    let entries = db.audit().list(first_page()).await.unwrap();
    assert_eq!(entries.iter().map(|e| e.id).collect::<Vec<_>>(), [id]);
    assert_eq!(entries[0].actor, Some(user.id));
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn a_second_writer_gets_busy_while_the_first_holds_the_connection(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = Database::connect(
        options,
        DatabaseOptions::default().with_acquire_timeout(SHORT_ACQUIRE_TIMEOUT),
    )
    .await
    .unwrap();
    let held = db.write().await.unwrap();

    let result = db.write().await;

    assert!(
        matches!(result, Err(StoreError::Busy)),
        "{:?}",
        result.err()
    );
    drop(held);
    db.close().await;
}

// --- The audit log -----------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn an_entry_reads_back_with_every_field(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;
    let user = new_user(1, "afkowner", Role::Owner);
    insert(&db, &user).await.unwrap();
    let metadata = AuditMetadata::new()
        .text("reason", "expired")
        .unwrap()
        .int("attempts", 3)
        .unwrap()
        .flag("locked", true)
        .unwrap();
    let new = NewAuditEntry::new(
        at(1_800_000_001_234),
        action("auth.login"),
        AuditOutcome::Failure,
    )
    .actor(user.id)
    .ip("192.0.2.10".parse().unwrap())
    .target(AuditTarget::User(user.id))
    .metadata(metadata.clone());

    let id = db.write().await.unwrap().commit(new).await.unwrap();

    let entries = db.audit().list(first_page()).await.unwrap();
    assert_eq!(
        entries.first(),
        Some(&AuditRecord {
            id,
            at: at(1_800_000_001_234),
            actor: Some(user.id),
            ip: Some("192.0.2.10".parse().unwrap()),
            action: action("auth.login"),
            target: Some(RecordedTarget::Known(AuditTarget::User(user.id))),
            outcome: AuditOutcome::Failure,
            metadata: RecordedMetadata::from(&metadata),
        })
    );
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn every_target_reads_back(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = connect(options).await;
    let created = at(1_800_000_000_000);
    let targets = [
        AuditTarget::Account(AccountId::new_v7(created, [2; 10]).unwrap()),
        AuditTarget::Bot(BotId::new_v7(created, [3; 10]).unwrap()),
        AuditTarget::Agent(AgentId::new_v7(created, [4; 10]).unwrap()),
        AuditTarget::Mode(ModeId::new_v7(created, [5; 10]).unwrap()),
        AuditTarget::User(user_id(6)),
    ];

    for target in targets {
        db.write()
            .await
            .unwrap()
            .commit(entry("x.y").target(target))
            .await
            .unwrap();
    }

    let mut read: Vec<_> = db
        .audit()
        .list(first_page())
        .await
        .unwrap()
        .into_iter()
        .map(|record| record.target)
        .collect();
    read.reverse();
    assert_eq!(read, targets.map(|t| Some(RecordedTarget::Known(t))));
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn an_unknown_target_type_reads_back_as_unrecognized(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;
    let id = user_id(1).to_string();
    raw_entry(
        db.write_pool(),
        "x.y",
        (Some("session"), Some(&id)),
        "success",
        None,
    )
    .await
    .unwrap();

    let entries = db.audit().list(first_page()).await.unwrap();

    assert_eq!(
        entries[0].target,
        Some(RecordedTarget::Unrecognized {
            kind: TargetKind::try_from("session").unwrap(),
            id: Uuid::parse_str(&id).unwrap(),
        })
    );
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn a_one_sided_target_is_refused(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = connect(options).await;
    let pool = db.write_pool();
    let id = user_id(1).to_string();

    assert_refused(&raw_entry(pool, "x.y", (Some("user"), None), "success", None).await);
    assert_refused(&raw_entry(pool, "x.y", (None, Some(&id)), "success", None).await);
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn every_outcome_is_stored_and_an_unknown_one_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;
    let pool = db.write_pool();

    for outcome in AuditOutcome::ALL {
        raw_entry(pool, "x.y", (None, None), outcome.as_str(), None)
            .await
            .unwrap();
    }

    assert_refused(&raw_entry(pool, "x.y", (None, None), "error", None).await);
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn metadata_must_be_a_json_object(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = connect(options).await;
    let pool = db.write_pool();

    for bad in ["[1, 2]", "1", "\"text\"", "not json"] {
        assert_refused(&raw_entry(pool, "x.y", (None, None), "success", Some(bad)).await);
    }
    raw_entry(pool, "x.y", (None, None), "success", Some("{}"))
        .await
        .unwrap();
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn empty_metadata_is_stored_as_null(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let db = connect(options).await;

    let id = db
        .write()
        .await
        .unwrap()
        .commit(entry("x.y"))
        .await
        .unwrap();

    let stored = sqlx::query_scalar!("SELECT metadata_json FROM audit_log WHERE id = ?", id.get())
        .fetch_one(db.read_pool())
        .await
        .unwrap();
    assert_eq!(stored, None);
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn metadata_a_newer_server_wrote_reads_back_as_text(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;
    raw_entry(
        db.write_pool(),
        "x.y",
        (None, None),
        "success",
        Some(r#"{"list":[1,2],"big":9007199254740993,"ok":"fine"}"#),
    )
    .await
    .unwrap();

    let entries = db.audit().list(first_page()).await.unwrap();

    let mut values = entries[0].metadata.entries().to_vec();
    values.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        values,
        [
            (
                "big".to_owned(),
                RecordedValue::Unrecognized("9007199254740993".to_owned())
            ),
            (
                "list".to_owned(),
                RecordedValue::Unrecognized("[1,2]".to_owned())
            ),
            ("ok".to_owned(), RecordedValue::Text("fine".to_owned())),
        ]
    );
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn ip_addresses_are_stored_canonically(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;
    let cases: [(Option<IpAddr>, Option<&str>); 4] = [
        (Some("192.0.2.10".parse().unwrap()), Some("192.0.2.10")),
        (Some("2001:db8::1".parse().unwrap()), Some("2001:db8::1")),
        (
            Some("::ffff:192.0.2.10".parse().unwrap()),
            Some("192.0.2.10"),
        ),
        (None, None),
    ];

    for (ip, stored) in cases {
        let new = match ip {
            Some(ip) => entry("x.y").ip(ip),
            None => entry("x.y"),
        };
        let id = db.write().await.unwrap().commit(new).await.unwrap();

        let text = sqlx::query_scalar!("SELECT actor_ip FROM audit_log WHERE id = ?", id.get())
            .fetch_one(db.read_pool())
            .await
            .unwrap();
        assert_eq!(text.as_deref(), stored);
        let read = db.audit().list(first_page()).await.unwrap();
        assert_eq!(read[0].ip, stored.map(|s| s.parse().unwrap()));
    }
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn an_actor_who_doesnt_exist_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;

    let result = db
        .write()
        .await
        .unwrap()
        .commit(entry("x.y").actor(user_id(7)))
        .await;

    assert!(matches!(result, Err(StoreError::Backend(_))), "{result:?}");
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn entries_list_newest_first_in_pages(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;
    let mut ids = Vec::new();
    for n in 0..5 {
        ids.push(
            db.write()
                .await
                .unwrap()
                .commit(entry(&format!("x.e{n}")))
                .await
                .unwrap(),
        );
    }
    let page = |before: Option<AuditEntryId>| AuditPage {
        before,
        limit: AuditLimit::new(2).unwrap(),
    };

    let first: Vec<_> = db
        .audit()
        .list(page(None))
        .await
        .unwrap()
        .iter()
        .map(|e| e.id)
        .collect();
    let second: Vec<_> = db
        .audit()
        .list(page(first.last().copied()))
        .await
        .unwrap()
        .iter()
        .map(|e| e.id)
        .collect();
    let third: Vec<_> = db
        .audit()
        .list(page(second.last().copied()))
        .await
        .unwrap()
        .iter()
        .map(|e| e.id)
        .collect();

    assert_eq!(first, [ids[4], ids[3]]);
    assert_eq!(second, [ids[2], ids[1]]);
    assert_eq!(third, [ids[0]]);
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn a_row_that_doesnt_read_back_is_corrupt_with_its_id(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;
    let id = raw_entry(
        db.write_pool(),
        "Not An Action",
        (None, None),
        "success",
        None,
    )
    .await
    .unwrap();

    let result = db.audit().list(first_page()).await;

    assert!(
        matches!(result, Err(StoreError::Corrupt { table: "audit_log", column: "action", rowid }) if rowid == id),
        "{result:?}"
    );
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn a_database_errors_source_chain_reaches_sqlx(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;

    let error = db
        .write()
        .await
        .unwrap()
        .commit(entry("x.y").actor(user_id(7)))
        .await
        .unwrap_err();

    let source = std::error::Error::source(&error).unwrap();
    assert!(source.downcast_ref::<sqlx::Error>().is_some(), "{source}");
    db.close().await;
}

// --- Values that don't read back, and values that can't be stored -----------

/// A row that passes every CHECK but holds a value the Rust types refuse.
async fn corrupt_user(pool: &SqlitePool, username: &str, created_at: &str, changed_at: &str) {
    let id = user_id(1).to_string();
    sqlx::query!(
        "INSERT INTO users (id, username, password_hash, role, disabled, created_at, password_changed_at) VALUES (?, ?, ?, 'member', 0, ?, ?)",
        id,
        username,
        HASH,
        created_at,
        changed_at
    )
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test(migrations = false)]
async fn a_stored_username_the_rules_refuse_is_corrupt(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;
    corrupt_user(db.write_pool(), "two words", STORED_AT, STORED_AT).await;

    let result = db.users().get(user_id(1)).await;

    assert!(
        matches!(
            result,
            Err(StoreError::Corrupt {
                table: "users",
                column: "username",
                rowid: 1
            })
        ),
        "{result:?}"
    );
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn stored_times_that_arent_dates_are_corrupt(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;
    // The shape CHECK passes; February has no 30th.
    corrupt_user(
        db.write_pool(),
        "afkbot1",
        STORED_AT,
        "2026-02-30T12:00:00.000Z",
    )
    .await;

    let result = db.users().get(user_id(1)).await;

    assert!(
        matches!(
            result,
            Err(StoreError::Corrupt {
                table: "users",
                column: "password_changed_at",
                rowid: 1
            })
        ),
        "{result:?}"
    );
    db.close().await;
}

/// An audit row with the given columns, the rest valid.
async fn corrupt_entry(
    pool: &SqlitePool,
    at: &str,
    actor: Option<&str>,
    ip: Option<&str>,
    target: (Option<&str>, Option<&str>),
) -> i64 {
    let (target_type, target_id) = target;
    sqlx::query!(
        "INSERT INTO audit_log (at, actor_user_id, actor_ip, action, target_type, target_id, outcome) VALUES (?, ?, ?, 'x.y', ?, ?, 'success')",
        at,
        actor,
        ip,
        target_type,
        target_id
    )
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid()
}

#[sqlx::test(migrations = false)]
async fn every_audit_column_that_doesnt_read_back_names_itself(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;
    let pool = db.write_pool();
    let v4 = "550e8400-e29b-41d4-a716-446655440000";
    raw_user(pool, v4, "oldid", HASH, "member").await.unwrap();
    let v7 = user_id(3).to_string();
    let cases = [
        (
            "at",
            corrupt_entry(pool, "2026-02-30T12:00:00.000Z", None, None, (None, None)).await,
        ),
        (
            "actor_user_id",
            corrupt_entry(pool, STORED_AT, Some(v4), None, (None, None)).await,
        ),
        (
            "actor_ip",
            corrupt_entry(pool, STORED_AT, None, Some("not-an-ip"), (None, None)).await,
        ),
        (
            "target_type",
            corrupt_entry(pool, STORED_AT, None, None, (Some("Session"), Some(&v7))).await,
        ),
        (
            "target_type",
            corrupt_entry(pool, STORED_AT, None, None, (Some("user"), Some(v4))).await,
        ),
    ];

    // Each row is listed alone, newest first, so its own error shows.
    for (column, id) in cases {
        let page = AuditPage {
            before: Some(AuditEntryId::new(id + 1)),
            limit: AuditLimit::new(1).unwrap(),
        };
        let result = db.audit().list(page).await;
        assert!(
            matches!(&result, Err(StoreError::Corrupt { table: "audit_log", column: c, rowid }) if *c == column && *rowid == id),
            "{column}: {result:?}"
        );
    }
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn a_time_outside_the_years_0_to_9999_is_unstorable(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = connect(options).await;
    let far = Utc.with_ymd_and_hms(10_000, 1, 1, 0, 0, 0).unwrap();
    let mut user = new_user(1, "afkbot1", Role::Member);
    user.created_at = far;

    let mut tx = db.write().await.unwrap();
    let insert = tx.users().insert(&user).await;
    let record = tx
        .audit()
        .record(&NewAuditEntry::new(
            far,
            action("x.y"),
            AuditOutcome::Success,
        ))
        .await;
    drop(tx);

    assert!(
        matches!(
            insert,
            Err(InsertUserError::Store(StoreError::Unstorable {
                table: "users",
                column: "created_at"
            }))
        ),
        "{insert:?}"
    );
    assert!(
        matches!(
            record,
            Err(StoreError::Unstorable {
                table: "audit_log",
                column: "at"
            })
        ),
        "{record:?}"
    );
    db.close().await;
}
