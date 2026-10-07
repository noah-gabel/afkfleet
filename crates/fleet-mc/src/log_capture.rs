//! A process-wide log capture for the redaction tests (ADR-0011).
//!
//! It records every level from every target, azalea's included, since azalea
//! logs from the host thread and from Bevy's IO pool. `log` records, which
//! reqwest and rustls emit, arrive through tracing-subscriber's `tracing-log`
//! bridge, under their own `log` target.
//!
//! A failing check names only targets and counts, never the captured lines or
//! what it looked for, so a run with a real token can't print the token.

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

/// One captured event, span or span update.
struct Line {
    target: String,
    text: String,
}

/// Everything captured so far, in this process.
static LINES: Mutex<Vec<Line>> = Mutex::new(Vec::new());

/// Installs the capture as the process's global subscriber and `log`
/// logger, once. Every test that checks the capture calls it first.
pub(crate) fn install() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        tracing_subscriber::registry()
            .with(Capture)
            .try_init()
            .expect("no other global subscriber or logger is set in the test process");
    });
}

/// How many captured lines from `target` contain `needle`.
pub(crate) fn count(target: &str, needle: &str) -> usize {
    lines()
        .iter()
        .filter(|line| line.target == target && line.text.contains(needle))
        .count()
}

/// Fails if any captured line contains `secret`. The message names only the
/// targets and how often each one showed it.
pub(crate) fn assert_absent(secret: &str) {
    let mut hits = BTreeMap::<&str, usize>::new();
    let lines = lines();
    for line in lines.iter().filter(|line| line.text.contains(secret)) {
        *hits.entry(line.target.as_str()).or_default() += 1;
    }
    let report = hits
        .iter()
        .map(|(target, count)| format!("{target}: {count}"))
        .collect::<Vec<_>>()
        .join(", ");
    assert!(
        hits.is_empty(),
        "a secret appeared in the captured logs ({report})"
    );
}

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
    use std::panic;

    #[test]
    fn log_records_are_captured_under_their_own_target() {
        install();

        log::trace!(target: "capture_test_log", "probe-log-5d1e");

        assert_eq!(count("capture_test_log", "probe-log-5d1e"), 1);
    }

    #[test]
    fn tracing_events_and_span_fields_are_captured() {
        install();

        tracing::trace!(target: "capture_test_event", field = "probe-event-0b7c", "an event");
        let span = tracing::trace_span!(target: "capture_test_span", "a span", field = "probe-span-3a92", later = tracing::field::Empty);
        span.record("later", "probe-record-8e41");

        assert_eq!(count("capture_test_event", "probe-event-0b7c"), 1);
        assert_eq!(count("capture_test_span", "probe-span-3a92"), 1);
        assert_eq!(count("capture_test_span", "probe-record-8e41"), 1);
    }

    #[test]
    fn a_failed_check_names_targets_and_counts_but_never_the_secret() {
        install();
        let secret = "probe-secret-c4f2";
        tracing::trace!(target: "capture_test_leak", "{secret}");
        tracing::trace!(target: "capture_test_leak", value = secret);

        let failure = panic::catch_unwind(|| assert_absent(secret)).unwrap_err();

        let message = failure
            .downcast_ref::<String>()
            .expect("assert! panics with a formatted message");
        assert!(message.contains("capture_test_leak: 2"), "{message}");
        assert!(!message.contains(secret));
    }

    #[test]
    fn a_check_passes_when_the_secret_never_appeared() {
        install();
        tracing::trace!(target: "capture_test_clean", "nothing secret here");

        assert_absent("probe-absent-9a0d");
    }
}
