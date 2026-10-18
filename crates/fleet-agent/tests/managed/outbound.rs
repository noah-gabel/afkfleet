//! The outbox's request stream (Plan.md P10.7; ADR-0019, S66, R50, R51) on
//! paused time: the heartbeat after an interval of nothing, the wake on a
//! push, the end of an old stream, the lane, and `Debug`.

use core::num::{NonZeroU64, NonZeroUsize};
use core::time::Duration;

use chrono::DateTime;
use fleet_agent::managed::credentials::{SessionLink as _, Unsent};
use fleet_agent::managed::outbound::{Outbound, Outbox};
use fleet_core::bot::{BotSnapshot, BotState};
use fleet_core::chat::{IncomingChat, PlayerChatKind};
use fleet_core::value::{AgentCapacity, AgentName, AgentVersion};
use fleet_proto::control::{AgentMessage, Hello, SessionRequest, SessionRequestId};
use tokio_stream::StreamExt as _;

use crate::rig::bot;

const INTERVAL: Duration = Duration::from_secs(10);

fn outbox() -> Outbox {
    Outbox::new(NonZeroUsize::new(4).unwrap(), NonZeroUsize::new(4).unwrap())
}

fn hello() -> Hello {
    Hello {
        name: AgentName::try_from("agent-1").unwrap(),
        agent_version: AgentVersion::try_from("0.1.0").unwrap(),
        protocol_version: fleet_proto::PROTOCOL_VERSION,
        capacity: AgentCapacity::try_from(4).unwrap(),
        running: Vec::new(),
    }
}

fn status(n: u8, secs: i64) -> BotSnapshot {
    BotSnapshot {
        bot_id: bot(n),
        state: BotState::Stopped,
        since: DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap(),
        last_disconnect: None,
    }
}

/// The next message within `within`, as the server would read it.
async fn next_within(outbound: &mut Outbound, within: Duration) -> Option<AgentMessage> {
    tokio::time::timeout(within, outbound.next())
        .await
        .ok()
        .flatten()
        .map(|message| AgentMessage::try_from(message).unwrap())
}

#[tokio::test(start_paused = true)]
async fn the_hello_goes_and_nothing_follows_until_welcome_not_even_a_heartbeat() {
    let outbox = outbox();
    let (_generation, mut outbound) = outbox.open(hello());

    let first = next_within(&mut outbound, Duration::from_secs(1)).await;
    let after = next_within(&mut outbound, Duration::from_secs(60)).await;

    assert!(matches!(first, Some(AgentMessage::Hello(_))), "{first:?}");
    assert_eq!(after, None);
}

#[tokio::test(start_paused = true)]
async fn a_heartbeat_goes_after_one_interval_of_nothing_and_anything_sent_restarts_it() {
    let outbox = outbox();
    let (generation, mut outbound) = outbox.open(hello());
    let _ = next_within(&mut outbound, Duration::from_secs(1)).await;
    outbox.welcomed(generation, INTERVAL);
    let started = tokio::time::Instant::now();

    let first = next_within(&mut outbound, Duration::from_secs(60)).await;
    let first_at = started.elapsed();
    tokio::time::sleep(Duration::from_secs(4)).await;
    outbox.status(status(1, 1));
    let sent = next_within(&mut outbound, Duration::from_secs(1)).await;
    let sent_at = tokio::time::Instant::now();
    let second = next_within(&mut outbound, Duration::from_secs(60)).await;

    assert_eq!(first, Some(AgentMessage::Heartbeat));
    assert_eq!(first_at, INTERVAL);
    assert_eq!(sent, Some(AgentMessage::Status(status(1, 1))));
    assert_eq!(second, Some(AgentMessage::Heartbeat));
    assert_eq!(
        sent_at.elapsed(),
        INTERVAL,
        "a whole interval after the status"
    );
}

#[tokio::test(start_paused = true)]
async fn a_push_wakes_the_waiting_stream() {
    let outbox = outbox();
    let (generation, mut outbound) = outbox.open(hello());
    let _ = next_within(&mut outbound, Duration::from_secs(1)).await;
    outbox.welcomed(generation, INTERVAL);
    let waiting = tokio::spawn(async move { next_within(&mut outbound, INTERVAL / 2).await });
    tokio::task::yield_now().await;

    outbox.status(status(2, 1));

    assert_eq!(
        waiting.await.unwrap(),
        Some(AgentMessage::Status(status(2, 1)))
    );
}

#[tokio::test(start_paused = true)]
async fn an_old_streams_body_ends_once_the_next_one_opens() {
    let outbox = outbox();
    let (first, mut old) = outbox.open(hello());
    let _ = next_within(&mut old, Duration::from_secs(1)).await;
    outbox.ended(first);
    let (_second, _new) = outbox.open(hello());

    let ended = tokio::time::timeout(Duration::from_secs(1), old.next()).await;

    assert!(matches!(ended, Ok(None)), "{ended:?}");
}

#[tokio::test(start_paused = true)]
async fn an_overflow_ends_the_request_stream() {
    let outbox = outbox();
    let (generation, mut outbound) = outbox.open(hello());
    let _ = next_within(&mut outbound, Duration::from_secs(1)).await;
    outbox.welcomed(generation, INTERVAL);
    // Twice max_bots bots fit the per-bot class; one more doesn't.
    for n in 1..=9 {
        outbox.status(status(n, 1));
    }

    let ended = tokio::time::timeout(Duration::from_secs(1), outbound.next()).await;

    assert!(outbox.overflowed());
    assert!(matches!(ended, Ok(None)), "{ended:?}");
}

#[tokio::test(start_paused = true)]
async fn the_lane_answers_no_stream_until_welcome_and_its_generation_then() {
    let outbox = outbox();
    let request = SessionRequest {
        id: SessionRequestId::new(NonZeroU64::MIN),
        bot_id: bot(1),
        fresh: false,
    };
    let (generation, _outbound) = outbox.open(hello());

    let before = outbox.send(request);
    outbox.welcomed(generation, INTERVAL);
    let welcomed = outbox.send(request);
    outbox.ended(generation);
    let after = outbox.send(request);

    assert_eq!(before, Err(Unsent::NoStream));
    assert_eq!(welcomed, Ok(generation));
    assert_eq!(after, Err(Unsent::NoStream));
    assert!(!outbox.is_live(generation));
}

#[tokio::test(start_paused = true)]
async fn the_outboxs_debug_shows_counts_and_never_chat() {
    let outbox = outbox();
    outbox.received(
        bot(1),
        DateTime::from_timestamp(1_790_000_000, 0).unwrap(),
        IncomingChat::player(PlayerChatKind::Whisper, "SecretSender", None, "secret text"),
    );

    let debug = format!("{outbox:?}");

    assert!(debug.contains("chat: 1"), "{debug}");
    assert!(
        !debug.contains("secret") && !debug.contains("Secret"),
        "{debug}"
    );
}
