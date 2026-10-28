//! The reconciler on paused time: the real registry, a stream that a test
//! drives as the agent, and an in-memory store behind the `Assignments`
//! port (sqlx can't run on paused time, R22).

use core::time::Duration;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use fleet_core::account::{AccountKind, AccountStatus};
use fleet_core::bot::{
    BotAccount, BotSnapshot, BotState, DesiredRunState, LastReport, LastState, PauseReason,
    StickyState,
};
use fleet_core::disconnect::{ConflictKind, ConflictTexts};
use fleet_core::id::{AccountId, AgentId, BotId, RequestId, ServerSettingsId};
use fleet_core::mode::ModeDefinition;
use fleet_core::pki::CertFingerprint;
use fleet_core::placement::{DetachReason, McIdentity, UnassignReason, Unplaced};
use fleet_core::value::{AgentCapacity, AgentName, ServerAddress, ServerKey};
use fleet_proto::control::{
    AgentMessage, BotEvent, BotEventKind, KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT,
    RECONNECT_DELAY_MAX, ServerMessage, WELCOME_TIMEOUT, Welcome,
};
use fleet_proto::generated as wire;
use fleet_startup::config::{LogConfig, LogFormat};
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::log_buffer::LogBuffer;
use fleet_testkit::system::ManualClock;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt as _;

use super::*;
use crate::grpc::registry::{Activation, Halt, Lease};
use crate::ports::agent_connections::{Admitted, StreamEnd};
use crate::ports::assignments::{AssignError, Assigned};
use crate::ports::bots::{Bot, BotRecord, Fleet, ReleaseRecord, UnreadableBot};
use crate::ports::live_agents::LiveAgents as _;
use crate::ports::server_settings::UnreadableSettings;
use crate::ports::store::StoreError;

const T0: &str = "2027-01-15T08:00:00Z";
/// How long a test waits for a message it expects now: well below the
/// interval, so paused time can't reach a pass that wasn't due.
const NOW: Duration = Duration::from_millis(100);

fn at(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

fn agent(n: u8) -> AgentId {
    AgentId::new_v7(at(T0), [n; 10]).unwrap()
}

fn bot(n: u8) -> BotId {
    BotId::new_v7(at(T0), [n; 10]).unwrap()
}

fn paused() -> LastState {
    LastState::Paused(PauseReason::Conflict {
        kind: ConflictKind::DuplicateLogin,
    })
}

/// Bot `n` of the offline account `AfkBot<n>`: placeable, unassigned.
fn record(n: u8) -> BotRecord {
    let name: fleet_core::value::McUsername = format!("AfkBot{n}").parse().unwrap();
    BotRecord {
        bot: Bot {
            id: bot(n),
            account: AccountId::new_v7(at(T0), [n; 10]).unwrap(),
            server: Some("play.example.com".parse().unwrap()),
            mode: fleet_core::mode::ModePreset::Afk.id(),
            desired: DesiredRunState::Running,
            agent: None,
            last: None,
        },
        account: BotAccount::Offline(name.clone()),
        identity: McIdentity::offline(&name),
        kind: AccountKind::Offline,
        status: AccountStatus::Active,
        owner_enabled: true,
        mode: ModeDefinition::afk(),
    }
}

/// `record(n)` assigned to agent `a`.
fn on(n: u8, a: u8) -> BotRecord {
    let mut record = record(n);
    record.bot.agent = Some(agent(a));
    record
}

/// A call the reconciler made on the store.
#[derive(Debug, Clone, PartialEq)]
enum Call {
    Assign(BotId, AgentId),
    Unassign(BotId, AgentId, UnassignReason, Option<LastReport>),
    Detach(BotId, AgentId, DetachReason, Option<LastReport>),
    Stray(BotId, AgentId),
}

#[derive(Debug, Default)]
struct State {
    fleet: Fleet,
    calls: Vec<Call>,
    fail_reads: bool,
    busy_detaches: u32,
    busy_strays: u32,
    /// Every unassignment fails busy.
    failing_unassigns: bool,
    /// How many unassignments were tried.
    unassign_attempts: u32,
    /// Ends this agent's stream inside the next assignment, as a stream that
    /// ends between the commit and the message.
    end_on_assign: Option<(AgentRegistry, AgentId)>,
    /// Clears the assigned bot's stored state inside its assignment, as a
    /// Resume that commits after the pass read the fleet (S48).
    clear_on_assign: bool,
}

/// An in-memory store with the repository's rules.
#[derive(Debug, Default)]
struct Store(Mutex<State>);

impl Store {
    fn with(bots: Vec<BotRecord>, enabled: &[u8]) -> Arc<Self> {
        let store = Self::default();
        {
            let mut state = store.state();
            state.fleet.bots = bots;
            state.fleet.enabled_agents = enabled.iter().map(|&n| agent(n)).collect();
        }
        Arc::new(store)
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }

    fn calls(&self) -> Vec<Call> {
        self.state().calls.clone()
    }

    fn agent_of(&self, id: BotId) -> Option<AgentId> {
        self.state()
            .fleet
            .bots
            .iter()
            .find(|record| record.bot.id == id)
            .and_then(|record| record.bot.agent)
    }

    fn change(&self, id: BotId, change: impl FnOnce(&mut BotRecord)) {
        let mut state = self.state();
        let record = state
            .fleet
            .bots
            .iter_mut()
            .find(|record| record.bot.id == id)
            .unwrap();
        change(record);
    }
}

#[async_trait::async_trait]
impl Assignments for Store {
    async fn fleet(&self) -> Result<Fleet, StoreError> {
        let state = self.state();
        if state.fail_reads {
            return Err(StoreError::Busy);
        }
        Ok(state.fleet.clone())
    }

    async fn assign(&self, bot: BotId, agent: AgentId) -> Result<Option<Assigned>, AssignError> {
        let mut state = self.state();
        state.calls.push(Call::Assign(bot, agent));
        if let Some((registry, ending)) = state.end_on_assign.take() {
            let _ = registry.end(ending, StreamEnd::StreamError);
        }
        let enabled = state.fleet.enabled_agents.contains(&agent);
        let clear = state.clear_on_assign;
        let Some(record) = state
            .fleet
            .bots
            .iter_mut()
            .find(|record| record.bot.id == bot && record.bot.agent.is_none())
        else {
            return Ok(None);
        };
        if !enabled {
            return Ok(None);
        }
        if clear {
            record.bot.last = None;
        }
        record.bot.agent = Some(agent);
        Ok(Some(Assigned {
            last: record.bot.last,
        }))
    }

    async fn unassign(
        &self,
        bot: BotId,
        agent: AgentId,
        reason: UnassignReason,
        last: Option<LastReport>,
    ) -> Result<bool, AssignError> {
        let attempts = {
            let mut state = self.state();
            state.unassign_attempts += 1;
            state.unassign_attempts
        };
        // A pass after pass on paused time never lets the test go on: end
        // the task instead of hanging.
        assert!(attempts <= 20, "the reconciler spins on a failing write");
        let mut state = self.state();
        if state.failing_unassigns {
            return Err(AssignError::Store(StoreError::Busy));
        }
        state.calls.push(Call::Unassign(bot, agent, reason, last));
        let Some(record) = state
            .fleet
            .bots
            .iter_mut()
            .find(|record| record.bot.id == bot && record.bot.agent == Some(agent))
        else {
            return Ok(false);
        };
        if last.is_some() {
            record.bot.last = last;
        }
        Ok(true)
    }

    async fn detach(
        &self,
        bot: BotId,
        agent: AgentId,
        reason: DetachReason,
        last: Option<LastReport>,
    ) -> Result<bool, AssignError> {
        let mut state = self.state();
        if state.busy_detaches > 0 {
            state.busy_detaches -= 1;
            return Err(AssignError::Store(StoreError::Busy));
        }
        state.calls.push(Call::Detach(bot, agent, reason, last));
        let mut changed = false;
        if let Some(record) = state
            .fleet
            .bots
            .iter_mut()
            .find(|record| record.bot.id == bot && record.bot.agent == Some(agent))
        {
            if last.is_some() {
                record.bot.last = last;
            }
            record.bot.agent = None;
            changed = true;
        }
        let before = state.fleet.releases.len();
        state
            .fleet
            .releases
            .retain(|record| !(record.bot == bot && record.agent == agent));
        Ok(changed || state.fleet.releases.len() != before)
    }

    async fn stray(
        &self,
        bot: BotId,
        agent: AgentId,
        _assigned_to: Option<AgentId>,
    ) -> Result<bool, AssignError> {
        let mut state = self.state();
        if state.busy_strays > 0 {
            state.busy_strays -= 1;
            return Err(AssignError::Store(StoreError::Busy));
        }
        state.calls.push(Call::Stray(bot, agent));
        let identity = state
            .fleet
            .bots
            .iter()
            .find(|record| record.bot.id == bot)
            .map(|record| record.identity.clone());
        let Some(identity) = identity else {
            return Ok(false);
        };
        state.fleet.releases.push(ReleaseRecord {
            bot,
            agent,
            identity,
        });
        Ok(true)
    }
}

/// A live stream the test drives as agent `n`.
struct Stream {
    lease: Lease,
    queued: mpsc::Receiver<wire::ServerMessage>,
    ended: oneshot::Receiver<StreamEnd>,
}

impl Stream {
    /// The next message within [`NOW`], read back; `None` when none came.
    async fn next(&mut self) -> Option<ServerMessage> {
        let message = tokio::time::timeout(NOW, self.queued.recv())
            .await
            .ok()
            .flatten()?;
        Some(ServerMessage::try_from(message).unwrap())
    }
}

/// Agent `n`'s stream, welcomed with `capacity` and the hello `running`, its
/// `Welcome` taken off; its queue holds `depth` messages.
fn connect(
    registry: &AgentRegistry,
    n: u8,
    capacity: u32,
    running: Vec<BotSnapshot>,
    depth: usize,
) -> Stream {
    let lease = registry
        .claim(&Admitted {
            request: RequestId::new_v7(at(T0), [n; 10]).unwrap(),
            agent: agent(n),
            name: AgentName::try_from(format!("agent-{n}").as_str()).unwrap(),
            fingerprint: CertFingerprint::from_digest([n; 32]),
            not_after: at("2027-03-16T08:00:00Z"),
            peer: None,
        })
        .unwrap();
    let (queue, mut queued) = mpsc::channel(depth);
    let welcome = wire::ServerMessage::try_from(ServerMessage::Welcome(Welcome {
        heartbeat_interval: Duration::from_secs(10),
        agent_id: agent(n),
    }))
    .unwrap();
    let ended = registry
        .activate(
            &lease,
            Activation {
                queue,
                halt: Halt::default(),
                connected_at: at(T0),
                capacity: AgentCapacity::try_from(capacity).unwrap(),
                running,
                welcome,
            },
        )
        .unwrap();
    queued.try_recv().unwrap();
    Stream {
        lease,
        queued,
        ended,
    }
}

fn snapshot(n: u8, state: BotState) -> BotSnapshot {
    BotSnapshot {
        bot_id: bot(n),
        state,
        since: at(T0),
        last_disconnect: None,
    }
}

fn online(n: u8) -> BotSnapshot {
    snapshot(
        n,
        BotState::Online {
            since: at(T0),
            attempt: core::num::NonZeroU32::MIN,
        },
    )
}

fn paused_snapshot(n: u8) -> BotSnapshot {
    snapshot(
        n,
        BotState::Paused {
            reason: PauseReason::Conflict {
                kind: ConflictKind::DuplicateLogin,
            },
        },
    )
}

/// A running reconciler: its hints, its stop token and its task.
struct Running {
    hints: Hints,
    stop: CancellationToken,
    task: JoinHandle<()>,
}

impl Running {
    async fn stop(self) {
        self.stop.cancel();
        tokio::time::timeout(Duration::from_secs(1), self.task)
            .await
            .unwrap()
            .unwrap();
    }
}

/// Starts the reconciler of `registry` over `store`, with hints of `depth`,
/// offline accounts allowed.
fn start(registry: &AgentRegistry, store: &Arc<Store>, depth: usize) -> Running {
    start_with(registry, store, depth, true)
}

/// [`start`], offline accounts allowed only when `allow_offline`.
fn start_with(
    registry: &AgentRegistry,
    store: &Arc<Store>,
    depth: usize,
    allow_offline: bool,
) -> Running {
    start_at(registry, store, depth, allow_offline, ChatRate::default())
}

/// [`start_with`], every bot chatting at `chat_rate`.
fn start_at(
    registry: &AgentRegistry,
    store: &Arc<Store>,
    depth: usize,
    allow_offline: bool,
    chat_rate: ChatRate,
) -> Running {
    let (hints, inbox) = hints(depth);
    let reconciler = Reconciler::new(
        registry.clone(),
        Arc::clone(store) as Arc<dyn Assignments>,
        Arc::new(ManualClock::new(at(T0))),
        inbox,
        ReconcileTimings::default(),
        allow_offline,
        chat_rate,
    );
    spawned(reconciler, hints)
}

/// `reconciler`'s task, sent `hints`.
fn spawned(reconciler: Reconciler, hints: Hints) -> Running {
    let stop = CancellationToken::new();
    let task = tokio::spawn(reconciler.run(stop.clone()));
    Running { hints, stop, task }
}

/// The bots each pass told the live events sockets about, with how many
/// store calls came before it (ADR-0020, S42).
#[derive(Debug)]
struct Told {
    store: Arc<Store>,
    calls: Mutex<Vec<(usize, Vec<BotId>)>>,
}

impl Told {
    /// What the passes told since the last call.
    fn take(&self) -> Vec<(usize, Vec<BotId>)> {
        core::mem::take(&mut *self.calls.lock().unwrap())
    }
}

impl LiveHints for Told {
    fn bots(&self, bots: &[BotId]) {
        // An empty list is no hint, as the bus takes it.
        if !bots.is_empty() {
            let calls = self.store.calls().len();
            self.calls.lock().unwrap().push((calls, bots.to_vec()));
        }
    }

    fn mode(&self, event: crate::ports::live_hints::ModeEvent) {
        panic!("the reconciler told a mode: {event:?}");
    }

    fn role_changed(&self, user: fleet_core::id::UserId) {
        panic!("the reconciler told a role change: {user}");
    }
}

/// [`start`], on `clock`, telling the live events sockets through the
/// returned [`Told`].
fn start_telling(
    registry: &AgentRegistry,
    store: &Arc<Store>,
    clock: &ManualClock,
) -> (Running, Arc<Told>) {
    let (hints, inbox) = hints(8);
    let told = Arc::new(Told {
        store: Arc::clone(store),
        calls: Mutex::new(Vec::new()),
    });
    let reconciler = Reconciler::new(
        registry.clone(),
        Arc::clone(store) as Arc<dyn Assignments>,
        Arc::new(clock.clone()),
        inbox,
        ReconcileTimings::default(),
        true,
        ChatRate::default(),
    )
    .with_live_hints(Arc::clone(&told) as Arc<dyn LiveHints>);
    (spawned(reconciler, hints), told)
}

fn registry() -> AgentRegistry {
    AgentRegistry::new(Arc::new(ManualClock::new(at(T0))))
}

/// The bots of a full reconcile, in order.
fn ids_of(reconciled: Vec<(BotId, DesiredRunState, Option<StickyState>)>) -> Vec<BotId> {
    reconciled.into_iter().map(|(bot, _, _)| bot).collect()
}

/// The bots of a full reconcile, with their desired state and restore.
fn reconciled(
    message: Option<ServerMessage>,
) -> Vec<(BotId, DesiredRunState, Option<StickyState>)> {
    match message {
        Some(ServerMessage::Reconcile(assignments)) => assignments
            .into_iter()
            .map(|assignment| {
                (
                    assignment.spec.id,
                    assignment.spec.desired,
                    assignment.restore,
                )
            })
            .collect(),
        other => panic!("not a reconcile: {other:?}"),
    }
}

// --- Welcomes ---------------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_welcome_gets_one_full_reconcile_even_empty() {
    let registry = registry();
    let store = Store::with(Vec::new(), &[1]);
    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);

    let running = start(&registry, &store, 8);

    assert_eq!(reconciled(stream.next().await), []);
    assert_eq!(stream.next().await.map(|_| ()), None, "nothing more");
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_welcome_gets_its_stored_bots_with_their_sticky_restore_and_holds_them() {
    let registry = registry();
    let mut paused_bot = on(1, 1);
    paused_bot.bot.last = Some(LastReport {
        state: paused(),
        at: at(T0),
    });
    let store = Store::with(vec![paused_bot], &[1]);
    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);

    let running = start(&registry, &store, 8);

    assert_eq!(
        reconciled(stream.next().await),
        [(
            bot(1),
            DesiredRunState::Running,
            Some(StickyState::Paused(PauseReason::Conflict {
                kind: ConflictKind::DuplicateLogin
            }))
        )]
    );
    assert!(registry.held(agent(1), bot(1)).is_some());
    assert_eq!(store.calls(), [], "a stored assignment is no change");
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_hellos_bot_it_wasnt_given_is_fenced_before_its_reconcile_and_left_out() {
    let registry = registry();
    let store = Store::with(vec![record(1)], &[1]);
    let mut stream = connect(&registry, 1, 5, vec![online(1)], 8);

    let running = start(&registry, &store, 8);

    assert_eq!(reconciled(stream.next().await), []);
    assert_eq!(store.calls(), [Call::Stray(bot(1), agent(1))]);
    assert_eq!(registry.held(agent(1), bot(1)), None, "held releasing only");
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_stray_that_cant_be_written_holds_back_the_reconcile_until_the_next_pass() {
    let registry = registry();
    let store = Store::with(vec![record(1)], &[1]);
    store.state().busy_strays = 1;
    let mut stream = connect(&registry, 1, 5, vec![online(1)], 8);

    let running = start(&registry, &store, 8);

    assert_eq!(stream.next().await.map(|_| ()), None, "no reconcile yet");
    running.hints.changed(Change::Store);
    assert_eq!(reconciled(stream.next().await), []);
    assert_eq!(store.calls(), [Call::Stray(bot(1), agent(1))]);
    running.stop().await;
}

// --- Placing ----------------------------------------------------------------------------------

/// Every spec carries `[rate_limits]`' chat rate, in a welcome's reconcile
/// and in a later placement (ADR-0020, S3).
#[tokio::test(start_paused = true)]
async fn every_spec_carries_the_configured_chat_rate() {
    let registry = registry();
    let store = Store::with(vec![on(1, 1)], &[1]);
    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);
    let rate = ChatRate::try_new(5, 2).unwrap();
    let running = start_at(&registry, &store, 8, true, rate);
    let Some(ServerMessage::Reconcile(welcomed)) = stream.next().await else {
        panic!("a reconcile");
    };

    store.state().fleet.bots.push(record(2));
    running.hints.changed(Change::Store);
    let Some(ServerMessage::Assign(placed)) = stream.next().await else {
        panic!("an assignment");
    };

    let rates: Vec<ChatRate> = welcomed
        .iter()
        .chain([&placed])
        .map(|assignment| assignment.spec.chat_rate)
        .collect();
    assert_eq!(rates, [rate, rate]);
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_waiting_bot_is_placed_after_its_commit_on_a_hint() {
    let registry = registry();
    let store = Store::with(Vec::new(), &[1]);
    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);
    let running = start(&registry, &store, 8);
    assert_eq!(reconciled(stream.next().await), []);

    store.state().fleet.bots.push(record(2));
    running.hints.changed(Change::Store);

    let Some(ServerMessage::Assign(assignment)) = stream.next().await else {
        panic!("an assignment");
    };
    assert_eq!(assignment.spec.id, bot(2));
    assert_eq!(assignment.restore, None);
    assert_eq!(store.calls(), [Call::Assign(bot(2), agent(1))]);
    assert_eq!(store.agent_of(bot(2)), Some(agent(1)));
    assert!(registry.held(agent(1), bot(2)).is_some());
    running.stop().await;
}

/// A Resume that clears a waiting bot's stored Paused after the pass read the
/// fleet: the placement restores what its assignment read, not the pass's
/// earlier read, so the bot starts instead of resting Paused (S48).
#[tokio::test(start_paused = true)]
async fn a_bot_resumed_during_a_pass_is_placed_without_the_cleared_restore() {
    let registry = registry();
    let store = Store::with(Vec::new(), &[1]);
    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);
    let running = start(&registry, &store, 8);
    assert_eq!(reconciled(stream.next().await), []);
    let mut waiting = record(2);
    waiting.bot.last = Some(LastReport {
        state: paused(),
        at: at(T0),
    });
    store.state().fleet.bots.push(waiting);
    store.state().clear_on_assign = true;

    running.hints.changed(Change::Store);

    let Some(ServerMessage::Assign(assignment)) = stream.next().await else {
        panic!("an assignment");
    };
    assert_eq!(assignment.spec.id, bot(2));
    assert_eq!(assignment.restore, None);
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_paused_bot_still_paused_at_its_assignment_is_placed_paused() {
    let registry = registry();
    let store = Store::with(Vec::new(), &[1]);
    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);
    let running = start(&registry, &store, 8);
    assert_eq!(reconciled(stream.next().await), []);
    let mut waiting = record(2);
    waiting.bot.last = Some(LastReport {
        state: paused(),
        at: at(T0),
    });
    store.state().fleet.bots.push(waiting);

    running.hints.changed(Change::Store);

    let Some(ServerMessage::Assign(assignment)) = stream.next().await else {
        panic!("an assignment");
    };
    assert!(matches!(assignment.restore, Some(StickyState::Paused(_))));
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn the_interval_runs_a_pass_without_a_hint() {
    let registry = registry();
    let store = Store::with(Vec::new(), &[1]);
    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);
    let running = start(&registry, &store, 8);
    assert_eq!(reconciled(stream.next().await), []);
    store.state().fleet.bots.push(record(2));

    tokio::time::sleep(
        RECONCILE_INTERVAL
            .checked_sub(Duration::from_secs(1))
            .unwrap(),
    )
    .await;
    let before = stream.queued.try_recv().is_ok();
    tokio::time::sleep(Duration::from_secs(1)).await;

    assert!(!before, "not before the interval");
    assert!(matches!(
        stream.next().await,
        Some(ServerMessage::Assign(_))
    ));
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_full_hint_queue_folds_into_one_pass_and_warns_once() {
    let buffer = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: "info".parse().unwrap(),
    };
    let subscriber =
        tracing_subscriber::registry().with(layer_with(&config, &NoRules, false, buffer.clone()));
    let _guard = tracing::subscriber::set_default(subscriber);
    let registry = registry();
    let store = Store::with(Vec::new(), &[1]);
    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);
    let running = start(&registry, &store, 1);
    assert_eq!(reconciled(stream.next().await), []);
    store.state().fleet.bots.push(record(2));

    // The task doesn't run until the test yields: one hint fits, two don't.
    for _ in 0..3 {
        running.hints.changed(Change::Store);
    }

    assert!(matches!(
        stream.next().await,
        Some(ServerMessage::Assign(_))
    ));
    let warnings = buffer
        .lines_with("the reconciler's hints overflowed; a full pass catches up")
        .unwrap();
    assert_eq!(warnings.len(), 1);
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_stream_ended_between_the_commit_and_the_message_holds_nothing() {
    let registry = registry();
    let store = Store::with(Vec::new(), &[1]);
    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);
    let running = start(&registry, &store, 8);
    assert_eq!(reconciled(stream.next().await), []);
    {
        let mut state = store.state();
        state.fleet.bots.push(record(2));
        state.end_on_assign = Some((registry.clone(), agent(1)));
    }

    running.hints.changed(Change::Store);
    tokio::time::sleep(NOW).await;

    assert_eq!(store.agent_of(bot(2)), Some(agent(1)), "committed");
    assert_eq!(registry.held(agent(1), bot(2)), None, "never held");
    assert!(stream.queued.try_recv().is_err(), "never sent");
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_full_agent_queue_ends_its_stream_too_slow() {
    let registry = registry();
    let store = Store::with(Vec::new(), &[1]);
    // Room for the reconcile only, never the assignment after it.
    let mut stream = connect(&registry, 1, 5, Vec::new(), 1);
    let running = start(&registry, &store, 8);
    tokio::time::sleep(NOW).await;
    store.state().fleet.bots.push(record(2));

    running.hints.changed(Change::Store);
    tokio::time::sleep(NOW).await;

    assert_eq!(stream.ended.try_recv().unwrap(), StreamEnd::TooSlow);
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_failed_read_skips_the_pass_and_the_next_one_catches_up() {
    let registry = registry();
    let store = Store::with(Vec::new(), &[1]);
    store.state().fail_reads = true;
    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);

    let running = start(&registry, &store, 8);
    let skipped = stream.next().await;
    store.state().fail_reads = false;
    running.hints.changed(Change::Store);

    assert_eq!(skipped.map(|_| ()), None);
    assert_eq!(reconciled(stream.next().await), []);
    running.stop().await;
}

// --- Conflict texts (ADR-0020, S74, S76, R132) -------------------------------------------

fn texts(entries: &[&str]) -> ConflictTexts {
    ConflictTexts::try_new(entries).unwrap()
}

fn server_key(address: &str) -> ServerKey {
    ServerKey::from(&address.parse::<ServerAddress>().unwrap())
}

impl Store {
    /// Gives `server` the conflict texts `entries`.
    fn texts(&self, server: &str, entries: &[&str]) {
        self.state()
            .fleet
            .server_texts
            .readable
            .insert(server_key(server), texts(entries));
    }
}

/// A bot's spec carries its server's texts, the row keyed with the default
/// port the bot's address leaves out (S74).
#[tokio::test(start_paused = true)]
async fn a_bots_spec_carries_its_servers_conflict_texts() {
    let registry = registry();
    let store = Store::with(vec![on(1, 1), on(2, 1)], &[1]);
    store.change(bot(2), |record| {
        record.bot.server = Some("other.example.com".parse().unwrap());
    });
    store.texts("play.example.com:25565", &["Already connected"]);
    let mut stream = connect(&registry, 1, 5, vec![online(1), online(2)], 8);
    let running = start(&registry, &store, 8);

    let Some(ServerMessage::Reconcile(assignments)) = stream.next().await else {
        panic!("a reconcile");
    };

    let of = |n| {
        assignments
            .iter()
            .find(|assignment| assignment.spec.id == bot(n))
            .map(|assignment| assignment.spec.conflict_texts.clone())
    };
    assert_eq!(of(1), Some(texts(&["Already connected"])));
    assert_eq!(
        of(2),
        Some(ConflictTexts::default()),
        "a server without settings"
    );
    running.stop().await;
}

/// A change of its server's texts reaches a running bot as an `Update` in
/// its session, and their delete as an `Update` without texts.
#[tokio::test(start_paused = true)]
async fn a_change_of_its_servers_texts_reaches_a_running_bot_as_an_update() {
    let (_registry, store, mut stream, running) = running_bot().await;

    store.texts("play.example.com", &["Already connected"]);
    running.hints.changed(Change::Store);
    let Some(ServerMessage::Update(changed)) = stream.next().await else {
        panic!("an update");
    };
    store.state().fleet.server_texts.readable.clear();
    running.hints.changed(Change::Store);
    let Some(ServerMessage::Update(deleted)) = stream.next().await else {
        panic!("an update");
    };

    assert_eq!(changed.conflict_texts, texts(&["Already connected"]));
    assert_eq!(deleted.conflict_texts, ConflictTexts::default());
    running.stop().await;
}

/// A bot whose row lost its server keeps the server of the spec last sent,
/// and gets that server's texts; while that server's texts don't read back,
/// nothing is sent, the last spec kept (R132).
#[tokio::test(start_paused = true)]
async fn the_fallback_server_brings_its_own_texts_and_an_unreadable_row_sends_nothing() {
    let (_registry, store, mut stream, running) = running_bot().await;
    store.texts("play.example.com", &["Already connected"]);
    store.change(bot(1), |record| record.bot.server = None);

    running.hints.changed(Change::Store);
    let Some(ServerMessage::Update(fallback)) = stream.next().await else {
        panic!("an update");
    };
    {
        let mut state = store.state();
        state.fleet.server_texts.readable.clear();
        state.fleet.server_texts.unreadable.insert(
            server_key("play.example.com"),
            UnreadableSettings { rowid: 1, id: None },
        );
    }
    running.hints.changed(Change::Store);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let quiet = stream.queued.try_recv();

    assert_eq!(fallback.server, "play.example.com".parse().unwrap());
    assert_eq!(fallback.conflict_texts, texts(&["Already connected"]));
    assert!(
        quiet.is_err(),
        "nothing while the row doesn't read: {quiet:?}"
    );
    running.stop().await;
}

// --- Stopping and moving ------------------------------------------------------------------

/// A reconciler whose agent 1 runs bot 1, reconciled; and agent 1's stream.
async fn running_bot() -> (AgentRegistry, Arc<Store>, Stream, Running) {
    let registry = registry();
    let store = Store::with(vec![on(1, 1)], &[1]);
    let mut stream = connect(&registry, 1, 5, vec![online(1)], 8);
    let running = start(&registry, &store, 8);
    assert_eq!(
        reconciled(stream.next().await),
        [(bot(1), DesiredRunState::Running, None)]
    );
    (registry, store, stream, running)
}

#[tokio::test(start_paused = true)]
async fn a_bot_that_stops_qualifying_is_stopped_then_unassigned_at_its_bound() {
    let (registry, store, mut stream, running) = running_bot().await;
    store.change(bot(1), |record| {
        record.bot.desired = DesiredRunState::Stopped;
    });

    running.hints.changed(Change::Store);
    let Some(ServerMessage::Update(spec)) = stream.next().await else {
        panic!("a spec update");
    };
    tokio::time::sleep(STOP_BOUND.checked_sub(Duration::from_millis(200)).unwrap()).await;
    let before = stream.queued.try_recv().is_ok();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after = stream.next().await;

    assert_eq!(spec.desired, DesiredRunState::Stopped);
    assert!(!before, "nothing before the bound");
    assert!(matches!(after, Some(ServerMessage::Unassign(id)) if id == bot(1)));
    assert_eq!(
        store.calls(),
        [Call::Unassign(
            bot(1),
            agent(1),
            UnassignReason::Unqualified(Unplaced::Stopped),
            registry.latest(bot(1)).map(|report| report.last())
        )]
    );
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_passed_bound_whose_unassign_fails_is_tried_again_on_the_interval_not_at_once() {
    let (_registry, store, mut stream, running) = running_bot().await;
    store.change(bot(1), |record| {
        record.bot.desired = DesiredRunState::Stopped;
    });
    running.hints.changed(Change::Store);
    assert!(matches!(
        stream.next().await,
        Some(ServerMessage::Update(_))
    ));
    store.state().failing_unassigns = true;

    tokio::time::sleep(STOP_BOUND + Duration::from_secs(1)).await;
    let at_the_bound = store.state().unassign_attempts;
    tokio::time::sleep(RECONCILE_INTERVAL).await;
    let after_an_interval = store.state().unassign_attempts;

    assert_eq!((at_the_bound, after_an_interval), (1, 2));
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_resting_report_unassigns_before_the_bound() {
    let (registry, store, mut stream, running) = running_bot().await;
    store.change(bot(1), |record| {
        record.bot.desired = DesiredRunState::Stopped;
    });
    running.hints.changed(Change::Store);
    assert!(matches!(
        stream.next().await,
        Some(ServerMessage::Update(_))
    ));

    let (verdict, _) = registry.admit(
        &stream.lease,
        &AgentMessage::Status(snapshot(1, BotState::Stopped)),
    );
    running.hints.changed(Change::Settled(agent(1)));

    assert!(matches!(
        verdict,
        crate::grpc::registry::Verdict::Held { settles: true, .. }
    ));
    assert!(matches!(stream.next().await, Some(ServerMessage::Unassign(id)) if id == bot(1)));
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_deleted_bot_is_unassigned_at_once() {
    let (_registry, store, mut stream, running) = running_bot().await;
    store.state().fleet.bots.clear();

    running.hints.changed(Change::Store);

    assert!(matches!(stream.next().await, Some(ServerMessage::Unassign(id)) if id == bot(1)));
    assert_eq!(store.calls(), [], "its row is gone: nothing to record");
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_paused_surplus_bot_moves_after_its_removed_with_its_restore() {
    let registry = registry();
    let mut sticky = on(1, 1);
    sticky.bot.last = Some(LastReport {
        state: paused(),
        at: at(T0),
    });
    let store = Store::with(vec![sticky, on(2, 1)], &[1, 2]);
    // Agent 1 came back with room for one: bot 2 stays, bot 1 rests and
    // is left out.
    let mut first = connect(&registry, 1, 1, vec![paused_snapshot(1), online(2)], 8);
    let mut second = connect(&registry, 2, 5, Vec::new(), 8);
    let running = start(&registry, &store, 8);
    assert_eq!(
        reconciled(first.next().await),
        [(bot(2), DesiredRunState::Running, None)]
    );
    assert_eq!(
        reconciled(second.next().await),
        [],
        "not before its Removed"
    );

    let (verdict, _) = registry.admit(
        &first.lease,
        &AgentMessage::Event(BotEvent {
            bot_id: bot(1),
            at: at(T0),
            kind: BotEventKind::Removed,
        }),
    );
    running.hints.changed(Change::Settled(agent(1)));

    assert_eq!(verdict, crate::grpc::registry::Verdict::Settled);
    let Some(ServerMessage::Assign(assignment)) = second.next().await else {
        panic!("an assignment");
    };
    assert_eq!(assignment.spec.id, bot(1));
    assert!(matches!(assignment.restore, Some(StickyState::Paused(_))));
    let calls = store.calls();
    assert!(
        matches!(calls.first(), Some(Call::Unassign(id, a, UnassignReason::Capacity, Some(_))) if *id == bot(1) && *a == agent(1)),
        "{calls:?}"
    );
    assert!(calls.contains(&Call::Assign(bot(1), agent(2))));
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_removed_from_an_older_stream_never_clears_a_bot_its_newer_stream_was_given() {
    let registry = registry();
    let store = Store::with(vec![on(1, 1)], &[1, 2]);
    let mut old = connect(&registry, 1, 5, vec![online(1)], 8);
    let mut other = connect(&registry, 2, 5, Vec::new(), 8);
    let running = start(&registry, &store, 8);
    assert_eq!(ids_of(reconciled(old.next().await)), [bot(1)]);
    assert_eq!(reconciled(other.next().await), []);
    // The bot is stopped and let go: held releasing on the old stream.
    store.change(bot(1), |record| {
        record.bot.desired = DesiredRunState::Stopped;
    });
    running.hints.changed(Change::Store);
    assert!(matches!(old.next().await, Some(ServerMessage::Update(_))));
    let _ = registry.admit(
        &old.lease,
        &AgentMessage::Status(snapshot(1, BotState::Stopped)),
    );
    running.hints.changed(Change::Settled(agent(1)));
    assert!(matches!(old.next().await, Some(ServerMessage::Unassign(id)) if id == bot(1)));
    // It qualifies again, and its Removed's clearing stays busy twice.
    store.change(bot(1), |record| {
        record.bot.desired = DesiredRunState::Running;
    });
    store.state().busy_detaches = 2;
    let _ = registry.admit(
        &old.lease,
        &AgentMessage::Event(BotEvent {
            bot_id: bot(1),
            at: at(T0),
            kind: BotEventKind::Removed,
        }),
    );
    running.hints.changed(Change::Settled(agent(1)));
    tokio::time::sleep(NOW).await;

    // The agent reconnects without the bot: its row still names it, so its
    // new stream is given the bot back.
    let _ = old.lease.free();
    let mut new = connect(&registry, 1, 5, Vec::new(), 8);
    running.hints.changed(Change::Welcomed(agent(1)));
    let given = ids_of(reconciled(new.next().await));
    running.hints.changed(Change::Store);
    tokio::time::sleep(NOW).await;
    let assigned = store.agent_of(bot(1));
    // The new stream goes before its own Removed: nothing may place the
    // bot elsewhere.
    let _ = new.lease.free();
    running.hints.changed(Change::Ended(agent(1)));
    let elsewhere = other.next().await;

    assert_eq!(given, [bot(1)]);
    assert_eq!(assigned, Some(agent(1)), "the old stream's note is stale");
    assert_eq!(elsewhere.map(|_| ()), None);
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_busy_detach_is_tried_again_on_the_next_pass() {
    let (registry, store, stream, running) = running_bot().await;
    // Bot 1 stopped qualifying and rested: it's released.
    store.change(bot(1), |record| record.owner_enabled = false);
    let _ = registry.admit(
        &stream.lease,
        &AgentMessage::Status(snapshot(1, BotState::Stopped)),
    );
    running.hints.changed(Change::Store);
    tokio::time::sleep(NOW).await;
    store.state().busy_detaches = 1;
    let _ = registry.admit(
        &stream.lease,
        &AgentMessage::Event(BotEvent {
            bot_id: bot(1),
            at: at(T0),
            kind: BotEventKind::Removed,
        }),
    );

    running.hints.changed(Change::Settled(agent(1)));
    tokio::time::sleep(NOW).await;
    let busy = store.agent_of(bot(1));
    running.hints.changed(Change::Store);
    tokio::time::sleep(NOW).await;

    assert_eq!(busy, Some(agent(1)), "the busy detach changed nothing");
    assert_eq!(store.agent_of(bot(1)), None, "the next pass detached it");
    running.stop().await;
}

// --- Assignments an agent never got --------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn an_assignment_its_agent_never_got_is_sent_again_on_the_next_pass() {
    let registry = registry();
    let store = Store::with(vec![record(2)], &[1]);
    store.change(bot(2), |record| {
        record.bot.desired = DesiredRunState::Stopped;
    });
    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);
    let running = start(&registry, &store, 8);
    assert_eq!(reconciled(stream.next().await), []);
    // As if an earlier pass stored it but its message couldn't be sent.
    store.change(bot(2), |record| {
        record.bot.desired = DesiredRunState::Running;
        record.bot.agent = Some(agent(1));
    });

    running.hints.changed(Change::Store);
    let resent = stream.next().await;
    running.hints.changed(Change::Store);
    let again = stream.next().await;

    assert!(
        matches!(resent, Some(ServerMessage::Assign(ref a)) if a.spec.id == bot(2)),
        "{resent:?}"
    );
    assert_eq!(again.map(|_| ()), None, "held now: sent once");
    assert!(registry.held(agent(1), bot(2)).is_some());
    assert_eq!(store.calls(), [], "nothing more to store");
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn an_undelivered_assignment_that_no_longer_qualifies_is_logged_not_sent() {
    let buffer = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: "info".parse().unwrap(),
    };
    let subscriber =
        tracing_subscriber::registry().with(layer_with(&config, &NoRules, false, buffer.clone()));
    let _guard = tracing::subscriber::set_default(subscriber);
    let registry = registry();
    let store = Store::with(Vec::new(), &[1]);
    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);
    let running = start(&registry, &store, 8);
    assert_eq!(reconciled(stream.next().await), []);
    let mut stopped = on(2, 1);
    stopped.bot.desired = DesiredRunState::Stopped;
    store.state().fleet.bots.push(stopped);

    running.hints.changed(Change::Store);
    let sent = stream.next().await;

    assert_eq!(sent.map(|_| ()), None);
    let logged = buffer
        .lines_with("an assignment its agent never got isn't sent again: its next hello settles it")
        .unwrap();
    assert_eq!(logged.len(), 1, "{}", buffer.text());
    assert_eq!(logged[0]["reason"], "stopped");
    running.stop().await;
}

// --- Bots that don't read back ------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn an_unreadable_bot_is_left_alone_and_logged_each_pass_and_the_rest_reconciled() {
    let buffer = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: "info".parse().unwrap(),
    };
    let subscriber =
        tracing_subscriber::registry().with(layer_with(&config, &NoRules, false, buffer.clone()));
    let _guard = tracing::subscriber::set_default(subscriber);
    let registry = registry();
    let store = Store::with(vec![record(2)], &[1]);
    store.state().fleet.unreadable.push(UnreadableBot {
        bot: bot(9),
        agent: Some(agent(1)),
        identity: None,
        table: "bots",
        column: "server_address",
        rowid: 9,
        by_server: false,
    });
    // Its agent's stream holds it from before its row stopped reading back.
    let mut stream = connect(&registry, 1, 2, Vec::new(), 8);
    let generation = registry.placement_view().first().unwrap().generation;
    registry
        .settle(
            agent(1),
            generation,
            vec![(
                bot(9),
                crate::grpc::registry::HoldEntry {
                    hold: crate::grpc::registry::Hold::Assigned,
                    since: at(T0),
                    sent: None,
                    stopping: None,
                },
            )],
            ServerMessage::Reconcile(Vec::new()),
        )
        .unwrap();
    assert_eq!(reconciled(stream.next().await), []);

    let running = start(&registry, &store, 8);
    let first = stream.next().await;
    running.hints.changed(Change::Store);
    let second = stream.next().await;

    assert!(
        matches!(first, Some(ServerMessage::Assign(ref a)) if a.spec.id == bot(2)),
        "{first:?}"
    );
    assert_eq!(second.map(|_| ()), None, "never unassigned");
    let logged = buffer
        .lines_with("a bot doesn't read back: the reconciler leaves it alone until it's fixed")
        .unwrap();
    assert_eq!(logged.len(), 2, "once per pass");
    assert_eq!(logged[0]["level"], "ERROR");
    running.stop().await;
}

/// Two servers' settings rows whose texts don't read back in `store`: one,
/// `settings`, with three bots on its server; one whose ID doesn't read,
/// with none. And a bot whose own mode doesn't read back.
fn broken_settings(store: &Store, settings: ServerSettingsId) {
    let mut state = store.state();
    let texts = &mut state.fleet.server_texts.unreadable;
    texts.insert(
        server_key("broken.example.com"),
        UnreadableSettings {
            rowid: 4,
            id: Some(settings),
        },
    );
    // A row no bot plays on is logged too; this one's ID doesn't read.
    texts.insert(
        server_key("empty.example.com"),
        UnreadableSettings { rowid: 5, id: None },
    );
    for n in [7, 8, 9] {
        state.fleet.unreadable.push(UnreadableBot {
            bot: bot(n),
            agent: None,
            identity: None,
            table: "server_settings",
            column: "conflict_texts",
            rowid: 4,
            by_server: true,
        });
    }
    state.fleet.unreadable.push(UnreadableBot {
        bot: bot(6),
        agent: None,
        identity: None,
        table: "modes",
        column: "definition_json",
        rowid: 3,
        by_server: false,
    });
}

/// A server's settings row whose texts don't read back is logged once a
/// pass, with its ID and key, however many bots play there; those bots
/// aren't logged on their own, and a bot whose own row doesn't read back
/// still is (R132, PR #79's review).
#[tokio::test(start_paused = true)]
async fn an_unreadable_servers_settings_row_is_logged_once_a_pass_not_once_per_bot() {
    let buffer = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: "info".parse().unwrap(),
    };
    let subscriber =
        tracing_subscriber::registry().with(layer_with(&config, &NoRules, false, buffer.clone()));
    let _guard = tracing::subscriber::set_default(subscriber);
    let registry = registry();
    let store = Store::with(vec![record(2)], &[1]);
    let settings = ServerSettingsId::new_v7(at(T0), [4; 10]).unwrap();
    broken_settings(&store, settings);
    // A welcomed stream that holds nothing.
    let mut stream = connect(&registry, 1, 2, Vec::new(), 8);
    let generation = registry.placement_view().first().unwrap().generation;
    registry
        .settle(
            agent(1),
            generation,
            Vec::new(),
            ServerMessage::Reconcile(Vec::new()),
        )
        .unwrap();
    assert_eq!(reconciled(stream.next().await), []);

    let running = start(&registry, &store, 8);
    let first = stream.next().await;
    running.hints.changed(Change::Store);
    let second = stream.next().await;

    assert!(
        matches!(first, Some(ServerMessage::Assign(ref a)) if a.spec.id == bot(2)),
        "{first:?}"
    );
    assert_eq!(
        second.map(|_| ()),
        None,
        "the bots on that server are left alone"
    );
    let rows = buffer
        .lines_with("a server's settings don't read back: the reconciler leaves its bots alone until they're fixed")
        .unwrap();
    assert_eq!(rows.len(), 4, "each row once a pass, two passes");
    let mut servers: Vec<&str> = rows
        .iter()
        .map(|line| line["server"].as_str().unwrap())
        .collect();
    servers.sort_unstable();
    assert_eq!(
        servers,
        [
            "broken.example.com:25565",
            "broken.example.com:25565",
            "empty.example.com:25565",
            "empty.example.com:25565"
        ]
    );
    for line in &rows {
        assert_eq!(line["level"], "ERROR");
        let id = &line["server_settings_id"];
        if line["server"] == "broken.example.com:25565" {
            assert_eq!(id, &settings.to_string(), "{line}");
        } else {
            assert!(id.is_null(), "an ID that doesn't read isn't named: {line}");
        }
    }
    let bots = buffer
        .lines_with("a bot doesn't read back: the reconciler leaves it alone until it's fixed")
        .unwrap();
    assert_eq!(
        bots.len(),
        2,
        "only the bot whose own row doesn't read, once a pass"
    );
    assert!(bots.iter().all(|line| line["bot_id"] == bot(6).to_string()));
    running.stop().await;
}

// --- The placement gate -------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_late_holder_keeps_its_bot_off_the_agent_that_came_back_first() {
    let registry = registry();
    let store = Store::with(vec![record(1)], &[1, 2]);
    let mut early = connect(&registry, 2, 5, Vec::new(), 8);
    let running = start(&registry, &store, 8);
    assert_eq!(reconciled(early.next().await), [], "the gate is closed");

    let mut late = connect(&registry, 1, 5, vec![online(1)], 8);
    running.hints.changed(Change::Welcomed(agent(1)));

    assert_eq!(reconciled(late.next().await), []);
    assert_eq!(early.next().await.map(|_| ()), None, "fenced by its stray");
    assert_eq!(store.calls(), [Call::Stray(bot(1), agent(1))]);
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn the_gate_opens_once_every_enabled_agent_is_welcomed() {
    let registry = registry();
    let store = Store::with(vec![record(1)], &[1]);
    let running = start(&registry, &store, 8);
    tokio::time::sleep(NOW).await;

    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);
    running.hints.changed(Change::Welcomed(agent(1)));

    assert_eq!(
        reconciled(stream.next().await),
        [(bot(1), DesiredRunState::Running, None)]
    );
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn the_gate_opens_at_the_grace_and_not_a_moment_before() {
    let registry = registry();
    let store = Store::with(vec![record(1)], &[1, 2]);
    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);
    let running = start(&registry, &store, 8);
    assert_eq!(reconciled(stream.next().await), []);

    tokio::time::sleep(
        PLACEMENT_GRACE
            .checked_sub(Duration::from_millis(1))
            .unwrap(),
    )
    .await;
    let before = stream.queued.try_recv().is_ok();
    tokio::time::sleep(Duration::from_millis(1)).await;

    assert!(!before);
    assert!(matches!(
        stream.next().await,
        Some(ServerMessage::Assign(_))
    ));
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_disabled_agent_never_holds_the_gate() {
    let registry = registry();
    // Agent 2 is disabled: not among the enabled agents.
    let store = Store::with(vec![record(1)], &[1]);
    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);

    let running = start(&registry, &store, 8);

    assert_eq!(
        reconciled(stream.next().await),
        [(bot(1), DesiredRunState::Running, None)]
    );
    running.stop().await;
}

#[test]
fn the_grace_covers_an_agents_way_back_after_a_restart() {
    // Its keepalive notices the old server, it reconnects, and its hello is
    // welcomed.
    let way_back = KEEPALIVE_INTERVAL + KEEPALIVE_TIMEOUT + RECONNECT_DELAY_MAX + WELCOME_TIMEOUT;

    assert!(
        PLACEMENT_GRACE >= way_back,
        "{PLACEMENT_GRACE:?} < {way_back:?}"
    );
}

// --- The task ---------------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn the_stop_token_ends_the_task() {
    let registry = registry();
    let store = Store::with(Vec::new(), &[]);
    let running = start(&registry, &store, 8);
    tokio::time::sleep(NOW).await;

    running.stop.cancel();
    let ended = tokio::time::timeout(NOW, running.task).await;

    assert!(matches!(ended, Ok(Ok(()))), "{ended:?}");
}

#[tokio::test(start_paused = true)]
async fn an_offline_bot_is_never_placed_while_offline_accounts_are_off() {
    let registry = registry();
    let store = Store::with(vec![record(1)], &[1]);
    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);

    let running = start_with(&registry, &store, 8, false);

    assert_eq!(reconciled(stream.next().await), []);
    assert_eq!(store.calls(), []);
    running.stop().await;
}

// --- What the passes tell the live events sockets (ADR-0020, S42, R37) -----------------------

#[tokio::test(start_paused = true)]
async fn a_pass_tells_the_bots_it_held_or_wrote_after_its_writes() {
    let registry = registry();
    let store = Store::with(vec![on(1, 1)], &[1]);
    let mut stream = connect(&registry, 1, 5, vec![online(1)], 8);
    let (running, told) = start_telling(&registry, &store, &ManualClock::new(at(T0)));
    assert_eq!(ids_of(reconciled(stream.next().await)), [bot(1)]);
    let after_the_welcome = told.take();

    store.state().fleet.bots.push(record(2));
    running.hints.changed(Change::Store);
    assert!(matches!(
        stream.next().await,
        Some(ServerMessage::Assign(_))
    ));

    assert_eq!(after_the_welcome, [(0, vec![bot(1)])], "held, not written");
    assert_eq!(store.calls(), [Call::Assign(bot(2), agent(1))]);
    assert_eq!(told.take(), [(1, vec![bot(2)])], "after its assignment");
    running.stop().await;
}

/// Bot 2 on agent 2, which has no stream; agent 1's stream, reconciled
/// empty; and what the first pass told.
async fn waiting_on_agent_2(
    clock: &ManualClock,
    store: &Arc<Store>,
) -> (AgentRegistry, Stream, Running, Arc<Told>) {
    let registry = registry();
    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);
    let (running, told) = start_telling(&registry, store, clock);
    assert_eq!(reconciled(stream.next().await), []);
    (registry, stream, running, told)
}

#[tokio::test(start_paused = true)]
async fn the_first_pass_only_records_what_each_bots_agent_shows() {
    let store = Store::with(vec![on(2, 2)], &[1, 2]);

    let (_registry, mut stream, running, told) =
        waiting_on_agent_2(&ManualClock::new(at(T0)), &store).await;
    running.hints.changed(Change::Store);
    let again = stream.next().await;

    assert_eq!(again.map(|_| ()), None);
    assert_eq!(told.take(), []);
    assert_eq!(store.calls(), []);
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn an_agent_disabled_between_passes_tells_its_bots() {
    let store = Store::with(vec![on(2, 2)], &[1, 2]);
    let (_registry, mut stream, running, told) =
        waiting_on_agent_2(&ManualClock::new(at(T0)), &store).await;

    store.state().fleet.enabled_agents.remove(&agent(2));
    running.hints.changed(Change::Store);
    let quiet = stream.next().await;

    assert_eq!(quiet.map(|_| ()), None);
    assert_eq!(store.calls(), []);
    assert_eq!(told.take(), [(0, vec![bot(2)])]);
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_certificate_that_expires_between_passes_tells_its_agents_bots() {
    let store = Store::with(vec![on(2, 2)], &[1, 2]);
    store
        .state()
        .fleet
        .agent_expiry
        .insert(agent(2), at("2027-01-15T08:01:00Z"));
    let clock = ManualClock::new(at(T0));
    let (_registry, mut stream, running, told) = waiting_on_agent_2(&clock, &store).await;
    running.hints.changed(Change::Store);
    assert_eq!(stream.next().await.map(|_| ()), None);
    let before_it_expired = told.take();

    clock.advance(Duration::from_secs(60));
    running.hints.changed(Change::Store);
    assert_eq!(stream.next().await.map(|_| ()), None);

    assert_eq!(before_it_expired, []);
    assert_eq!(told.take(), [(0, vec![bot(2)])]);
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_bot_released_by_the_host_between_passes_tells_it() {
    let mut stopped = on(2, 2);
    stopped.bot.desired = DesiredRunState::Stopped;
    // Agent 2 is disabled: its bots wait for the host's release.
    let store = Store::with(vec![stopped], &[1]);
    let (_registry, mut stream, running, told) =
        waiting_on_agent_2(&ManualClock::new(at(T0)), &store).await;
    let calls = store.calls().len();

    store.change(bot(2), |record| record.bot.agent = None);
    running.hints.changed(Change::Store);
    assert_eq!(stream.next().await.map(|_| ()), None);

    assert_eq!(told.take(), [(calls, vec![bot(2)])]);
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_failed_read_tells_nothing_and_the_next_pass_compares_with_the_last_read() {
    let store = Store::with(vec![on(2, 2)], &[1, 2]);
    let (_registry, mut stream, running, told) =
        waiting_on_agent_2(&ManualClock::new(at(T0)), &store).await;

    {
        let mut state = store.state();
        state.fail_reads = true;
        state.fleet.enabled_agents.remove(&agent(2));
    }
    running.hints.changed(Change::Store);
    assert_eq!(stream.next().await.map(|_| ()), None);
    let after_the_failed_read = told.take();
    store.state().fail_reads = false;
    running.hints.changed(Change::Store);
    assert_eq!(stream.next().await.map(|_| ()), None);

    assert_eq!(after_the_failed_read, []);
    assert_eq!(told.take(), [(0, vec![bot(2)])]);
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn an_assignment_whose_stream_ended_before_its_message_still_tells_its_bot() {
    let registry = registry();
    let mut stopped = record(2);
    stopped.bot.desired = DesiredRunState::Stopped;
    let store = Store::with(vec![stopped], &[1]);
    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);
    let (running, told) = start_telling(&registry, &store, &ManualClock::new(at(T0)));
    assert_eq!(reconciled(stream.next().await), []);
    store.change(bot(2), |record| {
        record.bot.desired = DesiredRunState::Running;
    });
    store.state().end_on_assign = Some((registry.clone(), agent(1)));

    running.hints.changed(Change::Store);
    tokio::time::sleep(NOW).await;

    assert_eq!(registry.held(agent(1), bot(2)), None, "never held");
    assert_eq!(told.take(), [(1, vec![bot(2)])], "its row changed");
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_change_sent_to_a_held_bot_tells_it() {
    let registry = registry();
    let store = Store::with(vec![on(1, 1)], &[1]);
    let mut stream = connect(&registry, 1, 5, vec![online(1)], 8);
    let (running, told) = start_telling(&registry, &store, &ManualClock::new(at(T0)));
    assert_eq!(ids_of(reconciled(stream.next().await)), [bot(1)]);
    told.take();
    store.change(bot(1), |record| {
        record.bot.server = Some("mc.example.com".parse().unwrap());
    });

    running.hints.changed(Change::Store);
    let sent = stream.next().await;

    assert!(matches!(sent, Some(ServerMessage::Update(_))), "{sent:?}");
    assert_eq!(store.calls(), []);
    assert_eq!(told.take(), [(0, vec![bot(1)])]);
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_new_stream_whose_reconcile_is_held_back_still_tells_its_agents_bots() {
    let registry = registry();
    let mut stray = record(2);
    stray.bot.desired = DesiredRunState::Stopped;
    let store = Store::with(vec![on(1, 1), stray], &[1]);
    let mut old = connect(&registry, 1, 5, vec![online(1)], 8);
    let (running, told) = start_telling(&registry, &store, &ManualClock::new(at(T0)));
    assert_eq!(ids_of(reconciled(old.next().await)), [bot(1)]);
    told.take();

    // Its agent reconnects running a bot it wasn't given, whose fence is
    // busy: the new stream's full reconcile waits, so nothing holds bot 1.
    let _ = old.lease.free();
    store.state().busy_strays = 1;
    let mut new = connect(&registry, 1, 5, vec![online(1), online(2)], 8);
    running.hints.changed(Change::Welcomed(agent(1)));
    let held_back = new.next().await;

    assert_eq!(held_back.map(|_| ()), None, "no reconcile yet");
    let calls = store.calls().len();
    assert_eq!(told.take(), [(calls, vec![bot(1)])], "its new stream");
    running.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_bot_a_pass_assigned_is_not_told_again_by_the_next() {
    let registry = registry();
    let store = Store::with(Vec::new(), &[1]);
    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);
    let (running, told) = start_telling(&registry, &store, &ManualClock::new(at(T0)));
    assert_eq!(reconciled(stream.next().await), []);
    store.state().fleet.bots.push(record(2));
    running.hints.changed(Change::Store);
    assert!(matches!(
        stream.next().await,
        Some(ServerMessage::Assign(_))
    ));
    let assigned = told.take();

    running.hints.changed(Change::Store);
    let quiet = stream.next().await;

    assert_eq!(assigned, [(1, vec![bot(2)])]);
    assert_eq!(quiet.map(|_| ()), None);
    assert_eq!(told.take(), [], "nothing changed since its assignment");
    running.stop().await;
}

/// A release record of bot 2 on agent 1, whose row still names it, and an
/// agent 1 whose hello doesn't list it: the welcome's pass detaches it, and
/// the next pass tells nothing more.
#[tokio::test(start_paused = true)]
async fn a_bot_a_pass_detached_is_not_told_again_by_the_next() {
    let registry = registry();
    let mut stopped = on(2, 1);
    stopped.bot.desired = DesiredRunState::Stopped;
    let identity = stopped.identity.clone();
    let store = Store::with(vec![stopped], &[1]);
    store.state().fleet.releases.push(ReleaseRecord {
        bot: bot(2),
        agent: agent(1),
        identity,
    });
    let mut stream = connect(&registry, 1, 5, Vec::new(), 8);
    let (running, told) = start_telling(&registry, &store, &ManualClock::new(at(T0)));
    let welcomed = stream.next().await;
    let calls = store.calls().len();
    let detached = (store.agent_of(bot(2)), told.take());

    running.hints.changed(Change::Store);
    let quiet = stream.next().await;

    assert!(
        matches!(welcomed, Some(ServerMessage::Reconcile(_))),
        "{welcomed:?}"
    );
    assert_eq!(
        detached,
        (None, vec![(calls, vec![bot(2)])]),
        "after its writes"
    );
    assert_eq!(quiet.map(|_| ()), None);
    assert_eq!(told.take(), [], "nothing changed since its detach");
    running.stop().await;
}
