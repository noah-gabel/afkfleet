//! Getting into the nearest minecart (ADR-0034).
//!
//! A stopgap for servers that put a bot back beside its minecart instead of
//! in it; it goes once scripted modes can do this themselves. So it's one
//! component and one system for minecarts alone, and nothing here is a
//! search for entities or a layer for vehicles.
//!
//! The action, [`enter_minecart`], only marks the bot as [`Entering`]; a bot
//! that rides already ([`Riding`], whatever it rides) isn't marked. The
//! click comes from [`enter_when_in_reach`], each game tick while the mark is
//! there:
//!
//! - **It waits for a minecart, [`PATIENCE`] ticks at most.** Right at a
//!   join, when a mode's steps at start run, a bot has no minecart within
//!   its reach yet, and a tick or two later it has (measured, ADR-0034).
//!   When none came within reach in that time, the mark goes and nothing
//!   happened.
//! - **It takes the nearest minecart within reach.** A minecart is within
//!   reach when the line from the bot's eyes to the middle of the minecart's
//!   box enters the box within the bot's entity interaction range: what a
//!   player aiming at its middle could click. Only `EntityKind::Minecart`
//!   counts, the one a player rides, and only one the server told this bot
//!   about. Nothing checks what's in between.
//! - **It turns to it, clicks it and turns back, on the wire alone.** The
//!   look at the minecart goes out as its own packet, then one click on the
//!   minecart by its ID. The bot's own look never changes, so azalea's next
//!   position packet, in the same tick, says the bot's look again: whatever
//!   the mode's steps made it meanwhile.
//!
//! The server answers with a teleport to the seat, which keeps the bot's
//! look, and a `SetPassengers`; from there on the bot rides as the
//! [module above](super) has it.

use azalea::PhysicsState;
use azalea::bot::direction_looking_at;
use azalea::core::aabb::Aabb;
use azalea::core::delta::LpVec3;
use azalea::core::position::Vec3;
use azalea::ecs::change_detection::Mut;
use azalea::ecs::component::Component;
use azalea::ecs::entity::Entity;
use azalea::ecs::query::{With, Without};
use azalea::ecs::system::{Commands, Query};
use azalea::ecs::world::World;
use azalea::entity::dimensions::{EntityDimensions, calculate_dimensions};
use azalea::entity::indexing::EntityIdIndex;
use azalea::entity::{
    Attributes, EntityKindComponent, HasClientLoaded, LocalEntity, LookDirection, Physics, Pose,
    Position,
};
use azalea::movement::LastSentLookDirection;
use azalea::packet::game::SendGamePacketEvent;
use azalea::protocol::common::movements::MoveFlags;
use azalea::protocol::packets::game::s_interact::InteractionHand;
use azalea::protocol::packets::game::{ServerboundInteract, ServerboundMovePlayerRot};
use azalea::registry::builtin::EntityKind;
// The `Component` derive names its crate as `bevy_ecs`, which fleet-mc only
// has through azalea.
use azalea::ecs as bevy_ecs;
use fleet_core::mc::SessionError;
use tracing::debug;

use super::Riding;

/// How many game ticks a bot keeps looking for a minecart within reach: ten
/// seconds. On the test server the click came 26 to 96 ms after an action
/// at the join (ADR-0034); ten seconds are a guess for a server that just
/// restarted, far away.
pub(crate) const PATIENCE: u32 = 200;

/// On a bot that wants into a minecart.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Entering {
    /// How many more game ticks it looks for one.
    ticks_left: u32,
}

/// Where eyes at `eye` click the box `aabb`, and how far that is, squared:
/// where the line to the box's middle enters it, if that's within `range`.
/// Eyes inside the box click it where they are.
#[must_use]
fn click_point(eye: Vec3, aabb: &Aabb, range: f64) -> Option<(Vec3, f64)> {
    if aabb.contains(eye) {
        return Some((eye, 0.0));
    }
    let at = aabb.clip(eye, aabb.get_center())?;
    let distance_squared = eye.distance_squared_to(at);
    (distance_squared <= range * range).then_some((at, distance_squared))
}

/// Marks `bot` as wanting into the nearest minecart: [`enter_when_in_reach`]
/// clicks one as soon as one is within reach. A bot that rides already isn't
/// marked. See the [module docs](self).
///
/// # Errors
/// [`SessionError::NotInWorld`] if the bot's entity is gone.
pub(crate) fn enter_minecart(world: &mut World, bot: Entity) -> Result<(), SessionError> {
    let mut own = world
        .get_entity_mut(bot)
        .map_err(|_| SessionError::NotInWorld)?;
    if own.contains::<Riding>() {
        debug!("the bot rides already");
        return Ok(());
    }
    own.insert(Entering {
        ticks_left: PATIENCE,
    });
    Ok(())
}

/// What [`enter_when_in_reach`] reads and writes of a bot that wants in.
type Wanting = (
    Entity,
    &'static mut Entering,
    &'static Position,
    &'static EntityDimensions,
    &'static Attributes,
    &'static EntityIdIndex,
    &'static LookDirection,
    &'static mut LastSentLookDirection,
    &'static Physics,
    &'static PhysicsState,
);

/// The entities a bot may get into: every entity but a bot itself.
type Others<'world, 'state> = Query<
    'world,
    'state,
    (Entity, &'static Position, &'static EntityKindComponent),
    Without<LocalEntity>,
>;

/// Gets each bot marked [`Entering`] into the nearest minecart within its
/// reach, once one is there; see the [module docs](self). A bot the server
/// hasn't heard is loaded yet waits: a server ignores its clicks.
pub(super) fn enter_when_in_reach(
    mut commands: Commands,
    mut bots: Query<Wanting, (With<LocalEntity>, With<HasClientLoaded>)>,
    riding: Query<(), With<Riding>>,
    others: Others,
) {
    let size = calculate_dimensions(EntityKind::Minecart, Pose::Standing);
    for (
        bot,
        mut entering,
        position,
        dimensions,
        attributes,
        index,
        look,
        last_sent,
        physics,
        state,
    ) in &mut bots
    {
        if riding.contains(bot) {
            debug!("the bot rides by now");
            commands.entity(bot).remove::<Entering>();
            continue;
        }
        let eye = position.up(dimensions.eye_height.into());
        let range = attributes.entity_interaction_range.calculate();
        let nearest = others
            .iter()
            .filter(|(_, _, kind)| ***kind == EntityKind::Minecart)
            .filter_map(|(cart, position, _)| {
                let id = index.get_by_ecs_entity(cart)?;
                let (at, distance_squared) =
                    click_point(eye, &size.make_bounding_box(**position), range)?;
                Some((distance_squared, id, at - **position, at))
            })
            // Two equally near: the same one each time.
            .min_by(|one, other| one.0.total_cmp(&other.0).then(one.1.0.cmp(&other.1.0)));
        let Some((_, cart, offset, at)) = nearest else {
            entering.ticks_left = entering.ticks_left.saturating_sub(1);
            if entering.ticks_left == 0 {
                debug!("no minecart came within the bot's reach");
                commands.entity(bot).remove::<Entering>();
            }
            continue;
        };
        commands.entity(bot).remove::<Entering>();
        if let Some(toward) = turned_to(*look, eye, at) {
            say_look(&mut commands, bot, toward, last_sent, physics);
        }
        debug!(vehicle_id = cart.0, "the bot right-clicks a minecart");
        commands.trigger(SendGamePacketEvent::new(
            bot,
            ServerboundInteract {
                entity_id: cart,
                hand: InteractionHand::MainHand,
                // Where on the minecart, from its position, which a
                // minecart doesn't read.
                location: LpVec3::from(offset),
                // A server doesn't seat a sneaking player.
                using_secondary_action: state.trying_to_crouch,
            },
        ));
    }
}

/// `look` turned to face `point` from `eye`, in azalea's steps, as every
/// look of the bot's; `None` for eyes at the point itself, which have no
/// line to it.
#[must_use]
fn turned_to(look: LookDirection, eye: Vec3, point: Vec3) -> Option<LookDirection> {
    if eye == point {
        return None;
    }
    let mut toward = look;
    toward.update(direction_looking_at(eye, point));
    Some(toward)
}

/// Sends `toward` as the bot's look at once, unless it's the last look
/// sent, and notes it as that: azalea's position packet compares the bot's
/// look with it, and sends the bot's own again after this.
fn say_look(
    commands: &mut Commands,
    bot: Entity,
    toward: LookDirection,
    mut last_sent: Mut<LastSentLookDirection>,
    physics: &Physics,
) {
    if (last_sent.y_rot.to_bits(), last_sent.x_rot.to_bits())
        == (toward.y_rot().to_bits(), toward.x_rot().to_bits())
    {
        return;
    }
    last_sent.y_rot = toward.y_rot();
    last_sent.x_rot = toward.x_rot();
    commands.trigger(SendGamePacketEvent::new(
        bot,
        ServerboundMovePlayerRot {
            look_direction: toward,
            flags: MoveFlags {
                on_ground: physics.on_ground(),
                horizontal_collision: physics.horizontal_collision,
            },
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use azalea::app::App;
    use azalea::core::entity_id::MinecraftEntityId;
    use azalea::core::tick::GameTick;
    use azalea::ecs::observer::On;
    use azalea::entity::view_vector;
    use azalea::protocol::packets::game::ServerboundGamePacket;
    use rstest::rstest;

    use crate::riding::RidingPlugin;

    /// A minecart's box, 0.98 wide and 0.7 high, standing at `feet`.
    fn cart_box(feet: Vec3) -> Aabb {
        calculate_dimensions(EntityKind::Minecart, Pose::Standing).make_bounding_box(feet)
    }

    /// Within a millionth of a block: azalea keeps an entity's size as
    /// `f32`, so a box's faces are that far from 0.49 and 0.7.
    fn close(a: Vec3, b: Vec3) -> bool {
        a.distance_squared_to(b) < 1e-12
    }

    // --- Where the bot clicks a box ---

    #[test]
    fn the_click_is_where_the_line_to_the_middle_enters_the_box() {
        // Eyes level with the box's middle, 2 blocks north of it: the line
        // runs south and enters the north face.
        let eye = Vec3::new(0.0, 0.35, -2.0);

        let (at, distance_squared) = click_point(eye, &cart_box(Vec3::ZERO), 3.0).unwrap();

        assert!(close(at, Vec3::new(0.0, 0.35, -0.49)), "{at:?}");
        assert!((distance_squared - 1.51 * 1.51).abs() < 1e-6);
    }

    #[rstest]
    #[case::just_within(3.48, true)]
    #[case::just_beyond(3.50, false)]
    fn the_range_counts_to_where_the_line_enters_the_box(#[case] away: f64, #[case] reached: bool) {
        // The north face is 0.49 nearer than the middle: 2.99 and 3.01.
        let eye = Vec3::new(0.0, 0.35, -away);

        let clicked = click_point(eye, &cart_box(Vec3::ZERO), 3.0);

        assert_eq!(clicked.is_some(), reached, "{clicked:?}");
    }

    #[test]
    fn eyes_inside_the_box_click_it_where_they_are() {
        let eye = Vec3::new(0.1, 0.5, -0.1);

        assert_eq!(
            click_point(eye, &cart_box(Vec3::ZERO), 3.0),
            Some((eye, 0.0))
        );
    }

    // --- The action and the plugin's system on a bot's App ---

    const BOT: MinecraftEntityId = MinecraftEntityId(1);
    /// Where the bot stands.
    const FEET: Vec3 = Vec3::new(0.5, 64.0, 0.5);
    /// Where the bot looks: somewhere a mode's look step left it.
    const LOOK: (f32, f32) = (-45.0, 19.95);

    /// The eyes of the bot standing at [`FEET`].
    fn eye() -> Vec3 {
        FEET.up(1.62)
    }

    /// What the bot sent, in order.
    #[derive(Debug, Clone, PartialEq)]
    enum Did {
        /// This look, as a packet of its own.
        Looked(LookDirection),
        /// A right-click on the entity with this ID, with the main hand,
        /// this far from the entity's position, sneaking or not.
        Clicked(MinecraftEntityId, Vec3, bool),
        /// Any other packet.
        Other,
    }

    /// An App with the riding plugin and a loaded bot on foot that records
    /// what it sends.
    struct Scene {
        app: App,
        bot: Entity,
        did: Arc<Mutex<Vec<Did>>>,
        next_id: i32,
    }

    impl Scene {
        fn new() -> Self {
            let mut app = App::new();
            app.add_plugins(RidingPlugin);
            let dimensions = calculate_dimensions(EntityKind::Player, Pose::Standing);
            let bot = app
                .world_mut()
                .spawn((
                    LocalEntity,
                    HasClientLoaded,
                    BOT,
                    EntityIdIndex::default(),
                    Position::new(FEET),
                    Physics::new(&dimensions, FEET),
                    dimensions,
                    Attributes::new(EntityKind::Player),
                    LookDirection::new(LOOK.0, LOOK.1),
                    LastSentLookDirection {
                        y_rot: LOOK.0,
                        x_rot: LOOK.1,
                    },
                    PhysicsState::default(),
                ))
                .id();
            let did = Arc::new(Mutex::new(Vec::new()));
            let seen = Arc::clone(&did);
            app.add_observer(move |sent: On<SendGamePacketEvent>| {
                seen.lock().unwrap().push(match &sent.event().packet {
                    ServerboundGamePacket::MovePlayerRot(look) => Did::Looked(look.look_direction),
                    ServerboundGamePacket::Interact(click) => {
                        assert_eq!(click.hand, InteractionHand::MainHand);
                        Did::Clicked(
                            click.entity_id,
                            Vec3::from(click.location),
                            click.using_secondary_action,
                        )
                    }
                    _ => Did::Other,
                });
            });
            Self {
                app,
                bot,
                did,
                next_id: 2,
            }
        }

        /// An entity of `kind` at `feet`, which the server told the bot
        /// about. Returns the server's ID of it.
        fn spawn(&mut self, kind: EntityKind, feet: Vec3) -> MinecraftEntityId {
            let entity = self.unknown(kind, feet);
            let id = MinecraftEntityId(self.next_id);
            self.next_id += 1;
            self.app
                .world_mut()
                .get_mut::<EntityIdIndex>(self.bot)
                .unwrap()
                .insert(id, entity);
            id
        }

        /// An entity of `kind` at `feet` that isn't in the bot's index.
        fn unknown(&mut self, kind: EntityKind, feet: Vec3) -> Entity {
            self.app
                .world_mut()
                .spawn((Position::new(feet), EntityKindComponent(kind)))
                .id()
        }

        /// The action, as a mode's step performs it.
        fn enter(&mut self) -> Result<(), SessionError> {
            enter_minecart(self.app.world_mut(), self.bot)
        }

        fn tick(&mut self) {
            self.app.world_mut().run_schedule(GameTick);
        }

        fn did(&self) -> Vec<Did> {
            self.did.lock().unwrap().clone()
        }

        fn look(&self) -> LookDirection {
            *self.app.world().get::<LookDirection>(self.bot).unwrap()
        }

        fn last_sent_look(&self) -> (f32, f32) {
            let last_sent = self
                .app
                .world()
                .get::<LastSentLookDirection>(self.bot)
                .unwrap();
            (last_sent.y_rot, last_sent.x_rot)
        }

        fn wants_in(&self) -> bool {
            self.app.world().get::<Entering>(self.bot).is_some()
        }
    }

    /// Whether `look` points from `eye` at `point`, within azalea's steps of
    /// 0.15 degrees.
    fn faces(look: LookDirection, eye: Vec3, point: Vec3) -> bool {
        let wanted = (point - eye).normalize();
        let got = view_vector(look);
        wanted.distance_squared_to(got) < 0.003 * 0.003
    }

    /// A minecart on the block east of the bot.
    const BESIDE: Vec3 = Vec3::new(1.5, 64.0625, 0.5);

    #[test]
    fn the_action_alone_sends_nothing() {
        let mut scene = Scene::new();
        scene.spawn(EntityKind::Minecart, BESIDE);

        assert_eq!(scene.enter(), Ok(()));

        assert_eq!(scene.did(), []);
        assert!(scene.wants_in());
    }

    #[test]
    fn at_the_next_tick_the_bot_looks_at_the_minecart_beside_it_and_clicks_it_once() {
        let mut scene = Scene::new();
        let cart = scene.spawn(EntityKind::Minecart, BESIDE);
        scene.enter().unwrap();

        scene.tick();

        let middle = cart_box(BESIDE).get_center();
        let did = scene.did();
        let [Did::Looked(sent), Did::Clicked(clicked, offset, false)] = did.as_slice() else {
            panic!("not a look and then one click: {did:?}");
        };
        assert!(faces(*sent, eye(), middle), "{sent:?}");
        assert_eq!(*clicked, cart);
        // On the line to the middle, at the box's top: the eyes are above.
        // The packet keeps it to a few thousandths of a block.
        let at = BESIDE + *offset;
        assert!((offset.y - 0.7).abs() < 0.01, "{offset:?}");
        assert!(faces(*sent, eye(), at), "{at:?}");
        assert!(!scene.wants_in());
    }

    /// The bot's own look never changes, and azalea's position packet
    /// compares it with the last look sent: so the next one says the bot's
    /// look again.
    #[test]
    fn the_bot_keeps_its_own_look_and_azalea_has_it_to_send_again() {
        let mut scene = Scene::new();
        scene.spawn(EntityKind::Minecart, BESIDE);
        scene.enter().unwrap();

        scene.tick();

        let [Did::Looked(sent), Did::Clicked(..)] = scene.did()[..] else {
            panic!("not a look and then a click: {:?}", scene.did());
        };
        assert_eq!(scene.look(), LookDirection::new(LOOK.0, LOOK.1));
        assert_eq!(scene.last_sent_look(), (sent.y_rot(), sent.x_rot()));
        assert_ne!(scene.last_sent_look(), LOOK);
    }

    /// A look step right after the action, before the tick: the click still
    /// goes to the minecart, and the step's look is the bot's.
    #[test]
    fn a_look_between_the_action_and_the_click_stays_the_bots_look() {
        let mut scene = Scene::new();
        let cart = scene.spawn(EntityKind::Minecart, BESIDE);
        scene.enter().unwrap();
        let stepped = LookDirection::new(120.0, -30.0);
        *scene
            .app
            .world_mut()
            .get_mut::<LookDirection>(scene.bot)
            .unwrap() = stepped;

        scene.tick();

        assert!(
            matches!(scene.did().as_slice(), [Did::Looked(_), Did::Clicked(clicked, ..)] if *clicked == cart),
            "{:?}",
            scene.did()
        );
        assert_eq!(scene.look(), stepped);
    }

    #[test]
    fn a_bot_that_faces_the_minecart_already_only_clicks() {
        let mut scene = Scene::new();
        let cart = scene.spawn(EntityKind::Minecart, BESIDE);
        scene.enter().unwrap();
        scene.tick();
        let Did::Looked(at_the_cart) = scene.did()[0] else {
            panic!("no look: {:?}", scene.did());
        };
        *scene
            .app
            .world_mut()
            .get_mut::<LookDirection>(scene.bot)
            .unwrap() = at_the_cart;
        scene.did.lock().unwrap().clear();

        scene.enter().unwrap();
        scene.tick();

        assert!(
            matches!(scene.did().as_slice(), [Did::Clicked(clicked, ..)] if *clicked == cart),
            "{:?}",
            scene.did()
        );
    }

    #[test]
    fn a_sneaking_bots_click_says_so() {
        let mut scene = Scene::new();
        scene.spawn(EntityKind::Minecart, BESIDE);
        scene
            .app
            .world_mut()
            .get_mut::<PhysicsState>(scene.bot)
            .unwrap()
            .trying_to_crouch = true;
        scene.enter().unwrap();

        scene.tick();

        assert!(
            matches!(
                scene.did().as_slice(),
                [Did::Looked(_), Did::Clicked(_, _, true)]
            ),
            "{:?}",
            scene.did()
        );
    }

    #[test]
    fn the_nearest_of_two_minecarts_is_clicked() {
        let mut scene = Scene::new();
        let _farther = scene.spawn(EntityKind::Minecart, Vec3::new(0.5, 64.0625, 2.5));
        let nearer = scene.spawn(EntityKind::Minecart, BESIDE);
        let _farthest = scene.spawn(EntityKind::Minecart, Vec3::new(-1.5, 64.0625, -1.5));
        scene.enter().unwrap();

        scene.tick();

        assert!(
            matches!(scene.did().as_slice(), [Did::Looked(_), Did::Clicked(clicked, ..)] if *clicked == nearer),
            "{:?}",
            scene.did()
        );
    }

    /// Right at a join a bot has no minecart within its reach yet: it
    /// clicks at the first tick that has one.
    #[test]
    fn a_minecart_that_appears_later_is_clicked_then() {
        let mut scene = Scene::new();
        scene.enter().unwrap();
        for _ in 0..30 {
            scene.tick();
        }
        let before = scene.did();
        let cart = scene.spawn(EntityKind::Minecart, BESIDE);

        scene.tick();

        assert_eq!(before, []);
        assert!(
            matches!(scene.did().as_slice(), [Did::Looked(_), Did::Clicked(clicked, ..)] if *clicked == cart),
            "{:?}",
            scene.did()
        );
        assert!(!scene.wants_in());
    }

    #[test]
    fn the_bot_stops_looking_for_a_minecart_after_its_patience() {
        let mut scene = Scene::new();
        scene.enter().unwrap();
        for _ in 0..PATIENCE - 1 {
            scene.tick();
        }
        let still = scene.wants_in();

        scene.tick();
        let gave_up = !scene.wants_in();
        scene.spawn(EntityKind::Minecart, BESIDE);
        scene.tick();

        assert!(still);
        assert!(gave_up);
        assert_eq!(scene.did(), []);
    }

    #[test]
    fn the_action_again_starts_the_patience_again() {
        let mut scene = Scene::new();
        scene.enter().unwrap();
        for _ in 0..PATIENCE - 1 {
            scene.tick();
        }

        scene.enter().unwrap();
        for _ in 0..PATIENCE - 1 {
            scene.tick();
        }

        assert!(scene.wants_in());
    }

    #[test]
    fn it_clicks_once_and_never_again() {
        let mut scene = Scene::new();
        scene.spawn(EntityKind::Minecart, BESIDE);
        scene.enter().unwrap();

        for _ in 0..40 {
            scene.tick();
        }

        assert_eq!(scene.did().len(), 2, "{:?}", scene.did());
    }

    #[rstest]
    #[case::any_vehicle(MinecraftEntityId(2))]
    #[case::one_it_doesnt_know(MinecraftEntityId(77))]
    fn a_bot_that_rides_already_does_nothing(#[case] vehicle: MinecraftEntityId) {
        let mut scene = Scene::new();
        scene.spawn(EntityKind::Minecart, BESIDE);
        scene
            .app
            .world_mut()
            .entity_mut(scene.bot)
            .insert(Riding { vehicle });

        assert_eq!(scene.enter(), Ok(()));
        scene.tick();

        assert!(!scene.wants_in());
        assert_eq!(scene.did(), []);
    }

    /// Someone, or the server, put it in while it waited for a minecart.
    #[test]
    fn a_bot_that_rides_by_the_time_of_the_click_doesnt_click() {
        let mut scene = Scene::new();
        scene.enter().unwrap();
        scene.tick();
        scene.spawn(EntityKind::Minecart, BESIDE);
        scene.app.world_mut().entity_mut(scene.bot).insert(Riding {
            vehicle: MinecraftEntityId(2),
        });

        scene.tick();

        assert!(!scene.wants_in());
        assert_eq!(scene.did(), []);
    }

    #[test]
    fn with_no_minecart_in_reach_nothing_is_sent() {
        let mut scene = Scene::new();
        // Four blocks east: the box's near side is beyond 3 blocks.
        scene.spawn(EntityKind::Minecart, Vec3::new(5.0, 64.0625, 0.5));
        scene.enter().unwrap();

        for _ in 0..PATIENCE {
            scene.tick();
        }

        assert_eq!(scene.did(), []);
        assert_eq!(scene.last_sent_look(), LOOK);
        assert!(!scene.wants_in());
    }

    #[rstest]
    #[case::a_chest_minecart(EntityKind::ChestMinecart)]
    #[case::a_hopper_minecart(EntityKind::HopperMinecart)]
    #[case::a_boat(EntityKind::OakBoat)]
    #[case::a_pig(EntityKind::Pig)]
    fn only_a_minecart_a_player_rides_counts(#[case] kind: EntityKind) {
        let mut scene = Scene::new();
        scene.spawn(kind, BESIDE);
        scene.enter().unwrap();

        scene.tick();

        assert_eq!(scene.did(), []);
        assert!(scene.wants_in());
    }

    /// An entity the bot has no ID for can't be clicked: one the server
    /// already took away.
    #[test]
    fn a_minecart_the_server_didnt_tell_the_bot_about_doesnt_count() {
        let mut scene = Scene::new();
        scene.unknown(EntityKind::Minecart, BESIDE);
        scene.enter().unwrap();

        scene.tick();

        assert_eq!(scene.did(), []);
    }

    #[test]
    fn the_reach_is_the_bots_own_entity_interaction_range() {
        let mut scene = Scene::new();
        // The line to its middle enters this one's box about 3.6 away.
        let cart = scene.spawn(EntityKind::Minecart, Vec3::new(4.5, 64.0625, 0.5));
        scene.enter().unwrap();
        scene.tick();
        let out_of_reach = scene.did();
        scene
            .app
            .world_mut()
            .get_mut::<Attributes>(scene.bot)
            .unwrap()
            .entity_interaction_range
            .base = 4.0;

        scene.tick();

        assert_eq!(out_of_reach, []);
        assert!(
            matches!(scene.did().as_slice(), [Did::Looked(_), Did::Clicked(clicked, ..)] if *clicked == cart),
            "{:?}",
            scene.did()
        );
    }

    /// A server ignores what a client does before it said it has loaded,
    /// and azalea sends no position for such a bot either.
    #[test]
    fn a_bot_that_hasnt_loaded_yet_waits_without_losing_patience() {
        let mut scene = Scene::new();
        let cart = scene.spawn(EntityKind::Minecart, BESIDE);
        scene
            .app
            .world_mut()
            .entity_mut(scene.bot)
            .remove::<HasClientLoaded>();
        scene.enter().unwrap();
        for _ in 0..PATIENCE + 5 {
            scene.tick();
        }
        let before = scene.did();
        scene
            .app
            .world_mut()
            .entity_mut(scene.bot)
            .insert(HasClientLoaded);

        scene.tick();

        assert_eq!(before, []);
        assert!(
            matches!(scene.did().as_slice(), [Did::Looked(_), Did::Clicked(clicked, ..)] if *clicked == cart),
            "{:?}",
            scene.did()
        );
    }

    #[test]
    fn a_bot_whose_entity_is_gone_is_not_in_world() {
        let mut scene = Scene::new();
        scene.spawn(EntityKind::Minecart, BESIDE);
        scene.app.world_mut().despawn(scene.bot);

        assert_eq!(scene.enter(), Err(SessionError::NotInWorld));
        scene.tick();

        assert_eq!(scene.did(), []);
    }

    /// A bot between two worlds lacks some of these for a moment: the
    /// system waits for them, and never panics over one.
    #[rstest]
    #[case::position(|scene: &mut Scene| { scene.app.world_mut().entity_mut(scene.bot).remove::<Position>(); })]
    #[case::dimensions(|scene: &mut Scene| { scene.app.world_mut().entity_mut(scene.bot).remove::<EntityDimensions>(); })]
    #[case::attributes(|scene: &mut Scene| { scene.app.world_mut().entity_mut(scene.bot).remove::<Attributes>(); })]
    #[case::index(|scene: &mut Scene| { scene.app.world_mut().entity_mut(scene.bot).remove::<EntityIdIndex>(); })]
    #[case::look(|scene: &mut Scene| { scene.app.world_mut().entity_mut(scene.bot).remove::<LookDirection>(); })]
    #[case::last_sent_look(|scene: &mut Scene| { scene.app.world_mut().entity_mut(scene.bot).remove::<LastSentLookDirection>(); })]
    #[case::physics(|scene: &mut Scene| { scene.app.world_mut().entity_mut(scene.bot).remove::<Physics>(); })]
    #[case::physics_state(|scene: &mut Scene| { scene.app.world_mut().entity_mut(scene.bot).remove::<PhysicsState>(); })]
    fn a_bot_without_a_component_the_click_needs_sends_nothing(#[case] remove: fn(&mut Scene)) {
        let mut scene = Scene::new();
        scene.spawn(EntityKind::Minecart, BESIDE);
        remove(&mut scene);

        assert_eq!(scene.enter(), Ok(()));
        scene.tick();

        assert_eq!(scene.did(), []);
    }
}
