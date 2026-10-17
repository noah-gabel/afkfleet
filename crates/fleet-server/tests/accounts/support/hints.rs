//! A reconciler that records the hints it gets, to check which use cases
//! send one, and only after their commit (Plan.md P10.5; ADR-0019, S37).

use std::sync::Mutex;

use fleet_server::ports::reconcile::{Change, Reconcile};

/// The hints a service sent.
#[derive(Debug, Default)]
pub(crate) struct Hinted(Mutex<Vec<Change>>);

impl Reconcile for Hinted {
    fn changed(&self, change: Change) {
        self.0.lock().unwrap().push(change);
    }
}

impl Hinted {
    /// The hints since the last call.
    pub(crate) fn take(&self) -> Vec<Change> {
        core::mem::take(&mut *self.0.lock().unwrap())
    }
}
