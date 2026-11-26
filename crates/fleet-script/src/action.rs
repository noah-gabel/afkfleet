//! [`Action`]: what a hook asks the bot to do, in the order it asked.

use core::time::Duration;

use fleet_core::chat::ChatMessage;
use fleet_core::mc::MenuCommand;
use fleet_core::mode::HotbarSlot;
use fleet_core::script::{Ident, Identifier, Input, Position, Refusal, SendId};

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

/// A kind of thing a script asks for that can be refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Asked {
    /// A tap or a hold of a key or a button.
    Press,
    /// A look in a direction, a turn, or a look at a point or an entity.
    Look,
    /// A hotbar slot.
    Hotbar,
    /// Getting into a vehicle, or out of it.
    Vehicle,
    /// A declared message: `chat.send`.
    Send,
    /// A reply's window: `chat.expect`.
    Expect,
    /// A timer.
    Timer,
    /// A line for the log.
    Log,
    /// A click, a screen's button, a trade or an anvil's name (Phase 16).
    Click,
    /// Opening the inventory or closing a screen.
    Screen,
    /// Answering a sign editor.
    Sign,
}

impl Asked {
    /// Every kind, in a fixed order.
    pub const ALL: [Self; 11] = [
        Self::Press,
        Self::Look,
        Self::Hotbar,
        Self::Vehicle,
        Self::Send,
        Self::Expect,
        Self::Timer,
        Self::Log,
        Self::Click,
        Self::Screen,
        Self::Sign,
    ];

    /// The kind as a short name: for a metric's label and the script's
    /// log.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Press => "press",
            Self::Look => "look",
            Self::Hotbar => "hotbar",
            Self::Vehicle => "vehicle",
            Self::Send => "send",
            Self::Expect => "expect",
            Self::Timer => "timer",
            Self::Log => "log",
            Self::Click => "click",
            Self::Screen => "screen",
            Self::Sign => "sign",
        }
    }
}

/// What a call on an instance refused of one kind for one reason, and how
/// often: for whoever runs the script to count, and to write into its log
/// (ADR-0036, D7; ADR-0037, C7).
///
/// The script itself got each refusal as a result. This is the same thing
/// told to the runtime, at most once for each kind and reason however
/// often a hook asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Refused {
    /// What was asked for.
    pub asked: Asked,
    /// Why it wasn't taken.
    pub reason: Refusal,
    /// How often, in this call.
    pub times: u32,
}

/// Counts one more refusal of `asked` for `reason` in `refused`.
pub(crate) fn note(refused: &mut Vec<Refused>, asked: Asked, reason: Refusal, times: u32) {
    let found = refused
        .iter_mut()
        .find(|known| known.asked == asked && known.reason == reason);
    match found {
        Some(known) => known.times = known.times.saturating_add(times),
        None => refused.push(Refused {
            asked,
            reason,
            times,
        }),
    }
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
    /// Something done in a screen or a sign editor (Phase 16): a click, a
    /// screen's button, a trade or an anvil's name, a close, the inventory
    /// opened, a sign answered. The session decides it again at the moment
    /// of sending, and an action's fate comes back as `on_click`.
    Menu(MenuCommand),
    /// A line for the script's log: one line, at most
    /// [`LOG_LINE_CHARS`](fleet_core::script::limits::LOG_LINE_CHARS)
    /// characters, nothing in it that could reorder or hide a line.
    Log(String),
}
