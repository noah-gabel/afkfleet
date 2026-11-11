//! The targets over text: fleet-core's value objects, the sanitizer, the
//! chat formats and the agents' private targets.

use core::fmt::{Debug, Display};
use core::str::FromStr;

use fleet_core::account::{AccountKind, RefusalReason};
use fleet_core::audit::{AuditAction, AuditOutcome, TargetKind};
use fleet_core::auth::{NewPassword, TotpCode};
use fleet_core::authz::{CommandAllowlist, GrantLevel, ModeVisibility, Role};
use fleet_core::bot::DesiredRunState;
use fleet_core::chat::{
    ChatFormats, ChatKind, ChatMessage, IncomingChat, PlayerChatKind, READ_MAX,
};
use fleet_core::disconnect::ConflictTexts;
use fleet_core::enrollment::EnrollmentBundle;
use fleet_core::id::{BotId, UserId};
use fleet_core::mode::{ModeName, ModePreset};
use fleet_core::net::{IpNet, PrivateTargets};
use fleet_core::pki::CertFingerprint;
use fleet_core::text::sanitize_untrusted;
use fleet_core::value::{
    AgentName, AgentVersion, McUsername, McUuid, ServerAddress, ServerKey, Username, UsernamePrefix,
};
use secrecy::SecretString;

use crate::{halves, lossy};

/// Checks that a value `parse` accepts reads back as itself from what it
/// writes: `parse(value.to_string()) == value`.
///
/// # Panics
/// When a written value doesn't parse, or parses as another value.
pub fn reads_back<T, E>(text: &str, parse: impl Fn(&str) -> Result<T, E>)
where
    T: Display + PartialEq + Debug,
{
    if let Ok(value) = parse(text) {
        let written = value.to_string();
        let again = parse(&written).ok();
        assert_eq!(
            again.as_ref(),
            Some(&value),
            "{} doesn't read back from what it writes",
            core::any::type_name::<T>()
        );
    }
}

/// [`reads_back`] for a type that parses through `FromStr`.
fn from_str_reads_back<T>(text: &str)
where
    T: FromStr + Display + PartialEq + Debug,
{
    reads_back(text, T::from_str);
}

/// Every string parser of fleet-core over any text: none panics, and a
/// value that's accepted and can be written reads back as itself.
///
/// # Panics
/// When a property breaks.
pub fn value_objects(data: &[u8]) {
    let text = &*lossy(data);
    from_str_reads_back::<Username>(text);
    from_str_reads_back::<McUsername>(text);
    from_str_reads_back::<McUuid>(text);
    from_str_reads_back::<ServerAddress>(text);
    from_str_reads_back::<ServerKey>(text);
    from_str_reads_back::<AgentName>(text);
    from_str_reads_back::<AgentVersion>(text);
    from_str_reads_back::<ModeName>(text);
    from_str_reads_back::<CertFingerprint>(text);
    from_str_reads_back::<IpNet>(text);
    from_str_reads_back::<UserId>(text);
    from_str_reads_back::<BotId>(text);
    from_str_reads_back::<Role>(text);
    from_str_reads_back::<GrantLevel>(text);
    from_str_reads_back::<ModeVisibility>(text);
    from_str_reads_back::<AccountKind>(text);
    from_str_reads_back::<RefusalReason>(text);
    from_str_reads_back::<AuditOutcome>(text);
    from_str_reads_back::<ChatKind>(text);
    from_str_reads_back::<ModePreset>(text);
    reads_back(text, |text| AuditAction::try_from(text));
    // Parsed only: nothing writes them out again.
    let _ = text.parse::<DesiredRunState>();
    let _ = TargetKind::try_from(text);
    let _ = UsernamePrefix::try_from(text);
    let _ = ChatMessage::try_from(text);
    let _ = EnrollmentBundle::try_from(text);
    let _ = NewPassword::try_from(SecretString::from(text));
    let _ = TotpCode::parse(text);
    // The lists, a line an entry.
    let lines: Vec<&str> = text.split('\n').collect();
    let _ = CommandAllowlist::try_new(&lines);
    let _ = ConflictTexts::try_new(&lines);
}

/// Whether `c` may be in sanitized text that keeps its line breaks.
fn is_clean(c: char) -> bool {
    c != '§' && (c == '\n' || !c.is_control())
}

/// The sanitizer over any text and any cap: a received chat line and a log
/// line come out within their length, without a control character or a `§`,
/// and sanitizing them again changes nothing.
///
/// # Panics
/// When a property breaks.
pub fn chat_sanitizer(data: &[u8]) {
    let (cap, raw) = match data.split_first_chunk::<2>() {
        Some((cap, raw)) => (usize::from(u16::from_le_bytes(*cap)) % 2049, raw),
        None => (0, data),
    };
    let raw = &*lossy(raw);

    let line = sanitize_untrusted(raw, cap);
    assert!(line.text.chars().count() <= cap, "a log line over its cap");
    assert!(
        line.text.chars().all(|c| is_clean(c) && c != '\n'),
        "a log line with a line break, a control character or a section sign"
    );
    assert_eq!(
        sanitize_untrusted(&line.text, cap).text,
        line.text,
        "a log line changes when sanitized again"
    );

    let chat = IncomingChat::system(raw);
    assert!(
        chat.text().chars().count() <= IncomingChat::MAX_TEXT_LEN,
        "a chat line over its cap"
    );
    assert!(
        chat.text().chars().all(is_clean),
        "a chat line with a control character or a section sign"
    );
    assert_eq!(
        IncomingChat::system(chat.text()).text(),
        chat.text(),
        "a chat line changes when sanitized again"
    );
}

/// How long an entry must be before its absence from an error is checked:
/// shorter ones show up in any message by chance ("a", "format").
const TELLING_CHARS: usize = 12;

/// [`ChatFormats::try_new`] over any list of strings, a line each (ADR-0020,
/// S80, R138): no panic; an error never holds an entry's text; and the
/// formats it accepts read back as themselves.
///
/// # Panics
/// When a property breaks.
pub fn chat_formats(data: &[u8]) {
    let text = &*lossy(data);
    let entries: Vec<&str> = text.split('\n').collect();
    match ChatFormats::try_new(&entries) {
        Ok(formats) => {
            let written: Vec<&str> = formats.iter().collect();
            assert_eq!(
                ChatFormats::try_new(&written).as_ref(),
                Ok(&formats),
                "accepted chat formats don't read back as themselves"
            );
        }
        Err(error) => {
            let shown = format!("{error} {error:?}");
            for entry in entries {
                assert!(
                    entry.chars().count() < TELLING_CHARS || !shown.contains(entry),
                    "a chat format's error holds its text"
                );
            }
        }
    }
}

/// [`ChatFormats::sender_of`] over any valid formats (the bytes before the
/// first zero byte, a line each) and any received line (the bytes after it):
/// no panic; only a system line has a parsed sender; and a sender is read
/// from the characters the formats may read, the line's first [`READ_MAX`].
///
/// # Panics
/// When a property breaks.
pub fn chat_sender(data: &[u8]) {
    let (formats, line) = halves(data);
    let formats = &*lossy(formats);
    let entries: Vec<&str> = formats.split('\n').collect();
    let Ok(formats) = ChatFormats::try_new(&entries) else {
        return;
    };
    let line = &*lossy(line);

    let system = IncomingChat::system(line);
    if let Some(sender) = formats.sender_of(&system) {
        let head: String = system.text().chars().take(READ_MAX).collect();
        assert!(
            head.contains(sender.as_str()),
            "a parsed sender that isn't in the characters a format reads"
        );
    }
    let said = IncomingChat::player(PlayerChatKind::Chat, "Steve", None, line);
    assert_eq!(
        formats.sender_of(&said),
        None,
        "a player's line got a parsed sender"
    );
}

/// [`IpNet`]'s parser over any lines, and [`PrivateTargets::problems`] over
/// the networks among them (ADR-0019, group G): no panic, and the list has
/// no problem exactly when it's accepted.
///
/// # Panics
/// When a property breaks.
pub fn private_targets(data: &[u8]) {
    let text = &*lossy(data);
    let networks: Vec<IpNet> = text
        .split('\n')
        .filter_map(|line| line.parse().ok())
        .collect();

    let problems = PrivateTargets::problems(&networks);
    let accepted = PrivateTargets::try_new(networks);

    assert_eq!(
        problems.is_empty(),
        accepted.is_ok(),
        "a list with problems is accepted, or one without is refused"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A value that writes itself in a way its parser refuses.
    #[derive(Debug, PartialEq)]
    struct Lossy(u8);

    impl Display for Lossy {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "#{}", self.0)
        }
    }

    /// A value that writes another value than itself.
    #[derive(Debug, PartialEq)]
    struct Drifting(u8);

    impl Display for Drifting {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "{}", self.0.wrapping_add(1))
        }
    }

    #[test]
    fn a_value_that_reads_back_passes() {
        reads_back("7", str::parse::<u8>);
    }

    #[test]
    fn text_that_isnt_a_value_passes() {
        reads_back("seven", str::parse::<u8>);
    }

    #[test]
    #[should_panic(expected = "doesn't read back from what it writes")]
    fn a_value_whose_writing_doesnt_parse_fails() {
        reads_back("7", |text| text.parse().map(Lossy));
    }

    #[test]
    #[should_panic(expected = "doesn't read back from what it writes")]
    fn a_value_that_reads_back_as_another_fails() {
        reads_back("7", |text| text.parse().map(Drifting));
    }

    #[test]
    fn clean_text_has_no_control_character_or_section_sign() {
        assert!(is_clean('a'));
        assert!(is_clean('\n'));
        assert!(!is_clean('\r'));
        assert!(!is_clean('\u{7}'));
        assert!(!is_clean('§'));
    }
}
