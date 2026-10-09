//! [`Fleet`]: the runtime's API, and [`Supervisor`], the task behind it
//! (Plan.md P4.7; ADR-0013).
//!
//! - The supervisor runs one [`BotActor`](crate::BotActor) per bot in a
//!   `JoinSet`, each with a child of its own token. It keeps what must
//!   outlive an actor: the bot's spec, its snapshot and breaker `watch`es,
//!   its chat bucket and its restart window.
//! - When an actor panics or one of its tasks crashes, the supervisor starts
//!   a new one from `fleet_core::bot::restore`, so a crash never skips the
//!   backoff, resets the breaker or refills the chat bucket. The 6th crash
//!   within 10 min ends in `Failed(CrashLoop)`.
//! - The [`Fleet`] handle talks to the supervisor over a bounded queue and
//!   waits for each answer only up to the reply timeout, so it always
//!   answers. The supervisor never waits for an actor: it forwards one
//!   `BotCommand` per call with `try_send`.
//! - Bots start and stop only through their spec's desired state.
//! - A shutdown cancels every actor, waits for them up to a timeout, and
//!   aborts the rest.
//! - [`Fleet::new`] describes the runtime's metrics and registers every
//!   series at 0 (P4.9).

mod command;
mod error;
mod supervisor;

use core::time::Duration;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::bot::{BotSnapshot, BotSpec, StickyState};
use fleet_core::chat::ChatMessage;
use fleet_core::id::BotId;
use fleet_core::mc::{MinecraftConnector, SessionCredentialProvider};
use fleet_core::resilience::{CircuitPolicy, RetryPolicy};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{Semaphore, broadcast, mpsc, oneshot};

pub use error::{CapacitySetting, FleetError, FleetSetupError, SendChatError, ShutdownReport};
pub use supervisor::Supervisor;

use self::command::{Command, Lifecycle, Reply};
use crate::chat::{ChatQuota, ChatTicket};
use crate::config::RuntimeConfig;
use crate::event::FleetEvent;
use crate::metrics;

/// Everything a fleet is built from (ADR-0013).
#[derive(Debug)]
pub struct FleetParts<C, P> {
    /// Starts every bot's sessions.
    pub connector: Arc<C>,
    /// Hands out every bot's session credentials.
    pub credentials: Arc<P>,
    /// How long bots back off, and when a session counts as stable.
    pub retry: RetryPolicy,
    /// When a bot's circuit breaker opens, and for how long.
    pub circuit: CircuitPolicy,
    /// The runtime's settings.
    pub config: RuntimeConfig,
    /// The wall time at which the runtime's clock starts. The runtime never
    /// reads the wall clock itself.
    pub anchor: DateTime<Utc>,
    /// The seed of all the runtime's randomness. The runtime never reads the
    /// OS's randomness itself.
    pub seed: u64,
}

/// The runtime's API: a cheap handle on the fleet's [`Supervisor`]
/// (Plan.md P4.7; ADR-0013). Clones talk to the same supervisor.
///
/// Every call answers within the reply timeout. A full supervisor queue is
/// [`FleetError::Busy`] at once, and a supervisor that has ended is
/// [`FleetError::ShuttingDown`]. Bots start and stop only through their
/// spec's desired state.
#[derive(Debug, Clone)]
pub struct Fleet {
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<FleetEvent>,
    reply_timeout: Duration,
}

impl Fleet {
    /// Builds a fleet: the handle, and the supervisor its caller runs
    /// (`supervisor.run(cancel)`) in a task it owns.
    ///
    /// # Errors
    /// - [`FleetSetupError::CapacityTooLarge`] if a channel capacity in
    ///   `config` is larger than tokio allows.
    /// - [`FleetSetupError::ChatQuota`] if the chat rate limit is invalid.
    pub fn new<C: MinecraftConnector, P: SessionCredentialProvider>(
        parts: FleetParts<C, P>,
    ) -> Result<(Self, Supervisor<C, P>), FleetSetupError> {
        // Checked before any channel is built: tokio panics on a capacity
        // that's too large.
        check_capacities(&parts.config)?;
        let quota = ChatQuota::try_new(parts.config.chat_interval, parts.config.chat_burst)?;
        // Into the recorder installed now; the agent installs its exporter
        // first (P5.3).
        metrics::register();
        let (commands, inbox) = mpsc::channel(parts.config.supervisor_queue.get());
        let (events, _) = broadcast::channel(parts.config.event_buffer.get());
        let fleet = Self {
            commands,
            events: events.clone(),
            reply_timeout: parts.config.reply_timeout,
        };
        let supervisor = Supervisor::new(parts, quota, inbox, events);
        Ok((fleet, supervisor))
    }

    /// Runs `spec`'s bot: a new bot starts as its desired state says, and a
    /// known one takes on the new spec. A new bot restored as `restore`
    /// starts there, so a bot that was paused for a human stays paused; a
    /// restore for a bot the fleet already runs is ignored. A spec equal to
    /// the bot's current one changes nothing.
    ///
    /// # Errors
    /// [`FleetError::AccountChanged`], [`FleetError::AccountInUse`],
    /// [`FleetError::AtCapacity`], [`FleetError::Busy`],
    /// [`FleetError::ShuttingDown`] or [`FleetError::TimedOut`].
    pub async fn apply(
        &self,
        spec: BotSpec,
        restore: Option<StickyState>,
    ) -> Result<(), FleetError> {
        self.call(self.reply_timeout, |reply| Command::Apply {
            spec: Box::new(spec),
            restore,
            reply,
        })
        .await
    }

    /// Removes a bot. It returns once the supervisor has accepted it; the bot
    /// stops in the background, and `Removed` follows. Removing a bot that's
    /// already being removed is fine.
    ///
    /// # Errors
    /// [`FleetError::UnknownBot`], [`FleetError::Busy`],
    /// [`FleetError::ShuttingDown`] or [`FleetError::TimedOut`].
    pub async fn remove(&self, id: BotId) -> Result<(), FleetError> {
        self.call(self.reply_timeout, |reply| Command::Remove { id, reply })
            .await
    }

    /// Tries a Failed bot again.
    ///
    /// # Errors
    /// [`FleetError::UnknownBot`], [`FleetError::Busy`],
    /// [`FleetError::ShuttingDown`] or [`FleetError::TimedOut`].
    pub async fn reset(&self, id: BotId) -> Result<(), FleetError> {
        self.lifecycle(id, Lifecycle::Reset).await
    }

    /// Connects a Paused bot again.
    ///
    /// # Errors
    /// [`FleetError::UnknownBot`], [`FleetError::Busy`],
    /// [`FleetError::ShuttingDown`] or [`FleetError::TimedOut`].
    pub async fn resume(&self, id: BotId) -> Result<(), FleetError> {
        self.lifecycle(id, Lifecycle::Resume).await
    }

    /// Stops a bot and starts it again: a new run that reconnects. Nothing
    /// happens while its desired state is Stopped.
    ///
    /// # Errors
    /// [`FleetError::UnknownBot`], [`FleetError::Busy`],
    /// [`FleetError::ShuttingDown`] or [`FleetError::TimedOut`].
    pub async fn restart(&self, id: BotId) -> Result<(), FleetError> {
        self.lifecycle(id, Lifecycle::Restart).await
    }

    /// Queues a user's chat message and returns its ticket. The outcome comes
    /// later as a `ChatSent` or `ChatFailed` event with that ticket.
    ///
    /// # Errors
    /// A [`SendChatError::Chat`] if the bot's queue refuses the message, or a
    /// [`SendChatError::Fleet`] with [`FleetError::UnknownBot`],
    /// [`FleetError::Busy`] (also when the bot's actor ended before it
    /// answered), [`FleetError::ShuttingDown`] or [`FleetError::TimedOut`].
    pub async fn send_chat(
        &self,
        id: BotId,
        message: ChatMessage,
    ) -> Result<ChatTicket, SendChatError> {
        let call = async {
            let (reply, answer) = oneshot::channel();
            self.send(Command::SendChat { id, message, reply })?;
            // A dropped reply means the supervisor has ended.
            let actor = answer.await.map_err(|_| FleetError::ShuttingDown)??;
            // The actor ended before it answered, as with a closed inbox.
            let ticket = actor.await.map_err(|_| FleetError::Busy)??;
            Ok(ticket)
        };
        tokio::time::timeout(self.reply_timeout, call)
            .await
            .unwrap_or(Err(FleetError::TimedOut.into()))
    }

    /// Returns a bot's snapshot. A bot that's being removed still has one
    /// until its `Removed`.
    ///
    /// # Errors
    /// [`FleetError::UnknownBot`], [`FleetError::Busy`],
    /// [`FleetError::ShuttingDown`] or [`FleetError::TimedOut`].
    pub async fn snapshot(&self, id: BotId) -> Result<BotSnapshot, FleetError> {
        self.call(self.reply_timeout, |reply| Command::Snapshot { id, reply })
            .await
    }

    /// Returns every bot's snapshot, sorted by bot ID. A bot that's being
    /// removed is in it until its `Removed`.
    ///
    /// # Errors
    /// [`FleetError::Busy`], [`FleetError::ShuttingDown`] or
    /// [`FleetError::TimedOut`].
    pub async fn snapshot_all(&self) -> Result<Vec<BotSnapshot>, FleetError> {
        self.call(self.reply_timeout, |reply| Command::SnapshotAll { reply })
            .await
    }

    /// Subscribes to the fleet's events. A subscriber that lags behind gets
    /// `Lagged` and resyncs from [`snapshot_all`](Self::snapshot_all)
    /// (Plan.md §6 row 12).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<FleetEvent> {
        self.events.subscribe()
    }

    /// Shuts the fleet down: every actor stops its bot, and those still
    /// running after `timeout` are aborted. It waits `timeout` plus the
    /// reply timeout at most. Every later call answers
    /// [`FleetError::ShuttingDown`].
    ///
    /// # Errors
    /// [`FleetError::ShuttingDown`] if the fleet is already shutting down,
    /// [`FleetError::Busy`] or [`FleetError::TimedOut`].
    pub async fn shutdown(&self, timeout: Duration) -> Result<ShutdownReport, FleetError> {
        let limit = timeout.saturating_add(self.reply_timeout);
        self.call(limit, |reply| Command::Shutdown { timeout, reply })
            .await
    }

    /// Sends a lifecycle call.
    async fn lifecycle(&self, id: BotId, action: Lifecycle) -> Result<(), FleetError> {
        self.call(self.reply_timeout, |reply| Command::Lifecycle {
            id,
            action,
            reply,
        })
        .await
    }

    /// Sends the call `command` builds and waits up to `limit` for the
    /// supervisor's answer.
    async fn call<T>(
        &self,
        limit: Duration,
        command: impl FnOnce(Reply<T>) -> Command,
    ) -> Result<T, FleetError> {
        let (reply, answer) = oneshot::channel();
        self.send(command(reply))?;
        match tokio::time::timeout(limit, answer).await {
            Ok(Ok(result)) => result,
            // A dropped reply means the supervisor has ended.
            Ok(Err(_)) => Err(FleetError::ShuttingDown),
            Err(_) => Err(FleetError::TimedOut),
        }
    }

    /// Hands `command` to the supervisor without waiting for room.
    fn send(&self, command: Command) -> Result<(), FleetError> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => FleetError::Busy,
                TrySendError::Closed(_) => FleetError::ShuttingDown,
            })
    }
}

/// The largest capacity tokio's `broadcast` accepts.
const BROADCAST_MAX: usize = usize::MAX >> 1;

/// Checks every channel capacity the fleet hands to tokio against tokio's
/// documented limits, so building a channel can't panic.
fn check_capacities(config: &RuntimeConfig) -> Result<(), FleetSetupError> {
    let checks = [
        (
            CapacitySetting::EventBuffer,
            config.event_buffer,
            BROADCAST_MAX,
        ),
        (
            CapacitySetting::SupervisorQueue,
            config.supervisor_queue,
            Semaphore::MAX_PERMITS,
        ),
        (
            CapacitySetting::ActorInbox,
            config.actor_inbox,
            Semaphore::MAX_PERMITS,
        ),
        (
            CapacitySetting::ChatQueue,
            config.chat_queue,
            Semaphore::MAX_PERMITS,
        ),
    ];
    match checks
        .into_iter()
        .find(|&(_, capacity, limit)| capacity.get() > limit)
    {
        Some((setting, ..)) => Err(FleetSetupError::CapacityTooLarge { setting }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroUsize;

    use fleet_testkit::mc::{FakeConnector, FakeCredentials};
    use rstest::rstest;
    use tokio::sync::oneshot;

    use super::*;
    use crate::chat::{ChatBucketError, ChatError};

    /// The largest `broadcast` capacity tokio accepts, as its docs say.
    const TOKIO_BROADCAST_MAX: usize = usize::MAX / 2;

    /// The largest `mpsc` capacity tokio accepts, as its docs say.
    const TOKIO_MPSC_MAX: usize = tokio::sync::Semaphore::MAX_PERMITS;

    fn bot() -> BotId {
        "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap()
    }

    fn parts(config: RuntimeConfig) -> FleetParts<FakeConnector, FakeCredentials> {
        FleetParts {
            connector: Arc::new(FakeConnector::new()),
            credentials: Arc::new(FakeCredentials::new()),
            retry: RetryPolicy::try_new(
                Duration::from_secs(5),
                Duration::from_mins(5),
                Duration::from_mins(5),
            )
            .unwrap(),
            circuit: CircuitPolicy::try_new(
                NonZeroUsize::new(8).unwrap(),
                Duration::from_mins(10),
                Duration::from_mins(15),
            )
            .unwrap(),
            config,
            anchor: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            seed: 1,
        }
    }

    /// A handle whose supervisor is the test: it reads the calls from the
    /// returned queue.
    fn handle() -> (Fleet, mpsc::Receiver<Command>) {
        let (commands, queue) = mpsc::channel(1);
        let fleet = Fleet {
            commands,
            events: broadcast::channel(1).0,
            reply_timeout: Duration::from_secs(5),
        };
        (fleet, queue)
    }

    fn with(setting: CapacitySetting, value: usize) -> RuntimeConfig {
        let value = NonZeroUsize::new(value).unwrap();
        let mut config = RuntimeConfig::default();
        match setting {
            CapacitySetting::EventBuffer => config.event_buffer = value,
            CapacitySetting::SupervisorQueue => config.supervisor_queue = value,
            CapacitySetting::ActorInbox => config.actor_inbox = value,
            CapacitySetting::ChatQueue => config.chat_queue = value,
        }
        config
    }

    #[rstest]
    #[case::event_buffer(CapacitySetting::EventBuffer, TOKIO_BROADCAST_MAX)]
    #[case::supervisor_queue(CapacitySetting::SupervisorQueue, TOKIO_MPSC_MAX)]
    #[case::actor_inbox(CapacitySetting::ActorInbox, TOKIO_MPSC_MAX)]
    #[case::chat_queue(CapacitySetting::ChatQueue, TOKIO_MPSC_MAX)]
    fn a_capacity_up_to_tokios_limit_passes_and_one_more_is_refused(
        #[case] setting: CapacitySetting,
        #[case] limit: usize,
    ) {
        assert_eq!(check_capacities(&with(setting, limit)), Ok(()));
        assert_eq!(
            check_capacities(&with(setting, limit + 1)),
            Err(FleetSetupError::CapacityTooLarge { setting })
        );
    }

    #[test]
    fn the_default_capacities_pass() {
        assert_eq!(check_capacities(&RuntimeConfig::default()), Ok(()));
    }

    #[test]
    fn a_fleet_with_a_capacity_tokio_refuses_is_never_built() {
        let config = with(CapacitySetting::EventBuffer, usize::MAX);

        let built = Fleet::new(parts(config));

        assert_eq!(
            built.err(),
            Some(FleetSetupError::CapacityTooLarge {
                setting: CapacitySetting::EventBuffer
            })
        );
    }

    #[test]
    fn a_fleet_with_a_zero_chat_interval_is_never_built() {
        let config = RuntimeConfig {
            chat_interval: Duration::ZERO,
            ..RuntimeConfig::default()
        };

        let built = Fleet::new(parts(config));

        assert_eq!(
            built.err(),
            Some(FleetSetupError::ChatQuota(ChatBucketError::ZeroInterval))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_dropped_supervisor_reply_answers_shutting_down() {
        let (fleet, mut queue) = handle();
        let call = tokio::spawn(async move { fleet.snapshot(bot()).await });

        drop(queue.recv().await.unwrap());

        assert_eq!(call.await.unwrap(), Err(FleetError::ShuttingDown));
    }

    #[tokio::test(start_paused = true)]
    async fn a_dropped_actor_chat_reply_answers_busy() {
        let (fleet, mut queue) = handle();
        let call = tokio::spawn(async move { fleet.send_chat(bot(), "hi".parse().unwrap()).await });

        let Some(Command::SendChat { reply, .. }) = queue.recv().await else {
            panic!("expected a chat call");
        };
        let (actor_reply, answer) = oneshot::channel();
        reply.send(Ok(answer)).unwrap();
        drop(actor_reply);

        assert_eq!(
            call.await.unwrap(),
            Err(SendChatError::Fleet(FleetError::Busy))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_actors_chat_answer_is_the_callers() {
        let (fleet, mut queue) = handle();
        let call = tokio::spawn(async move { fleet.send_chat(bot(), "hi".parse().unwrap()).await });

        let Some(Command::SendChat { reply, .. }) = queue.recv().await else {
            panic!("expected a chat call");
        };
        let (actor_reply, answer) = oneshot::channel();
        reply.send(Ok(answer)).unwrap();
        actor_reply.send(Err(ChatError::QueueFull)).unwrap();

        assert_eq!(
            call.await.unwrap(),
            Err(SendChatError::Chat(ChatError::QueueFull))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_chat_answer_the_actor_never_gives_times_out() {
        let (fleet, mut queue) = handle();
        let call = tokio::spawn(async move { fleet.send_chat(bot(), "hi".parse().unwrap()).await });
        let Some(Command::SendChat { reply, .. }) = queue.recv().await else {
            panic!("expected a chat call");
        };
        let (_actor_reply, answer) = oneshot::channel();
        reply.send(Ok(answer)).unwrap();
        let started = tokio::time::Instant::now();

        assert_eq!(
            call.await.unwrap(),
            Err(SendChatError::Fleet(FleetError::TimedOut))
        );
        assert_eq!(started.elapsed(), Duration::from_secs(5));
    }

    #[tokio::test(start_paused = true)]
    async fn a_shutdown_waits_its_timeout_plus_the_reply_timeout() {
        let (fleet, mut queue) = handle();
        let started = tokio::time::Instant::now();
        let call = tokio::spawn(async move { fleet.shutdown(Duration::from_secs(10)).await });
        let _held = queue.recv().await.unwrap();

        assert_eq!(call.await.unwrap(), Err(FleetError::TimedOut));
        assert_eq!(started.elapsed(), Duration::from_secs(15));
    }
}
