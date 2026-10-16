//! The message limits (S25): the largest legitimate hello is welcomed, a
//! message over [`MAX_AGENT_MESSAGE_BYTES`] ends the stream, and a hello
//! with more than 1000 bots is refused before it's decoded.

use core::num::NonZeroU32;

use chrono::{DateTime, Utc};
use fleet_core::bot::{BotSnapshot, BotState};
use fleet_core::disconnect::DisconnectReason;
use fleet_core::id::BotId;
use fleet_core::value::{AgentCapacity, AgentName, AgentVersion};
use fleet_proto::PROTOCOL_VERSION;
use fleet_proto::control::{AgentMessage, Hello, MAX_AGENT_MESSAGE_BYTES, MAX_BOTS};
use fleet_proto::generated as wire;
use fleet_server::grpc::control::{FIRST_MESSAGE_UNREADABLE, ONE_HELLO, TOO_LARGE};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tonic::Code;

use super::support::{Settings, hello, request_id, sized, start, text};

/// A character of four bytes in UTF-8 that no sanitizer strips.
const WIDE: char = '\u{1D538}';

/// The earliest time the wire carries: its varint takes the most bytes.
fn earliest() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("0000-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn bot(n: usize) -> BotId {
    let mut random = [0xAB; 10];
    random[..8].copy_from_slice(&u64::try_from(n).unwrap().to_be_bytes());
    BotId::new_v7(
        DateTime::from_timestamp_millis(1_790_000_000_000).unwrap(),
        random,
    )
    .unwrap()
}

/// A bot's status with every field at its largest.
fn largest_snapshot(n: usize) -> BotSnapshot {
    let wide: String = core::iter::repeat_n(WIDE, 1024).collect();
    BotSnapshot {
        bot_id: bot(n),
        state: BotState::Online {
            since: earliest(),
            attempt: NonZeroU32::MAX,
        },
        since: earliest(),
        last_disconnect: Some(DisconnectReason::kicked(Some(&"k".repeat(128)), &wide)),
    }
}

/// fleet-proto's largest hello (its `limits` tests), for `name`.
fn largest_hello(name: &str) -> wire::AgentMessage {
    wire::AgentMessage::try_from(AgentMessage::Hello(Hello {
        name: AgentName::try_from(name).unwrap(),
        agent_version: AgentVersion::try_from("1".repeat(AgentVersion::MAX_LEN).as_str()).unwrap(),
        protocol_version: PROTOCOL_VERSION,
        capacity: AgentCapacity::try_from(1000_u32).unwrap(),
        running: (0..MAX_BOTS).map(largest_snapshot).collect(),
    }))
    .unwrap()
}

#[sqlx::test(migrations = false)]
async fn the_largest_legitimate_hello_is_welcomed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let name = "a".repeat(AgentName::MAX_LEN);
    let agent = port.enroll(1, &name).await;
    let mut call = port.open(&agent).await.unwrap();

    call.send(largest_hello(&name)).await;

    let welcome = call.welcome().await;
    assert_eq!(welcome.agent_id, agent.id);
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_message_at_the_limit_is_read_and_one_byte_over_ends_the_stream(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    let (mut at_limit, _) = port.welcomed(&agent).await;

    // Read, and dropped because it doesn't convert; the session answers
    // the hello after it.
    at_limit.send(sized(MAX_AGENT_MESSAGE_BYTES)).await;
    at_limit.send(hello("agent-1")).await;
    let status = at_limit.ended().await;
    assert_eq!(status.message(), ONE_HELLO, "{status:?}");

    let (mut over, _) = port.welcomed(&agent).await;
    over.send(sized(MAX_AGENT_MESSAGE_BYTES + 1)).await;
    let status = over.ended().await;
    assert_eq!(status.code(), Code::OutOfRange, "{status:?}");
    assert_eq!(status.message(), TOO_LARGE);
    assert_eq!(request_id(status.metadata()), over.request_id());

    // Each end is recorded after its answer.
    let ends = port
        .entries_once(&fleet_core::audit::AuditAction::AGENT_DISCONNECT, 2)
        .await;
    assert_eq!(ends.len(), 2, "{ends:?}");
    for end in &ends {
        assert_eq!(text(end, "reason").as_deref(), Some("protocol_error"));
    }
    port.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_hello_with_more_than_1000_bots_is_refused_unread(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    let mut call = port.open(&agent).await.unwrap();
    // Empty entries: 2 bytes each on the wire, so the message is small.
    let crowded = wire::AgentMessage {
        msg: Some(wire::agent_message::Msg::Hello(wire::Hello {
            name: "agent-1".to_owned(),
            agent_version: "0.1.0".to_owned(),
            protocol_version: PROTOCOL_VERSION,
            capacity: 10,
            running: vec![wire::BotSnapshot::default(); MAX_BOTS + 1],
        })),
    };

    call.send(crowded).await;

    let status = call.ended().await;
    assert_eq!(status.code(), Code::InvalidArgument, "{status:?}");
    assert_eq!(status.message(), FIRST_MESSAGE_UNREADABLE);
    assert_eq!(port.audit().await, Vec::new());
    port.stop().await;
}
