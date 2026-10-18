//! A managed agent's inside without a server (paused time): a real fleet on
//! fleet-testkit's fake connector, the managed credentials over the real
//! outbox, and the conductor in its task; the test plays the link and the
//! server, answering every session request with an offline grant.

use core::num::NonZeroUsize;
use core::time::Duration;
use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_agent::managed::conductor::{self, Conductor, ConductorParts, Inbox, Instruction};
use fleet_agent::managed::credentials::{LinkGeneration, ManagedCredentials, SessionAnswer};
use fleet_agent::managed::outbound::{Outbound, Outbox};
use fleet_core::bot::{BotAccount, BotSnapshot, BotSpec, BotState, DesiredRunState, StickyState};
use fleet_core::disconnect::ConflictTexts;
use fleet_core::id::BotId;
use fleet_core::mc::{SessionCredentials, SessionEvent};
use fleet_core::mode::ModePreset;
use fleet_core::resilience::{CircuitPolicy, RetryPolicy};
use fleet_core::value::{AgentCapacity, AgentName, AgentVersion, McUsername};
use fleet_proto::control::{AgentMessage, Assignment, BotEventKind, Hello, SessionGrant};
use fleet_runtime::{Fleet, FleetParts, RuntimeClock, RuntimeConfig};
use fleet_testkit::mc::{EmitOutcome, FakeConnector, SessionController};
use tokio::task::JoinSet;
use tokio_stream::StreamExt as _;
use tokio_util::sync::CancellationToken;

pub(crate) const fn secs(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

/// The run's start.
pub(crate) fn anchor() -> DateTime<Utc> {
    DateTime::from_timestamp(1_790_000_000, 0).unwrap()
}

/// Bot `n`.
pub(crate) fn bot(n: u8) -> BotId {
    BotId::new_v7(anchor(), [n; 10]).unwrap()
}

/// Bot `n`'s spec on account `account`, running.
pub(crate) fn spec_on(n: u8, account: u8) -> BotSpec {
    BotSpec {
        id: bot(n),
        account: BotAccount::Offline(format!("AfkBot{account}").parse().unwrap()),
        server: "mc.example.com".try_into().unwrap(),
        mode: ModePreset::Afk.definition(),
        desired: DesiredRunState::Running,
        conflict_texts: ConflictTexts::default(),
    }
}

/// Bot `n`'s spec, on its own account.
pub(crate) fn spec(n: u8) -> BotSpec {
    spec_on(n, n)
}

/// An assignment of `spec` with `restore`.
pub(crate) fn assign(spec: BotSpec, restore: Option<StickyState>) -> Assignment {
    Assignment { spec, restore }
}

/// Lets every task run until it waits; time doesn't move.
pub(crate) async fn settle() {
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
}

/// Runtime settings small enough to reach their bounds.
pub(crate) fn runtime() -> RuntimeConfig {
    RuntimeConfig {
        max_bots: NonZeroUsize::new(4).unwrap(),
        ..RuntimeConfig::default()
    }
}

/// The rig; see the [module docs](self).
pub(crate) struct Rig {
    pub(crate) fleet: Fleet,
    pub(crate) fake: FakeConnector,
    pub(crate) outbox: Outbox,
    pub(crate) credentials: Arc<ManagedCredentials<Outbox>>,
    pub(crate) inbox: Inbox,
    stop: CancellationToken,
    tasks: JoinSet<()>,
    /// The conductor, while a test holds it back.
    parked: Option<Conductor>,
    stream: Option<(LinkGeneration, Outbound)>,
    /// The offline account of each bot the rig answers sessions for.
    accounts: HashMap<BotId, McUsername>,
    /// What the streams sent but hellos, heartbeats and session requests.
    pub(crate) sent: Vec<AgentMessage>,
}

impl Rig {
    /// The rig with [`runtime`]'s settings, its conductor running.
    pub(crate) fn new() -> Self {
        let mut rig = Self::parked(runtime());
        rig.conduct();
        rig
    }

    /// The rig with `config`, its conductor held back until
    /// [`Rig::conduct`].
    pub(crate) fn parked(config: RuntimeConfig) -> Self {
        let fake = FakeConnector::new();
        let clock = RuntimeClock::new(anchor());
        let outbox = Outbox::new(config.max_bots, config.chat_queue);
        let credentials = Arc::new(ManagedCredentials::new(
            outbox.clone(),
            clock,
            config.connect_timeout,
            config.session_request_timeout,
        ));
        let (fleet, supervisor) = Fleet::new(FleetParts {
            connector: Arc::new(fake.clone()),
            credentials: Arc::clone(&credentials),
            retry: RetryPolicy::try_new(secs(5), secs(300), secs(300)).unwrap(),
            circuit: CircuitPolicy::try_new(NonZeroUsize::new(8).unwrap(), secs(600), secs(900))
                .unwrap(),
            config,
            anchor: anchor(),
            seed: 1,
        })
        .unwrap();
        let (inbox, instructions) = conductor::inbox();
        let conductor = Conductor::new(ConductorParts {
            fleet: fleet.clone(),
            events: fleet.subscribe(),
            outbox: outbox.clone(),
            credentials: Arc::clone(&credentials),
            inbound: instructions,
            clock,
            max_bots: config.max_bots.get(),
            chat_queue: config.chat_queue.get(),
            event_buffer: config.event_buffer.get(),
        });
        let stop = CancellationToken::new();
        let mut tasks = JoinSet::new();
        tasks.spawn(supervisor.run(stop.child_token()));
        Self {
            fleet,
            fake,
            outbox,
            credentials,
            inbox,
            stop,
            tasks,
            parked: Some(conductor),
            stream: None,
            accounts: HashMap::new(),
            sent: Vec::new(),
        }
    }

    /// Starts the conductor's task.
    pub(crate) fn conduct(&mut self) {
        if let Some(conductor) = self.parked.take() {
            self.tasks.spawn(conductor.run(self.stop.child_token()));
        }
    }

    /// The current stream's generation.
    pub(crate) fn generation(&self) -> LinkGeneration {
        self.stream.as_ref().expect("a stream is open").0
    }

    /// Ends the current stream, as the link does at every attempt's end.
    pub(crate) fn end(&mut self) {
        if let Some((generation, _)) = self.stream.take() {
            self.outbox.ended(generation);
            self.credentials.link_ended(generation);
        }
    }

    /// A new stream with `running` in its hello, welcomed, without the
    /// conductor's barrier: for a test whose conductor is held back.
    pub(crate) async fn open(&mut self, running: Vec<BotSnapshot>) {
        self.end();
        let (generation, mut outbound) = self.outbox.open(Hello {
            name: AgentName::try_from("agent-1").unwrap(),
            agent_version: AgentVersion::try_from("0.1.0").unwrap(),
            protocol_version: fleet_proto::PROTOCOL_VERSION,
            capacity: AgentCapacity::try_from(4).unwrap(),
            running,
        });
        let first = outbound.next().await.expect("the hello");
        assert!(matches!(
            AgentMessage::try_from(first),
            Ok(AgentMessage::Hello(_))
        ));
        self.outbox.welcomed(generation, secs(10));
        self.stream = Some((generation, outbound));
    }

    /// A new stream: the barrier, the hello with its snapshot, and the
    /// welcome. The hello's snapshot.
    pub(crate) async fn hello(&mut self) -> Vec<BotSnapshot> {
        self.end();
        let running = tokio::time::timeout(secs(5), self.inbox.barrier())
            .await
            .expect("the barrier answers")
            .expect("the snapshot");
        self.open(running.clone()).await;
        running
    }

    /// Queues `instruction` on the current stream and lets it run.
    pub(crate) async fn instruct(&mut self, instruction: Instruction) {
        let specs: Vec<&BotSpec> = match &instruction {
            Instruction::Reconcile(assignments) => assignments
                .iter()
                .map(|assignment| &assignment.spec)
                .collect(),
            Instruction::Assign(assignment) => vec![&assignment.spec],
            Instruction::Update(spec) => vec![spec],
            Instruction::Unassign(_) | Instruction::SendChat(_) | Instruction::Lifecycle(_) => {
                Vec::new()
            }
        };
        for spec in specs {
            if let BotAccount::Offline(name) = &spec.account {
                self.accounts.insert(spec.id, name.clone());
            }
        }
        self.inbox
            .instruction(self.generation(), instruction)
            .unwrap();
        self.pump().await;
    }

    /// Reads what the current stream sends until it would wait, answering
    /// session requests with offline grants.
    pub(crate) async fn pump(&mut self) {
        loop {
            settle().await;
            let Some((_, outbound)) = self.stream.as_mut() else {
                return;
            };
            let Ok(Some(message)) =
                tokio::time::timeout(Duration::from_millis(1), outbound.next()).await
            else {
                return;
            };
            match AgentMessage::try_from(message).unwrap() {
                AgentMessage::SessionRequest(request) => {
                    let username = self.accounts[&request.bot_id].clone();
                    self.credentials.answer(SessionAnswer::Grant(SessionGrant {
                        id: request.id,
                        bot_id: request.bot_id,
                        credentials: SessionCredentials::Offline { username },
                    }));
                }
                AgentMessage::Hello(_) | AgentMessage::Heartbeat => {}
                other => self.sent.push(other),
            }
        }
    }

    /// The `index`-th session the fake connector started, joined.
    pub(crate) async fn join(&mut self, index: usize) -> SessionController {
        self.pump().await;
        let session = tokio::time::timeout(secs(5), self.fake.session(index))
            .await
            .expect("the bot connects");
        assert_eq!(session.emit(SessionEvent::Joined), EmitOutcome::Queued);
        self.pump().await;
        session
    }

    /// The states bot `n`'s statuses sent, in order.
    pub(crate) fn states(&self, n: u8) -> Vec<BotState> {
        self.sent
            .iter()
            .filter_map(|message| match message {
                AgentMessage::Status(status) if status.bot_id == bot(n) => Some(status.state),
                _ => None,
            })
            .collect()
    }

    /// The events bot `n` sent, in order.
    pub(crate) fn events(&self, n: u8) -> Vec<BotEventKind> {
        self.sent
            .iter()
            .filter_map(|message| match message {
                AgentMessage::Event(event) if event.bot_id == bot(n) => Some(event.kind.clone()),
                _ => None,
            })
            .collect()
    }

    /// Whether bot `n` sent its `Removed`.
    pub(crate) fn removed(&self, n: u8) -> bool {
        self.events(n).contains(&BotEventKind::Removed)
    }

    /// The fleet's snapshot of every bot.
    pub(crate) async fn running(&self) -> Vec<BotId> {
        self.fleet
            .snapshot_all()
            .await
            .unwrap()
            .into_iter()
            .map(|bot| bot.bot_id)
            .collect()
    }

    /// Stops the conductor and the fleet.
    pub(crate) async fn stop(mut self) {
        self.stop.cancel();
        self.tasks.shutdown().await;
    }
}
