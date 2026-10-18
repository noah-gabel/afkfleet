//! One log of everything a case runs: the server's lines and every
//! agent's (each in its `agent{agent=…}` span), JSON at `debug` through
//! fleet-startup's layer, as the test thread's default subscriber.
//!
//! Every line wakes the waits ([`Logs::until`]): nearly every change a case
//! waits for is logged (a stream's start and end, an assignment, a grant, a
//! bot's state), and a wait looks again every [`RECHECK`] for one that
//! isn't. A wait never sleeps a fixed time to let something happen, and
//! never spins: it waits for a line or the re-check, within [`PATIENCE`].

use core::pin::pin;
use core::time::Duration;
use std::io;
use std::sync::Arc;

use fleet_server::config::{LogConfig, LogFormat};
use fleet_startup::telemetry::{NoRules, layer_with};
use fleet_testkit::log_buffer::LogBuffer;
use tokio::sync::Notify;
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;

/// The longest wait between two of an agent's attempts:
/// `fleet_proto::control::RECONNECT_DELAY_MAX`, which the agent's backoff
/// never exceeds (ADR-0019, S67).
pub(crate) const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// The longest a case waits for anything: a reconnect after the worst
/// backoff ([`RECONNECT_MAX`]) and a whole attempt (`WELCOME_TIMEOUT`,
/// 30 s), with as much again to spare, since CI retries nothing.
pub(crate) const PATIENCE: Duration = Duration::from_secs(90);

/// How often a wait looks again without a new line.
const RECHECK: Duration = Duration::from_millis(100);

/// The case's log; clones share it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Logs {
    buffer: LogBuffer,
    wake: Arc<Notify>,
}

impl Logs {
    /// A new log as this thread's default subscriber, for as long as the
    /// guard lives.
    pub(crate) fn capture() -> (Self, DefaultGuard) {
        let logs = Self::default();
        let config = LogConfig {
            format: LogFormat::Json,
            filter: "debug".parse().unwrap(),
        };
        let subscriber = tracing_subscriber::registry().with(layer_with(
            &config,
            &NoRules,
            false,
            Waking(logs.clone()),
        ));
        let guard = tracing::subscriber::set_default(subscriber);
        (logs, guard)
    }

    /// The lines so far.
    pub(crate) fn buffer(&self) -> &LogBuffer {
        &self.buffer
    }

    /// How many lines say `message`.
    pub(crate) fn count(&self, message: &str) -> usize {
        self.buffer.lines_with(message).unwrap().len()
    }

    /// Waits until `done`, looking again at every line and every
    /// [`RECHECK`]; fails the case with `what` and the log after
    /// [`PATIENCE`].
    pub(crate) async fn until(&self, what: &str, mut done: impl AsyncFnMut() -> bool) {
        let waited = tokio::time::timeout(PATIENCE, async {
            loop {
                // Registered before looking, so a line between the look
                // and the wait still wakes it.
                let mut line = pin!(self.wake.notified());
                line.as_mut().enable();
                if done().await {
                    return;
                }
                // Either way it looks again: a line came, or the re-check.
                let _ = tokio::time::timeout(RECHECK, line).await;
            }
        })
        .await;
        assert!(
            waited.is_ok(),
            "{what}: not within {PATIENCE:?}\n{}",
            self.buffer.text()
        );
    }
}

/// The layer's writer: into the buffer, waking every wait.
#[derive(Debug, Clone)]
struct Waking(Logs);

impl io::Write for Waking {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.0.buffer.clone().write(bytes)?;
        self.0.wake.notify_waiters();
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Waking {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
