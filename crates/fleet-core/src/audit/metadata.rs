//! [`AuditMetadata`]: an entry's details, checked when they're written, and
//! [`RecordedMetadata`], how stored details read back.

use core::fmt::Write as _;
use std::collections::BTreeMap;

use super::name::{self, Dots};
use super::{AuditError, has_secret_word};
use crate::text::sanitize_untrusted;

/// One metadata value, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataValue {
    /// Sanitized text, at most [`AuditMetadata::MAX_TEXT_CHARS`] characters.
    /// A value that was cut ends in `…`.
    Text(String),
    /// An integer within ±[`AuditMetadata::MAX_INT`].
    Int(i64),
    /// A flag.
    Bool(bool),
}

/// An entry's details, stored as a JSON object in `metadata_json`.
///
/// Every limit holds the moment an entry is added, and no entry is ever
/// refused for its size, so whoever controls a text value can't decide
/// whether an entry gets recorded. Only a deterministic mistake fails: an
/// invalid, secret or duplicate key, a 17th entry, an integer out of range.
/// - at most [`AuditMetadata::MAX_ENTRIES`] entries;
/// - keys of `a`–`z`, `0`–`9` and `_`, at most [`AuditMetadata::MAX_KEY_LEN`]
///   characters, never one that names a secret ([`super::SECRET_WORDS`]);
/// - values only from plain text (sanitized and cut to
///   [`AuditMetadata::MAX_TEXT_CHARS`] characters), integers within
///   ±[`AuditMetadata::MAX_INT`] (so the TypeScript app reads them exactly)
///   and booleans. A `SecretString` has no conversion, so a secret only gets
///   in through an explicit `expose_secret()`, which review sees;
/// - at most [`AuditMetadata::MAX_JSON_BYTES`] bytes of JSON, measured on
///   exactly the text [`AuditMetadata::to_json`] writes. Every free entry
///   slot keeps room for the largest integer or boolean entry, and a text
///   value is cut to what's left (on a character boundary, measured after
///   JSON escaping), so a later entry always fits.
///
/// A text value that was cut, for either limit, ends in `…`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditMetadata {
    entries: BTreeMap<String, MetadataValue>,
}

impl AuditMetadata {
    /// The most entries.
    pub const MAX_ENTRIES: usize = 16;
    /// The longest key, in characters.
    pub const MAX_KEY_LEN: usize = 32;
    /// The longest text value, in characters, after sanitizing.
    pub const MAX_TEXT_CHARS: usize = 256;
    /// The largest integer magnitude: 2^53 − 1, JavaScript's largest exact
    /// integer.
    pub const MAX_INT: i64 = (1 << 53) - 1;
    /// The longest JSON object, in bytes.
    pub const MAX_JSON_BYTES: usize = 4096;

    /// The most bytes one integer or boolean entry adds to the JSON: a comma,
    /// the longest key in quotes, a colon and the longest value (the most
    /// negative integer, or `false`). Every free slot keeps this much room.
    const SLOT_BYTES: usize =
        1 + (Self::MAX_KEY_LEN + 2) + 1 + max(int_len(-Self::MAX_INT), "false".len());

    /// Empty metadata, stored as `NULL`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a text entry. The text is sanitized like any untrusted text, cut
    /// to [`AuditMetadata::MAX_TEXT_CHARS`] characters, and cut further to
    /// the room left in the JSON; a cut value ends in `…`. It never fails
    /// because of the text.
    ///
    /// # Errors
    /// The key errors ([`AuditError::NameLength`], [`AuditError::NameChar`],
    /// [`AuditError::SecretKey`], [`AuditError::DuplicateKey`]) and
    /// [`AuditError::TooManyEntries`].
    pub fn text(self, key: &str, value: &str) -> Result<Self, AuditError> {
        self.check_key(key)?;
        let sanitized = sanitize_untrusted(value, Self::MAX_TEXT_CHARS);
        let mut text = sanitized.text;
        if sanitized.truncated {
            text.pop();
            text.push(ELLIPSIS);
        }
        let text = cut_to(text, self.text_room(key));
        Ok(self.with(key, MetadataValue::Text(text)))
    }

    /// Adds an integer entry.
    ///
    /// # Errors
    /// [`AuditError::IntOutOfRange`] beyond ±[`AuditMetadata::MAX_INT`], and
    /// the errors of [`AuditMetadata::text`].
    pub fn int(self, key: &str, value: i64) -> Result<Self, AuditError> {
        if !(-Self::MAX_INT..=Self::MAX_INT).contains(&value) {
            return Err(AuditError::IntOutOfRange);
        }
        self.check_key(key)?;
        Ok(self.with(key, MetadataValue::Int(value)))
    }

    /// Adds a boolean entry.
    ///
    /// # Errors
    /// The errors of [`AuditMetadata::text`].
    pub fn flag(self, key: &str, value: bool) -> Result<Self, AuditError> {
        self.check_key(key)?;
        Ok(self.with(key, MetadataValue::Bool(value)))
    }

    /// Checks that `key` may be added: a valid name, no secret word, not
    /// there yet, and a free slot.
    fn check_key(&self, key: &str) -> Result<(), AuditError> {
        name::check(key, Self::MAX_KEY_LEN, Dots::Forbidden)?;
        if has_secret_word(key) {
            return Err(AuditError::SecretKey);
        }
        if self.entries.contains_key(key) {
            return Err(AuditError::DuplicateKey);
        }
        if self.entries.len() >= Self::MAX_ENTRIES {
            return Err(AuditError::TooManyEntries {
                max: Self::MAX_ENTRIES,
            });
        }
        Ok(())
    }

    /// Adds an entry whose key passed [`AuditMetadata::check_key`].
    fn with(mut self, key: &str, value: MetadataValue) -> Self {
        self.entries.insert(key.to_owned(), value);
        self
    }

    /// The bytes a text value under `key` may take once escaped: the limit,
    /// less the JSON so far, less this entry's structure (`,"key":""`), less
    /// the room every slot still free after it keeps.
    ///
    /// The JSON always fits its free slots (the assertion below proves the
    /// start, and each entry keeps it), and an entry's structure takes at
    /// most a slot, so the room left is never negative and always holds a
    /// `…`.
    fn text_room(&self, key: &str) -> usize {
        let comma = usize::from(!self.entries.is_empty());
        let structure = comma + key.len() + 2 + 1 + 2;
        let free_after = Self::MAX_ENTRIES.saturating_sub(self.entries.len() + 1);
        Self::MAX_JSON_BYTES
            .saturating_sub(self.to_json().len())
            .saturating_sub(structure)
            .saturating_sub(free_after * Self::SLOT_BYTES)
    }

    /// Whether there are no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entries, sorted by key.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &MetadataValue)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value))
    }

    /// The compact JSON object stored in `metadata_json`: keys sorted,
    /// strings escaped as `serde_json` escapes them, other characters as
    /// UTF-8. Empty metadata is `{}` here; the store writes `NULL` for it.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut json = String::from("{");
        for (index, (key, value)) in self.entries.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            push_json_string(&mut json, key);
            json.push(':');
            match value {
                MetadataValue::Text(text) => push_json_string(&mut json, text),
                MetadataValue::Int(n) => json.push_str(&n.to_string()),
                MetadataValue::Bool(b) => json.push_str(if *b { "true" } else { "false" }),
            }
        }
        json.push('}');
        json
    }
}

// The guarantee that no entry is ever refused for its size: the empty
// object with every slot holding the largest integer or boolean entry fits.
// Raising `MAX_ENTRIES`, `MAX_KEY_LEN` or `MAX_INT` past that fails the build.
const _: () = assert!(
    2 + AuditMetadata::MAX_ENTRIES * AuditMetadata::SLOT_BYTES <= AuditMetadata::MAX_JSON_BYTES
);

/// What a cut text value ends in.
const ELLIPSIS: char = '…';

/// The number of characters `n` takes in decimal, with its sign.
const fn int_len(n: i64) -> usize {
    let mut magnitude = n.unsigned_abs();
    let mut len = if n < 0 { 2 } else { 1 };
    while magnitude >= 10 {
        magnitude /= 10;
        len += 1;
    }
    len
}

/// The larger of two lengths.
const fn max(a: usize, b: usize) -> usize {
    if a > b { a } else { b }
}

/// Cuts `text` so that, escaped, it takes at most `room` bytes: on a
/// character boundary, ending in `…`. A text that fits is returned as it is.
fn cut_to(text: String, room: usize) -> String {
    if text.chars().map(escaped_len).sum::<usize>() <= room {
        return text;
    }
    let room = room.saturating_sub(escaped_len(ELLIPSIS));
    let mut used = 0;
    let mut cut = String::new();
    for c in text.chars() {
        let len = escaped_len(c);
        if used + len > room {
            break;
        }
        used += len;
        cut.push(c);
    }
    cut.push(ELLIPSIS);
    cut
}

/// The bytes `c` takes in a JSON string.
fn escaped_len(c: char) -> usize {
    let mut escaped = String::new();
    push_json_char(&mut escaped, c);
    escaped.len()
}

/// Appends `text` as a JSON string, escaped exactly as `serde_json` escapes.
fn push_json_string(json: &mut String, text: &str) {
    json.push('"');
    for c in text.chars() {
        push_json_char(json, c);
    }
    json.push('"');
}

/// Appends one character of a JSON string, escaped exactly as `serde_json`
/// escapes: `"` and `\` with a backslash, the short escapes for `\b`, `\f`,
/// `\n`, `\r` and `\t`, other control characters as `\u00xx`, everything else
/// as it is.
fn push_json_char(json: &mut String, c: char) {
    match c {
        '"' => json.push_str("\\\""),
        '\\' => json.push_str("\\\\"),
        '\u{8}' => json.push_str("\\b"),
        '\u{c}' => json.push_str("\\f"),
        '\n' => json.push_str("\\n"),
        '\r' => json.push_str("\\r"),
        '\t' => json.push_str("\\t"),
        c if u32::from(c) < 0x20 => {
            // Writing to a String can't fail.
            let _ = write!(json, "\\u{:04x}", u32::from(c));
        }
        c => json.push(c),
    }
}

/// One metadata value as it reads back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedValue {
    /// Text, sanitized again on the way out.
    Text(String),
    /// An integer within ±[`AuditMetadata::MAX_INT`].
    Int(i64),
    /// A flag.
    Bool(bool),
    /// A value this version doesn't know (written by a newer server), as its
    /// JSON text, sanitized and cut to [`AuditMetadata::MAX_TEXT_CHARS`]
    /// characters.
    Unrecognized(String),
}

/// Stored details as they read back: tolerant of whatever a newer version
/// may have written, so an unknown value or key never fails a list. Keys
/// that aren't valid names are kept as sanitized text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordedMetadata {
    entries: Vec<(String, RecordedValue)>,
}

impl RecordedMetadata {
    /// No details (a `NULL` column).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a stored text value.
    pub fn push_text(&mut self, key: &str, text: &str) {
        let text = sanitize_untrusted(text, AuditMetadata::MAX_TEXT_CHARS).text;
        self.push(key, RecordedValue::Text(text));
    }

    /// Adds a stored integer; one beyond ±[`AuditMetadata::MAX_INT`] reads
    /// back as unrecognized.
    pub fn push_int(&mut self, key: &str, value: i64) {
        if (-AuditMetadata::MAX_INT..=AuditMetadata::MAX_INT).contains(&value) {
            self.push(key, RecordedValue::Int(value));
        } else {
            self.push(key, RecordedValue::Unrecognized(value.to_string()));
        }
    }

    /// Adds a stored boolean.
    pub fn push_bool(&mut self, key: &str, value: bool) {
        self.push(key, RecordedValue::Bool(value));
    }

    /// Adds a stored value this version doesn't know, given as its JSON text.
    pub fn push_unrecognized(&mut self, key: &str, json: &str) {
        let text = sanitize_untrusted(json, AuditMetadata::MAX_TEXT_CHARS).text;
        self.push(key, RecordedValue::Unrecognized(text));
    }

    /// Adds an entry; a key that isn't a valid name is kept as sanitized
    /// text.
    fn push(&mut self, key: &str, value: RecordedValue) {
        let key = match name::check(key, AuditMetadata::MAX_KEY_LEN, Dots::Forbidden) {
            Ok(()) => key.to_owned(),
            Err(_) => sanitize_untrusted(key, AuditMetadata::MAX_TEXT_CHARS).text,
        };
        self.entries.push((key, value));
    }

    /// The entries, in the order they were added.
    #[must_use]
    pub fn entries(&self) -> &[(String, RecordedValue)] {
        &self.entries
    }
}

impl From<&AuditMetadata> for RecordedMetadata {
    fn from(metadata: &AuditMetadata) -> Self {
        let entries = metadata
            .iter()
            .map(|(key, value)| {
                let value = match value {
                    MetadataValue::Text(text) => RecordedValue::Text(text.clone()),
                    MetadataValue::Int(n) => RecordedValue::Int(*n),
                    MetadataValue::Bool(b) => RecordedValue::Bool(*b),
                };
                (key.to_owned(), value)
            })
            .collect();
        Self { entries }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rstest::rstest;

    use super::*;

    #[test]
    fn empty_metadata_is_an_empty_object() {
        let metadata = AuditMetadata::new();

        assert!(metadata.is_empty());
        assert_eq!(metadata.to_json(), "{}");
    }

    #[test]
    fn entries_are_written_sorted_and_compact() {
        let metadata = AuditMetadata::new()
            .text("reason", "expired")
            .unwrap()
            .int("attempts", -3)
            .unwrap()
            .flag("locked", true)
            .unwrap();

        assert_eq!(
            metadata.to_json(),
            r#"{"attempts":-3,"locked":true,"reason":"expired"}"#
        );
    }

    #[test]
    fn quotes_and_backslashes_are_escaped() {
        let metadata = AuditMetadata::new().text("path", r#"a "b" \c"#).unwrap();

        assert_eq!(metadata.to_json(), r#"{"path":"a \"b\" \\c"}"#);
    }

    #[test]
    fn text_is_sanitized_and_cut() {
        let long = "x".repeat(300);
        let metadata = AuditMetadata::new()
            .text("note", "line one\nline two\u{202e}")
            .unwrap()
            .text("long", &long)
            .unwrap();

        assert_eq!(
            metadata.iter().collect::<Vec<_>>(),
            [
                (
                    "long",
                    &MetadataValue::Text(format!("{}…", "x".repeat(255)))
                ),
                (
                    "note",
                    &MetadataValue::Text("line one | line two".to_owned())
                ),
            ]
        );
    }

    #[rstest]
    #[case::empty("", AuditError::NameLength { len: 0, max: 32 })]
    #[case::too_long("abcdefghijklmnopqrstuvwxyzabcdefg", AuditError::NameLength { len: 33, max: 32 })]
    #[case::dotted("a.b", AuditError::NameChar { index: 1 })]
    #[case::uppercase("Reason", AuditError::NameChar { index: 0 })]
    #[case::secret("password", AuditError::SecretKey)]
    #[case::secret_part("refresh_token", AuditError::SecretKey)]
    #[case::secret_plural("api_keys", AuditError::SecretKey)]
    fn bad_keys_are_refused(#[case] key: &str, #[case] expected: AuditError) {
        assert_eq!(AuditMetadata::new().flag(key, true), Err(expected));
    }

    #[test]
    fn a_duplicate_key_is_refused() {
        let metadata = AuditMetadata::new().int("attempts", 1).unwrap();

        assert_eq!(metadata.int("attempts", 2), Err(AuditError::DuplicateKey));
    }

    #[test]
    fn the_seventeenth_entry_is_refused() {
        let mut metadata = AuditMetadata::new();
        for n in 0..16 {
            metadata = metadata.int(&format!("k{n}"), n).unwrap();
        }

        assert_eq!(
            metadata.int("k16", 16),
            Err(AuditError::TooManyEntries { max: 16 })
        );
    }

    #[rstest]
    #[case::max(AuditMetadata::MAX_INT, true)]
    #[case::min(-AuditMetadata::MAX_INT, true)]
    #[case::above(AuditMetadata::MAX_INT + 1, false)]
    #[case::below(-AuditMetadata::MAX_INT - 1, false)]
    #[case::i64_max(i64::MAX, false)]
    #[case::i64_min(i64::MIN, false)]
    fn integers_stay_within_what_javascript_reads_exactly(#[case] value: i64, #[case] ok: bool) {
        let result = AuditMetadata::new().int("count", value);

        assert_eq!(result.is_ok(), ok, "{value}: {result:?}");
        if !ok {
            assert_eq!(result, Err(AuditError::IntOutOfRange));
        }
    }

    /// The text value stored under `key`.
    fn text_of<'m>(metadata: &'m AuditMetadata, key: &str) -> &'m str {
        match metadata.iter().find(|(k, _)| *k == key) {
            Some((_, MetadataValue::Text(text))) => text,
            other => panic!("no text under {key}: {other:?}"),
        }
    }

    /// A 32-character key that names no secret: `k` and 31 digits.
    fn long_key(n: usize) -> String {
        format!("k{n:031}")
    }

    #[test]
    fn four_long_texts_all_fit_and_only_the_last_is_cut() {
        // 256 four-byte characters: 1024 bytes each, 4 KiB together.
        let full = "😀".repeat(256);

        let metadata = AuditMetadata::new()
            .text("k0", &full)
            .unwrap()
            .text("k1", &full)
            .unwrap()
            .text("k2", &full)
            .unwrap()
            .text("k3", &full)
            .unwrap();

        assert!(metadata.to_json().len() <= AuditMetadata::MAX_JSON_BYTES);
        for kept in ["k0", "k1", "k2"] {
            assert_eq!(text_of(&metadata, kept), full, "{kept}");
        }
        let cut = text_of(&metadata, "k3");
        let prefix = cut.strip_suffix('…').unwrap();
        assert!(!prefix.is_empty() && full.starts_with(prefix), "{cut}");
    }

    #[test]
    fn a_cut_is_measured_after_json_escaping() {
        // 256 quotes: 256 characters, but 512 bytes once escaped.
        let quotes = "\"".repeat(256);
        let mut metadata = AuditMetadata::new();

        for n in 0..AuditMetadata::MAX_ENTRIES {
            metadata = metadata.text(&format!("k{n}"), &quotes).unwrap();
        }

        let json = metadata.to_json();
        assert!(
            json.len() <= AuditMetadata::MAX_JSON_BYTES,
            "{}",
            json.len()
        );
        assert!(serde_json::from_str::<serde_json::Value>(&json).is_ok());
        let last = text_of(&metadata, "k9");
        assert!(last.ends_with('…'), "{last}");
        assert!(
            last.trim_end_matches('…').chars().all(|c| c == '"'),
            "{last}"
        );
    }

    #[rstest]
    #[case::zero(0, 1)]
    #[case::minus_one(-1, 2)]
    #[case::nine(9, 1)]
    #[case::ten(10, 2)]
    #[case::most_negative_metadata_int(-AuditMetadata::MAX_INT, 17)]
    #[case::i64_min(i64::MIN, 20)]
    fn int_len_counts_the_sign_and_every_digit(#[case] n: i64, #[case] len: usize) {
        assert_eq!(int_len(n), len);
        assert_eq!(n.to_string().len(), len);
    }

    #[test]
    fn ints_and_bools_never_come_near_the_budget() {
        let mut ints = AuditMetadata::new();
        let mut flags = AuditMetadata::new();

        for n in 0..AuditMetadata::MAX_ENTRIES {
            ints = ints.int(&long_key(n), -AuditMetadata::MAX_INT).unwrap();
            flags = flags.flag(&long_key(n), false).unwrap();
        }

        assert!(ints.to_json().len() < 1024, "{}", ints.to_json().len());
        assert!(flags.to_json().len() < 1024, "{}", flags.to_json().len());
    }

    #[test]
    fn ints_still_fit_after_texts_used_the_budget() {
        let full = "😀".repeat(256);
        let mut metadata = AuditMetadata::new();

        for n in 0..12 {
            metadata = metadata.text(&long_key(n), &full).unwrap();
        }
        for n in 12..AuditMetadata::MAX_ENTRIES {
            metadata = metadata.int(&long_key(n), -AuditMetadata::MAX_INT).unwrap();
        }

        assert!(metadata.to_json().len() <= AuditMetadata::MAX_JSON_BYTES);
    }

    #[test]
    fn written_metadata_reads_back_as_the_same_values() {
        let metadata = AuditMetadata::new()
            .text("reason", "expired")
            .unwrap()
            .int("attempts", 3)
            .unwrap()
            .flag("locked", false)
            .unwrap();

        let recorded = RecordedMetadata::from(&metadata);

        assert_eq!(
            recorded.entries(),
            [
                ("attempts".to_owned(), RecordedValue::Int(3)),
                ("locked".to_owned(), RecordedValue::Bool(false)),
                (
                    "reason".to_owned(),
                    RecordedValue::Text("expired".to_owned())
                ),
            ]
        );
    }

    #[test]
    fn reading_is_tolerant() {
        let mut recorded = RecordedMetadata::new();

        recorded.push_text("note", "a\nb");
        recorded.push_int("big", i64::MAX);
        recorded.push_bool("flag", true);
        recorded.push_unrecognized("list", "[1,2]");
        recorded.push_text("Bad Key\u{202e}", "x");

        assert_eq!(
            recorded.entries(),
            [
                ("note".to_owned(), RecordedValue::Text("a | b".to_owned())),
                (
                    "big".to_owned(),
                    RecordedValue::Unrecognized(i64::MAX.to_string())
                ),
                ("flag".to_owned(), RecordedValue::Bool(true)),
                (
                    "list".to_owned(),
                    RecordedValue::Unrecognized("[1,2]".to_owned())
                ),
                ("Bad Key".to_owned(), RecordedValue::Text("x".to_owned())),
            ]
        );
    }

    #[test]
    fn unrecognized_values_are_cut() {
        let mut recorded = RecordedMetadata::new();

        recorded.push_unrecognized("blob", &"y".repeat(500));

        assert_eq!(
            recorded.entries(),
            [(
                "blob".to_owned(),
                RecordedValue::Unrecognized("y".repeat(256))
            )]
        );
    }

    /// A metadata key that never names a secret.
    fn key() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9_]{0,12}".prop_filter("no secret words", |key| !has_secret_word(key))
    }

    #[derive(Debug, Clone)]
    enum Value {
        Text(String),
        Int(i64),
        Bool(bool),
    }

    fn value() -> impl Strategy<Value = Value> {
        prop_oneof![
            any::<String>().prop_map(Value::Text),
            "[\"\\\\é€😀a\\n]{200,300}".prop_map(Value::Text),
            (-AuditMetadata::MAX_INT..=AuditMetadata::MAX_INT).prop_map(Value::Int),
            any::<bool>().prop_map(Value::Bool),
        ]
    }

    proptest! {
        /// The JSON writer agrees with serde_json byte for byte, so the size
        /// it measures is the size stored, and with valid keys no entry is
        /// ever refused: long texts are cut to fit.
        #[test]
        fn the_json_is_what_serde_json_writes(entries in proptest::collection::btree_map(key(), value(), 0..16)) {
            let mut metadata = AuditMetadata::new();
            for (key, value) in &entries {
                let next = match value {
                    Value::Text(text) => metadata.clone().text(key, text),
                    Value::Int(n) => metadata.clone().int(key, *n),
                    Value::Bool(b) => metadata.clone().flag(key, *b),
                };
                metadata = next.unwrap();
            }

            let mut expected = serde_json::Map::new();
            for (key, value) in metadata.iter() {
                let json = match value {
                    MetadataValue::Text(text) => serde_json::Value::from(text.as_str()),
                    MetadataValue::Int(n) => serde_json::Value::from(*n),
                    MetadataValue::Bool(b) => serde_json::Value::from(*b),
                };
                expected.insert(key.to_owned(), json);
            }
            let json = metadata.to_json();
            prop_assert_eq!(&json, &serde_json::to_string(&expected).unwrap());
            prop_assert!(json.len() <= AuditMetadata::MAX_JSON_BYTES);
        }
    }
}
