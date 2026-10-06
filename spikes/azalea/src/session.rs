//! Starting one bot on a host thread, three ways (P1.2/P1.3):
//!
//! - [`Variant::Join`]: plain `Client::join`. Fixed plugin set, so
//!   auto-reconnect and auto-respawn are **on**.
//! - [`Variant::Builder`]: `ClientBuilder` with both plugins disabled. It never
//!   returns a `Client`; a handler hands it out on `Init`. `start()` runs a
//!   nested `LocalSet` inside our host task until `exit()`.
//! - [`Variant::Custom`]: what `Client::join` does internally, rebuilt from
//!   public (partly `#[doc(hidden)]`) pieces so we choose the plugins and keep
//!   the runner's `AppExit` receiver.
//!
//! In every variant, events end up in a **bounded** channel. `Tick` (20 Hz) is
//! never queued; it only updates `last_tick`, which is all a watchdog needs.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

// `#[derive(Component)]` expands to `bevy_ecs::…` paths; azalea re-exports the crate.
use azalea::ecs as bevy_ecs;
use azalea::{
    Client, ClientBuilder, DefaultPlugins, Event,
    account::Account,
    app::{App, PluginGroup},
    auto_reconnect::AutoReconnectPlugin,
    auto_respawn::AutoRespawnPlugin,
    bot::DefaultBotPlugins,
    ecs::component::Component,
    events::LocalPlayerEvents,
    join::{ConnectOpts, StartJoinServerEvent},
    protocol::address::ResolvableAddr,
    start_ecs_runner,
};
use tokio::{
    sync::{mpsc, oneshot},
    task,
};
use tracing::{debug, info, warn};

use crate::Res;

/// Capacity of the bounded per-bot event channel.
pub const EVENT_CAPACITY: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Join,
    Builder,
    Custom,
}

impl std::str::FromStr for Variant {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "join" => Ok(Self::Join),
            "builder" => Ok(Self::Builder),
            "custom" => Ok(Self::Custom),
            other => Err(format!(
                "unknown variant `{other}` (join | builder | custom)"
            )),
        }
    }
}

/// Extra setup for the App (Custom and Builder only), e.g. a panic plugin.
pub type AppSetup = Box<dyn FnOnce(&mut App) + Send>;

/// Everything the multi-threaded side gets back for one bot.
pub struct Session {
    /// Kept here because `client.username()` panics before login and after `exit()`.
    pub name: String,
    pub client: Client,
    pub events: mpsc::Receiver<Event>,
    pub ticks: Arc<TickStats>,
    /// Resolves with a description of how the ECS runner ended.
    pub runner_end: oneshot::Receiver<String>,
}

/// Tick bookkeeping shared between the bridge and the consumer.
#[derive(Debug)]
pub struct TickStats {
    epoch: Instant,
    last_tick_ms: AtomicU64,
    pub ticks: AtomicU64,
    pub dropped_events: AtomicU64,
    /// `Event::Packet`s seen. They are only forwarded when `forward_packets` is set.
    pub packets: AtomicU64,
    pub forward_packets: AtomicBool,
}

impl TickStats {
    fn new() -> Self {
        Self {
            epoch: Instant::now(),
            last_tick_ms: AtomicU64::new(0),
            ticks: AtomicU64::new(0),
            dropped_events: AtomicU64::new(0),
            packets: AtomicU64::new(0),
            forward_packets: AtomicBool::new(false),
        }
    }
    fn tick(&self) {
        let ms = u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_tick_ms.store(ms, Ordering::Relaxed);
        self.ticks.fetch_add(1, Ordering::Relaxed);
    }
    /// Milliseconds since the last tick, or `None` before the first one.
    pub fn since_last_tick_ms(&self) -> Option<u64> {
        let last = self.last_tick_ms.load(Ordering::Relaxed);
        let now = u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
        (self.ticks.load(Ordering::Relaxed) > 0).then(|| now.saturating_sub(last))
    }
    fn forward(&self, tx: &mpsc::Sender<Event>, event: Event) {
        if let Event::Tick = event {
            self.tick();
            return;
        }
        if let Event::Packet(_) = event {
            self.packets.fetch_add(1, Ordering::Relaxed);
            if !self.forward_packets.load(Ordering::Relaxed) {
                return;
            }
        }
        if tx.try_send(event).is_err() {
            self.dropped_events.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Connects one bot. Must run on a host thread (inside its `LocalSet`).
pub async fn connect(
    variant: Variant,
    account: Account,
    address: String,
    setup: Option<AppSetup>,
) -> Res<Session> {
    let name = account.username().to_owned();
    match variant {
        Variant::Join => {
            if setup.is_some() {
                warn!("Variant::Join can't add plugins; setup ignored");
            }
            let (client, rx) = Client::join(account, address.as_str()).await?;
            let (ticks, events) = pump(rx);
            let (end_tx, runner_end) = oneshot::channel();
            // Client::join drops the runner's AppExit receiver, so there's no
            // way to learn how the runner ended.
            let _ = end_tx.send("unknown: Client::join discards the AppExit receiver".to_owned());
            Ok(Session {
                name,
                client,
                events,
                ticks,
                runner_end,
            })
        }
        Variant::Custom => connect_custom(account, address, setup).await,
        Variant::Builder => connect_builder(account, address, setup).await,
    }
}

/// Forwards an unbounded azalea receiver into a bounded channel on the host
/// thread. The bounded sender is dropped when azalea's sender goes away.
fn pump(mut rx: mpsc::UnboundedReceiver<Event>) -> (Arc<TickStats>, mpsc::Receiver<Event>) {
    let ticks = Arc::new(TickStats::new());
    let (tx, events) = mpsc::channel(EVENT_CAPACITY);
    let pump_ticks = ticks.clone();
    task::spawn_local(async move {
        while let Some(event) = rx.recv().await {
            pump_ticks.forward(&tx, event);
        }
        debug!("azalea event sender dropped; pump ends");
    });
    (ticks, events)
}

fn bot_plugins() -> azalea::app::PluginGroupBuilder {
    DefaultBotPlugins
        .build()
        .disable::<AutoReconnectPlugin>()
        .disable::<AutoRespawnPlugin>()
}

/// `Client::join`, rebuilt so we choose the plugins and keep `appexit_rx`.
async fn connect_custom(
    account: Account,
    address: String,
    setup: Option<AppSetup>,
) -> Res<Session> {
    let name = account.username().to_owned();
    let address = address.as_str().resolve().await?;

    let mut app = App::new();
    app.add_plugins((DefaultPlugins, bot_plugins()));
    if let Some(setup) = setup {
        setup(&mut app);
    }
    let (ecs, start_running_systems, appexit_rx) = start_ecs_runner(app.main_mut());
    start_running_systems();

    let (callback_tx, mut callback_rx) = mpsc::unbounded_channel();
    ecs.write().write_message(StartJoinServerEvent {
        account,
        connect_opts: ConnectOpts {
            address,
            server_proxy: None,
            sessionserver_proxy: None,
        },
        start_join_callback_tx: Some(callback_tx),
    });
    let entity = callback_rx.recv().await.ok_or("join callback dropped")?;

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    ecs.write()
        .entity_mut(entity)
        .insert(LocalPlayerEvents(event_tx));
    let (ticks, events) = pump(event_rx);

    let end_name = name.clone();
    let (end_tx, runner_end) = oneshot::channel();
    task::spawn_local(async move {
        let end = match appexit_rx.await {
            Ok(exit) => format!("runner returned {exit:?}"),
            Err(_) => "runner task died without sending AppExit (panic?)".to_owned(),
        };
        info!(%end_name, %end, "custom runner ended");
        let _ = end_tx.send(end);
    });

    Ok(Session {
        name,
        client: Client::new(entity, ecs),
        events,
        ticks,
        runner_end,
    })
}

/// The handler state for [`Variant::Builder`]. `Default` is required by
/// azalea; the real value is passed with `set_state`.
#[derive(Clone, Component, Default)]
struct Bridge(Option<Arc<BridgeInner>>);

struct BridgeInner {
    client_tx: Mutex<Option<oneshot::Sender<Client>>>,
    events: mpsc::Sender<Event>,
    ticks: Arc<TickStats>,
}

/// azalea spawns one `spawn_local` task per event for this handler, so it
/// does everything synchronously and never awaits: otherwise a slow consumer
/// would pile up handler tasks without bound.
async fn bridge_handler(client: Client, event: Event, state: Bridge) {
    let Some(inner) = state.0 else { return };
    if let Event::Init = event {
        let sender = inner.client_tx.lock().ok().and_then(|mut slot| slot.take());
        if let Some(sender) = sender {
            let _ = sender.send(client);
        }
    }
    inner.ticks.forward(&inner.events, event);
}

async fn connect_builder(
    account: Account,
    address: String,
    setup: Option<AppSetup>,
) -> Res<Session> {
    let name = account.username().to_owned();
    let (client_tx, client_rx) = oneshot::channel();
    let (event_tx, events) = mpsc::channel(EVENT_CAPACITY);
    let ticks = Arc::new(TickStats::new());
    let bridge = Bridge(Some(Arc::new(BridgeInner {
        client_tx: Mutex::new(Some(client_tx)),
        events: event_tx,
        ticks: ticks.clone(),
    })));

    let mut builder = ClientBuilder::new_without_plugins()
        .add_plugins(DefaultPlugins)
        .add_plugins(bot_plugins())
        .reconnect_after(None)
        .set_handler(bridge_handler)
        .set_state(bridge);
    if let Some(setup) = setup {
        builder = builder.add_plugins(SetupPlugin(Mutex::new(Some(setup))));
    }

    // `start()` blocks until exit and creates its own (nested) LocalSet.
    let end_name = name.clone();
    let start = task::spawn_local(async move { builder.start(account, address.as_str()).await });
    let (end_tx, runner_end) = oneshot::channel();
    task::spawn_local(async move {
        let end = match start.await {
            Ok(exit) => format!("start() returned {exit:?}"),
            Err(e) if e.is_panic() => "start() panicked".to_owned(),
            Err(e) => format!("start() task failed: {e}"),
        };
        info!(%end_name, %end, "builder runner ended");
        let _ = end_tx.send(end);
    });

    let client = client_rx.await.map_err(|_| "builder ended before Init")?;
    Ok(Session {
        name,
        client,
        events,
        ticks,
        runner_end,
    })
}

/// Lets a one-shot `AppSetup` closure act as a Bevy plugin.
struct SetupPlugin(Mutex<Option<AppSetup>>);
impl azalea::app::Plugin for SetupPlugin {
    fn build(&self, app: &mut App) {
        if let Some(setup) = self.0.lock().ok().and_then(|mut s| s.take()) {
            setup(app);
        }
    }
}

/// A Swarm shard: several bots in **one** App/World with one runner (P1.6,
/// P1.7). Every bot gets its own `Bridge` state, so each still ends up with
/// its own `Session`. `runner_end` reports the shared runner for all of them.
pub async fn connect_swarm(
    accounts: Vec<Account>,
    address: String,
    setup: Option<AppSetup>,
    join_delay: Option<std::time::Duration>,
) -> Res<Vec<Session>> {
    use azalea::swarm::{DefaultSwarmPlugins, SwarmBuilder};

    let mut pending = Vec::new();
    let mut builder = SwarmBuilder::new_without_plugins()
        .add_plugins((DefaultPlugins, bot_plugins(), DefaultSwarmPlugins))
        .reconnect_after(None)
        .set_handler(bridge_handler);
    for account in accounts {
        let name = account.username().to_owned();
        let (client_tx, client_rx) = oneshot::channel();
        let (event_tx, events) = mpsc::channel(EVENT_CAPACITY);
        let ticks = Arc::new(TickStats::new());
        let bridge = Bridge(Some(Arc::new(BridgeInner {
            client_tx: Mutex::new(Some(client_tx)),
            events: event_tx,
            ticks: ticks.clone(),
        })));
        builder = builder.add_account_with_state(account, bridge);
        pending.push((name, client_rx, events, ticks));
    }
    if let Some(delay) = join_delay {
        builder = builder.join_delay(delay);
    }
    if let Some(setup) = setup {
        builder = builder.add_plugins(SetupPlugin(Mutex::new(Some(setup))));
    }

    let start = task::spawn_local(async move { builder.start(address.as_str()).await });
    let mut end_txs = Vec::new();
    let mut sessions_parts = Vec::new();
    for (name, client_rx, events, ticks) in pending {
        let (end_tx, runner_end) = oneshot::channel();
        end_txs.push(end_tx);
        sessions_parts.push((name, client_rx, events, ticks, runner_end));
    }
    task::spawn_local(async move {
        let end = match start.await {
            Ok(exit) => format!("swarm start() returned {exit:?}"),
            Err(e) if e.is_panic() => "swarm start() panicked".to_owned(),
            Err(e) => format!("swarm start() task failed: {e}"),
        };
        info!(%end, "swarm runner ended");
        for tx in end_txs {
            let _ = tx.send(end.clone());
        }
    });

    let mut sessions = Vec::new();
    for (name, client_rx, events, ticks, runner_end) in sessions_parts {
        let client = client_rx
            .await
            .map_err(|_| format!("{name}: swarm ended before Init"))?;
        sessions.push(Session {
            name,
            client,
            events,
            ticks,
            runner_end,
        });
    }
    Ok(sessions)
}

/// Switches every schedule of an App to Bevy's single-threaded executor, so
/// its systems run on the host thread that runs the App instead of the
/// process-wide `ComputeTaskPool` (P1.6/P1.7).
pub fn single_threaded(app: &mut App) {
    use azalea::ecs::schedule::{ExecutorKind, Schedules};
    let mut schedules = app.world_mut().resource_mut::<Schedules>();
    for (_, schedule) in schedules.iter_mut() {
        schedule.set_executor_kind(ExecutorKind::SingleThreaded);
    }
}

/// Runs two optional setups in order.
pub fn combine(a: Option<AppSetup>, b: Option<AppSetup>) -> Option<AppSetup> {
    match (a, b) {
        (Some(a), Some(b)) => Some(Box::new(move |app: &mut App| {
            a(app);
            b(app);
        })),
        (a, b) => a.or(b),
    }
}

/// `Some(single_threaded)` when `on`.
pub fn st_setup(on: bool) -> Option<AppSetup> {
    on.then(|| Box::new(single_threaded) as AppSetup)
}
