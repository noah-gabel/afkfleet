//! A writer that keeps what the log layer writes, for the crate's unit tests.

use std::io;
use std::sync::{Arc, Mutex};

use tracing::subscriber::DefaultGuard;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;

use crate::config::{LogConfig, LogFilter, LogFormat};

/// Collects everything written to it; clones share the buffer.
#[derive(Debug, Clone, Default)]
pub(crate) struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    /// Everything written so far, as text.
    pub(crate) fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }

    /// Every line written so far, parsed as JSON.
    pub(crate) fn json_lines(&self) -> Vec<serde_json::Value> {
        self.text()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Every JSON line whose message is `message`.
    pub(crate) fn lines_with(&self, message: &str) -> Vec<serde_json::Value> {
        self.json_lines()
            .into_iter()
            .filter(|line| line["message"] == message)
            .collect()
    }
}

/// Captures this thread's events as the agent's JSON lines, `debug` and up,
/// until the guard drops. Tasks on a current-thread runtime run on this
/// thread too.
pub(crate) fn json_on_this_thread() -> (Capture, DefaultGuard) {
    let capture = Capture::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: LogFilter::try_from("debug").unwrap(),
    };
    let subscriber =
        tracing_subscriber::registry().with(super::layer(&config, false, capture.clone()));
    (capture, tracing::subscriber::set_default(subscriber))
}

impl io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Self;

    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}
