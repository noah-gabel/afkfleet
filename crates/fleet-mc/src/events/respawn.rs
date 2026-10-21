//! Tells the session's bridge of the bot's deaths and respawns (ADR-0020,
//! S22, S27).
//!
//! azalea's Respawn packet handler removes `Dead` and `HasClientLoaded`
//! together, and `HasClientLoaded` comes back on the first tick the client
//! has loaded. `Dead` may come back in the Respawn's own frame: azalea's
//! `add_dead` puts it back when the old life's health 0 arrives with the
//! Respawn, and then only the next Respawn removes it. So a respawn is
//! confirmed from the removal and the next load, never from "no `Dead`
//! now": a load after a removal confirms a respawn the session asked for,
//! and a load without one, as after a dimension change of a living bot,
//! confirms nothing.
//!
//! Deaths are read here too, from azalea's `DeathEvent` in `Update`, inside
//! the frame: azalea's runner may run the next load's `GameTick` in the same
//! pass, before the session loop reads azalea's event channel, and the old
//! life's second death report must reach the bridge before that load, where
//! it's dropped as a duplicate.

use azalea::app::{App, Plugin, Update};
use azalea::ecs::lifecycle::{Add, Remove};
use azalea::ecs::message::MessageReader;
use azalea::ecs::observer::On;
use azalea::ecs::query::With;
use azalea::ecs::schedule::IntoScheduleConfigs as _;
use azalea::ecs::system::Query;
use azalea::entity::{Dead, HasClientLoaded, LocalEntity};
use azalea::packet::death_event_on_0_health;
use azalea::packet::game::DeathEvent;

use super::BridgeControl;

/// Tells the session's bridge of the bot's deaths, of each removal of its
/// `Dead` and of each load. One App runs one session, so its observers and
/// its system hold that session's bridge control, which doesn't count as an
/// event source.
pub(crate) struct RespawnWatchPlugin(pub(crate) BridgeControl);

impl Plugin for RespawnWatchPlugin {
    fn build(&self, app: &mut App) {
        let control = self.0.clone();
        app.add_observer(
            move |removed: On<Remove, Dead>, local: Query<(), With<LocalEntity>>| {
                if local.contains(removed.entity) {
                    control.dead_removed();
                }
            },
        );
        let control = self.0.clone();
        app.add_observer(
            move |loaded: On<Add, HasClientLoaded>, local: Query<(), With<LocalEntity>>| {
                if local.contains(loaded.entity) {
                    control.loaded();
                }
            },
        );
        let control = self.0.clone();
        app.add_systems(
            Update,
            (move |mut deaths: MessageReader<DeathEvent>, local: Query<(), With<LocalEntity>>| {
                for death in deaths.read() {
                    if local.contains(death.entity) {
                        control.died();
                    }
                }
            })
            .after(death_event_on_0_health),
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

    /// A session whose bot joined, and an App with only the watch and
    /// azalea's death message. The sink is kept: dropping it would end the
    /// session as crashed.
    async fn joined() -> (EventSink, McEvents, App) {
        let (sink, mut events) = bridge(
            "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap(),
            NonZeroUsize::new(8).unwrap(),
            Arc::default(),
            Arc::new(LivenessStamps::new(Instant::now())),
        );
        sink.forward(&Event::Spawn);
        assert_eq!(events.next().await, Some(SessionEvent::Joined));
        let mut app = App::new();
        app.add_message::<DeathEvent>();
        app.add_plugins(RespawnWatchPlugin(sink.control()));
        (sink, events, app)
    }

    /// As [`joined`], then the bot died, the death was read and a respawn
    /// asked for while it's still dead.
    async fn waiting() -> (EventSink, McEvents, App) {
        let (sink, mut events, app) = joined().await;
        sink.control().died();
        assert_eq!(events.next().await, Some(SessionEvent::Died));
        sink.control().respawn_asked(true);
        (sink, events, app)
    }

    fn death(app: &mut App, entity: Entity) {
        app.world_mut().write_message(DeathEvent {
            entity,
            packet: None,
        });
        app.update();
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

    /// Only the bot's own `Dead` counts: another entity's removal, then
    /// the bot's load, confirms nothing (S27).
    #[tokio::test]
    async fn another_entitys_respawn_confirms_nothing() {
        let (_sink, mut events, mut app) = waiting().await;
        let bot = app.world_mut().spawn((LocalEntity, Dead)).id();
        let other = app.world_mut().spawn(Dead).id();

        app.world_mut().entity_mut(other).remove::<Dead>();
        load(&mut app, bot);

        assert_eq!(ready(&mut events).await, None);
    }

    /// The bot's death reaches the bridge inside the frame, from azalea's
    /// `DeathEvent`; another entity's reports nothing (S27).
    #[tokio::test]
    async fn the_bots_death_is_read_in_the_frame_and_another_entitys_is_not() {
        let (_sink, mut events, mut app) = joined().await;
        let bot = app.world_mut().spawn(LocalEntity).id();
        let other = app.world_mut().spawn_empty().id();

        death(&mut app, other);
        let after_the_other = ready(&mut events).await;
        death(&mut app, bot);

        assert_eq!(after_the_other, None);
        assert_eq!(ready(&mut events).await, Some(SessionEvent::Died));
    }
}
