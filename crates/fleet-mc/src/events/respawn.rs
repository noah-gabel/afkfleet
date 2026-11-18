//! Tells the session's bridge of the bot's deaths and respawns (ADR-0020,
//! S22, S27), and of the packets that put it into another world (ADR-0036,
//! D6).
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
//!
//! **A world change** is a Login after the join's own, or a Respawn that
//! answers no death. Both packets are read here, right after the frame's
//! deaths, so a Respawn that comes in its death's own frame finds that death
//! in the bridge and counts as its respawn. The bridge reports the change
//! once the bot is placed in the new world (`crate::script`).

use azalea::app::{App, Plugin, Update};
use azalea::ecs::lifecycle::{Add, Remove};
use azalea::ecs::message::MessageReader;
use azalea::ecs::observer::On;
use azalea::ecs::query::With;
use azalea::ecs::schedule::IntoScheduleConfigs as _;
use azalea::ecs::system::Query;
use azalea::entity::{Dead, HasClientLoaded, LocalEntity};
use azalea::packet::death_event_on_0_health;
use azalea::packet::game::{DeathEvent, ReceiveGamePacketEvent};
use azalea::protocol::packets::game::ClientboundGamePacket;

use super::{BridgeControl, WorldPacket};

/// Tells the session's bridge of the bot's deaths, of each removal of its
/// `Dead`, of each load, and of each Login and Respawn packet. One App runs one session, so its observers and
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
        let deaths = self.0.clone();
        let worlds = self.0.clone();
        // azalea registers the message too; registering it again is a no-op,
        // and it lets the plugin work on its own.
        app.add_message::<ReceiveGamePacketEvent>().add_systems(
            Update,
            (
                (move |mut died: MessageReader<DeathEvent>,
                       local: Query<(), With<LocalEntity>>| {
                    for death in died.read() {
                        if local.contains(death.entity) {
                            deaths.died();
                        }
                    }
                })
                .after(death_event_on_0_health),
                // After the deaths: a Respawn in its death's own frame is
                // that death's.
                move |mut packets: MessageReader<ReceiveGamePacketEvent>,
                      local: Query<(), With<LocalEntity>>| {
                    for received in packets.read() {
                        if !local.contains(received.entity) {
                            continue;
                        }
                        match &*received.packet {
                            ClientboundGamePacket::Login(_) => {
                                worlds.world_packet(WorldPacket::Login);
                            }
                            ClientboundGamePacket::Respawn(_) => {
                                worlds.world_packet(WorldPacket::Respawn);
                            }
                            _ => {}
                        }
                    }
                },
            )
                .chain(),
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

    // --- A world change (Plan.md P15.14; ADR-0036, D6) ---

    use azalea::core::entity_id::MinecraftEntityId;

    use crate::testing::{login, receive, respawn};

    const BOT: MinecraftEntityId = MinecraftEntityId(1);

    #[tokio::test]
    async fn a_second_login_is_a_world_change_once_the_bot_is_placed() {
        let (sink, mut events, mut app) = joined().await;
        let bot = app.world_mut().spawn(LocalEntity).id();

        receive(&mut app, bot, login(BOT));
        app.update();
        let before_it_is_placed = ready(&mut events).await;
        sink.control().placed();

        assert_eq!(before_it_is_placed, None);
        assert_eq!(ready(&mut events).await, Some(SessionEvent::WorldChanged));
    }

    #[tokio::test]
    async fn a_respawn_packet_without_a_death_is_a_world_change() {
        let (sink, mut events, mut app) = joined().await;
        let bot = app.world_mut().spawn(LocalEntity).id();

        receive(&mut app, bot, respawn());
        app.update();
        sink.control().placed();

        assert_eq!(ready(&mut events).await, Some(SessionEvent::WorldChanged));
    }

    /// The death and its Respawn in one frame: the death is read first, so
    /// the Respawn is the death's and no world change (ADR-0020, S27).
    #[tokio::test]
    async fn a_respawn_packet_in_its_deaths_own_frame_is_no_world_change() {
        let (sink, mut events, mut app) = joined().await;
        let bot = app.world_mut().spawn(LocalEntity).id();

        receive(&mut app, bot, respawn());
        death(&mut app, bot);
        sink.control().placed();

        assert_eq!(ready(&mut events).await, Some(SessionEvent::Died));
        assert_eq!(ready(&mut events).await, None);
    }

    #[tokio::test]
    async fn another_entitys_packets_change_no_world() {
        let (sink, mut events, mut app) = joined().await;
        let _bot = app.world_mut().spawn(LocalEntity).id();
        let other = app.world_mut().spawn_empty().id();

        receive(&mut app, other, login(BOT));
        receive(&mut app, other, respawn());
        app.update();
        sink.control().placed();

        assert_eq!(ready(&mut events).await, None);
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
