//! The respawn retry: respawns a dead bot, again and again if it has to.

use core::num::NonZeroU32;
use core::time::Duration;

use fleet_core::mc::{SessionError, SessionHandle};
use tracing::{debug, warn};

/// How a respawn retry ended. It never ends in success: the actor drops it
/// when the server confirms the respawn with `Respawned` (ADR-0020, S23).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RespawnOutcome {
    /// A call found the session ended; its own end follows.
    SessionEnded,
    /// Every call failed or went unconfirmed: the session must end with
    /// `RespawnFailed`.
    GaveUp,
}

/// Calls `respawn()` until the actor drops this future, which it does once
/// the server confirmed the respawn, and gives up after `attempts` failures
/// in a row (ADR-0013; ADR-0020, S23).
///
/// A failure is a call that failed, after which it waits `interval`, or a
/// call that succeeded but wasn't confirmed within `interval`: `Ok` only
/// means the request went out. Either way the next call follows `interval`
/// after the last one. The first failure of a death logs at `warn` and the
/// rest at `debug`, so each death warns once. `Closed` ends the retry at
/// `debug` and never counts: it only means the session has ended. Every
/// call has its own timeout in the session handle, so none of them can
/// hang.
pub(super) async fn respawn<S: SessionHandle>(
    session: S,
    interval: Duration,
    attempts: NonZeroU32,
) -> RespawnOutcome {
    let mut failures: u32 = 0;
    loop {
        let failed = match session.respawn().await {
            Ok(()) => {
                // Dropped here once the server confirms the respawn.
                tokio::time::sleep(interval).await;
                None
            }
            Err(SessionError::Closed) => {
                debug!("the session has ended; the respawn stops");
                return RespawnOutcome::SessionEnded;
            }
            Err(error) => Some(error),
        };
        failures = failures.saturating_add(1);
        match (failed, failures == 1) {
            (Some(error), true) => {
                warn!(%error, retry_in = ?interval, "a respawn failed; retrying");
            }
            (Some(error), false) => debug!(%error, failures, "a respawn failed again"),
            (None, true) => {
                warn!(waited = ?interval, "the server didn't confirm a respawn; retrying");
            }
            (None, false) => debug!(failures, "the server didn't confirm a respawn again"),
        }
        if failures >= attempts.get() {
            return RespawnOutcome::GaveUp;
        }
        if failed.is_some() {
            tokio::time::sleep(interval).await;
        }
    }
}
