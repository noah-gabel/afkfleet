//! The config as read, before validation: serde's view of `server.toml`.
//!
//! - Every struct refuses unknown keys. None uses `#[serde(flatten)]`, which
//!   would silently turn that off.
//! - Texts are plain `String`s and required keys are `Option`s, so validation
//!   can check all of them and report every problem at once.
//! - garde checks the numbers against the ranges below. Each range is wide
//!   enough for any real setup and catches typos.

use garde::Validate;
use serde::Deserialize;

/// A range of a numeric key, inclusive at both ends.
pub(crate) struct Range {
    pub(crate) min: u64,
    pub(crate) max: u64,
}

/// `[http] request_timeout_secs`.
pub(crate) const REQUEST_TIMEOUT_SECS: Range = Range { min: 1, max: 300 };
/// `[http] max_body_bytes`: 1 KiB to 1 MiB, so the body limit stays a guard
/// against oversized requests even when it's raised.
pub(crate) const MAX_BODY_BYTES: Range = Range {
    min: 1024,
    max: 1_048_576,
};

/// Appendix A's `[http]` defaults.
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 15;
const DEFAULT_MAX_BODY_BYTES: u64 = 65_536;

/// The whole file.
#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields, expecting = "a table of server settings")]
pub(crate) struct RawConfig {
    #[serde(default)]
    #[garde(skip)]
    pub(crate) dev_mode: bool,
    #[serde(default)]
    #[garde(dive)]
    pub(crate) http: RawHttp,
    #[serde(default)]
    #[garde(skip)]
    pub(crate) database: RawDatabase,
    #[serde(default)]
    #[garde(skip)]
    pub(crate) log: RawLog,
}

/// `[http]`. Missing keys take Appendix A's defaults; a missing `bind` is
/// `127.0.0.1:8080`.
#[derive(Debug, Deserialize, Validate)]
#[serde(default, deny_unknown_fields, expecting = "a table of HTTP settings")]
pub(crate) struct RawHttp {
    #[garde(skip)]
    pub(crate) bind: Option<String>,
    #[garde(range(min = REQUEST_TIMEOUT_SECS.min, max = REQUEST_TIMEOUT_SECS.max))]
    pub(crate) request_timeout_secs: u64,
    #[garde(range(min = MAX_BODY_BYTES.min, max = MAX_BODY_BYTES.max))]
    pub(crate) max_body_bytes: u64,
}

impl Default for RawHttp {
    fn default() -> Self {
        Self {
            bind: None,
            request_timeout_secs: DEFAULT_REQUEST_TIMEOUT_SECS,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        }
    }
}

/// `[database]`. `path` is required.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, expecting = "a table of database settings")]
pub(crate) struct RawDatabase {
    pub(crate) path: Option<String>,
}

/// `[log]`. Missing keys take their defaults (`json`, `info`).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, expecting = "a table of log settings")]
pub(crate) struct RawLog {
    pub(crate) format: Option<String>,
    pub(crate) filter: Option<String>,
}

/// The rule that keeps secrets out of the config file (security rule 4): a
/// key whose name says it holds a secret must name a file instead. The test
/// below walks every key of [`RawConfig`] through serde's own field lists, so
/// a new key can't escape it.
#[cfg(test)]
mod secret_names {
    use rstest::rstest;
    use serde::Deserialize as _;
    use serde::de::value::Error;
    use serde::de::{
        self, DeserializeSeed, Deserializer, IntoDeserializer, MapAccess, SeqAccess, Visitor,
    };

    use super::RawConfig;

    /// Keys that match fleet-core's `SECRET_WORDS` but hold no secret, by
    /// their full path, each with the reason. Empty in Phase 6.
    const NOT_SECRETS: &[(&str, &str)] = &[];

    /// Whether one of the name's `_`-separated parts is a secret word. The
    /// list lives in fleet-core, shared with the audit log's metadata keys
    /// (ADR-0015), so a word added there covers both.
    fn looks_secret(name: &str) -> bool {
        fleet_core::audit::has_secret_word(name)
    }

    /// Whether the name says the key holds a path to a file, or a map of
    /// them.
    fn names_a_file(name: &str) -> bool {
        name.ends_with("_file") || name.ends_with("_files")
    }

    /// Every key of the raw config, as dotted paths, sections included.
    fn every_key() -> Vec<String> {
        let mut keys = Vec::new();
        RawConfig::deserialize(Walker {
            path: String::new(),
            keys: &mut keys,
        })
        .expect("the walker knows every shape the raw config uses");
        keys
    }

    /// A deserializer that reads nothing: it hands serde's derive a dummy
    /// value of every shape it asks for, and records the field names each
    /// struct asks for, which are exactly the keys it accepts. Options and
    /// lists are entered, so a section inside one is walked too. Shapes the
    /// raw config doesn't use (enums, untagged types) fail the walk, so no
    /// key can hide behind one.
    struct Walker<'a> {
        path: String,
        keys: &'a mut Vec<String>,
    }

    impl Walker<'_> {
        fn unknown_shape() -> Error {
            de::Error::custom("a serde shape the key walker doesn't know")
        }
    }

    impl<'de> Deserializer<'de> for Walker<'_> {
        type Error = Error;

        fn deserialize_struct<V: Visitor<'de>>(
            self,
            _name: &'static str,
            fields: &'static [&'static str],
            visitor: V,
        ) -> Result<V::Value, Error> {
            visitor.visit_map(StructKeys {
                names: fields.iter(),
                value_path: None,
                path: self.path,
                keys: self.keys,
            })
        }

        fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            visitor.visit_some(self)
        }

        fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            visitor.visit_seq(One(Some(Walker {
                path: format!("{}[0]", self.path),
                keys: self.keys,
            })))
        }

        fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            // A map's keys are data (e.g. key IDs), not config keys.
            visitor.visit_map(NoEntries)
        }

        fn deserialize_newtype_struct<V: Visitor<'de>>(
            self,
            _name: &'static str,
            visitor: V,
        ) -> Result<V::Value, Error> {
            visitor.visit_newtype_struct(self)
        }

        fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            visitor.visit_bool(false)
        }

        fn deserialize_u8<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            visitor.visit_u8(0)
        }

        fn deserialize_u16<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            visitor.visit_u16(0)
        }

        fn deserialize_u32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            visitor.visit_u32(0)
        }

        fn deserialize_u64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            visitor.visit_u64(0)
        }

        fn deserialize_i8<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            visitor.visit_i8(0)
        }

        fn deserialize_i16<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            visitor.visit_i16(0)
        }

        fn deserialize_i32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            visitor.visit_i32(0)
        }

        fn deserialize_i64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            visitor.visit_i64(0)
        }

        fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            visitor.visit_f32(0.0)
        }

        fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            visitor.visit_f64(0.0)
        }

        fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            visitor.visit_char('x')
        }

        fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            visitor.visit_str("")
        }

        fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            visitor.visit_string(String::new())
        }

        fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            visitor.visit_str("")
        }

        fn deserialize_any<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value, Error> {
            Err(Self::unknown_shape())
        }

        fn deserialize_enum<V: Visitor<'de>>(
            self,
            _name: &'static str,
            _variants: &'static [&'static str],
            _visitor: V,
        ) -> Result<V::Value, Error> {
            Err(Self::unknown_shape())
        }

        serde::forward_to_deserialize_any! {
            i128 u128 bytes byte_buf unit unit_struct tuple tuple_struct ignored_any
        }
    }

    /// A struct's fields, one key each, in the order serde lists them.
    struct StructKeys<'a> {
        names: core::slice::Iter<'static, &'static str>,
        value_path: Option<String>,
        path: String,
        keys: &'a mut Vec<String>,
    }

    impl<'de> MapAccess<'de> for StructKeys<'_> {
        type Error = Error;

        fn next_key_seed<K: DeserializeSeed<'de>>(
            &mut self,
            seed: K,
        ) -> Result<Option<K::Value>, Error> {
            let Some(field) = self.names.next() else {
                return Ok(None);
            };
            let path = if self.path.is_empty() {
                (*field).to_owned()
            } else {
                format!("{}.{field}", self.path)
            };
            self.keys.push(path.clone());
            self.value_path = Some(path);
            seed.deserialize(IntoDeserializer::<Error>::into_deserializer(*field))
                .map(Some)
        }

        fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
            let path = self.value_path.take().unwrap_or_default();
            seed.deserialize(Walker {
                path,
                keys: self.keys,
            })
        }
    }

    /// A list with one element, so a list of tables is walked once.
    struct One<'a>(Option<Walker<'a>>);

    impl<'de> SeqAccess<'de> for One<'_> {
        type Error = Error;

        fn next_element_seed<T: DeserializeSeed<'de>>(
            &mut self,
            seed: T,
        ) -> Result<Option<T::Value>, Error> {
            self.0
                .take()
                .map(|walker| seed.deserialize(walker))
                .transpose()
        }
    }

    /// An empty map.
    struct NoEntries;

    impl<'de> MapAccess<'de> for NoEntries {
        type Error = Error;

        fn next_key_seed<K: DeserializeSeed<'de>>(
            &mut self,
            _seed: K,
        ) -> Result<Option<K::Value>, Error> {
            Ok(None)
        }

        fn next_value_seed<V: DeserializeSeed<'de>>(
            &mut self,
            _seed: V,
        ) -> Result<V::Value, Error> {
            Err(de::Error::custom("an empty map has no values"))
        }
    }

    #[test]
    fn the_walk_finds_every_phase_6_key() {
        let keys = every_key();

        for key in [
            "dev_mode",
            "http",
            "http.bind",
            "http.request_timeout_secs",
            "http.max_body_bytes",
            "database",
            "database.path",
            "log",
            "log.format",
            "log.filter",
        ] {
            assert!(keys.iter().any(|found| found == key), "{key} in {keys:?}");
        }
    }

    #[test]
    fn no_key_holds_a_secret_unless_it_names_a_file() {
        let offenders: Vec<String> = every_key()
            .into_iter()
            .filter(|key| {
                let name = key.rsplit('.').next().unwrap_or(key);
                looks_secret(name)
                    && !names_a_file(name)
                    && !NOT_SECRETS.iter().any(|(allowed, _)| allowed == key)
            })
            .collect();

        assert!(
            offenders.is_empty(),
            "keys that would hold a secret: {offenders:?}"
        );
    }

    #[test]
    fn every_exception_is_a_real_key_with_a_reason() {
        let keys = every_key();

        for (key, reason) in NOT_SECRETS {
            assert!(keys.iter().any(|found| found == key), "{key} isn't a key");
            assert!(!reason.trim().is_empty(), "{key} has no reason");
        }
    }

    #[rstest]
    #[case::vault_key("vault_key", true)]
    #[case::api_keys("api_keys", true)]
    #[case::admin_password("admin_password", true)]
    #[case::secret("secret", true)]
    #[case::passphrase("vault_passphrase", true)]
    #[case::pepper("hash_pepper", true)]
    #[case::credentials("db_credentials", true)]
    #[case::token_ttl("access_token_ttl_secs", true)]
    #[case::key_file("ca_key_file", false)]
    #[case::key_files("master_key_files", false)]
    #[case::password_file("admin_password_file", false)]
    #[case::keep("backup_keep", false)]
    #[case::monkey("monkey", false)]
    #[case::bind("bind", false)]
    fn the_rule_flags_secret_names_that_name_no_file(#[case] name: &str, #[case] flagged: bool) {
        assert_eq!(looks_secret(name) && !names_a_file(name), flagged, "{name}");
    }
}
