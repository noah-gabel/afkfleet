//! Helpers shared by fleet-runtime's component tests.

use core::fmt::{self, Write as _};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata};

/// One logged event: its level, its fields, and the fields of the span it
/// was logged in.
#[derive(Debug, Clone)]
struct Logged {
    level: Level,
    fields: String,
    span: String,
}

/// Records the level and the fields of every event, and the span it ran in,
/// so a test can check what was logged at which level and where. A test
/// installs it for its thread with `tracing::subscriber::set_default`; the
/// current-thread runtime polls the spawned tasks there too.
#[derive(Debug, Clone, Default)]
pub(crate) struct Levels {
    events: Arc<Mutex<Vec<Logged>>>,
    spans: Arc<Mutex<Spans>>,
}

/// The spans the subscriber has seen, and the ones entered right now.
#[derive(Debug, Default)]
struct Spans {
    fields: HashMap<u64, String>,
    entered: Vec<u64>,
    next: u64,
}

impl Levels {
    /// The levels of the events whose fields contain `needle`, in order.
    pub(crate) fn of(&self, needle: &str) -> Vec<Level> {
        self.matching(needle).map(|logged| logged.level).collect()
    }

    /// The fields of the spans that the events whose fields contain `needle`
    /// were logged in, in order. An event outside any span gives `""`.
    pub(crate) fn spans_of(&self, needle: &str) -> Vec<String> {
        self.matching(needle).map(|logged| logged.span).collect()
    }

    /// Whether any event was logged at `warn` or `error`.
    pub(crate) fn any_warning(&self) -> bool {
        self.events
            .lock()
            .unwrap()
            .iter()
            .any(|logged| matches!(logged.level, Level::WARN | Level::ERROR))
    }

    fn matching(&self, needle: &str) -> impl Iterator<Item = Logged> {
        let events = self.events.lock().unwrap().clone();
        let needle = needle.to_owned();
        events
            .into_iter()
            .filter(move |logged| logged.fields.contains(&needle))
    }
}

struct Fields(String);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let _ = write!(self.0, "{}={value:?} ", field.name());
    }
}

impl tracing::Subscriber for Levels {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, attributes: &Attributes<'_>) -> Id {
        let mut fields = Fields(format!("{} ", attributes.metadata().name()));
        attributes.record(&mut fields);
        let mut spans = self.spans.lock().unwrap();
        spans.next += 1;
        let id = spans.next;
        spans.fields.insert(id, fields.0);
        Id::from_u64(id)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        let span = {
            let spans = self.spans.lock().unwrap();
            spans
                .entered
                .last()
                .and_then(|id| spans.fields.get(id))
                .cloned()
                .unwrap_or_default()
        };
        self.events.lock().unwrap().push(Logged {
            level: *event.metadata().level(),
            fields: fields.0,
            span,
        });
    }

    fn enter(&self, id: &Id) {
        self.spans.lock().unwrap().entered.push(id.into_u64());
    }

    fn exit(&self, _: &Id) {
        self.spans.lock().unwrap().entered.pop();
    }
}
