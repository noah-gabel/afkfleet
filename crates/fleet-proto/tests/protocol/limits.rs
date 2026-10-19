//! The shared size limits (Plan.md P10.4; ADR-0019, S25): the largest
//! message each side legitimately sends fits the limit the other side reads
//! with, with every value at its cap. The hello is larger than tonic's
//! default 4 MiB, which is why the limits are set at all.

use core::num::{NonZeroU32, NonZeroU64};

use chrono::{DateTime, Utc};
use fleet_core::bot::{
    BotAccount, BotSnapshot, BotSpec, BotState, DesiredRunState, FailReason, StickyState,
};
use fleet_core::chat::{ChatMessage, ChatSender, IncomingChat, PlayerChatKind};
use fleet_core::disconnect::{ConflictTexts, DisconnectReason, PermanentKind};
use fleet_core::id::{AccountId, BotId};
use fleet_core::mc::SessionCredentials;
use fleet_core::mode::{Action, ModeDefinition, ModeDraft, Schedule, Step};
use fleet_core::value::{AgentCapacity, AgentName, AgentVersion, ServerAddress};
use fleet_proto::PROTOCOL_VERSION;
use fleet_proto::control::{
    AgentMessage, Assignment, BotEvent, BotEventKind, Hello, MAX_AGENT_MESSAGE_BYTES, MAX_BOTS,
    MAX_SERVER_MESSAGE_BYTES, ServerMessage, SessionGrant, SessionRequestId, server_message_len,
};
use fleet_proto::generated as wire;
use prost::Message as _;
use secrecy::SecretString;

/// A character of four bytes in UTF-8 that no sanitizer strips.
const WIDE: char = '\u{1D538}';
/// A character of three bytes in UTF-8, one UTF-16 unit.
const EURO: char = '\u{20AC}';

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

fn wide(len: usize) -> String {
    core::iter::repeat_n(WIDE, len).collect()
}

/// A bot's status with every field at its largest.
fn largest_snapshot(n: usize) -> BotSnapshot {
    BotSnapshot {
        bot_id: bot(n),
        state: BotState::Online {
            since: earliest(),
            attempt: NonZeroU32::MAX,
        },
        since: earliest(),
        last_disconnect: Some(DisconnectReason::kicked(
            Some(&"k".repeat(128)),
            &wide(1024),
        )),
    }
}

fn largest_hello() -> AgentMessage {
    AgentMessage::Hello(Hello {
        name: AgentName::try_from("a".repeat(AgentName::MAX_LEN).as_str()).unwrap(),
        agent_version: AgentVersion::try_from("1".repeat(AgentVersion::MAX_LEN).as_str()).unwrap(),
        protocol_version: PROTOCOL_VERSION,
        capacity: AgentCapacity::try_from(1000_u32).unwrap(),
        running: (0..MAX_BOTS).map(largest_snapshot).collect(),
    })
}

fn chat(len: usize) -> ChatMessage {
    let text: String = core::iter::repeat_n(EURO, len).collect();
    ChatMessage::try_from(text.as_str()).unwrap()
}

/// A mode at its caps: both chat steps, and every other step a look.
fn largest_mode() -> ModeDefinition {
    let every = Schedule::Every {
        interval: ModeDefinition::MAX_INTERVAL,
        jitter: ModeDefinition::MAX_JITTER,
    };
    let mut steps = vec![
        Step {
            action: Action::SendChat {
                message: chat(ChatMessage::MAX_LEN),
            },
            schedule: Schedule::AtStart,
            probability: 100,
        },
        Step {
            action: Action::SendChat {
                message: chat(ChatMessage::MAX_LEN),
            },
            schedule: every,
            probability: 100,
        },
    ];
    while steps.len() < ModeDefinition::MAX_STEPS {
        steps.push(Step {
            action: Action::Look {
                yaw: -ModeDefinition::MAX_YAW,
                pitch: -ModeDefinition::MAX_PITCH,
            },
            schedule: every,
            probability: 100,
        });
    }
    ModeDraft { steps }.validate().unwrap()
}

/// The longest server address: a 253-character host and a five-digit port.
fn longest_address() -> ServerAddress {
    let host = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(61)
    );
    format!("{host}:65535").parse().unwrap()
}

fn largest_conflict_texts() -> ConflictTexts {
    let entries: Vec<String> = (0..ConflictTexts::MAX_ENTRIES)
        .map(|n| format!("{n:02}{}", wide(ConflictTexts::MAX_LEN - 2)))
        .collect();
    ConflictTexts::try_new(&entries).unwrap()
}

fn largest_reconcile() -> ServerMessage {
    let mode = largest_mode();
    let server = longest_address();
    let conflict_texts = largest_conflict_texts();
    let account = AccountId::new_v7(
        DateTime::from_timestamp_millis(1_790_000_000_000).unwrap(),
        [0xCD; 10],
    )
    .unwrap();
    ServerMessage::Reconcile(
        (0..MAX_BOTS)
            .map(|n| Assignment {
                spec: BotSpec {
                    id: bot(n),
                    account: BotAccount::Online(account),
                    server: server.clone(),
                    mode: mode.clone(),
                    desired: DesiredRunState::Running,
                    conflict_texts: conflict_texts.clone(),
                    chat_rate: fleet_core::chat::ChatRate::default(),
                },
                restore: Some(StickyState::Failed(FailReason::Permanent {
                    kind: PermanentKind::Banned,
                })),
            })
            .collect(),
    )
}

fn agent_bytes(message: AgentMessage) -> usize {
    wire::AgentMessage::try_from(message).unwrap().encoded_len()
}

fn server_bytes(message: ServerMessage) -> usize {
    wire::ServerMessage::try_from(message)
        .unwrap()
        .encoded_len()
}

#[test]
fn the_largest_hello_fits_the_agent_message_limit_and_tonics_default_doesnt() {
    let size = agent_bytes(largest_hello());

    assert!(size > 4 * 1024 * 1024, "{size} bytes");
    assert!(size <= MAX_AGENT_MESSAGE_BYTES, "{size} bytes");
}

#[test]
fn the_largest_reconcile_fits_the_server_message_limit() {
    let size = server_bytes(largest_reconcile());

    assert!(size > 64 * 1024 * 1024, "{size} bytes");
    assert!(size <= MAX_SERVER_MESSAGE_BYTES, "{size} bytes");
}

#[test]
fn the_largest_event_and_grant_fit_their_limits() {
    let chat = AgentMessage::Event(BotEvent {
        bot_id: bot(1),
        at: earliest(),
        kind: BotEventKind::ChatReceived(IncomingChat::player(
            PlayerChatKind::Whisper,
            &wide(ChatSender::MAX_NAME_LEN),
            Some(uuid::Uuid::from_bytes([0x11; 16])),
            &wide(IncomingChat::MAX_TEXT_LEN),
        )),
    });
    let grant = ServerMessage::SessionGrant(SessionGrant {
        id: SessionRequestId::new(NonZeroU64::MAX),
        bot_id: bot(1),
        credentials: SessionCredentials::Online {
            username: "AfkBot1".parse().unwrap(),
            uuid: "0123456789ab4def8123456789abcdef".parse().unwrap(),
            access_token: SecretString::from("t".repeat(16 * 1024)),
            expires_at: earliest(),
        },
    });

    assert!(agent_bytes(chat) <= MAX_AGENT_MESSAGE_BYTES);
    assert!(server_bytes(grant) <= MAX_SERVER_MESSAGE_BYTES);
}

#[test]
fn a_server_messages_length_is_what_it_encodes_to() {
    let messages = [
        wire::ServerMessage::try_from(largest_reconcile()).unwrap(),
        wire::ServerMessage::try_from(ServerMessage::Unassign(bot(1))).unwrap(),
        wire::ServerMessage::default(),
    ];

    for message in &messages {
        assert_eq!(server_message_len(message), message.encode_to_vec().len());
    }
}

#[test]
fn the_limits_hold_a_power_of_two_above_their_largest_message() {
    assert_eq!(MAX_AGENT_MESSAGE_BYTES, 8 * 1024 * 1024);
    assert_eq!(MAX_SERVER_MESSAGE_BYTES, 128 * 1024 * 1024);
}

#[test]
fn an_agent_notices_a_dead_server_within_its_longest_reconnect_wait() {
    let noticed =
        fleet_proto::control::KEEPALIVE_INTERVAL + fleet_proto::control::KEEPALIVE_TIMEOUT;

    assert!(
        noticed <= fleet_proto::control::RECONNECT_DELAY_MAX,
        "{noticed:?}"
    );
    assert!(fleet_proto::control::WELCOME_TIMEOUT > fleet_proto::control::HELLO_TIMEOUT);
}
