//! Helpers shared by fleet-runtime's component tests.

use core::fmt::{self, Write as _};
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata};

/// Records the level and the fields of every event, so a test can check
/// what was logged at which level. A test installs it for its thread with
/// `tracing::subscriber::set_default`; the current-thread runtime polls the
/// spawned tasks there too.
#[derive(Debug, Clone, Default)]
pub(crate) struct Levels {
    events: Arc<Mutex<Vec<(Level, String)>>>,
}

impl Levels {
    /// The levels of the events whose fields contain `needle`, in order.
    pub(crate) fn of(&self, needle: &str) -> Vec<Level> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, text)| text.contains(needle))
            .map(|(level, _)| *level)
            .collect()
    }

    /// Whether any event was logged at `warn` or `error`.
    pub(crate) fn any_warning(&self) -> bool {
        self.events
            .lock()
            .unwrap()
            .iter()
            .any(|(level, _)| matches!(*level, Level::WARN | Level::ERROR))
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

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        self.events
            .lock()
            .unwrap()
            .push((*event.metadata().level(), fields.0));
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}
