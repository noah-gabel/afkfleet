//! [`LinkFlowService`]: linking a Microsoft account through Microsoft's
//! device-code flow (Plan.md P9.4, §7.4, ADR-0018).
//!
//! # Starting a flow
//! [`LinkFlowService::start`] refuses, cheapest check first:
//! 1. while the server stops ([`LinkStartError::ShuttingDown`]);
//! 2. while the user's flow is finishing or another start of theirs is in
//!    progress ([`LinkStartError::InProgress`]);
//! 3. while `total` other users hold a flow ([`LinkStartError::AllBusy`]);
//! 4. when the user started their daily cap of flows in the last
//!    [`LINK_START_WINDOW`] ([`LinkStartError::DailyCap`]), counted from the
//!    audit log, so a restart doesn't reset it;
//! 5. when a Member already owns their quota of accounts
//!    ([`LinkStartError::OverQuota`]).
//!
//! Only then does it ask Microsoft for a device code, and records
//! `account.start_link` in a transaction that re-checks the user's session
//! and the two counts. The answer carries the code the user types at
//! Microsoft's page; nothing else ever shows it, and nothing logs it.
//!
//! # One flow per user
//! A start replaces the user's waiting flow: its poller stops, so its code
//! links nothing any more. A flow Microsoft already authorized isn't
//! replaced. A finished flow's result stays readable for
//! [`FINISHED_RETENTION`] ([`LinkFlows`]).
//!
//! # The poller
//! Each flow's poller runs on the service's [`TaskTracker`] with its own
//! child [`CancellationToken`]. It polls once per interval, slower after
//! Microsoft asks it to, until the user approves, declines or the code
//! expires; a passing failure only waits for the next poll. After the
//! approval it starts a Minecraft session, which checks the game and the
//! profile, retrying a passing failure for at most [`FINISH_WINDOW`].
//!
//! # Storing the account
//! The refresh token is sealed for the new account's ID before the
//! transaction. Inside it, the creator and the session that started the flow
//! are read again (a revoked session, a logout or a disabled creator stores
//! nothing), then the quota, then the insert: an already linked Minecraft
//! account is `taken`, naming nobody. The creator owns the account, which
//! gives them Manage; it's `pending` until approved, or `active` for the
//! Owner. Each outcome that touches the database is audited
//! (`account.link`).
//!
//! # Linking an account again (P9.6; ADR-0018, group E)
//! [`LinkFlowService::start_relink`] starts the same flow for an existing
//! Microsoft account that its caller holds Manage on, with the same caps,
//! the same daily count (its start is an `account.start_link` too, naming
//! the account) and the same one flow per user. It refuses a pending
//! account, which would skip its approval, and an offline one, before
//! Microsoft is asked; there's no quota. Its commit reads the account, the
//! caller's grant and the owner's role again and asks `authorize()` again:
//! an account gone, no longer the caller's to link again, or turned pending
//! stores nothing. A profile with another UUID than the account's stores
//! nothing either, and ends the flow `refused`/`profile_mismatch` (an
//! `account.relink` Failure). Otherwise the new refresh token replaces the
//! stored one only while the row still holds the one read in the same
//! transaction, the account is `active` again without a reason, and it
//! takes the profile's current name (`account.relink`). The new envelope
//! makes a refresh in flight drop its rotated token (ADR-0018, group C).
//!
//! # Shutdown
//! [`LinkFlowService::shutdown`] cancels every flow at once: a wait, a poll
//! or a session call in flight. Only a commit that started finishes. The
//! flows are lost with the server, and the users start again.

mod commit;
mod error;
mod poller;
mod table;

use core::fmt;
use core::num::NonZeroU32;
use core::time::Duration;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::account::{
    AccountStatus, LINK_START_WINDOW, RefusalReason, link_start_wait, over_quota, relinkable,
};
use fleet_core::audit::{AuditAction, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::authz::{Actor, Permission, authorize};
use fleet_core::id::{AccountId, LinkFlowId, UserId};
use fleet_core::resilience::Doubling;
use fleet_core::system::{Clock, SecureRandom, mint};
use fleet_core::time::sub;
use fleet_core::value::McUsername;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::info;

pub use self::error::LinkStartError;
use self::poller::{Running, Target};
pub use self::table::LinkFlows;
use super::audit::AuditService;
use super::users::{ActingUser, recheck};
use crate::ports::accounts::ListedAccount;
use crate::ports::msauth::{MicrosoftAuthProvider, UserCode, VerificationUri};
use crate::ports::pace::Pace;
use crate::ports::store::{Store, WriteTx};
use crate::ports::vault::Vault;

/// How long a finished flow's result stays readable.
pub const FINISHED_RETENTION: Duration = Duration::from_mins(15);

/// How long after Microsoft's approval the flow keeps trying to start the
/// Minecraft session and store the account.
pub const FINISH_WINDOW: Duration = Duration::from_mins(5);

/// The wait between those tries: 5 s, doubling to a minute.
pub const FINISH_BACKOFF: Doubling = Doubling::new(Duration::from_secs(5), Duration::from_mins(1));

/// How long a start refused while the user's flow is finishing, or another
/// start of theirs is in progress, asks the client to wait.
pub const IN_PROGRESS_RETRY_AFTER: Duration = Duration::from_secs(5);

/// How long a start refused because every place is taken asks the client to
/// wait.
pub const ALL_BUSY_RETRY_AFTER: Duration = Duration::from_secs(30);

/// What a started flow logs.
const STARTED: &str = "a link flow started";
/// What a start that replaced the user's waiting flow logs.
const REPLACED: &str = "a waiting link flow was replaced by a new start";

/// The ports the service runs on.
#[derive(Debug, Clone)]
pub struct LinkFlowPorts {
    /// Every expiry and wait, the audit entries' time, and new IDs.
    pub clock: Arc<dyn Clock>,
    /// New IDs.
    pub random: Arc<dyn SecureRandom>,
    /// The accounts, the users and their sessions, and the audit log.
    pub store: Arc<dyn Store>,
    /// Seals the refresh tokens.
    pub vault: Arc<dyn Vault>,
    /// Microsoft.
    pub microsoft: Arc<dyn MicrosoftAuthProvider>,
    /// The waits between polls and tries.
    pub pace: Arc<dyn Pace>,
}

/// The caps (`[rate_limits]`, `[accounts]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkFlowSettings {
    /// How many users hold a flow at once on the whole server.
    pub total: NonZeroU32,
    /// How many flows one user starts in any [`LINK_START_WINDOW`].
    pub per_user_per_day: NonZeroU32,
    /// How many accounts a Member may own.
    pub max_per_member: NonZeroU32,
}

/// A started flow, for its creator's answer only.
#[derive(Debug)]
pub struct StartedFlow {
    /// The flow.
    pub id: LinkFlowId,
    /// The code the user types at Microsoft's page.
    pub user_code: UserCode,
    /// Microsoft's page.
    pub verification_uri: VerificationUri,
    /// When the code expires.
    pub expires_at: DateTime<Utc>,
}

/// Where a flow stands.
#[derive(Clone, PartialEq, Eq)]
pub enum LinkFlowStatus {
    /// Waiting for the user at Microsoft's page until `expires_at`.
    Waiting {
        /// When the code expires.
        expires_at: DateTime<Utc>,
    },
    /// Microsoft authorized it: the profile is checked and the account
    /// stored.
    Finishing,
    /// The user declined at Microsoft's page.
    Declined,
    /// The code expired before the user approved it.
    Expired,
    /// The Minecraft account is linked already; the answer names nobody.
    Taken,
    /// The Member owns their quota of accounts already.
    OverQuota,
    /// Minecraft's side refused the account, or, linking an account again,
    /// the profile isn't the account's (`profile_mismatch`).
    Refused(RefusalReason),
    /// Microsoft or the server failed, or the creator's session ended.
    Failed,
    /// The account is stored.
    Linked {
        /// The new account, or the one linked again.
        account: AccountId,
        /// `pending`, or `active` for the Owner's and for an account linked
        /// again.
        status: AccountStatus,
        /// Its Minecraft name.
        name: McUsername,
    },
}

impl LinkFlowStatus {
    /// The status's stable name, as the API and the logs name it.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Waiting { .. } => "waiting",
            Self::Finishing => "finishing",
            Self::Declined => "declined",
            Self::Expired => "expired",
            Self::Taken => "taken",
            Self::OverQuota => "over_quota",
            Self::Refused(_) => "refused",
            Self::Failed => "failed",
            Self::Linked { .. } => "linked",
        }
    }
}

/// Without the Minecraft name.
impl fmt::Debug for LinkFlowStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Waiting { expires_at } => f
                .debug_struct("Waiting")
                .field("expires_at", expires_at)
                .finish(),
            Self::Refused(reason) => f.debug_tuple("Refused").field(reason).finish(),
            Self::Linked {
                account, status, ..
            } => f
                .debug_struct("Linked")
                .field("account", account)
                .field("status", status)
                .finish_non_exhaustive(),
            Self::Finishing
            | Self::Declined
            | Self::Expired
            | Self::Taken
            | Self::OverQuota
            | Self::Failed => f.write_str(self.as_str()),
        }
    }
}

/// A flow, as its creator reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkFlowView {
    /// Who started it: nobody else may read it.
    pub creator: UserId,
    /// Where it stands.
    pub status: LinkFlowStatus,
}

/// The link flows; see the [module docs](self). Clones share everything.
#[derive(Clone)]
pub struct LinkFlowService {
    inner: Arc<Inner>,
}

/// What every clone and every poller share.
struct Inner {
    ports: LinkFlowPorts,
    settings: LinkFlowSettings,
    audit: AuditService,
    flows: LinkFlows,
    /// Owns every poller; [`LinkFlowService::shutdown`] waits for them.
    tasks: TaskTracker,
    /// Each flow's token is a child of it.
    stop: CancellationToken,
}

impl LinkFlowService {
    /// A service over `ports` with `settings`.
    #[must_use]
    pub fn new(ports: LinkFlowPorts, settings: LinkFlowSettings) -> Self {
        Self {
            inner: Arc::new(Inner {
                audit: AuditService::new(Arc::clone(&ports.clock), Arc::clone(&ports.store)),
                flows: LinkFlows::new(Arc::clone(&ports.clock)),
                ports,
                settings,
                tasks: TaskTracker::new(),
                stop: CancellationToken::new(),
            }),
        }
    }

    /// Starts a flow for `by`; see the [module docs](self).
    ///
    /// # Errors
    /// The refusals in the module docs' order, then
    /// [`LinkStartError::Microsoft`] when Microsoft failed,
    /// [`LinkStartError::SessionGone`] when the session ended meanwhile, and
    /// the server's failures.
    pub async fn start(&self, by: &ActingUser) -> Result<StartedFlow, LinkStartError> {
        self.start_flow(by, Target::Link).await
    }

    /// Starts a flow for `by` that links the Microsoft account `account`
    /// again; see the [module docs](self).
    ///
    /// # Errors
    /// The refusals in the module docs' order, with
    /// [`LinkStartError::AccountGone`], [`LinkStartError::Denied`] and
    /// [`LinkStartError::NotRelinkable`] in place of the quota, then
    /// [`LinkStartError::Microsoft`], [`LinkStartError::SessionGone`], and
    /// the server's failures.
    pub async fn start_relink(
        &self,
        by: &ActingUser,
        account: AccountId,
    ) -> Result<StartedFlow, LinkStartError> {
        self.start_flow(by, Target::Relink { account }).await
    }

    /// Starts a flow of either kind; see the [module docs](self).
    async fn start_flow(
        &self,
        by: &ActingUser,
        target: Target,
    ) -> Result<StartedFlow, LinkStartError> {
        let inner = &self.inner;
        if inner.stop.is_cancelled() {
            return Err(LinkStartError::ShuttingDown);
        }
        let user = by.actor.id;
        let reservation = inner.flows.reserve(user, inner.settings.total)?;
        let now = inner.ports.clock.now();
        let starts = inner
            .ports
            .store
            .audit()
            .user_successes_since(
                user,
                &AuditAction::ACCOUNT_START_LINK,
                sub(now, LINK_START_WINDOW),
            )
            .await?;
        if let Some(retry_after) = link_start_wait(&starts, inner.settings.per_user_per_day, now) {
            return Err(LinkStartError::DailyCap { retry_after });
        }
        target_allowed(inner, &by.actor, target).await?;
        let device = tokio::select! {
            biased;
            // Dropping the call is safe: nothing was stored yet.
            () = inner.stop.cancelled() => return Err(LinkStartError::ShuttingDown),
            device = inner.ports.microsoft.start_device_flow() => {
                device.map_err(LinkStartError::Microsoft)?
            }
        };
        if let Some(replaced) = reservation.replace()? {
            info!(user_id = %user, flow_id = %replaced, "{REPLACED}");
        }
        let now = inner.ports.clock.now();
        let id: LinkFlowId =
            mint(now, inner.ports.random.as_ref()).map_err(LinkStartError::Mint)?;
        let mut metadata = AuditMetadata::new().id("flow", id)?;
        if let Target::Relink { account } = target {
            metadata = metadata.id("account", account)?;
        }
        let entry = inner
            .audit
            .entry(AuditAction::ACCOUNT_START_LINK, AuditOutcome::Success)
            .actor(user)
            .ip(by.ip)
            .target(AuditTarget::User(user))
            .metadata(metadata);
        let mut tx = inner.ports.store.write().await?;
        let Some(acting) = recheck(tx.as_mut(), by).await? else {
            return Err(LinkStartError::SessionGone);
        };
        let starts = tx
            .audit()
            .user_successes_since(
                user,
                &AuditAction::ACCOUNT_START_LINK,
                sub(now, LINK_START_WINDOW),
            )
            .await?;
        if let Some(retry_after) = link_start_wait(&starts, inner.settings.per_user_per_day, now) {
            return Err(LinkStartError::DailyCap { retry_after });
        }
        target_still_allowed(inner, tx.as_mut(), &acting.actor, target).await?;
        tx.commit(entry).await?;
        let cancel = inner.stop.child_token();
        reservation.register(id, device.expires_at, cancel.clone());
        inner.tasks.spawn(poller::run(
            Arc::clone(&self.inner),
            Running {
                id,
                by: *by,
                target,
                code: device.code,
                interval: device.interval,
                expires_at: device.expires_at,
                cancel,
            },
        ));
        info!(user_id = %user, flow_id = %id, "{STARTED}");
        Ok(StartedFlow {
            id,
            user_code: device.user_code,
            verification_uri: device.verification_uri,
            expires_at: device.expires_at,
        })
    }

    /// The flow `id` and who started it; `None` for an unknown, replaced or
    /// long finished one.
    #[must_use]
    pub fn status(&self, id: LinkFlowId) -> Option<LinkFlowView> {
        self.inner.flows.view(id)
    }

    /// The flows, for the housekeeping's purge.
    #[must_use]
    pub fn flows(&self) -> LinkFlows {
        self.inner.flows.clone()
    }

    /// Stops the service: every flow is cancelled, a start in progress is
    /// refused, and this waits for every poller, each of which ends at once
    /// or after a commit that started. The caller bounds the wait (serve's
    /// shutdown deadline).
    pub async fn shutdown(&self) {
        self.inner.stop.cancel();
        self.inner.tasks.close();
        self.inner.tasks.wait().await;
    }
}

/// The check of the flow's kind before Microsoft is asked: the Member quota
/// for a new link, the account itself for a link again.
async fn target_allowed(
    inner: &Inner,
    actor: &Actor,
    target: Target,
) -> Result<(), LinkStartError> {
    let accounts = inner.ports.store.accounts();
    match target {
        Target::Link => {
            let owned = accounts.count_owned_by(actor.id).await?;
            if over_quota(actor.role, owned, inner.settings.max_per_member.get()) {
                return Err(LinkStartError::OverQuota);
            }
            Ok(())
        }
        Target::Relink { account } => {
            let listed = accounts.listed(account, actor.id).await?;
            may_relink(actor, listed.as_ref())
        }
    }
}

/// The same check again, inside the start's transaction, with the role it
/// read there.
async fn target_still_allowed(
    inner: &Inner,
    tx: &mut dyn WriteTx,
    actor: &Actor,
    target: Target,
) -> Result<(), LinkStartError> {
    match target {
        Target::Link => {
            let owned = tx.accounts().count_owned_by(actor.id).await?;
            if over_quota(actor.role, owned, inner.settings.max_per_member.get()) {
                return Err(LinkStartError::OverQuota);
            }
            Ok(())
        }
        Target::Relink { account } => {
            let listed = tx.accounts().listed(account, actor.id).await?;
            may_relink(actor, listed.as_ref())
        }
    }
}

/// Whether `actor` may link `listed` again: it exists, they hold Manage on
/// it, and it's a Microsoft account that isn't pending.
fn may_relink(actor: &Actor, listed: Option<&ListedAccount>) -> Result<(), LinkStartError> {
    let listed = listed.ok_or(LinkStartError::AccountGone)?;
    authorize(actor, Permission::RelinkAccount, &listed.context())
        .map_err(LinkStartError::Denied)?;
    if relinkable(listed.kind, listed.status) {
        Ok(())
    } else {
        Err(LinkStartError::NotRelinkable)
    }
}

/// Opaque: counts only.
impl fmt::Debug for LinkFlowService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LinkFlowService")
            .field("flows", &self.inner.flows)
            .field("settings", &self.inner.settings)
            .finish_non_exhaustive()
    }
}
