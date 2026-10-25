//! What the auth tests share: a server over a fresh database, its clock and
//! its services, a user with a real Argon2id password, more users by role,
//! their sessions and invites, requests, and redactions for snapshots.

use core::time::Duration;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::Method;
use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::auth::{InviteTtl, LockoutPolicy, NewPassword, SessionPolicy};
use fleet_core::authz::{Actor, CommandAllowlist, GrantLevel, Role};
use fleet_core::id::{AccountId, BotId, InviteId, ModeId, SessionId, UserId};
use fleet_core::mode::ModeName;
use fleet_core::pki::CertFingerprint;
use fleet_core::system::Clock as _;
use fleet_core::value::Username;
use fleet_server::app::agents::AgentService;
use fleet_server::app::auth::TokenService;
use fleet_server::app::bots::{BotService, ChatWait};
use fleet_server::app::challenges::{ChallengeUser, Challenges, SetupPurpose};
use fleet_server::app::enrollment::EnrollmentTokenService;
use fleet_server::app::event_tickets::EventTickets;
use fleet_server::app::events::{EventBus, LiveRights};
use fleet_server::app::health::HealthService;
use fleet_server::app::invites::InviteService;
use fleet_server::app::lockout::LockoutService;
use fleet_server::app::login::LoginService;
use fleet_server::app::mfa::{MfaPorts, MfaService};
use fleet_server::app::modes::ModeService;
use fleet_server::app::users::UserService;
use fleet_server::config::{Argon2Params, HttpConfig, RateLimitConfig};
use fleet_server::grpc::registry::AgentRegistry;
use fleet_server::http::middleware::RequestIds;
use fleet_server::http::openapi::ApiDocs;
use fleet_server::http::rate_limit::{RateLimits, TrustedProxies};
use fleet_server::http::router::{AppState, Services, router};
use fleet_server::http::ws::{LiveEvents, LiveSockets, SocketTimings};
use fleet_server::infra::crypto::attempt_keys::HmacAttemptKeys;
use fleet_server::infra::crypto::passwords::Argon2Passwords;
use fleet_server::infra::crypto::recovery_codes::Sha256RecoveryCodes;
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::infra::crypto::totp::TotpRs;
use fleet_server::infra::crypto::vault::{VaultKeys, XChaChaVault};
use fleet_server::infra::qr::SvgQr;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::accounts::{AccountReads, AccountWrites};
use fleet_server::ports::agents::{AgentReads, AgentWrites};
use fleet_server::ports::audit::{
    AuditEntryId, AuditLimit, AuditPage, AuditReads, AuditRecord, AuditWrites, NewAuditEntry,
};
use fleet_server::ports::bots::{Access, Bot, BotReads, BotRecord, BotWrites, Fleet, ListedBot};
use fleet_server::ports::chat::{ChatReads, ChatWrites};
use fleet_server::ports::enrollment_tokens::{EnrollmentTokenReads, EnrollmentTokenWrites};
use fleet_server::ports::envelopes::{EnvelopeReads, EnvelopeWrites};
use fleet_server::ports::grants::{GrantReads, GrantWrites};
use fleet_server::ports::invites::{InviteReads, InviteWrites, NewInvite};
use fleet_server::ports::live_hints::{LiveHints, ModeEvent};
use fleet_server::ports::login_attempts::{LoginAttemptReads, LoginAttemptWrites};
use fleet_server::ports::mfa::{MfaReads, MfaWrites};
use fleet_server::ports::modes::{
    ListedMode, ModeChange, ModeReads, ModeWrites, NewMode, StoredMode, WriteModeError,
};
use fleet_server::ports::page::Page;
use fleet_server::ports::passwords::{HashError, Passwords, VerifyError};
use fleet_server::ports::reconcile::{Change, Reconcile};
use fleet_server::ports::retention::{RetentionBatch, RetentionReads};
use fleet_server::ports::sessions::{
    LiveSince, Session, SessionReads, SessionWithUser, SessionWrites,
};
use fleet_server::ports::store::{Store, StoreError, UnauditedWrite, WriteTx};
use fleet_server::ports::tokens::TokenHash;
use fleet_server::ports::users::{NewUser, PasswordHash};
use fleet_server::ports::users::{UserReads, UserWrites};
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::{ExposeSecret as _, SecretBox, SecretString};
use serde_json::{Value, json};
use sqlx::sqlite::SqliteConnectOptions;
use totp_rs::Totp;
use tower::ServiceExt as _;

/// When every test starts: in the middle of a TOTP step.
pub(crate) const T0: i64 = 1_800_000_015_000;
/// Alice's password: strong, so the password service hashes it.
pub(crate) const PASSWORD: &str = "correct horse battery staple";

pub(crate) const LOGIN: &str = "/api/v1/auth/login";
pub(crate) const LOGIN_MFA: &str = "/api/v1/auth/login/mfa";
pub(crate) const REFRESH: &str = "/api/v1/auth/refresh";
pub(crate) const LOGOUT: &str = "/api/v1/auth/logout";
pub(crate) const STEP_UP: &str = "/api/v1/auth/step-up";
pub(crate) const RESET: &str = "/api/v1/me/mfa/reset";
pub(crate) const ENROLL: &str = "/api/v1/me/mfa/enroll";
pub(crate) const CONFIRM: &str = "/api/v1/me/mfa/confirm";
pub(crate) const ENROLLMENT_TOKENS: &str = "/api/v1/agents/enrollment-tokens";
pub(crate) const USERS: &str = "/api/v1/users";
pub(crate) const INVITES: &str = "/api/v1/invites";
pub(crate) const REGISTER: &str = "/api/v1/auth/register";
pub(crate) const ME: &str = "/api/v1/me";
pub(crate) const ME_PASSWORD: &str = "/api/v1/me/password";
pub(crate) const ME_SESSIONS: &str = "/api/v1/me/sessions";

/// A valid Argon2id PHC string (not a real hash), for users who never log
/// in with a password.
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// An invite's default lifetime, in milliseconds.
pub(crate) const INVITE_TTL: i64 = 72 * 3_600_000;
/// The CA fingerprint the tests' enrollment tokens pin.
pub(crate) const CA_FINGERPRINT: CertFingerprint = CertFingerprint::from_digest([0xAB; 32]);

pub(crate) fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

/// What the re-hash does in a test's password service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rehash {
    /// The real one.
    Works,
    /// The queue is full.
    Busy,
    /// Hashing fails.
    Fails,
}

/// The real password service, whose re-hash a test can make fail.
#[derive(Debug)]
pub(crate) struct TestPasswords {
    real: Argon2Passwords,
    rehash: Rehash,
}

#[async_trait]
impl Passwords for TestPasswords {
    async fn hash(
        &self,
        password: &NewPassword,
        username: &Username,
    ) -> Result<PasswordHash, HashError> {
        self.real.hash(password, username).await
    }

    async fn verify(
        &self,
        password: &SecretString,
        hash: &PasswordHash,
    ) -> Result<bool, VerifyError> {
        self.real.verify(password, hash).await
    }

    async fn verify_unknown_user(&self, password: &SecretString) -> Result<bool, VerifyError> {
        self.real.verify_unknown_user(password).await
    }

    fn needs_rehash(&self, hash: &PasswordHash) -> bool {
        self.real.needs_rehash(hash)
    }

    async fn rehash_verified(&self, password: &SecretString) -> Result<PasswordHash, HashError> {
        match self.rehash {
            Rehash::Works => self.real.rehash_verified(password).await,
            Rehash::Busy => Err(HashError::Busy),
            Rehash::Fails => Err(HashError::Failed("a test's hashing failure".into())),
        }
    }
}

/// How a test's server is set up.
pub(crate) struct Options {
    pub(crate) limits: RateLimitConfig,
    pub(crate) rehash: Rehash,
    /// Alice's stored hash, when it isn't one of today's parameters.
    pub(crate) hash: Option<PasswordHash>,
    pub(crate) invite_ttl: InviteTtl,
    /// Whether the control plane is on: the enrollment tokens' route
    /// answers 404 without it.
    pub(crate) control_plane: bool,
    /// `[chat] command_allowlist`, which the bots' mode switch and chat
    /// read.
    pub(crate) allowlist: CommandAllowlist,
    /// How long a chat line waits for its agent's answer: serve's, unless a
    /// test waits for no answer.
    pub(crate) chat_wait: ChatWait,
    /// The live events sockets' bounds: serve's, unless a test runs them
    /// short (P11.5).
    pub(crate) sockets: SocketTimings,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            limits: RateLimitConfig::default(),
            rehash: Rehash::Works,
            hash: None,
            invite_ttl: InviteTtl::default(),
            control_plane: false,
            allowlist: CommandAllowlist::default(),
            chat_wait: ChatWait::default(),
            sockets: SocketTimings::default(),
        }
    }
}

/// What changes, as if another request committed it, right before the
/// login service's next write transaction opens: between its checks and
/// its commit.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Meanwhile {
    /// The session is revoked.
    RevokeSession(SessionId),
    /// Alice's password changes (only its time; the hash stays).
    ChangePassword,
    /// Alice is disabled.
    Disable,
    /// Alice's authenticator is replaced (only its confirmation time).
    ReplaceMfa,
    /// A user's role changes.
    SetRole(UserId, Role),
    /// A user is disabled.
    DisableUser(UserId),
    /// An invite is used by a user.
    UseInvite(InviteId, UserId),
    /// Another user takes this username.
    AddUser(&'static str),
    /// A user's grant on an account is revoked.
    RevokeGrant(AccountId, UserId),
    /// A user's grant on an account is set to a level, by another user.
    SetGrant(AccountId, UserId, GrantLevel, UserId),
    /// A bot is switched to a mode.
    SetBotMode(BotId, ModeId),
    /// A mode is edited: its version is one more.
    EditMode(ModeId),
    /// A mode is deleted.
    DeleteMode(ModeId),
}

/// The database, with a change queued to land right before the next write
/// transaction opens, or right after it commits.
#[derive(Debug, Clone)]
pub(crate) struct HookedStore {
    inner: Database,
    alice: UserId,
    clock: ManualClock,
    queued: Arc<Mutex<Option<Meanwhile>>>,
    after_commit: Arc<Mutex<Option<Meanwhile>>>,
    lose_mode_update: Arc<Mutex<bool>>,
    sessions: HookedSessions,
    bots: HookedBots,
    modes: HookedModes,
}

impl HookedStore {
    /// `inner` with nothing queued; `alice` is the user several changes
    /// name.
    fn new(inner: Database, alice: UserId, clock: ManualClock) -> Self {
        Self {
            sessions: HookedSessions {
                inner: inner.clone(),
                fail_find: Arc::default(),
                hang_find: Arc::default(),
                hanging: Arc::new(tokio::sync::watch::Sender::new(0)),
                find_behind_writes: Arc::default(),
                found: Arc::new(tokio::sync::watch::Sender::new(0)),
            },
            modes: HookedModes {
                inner: inner.clone(),
                fail_listed: Arc::default(),
                hold_listed: Arc::default(),
                listeds: Arc::default(),
            },
            bots: HookedBots {
                inner: inner.clone(),
                accesses: Arc::default(),
                fail_access: Arc::default(),
                listed_ins: Arc::default(),
                fail_listed_in: Arc::default(),
                hold_listed_in: Arc::default(),
                hold_access: Arc::default(),
                commit_during_access: Arc::default(),
            },
            inner,
            alice,
            clock,
            queued: Arc::default(),
            after_commit: Arc::default(),
            lose_mode_update: Arc::default(),
        }
    }

    /// From now on, `SessionReads::find`, a live events socket's check of
    /// its session, fails as a busy database does; every other read works
    /// (P11.5).
    pub(crate) fn fail_socket_checks(&self) {
        *self.sessions.fail_find.lock().unwrap() = true;
    }

    /// From now on, `SessionReads::find` never answers, as a database stuck
    /// behind a long write; every other read works (P11.5).
    pub(crate) fn hang_socket_checks(&self) {
        *self.sessions.hang_find.lock().unwrap() = true;
    }

    /// From now on, `SessionReads::find` first waits for the write
    /// connection, as a read stuck behind writes: under a stream of commits,
    /// one has always landed by the time a check ends (P11.5).
    pub(crate) fn queue_socket_checks_behind_writes(&self) {
        *self.sessions.find_behind_writes.lock().unwrap() = true;
    }

    /// How many times the live events' rights were read
    /// (`BotReads::access`, P11.5).
    pub(crate) fn rights_reads(&self) -> usize {
        self.bots.accesses.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// From now on, reading the live events' rights fails as a busy
    /// database does (P11.5).
    pub(crate) fn fail_rights_reads(&self) {
        *self.bots.fail_access.lock().unwrap() = true;
    }

    /// From now on, reading one mode with its owner (`ModeReads::listed`, a
    /// live events socket's read of a mode) fails as a busy database does
    /// (P11.5).
    pub(crate) fn fail_mode_reads(&self) {
        *self.modes.fail_listed.lock().unwrap() = true;
    }

    /// How many times bots were read for live events (`BotReads::listed_in`,
    /// P11.5).
    pub(crate) fn bot_reads(&self) -> usize {
        self.bots
            .listed_ins
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// From now on, reading bots for live events fails as a busy database
    /// does (P11.5).
    pub(crate) fn fail_bot_reads(&self) {
        *self.bots.fail_listed_in.lock().unwrap() = true;
    }

    /// Holds the next read of bots for live events once it has read the
    /// database, until the returned hold is released (P11.5): the socket is
    /// then in the middle of a step, reading nothing from its client.
    pub(crate) fn hold_next_bot_read(&self) -> HeldRead {
        let (read, read_done) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        *self.bots.hold_listed_in.lock().unwrap() = Some((read, released));
        HeldRead { read_done, release }
    }

    /// How many times a mode was read with its owner (`ModeReads::listed`,
    /// a live events socket's read of a mode) since the start (P11.5).
    pub(crate) fn mode_reads(&self) -> usize {
        self.modes.listeds.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The next `ModeReads::listed`, a live events socket's read of a
    /// mode, tells the returned [`HeldRead`] once it read the database, then
    /// waits for its release before it answers (P11.5).
    pub(crate) fn hold_next_mode_read(&self) -> HeldRead {
        let (read, read_done) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        *self.modes.hold_listed.lock().unwrap() = Some((read, released));
        HeldRead { read_done, release }
    }

    /// Each of the next `reads` reads of the live events' rights commits
    /// once after it read the database and before it answers, as a write
    /// landing during the read does: each snapshot is older than the count
    /// when it comes (P11.5, S43).
    pub(crate) fn commit_during_next_rights_reads(&self, reads: u32) {
        self.bots
            .commit_during_access
            .store(reads, std::sync::atomic::Ordering::SeqCst);
    }

    /// Holds the next read of the live events' rights once it has read the
    /// database, until the returned hold is released (P11.5).
    pub(crate) fn hold_next_rights_read(&self) -> HeldRead {
        let (read, read_done) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        *self.bots.hold_access.lock().unwrap() = Some((read, released));
        HeldRead { read_done, release }
    }

    /// Waits, within `bound`, until `SessionReads::find`, a socket's check of
    /// its session, answered `count` times since the start (P11.5).
    pub(crate) async fn until_socket_checks(&self, count: u32, bound: Duration) {
        let mut found = self.sessions.found.subscribe();
        tokio::time::timeout(bound, found.wait_for(|found| *found >= count))
            .await
            .expect("the checks answered in time")
            .unwrap();
    }

    /// Waits, within `bound`, until a `find` hangs.
    pub(crate) async fn until_a_socket_check_hangs(&self, bound: Duration) {
        let mut hanging = self.sessions.hanging.subscribe();
        tokio::time::timeout(bound, hanging.wait_for(|hanging| *hanging > 0))
            .await
            .expect("a check hangs in time")
            .unwrap();
    }

    /// The token service's store: this one's session reads, with queues of
    /// its own, so its writes never take a change queued for another
    /// service's.
    fn sharing_session_reads(&self) -> Self {
        Self {
            queued: Arc::default(),
            after_commit: Arc::default(),
            lose_mode_update: Arc::default(),
            ..self.clone()
        }
    }

    /// Makes the next write transaction's `ModeWrites::update` and
    /// `ModeWrites::delete` answer that they matched no row, as no real one
    /// can after its own re-read found it.
    pub(crate) fn lose_next_mode_write(&self) {
        *self.lose_mode_update.lock().unwrap() = true;
    }

    /// Queues `change` for the next write transaction.
    pub(crate) fn queue(&self, change: Meanwhile) {
        *self.queued.lock().unwrap() = Some(change);
    }

    /// Queues `change` to land right after the next write transaction
    /// commits: before its caller reads anything again.
    pub(crate) fn queue_after_commit(&self, change: Meanwhile) {
        *self.after_commit.lock().unwrap() = Some(change);
    }

    async fn apply(&self, change: Meanwhile) {
        let now = self.clock.now();
        let id = self.alice.to_string();
        match change {
            Meanwhile::SetBotMode(bot, mode) => {
                let mut tx = self.inner.write().await.unwrap();
                assert!(tx.bots().set_mode(bot, mode).await.unwrap());
                tx.commit(setup_entry()).await.unwrap();
            }
            Meanwhile::EditMode(mode) => {
                let mode = mode.to_string();
                sqlx::query!("UPDATE modes SET version = version + 1 WHERE id = ?", mode)
                    .execute(self.inner.write_pool())
                    .await
                    .unwrap();
            }
            Meanwhile::DeleteMode(mode) => {
                let mode = mode.to_string();
                sqlx::query!("DELETE FROM modes WHERE id = ?", mode)
                    .execute(self.inner.write_pool())
                    .await
                    .unwrap();
            }
            Meanwhile::RevokeSession(session) => {
                let mut tx = self.inner.write().await.unwrap();
                assert!(tx.sessions().revoke(session, now).await.unwrap());
                tx.commit(setup_entry()).await.unwrap();
            }
            Meanwhile::ChangePassword => {
                let changed = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
                sqlx::query!(
                    "UPDATE users SET password_changed_at = ? WHERE id = ?",
                    changed,
                    id
                )
                .execute(self.inner.write_pool())
                .await
                .unwrap();
            }
            Meanwhile::Disable => {
                sqlx::query!("UPDATE users SET disabled = 1 WHERE id = ?", id)
                    .execute(self.inner.write_pool())
                    .await
                    .unwrap();
            }
            Meanwhile::ReplaceMfa => {
                let mut tx = self.inner.write().await.unwrap();
                let mut current = tx.mfa().find(self.alice).await.unwrap().unwrap();
                current.confirmed_at = now + chrono::TimeDelta::milliseconds(1);
                assert!(tx.mfa().replace(&current).await.unwrap());
                tx.commit(setup_entry()).await.unwrap();
            }
            Meanwhile::SetRole(user, role) => {
                let mut tx = self.inner.write().await.unwrap();
                assert!(tx.users().set_role(user, role).await.unwrap());
                tx.commit(setup_entry()).await.unwrap();
            }
            Meanwhile::DisableUser(user) => {
                let mut tx = self.inner.write().await.unwrap();
                assert!(tx.users().set_disabled(user, true).await.unwrap());
                tx.commit(setup_entry()).await.unwrap();
            }
            Meanwhile::UseInvite(invite, user) => {
                let mut tx = self.inner.write().await.unwrap();
                assert!(tx.invites().use_invite(invite, user, now).await.unwrap());
                tx.commit(setup_entry()).await.unwrap();
            }
            Meanwhile::RevokeGrant(account, user) => {
                let mut tx = self.inner.write().await.unwrap();
                assert!(tx.grants().delete(account, user).await.unwrap());
                tx.commit(setup_entry()).await.unwrap();
            }
            Meanwhile::SetGrant(account, user, level, by) => {
                let mut tx = self.inner.write().await.unwrap();
                assert!(
                    tx.grants()
                        .set_level(account, user, level, by, now)
                        .await
                        .unwrap()
                );
                tx.commit(setup_entry()).await.unwrap();
            }
            Meanwhile::AddUser(name) => {
                let mut tx = self.inner.write().await.unwrap();
                tx.users()
                    .insert(&NewUser {
                        id: UserId::new_v7(now, [0xEE; 10]).unwrap(),
                        username: Username::try_from(name).unwrap(),
                        password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
                        role: Role::Member,
                        created_at: now,
                    })
                    .await
                    .unwrap();
                tx.commit(setup_entry()).await.unwrap();
            }
        }
    }
}

/// A write transaction whose commit is followed by `after`, as if another
/// request committed it right then.
struct HookedTx {
    inner: Box<dyn WriteTx>,
    store: HookedStore,
    after: Meanwhile,
}

#[async_trait]
impl WriteTx for HookedTx {
    fn users(&mut self) -> &mut dyn UserWrites {
        self.inner.users()
    }

    fn sessions(&mut self) -> &mut dyn SessionWrites {
        self.inner.sessions()
    }

    fn login_attempts(&mut self) -> &mut dyn LoginAttemptWrites {
        self.inner.login_attempts()
    }

    fn mfa(&mut self) -> &mut dyn MfaWrites {
        self.inner.mfa()
    }

    fn invites(&mut self) -> &mut dyn InviteWrites {
        self.inner.invites()
    }

    fn accounts(&mut self) -> &mut dyn AccountWrites {
        self.inner.accounts()
    }

    fn grants(&mut self) -> &mut dyn GrantWrites {
        self.inner.grants()
    }

    fn envelopes(&mut self) -> &mut dyn EnvelopeWrites {
        self.inner.envelopes()
    }

    fn enrollment_tokens(&mut self) -> &mut dyn EnrollmentTokenWrites {
        self.inner.enrollment_tokens()
    }

    fn agents(&mut self) -> &mut dyn AgentWrites {
        self.inner.agents()
    }

    fn bots(&mut self) -> &mut dyn BotWrites {
        self.inner.bots()
    }

    fn modes(&mut self) -> &mut dyn ModeWrites {
        self.inner.modes()
    }

    fn chat(&mut self) -> &mut dyn ChatWrites {
        self.inner.chat()
    }

    fn audit(&mut self) -> &mut dyn AuditWrites {
        self.inner.audit()
    }

    async fn commit(self: Box<Self>, entry: NewAuditEntry) -> Result<AuditEntryId, StoreError> {
        let Self {
            inner,
            store,
            after,
        } = *self;
        let id = inner.commit(entry).await?;
        store.apply(after).await;
        Ok(id)
    }

    async fn commit_unaudited(self: Box<Self>, reason: UnauditedWrite) -> Result<(), StoreError> {
        let Self {
            inner,
            store,
            after,
        } = *self;
        inner.commit_unaudited(reason).await?;
        store.apply(after).await;
        Ok(())
    }
}

/// A write transaction whose mode updates and deletes match no row: a bug
/// a store could have, which a write must never answer as a success.
struct LosingTx {
    inner: Box<dyn WriteTx>,
}

#[async_trait]
impl WriteTx for LosingTx {
    fn users(&mut self) -> &mut dyn UserWrites {
        self.inner.users()
    }

    fn sessions(&mut self) -> &mut dyn SessionWrites {
        self.inner.sessions()
    }

    fn login_attempts(&mut self) -> &mut dyn LoginAttemptWrites {
        self.inner.login_attempts()
    }

    fn mfa(&mut self) -> &mut dyn MfaWrites {
        self.inner.mfa()
    }

    fn invites(&mut self) -> &mut dyn InviteWrites {
        self.inner.invites()
    }

    fn accounts(&mut self) -> &mut dyn AccountWrites {
        self.inner.accounts()
    }

    fn grants(&mut self) -> &mut dyn GrantWrites {
        self.inner.grants()
    }

    fn envelopes(&mut self) -> &mut dyn EnvelopeWrites {
        self.inner.envelopes()
    }

    fn enrollment_tokens(&mut self) -> &mut dyn EnrollmentTokenWrites {
        self.inner.enrollment_tokens()
    }

    fn agents(&mut self) -> &mut dyn AgentWrites {
        self.inner.agents()
    }

    fn bots(&mut self) -> &mut dyn BotWrites {
        self.inner.bots()
    }

    fn modes(&mut self) -> &mut dyn ModeWrites {
        self
    }

    fn chat(&mut self) -> &mut dyn ChatWrites {
        self.inner.chat()
    }

    fn audit(&mut self) -> &mut dyn AuditWrites {
        self.inner.audit()
    }

    async fn commit(self: Box<Self>, entry: NewAuditEntry) -> Result<AuditEntryId, StoreError> {
        self.inner.commit(entry).await
    }

    async fn commit_unaudited(self: Box<Self>, reason: UnauditedWrite) -> Result<(), StoreError> {
        self.inner.commit_unaudited(reason).await
    }
}

#[async_trait]
impl ModeWrites for LosingTx {
    async fn find(&mut self, id: ModeId) -> Result<Option<StoredMode>, StoreError> {
        self.inner.modes().find(id).await
    }

    async fn listed(&mut self, id: ModeId) -> Result<Option<ListedMode>, StoreError> {
        self.inner.modes().listed(id).await
    }

    async fn count_owned_by(&mut self, owner: UserId) -> Result<u32, StoreError> {
        self.inner.modes().count_owned_by(owner).await
    }

    async fn name_taken(
        &mut self,
        owner: UserId,
        name: &ModeName,
        shared: bool,
        except: Option<ModeId>,
    ) -> Result<bool, StoreError> {
        self.inner
            .modes()
            .name_taken(owner, name, shared, except)
            .await
    }

    async fn insert(&mut self, mode: &NewMode) -> Result<(), WriteModeError> {
        self.inner.modes().insert(mode).await
    }

    async fn update(&mut self, _change: &ModeChange) -> Result<bool, WriteModeError> {
        Ok(false)
    }

    async fn delete(&mut self, _id: ModeId) -> Result<bool, StoreError> {
        Ok(false)
    }
}

/// The database's mode reads, `listed` failing on request.
#[derive(Debug, Clone)]
struct HookedModes {
    inner: Database,
    fail_listed: Arc<Mutex<bool>>,
    /// The next `listed` tells the first once it read, then waits for the
    /// second.
    hold_listed: Arc<Mutex<Option<Hold>>>,
    /// How many `listed`s were asked for.
    listeds: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl ModeReads for HookedModes {
    async fn find(&self, id: ModeId) -> Result<Option<StoredMode>, StoreError> {
        self.inner.modes().find(id).await
    }

    async fn visible(
        &self,
        viewer: Actor,
        page: Page<ModeId>,
    ) -> Result<Vec<ListedMode>, StoreError> {
        self.inner.modes().visible(viewer, page).await
    }

    async fn listed(&self, id: ModeId) -> Result<Option<ListedMode>, StoreError> {
        self.listeds
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if *self.fail_listed.lock().unwrap() {
            return Err(StoreError::Busy);
        }
        let read = self.inner.modes().listed(id).await;
        let hold = self.hold_listed.lock().unwrap().take();
        if let Some((read_done, released)) = hold {
            let _ = read_done.send(());
            let _ = released.await;
        }
        read
    }
}

/// A live events read held by [`HookedStore::hold_next_rights_read`],
/// [`HookedStore::hold_next_bot_read`] or [`HookedStore::hold_next_mode_read`]:
/// the rights, a socket's bots or a mode.
pub(crate) struct HeldRead {
    read_done: tokio::sync::oneshot::Receiver<()>,
    release: tokio::sync::oneshot::Sender<()>,
}

impl HeldRead {
    /// Waits, within `bound`, until the held read has read the database.
    pub(crate) async fn read(&mut self, bound: Duration) {
        tokio::time::timeout(bound, &mut self.read_done)
            .await
            .expect("the held read reads in time")
            .unwrap();
    }

    /// Lets the held read answer.
    pub(crate) fn release(self) {
        let _ = self.release.send(());
    }
}

/// The database's bot reads, with the live events' rights counted, and
/// failing or held on request.
#[derive(Debug, Clone)]
struct HookedBots {
    inner: Database,
    accesses: Arc<std::sync::atomic::AtomicUsize>,
    fail_access: Arc<Mutex<bool>>,
    listed_ins: Arc<std::sync::atomic::AtomicUsize>,
    fail_listed_in: Arc<Mutex<bool>>,
    /// The next `listed_in` tells the first once it read, then waits for
    /// the second.
    hold_listed_in: Arc<Mutex<Option<Hold>>>,
    /// The next `access` tells the first once it read, then waits for the
    /// second.
    hold_access: Arc<Mutex<Option<Hold>>>,
    /// How many of the next `access`es commit once after they read.
    commit_during_access: Arc<std::sync::atomic::AtomicU32>,
}

/// A held read's two ends: told once it read, then released.
type Hold = (
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
);

#[async_trait]
impl BotReads for HookedBots {
    async fn find(&self, id: BotId) -> Result<Option<Bot>, StoreError> {
        self.inner.bots().find(id).await
    }

    async fn of_account(&self, account: AccountId) -> Result<Option<Bot>, StoreError> {
        self.inner.bots().of_account(account).await
    }

    async fn record(&self, id: BotId) -> Result<Option<BotRecord>, StoreError> {
        self.inner.bots().record(id).await
    }

    async fn records(&self, page: Page<BotId>) -> Result<Vec<BotRecord>, StoreError> {
        self.inner.bots().records(page).await
    }

    async fn listed(&self, id: BotId, viewer: UserId) -> Result<Option<ListedBot>, StoreError> {
        self.inner.bots().listed(id, viewer).await
    }

    async fn listed_in(&self, ids: &[BotId], viewer: UserId) -> Result<Vec<ListedBot>, StoreError> {
        self.listed_ins
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if *self.fail_listed_in.lock().unwrap() {
            return Err(StoreError::Busy);
        }
        let read = self.inner.bots().listed_in(ids, viewer).await;
        let hold = self.hold_listed_in.lock().unwrap().take();
        if let Some((read_done, released)) = hold {
            let _ = read_done.send(());
            let _ = released.await;
        }
        read
    }

    async fn visible(
        &self,
        viewer: Actor,
        page: Page<BotId>,
    ) -> Result<Vec<ListedBot>, StoreError> {
        self.inner.bots().visible(viewer, page).await
    }

    async fn fleet(&self) -> Result<Fleet, StoreError> {
        self.inner.bots().fleet().await
    }

    async fn access(&self) -> Result<Access, StoreError> {
        self.accesses
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if *self.fail_access.lock().unwrap() {
            return Err(StoreError::Busy);
        }
        let read = self.inner.bots().access().await;
        let commits = self.commit_during_access.try_update(
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
            |left| left.checked_sub(1),
        );
        if commits.is_ok() {
            let tx = self.inner.write().await.unwrap();
            tx.commit(NewAuditEntry::new(
                at(T0),
                AuditAction::try_from("test.commit_during_a_read").unwrap(),
                AuditOutcome::Success,
            ))
            .await
            .unwrap();
        }
        let hold = self.hold_access.lock().unwrap().take();
        if let Some((read_done, released)) = hold {
            let _ = read_done.send(());
            let _ = released.await;
        }
        read
    }
}

/// The database's session reads, with `find` failing once
/// [`HookedStore::fail_socket_checks`] asked it to.
#[derive(Debug, Clone)]
struct HookedSessions {
    inner: Database,
    fail_find: Arc<Mutex<bool>>,
    hang_find: Arc<Mutex<bool>>,
    /// How many `find`s hang.
    hanging: Arc<tokio::sync::watch::Sender<u32>>,
    /// Each `find` waits for the write connection first.
    find_behind_writes: Arc<Mutex<bool>>,
    /// How many `find`s answered.
    found: Arc<tokio::sync::watch::Sender<u32>>,
}

#[async_trait]
impl SessionReads for HookedSessions {
    async fn find_by_access_hash(
        &self,
        hash: &TokenHash,
    ) -> Result<Option<SessionWithUser>, StoreError> {
        self.inner.sessions().find_by_access_hash(hash).await
    }

    async fn refresh_hash_exists(&self, hash: &TokenHash) -> Result<bool, StoreError> {
        self.inner.sessions().refresh_hash_exists(hash).await
    }

    async fn find(&self, id: SessionId) -> Result<Option<SessionWithUser>, StoreError> {
        if *self.fail_find.lock().unwrap() {
            return Err(StoreError::Busy);
        }
        if *self.hang_find.lock().unwrap() {
            self.hanging.send_modify(|hanging| *hanging += 1);
            return std::future::pending().await;
        }
        let behind_writes = *self.find_behind_writes.lock().unwrap();
        if behind_writes {
            // Taken and dropped: rolled back, so it moves no count.
            drop(self.inner.write().await?);
        }
        let found = self.inner.sessions().find(id).await;
        self.found.send_modify(|found| *found += 1);
        found
    }

    async fn list_live(
        &self,
        user: UserId,
        live: LiveSince,
        page: Page<SessionId>,
    ) -> Result<Vec<Session>, StoreError> {
        self.inner.sessions().list_live(user, live, page).await
    }
}

#[async_trait]
impl Store for HookedStore {
    async fn write(&self) -> Result<Box<dyn WriteTx>, StoreError> {
        let queued = self.queued.lock().unwrap().take();
        if let Some(change) = queued {
            self.apply(change).await;
        }
        let tx = self.inner.write().await?;
        let after = self.after_commit.lock().unwrap().take();
        let tx: Box<dyn WriteTx> = match after {
            None => tx,
            Some(after) => Box::new(HookedTx {
                inner: tx,
                store: self.clone(),
                after,
            }),
        };
        let lose = core::mem::take(&mut *self.lose_mode_update.lock().unwrap());
        Ok(if lose {
            Box::new(LosingTx { inner: tx })
        } else {
            tx
        })
    }

    fn users(&self) -> &dyn UserReads {
        self.inner.users()
    }

    fn sessions(&self) -> &dyn SessionReads {
        &self.sessions
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
        &self.bots
    }

    fn modes(&self) -> &dyn ModeReads {
        &self.modes
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

    fn retention(&self) -> &dyn RetentionReads {
        self.inner.retention()
    }

    async fn retention_batch(&self) -> Result<Box<dyn RetentionBatch>, StoreError> {
        self.inner.retention_batch().await
    }

    async fn ping(&self) -> Result<(), StoreError> {
        self.inner.ping().await
    }

    fn commits(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.commits()
    }
}

/// A user with a live session.
pub(crate) struct Person {
    pub(crate) id: UserId,
    pub(crate) session: SessionId,
    pub(crate) token: SecretString,
}

pub(crate) struct Setup {
    pub(crate) app: Router,
    pub(crate) hooked: HookedStore,
    pub(crate) db: Database,
    pub(crate) clock: ManualClock,
    pub(crate) tokens: TokenService,
    pub(crate) challenges: Challenges,
    pub(crate) mfa: MfaService,
    pub(crate) lockout: LockoutService,
    /// The login service the app runs, for a test of an error the HTTP
    /// answer doesn't tell apart.
    pub(crate) login: LoginService,
    /// The random source every service shares, for a test that makes it fail.
    pub(crate) random: SeededRandom,
    pub(crate) alice: UserId,
    /// The agents' live streams, which a test claims directly.
    pub(crate) registry: AgentRegistry,
    /// The reconciler's hints the bots' writes sent.
    pub(crate) hints: Arc<Hinted>,
    /// The live events' hints the services sent (P11.5).
    pub(crate) heard: Arc<Heard>,
    /// The bus a chat send waits on, where a test answers for the agent.
    pub(crate) events: EventBus,
    /// The live events: their tickets, and the sockets' owner a test shuts
    /// down (P11.5).
    pub(crate) live_events: LiveEvents,
}

/// A reconciler that records the hints it gets, to check which writes send
/// one, and only after their commit (Plan.md P10.5; ADR-0019, S37).
#[derive(Debug, Default)]
pub(crate) struct Hinted(Mutex<Vec<Change>>);

impl Reconcile for Hinted {
    fn changed(&self, change: Change) {
        self.0.lock().unwrap().push(change);
    }
}

impl Hinted {
    /// The hints since the last call.
    pub(crate) fn take(&self) -> Vec<Change> {
        core::mem::take(&mut *self.0.lock().unwrap())
    }
}

/// One hint a service sent the live events sockets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Hint {
    /// These bots may read differently.
    Bots(Vec<BotId>),
    /// A mode was created, changed or deleted.
    Mode(ModeEvent),
    /// This user's role changed.
    RoleChanged(UserId),
}

/// The live events' hints the services send, each with the commit count at
/// the call, passed on to the bus the sockets read (ADR-0020, S42, R36): a
/// hint counted before its commit shows as one count short.
#[derive(Debug)]
pub(crate) struct Heard {
    bus: EventBus,
    commits: tokio::sync::watch::Receiver<u64>,
    calls: Mutex<Vec<(u64, Hint)>>,
}

impl Heard {
    fn new(bus: EventBus, commits: tokio::sync::watch::Receiver<u64>) -> Self {
        Self {
            bus,
            commits,
            calls: Mutex::new(Vec::new()),
        }
    }

    /// The hints since the last call, each with the commit count it came at.
    pub(crate) fn take(&self) -> Vec<(u64, Hint)> {
        core::mem::take(&mut *self.calls.lock().unwrap())
    }

    fn heard(&self, hint: Hint) {
        let count = *self.commits.borrow();
        self.calls.lock().unwrap().push((count, hint));
    }
}

impl LiveHints for Heard {
    fn bots(&self, bots: &[BotId]) {
        // An empty list is no hint, as the bus takes it.
        if !bots.is_empty() {
            self.heard(Hint::Bots(bots.to_vec()));
        }
        self.bus.bots(bots);
    }

    fn mode(&self, event: ModeEvent) {
        self.heard(Hint::Mode(event));
        self.bus.mode(event);
    }

    fn role_changed(&self, user: UserId) {
        self.heard(Hint::RoleChanged(user));
        self.bus.role_changed(user);
    }
}

/// The 2FA service over `db`, its vault on an in-memory key.
fn mfa_service(
    db: &Database,
    clock: &ManualClock,
    random: &SeededRandom,
    challenges: &Challenges,
) -> MfaService {
    let keys = VaultKeys::from_keys(
        BTreeMap::from([(1, SecretBox::new(Box::new([0x42; 32])))]),
        1,
    )
    .unwrap();
    MfaService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(db.clone()),
        Arc::new(XChaChaVault::new(keys, Arc::new(random.clone()))),
        MfaPorts {
            totp: Arc::new(TotpRs::new()),
            recovery: Arc::new(Sha256RecoveryCodes::new()),
            qr: Arc::new(SvgQr::new()),
        },
        challenges.clone(),
    )
}

/// The router over `state`, behind the real stack, every request from
/// 192.0.2.10.
fn app(state: AppState, clock: &ManualClock) -> Router {
    let http = HttpConfig {
        bind: HttpConfig::DEFAULT_BIND,
        request_timeout: Duration::from_secs(15),
        max_body_bytes: 65_536,
        trusted_proxies: TrustedProxies::default(),
    };
    let ids = RequestIds::new(Arc::new(clock.clone()), Arc::new(SeededRandom::new(5)));
    router(state, &http, ids, ApiDocs::Hidden)
        .layer(MockConnectInfo(SocketAddr::from(([192, 0, 2, 10], 4711))))
}

pub(crate) async fn setup(options: SqliteConnectOptions) -> Setup {
    setup_with(options, Options::default()).await
}

pub(crate) async fn setup_with(options: SqliteConnectOptions, settings: Options) -> Setup {
    let db = Database::connect(options, DatabaseOptions::default())
        .await
        .unwrap();
    let clock = ManualClock::new(at(T0));
    let random = SeededRandom::new(81);
    let token_port = Arc::new(Sha256Tokens::new(Arc::new(random.clone())));
    let alice = UserId::new_v7(at(T0), [1; 10]).unwrap();
    let hooked = HookedStore::new(db.clone(), alice, clock.clone());
    let tokens = token_service(&clock, &random, &hooked, &token_port);
    let challenges = Challenges::new(Arc::new(clock.clone()), token_port.clone());
    let mfa = mfa_service(&db, &clock, &random, &challenges);
    let (passwords, alice_hash) = passwords(&random, (settings.rehash, settings.hash)).await;
    let lockout = lockout_service(&clock, &db);
    let login = LoginService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(hooked.clone()),
        passwords.clone(),
        lockout.clone(),
        tokens.clone(),
        mfa.clone(),
    );
    let events = EventBus::new();
    let heard = Arc::new(Heard::new(events.clone(), db.commits()));
    let (users, invites) = accounts(
        &clock,
        &random,
        &hooked,
        (passwords, settings.invite_ttl),
        &heard,
    );
    let limits = RateLimits::new(&settings.limits, Arc::new(clock.clone()));
    let enrollment_tokens = settings
        .control_plane
        .then(|| enrollment_tokens(&clock, &random, &hooked));
    let (registry, agents) = agents(&clock, &hooked, &heard);
    let hints = Arc::new(Hinted::default());
    let bots = bot_service(
        &clock,
        &random,
        (&hooked, &events),
        &registry,
        (&settings.allowlist, settings.chat_wait),
        (&hints, &heard),
    );
    let live_events = build_live_events(
        &clock,
        (token_port, &hooked),
        &events,
        (settings.sockets, settings.limits.ws_connections_per_user),
    );
    let state = AppState::new(Services {
        health: HealthService::new(Arc::new(db.clone())),
        tokens: tokens.clone(),
        limits,
        mfa: mfa.clone(),
        login: login.clone(),
        users,
        invites,
        link_flows: idle_link_flows(&db),
        accounts: idle_accounts(&db),
        grants: idle_grants(&db),
        enrollment_tokens,
        agents,
        bots,
        modes: mode_service(
            &clock,
            &random,
            &hooked,
            &settings.allowlist,
            (&hints, &heard),
        ),
        events: live_events.clone(),
        audit: fleet_server::app::audit::AuditService::new(
            Arc::new(clock.clone()),
            Arc::new(hooked.clone()),
        ),
    });
    let app = app(state, &clock);
    insert_alice(&db, alice, alice_hash).await;
    Setup {
        app,
        hooked,
        db,
        clock,
        tokens,
        challenges,
        mfa,
        lockout,
        login,
        random,
        alice,
        registry,
        hints,
        heard,
        events,
        live_events,
    }
}

/// The passwords, rehashing as `rehash` says, and Alice's hash: `hash`, or
/// her password's.
async fn passwords(
    random: &SeededRandom,
    (rehash, hash): (Rehash, Option<PasswordHash>),
) -> (Arc<TestPasswords>, PasswordHash) {
    let passwords = Arc::new(TestPasswords {
        real: Argon2Passwords::new(Argon2Params::default(), Arc::new(random.clone()))
            .await
            .unwrap(),
        rehash,
    });
    let alice_hash = match hash {
        Some(hash) => hash,
        None => passwords
            .hash(
                &NewPassword::try_from(SecretString::from(PASSWORD)).unwrap(),
                &Username::try_from("alice").unwrap(),
            )
            .await
            .unwrap(),
    };
    (passwords, alice_hash)
}

/// The token service, reading sessions through `hooked`'s switches, with
/// write queues of its own.
fn token_service(
    clock: &ManualClock,
    random: &SeededRandom,
    hooked: &HookedStore,
    tokens: &Arc<Sha256Tokens>,
) -> TokenService {
    TokenService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(hooked.sharing_session_reads()),
        tokens.clone(),
        SessionPolicy::default(),
    )
}

/// The live events as serve builds them: tickets on `clock`, as many a user
/// as their sockets, the bus chat waits on, an owner of their own, and the
/// rights read through `hooked`, with `timings`.
fn build_live_events(
    clock: &ManualClock,
    (tokens, hooked): (Arc<Sha256Tokens>, &HookedStore),
    bus: &EventBus,
    (timings, per_user): (SocketTimings, core::num::NonZeroU32),
) -> LiveEvents {
    LiveEvents::new(
        EventTickets::new(Arc::new(clock.clone()), tokens).with_per_user(per_user),
        bus.clone(),
        LiveSockets::new(tokio_util::sync::CancellationToken::new()),
        LiveRights::new(Arc::new(hooked.clone())),
    )
    .with_timings(timings)
}

/// The lockout service over `db`, with a fixed attempt key.
fn lockout_service(clock: &ManualClock, db: &Database) -> LockoutService {
    LockoutService::new(
        Arc::new(clock.clone()),
        Arc::new(db.clone()),
        Arc::new(HmacAttemptKeys::new(SecretBox::new(Box::new([7; 32])))),
        LockoutPolicy::default(),
    )
}

/// The bots' service as serve builds it, over `hooked` and `registry`,
/// with offline accounts on, `allowlist` and `chat_wait`, recording its
/// hints in `hints`, waiting for chat answers on `events`.
fn bot_service(
    clock: &ManualClock,
    random: &SeededRandom,
    (hooked, events): (&HookedStore, &EventBus),
    registry: &AgentRegistry,
    (allowlist, chat_wait): (&CommandAllowlist, ChatWait),
    (hints, heard): (&Arc<Hinted>, &Arc<Heard>),
) -> BotService {
    BotService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(hooked.clone()),
        Arc::new(registry.clone()),
        events.clone(),
        true,
    )
    .with_allowlist(allowlist.clone())
    .with_reconcile(Arc::clone(hints) as Arc<dyn Reconcile>)
    .with_live_hints(Arc::clone(heard) as Arc<dyn LiveHints>)
    .with_chat_wait(chat_wait)
}

/// The modes' service as serve builds it, over `hooked`, with `allowlist`,
/// recording its hints to the reconciler in `hints` and to the sockets in
/// `heard`.
fn mode_service(
    clock: &ManualClock,
    random: &SeededRandom,
    hooked: &HookedStore,
    allowlist: &CommandAllowlist,
    (hints, heard): (&Arc<Hinted>, &Arc<Heard>),
) -> ModeService {
    ModeService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(hooked.clone()),
    )
    .with_allowlist(allowlist.clone())
    .with_reconcile(Arc::clone(hints) as Arc<dyn Reconcile>)
    .with_live_hints(Arc::clone(heard) as Arc<dyn LiveHints>)
}

/// Stores Alice, a Member, with `hash`.
async fn insert_alice(db: &Database, alice: UserId, hash: PasswordHash) {
    let mut tx = db.write().await.unwrap();
    tx.users()
        .insert(&NewUser {
            id: alice,
            username: Username::try_from("alice").unwrap(),
            password_hash: hash,
            role: Role::Member,
            created_at: at(T0 - 60_000),
        })
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
}

/// Each agent's one stream, which a test claims directly, and the agents'
/// service over `hooked`, ending streams there.
fn agents(
    clock: &ManualClock,
    hooked: &HookedStore,
    heard: &Arc<Heard>,
) -> (AgentRegistry, AgentService) {
    let registry = AgentRegistry::new(Arc::new(clock.clone()));
    let agents = AgentService::new(
        Arc::new(clock.clone()),
        Arc::new(hooked.clone()),
        Arc::new(registry.clone()),
    )
    .with_live_hints(Arc::clone(heard) as Arc<dyn LiveHints>);
    (registry, agents)
}

/// The enrollment tokens' service over `hooked`, pinning [`CA_FINGERPRINT`].
fn enrollment_tokens(
    clock: &ManualClock,
    random: &SeededRandom,
    hooked: &HookedStore,
) -> EnrollmentTokenService {
    EnrollmentTokenService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(hooked.clone()),
        Arc::new(Sha256Tokens::new(Arc::new(random.clone()))),
        CA_FINGERPRINT,
    )
}

/// The users' and invites' services over `hooked`, so a queued change
/// lands right before their write transactions too.
fn accounts(
    clock: &ManualClock,
    random: &SeededRandom,
    hooked: &HookedStore,
    (passwords, ttl): (Arc<TestPasswords>, InviteTtl),
    heard: &Arc<Heard>,
) -> (UserService, InviteService) {
    let users = UserService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(hooked.clone()),
        passwords,
    )
    .with_live_hints(Arc::clone(heard) as Arc<dyn LiveHints>);
    let invites = InviteService::new(
        Arc::new(clock.clone()),
        Arc::new(random.clone()),
        Arc::new(hooked.clone()),
        Arc::new(Sha256Tokens::new(Arc::new(random.clone()))),
        ttl,
    );
    (users, invites)
}

pub(crate) fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from("test.login_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// What a client sees of a response.
#[derive(Debug)]
pub(crate) struct Seen {
    pub(crate) status: u16,
    pub(crate) body: Value,
    pub(crate) text: String,
    pub(crate) headers: BTreeMap<String, String>,
}

impl Seen {
    pub(crate) fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or_default()
    }

    pub(crate) fn secret(&self, field: &str) -> SecretString {
        SecretString::from(self.body[field].as_str().unwrap())
    }

    /// The body without its request ID, which differs per request.
    pub(crate) fn body_without_request_id(&self) -> Value {
        let mut body = self.body.clone();
        if let Some(error) = body.get_mut("error").and_then(Value::as_object_mut) {
            error.remove("request_id");
        }
        body
    }
}

impl Setup {
    pub(crate) async fn send(&self, request: Request) -> Seen {
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter(|(name, _)| name.as_str() != "x-request-id")
            .map(|(name, value)| (name.to_string(), value.to_str().unwrap().to_owned()))
            .collect();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        Seen {
            status,
            body,
            text,
            headers,
        }
    }

    /// `POST path` with `bearer` and the JSON `body`, if any.
    pub(crate) async fn post(
        &self,
        path: &str,
        bearer: Option<&SecretString>,
        body: Option<Value>,
    ) -> Seen {
        self.request(Method::POST, path, bearer, body).await
    }

    /// `GET path` with `bearer`.
    pub(crate) async fn get(&self, path: &str, bearer: Option<&SecretString>) -> Seen {
        self.request(Method::GET, path, bearer, None).await
    }

    /// `PATCH path` with `bearer` and the JSON `body`.
    pub(crate) async fn patch(
        &self,
        path: &str,
        bearer: Option<&SecretString>,
        body: Value,
    ) -> Seen {
        self.request(Method::PATCH, path, bearer, Some(body)).await
    }

    /// `DELETE path` with `bearer`.
    pub(crate) async fn delete(&self, path: &str, bearer: Option<&SecretString>) -> Seen {
        self.request(Method::DELETE, path, bearer, None).await
    }

    /// `method path` with `bearer` and the JSON `body`, if any.
    pub(crate) async fn request(
        &self,
        method: Method,
        path: &str,
        bearer: Option<&SecretString>,
        body: Option<Value>,
    ) -> Seen {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = bearer {
            request = request.header("authorization", format!("Bearer {}", token.expose_secret()));
        }
        let request = match body {
            Some(body) => request
                .header("content-type", "application/json")
                .body(Body::from(body.to_string())),
            None => request.body(Body::empty()),
        };
        self.send(request.unwrap()).await
    }

    pub(crate) async fn login(&self, username: &str, password: &str) -> Seen {
        self.post(
            LOGIN,
            None,
            Some(json!({ "username": username, "password": password })),
        )
        .await
    }

    pub(crate) async fn code_step(&self, mfa_token: &SecretString, code: &str) -> Seen {
        self.post(
            LOGIN_MFA,
            None,
            Some(json!({ "mfa_token": mfa_token.expose_secret(), "code": code })),
        )
        .await
    }

    /// Alice's 2FA, confirmed through the service: the authenticator app and
    /// the recovery codes. The clock moves on a step, so the confirming code
    /// is behind.
    pub(crate) async fn enroll_alice(&self) -> (Totp, Vec<SecretString>) {
        let issued = self
            .challenges
            .issue_setup(self.alice_user(), SetupPurpose::FirstEnrollment)
            .unwrap();
        let grant = self.challenges.setup(issued.token.expose_secret()).unwrap();
        let enrollment = self.mfa.enroll(&grant).await.unwrap();
        let app = Totp::from_url(enrollment.otpauth_uri.expose_secret()).unwrap();
        let code = fleet_core::auth::TotpCode::parse(&self.code(&app, 0)).unwrap();
        let codes = self
            .mfa
            .confirm(&grant, &code, "192.0.2.10".parse().unwrap())
            .await
            .unwrap();
        self.clock.advance(Duration::from_secs(30));
        (app, codes)
    }

    pub(crate) fn alice_user(&self) -> ChallengeUser {
        ChallengeUser {
            id: self.alice,
            role: Role::Member,
            password_changed_at: at(T0 - 60_000),
        }
    }

    /// The app's code now, `steps` steps away.
    pub(crate) fn code(&self, app: &Totp, steps: i64) -> String {
        let now = u64::try_from(self.clock.now().timestamp() + steps * 30).unwrap();
        app.generate(now).to_string()
    }

    /// Logs Alice in with her app: the token response.
    pub(crate) async fn full_login(&self, app: &Totp) -> Seen {
        let first = self.login("alice", PASSWORD).await;
        assert_eq!(first.status, 200, "{}", first.text);
        let seen = self
            .code_step(&first.secret("token"), &self.code(app, 0))
            .await;
        assert_eq!(seen.status, 200, "{}", seen.text);
        self.clock.advance(Duration::from_secs(30));
        seen
    }

    /// A live session of Alice: its ID and its access token.
    pub(crate) async fn session(&self) -> (SessionId, SecretString) {
        self.session_of(self.alice).await
    }

    /// A live session of `user`: its ID and its access token.
    pub(crate) async fn session_of(&self, user: UserId) -> (SessionId, SecretString) {
        let prepared = self.tokens.prepare(user).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.sessions().insert(&prepared.new_session).await.unwrap();
        tx.commit(setup_entry()).await.unwrap();
        (prepared.new_session.id, prepared.tokens.access_token)
    }

    /// Adds `name` with `role`, as [`Setup::add_user`] does, with a live
    /// session.
    pub(crate) async fn person(&self, n: u8, name: &str, role: Role) -> Person {
        let id = self.add_user(n, name, role).await;
        let (session, token) = self.session_of(id).await;
        Person { id, session, token }
    }

    /// Alice, a Member, with a live session.
    pub(crate) async fn alice_person(&self) -> Person {
        let (session, token) = self.session().await;
        Person {
            id: self.alice,
            session,
            token,
        }
    }

    /// Stamps `session`'s step-up now, as `POST /auth/step-up` does.
    pub(crate) async fn stamp(&self, session: SessionId) {
        let mut tx = self.db.write().await.unwrap();
        assert!(
            tx.sessions()
                .stamp_step_up(session, self.clock.now())
                .await
                .unwrap()
        );
        tx.commit(setup_entry()).await.unwrap();
    }

    /// A session of `user` whose login was `millis` from `T0` (in the past
    /// for an expired one): its ID and its access token.
    pub(crate) async fn session_since(
        &self,
        user: UserId,
        millis: i64,
    ) -> (SessionId, SecretString) {
        let mut prepared = self.tokens.prepare(user).unwrap();
        prepared.new_session.created_at = at(T0 + millis);
        let mut tx = self.db.write().await.unwrap();
        tx.sessions().insert(&prepared.new_session).await.unwrap();
        tx.commit(setup_entry()).await.unwrap();
        (prepared.new_session.id, prepared.tokens.access_token)
    }

    /// Adds an invite by `creator` for `role` with a real code, created
    /// `millis` after `T0` and living 72 hours: its ID and its code.
    pub(crate) async fn invite_code(
        &self,
        n: u8,
        creator: UserId,
        role: Role,
        millis: i64,
    ) -> (InviteId, SecretString) {
        use fleet_server::ports::tokens::{TokenKind, Tokens as _};
        let minted = Sha256Tokens::new(Arc::new(SeededRandom::new(u64::from(n) + 1_000)))
            .mint(TokenKind::Invite)
            .unwrap();
        let id = InviteId::new_v7(at(T0 + millis), [n; 10]).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.invites()
            .insert(&NewInvite {
                id,
                code_hash: minted.hash,
                role,
                created_by: creator,
                created_at: at(T0 + millis),
                expires_at: at(T0 + millis + INVITE_TTL),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        (id, minted.token)
    }

    /// Adds user `name` with `role`, its ID minted from `n`, created a minute
    /// before `T0`. Its hash is no real one: it never logs in by password.
    pub(crate) async fn add_user(&self, n: u8, name: &str, role: Role) -> UserId {
        let id = UserId::new_v7(at(T0 - 60_000), [n; 10]).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.users()
            .insert(&NewUser {
                id,
                username: Username::try_from(name).unwrap(),
                password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
                role,
                created_at: at(T0 - 60_000),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        id
    }

    /// Adds an invite by `creator` for `role`, its ID and its code's hash
    /// minted from `n`, created `millis` after `T0` and living 72 hours.
    pub(crate) async fn add_invite(
        &self,
        n: u8,
        creator: UserId,
        role: Role,
        millis: i64,
    ) -> InviteId {
        let id = InviteId::new_v7(at(T0 + millis), [n; 10]).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.invites()
            .insert(&NewInvite {
                id,
                code_hash: TokenHash::from_bytes([n; 32]),
                role,
                created_by: creator,
                created_at: at(T0 + millis),
                expires_at: at(T0 + millis + INVITE_TTL),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        id
    }

    /// Marks `invite` used by `user` now.
    pub(crate) async fn use_invite(&self, invite: InviteId, user: UserId) {
        let mut tx = self.db.write().await.unwrap();
        assert!(
            tx.invites()
                .use_invite(invite, user, self.clock.now())
                .await
                .unwrap()
        );
        tx.commit(setup_entry()).await.unwrap();
    }

    /// Whether `user`'s session `id` is still live.
    pub(crate) async fn live(&self, id: SessionId) -> bool {
        let mut tx = self.db.write().await.unwrap();
        tx.sessions()
            .find(id)
            .await
            .unwrap()
            .is_some_and(|found| found.session.revoked_at.is_none())
    }

    /// The audit log, newest first.
    pub(crate) async fn audit(&self) -> Vec<AuditRecord> {
        self.db
            .audit()
            .list(AuditPage {
                before: None,
                limit: AuditLimit::new(100).unwrap(),
            })
            .await
            .unwrap()
    }

    /// The newest entry of `action`.
    pub(crate) async fn last(&self, action: &AuditAction) -> AuditRecord {
        self.audit()
            .await
            .into_iter()
            .find(|entry| &entry.action == action)
            .unwrap()
    }

    /// Alice's stored password hash.
    pub(crate) async fn alice_hash(&self) -> String {
        self.db
            .users()
            .get(self.alice)
            .await
            .unwrap()
            .unwrap()
            .password_hash
            .expose_secret()
            .to_owned()
    }

    /// Alice's failure streak, if any.
    pub(crate) async fn streak(&self) -> Option<u32> {
        use fleet_server::ports::login_attempts::AttemptKeys as _;
        let key = HmacAttemptKeys::new(SecretBox::new(Box::new([7; 32])))
            .key(&Username::try_from("alice").unwrap());
        self.db
            .login_attempts()
            .find(&key)
            .await
            .unwrap()
            .map(|attempt| attempt.failures)
    }
}

/// `value` for a snapshot: every ID that `names` lists becomes its name,
/// any other ID `[id]`, and every time its distance from `T0`
/// (`[T0+60s]`), so nothing minted from `SeededRandom` is pinned.
pub(crate) fn redacted(value: &Value, names: &[(String, &str)]) -> Value {
    match value {
        Value::String(text) => {
            if let Some((_, name)) = names.iter().find(|(id, _)| id == text) {
                return json!(format!("[{name}]"));
            }
            if text.parse::<UserId>().is_ok() {
                return json!("[id]");
            }
            if let Ok(time) = serde_json::from_value::<fleet_api_types::Timestamp>(value.clone()) {
                let millis = time.get().timestamp_millis() - T0;
                return json!(match millis {
                    0 => "[T0]".to_owned(),
                    m if m % 1000 == 0 => format!("[T0{:+}s]", m / 1000),
                    m => format!("[T0{m:+}ms]"),
                });
            }
            value.clone()
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| redacted(item, names)).collect())
        }
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, item)| (key.clone(), redacted(item, names)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The link flows, which these tests never start (P9.4): a service over
/// `db` with Microsoft's real client, never called.
fn idle_link_flows(
    db: &fleet_server::infra::sqlite::Database,
) -> fleet_server::app::link_flows::LinkFlowService {
    use fleet_server::app::link_flows::{LinkFlowPorts, LinkFlowService, LinkFlowSettings};
    let clock: Arc<dyn fleet_core::system::Clock> =
        Arc::new(fleet_testkit::system::ManualClock::new(
            chrono::DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
        ));
    let random: Arc<dyn fleet_core::system::SecureRandom> =
        Arc::new(fleet_testkit::system::SeededRandom::new(3));
    let keys = fleet_server::infra::crypto::vault::VaultKeys::from_keys(
        std::collections::BTreeMap::from([(1, secrecy::SecretBox::new(Box::new([1_u8; 32])))]),
        1,
    )
    .unwrap();
    LinkFlowService::new(
        LinkFlowPorts {
            live_hints: std::sync::Arc::new(fleet_server::ports::live_hints::NoLiveHints),
            microsoft: Arc::new(
                fleet_server::infra::msauth::ReqwestMicrosoftAuth::new(Arc::clone(&clock)).unwrap(),
            ),
            vault: Arc::new(fleet_server::infra::crypto::vault::XChaChaVault::new(
                keys,
                Arc::clone(&random),
            )),
            store: Arc::new(db.clone()),
            pace: Arc::new(fleet_server::infra::system::TokioPace),
            clock,
            random,
        },
        LinkFlowSettings {
            total: core::num::NonZeroU32::MIN,
            per_user_per_day: core::num::NonZeroU32::MIN,
            max_per_member: core::num::NonZeroU32::MIN,
        },
    )
}

/// The grants, which these tests never write.
fn idle_grants(
    db: &fleet_server::infra::sqlite::Database,
) -> fleet_server::app::grants::GrantService {
    fleet_server::app::grants::GrantService::new(
        Arc::new(fleet_testkit::system::ManualClock::new(
            chrono::DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
        )),
        Arc::new(db.clone()),
    )
}

/// The account service, which these tests never call (P9.7).
fn idle_accounts(
    db: &fleet_server::infra::sqlite::Database,
) -> fleet_server::app::accounts::AccountService {
    fleet_server::app::accounts::AccountService::new(
        Arc::new(fleet_testkit::system::ManualClock::new(
            chrono::DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
        )),
        Arc::new(fleet_testkit::system::SeededRandom::new(4)),
        Arc::new(db.clone()),
        fleet_server::app::accounts::AccountSettings {
            allow_offline: false,
            max_per_member: core::num::NonZeroU32::MIN,
        },
    )
}
