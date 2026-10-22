//! The managed check's log check (ADR-0019, S73, the user's conditions at U21):
//! every value the run saw that must never reach a log, looked for in every
//! line fleet-testkit's process-wide capture recorded, at every level and
//! from every target, dependencies included.
//!
//! - **A finding names only the value's kind** (a Microsoft token, the
//!   password, a code, …) **and the line's level and target**, with how
//!   many lines: never the value, the line or any part of it.
//! - **The values are kept as [`SecretString`]s**, never in `Debug` (it
//!   shows a count), never in a panic or on disk, and dropped with the run.
//! - **The account's name and UUID** are the user's personal data rather
//!   than secrets: they're looked for in afkfleet's own targets (`fleet_*`)
//!   but for the two received-chat lines at `debug` ([`CHAT_LINES`]: fleet-mc's
//!   bridge and the agent's event log), which carry the server's "joined
//!   the game" as CLAUDE.md allows chat to; those targets at any other
//!   level are looked in too. The name
//!   counts only as a whole word, so a short one never matches a field such
//!   as `max_bots`; the UUID in both its forms.
//! - **Every token of the server's** is also found by its shape, its
//!   prefix ([`SERVER_TOKENS`]) and the 43 characters of its random part,
//!   seen or not: fleet-client never shows its access
//!   tokens, and a refresh rotates the refresh token.
//! - **"PRIVATE KEY"** reaches a log only where azalea-auth logs the chat
//!   certificate's response at TRACE, P1.8's accepted risk.

use core::fmt;
use std::collections::BTreeSet;

use fleet_server::ports::tokens::TokenKind;
use fleet_testkit::log_capture::{self, SecretLeak};
use secrecy::{ExposeSecret as _, SecretString};

/// The lines that log received chat, at `debug` only, as CLAUDE.md allows
/// chat to be logged: fleet-mc's bridge, and the agent's event log. They
/// may carry the account's name, in a server's "joined the game".
const CHAT_LINES: [(&str, &str); 2] = [
    ("fleet_mc::events::bridge", "DEBUG"),
    ("fleet_agent::run::events", "DEBUG"),
];
/// Where azalea-auth logs the chat certificate's response.
const CERTS_TARGET: &str = "azalea_auth::certs";

/// Every kind of the server's tokens, each found by its prefix in any line
/// whether the run saw the token or not: the client never shows its access
/// tokens, and a refresh rotates the refresh token.
const SERVER_TOKENS: [TokenKind; 7] = [
    TokenKind::Access,
    TokenKind::Refresh,
    TokenKind::Mfa,
    TokenKind::Setup,
    TokenKind::Invite,
    TokenKind::Enrollment,
    TokenKind::EventTicket,
];

/// A server token's kind in a finding; a new kind doesn't compile until
/// it's named here and listed in [`SERVER_TOKENS`].
const fn token_name(kind: TokenKind) -> &'static str {
    match kind {
        TokenKind::Access => "access",
        TokenKind::Refresh => "refresh",
        TokenKind::Mfa => "MFA step",
        TokenKind::Setup => "TOTP setup",
        TokenKind::Invite => "invite",
        TokenKind::Enrollment => "enrollment",
        TokenKind::EventTicket => "live events ticket",
    }
}

/// What a value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// The Owner's password.
    Password,
    /// The authenticator's secret, or its otpauth URI.
    TotpSecret,
    /// A recovery code.
    RecoveryCode,
    /// The client's refresh token.
    RefreshToken,
    /// Microsoft's user code, shown to the user once.
    UserCode,
    /// Microsoft's device code.
    DeviceCode,
    /// A Microsoft access or refresh token.
    MicrosoftToken,
    /// A Minecraft session token.
    MinecraftToken,
    /// The agent's enrollment bundle.
    Bundle,
    /// The bundle's enrollment token alone.
    EnrollmentToken,
    /// A private key's PEM body.
    Key,
    /// The account's Minecraft name.
    Name,
    /// The account's UUID.
    Uuid,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Password => "the password",
            Self::TotpSecret => "the TOTP secret",
            Self::RecoveryCode => "a recovery code",
            Self::RefreshToken => "the client's refresh token",
            Self::UserCode => "Microsoft's user code",
            Self::DeviceCode => "Microsoft's device code",
            Self::MicrosoftToken => "a Microsoft token",
            Self::MinecraftToken => "a Minecraft token",
            Self::Bundle => "the enrollment bundle",
            Self::EnrollmentToken => "the enrollment token",
            Self::Key => "a private key",
            Self::Name => "the Minecraft name",
            Self::Uuid => "the UUID",
        }
    }
}

/// The values a run saw; see the [module docs](self).
#[derive(Default)]
pub(crate) struct Secrets(Vec<(Kind, SecretString)>);

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secrets({} values)", self.0.len())
    }
}

impl Secrets {
    /// Keeps `value` as a `kind`; an empty one says nothing.
    pub(crate) fn saw(&mut self, kind: Kind, value: &str) {
        if !value.is_empty() {
            self.0.push((kind, SecretString::from(value.to_owned())));
        }
    }

    /// Keeps everything `other` saw.
    pub(crate) fn extend(&mut self, other: Self) {
        self.0.extend(other.0);
    }

    /// Every finding, each once: a kind, a level, a target and how many
    /// lines; see the [module docs](self).
    pub(crate) fn findings(&self) -> Vec<String> {
        let mut findings = BTreeSet::new();
        for (kind, value) in &self.0 {
            let value = value.expose_secret();
            let found = match kind {
                Kind::Name => log_capture::find(|text| whole_word(text, value)),
                _ => log_capture::check_absent(value),
            };
            let Err(SecretLeak { levels, .. }) = found else {
                continue;
            };
            for ((target, level), lines) in levels {
                let personal = matches!(kind, Kind::Name | Kind::Uuid);
                let chat = CHAT_LINES.contains(&(target.as_str(), level.as_str()));
                if personal && (!target.starts_with("fleet_") || chat) {
                    continue;
                }
                findings.insert(format!("{}: {level} {target} ({lines} lines)", kind.name()));
            }
        }
        for kind in SERVER_TOKENS {
            let prefix = kind.prefix();
            if let Err(SecretLeak { levels, .. }) =
                log_capture::find(|text| holds_token(text, prefix))
            {
                for ((target, level), lines) in levels {
                    findings.insert(format!(
                        "a server token ({}): {level} {target} ({lines} lines)",
                        token_name(kind)
                    ));
                }
            }
        }
        if let Err(SecretLeak { levels, .. }) = log_capture::check_absent("PRIVATE KEY") {
            for ((target, level), lines) in levels {
                if target != CERTS_TARGET {
                    findings.insert(format!("\"PRIVATE KEY\": {level} {target} ({lines} lines)"));
                }
            }
        }
        findings.into_iter().collect()
    }
}

/// A token's random part: 32 bytes as base64url without padding, as
/// fleet-server mints it.
const TOKEN_SECRET_CHARS: usize = 43;

/// Whether `text` holds a whole token behind `prefix`: the prefix, then at
/// least the 43 base64url characters of its random part. A bare prefix, as
/// a migration's comment names a format (`afk_iv_…`) and sqlx logs it at
/// `debug`, isn't one.
fn holds_token(text: &str, prefix: &str) -> bool {
    let base64url = |c: &char| c.is_ascii_alphanumeric() || *c == '-' || *c == '_';
    text.match_indices(prefix).any(|(at, _)| {
        text.get(at + prefix.len()..).is_some_and(|tail| {
            tail.chars()
                .take(TOKEN_SECRET_CHARS)
                .filter(base64url)
                .count()
                == TOKEN_SECRET_CHARS
        })
    })
}

/// Whether `word` appears in `text` with no name character on either side:
/// a Minecraft name's letters, digits and `_`.
fn whole_word(text: &str, word: &str) -> bool {
    let name_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(word).any(|(at, _)| {
        let before = text.get(..at).and_then(|head| head.chars().next_back());
        let after = text
            .get(at + word.len()..)
            .and_then(|tail| tail.chars().next());
        !before.is_some_and(name_char) && !after.is_some_and(name_char)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake secret, as a test plants it: never a real one.
    const PLANTED: &str = "planted-7f3a9c2e5b8d41";

    #[test]
    fn a_planted_secret_fails_the_check_and_its_report_never_holds_it() {
        log_capture::install().unwrap();
        tracing::warn!(target: "fleet_planted", "a line that leaks {PLANTED} here");
        let mut secrets = Secrets::default();
        secrets.saw(Kind::MinecraftToken, PLANTED);

        let findings = secrets.findings();

        assert_eq!(
            findings,
            ["a Minecraft token: WARN fleet_planted (1 lines)"],
            "the kind, the level, the target and the count"
        );
        let report = findings.join("\n");
        assert!(!report.contains(PLANTED), "never the secret");
        assert!(!report.contains("planted-"), "never a part of it");
        assert!(!report.contains("a line that leaks"), "never the line");
        assert_eq!(format!("{secrets:?}"), "Secrets(1 values)");
    }

    #[test]
    fn a_name_counts_only_as_a_whole_word_and_only_outside_the_chat_line() {
        log_capture::install().unwrap();
        tracing::info!(target: "fleet_planted_fields", "max_bots=4 Maxwell");
        tracing::debug!(target: "fleet_agent::run::events", "<Max> joined the game");
        let mut secrets = Secrets::default();
        secrets.saw(Kind::Name, "Max");
        let quiet = secrets.findings();
        tracing::info!(target: "fleet_planted_name", "the bot Max joined");

        let found = secrets.findings();

        assert_eq!(quiet, Vec::<String>::new());
        assert_eq!(
            found,
            ["the Minecraft name: INFO fleet_planted_name (1 lines)"]
        );
    }

    #[test]
    fn the_name_is_allowed_only_in_the_two_chat_lines_at_debug() {
        log_capture::install().unwrap();
        let mut secrets = Secrets::default();
        secrets.saw(Kind::Name, "Steve");
        secrets.saw(Kind::Uuid, "0b7c5d1e-9a3f-4c2b-8e41-5d1e0b7c9a3f");
        tracing::debug!(target: "fleet_mc::events::bridge", text = "Steve joined the game", "chat received");
        tracing::debug!(target: "fleet_agent::run::events", "<Steve> joined the game");
        let allowed = secrets.findings();
        tracing::info!(target: "fleet_mc::events::bridge", "Steve");
        tracing::info!(target: "fleet_agent::run::events", "0b7c5d1e-9a3f-4c2b-8e41-5d1e0b7c9a3f");

        let flagged = secrets.findings();

        assert_eq!(allowed, Vec::<String>::new(), "both chat lines at debug");
        assert_eq!(
            flagged,
            [
                "the Minecraft name: INFO fleet_mc::events::bridge (1 lines)",
                "the UUID: INFO fleet_agent::run::events (1 lines)",
            ],
            "the same targets at another level"
        );
    }

    #[test]
    fn any_server_token_in_a_line_fails_the_check_by_its_kind_unrecorded() {
        log_capture::install().unwrap();
        let access = format!("{}{}", TokenKind::Access.prefix(), "Q".repeat(43));
        let enrollment = format!("{}{}", TokenKind::Enrollment.prefix(), "Z".repeat(43));
        tracing::info!(target: "fleet_planted_token", "bearer {access}");
        tracing::warn!(target: "fleet_planted_bundle", "{enrollment}.fingerprint");

        let findings = Secrets::default().findings();

        assert_eq!(
            findings,
            [
                "a server token (access): INFO fleet_planted_token (1 lines)",
                "a server token (enrollment): WARN fleet_planted_bundle (1 lines)",
            ],
            "found by the prefix alone, though the run never saw either"
        );
        let report = findings.join(
            "
",
        );
        assert!(
            !report.contains("QQQQ") && !report.contains("ZZZZ"),
            "never the token"
        );
        assert!(!report.contains("bearer"), "never the line");
    }

    #[test]
    fn a_prefix_that_isnt_a_whole_token_is_no_finding() {
        log_capture::install().unwrap();
        // sqlx logs each migration's SQL at debug, comments included.
        tracing::debug!(
            target: "sqlx::query",
            "-- SHA-256 of the code's text (`afk_iv_…`), prefix included."
        );
        let short = format!("{}{}", TokenKind::Enrollment.prefix(), "Z".repeat(42));
        tracing::debug!(target: "fleet_planted_short", "{short}");

        let findings = Secrets::default().findings();

        assert_eq!(findings, Vec::<String>::new());
    }

    #[test]
    fn whole_words_are_bounded_by_what_isnt_a_name_character() {
        assert!(whole_word("name=Max ", "Max"));
        assert!(whole_word("Max", "Max"));
        assert!(whole_word("<Max>", "Max"));
        assert!(!whole_word("max_bots", "max"));
        assert!(!whole_word("Maxwell", "Max"));
        assert!(!whole_word("_Max", "Max"));
    }
}
