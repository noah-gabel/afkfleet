//! The agent's config: `agent.toml` plus `AFKFLEET_AGENT__…` environment
//! variables (Plan.md Appendix A, ADR-0014).
//!
//! [`load`] reads the file at the exact path it's given, lets environment
//! variables override any key (`AFKFLEET_AGENT__RUNTIME__MAX_BOTS=20`), and
//! validates everything into an [`AgentConfig`]: the settings the runtime and
//! the Minecraft adapter take, and the bots of a standalone agent.
//!
//! - **Unknown keys are errors,** in the file and in the environment, so a
//!   typo can't silently keep a default.
//! - **Parse errors stop at the first** (bad TOML, a wrong type, an unknown
//!   key): nothing can be checked after them.
//! - **Validation reports every problem at once,** each with its key path
//!   (`standalone.bots[1].username`), and never echoes a value.
//! - **Standalone accounts are offline accounts** by construction: a bot
//!   entry only has a `username`.
//!
//! Secrets never go in this file; Phase 10's keys only name files.

mod error;
mod log;
mod raw;
mod validate;

use std::path::{Path, PathBuf};

use figment::providers::{Env, Format, Toml};
use figment::{Figment, Provider};
use fleet_core::bot::BotAccount;
use fleet_core::disconnect::ConflictTexts;
use fleet_core::mode::ModePreset;
use fleet_core::resilience::{CircuitPolicy, RetryPolicy};
use fleet_core::value::{AgentName, McUsername, ServerAddress};
use fleet_mc::McConfig;
use fleet_runtime::RuntimeConfig;
use raw::RawConfig;

pub use error::{
    ConfigError, KeyPath, KeySegment, ParseError, ParseProblem, ParseSource, Problem, ProblemKind,
    Problems,
};
pub use log::{LogConfig, LogFilter, LogFilterError, LogFormat, UnknownLogFormatError};

/// The prefix of the environment variables that override config keys:
/// `AFKFLEET_AGENT__` plus the key path in capitals, with `__` between the
/// parts (`AFKFLEET_AGENT__RUNTIME__MAX_BOTS`).
pub const ENV_PREFIX: &str = "AFKFLEET_AGENT__";

/// The file name of the default heartbeat file, in the OS's temp directory.
pub const DEFAULT_HEARTBEAT_FILE_NAME: &str = "afkfleet-agent.alive";

/// The agent's validated config.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentConfig {
    /// The agent's name, for its logs and later the app.
    pub name: AgentName,
    /// The runtime's settings; `[runtime]` sets the connect, watchdog,
    /// liveness and shutdown timeouts and `max_bots`.
    pub runtime: RuntimeConfig,
    /// The Minecraft adapter's settings; `[runtime] max_abandoned_threads`
    /// sets the abandoned-thread limit.
    pub mc: McConfig,
    /// The reconnect backoff (`[retry]`).
    pub retry: RetryPolicy,
    /// The circuit breaker (`[retry] circuit_*`).
    pub circuit: CircuitPolicy,
    /// The file the agent touches while it's healthy (P5.5); an absolute path.
    pub heartbeat_file: PathBuf,
    /// How the agent logs (`[log]`).
    pub log: LogConfig,
    /// Standalone or managed.
    pub mode: AgentMode,
}

/// Where the agent's bots come from.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentMode {
    /// `[standalone]`: the config lists the bots (development only, offline
    /// accounts). At least one, at most `[runtime] max_bots`.
    Standalone(Vec<StandaloneBot>),
    /// `[control_plane]`: the server assigns the bots (Phase 10).
    ControlPlane(ControlPlaneConfig),
}

/// One `[[standalone.bots]]` entry. It has no ID yet: the agent mints one
/// at every start (P5.3).
#[derive(Debug, Clone, PartialEq)]
pub struct StandaloneBot {
    /// The offline account's name.
    pub username: McUsername,
    /// The server to join.
    pub server: ServerAddress,
    /// The preset mode it runs.
    pub mode: ModePreset,
    /// Kick texts that count as a duplicate login (empty by default).
    pub conflict_texts: ConflictTexts,
}

impl StandaloneBot {
    /// The bot's account: always an offline one.
    #[must_use]
    pub fn account(&self) -> BotAccount {
        BotAccount::Offline(self.username.clone())
    }
}

/// `[control_plane]`: how a managed agent reaches the server. Phase 5 only
/// checks that the keys are there; Phase 10 checks the URL and the files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlPlaneConfig {
    /// The server's gRPC URL.
    pub url: String,
    /// The CA certificate that signs the server's certificate.
    pub ca_cert_file: PathBuf,
    /// The agent's client certificate.
    pub cert_file: PathBuf,
    /// The agent's private key.
    pub key_file: PathBuf,
}

/// Loads the config from the file at `path`, then from `AFKFLEET_AGENT__…`
/// environment variables, and validates it.
///
/// The file must exist at exactly this path; parent directories aren't
/// searched.
///
/// # Errors
/// - [`ConfigError::NotFound`] when there's no file at `path`.
/// - [`ConfigError::Parse`] for the first thing that can't be read: bad
///   TOML, a value of the wrong type, or an unknown key.
/// - [`ConfigError::Invalid`] with every problem validation finds.
pub fn load(path: &Path) -> Result<AgentConfig, ConfigError> {
    // figment treats a missing file as an empty one, so check first.
    if !path.is_file() {
        return Err(ConfigError::NotFound {
            path: path.to_owned(),
        });
    }
    let env = Env::prefixed(ENV_PREFIX).split("__");
    let env_name = env.metadata().name.into_owned();
    let raw: RawConfig = Figment::new()
        .merge(Toml::file_exact(path))
        .merge(env)
        .extract()
        .map_err(|error| {
            ConfigError::Parse(Box::new(ParseError::from_figment(&error, &env_name)))
        })?;
    validate::validate(&raw)
}
