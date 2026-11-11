//! Each target's seeds: a few inputs that already reach what the target
//! is about (a valid value, a whole message, a sealed envelope's shape), so
//! a run starts there, not at the empty input.
//!
//! They're built here, from the real types, so they can't go stale in a
//! folder of binary files: this crate's tests run every target over them,
//! and its `seeds` example writes them where cargo-fuzz reads a corpus.

use core::time::Duration;

use fleet_core::id::{AgentId, BotId};
use fleet_core::value::{AgentCapacity, AgentName, AgentVersion};
use fleet_proto::PROTOCOL_VERSION;
use fleet_proto::control::{AgentMessage, Hello, ServerMessage, Welcome};
use fleet_proto::generated as wire;
use fleet_server::ports::vault::header_of;
use prost::Message as _;
use uuid::Uuid;

/// An ID's UUID: version 7, as every ID is.
const ID: Uuid = Uuid::from_u128(0x0189_0000_0000_7000_8000_0000_0000_0001);

/// The seeds of the target `name`; none for a name that isn't a target's.
#[must_use]
pub fn seeds(name: &str) -> Vec<Vec<u8>> {
    match name {
        "value_objects" => texts(&[
            "alice",
            "AfkBot1",
            "mc.example.com:25565",
            "[2001:db8::1]:25565",
            "192.0.2.0/24",
            "01890000-0000-7000-8000-000000000001",
            "agent-1",
            "0.1.2",
            "owner",
            "manage",
            "shared",
            "bot.start",
            "/spawn\n/home",
        ]),
        "chat_sanitizer" => texts(&[
            "\u{40}\0§aHello\r\nworld",
            "\u{8}\0a\u{202e}b\u{200b}c\nd",
            "\0\u{8}§",
        ]),
        "chat_formats" => texts(&[
            "[{any}] {sender} : \n{sender} : ",
            "{{{sender}}} » ",
            "{sender",
            "",
        ]),
        "chat_sender" => texts(&[
            "[{any}] {sender} : \n{sender} : \0[VIP] Steve : hello",
            "{sender} : \0Steve : hello",
            "<{sender}> \0<Alex> hi\nthere",
        ]),
        "private_targets" => texts(&[
            "10.0.0.0/8\n192.168.1.0/24",
            "fd00::/8\n::1/128",
            "0.0.0.0/0",
        ]),
        "mode_definition" => texts(&[
            r#"{"steps":[{"action":{"type":"swing_arm"},"schedule":{"type":"every","interval_ms":20000,"jitter_ms":0},"probability":100}]}"#,
            r#"{"name":"Farm","visibility":"private","steps":[{"action":{"type":"send_chat","message":"/spawn"},"schedule":{"type":"at_start"},"probability":100}]}"#,
            r#"{"version":1,"name":"Farm","visibility":"shared","steps":[{"action":{"type":"look","yaw":-90.5,"pitch":12.0},"schedule":{"type":"at_start"},"probability":50}]}"#,
        ]),
        "agent_message" => agent_messages(),
        "server_message" => server_messages(),
        "vault_envelope" => {
            let mut shaped = header_of(1).to_vec();
            shaped.extend_from_slice(&[0; 48]);
            vec![b"\x07\x01a secret".to_vec(), shaped, vec![1, 0, 0, 0, 1]]
        }
        "agent_metrics" => texts(&[
            "GET /metrics HTTP/1.1\r\nHost: agent\r\n\r\n",
            "HEAD /metrics?x=1 HTTP/1.0\r\n\r\n",
            "POST /metrics HTTP/1.1\r\n\r\n",
            "GET / HTTP/1.1\r\n\r\n",
            "GET /metrics HTTP/1.1\r\n",
        ]),
        _ => Vec::new(),
    }
}

/// `texts` as byte strings.
fn texts(texts: &[&str]) -> Vec<Vec<u8>> {
    texts.iter().map(|text| text.as_bytes().to_vec()).collect()
}

/// A hello, a heartbeat and an enrollment request, as an agent sends them.
fn agent_messages() -> Vec<Vec<u8>> {
    let hello = AgentName::try_from("agent-1")
        .ok()
        .zip(AgentVersion::try_from("0.1.2").ok())
        .zip(AgentCapacity::try_from(20).ok())
        .map(|((name, agent_version), capacity)| {
            AgentMessage::Hello(Hello {
                name,
                agent_version,
                protocol_version: PROTOCOL_VERSION,
                capacity,
                running: Vec::new(),
            })
        });
    let enroll = wire::EnrollRequest {
        token: "a-token".to_owned(),
        name: "agent-1".to_owned(),
        csr_der: vec![0x30, 0x00],
    };
    hello
        .into_iter()
        .chain([AgentMessage::Heartbeat])
        .filter_map(|message| wire::AgentMessage::try_from(message).ok())
        .map(|message| message.encode_to_vec())
        .chain([enroll.encode_to_vec()])
        .collect()
}

/// A welcome and an unassignment, as the server sends them.
fn server_messages() -> Vec<Vec<u8>> {
    let welcome = AgentId::try_from(ID).ok().map(|agent_id| {
        ServerMessage::Welcome(Welcome {
            heartbeat_interval: Duration::from_secs(10),
            agent_id,
        })
    });
    let unassign = BotId::try_from(ID).ok().map(ServerMessage::Unassign);
    welcome
        .into_iter()
        .chain(unassign)
        .filter_map(|message| wire::ServerMessage::try_from(message).ok())
        .map(|message| message.encode_to_vec())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TARGETS;

    #[test]
    fn every_target_has_seeds_and_no_other_name_has() {
        for (name, _) in TARGETS {
            assert!(!seeds(name).is_empty(), "{name}");
        }
        assert_eq!(seeds("no_such_target"), Vec::<Vec<u8>>::new());
    }

    #[test]
    fn the_message_seeds_are_whole_messages() {
        assert_eq!(agent_messages().len(), 3);
        assert_eq!(server_messages().len(), 2);
    }
}
