//! Logging for a binary: the format and filter from `[log]`, and a panic
//! hook that logs through `tracing` (Plan.md P5.2, ADR-0014, ADR-0015).
//!
//! - **JSON** (the default) writes one object per line, with the event's
//!   fields at the top level next to `timestamp`, `level`, `target`, `span`
//!   and `spans`. **Pretty** writes one human-readable line per event,
//!   colored only on a terminal. Both go to stdout, or to stderr for a
//!   command whose stdout carries its result alone ([`init_on`]).
//! - **The filter** is the operator's `[log] filter`, with two rules no
//!   binary can change (ADR-0011, ADR-0020):
//!   - `azalea_auth` never logs below `info`, because its `trace` lines hold
//!     secrets: the agent's chat-signing private key, and the server's
//!     Microsoft tokens (P9.3); nor do `tungstenite` and `tokio_tungstenite`,
//!     whose `trace` lines hold every WebSocket frame, chat text included
//!     ([`FIXED_CAPS`], S56)
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

pub use filter::{FIXED_CAPS, FilterRules, NoRules};
pub use format::layer_with;
pub use panic::PANIC_TARGET;

/// Why [`init_with`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TelemetryError {
    /// A global `tracing` subscriber or `log` logger is already installed.
    #[error("logging is already set up in this process")]
    AlreadyInstalled,
}

/// Where a process's log lines go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogOutput {
    /// stdout: every command's, unless its stdout carries its result alone.
    Stdout,
    /// stderr: a command whose stdout carries its result alone, such as the
    /// bundle of `afkfleet-server agent create-enrollment-token`
    /// (ADR-0019).
    Stderr,
}

/// Sets up logging for the whole process on stdout: [`init_on`] with
/// [`LogOutput::Stdout`].
///
/// # Errors
/// [`TelemetryError::AlreadyInstalled`] if a global subscriber or logger is
/// already installed.
pub fn init_with<R>(config: &LogConfig, rules: &R) -> Result<(), TelemetryError>
where
    R: FilterRules + ?Sized,
{
    init_on(config, rules, LogOutput::Stdout)
}

/// Sets up logging for the whole process: installs the global subscriber
/// with [`layer_with`] on `output` and the `log` bridge, runs the rules'
/// [`FilterRules::warn_at_startup`], and installs the panic hook. Pretty
/// lines are colored only when `output` is a terminal.
///
/// # Errors
/// [`TelemetryError::AlreadyInstalled`] if a global subscriber or logger is
/// already installed.
pub fn init_on<R>(config: &LogConfig, rules: &R, output: LogOutput) -> Result<(), TelemetryError>
where
    R: FilterRules + ?Sized,
{
    let registry = tracing_subscriber::registry();
    let installed = match output {
        LogOutput::Stdout => {
            let colored = std::io::stdout().is_terminal();
            registry
                .with(layer_with(config, rules, colored, std::io::stdout))
                .try_init()
        }
        LogOutput::Stderr => {
            let colored = std::io::stderr().is_terminal();
            registry
                .with(layer_with(config, rules, colored, std::io::stderr))
                .try_init()
        }
    };
    installed.map_err(|_| TelemetryError::AlreadyInstalled)?;
    rules.warn_at_startup(&config.filter);
    panic::install_hook();
    Ok(())
}
