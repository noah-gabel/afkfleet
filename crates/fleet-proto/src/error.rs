//! [`ConvertError`]: why a value can't cross the wire.

use fleet_core::chat::{ChatMessageError, IncomingChatError};
use fleet_core::disconnect::ConflictTextsError;
use fleet_core::mode::{HotbarSlotError, ModeError};
use fleet_core::value::{
    AgentCapacityError, AgentNameError, AgentVersionError, McUsernameError, ServerAddressError,
};

/// Why a message can't be converted: which field, where in a list, and what's
/// wrong with it.
///
/// It never holds the field's value. A message from the other side may carry
/// chat, a kick or a token, so an error can go into a log at any level
/// (ADR-0019).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{field}{}: {problem}", at(*.index))]
pub struct ConvertError {
    /// The field, as `Message.field` in the protocol's names.
    pub field: &'static str,
    /// The entry's position in the innermost list the field belongs to,
    /// counting from 0: a bot in a hello or a full reconcile, a step of a mode.
    pub index: Option<usize>,
    /// What's wrong with it.
    pub problem: Problem,
}

/// Formats an error's list position.
fn at(index: Option<usize>) -> String {
    index.map_or_else(String::new, |index| format!(" [{index}]"))
}

impl ConvertError {
    /// An error in `field`, outside any list.
    #[must_use]
    pub const fn new(field: &'static str, problem: Problem) -> Self {
        Self {
            field,
            index: None,
            problem,
        }
    }

    /// The same error, at position `index` of the innermost list. A position
    /// that's already set stays: it's the more precise one.
    #[must_use]
    pub const fn at(self, index: usize) -> Self {
        match self.index {
            Some(_) => self,
            None => Self {
                index: Some(index),
                ..self
            },
        }
    }
}

/// What's wrong with a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Problem {
    /// The field is missing: a message, a oneof or an enum that isn't set,
    /// a time that isn't there, or a number that is 0 where it counts from 1.
    #[error("missing")]
    Missing,
    /// An enum value or a oneof case this version doesn't know.
    #[error("a value this version doesn't know")]
    Unknown,
    /// An ID or a UUID that isn't 16 bytes long.
    #[error("{len} bytes instead of 16")]
    WrongLength {
        /// The field's length in bytes.
        len: usize,
    },
    /// An entity's ID that isn't a version 7 UUID.
    #[error("not a version 7 UUID")]
    NotV7,
    /// A Minecraft profile's UUID that is the nil UUID.
    #[error("the nil UUID")]
    Nil,
    /// A number or a time outside its range.
    #[error("out of range")]
    OutOfRange,
    /// A list with more entries than it may have.
    #[error("{count} entries; the limit is {max}")]
    TooMany {
        /// How many entries it has.
        count: usize,
        /// The most it may have.
        max: usize,
    },
    /// A bot that appears twice in a list.
    #[error("a bot that appears twice")]
    Duplicate,
    /// A validated text that isn't in the exact form its type keeps, such as a
    /// chat message with spaces around it.
    #[error("not in its exact form")]
    NotExact,
    /// A Minecraft access token that's empty, longer than 16 KiB or not
    /// visible ASCII.
    #[error("not a Minecraft access token")]
    InvalidToken,
    /// An invalid chat message.
    #[error(transparent)]
    ChatMessage(#[from] ChatMessageError),
    /// Received chat whose sender doesn't fit its kind.
    #[error(transparent)]
    IncomingChat(#[from] IncomingChatError),
    /// An invalid list of conflict texts.
    #[error(transparent)]
    ConflictTexts(#[from] ConflictTextsError),
    /// An invalid mode.
    #[error(transparent)]
    Mode(#[from] ModeError),
    /// An invalid hotbar slot.
    #[error(transparent)]
    HotbarSlot(#[from] HotbarSlotError),
    /// An invalid Minecraft username.
    #[error(transparent)]
    McUsername(#[from] McUsernameError),
    /// An invalid server address.
    #[error(transparent)]
    ServerAddress(#[from] ServerAddressError),
    /// An invalid agent name.
    #[error(transparent)]
    AgentName(#[from] AgentNameError),
    /// An invalid agent version.
    #[error(transparent)]
    AgentVersion(#[from] AgentVersionError),
    /// An invalid agent capacity.
    #[error(transparent)]
    AgentCapacity(#[from] AgentCapacityError),
}
