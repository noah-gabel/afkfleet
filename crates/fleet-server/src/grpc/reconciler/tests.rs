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
use fleet_core::disconnect::ConflictKind;
use fleet_core::id::{AccountId, AgentId, BotId, RequestId};
use fleet_core::mode::ModeDefinition;
use fleet_core::pki::CertFingerprint;
use fleet_core::placement::{DetachReason, McIdentity, UnassignReason, Unplaced};
use fleet_core::value::{AgentCapacity, AgentName};
use fleet_proto::control::{
    AgentMessage, BotEvent, BotEventKind, RECONNECT_DELAY_MAX, ServerMessage, WELCOME_TIMEOUT,
    Welcome,
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
use crate::ports::assignments::AssignError;
use crate::ports::bots::{Bot, BotRecord, Fleet, ReleaseRecord};
use crate::ports::live_agents::LiveAgents as _;
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

    async fn assign(&self, bot: BotId, agent: AgentId) -> Result<bool, AssignError> {
        let mut state = self.state();
        state.calls.push(Call::Assign(bot, agent));
        if let Some((registry, ending)) = state.end_on_assign.take() {
            let _ = registry.end(ending, StreamEnd::StreamError);
        }
        let enabled = state.fleet.enabled_agents.contains(&agent);
        let Some(record) = state
            .fleet
            .bots
            .iter_mut()
            .find(|record| record.bot.id == bot && record.bot.agent.is_none())
        else {
            return Ok(false);
        };
        if !enabled {
            return Ok(false);
        }
        record.bot.agent = Some(agent);
        Ok(true)
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
    let (hints, inbox) = hints(depth);
    let clock = ManualClock::new(at(T0));
    let reconciler = Reconciler::new(
        registry.clone(),
        Arc::clone(store) as Arc<dyn Assignments>,
        Arc::new(clock),
        inbox,
        ReconcileTimings::default(),
        allow_offline,
    );
    let stop = CancellationToken::new();
    let task = tokio::spawn(reconciler.run(stop.clone()));
    Running { hints, stop, task }
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

    assert_eq!(verdict, crate::grpc::registry::Verdict::Settled);
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
    // Its keepalive notices the old server (10 s and 20 s), it reconnects,
    // and its hello is welcomed.
    let way_back = Duration::from_secs(30) + RECONNECT_DELAY_MAX + WELCOME_TIMEOUT;

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
