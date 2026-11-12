//! The extractors' tests read a refusal's `debug` line through this.

use fleet_startup::config::{LogConfig, LogFormat};
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::log_buffer::LogBuffer;
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::layer::SubscriberExt as _;

/// The server's log layer at `trace`, writing JSON into a buffer, as this
/// thread's default subscriber until the guard is dropped.
pub(super) fn capture() -> (LogBuffer, DefaultGuard) {
    let buffer = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: "trace".parse().unwrap(),
    };
    let subscriber =
        tracing_subscriber::registry().with(layer_with(&config, &NoRules, false, buffer.clone()));
    (buffer, tracing::subscriber::set_default(subscriber))
}
