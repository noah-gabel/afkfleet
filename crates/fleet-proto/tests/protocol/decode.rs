//! Reading what the other side sent never panics, and what it accepts is a
//! valid value that reads back the same: random and mutated protobuf bytes,
//! received chat in any form, and any mode.

use core::time::Duration;

use fleet_core::chat::{ChatMessage, ChatSender, IncomingChat, PlayerChatKind};
use fleet_core::disconnect::{DisconnectReason, Kick};
use fleet_core::mode::{Action, HotbarSlot, ModeDefinition, ModeDraft, Schedule, Step};
use fleet_proto::control::{AgentMessage, ServerMessage};
use fleet_proto::generated as wire;
use proptest::collection::vec;
use proptest::prelude::*;
use prost::Message;

use crate::strategies::*;
use crate::{over_the_wire, same_server_message};

/// A valid message's bytes with some bytes overwritten, or cut short.
fn mutated(bytes: Vec<u8>) -> impl Strategy<Value = Vec<u8>> {
    let len = bytes.len();
    (
        vec((any::<prop::sample::Index>(), any::<u8>()), 0..4),
        prop_oneof![3 => Just(len), 1 => 0..=len],
    )
        .prop_map(move |(changes, keep)| {
            let mut bytes = bytes.clone();
            if !bytes.is_empty() {
                for (at, byte) in changes {
                    let at = at.index(bytes.len());
                    bytes[at] = byte;
                }
            }
            bytes.truncate(keep);
            bytes
        })
}

fn agent_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        vec(any::<u8>(), 0..200),
        agent_message()
            .prop_map(|message| wire::AgentMessage::try_from(message)
                .unwrap()
                .encode_to_vec())
            .prop_flat_map(mutated),
    ]
}

fn server_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        vec(any::<u8>(), 0..200),
        server_message()
            .prop_map(|message| wire::ServerMessage::try_from(message)
                .unwrap()
                .encode_to_vec())
            .prop_flat_map(mutated),
    ]
}

fn wire_kind() -> impl Strategy<Value = i32> {
    -2..8_i32
}

fn wire_chat() -> impl Strategy<Value = wire::IncomingChat> {
    (
        wire_kind(),
        proptest::option::of((raw_text(), proptest::option::of(vec(any::<u8>(), 0..20)))),
        raw_text(),
        any::<bool>(),
    )
        .prop_map(|(kind, sender, text, truncated)| wire::IncomingChat {
            kind,
            sender: sender.map(|(name, uuid)| wire::ChatSender { name, uuid }),
            text,
            truncated,
        })
}

fn wire_kick() -> impl Strategy<Value = wire::DisconnectReason> {
    (proptest::option::of(any::<String>()), raw_text()).prop_map(|(key, message)| {
        wire::DisconnectReason {
            reason: Some(wire::disconnect_reason::Reason::Kicked(wire::Kick {
                key,
                message,
            })),
        }
    })
}

fn angle() -> impl Strategy<Value = f32> {
    prop_oneof![
        4 => -90.0_f32..90.0,
        2 => -200.0_f32..200.0,
        1 => any::<f32>(),
        1 => Just(f32::NAN),
        1 => Just(0.0),
    ]
}

fn wire_action() -> impl Strategy<Value = Option<wire::Action>> {
    use wire::action::Action as A;
    proptest::option::weighted(
        0.95,
        proptest::option::weighted(
            0.95,
            prop_oneof![
                (angle(), angle())
                    .prop_map(|(yaw, pitch)| A::Look(wire::action::Look { yaw, pitch })),
                (angle(), angle()).prop_map(|(max_yaw, max_pitch)| A::RotateRandom(
                    wire::action::RotateRandom { max_yaw, max_pitch }
                )),
                Just(A::Jump(wire::Unit {})),
                any::<bool>().prop_map(|on| A::Sneak(wire::action::Sneak { on })),
                Just(A::SwingArm(wire::Unit {})),
                Just(A::UseItem(wire::Unit {})),
                any::<bool>().prop_map(|on| A::HoldUse(wire::action::HoldUse { on })),
                Just(A::AttackFacingEntity(wire::Unit {})),
                prop_oneof![0..12_u32, any::<u32>()]
                    .prop_map(|slot| A::SelectHotbarSlot(wire::action::SelectHotbarSlot { slot })),
                prop_oneof![
                    raw_text(),
                    " /spawn ".prop_map(String::from),
                    "hello".prop_map(String::from)
                ]
                .prop_map(|message| A::SendChat(wire::action::SendChat { message })),
            ],
        ),
    )
    .prop_map(|action| action.map(|action| wire::Action { action }))
}

fn wire_schedule() -> impl Strategy<Value = Option<wire::Schedule>> {
    use wire::schedule::Schedule as S;
    proptest::option::weighted(
        0.95,
        proptest::option::weighted(
            0.95,
            prop_oneof![
                Just(S::AtStart(wire::Unit {})),
                (
                    prop_oneof![4 => 0..100_000_u64, 1 => any::<u64>()],
                    prop_oneof![4 => 0..100_000_u64, 1 => any::<u64>()]
                )
                    .prop_map(|(interval_ms, jitter_ms)| S::Every(
                        wire::schedule::Every {
                            interval_ms,
                            jitter_ms
                        }
                    )),
            ],
        ),
    )
    .prop_map(|schedule| schedule.map(|schedule| wire::Schedule { schedule }))
}

fn wire_step() -> impl Strategy<Value = wire::Step> {
    (
        wire_action(),
        wire_schedule(),
        prop_oneof![8 => 1..=100_u32, 1 => 0..=300_u32, 1 => any::<u32>()],
    )
        .prop_map(|(action, schedule, probability)| wire::Step {
            action,
            schedule,
            probability,
        })
}

fn wire_mode() -> impl Strategy<Value = wire::ModeDefinition> {
    prop_oneof![
        4 => vec(wire_step(), 0..=4),
        1 => vec(wire_step(), 30..=35),
    ]
    .prop_map(|steps| wire::ModeDefinition { steps })
}

/// The draft a wire mode stands for, mapped independently of the crate, or
/// `None` if it can't be one: a missing action or schedule, a number too big
/// for its field, or a chat message that isn't in its exact form.
fn draft_of(mode: &wire::ModeDefinition) -> Option<ModeDraft> {
    use wire::action::Action as A;
    use wire::schedule::Schedule as S;
    let steps = mode
        .steps
        .iter()
        .map(|step| {
            let action = match step.action.as_ref()?.action.as_ref()? {
                A::Look(look) => Action::Look {
                    yaw: look.yaw,
                    pitch: look.pitch,
                },
                A::RotateRandom(rotate) => Action::RotateRandom {
                    max_yaw: rotate.max_yaw,
                    max_pitch: rotate.max_pitch,
                },
                A::Jump(_) => Action::Jump,
                A::Sneak(sneak) => Action::Sneak { on: sneak.on },
                A::SwingArm(_) => Action::SwingArm,
                A::UseItem(_) => Action::UseItem,
                A::HoldUse(hold) => Action::HoldUse { on: hold.on },
                A::AttackFacingEntity(_) => Action::AttackFacingEntity,
                A::SelectHotbarSlot(select) => Action::SelectHotbarSlot {
                    slot: HotbarSlot::try_from(u8::try_from(select.slot).ok()?).ok()?,
                },
                A::SendChat(send) => {
                    let message = ChatMessage::try_from(send.message.as_str()).ok()?;
                    if message.as_str() != send.message {
                        return None;
                    }
                    Action::SendChat { message }
                }
            };
            let schedule = match step.schedule.as_ref()?.schedule.as_ref()? {
                S::AtStart(_) => Schedule::AtStart,
                S::Every(every) => Schedule::Every {
                    interval: Duration::from_millis(every.interval_ms),
                    jitter: Duration::from_millis(every.jitter_ms),
                },
            };
            Some(Step {
                action,
                schedule,
                probability: u8::try_from(step.probability).ok()?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(ModeDraft { steps })
}

proptest! {
    #[test]
    fn any_agent_bytes_are_refused_or_read_back_the_same(bytes in agent_bytes()) {
        if let Ok(decoded) = wire::AgentMessage::decode(bytes.as_slice())
            && let Ok(message) = AgentMessage::try_from(decoded)
        {
            prop_assert_eq!(
                over_the_wire::<AgentMessage, wire::AgentMessage>(message.clone()),
                Ok(message)
            );
        }
    }

    #[test]
    fn any_server_bytes_are_refused_or_read_back_the_same(bytes in server_bytes()) {
        if let Ok(decoded) = wire::ServerMessage::decode(bytes.as_slice())
            && let Ok(message) = ServerMessage::try_from(decoded)
        {
            let back = over_the_wire::<ServerMessage, wire::ServerMessage>(message.clone()).unwrap();
            prop_assert!(same_server_message(&back, &message));
        }
    }

    #[test]
    fn received_chat_is_always_clean_and_capped(chat in wire_chat()) {
        if let Ok(read) = IncomingChat::try_from(chat.clone()) {
            prop_assert!(read.text().chars().count() <= IncomingChat::MAX_TEXT_LEN);
            prop_assert!(
                read.sender().is_none_or(|sender| sender.name().chars().count() <= ChatSender::MAX_NAME_LEN)
            );
            // The sanitizer leaves the text and the name as they are: they're clean.
            let cleaned = IncomingChat::system(read.text());
            prop_assert_eq!(cleaned.text(), read.text());
            if let Some(sender) = read.sender() {
                let again = IncomingChat::player(PlayerChatKind::Chat, sender.name(), None, "");
                prop_assert_eq!(again.sender().map(ChatSender::name), Some(sender.name()));
            }
            // And it reads back unchanged.
            prop_assert_eq!(IncomingChat::try_from(wire::IncomingChat::from(read.clone())), Ok(read.clone()));
            // The agent's flag is kept, and a cut the server made is added.
            prop_assert!(!chat.truncated || read.is_truncated());
        }
    }

    #[test]
    fn a_kick_is_always_clean_and_capped(reason in wire_kick()) {
        let read = DisconnectReason::try_from(reason).unwrap();
        let DisconnectReason::Kicked(kick) = &read else {
            return Err(TestCaseError::fail("not a kick"));
        };
        prop_assert!(kick.message().chars().count() <= Kick::MAX_MESSAGE_LEN);
        let again = DisconnectReason::kicked(None, kick.message());
        let DisconnectReason::Kicked(again) = &again else {
            return Err(TestCaseError::fail("not a kick"));
        };
        prop_assert_eq!(again.message(), kick.message());
        prop_assert_eq!(DisconnectReason::try_from(wire::DisconnectReason::from(read.clone())), Ok(read.clone()));
    }

    #[test]
    fn a_mode_is_read_exactly_when_fleet_core_accepts_it(mode in wire_mode()) {
        let expected = draft_of(&mode).and_then(|draft| draft.validate().ok());
        prop_assert_eq!(ModeDefinition::try_from(mode).ok(), expected);
    }
}
