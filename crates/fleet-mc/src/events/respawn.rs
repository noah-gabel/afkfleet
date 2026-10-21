//! Confirms a respawn the session asked for (ADR-0020, S22). azalea's
//! Respawn packet handler removes `Dead` and `HasClientLoaded` together, a
//! dimension change included, and `HasClientLoaded` comes back on the first
//! tick the client has loaded. An observer of that addition tells the
//! session's bridge when the bot's entity isn't dead, which confirms a
//! respawn the session asked for, if one is pending: a load without one, as
//! after a dimension change, confirms nothing.

use azalea::app::{App, Plugin};
use azalea::ecs::lifecycle::Add;
use azalea::ecs::observer::On;
use azalea::ecs::query::{With, Without};
use azalea::ecs::system::Query;
use azalea::entity::{Dead, HasClientLoaded, LocalEntity};

use super::BridgeControl;

/// Tells the session's bridge each time the bot's entity is loaded again
/// and isn't dead. One App runs one session, so its observer holds that
/// session's bridge control, which doesn't count as an event source.
pub(crate) struct RespawnWatchPlugin(pub(crate) BridgeControl);

impl Plugin for RespawnWatchPlugin {
    fn build(&self, app: &mut App) {
        let control = self.0.clone();
        app.add_observer(
            move |loaded: On<Add, HasClientLoaded>,
                  alive: Query<(), (With<LocalEntity>, Without<Dead>)>| {
                if alive.contains(loaded.entity) {
                    control.loaded_and_alive();
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::num::NonZeroUsize;
    use core::time::Duration;
    use std::sync::Arc;
    use std::time::Instant;

    use azalea::Event;
    use azalea::ecs::entity::Entity;
    use fleet_core::mc::{SessionEvent, SessionEvents as _};

    use crate::events::{EventSink, LivenessStamps, McEvents, bridge};

    /// A session whose bot died, the death read and a respawn asked for
    /// while it's still dead, and an App with only the watch. The sink is
    /// kept: dropping it would end the session as crashed.
    async fn waiting() -> (EventSink, McEvents, App) {
        let (sink, mut events) = bridge(
            "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap(),
            NonZeroUsize::new(8).unwrap(),
            Arc::default(),
            Arc::new(LivenessStamps::new(Instant::now())),
        );
        sink.forward(&Event::Spawn);
        sink.forward(&Event::Death(None));
        assert_eq!(events.next().await, Some(SessionEvent::Joined));
        assert_eq!(events.next().await, Some(SessionEvent::Died));
        sink.control().respawn_asked(false);
        let mut app = App::new();
        app.add_plugins(RespawnWatchPlugin(sink.control()));
        (sink, events, app)
    }

    /// The next event, if one is queued already.
    async fn ready(events: &mut McEvents) -> Option<SessionEvent> {
        tokio::time::timeout(Duration::from_millis(50), events.next())
            .await
            .ok()
            .flatten()
    }

    fn load(app: &mut App, entity: Entity) {
        app.world_mut().entity_mut(entity).insert(HasClientLoaded);
    }

    #[tokio::test]
    async fn a_load_while_dead_confirms_nothing_and_the_next_one_alive_does() {
        let (_sink, mut events, mut app) = waiting().await;
        let bot = app.world_mut().spawn((LocalEntity, Dead)).id();

        load(&mut app, bot);
        let while_dead = ready(&mut events).await;
        // What azalea's Respawn handler does, then the next load.
        app.world_mut()
            .entity_mut(bot)
            .remove::<(Dead, HasClientLoaded)>();
        load(&mut app, bot);

        assert_eq!(while_dead, None);
        assert_eq!(ready(&mut events).await, Some(SessionEvent::Respawned));
    }

    #[tokio::test]
    async fn another_entitys_load_confirms_nothing() {
        let (_sink, mut events, mut app) = waiting().await;
        let other = app.world_mut().spawn_empty().id();

        load(&mut app, other);

        assert_eq!(ready(&mut events).await, None);
    }
}
