//! A scripted mode's inputs on a bot's App: the keys, the leash, the two
//! guards of mining and the right button's cadence, each with the session's
//! real bridge behind it.

use core::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use azalea::app::{App, Update};
use azalea::core::direction::Direction as Face;
use azalea::core::entity_id::MinecraftEntityId;
use azalea::core::hit_result::{BlockHitResult, HitResult};
use azalea::core::registry_holder::RegistryHolder;
use azalea::core::tick::GameTick;
use azalea::ecs::component::Component;
use azalea::ecs::entity::Entity;
use azalea::ecs::observer::On;
use azalea::entity::dimensions::calculate_dimensions;
use azalea::entity::indexing::EntityIdIndex;
use azalea::entity::inventory::Inventory;
use azalea::entity::{
    Attributes, HasClientLoaded, Jumping, LocalEntity, LookDirection, Physics, Pose, Position,
};
use azalea::interact::StartUseItemQueued;
use azalea::interact::pick::{HitResultComponent, update_hit_result_component};
use azalea::inventory::{ItemStack, components};
use azalea::mining::{LeftClickMine, MineProgress, Mining, MiningQueued};
use azalea::packet::game::SendGamePacketEvent;
use azalea::protocol::packets::game::ServerboundGamePacket;
use azalea::protocol::packets::game::s_player_action::Action as PlayerAction;
use azalea::registry::builtin::{EntityKind, ItemKind};
use azalea::world::{WorldName, Worlds};
use azalea::{BlockPos, Event, PhysicsState, Vec3, WalkDirection};
use fleet_core::mc::{Guards, LetGo, SessionError, SessionEvent};
use fleet_core::mode::GameAction;
use fleet_core::script::{BlockGuard, Input, Key, LeashRadius, MouseButton, ToolGuard};
use rstest::rstest;

use super::held::{Held, RightHold};
use super::{Arriving, Guarded, Placement, ScriptPlugin, act, guard};
use crate::events::{EventSink, LivenessStamps, McEvents, bridge};
use crate::riding::Riding;
use crate::testing::{login, receive, respawn, teleport};

const BOT: MinecraftEntityId = MinecraftEntityId(1);
/// Where the bot stands: on the block below `y = 64`.
const FEET: Vec3 = Vec3::new(0.5, 64.0, 0.5);

/// What the bot sent that matters here, in order.
#[derive(Debug, Clone, PartialEq)]
enum Sent {
    /// A player action and its block.
    Action(PlayerAction, BlockPos),
    /// Any other packet.
    Other,
}

/// An App with the plugin, the session's real bridge with `Joined` read,
/// and a loaded bot that records what it sends.
struct Scene {
    app: App,
    bot: Entity,
    _sink: EventSink,
    events: McEvents,
    /// Keeps the bot's world alive: azalea's `Worlds` holds it only weakly.
    _world: Box<dyn core::any::Any>,
    sent: Arc<Mutex<Vec<Sent>>>,
}

fn key(key: Key) -> Input {
    Input::Key(key)
}

const LEFT: Input = Input::Mouse(MouseButton::Left);
const RIGHT: Input = Input::Mouse(MouseButton::Right);

impl Scene {
    /// A bot the server hasn't placed yet: no teleport came.
    fn new() -> Self {
        let (sink, events) = bridge(
            "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap(),
            NonZeroUsize::new(16).unwrap(),
            Arc::default(),
            Arc::new(LivenessStamps::new(Instant::now())),
        );
        sink.forward(&Event::Spawn);
        let mut app = App::new();
        // With the session's own watch of the packets that change a
        // world, as in a session's App.
        app.add_message::<azalea::packet::game::DeathEvent>();
        app.add_plugins((
            ScriptPlugin(sink.control()),
            crate::events::RespawnWatchPlugin(sink.control()),
            crate::riding::RidingPlugin,
        ));
        // azalea's own pick gives the bot its target, which a test then
        // sets: the component can't be made from outside azalea.
        app.add_systems(Update, update_hit_result_component);
        let mut worlds = Worlds::new();
        let overworld = WorldName::new("minecraft:overworld");
        let world = worlds.get_or_insert(overworld.clone(), 384, -64, &RegistryHolder::default());
        app.insert_resource(worlds);
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
                LookDirection::new(0.0, 0.0),
                overworld,
            ))
            .id();
        app.world_mut().entity_mut(bot).insert((
            PhysicsState::default(),
            Jumping(false),
            Inventory::default(),
            MineProgress::default(),
            azalea::interact::BlockStatePredictionHandler::default(),
        ));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&sent);
        app.add_observer(move |packet: On<SendGamePacketEvent>| {
            seen.lock().unwrap().push(match &packet.event().packet {
                ServerboundGamePacket::PlayerAction(action) => {
                    Sent::Action(action.action, action.pos)
                }
                _ => Sent::Other,
            });
        });
        app.update();
        let mut scene = Self {
            app,
            bot,
            _sink: sink,
            events,
            _world: Box::new(world),
            sent,
        };
        assert_eq!(scene.events(), [SessionEvent::Joined]);
        scene
    }

    /// A bot the server placed at [`FEET`]: its leash is anchored there.
    fn placed() -> Self {
        let mut scene = Self::new();
        scene.teleported(FEET);
        scene.tick();
        assert_eq!(scene.anchor(), Some(FEET));
        scene
    }

    fn act(&mut self, action: GameAction) -> Result<(), SessionError> {
        act(self.app.world_mut(), self.bot, action)
    }

    fn press(&mut self, input: Input) {
        assert_eq!(self.act(GameAction::Press(input)), Ok(()));
    }

    fn tap(&mut self, input: Input) {
        assert_eq!(self.act(GameAction::Tap(input)), Ok(()));
    }

    fn release(&mut self, input: Input) {
        assert_eq!(self.act(GameAction::Release(input)), Ok(()));
    }

    fn tick(&mut self) {
        self.app.world_mut().run_schedule(GameTick);
    }

    fn ticks(&mut self, count: usize) {
        for _ in 0..count {
            self.tick();
        }
    }

    /// The server sends `packet`, and the frame that reads it runs.
    fn receives(&mut self, packet: azalea::protocol::packets::game::ClientboundGamePacket) {
        receive(&mut self.app, self.bot, packet);
        self.app.update();
    }

    /// The server teleports the bot to `to`: azalea moves it, then the
    /// frame's systems read the packet.
    fn teleported(&mut self, to: Vec3) {
        self.move_to(to);
        self.receives(teleport(to));
    }

    fn move_to(&mut self, to: Vec3) {
        **self.app.world_mut().get_mut::<Position>(self.bot).unwrap() = to;
    }

    fn get<T: Component + Clone>(&self) -> Option<T> {
        self.app.world().get::<T>(self.bot).cloned()
    }

    fn has<T: Component>(&self) -> bool {
        self.app.world().get::<T>(self.bot).is_some()
    }

    fn held(&self) -> Held {
        self.get::<Held>().unwrap()
    }

    fn placement(&self) -> Placement {
        self.get::<Placement>().unwrap()
    }

    fn anchor(&self) -> Option<Vec3> {
        self.placement()
            .anchor
            .map(|at| Vec3::new(at.x(), at.y(), at.z()))
    }

    /// azalea's key state: where it walks, whether it tries to sprint and
    /// to crouch, and whether it jumps.
    fn keys(&self) -> (WalkDirection, bool, bool, bool) {
        let state = self.get::<PhysicsState>().unwrap();
        (
            state.move_direction,
            state.trying_to_sprint,
            state.trying_to_crouch,
            *self.get::<Jumping>().unwrap(),
        )
    }

    /// What the session reported since the last call.
    fn events(&mut self) -> Vec<SessionEvent> {
        self.events.queued()
    }

    fn sent(&self) -> Vec<Sent> {
        self.sent.lock().unwrap().clone()
    }

    fn actions_sent(&self) -> Vec<Sent> {
        self.sent()
            .into_iter()
            .filter(|sent| *sent != Sent::Other)
            .collect()
    }

    /// Puts what the bot looks at: `block`, or nothing within reach.
    fn looks_at(&mut self, block: Option<BlockPos>) {
        let hit = HitResult::Block(BlockHitResult {
            location: block.map_or(Vec3::ZERO, |block| block.center()),
            miss: block.is_none(),
            direction: Face::North,
            block_pos: block.unwrap_or_default(),
            inside: false,
            world_border: false,
        });
        **self
            .app
            .world_mut()
            .get_mut::<HitResultComponent>(self.bot)
            .unwrap() = hit;
    }

    /// Puts `stack` into the bot's hand.
    fn holds(&mut self, stack: ItemStack) {
        let mut inventory = self.app.world_mut().get_mut::<Inventory>(self.bot).unwrap();
        let slot = *inventory.inventory_menu.hotbar_slots_range().start()
            + usize::from(inventory.selected_hotbar_slot);
        *inventory.inventory_menu.slot_mut(slot).unwrap() = stack;
    }

    fn guards(&mut self, guards: Guards) {
        assert_eq!(guard(self.app.world_mut(), self.bot, guards), Ok(()));
    }
}

/// A pickaxe with `left` uses left of its 250.
fn pickaxe(left: i32) -> ItemStack {
    ItemStack::new(ItemKind::IronPickaxe, 1)
        .with_component(components::Damage { amount: 250 - left })
}

fn block(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos::new(x, y, z)
}

const STILL: (WalkDirection, bool, bool, bool) = (WalkDirection::None, false, false, false);

// --- The keys (Plan.md P15.10) ---

#[test]
fn a_new_sessions_bot_holds_nothing() {
    let mut scene = Scene::new();

    assert_eq!(scene.held(), Held::default());
    assert_eq!(scene.get::<Guarded>(), Some(Guarded(Guards::default())));
    scene.ticks(3);

    assert_eq!(scene.keys(), STILL);
    assert_eq!(scene.held(), Held::default());
    assert!(!scene.has::<LeftClickMine>() && !scene.has::<StartUseItemQueued>());
    assert_eq!(scene.sent(), []);
    assert_eq!(scene.events(), []);
}

#[rstest]
#[case::forward(Key::Forward, (WalkDirection::Forward, false, false, false))]
#[case::back(Key::Back, (WalkDirection::Backward, false, false, false))]
#[case::left(Key::Left, (WalkDirection::Left, false, false, false))]
#[case::right(Key::Right, (WalkDirection::Right, false, false, false))]
#[case::sprint(Key::Sprint, (WalkDirection::None, true, false, false))]
#[case::sneak(Key::Sneak, (WalkDirection::None, false, true, false))]
#[case::jump(Key::Jump, (WalkDirection::None, false, false, true))]
fn a_held_key_is_azaleas_key_state_from_the_next_tick_until_its_release(
    #[case] held: Key,
    #[case] state: (WalkDirection, bool, bool, bool),
) {
    let mut scene = Scene::placed();

    scene.press(key(held));
    let before_the_tick = scene.keys();
    scene.tick();
    let down = scene.keys();
    scene.ticks(5);
    let still_down = scene.keys();
    scene.release(key(held));
    scene.tick();

    assert_eq!(before_the_tick, STILL);
    assert_eq!(down, state);
    assert_eq!(still_down, state);
    assert_eq!(scene.keys(), STILL);
}

#[test]
fn keys_held_together_are_one_key_state() {
    let mut scene = Scene::placed();

    for held in [Key::Forward, Key::Left, Key::Sprint, Key::Jump, Key::Sneak] {
        scene.press(key(held));
    }
    scene.tick();

    assert_eq!(scene.keys(), (WalkDirection::ForwardLeft, true, true, true));
}

#[rstest]
#[case::forward(Key::Forward, (WalkDirection::Forward, false, false, false))]
#[case::sneak(Key::Sneak, (WalkDirection::None, false, true, false))]
#[case::sprint(Key::Sprint, (WalkDirection::None, true, false, false))]
#[case::jump(Key::Jump, (WalkDirection::None, false, false, true))]
fn a_tapped_key_is_down_for_exactly_one_game_tick(
    #[case] tapped: Key,
    #[case] state: (WalkDirection, bool, bool, bool),
) {
    let mut scene = Scene::placed();

    scene.tap(key(tapped));
    scene.tick();
    let during = scene.keys();
    scene.tick();
    let after = scene.keys();
    scene.ticks(3);

    assert_eq!(during, state);
    assert_eq!(after, STILL);
    assert_eq!(scene.keys(), STILL);
    assert!(!scene.held().is_anything_down());
}

#[test]
fn a_tap_of_a_held_key_leaves_it_down() {
    let mut scene = Scene::placed();
    scene.press(key(Key::Forward));
    scene.tick();

    scene.tap(key(Key::Forward));
    scene.ticks(3);

    assert_eq!(scene.keys().0, WalkDirection::Forward);
}

#[rstest]
#[case::drop(Key::Drop, PlayerAction::DropItem)]
#[case::drop_stack(Key::DropStack, PlayerAction::DropAllItems)]
#[case::swap(Key::SwapOffhand, PlayerAction::SwapItemWithOffhand)]
fn a_key_that_only_taps_sends_its_packet_once_pressed_or_tapped(
    #[case] tapped: Key,
    #[case] action: PlayerAction,
) {
    let mut scene = Scene::placed();

    scene.tap(key(tapped));
    scene.press(key(tapped));
    scene.ticks(3);

    // As the vanilla client sends them: the origin as the block.
    assert_eq!(
        scene.actions_sent(),
        [
            Sent::Action(action, BlockPos::default()),
            Sent::Action(action, BlockPos::default())
        ]
    );
    assert!(!scene.held().is_anything_down());
    assert_eq!(scene.keys(), STILL);
}

#[test]
fn releasing_the_keys_lets_go_of_every_key_and_leaves_the_buttons() {
    let mut scene = Scene::placed();
    for held in [Key::Forward, Key::Sprint, Key::Jump, Key::Sneak] {
        scene.press(key(held));
    }
    scene.press(LEFT);
    scene.press(RIGHT);
    scene.tick();

    assert_eq!(scene.act(GameAction::ReleaseKeys), Ok(()));
    scene.tick();

    assert_eq!(scene.keys(), STILL);
    assert!(scene.held().left && scene.held().right.is_some());
}

#[test]
fn releasing_all_lets_go_of_every_key_and_both_buttons_and_stops_mining() {
    let mut scene = Scene::placed();
    for held in [Key::Forward, Key::Sneak] {
        scene.press(key(held));
    }
    scene.looks_at(Some(block(0, 65, 2)));
    scene.press(LEFT);
    scene.press(RIGHT);
    scene.tick();
    scene.app.world_mut().entity_mut(scene.bot).insert(Mining {
        pos: block(0, 65, 2),
        dir: Face::North,
        force: true,
    });
    assert!(scene.has::<LeftClickMine>());

    assert_eq!(scene.act(GameAction::ReleaseAll), Ok(()));
    // A use still queued never reached the server: it's cancelled.
    assert!(!scene.has::<StartUseItemQueued>());
    scene.tick();

    assert_eq!(scene.keys(), STILL);
    assert!(!scene.held().is_anything_down());
    assert!(!scene.has::<LeftClickMine>() && !scene.has::<Mining>());
    let sent = scene.actions_sent();
    assert!(
        sent.contains(&Sent::Action(
            PlayerAction::ReleaseUseItem,
            BlockPos::default()
        )),
        "{sent:?}"
    );
    assert!(
        sent.contains(&Sent::Action(
            PlayerAction::AbortDestroyBlock,
            block(0, 65, 2)
        )),
        "{sent:?}"
    );
    // Doing it twice is as good as once.
    assert_eq!(scene.act(GameAction::ReleaseAll), Ok(()));
    scene.tick();
    assert_eq!(scene.keys(), STILL);
}

#[test]
fn releasing_all_also_ends_a_sneak_a_step_mode_left_behind() {
    // What azalea's `set_crouching` sets, as a step mode's `Sneak` does.
    let mut scene = Scene::placed();
    scene
        .app
        .world_mut()
        .get_mut::<PhysicsState>(scene.bot)
        .unwrap()
        .trying_to_crouch = true;

    assert_eq!(scene.act(GameAction::ReleaseAll), Ok(()));
    scene.tick();

    assert_eq!(scene.keys(), STILL);
}

#[test]
fn an_input_of_a_bot_that_is_gone_is_not_in_world() {
    let mut scene = Scene::placed();
    let bot = scene.bot;
    scene.app.world_mut().despawn(bot);

    for action in [
        GameAction::Press(key(Key::Forward)),
        GameAction::Tap(key(Key::Drop)),
        GameAction::Press(LEFT),
        GameAction::HoldUntilUsed,
        GameAction::ReleaseAll,
        GameAction::Mount,
        GameAction::Dismount,
    ] {
        assert_eq!(
            scene.act(action),
            Err(SessionError::NotInWorld),
            "{action:?}"
        );
    }
    assert_eq!(
        guard(scene.app.world_mut(), bot, Guards::default()),
        Err(SessionError::NotInWorld)
    );
}

// --- Where the bot was put, and its leash (Plan.md P15.12) ---

#[test]
fn the_anchor_is_where_the_server_put_the_bot_at_its_join() {
    let mut scene = Scene::new();
    let before_the_teleport = scene.placement();

    scene.teleported(Vec3::new(10.5, 70.0, -3.5));
    let before_the_tick = scene.placement().arriving;
    scene.tick();

    assert_eq!(
        before_the_teleport,
        Placement {
            anchor: None,
            last: None,
            arriving: Arriving::AwaitingTeleport
        }
    );
    assert_eq!(before_the_tick, Arriving::Teleported);
    assert_eq!(scene.anchor(), Some(Vec3::new(10.5, 70.0, -3.5)));
    assert_eq!(scene.placement().arriving, Arriving::Placed);
    // The join's own placement is no world change.
    assert_eq!(scene.events(), []);
}

#[test]
fn a_bot_whose_client_hasnt_loaded_isnt_placed_yet() {
    let mut scene = Scene::new();
    let bot = scene.bot;
    scene
        .app
        .world_mut()
        .entity_mut(bot)
        .remove::<HasClientLoaded>();

    scene.teleported(FEET);
    scene.ticks(3);
    let waiting = scene.placement();
    scene
        .app
        .world_mut()
        .entity_mut(bot)
        .insert(HasClientLoaded);
    scene.tick();

    assert_eq!(waiting.anchor, None);
    assert_eq!(waiting.arriving, Arriving::Teleported);
    assert_eq!(scene.anchor(), Some(FEET));
}

#[rstest]
#[case::a_second_login(login(BOT))]
#[case::a_respawn_without_a_death(respawn())]
fn a_world_change_lets_go_of_everything_and_sets_a_new_anchor(
    #[case] packet: azalea::protocol::packets::game::ClientboundGamePacket,
) {
    let mut scene = Scene::placed();
    for held in [Key::Forward, Key::Sprint, Key::Sneak] {
        scene.press(key(held));
    }
    scene.press(LEFT);
    scene.press(RIGHT);
    scene.tick();
    let bot = scene.bot;
    scene.app.world_mut().entity_mut(bot).insert((
        LeftClickMine,
        Mining {
            pos: block(0, 65, 2),
            dir: Face::North,
            force: true,
        },
    ));

    scene.receives(packet);
    let between_the_worlds = scene.placement();
    scene.tick();
    let there = Vec3::new(100.5, 80.0, 100.5);
    scene.teleported(there);
    let before_the_tick = scene.events();
    scene.tick();

    assert_eq!(between_the_worlds.anchor, None);
    assert_eq!(between_the_worlds.arriving, Arriving::AwaitingTeleport);
    assert!(!scene.held().is_anything_down());
    assert_eq!(scene.keys(), STILL);
    // Nothing is aborted on the wire: the block was in the other world.
    assert!(
        !scene.has::<LeftClickMine>()
            && !scene.has::<Mining>()
            && !scene.has::<StartUseItemQueued>()
    );
    assert_eq!(scene.anchor(), Some(there));
    // Reported once the bot is placed there, not before.
    assert_eq!(before_the_tick, []);
    assert_eq!(scene.events(), [SessionEvent::WorldChanged]);
}

#[test]
fn a_server_teleport_of_more_than_eight_blocks_moves_the_anchor_and_a_shorter_one_doesnt() {
    let mut scene = Scene::placed();

    scene.teleported(Vec3::new(8.4, 64.0, 0.5));
    scene.tick();
    let after_a_short_one = scene.anchor();
    // 8.2 blocks from where the bot stood just now.
    scene.teleported(Vec3::new(16.6, 64.0, 0.5));
    scene.tick();

    assert_eq!(after_a_short_one, Some(FEET));
    assert_eq!(scene.anchor(), Some(Vec3::new(16.6, 64.0, 0.5)));
    assert_eq!(scene.events(), [], "a teleport is no world change");
}

#[test]
fn within_its_radius_a_bot_walks_as_it_likes() {
    let mut scene = Scene::placed();
    scene.press(key(Key::Forward));

    for step in 1..=7 {
        scene.move_to(Vec3::new(0.5 + f64::from(step), 64.0, 0.5));
        scene.tick();
    }

    assert_eq!(scene.keys().0, WalkDirection::Forward);
    assert_eq!(scene.events(), []);
}

#[test]
fn beyond_its_radius_a_tick_further_lets_go_of_every_movement_key_before_the_physics() {
    let mut scene = Scene::placed();
    for held in [Key::Forward, Key::Sprint, Key::Jump, Key::Sneak] {
        scene.press(key(held));
    }
    scene.press(LEFT);
    scene.move_to(Vec3::new(8.4, 64.0, 0.5));
    scene.tick();
    let at_the_edge = scene.keys();

    // 8.6 blocks out: beyond the 8, and further than a tick ago.
    scene.move_to(Vec3::new(9.1, 64.0, 0.5));
    scene.tick();

    assert_eq!(at_the_edge, (WalkDirection::Forward, true, true, true));
    // Sneaking moves nothing, and stays; so does the mouse.
    assert_eq!(scene.keys(), (WalkDirection::None, false, true, false));
    assert!(scene.held().left);
    assert_eq!(scene.events(), [SessionEvent::LetGo(LetGo::Leash)]);
}

#[test]
fn beyond_its_radius_a_bot_may_walk_back_and_along_the_edge() {
    let mut scene = Scene::placed();
    scene.move_to(Vec3::new(10.5, 64.0, 0.5));
    scene.tick();
    scene.press(key(Key::Back));

    // Back towards the anchor, then sideways at the same distance.
    scene.move_to(Vec3::new(10.0, 64.0, 0.5));
    scene.tick();
    scene.move_to(Vec3::new(0.5, 64.0, 10.0));
    scene.tick();

    assert_eq!(scene.keys().0, WalkDirection::Backward);
    assert_eq!(scene.events(), []);
}

#[test]
fn a_bot_carried_further_with_no_key_down_reports_nothing() {
    // Water, a piston, a minecart: the leash stops walking, not being moved.
    let mut scene = Scene::placed();
    scene.press(key(Key::Sneak));

    for step in 9..14 {
        scene.move_to(Vec3::new(0.5 + f64::from(step), 64.0, 0.5));
        scene.tick();
    }

    assert_eq!(scene.keys(), (WalkDirection::None, false, true, false));
    assert_eq!(scene.events(), []);
}

#[test]
fn height_is_no_distance_for_the_leash() {
    let mut scene = Scene::placed();
    scene.press(key(Key::Forward));

    scene.move_to(Vec3::new(0.5, 200.0, 0.5));
    scene.tick();
    scene.move_to(Vec3::new(0.5, -60.0, 0.5));
    scene.tick();

    assert_eq!(scene.keys().0, WalkDirection::Forward);
    assert_eq!(scene.events(), []);
}

#[test]
fn the_leash_is_as_long_as_the_mode_set_it_and_keeps_its_anchor() {
    let mut scene = Scene::placed();
    scene.guards(Guards {
        leash: LeashRadius::try_from(2).unwrap(),
        ..Guards::default()
    });
    scene.press(key(Key::Forward));
    scene.move_to(Vec3::new(2.4, 64.0, 0.5));
    scene.tick();
    let within_two = scene.keys().0;

    scene.move_to(Vec3::new(2.7, 64.0, 0.5));
    scene.tick();

    assert_eq!(within_two, WalkDirection::Forward);
    assert_eq!(scene.keys().0, WalkDirection::None);
    assert_eq!(scene.anchor(), Some(FEET));
    assert_eq!(scene.events(), [SessionEvent::LetGo(LetGo::Leash)]);
}

#[test]
fn a_tapped_key_is_let_go_of_by_the_leash_too() {
    let mut scene = Scene::placed();
    scene.move_to(Vec3::new(9.0, 64.0, 0.5));
    scene.tick();

    scene.tap(key(Key::Forward));
    scene.move_to(Vec3::new(9.5, 64.0, 0.5));
    scene.tick();

    assert_eq!(scene.keys().0, WalkDirection::None);
    assert_eq!(scene.events(), [SessionEvent::LetGo(LetGo::Leash)]);
}

#[test]
fn a_riding_bot_is_on_its_leash_too() {
    let mut scene = Scene::placed();
    let bot = scene.bot;
    scene.app.world_mut().entity_mut(bot).insert(Riding {
        vehicle: MinecraftEntityId(7),
    });
    scene.press(key(Key::Forward));

    scene.move_to(Vec3::new(9.0, 64.0, 0.5));
    scene.tick();

    assert_eq!(scene.keys().0, WalkDirection::None);
    assert_eq!(scene.events(), [SessionEvent::LetGo(LetGo::Leash)]);
}

// --- The left button: mining behind its two guards (Plan.md P15.11) ---

#[test]
fn a_held_left_button_is_azaleas_hold_to_mine_from_the_next_tick() {
    let mut scene = Scene::placed();
    scene.looks_at(Some(block(0, 65, 2)));

    scene.press(LEFT);
    let before_the_tick = scene.has::<LeftClickMine>();
    scene.tick();
    let held = scene.has::<LeftClickMine>();
    scene.release(LEFT);
    scene.tick();

    assert!(!before_the_tick && held);
    assert!(!scene.has::<LeftClickMine>());
}

#[rstest]
#[case::the_block_it_stands_on(block(0, 63, 0), false)]
#[case::a_block_under_that(block(0, 60, 0), false)]
#[case::the_floor_beside_it(block(1, 63, 0), false)]
#[case::a_block_at_its_feet(block(1, 64, 0), true)]
#[case::a_block_at_its_eyes(block(1, 65, 0), true)]
#[case::a_block_above_it(block(0, 66, 0), true)]
fn the_mining_guard_holds_the_hold_back_while_the_bot_looks_at_a_block_below_its_feet(
    #[case] looked_at: BlockPos,
    #[case] mined: bool,
) {
    let mut scene = Scene::placed();
    scene.looks_at(Some(looked_at));
    scene.press(LEFT);

    scene.ticks(2);

    assert_eq!(scene.has::<LeftClickMine>(), mined);
    // The button stays down either way: the hold only pauses.
    assert!(scene.held().left);
    assert_eq!(scene.events(), []);
}

#[test]
fn a_paused_hold_goes_on_when_the_bot_looks_at_a_block_it_may_mine() {
    let mut scene = Scene::placed();
    scene.looks_at(Some(block(0, 63, 0)));
    scene.press(LEFT);
    scene.tick();
    let on_the_floor = scene.has::<LeftClickMine>();

    scene.looks_at(Some(block(0, 65, 2)));
    scene.tick();
    let on_the_wall = scene.has::<LeftClickMine>();
    scene.looks_at(None);
    scene.tick();

    assert!(!on_the_floor && on_the_wall);
    // Looking at nothing is azalea's to handle: it stops by itself.
    assert!(scene.has::<LeftClickMine>());
}

#[test]
fn a_bot_on_a_slab_doesnt_mine_at_the_slabs_height() {
    // Feet half a block up, inside the cube of the block it stands on.
    let mut scene = Scene::placed();
    scene.move_to(Vec3::new(0.5, 64.5, 0.5));
    scene.tick();
    scene.press(LEFT);

    scene.looks_at(Some(block(1, 64, 0)));
    scene.tick();
    let beside_the_slab = scene.has::<LeftClickMine>();
    scene.looks_at(Some(block(1, 65, 0)));
    scene.tick();

    assert!(!beside_the_slab);
    assert!(scene.has::<LeftClickMine>());
}

#[test]
fn what_azalea_is_about_to_mine_below_the_feet_never_starts() {
    let mut scene = Scene::placed();
    scene.looks_at(Some(block(0, 65, 2)));
    scene.press(LEFT);
    scene.tick();
    let bot = scene.bot;
    // Queued for a block that was level with the feet; then the bot rose.
    scene.app.world_mut().entity_mut(bot).insert(MiningQueued {
        position: block(1, 64, 0),
        direction: Face::North,
        force: true,
    });
    scene.move_to(Vec3::new(0.5, 65.2, 0.5));

    scene.tick();

    assert!(!scene.has::<MiningQueued>());
    assert_eq!(scene.actions_sent(), []);
}

#[test]
fn a_block_being_mined_is_let_alone_once_it_reaches_below_the_feet() {
    let mut scene = Scene::placed();
    scene.looks_at(Some(block(1, 64, 0)));
    scene.press(LEFT);
    scene.tick();
    let bot = scene.bot;
    scene.app.world_mut().entity_mut(bot).insert(Mining {
        pos: block(1, 64, 0),
        dir: Face::North,
        force: true,
    });
    **scene.app.world_mut().get_mut::<MineProgress>(bot).unwrap() = 0.6;
    scene.tick();
    let while_level = (scene.has::<Mining>(), scene.actions_sent());

    // A jump: the block's bottom is below the feet now.
    scene.move_to(Vec3::new(0.5, 64.9, 0.5));
    scene.tick();

    assert_eq!(while_level, (true, Vec::new()));
    assert!(!scene.has::<Mining>() && !scene.has::<LeftClickMine>());
    assert_eq!(
        scene.actions_sent(),
        [Sent::Action(
            PlayerAction::AbortDestroyBlock,
            block(1, 64, 0)
        )]
    );
    assert!(scene.get::<MineProgress>().unwrap().abs() < f32::EPSILON);
    // Paused, not ended.
    assert!(scene.held().left);
}

#[test]
fn letting_go_of_the_left_button_stops_mining_at_the_next_tick() {
    let mut scene = Scene::placed();
    scene.looks_at(Some(block(0, 65, 2)));
    scene.press(LEFT);
    scene.tick();
    let bot = scene.bot;
    scene.app.world_mut().entity_mut(bot).insert(Mining {
        pos: block(0, 65, 2),
        dir: Face::North,
        force: true,
    });

    scene.release(LEFT);
    scene.tick();

    assert!(!scene.has::<Mining>() && !scene.has::<LeftClickMine>());
    assert_eq!(
        scene.actions_sent(),
        [Sent::Action(
            PlayerAction::AbortDestroyBlock,
            block(0, 65, 2)
        )]
    );
    assert!(!scene.held().left);
    assert_eq!(scene.events(), []);
}

#[rstest]
#[case::many_uses_left(pickaxe(200), ToolGuard::DEFAULT, true)]
#[case::one_more_than_the_guard_keeps(pickaxe(3), ToolGuard::DEFAULT, true)]
#[case::as_many_as_the_guard_keeps(pickaxe(2), ToolGuard::DEFAULT, false)]
#[case::fewer(pickaxe(1), ToolGuard::DEFAULT, false)]
#[case::the_guard_is_off(pickaxe(1), ToolGuard::OFF, true)]
#[case::a_guard_of_ten(pickaxe(10), ToolGuard::try_from(10).unwrap(), false)]
#[case::an_item_that_doesnt_wear(ItemStack::new(ItemKind::Stick, 1), ToolGuard::DEFAULT, true)]
#[case::an_empty_hand(ItemStack::Empty, ToolGuard::DEFAULT, true)]
fn the_tool_guard_ends_a_hold_when_the_tool_is_nearly_used_up(
    #[case] tool: ItemStack,
    #[case] tool_guard: ToolGuard,
    #[case] goes_on: bool,
) {
    let mut scene = Scene::placed();
    scene.guards(Guards {
        leash: LeashRadius::DEFAULT,
        tool_guard,
        block_guard: BlockGuard::ANY,
    });
    scene.holds(pickaxe(200));
    scene.looks_at(Some(block(0, 65, 2)));
    scene.press(LEFT);
    scene.tick();
    let bot = scene.bot;
    scene.app.world_mut().entity_mut(bot).insert(Mining {
        pos: block(0, 65, 2),
        dir: Face::North,
        force: true,
    });

    // The tool wears down while it mines.
    scene.holds(tool);
    scene.tick();

    assert_eq!(scene.held().left, goes_on);
    assert_eq!(scene.has::<LeftClickMine>(), goes_on);
    assert_eq!(scene.has::<Mining>(), goes_on);
    if goes_on {
        assert_eq!(scene.events(), []);
    } else {
        assert_eq!(scene.events(), [SessionEvent::LetGo(LetGo::ToolGuard)]);
        assert_eq!(
            scene.actions_sent(),
            [Sent::Action(
                PlayerAction::AbortDestroyBlock,
                block(0, 65, 2)
            )]
        );
    }
}

// --- The block guard (ADR-0042's amendment, G13) ---

/// The guards of a mode that lists `blocks` for mining.
fn listing(blocks: &[&str]) -> Guards {
    let blocks: Vec<String> = blocks.iter().map(|block| (*block).to_owned()).collect();
    Guards {
        block_guard: BlockGuard::try_from(blocks).unwrap(),
        ..Guards::default()
    }
}

#[rstest]
#[case::the_listed_block(BlockKind::Cobblestone, &["cobblestone"], true)]
#[case::the_wall_behind_it(BlockKind::Stone, &["cobblestone"], false)]
#[case::one_of_two_listed(BlockKind::Stone, &["cobblestone", "minecraft:stone"], true)]
#[case::air_where_the_block_was(BlockKind::Air, &["cobblestone"], false)]
#[case::any_block_with_nothing_listed(BlockKind::Stone, &[], true)]
fn the_block_guard_holds_the_hold_back_while_the_bot_looks_at_a_block_the_mode_doesnt_list(
    #[case] kind: BlockKind,
    #[case] listed: &[&str],
    #[case] mined: bool,
) {
    let mut scene = Scene::placed();
    let _loaded = scene.chunk();
    scene.set_block(block(0, 65, 2), kind);
    scene.guards(listing(listed));
    scene.looks_at(Some(block(0, 65, 2)));
    scene.press(LEFT);

    scene.ticks(2);

    assert_eq!(scene.has::<LeftClickMine>(), mined);
    // The button stays down either way: the hold only pauses.
    assert!(scene.held().left);
    assert_eq!(scene.events(), []);
}

/// No chunk is loaded here, so the bot can't name the block: a mode that
/// lists blocks mines none it can't name, and one that lists nothing mines
/// as before.
#[rstest]
#[case::a_mode_that_lists_blocks(&["cobblestone"], false)]
#[case::a_mode_that_lists_nothing(&[], true)]
fn a_block_the_bot_cant_name_is_mined_only_by_a_mode_that_lists_nothing(
    #[case] listed: &[&str],
    #[case] mined: bool,
) {
    let mut scene = Scene::placed();
    scene.guards(listing(listed));
    scene.looks_at(Some(block(0, 65, 2)));
    scene.press(LEFT);

    scene.ticks(2);

    assert_eq!(scene.has::<LeftClickMine>(), mined);
    assert!(scene.held().left);
}

/// What PR #126's generator showed: once the block is gone, the crosshair
/// lands on the wall behind it.
#[test]
fn at_a_generator_the_hold_pauses_on_the_wall_and_goes_on_when_the_next_block_forms() {
    let mut scene = Scene::placed();
    let _loaded = scene.chunk();
    let (cobble, wall) = (block(2, 64, 0), block(3, 64, 0));
    scene.set_block(cobble, BlockKind::Cobblestone);
    scene.set_block(wall, BlockKind::Stone);
    scene.guards(listing(&["cobblestone"]));
    scene.looks_at(Some(cobble));
    scene.press(LEFT);
    scene.tick();
    let on_the_block = scene.has::<LeftClickMine>();

    scene.set_block(cobble, BlockKind::Air);
    scene.looks_at(Some(wall));
    scene.tick();
    let on_the_wall = scene.has::<LeftClickMine>();

    scene.set_block(cobble, BlockKind::Cobblestone);
    scene.looks_at(Some(cobble));
    scene.tick();

    assert!(on_the_block && !on_the_wall);
    assert!(scene.has::<LeftClickMine>());
    assert!(scene.held().left);
    assert_eq!(scene.events(), []);
    assert_eq!(scene.actions_sent(), []);
}

#[test]
fn what_azalea_is_about_to_mine_that_the_mode_doesnt_list_never_starts() {
    let mut scene = Scene::placed();
    let _loaded = scene.chunk();
    let (cobble, wall) = (block(2, 64, 0), block(3, 64, 0));
    scene.set_block(cobble, BlockKind::Cobblestone);
    scene.set_block(wall, BlockKind::Stone);
    scene.guards(listing(&["cobblestone"]));
    scene.looks_at(Some(cobble));
    scene.press(LEFT);
    scene.tick();
    let bot = scene.bot;
    // Queued for the wall, whatever the crosshair is on by now.
    scene.app.world_mut().entity_mut(bot).insert(MiningQueued {
        position: wall,
        direction: Face::North,
        force: true,
    });

    scene.tick();

    assert!(!scene.has::<MiningQueued>());
    assert_eq!(scene.actions_sent(), []);
    assert!(scene.held().left);
}

#[test]
fn a_block_being_mined_is_let_alone_once_it_is_no_listed_block_any_more() {
    let mut scene = Scene::placed();
    let _loaded = scene.chunk();
    let cobble = block(2, 64, 0);
    scene.set_block(cobble, BlockKind::Cobblestone);
    scene.guards(listing(&["cobblestone"]));
    scene.looks_at(Some(cobble));
    scene.press(LEFT);
    scene.tick();
    let bot = scene.bot;
    scene.app.world_mut().entity_mut(bot).insert(Mining {
        pos: cobble,
        dir: Face::North,
        force: true,
    });
    **scene.app.world_mut().get_mut::<MineProgress>(bot).unwrap() = 0.6;
    scene.tick();
    let while_listed = (scene.has::<Mining>(), scene.actions_sent());

    // The server put another block there.
    scene.set_block(cobble, BlockKind::Stone);
    scene.tick();

    assert_eq!(while_listed, (true, Vec::new()));
    assert!(!scene.has::<Mining>() && !scene.has::<LeftClickMine>());
    assert_eq!(
        scene.actions_sent(),
        [Sent::Action(PlayerAction::AbortDestroyBlock, cobble)]
    );
    assert!(scene.get::<MineProgress>().unwrap().abs() < f32::EPSILON);
    // Paused, not ended.
    assert!(scene.held().left);
    assert_eq!(scene.events(), []);
}

#[test]
fn the_floor_is_never_mined_even_when_the_mode_lists_it() {
    let mut scene = Scene::placed();
    let _loaded = scene.chunk();
    scene.set_block(block(0, 63, 0), BlockKind::Cobblestone);
    scene.guards(listing(&["cobblestone"]));
    scene.looks_at(Some(block(0, 63, 0)));
    scene.press(LEFT);

    scene.ticks(2);

    assert!(!scene.has::<LeftClickMine>());
}

// --- The right button (Plan.md P15.11) ---

/// The game ticks, counted from the one after the press, in which a use
/// went out, over `ticks` of holding the right button with `stack` in
/// hand. azalea's own handler takes the queued use out each tick; here the
/// test does.
fn uses_over(scene: &mut Scene, ticks: usize) -> Vec<usize> {
    let bot = scene.bot;
    let mut used = Vec::new();
    for tick in 0..ticks {
        scene.tick();
        if scene.has::<StartUseItemQueued>() {
            used.push(tick);
            scene
                .app
                .world_mut()
                .entity_mut(bot)
                .remove::<StartUseItemQueued>();
        }
    }
    used
}

#[rstest]
#[case::a_stick(ItemStack::new(ItemKind::Stick, 1))]
#[case::a_block(ItemStack::new(ItemKind::Cobblestone, 64))]
#[case::an_empty_hand(ItemStack::Empty)]
fn a_held_right_button_uses_again_every_four_ticks_as_the_vanilla_client_does(
    #[case] stack: ItemStack,
) {
    let mut scene = Scene::placed();
    scene.holds(stack);

    scene.press(RIGHT);
    let used = uses_over(&mut scene, 13);

    // The press's own use goes out at the first tick, then every fourth.
    assert_eq!(used, [0, 4, 8, 12]);
    assert_eq!(scene.events(), []);
}

#[rstest]
#[case::a_bow(ItemStack::new(ItemKind::Bow, 1))]
#[case::a_shield(ItemStack::new(ItemKind::Shield, 1))]
#[case::food(ItemStack::new(ItemKind::Bread, 3))]
#[case::a_trident(ItemStack::new(ItemKind::Trident, 1))]
#[case::a_crossbow(ItemStack::new(ItemKind::Crossbow, 1))]
#[case::a_spyglass(ItemStack::new(ItemKind::Spyglass, 1))]
fn with_an_item_used_over_time_a_held_right_button_is_one_long_use(#[case] stack: ItemStack) {
    let mut scene = Scene::placed();
    scene.holds(stack);

    scene.press(RIGHT);
    let used = uses_over(&mut scene, 40);

    assert_eq!(used, [0]);
    assert!(scene.held().right.is_some());
}

#[test]
fn holding_on_after_one_piece_of_food_is_eaten_starts_the_next() {
    let mut scene = Scene::placed();
    scene.holds(ItemStack::new(ItemKind::Bread, 3));
    scene.press(RIGHT);
    let first = uses_over(&mut scene, 33);

    // The server took one: the stack in hand changed.
    scene.holds(ItemStack::new(ItemKind::Bread, 2));
    let second = uses_over(&mut scene, 6);

    assert_eq!(first, [0]);
    assert_eq!(second, [0]);
}

#[test]
fn held_until_used_the_right_button_goes_up_once_the_item_in_hand_changed() {
    let mut scene = Scene::placed();
    scene.holds(ItemStack::new(ItemKind::Bread, 3));

    assert_eq!(scene.act(GameAction::HoldUntilUsed), Ok(()));
    let eating = uses_over(&mut scene, 33);
    let still_held = scene.held().right.is_some();
    scene.holds(ItemStack::new(ItemKind::Bread, 2));
    let after = uses_over(&mut scene, 12);

    assert_eq!(eating, [0]);
    assert!(still_held);
    assert_eq!(after, Vec::<usize>::new());
    assert_eq!(scene.held().right, None);
    assert_eq!(
        scene.actions_sent(),
        [Sent::Action(
            PlayerAction::ReleaseUseItem,
            BlockPos::default()
        )]
    );
    assert_eq!(scene.events(), [SessionEvent::LetGo(LetGo::ItemUsed)]);
}

#[test]
fn a_press_of_the_right_button_notes_what_was_in_hand() {
    let mut scene = Scene::placed();
    scene.holds(ItemStack::new(ItemKind::Bread, 3));

    scene.press(RIGHT);

    assert_eq!(
        scene.held().right,
        Some(RightHold {
            until_used: false,
            since_use: 0,
            used: ItemStack::new(ItemKind::Bread, 3),
        })
    );
    assert!(scene.has::<StartUseItemQueued>());
}

#[test]
fn letting_go_of_the_right_button_cancels_a_queued_use_and_says_so_to_the_server() {
    let mut scene = Scene::placed();
    scene.holds(ItemStack::new(ItemKind::Bow, 1));
    scene.press(RIGHT);

    scene.release(RIGHT);
    let used = uses_over(&mut scene, 10);

    assert_eq!(used, Vec::<usize>::new());
    assert_eq!(scene.held().right, None);
    assert_eq!(
        scene.actions_sent(),
        [Sent::Action(
            PlayerAction::ReleaseUseItem,
            BlockPos::default()
        )]
    );
    assert_eq!(scene.events(), []);
}

#[test]
fn a_tap_of_the_right_button_is_one_use() {
    let mut scene = Scene::placed();

    scene.tap(RIGHT);
    let used = uses_over(&mut scene, 10);

    assert_eq!(used, [0]);
    assert_eq!(scene.held().right, None);
}

// --- Looking at a point, and getting out (Plan.md P15.10, P15.13) ---

#[test]
fn looking_at_a_point_turns_the_bot_to_it_from_its_eyes() {
    let mut scene = Scene::placed();

    // Straight east of the eyes, 1.62 above the feet.
    let east = fleet_core::script::Position::try_new(10.5, 65.62, 0.5).unwrap();
    assert_eq!(scene.act(GameAction::LookAtPosition(east)), Ok(()));
    let looked_east = scene.get::<LookDirection>().unwrap();
    // Straight down at its own feet's block.
    let down = fleet_core::script::Position::try_new(0.5, 60.0, 0.5).unwrap();
    assert_eq!(scene.act(GameAction::LookAtPosition(down)), Ok(()));
    let looked_down = scene.get::<LookDirection>().unwrap();

    assert!((looked_east.y_rot() - -90.0).abs() < 0.2, "{looked_east:?}");
    assert!(looked_east.x_rot().abs() < 0.2, "{looked_east:?}");
    assert!((looked_down.x_rot() - 90.0).abs() < 0.2, "{looked_down:?}");
}

#[test]
fn getting_out_is_a_tap_of_the_sneak_key_and_nothing_for_a_bot_on_foot() {
    let mut scene = Scene::placed();

    assert_eq!(scene.act(GameAction::Dismount), Ok(()));
    scene.tick();
    let on_foot = scene.keys();
    let bot = scene.bot;
    scene.app.world_mut().entity_mut(bot).insert(Riding {
        vehicle: MinecraftEntityId(7),
    });
    assert_eq!(scene.act(GameAction::Dismount), Ok(()));
    scene.tick();
    let seated = scene.keys();
    scene.tick();

    assert_eq!(on_foot, STILL);
    assert_eq!(seated, (WalkDirection::None, false, true, false));
    assert_eq!(scene.keys(), STILL);
}

// --- What a hook sees (Plan.md P15.13) ---

use azalea::block::BlockState;
use azalea::core::hit_result::EntityHitResult;
use azalea::core::position::ChunkPos;
use azalea::entity::EntityKindComponent;
use azalea::entity::metadata::Health;
use azalea::local_player::Hunger;
use azalea::registry::builtin::BlockKind;
use azalea::world::{Chunk, PartialChunkStorage};
use fleet_core::mc::Observation;
use fleet_core::mode::HotbarSlot;
use fleet_core::script::{
    BlockPosition, EntityFact, HeldItem, Identifier, LookingAt, Position as Place,
};

use super::facts::observe;

fn id(text: &str) -> Identifier {
    Identifier::try_from(text).unwrap()
}

fn place(x: f64, y: f64, z: f64) -> Place {
    Place::try_new(x, y, z).unwrap()
}

impl Scene {
    fn observe(&mut self) -> Result<Observation, SessionError> {
        observe(self.app.world_mut(), self.bot)
    }

    /// An entity of `kind` at `at`, which the server told the bot about
    /// under `server_id`.
    fn entity(&mut self, kind: EntityKind, at: Vec3, server_id: i32) -> Entity {
        let entity = self
            .app
            .world_mut()
            .spawn((
                Position::new(at),
                EntityKindComponent(kind),
                MinecraftEntityId(server_id),
            ))
            .id();
        self.app
            .world_mut()
            .get_mut::<EntityIdIndex>(self.bot)
            .unwrap()
            .insert(MinecraftEntityId(server_id), entity);
        entity
    }

    /// Loads the chunk the bot stands in, all air, and returns what keeps
    /// it loaded.
    fn chunk(&mut self) -> PartialChunkStorage {
        let worlds = self.app.world().resource::<Worlds>();
        let world = worlds
            .get(&WorldName::new("minecraft:overworld"))
            .expect("the scene's world");
        let mut loaded = PartialChunkStorage::default();
        loaded.set(
            &ChunkPos::new(0, 0),
            Some(Chunk::default()),
            &mut world.write().chunks,
        );
        loaded
    }

    fn set_block(&mut self, at: BlockPos, kind: BlockKind) {
        let worlds = self.app.world().resource::<Worlds>();
        let world = worlds
            .get(&WorldName::new("minecraft:overworld"))
            .expect("the scene's world");
        assert!(
            world
                .read()
                .set_block_state(at, BlockState::from(kind))
                .is_some(),
            "{at:?} is in a loaded chunk"
        );
    }
}

#[test]
fn a_bot_the_server_hasnt_placed_has_nothing_to_observe() {
    let mut scene = Scene::new();

    let between_worlds = scene.observe();
    scene.teleported(FEET);
    scene.tick();
    let placed = scene.observe();
    let bot = scene.bot;
    scene.app.world_mut().despawn(bot);

    assert_eq!(between_worlds, Err(SessionError::NotInWorld));
    assert!(placed.is_ok(), "{placed:?}");
    assert_eq!(scene.observe(), Err(SessionError::NotInWorld));
}

#[test]
fn the_snapshot_says_how_the_bot_is_and_where_its_leash_is_anchored() {
    let mut scene = Scene::placed();
    let bot = scene.bot;
    scene.app.world_mut().entity_mut(bot).insert((
        Health(13.5),
        Hunger {
            food: 17,
            saturation: 2.0,
        },
        LookDirection::new(45.0, -10.5),
    ));
    scene
        .app
        .world_mut()
        .get_mut::<Inventory>(bot)
        .unwrap()
        .selected_hotbar_slot = 3;
    scene.holds(pickaxe(7));
    scene.move_to(Vec3::new(2.25, 65.0, -1.5));

    let seen = scene.observe().unwrap();

    let me = seen.facts.bot();
    assert!((me.health - 13.5).abs() < f32::EPSILON);
    assert_eq!(me.food, 17);
    assert_eq!(me.position, place(2.25, 65.0, -1.5));
    assert!(
        (me.yaw - 45.0).abs() < 0.2 && (me.pitch - -10.5).abs() < 0.2,
        "{me:?}"
    );
    assert_eq!(me.dimension, id("minecraft:overworld"));
    assert_eq!(me.riding, None);
    assert_eq!(
        me.held,
        Some(HeldItem {
            id: id("minecraft:iron_pickaxe"),
            count: 1,
            durability: Some(7),
        })
    );
    assert_eq!(me.selected, HotbarSlot::try_from(3).unwrap());
    assert_eq!(me.looking_at, None);
    // Where the server put it, not where it stands now.
    assert_eq!(seen.anchor, Some(place(0.5, 64.0, 0.5)));
}

#[rstest]
#[case::an_empty_hand(ItemStack::Empty, None)]
#[case::a_stack_that_doesnt_wear(
    ItemStack::new(ItemKind::Cobblestone, 48),
    Some(("minecraft:cobblestone", 48, None))
)]
#[case::a_new_tool(
    ItemStack::new(ItemKind::IronPickaxe, 1),
    Some(("minecraft:iron_pickaxe", 1, Some(250)))
)]
#[case::a_worn_tool(pickaxe(2), Some(("minecraft:iron_pickaxe", 1, Some(2))))]
#[case::a_tool_that_cant_break(
    ItemStack::new(ItemKind::IronPickaxe, 1).with_component(components::Unbreakable),
    Some(("minecraft:iron_pickaxe", 1, None))
)]
fn the_held_item_is_its_kind_its_count_and_the_uses_it_has_left(
    #[case] stack: ItemStack,
    #[case] held: Option<(&str, u32, Option<u32>)>,
) {
    let mut scene = Scene::placed();
    scene.holds(stack);

    let seen = scene.observe().unwrap();

    assert_eq!(
        seen.facts.bot().held,
        held.map(|(kind, count, durability)| HeldItem {
            id: id(kind),
            count,
            durability,
        })
    );
}

#[test]
fn entities_near_the_bot_are_seen_nearest_first_with_who_sits_in_a_vehicle() {
    let mut scene = Scene::placed();
    scene.entity(EntityKind::Pig, Vec3::new(5.5, 64.0, 0.5), 10);
    scene.entity(EntityKind::Minecart, Vec3::new(2.5, 64.0625, 0.5), 11);
    scene.entity(EntityKind::Minecart, Vec3::new(0.5, 64.0625, 3.5), 12);
    // 16.5 blocks away: out of a snapshot's range.
    scene.entity(EntityKind::Zombie, Vec3::new(17.0, 64.0, 0.5), 13);
    scene.receives(crate::testing::passengers(
        MinecraftEntityId(12),
        &[MinecraftEntityId(99)],
    ));

    let seen = scene.observe().unwrap();

    assert_eq!(
        seen.facts.entities(),
        [
            EntityFact {
                kind: id("minecraft:minecart"),
                position: place(2.5, 64.0625, 0.5),
                occupied: false,
            },
            EntityFact {
                kind: id("minecraft:minecart"),
                position: place(0.5, 64.0625, 3.5),
                occupied: true,
            },
            EntityFact {
                kind: id("minecraft:pig"),
                position: place(5.5, 64.0, 0.5),
                occupied: false,
            },
        ]
    );
    assert_eq!(
        seen.facts
            .nearest_free(&id("minecart"))
            .map(|cart| cart.position),
        Some(place(2.5, 64.0625, 0.5))
    );
}

#[test]
fn a_riding_bot_says_what_it_sits_in() {
    let mut scene = Scene::placed();
    scene.entity(EntityKind::Minecart, Vec3::new(0.5, 64.0625, 0.5), 11);
    let bot = scene.bot;
    scene.app.world_mut().entity_mut(bot).insert(Riding {
        vehicle: MinecraftEntityId(11),
    });
    let in_a_minecart = scene.observe().unwrap();
    // A vehicle the server never showed the bot.
    scene.app.world_mut().entity_mut(bot).insert(Riding {
        vehicle: MinecraftEntityId(77),
    });

    let in_something = scene.observe().unwrap();

    assert_eq!(
        in_a_minecart.facts.bot().riding,
        Some(id("minecraft:minecart"))
    );
    assert_eq!(
        in_something.facts.bot().riding,
        Some(id("minecraft:unknown"))
    );
}

#[test]
fn the_blocks_around_the_bot_and_the_one_it_looks_at_are_its_worlds() {
    let mut scene = Scene::placed();
    let _loaded = scene.chunk();
    scene.set_block(block(0, 63, 0), BlockKind::Stone);
    scene.set_block(block(0, 65, 2), BlockKind::Cobblestone);
    scene.looks_at(Some(block(0, 65, 2)));

    let seen = scene.observe().unwrap();

    let blocks = seen.facts.blocks();
    let at = |x, y, z| blocks.get(BlockPosition { x, y, z }).cloned();
    assert_eq!(blocks.center(), BlockPosition { x: 0, y: 64, z: 0 });
    assert_eq!(at(0, 63, 0), Some(id("minecraft:stone")));
    assert_eq!(at(0, 65, 2), Some(id("minecraft:cobblestone")));
    assert_eq!(at(0, 64, 0), Some(id("minecraft:air")));
    // The chunk west of the bot's isn't loaded.
    assert_eq!(at(-1, 64, 0), None);
    assert_eq!(
        seen.facts.bot().looking_at,
        Some(LookingAt::Block {
            id: id("minecraft:cobblestone"),
            at: BlockPosition { x: 0, y: 65, z: 2 },
        })
    );
}

#[test]
fn a_block_the_bot_looks_at_in_a_chunk_it_hasnt_loaded_is_nothing() {
    let mut scene = Scene::placed();
    scene.looks_at(Some(block(0, 65, 2)));

    let seen = scene.observe().unwrap();

    assert_eq!(seen.facts.bot().looking_at, None);
    assert_eq!(
        seen.facts.blocks().get(BlockPosition { x: 0, y: 64, z: 0 }),
        None
    );
}

#[test]
fn an_entity_under_the_crosshair_is_its_kind_and_its_place() {
    let mut scene = Scene::placed();
    let pig = scene.entity(EntityKind::Pig, Vec3::new(0.5, 64.0, 2.5), 10);
    let bot = scene.bot;
    **scene
        .app
        .world_mut()
        .get_mut::<HitResultComponent>(bot)
        .unwrap() = HitResult::Entity(EntityHitResult {
        location: Vec3::new(0.5, 64.5, 2.1),
        entity: pig,
    });

    let seen = scene.observe().unwrap();

    assert_eq!(
        seen.facts.bot().looking_at,
        Some(LookingAt::Entity {
            kind: id("minecraft:pig"),
            at: place(0.5, 64.0, 2.5),
        })
    );
}

#[rstest]
#[case::the_nether("minecraft:the_nether", "minecraft:the_nether")]
#[case::a_servers_own("lobby:hub_1", "lobby:hub_1")]
#[case::a_name_that_is_no_identifier("Not A Name!", "minecraft:unknown")]
fn the_dimension_is_an_identifier_whatever_the_server_calls_its_world(
    #[case] world: &str,
    #[case] dimension: &str,
) {
    let mut scene = Scene::placed();
    let bot = scene.bot;
    scene
        .app
        .world_mut()
        .entity_mut(bot)
        .insert(WorldName::new(world));

    let seen = scene.observe().unwrap();

    assert_eq!(seen.facts.bot().dimension, id(dimension));
}

// --- What a snapshot costs the host thread (Plan.md P15.19, ADR-0043) ---

/// Measures one snapshot on the session's own World: the cube of 729
/// blocks around the bot in a chunk full of stone, and 64 entities in
/// range among 200 the bot knows. `just load` runs it and prints the
/// line; it asserts only that the snapshot is the full one.
#[test]
fn load_a_snapshots_cost_on_the_host_thread() {
    let mut scene = Scene::placed();
    let _loaded = scene.chunk();
    // The bot stands in its chunk's corner: the quarter of the cube that
    // lies in this chunk is known, as at the edge of what a server sent.
    for x in 0..=4 {
        for z in 0..=4 {
            for y in 60..64 {
                let kind = if (x + z) % 2 == 0 {
                    BlockKind::Stone
                } else {
                    BlockKind::Cobblestone
                };
                scene.set_block(block(x, y, z), kind);
            }
        }
    }
    for n in 0..200_i32 {
        // Every third one within 16 blocks, the others far away.
        let far = if n % 3 == 0 { 0.0 } else { 40.0 };
        let at = Vec3::new(2.0 + far + f64::from(n) * 0.04, 64.0, 3.0);
        scene.entity(EntityKind::Zombie, at, 1_000 + n);
    }
    let seen = scene.observe().expect("a placed bot is observed");
    assert_eq!(seen.facts.entities().len(), 64);
    assert_eq!(
        seen.facts
            .blocks()
            .get(fleet_core::script::BlockPosition { x: 2, y: 63, z: 2 })
            .map(Identifier::as_str),
        Some("minecraft:stone")
    );

    let rounds = 2_000_u32;
    let begun = Instant::now();
    for _ in 0..rounds {
        let again = scene.observe().expect("a placed bot is observed");
        assert_eq!(again.facts.entities().len(), 64);
    }
    let each = begun.elapsed() / rounds;

    println!(
        "LOAD snapshot: {each:.1?} a snapshot on the host thread (729 block places, 64 of 200 entities)"
    );
}
