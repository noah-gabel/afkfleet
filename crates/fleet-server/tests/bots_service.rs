//! Tests for the bots' reads and writes (Plan.md P10.5, P11.2; ADR-0019,
//! S41, S48): each bot comes with what its state reads as now, from its row,
//! the latest report the server keeps, the agent its row names and that
//! agent's live stream. A bot without an agent reads Unplaced, with
//! why and its sticky state; a bot whose agent has no live stream reads
//! Unknown, with the agent's status, when it was last heard from and the
//! bot's last known state; a bot on a live agent reads its report on that
//! stream, or that none has come yet. A Resume of a bot that runs nowhere
//! takes the kept report with its stored state, and puts it back when its
//! commit fails. A user's chat line is judged, stored with its sender and
//! recorded before it's sent, and answered as its agent says; a refusal
//! before the commit writes nothing, and one after it keeps the row and the
//! entry (ADR-0020, S5–S7); a line its route doesn't take is refused before
//! anything is read (S10). The HTTP tests (`tests/auth/bots.rs`,
//! `tests/auth/bots/chat.rs`) cover the rest.
//!
//! Each test gets a fresh database from `#[sqlx::test]`. Time isn't paused:
//! sqlx's worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::num::NonZeroU32;
use core::time::Duration;
use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use fleet_core::agent::AgentStatus;
use fleet_core::audit::{AuditAction, AuditOutcome, RecordedValue};
use fleet_core::authz::{Actor, AuthzError, CommandAllowlist, Role};
use fleet_core::bot::{
    BotSnapshot, BotState, ChatRefusal, CommandRefusal, DesiredRunState, LastReport, LastState,
    Lifecycle, PauseReason, ShownState, StickyState, UnplacedWhy,
};
use fleet_core::chat::ChatMessage;
use fleet_core::disconnect::ConflictKind;
use fleet_core::id::{AccountId, AgentId, BotId, ChatRequestId, ModeId, SessionId, UserId};
use fleet_core::mode::ModePreset;
use fleet_core::pki::CertFingerprint;
use fleet_core::placement::Unplaced;
use fleet_core::system::RandomError;
use fleet_core::value::{AgentName, ServerAddress, Username};
use fleet_proto::control::ChatFailedReason;
use fleet_server::app::bots::{
    BotAction, BotService, BotWriteError, ChatHistory, ChatHistoryError, ChatOutcome, ChatRoute,
    ChatSendError, ChatWait, Controlled,
};
use fleet_server::app::events::{EventBus, LiveEvent, LiveEventKind};
use fleet_server::app::users::ActingUser;
use fleet_server::config::{LogConfig, LogFormat};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::accounts::{AccountReads, AccountWrites};
use fleet_server::ports::agents::{AgentReads, AgentWrites};
use fleet_server::ports::agents::{NewAgent, NewAgentCert};
use fleet_server::ports::audit::{AuditEntryId, AuditReads, AuditRecord, AuditWrites};
use fleet_server::ports::audit::{AuditLimit, AuditPage, NewAuditEntry};
use fleet_server::ports::bots::{Bot, NewBot};
use fleet_server::ports::bots::{BotReads, BotWrites};
use fleet_server::ports::chat::{ChatEntry, ChatMessageId, ChatReads, ChatWrites, NewChatMessage};
use fleet_server::ports::enrollment_tokens::{EnrollmentTokenReads, EnrollmentTokenWrites};
use fleet_server::ports::envelopes::{EnvelopeReads, EnvelopeWrites};
use fleet_server::ports::grants::{GrantReads, GrantWrites};
use fleet_server::ports::invites::{InviteReads, InviteWrites};
use fleet_server::ports::live_agents::{
    AnswerRefused, CommandRefused, HeldBot, LatestReport, LiveAgents, LiveStream, PermissionEnd,
    SessionAnswer,
};
use fleet_server::ports::login_attempts::{LoginAttemptReads, LoginAttemptWrites};
use fleet_server::ports::mfa::{MfaReads, MfaWrites};
use fleet_server::ports::modes::{ModeReads, ModeWrites};
use fleet_server::ports::page::{Page, PageLimit};
use fleet_server::ports::retention::{RetentionBatch, RetentionReads};
use fleet_server::ports::sessions::{NewSession, SessionReads, SessionWrites};
use fleet_server::ports::store::{Store, StoreError, UnauditedWrite, WriteTx};
use fleet_server::ports::tokens::TokenHash;
use fleet_server::ports::users::{NewUser, PasswordHash, UserReads, UserWrites};
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::log_buffer::LogBuffer;
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::SecretString;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::Notify;
use tokio::time::Instant;
use tracing_subscriber::layer::SubscriberExt as _;

const T0: &str = "2027-01-15T08:00:00Z";
/// How long a test waits for what must come: bounded, never a sleep.
const BOUND: core::time::Duration = core::time::Duration::from_secs(5);
/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";

fn at(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

fn agent(n: u8) -> AgentId {
    AgentId::new_v7(at(T0), [n; 10]).unwrap()
}

/// Bot `n`'s ID: later bots are newer.
fn bot(n: u8) -> BotId {
    BotId::new_v7(at(T0) + TimeDelta::seconds(i64::from(n)), [n; 10]).unwrap()
}

/// The entry a test commits its setup with. A test-only name, never listed
/// in `audit_actions!`.
fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from("test.bots_setup").unwrap(),
        AuditOutcome::Success,
    )
}

fn paused() -> LastState {
    LastState::Paused(PauseReason::Conflict {
        kind: ConflictKind::DuplicateLogin,
    })
}

/// The live streams the test sets up.
#[derive(Debug, Default)]
struct Live {
    streams: Mutex<HashMap<AgentId, LiveStream>>,
    held: Mutex<HashMap<(AgentId, BotId), HeldBot>>,
    latest: Mutex<HashMap<BotId, LatestReport>>,
    commands: Mutex<Vec<(AgentId, BotId, Lifecycle)>>,
    /// The chat lines queued, in order.
    chats: Mutex<Vec<(AgentId, BotId, ChatRequestId, ChatMessage)>>,
    /// How the next chat line is refused, when a test says so.
    chat_refusal: Mutex<Option<CommandRefused>>,
    /// How the agent answers a line, on which bus, when a test says so: at
    /// once, as the ingest publishes a `ChatSent` or `ChatFailed`.
    chat_reply: Mutex<Option<(EventBus, LiveEventKind)>>,
}

impl LiveAgents for Live {
    fn end(&self, _agent: AgentId, _reason: PermissionEnd) -> bool {
        false
    }

    fn live(&self, agent: AgentId) -> Option<LiveStream> {
        self.streams.lock().unwrap().get(&agent).copied()
    }

    fn held(&self, agent: AgentId, bot: BotId) -> Option<HeldBot> {
        self.held.lock().unwrap().get(&(agent, bot)).cloned()
    }

    fn latest(&self, bot: BotId) -> Option<LatestReport> {
        self.latest.lock().unwrap().get(&bot).cloned()
    }

    fn take_latest(&self, bot: BotId) -> Option<LatestReport> {
        self.latest.lock().unwrap().remove(&bot)
    }

    fn put_back_latest(&self, report: LatestReport) {
        self.latest
            .lock()
            .unwrap()
            .entry(report.snapshot.bot_id)
            .or_insert(report);
    }

    fn command(
        &self,
        agent: AgentId,
        generation: u64,
        bot: BotId,
        command: Lifecycle,
    ) -> Result<(), CommandRefused> {
        match self.held(agent, bot) {
            None => return Err(CommandRefused::NotHeld),
            Some(held) if held.generation != generation => {
                return Err(CommandRefused::NotConnected);
            }
            Some(_) => {}
        }
        self.commands.lock().unwrap().push((agent, bot, command));
        Ok(())
    }

    fn send_chat(
        &self,
        agent: AgentId,
        generation: u64,
        bot: BotId,
        request: ChatRequestId,
        message: &ChatMessage,
    ) -> Result<(), CommandRefused> {
        if let Some(refusal) = self.chat_refusal.lock().unwrap().take() {
            return Err(refusal);
        }
        match self.held(agent, bot) {
            None => return Err(CommandRefused::NotHeld),
            Some(held) if held.generation != generation => {
                return Err(CommandRefused::NotConnected);
            }
            Some(_) => {}
        }
        self.chats
            .lock()
            .unwrap()
            .push((agent, bot, request, message.clone()));
        if let Some((bus, reply)) = self.chat_reply.lock().unwrap().take() {
            let kind = match reply {
                LiveEventKind::ChatSent(_) => LiveEventKind::ChatSent(request),
                LiveEventKind::ChatFailed(_, reason) => LiveEventKind::ChatFailed(request, reason),
                other => other,
            };
            bus.publish(LiveEvent {
                bot,
                agent,
                generation: 2,
                at: at(T0),
                kind,
            });
        }
        Ok(())
    }

    /// The bots' service never answers a session request.
    fn answer_session(
        &self,
        _agent: AgentId,
        _generation: u64,
        _answer: SessionAnswer,
    ) -> Result<(), AnswerRefused> {
        Err(AnswerRefused::NotConnected)
    }
}

/// The database, whose write transactions follow `script`.
#[derive(Debug, Clone)]
struct FailingCommits {
    inner: Database,
    script: Script,
}

/// What a [`FailingCommits`] transaction does differently.
#[derive(Debug, Clone)]
enum Script {
    /// Its commit fails busy: nothing it wrote is kept.
    FailCommit,
    /// Its commit says so through the `Notify`, then never finishes: only
    /// its caller's drop ends it, which rolls it back.
    HangCommit(Arc<Notify>),
    /// Its `clear_sticky` answers that it changed nothing.
    ClearNothing,
    /// Its chat `append` stores nothing, as for a bot whose row no longer
    /// names the agent.
    AppendNothing,
}

/// A write transaction that follows `script`.
struct FailingTx {
    inner: Box<dyn WriteTx>,
    script: Script,
}

#[async_trait]
impl WriteTx for FailingTx {
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
        if matches!(self.script, Script::ClearNothing) {
            self
        } else {
            self.inner.bots()
        }
    }

    fn modes(&mut self) -> &mut dyn ModeWrites {
        self.inner.modes()
    }

    fn chat(&mut self) -> &mut dyn ChatWrites {
        if matches!(self.script, Script::AppendNothing) {
            self
        } else {
            self.inner.chat()
        }
    }

    fn audit(&mut self) -> &mut dyn AuditWrites {
        self.inner.audit()
    }

    async fn commit(
        self: Box<Self>,
        entry: fleet_server::ports::audit::NewAuditEntry,
    ) -> Result<AuditEntryId, StoreError> {
        match &self.script {
            // Dropping the transaction rolls it back.
            Script::FailCommit => Err(StoreError::Busy),
            Script::HangCommit(started) => {
                started.notify_one();
                std::future::pending().await
            }
            Script::ClearNothing | Script::AppendNothing => self.inner.commit(entry).await,
        }
    }

    async fn commit_unaudited(self: Box<Self>, reason: UnauditedWrite) -> Result<(), StoreError> {
        match &self.script {
            Script::FailCommit => Err(StoreError::Busy),
            Script::HangCommit(started) => {
                started.notify_one();
                std::future::pending().await
            }
            Script::ClearNothing | Script::AppendNothing => {
                self.inner.commit_unaudited(reason).await
            }
        }
    }
}

/// The chat log, storing nothing ([`Script::AppendNothing`]).
#[async_trait]
impl ChatWrites for FailingTx {
    async fn append(
        &mut self,
        _message: &NewChatMessage,
    ) -> Result<Option<ChatMessageId>, StoreError> {
        Ok(None)
    }
}

/// The bots' writes, through to the database but for `clear_sticky`, which
/// changes nothing ([`Script::ClearNothing`]).
#[async_trait]
impl BotWrites for FailingTx {
    async fn insert(&mut self, bot: &NewBot) -> Result<(), StoreError> {
        self.inner.bots().insert(bot).await
    }

    async fn find(&mut self, id: BotId) -> Result<Option<Bot>, StoreError> {
        self.inner.bots().find(id).await
    }

    async fn agent_of(&mut self, id: BotId) -> Result<Option<Option<AgentId>>, StoreError> {
        self.inner.bots().agent_of(id).await
    }

    async fn id_of_account(&mut self, account: AccountId) -> Result<Option<BotId>, StoreError> {
        self.inner.bots().id_of_account(account).await
    }

    async fn owned_by(&mut self, owner: UserId) -> Result<Vec<BotId>, StoreError> {
        self.inner.bots().owned_by(owner).await
    }

    async fn assigned_to(&mut self, agent: AgentId) -> Result<Vec<BotId>, StoreError> {
        self.inner.bots().assigned_to(agent).await
    }

    async fn assign(&mut self, bot: BotId, agent: AgentId) -> Result<bool, StoreError> {
        self.inner.bots().assign(bot, agent).await
    }

    async fn record_state(
        &mut self,
        bot: BotId,
        agent: AgentId,
        last: &LastReport,
    ) -> Result<bool, StoreError> {
        self.inner.bots().record_state(bot, agent, last).await
    }

    async fn detach(&mut self, bot: BotId, agent: AgentId) -> Result<bool, StoreError> {
        self.inner.bots().detach(bot, agent).await
    }

    async fn stray(
        &mut self,
        bot: BotId,
        agent: AgentId,
        at: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        self.inner.bots().stray(bot, agent, at).await
    }

    async fn release_all(&mut self, agent: AgentId) -> Result<Vec<BotId>, StoreError> {
        self.inner.bots().release_all(agent).await
    }

    async fn set_server(&mut self, bot: BotId, server: &ServerAddress) -> Result<bool, StoreError> {
        self.inner.bots().set_server(bot, server).await
    }

    async fn set_mode(&mut self, bot: BotId, mode: ModeId) -> Result<bool, StoreError> {
        self.inner.bots().set_mode(bot, mode).await
    }

    async fn accounts_with_mode(&mut self, mode: ModeId) -> Result<Vec<AccountId>, StoreError> {
        self.inner.bots().accounts_with_mode(mode).await
    }

    async fn set_desired(
        &mut self,
        bot: BotId,
        desired: DesiredRunState,
    ) -> Result<bool, StoreError> {
        self.inner.bots().set_desired(bot, desired).await
    }

    async fn clear_sticky(&mut self, _bot: BotId, _last: &LastReport) -> Result<bool, StoreError> {
        Ok(false)
    }
}

#[async_trait]
impl Store for FailingCommits {
    async fn write(&self) -> Result<Box<dyn WriteTx>, StoreError> {
        Ok(Box::new(FailingTx {
            inner: self.inner.write().await?,
            script: self.script.clone(),
        }))
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

    fn envelopes(&self) -> &dyn EnvelopeReads {
        self.inner.envelopes()
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

struct Setup {
    db: Database,
    live: Arc<Live>,
    clock: ManualClock,
    random: SeededRandom,
    events: EventBus,
}

impl Setup {
    /// One user, and agents 1 and 2 with a certificate valid for 60 days,
    /// heard from last at T0.
    async fn new(options: SqliteConnectOptions) -> Self {
        let db = Database::connect(options, DatabaseOptions::default())
            .await
            .unwrap();
        let mut tx = db.write().await.unwrap();
        tx.users()
            .insert(&NewUser {
                id: owner(),
                username: Username::try_from("owner").unwrap(),
                password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
                role: Role::Owner,
                created_at: at(T0),
            })
            .await
            .unwrap();
        for n in 1..=2 {
            tx.agents()
                .insert(&NewAgent {
                    id: agent(n),
                    name: AgentName::try_from(format!("agent-{n}").as_str()).unwrap(),
                    created_at: at(T0),
                })
                .await
                .unwrap();
            tx.agents()
                .insert_cert(&NewAgentCert {
                    fingerprint: CertFingerprint::from_digest([n; 32]),
                    agent: agent(n),
                    not_after: at(T0) + TimeDelta::days(60),
                    created_at: at(T0),
                })
                .await
                .unwrap();
        }
        tx.commit(setup_entry()).await.unwrap();
        Self {
            db,
            live: Arc::new(Live::default()),
            clock: ManualClock::new(at(T0) + TimeDelta::hours(1)),
            random: SeededRandom::new(5),
            events: EventBus::new(),
        }
    }

    fn service(&self, allow_offline: bool) -> BotService {
        BotService::new(
            Arc::new(self.clock.clone()),
            Arc::new(self.random.clone()),
            Arc::new(self.db.clone()),
            Arc::clone(&self.live) as Arc<dyn LiveAgents>,
            self.events.clone(),
            allow_offline,
        )
    }

    /// Adds the offline account `AfkBot<n>` and its bot `n`: no server,
    /// stopped, unassigned.
    async fn bot(&self, n: u8) -> BotId {
        let account = AccountId::new_v7(at(T0), [n; 10]).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.accounts()
            .insert(&McAccount {
                id: account,
                owner: owner(),
                name: format!("AfkBot{n}").parse().unwrap(),
                created_at: at(T0),
                credential: AccountCredential::Offline,
            })
            .await
            .unwrap();
        tx.bots()
            .insert(&NewBot {
                id: bot(n),
                account,
                mode: ModePreset::Afk.id(),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        bot(n)
    }

    /// Gives `bot` a server and sets it running, as E's endpoints will.
    async fn run(&self, bot: BotId) {
        let id = bot.as_uuid().hyphenated().to_string();
        sqlx::query!(
            "UPDATE bots SET server_address = 'play.example.com', desired_state = 'running' WHERE id = ?",
            id
        )
        .execute(self.db.write_pool())
        .await
        .unwrap();
    }

    /// Assigns `bot` to `agent`, with `last` as its stored last state.
    async fn assign(&self, bot: BotId, agent: AgentId, last: Option<LastReport>) {
        let mut tx = self.db.write().await.unwrap();
        assert!(tx.bots().assign(bot, agent).await.unwrap());
        if let Some(last) = last {
            assert!(tx.bots().record_state(bot, agent, &last).await.unwrap());
        }
        tx.commit(setup_entry()).await.unwrap();
    }

    /// Stores `last` for `bot`, which ends up unassigned.
    async fn rested(&self, bot: BotId, last: LastReport) {
        self.assign(bot, agent(1), Some(last)).await;
        let mut tx = self.db.write().await.unwrap();
        assert!(tx.bots().detach(bot, agent(1)).await.unwrap());
        tx.commit(setup_entry()).await.unwrap();
    }

    async fn disable(&self, agent: AgentId) {
        let id = agent.as_uuid().hyphenated().to_string();
        sqlx::query!("UPDATE agents SET enabled = 0 WHERE id = ?", id)
            .execute(self.db.write_pool())
            .await
            .unwrap();
    }

    async fn shown(&self, bot: BotId) -> ShownState {
        self.service(true)
            .view(&the_owner(), bot)
            .await
            .unwrap()
            .unwrap()
            .shown
    }
}

fn owner() -> UserId {
    UserId::new_v7(at(T0), [1; 10]).unwrap()
}

/// The setup's one user, the Owner, who sees every bot.
fn the_owner() -> Actor {
    Actor {
        id: owner(),
        role: Role::Owner,
    }
}

fn last(state: LastState, minutes: i64) -> LastReport {
    LastReport {
        state,
        at: at(T0) + TimeDelta::minutes(minutes),
    }
}

// --- Without an agent ---------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_bot_without_an_agent_reads_unplaced_with_why(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;

    let shown = setup.shown(bot).await;

    assert_eq!(
        shown,
        ShownState::Unplaced {
            why: UnplacedWhy::Unqualified(Unplaced::NoServer),
            sticky: None,
        }
    );
}

#[sqlx::test(migrations = false)]
async fn a_qualifying_bot_without_an_agent_awaits_one_in_its_sticky_state(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.run(bot).await;
    setup.rested(bot, last(paused(), 5)).await;

    let shown = setup.shown(bot).await;

    assert_eq!(
        shown,
        ShownState::Unplaced {
            why: UnplacedWhy::AwaitingAgent,
            sticky: Some(StickyState::Paused(PauseReason::Conflict {
                kind: ConflictKind::DuplicateLogin
            })),
        }
    );
}

#[sqlx::test(migrations = false)]
async fn an_offline_bot_reads_unplaced_while_offline_accounts_are_off(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.run(bot).await;

    let view = setup
        .service(false)
        .view(&the_owner(), bot)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        view.shown,
        ShownState::Unplaced {
            why: UnplacedWhy::Unqualified(Unplaced::OfflineNotAllowed),
            sticky: None,
        }
    );
}

// --- On an agent ------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_bot_on_an_agent_without_a_live_stream_reads_unknown_with_its_last_state(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.run(bot).await;
    setup
        .assign(bot, agent(1), Some(last(LastState::Online, 7)))
        .await;

    let shown = setup.shown(bot).await;

    assert_eq!(
        shown,
        ShownState::Unknown {
            agent: AgentStatus::Offline,
            agent_last_seen: None,
            last_known: Some(last(LastState::Online, 7)),
        }
    );
}

#[sqlx::test(migrations = false)]
async fn a_bot_on_a_disabled_agent_reads_unknown_even_while_its_stream_lives(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.assign(bot, agent(1), None).await;
    setup.disable(agent(1)).await;
    let seen = at(T0) + TimeDelta::minutes(30);
    setup.live.streams.lock().unwrap().insert(
        agent(1),
        LiveStream {
            connected_at: at(T0),
            last_seen: seen,
        },
    );

    let shown = setup.shown(bot).await;

    assert_eq!(
        shown,
        ShownState::Unknown {
            agent: AgentStatus::Disabled,
            agent_last_seen: Some(seen),
            last_known: None,
        }
    );
}

#[sqlx::test(migrations = false)]
async fn a_bot_on_a_live_agent_reads_its_report_on_that_stream(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.run(bot).await;
    setup.assign(bot, agent(1), None).await;
    let report = BotSnapshot {
        bot_id: bot,
        state: BotState::Stopped,
        since: at(T0),
        last_disconnect: None,
    };
    setup.live.streams.lock().unwrap().insert(
        agent(1),
        LiveStream {
            connected_at: at(T0),
            last_seen: at(T0),
        },
    );
    setup.live.held.lock().unwrap().insert(
        (agent(1), bot),
        HeldBot {
            since: at(T0),
            report: Some(report.clone()),
            generation: 1,
        },
    );

    let shown = setup.shown(bot).await;

    assert_eq!(shown, ShownState::Reported(report));
}

#[sqlx::test(migrations = false)]
async fn a_bot_on_a_live_agent_awaits_its_report_since_it_was_given(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let given = setup.bot(1).await;
    let welcomed = setup.bot(2).await;
    setup.assign(given, agent(1), None).await;
    setup.assign(welcomed, agent(1), None).await;
    let connected = at(T0) + TimeDelta::minutes(1);
    let held = at(T0) + TimeDelta::minutes(2);
    setup.live.streams.lock().unwrap().insert(
        agent(1),
        LiveStream {
            connected_at: connected,
            last_seen: connected,
        },
    );
    setup.live.held.lock().unwrap().insert(
        (agent(1), given),
        HeldBot {
            since: held,
            report: None,
            generation: 1,
        },
    );

    let given = setup.shown(given).await;
    let welcomed = setup.shown(welcomed).await;

    assert_eq!(given, ShownState::AwaitingReport { since: held });
    assert_eq!(
        welcomed,
        ShownState::AwaitingReport { since: connected },
        "not held yet: since its stream was welcomed"
    );
}

// --- Reading ----------------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_bot_that_doesnt_exist_reads_as_none(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;

    let view = setup
        .service(true)
        .view(&the_owner(), bot(9))
        .await
        .unwrap();

    assert_eq!(view, None);
}

#[sqlx::test(migrations = false)]
async fn the_list_pages_the_bots_newest_first_each_with_its_state(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    for n in 1..=3 {
        setup.bot(n).await;
    }
    setup.assign(bot(2), agent(2), None).await;
    let service = setup.service(true);

    let first = service
        .list(
            &the_owner(),
            Page {
                before: None,
                limit: PageLimit::new(2).unwrap(),
            },
        )
        .await
        .unwrap();
    let second = service
        .list(
            &the_owner(),
            Page {
                before: first.next,
                limit: PageLimit::new(2).unwrap(),
            },
        )
        .await
        .unwrap();

    let ids = |items: &[fleet_server::app::bots::BotView]| {
        items
            .iter()
            .map(|view| view.record.bot.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&first.items), [bot(3), bot(2)]);
    assert_eq!(first.next, Some(bot(2)));
    assert_eq!(ids(&second.items), [bot(1)]);
    assert_eq!(second.next, None);
    assert!(matches!(
        first.items[1].shown,
        ShownState::Unknown {
            agent: AgentStatus::Offline,
            ..
        }
    ));
}

/// What a live events socket reads (ADR-0020, S41): one query for many
/// bots, each read exactly as `view` reads it, unknown IDs left out.
#[sqlx::test(migrations = false)]
async fn the_views_of_many_bots_are_each_what_view_reads_in_id_order(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    for n in 1..=4 {
        setup.bot(n).await;
    }
    setup.assign(bot(2), agent(2), None).await;
    setup.assign(bot(3), agent(2), None).await;
    let service = setup.service(true);

    let views = service
        .views(&the_owner(), &[bot(3), bot(9), bot(1), bot(2)])
        .await
        .unwrap();
    let none = service.views(&the_owner(), &[]).await.unwrap();

    let mut expected = Vec::new();
    for n in [1, 2, 3] {
        expected.push(service.view(&the_owner(), bot(n)).await.unwrap().unwrap());
    }
    assert_eq!(views, expected);
    assert_eq!(none, Vec::new());
}

#[sqlx::test(migrations = false)]
async fn a_read_that_fails_is_an_error(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    let service = setup.service(true);
    setup.db.close().await;

    let view = service.view(&the_owner(), bot).await;
    let views = service.views(&the_owner(), &[bot]).await;
    assert!(
        matches!(views, Err(StoreError::Backend(_) | StoreError::Busy)),
        "{views:?}"
    );
    let list = service
        .list(
            &the_owner(),
            Page {
                before: None,
                limit: PageLimit::new(2).unwrap(),
            },
        )
        .await;

    assert!(
        matches!(view, Err(StoreError::Backend(_) | StoreError::Busy)),
        "{view:?}"
    );
    assert!(
        matches!(
            list,
            Err(fleet_server::app::bots::ListBotsError::Store(
                StoreError::Backend(_) | StoreError::Busy
            ))
        ),
        "{list:?}"
    );
}

// --- What the server keeps in memory (R35) and the clear's put-back (S48) ------------------

/// A report kept from `agent(1)`'s stream of `bot` in `state`, at `minutes`.
fn kept(bot: BotId, state: BotState, minutes: i64) -> LatestReport {
    LatestReport {
        agent: agent(1),
        generation: 1,
        snapshot: BotSnapshot {
            bot_id: bot,
            state,
            since: at(T0),
            last_disconnect: None,
        },
        at: at(T0) + TimeDelta::minutes(minutes),
    }
}

fn paused_state() -> BotState {
    BotState::Paused {
        reason: PauseReason::Conflict {
            kind: ConflictKind::DuplicateLogin,
        },
    }
}

#[sqlx::test(migrations = false)]
async fn a_bot_reads_the_newer_of_its_stored_state_and_the_kept_report(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let newer = setup.bot(1).await;
    let older = setup.bot(2).await;
    setup.rested(newer, last(LastState::Stopped, 5)).await;
    setup.rested(older, last(LastState::Stopped, 9)).await;
    setup
        .live
        .latest
        .lock()
        .unwrap()
        .insert(newer, kept(newer, paused_state(), 7));
    setup
        .live
        .latest
        .lock()
        .unwrap()
        .insert(older, kept(older, paused_state(), 7));

    let newer = setup.shown(newer).await;
    let older = setup.shown(older).await;

    assert!(
        matches!(
            newer,
            ShownState::Unplaced {
                sticky: Some(StickyState::Paused(_)),
                ..
            }
        ),
        "the kept Paused is newer: {newer:?}"
    );
    assert!(
        matches!(older, ShownState::Unplaced { sticky: None, .. }),
        "the stored Stopped is newer: {older:?}"
    );
}

impl Setup {
    /// The setup's Owner acting through a live session.
    async fn acting(&self) -> ActingUser {
        let session = SessionId::new_v7(at(T0), [7; 10]).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.sessions()
            .insert(&NewSession {
                id: session,
                user_id: owner(),
                access_hash: TokenHash::from_bytes([1; 32]),
                refresh_hash: TokenHash::from_bytes([2; 32]),
                created_at: at(T0),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        ActingUser {
            actor: the_owner(),
            session_id: session,
            ip: IpAddr::from([192, 0, 2, 10]),
        }
    }

    /// How many `bot.resume` entries the audit log holds.
    async fn resumes(&self) -> usize {
        self.db
            .audit()
            .list(AuditPage {
                before: None,
                limit: AuditLimit::new(100).unwrap(),
            })
            .await
            .unwrap()
            .iter()
            .filter(|record| record.action == AuditAction::BOT_RESUME)
            .count()
    }

    fn service_over(&self, store: Arc<dyn Store>) -> BotService {
        BotService::new(
            Arc::new(self.clock.clone()),
            Arc::new(self.random.clone()),
            store,
            Arc::clone(&self.live) as Arc<dyn LiveAgents>,
            self.events.clone(),
            true,
        )
    }
}

#[sqlx::test(migrations = false)]
async fn a_resume_of_a_bot_that_runs_nowhere_takes_the_kept_report_with_its_stored_state(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.rested(bot, last(paused(), 5)).await;
    setup
        .live
        .latest
        .lock()
        .unwrap()
        .insert(bot, kept(bot, paused_state(), 5));
    let by = setup.acting().await;

    let done = setup
        .service(true)
        .control(&by, bot, BotAction::Command(Lifecycle::Resume))
        .await
        .unwrap();

    assert_eq!(done, Controlled::Cleared);
    assert_eq!(setup.db.bots().find(bot).await.unwrap().unwrap().last, None);
    assert_eq!(setup.live.latest(bot), None);
}

#[sqlx::test(migrations = false)]
async fn a_resume_whose_commit_fails_puts_the_kept_report_back_and_keeps_the_stored_state(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.rested(bot, last(paused(), 5)).await;
    let report = kept(bot, paused_state(), 5);
    setup
        .live
        .latest
        .lock()
        .unwrap()
        .insert(bot, report.clone());
    let by = setup.acting().await;
    let failing = FailingCommits {
        inner: setup.db.clone(),
        script: Script::FailCommit,
    };

    let refused = setup
        .service_over(Arc::new(failing))
        .control(&by, bot, BotAction::Command(Lifecycle::Resume))
        .await;

    assert!(
        matches!(refused, Err(BotWriteError::Store(StoreError::Busy))),
        "{refused:?}"
    );
    assert_eq!(setup.live.latest(bot), Some(report));
    assert_eq!(
        setup.db.bots().find(bot).await.unwrap().unwrap().last,
        Some(last(paused(), 5))
    );
}

/// A resume whose caller is dropped mid-commit (a timeout, a client gone)
/// rolls back, and the kept report it took comes back (PR #54's review).
#[sqlx::test(migrations = false)]
async fn a_resume_dropped_during_its_commit_puts_the_kept_report_back(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.rested(bot, last(paused(), 5)).await;
    let report = kept(bot, paused_state(), 5);
    setup
        .live
        .latest
        .lock()
        .unwrap()
        .insert(bot, report.clone());
    let by = setup.acting().await;
    let started = Arc::new(Notify::new());
    let service = setup.service_over(Arc::new(FailingCommits {
        inner: setup.db.clone(),
        script: Script::HangCommit(Arc::clone(&started)),
    }));

    tokio::time::timeout(BOUND, async {
        tokio::select! {
            done = service.control(&by, bot, BotAction::Command(Lifecycle::Resume)) => {
                panic!("the commit never finishes: {done:?}")
            }
            () = started.notified() => {}
        }
    })
    .await
    .unwrap();

    assert_eq!(setup.live.latest(bot), Some(report));
    assert_eq!(
        setup.db.bots().find(bot).await.unwrap().unwrap().last,
        Some(last(paused(), 5))
    );
}

/// A clear that finds nothing to clear refuses the resume: no entry, and
/// the kept report stays (PR #54's review).
#[sqlx::test(migrations = false)]
async fn a_resume_whose_clear_changes_nothing_is_refused_and_takes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.rested(bot, last(paused(), 5)).await;
    let report = kept(bot, paused_state(), 5);
    setup
        .live
        .latest
        .lock()
        .unwrap()
        .insert(bot, report.clone());
    let by = setup.acting().await;
    let service = setup.service_over(Arc::new(FailingCommits {
        inner: setup.db.clone(),
        script: Script::ClearNothing,
    }));

    let refused = service
        .control(&by, bot, BotAction::Command(Lifecycle::Resume))
        .await;

    assert!(
        matches!(
            refused,
            Err(BotWriteError::Refused(CommandRefusal::Unreachable))
        ),
        "{refused:?}"
    );
    assert_eq!(setup.live.latest(bot), Some(report));
    assert_eq!(setup.resumes().await, 0, "no entry");
}

// --- Chat (P11.3; ADR-0020, S5–S7) ------------------------------------------------------

impl Setup {
    /// Makes `bot` run on agent 1's live stream, generation 1, which
    /// reported it in `state`.
    async fn held_in(&self, bot: BotId, state: BotState) {
        self.run(bot).await;
        self.assign(bot, agent(1), None).await;
        self.live.streams.lock().unwrap().insert(
            agent(1),
            LiveStream {
                connected_at: at(T0),
                last_seen: at(T0),
            },
        );
        self.live.held.lock().unwrap().insert(
            (agent(1), bot),
            HeldBot {
                since: at(T0),
                report: Some(BotSnapshot {
                    bot_id: bot,
                    state,
                    since: at(T0),
                    last_disconnect: None,
                }),
                generation: 1,
            },
        );
    }

    /// The agent answers the next line with `reply`.
    fn reply(&self, reply: LiveEventKind) {
        *self.live.chat_reply.lock().unwrap() = Some((self.events.clone(), reply));
    }

    /// The service, waiting 50 ms for an answer, with `/spawn` listed.
    fn chatting(&self) -> BotService {
        self.chatting_over(Arc::new(self.db.clone()))
    }

    fn chatting_over(&self, store: Arc<dyn Store>) -> BotService {
        self.service_over(store)
            .with_allowlist(CommandAllowlist::try_new(&["/spawn"]).unwrap())
            .with_chat_wait(ChatWait::default().with_answer(Duration::from_millis(50)))
    }

    /// `bot`'s chat, newest first.
    async fn lines(&self, bot: BotId) -> Vec<ChatEntry> {
        self.db
            .chat()
            .page(
                bot,
                Page {
                    before: None,
                    limit: PageLimit::new(100).unwrap(),
                },
            )
            .await
            .unwrap()
            .into_iter()
            .map(|line| line.record.entry)
            .collect()
    }

    /// The chat entries in the audit log, oldest first.
    async fn chat_entries(&self) -> Vec<AuditRecord> {
        let mut records: Vec<AuditRecord> = self
            .db
            .audit()
            .list(AuditPage {
                before: None,
                limit: AuditLimit::new(100).unwrap(),
            })
            .await
            .unwrap()
            .into_iter()
            .filter(|record| {
                record.action == AuditAction::BOT_SEND_CHAT
                    || record.action == AuditAction::BOT_SEND_COMMAND
            })
            .collect();
        records.reverse();
        records
    }
}

fn online() -> BotState {
    BotState::Online {
        since: at(T0),
        attempt: NonZeroU32::MIN,
    }
}

fn line(text: &str) -> ChatMessage {
    ChatMessage::try_from(text).unwrap()
}

fn users_line(text: &str) -> ChatEntry {
    ChatEntry::UserSent {
        message: line(text),
        by: Some(owner()),
    }
}

/// An entry's metadata as text values by key.
fn metadata(record: &AuditRecord) -> BTreeMap<String, String> {
    record
        .metadata
        .entries()
        .iter()
        .map(|(key, value)| {
            let value = match value {
                RecordedValue::Text(text) | RecordedValue::Unrecognized(text) => text.clone(),
                RecordedValue::Int(int) => int.to_string(),
                RecordedValue::Bool(flag) => flag.to_string(),
            };
            (key.clone(), value)
        })
        .collect()
}

/// A request ID the fake agent replaces with the line's own.
fn any_request() -> ChatRequestId {
    ChatRequestId::new_v7(at(T0), [0; 10]).unwrap()
}

#[sqlx::test(migrations = false)]
async fn a_line_is_stored_and_recorded_before_it_is_sent_and_reads_sent_when_its_agent_says_so(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.held_in(bot, online()).await;
    setup.reply(LiveEventKind::ChatSent(any_request()));
    let by = setup.acting().await;

    let done = setup
        .chatting()
        .send_chat(&by, bot, &line("hello"), ChatRoute::Chat, Instant::now())
        .await
        .unwrap();

    assert_eq!(done.outcome, ChatOutcome::Sent);
    let sent = setup.live.chats.lock().unwrap().clone();
    assert_eq!(sent, [(agent(1), bot, done.request, line("hello"))]);
    assert_eq!(setup.lines(bot).await, [users_line("hello")]);
    let entries = setup.chat_entries().await;
    assert_eq!(entries.len(), 1);
    let entry = entries.first().unwrap();
    assert_eq!(entry.action, AuditAction::BOT_SEND_CHAT);
    assert_eq!(entry.actor, Some(owner()));
    assert_eq!(
        metadata(entry),
        BTreeMap::from([
            ("request".to_owned(), done.request.to_string()),
            ("message".to_owned(), done.message.get().to_string()),
            ("command".to_owned(), "none".to_owned()),
        ])
    );
}

#[sqlx::test(migrations = false)]
async fn a_line_without_an_answer_in_time_reads_pending_and_is_kept(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.held_in(bot, online()).await;
    let by = setup.acting().await;

    let done = setup
        .chatting()
        .send_chat(&by, bot, &line("anyone?"), ChatRoute::Chat, Instant::now())
        .await
        .unwrap();

    assert_eq!(done.outcome, ChatOutcome::Pending);
    assert_eq!(setup.lines(bot).await, [users_line("anyone?")]);
    assert_eq!(setup.chat_entries().await.len(), 1);
}

/// What the agent answered comes back as it is, and the row and the entry
/// stay: the line is what the user asked (S7).
#[sqlx::test(migrations = false)]
async fn an_agents_refusal_comes_back_with_its_reason_and_keeps_the_row_and_entry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let by = setup.acting().await;
    let reasons = [
        ChatFailedReason::RateLimited,
        ChatFailedReason::ChatUnavailable,
    ];

    for (n, reason) in (1_u8..).zip(reasons) {
        let bot = setup.bot(n).await;
        setup.held_in(bot, online()).await;
        setup.reply(LiveEventKind::ChatFailed(any_request(), reason));

        let refused = setup
            .chatting()
            .send_chat(
                &by,
                bot,
                &line("/me waves"),
                ChatRoute::Command,
                Instant::now(),
            )
            .await;

        assert!(
            matches!(refused, Err(ChatSendError::Failed(found)) if found == reason),
            "{reason:?}: {refused:?}"
        );
        assert_eq!(setup.lines(bot).await, [users_line("/me waves")]);
        assert_eq!(setup.chat_entries().await.len(), usize::from(n));
    }
}

/// A bot that isn't online: refused before the transaction writes or
/// records anything, and nothing is sent.
#[sqlx::test(migrations = false)]
async fn a_bot_that_isnt_online_refuses_a_line_before_anything_is_written(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let by = setup.acting().await;
    let stopped = setup.bot(1).await;
    let connecting = setup.bot(2).await;
    setup
        .held_in(
            connecting,
            BotState::Connecting {
                attempt: NonZeroU32::MIN,
                auth_retried: false,
            },
        )
        .await;

    for bot in [stopped, connecting] {
        let refused = setup
            .chatting()
            .send_chat(&by, bot, &line("hello"), ChatRoute::Chat, Instant::now())
            .await;

        assert!(
            matches!(refused, Err(ChatSendError::Refused(ChatRefusal::NotOnline))),
            "{refused:?}"
        );
        assert_eq!(setup.lines(bot).await, Vec::<ChatEntry>::new());
    }
    assert_eq!(setup.chat_entries().await.len(), 0);
    assert_eq!(setup.live.chats.lock().unwrap().len(), 0);
}

#[sqlx::test(migrations = false)]
async fn a_bot_whose_agents_stream_is_gone_refuses_a_line_before_anything_is_written(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.held_in(bot, online()).await;
    setup.live.streams.lock().unwrap().clear();
    let by = setup.acting().await;

    let refused = setup
        .chatting()
        .send_chat(&by, bot, &line("hello"), ChatRoute::Chat, Instant::now())
        .await;

    assert!(
        matches!(
            refused,
            Err(ChatSendError::Refused(ChatRefusal::Unreachable))
        ),
        "{refused:?}"
    );
    assert_eq!(setup.lines(bot).await, Vec::<ChatEntry>::new());
    assert_eq!(setup.chat_entries().await.len(), 0);
    assert_eq!(setup.live.chats.lock().unwrap().len(), 0);
}

/// A stream that ended between the commit and the send: the line was
/// stored and recorded, and stays (S7).
#[sqlx::test(migrations = false)]
async fn a_refusal_after_the_commit_keeps_the_row_and_the_entry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let by = setup.acting().await;
    let refusals = [
        CommandRefused::NotHeld,
        CommandRefused::NotConnected,
        CommandRefused::QueueFull,
        CommandRefused::Unsendable,
    ];

    for (n, refusal) in (1_u8..).zip(refusals) {
        let bot = setup.bot(n).await;
        setup.held_in(bot, online()).await;
        *setup.live.chat_refusal.lock().unwrap() = Some(refusal);

        let refused = setup
            .chatting()
            .send_chat(&by, bot, &line("hello"), ChatRoute::Chat, Instant::now())
            .await;

        let as_expected = match refusal {
            CommandRefused::Unsendable => matches!(
                refused,
                Err(ChatSendError::Write(BotWriteError::Unsendable))
            ),
            CommandRefused::NotHeld | CommandRefused::NotConnected | CommandRefused::QueueFull => {
                matches!(
                    refused,
                    Err(ChatSendError::Refused(ChatRefusal::Unreachable))
                )
            }
        };
        assert!(as_expected, "{refusal:?}: {refused:?}");
        assert_eq!(setup.lines(bot).await, [users_line("hello")]);
        assert_eq!(setup.chat_entries().await.len(), usize::from(n));
    }
}

/// The chat events a send puts on the bus, in the order they came.
fn chat_events(
    events: &mut tokio::sync::broadcast::Receiver<Arc<fleet_server::app::events::BusEvent>>,
) -> Vec<LiveEventKind> {
    let mut kinds = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let Some(event) = event.agent() {
            kinds.push(event.kind.clone());
        }
    }
    kinds
}

/// S34: the user's line goes on the bus with its sender and request once
/// it's stored, before it goes to the agent, so its answer, which the agent
/// gives while it's sent, comes after it.
#[sqlx::test(migrations = false)]
async fn a_users_line_is_on_the_bus_before_its_send_with_its_sender_and_request(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let by = setup.acting().await;
    let bot = setup.bot(1).await;
    setup.held_in(bot, online()).await;
    setup.reply(LiveEventKind::ChatSent(
        ChatRequestId::new_v7(at(T0), [0; 10]).unwrap(),
    ));
    let mut events = setup.events.subscribe();

    let done = setup
        .chatting()
        .send_chat(
            &by,
            bot,
            &line("/login secret"),
            ChatRoute::Command,
            Instant::now(),
        )
        .await
        .unwrap();

    let kinds = chat_events(&mut events);
    let [
        LiveEventKind::Chat {
            line: sent,
            request,
        },
        LiveEventKind::ChatSent(answered),
    ] = kinds.as_slice()
    else {
        panic!("not the line, then its answer: {kinds:?}");
    };
    assert_eq!((*request, *answered), (Some(done.request), done.request));
    assert_eq!(sent.record.id, done.message);
    assert_eq!(sent.record.bot, bot);
    assert_eq!(sent.record.entry, users_line("/login secret"));
    assert_eq!(sent.sent_by, Some(Username::try_from("owner").unwrap()));
    assert_eq!(
        setup.lines(bot).await,
        core::slice::from_ref(&sent.record.entry),
        "the line the bus carries is the stored one"
    );
}

/// S34, R35: a line refused after its commit goes on the bus, then the
/// refusal: unreachable, or unsendable.
#[sqlx::test(migrations = false)]
async fn a_line_refused_after_its_commit_is_followed_on_the_bus_by_why(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    use fleet_server::app::events::NotSent;

    let setup = Setup::new(options).await;
    let by = setup.acting().await;
    let refusals = [
        (CommandRefused::NotHeld, NotSent::Unreachable),
        (CommandRefused::NotConnected, NotSent::Unreachable),
        (CommandRefused::QueueFull, NotSent::Unreachable),
        (CommandRefused::Unsendable, NotSent::Unsendable),
    ];

    for (n, (refusal, expected)) in (1_u8..).zip(refusals) {
        let bot = setup.bot(n).await;
        setup.held_in(bot, online()).await;
        *setup.live.chat_refusal.lock().unwrap() = Some(refusal);
        let mut events = setup.events.subscribe();

        let refused = setup
            .chatting()
            .send_chat(&by, bot, &line("hello"), ChatRoute::Chat, Instant::now())
            .await;

        assert!(refused.is_err(), "{refusal:?}");
        let kinds = chat_events(&mut events);
        let [
            LiveEventKind::Chat {
                request: Some(line_request),
                ..
            },
            LiveEventKind::ChatNotSent { request, why },
        ] = kinds.as_slice()
        else {
            panic!("{refusal:?}: not the line, then why: {kinds:?}");
        };
        assert_eq!((line_request, why), (request, &expected), "{refusal:?}");
    }
}

/// The history: a bot's chat newest first, with its account as the viewer
/// sees it, for whoever may see the bot (P11.3; ADR-0020, U3).
#[sqlx::test(migrations = false)]
async fn the_history_is_the_bots_chat_newest_first_with_its_account(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.held_in(bot, online()).await;
    let mut tx = setup.db.write().await.unwrap();
    let mut stored = Vec::new();
    for (n, text) in (1_i64..).zip(["one", "two", "three"]) {
        let row = NewChatMessage {
            bot,
            agent: agent(1),
            at: at(T0) + TimeDelta::seconds(n),
            entry: users_line(text),
        };
        stored.push(tx.chat().append(&row).await.unwrap().unwrap());
    }
    tx.commit(setup_entry()).await.unwrap();

    let first = setup
        .chatting()
        .chat(
            &the_owner(),
            bot,
            Page {
                before: None,
                limit: PageLimit::new(2).unwrap(),
            },
        )
        .await
        .unwrap();
    let rest = setup
        .chatting()
        .chat(
            &the_owner(),
            bot,
            Page {
                before: first.lines.next,
                limit: PageLimit::new(2).unwrap(),
            },
        )
        .await
        .unwrap();

    let ids = |page: &ChatHistory| {
        page.lines
            .items
            .iter()
            .map(|line| line.record.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&first), [stored[2], stored[1]]);
    assert_eq!(first.lines.next, Some(stored[1]));
    assert_eq!(ids(&rest), [stored[0]]);
    assert_eq!(rest.lines.next, None);
    assert_eq!(first.account.owner.id, owner());
    assert_eq!(
        first.lines.items[0].sent_by.as_ref().map(Username::as_str),
        Some("owner")
    );
}

/// An unknown bot has no history, and one the viewer may not see is
/// refused as unknown (`authorize()`'s 404).
#[sqlx::test(migrations = false)]
async fn an_unknown_or_unseen_bots_history_is_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let known = setup.bot(1).await;
    let stranger = Actor {
        id: UserId::new_v7(at(T0), [9; 10]).unwrap(),
        role: Role::Member,
    };
    let page = Page {
        before: None,
        limit: PageLimit::new(10).unwrap(),
    };

    let unknown = setup.chatting().chat(&the_owner(), bot(99), page).await;
    let unseen = setup.chatting().chat(&stranger, known, page).await;

    assert!(
        matches!(unknown, Err(ChatHistoryError::NotFound)),
        "{unknown:?}"
    );
    assert!(
        matches!(unseen, Err(ChatHistoryError::Denied(AuthzError::NotFound))),
        "{unseen:?}"
    );
}

/// The message of the line a committed line that wasn't sent logs.
const NOT_SENT: &str =
    "a user's chat line was stored and recorded, but its stream no longer took it";

/// The server's log layer at `debug` into a buffer, as this thread's
/// default subscriber.
fn capture() -> (LogBuffer, tracing::subscriber::DefaultGuard) {
    let buffer = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: "debug".parse().unwrap(),
    };
    let subscriber =
        tracing_subscriber::registry().with(layer_with(&config, &NoRules, false, buffer.clone()));
    let guard = tracing::subscriber::set_default(subscriber);
    (buffer, guard)
}

/// A committed line its stream no longer took is logged at `debug` with its
/// IDs and why, never its text: the answer alone, 409 `bot_unreachable`,
/// doesn't tell it from a refusal before the commit.
#[sqlx::test(migrations = false)]
async fn a_committed_line_that_isnt_sent_is_logged_at_debug_with_its_ids_only(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let by = setup.acting().await;
    let (logs, _guard) = capture();
    let refusals = [
        (CommandRefused::NotHeld, "NotHeld"),
        (CommandRefused::NotConnected, "NotConnected"),
        (CommandRefused::QueueFull, "QueueFull"),
    ];

    let mut sent = Vec::new();
    for (n, (refusal, _)) in (1_u8..).zip(refusals) {
        let bot = setup.bot(n).await;
        setup.held_in(bot, online()).await;
        *setup.live.chat_refusal.lock().unwrap() = Some(refusal);
        let refused = setup
            .chatting()
            .send_chat(
                &by,
                bot,
                &line("hello secret_label"),
                ChatRoute::Chat,
                Instant::now(),
            )
            .await;
        assert!(
            matches!(
                refused,
                Err(ChatSendError::Refused(ChatRefusal::Unreachable))
            ),
            "{refused:?}"
        );
        sent.push(bot);
    }

    let lines = logs.lines_with(NOT_SENT).unwrap();
    assert_eq!(lines.len(), 3, "{}", logs.text());
    for ((line, bot), (_, name)) in lines.iter().zip(&sent).zip(refusals) {
        assert_eq!(line["level"], "DEBUG");
        assert_eq!(line["user_id"], by.actor.id.to_string());
        assert_eq!(line["bot_id"], bot.to_string());
        assert_eq!(line["agent_id"], agent(1).to_string());
        assert!(line["chat_request"].is_string(), "{line}");
        assert!(line["chat_line"].is_i64(), "{line}");
        assert_eq!(line["refused"], name);
    }
    assert!(!logs.text().contains("secret_label"), "{}", logs.text());
}

/// A line its route doesn't take reached the service only because the
/// handler's check is missing (S10): it's refused before anything is read,
/// so an unknown bot gets that answer too, and nothing is written, recorded
/// or sent.
#[sqlx::test(migrations = false)]
async fn a_line_its_route_doesnt_take_is_refused_before_anything_is_read(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let by = setup.acting().await;
    let held = setup.bot(1).await;
    setup.held_in(held, online()).await;
    let unknown = bot(99);
    let cases = [
        (ChatRoute::Chat, "/op AfkBot2"),
        (ChatRoute::Command, "hello"),
    ];

    for (route, text) in cases {
        for target in [held, unknown] {
            let refused = setup
                .chatting()
                .send_chat(&by, target, &line(text), route, Instant::now())
                .await;

            assert!(
                matches!(refused, Err(ChatSendError::WrongRoute(refused)) if refused == route),
                "{route:?} {text:?}: {refused:?}"
            );
        }
    }
    assert_eq!(setup.lines(held).await, Vec::<ChatEntry>::new());
    assert_eq!(setup.chat_entries().await.len(), 0);
    assert_eq!(setup.live.chats.lock().unwrap().len(), 0);
}

/// Each route sends the lines it takes: `/chat` plain chat and allowlisted
/// commands, `/command` every command.
#[sqlx::test(migrations = false)]
async fn each_route_sends_the_lines_it_takes(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let by = setup.acting().await;
    let bot = setup.bot(1).await;
    setup.held_in(bot, online()).await;
    let cases = [
        (ChatRoute::Chat, "hello"),
        (ChatRoute::Chat, "/spawn here"),
        (ChatRoute::Command, "/spawn here"),
        (ChatRoute::Command, "/op AfkBot2"),
    ];

    for (route, text) in cases {
        let sent = setup
            .chatting()
            .send_chat(&by, bot, &line(text), route, Instant::now())
            .await;

        assert!(sent.is_ok(), "{route:?} {text:?}: {sent:?}");
    }
    assert_eq!(setup.live.chats.lock().unwrap().len(), cases.len());
}

#[sqlx::test(migrations = false)]
async fn a_line_whose_bot_no_longer_names_the_agent_is_refused_and_nothing_is_kept(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.held_in(bot, online()).await;
    let by = setup.acting().await;
    let service = setup.chatting_over(Arc::new(FailingCommits {
        inner: setup.db.clone(),
        script: Script::AppendNothing,
    }));

    let refused = service
        .send_chat(&by, bot, &line("hello"), ChatRoute::Chat, Instant::now())
        .await;

    assert!(
        matches!(
            refused,
            Err(ChatSendError::Refused(ChatRefusal::Unreachable))
        ),
        "{refused:?}"
    );
    assert_eq!(setup.chat_entries().await.len(), 0);
    assert_eq!(setup.live.chats.lock().unwrap().len(), 0);
}

#[sqlx::test(migrations = false)]
async fn a_line_whose_commit_fails_is_never_sent(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.held_in(bot, online()).await;
    let by = setup.acting().await;
    let service = setup.chatting_over(Arc::new(FailingCommits {
        inner: setup.db.clone(),
        script: Script::FailCommit,
    }));

    let refused = service
        .send_chat(&by, bot, &line("hello"), ChatRoute::Chat, Instant::now())
        .await;

    assert!(
        matches!(
            refused,
            Err(ChatSendError::Write(BotWriteError::Store(StoreError::Busy)))
        ),
        "{refused:?}"
    );
    assert_eq!(setup.lines(bot).await, Vec::<ChatEntry>::new());
    assert_eq!(setup.live.chats.lock().unwrap().len(), 0);
}

#[sqlx::test(migrations = false)]
async fn a_failing_random_source_writes_and_sends_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.held_in(bot, online()).await;
    let by = setup.acting().await;
    setup.random.fail_next(RandomError::Unavailable);

    let refused = setup
        .chatting()
        .send_chat(&by, bot, &line("hello"), ChatRoute::Chat, Instant::now())
        .await;

    assert!(
        matches!(refused, Err(ChatSendError::Mint(_))),
        "{refused:?}"
    );
    assert_eq!(setup.lines(bot).await, Vec::<ChatEntry>::new());
    assert_eq!(setup.chat_entries().await.len(), 0);
    assert_eq!(setup.live.chats.lock().unwrap().len(), 0);
}

/// The entry names the route, whether the line is a command, whether the
/// allowlist lists it, and only a listed command's name: never the text
/// (S6).
#[sqlx::test(migrations = false)]
async fn a_lines_entry_names_its_route_and_command_kind_never_its_text(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    setup.held_in(bot, online()).await;
    let by = setup.acting().await;
    let cases = [
        (
            ChatRoute::Chat,
            "hello there",
            AuditAction::BOT_SEND_CHAT,
            "none",
            None,
        ),
        (
            ChatRoute::Chat,
            "/spawn now",
            AuditAction::BOT_SEND_CHAT,
            "allowlisted",
            Some("/spawn"),
        ),
        (
            ChatRoute::Command,
            "/spawn",
            AuditAction::BOT_SEND_COMMAND,
            "allowlisted",
            Some("/spawn"),
        ),
        (
            ChatRoute::Command,
            "/op AfkBot2",
            AuditAction::BOT_SEND_COMMAND,
            "unlisted",
            None,
        ),
    ];

    for (route, text, action, command, name) in cases {
        let done = setup
            .chatting()
            .send_chat(&by, bot, &line(text), route, Instant::now())
            .await
            .unwrap();

        let entries = setup.chat_entries().await;
        let entry = entries.last().unwrap();
        let mut expected = BTreeMap::from([
            ("request".to_owned(), done.request.to_string()),
            ("message".to_owned(), done.message.get().to_string()),
            ("command".to_owned(), command.to_owned()),
        ]);
        if let Some(name) = name {
            expected.insert("command_name".to_owned(), name.to_owned());
        }
        assert_eq!(entry.action, action, "{text}");
        assert_eq!(metadata(entry), expected, "{text}");
        let recorded = format!("{:?}", entry.metadata);
        assert!(
            !recorded.contains("there")
                && !recorded.contains("AfkBot2")
                && !recorded.contains("now"),
            "{recorded}"
        );
    }
}
