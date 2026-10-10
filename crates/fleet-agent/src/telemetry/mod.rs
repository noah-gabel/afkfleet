//! Logging: fleet-startup's log layer and panic hook, with azalea's rules on
//! top (Plan.md P5.2, ADR-0014, ADR-0015).
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
//!     the chat-signing private key (fleet-startup applies this one)
//!   - panic reports always get through (fleet-startup too)
//! - **The panic hook** replaces the default one and logs one `error` event
//!   with the location, the thread and the payload. The payload is untrusted
//!   text, so it's sanitized into one line and capped.
//! - `log` records (reqwest, rustls, hickory) are bridged into `tracing` and
//!   filtered by their own targets.

mod filter;

#[cfg(test)]
pub(crate) mod capture;

use tracing::Subscriber;
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::registry::LookupSpan;

use crate::config::LogConfig;
use filter::AzaleaRules;

pub use fleet_startup::telemetry::{PANIC_TARGET, TelemetryError};

/// The agent's log layer, writing to `writer`: JSON or pretty lines, with the
/// filter that `[log] filter` and azalea's rules make (see
/// [`telemetry`](crate::telemetry)). `ansi` turns on colors for the pretty
/// format; [`init`] passes whether stdout is a terminal.
#[must_use]
pub fn layer<S, W>(config: &LogConfig, ansi: bool, writer: W) -> Box<dyn Layer<S> + Send + Sync>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    fleet_startup::telemetry::layer_with(config, &AzaleaRules, ansi, writer)
}

/// Sets up logging for the whole process: installs the global subscriber
/// with [`layer`] on stdout and the `log` bridge, warns once if the filter
/// lifts an azalea target above `warn`, and installs the panic hook.
///
/// # Errors
/// [`TelemetryError::AlreadyInstalled`] if a global subscriber or logger is
/// already installed.
pub fn init(config: &LogConfig) -> Result<(), TelemetryError> {
    fleet_startup::telemetry::init_with(config, &AzaleaRules)
}
