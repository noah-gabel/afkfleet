//! Doubles that record the hints a service sends, to check which use cases
//! send one, and only after their commit: a reconciler (Plan.md P10.5;
//! ADR-0019, S37), and the live events sockets (ADR-0020, S42).

use std::sync::Mutex;

use fleet_core::id::{BotId, UserId};
use fleet_server::ports::live_hints::{LiveHints, ModeEvent};
use fleet_server::ports::reconcile::{Change, Reconcile};
use fleet_server::ports::store::Store;

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

/// The bots a service told the live events sockets about, each with the
/// database's commit count at the call: a hint sent before its commit shows
/// as one count short. The accounts' services never tell a mode or a role.
#[derive(Debug)]
pub(crate) struct Heard {
    commits: tokio::sync::watch::Receiver<u64>,
    calls: Mutex<Vec<(u64, Vec<BotId>)>>,
}

impl Heard {
    /// Counting `store`'s commits.
    pub(crate) fn new(store: &dyn Store) -> Self {
        Self {
            commits: store.commits(),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// The hints since the last call.
    pub(crate) fn take(&self) -> Vec<(u64, Vec<BotId>)> {
        core::mem::take(&mut *self.calls.lock().unwrap())
    }

    /// What a service that told `bots` once after its commit leaves: the
    /// commit count now, with `bots`.
    pub(crate) fn once(&self, bots: &[BotId]) -> Vec<(u64, Vec<BotId>)> {
        vec![(*self.commits.borrow(), bots.to_vec())]
    }
}

impl LiveHints for Heard {
    fn bots(&self, bots: &[BotId]) {
        // An empty list is no hint, as the bus takes it.
        if !bots.is_empty() {
            let count = *self.commits.borrow();
            self.calls.lock().unwrap().push((count, bots.to_vec()));
        }
    }

    fn mode(&self, event: ModeEvent) {
        panic!("an accounts' service told a mode: {event:?}");
    }

    fn role_changed(&self, user: UserId) {
        panic!("an accounts' service told a role change: {user}");
    }
}
