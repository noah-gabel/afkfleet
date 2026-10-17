//! Tests for the bots' reads and writes (Plan.md P10.5, P11.2; ADR-0019,
//! S41, S48): each bot comes with what its state reads as now, from its row,
//! the latest report the server keeps, the agent its row names and that
//! agent's live stream. A bot without an agent reads Unplaced, with
//! why and its sticky state; a bot whose agent has no live stream reads
//! Unknown, with the agent's status, when it was last heard from and the
//! bot's last known state; a bot on a live agent reads its report on that
//! stream, or that none has come yet. A Resume of a bot that runs nowhere
//! takes the kept report with its stored state, and puts it back when its
//! commit fails. The HTTP tests (`tests/auth/bots.rs`) cover the rest.
//!
//! Each test gets a fresh database from `#[sqlx::test]`. Time isn't paused:
//! sqlx's worker threads would make paused timeouts fire early (CLAUDE.md).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use fleet_core::agent::AgentStatus;
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::authz::{Actor, Role};
use fleet_core::bot::{
    BotSnapshot, BotState, CommandRefusal, DesiredRunState, LastReport, LastState, Lifecycle,
    PauseReason, ShownState, StickyState, UnplacedWhy,
};
use fleet_core::disconnect::ConflictKind;
use fleet_core::id::{AccountId, AgentId, BotId, ModeId, SessionId, UserId};
use fleet_core::mode::ModePreset;
use fleet_core::pki::CertFingerprint;
use fleet_core::placement::Unplaced;
use fleet_core::value::{AgentName, ServerAddress, Username};
use fleet_server::app::bots::{BotAction, BotService, BotWriteError, Controlled};
use fleet_server::app::users::ActingUser;
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::accounts::{AccountReads, AccountWrites};
use fleet_server::ports::agents::{AgentReads, AgentWrites};
use fleet_server::ports::agents::{NewAgent, NewAgentCert};
use fleet_server::ports::audit::{AuditEntryId, AuditReads, AuditWrites};
use fleet_server::ports::audit::{AuditLimit, AuditPage, NewAuditEntry};
use fleet_server::ports::bots::{Bot, NewBot};
use fleet_server::ports::bots::{BotReads, BotWrites};
use fleet_server::ports::enrollment_tokens::{EnrollmentTokenReads, EnrollmentTokenWrites};
use fleet_server::ports::envelopes::{EnvelopeReads, EnvelopeWrites};
use fleet_server::ports::grants::{GrantReads, GrantWrites};
use fleet_server::ports::invites::{InviteReads, InviteWrites};
use fleet_server::ports::live_agents::{
    CommandRefused, HeldBot, LatestReport, LiveAgents, LiveStream, PermissionEnd,
};
use fleet_server::ports::login_attempts::{LoginAttemptReads, LoginAttemptWrites};
use fleet_server::ports::mfa::{MfaReads, MfaWrites};
use fleet_server::ports::modes::{ModeReads, ModeWrites};
use fleet_server::ports::page::{Page, PageLimit};
use fleet_server::ports::sessions::{NewSession, SessionReads, SessionWrites};
use fleet_server::ports::store::{Store, StoreError, UnauditedWrite, WriteTx};
use fleet_server::ports::tokens::TokenHash;
use fleet_server::ports::users::{NewUser, PasswordHash, UserReads, UserWrites};
use fleet_testkit::system::ManualClock;
use secrecy::SecretString;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::Notify;

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
            Script::ClearNothing => self.inner.commit(entry).await,
        }
    }

    async fn commit_unaudited(self: Box<Self>, reason: UnauditedWrite) -> Result<(), StoreError> {
        match &self.script {
            Script::FailCommit => Err(StoreError::Busy),
            Script::HangCommit(started) => {
                started.notify_one();
                std::future::pending().await
            }
            Script::ClearNothing => self.inner.commit_unaudited(reason).await,
        }
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

    fn audit(&self) -> &dyn AuditReads {
        self.inner.audit()
    }

    async fn ping(&self) -> Result<(), StoreError> {
        self.inner.ping().await
    }
}

struct Setup {
    db: Database,
    live: Arc<Live>,
    clock: ManualClock,
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
        }
    }

    fn service(&self, allow_offline: bool) -> BotService {
        BotService::new(
            Arc::new(self.clock.clone()),
            Arc::new(self.db.clone()),
            Arc::clone(&self.live) as Arc<dyn LiveAgents>,
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

#[sqlx::test(migrations = false)]
async fn a_read_that_fails_is_an_error(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = Setup::new(options).await;
    let bot = setup.bot(1).await;
    let service = setup.service(true);
    setup.db.close().await;

    let view = service.view(&the_owner(), bot).await;
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
            store,
            Arc::clone(&self.live) as Arc<dyn LiveAgents>,
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
