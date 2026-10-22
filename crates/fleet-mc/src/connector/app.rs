//! A session's Bevy App, as ADR-0008 §1 and §3 decided: azalea's plugins
//! without auto-reconnect and auto-respawn, the packet-liveness and
//! chat-signing plugins, the death and respawn watch (ADR-0020, S22, S27),
//! the bot's event channel, and the single-threaded executor on every
//! schedule.

use std::sync::{Arc, Mutex, PoisonError};

use azalea::app::{App, PluginGroup as _};
use azalea::auto_reconnect::AutoReconnectPlugin;
use azalea::auto_respawn::AutoRespawnPlugin;
use azalea::bot::DefaultBotPlugins;
use azalea::ecs::lifecycle::Add;
use azalea::ecs::observer::On;
use azalea::ecs::schedule::{ExecutorKind, Schedules};
use azalea::ecs::system::Commands;
use azalea::entity::LocalEntity;
use azalea::events::LocalPlayerEvents;
use azalea::{DefaultPlugins, Event};
use tokio::sync::mpsc;
use tracing::warn;

use crate::events::{LivenessStamps, PacketLivenessPlugin, RespawnWatchPlugin};
use crate::signing::SigningPlugin;

/// The channel azalea's `LocalPlayerEvents` sends a session's events on. The
/// session loop drains it into the bounded bridge at once.
#[expect(
    clippy::disallowed_methods,
    reason = "azalea's LocalPlayerEvents takes an unbounded sender; the session loop drains it into the bounded bridge at once (ADR-0011, approved)"
)]
pub(super) fn event_channel() -> (mpsc::UnboundedSender<Event>, mpsc::UnboundedReceiver<Event>) {
    mpsc::unbounded_channel()
}

/// Builds the App of one session. `events` becomes the bot's
/// `LocalPlayerEvents` when azalea spawns it, `signing` publishes where its
/// chat signing is, and `respawns` confirms the respawns the session asks
/// for. `extra` adds to the App before the executor is set, so whatever it
/// adds runs single-threaded too (the fault-injection hook's systems, in the
/// slow tests).
pub(super) fn build_app(
    stamps: Arc<LivenessStamps>,
    events: mpsc::UnboundedSender<Event>,
    signing: SigningPlugin,
    respawns: RespawnWatchPlugin,
    extra: impl FnOnce(&mut App),
) -> App {
    let mut app = App::new();
    app.add_plugins((
        DefaultPlugins,
        // The bot's lifecycle is the core state machine's (ADR-0008 §1).
        DefaultBotPlugins
            .build()
            .disable::<AutoReconnectPlugin>()
            .disable::<AutoRespawnPlugin>(),
        PacketLivenessPlugin(stamps),
        signing,
        respawns,
    ));
    give_events_to_the_bot(&mut app, events);
    extra(&mut app);
    single_threaded(&mut app);
    app
}

/// Makes `events` the bot's `LocalPlayerEvents` the moment azalea spawns the
/// bot's entity, when its `LocalEntity` is added. azalea spawns it, polls the
/// connect and reports a failed one all in the same frame, so a channel added
/// any later can miss the session's first events: on a loaded Linux host, a
/// refused connect was lost that way (found in CI, group D).
///
/// The sender is moved into the bot's component, never cloned, so the channel
/// closes when the bot's entity goes away. An App runs one session, so a
/// second local entity gets no channel, and that's logged.
fn give_events_to_the_bot(app: &mut App, events: mpsc::UnboundedSender<Event>) {
    let events = Mutex::new(Some(events));
    app.add_observer(
        move |spawned: On<Add, LocalEntity>, mut commands: Commands| {
            let sender = events.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(sender) = sender {
                commands
                    .entity(spawned.entity)
                    .insert(LocalPlayerEvents(sender));
            } else {
                warn!("a second local entity in one session's App gets no event channel");
            }
        },
    );
}

/// Runs every schedule on the App's own thread (ADR-0008 §3): with Bevy's
/// multi-threaded executor, every App shares a process-wide pool, so one hung
/// session would freeze them all.
fn single_threaded(app: &mut App) {
    let mut schedules = app.world_mut().resource_mut::<Schedules>();
    for (_, schedule) in schedules.iter_mut() {
        schedule.set_executor_kind(ExecutorKind::SingleThreaded);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::time::Instant;

    use azalea::join::ConnectionFailedEvent;
    use azalea::protocol::connect::ConnectionError;
    use tokio::sync::mpsc::error::TryRecvError;

    use crate::signing::SigningState;

    fn stamps() -> Arc<LivenessStamps> {
        Arc::new(LivenessStamps::new(Instant::now()))
    }

    fn signing() -> SigningPlugin {
        SigningPlugin {
            online: false,
            state: Arc::new(tokio::sync::watch::Sender::new(SigningState::new(false))),
            commands: Arc::new(tokio::sync::watch::Sender::new(
                crate::command_tree::MessageCommands::default(),
            )),
        }
    }

    /// The respawn watch of a bridge nobody reads.
    fn respawns() -> RespawnWatchPlugin {
        let (sink, _events) = crate::events::bridge(
            "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap(),
            core::num::NonZeroUsize::MIN,
            Arc::default(),
            stamps(),
        );
        RespawnWatchPlugin(sink.control())
    }

    fn app() -> App {
        let (events, _) = event_channel();
        build_app(stamps(), events, signing(), respawns(), |_| {})
    }

    /// A session's App and the receiver of its bot's events.
    fn listening_app() -> (App, mpsc::UnboundedReceiver<Event>) {
        let (events, receiver) = event_channel();
        (
            build_app(stamps(), events, signing(), respawns(), |_| {}),
            receiver,
        )
    }

    /// Every schedule's executor kind.
    fn executors(app: &App) -> Vec<ExecutorKind> {
        app.world()
            .resource::<Schedules>()
            .iter()
            .map(|(_, schedule)| schedule.get_executor_kind())
            .collect()
    }

    #[test]
    fn every_schedule_runs_single_threaded() {
        let app = app();

        let executors = executors(&app);

        assert_ne!(executors.len(), 0);
        assert!(
            executors
                .iter()
                .all(|kind| *kind == ExecutorKind::SingleThreaded),
            "{executors:?}"
        );
    }

    #[test]
    fn what_extra_adds_runs_single_threaded_too() {
        let (events, _) = event_channel();
        let app = build_app(stamps(), events, signing(), respawns(), |app| {
            for (_, schedule) in app.world_mut().resource_mut::<Schedules>().iter_mut() {
                schedule.set_executor_kind(ExecutorKind::MultiThreaded);
            }
        });

        assert!(
            executors(&app)
                .iter()
                .all(|kind| *kind == ExecutorKind::SingleThreaded)
        );
    }

    #[test]
    fn auto_reconnect_and_auto_respawn_are_left_out() {
        let app = app();

        assert!(!app.is_plugin_added::<AutoReconnectPlugin>());
        assert!(!app.is_plugin_added::<AutoRespawnPlugin>());
    }

    #[test]
    fn azaleas_bot_plugins_and_the_liveness_plugin_are_added() {
        let app = app();

        assert!(app.is_plugin_added::<azalea::bot::BotPlugin>());
        assert!(app.is_plugin_added::<PacketLivenessPlugin>());
    }

    // --- The bot's event channel (found in CI on Linux, group D) ---

    #[test]
    fn the_bot_has_its_event_channel_as_soon_as_azalea_spawns_it() {
        let (mut app, _receiver) = listening_app();

        let bot = app.world_mut().spawn(LocalEntity).id();

        assert!(app.world().get::<LocalPlayerEvents>(bot).is_some());
    }

    #[test]
    fn a_second_local_entity_gets_no_event_channel() {
        let (mut app, _receiver) = listening_app();
        let bot = app.world_mut().spawn(LocalEntity).id();

        let second = app.world_mut().spawn(LocalEntity).id();

        assert!(app.world().get::<LocalPlayerEvents>(bot).is_some());
        assert!(app.world().get::<LocalPlayerEvents>(second).is_none());
    }

    #[test]
    fn the_channel_closes_with_the_bots_entity() {
        let (mut app, mut receiver) = listening_app();
        let bot = app.world_mut().spawn(LocalEntity).id();
        assert!(app.world().get::<LocalPlayerEvents>(bot).is_some());

        app.world_mut().despawn(bot);

        assert_eq!(receiver.try_recv().err(), Some(TryRecvError::Disconnected));
    }

    #[test]
    fn a_connect_failure_in_the_frame_that_spawned_the_bot_reaches_the_session() {
        let (mut app, mut receiver) = listening_app();
        let bot = app.world_mut().spawn(LocalEntity).id();
        app.world_mut().write_message(ConnectionFailedEvent {
            entity: bot,
            error: Arc::new(ConnectionError::Io(io::Error::from(
                io::ErrorKind::ConnectionRefused,
            ))),
        });

        app.update();

        let mut sent = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            sent.push(event);
        }
        assert!(
            sent.iter()
                .any(|event| matches!(event, Event::ConnectionFailed(_))),
            "{sent:?}"
        );
    }

    // --- A `Dead` re-inserted in the Respawn's frame (ADR-0020, S27) ---

    use core::time::Duration;

    use azalea::ecs::entity::Entity;
    use azalea::entity::metadata::Health;
    use azalea::entity::{Dead, HasClientLoaded};
    use fleet_core::mc::{SessionEvent, SessionEvents as _};

    use crate::actions;
    use crate::events::{EventSink, McEvents};

    /// The session's real App on a bridge the test reads, its bot joined,
    /// loaded and at full health.
    struct Scene {
        app: App,
        bot: Entity,
        sink: EventSink,
        events: McEvents,
        channel: mpsc::UnboundedReceiver<Event>,
    }

    impl Scene {
        async fn joined() -> Self {
            let (sink, mut events) = crate::events::bridge(
                "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap(),
                core::num::NonZeroUsize::new(8).unwrap(),
                Arc::default(),
                stamps(),
            );
            let (sender, channel) = event_channel();
            let mut app = build_app(
                stamps(),
                sender,
                signing(),
                RespawnWatchPlugin(sink.control()),
                |_| {},
            );
            let bot = app
                .world_mut()
                .spawn((LocalEntity, Health(20.0), HasClientLoaded))
                .id();
            sink.forward(&Event::Spawn);
            assert_eq!(events.next().await, Some(SessionEvent::Joined));
            Self {
                app,
                bot,
                sink,
                events,
                channel,
            }
        }

        /// Sets the bot's health, as a `SetHealth` packet does: a change
        /// even to the same value.
        fn health(&mut self, health: f32) {
            *self.app.world_mut().get_mut::<Health>(self.bot).unwrap() = Health(health);
        }

        /// What azalea's Respawn handler does to the bot's entity.
        fn respawn_packet(&mut self) {
            self.app
                .world_mut()
                .entity_mut(self.bot)
                .remove::<(Dead, HasClientLoaded)>();
        }

        /// The next load, as `player_loaded_packet` does it in `GameTick`.
        fn load(&mut self) {
            self.app
                .world_mut()
                .entity_mut(self.bot)
                .insert(HasClientLoaded);
        }

        /// Hands what azalea sent on the bot's channel to the bridge, as the
        /// session loop does.
        fn forward(&mut self) {
            while let Ok(event) = self.channel.try_recv() {
                self.sink.forward(&event);
            }
        }

        /// The respawn job's call (`connector::session`), with the bot's
        /// state as the job sees it.
        fn ask(&self) {
            self.sink
                .control()
                .respawn_asked(actions::loaded_in(self.app.world(), self.bot));
        }

        fn dead(&self) -> bool {
            self.app.world().get::<Dead>(self.bot).is_some()
        }

        /// The session events queued now.
        async fn queued(&mut self) -> Vec<SessionEvent> {
            let mut queued = Vec::new();
            while let Ok(Some(event)) =
                tokio::time::timeout(Duration::from_millis(50), self.events.next()).await
            {
                queued.push(event);
            }
            queued
        }

        /// The bot died, the actor read it and asked for the respawn; then
        /// the Respawn came in the same frame as the old life's health 0,
        /// so azalea's `add_dead` put `Dead` back after the handler removed
        /// it; then the next load. Returns what was queued in that frame
        /// and at the load, and after the frame's channel, handed over last.
        async fn respawned_with_a_stale_dead(&mut self) -> [Vec<SessionEvent>; 3] {
            self.health(0.0);
            self.app.update();
            self.forward();
            assert_eq!(self.queued().await, [SessionEvent::Died]);
            self.ask();

            self.health(0.0);
            self.respawn_packet();
            self.app.update();
            let in_the_frame = self.queued().await;
            assert!(self.dead(), "add_dead put Dead back");
            self.health(20.0);
            self.app.update();
            self.load();
            let at_the_load = self.queued().await;
            self.forward();
            let after = self.queued().await;
            [in_the_frame, at_the_load, after]
        }
    }

    /// A `Dead` re-inserted in the Respawn's frame and never removed again
    /// doesn't hide the respawn: it's confirmed once, at the next load, and
    /// the old life's second death report adds nothing, even when azalea's
    /// channel is read after the load (PR #65's review; ADR-0020, S27).
    #[tokio::test]
    async fn a_dead_put_back_in_the_respawns_frame_still_confirms_the_respawn_once() {
        let mut scene = Scene::joined().await;

        let [in_the_frame, at_the_load, after] = scene.respawned_with_a_stale_dead().await;

        assert_eq!(in_the_frame, []);
        assert_eq!(at_the_load, [SessionEvent::Respawned]);
        assert_eq!(after, [], "the old life's death report");
    }

    /// The next death, with the stale `Dead` still there, is reported once,
    /// and its respawn waits for its own Respawn and load (ADR-0020, S27).
    #[tokio::test]
    async fn the_next_death_after_a_stale_dead_is_reported_and_waits_for_its_own_respawn() {
        let mut scene = Scene::joined().await;
        scene.respawned_with_a_stale_dead().await;

        scene.health(0.0);
        scene.app.update();
        scene.forward();
        let died = scene.queued().await;
        scene.ask();
        let asked = scene.queued().await;
        scene.respawn_packet();
        scene.health(20.0);
        scene.app.update();
        scene.load();
        let loaded = scene.queued().await;

        assert_eq!(died, [SessionEvent::Died]);
        assert_eq!(asked, [], "loaded, but no Respawn since this death");
        assert_eq!(loaded, [SessionEvent::Respawned]);
    }
}
