//! A writer that keeps what the log layer writes, for the telemetry tests.

use std::io;
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::MakeWriter;

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
