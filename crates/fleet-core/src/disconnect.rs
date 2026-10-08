//! Why a Minecraft session ended, and what that means for the bot.
//!
//! The adapter reports a [`DisconnectReason`]; [`DisconnectReason::classify`]
//! decides whether the bot retries, stops for good, or pauses because a human
//! logged in (ADR-0008 §5, §6 and §9; ADR-0010). Kick reasons are classified by
//! their translation key, never by their text: the text depends on the server's
//! language and plugins. The session server's answers to a join come from the
//! account's `join()` hook and are split by what they mean for the account
//! (ADR-0011).
//!
//! Some proxies report a duplicate login as plain text. A bot's
//! [`ConflictTexts`] list those texts, and [`ConflictTexts::classify`] applies
//! them on top of [`DisconnectReason::classify`] (ADR-0013).

use std::collections::BTreeSet;

use crate::text::{self, LineBreaks};

/// A kick's translation key, e.g. `multiplayer.disconnect.banned`.
///
/// Keys are 1–128 characters of `a`–`z`, `0`–`9`, `.`, `_` and `-`. A server
/// can send anything, so a reason with any other key counts as having none.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TranslationKey(String);

impl TranslationKey {
    /// The longest key accepted.
    pub const MAX_LEN: usize = 128;

    /// Returns the key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Accepts `raw` if it looks like a translation key.
    fn parse(raw: &str) -> Option<Self> {
        let valid = (1..=Self::MAX_LEN).contains(&raw.len())
            && raw.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
            });
        valid.then(|| Self(raw.to_owned()))
    }
}

/// A kick sent by the server.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Kick {
    key: Option<TranslationKey>,
    message: String,
}

impl Kick {
    /// The longest kick message kept, in characters.
    pub const MAX_MESSAGE_LEN: usize = 1024;

    /// Returns the translation key, if the reason had a valid one. A plain text
    /// reason (a custom kick message, or one from a plugin) has none.
    #[must_use]
    pub const fn key(&self) -> Option<&TranslationKey> {
        self.key.as_ref()
    }

    /// Returns the sanitized kick message, for display only.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Why connecting failed before the bot reached the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectFailure {
    /// The server refused the connection.
    Refused,
    /// Connecting timed out.
    TimedOut,
    /// The host name couldn't be resolved.
    Unresolvable,
    /// No session could be started locally, e.g. because too many host threads
    /// hung (ADR-0008 §5).
    HostUnavailable,
    /// Any other network error.
    Other,
}

/// Why the session server refused the account for good (ADR-0011). Retrying
/// can't change it; only the account's owner can.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountRestriction {
    /// Mojang banned the account from multiplayer.
    Banned,
    /// The account's settings don't allow multiplayer, e.g. parental controls.
    MultiplayerDisabled,
}

/// Why the session server couldn't confirm a join this time (ADR-0011).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionServerFailure {
    /// It couldn't be reached, or it said it's unavailable.
    Unreachable,
    /// It turned the join down for now: too many requests.
    RateLimited,
    /// It didn't answer within the adapter's session-join timeout.
    TimedOut,
    /// It answered with something the adapter doesn't know.
    Unexpected,
}

/// Why a Minecraft session ended.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DisconnectReason {
    /// The server kicked the bot.
    Kicked(Kick),
    /// The connection closed without a reason.
    ConnectionClosed,
    /// Connecting failed.
    ConnectFailed {
        /// What went wrong.
        failure: ConnectFailure,
    },
    /// The session server rejected the bot's Minecraft token. The adapter learns
    /// this from the account's `join()` hook, since azalea raises no event for it
    /// (ADR-0008 §9).
    AuthRejected,
    /// The session server refused the account for a reason that won't heal.
    /// Like [`AuthRejected`](Self::AuthRejected), the adapter learns this from
    /// the account's `join()` hook (ADR-0011).
    AccountRestricted {
        /// Why.
        restriction: AccountRestriction,
    },
    /// The session server couldn't confirm the join this time: an outage, an
    /// HTTP error, rate limiting, an answer the adapter doesn't know, or the
    /// adapter's own timeout. The adapter learns this from the account's
    /// `join()` hook (ADR-0011).
    SessionServerFailed {
        /// What went wrong.
        failure: SessionServerFailure,
    },
    /// The session's ECS runner died, e.g. after a panic (ADR-0008 §5).
    SessionCrashed,
    /// No game tick for the watchdog timeout: the session hung.
    WatchdogTimeout,
    /// No packet from the server for the liveness timeout: the server froze or
    /// the link died.
    LivenessTimeout,
}

/// What a disconnect means for the bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisconnectClass {
    /// Worth retrying after a backoff.
    Transient,
    /// Retrying won't help until something changes; the user has to reset the bot.
    Permanent {
        /// Why.
        kind: PermanentKind,
    },
    /// Someone else logged in to the account. The bot must not fight them.
    Conflict {
        /// Why.
        kind: ConflictKind,
    },
    /// The Minecraft session isn't valid.
    AuthInvalid,
}

/// Why a disconnect is permanent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermanentKind {
    /// The server banned the account or its IP address.
    Banned,
    /// The account isn't on the whitelist.
    NotWhitelisted,
    /// The server runs a different Minecraft version.
    WrongVersion,
    /// Mojang banned the account from multiplayer, on every server. The
    /// session server said so (ADR-0011).
    AccountBanned,
    /// The account's settings don't allow multiplayer. The session server
    /// said so (ADR-0011).
    MultiplayerDisabled,
}

/// Why a disconnect is a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConflictKind {
    /// The account logged in from somewhere else: a human is playing.
    DuplicateLogin,
}

impl DisconnectReason {
    /// Builds a kick reason from what the server sent. The key is checked and the
    /// message sanitized, since both come from the server.
    #[must_use]
    pub fn kicked(raw_key: Option<&str>, raw_message: &str) -> Self {
        Self::Kicked(Kick {
            key: raw_key.and_then(TranslationKey::parse),
            message: text::sanitize(raw_message, Kick::MAX_MESSAGE_LEN, LineBreaks::Keep).text,
        })
    }

    /// Classifies the reason (ADR-0008 §6, ADR-0011).
    ///
    /// Kicks are classified by translation key; a kick without a known key
    /// (plain text, or a key this version doesn't know) is transient. A
    /// rejected session is [`DisconnectClass::AuthInvalid`], and an account
    /// the session server restricts is permanent. Every other reason is
    /// transient, including a session server that couldn't confirm the join.
    #[must_use]
    pub fn classify(&self) -> DisconnectClass {
        match self {
            Self::Kicked(kick) => kick
                .key
                .as_ref()
                .map_or(DisconnectClass::Transient, |key| classify_key(key.as_str())),
            Self::AuthRejected => DisconnectClass::AuthInvalid,
            Self::AccountRestricted { restriction } => DisconnectClass::Permanent {
                kind: match restriction {
                    AccountRestriction::Banned => PermanentKind::AccountBanned,
                    AccountRestriction::MultiplayerDisabled => PermanentKind::MultiplayerDisabled,
                },
            },
            Self::ConnectionClosed
            | Self::ConnectFailed { .. }
            | Self::SessionServerFailed { .. }
            | Self::SessionCrashed
            | Self::WatchdogTimeout
            | Self::LivenessTimeout => DisconnectClass::Transient,
        }
    }
}

/// Why config entries can't form [`ConflictTexts`].
///
/// `index` is the entry's position in the list, counting from 0. The entry's
/// text is never included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConflictTextsError {
    /// The list has more than [`ConflictTexts::MAX_ENTRIES`] entries.
    #[error("the list has {count} conflict texts; the limit is {max}", max = ConflictTexts::MAX_ENTRIES)]
    TooMany {
        /// How many entries it has.
        count: usize,
    },
    /// The entry is empty.
    #[error("conflict text {index} is empty")]
    Empty {
        /// The entry's position, from 0.
        index: usize,
    },
    /// The entry contains a character that a kick message never has: `§`, a
    /// control character other than `\n`, a bidirectional control or an
    /// invisible character. Kick messages are sanitized, so the entry could
    /// never match.
    #[error(
        "conflict text {index} contains a character kick messages never have, at position {position}"
    )]
    ForbiddenChar {
        /// The entry's position, from 0.
        index: usize,
        /// The character's position in the entry, counted in characters.
        position: usize,
    },
    /// The entry is longer than [`ConflictTexts::MAX_LEN`] characters.
    #[error("conflict text {index} is {chars} characters long; the limit is {max}", max = ConflictTexts::MAX_LEN)]
    TooLong {
        /// The entry's position, from 0.
        index: usize,
        /// The entry's length in characters.
        chars: usize,
    },
}

/// Kick messages that count as a duplicate login: someone else logged in to
/// the bot's account (Plan.md P4.1; ADR-0010, ADR-0013).
///
/// Vanilla servers send a duplicate login with its translation key, which
/// [`DisconnectReason::classify`] knows. Some proxies kick with plain text
/// instead (`BungeeCord` and Waterfall in online mode probably do), so a bot's
/// spec can list those texts. [`ConflictTexts::classify`] then compares a
/// kick's sanitized message against them, exactly and case-sensitively, but
/// only when [`DisconnectReason::classify`] calls the kick transient: a key
/// that means something else always wins.
///
/// The default list is empty, which changes nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConflictTexts {
    texts: BTreeSet<String>,
}

impl ConflictTexts {
    /// The most entries a list may have.
    pub const MAX_ENTRIES: usize = 16;

    /// The longest entry, in characters: the longest kick message kept.
    pub const MAX_LEN: usize = Kick::MAX_MESSAGE_LEN;

    /// Builds the list from config entries.
    ///
    /// Entries are compared exactly, so they're taken as written, without
    /// trimming. Duplicates count once.
    ///
    /// # Errors
    /// - [`ConflictTextsError::TooMany`] for more than
    ///   [`ConflictTexts::MAX_ENTRIES`] entries.
    /// - [`ConflictTextsError::Empty`] for an empty entry.
    /// - [`ConflictTextsError::ForbiddenChar`] for an entry with a character
    ///   that sanitizing would strip.
    /// - [`ConflictTextsError::TooLong`] for an entry longer than
    ///   [`ConflictTexts::MAX_LEN`] characters.
    ///
    /// The first invalid entry decides the error.
    pub fn try_new<S: AsRef<str>>(entries: &[S]) -> Result<Self, ConflictTextsError> {
        if entries.len() > Self::MAX_ENTRIES {
            return Err(ConflictTextsError::TooMany {
                count: entries.len(),
            });
        }
        let texts = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| conflict_text(index, entry.as_ref()))
            .collect::<Result<_, _>>()?;
        Ok(Self { texts })
    }

    /// Whether the list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.texts.is_empty()
    }

    /// Returns the texts, in sorted order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.texts.iter().map(String::as_str)
    }

    /// Classifies `reason` like [`DisconnectReason::classify`], except that a
    /// kick it calls transient whose message is on this list is a duplicate
    /// login.
    #[must_use]
    pub fn classify(&self, reason: &DisconnectReason) -> DisconnectClass {
        match (reason.classify(), reason) {
            (DisconnectClass::Transient, DisconnectReason::Kicked(kick))
                if self.texts.contains(kick.message()) =>
            {
                DisconnectClass::Conflict {
                    kind: ConflictKind::DuplicateLogin,
                }
            }
            (class, _) => class,
        }
    }
}

/// Checks one config entry of [`ConflictTexts`] at `index`.
fn conflict_text(index: usize, entry: &str) -> Result<String, ConflictTextsError> {
    if entry.is_empty() {
        return Err(ConflictTextsError::Empty { index });
    }
    if let Some(position) = text::first_stripped(entry, LineBreaks::Keep) {
        return Err(ConflictTextsError::ForbiddenChar { index, position });
    }
    let chars = entry.chars().count();
    if chars > ConflictTexts::MAX_LEN {
        return Err(ConflictTextsError::TooLong { index, chars });
    }
    Ok(entry.to_owned())
}

/// The kick keys of ADR-0008 §6. Every other key is transient.
fn classify_key(key: &str) -> DisconnectClass {
    match key {
        "multiplayer.disconnect.banned"
        | "multiplayer.disconnect.banned.reason"
        | "multiplayer.disconnect.ip_banned"
        | "multiplayer.disconnect.banned_ip.reason" => DisconnectClass::Permanent {
            kind: PermanentKind::Banned,
        },
        "multiplayer.disconnect.not_whitelisted" => DisconnectClass::Permanent {
            kind: PermanentKind::NotWhitelisted,
        },
        "multiplayer.disconnect.incompatible" => DisconnectClass::Permanent {
            kind: PermanentKind::WrongVersion,
        },
        // Sent to the session that was already online: a human logging in kicks
        // the bot, and a bot reconnecting would kick the human.
        "multiplayer.disconnect.duplicate_login" => DisconnectClass::Conflict {
            kind: ConflictKind::DuplicateLogin,
        },
        "multiplayer.disconnect.unverified_username" => DisconnectClass::AuthInvalid,
        // Including slow_login, server_full, server_shutdown, kicked and idling.
        _ => DisconnectClass::Transient,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    const BANNED: DisconnectClass = DisconnectClass::Permanent {
        kind: PermanentKind::Banned,
    };
    const NOT_WHITELISTED: DisconnectClass = DisconnectClass::Permanent {
        kind: PermanentKind::NotWhitelisted,
    };
    const WRONG_VERSION: DisconnectClass = DisconnectClass::Permanent {
        kind: PermanentKind::WrongVersion,
    };
    const DUPLICATE_LOGIN: DisconnectClass = DisconnectClass::Conflict {
        kind: ConflictKind::DuplicateLogin,
    };
    const ACCOUNT_BANNED: DisconnectClass = DisconnectClass::Permanent {
        kind: PermanentKind::AccountBanned,
    };
    const MULTIPLAYER_DISABLED: DisconnectClass = DisconnectClass::Permanent {
        kind: PermanentKind::MultiplayerDisabled,
    };

    /// The kick rows of ADR-0008 §6, with the keys and texts the spike saw
    /// (spikes/azalea/FINDINGS.md, P1.3 and P1.8).
    #[rstest]
    #[case::banned(
        "multiplayer.disconnect.banned",
        "You are banned from this server",
        BANNED
    )]
    #[case::banned_with_reason(
        "multiplayer.disconnect.banned.reason",
        "You are banned from this server.\nReason: Banned by an operator.",
        BANNED
    )]
    #[case::ip_banned(
        "multiplayer.disconnect.ip_banned",
        "You have been IP banned from this server",
        BANNED
    )]
    #[case::ip_banned_with_reason(
        "multiplayer.disconnect.banned_ip.reason",
        "Your IP address is banned from this server.\nReason: Banned by an operator.",
        BANNED
    )]
    #[case::not_whitelisted(
        "multiplayer.disconnect.not_whitelisted",
        "You are not white-listed on this server!",
        NOT_WHITELISTED
    )]
    #[case::newer_server(
        "multiplayer.disconnect.incompatible",
        "Incompatible client! Please use 26.3",
        WRONG_VERSION
    )]
    #[case::older_server(
        "multiplayer.disconnect.incompatible",
        "Incompatible client! Please use 1.21.11",
        WRONG_VERSION
    )]
    #[case::duplicate_login(
        "multiplayer.disconnect.duplicate_login",
        "You logged in from another location",
        DUPLICATE_LOGIN
    )]
    #[case::unverified_username(
        "multiplayer.disconnect.unverified_username",
        "Failed to verify username!",
        DisconnectClass::AuthInvalid
    )]
    #[case::slow_login(
        "multiplayer.disconnect.slow_login",
        "Took too long to log in",
        DisconnectClass::Transient
    )]
    #[case::server_full(
        "multiplayer.disconnect.server_full",
        "The server is full!",
        DisconnectClass::Transient
    )]
    #[case::server_shutdown(
        "multiplayer.disconnect.server_shutdown",
        "Server closed",
        DisconnectClass::Transient
    )]
    #[case::kicked(
        "multiplayer.disconnect.kicked",
        "Kicked by an operator",
        DisconnectClass::Transient
    )]
    #[case::idling(
        "multiplayer.disconnect.idling",
        "You have been idle for too long!",
        DisconnectClass::Transient
    )]
    fn classifies_kicks_by_key(
        #[case] key: &str,
        #[case] message: &str,
        #[case] expected: DisconnectClass,
    ) {
        let reason = DisconnectReason::kicked(Some(key), message);

        assert_eq!(reason.classify(), expected);
        let DisconnectReason::Kicked(kick) = reason else {
            panic!("expected a kick");
        };
        assert_eq!(kick.key().map(TranslationKey::as_str), Some(key));
        assert_eq!(kick.message(), message);
    }

    #[rstest]
    #[case::connection_closed(DisconnectReason::ConnectionClosed, DisconnectClass::Transient)]
    #[case::refused(
        DisconnectReason::ConnectFailed { failure: ConnectFailure::Refused },
        DisconnectClass::Transient
    )]
    #[case::timed_out(
        DisconnectReason::ConnectFailed { failure: ConnectFailure::TimedOut },
        DisconnectClass::Transient
    )]
    #[case::unresolvable(
        DisconnectReason::ConnectFailed { failure: ConnectFailure::Unresolvable },
        DisconnectClass::Transient
    )]
    #[case::other_network_error(
        DisconnectReason::ConnectFailed { failure: ConnectFailure::Other },
        DisconnectClass::Transient
    )]
    #[case::host_unavailable(
        DisconnectReason::ConnectFailed { failure: ConnectFailure::HostUnavailable },
        DisconnectClass::Transient
    )]
    #[case::auth_rejected(DisconnectReason::AuthRejected, DisconnectClass::AuthInvalid)]
    #[case::account_banned(
        DisconnectReason::AccountRestricted { restriction: AccountRestriction::Banned },
        ACCOUNT_BANNED
    )]
    #[case::multiplayer_disabled(
        DisconnectReason::AccountRestricted {
            restriction: AccountRestriction::MultiplayerDisabled,
        },
        MULTIPLAYER_DISABLED
    )]
    #[case::session_server_unreachable(
        DisconnectReason::SessionServerFailed { failure: SessionServerFailure::Unreachable },
        DisconnectClass::Transient
    )]
    #[case::session_server_rate_limited(
        DisconnectReason::SessionServerFailed { failure: SessionServerFailure::RateLimited },
        DisconnectClass::Transient
    )]
    #[case::session_server_timed_out(
        DisconnectReason::SessionServerFailed { failure: SessionServerFailure::TimedOut },
        DisconnectClass::Transient
    )]
    #[case::session_server_unexpected(
        DisconnectReason::SessionServerFailed { failure: SessionServerFailure::Unexpected },
        DisconnectClass::Transient
    )]
    #[case::session_crashed(DisconnectReason::SessionCrashed, DisconnectClass::Transient)]
    #[case::watchdog_timeout(DisconnectReason::WatchdogTimeout, DisconnectClass::Transient)]
    #[case::liveness_timeout(DisconnectReason::LivenessTimeout, DisconnectClass::Transient)]
    fn classifies_other_reasons(
        #[case] reason: DisconnectReason,
        #[case] expected: DisconnectClass,
    ) {
        assert_eq!(reason.classify(), expected);
    }

    #[rstest]
    #[case::plain_text_reason(None)]
    #[case::unknown_key(Some("multiplayer.disconnect.something_new"))]
    #[case::uppercase_key(Some("MULTIPLAYER.DISCONNECT.BANNED"))]
    #[case::key_with_a_space(Some("multiplayer.disconnect.banned "))]
    #[case::empty_key(Some(""))]
    fn kicks_without_a_known_key_are_transient(#[case] key: Option<&str>) {
        let reason = DisconnectReason::kicked(key, "You are banned from this server");
        assert_eq!(reason.classify(), DisconnectClass::Transient);
    }

    #[rstest]
    #[case::uppercase("MULTIPLAYER.DISCONNECT.BANNED")]
    #[case::control_char("multiplayer.disconnect.banned\n")]
    #[case::empty("")]
    #[case::too_long(&"a".repeat(129))]
    fn invalid_keys_are_dropped(#[case] key: &str) {
        let DisconnectReason::Kicked(kick) = DisconnectReason::kicked(Some(key), "bye") else {
            panic!("expected a kick");
        };
        assert_eq!(kick.key(), None);
    }

    #[test]
    fn accepts_a_key_of_128_chars() {
        let key = "a".repeat(128);
        let DisconnectReason::Kicked(kick) = DisconnectReason::kicked(Some(&key), "bye") else {
            panic!("expected a kick");
        };
        assert_eq!(kick.key().map(TranslationKey::as_str), Some(key.as_str()));
    }

    #[test]
    fn kick_messages_are_sanitized_and_capped() {
        let raw = format!("§cBanned\u{202E}\u{200B}\n{}", "x".repeat(2000));
        let DisconnectReason::Kicked(kick) = DisconnectReason::kicked(None, &raw) else {
            panic!("expected a kick");
        };

        assert!(kick.message().starts_with("Banned\nxxx"));
        assert_eq!(kick.message().chars().count(), Kick::MAX_MESSAGE_LEN);
    }

    proptest! {
        #[test]
        fn never_panics(key in proptest::option::of(any::<String>()), message in any::<String>()) {
            let reason = DisconnectReason::kicked(key.as_deref(), &message);
            let _ = reason.classify();
        }
    }

    // Conflict texts.

    /// A proxy's plain-text duplicate-login kick (a placeholder text).
    const PROXY_TEXT: &str = "You are already connected to this proxy!";

    fn texts(entries: &[&str]) -> ConflictTexts {
        ConflictTexts::try_new(entries).unwrap()
    }

    #[test]
    fn an_empty_list_is_the_default() {
        let list = ConflictTexts::try_new::<&str>(&[]).unwrap();

        assert!(list.is_empty());
        assert_eq!(list, ConflictTexts::default());
    }

    #[test]
    fn keeps_entries_exactly_as_written() {
        let list = texts(&[" Spaced out ", "Line one\nline two", "Ünïcödé 😀"]);

        assert_eq!(
            list.iter().collect::<Vec<_>>(),
            [" Spaced out ", "Line one\nline two", "Ünïcödé 😀"]
        );
    }

    #[test]
    fn duplicates_count_once() {
        let list = texts(&[PROXY_TEXT, PROXY_TEXT]);

        assert_eq!(list.iter().collect::<Vec<_>>(), [PROXY_TEXT]);
    }

    #[test]
    fn accepts_the_most_entries_and_the_longest_entry() {
        let entries: Vec<String> = (0..ConflictTexts::MAX_ENTRIES)
            .map(|i| format!("{i}{}", "x".repeat(ConflictTexts::MAX_LEN - 2)))
            .collect();

        let list = ConflictTexts::try_new(&entries).unwrap();

        assert_eq!(list.iter().count(), ConflictTexts::MAX_ENTRIES);
    }

    #[test]
    fn rejects_too_many_entries() {
        let entries = vec!["text"; ConflictTexts::MAX_ENTRIES + 1];

        assert_eq!(
            ConflictTexts::try_new(&entries),
            Err(ConflictTextsError::TooMany { count: 17 })
        );
    }

    #[rstest]
    #[case::empty("", ConflictTextsError::Empty { index: 1 })]
    #[case::section_sign("§cKicked", ConflictTextsError::ForbiddenChar { index: 1, position: 0 })]
    #[case::control_char("Kicked\t!", ConflictTextsError::ForbiddenChar { index: 1, position: 6 })]
    #[case::carriage_return("Line\r\nbreak", ConflictTextsError::ForbiddenChar { index: 1, position: 4 })]
    #[case::bidi_control("Kick\u{202E}ed", ConflictTextsError::ForbiddenChar { index: 1, position: 4 })]
    #[case::invisible("Kick\u{200B}ed", ConflictTextsError::ForbiddenChar { index: 1, position: 4 })]
    #[case::too_long(
        &"x".repeat(1025),
        ConflictTextsError::TooLong { index: 1, chars: 1025 }
    )]
    #[case::too_long_in_chars(
        &"😀".repeat(1025),
        ConflictTextsError::TooLong { index: 1, chars: 1025 }
    )]
    fn rejects_an_invalid_entry_by_its_index(
        #[case] entry: &str,
        #[case] expected: ConflictTextsError,
    ) {
        assert_eq!(ConflictTexts::try_new(&["fine", entry]), Err(expected));
    }

    #[test]
    fn the_first_invalid_entry_decides_the_error() {
        assert_eq!(
            ConflictTexts::try_new(&["ok", "§bad", ""]),
            Err(ConflictTextsError::ForbiddenChar {
                index: 1,
                position: 0
            })
        );
    }

    #[test]
    fn errors_never_include_the_entry() {
        let error = ConflictTexts::try_new(&["§secret text"]).unwrap_err();

        assert!(!error.to_string().contains("secret"), "{error}");
        assert!(!format!("{error:?}").contains("secret"), "{error:?}");
    }

    #[rstest]
    #[case::too_many(
        ConflictTextsError::TooMany { count: 17 },
        "the list has 17 conflict texts; the limit is 16"
    )]
    #[case::empty(ConflictTextsError::Empty { index: 2 }, "conflict text 2 is empty")]
    #[case::forbidden_char(
        ConflictTextsError::ForbiddenChar { index: 0, position: 3 },
        "conflict text 0 contains a character kick messages never have, at position 3"
    )]
    #[case::too_long(
        ConflictTextsError::TooLong { index: 1, chars: 1025 },
        "conflict text 1 is 1025 characters long; the limit is 1024"
    )]
    fn errors_have_fixed_messages(#[case] error: ConflictTextsError, #[case] message: &str) {
        assert_eq!(error.to_string(), message);
    }

    #[rstest]
    #[case::plain_text(None)]
    #[case::unknown_key(Some("proxy.disconnect.already_connected"))]
    #[case::invalid_key(Some("Not A Key"))]
    #[case::known_transient_key(Some("multiplayer.disconnect.kicked"))]
    fn a_listed_transient_kick_is_a_duplicate_login(#[case] key: Option<&str>) {
        let reason = DisconnectReason::kicked(key, PROXY_TEXT);

        assert_eq!(texts(&[PROXY_TEXT]).classify(&reason), DUPLICATE_LOGIN);
    }

    #[test]
    fn the_sanitized_message_is_compared() {
        let reason = DisconnectReason::kicked(None, &format!("§c{PROXY_TEXT}\u{200B}"));

        assert_eq!(texts(&[PROXY_TEXT]).classify(&reason), DUPLICATE_LOGIN);
    }

    #[rstest]
    #[case::different_case("you are already connected to this proxy!")]
    #[case::prefix("You are already connected")]
    #[case::longer("You are already connected to this proxy! Try again.")]
    #[case::trailing_space("You are already connected to this proxy! ")]
    fn only_an_exact_match_counts(#[case] message: &str) {
        let reason = DisconnectReason::kicked(None, message);

        assert_eq!(
            texts(&[PROXY_TEXT]).classify(&reason),
            DisconnectClass::Transient
        );
    }

    #[rstest]
    #[case::banned("multiplayer.disconnect.banned", BANNED)]
    #[case::not_whitelisted("multiplayer.disconnect.not_whitelisted", NOT_WHITELISTED)]
    #[case::wrong_version("multiplayer.disconnect.incompatible", WRONG_VERSION)]
    #[case::unverified_username(
        "multiplayer.disconnect.unverified_username",
        DisconnectClass::AuthInvalid
    )]
    #[case::duplicate_login("multiplayer.disconnect.duplicate_login", DUPLICATE_LOGIN)]
    fn a_key_that_isnt_transient_wins_over_the_list(
        #[case] key: &str,
        #[case] expected: DisconnectClass,
    ) {
        let reason = DisconnectReason::kicked(Some(key), PROXY_TEXT);

        assert_eq!(texts(&[PROXY_TEXT]).classify(&reason), expected);
    }

    #[rstest]
    #[case::connection_closed(DisconnectReason::ConnectionClosed)]
    #[case::auth_rejected(DisconnectReason::AuthRejected)]
    #[case::session_crashed(DisconnectReason::SessionCrashed)]
    #[case::liveness_timeout(DisconnectReason::LivenessTimeout)]
    fn reasons_other_than_kicks_are_never_compared(#[case] reason: DisconnectReason) {
        assert_eq!(texts(&[PROXY_TEXT]).classify(&reason), reason.classify());
    }

    proptest! {
        #[test]
        fn an_empty_list_classifies_like_classify(
            key in proptest::option::of(any::<String>()),
            message in any::<String>(),
        ) {
            let reason = DisconnectReason::kicked(key.as_deref(), &message);
            prop_assert_eq!(ConflictTexts::default().classify(&reason), reason.classify());
        }

        #[test]
        fn a_list_only_turns_transient_kicks_into_conflicts(
            key in proptest::option::of(any::<String>()),
            message in any::<String>(),
            listed in any::<bool>(),
        ) {
            let reason = DisconnectReason::kicked(key.as_deref(), &message);
            let DisconnectReason::Kicked(kick) = &reason else {
                panic!("expected a kick");
            };
            let entries: Vec<&str> = if listed && !kick.message().is_empty() {
                vec![kick.message()]
            } else {
                vec![PROXY_TEXT]
            };
            let list = ConflictTexts::try_new(&entries).unwrap();

            let class = list.classify(&reason);

            let expected = match reason.classify() {
                DisconnectClass::Transient if entries.contains(&kick.message()) => DUPLICATE_LOGIN,
                other => other,
            };
            prop_assert_eq!(class, expected);
        }
    }
}
