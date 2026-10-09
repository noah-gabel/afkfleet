//! The config as read, before validation: serde's view of `agent.toml`.
//!
//! - Every struct refuses unknown keys. None uses `#[serde(flatten)]`, which
//!   would silently turn that off.
//! - Texts are plain `String`s and required keys are `Option`s, so validation
//!   can check all of them and report every problem at once.
//! - garde checks the numbers against the ranges below. Each range is wide
//!   enough for any real setup and catches typos; the upper bounds also keep
//!   every duration far from overflowing.

use std::path::PathBuf;

use fleet_mc::McConfig;
use fleet_runtime::RuntimeConfig;
use garde::Validate;
use serde::Deserialize;

/// A range of a numeric key, inclusive at both ends.
pub(crate) struct Range {
    pub(crate) min: u64,
    pub(crate) max: u64,
}

/// `[runtime] max_bots`.
pub(crate) const MAX_BOTS: Range = Range { min: 1, max: 1000 };
/// `[runtime] watchdog_timeout_secs` and `packet_liveness_timeout_secs`.
pub(crate) const STALL_TIMEOUT_SECS: Range = Range { min: 5, max: 600 };
/// `[runtime] connect_timeout_secs`.
pub(crate) const CONNECT_TIMEOUT_SECS: Range = Range { min: 5, max: 300 };
/// `[runtime] max_abandoned_threads`.
pub(crate) const MAX_ABANDONED_THREADS: Range = Range { min: 1, max: 100 };
/// `[runtime] shutdown_timeout_secs`.
pub(crate) const SHUTDOWN_TIMEOUT_SECS: Range = Range { min: 1, max: 300 };
/// `[retry] base_delay_secs`.
pub(crate) const BASE_DELAY_SECS: Range = Range { min: 1, max: 3600 };
/// `[retry] max_delay_secs`. fleet-core also wants at least twice the base.
pub(crate) const MAX_DELAY_SECS: Range = Range {
    min: 1,
    max: 86_400,
};
/// `[retry] stable_after_secs`. At least a minute: a stable session resets
/// the attempt counter and records a breaker success, so a tiny value would
/// let a server that kicks the bot a few seconds after every join cause
/// endless reconnects at the base delay, with the breaker never opening.
pub(crate) const STABLE_AFTER_SECS: Range = Range {
    min: 60,
    max: 86_400,
};
/// `[retry] circuit_failures`.
pub(crate) const CIRCUIT_FAILURES: Range = Range { min: 1, max: 1000 };
/// `[retry] circuit_window_secs` and `circuit_cooldown_secs`.
pub(crate) const CIRCUIT_SECS: Range = Range {
    min: 1,
    max: 86_400,
};

/// Appendix A's `[retry]` defaults.
const DEFAULT_BASE_DELAY_SECS: u64 = 5;
const DEFAULT_MAX_DELAY_SECS: u64 = 300;
const DEFAULT_STABLE_AFTER_SECS: u64 = 300;
const DEFAULT_CIRCUIT_FAILURES: u64 = 8;
const DEFAULT_CIRCUIT_WINDOW_SECS: u64 = 600;
const DEFAULT_CIRCUIT_COOLDOWN_SECS: u64 = 900;

/// The whole file.
#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields, expecting = "a table of agent settings")]
pub(crate) struct RawConfig {
    #[garde(skip)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    #[garde(dive)]
    pub(crate) runtime: RawRuntime,
    #[serde(default)]
    #[garde(dive)]
    pub(crate) retry: RawRetry,
    #[garde(skip)]
    pub(crate) standalone: Option<RawStandalone>,
    #[garde(skip)]
    pub(crate) control_plane: Option<RawControlPlane>,
}

/// `[runtime]`. Missing keys take the runtime's and the adapter's defaults.
#[derive(Debug, Deserialize, Validate)]
#[serde(
    default,
    deny_unknown_fields,
    expecting = "a table of runtime settings"
)]
pub(crate) struct RawRuntime {
    #[garde(range(min = MAX_BOTS.min, max = MAX_BOTS.max))]
    pub(crate) max_bots: u64,
    #[garde(range(min = STALL_TIMEOUT_SECS.min, max = STALL_TIMEOUT_SECS.max))]
    pub(crate) watchdog_timeout_secs: u64,
    #[garde(range(min = STALL_TIMEOUT_SECS.min, max = STALL_TIMEOUT_SECS.max))]
    pub(crate) packet_liveness_timeout_secs: u64,
    #[garde(range(min = CONNECT_TIMEOUT_SECS.min, max = CONNECT_TIMEOUT_SECS.max))]
    pub(crate) connect_timeout_secs: u64,
    #[garde(range(min = MAX_ABANDONED_THREADS.min, max = MAX_ABANDONED_THREADS.max))]
    pub(crate) max_abandoned_threads: u64,
    #[garde(range(min = SHUTDOWN_TIMEOUT_SECS.min, max = SHUTDOWN_TIMEOUT_SECS.max))]
    pub(crate) shutdown_timeout_secs: u64,
    #[garde(skip)]
    pub(crate) heartbeat_file: Option<PathBuf>,
}

impl Default for RawRuntime {
    fn default() -> Self {
        let runtime = RuntimeConfig::default();
        let mc = McConfig::default();
        Self {
            max_bots: count(runtime.max_bots.get()),
            watchdog_timeout_secs: runtime.watchdog_timeout.as_secs(),
            packet_liveness_timeout_secs: runtime.packet_liveness_timeout.as_secs(),
            connect_timeout_secs: runtime.connect_timeout.as_secs(),
            max_abandoned_threads: count(mc.max_abandoned_threads.get()),
            shutdown_timeout_secs: runtime.shutdown_timeout.as_secs(),
            heartbeat_file: None,
        }
    }
}

/// `[retry]`. Missing keys take Appendix A's values.
#[derive(Debug, Deserialize, Validate)]
#[serde(default, deny_unknown_fields, expecting = "a table of retry settings")]
pub(crate) struct RawRetry {
    #[garde(range(min = BASE_DELAY_SECS.min, max = BASE_DELAY_SECS.max))]
    pub(crate) base_delay_secs: u64,
    #[garde(range(min = MAX_DELAY_SECS.min, max = MAX_DELAY_SECS.max))]
    pub(crate) max_delay_secs: u64,
    #[garde(range(min = STABLE_AFTER_SECS.min, max = STABLE_AFTER_SECS.max))]
    pub(crate) stable_after_secs: u64,
    #[garde(range(min = CIRCUIT_FAILURES.min, max = CIRCUIT_FAILURES.max))]
    pub(crate) circuit_failures: u64,
    #[garde(range(min = CIRCUIT_SECS.min, max = CIRCUIT_SECS.max))]
    pub(crate) circuit_window_secs: u64,
    #[garde(range(min = CIRCUIT_SECS.min, max = CIRCUIT_SECS.max))]
    pub(crate) circuit_cooldown_secs: u64,
}

impl Default for RawRetry {
    fn default() -> Self {
        Self {
            base_delay_secs: DEFAULT_BASE_DELAY_SECS,
            max_delay_secs: DEFAULT_MAX_DELAY_SECS,
            stable_after_secs: DEFAULT_STABLE_AFTER_SECS,
            circuit_failures: DEFAULT_CIRCUIT_FAILURES,
            circuit_window_secs: DEFAULT_CIRCUIT_WINDOW_SECS,
            circuit_cooldown_secs: DEFAULT_CIRCUIT_COOLDOWN_SECS,
        }
    }
}

/// `[standalone]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, expecting = "a table with the standalone bots")]
pub(crate) struct RawStandalone {
    #[serde(default)]
    pub(crate) bots: Vec<RawBot>,
}

/// One `[[standalone.bots]]` entry.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, expecting = "a table with a bot's settings")]
pub(crate) struct RawBot {
    pub(crate) username: Option<String>,
    pub(crate) server: Option<String>,
    pub(crate) mode: Option<String>,
    #[serde(default)]
    pub(crate) conflict_texts: Vec<String>,
}

/// `[control_plane]`.
#[derive(Debug, Deserialize)]
#[serde(
    deny_unknown_fields,
    expecting = "a table with the control plane's settings"
)]
pub(crate) struct RawControlPlane {
    pub(crate) url: Option<String>,
    pub(crate) ca_cert_file: Option<String>,
    pub(crate) cert_file: Option<String>,
    pub(crate) key_file: Option<String>,
}

/// A count as a config number. A `usize` always fits into a `u64` on the
/// platforms afkfleet builds for; anything larger is clamped.
fn count(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}
