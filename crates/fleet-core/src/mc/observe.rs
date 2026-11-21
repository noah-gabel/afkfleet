//! What a session reports and takes for scripted modes: an [`Observation`]
//! of the game right before a hook, the [`Guards`] a mode sets below its
//! script, and [`LetGo`], why a session let go of inputs by itself
//! (Plan.md P15.10 to P15.13; ADR-0036, D9 to D12).

use crate::script::{
    BlockGuard, Input, Key, LeashRadius, MouseButton, Position, Snapshot, ToolGuard,
};

/// What a session sees of the game at one moment, taken on its host thread
/// right before a hook runs (ADR-0036, D12).
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    /// The bot, the entities near it and the blocks around it.
    pub facts: Snapshot,
    /// Where the bot was put, which its leash is measured from: its
    /// position at the join, a respawn, a world change, or after a server
    /// teleport of more than
    /// [`Leash::TELEPORT_RESET`](crate::script::Leash::TELEPORT_RESET)
    /// blocks. `None` until the server has placed the bot in its world.
    pub anchor: Option<Position>,
}

/// The three settings of a scripted mode that a session applies below the
/// script, every game tick (ADR-0036, D10, D11; ADR-0042's amendment, G13).
///
/// A new session starts with the defaults of all three, so whoever runs a
/// script sets them again at every join, before it presses anything.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Guards {
    /// How far the bot walks from where it was put.
    pub leash: LeashRadius,
    /// How many uses a tool keeps when the bot stops mining with it.
    pub tool_guard: ToolGuard,
    /// The blocks a held left button mines; none listed for any block.
    pub block_guard: BlockGuard,
}

/// Why a session let go of inputs by itself, below the script
/// ([`SessionEvent::LetGo`](super::SessionEvent::LetGo)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LetGo {
    /// The bot was beyond its leash and a game tick took it further: every
    /// movement key is up.
    Leash,
    /// The held tool is down to the uses its guard keeps: the left button
    /// is up, and mining stopped.
    ToolGuard,
    /// The item the right button was held for, until used, is used up
    /// once: the right button is up.
    ItemUsed,
}

impl LetGo {
    /// Every reason, in a fixed order.
    pub const ALL: [Self; 3] = [Self::Leash, Self::ToolGuard, Self::ItemUsed];

    /// The inputs the session let go of.
    #[must_use]
    pub fn inputs(self) -> Vec<Input> {
        match self {
            Self::Leash => Key::ALL
                .into_iter()
                .filter(|key| key.is_movement())
                .map(Input::Key)
                .collect(),
            Self::ToolGuard => vec![Input::Mouse(MouseButton::Left)],
            Self::ItemUsed => vec![Input::Mouse(MouseButton::Right)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_leash_lets_go_of_every_movement_key_and_nothing_else() {
        let inputs = LetGo::Leash.inputs();

        assert!(inputs.contains(&Input::Key(Key::Forward)));
        assert!(inputs.contains(&Input::Key(Key::Back)));
        assert!(inputs.contains(&Input::Key(Key::Left)));
        assert!(inputs.contains(&Input::Key(Key::Right)));
        assert!(inputs.iter().all(|input| input.is_movement()));
        assert_eq!(
            inputs.len(),
            Key::ALL.iter().filter(|key| key.is_movement()).count()
        );
    }

    #[test]
    fn the_guards_of_the_buttons_let_go_of_their_button() {
        assert_eq!(LetGo::ToolGuard.inputs(), [Input::Mouse(MouseButton::Left)]);
        assert_eq!(LetGo::ItemUsed.inputs(), [Input::Mouse(MouseButton::Right)]);
    }

    #[test]
    fn a_new_sessions_guards_are_the_defaults_of_a_mode_that_sets_none() {
        assert_eq!(
            Guards::default(),
            Guards {
                leash: LeashRadius::DEFAULT,
                tool_guard: ToolGuard::DEFAULT,
                block_guard: BlockGuard::ANY,
            }
        );
    }

    #[test]
    fn every_reason_is_listed_once() {
        for reason in LetGo::ALL {
            assert_eq!(
                LetGo::ALL.iter().filter(|other| **other == reason).count(),
                1
            );
            assert_ne!(reason.inputs(), []);
        }
    }
}
