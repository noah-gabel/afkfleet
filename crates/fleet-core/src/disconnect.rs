//! Why a Minecraft session ended, and what that means for the bot.
//!
//! The adapter reports a [`DisconnectReason`]; [`DisconnectReason::classify`]
//! decides whether the bot retries, stops for good, or pauses because a human
//! logged in (ADR-0008 §5, §6 and §9; ADR-0010). Kick reasons are classified by
//! their translation key, never by their text: the text depends on the server's
//! language and plugins.

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
    /// The account or its IP address is banned.
    Banned,
    /// The account isn't on the whitelist.
    NotWhitelisted,
    /// The server runs a different Minecraft version.
    WrongVersion,
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

    /// Classifies the reason (ADR-0008 §6).
    ///
    /// Kicks are classified by translation key; a kick without a known key
    /// (plain text, or a key this version doesn't know) is transient. Every
    /// other reason is transient, except a rejected session, which is
    /// [`DisconnectClass::AuthInvalid`].
    #[must_use]
    pub fn classify(&self) -> DisconnectClass {
        match self {
            Self::Kicked(kick) => kick
                .key
                .as_ref()
                .map_or(DisconnectClass::Transient, |key| classify_key(key.as_str())),
            Self::AuthRejected => DisconnectClass::AuthInvalid,
            Self::ConnectionClosed
            | Self::ConnectFailed { .. }
            | Self::SessionCrashed
            | Self::WatchdogTimeout
            | Self::LivenessTimeout => DisconnectClass::Transient,
        }
    }
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
}
