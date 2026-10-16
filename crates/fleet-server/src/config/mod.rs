//! The server's config: `server.toml` plus `AFKFLEET_SERVER__…` environment
//! variables (Plan.md P6.2, Appendix A, ADR-0015).
//!
//! [`load`] reads the file at the exact path it's given, lets environment
//! variables override any key (`AFKFLEET_SERVER__HTTP__BIND=0.0.0.0:8080`),
//! and validates everything into a [`ServerConfig`]. The loader and the
//! error types are fleet-startup's, shared with the agent.
//!
//! - **Unknown keys are errors,** in the file and in the environment, so a
//!   typo can't silently keep a default.
//! - **Parse errors stop at the first** (bad TOML, a wrong type, an unknown
//!   key): nothing can be checked after them.
//! - **Validation reports every problem at once,** each with its key path
//!   (`http.bind`), and never echoes a value.
//! - **Each phase adds its own keys,** so a key of a later phase is still an
//!   unknown key (Appendix A). Phase 7 adds `[auth]`, `[vault]`,
//!   `[rate_limits]` and `[http] trusted_proxies` (ADR-0016); Phase 9 adds
//!   `[accounts]` and the link flows' rate limits (ADR-0018); Phase 10 adds
//!   the optional `[grpc]` (ADR-0019).
//! - **Secrets never go in this file.** A key for a secret names the file
//!   that holds it and ends in `_file` or `_files`; a test checks every
//!   key's name (security rule 4). fleet-startup's `secret_file` reads those
//!   files, and only `serve` reads them (ADR-0016).

mod accounts;
mod auth;
mod error;
mod rate_limits;
mod raw;
mod validate;

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::time::Duration;

use raw::RawConfig;

use crate::http::rate_limit::TrustedProxies;

pub use accounts::AccountsConfig;
pub use auth::{Argon2Params, Argon2ParamsError, AuthConfig, Bounds};
pub use error::{ConfigError, Problem, ProblemKind, Problems};
pub use fleet_startup::config::{
    KeyPath, KeySegment, LogConfig, LogFilter, LogFilterError, LogFormat, ParseError, ParseProblem,
    ParseSource, UnknownLogFormatError,
};
pub use rate_limits::{LinkFlowLimits, Rate, RateLimitConfig};

/// The prefix of the environment variables that override config keys:
/// `AFKFLEET_SERVER__` plus the key path in capitals, with `__` between the
/// parts (`AFKFLEET_SERVER__HTTP__BIND`).
pub const ENV_PREFIX: &str = "AFKFLEET_SERVER__";

/// The server's validated config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// `dev_mode`: turns on development-only features. Off by default, and
    /// the server warns at startup when it's on.
    pub dev_mode: bool,
    /// How the server serves HTTP (`[http]`).
    pub http: HttpConfig,
    /// Where the server keeps its data (`[database]`).
    pub database: DatabaseConfig,
    /// How the server authenticates users (`[auth]`).
    pub auth: AuthConfig,
    /// The vault's keys (`[vault]`).
    pub vault: VaultConfig,
    /// The request limits (`[rate_limits]`).
    pub rate_limits: RateLimitConfig,
    /// The linked Minecraft accounts (`[accounts]`).
    pub accounts: AccountsConfig,
    /// How the server logs (`[log]`).
    pub log: LogConfig,
    /// The agents' control plane (`[grpc]`), or `None` when the section is
    /// absent: then serve has no gRPC listener and reads no PKI file.
    pub grpc: Option<GrpcConfig>,
}

/// `[grpc]`: the agents' control plane, its listener and its PKI (Plan.md
/// P10.2, Appendix A; ADR-0019). Optional; when it's there, its four files
/// are required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrpcConfig {
    /// The address the gRPC listener binds: an IP address and a port other
    /// than 0 and other than `[http] bind`'s. Defaults to
    /// [`GrpcConfig::DEFAULT_BIND`].
    pub bind: SocketAddr,
    /// The PKI's files, which `pki init` writes and serve reads.
    pub pki: PkiPaths,
    /// How long an agent's certificate is valid
    /// (`agent_cert_lifetime_days`, 1–365 days, default 60), read when a
    /// certificate is signed.
    pub agent_cert_lifetime: Duration,
}

impl GrpcConfig {
    /// `127.0.0.1:7443`: a server started without `bind` takes agents only
    /// from this machine. A container sets `0.0.0.0:7443` itself.
    pub const DEFAULT_BIND: SocketAddr =
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 7443));
    /// `agent_cert_lifetime_days`: 1 to 365. Default 60.
    pub const AGENT_CERT_LIFETIME_DAYS: Bounds = Bounds { min: 1, max: 365 };
}

/// The PKI's four files (`[grpc]`), each an absolute path and none the same
/// as another. The keys are secrets, read through fleet-startup's
/// `secret_file`; so are the certificates, which follow the same rules
/// (ADR-0019).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkiPaths {
    /// `ca_cert_file`: the CA's certificate, which agents trust.
    pub ca_cert_file: PathBuf,
    /// `ca_key_file`: the CA's private key, its own secret.
    pub ca_key_file: PathBuf,
    /// `server_cert_file`: the gRPC listener's certificate.
    pub server_cert_file: PathBuf,
    /// `server_key_file`: the gRPC listener's private key.
    pub server_key_file: PathBuf,
}

/// `[http]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpConfig {
    /// The address the HTTP API listens on: an IP address and a port other
    /// than 0. Defaults to [`HttpConfig::DEFAULT_BIND`].
    pub bind: SocketAddr,
    /// How long a request may take before it's answered with a timeout
    /// (`request_timeout_secs`, 1–19 s, default 15 s). It stays below
    /// `fleet-client`'s 20 s for a whole request, so a refresh never commits
    /// after the client gave up on it (Plan.md Appendix A, ADR-0017).
    pub request_timeout: Duration,
    /// The largest request body the server reads (`max_body_bytes`,
    /// 1 KiB–1 MiB, default 64 KiB).
    pub max_body_bytes: usize,
    /// The proxies whose `X-Forwarded-For` the server believes
    /// (`trusted_proxies`): at most [`TrustedProxies::MAX`] CIDR networks,
    /// none by default.
    pub trusted_proxies: TrustedProxies,
}

impl HttpConfig {
    /// `127.0.0.1:8080`: a server started without `bind` listens only on
    /// this machine. A container sets `0.0.0.0:8080` itself.
    pub const DEFAULT_BIND: SocketAddr =
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080));
}

/// `[vault]`: the files holding the vault's keys (ADR-0016). Required; only
/// `serve` reads the files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultConfig {
    /// `master_key_files`: each key ID (1 to 4294967295) and the absolute path
    /// of the file holding its key; 1 to [`VaultConfig::MAX_KEY_FILES`] of them.
    pub key_files: BTreeMap<u32, PathBuf>,
    /// `active_key_id`: the key new secrets are sealed with, one of
    /// `key_files`.
    pub active_key_id: u32,
}

impl VaultConfig {
    /// The most key files `master_key_files` may list.
    pub const MAX_KEY_FILES: usize = 16;
}

/// `[database]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseConfig {
    /// The SQLite file. Required and absolute, so a server started from
    /// another directory can't silently create a fresh, empty database.
    pub path: PathBuf,
}

impl ServerConfig {
    /// Logs one warning when dev mode is on, so a production server can't
    /// run in dev mode unnoticed. The server calls it right after logging
    /// starts.
    pub fn warn_if_dev_mode(&self) {
        if self.dev_mode {
            tracing::warn!(
                "dev mode is on: development-only features are enabled; never run a production \
                 server in dev mode"
            );
        }
    }
}

/// Loads the config from the file at `path`, then from `AFKFLEET_SERVER__…`
/// environment variables, and validates it.
///
/// The file must exist at exactly this path; parent directories aren't
/// searched.
///
/// # Errors
/// - [`ConfigError::NotFound`](fleet_startup::config::ConfigError::NotFound)
///   when there's no file at `path`.
/// - [`ConfigError::Parse`](fleet_startup::config::ConfigError::Parse) for
///   the first thing that can't be read: bad TOML, a value of the wrong
///   type, or an unknown key.
/// - [`ConfigError::Invalid`](fleet_startup::config::ConfigError::Invalid)
///   with every problem validation finds.
pub fn load(path: &Path) -> Result<ServerConfig, ConfigError> {
    let raw = fleet_startup::config::extract::<RawConfig, ProblemKind>(path, ENV_PREFIX)?;
    validate::validate(&raw)
}
