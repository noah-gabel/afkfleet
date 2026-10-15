//! A flow's poller: one task per flow on the service's tracker (see the
//! [module docs](super)).
//!
//! 1. **Waiting.** It waits one interval, checks the code's expiry on the
//!    clock, then polls. `SlowDown` and a 429 lengthen the interval by 5 s; a
//!    passing failure only waits for the next poll. Declined, Expired, or
//!    a failure that won't pass (`Certificate`, `Unexpected`) end the flow.
//! 2. **Finishing.** After Microsoft's approval it starts a Minecraft
//!    session, which checks the game and the profile. A passing failure is
//!    retried after [`FINISH_BACKOFF`], for at most [`FINISH_WINDOW`] and
//!    never past the access token's expiry; a refusal ends the flow
//!    `refused`.
//! 3. **Storing** ([`commit`](super::commit)).
//!
//! Every wait and every call to Microsoft stops at once when the flow's
//! token is cancelled (a new start or shutdown): each is safe to drop,
//! since a poll can be repeated and nothing is stored before the commit.
//! Logs name the flow and its creator, never a code, a token, a name or a
//! UUID.

use core::num::NonZeroU32;
use core::time::Duration;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::account::RefusalReason;
use fleet_core::id::{AccountId, LinkFlowId, UserId};
use fleet_core::time::add;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use super::table::LinkFlows;
use super::{FINISH_BACKOFF, FINISH_WINDOW, Inner, LinkFlowStatus, commit};
use crate::app::users::ActingUser;
use crate::ports::msauth::{
    DeviceCode, Failure, MicrosoftTokens, MinecraftSession, PollInterval, PollOutcome, SessionError,
};

/// What a flow's end logs.
const ENDED: &str = "a link flow ended";
/// What a poll that failed for a moment logs.
const POLL_RETRY: &str = "a link flow's poll failed; it polls again after its interval";
/// What a poll that can't succeed logs.
const POLL_FAILED: &str = "a link flow's poll failed, and the flow ended";
/// What a failed session start that's retried logs.
const SESSION_RETRY: &str = "a link flow couldn't start the Minecraft session; it tries again";
/// What a session start that can't succeed logs.
const SESSION_FAILED: &str = "a link flow couldn't start the Minecraft session, and the flow ended";
/// What an approval that arrived for a replaced or cancelled flow logs.
const STALE: &str = "a link flow was approved after it was replaced; nothing was stored";

/// What a flow links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    /// A new account.
    Link,
    /// An existing Microsoft account, again.
    Relink {
        /// The account.
        account: AccountId,
    },
}

/// What a flow's poller needs.
pub(super) struct Running {
    /// The flow.
    pub(super) id: LinkFlowId,
    /// Who started it, and their session.
    pub(super) by: ActingUser,
    /// What it links.
    pub(super) target: Target,
    /// The device code: a secret, held only here.
    pub(super) code: DeviceCode,
    /// The interval Microsoft asked for.
    pub(super) interval: PollInterval,
    /// When the code expires.
    pub(super) expires_at: DateTime<Utc>,
    /// Stops this flow.
    pub(super) cancel: CancellationToken,
}

/// How the waiting phase ended.
enum Waited {
    /// The flow's token was cancelled.
    Cancelled,
    /// The flow ended with this status.
    Ended(LinkFlowStatus),
    /// Microsoft authorized it.
    Authorized(MicrosoftTokens),
}

/// Marks its flow `failed` when dropped while it still holds a place: a
/// poller that ended without an outcome (stopped, or aborted with its
/// runtime) never keeps one. Ending the flow first makes it do nothing.
struct FlowGuard {
    flows: LinkFlows,
    user: UserId,
    id: LinkFlowId,
}

impl Drop for FlowGuard {
    fn drop(&mut self) {
        self.flows.end(self.user, self.id, LinkFlowStatus::Failed);
    }
}

/// Runs the flow to its end; see the [module docs](self).
pub(super) async fn run(inner: Arc<Inner>, running: Running) {
    let user = running.by.actor.id;
    let _guard = FlowGuard {
        flows: inner.flows.clone(),
        user,
        id: running.id,
    };
    let status = match wait_for_approval(&inner, &running).await {
        Waited::Cancelled => return,
        Waited::Ended(status) => status,
        Waited::Authorized(tokens) => {
            if !inner.flows.begin_finishing(user, running.id) {
                info!(user_id = %user, flow_id = %running.id, "{STALE}");
                return;
            }
            match finish(&inner, &running, tokens).await {
                Some(status) => status,
                None => return,
            }
        }
    };
    let reason = match &status {
        LinkFlowStatus::Refused(reason) => Some(reason.as_str()),
        _ => None,
    };
    info!(
        user_id = %user,
        flow_id = %running.id,
        outcome = status.as_str(),
        reason,
        "{ENDED}"
    );
    inner.flows.end(user, running.id, status);
}

/// Polls until Microsoft answers for good or the code expires.
async fn wait_for_approval(inner: &Inner, running: &Running) -> Waited {
    let mut interval = running.interval;
    loop {
        tokio::select! {
            biased;
            () = running.cancel.cancelled() => return Waited::Cancelled,
            () = inner.ports.pace.wait(interval.get()) => {}
        }
        if inner.ports.clock.now() >= running.expires_at {
            return Waited::Ended(LinkFlowStatus::Expired);
        }
        let polled = tokio::select! {
            biased;
            // Dropping a poll is safe: the next one asks again.
            () = running.cancel.cancelled() => return Waited::Cancelled,
            polled = inner.ports.microsoft.poll(&running.code) => polled,
        };
        match polled {
            Ok(PollOutcome::Pending) => {}
            Ok(PollOutcome::SlowDown) => interval = interval.slowed_down(),
            Ok(PollOutcome::Declined) => return Waited::Ended(LinkFlowStatus::Declined),
            Ok(PollOutcome::Expired) => return Waited::Ended(LinkFlowStatus::Expired),
            Ok(PollOutcome::Authorized(tokens)) => return Waited::Authorized(tokens),
            Err(failure @ Failure::RateLimited) => {
                debug!(flow_id = %running.id, error = %failure, "{POLL_RETRY}");
                interval = interval.slowed_down();
            }
            Err(
                failure @ (Failure::Unreachable | Failure::TimedOut | Failure::Unavailable { .. }),
            ) => {
                debug!(flow_id = %running.id, error = %failure, "{POLL_RETRY}");
            }
            Err(failure @ (Failure::Certificate | Failure::Unexpected { .. })) => {
                error!(flow_id = %running.id, error = %failure, "{POLL_FAILED}");
                return Waited::Ended(LinkFlowStatus::Failed);
            }
        }
    }
}

/// Starts the Minecraft session and stores the account; `None` when the
/// flow was cancelled.
async fn finish(
    inner: &Inner,
    running: &Running,
    tokens: MicrosoftTokens,
) -> Option<LinkFlowStatus> {
    let MicrosoftTokens {
        access,
        access_expires_at,
        refresh,
    } = tokens;
    let deadline = add(inner.ports.clock.now(), FINISH_WINDOW).min(access_expires_at);
    let mut retries = Retries::new(deadline);
    let session: MinecraftSession = loop {
        let started = tokio::select! {
            biased;
            // Dropping the call is safe: nothing is stored yet.
            () = running.cancel.cancelled() => return None,
            started = inner.ports.microsoft.minecraft_session(&access) => started,
        };
        let failure = match started {
            Ok(session) => break session,
            Err(SessionError::NotOwned) => {
                return Some(LinkFlowStatus::Refused(RefusalReason::NotOwned));
            }
            Err(SessionError::NoProfile) => {
                return Some(LinkFlowStatus::Refused(RefusalReason::NoProfile));
            }
            Err(SessionError::Xbox(refusal)) => {
                let (reason, xerr) = refusal.reason();
                if let Some(xerr) = xerr {
                    info!(flow_id = %running.id, xerr, "Xbox Live refused a link flow's account");
                }
                return Some(LinkFlowStatus::Refused(reason));
            }
            Err(SessionError::Failed(failure)) => failure,
        };
        let delay = match failure {
            Failure::Unreachable
            | Failure::TimedOut
            | Failure::RateLimited
            | Failure::Unavailable { .. } => retries.next(inner.ports.clock.now()),
            Failure::Certificate | Failure::Unexpected { .. } => None,
        };
        let Some(delay) = delay else {
            error!(flow_id = %running.id, error = %failure, "{SESSION_FAILED}");
            return Some(LinkFlowStatus::Failed);
        };
        warn!(flow_id = %running.id, error = %failure, "{SESSION_RETRY}");
        if !pause(inner, &running.cancel, delay).await {
            return None;
        }
    };
    drop(access);
    commit::store(inner, running, refresh, session, &mut retries).await
}

/// The finishing phase's retries, one count for the session and the
/// commit: [`FINISH_BACKOFF`] until `deadline`.
pub(super) struct Retries {
    deadline: DateTime<Utc>,
    failures: u32,
}

impl Retries {
    const fn new(deadline: DateTime<Utc>) -> Self {
        Self {
            deadline,
            failures: 0,
        }
    }

    /// How long to wait at `now` before the next try, or `None` when it
    /// would come at or after the deadline.
    pub(super) fn next(&mut self, now: DateTime<Utc>) -> Option<Duration> {
        self.failures = self.failures.saturating_add(1);
        let failures = NonZeroU32::new(self.failures).unwrap_or(NonZeroU32::MIN);
        let delay = FINISH_BACKOFF.delay(failures);
        (add(now, delay) < self.deadline).then_some(delay)
    }
}

/// Waits `delay`; `false` when the flow was cancelled meanwhile.
pub(super) async fn pause(inner: &Inner, cancel: &CancellationToken, delay: Duration) -> bool {
    tokio::select! {
        biased;
        () = cancel.cancelled() => false,
        () = inner.ports.pace.wait(delay) => true,
    }
}
