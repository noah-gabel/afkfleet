//! Tests for the server's ingest (Plan.md P10.8; ADR-0019, S70, U16, U17):
//! what the front publishes at once and what it queues, each stream's
//! share and the queue's bound, and the writer's batches: one transaction a
//! tick and none when idle, at most a batch, a busy database keeping the
//! batch, a failure dropping it, chat published after its commit with its
//! row's ID, the sticky states the marks hand it, and the final flush.
//!
//! Each test gets a fresh database from `#[sqlx::test]`. Time isn't paused:
//! sqlx's worker threads would make paused timeouts fire early (CLAUDE.md);
//! the writer's ticks are called directly, and its waits lowered.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::num::NonZeroUsize;
use core::time::Duration;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::authz::Role;
use fleet_core::bot::{
    BotNotification, BotSnapshot, BotState, FailReason, LastReport, LastState, PauseReason,
};
use fleet_core::chat::{ChatMessage, IncomingChat, PlayerChatKind};
use fleet_core::disconnect::ConflictKind;
use fleet_core::id::RequestId;
use fleet_core::id::{AccountId, AgentId, BotId, ChatRequestId, UserId};
use fleet_core::mode::ModePreset;
use fleet_core::pki::CertFingerprint;
use fleet_core::value::AgentCapacity;
use fleet_core::value::{AgentName, Username};
use fleet_proto::control::{AgentMessage, ChatFailedReason, ServerMessage, Welcome};
use fleet_proto::generated as wire;
use fleet_server::app::agents::ingest::{IngestBounds, IngestTimings, IngestWriter, Tick, ingest};
use fleet_server::app::events::{BusEvent, EventBus, LiveEvent, LiveEventKind};
use fleet_server::config::{LogConfig, LogFormat};
use fleet_server::grpc::registry::{
    Activation, AgentRegistry, Halt, Hold, HoldEntry, Lease, Verdict,
};
use fleet_server::infra::sqlite::{Database, DatabaseOptions};
use fleet_server::ports::accounts::{AccountCredential, McAccount};
use fleet_server::ports::agent_connections::Admitted;
use fleet_server::ports::agents::NewAgent;
use fleet_server::ports::audit::{AuditLimit, AuditPage, NewAuditEntry};
use fleet_server::ports::bots::NewBot;
use fleet_server::ports::chat::{ChatEntry, ChatMessageId, ChatRecord, ListedChat};
use fleet_server::ports::ingest::{
    Ingest, NotTaken, Report, Reported, StickyMarks, StickyWrite, StreamShare,
};
use fleet_server::ports::live_agents::LiveAgents as _;
use fleet_server::ports::page::{Page, PageLimit};
use fleet_server::ports::store::Store;
use fleet_server::ports::users::{NewUser, PasswordHash};
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::log_buffer::LogBuffer;
use fleet_testkit::system::ManualClock;
use secrecy::SecretString;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::Notify;
use tokio::sync::broadcast::error::TryRecvError;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt as _;

#[path = "ingest_service/observed.rs"]
mod observed;

use self::observed::{Log, ObservedStore, lines};

/// The time everything starts at, in milliseconds.
const T0: i64 = 1_800_000_000_000;
/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";
/// How long a batch waits for the write connection here.
const WAIT: Duration = Duration::from_millis(50);

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

fn setup_entry() -> NewAuditEntry {
    NewAuditEntry::new(
        at(T0),
        AuditAction::try_from("test.ingest_setup").unwrap(),
        AuditOutcome::Success,
    )
}

/// The marks a test hands the writer, and what it was told back.
#[derive(Debug, Default)]
struct ScriptedMarks {
    writes: Mutex<Vec<StickyWrite>>,
    remarked: Mutex<Vec<BotId>>,
}

impl ScriptedMarks {
    fn mark(&self, write: StickyWrite) {
        self.writes.lock().unwrap().push(write);
    }

    fn remarked(&self) -> Vec<BotId> {
        self.remarked.lock().unwrap().clone()
    }
}

impl StickyMarks for ScriptedMarks {
    fn any(&self) -> bool {
        !self.writes.lock().unwrap().is_empty()
    }

    fn take(&self) -> Vec<StickyWrite> {
        core::mem::take(&mut *self.writes.lock().unwrap())
    }

    fn remark(&self, bots: &[BotId]) {
        self.remarked.lock().unwrap().extend_from_slice(bots);
    }
}

/// A database with one user, agent 1 and bot 1 assigned to it.
struct World {
    db: Database,
    agent: AgentId,
    bot: BotId,
    owner: UserId,
}

impl World {
    async fn new(options: SqliteConnectOptions) -> Self {
        let db = Database::connect(options, DatabaseOptions::default())
            .await
            .unwrap();
        let owner = UserId::new_v7(at(T0), [1; 10]).unwrap();
        let mut tx = db.write().await.unwrap();
        tx.users()
            .insert(&NewUser {
                id: owner,
                username: Username::try_from("user1").unwrap(),
                password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
                role: Role::Member,
                created_at: at(T0),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        let agent = Self::agent(&db, 1).await;
        let world = Self {
            db,
            agent,
            bot: BotId::new_v7(at(T0), [1; 10]).unwrap(),
            owner,
        };
        world.add_bot(1, Some(agent)).await;
        world
    }

    async fn agent(db: &Database, n: u8) -> AgentId {
        let agent = AgentId::new_v7(at(T0), [n; 10]).unwrap();
        let mut tx = db.write().await.unwrap();
        tx.agents()
            .insert(&NewAgent {
                id: agent,
                name: AgentName::try_from(format!("agent-{n}").as_str()).unwrap(),
                created_at: at(T0),
            })
            .await
            .unwrap();
        tx.commit(setup_entry()).await.unwrap();
        agent
    }

    /// Offline account `n` and its bot `n`, assigned to `agent` if given.
    async fn add_bot(&self, n: u8, agent: Option<AgentId>) -> BotId {
        let account = McAccount {
            id: AccountId::new_v7(at(T0 + i64::from(n)), [n; 10]).unwrap(),
            owner: self.owner,
            name: format!("AfkBot{n}").parse().unwrap(),
            created_at: at(T0 + i64::from(n)),
            credential: AccountCredential::Offline,
        };
        let bot = BotId::new_v7(at(T0), [n; 10]).unwrap();
        let mut tx = self.db.write().await.unwrap();
        tx.accounts().insert(&account).await.unwrap();
        tx.bots()
            .insert(&NewBot {
                id: bot,
                account: account.id,
                mode: ModePreset::Afk.id(),
            })
            .await
            .unwrap();
        if let Some(agent) = agent {
            assert!(tx.bots().assign(bot, agent).await.unwrap());
        }
        tx.commit(setup_entry()).await.unwrap();
        bot
    }

    /// The front and the writer over this database, `marks` and `bus`.
    fn ingest(
        &self,
        marks: Arc<dyn StickyMarks>,
        bus: &EventBus,
        batch: usize,
        bounds: IngestBounds,
    ) -> (impl Ingest, IngestWriter) {
        ingest(
            Arc::new(self.db.clone()),
            marks,
            bus.clone(),
            IngestTimings {
                interval: Duration::from_millis(500),
                batch_rows: NonZeroUsize::new(batch).unwrap(),
                wait: WAIT,
            },
            bounds,
        )
    }

    fn report(&self, millis: i64, reported: Reported) -> Report {
        Report {
            agent: self.agent,
            generation: 1,
            bot: self.bot,
            received_at: at(millis),
            reported,
        }
    }

    /// Every chat message of `bot`, newest first.
    async fn chat(&self, bot: BotId) -> Vec<ChatRecord> {
        self.db
            .chat()
            .page(
                bot,
                Page::<ChatMessageId> {
                    before: None,
                    limit: PageLimit::new(100).unwrap(),
                },
            )
            .await
            .unwrap()
            .into_iter()
            .map(|line| line.record)
            .collect()
    }

    async fn audit_count(&self) -> usize {
        self.db
            .audit()
            .list(AuditPage {
                before: None,
                limit: AuditLimit::new(100).unwrap(),
            })
            .await
            .unwrap()
            .len()
    }
}

fn bounds(queue: usize, stream: usize) -> IngestBounds {
    IngestBounds {
        queue: NonZeroUsize::new(queue).unwrap(),
        stream: NonZeroUsize::new(stream).unwrap(),
    }
}

fn received(text: &str) -> Reported {
    Reported::ChatReceived(IncomingChat::player(
        PlayerChatKind::Chat,
        "Steve",
        None,
        text,
    ))
}

fn mode(text: &str) -> Reported {
    Reported::ModeChatSent(ChatMessage::try_from(text).unwrap())
}

fn texts(records: &[ChatRecord]) -> Vec<String> {
    records
        .iter()
        .rev()
        .map(|record| match &record.entry {
            ChatEntry::Received(chat) => chat.text().to_owned(),
            ChatEntry::ModeSent(message) | ChatEntry::UserSent { message, .. } => {
                message.as_str().to_owned()
            }
        })
        .collect()
}

fn paused() -> LastState {
    LastState::Paused(PauseReason::Conflict {
        kind: ConflictKind::DuplicateLogin,
    })
}

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

#[sqlx::test(migrations = false)]
async fn a_tick_writes_what_waits_in_one_transaction_in_receipt_order(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let world = World::new(options).await;
    let bus = EventBus::new();
    let (front, mut writer) = world.ingest(
        Arc::new(ScriptedMarks::default()),
        &bus,
        10,
        IngestBounds::default(),
    );
    let share = front.share();
    let audited = world.audit_count().await;

    for (n, reported) in [received("one"), mode("/afk"), received("three")]
        .into_iter()
        .enumerate()
    {
        front
            .take(
                &share,
                world.report(T0 + i64::try_from(n).unwrap(), reported),
            )
            .unwrap();
    }
    let tick = writer.tick().await;

    assert_eq!(
        tick,
        Tick::Written {
            stored: 3,
            skipped: 0,
            sticky: 0
        }
    );
    let stored = world.chat(world.bot).await;
    assert_eq!(texts(&stored), ["one", "/afk", "three"]);
    assert_eq!(
        stored.last().unwrap().at,
        at(T0),
        "the server's receipt time"
    );
    assert_eq!(world.audit_count().await, audited, "no audit entry");
}

#[sqlx::test(migrations = false)]
async fn an_idle_tick_writes_nothing_and_never_waits_for_the_write_connection(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let world = World::new(options).await;
    let bus = EventBus::new();
    let (_front, mut writer) = world.ingest(
        Arc::new(ScriptedMarks::default()),
        &bus,
        10,
        IngestBounds::default(),
    );
    // Holding the one write connection: a tick that tried to write would
    // wait for it.
    let held = world.db.write().await.unwrap();

    let started = tokio::time::Instant::now();
    let tick = writer.tick().await;
    let took = started.elapsed();
    drop(held);

    assert_eq!(tick, Tick::Idle);
    assert!(took < WAIT, "it waited {took:?}");
}

#[sqlx::test(migrations = false)]
async fn a_tick_writes_at_most_its_batch(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let world = World::new(options).await;
    let bus = EventBus::new();
    let (front, mut writer) = world.ingest(
        Arc::new(ScriptedMarks::default()),
        &bus,
        2,
        IngestBounds::default(),
    );
    let share = front.share();
    for n in 0..3 {
        front
            .take(&share, world.report(T0 + n, received(&format!("m{n}"))))
            .unwrap();
    }

    let first = writer.tick().await;
    let second = writer.tick().await;
    let third = writer.tick().await;

    assert_eq!(
        first,
        Tick::Written {
            stored: 2,
            skipped: 0,
            sticky: 0
        }
    );
    assert_eq!(
        second,
        Tick::Written {
            stored: 1,
            skipped: 0,
            sticky: 0
        }
    );
    assert_eq!(third, Tick::Idle);
    assert_eq!(texts(&world.chat(world.bot).await), ["m0", "m1", "m2"]);
}

#[sqlx::test(migrations = false)]
async fn a_busy_database_keeps_the_batch_and_writes_it_first_at_the_next_tick(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let world = World::new(options).await;
    let bus = EventBus::new();
    let (front, mut writer) = world.ingest(
        Arc::new(ScriptedMarks::default()),
        &bus,
        10,
        IngestBounds::default(),
    );
    let share = front.share();
    front
        .take(&share, world.report(T0, received("first")))
        .unwrap();

    let held = world.db.write().await.unwrap();
    let busy = writer.tick().await;
    drop(held);
    front
        .take(&share, world.report(T0 + 1, received("second")))
        .unwrap();
    let after = writer.tick().await;

    assert_eq!(busy, Tick::Kept { rows: 1 });
    assert_eq!(
        after,
        Tick::Written {
            stored: 2,
            skipped: 0,
            sticky: 0
        }
    );
    assert_eq!(texts(&world.chat(world.bot).await), ["first", "second"]);
}

#[sqlx::test(migrations = false)]
async fn a_failed_batch_is_dropped_logged_once_and_the_recovery_counts_the_rows_lost(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (logs, _guard) = capture();
    let world = World::new(options).await;
    let bus = EventBus::new();
    let (front, mut writer) = world.ingest(
        Arc::new(ScriptedMarks::default()),
        &bus,
        10,
        IngestBounds::default(),
    );
    let share = front.share();
    // A time no column can store fails its batch, which isn't a busy one.
    let far = DateTime::<Utc>::from_timestamp(253_402_300_800, 0).unwrap();
    let unstorable = |text: &str| Report {
        received_at: far,
        ..world.report(T0, received(text))
    };

    front.take(&share, unstorable("lost 1")).unwrap();
    front
        .take(&share, world.report(T0, received("lost 2")))
        .unwrap();
    let first = writer.tick().await;
    front.take(&share, unstorable("lost 3")).unwrap();
    let second = writer.tick().await;
    front
        .take(&share, world.report(T0, received("kept")))
        .unwrap();
    let recovered = writer.tick().await;

    assert_eq!(first, Tick::Dropped { rows: 2 });
    assert_eq!(second, Tick::Dropped { rows: 1 });
    assert_eq!(
        recovered,
        Tick::Written {
            stored: 1,
            skipped: 0,
            sticky: 0
        }
    );
    assert_eq!(texts(&world.chat(world.bot).await), ["kept"]);
    let failed = logs
        .lines_with("the ingest couldn't write a batch; its chat messages are lost")
        .unwrap();
    assert_eq!(failed.len(), 2, "{}", logs.text());
    assert_eq!(failed[0]["level"], "ERROR");
    assert_eq!(failed[1]["level"], "DEBUG");
    let recovery = logs
        .lines_with("the ingest writes again; chat messages were lost meanwhile")
        .unwrap();
    assert_eq!(recovery.len(), 1);
    assert_eq!(recovery[0]["level"], "WARN");
    assert_eq!(recovery[0]["lost"], 3);
    assert!(!logs.text().contains("lost 1"), "no chat text in the logs");
}

#[sqlx::test(migrations = false)]
async fn chat_is_published_only_after_its_commit_with_its_row_id(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let world = World::new(options).await;
    let bus = EventBus::new();
    let mut events = bus.subscribe();
    let (front, mut writer) = world.ingest(
        Arc::new(ScriptedMarks::default()),
        &bus,
        10,
        IngestBounds::default(),
    );
    let share = front.share();

    front
        .take(&share, world.report(T0, received("hi")))
        .unwrap();
    let before = events.try_recv();
    writer.tick().await;
    let after = events.try_recv().unwrap();

    assert_eq!(before.map(drop), Err(TryRecvError::Empty));
    let id = world.chat(world.bot).await.first().unwrap().id;
    assert_eq!(
        *after,
        BusEvent::Agent(LiveEvent {
            bot: world.bot,
            agent: world.agent,
            generation: 1,
            at: at(T0),
            kind: LiveEventKind::Chat {
                line: ListedChat {
                    record: ChatRecord {
                        id,
                        bot: world.bot,
                        at: at(T0),
                        entry: ChatEntry::Received(IncomingChat::player(
                            PlayerChatKind::Chat,
                            "Steve",
                            None,
                            "hi"
                        )),
                    },
                    sent_by: None,
                },
                request: None,
            },
        })
    );
}

#[sqlx::test(migrations = false)]
async fn statuses_deaths_alerts_and_outcomes_are_published_at_once_and_never_written(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let world = World::new(options).await;
    let bus = EventBus::new();
    let mut events = bus.subscribe();
    let (front, mut writer) = world.ingest(
        Arc::new(ScriptedMarks::default()),
        &bus,
        10,
        IngestBounds::default(),
    );
    let share = front.share();
    let request = ChatRequestId::new_v7(at(T0), [3; 10]).unwrap();
    let snapshot = BotSnapshot {
        bot_id: world.bot,
        state: BotState::Stopped,
        since: at(T0),
        last_disconnect: None,
    };
    let alert = BotNotification::Failed {
        reason: FailReason::Auth,
    };
    let reported = [
        Reported::Status(snapshot.clone()),
        Reported::Died,
        Reported::Alert(alert),
        Reported::ChatSent(request),
        Reported::ChatFailed(request, ChatFailedReason::RateLimited),
    ];

    for reported in reported {
        front.take(&share, world.report(T0, reported)).unwrap();
    }
    let published: Vec<LiveEventKind> = (0..5)
        .map(|_| events.try_recv().unwrap().agent().unwrap().kind.clone())
        .collect();
    let tick = writer.tick().await;

    assert_eq!(
        published,
        [
            LiveEventKind::Status(snapshot),
            LiveEventKind::Died,
            LiveEventKind::Alert(alert),
            LiveEventKind::ChatSent(request),
            LiveEventKind::ChatFailed(request, ChatFailedReason::RateLimited),
        ]
    );
    assert_eq!(tick, Tick::Idle, "nothing of theirs is written");
}

#[sqlx::test(migrations = false)]
async fn a_streams_share_bounds_its_rows_in_flight_until_they_are_written(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let world = World::new(options).await;
    let bus = EventBus::new();
    let (front, mut writer) =
        world.ingest(Arc::new(ScriptedMarks::default()), &bus, 10, bounds(10, 2));
    let one = front.share();
    let other = front.share();

    let first = [
        front.take(&one, world.report(T0, received("a"))),
        front.take(&one, world.report(T0, received("b"))),
        front.take(&one, world.report(T0, received("c"))),
        front.take(&other, world.report(T0, received("d"))),
    ];
    writer.tick().await;
    let after = front.take(&one, world.report(T0, received("e")));

    assert_eq!(first, [Ok(()), Ok(()), Err(NotTaken::StreamFull), Ok(())]);
    assert_eq!(after, Ok(()), "the written rows freed their places");
}

#[sqlx::test(migrations = false)]
async fn a_full_queue_refuses_the_row_and_a_closed_one_says_so(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let world = World::new(options).await;
    let bus = EventBus::new();
    let (front, writer) = world.ingest(Arc::new(ScriptedMarks::default()), &bus, 10, bounds(2, 10));
    let share = front.share();

    let queued = [
        front.take(&share, world.report(T0, received("a"))),
        front.take(&share, world.report(T0, received("b"))),
        front.take(&share, world.report(T0, received("c"))),
    ];
    drop(writer);
    let closed = front.take(&share, world.report(T0, received("d")));
    let status = front.take(&share, world.report(T0, Reported::Died));

    assert_eq!(queued, [Ok(()), Ok(()), Err(NotTaken::QueueFull)]);
    assert_eq!(closed, Err(NotTaken::Closed));
    assert_eq!(status, Ok(()), "what isn't chat is never refused");
}

#[sqlx::test(migrations = false)]
async fn a_message_for_a_bot_no_longer_the_agents_is_skipped_and_counted(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let world = World::new(options).await;
    let unassigned = world.add_bot(2, None).await;
    let bus = EventBus::new();
    let mut events = bus.subscribe();
    let (front, mut writer) = world.ingest(
        Arc::new(ScriptedMarks::default()),
        &bus,
        10,
        IngestBounds::default(),
    );
    let share = front.share();

    front
        .take(
            &share,
            Report {
                bot: unassigned,
                ..world.report(T0, received("elsewhere"))
            },
        )
        .unwrap();
    front
        .take(&share, world.report(T0, received("here")))
        .unwrap();
    let tick = writer.tick().await;

    assert_eq!(
        tick,
        Tick::Written {
            stored: 1,
            skipped: 1,
            sticky: 0
        }
    );
    assert_eq!(world.chat(unassigned).await, Vec::new());
    let published = events.try_recv().unwrap();
    assert_eq!(
        published.agent().unwrap().bot,
        world.bot,
        "a skipped row is never published"
    );
    assert_eq!(events.try_recv().map(drop), Err(TryRecvError::Empty));
}

#[sqlx::test(migrations = false)]
async fn a_sticky_crossing_is_written_in_the_batch_only_while_its_row_names_the_agent(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let world = World::new(options).await;
    let elsewhere = world
        .add_bot(2, Some(World::agent(&world.db, 2).await))
        .await;
    let marks = Arc::new(ScriptedMarks::default());
    let bus = EventBus::new();
    let (_front, mut writer) = world.ingest(marks.clone(), &bus, 10, IngestBounds::default());
    marks.mark(StickyWrite {
        bot: world.bot,
        agent: world.agent,
        last: LastReport {
            state: paused(),
            at: at(T0 + 5),
        },
    });
    marks.mark(StickyWrite {
        bot: elsewhere,
        agent: world.agent,
        last: LastReport {
            state: paused(),
            at: at(T0 + 5),
        },
    });

    let tick = writer.tick().await;

    assert_eq!(
        tick,
        Tick::Written {
            stored: 0,
            skipped: 0,
            sticky: 1
        }
    );
    let row = world.db.bots().find(world.bot).await.unwrap().unwrap();
    assert_eq!(
        row.last,
        Some(LastReport {
            state: paused(),
            at: at(T0 + 5)
        })
    );
    let other = world.db.bots().find(elsewhere).await.unwrap().unwrap();
    assert_eq!(other.last, None, "another agent's bot");
}

#[sqlx::test(migrations = false)]
async fn a_sticky_write_never_goes_backwards(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (logs, _guard) = capture();
    let world = World::new(options).await;
    let marks = Arc::new(ScriptedMarks::default());
    let bus = EventBus::new();
    let (_front, mut writer) = world.ingest(marks.clone(), &bus, 10, IngestBounds::default());
    let mut tx = world.db.write().await.unwrap();
    assert!(
        tx.bots()
            .record_state(
                world.bot,
                world.agent,
                &LastReport {
                    state: LastState::Online,
                    at: at(T0 + 10),
                },
            )
            .await
            .unwrap()
    );
    tx.commit(setup_entry()).await.unwrap();
    marks.mark(StickyWrite {
        bot: world.bot,
        agent: world.agent,
        last: LastReport {
            state: paused(),
            at: at(T0 + 5),
        },
    });

    let tick = writer.tick().await;

    // Nothing was written, so nothing committed and nothing says it did.
    assert_eq!(tick, Tick::Idle);
    let row = world.db.bots().find(world.bot).await.unwrap().unwrap();
    assert_eq!(row.last.unwrap().state, LastState::Online);
    assert_eq!(
        logs.lines_with("the ingest wrote a batch").unwrap().len(),
        0,
        "{}",
        logs.text()
    );
}

#[sqlx::test(migrations = false)]
async fn after_failed_batches_a_tick_that_wrote_nothing_waits_for_a_real_write_to_say_so(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (logs, _guard) = capture();
    let world = World::new(options).await;
    let marks = Arc::new(ScriptedMarks::default());
    let bus = EventBus::new();
    let (front, mut writer) = world.ingest(marks.clone(), &bus, 10, IngestBounds::default());
    let share = front.share();
    // A time no column can store fails its batch, which isn't a busy one.
    let far = DateTime::<Utc>::from_timestamp(253_402_300_800, 0).unwrap();
    front
        .take(
            &share,
            Report {
                received_at: far,
                ..world.report(T0, received("lost"))
            },
        )
        .unwrap();
    let failed = writer.tick().await;
    // A sticky crossing older than the row's state: record_state refuses it.
    let mut tx = world.db.write().await.unwrap();
    assert!(
        tx.bots()
            .record_state(
                world.bot,
                world.agent,
                &LastReport {
                    state: LastState::Online,
                    at: at(T0 + 10),
                },
            )
            .await
            .unwrap()
    );
    tx.commit(setup_entry()).await.unwrap();
    marks.mark(StickyWrite {
        bot: world.bot,
        agent: world.agent,
        last: LastReport {
            state: paused(),
            at: at(T0 + 5),
        },
    });
    let refused = writer.tick().await;
    let quiet = logs
        .lines_with("the ingest writes again; chat messages were lost meanwhile")
        .unwrap()
        .len();
    front
        .take(&share, world.report(T0, received("kept")))
        .unwrap();
    let written = writer.tick().await;

    assert_eq!(failed, Tick::Dropped { rows: 1 });
    assert_eq!(refused, Tick::Idle);
    assert_eq!(quiet, 0, "nothing was written yet: {}", logs.text());
    assert_eq!(
        written,
        Tick::Written {
            stored: 1,
            skipped: 0,
            sticky: 0
        }
    );
    let recovery = logs
        .lines_with("the ingest writes again; chat messages were lost meanwhile")
        .unwrap();
    assert_eq!(recovery.len(), 1, "{}", logs.text());
    assert_eq!(recovery[0]["lost"], 1);
}

#[sqlx::test(migrations = false)]
async fn the_marks_of_a_batch_that_failed_are_set_again(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let world = World::new(options).await;
    let marks = Arc::new(ScriptedMarks::default());
    let bus = EventBus::new();
    let (front, mut writer) = world.ingest(marks.clone(), &bus, 10, IngestBounds::default());
    let share = front.share();
    marks.mark(StickyWrite {
        bot: world.bot,
        agent: world.agent,
        last: LastReport {
            state: paused(),
            at: at(T0 + 5),
        },
    });
    let far = DateTime::<Utc>::from_timestamp(253_402_300_800, 0).unwrap();
    front
        .take(
            &share,
            Report {
                received_at: far,
                ..world.report(T0, received("fails the batch"))
            },
        )
        .unwrap();

    let tick = writer.tick().await;

    assert_eq!(tick, Tick::Dropped { rows: 1 });
    assert_eq!(marks.remarked(), [world.bot]);
    let row = world.db.bots().find(world.bot).await.unwrap().unwrap();
    assert_eq!(row.last, None, "rolled back");
}

#[sqlx::test(migrations = false)]
async fn the_final_flush_writes_every_queued_row(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let world = World::new(options).await;
    let bus = EventBus::new();
    let (front, writer) = world.ingest(
        Arc::new(ScriptedMarks::default()),
        &bus,
        2,
        IngestBounds::default(),
    );
    let share = front.share();
    for n in 0..5 {
        front
            .take(&share, world.report(T0 + n, received(&format!("m{n}"))))
            .unwrap();
    }
    let stop = CancellationToken::new();
    stop.cancel();

    tokio::time::timeout(Duration::from_secs(5), writer.run(stop))
        .await
        .expect("the flush ends");

    assert_eq!(
        texts(&world.chat(world.bot).await),
        ["m0", "m1", "m2", "m3", "m4"]
    );
}

#[sqlx::test(migrations = false)]
async fn a_final_flush_that_finds_the_database_busy_drops_the_batch_with_one_warn(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (logs, _guard) = capture();
    let world = World::new(options).await;
    let bus = EventBus::new();
    let (front, writer) = world.ingest(
        Arc::new(ScriptedMarks::default()),
        &bus,
        10,
        IngestBounds::default(),
    );
    let share = front.share();
    front
        .take(&share, world.report(T0, received("too late")))
        .unwrap();
    let held = world.db.write().await.unwrap();
    let stop = CancellationToken::new();
    stop.cancel();

    tokio::time::timeout(Duration::from_secs(5), writer.run(stop))
        .await
        .expect("the flush ends");
    drop(held);

    assert_eq!(world.chat(world.bot).await, Vec::new());
    let warned = logs
        .lines_with("the ingest's last batch found the database busy; its chat messages are lost")
        .unwrap();
    assert_eq!(warned.len(), 1, "{}", logs.text());
    assert_eq!(warned[0]["level"], "WARN");
    assert_eq!(warned[0]["rows"], 1);
}

#[sqlx::test(migrations = false)]
async fn a_share_is_released_when_its_row_is_dropped(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let world = World::new(options).await;
    let bus = EventBus::new();
    let (front, mut writer) =
        world.ingest(Arc::new(ScriptedMarks::default()), &bus, 10, bounds(10, 1));
    let share: StreamShare = front.share();
    let far = DateTime::<Utc>::from_timestamp(253_402_300_800, 0).unwrap();
    front
        .take(
            &share,
            Report {
                received_at: far,
                ..world.report(T0, received("dropped"))
            },
        )
        .unwrap();

    assert_eq!(writer.tick().await, Tick::Dropped { rows: 1 });
    let after = front.take(&share, world.report(T0, received("next")));

    assert_eq!(after, Ok(()));
}

/// [`ScriptedMarks`] whose takes are logged with the store's writes.
#[derive(Debug)]
struct LoggedMarks {
    inner: ScriptedMarks,
    log: Log,
}

impl StickyMarks for LoggedMarks {
    fn any(&self) -> bool {
        self.inner.any()
    }

    fn take(&self) -> Vec<StickyWrite> {
        self.log.lock().unwrap().push("marks taken");
        self.inner.take()
    }

    fn remark(&self, bots: &[BotId]) {
        self.inner.remark(bots);
    }
}

/// Marks whose fence refuses them all: something is marked, nothing taken.
#[derive(Debug)]
struct FencedMarks;

impl StickyMarks for FencedMarks {
    fn any(&self) -> bool {
        true
    }

    fn take(&self) -> Vec<StickyWrite> {
        Vec::new()
    }

    fn remark(&self, _bots: &[BotId]) {}
}

impl World {
    /// The database, its writes logged in `log`.
    fn observed(&self, log: &Log) -> (Arc<ObservedStore>, Arc<Notify>) {
        let requested = Arc::new(Notify::new());
        let store = Arc::new(ObservedStore {
            inner: self.db.clone(),
            log: Arc::clone(log),
            requested: Arc::clone(&requested),
        });
        (store, requested)
    }

    /// The front and the writer over `store`, waiting up to `wait` for the
    /// write connection, holding at most `bounds`.
    fn ingest_over(
        store: Arc<dyn Store>,
        marks: Arc<dyn StickyMarks>,
        bus: &EventBus,
        wait: Duration,
        bounds: IngestBounds,
    ) -> (impl Ingest, IngestWriter) {
        ingest(
            store,
            marks,
            bus.clone(),
            IngestTimings {
                interval: Duration::from_millis(500),
                batch_rows: NonZeroUsize::new(10).unwrap(),
                wait,
            },
            bounds,
        )
    }
}

#[sqlx::test(migrations = false)]
async fn the_marks_are_taken_only_once_the_write_transaction_is_open(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let world = World::new(options).await;
    let log = Log::default();
    let (store, _requested) = world.observed(&log);
    let marks = Arc::new(LoggedMarks {
        inner: ScriptedMarks::default(),
        log: Arc::clone(&log),
    });
    marks.inner.mark(StickyWrite {
        bot: world.bot,
        agent: world.agent,
        last: LastReport {
            state: paused(),
            at: at(T0 + 5),
        },
    });
    let bus = EventBus::new();
    let (_front, mut writer) =
        World::ingest_over(store, marks, &bus, WAIT, IngestBounds::default());

    let tick = writer.tick().await;

    assert_eq!(
        tick,
        Tick::Written {
            stored: 0,
            skipped: 0,
            sticky: 1
        }
    );
    assert_eq!(
        lines(&log),
        [
            "write requested",
            "write opened",
            "marks taken",
            "committed"
        ]
    );
}

#[sqlx::test(migrations = false)]
async fn a_tick_whose_marks_the_fence_refused_commits_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let world = World::new(options).await;
    let log = Log::default();
    let (store, _requested) = world.observed(&log);
    let bus = EventBus::new();
    let (_front, mut writer) = World::ingest_over(
        store,
        Arc::new(FencedMarks),
        &bus,
        WAIT,
        IngestBounds::default(),
    );

    let tick = writer.tick().await;

    assert_eq!(tick, Tick::Idle);
    assert_eq!(
        lines(&log),
        ["write requested", "write opened"],
        "no commit"
    );
}

#[sqlx::test(migrations = false)]
async fn a_batch_whose_rows_were_all_skipped_commits_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let world = World::new(options).await;
    let elsewhere = world
        .add_bot(2, Some(World::agent(&world.db, 2).await))
        .await;
    let log = Log::default();
    let (store, _requested) = world.observed(&log);
    let bus = EventBus::new();
    let (front, mut writer) = World::ingest_over(
        store,
        Arc::new(ScriptedMarks::default()),
        &bus,
        WAIT,
        IngestBounds::default(),
    );
    let share = front.share();
    let mut report = world.report(T0, received("not this agent's bot"));
    report.bot = elsewhere;
    front.take(&share, report).unwrap();

    let tick = writer.tick().await;

    assert_eq!(
        tick,
        Tick::Written {
            stored: 0,
            skipped: 1,
            sticky: 0
        }
    );
    assert_eq!(
        lines(&log),
        ["write requested", "write opened"],
        "no commit"
    );
}

#[sqlx::test(migrations = false)]
async fn a_final_flush_without_chat_that_finds_the_database_busy_never_says_chat_was_lost(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let (logs, _guard) = capture();
    let world = World::new(options).await;
    let marks = Arc::new(ScriptedMarks::default());
    marks.mark(StickyWrite {
        bot: world.bot,
        agent: world.agent,
        last: LastReport {
            state: paused(),
            at: at(T0 + 5),
        },
    });
    let bus = EventBus::new();
    let (_front, writer) = world.ingest(marks, &bus, 10, IngestBounds::default());
    let held = world.db.write().await.unwrap();
    let stop = CancellationToken::new();
    stop.cancel();

    tokio::time::timeout(Duration::from_secs(5), writer.run(stop))
        .await
        .expect("the flush ends");
    drop(held);

    assert_eq!(
        logs.lines_with(
            "the ingest's last batch found the database busy; its chat messages are lost"
        )
        .unwrap()
        .len(),
        0,
        "{}",
        logs.text()
    );
    let warned = logs
        .lines_with(
            "the ingest's last batch found the database busy; no chat message waited, and the bots' states stay as their streams' ends recorded them",
        )
        .unwrap();
    assert_eq!(warned.len(), 1, "{}", logs.text());
    assert_eq!(warned[0]["level"], "WARN");
}

/// The registry of a live stream of the world's agent, reconciled, holding
/// its bot assigned, whose hello listed it online and which reported it
/// paused since: its crossing is marked (U16, R54). The caller keeps the
/// lease and the queue, so the stream lives.
fn held_registry(
    world: &World,
) -> (
    Arc<AgentRegistry>,
    Lease,
    mpsc::Receiver<wire::ServerMessage>,
) {
    let registry = Arc::new(AgentRegistry::new(Arc::new(ManualClock::new(at(T0)))));
    let lease = registry
        .claim(&Admitted {
            request: RequestId::new_v7(at(T0), [1; 10]).unwrap(),
            agent: world.agent,
            name: AgentName::try_from("agent-1").unwrap(),
            fingerprint: CertFingerprint::from_digest([1; 32]),
            not_after: at(T0 + 86_400_000),
            peer: None,
        })
        .unwrap();
    let (queue, queued) = mpsc::channel(16);
    let snapshot = |state: BotState, millis: i64| BotSnapshot {
        bot_id: world.bot,
        state,
        since: at(millis),
        last_disconnect: None,
    };
    let welcome = wire::ServerMessage::try_from(ServerMessage::Welcome(Welcome {
        heartbeat_interval: Duration::from_secs(10),
        agent_id: world.agent,
    }))
    .unwrap();
    let _ended = registry
        .activate(
            &lease,
            Activation {
                queue,
                halt: Halt::default(),
                connected_at: at(T0),
                capacity: AgentCapacity::try_from(4_u32).unwrap(),
                running: vec![snapshot(
                    BotState::Online {
                        since: at(T0),
                        attempt: core::num::NonZeroU32::MIN,
                    },
                    T0,
                )],
                welcome,
            },
        )
        .unwrap();
    registry
        .settle(
            world.agent,
            lease.generation(),
            vec![(
                world.bot,
                HoldEntry {
                    hold: Hold::Assigned,
                    since: at(T0),
                    sent: None,
                    stopping: None,
                },
            )],
            ServerMessage::Reconcile(Vec::new()),
        )
        .unwrap();
    let (verdict, _conflict) = registry.admit(
        &lease,
        &AgentMessage::Status(snapshot(
            BotState::Paused {
                reason: PauseReason::Conflict {
                    kind: ConflictKind::DuplicateLogin,
                },
            },
            T0 + 5,
        )),
    );
    assert!(matches!(verdict, Verdict::Held { .. }), "{verdict:?}");
    assert!(registry.any(), "the crossing is marked");
    (registry, lease, queued)
}

#[sqlx::test(migrations = false)]
async fn a_resume_or_reset_clear_that_lands_while_a_tick_waits_is_never_undone(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let world = World::new(options).await;
    let (registry, _lease, _queued) = held_registry(&world);
    let log = Log::default();
    let (store, requested) = world.observed(&log);
    let bus = EventBus::new();
    let (_front, mut writer) = World::ingest_over(
        store,
        registry.clone(),
        &bus,
        Duration::from_secs(5),
        IngestBounds::default(),
    );
    // The clear holds the write connection while the tick asks for it.
    let mut clear = world.db.write().await.unwrap();
    let tick = tokio::spawn(async move { writer.tick().await });
    requested.notified().await;

    // Meanwhile the bot was paused, unplaced and resumed: the clear takes
    // the kept report and clears the row; then the bot is placed on its
    // agent again (S48).
    let report = registry.take_latest(world.bot).expect("the paused report");
    let last = report.last();
    assert!(
        clear
            .bots()
            .record_state(world.bot, world.agent, &last)
            .await
            .unwrap()
    );
    assert!(clear.bots().detach(world.bot, world.agent).await.unwrap());
    assert!(clear.bots().clear_sticky(world.bot, &last).await.unwrap());
    assert!(clear.bots().assign(world.bot, world.agent).await.unwrap());
    clear.commit(setup_entry()).await.unwrap();
    let tick = tick.await.unwrap();

    let row = world.db.bots().find(world.bot).await.unwrap().unwrap();
    assert_eq!(row.last, None, "the clear stands");
    assert_eq!(tick, Tick::Idle);
}

/// Marks that always hold bot 1's crossing: every tick writes it again, as
/// the row's time allows one at the same time (S74's flush count).
#[derive(Debug)]
struct EverMarked(StickyWrite);

impl StickyMarks for EverMarked {
    fn any(&self) -> bool {
        true
    }

    fn take(&self) -> Vec<StickyWrite> {
        vec![self.0]
    }

    fn remark(&self, _bots: &[BotId]) {}
}

#[sqlx::test(migrations = false)]
async fn a_batch_the_database_kept_holds_its_places_in_the_queue(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let world = World::new(options).await;
    let bus = EventBus::new();
    let (front, mut writer) =
        world.ingest(Arc::new(ScriptedMarks::default()), &bus, 2, bounds(4, 10));
    let share = front.share();
    for text in ["a", "b"] {
        front
            .take(&share, world.report(T0, received(text)))
            .unwrap();
    }
    let held = world.db.write().await.unwrap();
    let kept = writer.tick().await;
    drop(held);

    let meanwhile =
        ["c", "d", "e"].map(|text| front.take(&share, world.report(T0, received(text))));
    let written = writer.tick().await;
    let after = front.take(&share, world.report(T0, received("f")));

    assert_eq!(kept, Tick::Kept { rows: 2 });
    assert_eq!(meanwhile, [Ok(()), Ok(()), Err(NotTaken::QueueFull)]);
    assert_eq!(
        written,
        Tick::Written {
            stored: 2,
            skipped: 0,
            sticky: 0
        }
    );
    assert_eq!(after, Ok(()), "the written rows freed their places");
}

#[sqlx::test(migrations = false)]
async fn a_batch_being_written_holds_its_places_in_the_queue(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let world = World::new(options).await;
    let log = Log::default();
    let (store, requested) = world.observed(&log);
    let bus = EventBus::new();
    let (front, mut writer) = World::ingest_over(
        store,
        Arc::new(ScriptedMarks::default()),
        &bus,
        Duration::from_secs(5),
        bounds(2, 10),
    );
    let share = front.share();
    for text in ["a", "b"] {
        front
            .take(&share, world.report(T0, received(text)))
            .unwrap();
    }
    // The tick has taken both rows off the queue and waits for the write
    // connection.
    let held = world.db.write().await.unwrap();
    // The task hands the writer back: dropping it would close the queue.
    let tick = tokio::spawn(async move { (writer.tick().await, writer) });
    requested.notified().await;

    let meanwhile = front.take(&share, world.report(T0, received("c")));
    drop(held);
    let (written, _writer) = tick.await.unwrap();
    let after = front.take(&share, world.report(T0, received("d")));

    assert_eq!(meanwhile, Err(NotTaken::QueueFull));
    assert_eq!(
        written,
        Tick::Written {
            stored: 2,
            skipped: 0,
            sticky: 0
        }
    );
    assert_eq!(after, Ok(()));
}

#[sqlx::test(migrations = false)]
async fn the_final_flush_commits_at_most_its_queues_batches_and_one_for_the_marks(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let world = World::new(options).await;
    let log = Log::default();
    let (store, _requested) = world.observed(&log);
    let marks = Arc::new(EverMarked(StickyWrite {
        bot: world.bot,
        agent: world.agent,
        last: LastReport {
            state: paused(),
            at: at(T0 + 5),
        },
    }));
    let bus = EventBus::new();
    let (front, mut writer) = ingest(
        store,
        marks,
        bus.clone(),
        IngestTimings {
            interval: Duration::from_millis(500),
            batch_rows: NonZeroUsize::new(2).unwrap(),
            wait: WAIT,
        },
        bounds(4, 10),
    );
    let share = front.share();
    for text in ["a", "b"] {
        front
            .take(&share, world.report(T0, received(text)))
            .unwrap();
    }
    // A batch the database kept, then the queue filled behind it.
    let held = world.db.write().await.unwrap();
    assert_eq!(writer.tick().await, Tick::Kept { rows: 2 });
    drop(held);
    for text in ["c", "d", "e", "f"] {
        let _ = front.take(&share, world.report(T0, received(text)));
    }
    let before = lines(&log).len();
    let stop = CancellationToken::new();
    stop.cancel();

    tokio::time::timeout(Duration::from_secs(5), writer.run(stop))
        .await
        .expect("the flush ends");

    // Four rows held in all: two batches of chat, and one for the marks.
    let commits = lines(&log)[before..]
        .iter()
        .filter(|line| **line == "committed")
        .count();
    assert_eq!(commits, 3, "{:?}", lines(&log));
}
