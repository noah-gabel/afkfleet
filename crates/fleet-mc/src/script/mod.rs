//! A scripted mode's part of a session: its inputs, what's kept below them,
//! and what a hook sees (Plan.md P15.10 to P15.14; ADR-0036, D9 to D12).
//!
//! A scripted mode is a virtual player at a vanilla client: it presses keys
//! and mouse buttons, and fleet-mc translates. Nothing here decides what a
//! bot may do; fleet-core does (`fleet_core::script`), and this module
//! applies it on the session's host thread:
//!
//! - **The inputs.** [`Held`] on the bot says which keys and buttons are
//!   down. A game action only changes it. Each game tick, before azalea's
//!   physics, the session writes azalea's own key state from it (what
//!   azalea's `walk`, `sprint`, `set_jumping` and `set_crouching` set), so
//!   a key is down from the tick after its press, and a tap for exactly
//!   one tick. A new session's bot holds nothing.
//! - **The leash** (`tick`). [`Placement`] holds where the server put the
//!   bot: its position once the client has loaded after a login or a
//!   respawn, or after a server teleport of more than 8 blocks. Each game
//!   tick, before azalea's physics, a bot that is beyond its radius and
//!   got further lets go of every movement key, whatever the runtime does.
//! - **The mouse** (`mouse`). The left button, held, is azalea's
//!   hold-to-mine, behind the mining guard and the tool guard, both judged
//!   each game tick before azalea's mining. The right button, held, uses
//!   again every 4 game ticks unless the held item is used over time.
//! - **What a hook sees** (`facts`): the snapshot, read on the host thread.
//!
//! **Why before azalea's own systems.** A guard that ran after azalea's
//! mining would stop a block one tick late, when it may be broken already;
//! a leash after azalea's physics would let one more tick of walking
//! through. `connector::app` pins both orders with probes in azalea's own
//! system sets.
//!
//! What the session lets go of by itself, and a world change, reach the
//! runtime through the session's bridge ([`BridgeControl`]).

mod facts;
mod held;
mod mouse;
#[cfg(test)]
mod tests;
mod tick;

use azalea::app::{App, Plugin, Update};
use azalea::bot::direction_looking_at;
use azalea::core::direction::Direction as Face;
use azalea::core::tick::GameTick;
// The `Component` derive names its crate as `bevy_ecs`, which fleet-mc only
// has through azalea.
use azalea::ecs as bevy_ecs;
use azalea::ecs::component::Component;
use azalea::ecs::entity::Entity;
use azalea::ecs::lifecycle::Add;
use azalea::ecs::message::MessageReader;
use azalea::ecs::observer::On;
use azalea::ecs::query::With;
use azalea::ecs::schedule::IntoScheduleConfigs as _;
use azalea::ecs::system::{Commands, Query};
use azalea::ecs::world::World;
use azalea::entity::dimensions::{EntityDimensions, calculate_dimensions};
use azalea::entity::{LocalEntity, LookDirection, Pose, Position as Feet};
use azalea::interact::{StartUseItemQueued, SwingArmEvent, handle_start_use_item_queued};
use azalea::mining::{LeftClickMine, Mining, MiningQueued, MiningSystems};
use azalea::packet::game::{ReceiveGamePacketEvent, SendGamePacketEvent};
use azalea::physics::PhysicsSystems;
use azalea::protocol::packets::game::ClientboundGamePacket;
use azalea::protocol::packets::game::s_player_action::{
    Action as PlayerAction, ServerboundPlayerAction,
};
use azalea::registry::builtin::EntityKind;
use azalea::{BlockPos, Client, Vec3};
use fleet_core::mc::{Guards, Observation, SessionError};
use fleet_core::mode::GameAction;
use fleet_core::script::{Input, Key, Leash, MouseButton, Position};
use tracing::debug;

pub(crate) use self::held::Held;
use crate::actions;
use crate::events::BridgeControl;
use crate::riding::{self, Riding};

/// On the bot: the leash's radius, the tool guard and the block guard its
/// mode set, or a new session's defaults.
#[derive(Component, Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Guarded(pub(crate) Guards);

/// On a seated bot that is getting out: the sneak key stays down until the
/// server lets the bot off, for at most a second. A key tapped for one game
/// tick was lost when the client caught up on ticks and both its inputs
/// reached the server between two of its ticks.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GettingOut {
    /// How many more game ticks the key stays down.
    pub(crate) ticks_left: u8,
}

impl GettingOut {
    /// A second of game ticks.
    pub(crate) const MOST_TICKS: u8 = 20;
}

/// Whether the server has placed the bot in its world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Arriving {
    /// A login or a respawn came, and the teleport that says where the bot
    /// is hasn't. A new session starts here.
    #[default]
    AwaitingTeleport,
    /// The teleport came; the client hasn't loaded since.
    Teleported,
    /// The bot is placed.
    Placed,
}

/// On the bot: where the server put it, for its leash.
#[derive(Component, Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Placement {
    /// Where the bot was put. `None` between worlds.
    pub(crate) anchor: Option<Position>,
    /// Where the bot's feet were at the last game tick.
    pub(crate) last: Option<Position>,
    /// How far the bot's arrival in its world is.
    pub(crate) arriving: Arriving,
}

/// On the bot: its session's bridge, for what the tick's systems report
/// of it: a placement, and what the session let go of.
#[derive(Component, Debug, Clone)]
pub(crate) struct Reports(pub(crate) BridgeControl);

/// Keeps a scripted mode's inputs, its leash and its guards; see the
/// [module docs](self).
pub(crate) struct ScriptPlugin(pub(crate) BridgeControl);

impl Plugin for ScriptPlugin {
    fn build(&self, app: &mut App) {
        // azalea registers the message too; registering it again is a
        // no-op, and it lets the plugin work on its own.
        let control = self.0.clone();
        app.add_message::<ReceiveGamePacketEvent>()
            // From the moment azalea spawns the bot it has these, with
            // nothing held: a new session starts with every key up.
            .add_observer(
                move |spawned: On<Add, LocalEntity>, mut commands: Commands| {
                    commands.entity(spawned.entity).insert((
                        Held::default(),
                        Guarded::default(),
                        Placement::default(),
                        Reports(control.clone()),
                    ));
                },
            )
            .add_systems(Update, read_packets)
            // Every system has its place among azalea's that read what it
            // writes in the same tick: the key state before azalea's
            // physics, hold-to-mine and what's queued before azalea's
            // mining, the next use before azalea sends the tick's use.
            // Among themselves they run in one fixed order, since each
            // reads or writes what's held.
            .add_systems(
                GameTick,
                (
                    (
                        tick::place,
                        tick::leash,
                        tick::getting_out,
                        tick::apply_keys,
                        mouse::guard_mining.before(MiningSystems),
                        mouse::hold_right.before(handle_start_use_item_queued),
                    )
                        .chain()
                        .before(PhysicsSystems),
                    tick::end_taps.after(PhysicsSystems),
                ),
            );
    }
}

/// Reads the frame's packets for what they mean to a scripted mode's bot:
/// a login or a respawn puts it between worlds with nothing held, and a
/// teleport says where it is.
fn read_packets(
    mut commands: Commands,
    mut packets: MessageReader<ReceiveGamePacketEvent>,
    mut bots: Query<(&mut Held, &mut Placement, &Guarded, &Feet), With<LocalEntity>>,
) {
    for received in packets.read() {
        let Ok((mut held, mut placement, guarded, position)) = bots.get_mut(received.entity) else {
            continue;
        };
        match &*received.packet {
            ClientboundGamePacket::Login(_) | ClientboundGamePacket::Respawn(_) => {
                debug!("the server puts the bot into a world; everything it held is let go");
                held.release_all();
                *placement = Placement::default();
                // The other world's block: nothing to abort on the wire.
                commands.entity(received.entity).remove::<(
                    LeftClickMine,
                    Mining,
                    MiningQueued,
                    StartUseItemQueued,
                )>();
            }
            // azalea has moved the bot already: its handlers ran before
            // this frame's systems.
            ClientboundGamePacket::PlayerPosition(_) => match placement.arriving {
                Arriving::AwaitingTeleport => placement.arriving = Arriving::Teleported,
                Arriving::Teleported => {}
                Arriving::Placed => {
                    let (Some(anchor), Some(now)) = (placement.anchor, tick::feet(position)) else {
                        continue;
                    };
                    let mut leash = Leash::new(guarded.0.leash, anchor);
                    if leash.teleported(placement.last.unwrap_or(now), now) {
                        debug!("the server teleported the bot far; its leash is anchored anew");
                        placement.anchor = Some(leash.anchor());
                    }
                    // The teleport's own jump is no walking.
                    placement.last = Some(now);
                }
            },
            _ => {}
        }
    }
}

/// Performs one of a scripted mode's inputs with `client`, on the host
/// thread.
///
/// # Errors
/// [`SessionError::NotInWorld`] if the bot has left its world.
pub(crate) fn perform(client: &Client, action: GameAction) -> Result<(), SessionError> {
    match action {
        // The two that write an attack, which takes the bot's connection.
        // No guard on the World is held meanwhile: azalea's locks aren't
        // reentrant.
        GameAction::Tap(Input::Mouse(MouseButton::Left)) => {
            if looks_at_an_entity(client) {
                actions::attack(client)
            } else {
                act(&mut client.ecs.write(), client.entity, action)
            }
        }
        GameAction::Press(Input::Mouse(MouseButton::Left)) => {
            // An entity under the crosshair when the button goes down is
            // attacked once, never again during the hold.
            if looks_at_an_entity(client) {
                actions::attack(client)?;
            }
            act(&mut client.ecs.write(), client.entity, action)
        }
        _ => act(&mut client.ecs.write(), client.entity, action),
    }
}

/// Whether the bot's crosshair is on an entity within reach.
fn looks_at_an_entity(client: &Client) -> bool {
    client
        .get_component::<azalea::interact::pick::HitResultComponent>()
        .is_some_and(|hit| hit.as_entity_hit_result().is_some())
}

/// Performs `action` for `bot` in `world`: everything of a scripted mode's
/// inputs that needs no `Client`.
///
/// # Errors
/// [`SessionError::NotInWorld`] if the bot's entity is gone, or lacks what
/// the action needs.
pub(crate) fn act(world: &mut World, bot: Entity, action: GameAction) -> Result<(), SessionError> {
    // Every input is the bot's own; a bot azalea no longer has holds
    // nothing.
    let held = |world: &mut World, change: fn(&mut Held, Key), key: Key| {
        let mut held = world.get_mut::<Held>(bot).ok_or(SessionError::NotInWorld)?;
        change(&mut held, key);
        Ok(())
    };
    if world.get::<Held>(bot).is_none() {
        return Err(SessionError::NotInWorld);
    }
    match action {
        GameAction::Press(Input::Key(key)) if key.can_be_held() => held(world, Held::press, key),
        GameAction::Tap(Input::Key(key)) if key.can_be_held() => held(world, Held::tap, key),
        // The three that only tap do the same pressed or tapped.
        GameAction::Press(Input::Key(key)) | GameAction::Tap(Input::Key(key)) => {
            tap_only(world, bot, key);
            Ok(())
        }
        GameAction::Release(Input::Key(key)) => held(world, Held::release, key),
        GameAction::ReleaseKeys => held(world, |held, _| held.release_keys(), Key::Forward),
        GameAction::ReleaseAll => {
            held(world, |held, _| held.release_all(), Key::Forward)?;
            // Mining stops at the next game tick, with the button up.
            mouse::let_go_of_use(world, bot)
        }
        GameAction::Press(Input::Mouse(MouseButton::Left)) => {
            held(world, |held, _| held.left = true, Key::Forward)
        }
        GameAction::Release(Input::Mouse(MouseButton::Left)) => {
            held(world, |held, _| held.left = false, Key::Forward)
        }
        // On an entity it's an attack, which [`perform`] writes; on
        // anything else the arm swings.
        GameAction::Tap(Input::Mouse(MouseButton::Left)) => {
            world.trigger(SwingArmEvent { entity: bot });
            Ok(())
        }
        GameAction::Press(Input::Mouse(MouseButton::Right)) => {
            mouse::press_right(world, bot, false)
        }
        GameAction::HoldUntilUsed => mouse::press_right(world, bot, true),
        GameAction::Tap(Input::Mouse(MouseButton::Right)) => mouse::use_once(world, bot),
        GameAction::Release(Input::Mouse(MouseButton::Right)) => {
            held(world, |held, _| held.right = None, Key::Forward)?;
            mouse::let_go_of_use(world, bot)
        }
        GameAction::LookAtPosition(at) => look_at(world, bot, at),
        GameAction::Mount => riding::mount(world, bot),
        // As a player gets out: the sneak key, until the server has let
        // the bot off ([`tick::getting_out`]).
        GameAction::Dismount => {
            if world.get::<Held>(bot).is_none() {
                return Err(SessionError::NotInWorld);
            }
            if world.get::<Riding>(bot).is_some() {
                world.entity_mut(bot).insert(GettingOut {
                    ticks_left: GettingOut::MOST_TICKS,
                });
            }
            Ok(())
        }
        // A step mode's actions are `actions::perform`'s own.
        GameAction::Look { .. }
        | GameAction::Turn { .. }
        | GameAction::Jump
        | GameAction::Sneak { .. }
        | GameAction::SwingArm
        | GameAction::UseItem
        | GameAction::HoldUse { .. }
        | GameAction::AttackFacingEntity
        | GameAction::SelectHotbarSlot { .. } => Ok(()),
    }
}

/// Sends what a key that only taps does, as the vanilla client sends it:
/// a player action with the origin as its block.
fn tap_only(world: &mut World, bot: Entity, key: Key) {
    let action = match key {
        Key::Drop => PlayerAction::DropItem,
        Key::DropStack => PlayerAction::DropAllItems,
        Key::SwapOffhand => PlayerAction::SwapItemWithOffhand,
        Key::Forward
        | Key::Back
        | Key::Left
        | Key::Right
        | Key::Jump
        | Key::Sneak
        | Key::Sprint => return,
    };
    world.trigger(SendGamePacketEvent::new(
        bot,
        ServerboundPlayerAction {
            action,
            pos: BlockPos::default(),
            direction: Face::Down,
            seq: 0,
        },
    ));
}

/// Turns the bot to look at `at` from its eyes. A riding bot's eyes are a
/// seated player's, above its seat (ADR-0026).
fn look_at(world: &mut World, bot: Entity, at: Position) -> Result<(), SessionError> {
    let feet = **world.get::<Feet>(bot).ok_or(SessionError::NotInWorld)?;
    let eye_height = if world.get::<Riding>(bot).is_some() {
        calculate_dimensions(EntityKind::Player, Pose::Standing).eye_height
    } else {
        world
            .get::<EntityDimensions>(bot)
            .ok_or(SessionError::NotInWorld)?
            .eye_height
    };
    let eye = feet.up(eye_height.into());
    let point = Vec3::new(at.x(), at.y(), at.z());
    let mut look = world
        .get_mut::<LookDirection>(bot)
        .ok_or(SessionError::NotInWorld)?;
    // Eyes at the point itself have no line to it.
    if eye != point {
        look.update(direction_looking_at(eye, point));
    }
    Ok(())
}

/// Takes a snapshot of the game as the bot sees it.
///
/// # Errors
/// [`SessionError::NotInWorld`] if the bot isn't placed in a world.
pub(crate) fn observe(client: &Client) -> Result<Observation, SessionError> {
    facts::observe(&mut client.ecs.write(), client.entity)
}

/// Sets the leash's radius, the tool guard and the block guard.
///
/// # Errors
/// [`SessionError::NotInWorld`] if the bot's entity is gone.
pub(crate) fn set_guards(client: &Client, guards: Guards) -> Result<(), SessionError> {
    guard(&mut client.ecs.write(), client.entity, guards)
}

/// [`set_guards`] for `bot` in `world`.
pub(crate) fn guard(world: &mut World, bot: Entity, guards: Guards) -> Result<(), SessionError> {
    world
        .get_mut::<Guarded>(bot)
        .ok_or(SessionError::NotInWorld)?
        .0 = guards;
    Ok(())
}
