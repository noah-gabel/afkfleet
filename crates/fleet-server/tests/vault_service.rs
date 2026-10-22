//! Tests for the vault's status and rotation (Plan.md P9.2, ADR-0018): the
//! status counts what each key seals from the headers alone; a rotation
//! re-encrypts only the secrets under other keys than the active one, in
//! one transaction, refuses everything when any secret doesn't open (one
//! Failure entry each), and writes nothing when something changed meanwhile
//! or the random source failed.
//!
//! Each test gets a fresh database from `#[sqlx::test]`. Time isn't paused:
//! sqlx's worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::account::AccountStatus;
use fleet_core::audit::{
    AuditAction, AuditOutcome, AuditTarget, RecordedMetadata, RecordedTarget, RecordedValue,
};
use fleet_core::authz::Role;
use fleet_core::id::{AccountId, UserId};
use fleet_core::system::RandomError;
use fleet_core::value::Username;
use fleet_server::app::vault::{
    BadEnvelope, BadReason, KeyUsage, PurposeCounts, RotateError, Rotation, VaultService,
};
use fleet_server::infra::crypto::vault::{VaultKeys, XChaChaVault};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::accounts::{AccountCredential, AccountReads, McAccount};
use fleet_server::ports::agents::AgentReads;
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditReads, AuditRecord, NewAuditEntry};
use fleet_server::ports::bots::BotReads;
use fleet_server::ports::chat::ChatReads;
use fleet_server::ports::enrollment_tokens::EnrollmentTokenReads;
use fleet_server::ports::envelopes::{EnvelopeReads, SealedEnvelope, SealedRecord};
use fleet_server::ports::grants::GrantReads;
use fleet_server::ports::invites::InviteReads;
use fleet_server::ports::login_attempts::LoginAttemptReads;
use fleet_server::ports::mfa::{MfaReads, UserMfa};
use fleet_server::ports::modes::ModeReads;
use fleet_server::ports::sessions::SessionReads;
use fleet_server::ports::store::{Store, StoreError, WriteTx};
use fleet_server::ports::users::{NewUser, PasswordHash, UserReads};
use fleet_server::ports::vault::{Vault, VaultError, VaultPurpose, key_id_of};
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::{ExposeSecret as _, SecretBox, SecretString};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// The time everything starts at, in milliseconds.
const T0: i64 = 1_800_000_000_000;

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

/// A vault over the keys `ids` (key `n` is `[n; 32]`), sealing with
/// `active`, drawing nonces from `random`.
fn vault_with(ids: &[u8], active: u8, random: &SeededRandom) -> XChaChaVault {
    let keys = ids
        .iter()
        .map(|&n| (u32::from(n), SecretBox::new(Box::new([n; 32]))))
        .collect::<BTreeMap<_, _>>();
    XChaChaVault::new(
        VaultKeys::from_keys(keys, u32::from(active)).unwrap(),
        Arc::new(random.clone()),
    )
}

fn vault(ids: &[u8], active: u8) -> XChaChaVault {
    vault_with(ids, active, &SeededRandom::new(u64::from(active)))
}

/// The entry a test commits its setup with. A test-only name, never listed
/// in `audit_actions!`.
fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from("test.vault_setup").unwrap(),
        AuditOutcome::Success,
    )
}

// --- Changes that land right before the rotation's write transaction ---------------------

/// What changes, as if a running server committed it, right before the
/// service's next write transaction opens: between its reads and its swaps.
#[derive(Debug, Clone)]
enum Meanwhile {
    /// A user's TOTP secret is replaced with this envelope.
    ReplaceTotp(UserId, Vec<u8>),
    /// User `n` is added with this TOTP envelope.
    AddTotp(u8, Vec<u8>),
}

/// The database, with a change queued to land right before the next write
/// transaction opens (a local copy of the auth tests' `HookedStore`).
#[derive(Debug, Clone)]
struct HookedStore {
    inner: Database,
    queued: Arc<Mutex<Option<Meanwhile>>>,
}

impl HookedStore {
    fn queue(&self, change: Meanwhile) {
        *self.queued.lock().unwrap() = Some(change);
    }
}

#[async_trait]
impl Store for HookedStore {
    async fn write(&self) -> Result<Box<dyn WriteTx>, StoreError> {
        let queued = self.queued.lock().unwrap().take();
        match queued {
            Some(Meanwhile::ReplaceTotp(user, envelope)) => {
                let mut tx = self.inner.write().await.unwrap();
                let mut current = tx.mfa().find(user).await.unwrap().unwrap();
                current.secret_envelope = envelope;
                assert!(tx.mfa().replace(&current).await.unwrap());
                tx.commit(setup_entry()).await.unwrap();
            }
            Some(Meanwhile::AddTotp(n, envelope)) => {
                add_user_with_totp(&self.inner, n, envelope).await;
            }
            None => {}
        }
        self.inner.write().await
    }

    fn users(&self) -> &dyn UserReads {
        self.inner.users()
    }

    fn sessions(&self) -> &dyn SessionReads {
        self.inner.sessions()
    }

    fn login_attempts(&self) -> &dyn LoginAttemptReads {
        self.inner.login_attempts()
    }

    fn mfa(&self) -> &dyn MfaReads {
        self.inner.mfa()
    }

    fn invites(&self) -> &dyn InviteReads {
        self.inner.invites()
    }

    fn accounts(&self) -> &dyn AccountReads {
        self.inner.accounts()
    }

    fn grants(&self) -> &dyn GrantReads {
        self.inner.grants()
    }

    fn enrollment_tokens(&self) -> &dyn EnrollmentTokenReads {
        self.inner.enrollment_tokens()
    }

    fn agents(&self) -> &dyn AgentReads {
        self.inner.agents()
    }

    fn bots(&self) -> &dyn BotReads {
        self.inner.bots()
    }

    fn modes(&self) -> &dyn ModeReads {
        self.inner.modes()
    }

    fn chat(&self) -> &dyn ChatReads {
        self.inner.chat()
    }

    fn envelopes(&self) -> &dyn EnvelopeReads {
        self.inner.envelopes()
    }

    fn audit(&self) -> &dyn AuditReads {
        self.inner.audit()
    }

    async fn ping(&self) -> Result<(), StoreError> {
        self.inner.ping().await
    }

    fn commits(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.commits()
    }
}

// --- Setup ----------------------------------------------------------------------------------

struct Setup {
    db: Database,
    store: HookedStore,
    service: VaultService,
}

async fn setup(options: SqliteConnectOptions) -> Setup {
    let db = Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap();
    let store = HookedStore {
        inner: db.clone(),
        queued: Arc::new(Mutex::new(None)),
    };
    let clock = ManualClock::new(at(T0 + 60_000));
    let service = VaultService::new(Arc::new(clock), Arc::new(store.clone()));
    Setup { db, store, service }
}

fn user_id(n: u8) -> UserId {
    UserId::new_v7(at(T0), [n; 10]).unwrap()
}

fn account_id(n: u8) -> AccountId {
    AccountId::new_v7(at(T0), [n; 10]).unwrap()
}

/// User `n`'s 20-byte TOTP secret.
fn totp_secret(n: u8) -> [u8; 20] {
    [n; 20]
}

/// Account `n`'s refresh token.
fn refresh_token(n: u8) -> Vec<u8> {
    format!("refresh token of account {n}").into_bytes()
}

/// Adds user `n` whose TOTP secret is `envelope`.
async fn add_user_with_totp(db: &Database, n: u8, envelope: Vec<u8>) -> UserId {
    let id = user_id(n);
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
            secret_envelope: envelope,
            last_used_step: 0,
            confirmed_at: at(T0),
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    id
}

/// Adds user `n` whose TOTP secret `sealer` sealed.
async fn user_sealed_by(db: &Database, n: u8, sealer: &dyn Vault) -> UserId {
    let envelope = sealer
        .seal(VaultPurpose::TotpSecret, user_id(n).into(), &totp_secret(n))
        .unwrap();
    add_user_with_totp(db, n, envelope).await
}

/// Adds Microsoft account `n` of `owner`, whose refresh token `sealer`
/// sealed, or an offline account without one.
async fn account_sealed_by(
    db: &Database,
    owner: UserId,
    n: u8,
    sealer: Option<&XChaChaVault>,
) -> AccountId {
    let id = account_id(n);
    let credential = match sealer {
        Some(sealer) => AccountCredential::Microsoft {
            uuid: format!("069a79f4-44e9-4726-a5be-fca90e38aa{n:02x}")
                .parse()
                .unwrap(),
            refresh_envelope: sealer
                .seal(VaultPurpose::MsRefreshToken, id.into(), &refresh_token(n))
                .unwrap(),
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

/// Sets `record`'s envelope to `new`, bypassing the vault.
async fn overwrite(db: &Database, record: SealedRecord, new: &[u8]) {
    let old = envelope_of(db, record).await;
    let mut tx = db.write().await.unwrap();
    assert!(tx.envelopes().swap(record, &old, new).await.unwrap());
    tx.commit(setup_entry()).await.unwrap();
}

async fn envelopes(db: &Database) -> Vec<SealedEnvelope> {
    let mut all = db.envelopes().all().await.unwrap();
    all.sort_by_key(|sealed| sealed.record);
    all
}

async fn envelope_of(db: &Database, record: SealedRecord) -> Vec<u8> {
    envelopes(db)
        .await
        .into_iter()
        .find(|sealed| sealed.record == record)
        .unwrap()
        .envelope
}

/// The `vault.rotate_key` entries, oldest first.
async fn rotate_entries(db: &Database) -> Vec<AuditRecord> {
    let mut entries: Vec<AuditRecord> = db
        .audit()
        .list(AuditPage {
            before: None,
            limit: AuditLimit::new(100).unwrap(),
        })
        .await
        .unwrap()
        .into_iter()
        .filter(|entry| entry.action == AuditAction::VAULT_ROTATE_KEY)
        .collect();
    entries.reverse();
    entries
}

fn metadata(pairs: &[(&str, RecordedValue)]) -> RecordedMetadata {
    let mut metadata = RecordedMetadata::new();
    for (key, value) in pairs {
        match value {
            RecordedValue::Int(value) => metadata.push_int(key, *value),
            RecordedValue::Text(value) => metadata.push_text(key, value),
            other => panic!("unused in these tests: {other:?}"),
        }
    }
    metadata
}

fn text(value: &str) -> RecordedValue {
    RecordedValue::Text(value.to_owned())
}

/// Opens every stored secret with `vault` and returns the plaintexts by row.
async fn opened_with(db: &Database, vault: &dyn Vault) -> BTreeMap<SealedRecord, Vec<u8>> {
    envelopes(db)
        .await
        .into_iter()
        .map(|sealed| {
            let plaintext = vault
                .open(
                    sealed.record.purpose(),
                    sealed.record.record(),
                    &sealed.envelope,
                )
                .unwrap();
            (sealed.record, plaintext.expose_secret().clone())
        })
        .collect()
}

// --- Rotation ----------------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn nothing_under_another_key_writes_and_records_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let current = vault(&[1, 2], 2);
    let owner = user_sealed_by(&setup.db, 1, &current).await;
    account_sealed_by(&setup.db, owner, 2, Some(&current)).await;
    account_sealed_by(&setup.db, owner, 3, None).await;
    let before = envelopes(&setup.db).await;

    let rotation = setup.service.rotate(&current).await.unwrap();

    assert_eq!(rotation, Rotation::NothingToDo);
    assert_eq!(envelopes(&setup.db).await, before);
    assert_eq!(rotate_entries(&setup.db).await, Vec::new());
}

#[sqlx::test(migrations = false)]
async fn a_rotation_re_encrypts_the_secrets_under_other_keys_and_records_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let old = vault(&[1], 1);
    let current = vault(&[1, 2], 2);
    let first = user_sealed_by(&setup.db, 1, &old).await;
    let second = user_sealed_by(&setup.db, 2, &current).await;
    account_sealed_by(&setup.db, first, 3, Some(&old)).await;
    account_sealed_by(&setup.db, first, 4, None).await;
    let already = envelope_of(&setup.db, SealedRecord::TotpSecret(second)).await;
    let plaintexts = opened_with(&setup.db, &current).await;

    let rotation = setup.service.rotate(&current).await.unwrap();

    assert_eq!(
        rotation,
        Rotation::Rotated {
            key_id: 2,
            totp_secrets: 1,
            refresh_tokens: 1,
        }
    );
    assert_eq!(opened_with(&setup.db, &vault(&[2], 2)).await, plaintexts);
    assert_eq!(
        envelope_of(&setup.db, SealedRecord::TotpSecret(second)).await,
        already
    );
    let entries = rotate_entries(&setup.db).await;
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!((entry.actor, entry.ip, &entry.target), (None, None, &None));
    assert_eq!(entry.at, at(T0 + 60_000));
    assert_eq!(
        entry.metadata,
        metadata(&[
            ("ms_refresh", RecordedValue::Int(1)),
            ("sealed_with", RecordedValue::Int(2)),
            ("totp", RecordedValue::Int(1)),
        ])
    );
}

/// P9.2's AAD swap test: refresh tokens swapped between two accounts don't
/// open, so nothing is re-encrypted, and each is recorded.
#[sqlx::test(migrations = false)]
async fn rotation_refuses_refresh_tokens_swapped_between_two_accounts_and_records_each(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let old = vault(&[1], 1);
    let owner = user_sealed_by(&setup.db, 1, &old).await;
    let first = account_sealed_by(&setup.db, owner, 2, Some(&old)).await;
    let second = account_sealed_by(&setup.db, owner, 3, Some(&old)).await;
    let first_token = envelope_of(&setup.db, SealedRecord::RefreshToken(first)).await;
    let second_token = envelope_of(&setup.db, SealedRecord::RefreshToken(second)).await;
    overwrite(&setup.db, SealedRecord::RefreshToken(first), &second_token).await;
    overwrite(&setup.db, SealedRecord::RefreshToken(second), &first_token).await;
    let before = envelopes(&setup.db).await;

    let rotation = setup.service.rotate(&vault(&[1, 2], 2)).await.unwrap();

    assert_eq!(
        rotation,
        Rotation::Refused(vec![
            BadEnvelope {
                record: SealedRecord::RefreshToken(first),
                reason: BadReason::Rejected,
                key_id: Some(1),
            },
            BadEnvelope {
                record: SealedRecord::RefreshToken(second),
                reason: BadReason::Rejected,
                key_id: Some(1),
            },
        ])
    );
    assert_eq!(envelopes(&setup.db).await, before);
    let entries = rotate_entries(&setup.db).await;
    let failures: Vec<_> = entries
        .iter()
        .map(|entry| {
            (
                entry.outcome,
                entry.actor,
                entry.target.clone(),
                entry.metadata.clone(),
            )
        })
        .collect();
    let expected = |account| {
        (
            AuditOutcome::Failure,
            None,
            Some(RecordedTarget::Known(AuditTarget::Account(account))),
            metadata(&[
                ("reason", text("rejected")),
                ("sealed_with", RecordedValue::Int(1)),
            ]),
        )
    };
    assert_eq!(failures, [expected(first), expected(second)]);
}

#[sqlx::test(migrations = false)]
async fn an_unknown_key_and_a_malformed_envelope_are_refused_each_with_its_entry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let user = user_sealed_by(&setup.db, 1, &vault(&[3], 3)).await;
    let account = account_sealed_by(&setup.db, user, 2, Some(&vault(&[1], 1))).await;
    let mut version_2 = envelope_of(&setup.db, SealedRecord::RefreshToken(account)).await;
    version_2[0] = 2;
    overwrite(&setup.db, SealedRecord::RefreshToken(account), &version_2).await;
    let before = envelopes(&setup.db).await;

    let rotation = setup.service.rotate(&vault(&[1, 2], 2)).await.unwrap();

    assert_eq!(
        rotation,
        Rotation::Refused(vec![
            BadEnvelope {
                record: SealedRecord::TotpSecret(user),
                reason: BadReason::UnknownKey,
                key_id: Some(3),
            },
            BadEnvelope {
                record: SealedRecord::RefreshToken(account),
                reason: BadReason::Malformed,
                key_id: None,
            },
        ])
    );
    assert_eq!(envelopes(&setup.db).await, before);
    let entries = rotate_entries(&setup.db).await;
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[0].target,
        Some(RecordedTarget::Known(AuditTarget::User(user)))
    );
    assert_eq!(
        entries[0].metadata,
        metadata(&[
            ("reason", text("unknown_key")),
            ("sealed_with", RecordedValue::Int(3)),
        ])
    );
    assert_eq!(
        entries[1].target,
        Some(RecordedTarget::Known(AuditTarget::Account(account)))
    );
    assert_eq!(
        entries[1].metadata,
        metadata(&[("reason", text("malformed"))])
    );
}

#[sqlx::test(migrations = false)]
async fn a_secret_replaced_meanwhile_rolls_the_rotation_back_without_an_entry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let old = vault(&[1], 1);
    let user = user_sealed_by(&setup.db, 1, &old).await;
    let replacement = old
        .seal(VaultPurpose::TotpSecret, user.into(), &totp_secret(9))
        .unwrap();
    setup
        .store
        .queue(Meanwhile::ReplaceTotp(user, replacement.clone()));

    let rotation = setup.service.rotate(&vault(&[1, 2], 2)).await.unwrap();

    assert_eq!(rotation, Rotation::ChangedMeanwhile);
    assert_eq!(
        envelope_of(&setup.db, SealedRecord::TotpSecret(user)).await,
        replacement
    );
    assert_eq!(rotate_entries(&setup.db).await, Vec::new());
}

#[sqlx::test(migrations = false)]
async fn a_secret_added_under_an_old_key_meanwhile_rolls_the_rotation_back(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let old = vault(&[1], 1);
    let user = user_sealed_by(&setup.db, 1, &old).await;
    let before = envelope_of(&setup.db, SealedRecord::TotpSecret(user)).await;
    let added = old
        .seal(VaultPurpose::TotpSecret, user_id(2).into(), &totp_secret(2))
        .unwrap();
    setup.store.queue(Meanwhile::AddTotp(2, added));

    let rotation = setup.service.rotate(&vault(&[1, 2], 2)).await.unwrap();

    assert_eq!(rotation, Rotation::ChangedMeanwhile);
    assert_eq!(
        envelope_of(&setup.db, SealedRecord::TotpSecret(user)).await,
        before
    );
    assert_eq!(rotate_entries(&setup.db).await, Vec::new());
}

#[sqlx::test(migrations = false)]
async fn a_failing_random_source_writes_and_records_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    user_sealed_by(&setup.db, 1, &vault(&[1], 1)).await;
    let before = envelopes(&setup.db).await;
    let random = SeededRandom::new(7);
    let current = vault_with(&[1, 2], 2, &random);
    random.fail_next(RandomError::Unavailable);

    let result = setup.service.rotate(&current).await;

    assert!(
        matches!(
            result,
            Err(RotateError::Seal(VaultError::Random(
                RandomError::Unavailable
            )))
        ),
        "{result:?}"
    );
    assert_eq!(envelopes(&setup.db).await, before);
    assert_eq!(rotate_entries(&setup.db).await, Vec::new());
}

// --- Status --------------------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn the_status_counts_each_key_and_purpose_configured_or_not(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let first = user_sealed_by(&setup.db, 1, &vault(&[1], 1)).await;
    user_sealed_by(&setup.db, 2, &vault(&[1], 1)).await;
    user_sealed_by(&setup.db, 3, &vault(&[2], 2)).await;
    account_sealed_by(&setup.db, first, 4, Some(&vault(&[1], 1))).await;
    account_sealed_by(&setup.db, first, 5, Some(&vault(&[4], 4))).await;
    let malformed = account_sealed_by(&setup.db, first, 6, Some(&vault(&[2], 2))).await;
    let mut version_2 = envelope_of(&setup.db, SealedRecord::RefreshToken(malformed)).await;
    version_2[0] = 2;
    overwrite(&setup.db, SealedRecord::RefreshToken(malformed), &version_2).await;
    account_sealed_by(&setup.db, first, 7, None).await;

    let status = setup
        .service
        .status(&BTreeSet::from([1, 2, 3]), 2)
        .await
        .unwrap();

    let usage = |configured, totp_secrets, refresh_tokens| KeyUsage {
        configured,
        counts: PurposeCounts {
            totp_secrets,
            refresh_tokens,
        },
    };
    assert_eq!(status.active, 2);
    assert_eq!(
        status.keys,
        BTreeMap::from([
            (1, usage(true, 2, 1)),
            (2, usage(true, 1, 0)),
            (3, usage(true, 0, 0)),
            (4, usage(false, 0, 1)),
        ])
    );
    assert_eq!(
        status.malformed,
        PurposeCounts {
            totp_secrets: 0,
            refresh_tokens: 1,
        }
    );
    assert!(!status.healthy());
}

#[sqlx::test(migrations = false)]
async fn a_status_is_healthy_when_every_key_in_use_is_configured_and_nothing_is_malformed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let owner = user_sealed_by(&setup.db, 1, &vault(&[1], 1)).await;
    account_sealed_by(&setup.db, owner, 2, Some(&vault(&[2], 2))).await;

    let healthy = setup
        .service
        .status(&BTreeSet::from([1, 2]), 2)
        .await
        .unwrap();
    let unconfigured = setup.service.status(&BTreeSet::from([2]), 2).await.unwrap();

    assert!(healthy.healthy());
    assert!(!unconfigured.healthy());
    assert_eq!(
        unconfigured.keys.get(&1).map(|usage| usage.configured),
        Some(false)
    );
}

#[sqlx::test(migrations = false)]
async fn an_empty_vault_is_healthy_and_lists_its_configured_keys(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;

    let status = setup.service.status(&BTreeSet::from([1]), 1).await.unwrap();

    assert_eq!(
        status.keys,
        BTreeMap::from([(
            1,
            KeyUsage {
                configured: true,
                counts: PurposeCounts::default(),
            }
        )])
    );
    assert!(status.healthy());
    assert!(key_id_of(&[]).is_err());
}
