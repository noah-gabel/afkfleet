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
//!   but for the agent's received-chat line at `debug`, which carries the
//!   server's "joined the game" as CLAUDE.md allows chat to. The name
//!   counts only as a whole word, so a short one never matches a field such
//!   as `max_bots`; the UUID in both its forms.
//! - **"PRIVATE KEY"** reaches a log only where azalea-auth logs the chat
//!   certificate's response at TRACE, P1.8's accepted risk.

use core::fmt;
use std::collections::BTreeSet;

use fleet_testkit::log_capture::{self, SecretLeak};
use secrecy::{ExposeSecret as _, SecretString};

/// The target of the agent's received-chat line.
const CHAT_TARGET: &str = "fleet_agent::run::events";
/// Where azalea-auth logs the chat certificate's response.
const CERTS_TARGET: &str = "azalea_auth::certs";

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
                if personal && (!target.starts_with("fleet_") || target == CHAT_TARGET) {
                    continue;
                }
                findings.insert(format!("{}: {level} {target} ({lines} lines)", kind.name()));
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
        tracing::debug!(target: CHAT_TARGET, "<Max> joined the game");
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
    fn whole_words_are_bounded_by_what_isnt_a_name_character() {
        assert!(whole_word("name=Max ", "Max"));
        assert!(whole_word("Max", "Max"));
        assert!(whole_word("<Max>", "Max"));
        assert!(!whole_word("max_bots", "max"));
        assert!(!whole_word("Maxwell", "Max"));
        assert!(!whole_word("_Max", "Max"));
    }
}
