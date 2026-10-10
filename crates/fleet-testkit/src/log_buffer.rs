//! A writer that keeps what a log layer writes, for format and filter tests
//! (ADR-0015).
//!
//! A test builds its own layer with a [`LogBuffer`] as the writer, installs
//! it for a scope (`tracing::subscriber::with_default`), and reads back
//! exactly what that layer wrote: filtered and formatted, JSON or pretty.
//!
//! **Which capture to use:**
//! - [`LogBuffer`]: what a binary's log layer prints, for tests of the
//!   format, the filter, and the lines a component logs.
//! - [`log_capture`](crate::log_capture): checking that a secret never
//!   reaches any log. It records every level of every target in the whole
//!   process, unfiltered, and its failures never show what it captured.
//!   Never check for real secrets with a `LogBuffer`: tests print its text
//!   when they fail.

use core::fmt;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::Value;
use tracing_subscriber::fmt::MakeWriter;

/// Collects everything written to it; clones share the buffer.
#[derive(Debug, Clone, Default)]
pub struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl LogBuffer {
    /// Everything written so far, as text. Bytes that aren't UTF-8 become
    /// U+FFFD; a log layer only writes UTF-8.
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes()).into_owned()
    }

    /// Every line written so far, parsed as JSON.
    ///
    /// # Errors
    /// [`NotJson`] with the position of the first line that isn't JSON.
    pub fn json_lines(&self) -> Result<Vec<Value>, NotJson> {
        self.text()
            .lines()
            .enumerate()
            .map(|(line, text)| serde_json::from_str(text).map_err(|_| NotJson { line }))
            .collect()
    }

    /// Every JSON line whose `message` is `message`.
    ///
    /// # Errors
    /// [`NotJson`] with the position of the first line that isn't JSON.
    pub fn lines_with(&self, message: &str) -> Result<Vec<Value>, NotJson> {
        Ok(self
            .json_lines()?
            .into_iter()
            .filter(|line| line.get("message").and_then(Value::as_str) == Some(message))
            .collect())
    }

    /// Locks the bytes, poison-tolerantly: a test that panicked while
    /// logging leaves them usable.
    fn bytes(&self) -> MutexGuard<'_, Vec<u8>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogBuffer {
    type Writer = Self;

    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

/// A line of a [`LogBuffer`] that isn't JSON. It names only the line's
/// position, never its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotJson {
    /// The line's position, counted from 0.
    pub line: usize,
}

impl fmt::Display for NotJson {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "line {} of the log buffer isn't JSON (counted from 0)",
            self.line
        )
    }
}

impl std::error::Error for NotJson {}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    fn written(chunks: &[&[u8]]) -> LogBuffer {
        let buffer = LogBuffer::default();
        let mut writer = buffer.clone();
        for chunk in chunks {
            writer.write_all(chunk).unwrap();
        }
        buffer
    }

    #[test]
    fn clones_share_one_buffer() {
        let buffer = LogBuffer::default();
        let mut first = buffer.make_writer();
        let mut second = buffer.clone();

        first.write_all(b"one\n").unwrap();
        second.write_all(b"two\n").unwrap();

        assert_eq!(buffer.text(), "one\ntwo\n");
    }

    #[test]
    fn text_replaces_invalid_utf8() {
        let buffer = written(&[b"ok \xff end"]);

        assert_eq!(buffer.text(), "ok \u{fffd} end");
    }

    #[test]
    fn an_empty_buffer_has_no_lines() {
        let buffer = LogBuffer::default();

        assert_eq!(buffer.text(), "");
        assert_eq!(buffer.json_lines(), Ok(Vec::new()));
    }

    #[test]
    fn every_line_is_parsed_as_json() {
        let buffer = written(&[b"{\"message\":\"a\"}\n", b"{\"message\":\"b\",\"n\":1}\n"]);

        let lines = buffer.json_lines().unwrap();

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["message"], "a");
        assert_eq!(lines[1]["n"], 1);
    }

    #[test]
    fn a_line_that_isnt_json_is_named_by_its_position_only() {
        let buffer = written(&[b"{\"message\":\"a\"}\n", b"hunter2 is not json\n"]);

        let error = buffer.json_lines().unwrap_err();

        assert_eq!(error, NotJson { line: 1 });
        assert!(!error.to_string().contains("hunter2"));
        assert!(!format!("{error:?}").contains("hunter2"));
        assert_eq!(buffer.lines_with("a"), Err(NotJson { line: 1 }));
    }

    #[test]
    fn lines_with_keeps_the_lines_with_that_message() {
        let buffer = written(&[
            b"{\"message\":\"keep\",\"n\":1}\n",
            b"{\"message\":\"skip\"}\n",
            b"{\"n\":3}\n",
            b"{\"message\":\"keep\",\"n\":4}\n",
        ]);

        let kept = buffer.lines_with("keep").unwrap();

        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0]["n"], 1);
        assert_eq!(kept[1]["n"], 4);
    }

    #[test]
    fn a_poisoned_lock_still_works() {
        let buffer = written(&[b"before\n"]);
        let inner = Arc::clone(&buffer.0);
        let poisoned = std::thread::spawn(move || {
            let _guard = inner.lock().unwrap();
            panic!("poisoning the buffer's lock on purpose");
        })
        .join();
        assert!(poisoned.is_err());

        buffer.clone().write_all(b"after\n").unwrap();

        assert_eq!(buffer.text(), "before\nafter\n");
    }

    #[test]
    fn a_log_layer_writes_into_it() {
        let buffer = LogBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buffer.clone())
            .with_ansi(false)
            .finish();

        tracing::subscriber::with_default(subscriber, || tracing::info!("hello buffer"));

        assert!(buffer.text().contains("hello buffer"));
    }
}
