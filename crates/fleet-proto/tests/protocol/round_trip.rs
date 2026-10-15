//! Every valid core value survives the wire unchanged: core, the generated
//! message, protobuf bytes, the generated message, core.

use chrono::{DateTime, Utc};
use fleet_core::bot::{BotNotification, BotSnapshot, BotSpec, BotState, StickyState};
use fleet_core::chat::IncomingChat;
use fleet_core::disconnect::DisconnectReason;
use fleet_core::mode::ModeDefinition;
use fleet_proto::control::{AgentMessage, BotEvent, BotEventKind, ServerMessage};
use fleet_proto::generated as wire;
use proptest::prelude::*;

use crate::strategies::*;
use crate::{over_the_wire, same_server_message};

/// Every value of each enum the wire maps, so a mapping that swaps two of them
/// fails here, not only when a random case happens to reach it.
mod every_value {
    use core::time::Duration;

    use fleet_core::bot::{FailReason, PauseReason};
    use fleet_core::chat::{IncomingChat, PlayerChatKind};
    use fleet_core::disconnect::{
        AccountRestriction, ConflictKind, ConnectFailure, DisconnectReason, PermanentKind,
        SessionServerFailure,
    };
    use fleet_proto::control::{
        AgentMessage, BotEvent, BotEventKind, ChatFailedReason, RejectReason, ServerMessage,
    };
    use fleet_proto::generated as wire;

    use super::{infallible, over_the_wire};
    use crate::same_server_message;

    const BOT: &str = "018bcfe5-6800-7bab-abab-abababababab";
    const REQUEST: &str = "018bcfe5-6800-7aaa-9aaa-aaaaaaaaaaaa";

    fn event(kind: BotEventKind) -> AgentMessage {
        AgentMessage::Event(BotEvent {
            bot_id: BOT.parse().unwrap(),
            at: chrono::DateTime::from_timestamp_millis(1_790_000_000_000).unwrap(),
            kind,
        })
    }

    #[test]
    fn every_chat_failure_reason_round_trips() {
        for reason in [
            ChatFailedReason::Disconnected,
            ChatFailedReason::ChatUnavailable,
            ChatFailedReason::TimedOut,
            ChatFailedReason::SessionBusy,
            ChatFailedReason::NotInWorld,
            ChatFailedReason::NotOnline,
            ChatFailedReason::QueueFull,
            ChatFailedReason::RateLimited,
            ChatFailedReason::UnknownBot,
            ChatFailedReason::AgentBusy,
            ChatFailedReason::ShuttingDown,
        ] {
            let message = event(BotEventKind::ChatFailed {
                request: REQUEST.parse().unwrap(),
                reason,
            });
            assert_eq!(
                over_the_wire::<AgentMessage, wire::AgentMessage>(message.clone()),
                Ok(message)
            );
        }
    }

    #[test]
    fn every_pause_and_fail_reason_round_trips() {
        let pause = PauseReason::Conflict {
            kind: ConflictKind::DuplicateLogin,
        };
        let fails = [
            FailReason::Permanent {
                kind: PermanentKind::Banned,
            },
            FailReason::Permanent {
                kind: PermanentKind::NotWhitelisted,
            },
            FailReason::Permanent {
                kind: PermanentKind::WrongVersion,
            },
            FailReason::Permanent {
                kind: PermanentKind::AccountBanned,
            },
            FailReason::Permanent {
                kind: PermanentKind::MultiplayerDisabled,
            },
            FailReason::Auth,
            FailReason::SessionDenied,
            FailReason::CrashLoop,
        ];
        let sticky = std::iter::once(fleet_core::bot::StickyState::Paused(pause))
            .chain(fails.map(fleet_core::bot::StickyState::Failed));
        for state in sticky {
            assert_eq!(
                infallible::<_, wire::StickyState>(state),
                Ok(state),
                "{state:?}"
            );
            let bot_state = fleet_core::bot::BotState::from(state);
            assert_eq!(over_the_wire::<_, wire::BotState>(bot_state), Ok(bot_state));
        }
    }

    #[test]
    fn every_disconnect_reason_round_trips() {
        let reasons = [
            DisconnectReason::ConnectionClosed,
            DisconnectReason::AuthRejected,
            DisconnectReason::SessionCrashed,
            DisconnectReason::WatchdogTimeout,
            DisconnectReason::LivenessTimeout,
            DisconnectReason::RespawnFailed,
        ]
        .into_iter()
        .chain(
            [
                ConnectFailure::Refused,
                ConnectFailure::TimedOut,
                ConnectFailure::Unresolvable,
                ConnectFailure::HostUnavailable,
                ConnectFailure::Other,
            ]
            .map(|failure| DisconnectReason::ConnectFailed { failure }),
        )
        .chain(
            [
                AccountRestriction::Banned,
                AccountRestriction::MultiplayerDisabled,
            ]
            .map(|restriction| DisconnectReason::AccountRestricted { restriction }),
        )
        .chain(
            [
                SessionServerFailure::Unreachable,
                SessionServerFailure::RateLimited,
                SessionServerFailure::TimedOut,
                SessionServerFailure::Unexpected,
            ]
            .map(|failure| DisconnectReason::SessionServerFailed { failure }),
        );
        for reason in reasons {
            assert_eq!(
                infallible::<_, wire::DisconnectReason>(reason.clone()),
                Ok(reason)
            );
        }
    }

    #[test]
    fn every_chat_kind_round_trips() {
        let mut chats = vec![IncomingChat::system("hello")];
        chats.extend(
            [
                PlayerChatKind::Chat,
                PlayerChatKind::Emote,
                PlayerChatKind::Whisper,
                PlayerChatKind::Announcement,
            ]
            .map(|kind| IncomingChat::player(kind, "AfkBot2", None, "hello")),
        );
        for chat in chats {
            assert_eq!(infallible::<_, wire::IncomingChat>(chat.clone()), Ok(chat));
        }
    }

    #[test]
    fn every_reject_reason_and_run_state_round_trips() {
        for reason in [
            RejectReason::IncompatibleProtocol,
            RejectReason::AgentDisabled,
        ] {
            let message = ServerMessage::Reject(reason);
            let back = over_the_wire::<_, wire::ServerMessage>(message.clone()).unwrap();
            assert!(same_server_message(&back, &message));
        }
        for desired in [
            fleet_core::bot::DesiredRunState::Running,
            fleet_core::bot::DesiredRunState::Stopped,
        ] {
            let spec = fleet_core::bot::BotSpec {
                id: BOT.parse().unwrap(),
                account: fleet_core::bot::BotAccount::Offline("AfkBot1".parse().unwrap()),
                server: "localhost:25565".parse().unwrap(),
                mode: fleet_core::mode::ModeDefinition::afk(),
                desired,
                conflict_texts: fleet_core::disconnect::ConflictTexts::default(),
            };
            assert_eq!(infallible::<_, wire::BotSpec>(spec.clone()), Ok(spec));
        }
        let welcome = ServerMessage::Welcome(fleet_proto::control::Welcome {
            heartbeat_interval: Duration::from_secs(10),
        });
        let back = over_the_wire::<_, wire::ServerMessage>(welcome.clone()).unwrap();
        assert!(same_server_message(&back, &welcome));
    }
}

/// The core-to-wire conversions that can't fail, sent over the wire.
fn infallible<T, W>(value: T) -> Result<T, fleet_proto::ConvertError>
where
    W: prost::Message + Default + From<T>,
    T: TryFrom<W, Error = fleet_proto::ConvertError>,
{
    let bytes = W::from(value).encode_to_vec();
    T::try_from(W::decode(bytes.as_slice()).unwrap())
}

proptest! {
    #[test]
    fn a_bot_spec_round_trips(spec in bot_spec()) {
        prop_assert_eq!(infallible::<BotSpec, wire::BotSpec>(spec.clone()), Ok(spec));
    }

    #[test]
    fn a_mode_round_trips(mode in mode()) {
        prop_assert_eq!(infallible::<ModeDefinition, wire::ModeDefinition>(mode.clone()), Ok(mode));
    }

    #[test]
    fn a_bot_state_round_trips(state in bot_state()) {
        prop_assert_eq!(over_the_wire::<BotState, wire::BotState>(state), Ok(state));
    }

    #[test]
    fn a_sticky_state_round_trips(sticky in sticky_state()) {
        prop_assert_eq!(infallible::<StickyState, wire::StickyState>(sticky), Ok(sticky));
    }

    #[test]
    fn an_alert_round_trips(notification in bot_notification()) {
        prop_assert_eq!(
            infallible::<BotNotification, wire::BotNotification>(notification),
            Ok(notification)
        );
    }

    #[test]
    fn a_disconnect_reason_round_trips(reason in disconnect_reason()) {
        prop_assert_eq!(
            infallible::<DisconnectReason, wire::DisconnectReason>(reason.clone()),
            Ok(reason)
        );
    }

    #[test]
    fn received_chat_round_trips(chat in incoming_chat()) {
        prop_assert_eq!(infallible::<IncomingChat, wire::IncomingChat>(chat.clone()), Ok(chat));
    }

    #[test]
    fn a_snapshot_round_trips(snapshot in snapshot()) {
        prop_assert_eq!(
            over_the_wire::<BotSnapshot, wire::BotSnapshot>(snapshot.clone()),
            Ok(snapshot)
        );
    }

    #[test]
    fn every_agent_message_round_trips(message in agent_message()) {
        prop_assert_eq!(
            over_the_wire::<AgentMessage, wire::AgentMessage>(message.clone()),
            Ok(message)
        );
    }

    #[test]
    fn every_server_message_round_trips(message in server_message()) {
        let back = over_the_wire::<ServerMessage, wire::ServerMessage>(message.clone()).unwrap();
        prop_assert!(same_server_message(&back, &message), "{back:?} != {message:?}");
    }

    #[test]
    fn a_time_finer_than_a_millisecond_arrives_truncated(
        millis in 0..4_000_000_000_000_i64,
        nanos in 0..1_000_000_u32,
        bot_id in bot_id(),
    ) {
        let at = DateTime::<Utc>::from_timestamp_millis(millis).unwrap()
            + chrono::TimeDelta::nanoseconds(i64::from(nanos));
        let event = AgentMessage::Event(BotEvent { bot_id, at, kind: BotEventKind::Died });
        let back = over_the_wire::<AgentMessage, wire::AgentMessage>(event).unwrap();
        let AgentMessage::Event(back) = back else {
            return Err(TestCaseError::fail("not an event"));
        };
        prop_assert_eq!(back.at, DateTime::<Utc>::from_timestamp_millis(millis).unwrap());
    }
}
