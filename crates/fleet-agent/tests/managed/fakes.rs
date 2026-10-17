//! A fake link to the control stream.

use std::sync::{Arc, Mutex};

use fleet_agent::managed::credentials::{LinkGeneration, SessionLink, Unsent};
use fleet_proto::control::SessionRequest;
use tokio::sync::watch;

/// Records every request and answers each send with what the test set
/// (generation 1 at first). Clones share both.
#[derive(Debug, Clone)]
pub(crate) struct FakeLink {
    outcome: Arc<Mutex<Result<LinkGeneration, Unsent>>>,
    sent: Arc<Mutex<Vec<SessionRequest>>>,
    count: watch::Sender<usize>,
}

impl Default for FakeLink {
    fn default() -> Self {
        Self {
            outcome: Arc::new(Mutex::new(Ok(LinkGeneration::new(1)))),
            sent: Arc::default(),
            count: watch::Sender::new(0),
        }
    }
}

impl FakeLink {
    /// Every later send answers `outcome`.
    pub(crate) fn answer(&self, outcome: Result<LinkGeneration, Unsent>) {
        *self.outcome.lock().unwrap() = outcome;
    }

    /// The requests sent so far.
    pub(crate) fn sent(&self) -> Vec<SessionRequest> {
        self.sent.lock().unwrap().clone()
    }

    /// Waits until `count` requests were sent; paused time makes the bound
    /// a virtual one.
    pub(crate) async fn wait_sent(&self, count: usize) -> SessionRequest {
        let mut seen = self.count.subscribe();
        tokio::time::timeout(
            core::time::Duration::from_secs(5),
            seen.wait_for(|sent| *sent >= count),
        )
        .await
        .expect("the provider should have sent the request")
        .unwrap();
        self.sent()[count - 1]
    }
}

impl SessionLink for FakeLink {
    fn send(&self, request: SessionRequest) -> Result<LinkGeneration, Unsent> {
        self.sent.lock().unwrap().push(request);
        self.count.send_modify(|count| *count += 1);
        *self.outcome.lock().unwrap()
    }
}
