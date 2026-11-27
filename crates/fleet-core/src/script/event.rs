//! [`Hook`] and [`HookEvent`]: what a script is called for, and
//! [`Refusal`], why an action wasn't taken (ADR-0036, D4, D6).

use core::fmt;

use super::menu::{ClickFate, ClickResult, MenuRefusal, SignRefusal};
use super::pattern::ChatMatch;
use super::settings::ScriptApiLevel;
use super::word::is_forbidden_in_declaration;

/// A function a script may define, which the runtime calls when its event
/// happens (ADR-0036, D6; ADR-0037, C6).
///
/// A script defines the hooks it needs. A function whose name starts with
/// [`Hook::PREFIX`] and isn't one of these, or one with other parameters,
/// is refused when the mode is saved, so a misspelled hook can't go
/// unnoticed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hook {
    /// `on_join(first)`: the bot is in the world after connecting.
    Join,
    /// `on_world_change()`: the server moved the bot to another world
    /// without a disconnect.
    WorldChange,
    /// `on_death()`: the bot died.
    Death,
    /// `on_respawn()`: the bot lives again after a death.
    Respawn,
    /// `on_timer(name)`: a timer the script set is due.
    Timer,
    /// `on_chat(key, values)`: a declared chat pattern matched.
    Chat,
    /// `on_stop()`: the script stops in an orderly way.
    Stop,
    /// `on_menu_open()`: a screen opened, the server's or the bot's own
    /// inventory (ADR-0036, M2). API level 2.
    MenuOpen,
    /// `on_menu_update()`: the open screen's content changed. API level 2.
    MenuUpdate,
    /// `on_menu_close()`: the screen closed. API level 2.
    MenuClose,
    /// `on_click(result)`: what became of a click, a button, a trade or a
    /// name the script sent (ADR-0036, R1). API level 2.
    Click,
    /// `on_sign_open()`: the server opened a sign editor (ADR-0036, M16).
    /// API level 2.
    SignOpen,
}

impl Hook {
    /// Every hook, in a fixed order.
    pub const ALL: [Self; 12] = [
        Self::Join,
        Self::WorldChange,
        Self::Death,
        Self::Respawn,
        Self::Timer,
        Self::Chat,
        Self::Stop,
        Self::MenuOpen,
        Self::MenuUpdate,
        Self::MenuClose,
        Self::Click,
        Self::SignOpen,
    ];

    /// What every hook's name starts with. No other function of a script
    /// may start with it.
    pub const PREFIX: &'static str = "on_";

    /// The function's name in a script.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Join => "on_join",
            Self::WorldChange => "on_world_change",
            Self::Death => "on_death",
            Self::Respawn => "on_respawn",
            Self::Timer => "on_timer",
            Self::Chat => "on_chat",
            Self::Stop => "on_stop",
            Self::MenuOpen => "on_menu_open",
            Self::MenuUpdate => "on_menu_update",
            Self::MenuClose => "on_menu_close",
            Self::Click => "on_click",
            Self::SignOpen => "on_sign_open",
        }
    }

    /// The function's parameters, as the reference names them.
    #[must_use]
    pub const fn parameters(self) -> &'static [&'static str] {
        match self {
            Self::Join => &["first"],
            Self::Timer => &["name"],
            Self::Chat => &["key", "values"],
            Self::Click => &["result"],
            Self::WorldChange
            | Self::Death
            | Self::Respawn
            | Self::Stop
            | Self::MenuOpen
            | Self::MenuUpdate
            | Self::MenuClose
            | Self::SignOpen => &[],
        }
    }

    /// The API level a script that defines this hook needs: Phase 16's
    /// menu hooks are level 2 (ADR-0036, D13).
    #[must_use]
    pub const fn level(self) -> ScriptApiLevel {
        match self {
            Self::Join
            | Self::WorldChange
            | Self::Death
            | Self::Respawn
            | Self::Timer
            | Self::Chat
            | Self::Stop => ScriptApiLevel::V1,
            Self::MenuOpen | Self::MenuUpdate | Self::MenuClose | Self::Click | Self::SignOpen => {
                ScriptApiLevel::V2
            }
        }
    }

    /// The hook with this function name, if there is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|hook| hook.name() == name)
    }

    /// Whether everything a script holds is let go before the hook runs:
    /// before the four that put the bot somewhere new, so the hook decides
    /// again what to hold (ADR-0036, D6).
    #[must_use]
    pub const fn releases_first(self) -> bool {
        match self {
            Self::Join | Self::WorldChange | Self::Death | Self::Respawn => true,
            Self::Timer
            | Self::Chat
            | Self::Stop
            | Self::MenuOpen
            | Self::MenuUpdate
            | Self::MenuClose
            | Self::Click
            | Self::SignOpen => false,
        }
    }
}

impl fmt::Display for Hook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Why text isn't a [`TimerName`]. It never holds the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TimerNameError {
    /// The name is empty.
    #[error("the timer's name is empty")]
    Empty,
    /// The name is longer than [`TimerName::MAX_LEN`] characters.
    #[error("the timer's name is {chars} characters long; the limit is {max}", max = TimerName::MAX_LEN)]
    TooLong {
        /// Its length in characters.
        chars: usize,
    },
    /// The name holds a control character, `§`, a text-direction mark or
    /// an invisible character.
    #[error("the timer's name holds a character it may not, at position {index}")]
    InvalidChar {
        /// The character's position, counted in characters.
        index: usize,
    },
}

/// The name a script gives a timer, which comes back in `on_timer(name)`.
///
/// It's the script's own text, 1 to [`TimerName::MAX_LEN`] characters. It
/// reaches the script's log and the app, so it holds nothing that could
/// reorder or hide a line there.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TimerName(String);

impl TimerName {
    /// The longest name, in characters.
    pub const MAX_LEN: usize = 32;

    /// Returns the name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for TimerName {
    type Error = TimerNameError;

    fn try_from(text: &str) -> Result<Self, TimerNameError> {
        let chars = text.chars().count();
        if chars == 0 {
            return Err(TimerNameError::Empty);
        }
        if chars > Self::MAX_LEN {
            return Err(TimerNameError::TooLong { chars });
        }
        match text.chars().position(is_forbidden_in_declaration) {
            Some(index) => Err(TimerNameError::InvalidChar { index }),
            None => Ok(Self(text.to_owned())),
        }
    }
}

impl fmt::Display for TimerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Something that happened, with what its hook is told.
#[derive(Debug, Clone, PartialEq)]
pub enum HookEvent {
    /// The bot is in the world after connecting.
    Join {
        /// Whether this is the first join since the bot was started;
        /// `false` after a reconnect.
        first: bool,
    },
    /// The server moved the bot to another world without a disconnect.
    WorldChange,
    /// The bot died.
    Death,
    /// The bot lives again after a death.
    Respawn,
    /// A timer the script set is due.
    Timer(TimerName),
    /// A declared chat pattern matched: its key and typed values.
    Chat(ChatMatch),
    /// The script stops in an orderly way.
    Stop,
    /// A screen opened.
    MenuOpen,
    /// The open screen's content changed.
    MenuUpdate,
    /// The screen closed.
    MenuClose,
    /// What became of an action the script sent onto a screen.
    Click(ClickOutcome),
    /// The server opened a sign editor.
    SignOpen,
}

/// What became of a click, a screen's button, a trade or an anvil's name a
/// script sent: what `on_click(result)` is told (ADR-0036, R1; ADR-0050).
///
/// Every action a hook was given `ok` for gets exactly one: the reason it
/// wasn't sent after all, from the guard at the moment of sending, or its
/// fate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClickOutcome {
    /// The slot it clicked; `None` for a button, a trade or a name.
    pub slot: Option<u16>,
    /// `Ok` for a fate of `ok`; otherwise why it wasn't sent, or what
    /// came instead of what was predicted.
    pub answer: Result<(), Refusal>,
}

impl ClickOutcome {
    /// An action that wasn't sent: the guard or the gate said `refusal` at
    /// the moment of sending.
    #[must_use]
    pub const fn refused(slot: Option<u16>, refusal: MenuRefusal) -> Self {
        Self {
            slot,
            answer: Err(Refusal::of_menu(refusal)),
        }
    }
}

impl From<ClickResult> for ClickOutcome {
    /// An action's fate.
    fn from(result: ClickResult) -> Self {
        Self {
            slot: result.slot,
            answer: match Refusal::of_fate(result.fate) {
                None => Ok(()),
                Some(reason) => Err(reason),
            },
        }
    }
}

impl HookEvent {
    /// The hook this event calls.
    #[must_use]
    pub const fn hook(&self) -> Hook {
        match self {
            Self::Join { .. } => Hook::Join,
            Self::WorldChange => Hook::WorldChange,
            Self::Death => Hook::Death,
            Self::Respawn => Hook::Respawn,
            Self::Timer(_) => Hook::Timer,
            Self::Chat(_) => Hook::Chat,
            Self::Stop => Hook::Stop,
            Self::MenuOpen => Hook::MenuOpen,
            Self::MenuUpdate => Hook::MenuUpdate,
            Self::MenuClose => Hook::MenuClose,
            Self::Click(_) => Hook::Click,
            Self::SignOpen => Hook::SignOpen,
        }
    }
}

/// Why an action a script asked for wasn't taken: `result.reason`, from a
/// closed list (ADR-0036, D4).
///
/// Every action returns a result a script can ignore; a refusal is never
/// an error that stops the script. A release is never refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// The action came faster than its limit allows.
    TooSoon,
    /// The bot is dead.
    Dead,
    /// The bot is between worlds, or its session is ending.
    NotInWorld,
    /// The bot is at the end of its leash, and this would take it further.
    Leashed,
    /// The block is below the bot's feet.
    MiningGuard,
    /// The held tool is nearly used up.
    ToolGuard,
    /// The block isn't one the mode lists for mining.
    BlockGuard,
    /// A number outside its range, an angle that isn't a number, an
    /// unknown kind.
    BadValue,
    /// No room for another timer.
    TooMany,
    /// `chat.expect` without a declared command sent before it in the same
    /// hook (ADR-0037, C4).
    NoCommand,
    /// A screen is open, and this needs none: a key, a mouse button, a
    /// look, the hotbar, a vehicle, or opening the inventory (ADR-0036, M8).
    ScreenOpen,
    /// No screen is open.
    NoMenu,
    /// Another screen is open than the one the hook saw.
    ScreenChanged,
    /// The screen's content changed since the hook saw it.
    ContentChanged,
    /// A slot the click touches holds something else now.
    SlotChanged,
    /// An earlier click's fate hasn't come yet (R1).
    ClickPending,
    /// No answer came for the screen's last click: the screen takes no
    /// click until it's opened again (R1).
    Unanswered,
    /// No sign editor is open.
    NoSign,
    /// Another sign editor is open than the one the hook saw.
    SignChanged,
    /// The sign editor was answered already.
    SignAnswered,
    /// The server corrected the screen after the click: it put things back
    /// or did otherwise than predicted (R1).
    Rejected,
    /// The server corrected the screen after a click on a result slot (a
    /// trade's, an anvil's, a craft's), or after a button, a trade or a
    /// name, which predict nothing: it may have done what was asked, once or
    /// more (ADR-0052).
    Unclear,
    /// The screen closed, or another opened, before the click's answer.
    Replaced,
}

impl Refusal {
    /// Every reason, in a fixed order.
    pub const ALL: [Self; 23] = [
        Self::TooSoon,
        Self::Dead,
        Self::NotInWorld,
        Self::Leashed,
        Self::MiningGuard,
        Self::ToolGuard,
        Self::BlockGuard,
        Self::BadValue,
        Self::TooMany,
        Self::NoCommand,
        Self::ScreenOpen,
        Self::NoMenu,
        Self::ScreenChanged,
        Self::ContentChanged,
        Self::SlotChanged,
        Self::ClickPending,
        Self::Unanswered,
        Self::NoSign,
        Self::SignChanged,
        Self::SignAnswered,
        Self::Rejected,
        Self::Unclear,
        Self::Replaced,
    ];

    /// A fate as a script reads it: `None` for `ok`, otherwise its reason.
    #[must_use]
    pub const fn of_fate(fate: ClickFate) -> Option<Self> {
        match fate {
            ClickFate::Ok => None,
            ClickFate::Rejected => Some(Self::Rejected),
            ClickFate::Replaced => Some(Self::Replaced),
            ClickFate::Unanswered => Some(Self::Unanswered),
            ClickFate::Unclear => Some(Self::Unclear),
        }
    }

    /// A screen's reason as a script reads it: [`From`], usable in a
    /// `const fn`.
    #[must_use]
    pub const fn of_menu(refusal: MenuRefusal) -> Self {
        match refusal {
            MenuRefusal::NoMenu => Self::NoMenu,
            MenuRefusal::ScreenChanged => Self::ScreenChanged,
            MenuRefusal::ContentChanged => Self::ContentChanged,
            MenuRefusal::SlotChanged => Self::SlotChanged,
            MenuRefusal::ClickPending => Self::ClickPending,
            MenuRefusal::Unanswered => Self::Unanswered,
            MenuRefusal::ScreenOpen => Self::ScreenOpen,
            MenuRefusal::BadValue => Self::BadValue,
        }
    }

    /// The reason as a script reads it in `result.reason`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TooSoon => "too_soon",
            Self::Dead => "dead",
            Self::NotInWorld => "not_in_world",
            Self::Leashed => "leashed",
            Self::MiningGuard => "mining_guard",
            Self::ToolGuard => "tool_guard",
            Self::BlockGuard => "block_guard",
            Self::BadValue => "bad_value",
            Self::TooMany => "too_many",
            Self::NoCommand => "no_command",
            Self::ScreenOpen => "screen_open",
            Self::NoMenu => "no_menu",
            Self::ScreenChanged => "screen_changed",
            Self::ContentChanged => "content_changed",
            Self::SlotChanged => "slot_changed",
            Self::ClickPending => "click_pending",
            Self::Unanswered => "unanswered",
            Self::NoSign => "no_sign",
            Self::SignChanged => "sign_changed",
            Self::SignAnswered => "sign_answered",
            Self::Rejected => "rejected",
            Self::Unclear => "unclear",
            Self::Replaced => "replaced",
        }
    }
}

impl From<MenuRefusal> for Refusal {
    /// The screen's reason as a script reads it: each the same text.
    fn from(refusal: MenuRefusal) -> Self {
        Self::of_menu(refusal)
    }
}

impl From<SignRefusal> for Refusal {
    /// The sign editor's reason as a script reads it: each the same text.
    fn from(refusal: SignRefusal) -> Self {
        match refusal {
            SignRefusal::NoSign => Self::NoSign,
            SignRefusal::SignChanged => Self::SignChanged,
            SignRefusal::Answered => Self::SignAnswered,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::Ident;
    use proptest::prelude::*;
    use rstest::rstest;

    #[rstest]
    #[case::join(Hook::Join, "on_join", &["first"], true)]
    #[case::world_change(Hook::WorldChange, "on_world_change", &[], true)]
    #[case::death(Hook::Death, "on_death", &[], true)]
    #[case::respawn(Hook::Respawn, "on_respawn", &[], true)]
    #[case::timer(Hook::Timer, "on_timer", &["name"], false)]
    #[case::chat(Hook::Chat, "on_chat", &["key", "values"], false)]
    #[case::stop(Hook::Stop, "on_stop", &[], false)]
    #[case::menu_open(Hook::MenuOpen, "on_menu_open", &[], false)]
    #[case::menu_update(Hook::MenuUpdate, "on_menu_update", &[], false)]
    #[case::menu_close(Hook::MenuClose, "on_menu_close", &[], false)]
    #[case::click(Hook::Click, "on_click", &["result"], false)]
    #[case::sign_open(Hook::SignOpen, "on_sign_open", &[], false)]
    fn each_hook_has_its_name_its_parameters_and_its_release(
        #[case] hook: Hook,
        #[case] name: &str,
        #[case] parameters: &[&str],
        #[case] releases_first: bool,
    ) {
        assert_eq!(hook.name(), name);
        assert_eq!(hook.to_string(), name);
        assert_eq!(hook.parameters(), parameters);
        assert_eq!(Hook::from_name(name), Some(hook));
        assert_eq!(hook.releases_first(), releases_first);
        assert!(name.starts_with(Hook::PREFIX));
    }

    #[test]
    fn every_hook_is_listed_once() {
        let mut names: Vec<&str> = Hook::ALL.iter().map(|hook| hook.name()).collect();
        names.sort_unstable();
        names.dedup();

        assert_eq!(names.len(), 12);
    }

    #[rstest]
    #[case::a_misspelled_hook("on_jion")]
    #[case::reconnect_isnt_a_hook("on_reconnect")]
    #[case::a_tick_isnt_a_hook("on_tick")]
    #[case::not_quite_phase_16s("on_menu_opened")]
    #[case::a_sign_closing_isnt_a_hook("on_sign_close")]
    #[case::another_case("On_Join")]
    #[case::without_the_prefix("join")]
    #[case::the_prefix_alone("on_")]
    #[case::nothing("")]
    fn no_other_name_is_a_hook(#[case] name: &str) {
        assert_eq!(Hook::from_name(name), None);
    }

    #[rstest]
    #[case::a_first_join(HookEvent::Join { first: true }, Hook::Join)]
    #[case::a_reconnect(HookEvent::Join { first: false }, Hook::Join)]
    #[case::a_world_change(HookEvent::WorldChange, Hook::WorldChange)]
    #[case::a_death(HookEvent::Death, Hook::Death)]
    #[case::a_respawn(HookEvent::Respawn, Hook::Respawn)]
    #[case::a_timer(HookEvent::Timer(TimerName::try_from("swing").unwrap()), Hook::Timer)]
    #[case::a_match(
        HookEvent::Chat(ChatMatch { key: Ident::try_from("balance").unwrap(), values: Vec::new() }),
        Hook::Chat
    )]
    #[case::a_stop(HookEvent::Stop, Hook::Stop)]
    #[case::a_menu_opening(HookEvent::MenuOpen, Hook::MenuOpen)]
    #[case::a_menu_update(HookEvent::MenuUpdate, Hook::MenuUpdate)]
    #[case::a_menu_closing(HookEvent::MenuClose, Hook::MenuClose)]
    #[case::a_click(
        HookEvent::Click(ClickOutcome { slot: Some(3), answer: Ok(()) }),
        Hook::Click
    )]
    #[case::a_sign(HookEvent::SignOpen, Hook::SignOpen)]
    fn each_event_calls_its_hook(#[case] event: HookEvent, #[case] hook: Hook) {
        assert_eq!(event.hook(), hook);
    }

    #[rstest]
    #[case::a_word("swing")]
    #[case::with_spaces_and_case("Get in!")]
    #[case::one_character("x")]
    #[case::an_umlaut("gr\u{fc}n")]
    #[case::the_longest("abcdefghijklmnopqrstuvwxyz012345")]
    fn a_timers_name_is_the_scripts_own_text(#[case] text: &str) {
        let name = TimerName::try_from(text).unwrap();

        assert_eq!(name.as_str(), text);
        assert_eq!(name.to_string(), text);
    }

    #[rstest]
    #[case::empty("", TimerNameError::Empty)]
    #[case::too_long("abcdefghijklmnopqrstuvwxyz0123456", TimerNameError::TooLong { chars: 33 })]
    #[case::a_line_break("a\nb", TimerNameError::InvalidChar { index: 1 })]
    #[case::a_section_sign("\u{a7}c", TimerNameError::InvalidChar { index: 0 })]
    #[case::a_direction_mark("ab\u{202e}", TimerNameError::InvalidChar { index: 2 })]
    #[case::a_zero_width_space("a\u{200b}", TimerNameError::InvalidChar { index: 1 })]
    fn a_timers_name_holds_nothing_that_hides_or_reorders(
        #[case] text: &str,
        #[case] error: TimerNameError,
    ) {
        assert_eq!(TimerName::try_from(text), Err(error));
    }

    #[rstest]
    #[case::too_soon(Refusal::TooSoon, "too_soon")]
    #[case::dead(Refusal::Dead, "dead")]
    #[case::not_in_world(Refusal::NotInWorld, "not_in_world")]
    #[case::leashed(Refusal::Leashed, "leashed")]
    #[case::mining_guard(Refusal::MiningGuard, "mining_guard")]
    #[case::tool_guard(Refusal::ToolGuard, "tool_guard")]
    #[case::block_guard(Refusal::BlockGuard, "block_guard")]
    #[case::bad_value(Refusal::BadValue, "bad_value")]
    #[case::too_many(Refusal::TooMany, "too_many")]
    #[case::no_command(Refusal::NoCommand, "no_command")]
    #[case::screen_open(Refusal::ScreenOpen, "screen_open")]
    #[case::no_menu(Refusal::NoMenu, "no_menu")]
    #[case::screen_changed(Refusal::ScreenChanged, "screen_changed")]
    #[case::content_changed(Refusal::ContentChanged, "content_changed")]
    #[case::slot_changed(Refusal::SlotChanged, "slot_changed")]
    #[case::click_pending(Refusal::ClickPending, "click_pending")]
    #[case::unanswered(Refusal::Unanswered, "unanswered")]
    #[case::no_sign(Refusal::NoSign, "no_sign")]
    #[case::sign_changed(Refusal::SignChanged, "sign_changed")]
    #[case::sign_answered(Refusal::SignAnswered, "sign_answered")]
    #[case::rejected(Refusal::Rejected, "rejected")]
    #[case::unclear(Refusal::Unclear, "unclear")]
    #[case::replaced(Refusal::Replaced, "replaced")]
    fn each_reason_reads_as_the_reference_names_it(#[case] reason: Refusal, #[case] text: &str) {
        assert_eq!(reason.as_str(), text);
        assert_eq!(reason.to_string(), text);
    }

    #[test]
    fn the_reasons_are_a_closed_list_of_twenty_three() {
        let mut texts: Vec<&str> = Refusal::ALL.iter().map(|reason| reason.as_str()).collect();
        texts.sort_unstable();
        texts.dedup();

        assert_eq!(texts.len(), 23);
        assert!(
            texts
                .iter()
                .all(|text| text.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
        );
    }

    #[test]
    fn phase_15s_hooks_are_level_1_and_the_menus_level_2() {
        for hook in Hook::ALL {
            let expected = if matches!(
                hook,
                Hook::MenuOpen | Hook::MenuUpdate | Hook::MenuClose | Hook::Click | Hook::SignOpen
            ) {
                ScriptApiLevel::V2
            } else {
                ScriptApiLevel::V1
            };

            assert_eq!(hook.level(), expected, "{hook}");
        }
    }

    #[test]
    fn every_reason_of_a_screen_and_a_sign_reads_the_same_as_a_scripts() {
        for refusal in MenuRefusal::ALL {
            assert_eq!(Refusal::from(refusal).as_str(), refusal.as_str());
        }
        for refusal in SignRefusal::ALL {
            assert_eq!(Refusal::from(refusal).as_str(), refusal.as_str());
        }
    }

    #[test]
    fn a_fate_of_ok_is_no_reason_and_every_other_fate_is_its_own() {
        for fate in ClickFate::ALL {
            match Refusal::of_fate(fate) {
                None => assert_eq!(fate, ClickFate::Ok),
                Some(reason) => assert_eq!(reason.as_str(), fate.as_str()),
            }
        }
        assert_eq!(Refusal::of_fate(ClickFate::Ok), None);
    }

    #[test]
    fn a_click_hears_its_fate_or_why_it_wasnt_sent_with_its_slot() {
        use crate::script::menu::{ScreenId, WindowId};
        let screen = crate::script::menu::ClickGate::new().opened(WindowId::new(3));
        let corrected = ClickResult {
            screen,
            slot: Some(12),
            fate: ClickFate::Rejected,
        };
        let taken = ClickResult {
            fate: ClickFate::Ok,
            ..corrected
        };
        let _: ScreenId = screen;

        assert_eq!(
            ClickOutcome::from(corrected),
            ClickOutcome {
                slot: Some(12),
                answer: Err(Refusal::Rejected)
            }
        );
        assert_eq!(
            ClickOutcome::from(taken),
            ClickOutcome {
                slot: Some(12),
                answer: Ok(())
            }
        );
        assert_eq!(
            ClickOutcome::refused(None, MenuRefusal::ScreenChanged),
            ClickOutcome {
                slot: None,
                answer: Err(Refusal::ScreenChanged)
            }
        );
    }

    #[test]
    fn the_errors_never_hold_the_text() {
        assert_eq!(
            TimerNameError::TooLong { chars: 40 }.to_string(),
            "the timer's name is 40 characters long; the limit is 32"
        );
        assert_eq!(
            TimerNameError::InvalidChar { index: 1 }.to_string(),
            "the timer's name holds a character it may not, at position 1"
        );
    }

    proptest! {
        #[test]
        fn a_timers_name_never_panics_and_is_bounded(text in any::<String>()) {
            if let Ok(name) = TimerName::try_from(text.as_str()) {
                prop_assert_eq!(name.as_str(), text.as_str());
                prop_assert!(text.chars().count() <= TimerName::MAX_LEN);
                prop_assert!(!text.chars().any(char::is_control));
            }
        }

        #[test]
        fn only_a_hooks_own_name_is_a_hook(name in "(on_)?[a-z_]{0,14}") {
            let found = Hook::from_name(&name);

            prop_assert_eq!(found.is_some(), Hook::ALL.iter().any(|hook| hook.name() == name));
        }
    }
}
