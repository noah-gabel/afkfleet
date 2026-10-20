//! The server's ingest over the port (Plan.md P10.8; ADR-0019, S28, S70,
//! U16): what an agent's stream reports about the bots it holds reaches the
//! chat log, the bots' sticky states and the bus; nothing about a bot it
//! doesn't hold does, a stream's chat past its share is dropped while the
//! stream goes on, and a batch touches no agent's columns.

use chrono::DateTime;
use fleet_core::bot::{BotState, LastState, PauseReason};
use fleet_core::chat::{ChatMessage, IncomingChat, PlayerChatKind};
use fleet_core::disconnect::ConflictKind;
use fleet_core::id::BotId;
use fleet_proto::control::{AgentMessage, BotEvent, BotEventKind, ServerMessage};
use fleet_proto::generated as wire;
use fleet_server::app::agents::ingest::{IngestBounds, Tick};
use fleet_server::app::bots::{BotAction, Controlled};
use fleet_server::app::events::LiveEventKind;
use fleet_server::grpc::registry::RefusedReports;
use fleet_server::ports::chat::{ChatEntry, ChatMessageId};
use fleet_server::ports::page::{Page, PageLimit};
use fleet_server::ports::store::Store as _;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::broadcast::error::TryRecvError;

use super::support::{CAPACITY, Call, Port, Settings, barrier, report, snapshot, start};

fn event(bot: BotId, kind: BotEventKind) -> wire::AgentMessage {
    wire::AgentMessage::try_from(AgentMessage::Event(BotEvent {
        bot_id: bot,
        at: DateTime::from_timestamp_millis(1_790_000_000_000).unwrap(),
        kind,
    }))
    .unwrap()
}

fn whisper(text: &str) -> IncomingChat {
    IncomingChat::player(PlayerChatKind::Whisper, "Steve", None, text)
}

fn paused() -> BotState {
    BotState::Paused {
        reason: PauseReason::Conflict {
            kind: ConflictKind::DuplicateLogin,
        },
    }
}

/// Sends the barrier and waits for its answer: every message sent before it
/// was handled.
async fn handled(call: &mut Call) {
    call.send(barrier()).await;
    assert!(
        matches!(call.next().await, Ok(Some(ServerMessage::SessionDenied(_)))),
        "the barrier's answer"
    );
}

/// Every chat message of `bot`, newest first.
async fn chat(port: &Port, bot: BotId) -> Vec<(ChatMessageId, ChatEntry)> {
    port.db
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
        .map(|line| (line.record.id, line.record.entry))
        .collect()
}

#[sqlx::test(migrations = false)]
async fn an_assigned_bots_chat_is_stored_and_published_with_its_row_id(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::reconciling()).await;
    let agent = port.enroll(1, "agent-1").await;
    let bot = port.bot(1).await;
    let (mut call, _) = port.welcomed(&agent).await;
    assert_eq!(call.reconciled().await.len(), 1);
    let mut events = port.events.subscribe();

    call.send(event(bot, BotEventKind::ChatReceived(whisper("hello"))))
        .await;
    call.send(event(
        bot,
        BotEventKind::ModeChatSent(ChatMessage::try_from("/afk").unwrap()),
    ))
    .await;
    handled(&mut call).await;
    let before = events.try_recv().map(drop);
    let tick = port.ingest_tick().await;

    assert_eq!(before, Err(TryRecvError::Empty), "not before the commit");
    assert_eq!(
        tick,
        Tick::Written {
            stored: 2,
            skipped: 0,
            sticky: 0
        }
    );
    let stored = chat(&port, bot).await;
    assert_eq!(
        stored
            .iter()
            .rev()
            .map(|(_, entry)| entry.clone())
            .collect::<Vec<_>>(),
        [
            ChatEntry::Received(whisper("hello")),
            ChatEntry::ModeSent(ChatMessage::try_from("/afk").unwrap()),
        ]
    );
    for (id, entry) in stored.iter().rev() {
        let published = events.try_recv().unwrap();
        assert_eq!(published.bot, bot);
        assert_eq!(published.agent, agent.id);
        assert_eq!(
            published.kind,
            LiveEventKind::Chat {
                id: *id,
                entry: entry.clone()
            }
        );
    }
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_status_of_an_assigned_bot_is_published_at_once_and_never_written_as_chat(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::reconciling()).await;
    let agent = port.enroll(1, "agent-1").await;
    let bot = port.bot(1).await;
    let (mut call, _) = port.welcomed(&agent).await;
    assert_eq!(call.reconciled().await.len(), 1);
    let mut events = port.events.subscribe();

    call.send(report(bot, BotState::Stopped)).await;
    handled(&mut call).await;

    let published = events.try_recv().unwrap();
    assert_eq!(
        published.kind,
        LiveEventKind::Status(snapshot(bot, BotState::Stopped))
    );
    assert_eq!(port.ingest_tick().await, Tick::Idle);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_hellos_strays_reports_reach_neither_the_table_nor_the_bus(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::ungated()).await;
    let agent = port.enroll(1, "agent-1").await;
    let bot = port.bot(1).await;
    // The agent runs the bot without the server's word: a restored backup.
    let mut call = port
        .welcomed_with(&agent, CAPACITY, vec![snapshot(bot, BotState::Stopped)])
        .await;
    assert_eq!(call.reconciled().await, []);
    let mut events = port.events.subscribe();

    call.send(report(bot, paused())).await;
    call.send(event(bot, BotEventKind::ChatReceived(whisper("stray"))))
        .await;
    handled(&mut call).await;

    assert_eq!(events.try_recv().map(drop), Err(TryRecvError::Empty));
    assert_eq!(port.ingest_tick().await, Tick::Idle);
    assert_eq!(chat(&port, bot).await, []);
    assert_eq!(port.bot_row(bot).await.unwrap().last, None);
    let refused = port.registry.take_refused_reports();
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].count, 2, "both dropped as reports");
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn chat_past_the_streams_share_is_dropped_counted_and_the_stream_goes_on(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(
        options,
        Settings {
            ingest: IngestBounds {
                queue: 10.try_into().unwrap(),
                stream: 1.try_into().unwrap(),
            },
            ..Settings::reconciling()
        },
    )
    .await;
    let agent = port.enroll(1, "agent-1").await;
    let bot = port.bot(1).await;
    let (mut call, _) = port.welcomed(&agent).await;
    assert_eq!(call.reconciled().await.len(), 1);

    call.send(event(bot, BotEventKind::ChatReceived(whisper("first"))))
        .await;
    call.send(event(bot, BotEventKind::ChatReceived(whisper("second"))))
        .await;
    handled(&mut call).await;
    let tick = port.ingest_tick().await;

    assert_eq!(
        tick,
        Tick::Written {
            stored: 1,
            skipped: 0,
            sticky: 0
        }
    );
    assert_eq!(
        port.registry.take_refused_reports(),
        [RefusedReports {
            agent: agent.id,
            name: "agent-1".try_into().unwrap(),
            count: 0,
            sessions_denied: 1,
            chat_dropped: 1,
        }],
        "the barrier's denial, and the second message"
    );
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_batch_writes_no_agent_column_and_a_bots_last_state_only_when_it_crosses(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::reconciling()).await;
    let agent = port.enroll(1, "agent-1").await;
    let bot = port.bot(1).await;
    let (mut call, _) = port.welcomed(&agent).await;
    assert_eq!(call.reconciled().await.len(), 1);
    let agent_before = port.row(&agent).await;
    let state_before = port.bot_row(bot).await.unwrap().last;

    call.send(report(bot, BotState::Stopped)).await;
    handled(&mut call).await;
    let crossed_nothing = port.ingest_tick().await;
    let unchanged = port.bot_row(bot).await.unwrap().last;
    call.send(report(bot, paused())).await;
    handled(&mut call).await;
    let crossed = port.ingest_tick().await;
    // The stream is still open: no end of its has recorded anything (S40).
    let row = port.bot_row(bot).await.unwrap();

    assert_eq!(crossed_nothing, Tick::Idle);
    assert_eq!(unchanged, state_before);
    assert_eq!(
        crossed,
        Tick::Written {
            stored: 0,
            skipped: 0,
            sticky: 1
        }
    );
    assert_eq!(
        row.last.map(|last| last.state),
        Some(LastState::from(paused())),
        "the Paused is stored while the stream lives (U16)"
    );
    assert_eq!(port.row(&agent).await, agent_before, "no agent column");
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_releasing_bots_chat_and_status_are_dropped_and_counted(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::reconciling()).await;
    let agent = port.enroll(1, "agent-1").await;
    let bot = port.bot(1).await;
    port.assign(bot, &agent).await;
    let by = port.acting(1).await;
    let mut call = port
        .welcomed_with(&agent, CAPACITY, vec![snapshot(bot, paused())])
        .await;
    assert_eq!(call.reconciled().await.len(), 1);
    // Stopping a paused bot unassigns it at once: it's held releasing until
    // its Removed.
    let stopped = port.bots().control(&by, bot, BotAction::Stop).await;
    assert_eq!(stopped.unwrap(), Controlled::Changed);
    assert!(
        matches!(call.next().await, Ok(Some(ServerMessage::Unassign(id))) if id == bot),
        "the unassignment"
    );
    assert_eq!(port.registry.take_refused_reports(), []);
    let mut events = port.events.subscribe();
    let state_before = port.bot_row(bot).await.unwrap().last;

    call.send(event(bot, BotEventKind::ChatReceived(whisper("late"))))
        .await;
    call.send(report(bot, BotState::Stopped)).await;
    handled(&mut call).await;

    assert_eq!(events.try_recv().map(drop), Err(TryRecvError::Empty));
    assert_eq!(port.ingest_tick().await, Tick::Idle);
    assert_eq!(chat(&port, bot).await, []);
    assert_eq!(port.bot_row(bot).await.unwrap().last, state_before);
    let refused = port.registry.take_refused_reports();
    assert_eq!(refused.len(), 1);
    assert_eq!((refused[0].count, refused[0].chat_dropped), (2, 0));
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_paused_bot_the_batch_wrote_is_restored_after_a_server_that_died_without_its_streams_end(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::reconciling()).await;
    let agent = port.enroll(1, "agent-1").await;
    let bot = port.bot(1).await;
    let (mut call, _) = port.welcomed(&agent).await;
    assert_eq!(call.reconciled().await.len(), 1);
    call.send(report(bot, paused())).await;
    handled(&mut call).await;
    let tick = port.ingest_tick().await;
    // The server dies: its stream's end records nothing (U16's crash case,
    // which S40's end transaction alone would lose).
    port.lose_ends(u32::MAX);

    let port = port.restart().await;
    // The agent lost its memory too: it says hello with no bots.
    let (mut call, _) = port.welcomed(&agent).await;
    let reconciled = call.reconciled().await;

    assert_eq!(
        tick,
        Tick::Written {
            stored: 0,
            skipped: 0,
            sticky: 1
        }
    );
    assert_eq!(reconciled.len(), 1);
    assert_eq!(reconciled[0].spec.id, bot);
    assert_eq!(
        reconciled[0].restore,
        LastState::from(paused()).sticky(),
        "{reconciled:?}"
    );
    port.stop().await;
}
