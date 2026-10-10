//! Logging for a binary: the format and filter from `[log]`, and a panic
//! hook that logs through `tracing` (Plan.md P5.2, ADR-0014, ADR-0015).
//!
//! - **JSON** (the default) writes one object per line, with the event's
//!   fields at the top level next to `timestamp`, `level`, `target`, `span`
//!   and `spans`. **Pretty** writes one human-readable line per event,
//!   colored only on a terminal. Both go to stdout.
//! - **The filter** is the operator's `[log] filter`, with two rules no
//!   binary can change (ADR-0011):
//!   - `azalea_auth` never logs below `info`, because its `trace` lines hold
//!     secrets: the agent's chat-signing private key, and the server's
//!     Microsoft tokens (P9.3)
//!   - panic reports always get through
//!
//!   A binary adds its own rules through [`FilterRules`]; the agent adds
//!   azalea's.
//! - **The panic hook** replaces the default one and logs one `error` event
//!   with the location, the thread and the payload. The payload is untrusted
//!   text, so it's sanitized into one line and capped.
//! - `log` records (reqwest, rustls, hickory) are bridged into `tracing` and
//!   filtered by their own targets.

mod filter;
mod format;
mod panic;

use std::io::IsTerminal as _;

use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

use crate::config::LogConfig;

pub use filter::FilterRules;
pub use format::layer_with;
pub use panic::PANIC_TARGET;

/// Why [`init_with`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TelemetryError {
    /// A global `tracing` subscriber or `log` logger is already installed.
    #[error("logging is already set up in this process")]
    AlreadyInstalled,
}

/// Sets up logging for the whole process: installs the global subscriber
/// with [`layer_with`] on stdout and the `log` bridge, runs the rules'
/// [`FilterRules::warn_at_startup`], and installs the panic hook.
///
/// # Errors
/// [`TelemetryError::AlreadyInstalled`] if a global subscriber or logger is
/// already installed.
pub fn init_with<R>(config: &LogConfig, rules: &R) -> Result<(), TelemetryError>
where
    R: FilterRules + ?Sized,
{
    let colored = std::io::stdout().is_terminal();
    tracing_subscriber::registry()
        .with(layer_with(config, rules, colored, std::io::stdout))
        .try_init()
        .map_err(|_| TelemetryError::AlreadyInstalled)?;
    rules.warn_at_startup(&config.filter);
    panic::install_hook();
    Ok(())
}
