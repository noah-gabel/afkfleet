//! Fakes for fleet-mc's diagnostics and for the stop signals.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use fleet_agent::diagnostics::{HostDiagnostics, HostSample};
use fleet_agent::signals::{ShutdownSignals, Signal};
use tokio::sync::mpsc;

/// Answers every sample with what the test set, and counts the samples.
/// Clones share both.
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeDiagnostics {
    sample: Arc<Mutex<HostSample>>,
    samples: Arc<AtomicUsize>,
}

impl FakeDiagnostics {
    /// Every later sample reads `sample`.
    pub(crate) fn set(&self, sample: HostSample) {
        *self.sample.lock().unwrap() = sample;
    }

    /// Every later sample reads `abandoned` abandoned threads.
    pub(crate) fn set_abandoned(&self, abandoned: usize) {
        self.sample.lock().unwrap().abandoned_threads = abandoned;
    }

    /// How many samples were taken.
    pub(crate) fn samples(&self) -> usize {
        self.samples.load(Ordering::SeqCst)
    }
}

impl HostDiagnostics for FakeDiagnostics {
    fn sample(&self) -> HostSample {
        self.samples.fetch_add(1, Ordering::SeqCst);
        *self.sample.lock().unwrap()
    }
}

/// Delivers the signals the test sends; waits forever once the sender is
/// gone, like the OS's source once its streams end.
#[derive(Debug)]
pub(crate) struct FakeSignals(mpsc::Receiver<Signal>);

impl FakeSignals {
    /// A source and the sender that feeds it.
    pub(crate) fn new() -> (mpsc::Sender<Signal>, Self) {
        let (sender, receiver) = mpsc::channel(4);
        (sender, Self(receiver))
    }
}

impl ShutdownSignals for FakeSignals {
    async fn recv(&mut self) -> Signal {
        match self.0.recv().await {
            Some(signal) => signal,
            None => core::future::pending().await,
        }
    }
}
