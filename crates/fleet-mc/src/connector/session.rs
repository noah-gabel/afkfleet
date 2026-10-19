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
use crate::command_tree::MessageCommands;
use crate::events::{BridgeControl, LivenessStamps, Phase};
use crate::host::HostThread;
use crate::signing::{self, Decision, Kind, SigningState};

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
    /// The server's commands with a message argument, published by its App.
    commands: watch::Receiver<MessageCommands>,
    /// How long after the game state chat may wait for signing.
    chat_signing_timeout: Duration,
    /// Whether unavailable chat was logged at `warn` already.
    warned_chat_unavailable: AtomicBool,
    /// Whether a refused command with a message was logged at `warn`
    /// already.
    warned_command_unavailable: AtomicBool,
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
    pub(super) commands: watch::Receiver<MessageCommands>,
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
                commands: parts.commands,
                chat_signing_timeout: parts.chat_signing_timeout,
                warned_chat_unavailable: AtomicBool::new(false),
                warned_command_unavailable: AtomicBool::new(false),
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

    /// Logs, once per session, that commands with a message argument can't
    /// go out on this server: it requires signed chat, and azalea signs no
    /// command (ADR-0020, S2). Never the command itself; later refusals only
    /// return the error.
    fn command_unavailable(&self) {
        if !self
            .inner
            .warned_command_unavailable
            .swap(true, Ordering::Relaxed)
        {
            warn!(
                bot_id = %self.inner.bot_id,
                "the server requires signed chat, and a command with a message can't be signed, so such commands fail on this server"
            );
        }
    }

    /// Logs the refusal of a message of `kind` once per session, in its own
    /// words.
    fn unavailable(&self, kind: Kind) {
        match kind {
            Kind::CommandWithMessage => self.command_unavailable(),
            Kind::Chat | Kind::Command => self.chat_unavailable(),
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
    /// send it unsigned, and the server would drop it (ADR-0011). A command
    /// with a message argument, by the server's command tree, fails the same
    /// way there at once, since azalea signs no command (ADR-0020, S2).
    /// Other commands and offline accounts never wait.
    fn send_chat(
        &self,
        message: ChatMessage,
    ) -> impl Future<Output = Result<(), SessionError>> + Send {
        let session = self.clone();
        async move {
            // azalea sends text starting with `/` as a command, never signed
            // (ADR-0008 §7).
            let kind = signing::kind(&message, &session.inner.commands.borrow());
            let timeout = session.inner.chat_signing_timeout;
            if kind == Kind::Chat && session.inner.control.phase() == Phase::Joined {
                let mut signing = session.inner.signing.clone();
                let ready = signing::signed_chat_ready(&mut signing, timeout).await;
                if ready == Err(SessionError::ChatUnavailable) {
                    session.chat_unavailable();
                }
                ready?;
            }
            let signing = session.inner.signing.clone();
            let commands = session.inner.commands.clone();
            // `ChatMessage` already keeps to azalea's limits, so nothing is
            // dropped or cut silently. The text is only queued for the next
            // tick, which can't fail.
            let sent = session
                .call(move |client| {
                    // Decided on the host thread, right before the message is
                    // queued, so neither unsigned chat nor a command whose
                    // message needs a signature ever slips out.
                    if !sendable(&signing, &commands, &message, timeout) {
                        return Err(SessionError::ChatUnavailable);
                    }
                    client.chat(message.as_str());
                    Ok(())
                })
                .await;
            if sent == Err(SessionError::ChatUnavailable) {
                session.unavailable(kind);
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

/// Whether `message` can go out now, given the session's `signing` and the
/// server's `commands` with a message argument: the host job's last check,
/// right before azalea queues it.
fn sendable(
    signing: &watch::Receiver<SigningState>,
    commands: &watch::Receiver<MessageCommands>,
    message: &ChatMessage,
    timeout: Duration,
) -> bool {
    let state = *signing.borrow();
    let kind = signing::kind(message, &commands.borrow());
    let now = tokio::time::Instant::now();
    let past_deadline = now >= signing::deadline(state, now, timeout);
    signing::decide(state.signing, kind, past_deadline) == Decision::Send
}

/// Waits until `receiver`'s sender is gone. It never sends.
async fn closed(mut receiver: watch::Receiver<()>) {
    while receiver.changed().await.is_ok() {}
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::signing::Signing;

    const TIMEOUT: Duration = Duration::from_secs(10);

    /// Plain chat goes out only once signing allows it; a command with a
    /// message, only where signing isn't needed (ADR-0020, S2); any other
    /// command always.
    #[rstest]
    #[case::chat_signed("hello", &["me"], Signing::Ready, true)]
    #[case::chat_unsigned_where_it_must_be("hello", &["me"], Signing::Failed, false)]
    #[case::an_emote_where_chat_is_signed("/me waves", &["me"], Signing::Ready, false)]
    #[case::an_emote_where_it_isnt_needed("/me waves", &["me"], Signing::NotNeeded, true)]
    #[case::a_command_without_a_message("/spawn", &["me"], Signing::Failed, true)]
    #[case::an_emote_before_the_tree("/me waves", &[], Signing::Ready, true)]
    #[tokio::test(start_paused = true)]
    async fn the_host_job_queues_only_what_the_server_takes(
        #[case] text: &str,
        #[case] names: &[&str],
        #[case] signing: Signing,
        #[case] expected: bool,
    ) {
        let (_state, state_rx) = watch::channel(SigningState {
            signing,
            in_game_since: Some(tokio::time::Instant::now()),
        });
        let (_commands, commands_rx) = watch::channel(MessageCommands::from_names(names));
        let message: ChatMessage = text.parse().unwrap();

        assert_eq!(
            sendable(&state_rx, &commands_rx, &message, TIMEOUT),
            expected
        );
    }
}
