//! The session-token service's test setup (P9.5): a fresh database behind
//! a [`FlakyStore`], a manual clock, a vault on seeded randomness, the
//! Microsoft fake and the service over them, plus helpers that read back
//! what the service stored. Every value here is invented.
//!
//! The tests run on real time (sqlx's worker threads make paused time fire
//! timeouts early, CLAUDE.md), with the manual clock for every expiry, and
//! bound every wait with [`bounded`].

use core::future::Future;
use core::pin::Pin;
use core::task::Poll;
use core::time::Duration;
use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::account::AccountStatus;
use fleet_core::audit::{AuditAction, AuditOutcome, RecordedTarget, RecordedValue};
use fleet_core::authz::Role;
use fleet_core::id::{AccountId, UserId};
use fleet_core::value::Username;
use fleet_server::app::session_tokens::{SessionTokenPorts, SessionTokenService};
use fleet_server::infra::crypto::vault::{VaultKeys, XChaChaVault};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditRecord, NewAuditEntry};
use fleet_server::ports::envelopes::SealedRecord;
use fleet_server::ports::msauth::{MicrosoftTokens, MinecraftSession};
use fleet_server::ports::store::Store as _;
use fleet_server::ports::users::{NewUser, PasswordHash};
use fleet_server::ports::vault::{Vault as _, VaultPurpose};
use fleet_startup::config::{LogConfig, LogFormat};
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::log_buffer::LogBuffer;
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::{ExposeSecret as _, SecretBox, SecretString};
use sqlx::sqlite::SqliteConnectOptions;
use tokio_util::sync::CancellationToken;
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::layer::SubscriberExt as _;

use super::msauth::{Call, FakeMicrosoftAuth, session, tokens};
use super::store::FlakyStore;

/// When every test starts, in milliseconds.
pub(crate) const T0: i64 = 1_800_000_000_000;
/// A day, in milliseconds: how long the fake's Minecraft sessions live.
pub(crate) const DAY: i64 = 86_400_000;
/// A Minecraft profile's UUID.
pub(crate) const PROFILE: &str = "069a79f4-44e9-4726-a5be-fca90e38aaf5";
/// Another one.
pub(crate) const OTHER_PROFILE: &str = "853c80ef-3c37-49fd-aa49-938b674adae6";
/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// How long [`bounded`] lets a call take before failing the test.
const TEST_BOUND: Duration = Duration::from_secs(10);

pub(crate) fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

/// The `n`-th invented profile UUID, for tests with many accounts.
pub(crate) fn profile(n: u8) -> String {
    format!("{n:08x}-0000-4000-8000-{n:012x}")
}

/// JSON logs at `debug` from every target, written on this thread until the
/// guard drops. The tests' runtime has one thread, so the service's tasks
/// log here too.
pub(crate) fn json_logs() -> (LogBuffer, DefaultGuard) {
    let logs = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: "debug".parse().unwrap(),
    };
    let subscriber =
        tracing_subscriber::registry().with(layer_with(&config, &NoRules, false, logs.clone()));
    (logs, tracing::subscriber::set_default(subscriber))
}

/// The levels of the lines whose message is `message`.
pub(crate) fn levels(logs: &LogBuffer, message: &str) -> Vec<String> {
    logs.lines_with(message)
        .unwrap()
        .iter()
        .map(|line| line["level"].as_str().unwrap().to_owned())
        .collect()
}

/// `call`'s answer; a call that hangs fails the test after 10 seconds.
pub(crate) async fn bounded<F: Future>(call: F) -> F::Output {
    tokio::time::timeout(TEST_BOUND, call)
        .await
        .expect("the call answered within the test's bound")
}

/// Polls `future` once: its answer, or `None` while it's pending.
pub(crate) async fn poll_once<F: Future + Unpin>(future: &mut F) -> Option<F::Output> {
    core::future::poll_fn(|cx| match Pin::new(&mut *future).poll(cx) {
        Poll::Ready(answer) => Poll::Ready(Some(answer)),
        Poll::Pending => Poll::Ready(None),
    })
    .await
}

/// The entry a test commits its setup with. A test-only name, never listed
/// in `audit_actions!`.
fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from("test.session_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// A vault over key 1 (`[1; 32]`), drawing nonces from `random`.
pub(crate) fn vault_with_key(key_id: u8, random: &SeededRandom) -> XChaChaVault {
    let keys = BTreeMap::from([(u32::from(key_id), SecretBox::new(Box::new([key_id; 32])))]);
    XChaChaVault::new(
        VaultKeys::from_keys(keys, u32::from(key_id)).unwrap(),
        Arc::new(random.clone()),
    )
}

/// The service and everything it runs on.
pub(crate) struct Setup {
    pub(crate) db: Database,
    pub(crate) store: Arc<FlakyStore>,
    pub(crate) clock: Arc<ManualClock>,
    pub(crate) random: SeededRandom,
    pub(crate) vault: Arc<XChaChaVault>,
    pub(crate) fake: Arc<FakeMicrosoftAuth>,
    pub(crate) stop: CancellationToken,
    pub(crate) service: SessionTokenService,
    /// The user who owns every account [`Setup::link`] adds.
    pub(crate) owner: UserId,
}

impl Setup {
    pub(crate) async fn new(options: SqliteConnectOptions) -> Self {
        let db = Database::connect(options, DatabaseOptions::default())
            .await
            .unwrap();
        let store = Arc::new(FlakyStore::new(db.clone()));
        let clock = Arc::new(ManualClock::new(at(T0)));
        let random = SeededRandom::new(9);
        let vault = Arc::new(vault_with_key(1, &random));
        let fake = Arc::new(FakeMicrosoftAuth::new());
        let stop = CancellationToken::new();
        let service = SessionTokenService::new(
            SessionTokenPorts {
                clock: clock.clone(),
                store: store.clone(),
                vault: vault.clone(),
                microsoft: fake.clone(),
            },
            &stop,
        );
        let mut setup = Self {
            db,
            store,
            clock,
            random,
            vault,
            fake,
            stop,
            service,
            owner: UserId::new_v7(at(T0), [0; 10]).unwrap(),
        };
        setup.owner = setup.add_user(1).await;
        setup
    }

    /// Adds the Member `user<n>`, and returns its ID.
    pub(crate) async fn add_user(&self, n: u8) -> UserId {
        let id = UserId::new_v7(at(T0), [n; 10]).unwrap();
        let user = NewUser {
            id,
            username: Username::try_from(format!("user{n}").as_str()).unwrap(),
            password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
            role: Role::Member,
            created_at: at(T0),
        };
        let mut tx = self.db.write().await.unwrap();
        tx.users().insert(&user).await.unwrap();
        tx.commit(setup_entry()).await.unwrap();
        id
    }

    /// Account `n`'s ID.
    pub(crate) fn account_id(n: u8) -> AccountId {
        AccountId::new_v7(at(T0 + i64::from(n)), [n; 10]).unwrap()
    }

    /// `plaintext` sealed for account `id`'s refresh token.
    pub(crate) fn seal(&self, id: AccountId, plaintext: &[u8]) -> Vec<u8> {
        self.vault
            .seal(VaultPurpose::MsRefreshToken, id.into(), plaintext)
            .unwrap()
    }

    /// Adds account `n` of the owner: named `name`, of `profile`, with
    /// `status`, its refresh token `refresh`.
    pub(crate) async fn link(
        &self,
        n: u8,
        name: &str,
        profile: &str,
        status: AccountStatus,
        refresh: &str,
    ) -> AccountId {
        let id = Self::account_id(n);
        let envelope = self.seal(id, refresh.as_bytes());
        self.link_with_envelope(n, name, profile, status, envelope)
            .await
    }

    /// Adds account `n` of the owner with `envelope` as it is.
    pub(crate) async fn link_with_envelope(
        &self,
        n: u8,
        name: &str,
        profile: &str,
        status: AccountStatus,
        envelope: Vec<u8>,
    ) -> AccountId {
        let id = Self::account_id(n);
        let account = McAccount {
            id,
            owner: self.owner,
            name: name.parse().unwrap(),
            created_at: at(T0),
            credential: AccountCredential::Microsoft {
                uuid: profile.parse().unwrap(),
                refresh_envelope: envelope,
                status,
            },
        };
        self.insert(&account).await;
        id
    }

    /// Adds the offline account `n` of the owner.
    pub(crate) async fn link_offline(&self, n: u8, name: &str) -> AccountId {
        let id = Self::account_id(n);
        let account = McAccount {
            id,
            owner: self.owner,
            name: name.parse().unwrap(),
            created_at: at(T0),
            credential: AccountCredential::Offline,
        };
        self.insert(&account).await;
        id
    }

    async fn insert(&self, account: &McAccount) {
        let mut tx = self.db.write().await.unwrap();
        tx.accounts().insert(account).await.unwrap();
        tx.commit(setup_entry()).await.unwrap();
    }

    /// The account `id` as stored.
    pub(crate) async fn account(&self, id: AccountId) -> McAccount {
        self.db.accounts().find(id).await.unwrap().unwrap()
    }

    /// The account `id`'s stored envelope.
    pub(crate) async fn envelope(&self, id: AccountId) -> Vec<u8> {
        match self.account(id).await.credential {
            AccountCredential::Microsoft {
                refresh_envelope, ..
            } => refresh_envelope,
            AccountCredential::Offline => panic!("an offline account has no envelope"),
        }
    }

    /// The refresh token the account `id` stores.
    pub(crate) async fn stored_token(&self, id: AccountId) -> String {
        let envelope = self.envelope(id).await;
        let plaintext = self
            .vault
            .open(VaultPurpose::MsRefreshToken, id.into(), &envelope)
            .unwrap();
        String::from_utf8(plaintext.expose_secret().clone()).unwrap()
    }

    /// The account `id`'s status as stored.
    pub(crate) async fn status(&self, id: AccountId) -> AccountStatus {
        self.account(id).await.status()
    }

    /// Sets the account `id`'s status, as an admin's change would.
    pub(crate) async fn set_status(&self, id: AccountId, status: AccountStatus) {
        let mut tx = self.db.write().await.unwrap();
        assert!(tx.accounts().set_status(id, status).await.unwrap());
        tx.commit(setup_entry()).await.unwrap();
    }

    /// Disables or enables `user`.
    pub(crate) async fn set_disabled(&self, user: UserId, disabled: bool) {
        let mut tx = self.db.write().await.unwrap();
        assert!(tx.users().set_disabled(user, disabled).await.unwrap());
        tx.commit(setup_entry()).await.unwrap();
    }

    /// Replaces the account `id`'s refresh token with `refresh`, as a link
    /// again would.
    pub(crate) async fn relink(&self, id: AccountId, refresh: &str) {
        let old = self.envelope(id).await;
        let new = self.seal(id, refresh.as_bytes());
        let mut tx = self.db.write().await.unwrap();
        assert!(
            tx.envelopes()
                .swap(SealedRecord::RefreshToken(id), &old, &new)
                .await
                .unwrap()
        );
        tx.commit(setup_entry()).await.unwrap();
    }

    /// Writes `status` into the account `id`'s row as it is, bypassing every
    /// check: `frozen` makes the row unreadable.
    pub(crate) async fn set_raw_status(&self, id: AccountId, status: &str) {
        let id = id.to_string();
        let done = sqlx::query!("UPDATE mc_accounts SET status = ? WHERE id = ?", status, id)
            .execute(self.db.write_pool())
            .await
            .unwrap();
        assert_eq!(done.rows_affected(), 1);
    }

    /// Deletes the account `id`, as an admin's delete would (group E).
    pub(crate) async fn delete(&self, id: AccountId) {
        let id = id.to_string();
        let done = sqlx::query!("DELETE FROM mc_accounts WHERE id = ?", id)
            .execute(self.db.write_pool())
            .await
            .unwrap();
        assert_eq!(done.rows_affected(), 1);
    }

    /// Every audit entry with `action`, oldest first.
    pub(crate) async fn entries(&self, action: &AuditAction) -> Vec<AuditRecord> {
        let page = AuditPage {
            before: None,
            limit: AuditLimit::new(100).unwrap(),
        };
        let mut entries: Vec<AuditRecord> = self
            .db
            .audit()
            .list(page)
            .await
            .unwrap()
            .into_iter()
            .filter(|entry| &entry.action == action)
            .collect();
        entries.reverse();
        entries
    }

    /// How many audit entries the server recorded: every one but the
    /// tests' own setup entries.
    pub(crate) async fn server_entries(&self) -> usize {
        let page = AuditPage {
            before: None,
            limit: AuditLimit::new(100).unwrap(),
        };
        self.db
            .audit()
            .list(page)
            .await
            .unwrap()
            .iter()
            .filter(|entry| entry.action.as_str() != "test.session_setup")
            .count()
    }

    /// The refresh tokens the fake was sent, in order.
    pub(crate) fn refreshed_with(&self) -> Vec<String> {
        self.fake
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Refresh { refresh_token } => Some(refresh_token),
                _ => None,
            })
            .collect()
    }

    /// The access tokens the fake started sessions with, in order.
    pub(crate) fn sessions_started_with(&self) -> Vec<String> {
        self.fake
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::MinecraftSession { access_token } => Some(access_token),
                _ => None,
            })
            .collect()
    }
}

/// Microsoft's tokens: the access token `access` and the refresh token to
/// keep, `refresh`.
pub(crate) fn ms_tokens(access: &str, refresh: &str) -> MicrosoftTokens {
    tokens(access, refresh, at(T0 + 3_600_000))
}

/// A Minecraft session `token` of `profile` named `name`, expiring a day
/// after T0.
pub(crate) fn mc_session(token: &str, profile: &str, name: &str) -> MinecraftSession {
    session(token, profile, name, at(T0 + DAY))
}

/// The audit entry's target, which must be the account `id`.
pub(crate) fn assert_targets(entry: &AuditRecord, id: AccountId) {
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(
            fleet_core::audit::AuditTarget::Account(id)
        ))
    );
    assert_eq!(entry.actor, None);
    assert_eq!(entry.ip, None);
}

/// `pairs` as recorded metadata reads back: sorted by key.
pub(crate) fn metadata(pairs: &[(&str, RecordedValue)]) -> Vec<(String, RecordedValue)> {
    let mut pairs: Vec<(String, RecordedValue)> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    pairs
}

pub(crate) fn text(value: &str) -> RecordedValue {
    RecordedValue::Text(value.to_owned())
}
