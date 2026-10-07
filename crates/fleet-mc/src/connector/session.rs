//! [`McSession`]: the actor's handle to one azalea session.

use core::fmt;
use core::future::Future;
use core::time::Duration;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use azalea::Client;
use fleet_core::chat::ChatMessage;
use fleet_core::id::BotId;
use fleet_core::mc::{Liveness, SessionError, SessionHandle};
use fleet_core::mode::GameAction;
use tokio::sync::{oneshot, watch};
use tokio::time;
use tracing::{debug, warn};

use super::driver::ClientSlot;
use crate::actions;
use crate::events::{BridgeControl, LivenessStamps, Phase};
use crate::host::HostThread;
use crate::signing::{self, Decision, SigningState};

/// How much longer than the driver's own wait for azalea's runner
/// [`disconnect`](McSession::disconnect) waits for the driver to end, before
/// it shuts the host thread down regardless.
const DRIVER_GRACE: Duration = Duration::from_secs(1);

/// The handle to one azalea session (the `SessionHandle` of
/// [`AzaleaConnector`](super::AzaleaConnector)). Clones control the same
/// session.
///
/// Every call runs on the session's host thread and waits for the answer
/// with the job timeout, so none blocks the caller's runtime. Before the bot
/// has joined, calls fail with [`SessionError::NotInWorld`]; once the session
/// ended or is torn down, with [`SessionError::Closed`].
#[derive(Clone)]
pub struct McSession {
    inner: Arc<Inner>,
}

struct Inner {
    bot_id: BotId,
    host: HostThread,
    slot: Arc<ClientSlot>,
    stamps: Arc<LivenessStamps>,
    control: BridgeControl,
    /// Taken by the first `disconnect()`. Dropping it, with the last handle,
    /// stops the driver too.
    stop: Mutex<Option<oneshot::Sender<()>>>,
    /// Closes when the driver has ended.
    driver_ended: watch::Receiver<()>,
    /// How long `disconnect()` waits for the driver to end.
    driver_wait: Duration,
    /// Where the session's chat signing is, published by its App.
    signing: watch::Receiver<SigningState>,
    /// How long after the game state chat may wait for signing.
    chat_signing_timeout: Duration,
    /// Whether unavailable chat was logged at `warn` already.
    warned_chat_unavailable: AtomicBool,
}

impl fmt::Debug for McSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McSession")
            .field("bot_id", &self.inner.bot_id)
            .field("host", &self.inner.host)
            .finish_non_exhaustive()
    }
}

/// What a session's handle is built from.
pub(super) struct Parts {
    pub(super) bot_id: BotId,
    pub(super) host: HostThread,
    pub(super) slot: Arc<ClientSlot>,
    pub(super) stamps: Arc<LivenessStamps>,
    pub(super) control: BridgeControl,
    pub(super) stop: oneshot::Sender<()>,
    pub(super) driver_ended: watch::Receiver<()>,
    pub(super) app_exit_timeout: Duration,
    pub(super) signing: watch::Receiver<SigningState>,
    pub(super) chat_signing_timeout: Duration,
}

impl McSession {
    pub(super) fn new(parts: Parts) -> Self {
        Self {
            inner: Arc::new(Inner {
                bot_id: parts.bot_id,
                host: parts.host,
                slot: parts.slot,
                stamps: parts.stamps,
                control: parts.control,
                stop: Mutex::new(Some(parts.stop)),
                driver_ended: parts.driver_ended,
                driver_wait: parts.app_exit_timeout.saturating_add(DRIVER_GRACE),
                signing: parts.signing,
                chat_signing_timeout: parts.chat_signing_timeout,
                warned_chat_unavailable: AtomicBool::new(false),
            }),
        }
    }

    /// Runs `work` with the session's `Client` on its host thread, if the bot
    /// is in a world.
    fn call<T>(
        &self,
        work: impl FnOnce(&Client) -> Result<T, SessionError> + Send + 'static,
    ) -> impl Future<Output = Result<T, SessionError>> + Send + 'static
    where
        T: Send + 'static,
    {
        let job = match self.inner.control.phase() {
            Phase::Starting => Err(SessionError::NotInWorld),
            Phase::Ended | Phase::Closed => Err(SessionError::Closed),
            Phase::Joined => {
                let slot = Arc::clone(&self.inner.slot);
                Ok(self.inner.host.run(move || async move {
                    // The `Client` is cloned only here, on the host thread.
                    match slot.get() {
                        Some(client) => work(&client),
                        // The driver has let go of it: the bot has left.
                        None => Err(SessionError::NotInWorld),
                    }
                }))
            }
        };
        async move { job?.await? }
    }

    /// Logs, once per session, that chat can't go out because it can't be
    /// signed. Later refusals only return the error.
    fn chat_unavailable(&self) {
        if !self
            .inner
            .warned_chat_unavailable
            .swap(true, Ordering::Relaxed)
        {
            let signing = self.inner.signing.borrow().signing;
            warn!(
                bot_id = %self.inner.bot_id,
                ?signing,
                "the server requires signed chat and the session can't sign it, so chat fails until it can"
            );
        }
    }

    /// Locks the stop signal, poison-tolerantly: it holds a plain value.
    fn lock_stop(&self) -> MutexGuard<'_, Option<oneshot::Sender<()>>> {
        self.inner
            .stop
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl SessionHandle for McSession {
    /// Skips an action with a non-finite angle and logs it, as a safety net
    /// behind mode validation (ADR-0010).
    fn perform(&self, action: GameAction) -> impl Future<Output = Result<(), SessionError>> + Send {
        let finite = actions::has_finite_angles(&action);
        if !finite {
            warn!(
                bot_id = %self.inner.bot_id,
                ?action,
                "skipped a game action with a non-finite angle; mode validation should have refused it"
            );
        }
        self.call(move |client| {
            if finite {
                actions::perform(client, action)
            } else {
                Ok(())
            }
        })
    }

    /// A chat message on a server that enforces secure chat waits for
    /// azalea's chat-signing session, at most until `chat_signing_timeout`
    /// after the bot entered the game, and fails with
    /// [`SessionError::ChatUnavailable`] if it can't be signed: azalea would
    /// send it unsigned, and the server would drop it (ADR-0011). Commands
    /// and offline accounts never wait.
    fn send_chat(
        &self,
        message: ChatMessage,
    ) -> impl Future<Output = Result<(), SessionError>> + Send {
        let session = self.clone();
        async move {
            // azalea sends text starting with `/` as a command, never signed
            // (ADR-0008 §7).
            let command = message.as_str().starts_with('/');
            let timeout = session.inner.chat_signing_timeout;
            if !command && session.inner.control.phase() == Phase::Joined {
                let mut signing = session.inner.signing.clone();
                let ready = signing::signed_chat_ready(&mut signing, timeout).await;
                if ready == Err(SessionError::ChatUnavailable) {
                    session.chat_unavailable();
                }
                ready?;
            }
            let signing = session.inner.signing.clone();
            // `ChatMessage` already keeps to azalea's limits, so nothing is
            // dropped or cut silently. The text is only queued for the next
            // tick, which can't fail.
            let sent = session
                .call(move |client| {
                    // Checked again on the host thread, right before the
                    // message is queued, so unsigned chat never slips out.
                    if !command && !sendable(&signing, timeout) {
                        return Err(SessionError::ChatUnavailable);
                    }
                    client.chat(message.as_str());
                    Ok(())
                })
                .await;
            if sent == Err(SessionError::ChatUnavailable) {
                session.chat_unavailable();
            }
            sent
        }
    }

    fn respawn(&self) -> impl Future<Output = Result<(), SessionError>> + Send {
        let control = self.inner.control.clone();
        self.call(move |client| {
            actions::respawn(client);
            // In the same job, so a death after the respawn is reported
            // again, and one before it isn't reported twice (P3.5).
            control.respawned();
            Ok(())
        })
    }

    /// Tears the session down in ADR-0008 §10's order. The bridge is closed
    /// first, so the end of the driver doesn't count as a crash; the driver
    /// then exits azalea, waits for its runner and drops the `Client`; last,
    /// the host thread is shut down, or abandoned if it hangs.
    fn disconnect(&self) -> impl Future<Output = ()> + Send {
        self.inner.control.close();
        if let Some(stop) = self.lock_stop().take() {
            // The driver may have ended already; then nobody listens.
            let _ = stop.send(());
        }
        let driver_ended = self.inner.driver_ended.clone();
        let driver_wait = self.inner.driver_wait;
        let shutdown = self.inner.host.shutdown();
        let bot_id = self.inner.bot_id;
        async move {
            if time::timeout(driver_wait, closed(driver_ended))
                .await
                .is_err()
            {
                debug!(%bot_id, "the session's driver didn't end in time; shutting its host thread down");
            }
            let outcome = shutdown.await;
            debug!(%bot_id, ?outcome, "session disconnected");
        }
    }

    fn liveness(&self) -> Liveness {
        self.inner.stamps.read()
    }
}

/// Whether a chat message can go out now, given the session's `signing`.
fn sendable(signing: &watch::Receiver<SigningState>, timeout: Duration) -> bool {
    let state = *signing.borrow();
    let now = tokio::time::Instant::now();
    let past_deadline = now >= signing::deadline(state, now, timeout);
    signing::decide(state.signing, false, past_deadline) == Decision::Send
}

/// Waits until `receiver`'s sender is gone. It never sends.
async fn closed(mut receiver: watch::Receiver<()>) {
    while receiver.changed().await.is_ok() {}
}
