//! The link flows' test setup (P9.4): a fresh database behind a
//! [`FlakyStore`], a manual clock, a vault on seeded randomness, the
//! Microsoft fake, the pacing fake and the service over them, plus users of
//! every role with live sessions, and helpers that read back what the
//! service stored. Every value here is invented.
//!
//! The tests run on real time, like the session-token service's: the pacing
//! fake moves the manual clock by each wait, so no test waits for real, and
//! [`bounded`] bounds every call.

use core::fmt;
use core::future::Future;
use core::num::NonZeroU32;
use core::time::Duration;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use fleet_core::account::AccountStatus;
use fleet_core::audit::{AuditAction, RecordedValue};
use fleet_core::authz::{Actor, Role};
use fleet_core::id::{AccountId, LinkFlowId, SessionId, UserId};
use fleet_core::system::Clock as _;
use fleet_core::value::Username;
use fleet_server::app::link_flows::{
    LinkFlowPorts, LinkFlowService, LinkFlowSettings, LinkFlowStatus, LinkStartError, StartedFlow,
};
use fleet_server::app::users::ActingUser;
use fleet_server::infra::crypto::vault::XChaChaVault;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditRecord};
use fleet_server::ports::msauth::{DeviceFlow, PollInterval};
use fleet_server::ports::sessions::{NewSession, Rotation};
use fleet_server::ports::store::Store as _;
use fleet_server::ports::tokens::TokenHash;
use fleet_server::ports::users::{NewUser, PasswordHash};
use fleet_server::ports::vault::{Vault as _, VaultPurpose};
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::{ExposeSecret as _, SecretString};
use sqlx::sqlite::SqliteConnectOptions;

use super::msauth::{Call, FakeMicrosoftAuth, Script, device_flow};
use super::pace::FakePace;
use super::service::{T0, at, bounded, profile, setup_entry, vault_with_key};
use super::store::FlakyStore;

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// Where every acting user's requests come from.
pub(crate) const IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));
/// How long the fake's codes live: 15 minutes, Microsoft's usual.
pub(crate) const CODE_LIFETIME: i64 = 900_000;
/// The user code every scripted start answers.
pub(crate) const USER_CODE: &str = "B7X4-KDLQ";

/// §7.4's and Appendix A's caps: 3 at once, 10 a day, 3 accounts a Member.
pub(crate) fn settings() -> LinkFlowSettings {
    LinkFlowSettings {
        total: NonZeroU32::new(3).unwrap(),
        per_user_per_day: NonZeroU32::new(10).unwrap(),
        max_per_member: NonZeroU32::new(3).unwrap(),
    }
}

/// The service and everything it runs on.
pub(crate) struct LinkSetup {
    ports: LinkFlowPorts,
    settings: LinkFlowSettings,
    pub(crate) db: Database,
    pub(crate) store: Arc<FlakyStore>,
    pub(crate) clock: Arc<ManualClock>,
    /// The service's randomness, for the flows' and the accounts' IDs.
    pub(crate) random: SeededRandom,
    pub(crate) vault: Arc<XChaChaVault>,
    /// The vault's own randomness, for its nonces: a test can fail a seal
    /// without failing an ID.
    pub(crate) vault_random: SeededRandom,
    pub(crate) fake: Arc<FakeMicrosoftAuth>,
    pub(crate) pace: Arc<FakePace>,
    pub(crate) service: LinkFlowService,
}

impl LinkSetup {
    pub(crate) async fn new(options: SqliteConnectOptions) -> Self {
        Self::with(options, settings()).await
    }

    pub(crate) async fn with(options: SqliteConnectOptions, settings: LinkFlowSettings) -> Self {
        let db = Database::connect(options, DatabaseOptions::default())
            .await
            .unwrap();
        let store = Arc::new(FlakyStore::new(db.clone()));
        let clock = Arc::new(ManualClock::new(at(T0)));
        let random = SeededRandom::new(11);
        let vault_random = SeededRandom::new(12);
        let vault = Arc::new(vault_with_key(1, &vault_random));
        let fake = Arc::new(FakeMicrosoftAuth::new());
        let pace = Arc::new(FakePace::new(clock.clone()));
        let ports = LinkFlowPorts {
            clock: clock.clone(),
            random: Arc::new(random.clone()),
            store: store.clone(),
            vault: vault.clone(),
            microsoft: fake.clone(),
            pace: pace.clone(),
        };
        let service = LinkFlowService::new(ports.clone(), settings);
        Self {
            ports,
            settings,
            db,
            store,
            clock,
            random,
            vault,
            vault_random,
            fake,
            pace,
            service,
        }
    }

    /// A second service over the same ports and database, as after a
    /// restart: no flow in memory.
    pub(crate) fn restarted(&self) -> LinkFlowService {
        LinkFlowService::new(self.ports.clone(), self.settings)
    }

    /// Adds `user<n>` with `role` and a live session, acting from [`IP`].
    pub(crate) async fn person(&self, n: u8, role: Role) -> ActingUser {
        let id = UserId::new_v7(at(T0), [n; 10]).unwrap();
        let session_id = SessionId::new_v7(at(T0), [n; 10]).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.users()
            .insert(&NewUser {
                id,
                username: Username::try_from(format!("user{n}").as_str()).unwrap(),
                password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
                role,
                created_at: at(T0),
            })
            .await
            .unwrap();
        tx.sessions()
            .insert(&NewSession {
                id: session_id,
                user_id: id,
                access_hash: TokenHash::from_bytes([n; 32]),
                refresh_hash: TokenHash::from_bytes([n.wrapping_add(100); 32]),
                created_at: at(T0),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        ActingUser {
            actor: Actor { id, role },
            session_id,
            ip: IP,
        }
    }

    /// Revokes `by`'s session, as a logout or `DELETE /me/sessions` does.
    pub(crate) async fn revoke(&self, by: &ActingUser) {
        let mut tx = self.db.write().await.unwrap();
        assert!(
            tx.sessions()
                .revoke(by.session_id, self.clock.now())
                .await
                .unwrap()
        );
        tx.commit(setup_entry()).await.unwrap();
    }

    /// Refreshes `by`'s session `n` (made by [`LinkSetup::person`] `n`), as
    /// `POST /auth/refresh` does: new tokens, the same session.
    pub(crate) async fn rotate(&self, by: &ActingUser, n: u8) {
        let mut tx = self.db.write().await.unwrap();
        assert!(
            tx.sessions()
                .rotate(&Rotation {
                    session_id: by.session_id,
                    replaced: TokenHash::from_bytes([n.wrapping_add(100); 32]),
                    access_hash: TokenHash::from_bytes([n.wrapping_add(50); 32]),
                    refresh_hash: TokenHash::from_bytes([n.wrapping_add(150); 32]),
                    at: self.clock.now(),
                })
                .await
                .unwrap()
        );
        tx.commit(setup_entry()).await.unwrap();
    }

    /// Disables `by`'s user.
    pub(crate) async fn disable(&self, by: &ActingUser) {
        let mut tx = self.db.write().await.unwrap();
        assert!(tx.users().set_disabled(by.actor.id, true).await.unwrap());
        tx.commit(setup_entry()).await.unwrap();
    }

    /// Gives `by`'s user `role`.
    pub(crate) async fn set_role(&self, by: &ActingUser, role: Role) {
        let mut tx = self.db.write().await.unwrap();
        assert!(tx.users().set_role(by.actor.id, role).await.unwrap());
        tx.commit(setup_entry()).await.unwrap();
    }

    /// Adds account `n` of `owner`: Microsoft with `status`, or offline for
    /// `None`.
    pub(crate) async fn add_account(&self, owner: UserId, n: u8, status: Option<AccountStatus>) {
        let id = AccountId::new_v7(at(T0), [n; 10]).unwrap();
        let credential = match status {
            Some(status) => AccountCredential::Microsoft {
                uuid: profile(n).parse().unwrap(),
                refresh_envelope: vec![n; 61],
                status,
            },
            None => AccountCredential::Offline,
        };
        let mut tx = self.db.write().await.unwrap();
        tx.accounts()
            .insert(&McAccount {
                id,
                owner,
                name: format!("Owned{n}").parse().unwrap(),
                created_at: at(T0),
                credential,
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
    }

    /// Scripts the next start to answer the device code `code`, polled every
    /// 5 seconds, expiring [`CODE_LIFETIME`] from now.
    pub(crate) fn script_start(&self, code: &str) {
        let expires_at = at(self.clock.now().timestamp_millis() + CODE_LIFETIME);
        self.fake
            .script_start(Script::Answer(Ok(device_flow(code, USER_CODE, expires_at))));
    }

    /// Scripts the next start with `flow` as it is.
    pub(crate) fn script_flow(&self, flow: DeviceFlow) {
        self.fake.script_start(Script::Answer(Ok(flow)));
    }

    /// `by`'s start.
    pub(crate) async fn start(&self, by: &ActingUser) -> Result<StartedFlow, LinkStartError> {
        bounded(self.service.start(by)).await
    }

    /// Starts a flow for `by` with the device code `code`.
    pub(crate) async fn started(&self, by: &ActingUser, code: &str) -> LinkFlowId {
        self.script_start(code);
        self.start(by).await.unwrap().id
    }

    /// The flow `id`'s status now.
    pub(crate) fn status(&self, id: LinkFlowId) -> Option<LinkFlowStatus> {
        self.service.status(id).map(|view| view.status)
    }

    /// The flow `id`'s status once it ended; fails the test after 10 seconds.
    pub(crate) async fn ended(&self, id: LinkFlowId) -> LinkFlowStatus {
        bounded(async {
            loop {
                match self.status(id) {
                    Some(LinkFlowStatus::Waiting { .. } | LinkFlowStatus::Finishing) => {}
                    Some(status) => return status,
                    None => panic!("the flow is gone"),
                }
                tokio::task::yield_now().await;
            }
        })
        .await
    }

    /// Waits until the flow `id` is finishing; fails the test after 10
    /// seconds.
    pub(crate) async fn finishing(&self, id: LinkFlowId) {
        bounded(async {
            while self.status(id) != Some(LinkFlowStatus::Finishing) {
                tokio::task::yield_now().await;
            }
        })
        .await;
    }

    /// The device codes the fake was polled with, in order.
    pub(crate) fn polled_with(&self) -> Vec<String> {
        self.fake
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Poll { device_code } => Some(device_code),
                _ => None,
            })
            .collect()
    }

    /// How many sessions the fake started.
    pub(crate) fn sessions_started(&self) -> usize {
        self.fake
            .calls()
            .iter()
            .filter(|call| matches!(call, Call::MinecraftSession { .. }))
            .count()
    }

    /// How many device flows the fake started.
    pub(crate) fn starts(&self) -> usize {
        self.fake
            .calls()
            .iter()
            .filter(|call| matches!(call, Call::StartDeviceFlow))
            .count()
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

    /// The account `id` as stored.
    pub(crate) async fn account(&self, id: AccountId) -> McAccount {
        self.db.accounts().find(id).await.unwrap().unwrap()
    }

    /// How many accounts `owner` owns.
    pub(crate) async fn owned(&self, owner: UserId) -> u32 {
        self.db.accounts().count_owned_by(owner).await.unwrap()
    }

    /// The refresh token the account `id` stores.
    pub(crate) async fn stored_token(&self, id: AccountId) -> String {
        let AccountCredential::Microsoft {
            refresh_envelope, ..
        } = self.account(id).await.credential
        else {
            panic!("an offline account has no token");
        };
        let plaintext = self
            .vault
            .open(VaultPurpose::MsRefreshToken, id.into(), &refresh_envelope)
            .unwrap();
        String::from_utf8(plaintext.expose_secret().clone()).unwrap()
    }
}

/// Drives `future` until `until` is ready, and returns its output; fails
/// the test if `future` answers first, or after 10 seconds.
pub(crate) async fn drive_until<F, G>(future: &mut F, until: G) -> G::Output
where
    F: Future + Unpin,
    F::Output: fmt::Debug,
    G: Future,
{
    bounded(async {
        tokio::select! {
            biased;
            reached = until => reached,
            answered = future => panic!("answered too early: {answered:?}"),
        }
    })
    .await
}

/// How long the poller waits between polls at first.
pub(crate) fn default_interval() -> Duration {
    PollInterval::DEFAULT.get()
}

/// The recorded metadata `entry` holds under `key`.
pub(crate) fn meta<'a>(entry: &'a AuditRecord, key: &str) -> Option<&'a RecordedValue> {
    entry
        .metadata
        .entries()
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}
