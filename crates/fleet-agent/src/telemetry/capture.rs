//! The agent's JSON lines on this thread, for the crate's unit tests, in
//! fleet-testkit's [`LogBuffer`].

use fleet_testkit::log_buffer::LogBuffer;
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::layer::SubscriberExt as _;

use crate::config::{LogConfig, LogFilter, LogFormat};

/// Captures this thread's events as the agent's JSON lines, `debug` and up,
/// until the guard drops. Tasks on a current-thread runtime run on this
/// thread too.
pub(crate) fn json_on_this_thread() -> (LogBuffer, DefaultGuard) {
    let capture = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: LogFilter::try_from("debug").unwrap(),
    };
    let subscriber =
        tracing_subscriber::registry().with(super::layer(&config, false, capture.clone()));
    (capture, tracing::subscriber::set_default(subscriber))
}
