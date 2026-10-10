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
//! - **Only Phase 6's keys exist.** Each later phase adds its own, so a key
//!   of a later phase is still an unknown key (Appendix A).
//! - **Secrets never go in this file.** A key for a secret names the file
//!   that holds it and ends in `_file` or `_files`; a test checks every
//!   key's name (security rule 4). P7.4 builds the loader for those files.

mod error;
mod raw;
mod validate;

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::time::Duration;

use raw::RawConfig;

pub use error::{ConfigError, Problem, ProblemKind, Problems};
pub use fleet_startup::config::{
    KeyPath, KeySegment, LogConfig, LogFilter, LogFilterError, LogFormat, ParseError, ParseProblem,
    ParseSource, UnknownLogFormatError,
};

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
    /// How the server logs (`[log]`).
    pub log: LogConfig,
}

/// `[http]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpConfig {
    /// The address the HTTP API listens on: an IP address and a port other
    /// than 0. Defaults to [`HttpConfig::DEFAULT_BIND`].
    pub bind: SocketAddr,
    /// How long a request may take before it's answered with a timeout
    /// (`request_timeout_secs`, 1–300 s, default 15 s).
    pub request_timeout: Duration,
    /// The largest request body the server reads (`max_body_bytes`,
    /// 1 KiB–1 MiB, default 64 KiB).
    pub max_body_bytes: usize,
}

impl HttpConfig {
    /// `127.0.0.1:8080`: a server started without `bind` listens only on
    /// this machine. A container sets `0.0.0.0:8080` itself.
    pub const DEFAULT_BIND: SocketAddr =
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080));
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
