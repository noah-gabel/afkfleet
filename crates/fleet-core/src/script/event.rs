//! [`Hook`] and [`HookEvent`]: what a script is called for, and
//! [`Refusal`], why an action wasn't taken (ADR-0036, D4, D6).

use core::fmt;

use super::pattern::ChatMatch;
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
}

impl Hook {
    /// Every hook, in a fixed order.
    pub const ALL: [Self; 7] = [
        Self::Join,
        Self::WorldChange,
        Self::Death,
        Self::Respawn,
        Self::Timer,
        Self::Chat,
        Self::Stop,
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
        }
    }

    /// The function's parameters, as the reference names them.
    #[must_use]
    pub const fn parameters(self) -> &'static [&'static str] {
        match self {
            Self::Join => &["first"],
            Self::Timer => &["name"],
            Self::Chat => &["key", "values"],
            Self::WorldChange | Self::Death | Self::Respawn | Self::Stop => &[],
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
            Self::Timer | Self::Chat | Self::Stop => false,
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
}

impl Refusal {
    /// Every reason, in a fixed order.
    pub const ALL: [Self; 10] = [
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
    ];

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

        assert_eq!(names.len(), 7);
    }

    #[rstest]
    #[case::a_misspelled_hook("on_jion")]
    #[case::reconnect_isnt_a_hook("on_reconnect")]
    #[case::a_tick_isnt_a_hook("on_tick")]
    #[case::phase_16s("on_menu_open")]
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
    fn each_reason_reads_as_the_reference_names_it(#[case] reason: Refusal, #[case] text: &str) {
        assert_eq!(reason.as_str(), text);
        assert_eq!(reason.to_string(), text);
    }

    #[test]
    fn the_reasons_are_a_closed_list_of_ten() {
        let mut texts: Vec<&str> = Refusal::ALL.iter().map(|reason| reason.as_str()).collect();
        texts.sort_unstable();
        texts.dedup();

        assert_eq!(texts.len(), 10);
        assert!(
            texts
                .iter()
                .all(|text| text.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
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
