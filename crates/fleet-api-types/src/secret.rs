//! [`SecretText`]: a secret in a request or a response (Plan.md P7.6, P7.7,
//! ADR-0016): a password, a token, a code, a TOTP secret, an otpauth URI or
//! its QR code, a recovery code.
//!
//! It's a plain JSON string on the wire, but in Rust it can't leak by
//! accident:
//! - **`Debug` prints `[redacted]`**, so a DTO that derives `Debug` never
//!   shows it, in a log line, a panic or a failed assertion.
//! - **No `Display`, `PartialEq`, `Hash` or `Deref`.** The text is read only
//!   through [`SecretText::expose`] or taken with [`SecretText::into_secret`],
//!   so every use is greppable.
//! - **It's wiped on drop.** The request body it was read from isn't (axum
//!   buffers it), so this narrows the copies, not removes them.
//!
//! Like [`BoundedText`](crate::BoundedText), it has no TypeScript type or
//! `OpenAPI` name of its own: every field holding one carries
//! `#[ts(type = "string")]` and `#[schema(schema_with =
//! SecretText::<MAX>::schema)]`, a string of at most `MAX` characters. `MAX`
//! is what the field may hold; a request's garde rule enforces it with
//! `length(chars, max = …)`, whose message names the limit, never the text.

use core::fmt;

use garde::rules::length::HasChars;
use secrecy::SecretString;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use utoipa::PartialSchema;
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use zeroize::Zeroizing;

/// A secret text of at most `MAX` characters (the request's garde rule
/// checks the limit). See the [module docs](self).
pub struct SecretText<const MAX: usize>(Zeroizing<String>);

impl<const MAX: usize> SecretText<MAX> {
    /// Wraps `text`, for a response.
    #[must_use]
    pub fn new(text: String) -> Self {
        Self(Zeroizing::new(text))
    }

    /// The text. Every caller is a place the secret is read.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The text as a [`SecretString`], for the server. It's copied into an
    /// allocation of exactly its length, and this one is wiped.
    #[must_use]
    pub fn into_secret(self) -> SecretString {
        SecretString::from(self.expose())
    }
}

/// Redacted.
impl<const MAX: usize> fmt::Debug for SecretText<MAX> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl<const MAX: usize> Serialize for SecretText<MAX> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.expose())
    }
}

impl<'de, const MAX: usize> Deserialize<'de> for SecretText<MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::new)
    }
}

/// For garde's `length(chars, …)` rule: the text's Unicode scalar values.
impl<const MAX: usize> HasChars for SecretText<MAX> {
    fn num_chars(&self) -> usize {
        self.expose().chars().count()
    }
}

/// A string of at most `MAX` characters.
impl<const MAX: usize> PartialSchema for SecretText<MAX> {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .max_length(Some(MAX))
            .into()
    }
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret as _;
    use serde_json::json;

    use super::*;

    type Short = SecretText<8>;

    #[test]
    fn it_is_a_plain_json_string_both_ways() {
        let text: Short = serde_json::from_str(r#""hunter2""#).unwrap();

        assert_eq!(text.expose(), "hunter2");
        assert_eq!(serde_json::to_value(&text).unwrap(), json!("hunter2"));
    }

    #[test]
    fn reading_a_value_that_isnt_a_string_fails() {
        for json in ["42", "null", "{}", "[]"] {
            assert!(serde_json::from_str::<Short>(json).is_err(), "{json}");
        }
    }

    #[test]
    fn debug_never_shows_the_text() {
        let text = Short::new("hunter2".to_owned());

        assert_eq!(format!("{text:?}"), "[redacted]");
        assert_eq!(format!("{text:#?}"), "[redacted]");
    }

    #[test]
    fn the_server_takes_it_as_a_secret_string() {
        let text = Short::new("hunter2".to_owned());

        assert_eq!(text.into_secret().expose_secret(), "hunter2");
    }

    #[test]
    fn garde_counts_characters_not_bytes() {
        assert_eq!(Short::new("äöü".to_owned()).num_chars(), 3);
    }

    #[test]
    fn its_schema_is_a_string_capped_at_max() {
        assert_eq!(
            serde_json::to_value(Short::schema()).unwrap(),
            json!({ "type": "string", "maxLength": 8 })
        );
    }
}
