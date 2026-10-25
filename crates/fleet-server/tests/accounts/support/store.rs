//! [`FlakyStore`]: the real database, whose next write transactions can be
//! made to fail with [`StoreError::Busy`], and whose next write or account
//! read can be held at a [`StoreGate`] while the test changes the row, for
//! the session-token service's races (P9.5).

use core::time::Duration;
use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use fleet_core::authz::Actor;
use fleet_core::id::{AccountId, UserId};
use fleet_server::infra::sqlite::Database;
use fleet_server::ports::accounts::{AccountReads, ListedAccount, McAccount};
use fleet_server::ports::agents::AgentReads;
use fleet_server::ports::audit::AuditReads;
use fleet_server::ports::bots::BotReads;
use fleet_server::ports::chat::ChatReads;
use fleet_server::ports::enrollment_tokens::EnrollmentTokenReads;
use fleet_server::ports::envelopes::EnvelopeReads;
use fleet_server::ports::grants::GrantReads;
use fleet_server::ports::invites::InviteReads;
use fleet_server::ports::login_attempts::LoginAttemptReads;
use fleet_server::ports::mfa::MfaReads;
use fleet_server::ports::modes::ModeReads;
use fleet_server::ports::page::Page;
use fleet_server::ports::retention::{RetentionBatch, RetentionReads};
use fleet_server::ports::sessions::SessionReads;
use fleet_server::ports::store::{Store, StoreError, WriteTx};
use fleet_server::ports::users::UserReads;
use tokio::sync::oneshot;

/// How long [`StoreGate::entered`] waits before failing the test.
const ENTER_BOUND: Duration = Duration::from_secs(5);

/// The database, which of its next writes to refuse, and its gates.
#[derive(Debug)]
pub(crate) struct FlakyStore {
    inner: Database,
    plan: Mutex<Plan>,
}

/// What the next calls do.
#[derive(Debug, Default)]
struct Plan {
    /// Writes that pass before the refusals.
    pass: u32,
    /// Writes refused then.
    refuse: u32,
    /// Where the next write waits.
    write_gate: Option<Held>,
    /// How many account reads pass before one waits at the gate.
    find_gate: Option<(u32, Held)>,
}

/// The store's end of a gate.
#[derive(Debug)]
struct Held {
    entered: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

impl Held {
    /// Says it's here, and waits for the release; a dropped gate releases.
    async fn wait(self) {
        // The test may not wait for the call; then nobody listens.
        let _ = self.entered.send(());
        let _ = self.release.await;
    }
}

/// The test's end of a gate: it learns when the held call arrived, and lets
/// it go on.
#[derive(Debug)]
pub(crate) struct StoreGate {
    entered: Option<oneshot::Receiver<()>>,
    release: oneshot::Sender<()>,
}

impl StoreGate {
    /// Waits until the held call arrived; fails the test after 5 seconds.
    pub(crate) async fn entered(&mut self) {
        if let Some(entered) = self.entered.take() {
            tokio::time::timeout(ENTER_BOUND, entered)
                .await
                .expect("the held call arrived in time")
                .expect("the store kept the gate");
        }
    }

    /// Lets the held call go on.
    pub(crate) fn release(self) {
        // The call may be gone already.
        let _ = self.release.send(());
    }
}

fn gate() -> (Held, StoreGate) {
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    (
        Held {
            entered: entered_tx,
            release: release_rx,
        },
        StoreGate {
            entered: Some(entered_rx),
            release: release_tx,
        },
    )
}

impl FlakyStore {
    pub(crate) fn new(inner: Database) -> Self {
        Self {
            inner,
            plan: Mutex::default(),
        }
    }

    fn plan(&self) -> MutexGuard<'_, Plan> {
        self.plan.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Refuses the next `n` write transactions with [`StoreError::Busy`].
    pub(crate) fn fail_next_writes(&self, n: u32) {
        self.fail_writes_after(0, n);
    }

    /// Lets `pass` write transactions through, then refuses `n`.
    pub(crate) fn fail_writes_after(&self, pass: u32, n: u32) {
        let mut plan = self.plan();
        plan.pass = pass;
        plan.refuse = n;
    }

    /// Holds the next write transaction, before it starts, at the returned
    /// gate.
    pub(crate) fn gate_next_write(&self) -> StoreGate {
        let (held, gate) = gate();
        self.plan().write_gate = Some(held);
        gate
    }

    /// Lets `pass` account reads through the read connections, then holds
    /// the next one, before it reads, at the returned gate.
    pub(crate) fn gate_account_read_after(&self, pass: u32) -> StoreGate {
        let (held, gate) = gate();
        self.plan().find_gate = Some((pass, held));
        gate
    }

    /// Whether this write is refused.
    fn refuse(&self) -> bool {
        let mut plan = self.plan();
        if plan.pass > 0 {
            plan.pass -= 1;
            return false;
        }
        if plan.refuse > 0 {
            plan.refuse -= 1;
            return true;
        }
        false
    }
}

#[async_trait]
impl Store for FlakyStore {
    async fn write(&self) -> Result<Box<dyn WriteTx>, StoreError> {
        let held = self.plan().write_gate.take();
        if let Some(held) = held {
            held.wait().await;
        }
        if self.refuse() {
            return Err(StoreError::Busy);
        }
        self.inner.write().await
    }

    fn users(&self) -> &dyn UserReads {
        self.inner.users()
    }

    fn sessions(&self) -> &dyn SessionReads {
        self.inner.sessions()
    }

    fn login_attempts(&self) -> &dyn LoginAttemptReads {
        self.inner.login_attempts()
    }

    fn mfa(&self) -> &dyn MfaReads {
        self.inner.mfa()
    }

    fn invites(&self) -> &dyn InviteReads {
        self.inner.invites()
    }

    fn accounts(&self) -> &dyn AccountReads {
        self
    }

    fn grants(&self) -> &dyn GrantReads {
        self.inner.grants()
    }

    fn enrollment_tokens(&self) -> &dyn EnrollmentTokenReads {
        self.inner.enrollment_tokens()
    }

    fn agents(&self) -> &dyn AgentReads {
        self.inner.agents()
    }

    fn bots(&self) -> &dyn BotReads {
        self.inner.bots()
    }

    fn modes(&self) -> &dyn ModeReads {
        self.inner.modes()
    }

    fn chat(&self) -> &dyn ChatReads {
        self.inner.chat()
    }

    fn envelopes(&self) -> &dyn EnvelopeReads {
        self.inner.envelopes()
    }

    fn audit(&self) -> &dyn AuditReads {
        self.inner.audit()
    }

    fn retention(&self) -> &dyn RetentionReads {
        self.inner.retention()
    }

    async fn retention_batch(&self) -> Result<Box<dyn RetentionBatch>, StoreError> {
        self.inner.retention_batch().await
    }

    async fn ping(&self) -> Result<(), StoreError> {
        self.inner.ping().await
    }

    fn commits(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.commits()
    }
}

#[async_trait]
impl AccountReads for FlakyStore {
    async fn find(&self, id: AccountId) -> Result<Option<McAccount>, StoreError> {
        let held = {
            let mut plan = self.plan();
            match plan.find_gate.take() {
                Some((0, held)) => Some(held),
                Some((pass, held)) => {
                    plan.find_gate = Some((pass - 1, held));
                    None
                }
                None => None,
            }
        };
        if let Some(held) = held {
            held.wait().await;
        }
        self.inner.accounts().find(id).await
    }

    async fn count_owned_by(&self, owner: UserId) -> Result<u32, StoreError> {
        self.inner.accounts().count_owned_by(owner).await
    }

    async fn count_offline(&self) -> Result<u64, StoreError> {
        self.inner.accounts().count_offline().await
    }

    async fn listed(
        &self,
        id: AccountId,
        viewer: UserId,
    ) -> Result<Option<ListedAccount>, StoreError> {
        self.inner.accounts().listed(id, viewer).await
    }

    async fn list_visible(
        &self,
        viewer: Actor,
        page: Page<AccountId>,
    ) -> Result<Vec<ListedAccount>, StoreError> {
        self.inner.accounts().list_visible(viewer, page).await
    }

    async fn awaiting_review(
        &self,
        viewer: Actor,
        page: Page<AccountId>,
    ) -> Result<Vec<ListedAccount>, StoreError> {
        self.inner.accounts().awaiting_review(viewer, page).await
    }

    async fn shared_of(
        &self,
        owner: UserId,
        viewer: UserId,
        page: Page<AccountId>,
    ) -> Result<Vec<ListedAccount>, StoreError> {
        self.inner.accounts().shared_of(owner, viewer, page).await
    }
}
