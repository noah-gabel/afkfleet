//! What a session shows and does for a scripted mode in a screen or a sign
//! editor (Plan.md P16.8 to P16.10; ADR-0036, M2, M3, M7, M8, M16, R1;
//! ADR-0050, ADR-0051): the [`MenuView`] an observation carries, the
//! [`MenuCommand`]s [`SessionHandle::menu`](super::SessionHandle::menu)
//! takes, and the [`MenuError`] it fails with.
//!
//! fleet-core's `script::menu` decides everything here; a session only
//! feeds it what the server sends and asks it before it sends anything.

use super::SessionError;
use crate::script::menu::{
    BotInventory, MenuAction, MenuRefusal, Screen, ScreenId, SignAnswer, SignEditor, SignRefusal,
};

/// What the bot sees of its inventory, its screen and its sign editor at
/// one moment: part of an [`Observation`](super::Observation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuView {
    /// The bot's own inventory: its armour, its 36 slots and its offhand.
    pub inventory: BotInventory,
    /// The screen that's open: the server's, or the bot's own inventory.
    pub screen: Option<Screen>,
    /// The sign editor the server opened last, while it's open and
    /// unanswered.
    pub sign: Option<SignEditor>,
}

impl Default for MenuView {
    /// An empty inventory, and neither a screen nor a sign editor open.
    fn default() -> Self {
        Self {
            inventory: BotInventory::new(core::iter::empty()),
            screen: None,
            sign: None,
        }
    }
}

/// Something a scripted mode does in a screen or a sign editor, through
/// [`SessionHandle::menu`](super::SessionHandle::menu).
///
/// Its `Debug` holds a sign's text, which is the script's: log it at
/// `debug` only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuCommand {
    /// A click, a screen's button, a trade or an anvil's name. It's guarded
    /// on the session's host thread at the moment of sending, against the
    /// screen as it is then; it goes out only if no other action's fate is
    /// still to come, with its barrier right behind it, and its fate comes
    /// as [`SessionEvent::Clicked`](super::SessionEvent::Clicked) (ADR-0036,
    /// M3, R1).
    Act(MenuAction),
    /// Closes the screen the hook saw, as Esc does. A close is never
    /// answered, and an action still pending on the screen is `replaced`.
    Close(ScreenId),
    /// Opens the bot's own inventory, as E does: only when no screen is
    /// open. Nothing goes to the server.
    OpenInventory,
    /// Answers the sign editor the hook saw, once (ADR-0036, M16).
    AnswerSign {
        /// The editor the hook saw.
        editor: SignEditor,
        /// What it writes, or that it cancels.
        answer: SignAnswer,
    },
}

/// Why a [`MenuCommand`] wasn't carried out. Nothing was sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MenuError {
    /// The session didn't run it: as for any other session call.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// The screen, its content or the rule of one action at a time said
    /// no: `result.reason`.
    #[error("refused in the screen: {0}")]
    Refused(MenuRefusal),
    /// The sign editor said no: `result.reason`.
    #[error("the sign wasn't answered: {0}")]
    Sign(SignRefusal),
}

impl From<MenuRefusal> for MenuError {
    fn from(refusal: MenuRefusal) -> Self {
        Self::Refused(refusal)
    }
}

impl From<SignRefusal> for MenuError {
    fn from(refusal: SignRefusal) -> Self {
        Self::Sign(refusal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn a_new_view_holds_an_empty_inventory_and_nothing_open() {
        let view = MenuView::default();

        assert!(view.inventory.slots().all(|slot| slot.item().is_none()));
        assert_eq!(view.inventory.slots().count(), 41);
        assert_eq!(view.screen, None);
        assert_eq!(view.sign, None);
    }

    #[rstest]
    #[case::session(MenuError::Session(SessionError::Closed), "the session has ended")]
    #[case::refused(
        MenuError::Refused(MenuRefusal::SlotChanged),
        "refused in the screen: slot_changed"
    )]
    #[case::sign(
        MenuError::Sign(SignRefusal::Answered),
        "the sign wasn't answered: sign_answered"
    )]
    fn errors_say_why_in_a_scripts_words(#[case] error: MenuError, #[case] message: &str) {
        assert_eq!(error.to_string(), message);
    }

    #[test]
    fn each_kind_of_no_converts_into_its_own_error() {
        assert_eq!(
            MenuError::from(SessionError::QueueFull),
            MenuError::Session(SessionError::QueueFull)
        );
        assert_eq!(
            MenuError::from(MenuRefusal::ClickPending),
            MenuError::Refused(MenuRefusal::ClickPending)
        );
        assert_eq!(
            MenuError::from(SignRefusal::NoSign),
            MenuError::Sign(SignRefusal::NoSign)
        );
    }
}
