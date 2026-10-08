//! The respawn retry: respawns a dead bot, again and again if it has to.

use core::num::NonZeroU32;
use core::time::Duration;

use fleet_core::mc::{SessionError, SessionHandle};
use tracing::{debug, warn};

/// How a respawn retry ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RespawnOutcome {
    /// A call succeeded. The ports can't tell whether the server took it, so
    /// that counts as alive (ADR-0013).
    Respawned,
    /// A call found the session ended; its own end follows.
    SessionEnded,
    /// Every call failed: the session must end with `RespawnFailed`.
    GaveUp,
}

/// Calls `respawn()` until it succeeds, waiting `interval` after each failed
/// call, and gives up after `attempts` failed calls in a row (ADR-0013).
///
/// The first failure logs at `warn` and the rest at `debug`, so each death
/// warns once. `Closed` ends the retry at `debug` and never counts: it only
/// means the session has ended. Every call has its own timeout in the
/// session handle, so none of them can hang.
pub(super) async fn respawn<S: SessionHandle>(
    session: S,
    interval: Duration,
    attempts: NonZeroU32,
) -> RespawnOutcome {
    let mut failures: u32 = 0;
    loop {
        match session.respawn().await {
            Ok(()) => return RespawnOutcome::Respawned,
            Err(SessionError::Closed) => {
                debug!("the session has ended; the respawn stops");
                return RespawnOutcome::SessionEnded;
            }
            Err(error) => {
                failures = failures.saturating_add(1);
                if failures == 1 {
                    warn!(%error, retry_in = ?interval, "a respawn failed; retrying");
                } else {
                    debug!(%error, failures, "a respawn failed again");
                }
                if failures >= attempts.get() {
                    return RespawnOutcome::GaveUp;
                }
                tokio::time::sleep(interval).await;
            }
        }
    }
}
