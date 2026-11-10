//! A bot sitting in a minecart (ADR-0026).
//!
//! azalea 0.16.0 has no riding on the client side: it ignores the server's
//! `SetPassengers`, keeps running its own physics on a passenger, and picks
//! what the bot looks at from wherever those physics left it. Seated in a
//! minecart, the bot falls out of its seat in its own picture, its eyes end
//! up inside the minecart's box, and every right-click lands on the minecart
//! instead of what the bot faces.
//!
//! So this module adds what azalea lacks for a passenger:
//!
//! - It **tracks the vehicle**. [`Riding`] is on the bot from the
//!   `SetPassengers` that lists it until one that doesn't, the vehicle's
//!   removal, or a respawn.
//! - It **keeps the bot at its seat**, each game tick after azalea's physics
//!   and before azalea's position packet: at the seat, without velocity, and
//!   with nothing to report. The server ignores a passenger's position (it
//!   kept the bot seated whatever azalea reported), so none is sent; looks
//!   still go out, and so does the answer to a teleport.
//! - It **picks from the seat**: what the bot looks at is worked out from a
//!   seated player's eyes with vanilla's rule for the own vehicle (see
//!   [`pick`]), and replaces azalea's result. A use and an attack read it as
//!   they do on foot.
//!
//! The server ends a ride with a `SetPassengers` first and a teleport after
//! it, which azalea applies as any other, so the bot's position is the
//! server's again once it has left; nothing here touches a bot that has.
//!
//! Only a minecart has a seat here. A bot on foot, or one riding anything
//! else, is left to azalea alone.

mod pick;

use std::collections::HashMap;

use azalea::PhysicsState;
use azalea::app::{App, Plugin, Update};
use azalea::core::entity_id::MinecraftEntityId;
use azalea::core::position::Vec3;
use azalea::core::tick::GameTick;
use azalea::ecs::component::Component;
use azalea::ecs::entity::Entity;
use azalea::ecs::message::MessageReader;
use azalea::ecs::query::{With, Without};
use azalea::ecs::schedule::IntoScheduleConfigs as _;
use azalea::ecs::system::{Commands, Query, Res};
use azalea::entity::dimensions::calculate_dimensions;
use azalea::entity::indexing::EntityIdIndex;
use azalea::entity::{
    Attributes, EntityKindComponent, LastSentPosition, LocalEntity, LookDirection, Physics, Pose,
    Position,
};
use azalea::interact::pick::{
    HitResultComponent, MaybePickableEntityQuery, update_hit_result_component,
};
use azalea::mining::MiningSystems;
use azalea::movement::{send_position, send_sprinting_if_needed};
use azalea::packet::game::ReceiveGamePacketEvent;
use azalea::physics::PhysicsSystems;
use azalea::physics::collision::entity_collisions::AabbQuery;
use azalea::protocol::packets::game::ClientboundGamePacket;
use azalea::registry::builtin::EntityKind;
use azalea::world::{WorldName, Worlds};
// The `Component` derive names its crate as `bevy_ecs`, which fleet-mc only
// has through azalea.
use azalea::ecs as bevy_ecs;
use tracing::debug;

use self::pick::Seated;

/// On the bot while the server has it as a passenger.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Riding {
    /// The server's ID of the entity the bot sits in.
    pub(crate) vehicle: MinecraftEntityId,
}

/// What a packet changes about the bot's ride.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    /// The bot is a passenger of this vehicle now.
    Mounted(MinecraftEntityId),
    /// The bot's ride is over.
    Left,
}

/// What `packet` changes for the bot with the server's ID `bot`, which rides
/// `riding` so far.
///
/// A `SetPassengers` lists all of a vehicle's passengers, so one for the
/// bot's vehicle without the bot ends the ride; a server sends that before
/// the teleport of every dismount. A vehicle the server removes, or a
/// respawn into a new world, ends it too.
#[must_use]
fn change(
    packet: &ClientboundGamePacket,
    bot: MinecraftEntityId,
    riding: Option<MinecraftEntityId>,
) -> Option<Change> {
    match packet {
        ClientboundGamePacket::SetPassengers(list) if list.passengers.contains(&bot) => {
            Some(Change::Mounted(list.vehicle))
        }
        ClientboundGamePacket::SetPassengers(list) if riding == Some(list.vehicle) => {
            Some(Change::Left)
        }
        ClientboundGamePacket::RemoveEntities(removed)
            if riding.is_some_and(|vehicle| removed.entity_ids.contains(&vehicle)) =>
        {
            Some(Change::Left)
        }
        // A new world: whatever the bot sat in isn't in it, and a server
        // that keeps the bot seated lists it again.
        ClientboundGamePacket::Respawn(_) | ClientboundGamePacket::Login(_) if riding.is_some() => {
            Some(Change::Left)
        }
        _ => None,
    }
}

/// How far a seated player's feet are above the position of a vehicle of
/// `kind`; `None` for a vehicle without a seat here.
///
/// A 26.1 server has a player in a minecart 0.4125 below the minecart's own
/// position: measured on the test server, and compared with it by the slow
/// riding scenario at every run.
#[must_use]
fn seat_offset(kind: EntityKind) -> Option<f64> {
    /// A seated player's feet, from its minecart's position.
    const IN_A_MINECART: f64 = -0.4125;
    (kind == EntityKind::Minecart).then_some(IN_A_MINECART)
}

/// Where the feet of a bot riding `riding` are, if its vehicle is known to
/// the bot and has a seat here.
fn seat(riding: Riding, index: &EntityIdIndex, vehicles: &Vehicles) -> Option<(Entity, Vec3)> {
    let vehicle = index.get_by_minecraft_entity(riding.vehicle)?;
    let (position, kind) = vehicles.get(vehicle).ok()?;
    let offset = seat_offset(**kind)?;
    Some((vehicle, position.up(offset)))
}

/// Tracks the bot's vehicle, keeps the bot at its seat and picks from there;
/// see the [module docs](self).
pub(crate) struct RidingPlugin;

impl Plugin for RidingPlugin {
    fn build(&self, app: &mut App) {
        // azalea registers the message too; registering it again is a no-op,
        // and it lets the plugin work on its own. azalea reads the packets
        // in `PreUpdate`, so a frame's `Update` sees them all.
        //
        // Every system has its place among azalea's that touch the same
        // data: Bevy warns at each session's start about a pair whose order
        // nobody declared. The bot's target is azalea's, then the seat's,
        // then what azalea's mining reads. The seat is kept after azalea's
        // physics, its attack (which slows a sprinting bot) and its
        // sprinting packet, in azalea's own order, and before its position
        // packet.
        app.add_message::<ReceiveGamePacketEvent>()
            .add_systems(
                Update,
                (
                    track_the_vehicle,
                    pick_from_the_seat
                        .after(update_hit_result_component)
                        .before(MiningSystems),
                )
                    .chain(),
            )
            .add_systems(
                GameTick,
                keep_the_seat
                    .after(PhysicsSystems)
                    .after(send_sprinting_if_needed)
                    .before(send_position),
            );
    }
}

/// Reads the frame's packets and puts [`Riding`] on the bot or takes it off.
fn track_the_vehicle(
    mut commands: Commands,
    mut packets: MessageReader<ReceiveGamePacketEvent>,
    bots: Query<(&MinecraftEntityId, Option<&Riding>), With<LocalEntity>>,
) {
    // A frame may hold several packets, and commands only apply after it.
    let mut rides: HashMap<Entity, Option<MinecraftEntityId>> = HashMap::new();
    for received in packets.read() {
        let Ok((bot_id, riding)) = bots.get(received.entity) else {
            continue;
        };
        let ride = rides
            .entry(received.entity)
            .or_insert_with(|| riding.map(|riding| riding.vehicle));
        match change(&received.packet, *bot_id, *ride) {
            Some(Change::Mounted(vehicle)) => {
                debug!(vehicle_id = vehicle.0, "the bot rides a vehicle");
                *ride = Some(vehicle);
                commands.entity(received.entity).insert(Riding { vehicle });
            }
            Some(Change::Left) => {
                debug!("the bot's ride is over");
                *ride = None;
                commands.entity(received.entity).remove::<Riding>();
            }
            None => {}
        }
    }
}

/// The vehicles a bot may sit in: every entity but a bot itself, which
/// keeps their positions apart from the one [`keep_the_seat`] writes.
type Vehicles<'world, 'state> =
    Query<'world, 'state, (&'static Position, &'static EntityKindComponent), Without<LocalEntity>>;

/// What [`keep_the_seat`] reads and writes of a riding bot.
type Kept = (
    &'static Riding,
    &'static EntityIdIndex,
    &'static mut Position,
    &'static mut Physics,
    &'static mut LastSentPosition,
    &'static mut PhysicsState,
);

/// What [`pick_from_the_seat`] reads and writes of a riding bot.
type Looking = (
    Entity,
    &'static Riding,
    &'static EntityIdIndex,
    &'static LookDirection,
    &'static WorldName,
    &'static Attributes,
    &'static mut HitResultComponent,
);

/// Puts a riding bot back at its seat after azalea's physics moved it, and
/// leaves azalea's position packet nothing to send.
fn keep_the_seat(mut bots: Query<Kept, With<LocalEntity>>, vehicles: Vehicles) {
    for (riding, index, mut position, mut physics, mut last_sent, mut state) in &mut bots {
        let Some((_, seat)) = seat(*riding, index, &vehicles) else {
            continue;
        };
        if **position != seat {
            **position = seat;
        }
        physics.velocity = Vec3::ZERO;
        // azalea sends a position when it moved since the last one sent, or
        // after 20 ticks without one.
        **last_sent = seat;
        state.position_remainder = 0;
    }
}

/// Replaces what azalea thinks a riding bot looks at with the pick from its
/// seat.
fn pick_from_the_seat(
    mut bots: Query<Looking, With<LocalEntity>>,
    vehicles: Vehicles,
    // Optional: a system whose resource is missing panics, which would end
    // the session. azalea inserts it with its own plugins.
    worlds: Option<Res<Worlds>>,
    aabb_query: AabbQuery,
    pickable_query: MaybePickableEntityQuery,
) {
    let Some(worlds) = worlds else {
        return;
    };
    for (bot, riding, index, look, world_name, attributes, mut hit) in &mut bots {
        let Some((vehicle, seat)) = seat(*riding, index, &vehicles) else {
            continue;
        };
        let Some(world) = worlds.get(world_name) else {
            continue;
        };
        // A passenger sits upright, whatever azalea makes of a seat that
        // reaches into the floor.
        let dimensions = calculate_dimensions(EntityKind::Player, Pose::Standing);
        let seated = Seated {
            bot,
            vehicle,
            eye: seat.up(dimensions.eye_height.into()),
            look: *look,
            body: dimensions.make_bounding_box(seat),
            block_range: attributes.block_interaction_range.calculate(),
            entity_range: attributes.entity_interaction_range.calculate(),
        };
        let picked = pick::pick(&seated, &world.read(), &aabb_query, &pickable_query);
        if **hit != picked {
            **hit = picked;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use azalea::core::aabb::Aabb;
    use azalea::core::game_type::{GameMode, OptionalGameType};
    use azalea::core::hit_result::HitResult;
    use azalea::core::position::ChunkPos;
    use azalea::core::registry_holder::RegistryHolder;
    use azalea::entity::metadata::{AbstractLiving, AbstractMinecart};
    use azalea::physics::collision::entity_collisions::LastBoundingBox;
    use azalea::protocol::packets::common::CommonPlayerSpawnInfo;
    use azalea::protocol::packets::game::{
        ClientboundKeepAlive, ClientboundRemoveEntities, ClientboundRespawn,
        ClientboundSetPassengers,
    };
    use azalea::registry::DataRegistry as _;
    use azalea::registry::data::DimensionKind;
    use azalea::registry::identifier::Identifier;
    use rstest::rstest;

    const BOT: MinecraftEntityId = MinecraftEntityId(1);
    const CART: MinecraftEntityId = MinecraftEntityId(2);
    const OTHER: MinecraftEntityId = MinecraftEntityId(3);

    fn passengers(
        vehicle: MinecraftEntityId,
        passengers: &[MinecraftEntityId],
    ) -> ClientboundGamePacket {
        ClientboundGamePacket::SetPassengers(ClientboundSetPassengers {
            vehicle,
            passengers: passengers.to_vec(),
        })
    }

    fn removed(entities: &[MinecraftEntityId]) -> ClientboundGamePacket {
        ClientboundGamePacket::RemoveEntities(ClientboundRemoveEntities {
            entity_ids: entities.to_vec(),
        })
    }

    // --- What a packet changes ---

    #[rstest]
    #[case::on_foot(None)]
    #[case::already_in_it(Some(CART))]
    #[case::from_another_vehicle(Some(OTHER))]
    fn a_passenger_list_with_the_bot_mounts_it(#[case] riding: Option<MinecraftEntityId>) {
        let packet = passengers(CART, &[OTHER, BOT]);

        assert_eq!(change(&packet, BOT, riding), Some(Change::Mounted(CART)));
    }

    #[rstest]
    #[case::empty(&[])]
    #[case::only_others(&[OTHER])]
    fn the_vehicles_list_without_the_bot_ends_the_ride(#[case] listed: &[MinecraftEntityId]) {
        let packet = passengers(CART, listed);

        assert_eq!(change(&packet, BOT, Some(CART)), Some(Change::Left));
    }

    #[rstest]
    #[case::on_foot(None)]
    #[case::in_the_cart(Some(CART))]
    fn another_vehicles_list_without_the_bot_changes_nothing(
        #[case] riding: Option<MinecraftEntityId>,
    ) {
        let packet = passengers(OTHER, &[]);

        assert_eq!(change(&packet, BOT, riding), None);
    }

    #[test]
    fn the_vehicles_removal_ends_the_ride() {
        let packet = removed(&[OTHER, CART]);

        assert_eq!(change(&packet, BOT, Some(CART)), Some(Change::Left));
    }

    /// A Respawn packet: a death's respawn, or a change of dimension.
    fn respawn() -> ClientboundGamePacket {
        ClientboundGamePacket::Respawn(ClientboundRespawn {
            common: CommonPlayerSpawnInfo {
                dimension_type: DimensionKind::new_raw(0),
                dimension: Identifier::new("minecraft:the_nether"),
                seed: 0,
                game_type: GameMode::Survival,
                previous_game_type: OptionalGameType(None),
                is_debug: false,
                is_flat: true,
                last_death_location: None,
                portal_cooldown: 0,
                sea_level: 63,
            },
            data_to_keep: 0,
        })
    }

    #[test]
    fn a_respawn_ends_the_ride() {
        assert_eq!(change(&respawn(), BOT, Some(CART)), Some(Change::Left));
    }

    #[rstest]
    #[case::of_another_entity(removed(&[OTHER]), Some(CART))]
    #[case::on_foot(removed(&[CART]), None)]
    #[case::a_respawn_on_foot(respawn(), None)]
    #[case::another_packet(ClientboundGamePacket::KeepAlive(ClientboundKeepAlive { id: 1 }), Some(CART))]
    fn other_packets_change_nothing(
        #[case] packet: ClientboundGamePacket,
        #[case] riding: Option<MinecraftEntityId>,
    ) {
        assert_eq!(change(&packet, BOT, riding), None);
    }

    // --- The seat ---

    #[test]
    fn a_minecarts_passenger_has_its_feet_below_the_minecarts_position() {
        let offset = seat_offset(EntityKind::Minecart).unwrap();

        assert!((offset - -0.4125).abs() < 1e-9, "{offset}");
    }

    #[rstest]
    #[case::a_boat(EntityKind::OakBoat)]
    #[case::a_horse(EntityKind::Horse)]
    #[case::a_chest_minecart(EntityKind::ChestMinecart)]
    fn other_vehicles_have_no_seat_here(#[case] kind: EntityKind) {
        assert_eq!(seat_offset(kind), None);
    }

    // --- The plugin on an App ---

    /// An App with only the plugin, a bot on foot far from its seat, and a
    /// vehicle of `kind` it knows.
    struct Scene {
        app: App,
        bot: Entity,
        vehicle: Entity,
    }

    const START: Vec3 = Vec3::new(8.0, 70.0, 8.0);
    const CART_AT: Vec3 = Vec3::new(0.5, 64.0625, 0.5);
    const SEAT: Vec3 = Vec3::new(0.5, 63.65, 0.5);

    impl Scene {
        fn new(kind: EntityKind) -> Self {
            let mut app = App::new();
            app.add_plugins(RidingPlugin);
            let vehicle = app
                .world_mut()
                .spawn((Position::new(CART_AT), EntityKindComponent(kind)))
                .id();
            let mut index = EntityIdIndex::default();
            index.insert(CART, vehicle);
            let mut last_sent = LastSentPosition::default();
            *last_sent = Vec3::new(1.0, 2.0, 3.0);
            let mut physics = Physics::default();
            physics.velocity = Vec3::new(0.0, -0.5, 0.25);
            let bot = app
                .world_mut()
                .spawn((
                    LocalEntity,
                    BOT,
                    index,
                    Position::new(START),
                    physics,
                    last_sent,
                    PhysicsState {
                        position_remainder: 19,
                        ..PhysicsState::default()
                    },
                ))
                .id();
            Self { app, bot, vehicle }
        }

        /// A frame in which the bot received `packets`, in order.
        fn receives(&mut self, packets: impl IntoIterator<Item = ClientboundGamePacket>) {
            for packet in packets {
                self.app.world_mut().write_message(ReceiveGamePacketEvent {
                    entity: self.bot,
                    packet: Arc::new(packet),
                });
            }
            self.app.update();
        }

        fn tick(&mut self) {
            self.app.world_mut().run_schedule(GameTick);
        }

        fn riding(&self) -> Option<Riding> {
            self.app.world().get::<Riding>(self.bot).copied()
        }

        fn position(&self) -> Vec3 {
            **self.app.world().get::<Position>(self.bot).unwrap()
        }

        fn velocity(&self) -> Vec3 {
            self.app.world().get::<Physics>(self.bot).unwrap().velocity
        }

        fn last_sent(&self) -> Vec3 {
            **self.app.world().get::<LastSentPosition>(self.bot).unwrap()
        }

        fn remainder(&self) -> u32 {
            self.app
                .world()
                .get::<PhysicsState>(self.bot)
                .unwrap()
                .position_remainder
        }
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        a.distance_squared_to(b) < 1e-18
    }

    #[test]
    fn the_servers_passenger_list_puts_the_bot_into_its_vehicle_and_takes_it_out() {
        let mut scene = Scene::new(EntityKind::Minecart);

        scene.receives([passengers(CART, &[BOT])]);
        let mounted = scene.riding();
        scene.receives([passengers(CART, &[])]);

        assert_eq!(mounted, Some(Riding { vehicle: CART }));
        assert_eq!(scene.riding(), None);
    }

    /// A dismount and a mount in one frame, either way round, end as the
    /// last packet says.
    #[test]
    fn several_changes_in_one_frame_apply_in_order() {
        let mut scene = Scene::new(EntityKind::Minecart);

        scene.receives([passengers(CART, &[BOT]), passengers(CART, &[])]);
        let left = scene.riding();
        scene.receives([
            passengers(CART, &[BOT]),
            passengers(CART, &[]),
            passengers(OTHER, &[BOT]),
        ]);

        assert_eq!(left, None);
        assert_eq!(scene.riding(), Some(Riding { vehicle: OTHER }));
    }

    #[test]
    fn another_entitys_packets_change_nothing() {
        let mut scene = Scene::new(EntityKind::Minecart);
        let other = scene.app.world_mut().spawn(OTHER).id();

        scene.app.world_mut().write_message(ReceiveGamePacketEvent {
            entity: other,
            packet: Arc::new(passengers(CART, &[BOT])),
        });
        scene.app.update();

        assert_eq!(scene.riding(), None);
    }

    #[test]
    fn a_riding_bot_is_kept_at_its_seat_without_velocity_and_with_nothing_to_report() {
        let mut scene = Scene::new(EntityKind::Minecart);
        scene.receives([passengers(CART, &[BOT])]);

        scene.tick();

        assert!(close(scene.position(), SEAT), "{:?}", scene.position());
        assert_eq!(scene.velocity(), Vec3::ZERO);
        assert_eq!(scene.last_sent(), scene.position());
        assert_eq!(scene.remainder(), 0);
    }

    #[test]
    fn the_seat_follows_the_vehicle() {
        let mut scene = Scene::new(EntityKind::Minecart);
        scene.receives([passengers(CART, &[BOT])]);
        scene.tick();
        let moved = Vec3::new(5.5, 64.0625, 0.5);
        *scene
            .app
            .world_mut()
            .get_mut::<Position>(scene.vehicle)
            .unwrap() = Position::new(moved);

        scene.tick();

        assert!(
            close(scene.position(), moved.up(-0.4125)),
            "{:?}",
            scene.position()
        );
    }

    /// The bot's own picture of a bot that isn't riding: where it is, how it
    /// moves, and what azalea's position packet goes by.
    fn untouched(scene: &Scene) -> bool {
        scene.position() == START
            && scene.velocity() == Vec3::new(0.0, -0.5, 0.25)
            && scene.last_sent() == Vec3::new(1.0, 2.0, 3.0)
            && scene.remainder() == 19
    }

    #[test]
    fn a_bot_on_foot_is_left_alone() {
        let mut scene = Scene::new(EntityKind::Minecart);

        scene.tick();

        assert!(untouched(&scene));
    }

    #[test]
    fn a_bot_that_left_its_vehicle_is_left_alone() {
        let mut scene = Scene::new(EntityKind::Minecart);
        scene.receives([passengers(CART, &[BOT]), passengers(CART, &[])]);

        scene.tick();

        assert!(untouched(&scene));
    }

    #[test]
    fn a_bot_in_a_vehicle_without_a_seat_here_is_left_alone() {
        let mut scene = Scene::new(EntityKind::OakBoat);
        scene.receives([passengers(CART, &[BOT])]);

        scene.tick();

        assert_eq!(scene.riding(), Some(Riding { vehicle: CART }));
        assert!(untouched(&scene));
    }

    #[test]
    fn a_bot_whose_vehicle_it_doesnt_know_is_left_alone() {
        let mut scene = Scene::new(EntityKind::Minecart);
        scene.receives([passengers(OTHER, &[BOT])]);

        scene.tick();

        assert!(untouched(&scene));
    }

    // --- What the bot looks at, with azalea's own pick before it ---

    /// A box of `width` and `height` standing at `feet`.
    fn standing(feet: Vec3, width: f64, height: f64) -> Aabb {
        Aabb {
            min: Vec3::new(feet.x - width / 2.0, feet.y, feet.z - width / 2.0),
            max: Vec3::new(feet.x + width / 2.0, feet.y + height, feet.z + width / 2.0),
        }
    }

    /// The scene after azalea's physics dropped a mounted bot onto the floor
    /// under its minecart: its own eyes are inside the minecart's box. A
    /// seated player looking steeply down south sees, through the
    /// minecart's top and front, a pig in front of it. Returns what the bot
    /// looks at after azalea's pick and the plugin's, with the minecart's
    /// and the pig's entities.
    fn looks_at_the_pig_behind_its_minecart(mounted: bool) -> (HitResult, Entity, Entity) {
        let mut scene = Scene::new(EntityKind::Minecart);
        scene.app.add_systems(Update, update_hit_result_component);
        let mut worlds = Worlds::new();
        let overworld = WorldName::new("minecraft:overworld");
        let world = worlds.get_or_insert(overworld.clone(), 384, -64, &RegistryHolder::default());
        scene.app.insert_resource(worlds);
        let pig = scene
            .app
            .world_mut()
            .spawn((
                AbstractLiving,
                LastBoundingBox(standing(Vec3::new(0.5, 63.5, 1.65), 0.9, 0.9)),
            ))
            .id();
        scene.app.world_mut().entity_mut(scene.vehicle).insert((
            AbstractMinecart,
            LastBoundingBox(standing(CART_AT, 0.98, 0.7)),
        ));
        world
            .write()
            .entities_by_chunk
            .entry(ChunkPos::new(0, 0))
            .or_default()
            .extend([pig, scene.vehicle]);
        let fallen = Vec3::new(0.5, 63.0, 0.5);
        let dimensions = calculate_dimensions(EntityKind::Player, Pose::Standing);
        scene.app.world_mut().entity_mut(scene.bot).insert((
            Position::new(fallen),
            Physics::new(&dimensions, fallen),
            dimensions,
            LookDirection::new(0.0, 52.0),
            overworld,
            Attributes::new(EntityKind::Player),
        ));
        if mounted {
            scene.receives([passengers(CART, &[BOT])]);
        }

        // azalea adds the bot's target in its first frame.
        scene.app.update();
        scene.app.update();

        let hit = (**scene
            .app
            .world()
            .get::<HitResultComponent>(scene.bot)
            .unwrap())
        .clone();
        (hit, scene.vehicle, pig)
    }

    /// What azalea 0.16.0 does alone, and what a bot on foot keeps getting:
    /// from inside the minecart's box, the minecart is all the bot can
    /// click.
    #[test]
    fn a_bot_that_isnt_riding_keeps_azaleas_target() {
        let (hit, vehicle, _) = looks_at_the_pig_behind_its_minecart(false);

        assert_eq!(
            hit.as_entity_hit_result().map(|hit| hit.entity),
            Some(vehicle)
        );
    }

    #[test]
    fn a_riding_bots_target_is_picked_from_its_seat_through_its_minecart() {
        let (hit, _, pig) = looks_at_the_pig_behind_its_minecart(true);

        assert_eq!(hit.as_entity_hit_result().map(|hit| hit.entity), Some(pig));
    }

    /// A system whose resource is missing panics, and a panic ends the
    /// session: without azalea's worlds, the pick does nothing.
    #[test]
    fn without_azaleas_worlds_the_pick_does_nothing() {
        let mut scene = Scene::new(EntityKind::Minecart);

        scene.receives([passengers(CART, &[BOT])]);
        scene.app.update();

        assert_eq!(scene.riding(), Some(Riding { vehicle: CART }));
    }
}
