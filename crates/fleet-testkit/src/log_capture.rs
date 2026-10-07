//! A process-wide log capture for redaction tests (ADR-0011): a test installs
//! it, runs code that handles a secret, and checks that the secret never
//! reached a log.
//!
//! It records every level from every target, dependencies included, since
//! they may log from threads the test doesn't own (azalea logs from its host
//! thread and from Bevy's IO pool). `log` records, which crates such as
//! reqwest and rustls emit, arrive through tracing-subscriber's `tracing-log`
//! bridge, under their own `log` target.
//!
//! A failed check names only targets and counts, never the captured lines or
//! what it looked for, so a run with a real secret can't print the secret.
//! nextest runs every test in a process of its own, so a test sees only its
//! own logs.

use core::fmt::{self, Write as _};
use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt as _;

/// The field a `log` record's target arrives in, through `tracing-log`.
const LOG_TARGET_FIELD: &str = "log.target";

/// Why [`install`] couldn't install the capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallError;

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("another global subscriber or `log` logger is already set in this process")
    }
}

impl std::error::Error for InstallError {}

/// A secret that [`check_absent`] found in the captured logs: where it
/// appeared, as targets and counts, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretLeak {
    /// How many captured lines of each target showed the secret.
    pub hits: BTreeMap<String, usize>,
}

impl fmt::Display for SecretLeak {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a secret appeared in the captured logs (")?;
        for (index, (target, count)) in self.hits.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{target}: {count}")?;
        }
        f.write_str(")")
    }
}

impl std::error::Error for SecretLeak {}

/// One captured event, span or span update.
struct Line {
    target: String,
    text: String,
}

/// Everything captured so far, in this process.
static LINES: Mutex<Vec<Line>> = Mutex::new(Vec::new());

/// Installs the capture as the process's global subscriber and `log` logger.
/// Calling it again returns the first call's result. Every test that checks
/// the capture calls it first.
///
/// # Errors
/// [`InstallError`] if another global subscriber or logger was set first.
pub fn install() -> Result<(), InstallError> {
    static INSTALLED: OnceLock<Result<(), InstallError>> = OnceLock::new();
    *INSTALLED.get_or_init(|| {
        tracing_subscriber::registry()
            .with(Capture)
            .try_init()
            .map_err(|_| InstallError)
    })
}

/// How many captured lines from `target` contain `needle`.
#[must_use]
pub fn count(target: &str, needle: &str) -> usize {
    lines()
        .iter()
        .filter(|line| line.target == target && line.text.contains(needle))
        .count()
}

/// Checks that no captured line contains `secret`.
///
/// # Errors
/// [`SecretLeak`], naming only the targets and how often each one showed
/// the secret.
pub fn check_absent(secret: &str) -> Result<(), SecretLeak> {
    let mut hits = BTreeMap::<String, usize>::new();
    for line in lines().iter().filter(|line| line.text.contains(secret)) {
        let count = hits.entry(line.target.clone()).or_default();
        *count = count.saturating_add(1);
    }
    if hits.is_empty() {
        Ok(())
    } else {
        Err(SecretLeak { hits })
    }
}

/// Locks the captured lines, poison-tolerantly: a test that panicked while
/// logging leaves them usable.
fn lines() -> MutexGuard<'static, Vec<Line>> {
    LINES.lock().unwrap_or_else(PoisonError::into_inner)
}

fn push(target: &str, text: String) {
    lines().push(Line {
        target: target.to_owned(),
        text,
    });
}

/// The capturing layer.
struct Capture;

impl<S> Layer<S> for Capture
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        let mut text = Text::default();
        attrs.record(&mut text);
        push(attrs.metadata().target(), text.text);
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let mut text = Text::default();
        values.record(&mut text);
        let target = ctx.metadata(id).map_or("", |metadata| metadata.target());
        push(target, text.text);
    }

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut text = Text::default();
        event.record(&mut text);
        let target = text
            .log_target
            .take()
            .unwrap_or_else(|| event.metadata().target().to_owned());
        push(&target, text.text);
    }
}

/// Writes every field as `name=value`. Strings are written as they are, so
/// nothing is hidden behind escaping.
#[derive(Default)]
struct Text {
    text: String,
    log_target: Option<String>,
}

impl Visit for Text {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == LOG_TARGET_FIELD {
            self.log_target = Some(value.to_owned());
        }
        let _ = write!(self.text, "{}={value} ", field.name());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let _ = write!(self.text, "{}={value:?} ", field.name());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installing_twice_succeeds() {
        assert_eq!(install(), Ok(()));
        assert_eq!(install(), Ok(()));
    }

    #[test]
    fn log_records_are_captured_under_their_own_target() {
        install().unwrap();

        log::trace!(target: "capture_test_log", "probe-log-5d1e");

        assert_eq!(count("capture_test_log", "probe-log-5d1e"), 1);
    }

    #[test]
    fn tracing_events_and_span_fields_are_captured() {
        install().unwrap();

        tracing::trace!(target: "capture_test_event", field = "probe-event-0b7c", "an event");
        let span = tracing::trace_span!(target: "capture_test_span", "a span", field = "probe-span-3a92", later = tracing::field::Empty);
        span.record("later", "probe-record-8e41");

        assert_eq!(count("capture_test_event", "probe-event-0b7c"), 1);
        assert_eq!(count("capture_test_span", "probe-span-3a92"), 1);
        assert_eq!(count("capture_test_span", "probe-record-8e41"), 1);
    }

    #[test]
    fn a_failed_check_names_targets_and_counts_but_never_the_secret() {
        install().unwrap();
        let secret = "probe-secret-c4f2";
        tracing::trace!(target: "capture_test_leak", "{secret}");
        tracing::trace!(target: "capture_test_leak", value = secret);

        let leak = check_absent(secret).unwrap_err();

        assert_eq!(leak.hits.get("capture_test_leak"), Some(&2));
        let shown = format!("{leak} {leak:?}");
        assert!(shown.contains("capture_test_leak: 2"), "{shown}");
        assert!(!shown.contains(secret));
    }

    #[test]
    fn a_check_passes_when_the_secret_never_appeared() {
        install().unwrap();
        tracing::trace!(target: "capture_test_clean", "nothing secret here");

        assert_eq!(check_absent("probe-absent-9a0d"), Ok(()));
    }

    #[test]
    fn errors_have_fixed_messages() {
        let leak = SecretLeak {
            hits: BTreeMap::from([("a".to_owned(), 1), ("b".to_owned(), 3)]),
        };

        assert_eq!(
            InstallError.to_string(),
            "another global subscriber or `log` logger is already set in this process"
        );
        assert_eq!(
            leak.to_string(),
            "a secret appeared in the captured logs (a: 1, b: 3)"
        );
    }
}
