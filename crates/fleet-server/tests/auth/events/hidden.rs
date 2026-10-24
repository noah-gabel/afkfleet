//! Hidden things leave no trace (ADR-0020, S32, S40): an event of any kind
//! on a bot a viewer can't see sends them nothing, not even a removal when
//! the bot goes; fewer such events than the bus holds send nothing at all;
//! and a flood past it shows only as one `resync`, which counts nothing.
//!
//! "Nothing" is read up to a quiet socket: two pings in a row, a ping
//! interval with no frame between, which a `resync` owed would have filled
//! (one is due once the frames waiting went).

use core::time::Duration;

use fleet_core::bot::{BotNotification, BotSnapshot, BotState, FailReason, PauseReason};
use fleet_core::chat::{ChatKind, IncomingChat};
use fleet_core::disconnect::ConflictKind;
use fleet_core::id::ChatRequestId;
use fleet_core::system::Clock as _;
use fleet_proto::control::ChatFailedReason;
use fleet_server::app::events::{EVENT_BUS_CAPACITY, LiveEventKind, NotSent};
use fleet_server::http::ws::SocketTimings;
use fleet_server::ports::chat::{ChatEntry, ChatMessageId, ChatRecord, ListedChat};
use fleet_server::ports::live_hints::LiveHints as _;
use fleet_server::ports::store::Store as _;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::super::support::{Options, Setup, setup_entry, setup_with};
use super::bots::{Watched, agent_event, watched};
use super::{PATIENCE, Served, next_event_after_pings, ready, until_quiet};

/// A ping every 100 ms, so a quiet socket shows within a test's time.
async fn pinging(options: SqliteConnectOptions) -> Setup {
    setup_with(
        options,
        Options {
            sockets: SocketTimings {
                ping_interval: Duration::from_millis(100),
                ..SocketTimings::default()
            },
            ..Options::default()
        },
    )
    .await
}

/// A chat request the hidden events name.
fn request() -> ChatRequestId {
    ChatRequestId::new_v7(
        chrono::DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
        [3; 10],
    )
    .unwrap()
}

/// Every kind of event about `watched`'s bot: each one its agent's stream
/// or a send publishes, then the server's own hint.
fn every_kind(setup: &Setup, watched: &Watched) {
    let now = setup.clock.now();
    let line = ListedChat {
        record: ChatRecord {
            id: ChatMessageId::new(1),
            bot: watched.bot,
            at: now,
            entry: ChatEntry::Received(
                IncomingChat::from_parts(ChatKind::System, None, "a hidden line", false).unwrap(),
            ),
        },
        sent_by: None,
    };
    let kinds = [
        LiveEventKind::Status(BotSnapshot {
            bot_id: watched.bot,
            state: BotState::Stopped,
            since: now,
            last_disconnect: None,
        }),
        LiveEventKind::Died,
        LiveEventKind::Alert(BotNotification::Paused {
            reason: PauseReason::Conflict {
                kind: ConflictKind::DuplicateLogin,
            },
        }),
        LiveEventKind::Alert(BotNotification::Failed {
            reason: FailReason::Auth,
        }),
        LiveEventKind::Chat {
            line: line.clone(),
            request: None,
        },
        LiveEventKind::Chat {
            line,
            request: Some(request()),
        },
        LiveEventKind::ChatSent(request()),
        LiveEventKind::ChatFailed(request(), ChatFailedReason::Disconnected),
        LiveEventKind::ChatNotSent {
            request: request(),
            why: NotSent::Unreachable,
        },
        LiveEventKind::ChatNotSent {
            request: request(),
            why: NotSent::Unsendable,
        },
    ];
    for kind in kinds {
        agent_event(setup, watched.bot, watched.agent, kind);
    }
    setup.events.bots(&[watched.bot]);
}

/// A frame of `watched`'s stranger's own bot.
fn is_own(watched: &Watched, frame: &Value) -> bool {
    frame["type"] == json!("bot") && frame["bot_id"] == json!(watched.own.to_string())
}

/// A `resync`, which counts nothing.
fn is_resync(frame: &Value) -> bool {
    *frame == json!({ "type": "resync" })
}

#[sqlx::test(migrations = false)]
async fn an_event_of_any_kind_on_a_hidden_bot_sends_no_frame(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = pinging(options).await;
    let watched = watched(&setup).await;
    let served = Served::start(&setup).await;
    let mut stranger = ready(&setup, &served, &watched.stranger.token).await;
    let mut member = ready(&setup, &served, &watched.member.token).await;

    every_kind(&setup, &watched);

    assert_eq!(until_quiet(&mut stranger).await, Vec::<Value>::new());
    // The bot's owner saw them: the kinds reach those who may see the bot.
    assert_eq!(
        next_event_after_pings(&mut member).await["bot_id"],
        json!(watched.bot.to_string())
    );
}

#[sqlx::test(migrations = false)]
async fn a_hidden_bot_that_goes_sends_no_bot_removed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = pinging(options).await;
    let watched = watched(&setup).await;
    let served = Served::start(&setup).await;
    let mut stranger = ready(&setup, &served, &watched.stranger.token).await;
    let mut member = ready(&setup, &served, &watched.member.token).await;
    let reads = setup.hooked.bot_reads();

    // Deleted, and hinted as the deletion's service does.
    let mut tx = setup.db.write().await.unwrap();
    tx.accounts()
        .delete(watched.account, setup.clock.now())
        .await
        .unwrap();
    tx.commit(setup_entry()).await.unwrap();
    setup.events.bots(&[watched.bot]);
    agent_event(&setup, watched.bot, watched.agent, LiveEventKind::Died);

    assert_eq!(until_quiet(&mut stranger).await, Vec::<Value>::new());
    assert_eq!(
        next_event_after_pings(&mut member).await,
        json!({ "type": "bot_removed", "bot_id": watched.bot.to_string() }),
        "its owner saw it go"
    );
    assert_eq!(
        setup.hooked.bot_reads(),
        reads,
        "a hint whose bots are all hidden reads nothing"
    );
}

/// Holds `watched`'s stranger's socket inside its own bot's read, while
/// `flood` hidden deaths and then another frame of its own bot are
/// published; then lets it go: every frame until it's quiet.
async fn held_through(setup: &Setup, watched: &Watched, flood: usize) -> Vec<Value> {
    let served = Served::start(setup).await;
    let mut stranger = ready(setup, &served, &watched.stranger.token).await;
    let mut held = setup.hooked.hold_next_bot_read();
    setup.events.bots(&[watched.own]);
    held.read(PATIENCE).await;
    for _ in 0..flood {
        agent_event(setup, watched.bot, watched.agent, LiveEventKind::Died);
    }
    setup.events.bots(&[watched.own]);
    held.release();
    until_quiet(&mut stranger).await
}

#[sqlx::test(migrations = false)]
async fn fewer_hidden_events_than_the_bus_holds_send_no_frame(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = pinging(options).await;
    let watched = watched(&setup).await;

    // With the second frame's hint, the bus holds all but one place.
    let frames = held_through(&setup, &watched, EVENT_BUS_CAPACITY - 2).await;

    assert_eq!(frames.len(), 2, "{frames:?}");
    assert!(
        frames.iter().all(|frame| is_own(&watched, frame)),
        "no resync and nothing else: {frames:?}"
    );
}

#[sqlx::test(migrations = false)]
async fn a_hidden_flood_past_the_bus_shows_only_as_one_resync_without_a_count(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = pinging(options).await;
    let watched = watched(&setup).await;

    let frames = held_through(&setup, &watched, EVENT_BUS_CAPACITY + 100).await;

    assert_eq!(frames.len(), 3, "{frames:?}");
    assert_eq!(frames.iter().filter(|frame| is_resync(frame)).count(), 1);
    assert!(
        frames
            .iter()
            .all(|frame| is_resync(frame) || is_own(&watched, frame)),
        "only the resync and the bot's own frames: {frames:?}"
    );
}

/// Two of the Member's bots hinted, with 100 deaths of the stranger's bot
/// between them, all waiting while a marker's read is held: the hidden
/// deaths count toward nothing, so both bots are still read together, in
/// one read (PR #67's review follow-up c, R48).
#[sqlx::test(migrations = false)]
async fn hidden_events_never_split_a_merged_bot_read(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = pinging(options).await;
    let watched = watched(&setup).await;
    let (_, second) = setup.bot_of(watched.member.id, 11).await;
    let (_, marker) = setup.bot_of(watched.member.id, 30).await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &watched.member.token).await;
    let reads = setup.hooked.bot_reads();

    let mut held = setup.hooked.hold_next_bot_read();
    setup.events.bots(&[marker]);
    held.read(PATIENCE).await;
    setup.events.bots(&[watched.bot]);
    for _ in 0..100 {
        agent_event(&setup, watched.own, watched.agent, LiveEventKind::Died);
    }
    setup.events.bots(&[second]);
    held.release();
    let frames = until_quiet(&mut socket).await;

    let named: Vec<&Value> = frames.iter().map(|frame| &frame["bot_id"]).collect();
    assert_eq!(named.len(), 3, "{frames:?}");
    assert_eq!(*named[0], json!(marker.to_string()));
    assert!(
        named.contains(&&json!(watched.bot.to_string())),
        "{frames:?}"
    );
    assert!(named.contains(&&json!(second.to_string())), "{frames:?}");
    assert_eq!(
        setup.hooked.bot_reads() - reads,
        2,
        "the marker's, then both bots in one"
    );
}
