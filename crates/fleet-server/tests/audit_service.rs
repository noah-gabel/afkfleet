//! Tests for `AuditService`: entries stamped by the clock, recording in a
//! transaction of its own, and listing in pages (Plan.md P6.8, ADR-0015).
//! The clock is fleet-testkit's `ManualClock`; IDs are minted from
//! `SeededRandom` and compared with each other, never with fixed bytes.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::sync::Arc;

use chrono::DateTime;
use fleet_core::audit::{AuditAction, AuditMetadata, AuditOutcome, AuditTarget, RecordedMetadata};
use fleet_core::authz::Role;
use fleet_core::id::UserId;
use fleet_core::system::{Clock, mint};
use fleet_core::value::Username;
use fleet_server::app::audit::AuditService;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::audit::{AuditEntryId, AuditLimit, AuditPage};
use fleet_server::ports::store::{Store, StoreError};
use fleet_server::ports::users::{NewUser, PasswordHash};
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::SecretString;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// The service over a fresh database, its clock, and the database itself.
async fn service(
    options: SqliteConnectOptions,
    db_options: DatabaseOptions,
) -> (AuditService, ManualClock, Database) {
    let db = Database::connect(options, db_options).await.unwrap();
    let clock = ManualClock::new(DateTime::from_timestamp(1_800_000_000, 123_456_789).unwrap());
    let service = AuditService::new(Arc::new(clock.clone()), Arc::new(db.clone()));
    (service, clock, db)
}

fn action(name: &str) -> AuditAction {
    AuditAction::try_from(name).unwrap()
}

fn page(before: Option<AuditEntryId>, limit: u32) -> AuditPage {
    AuditPage {
        before,
        limit: AuditLimit::new(limit).unwrap(),
    }
}

#[sqlx::test(migrations = false)]
async fn entries_are_stamped_with_the_clock_in_milliseconds(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (service, clock, db) = service(options, DatabaseOptions::default()).await;

    service
        .record(service.entry(action("x.first"), AuditOutcome::Success))
        .await
        .unwrap();
    clock.advance(Duration::from_millis(1_500));
    service
        .record(service.entry(action("x.second"), AuditOutcome::Success))
        .await
        .unwrap();

    let times: Vec<_> = service
        .list(page(None, 10))
        .await
        .unwrap()
        .iter()
        .map(|record| record.at)
        .collect();
    assert_eq!(
        times,
        [
            DateTime::from_timestamp_millis(1_800_000_001_623).unwrap(),
            DateTime::from_timestamp_millis(1_800_000_000_123).unwrap(),
        ]
    );
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn recorded_entries_list_newest_first_with_every_field(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (service, clock, db) = service(options, DatabaseOptions::default()).await;
    let random = SeededRandom::new(42);
    let user = NewUser {
        id: mint::<UserId>(clock.now(), &random).unwrap(),
        username: Username::try_from("afkowner").unwrap(),
        password_hash: PasswordHash::new(SecretString::from("$argon2id$v=19$x")).unwrap(),
        role: Role::Owner,
        created_at: clock.now(),
    };
    let mut tx = db.write().await.unwrap();
    tx.users().insert(&user).await.unwrap();
    let created = tx
        .commit(
            service
                .entry(action("user.create"), AuditOutcome::Success)
                .actor(user.id),
        )
        .await
        .unwrap();
    let metadata = AuditMetadata::new().int("attempts", 3).unwrap();

    let denied = service
        .record(
            service
                .entry(action("user.set_role"), AuditOutcome::Denied)
                .actor(user.id)
                .ip("192.0.2.10".parse().unwrap())
                .target(AuditTarget::User(user.id))
                .metadata(metadata.clone()),
        )
        .await
        .unwrap();

    let records = service.list(page(None, 10)).await.unwrap();
    assert_eq!(
        records.iter().map(|r| r.id).collect::<Vec<_>>(),
        [denied, created]
    );
    let newest = &records[0];
    assert_eq!(newest.action, action("user.set_role"));
    assert_eq!(newest.outcome, AuditOutcome::Denied);
    assert_eq!(newest.actor, Some(user.id));
    assert_eq!(newest.ip, Some("192.0.2.10".parse().unwrap()));
    assert_eq!(newest.metadata, RecordedMetadata::from(&metadata));
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn the_same_seed_mints_the_same_ids(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let (_service, clock, db) = service(options, DatabaseOptions::default()).await;

    let first: UserId = mint(clock.now(), &SeededRandom::new(7)).unwrap();
    let second: UserId = mint(clock.now(), &SeededRandom::new(7)).unwrap();
    let other: UserId = mint(clock.now(), &SeededRandom::new(8)).unwrap();

    assert_eq!(first, second);
    assert_ne!(first, other);
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn pages_follow_the_cursor_without_overlap(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (service, _clock, db) = service(options, DatabaseOptions::default()).await;
    let mut ids = Vec::new();
    for n in 0..5 {
        let entry = service.entry(action(&format!("x.e{n}")), AuditOutcome::Success);
        ids.push(service.record(entry).await.unwrap());
    }

    let mut seen = Vec::new();
    let mut before = None;
    loop {
        let records = service.list(page(before, 2)).await.unwrap();
        assert!(records.len() <= 2);
        let Some(last) = records.last() else { break };
        before = Some(last.id);
        seen.extend(records.iter().map(|r| r.id));
    }

    ids.reverse();
    assert_eq!(seen, ids);
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn a_failure_is_recorded_after_its_change_rolled_back(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (service, clock, db) = service(options, DatabaseOptions::default()).await;
    let user = NewUser {
        id: mint::<UserId>(clock.now(), &SeededRandom::new(1)).unwrap(),
        username: Username::try_from("afkbot1").unwrap(),
        password_hash: PasswordHash::new(SecretString::from("$argon2id$v=19$x")).unwrap(),
        role: Role::Member,
        created_at: clock.now(),
    };
    let mut tx = db.write().await.unwrap();
    tx.users().insert(&user).await.unwrap();
    drop(tx);

    service
        .record(service.entry(action("user.create"), AuditOutcome::Failure))
        .await
        .unwrap();

    assert!(db.users().get(user.id).await.unwrap().is_none());
    let records = service.list(page(None, 10)).await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].outcome, AuditOutcome::Failure);
    db.close().await;
}

#[sqlx::test(migrations = false)]
async fn recording_while_another_transaction_holds_the_writer_is_busy(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (service, _clock, db) = service(
        options,
        DatabaseOptions::default().with_acquire_timeout(Duration::from_millis(50)),
    )
    .await;
    let held = db.write().await.unwrap();

    let result = service
        .record(service.entry(action("x.y"), AuditOutcome::Success))
        .await;

    assert!(matches!(result, Err(StoreError::Busy)), "{result:?}");
    drop(held);
    db.close().await;
}
