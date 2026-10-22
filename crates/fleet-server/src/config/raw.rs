//! The config as read, before validation: serde's view of `server.toml`.
//!
//! - Every struct refuses unknown keys. None uses `#[serde(flatten)]`, which
//!   would silently turn that off.
//! - Texts are plain `String`s and required keys are `Option`s, so validation
//!   can check all of them and report every problem at once.
//! - garde checks the numbers against the ranges below. Each range is wide
//!   enough for any real setup and catches typos.

use std::collections::BTreeMap;

use garde::Validate;
use serde::Deserialize;

use super::{AccountsConfig, Argon2Params, GrpcConfig, RateLimitConfig};

/// A range of a numeric key, inclusive at both ends.
pub(crate) struct Range {
    pub(crate) min: u64,
    pub(crate) max: u64,
}

/// `[http] request_timeout_secs`: at most 19 s, below `fleet-client`'s 20 s
/// for a whole request, so the client always gets the server's answer. A
/// slower one could let a refresh commit after the client gave up, and the
/// client's next refresh would then send the replaced token: reuse, which
/// ends the session (Plan.md Appendix A, ADR-0017).
pub(crate) const REQUEST_TIMEOUT_SECS: Range = Range { min: 1, max: 19 };
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
    #[serde(default)]
    #[garde(skip)]
    pub(crate) auth: RawAuth,
    #[serde(default)]
    #[garde(skip)]
    pub(crate) vault: RawVault,
    #[serde(default)]
    #[garde(dive)]
    pub(crate) rate_limits: RawRateLimits,
    #[serde(default)]
    #[garde(dive)]
    pub(crate) accounts: RawAccounts,
    #[serde(default)]
    #[garde(skip)]
    pub(crate) chat: RawChat,
    /// Absent means the control plane is off.
    #[serde(default)]
    #[garde(dive)]
    pub(crate) grpc: Option<RawGrpc>,
}

/// `[grpc]`. The four files are required once the section is there;
/// validation checks them. A missing `bind` is `127.0.0.1:7443`.
#[derive(Debug, Deserialize, Validate)]
#[serde(default, deny_unknown_fields, expecting = "a table of gRPC settings")]
pub(crate) struct RawGrpc {
    #[garde(skip)]
    pub(crate) bind: Option<String>,
    #[garde(skip)]
    pub(crate) ca_cert_file: Option<String>,
    #[garde(skip)]
    pub(crate) ca_key_file: Option<String>,
    #[garde(skip)]
    pub(crate) server_cert_file: Option<String>,
    #[garde(skip)]
    pub(crate) server_key_file: Option<String>,
    #[garde(range(min = GrpcConfig::AGENT_CERT_LIFETIME_DAYS.min, max = GrpcConfig::AGENT_CERT_LIFETIME_DAYS.max))]
    pub(crate) agent_cert_lifetime_days: u32,
    // Validation checks the heartbeat keys' ranges and their rule together.
    #[garde(skip)]
    pub(crate) heartbeat_interval_secs: u32,
    #[garde(skip)]
    pub(crate) heartbeat_timeout_secs: u32,
}

/// Appendix A's agent certificate lifetime.
const DEFAULT_AGENT_CERT_LIFETIME_DAYS: u32 = 60;

/// Appendix A's heartbeat interval.
const DEFAULT_HEARTBEAT_INTERVAL_SECS: u32 = 10;

/// Appendix A's heartbeat timeout.
const DEFAULT_HEARTBEAT_TIMEOUT_SECS: u32 = 30;

impl Default for RawGrpc {
    fn default() -> Self {
        Self {
            bind: None,
            ca_cert_file: None,
            ca_key_file: None,
            server_cert_file: None,
            server_key_file: None,
            agent_cert_lifetime_days: DEFAULT_AGENT_CERT_LIFETIME_DAYS,
            heartbeat_interval_secs: DEFAULT_HEARTBEAT_INTERVAL_SECS,
            heartbeat_timeout_secs: DEFAULT_HEARTBEAT_TIMEOUT_SECS,
        }
    }
}

/// `[vault]`. Both keys are required; validation checks the key IDs, the
/// count and the paths. The map's keys are text, as TOML and the
/// environment give them (`AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1`).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, expecting = "a table of vault settings")]
pub(crate) struct RawVault {
    pub(crate) master_key_files: Option<BTreeMap<String, String>>,
    pub(crate) active_key_id: Option<u64>,
}

/// `[auth]`. Missing keys take Appendix A's defaults, §7.2's baseline.
///
/// The argon2 fields drop the keys' `argon2_` prefix (clippy's
/// `struct_field_names`), and serde names the key. Every range is checked in
/// validation, not by garde, whose report would name the field instead of
/// the key (garde 0.23 parses `rename` but never applies it).
#[derive(Debug, Deserialize)]
#[serde(
    default,
    deny_unknown_fields,
    expecting = "a table of authentication settings"
)]
pub(crate) struct RawAuth {
    #[serde(rename = "argon2_memory_kib")]
    pub(crate) memory_kib: u32,
    #[serde(rename = "argon2_iterations")]
    pub(crate) iterations: u32,
    #[serde(rename = "argon2_parallelism")]
    pub(crate) parallelism: u32,
    #[serde(rename = "argon2_max_concurrent")]
    pub(crate) max_concurrent: u32,
    pub(crate) access_token_ttl_secs: u32,
    pub(crate) refresh_token_idle_days: u32,
    pub(crate) refresh_token_absolute_days: u32,
    pub(crate) lockout_threshold: u32,
    pub(crate) invite_ttl_hours: u32,
}

/// Appendix A's token lifetimes, §7.2's.
const DEFAULT_ACCESS_TOKEN_TTL_SECS: u32 = 900;
const DEFAULT_REFRESH_TOKEN_IDLE_DAYS: u32 = 7;
const DEFAULT_REFRESH_TOKEN_ABSOLUTE_DAYS: u32 = 30;
/// §7.4's lockout threshold.
const DEFAULT_LOCKOUT_THRESHOLD: u32 = 5;
/// §7.2's invite lifetime.
const DEFAULT_INVITE_TTL_HOURS: u32 = 72;

impl Default for RawAuth {
    fn default() -> Self {
        let baseline = Argon2Params::default();
        Self {
            memory_kib: baseline.memory_kib(),
            iterations: baseline.iterations(),
            parallelism: baseline.parallelism(),
            max_concurrent: baseline.max_concurrent(),
            access_token_ttl_secs: DEFAULT_ACCESS_TOKEN_TTL_SECS,
            refresh_token_idle_days: DEFAULT_REFRESH_TOKEN_IDLE_DAYS,
            refresh_token_absolute_days: DEFAULT_REFRESH_TOKEN_ABSOLUTE_DAYS,
            lockout_threshold: DEFAULT_LOCKOUT_THRESHOLD,
            invite_ttl_hours: DEFAULT_INVITE_TTL_HOURS,
        }
    }
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
    /// Checked entry by entry in validation.
    #[garde(skip)]
    pub(crate) trusted_proxies: Vec<String>,
}

impl Default for RawHttp {
    fn default() -> Self {
        Self {
            bind: None,
            request_timeout_secs: DEFAULT_REQUEST_TIMEOUT_SECS,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            trusted_proxies: Vec::new(),
        }
    }
}

/// `[rate_limits]`. Missing keys take Appendix A's defaults; the keys are
/// the field names, so garde's report names them.
#[derive(Debug, Deserialize, Validate)]
#[serde(
    default,
    deny_unknown_fields,
    expecting = "a table of rate-limit settings"
)]
pub(crate) struct RawRateLimits {
    #[garde(range(min = RateLimitConfig::PER_IP_PER_SEC.min, max = RateLimitConfig::PER_IP_PER_SEC.max))]
    pub(crate) per_ip_per_sec: u32,
    #[garde(range(min = RateLimitConfig::PER_IP_BURST.min, max = RateLimitConfig::PER_IP_BURST.max))]
    pub(crate) per_ip_burst: u32,
    #[garde(range(min = RateLimitConfig::LOGIN_PER_IP_PER_MIN.min, max = RateLimitConfig::LOGIN_PER_IP_PER_MIN.max))]
    pub(crate) login_per_ip_per_min: u32,
    #[garde(range(min = RateLimitConfig::REFRESH_PER_IP_PER_MIN.min, max = RateLimitConfig::REFRESH_PER_IP_PER_MIN.max))]
    pub(crate) refresh_per_ip_per_min: u32,
    #[garde(range(min = RateLimitConfig::REGISTER_PER_IP_PER_HOUR.min, max = RateLimitConfig::REGISTER_PER_IP_PER_HOUR.max))]
    pub(crate) register_per_ip_per_hour: u32,
    #[garde(range(min = RateLimitConfig::PER_USER_PER_SEC.min, max = RateLimitConfig::PER_USER_PER_SEC.max))]
    pub(crate) per_user_per_sec: u32,
    #[garde(range(min = RateLimitConfig::PER_USER_BURST.min, max = RateLimitConfig::PER_USER_BURST.max))]
    pub(crate) per_user_burst: u32,
    #[garde(range(min = RateLimitConfig::LINK_FLOWS_TOTAL.min, max = RateLimitConfig::LINK_FLOWS_TOTAL.max))]
    pub(crate) link_flows_total: u32,
    #[garde(range(min = RateLimitConfig::LINK_FLOWS_PER_USER_PER_DAY.min, max = RateLimitConfig::LINK_FLOWS_PER_USER_PER_DAY.max))]
    pub(crate) link_flows_per_user_per_day: u32,
    #[garde(range(min = RateLimitConfig::GRANTS_BY_NAME_PER_USER_PER_HOUR.min, max = RateLimitConfig::GRANTS_BY_NAME_PER_USER_PER_HOUR.max))]
    pub(crate) grants_by_name_per_user_per_hour: u32,
    #[garde(range(min = RateLimitConfig::ENROLL_PER_IP_PER_MIN.min, max = RateLimitConfig::ENROLL_PER_IP_PER_MIN.max))]
    pub(crate) enroll_per_ip_per_min: u32,
    #[garde(range(min = RateLimitConfig::CONNECT_PER_CERT_PER_MIN.min, max = RateLimitConfig::CONNECT_PER_CERT_PER_MIN.max))]
    pub(crate) connect_per_cert_per_min: u32,
    #[garde(range(min = RateLimitConfig::BOT_ACTIONS_PER_BOT_PER_MIN.min, max = RateLimitConfig::BOT_ACTIONS_PER_BOT_PER_MIN.max))]
    pub(crate) bot_actions_per_bot_per_min: u32,
    #[garde(range(min = RateLimitConfig::FRESH_SESSIONS_PER_BOT_PER_HOUR.min, max = RateLimitConfig::FRESH_SESSIONS_PER_BOT_PER_HOUR.max))]
    pub(crate) fresh_sessions_per_bot_per_hour: u32,
    #[garde(range(min = RateLimitConfig::CHAT_PER_BOT_INTERVAL_SECS.min, max = RateLimitConfig::CHAT_PER_BOT_INTERVAL_SECS.max))]
    pub(crate) chat_per_bot_interval_secs: u32,
    #[garde(range(min = RateLimitConfig::CHAT_PER_BOT_BURST.min, max = RateLimitConfig::CHAT_PER_BOT_BURST.max))]
    pub(crate) chat_per_bot_burst: u32,
    #[garde(range(min = RateLimitConfig::CHAT_PER_USER_PER_MIN.min, max = RateLimitConfig::CHAT_PER_USER_PER_MIN.max))]
    pub(crate) chat_per_user_per_min: u32,
    #[garde(range(min = RateLimitConfig::WS_CONNECTIONS_PER_USER.min, max = RateLimitConfig::WS_CONNECTIONS_PER_USER.max))]
    pub(crate) ws_connections_per_user: u32,
}

impl Default for RawRateLimits {
    fn default() -> Self {
        Self {
            per_ip_per_sec: 20,
            per_ip_burst: 40,
            login_per_ip_per_min: 5,
            refresh_per_ip_per_min: 30,
            register_per_ip_per_hour: 5,
            per_user_per_sec: 10,
            per_user_burst: 30,
            link_flows_total: 3,
            link_flows_per_user_per_day: 10,
            grants_by_name_per_user_per_hour: 10,
            enroll_per_ip_per_min: 5,
            connect_per_cert_per_min: 10,
            bot_actions_per_bot_per_min: 5,
            fresh_sessions_per_bot_per_hour: 6,
            chat_per_bot_interval_secs: 3,
            chat_per_bot_burst: 3,
            chat_per_user_per_min: 20,
            ws_connections_per_user: 3,
        }
    }
}

/// `[accounts]`. Missing keys take Appendix A's defaults; the keys are the
/// field names, so garde's report names them.
#[derive(Debug, Deserialize, Validate)]
#[serde(
    default,
    deny_unknown_fields,
    expecting = "a table of account settings"
)]
pub(crate) struct RawAccounts {
    #[garde(range(min = AccountsConfig::MAX_PER_MEMBER.min, max = AccountsConfig::MAX_PER_MEMBER.max))]
    pub(crate) max_per_member: u32,
    #[garde(skip)]
    pub(crate) allow_offline: bool,
}

impl Default for RawAccounts {
    fn default() -> Self {
        Self {
            max_per_member: 3,
            allow_offline: false,
        }
    }
}

/// `[chat]`. A missing `command_allowlist` is empty: every command needs
/// Manage.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields, expecting = "a table of chat settings")]
pub(crate) struct RawChat {
    pub(crate) command_allowlist: Vec<String>,
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
    /// their full path, each with the reason.
    const NOT_SECRETS: &[(&str, &str)] = &[
        (
            "vault.active_key_id",
            "the ID of the key new secrets are sealed with, an integer; the keys are in master_key_files",
        ),
        (
            "auth.access_token_ttl_secs",
            "how long an access token lives, a number of seconds; tokens are minted at runtime and never configured",
        ),
        (
            "auth.refresh_token_idle_days",
            "how long a refresh token lives unused, a number of days; tokens are never configured",
        ),
        (
            "auth.refresh_token_absolute_days",
            "how long a session lives at most, a number of days; tokens are never configured",
        ),
    ];

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
    fn the_walk_finds_every_key() {
        let keys = every_key();

        for key in [
            "dev_mode",
            "http",
            "http.bind",
            "http.request_timeout_secs",
            "http.max_body_bytes",
            "http.trusted_proxies",
            "database",
            "database.path",
            "log",
            "log.format",
            "log.filter",
            "auth",
            "auth.argon2_memory_kib",
            "auth.argon2_iterations",
            "auth.argon2_parallelism",
            "auth.argon2_max_concurrent",
            "auth.access_token_ttl_secs",
            "auth.refresh_token_idle_days",
            "auth.refresh_token_absolute_days",
            "auth.lockout_threshold",
            "auth.invite_ttl_hours",
            "vault",
            "vault.master_key_files",
            "vault.active_key_id",
            "rate_limits",
            "rate_limits.per_ip_per_sec",
            "rate_limits.per_ip_burst",
            "rate_limits.login_per_ip_per_min",
            "rate_limits.refresh_per_ip_per_min",
            "rate_limits.register_per_ip_per_hour",
            "rate_limits.per_user_per_sec",
            "rate_limits.per_user_burst",
            "rate_limits.link_flows_total",
            "rate_limits.link_flows_per_user_per_day",
            "rate_limits.grants_by_name_per_user_per_hour",
            "rate_limits.enroll_per_ip_per_min",
            "rate_limits.connect_per_cert_per_min",
            "rate_limits.bot_actions_per_bot_per_min",
            "rate_limits.fresh_sessions_per_bot_per_hour",
            "rate_limits.chat_per_bot_interval_secs",
            "rate_limits.chat_per_bot_burst",
            "rate_limits.chat_per_user_per_min",
            "rate_limits.ws_connections_per_user",
            "accounts",
            "accounts.max_per_member",
            "accounts.allow_offline",
            "chat",
            "chat.command_allowlist",
            "grpc",
            "grpc.bind",
            "grpc.ca_cert_file",
            "grpc.ca_key_file",
            "grpc.server_cert_file",
            "grpc.server_key_file",
            "grpc.agent_cert_lifetime_days",
            "grpc.heartbeat_interval_secs",
            "grpc.heartbeat_timeout_secs",
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
