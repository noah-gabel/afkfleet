//! [`Action`]: what a hook asks the bot to do, in the order it asked.

use core::time::Duration;

use fleet_core::chat::ChatMessage;
use fleet_core::mode::HotbarSlot;
use fleet_core::script::{Ident, Identifier, Input, Position, SendId};

/// How an input goes down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PressKind {
    /// Pressed and let go at once.
    Tap,
    /// Pressed and kept down. A `hold_for` is this too: the instance lets
    /// go of it itself when its time is up, with an [`Action::Release`].
    Hold,
    /// The right button, kept down until the held item is used up once;
    /// whoever performs it reports the release
    /// ([`Instance::let_go`](crate::Instance::let_go)).
    HoldUntilUsed,
}

/// Something a hook asked for, already past the limits below the script
/// (ADR-0036, D4, D7).
///
/// A hook's actions are performed after it returns, in order. Every one of
/// them was allowed when the hook asked: the bot was in the world and
/// alive, the action's kind hadn't happened too recently, and for a press,
/// neither the leash nor a guard refused it. The session below still
/// applies its own checks each tick.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Let go of every key and both buttons, and stop mining. First before
    /// each hook that puts the bot somewhere new, and the only action left
    /// when a script stops.
    ReleaseAll,
    /// Let go of every key, not the buttons: `keys.release_all()`.
    ReleaseKeys,
    /// Press a key or a button.
    Press {
        /// Which one.
        input: Input,
        /// How.
        kind: PressKind,
    },
    /// Let go of a key or a button.
    Release(Input),
    /// Look in a direction, in degrees.
    LookAt {
        /// Left and right, −180 to 180.
        yaw: f64,
        /// Up (negative) and down, −90 to 90.
        pitch: f64,
    },
    /// Turn by an amount from where the bot looks, in degrees.
    LookTurn {
        /// Left and right.
        yaw: f64,
        /// Up (negative) and down.
        pitch: f64,
    },
    /// Look at a point in the world.
    LookAtPosition(Position),
    /// Select a hotbar slot.
    SelectHotbar(HotbarSlot),
    /// Get into the nearest free vehicle of this kind.
    Mount(Identifier),
    /// Get out of the vehicle.
    Dismount,
    /// Send a declared message.
    Send {
        /// The send's number, which a reply's window waits for.
        id: SendId,
        /// The declared message's key, for the script's log.
        key: Ident,
        /// The finished text.
        message: ChatMessage,
    },
    /// Open a window for a reply pattern once the command `send` is handed
    /// to the server (ADR-0037, C4).
    Expect {
        /// The command the window waits for.
        send: SendId,
        /// The reply pattern's key.
        pattern: Ident,
        /// How long the window stays open.
        window: Duration,
    },
    /// A line for the script's log: one line, at most
    /// [`LOG_LINE_CHARS`](fleet_core::script::limits::LOG_LINE_CHARS)
    /// characters, nothing in it that could reorder or hide a line.
    Log(String),
}
