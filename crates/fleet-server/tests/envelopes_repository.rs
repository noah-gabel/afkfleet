//! Tests for the stored vault envelopes (Plan.md P9.2, ADR-0018): every
//! envelope is listed with its row, a swap replaces an envelope only while
//! the row still holds the one that was read, and the count of envelopes
//! under other keys covers both tables, malformed ones included.
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
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::envelopes::{SealedEnvelope, SealedRecord};
use fleet_server::ports::mfa::UserMfa;
use fleet_server::ports::store::Store;
use fleet_server::ports::users::{NewUser, PasswordHash};
use fleet_server::ports::vault::header_of;
use secrecy::SecretString;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// The time everything starts at, in milliseconds.
const T0: i64 = 1_800_000_000_000;
/// A version 1 envelope's length for a 20-byte TOTP secret.
const TOTP_ENVELOPE_LEN: usize = 65;

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
        AuditAction::try_from("test.envelopes_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// An envelope of `len` bytes under `key_id`, its body all `byte`.
fn envelope(key_id: u32, byte: u8, len: usize) -> Vec<u8> {
    let mut bytes = header_of(key_id).to_vec();
    bytes.resize(len, byte);
    bytes
}

/// Adds user `n` with a TOTP secret sealed under `key_id`.
async fn user_with_totp(db: &Database, n: u8, key_id: u32) -> (UserId, Vec<u8>) {
    let id = UserId::new_v7(at(T0), [n; 10]).unwrap();
    let sealed = envelope(key_id, n, TOTP_ENVELOPE_LEN);
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
    tx.mfa()
        .insert(&UserMfa {
            user_id: id,
            secret_envelope: sealed.clone(),
            last_used_step: 0,
            confirmed_at: at(T0),
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    (id, sealed)
}

/// Adds account `n` of `owner`: a Microsoft account whose refresh token is
/// `sealed`, or an offline account when there's none.
async fn add_account(db: &Database, owner: UserId, n: u8, sealed: Option<Vec<u8>>) -> AccountId {
    let id = AccountId::new_v7(at(T0), [n; 10]).unwrap();
    let credential = match sealed {
        Some(refresh_envelope) => AccountCredential::Microsoft {
            uuid: format!("069a79f4-44e9-4726-a5be-fca90e38aa{n:02x}")
                .parse()
                .unwrap(),
            refresh_envelope,
            status: AccountStatus::Active,
        },
        None => AccountCredential::Offline,
    };
    let mut tx = db.write().await.unwrap();
    tx.accounts()
        .insert(&McAccount {
            id,
            owner,
            name: format!("Bot{n}").parse().unwrap(),
            created_at: at(T0),
            credential,
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    id
}

/// Every stored envelope, ordered by row.
async fn all(db: &Database) -> Vec<SealedEnvelope> {
    let mut all = db.envelopes().all().await.unwrap();
    all.sort_by_key(|sealed| sealed.record);
    all
}

/// Runs `swap` in its own transaction, committed only when it changed a row.
async fn swap(db: &Database, record: SealedRecord, old: &[u8], new: &[u8]) -> bool {
    let mut tx = db.write().await.unwrap();
    let swapped = tx.envelopes().swap(record, old, new).await.unwrap();
    if swapped {
        tx.commit(setup_entry()).await.unwrap();
    }
    swapped
}

async fn count_not_under(db: &Database, key_id: u32) -> u64 {
    let mut tx = db.write().await.unwrap();
    tx.envelopes().count_not_under(key_id).await.unwrap()
}

#[sqlx::test(migrations = false)]
async fn every_envelope_is_listed_with_its_row_and_offline_accounts_hold_none(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (first, first_totp) = user_with_totp(&db, 1, 1).await;
    let (second, second_totp) = user_with_totp(&db, 2, 2).await;
    let refresh = envelope(2, 0x33, 80);
    let online = add_account(&db, first, 3, Some(refresh.clone())).await;
    add_account(&db, second, 4, None).await;

    let listed = all(&db).await;

    let mut expected = vec![
        SealedEnvelope {
            record: SealedRecord::TotpSecret(first),
            envelope: first_totp,
        },
        SealedEnvelope {
            record: SealedRecord::TotpSecret(second),
            envelope: second_totp,
        },
        SealedEnvelope {
            record: SealedRecord::RefreshToken(online),
            envelope: refresh,
        },
    ];
    expected.sort_by_key(|sealed| sealed.record);
    assert_eq!(listed, expected);
}

#[sqlx::test(migrations = false)]
async fn nothing_is_listed_without_secrets(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;

    assert_eq!(all(&db).await, Vec::new());
}

#[sqlx::test(migrations = false)]
async fn a_swap_replaces_an_envelope_only_while_the_row_holds_the_old_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (user, totp) = user_with_totp(&db, 1, 1).await;
    let refresh = envelope(1, 0x33, 80);
    let account = add_account(&db, user, 2, Some(refresh.clone())).await;
    let new_totp = envelope(2, 0x44, TOTP_ENVELOPE_LEN);
    let new_refresh = envelope(2, 0x55, 80);
    let totp_record = SealedRecord::TotpSecret(user);
    let refresh_record = SealedRecord::RefreshToken(account);

    let stale_totp = swap(
        &db,
        totp_record,
        &new_refresh[..TOTP_ENVELOPE_LEN],
        &new_totp,
    )
    .await;
    let stale_refresh = swap(&db, refresh_record, &new_refresh, &new_refresh).await;
    let unchanged = all(&db).await;
    let swapped_totp = swap(&db, totp_record, &totp, &new_totp).await;
    let swapped_refresh = swap(&db, refresh_record, &refresh, &new_refresh).await;

    assert!(!stale_totp);
    assert!(!stale_refresh);
    assert_eq!(
        unchanged
            .iter()
            .map(|sealed| &sealed.envelope)
            .collect::<Vec<_>>(),
        [&totp, &refresh]
    );
    assert!(swapped_totp);
    assert!(swapped_refresh);
    assert_eq!(
        all(&db)
            .await
            .into_iter()
            .map(|sealed| sealed.envelope)
            .collect::<Vec<_>>(),
        [new_totp, new_refresh]
    );
}

#[sqlx::test(migrations = false)]
async fn a_swap_of_a_row_that_doesnt_exist_changes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let user = UserId::new_v7(at(T0), [9; 10]).unwrap();
    let account = AccountId::new_v7(at(T0), [9; 10]).unwrap();
    let old = envelope(1, 1, TOTP_ENVELOPE_LEN);
    let new = envelope(2, 2, TOTP_ENVELOPE_LEN);

    assert!(!swap(&db, SealedRecord::TotpSecret(user), &old, &new).await);
    assert!(!swap(&db, SealedRecord::RefreshToken(account), &old, &new).await);
}

#[sqlx::test(migrations = false)]
async fn the_count_covers_both_tables_and_malformed_envelopes(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let db = database(options).await;
    let (user, _) = user_with_totp(&db, 1, 1).await;
    user_with_totp(&db, 2, 2).await;
    add_account(&db, user, 3, Some(envelope(2, 3, 80))).await;
    let mut version_2 = envelope(2, 4, 80);
    version_2[0] = 2;
    add_account(&db, user, 4, Some(version_2)).await;
    add_account(&db, user, 5, None).await;

    assert_eq!(count_not_under(&db, 1).await, 3);
    assert_eq!(count_not_under(&db, 2).await, 2);
    assert_eq!(count_not_under(&db, 3).await, 4);
}
