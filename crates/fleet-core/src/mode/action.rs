//! [`Action`]: what a mode step makes the bot do, and [`HotbarSlot`].

use serde::{Deserialize, Serialize};

use crate::chat::ChatMessage;

/// Why a number isn't a [`HotbarSlot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HotbarSlotError {
    /// The number is above [`HotbarSlot::LAST`].
    #[error("hotbar slots are numbered 0 to 8")]
    OutOfRange,
}

/// One of the nine hotbar slots, numbered 0 to 8 from left to right.
///
/// azalea panics for a slot above 8 (ADR-0008 §8), so this type is the only
/// way to name one. It serializes as the plain number, and deserializing
/// rejects anything above 8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct HotbarSlot(u8);

impl HotbarSlot {
    /// The leftmost slot, 0.
    pub const FIRST: Self = Self(0);
    /// The rightmost slot, 8.
    pub const LAST: Self = Self(8);

    /// Returns the slot number, 0 to 8.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for HotbarSlot {
    type Error = HotbarSlotError;

    fn try_from(slot: u8) -> Result<Self, HotbarSlotError> {
        if slot <= Self::LAST.0 {
            Ok(Self(slot))
        } else {
            Err(HotbarSlotError::OutOfRange)
        }
    }
}

impl From<HotbarSlot> for u8 {
    fn from(slot: HotbarSlot) -> Self {
        slot.0
    }
}

/// What a mode step makes the bot do (Plan.md P2.7).
///
/// Angles are in degrees, as Minecraft's debug screen shows them: yaw turns
/// left and right, pitch looks up (negative) and down (positive).
/// [`ModeDraft::validate`](super::ModeDraft::validate) checks their ranges.
///
/// In JSON an action is an object tagged with its `"type"` in `snake_case`,
/// e.g. `{"type":"select_hotbar_slot","slot":3}` (ADR-0010).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Look in a fixed direction.
    Look {
        /// Degrees, −180 to 180.
        yaw: f32,
        /// Degrees, −90 (straight up) to 90 (straight down).
        pitch: f32,
    },
    /// Turn by a random amount, at most the given maxima in either direction.
    RotateRandom {
        /// The largest turn left or right, in degrees, 0 to 180.
        max_yaw: f32,
        /// The largest turn up or down, in degrees, 0 to 90.
        max_pitch: f32,
    },
    /// Jump once.
    Jump,
    /// Start or stop sneaking.
    Sneak {
        /// Whether the bot sneaks from now on.
        on: bool,
    },
    /// Swing the main arm.
    SwingArm,
    /// Use the held item, like a right-click.
    UseItem,
    /// Hold the use button down, or let go of it, like holding right-click
    /// (ADR-0011).
    ///
    /// Holding is for items used over time: it draws a bow until the bot
    /// lets go, raises a shield or eats. While the bot looks at a block or
    /// an entity within reach, holding clicks that instead, as
    /// [`Action::UseItem`] does, so for a shield, look at the sky or into
    /// open air. Right after a throw or a shot, that entity can be the
    /// projectile itself, which the bot still sees in front of it for a
    /// while (ADR-0011).
    ///
    /// Besides letting go, a hold ends when the bot dies, selects another
    /// hotbar slot, finishes the item (food) or disconnects. That's how the
    /// vanilla server handles them; only letting go is checked against a real
    /// server. At-start steps run again on each join, so an at-start hold
    /// comes back after a reconnect, but not after a respawn.
    HoldUse {
        /// Whether the bot holds the use button from now on.
        on: bool,
    },
    /// Attack the entity the bot is looking at, if one is within reach.
    AttackFacingEntity,
    /// Select a hotbar slot.
    SelectHotbarSlot {
        /// The slot.
        slot: HotbarSlot,
    },
    /// Send a chat message or a `/command`.
    SendChat {
        /// The text.
        message: ChatMessage,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn slot(n: u8) -> HotbarSlot {
        HotbarSlot::try_from(n).unwrap()
    }

    #[rstest]
    #[case::first(0)]
    #[case::middle(4)]
    #[case::last(8)]
    fn hotbar_slot_accepts_0_to_8(#[case] n: u8) {
        assert_eq!(slot(n).get(), n);
        assert_eq!(u8::from(slot(n)), n);
    }

    #[rstest]
    #[case::nine(9)]
    #[case::max(255)]
    fn hotbar_slot_rejects_numbers_above_8(#[case] n: u8) {
        assert_eq!(HotbarSlot::try_from(n), Err(HotbarSlotError::OutOfRange));
    }

    #[test]
    fn hotbar_slot_constants_are_the_ends() {
        assert_eq!((HotbarSlot::FIRST.get(), HotbarSlot::LAST.get()), (0, 8));
    }

    #[test]
    fn hotbar_slot_serializes_as_a_plain_number() {
        assert_eq!(serde_json::to_string(&slot(3)).unwrap(), "3");
        assert_eq!(serde_json::from_str::<HotbarSlot>("8").unwrap(), slot(8));
        assert!(serde_json::from_str::<HotbarSlot>("9").is_err());
        assert!(serde_json::from_str::<HotbarSlot>("-1").is_err());
    }

    #[test]
    fn action_json_matches_the_adr_example() {
        let action = Action::SelectHotbarSlot { slot: slot(3) };
        let json = r#"{"type":"select_hotbar_slot","slot":3}"#;

        assert_eq!(serde_json::to_string(&action).unwrap(), json);
        assert_eq!(serde_json::from_str::<Action>(json).unwrap(), action);
    }

    #[rstest]
    #[case::look(Action::Look { yaw: -90.5, pitch: 12.25 })]
    #[case::rotate_random(Action::RotateRandom { max_yaw: 30.0, max_pitch: 10.0 })]
    #[case::jump(Action::Jump)]
    #[case::sneak(Action::Sneak { on: true })]
    #[case::swing_arm(Action::SwingArm)]
    #[case::use_item(Action::UseItem)]
    #[case::hold_use(Action::HoldUse { on: false })]
    #[case::attack(Action::AttackFacingEntity)]
    #[case::select_hotbar_slot(Action::SelectHotbarSlot { slot: slot(8) })]
    #[case::send_chat(Action::SendChat { message: "/spawn".parse().unwrap() })]
    fn every_action_round_trips_through_json(#[case] action: Action) {
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(serde_json::from_str::<Action>(&json).unwrap(), action);
    }

    #[test]
    fn hold_use_is_tagged_hold_use() {
        let action = Action::HoldUse { on: true };
        let json = r#"{"type":"hold_use","on":true}"#;

        assert_eq!(serde_json::to_string(&action).unwrap(), json);
        assert_eq!(serde_json::from_str::<Action>(json).unwrap(), action);
    }

    #[rstest]
    #[case::unknown_type(r#"{"type":"fly"}"#)]
    #[case::missing_type(r#"{"yaw":1.0,"pitch":2.0}"#)]
    #[case::tuple_notation(r#"{"type":"select_hotbar_slot","slot":[3]}"#)]
    #[case::slot_above_8(r#"{"type":"select_hotbar_slot","slot":9}"#)]
    #[case::invalid_chat(r#"{"type":"send_chat","message":"a\nb"}"#)]
    #[case::empty_chat(r#"{"type":"send_chat","message":"  "}"#)]
    #[case::missing_field(r#"{"type":"look","yaw":1.0}"#)]
    #[case::wrong_field_type(r#"{"type":"sneak","on":"yes"}"#)]
    #[case::hold_use_without_on(r#"{"type":"hold_use"}"#)]
    #[case::hold_use_wrong_field_type(r#"{"type":"hold_use","on":"yes"}"#)]
    fn rejects_malformed_action_json(#[case] json: &str) {
        assert!(serde_json::from_str::<Action>(json).is_err(), "{json}");
    }

    #[rstest]
    #[case::unit_variant(r#"{"type":"jump","height":2}"#, Action::Jump)]
    #[case::struct_variant(
        r#"{"type":"look","yaw":1.0,"pitch":2.0,"speed":3}"#,
        Action::Look { yaw: 1.0, pitch: 2.0 }
    )]
    fn ignores_unknown_fields_in_stored_actions(#[case] json: &str, #[case] expected: Action) {
        assert_eq!(serde_json::from_str::<Action>(json).unwrap(), expected);
    }
}
