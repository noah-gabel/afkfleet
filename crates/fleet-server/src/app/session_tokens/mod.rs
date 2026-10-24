//! [`SessionTokenService`]: the server's one source of Minecraft session
//! tokens for linked Microsoft accounts (Plan.md P9.5, ADR-0018). The
//! session grants (P10.6) hand its tokens to the agent that runs an
//! account's bot, through [`MinecraftSessions`]; refresh tokens never leave
//! the server (security rule 6).
//!
//! # Who gets a session
//! Only an active Microsoft account whose owner isn't disabled. An unknown,
//! offline, pending, `reauth_required` or `refused` account, or a disabled
//! owner, is refused without asking Microsoft. The account and its owner are
//! read on every request, so a cached token is never handed out for an
//! account that stopped being usable.
//!
//! # A session
//! Every new Minecraft token is a refresh, whose rotated refresh token is
//! stored first, then [`MicrosoftAuthProvider::minecraft_session`]. No
//! Microsoft access token is kept. The session's profile must have the
//! account's UUID; a renamed profile's current name is handed out and stored
//! (`account.rename`). Each session is cached until [`REUSE_MARGIN`] before
//! it expires, read from the [`Clock`]. A fresh request, after a rejected
//! session, reuses only a session whose minting started after it arrived.
//!
//! # One refresh at a time, never lost
//! Each account has a slot with a flight lock. A request that needs
//! Microsoft takes the lock (waiting at most the wait bound, else
//! [`SessionTokenError::InFlight`]) and hands its owned guard to a task on
//! the service's [`TaskTracker`]; the caller only awaits the task. So a
//! refresh and the store of its rotated token finish together even when the
//! caller is dropped or times out ([`SessionTokenError::TimedOut`]), and the
//! next caller finds the result. The task checks the stop token only before
//! it sends a refresh; once sent, it runs to the end, bounded by the
//! adapter's bounds and the database's.
//!
//! The rotated token is stored before anything is handed out, in one
//! transaction with its `account.refresh` entry, and only while the row
//! still holds the envelope that was used. If the store fails, nothing is
//! handed out, the status is left alone, and the token stays in the slot:
//! the next request stores it before any refresh. It's discarded only when
//! the row's envelope changed (the account was linked again) or the row is
//! gone.
//!
//! # Microsoft's refusals
//! - `invalid_grant` marks the account `reauth_required`; `NotOwned`,
//!   `NoProfile` and the Xbox refusals mark it `refused` with their reason
//!   (`account.set_status`).
//! - A profile with another UUID marks it `refused` with the reason
//!   `profile_mismatch` (`security.account_profile_mismatch`).
//! - Each change is made only while the account is still active and holds
//!   the envelope that was used.
//!
//! # Backoff
//! Every other failure backs the account off ([`ACCOUNT_BACKOFF`]), and a
//! 429 pauses every account ([`GLOBAL_PAUSE`]). Meanwhile a request that
//! needs Microsoft gets [`SessionTokenError::BackingOff`] at once; a cached
//! session is still handed out. A session minted for an account ends its
//! backoff, and starts the pause's run over.

mod error;
mod flight;
mod slot;

use core::fmt;
use core::time::Duration;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::account::AccountStatus;
use fleet_core::id::AccountId;
use fleet_core::resilience::Doubling;
use fleet_core::system::Clock;
use fleet_core::value::{McUsername, McUuid};
use tokio::time::{Instant, timeout_at};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{error, warn};

pub use self::error::{SessionTokenError, WaitBoundError};
use self::slot::{Backoff, Slot, later};
use super::audit::AuditService;
use crate::ports::accounts::AccountCredential;
use crate::ports::live_hints::LiveHints;
use crate::ports::msauth::{Failure, MicrosoftAuthProvider, MinecraftSession};
use crate::ports::store::Store;
use crate::ports::vault::Vault;

/// The longest a request waits, for the account's flight and then for its
/// task's answer: above the task's worst case, the adapter's 20 s refresh
/// and 60 s session plus a few database acquires of 5 s.
pub const WAIT_BOUND: Duration = Duration::from_mins(2);

/// How long before it expires a cached session stops being handed out: the
/// agent's connect timeout, clock skew and the hop to the agent fit well
/// inside.
pub const REUSE_MARGIN: Duration = Duration::from_mins(10);

/// An account's backoff after a failed call: 5 s, doubling to 5 minutes.
pub const ACCOUNT_BACKOFF: Doubling = Doubling::new(Duration::from_secs(5), Duration::from_mins(5));

/// Every account's pause after Microsoft rate limited the server: 30 s,
/// doubling to 10 minutes.
pub const GLOBAL_PAUSE: Doubling = Doubling::new(Duration::from_secs(30), Duration::from_mins(10));

/// Where the session grants get an account's Minecraft session (P10.6;
/// ADR-0019, R46): the [`SessionTokenService`] in the server, a fake in the
/// gRPC port's tests, which run no Microsoft.
#[async_trait]
pub trait MinecraftSessions: Send + Sync + fmt::Debug {
    /// A Minecraft session for `account`; with `fresh`, none minted before
    /// this request.
    ///
    /// # Errors
    /// A [`SessionTokenError`].
    async fn session(
        &self,
        account: AccountId,
        fresh: bool,
    ) -> Result<MinecraftSession, SessionTokenError>;
}

/// The ports the service runs on.
#[derive(Debug, Clone)]
pub struct SessionTokenPorts {
    /// Every expiry and backoff, and the audit entries' time.
    pub clock: Arc<dyn Clock>,
    /// The accounts, their owners and the audit log.
    pub store: Arc<dyn Store>,
    /// Opens and seals the refresh tokens.
    pub vault: Arc<dyn Vault>,
    /// Microsoft.
    pub microsoft: Arc<dyn MicrosoftAuthProvider>,
    /// The live events sockets, told which bot a change of its account's
    /// status or name changed (ADR-0020, S42).
    pub live_hints: Arc<dyn LiveHints>,
}

/// The session-token service; see the [module docs](self). Clones share
/// everything but their wait bound.
#[derive(Clone)]
pub struct SessionTokenService {
    inner: Arc<Inner>,
    wait_bound: Duration,
}

/// What a task that ended without an answer logs.
const INTERRUPTED: &str = "a session-token task ended without an answer";

/// What every clone shares.
struct Inner {
    ports: SessionTokenPorts,
    audit: AuditService,
    /// Owns every flight; [`SessionTokenService::shutdown`] waits for them.
    tasks: TaskTracker,
    /// Checked before a refresh is sent, and while waiting for a flight.
    stop: CancellationToken,
    /// Each account's slot. Only accounts that may get a session have one,
    /// so it holds no more slots than there are accounts.
    slots: Mutex<HashMap<AccountId, Arc<Slot>>>,
    /// The pause after a 429, for every account.
    pause: Mutex<Backoff>,
}

/// An account that may get a session, as read for a request.
struct Usable {
    /// The profile's UUID the session must have.
    uuid: McUuid,
    /// The name as stored.
    name: McUsername,
    /// Its refresh token's envelope.
    envelope: Vec<u8>,
}

impl Inner {
    /// Reads `id` and its owner: the account if it may get a session.
    async fn usable(&self, id: AccountId) -> Result<Usable, SessionTokenError> {
        let account = self
            .ports
            .store
            .accounts()
            .find(id)
            .await?
            .ok_or(SessionTokenError::NotFound)?;
        let AccountCredential::Microsoft {
            uuid,
            refresh_envelope,
            status,
        } = account.credential
        else {
            return Err(SessionTokenError::Offline);
        };
        if status != AccountStatus::Active {
            return Err(SessionTokenError::NotActive { status });
        }
        let owner = self.ports.store.users().get(account.owner).await?;
        // A missing owner can't happen (a foreign key); it counts as disabled.
        if owner.is_none_or(|owner| owner.disabled) {
            return Err(SessionTokenError::OwnerDisabled);
        }
        Ok(Usable {
            uuid,
            name: account.name,
            envelope: refresh_envelope,
        })
    }

    /// The slots, poison-tolerantly.
    fn slots(&self) -> MutexGuard<'_, HashMap<AccountId, Arc<Slot>>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The pause, poison-tolerantly.
    fn pause(&self) -> MutexGuard<'_, Backoff> {
        self.pause.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `id`'s slot, if it has one.
    fn existing_slot(&self, id: AccountId) -> Option<Arc<Slot>> {
        self.slots().get(&id).map(Arc::clone)
    }

    /// `id`'s slot, made when it has none.
    fn slot(&self, id: AccountId) -> Arc<Slot> {
        Arc::clone(
            self.slots()
                .entry(id)
                .or_insert_with(|| Arc::new(Slot::new())),
        )
    }

    /// Forgets what `refusal` makes useless: the cached session of a refused
    /// account, and the kept token of one that's gone. A kept token waits
    /// through any other refusal (ADR-0018).
    fn forget(&self, id: AccountId, refusal: &SessionTokenError) {
        if !refusal.refuses_the_account() {
            return;
        }
        let Some(slot) = self.existing_slot(id) else {
            return;
        };
        slot.clear_cache();
        if matches!(refusal, SessionTokenError::NotFound) {
            // A flight that holds the lock finds the row gone itself.
            if let Ok(mut flight) = slot.flight.try_lock() {
                flight.unsaved = None;
            }
        }
    }

    /// Until when a call to Microsoft for `slot`'s account waits at `now`:
    /// the later of its backoff and the pause.
    fn backoff_until(&self, slot: &Slot, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let pause = self.pause().active_until(now);
        later(slot.backoff_until(now), pause)
    }

    /// Records `failure` at `now`: the account backs off, and a 429 pauses
    /// every account.
    fn record_failure(&self, slot: &Slot, failure: &Failure, now: DateTime<Utc>) {
        slot.fail(ACCOUNT_BACKOFF, now);
        if matches!(failure, Failure::RateLimited) {
            let until = self.pause().fail(GLOBAL_PAUSE, now);
            warn!(%until, "Microsoft is rate limiting the server; every account waits");
        }
    }

    /// Records a session minted for `slot`'s account: its backoff ends, and
    /// the pause's run starts over, though a running pause runs out.
    fn record_success(&self, slot: &Slot) {
        slot.succeed();
        self.pause().reset_streak();
    }
}

impl SessionTokenService {
    /// A service over `ports`, which stops with `stop` (it keeps a child of
    /// it), waiting at most [`WAIT_BOUND`].
    #[must_use]
    pub fn new(ports: SessionTokenPorts, stop: &CancellationToken) -> Self {
        Self {
            inner: Arc::new(Inner {
                audit: AuditService::new(Arc::clone(&ports.clock), Arc::clone(&ports.store)),
                ports,
                tasks: TaskTracker::new(),
                stop: stop.child_token(),
                slots: Mutex::default(),
                pause: Mutex::default(),
            }),
            wait_bound: WAIT_BOUND,
        }
    }

    /// This service with a shorter wait bound, for tests. It can only
    /// shorten the wait, never lengthen it or make it unbounded.
    ///
    /// # Errors
    /// [`WaitBoundError`] for zero or more than [`WAIT_BOUND`].
    pub fn with_wait_bound(self, bound: Duration) -> Result<Self, WaitBoundError> {
        if bound.is_zero() || bound > WAIT_BOUND {
            return Err(WaitBoundError);
        }
        Ok(Self {
            wait_bound: bound,
            ..self
        })
    }

    /// A Minecraft session for `account`. With `fresh`, after a rejected
    /// session, a session minted before this request isn't reused.
    ///
    /// # Errors
    /// A [`SessionTokenError`]; [`SessionTokenError::retryable`] tells
    /// whether asking again later may help.
    pub async fn session(
        &self,
        account: AccountId,
        fresh: bool,
    ) -> Result<MinecraftSession, SessionTokenError> {
        let deadline = Instant::now() + self.wait_bound;
        // First, before any await: which minting a fresh request saw.
        let seen = self
            .inner
            .existing_slot(account)
            .map_or(0, |slot| slot.mint());
        if let Err(refusal) = self.inner.usable(account).await {
            self.inner.forget(account, &refusal);
            return Err(refusal);
        }
        let slot = self.inner.slot(account);
        let clock = &self.inner.ports.clock;
        if !fresh && let Some(session) = slot.reusable(clock.now(), false, seen) {
            return Ok(session);
        }
        let flight = tokio::select! {
            biased;
            // Both are cancel-safe: a dropped `lock_owned` leaves the queue.
            () = self.inner.stop.cancelled() => return Err(SessionTokenError::ShuttingDown),
            locked = timeout_at(deadline, Arc::clone(&slot.flight).lock_owned()) => {
                locked.map_err(|_| SessionTokenError::InFlight)?
            }
        };
        let now = clock.now();
        if let Some(session) = slot.reusable(now, fresh, seen) {
            return Ok(session);
        }
        if flight.unsaved.is_none()
            && let Some(until) = self.inner.backoff_until(&slot, now)
        {
            return Err(SessionTokenError::BackingOff { until });
        }
        // The flight checks the stop again right before it sends a refresh.
        let task =
            self.inner
                .tasks
                .spawn(flight::fly(Arc::clone(&self.inner), account, slot, flight));
        match timeout_at(deadline, task).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => {
                error!(account_id = %account, "{INTERRUPTED}");
                Err(SessionTokenError::Interrupted)
            }
            // The task goes on, and the next request finds its result.
            Err(_) => Err(SessionTokenError::TimedOut),
        }
    }

    /// Stops the service: no new refresh starts, a request waiting for an
    /// account's flight is refused, and this waits for every running task,
    /// each of which ends within its own bounds. The caller bounds the wait:
    /// serve gives it the drain's time (ADR-0019, S64).
    pub async fn shutdown(&self) {
        self.inner.stop.cancel();
        self.inner.tasks.close();
        self.inner.tasks.wait().await;
    }

    /// How many of its tasks still run: what serve's shutdown leaves
    /// behind when they outlast its bound.
    #[must_use]
    pub fn running(&self) -> usize {
        self.inner.tasks.len()
    }
}

#[async_trait]
impl MinecraftSessions for SessionTokenService {
    async fn session(
        &self,
        account: AccountId,
        fresh: bool,
    ) -> Result<MinecraftSession, SessionTokenError> {
        Self::session(self, account, fresh).await
    }
}

/// Opaque: only the wait bound.
impl fmt::Debug for SessionTokenService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionTokenService")
            .field("wait_bound", &self.wait_bound)
            .finish_non_exhaustive()
    }
}
