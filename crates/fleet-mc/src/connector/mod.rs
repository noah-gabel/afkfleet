//! [`AzaleaConnector`] and [`McSession`]: the `fleet_core::mc` ports with
//! azalea (Plan.md P3.4; ADR-0008 §1–5 and §10, ADR-0011).
//!
//! `connect()` starts a host thread for the session, queues the session's
//! driver on it and returns at once; how connecting goes arrives as events.
//!
//! - `app` builds the session's Bevy App: azalea's plugins without
//!   auto-reconnect and auto-respawn, the packet-liveness plugin, and the
//!   single-threaded executor (ADR-0008 §1, §3).
//! - `driver` runs on the host thread for the whole session: it resolves the
//!   address and starts azalea under the connect timeout, feeds the event
//!   bridge, and tears azalea down (ADR-0008 §1, §5, §10).
//! - `session` is [`McSession`], the actor's handle: its calls run on the host
//!   thread, and `disconnect()` closes the bridge, stops the driver and shuts
//!   the thread down.

mod app;
mod driver;
mod session;

use core::fmt;
use core::future::Future;
use std::sync::Arc;
use std::time::Instant;

#[cfg(feature = "fault-injection")]
use azalea::app::App;
#[cfg(feature = "fault-injection")]
use fleet_core::id::BotId;
use fleet_core::mc::{ConnectError, ConnectParams, MinecraftConnector};
use tokio::sync::{oneshot, watch};
use tracing::warn;

use self::driver::{ClientSlot, Driver, Worlds};
pub use self::session::McSession;
use self::session::Parts;
use crate::account::account;
use crate::events::{EventCounters, LivenessStamps, McEvents, bridge};
use crate::{McConfig, McHostPool};

/// A test's hook into every session's App (the `fault-injection` feature).
#[cfg(feature = "fault-injection")]
type AppHook = Arc<dyn Fn(BotId, &mut App) + Send + Sync>;

/// Starts Minecraft sessions with azalea, one host thread each (the
/// `MinecraftConnector` of fleet-mc).
///
/// Clones share the host pool, the diagnostics and the tracked Worlds.
#[derive(Clone)]
pub struct AzaleaConnector {
    config: McConfig,
    pool: McHostPool,
    counters: Arc<EventCounters>,
    worlds: Arc<Worlds>,
    #[cfg(feature = "fault-injection")]
    hook: Option<AppHook>,
}

impl fmt::Debug for AzaleaConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AzaleaConnector")
            .field("config", &self.config)
            .field("pool", &self.pool)
            .finish_non_exhaustive()
    }
}

impl AzaleaConnector {
    /// Creates a connector with its own host pool, both tuned by `config`.
    #[must_use]
    pub fn new(config: &McConfig) -> Self {
        Self {
            config: *config,
            pool: McHostPool::new(config),
            counters: Arc::default(),
            worlds: Arc::default(),
            #[cfg(feature = "fault-injection")]
            hook: None,
        }
    }

    /// The host pool the sessions run on, with its thread counts.
    #[must_use]
    pub const fn pool(&self) -> &McHostPool {
        &self.pool
    }

    /// Returns how many sessions' ECS Worlds are still alive. A torn-down
    /// session's World is freed, so after a full teardown this goes back to
    /// where it was (P3.8).
    #[must_use]
    pub fn live_worlds(&self) -> usize {
        self.worlds.live()
    }

    /// Returns how many chat messages the sessions dropped because their
    /// event queue was full. It only counts up.
    #[must_use]
    pub fn dropped_chat(&self) -> u64 {
        self.counters.dropped_chat()
    }

    /// Returns how many action-bar messages the sessions ignored. They're
    /// status displays, not chat. It only counts up.
    #[must_use]
    pub fn ignored_action_bar(&self) -> u64 {
        self.counters.ignored_action_bar()
    }

    /// Starts a session: its host thread, its driver and its handles.
    fn start(&self, params: ConnectParams) -> Result<(McSession, McEvents), ConnectError> {
        // The connect timeout runs from here (ADR-0011).
        let started = Instant::now();
        let ConnectParams {
            bot_id,
            server,
            credentials,
            connect_timeout,
        } = params;
        let host = self.pool.spawn(bot_id)?;
        let stamps = Arc::new(LivenessStamps::new(started));
        let (sink, events) = bridge(
            bot_id,
            self.config.event_capacity,
            Arc::clone(&self.counters),
            Arc::clone(&stamps),
        );
        let control = sink.control();
        let (account, reports) = account(credentials, bot_id, self.config.session_join_timeout);
        let slot = Arc::new(ClientSlot::default());
        let (stop_tx, stop_rx) = oneshot::channel();
        let (ended_tx, ended_rx) = watch::channel(());
        let driver = Driver {
            bot_id,
            server,
            started,
            connect_timeout,
            account,
            reports,
            sink,
            stamps: Arc::clone(&stamps),
            slot: Arc::clone(&slot),
            worlds: Arc::clone(&self.worlds),
            stop: stop_rx,
            ended: ended_tx,
            app_exit_timeout: self.config.app_exit_timeout,
            #[cfg(feature = "fault-injection")]
            hook: self.hook.clone(),
        };
        if let Err(error) = host.start(move || driver.run()) {
            // Dropping the only handle ends the new thread.
            warn!(%bot_id, %error, "the new host thread didn't take the session's driver");
            return Err(ConnectError::HostUnavailable);
        }
        let session = McSession::new(Parts {
            bot_id,
            host,
            slot,
            stamps,
            control,
            stop: stop_tx,
            driver_ended: ended_rx,
            app_exit_timeout: self.config.app_exit_timeout,
        });
        Ok((session, events))
    }
}

#[cfg(feature = "fault-injection")]
impl AzaleaConnector {
    /// Adds `hook` to every session's App, after azalea's plugins and before
    /// the single-threaded executor is set. The slow tests use it to inject a
    /// panic into one session, or to watch packets from a second one.
    ///
    /// Only the `fault-injection` feature has it, which no production crate
    /// enables, and no release build can (ADR-0011).
    #[must_use]
    pub fn with_app_hook(mut self, hook: impl Fn(BotId, &mut App) + Send + Sync + 'static) -> Self {
        self.hook = Some(Arc::new(hook));
        self
    }
}

impl MinecraftConnector for AzaleaConnector {
    type Session = McSession;
    type Events = McEvents;

    fn connect(
        &self,
        params: ConnectParams,
    ) -> impl Future<Output = Result<(Self::Session, Self::Events), ConnectError>> + Send {
        let started = self.start(params);
        async move { started }
    }
}
