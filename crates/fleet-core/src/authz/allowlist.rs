//! [`CommandAllowlist`]: the `/commands` a bot may send with Control.

use std::collections::BTreeSet;

use crate::chat::{ChatMessage, ChatMessageError};
use crate::mode::ModeDefinition;

/// Why config entries can't form a [`CommandAllowlist`].
///
/// `index` is the entry's position in the list, counting from 0. The entry's
/// text is never included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CommandAllowlistError {
    /// The list has more than [`CommandAllowlist::MAX_ENTRIES`] entries.
    #[error("the allowlist has {count} entries; the limit is {max}", max = CommandAllowlist::MAX_ENTRIES)]
    TooMany {
        /// How many entries it has.
        count: usize,
    },
    /// The entry isn't valid chat text: it's empty, too long, or contains a
    /// forbidden character.
    #[error("allowlist entry {index}: {reason}")]
    Invalid {
        /// The entry.
        index: usize,
        /// Why it isn't a valid [`ChatMessage`].
        reason: ChatMessageError,
    },
    /// The entry doesn't start with `/`.
    #[error("allowlist entry {index} doesn't start with `/`")]
    MissingSlash {
        /// The entry.
        index: usize,
    },
    /// The entry has no command name after the `/`.
    #[error("allowlist entry {index} has no command name")]
    EmptyName {
        /// The entry.
        index: usize,
    },
    /// The entry has arguments after the command name. The allowlist holds
    /// names only, and an allowlisted command passes with any arguments.
    #[error("allowlist entry {index} has arguments; list only the command")]
    HasArguments {
        /// The entry.
        index: usize,
    },
}

/// The `/commands` a bot may send with Control. Every other command needs
/// Manage (Plan.md §7.3).
///
/// It's built from the `chat.command_allowlist` config entries, which are
/// written the way they're typed in chat: `"/spawn"`. Each entry is one
/// command name without arguments, and an allowlisted command passes with any
/// arguments. Names are compared exactly and case-sensitively with
/// [`ChatMessage::command_name`], so the check fails closed: `/Spawn`,
/// `/minecraft:spawn` and `/spawn` followed by an invisible character aren't
/// `/spawn`, and need Manage (ADR-0010).
///
/// The default allowlist is empty, so every command needs Manage.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandAllowlist {
    names: BTreeSet<String>,
}

impl CommandAllowlist {
    /// The most entries an allowlist may have.
    pub const MAX_ENTRIES: usize = 64;

    /// Builds an allowlist from config entries such as `"/spawn"`.
    ///
    /// Surrounding whitespace is trimmed, and duplicates count once.
    ///
    /// # Errors
    /// - [`CommandAllowlistError::TooMany`] for more than
    ///   [`CommandAllowlist::MAX_ENTRIES`] entries.
    /// - [`CommandAllowlistError::Invalid`] for an entry that isn't a valid
    ///   [`ChatMessage`].
    /// - [`CommandAllowlistError::MissingSlash`],
    ///   [`CommandAllowlistError::EmptyName`] or
    ///   [`CommandAllowlistError::HasArguments`] for an entry that isn't `/`
    ///   followed by a command name.
    ///
    /// The first invalid entry decides the error.
    pub fn try_new<S: AsRef<str>>(entries: &[S]) -> Result<Self, CommandAllowlistError> {
        if entries.len() > Self::MAX_ENTRIES {
            return Err(CommandAllowlistError::TooMany {
                count: entries.len(),
            });
        }
        let names = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| command_name(index, entry.as_ref()))
            .collect::<Result<_, _>>()?;
        Ok(Self { names })
    }

    /// Checks a chat message: only a command off the allowlist needs Manage.
    /// Plain chat doesn't.
    #[must_use]
    pub fn check(&self, message: &ChatMessage) -> CommandCheck {
        CommandCheck {
            unlisted_command: message
                .command_name()
                .is_some_and(|name| !self.names.contains(name)),
        }
    }

    /// Checks a mode a bot is to run: it needs Manage if any of its chat
    /// steps sends a command off the allowlist (ADR-0010).
    #[must_use]
    pub fn check_mode(&self, mode: &ModeDefinition) -> CommandCheck {
        CommandCheck {
            unlisted_command: mode
                .commands()
                .any(|command| self.check(command).unlisted_command),
        }
    }
}

/// Parses one config entry, the entry at `index`, into the command name it
/// allows.
fn command_name(index: usize, entry: &str) -> Result<String, CommandAllowlistError> {
    let message = ChatMessage::try_from(entry)
        .map_err(|reason| CommandAllowlistError::Invalid { index, reason })?;
    let name = message
        .command_name()
        .ok_or(CommandAllowlistError::MissingSlash { index })?;
    if name.is_empty() {
        return Err(CommandAllowlistError::EmptyName { index });
    }
    // The name runs up to the first whitespace, so anything left after the
    // `/` and the name is an argument.
    if message.as_str().len() != '/'.len_utf8() + name.len() {
        return Err(CommandAllowlistError::HasArguments { index });
    }
    Ok(name.to_owned())
}

/// The result of checking a chat message or a mode against the
/// [`CommandAllowlist`], for [`Permission::SendChat`] and
/// [`Permission::SetBotMode`].
///
/// Only the allowlist creates one, so a caller can't claim that a command is
/// allowlisted without checking it. It holds no chat text, so its `Debug`
/// output is safe to log.
///
/// [`Permission::SendChat`]: super::Permission::SendChat
/// [`Permission::SetBotMode`]: super::Permission::SetBotMode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommandCheck {
    unlisted_command: bool,
}

impl CommandCheck {
    /// Whether a command off the allowlist was found, so the action needs
    /// Manage.
    #[must_use]
    pub const fn needs_manage(self) -> bool {
        self.unlisted_command
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::{Action, ModeDraft, Schedule, Step};
    use core::time::Duration;
    use proptest::prelude::*;
    use rstest::rstest;

    fn allowlist(entries: &[&str]) -> CommandAllowlist {
        CommandAllowlist::try_new(entries).unwrap()
    }

    fn message(text: &str) -> ChatMessage {
        ChatMessage::try_from(text).unwrap()
    }

    fn chat_step(text: &str, schedule: Schedule) -> Step {
        Step {
            action: Action::SendChat {
                message: message(text),
            },
            schedule,
            probability: 100,
        }
    }

    /// A mode that sends `at_start` once and `repeating` every minute.
    fn chat_mode(at_start: &str, repeating: &str) -> ModeDefinition {
        let every_minute = Schedule::Every {
            interval: Duration::from_secs(60),
            jitter: Duration::ZERO,
        };
        ModeDraft {
            steps: vec![
                chat_step(at_start, Schedule::AtStart),
                chat_step(repeating, every_minute),
            ],
        }
        .validate()
        .unwrap()
    }

    #[rstest]
    #[case::plain_chat("hello there", false)]
    #[case::listed("/spawn", false)]
    #[case::listed_with_arguments("/spawn home", false)]
    #[case::second_entry("/home", false)]
    #[case::other_command("/op AfkBot1", true)]
    #[case::wrong_case("/Spawn", true)]
    #[case::namespaced("/minecraft:spawn", true)]
    #[case::zero_width_space("/spawn\u{200B}", true)]
    #[case::lone_slash("/", true)]
    #[case::space_after_slash("/ spawn", true)]
    #[case::double_slash("//spawn", true)]
    fn check_compares_command_names_exactly(#[case] text: &str, #[case] needs_manage: bool) {
        let check = allowlist(&["/spawn", "/home"]).check(&message(text));

        assert_eq!(check.needs_manage(), needs_manage);
    }

    #[rstest]
    #[case::namespaced("/minecraft:spawn")]
    #[case::worldedit("//wand")]
    #[case::trimmed("  /afk ")]
    fn accepts_any_command_name(#[case] entry: &str) {
        let check = allowlist(&[entry]).check(&message(entry));

        assert!(!check.needs_manage());
    }

    #[test]
    fn duplicates_count_once() {
        assert_eq!(
            allowlist(&["/spawn", "/spawn", " /spawn"]),
            allowlist(&["/spawn"])
        );
    }

    #[rstest]
    #[case::command("/spawn", true)]
    #[case::plain_chat("hello", false)]
    fn the_default_allowlist_lets_no_command_through(#[case] text: &str, #[case] needs: bool) {
        let check = CommandAllowlist::default().check(&message(text));

        assert_eq!(check.needs_manage(), needs);
    }

    #[rstest]
    #[case::missing_slash(&["/spawn", "home"], CommandAllowlistError::MissingSlash { index: 1 })]
    #[case::lone_slash(&["/"], CommandAllowlistError::EmptyName { index: 0 })]
    #[case::space_after_slash(&["/ spawn"], CommandAllowlistError::EmptyName { index: 0 })]
    #[case::arguments(&["/afk", "/home set"], CommandAllowlistError::HasArguments { index: 1 })]
    #[case::non_breaking_space(&["/home\u{A0}set"], CommandAllowlistError::HasArguments { index: 0 })]
    #[case::blank(&["/spawn", "   "], CommandAllowlistError::Invalid { index: 1, reason: ChatMessageError::Empty })]
    #[case::control_char(
        &["/sp\u{7}awn"],
        CommandAllowlistError::Invalid { index: 0, reason: ChatMessageError::ForbiddenChar { index: 3 } }
    )]
    #[case::first_error_wins(&["home", "/"], CommandAllowlistError::MissingSlash { index: 0 })]
    fn rejects_invalid_entries_by_position(
        #[case] entries: &[&str],
        #[case] error: CommandAllowlistError,
    ) {
        assert_eq!(CommandAllowlist::try_new(entries), Err(error));
    }

    #[test]
    fn rejects_an_entry_longer_than_a_chat_message() {
        let entry = format!("/{}", "x".repeat(ChatMessage::MAX_LEN));

        assert_eq!(
            CommandAllowlist::try_new(&[entry]),
            Err(CommandAllowlistError::Invalid {
                index: 0,
                reason: ChatMessageError::TooLong { units: 257 }
            })
        );
    }

    #[test]
    fn accepts_64_entries_and_rejects_65() {
        let entries: Vec<String> = (0..=CommandAllowlist::MAX_ENTRIES)
            .map(|n| format!("/cmd{n}"))
            .collect();

        assert!(CommandAllowlist::try_new(&entries[..64]).is_ok());
        assert_eq!(
            CommandAllowlist::try_new(&entries),
            Err(CommandAllowlistError::TooMany { count: 65 })
        );
    }

    #[rstest]
    #[case::afk(ModeDefinition::afk(), false)]
    #[case::farm(ModeDefinition::farm(), false)]
    #[case::listed_commands(chat_mode("/spawn", "/afk"), false)]
    #[case::plain_chat(chat_mode("hi all", "still here"), false)]
    #[case::unlisted_at_start(chat_mode("/op AfkBot1", "/afk"), true)]
    #[case::unlisted_repeating(chat_mode("/spawn", "/home"), true)]
    fn check_mode_needs_manage_for_any_unlisted_command(
        #[case] mode: ModeDefinition,
        #[case] needs_manage: bool,
    ) {
        let check = allowlist(&["/spawn", "/afk"]).check_mode(&mode);

        assert_eq!(check.needs_manage(), needs_manage);
    }

    proptest! {
        #[test]
        fn try_new_never_panics(entries in proptest::collection::vec(any::<String>(), 0..70)) {
            let _ = CommandAllowlist::try_new(&entries);
        }

        #[test]
        fn only_an_exact_entry_lets_a_command_through(
            name in "[a-zA-Z:/]{0,8}",
            arguments in "( [a-z]{1,5}){0,2}",
        ) {
            let check = allowlist(&["/spawn", "/home", "/minecraft:tp"])
                .check(&message(&format!("/{name}{arguments}")));

            let listed = ["spawn", "home", "minecraft:tp"].contains(&name.as_str());
            prop_assert_eq!(check.needs_manage(), !listed);
        }

        #[test]
        fn plain_chat_never_needs_manage(text in "[a-z][a-z /]{0,30}") {
            let check = allowlist(&["/spawn"]).check(&message(&text));

            prop_assert!(!check.needs_manage());
        }
    }
}
