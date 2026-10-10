//! The vocabulary of the server's audit log (Plan.md P6.4, P6.8, ADR-0015):
//! what happened ([`AuditAction`]), how it ended ([`AuditOutcome`]), what it
//! touched ([`AuditTarget`]) and details that hold no secrets
//! ([`AuditMetadata`]).
//!
//! **Writing is strict, reading is tolerant.** An entry is recorded only from
//! these checked types. A row a newer server wrote (after a rollback, say) may
//! hold an action, a target type or a metadata value this version doesn't
//! know, so the read side ([`RecordedTarget`], [`RecordedMetadata`]) keeps
//! those as sanitized text instead of failing the whole list. Only a value
//! that breaks the format itself is an error.
//!
//! **No secrets.** A metadata key that names a secret is refused, values come
//! only from plain text, integers and booleans (a `SecretString` has no
//! conversion), and text is sanitized like any untrusted text. The secret
//! words are [`SECRET_WORDS`], which the server's config check uses too.

mod action;
mod metadata;
mod name;
mod outcome;
mod target;

pub use self::action::AuditAction;
pub use self::metadata::{AuditMetadata, MetadataValue, RecordedMetadata, RecordedValue};
pub use self::outcome::AuditOutcome;
pub use self::target::{AuditTarget, RecordedTarget, TargetKind};

/// Name parts that say a name holds a secret, singular or plural: a config
/// key with one must name a file (P6.2), and an audit metadata key with one
/// is refused. One list for both, so a word added here covers both.
pub const SECRET_WORDS: [&str; 7] = [
    "key",
    "token",
    "password",
    "secret",
    "passphrase",
    "pepper",
    "credential",
];

/// Whether one of the name's `_`-separated parts is a [`SECRET_WORDS`]
/// entry, singular or plural.
#[must_use]
pub fn has_secret_word(name: &str) -> bool {
    name.split('_').any(|part| {
        SECRET_WORDS
            .iter()
            .any(|word| part == *word || part.strip_suffix('s') == Some(word))
    })
}

/// Why a value isn't a valid part of an audit entry. No variant carries the
/// value itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuditError {
    /// A name (action, target type or metadata key) is empty or too long.
    #[error("a name has 1 to {max} characters, not {len}")]
    NameLength {
        /// The name's length in characters.
        len: usize,
        /// The limit.
        max: usize,
    },
    /// A name has a character other than `a`–`z`, `0`–`9` and `_` (and `.`
    /// between an action's parts).
    #[error("a name may only use a-z, 0-9 and _; character {index} isn't one of them")]
    NameChar {
        /// The character's position, counted in characters.
        index: usize,
    },
    /// An action isn't two or more non-empty parts separated by dots.
    #[error("an action is two or more parts separated by dots, like area.event")]
    ActionShape,
    /// The stored outcome isn't one of the [`AuditOutcome`] names.
    #[error("unknown outcome")]
    UnknownOutcome,
    /// A known target type's ID isn't a version 7 UUID.
    #[error("the target's ID isn't a version 7 UUID")]
    TargetId,
    /// The metadata already has [`AuditMetadata::MAX_ENTRIES`] entries.
    #[error("metadata has at most {max} entries")]
    TooManyEntries {
        /// The limit.
        max: usize,
    },
    /// The metadata already has an entry with this key.
    #[error("the metadata already has this key")]
    DuplicateKey,
    /// A metadata key names a secret.
    #[error("a metadata key may not name a secret")]
    SecretKey,
    /// An integer is beyond what JavaScript represents exactly.
    #[error("metadata integers are within ±(2^53 - 1)")]
    IntOutOfRange,
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn the_secret_words_are_exactly_these() {
        assert_eq!(
            SECRET_WORDS,
            [
                "key",
                "token",
                "password",
                "secret",
                "passphrase",
                "pepper",
                "credential"
            ]
        );
    }

    #[rstest]
    #[case::word("password", true)]
    #[case::part("vault_key", true)]
    #[case::plural("api_keys", true)]
    #[case::credentials("db_credentials", true)]
    #[case::token_ttl("access_token_ttl_secs", true)]
    #[case::inside_a_part("monkey", false)]
    #[case::keep("backup_keep", false)]
    #[case::plain("attempts", false)]
    fn a_name_with_a_secret_part_is_flagged(#[case] name: &str, #[case] flagged: bool) {
        assert_eq!(has_secret_word(name), flagged, "{name}");
    }
}
