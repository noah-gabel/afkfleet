//! Logging: the format and filter from `[log]`, azalea's caps, and a panic
//! hook that logs through `tracing` (Plan.md P5.2, ADR-0014).
//!
//! - **JSON** (the default) writes one object per line, with the event's
//!   fields at the top level next to `timestamp`, `level`, `target`, `span`
//!   and `spans`. **Pretty** writes one human-readable line per event,
//!   colored only on a terminal. Both go to stdout.
//! - **The filter** is the operator's `[log] filter`, with azalea's rules on
//!   top (ADR-0011):
//!   - azalea's targets stay at `warn` unless a directive names them, so a
//!     broad `debug` can't turn on azalea's own kick rendering, which a
//!     hostile server can crash or slow down
//!   - `azalea_auth` never logs below `info`, because its `trace` lines hold
//!     the chat-signing private key
//!   - panic reports always get through
//! - **The panic hook** replaces the default one and logs one `error` event
//!   with the location, the thread and the payload. The payload is untrusted
//!   text, so it's sanitized into one line and capped.
//! - `log` records (reqwest, rustls, hickory) are bridged into `tracing` and
//!   filtered by their own targets.

mod filter;
mod format;
mod panic;

#[cfg(test)]
pub(crate) mod capture;

use std::io::IsTerminal as _;

use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

use crate::config::LogConfig;

pub use format::layer;
pub use panic::PANIC_TARGET;

/// Why [`init`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TelemetryError {
    /// A global `tracing` subscriber or `log` logger is already installed.
    #[error("logging is already set up in this process")]
    AlreadyInstalled,
}

/// Sets up logging for the whole process: installs the global subscriber
/// with [`layer`] on stdout and the `log` bridge, warns once if the filter
/// lifts an azalea target above `warn`, and installs the panic hook.
///
/// # Errors
/// [`TelemetryError::AlreadyInstalled`] if a global subscriber or logger is
/// already installed.
pub fn init(config: &LogConfig) -> Result<(), TelemetryError> {
    let colored = std::io::stdout().is_terminal();
    tracing_subscriber::registry()
        .with(layer(config, colored, std::io::stdout))
        .try_init()
        .map_err(|_| TelemetryError::AlreadyInstalled)?;
    filter::warn_if_azalea_is_lifted(&config.filter);
    panic::install_hook();
    Ok(())
}
