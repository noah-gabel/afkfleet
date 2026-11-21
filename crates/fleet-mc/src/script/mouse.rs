//! The mouse buttons of a scripted mode (Plan.md P15.11; ADR-0036, D11).
//!
//! **The left button, held, is azalea's hold-to-mine** (its `LeftClickMine`,
//! what `Client::left_click_mine` sets), behind three guards that fleet-core
//! decides and [`guard_mining`] applies each game tick, **before azalea's
//! mining systems**:
//!
//! - **The mining guard:** never a block that reaches below the bot's feet.
//!   While the bot looks at one, hold-to-mine is off, so nothing starts;
//!   what azalea already queued or mines is judged by its own block and the
//!   bot's feet *now*, so a block that was level with the feet when mining
//!   began is let alone once the bot has risen. The hold only pauses.
//! - **The block guard:** a mode may list the blocks it mines (ADR-0042's
//!   amendment, G13). Any other block is held back exactly as the mining
//!   guard holds one back: nothing starts on it, what azalea queued or
//!   mines there is stopped, and the hold goes on once the crosshair is on
//!   a listed block again. Each block is judged by what the bot knows it
//!   to be *now*; one it can't name is no listed block.
//! - **The tool guard:** when the held item wears and is down to the uses
//!   the mode keeps, the button goes up, and the runtime hears of it.
//!
//! Whatever azalea queued or mines is stopped while the button is up, so
//! letting go needs nothing but the button's state.
//!
//! **The right button, held,** uses once when it goes down and then,
//! by [`hold_right`], again every 4 game ticks, as the vanilla client does
//! while the player isn't using an item. Whether the bot is using one is
//! read off the item: one that's used over time (food, a bow, a shield)
//! is one long use, started again only when the stack in hand changed,
//! which is how a second piece of food follows the first. Held "until
//! used", the button goes up at that change instead.

use azalea::BlockPos;
use azalea::core::direction::Direction as Face;
use azalea::ecs::entity::Entity;
use azalea::ecs::query::{Has, With};
use azalea::ecs::system::{Commands, Query, Res};
use azalea::ecs::world::World;
use azalea::entity::inventory::Inventory;
use azalea::entity::{LocalEntity, LookDirection, Position};
use azalea::interact::pick::HitResultComponent;
use azalea::interact::{BlockStatePredictionHandler, StartUseItemQueued};
use azalea::inventory::{ItemStack, components};
use azalea::mining::{LeftClickMine, MineProgress, Mining, MiningQueued};
use azalea::packet::game::SendGamePacketEvent;
use azalea::protocol::packets::game::s_interact::InteractionHand;
use azalea::protocol::packets::game::s_player_action::{
    Action as PlayerAction, ServerboundPlayerAction,
};
use azalea::registry::builtin::ItemKind;
use azalea::world::{WorldName, Worlds};
use fleet_core::mc::{LetGo, SessionError};
use fleet_core::script::may_mine;
use tracing::debug;

use super::facts::{block, held_item, identifier};
use super::held::{Held, RightHold};
use super::{Guarded, Reports};
use crate::actions;

/// How many game ticks lie between two uses of a held right button: the
/// vanilla client's own delay.
pub(super) const USE_EVERY_TICKS: u32 = 4;

/// What [`guard_mining`] reads and writes of a bot.
type Mined = (
    Entity,
    &'static Reports,
    &'static mut Held,
    &'static Guarded,
    &'static Position,
    &'static Inventory,
    Option<&'static WorldName>,
    Option<&'static HitResultComponent>,
    Option<&'static Mining>,
    Option<&'static MiningQueued>,
    Has<LeftClickMine>,
    Option<&'static mut MineProgress>,
);

/// Applies the three guards of mining, and the button's own state, to what
/// azalea mines; see the [module docs](self).
pub(super) fn guard_mining(
    mut commands: Commands,
    worlds: Option<Res<Worlds>>,
    mut bots: Query<Mined, With<LocalEntity>>,
) {
    // Every world the client knows; none before azalea has made the first.
    let worlds: Option<&Worlds> = worlds.map(Res::into_inner);
    for (
        bot,
        reports,
        mut held,
        guarded,
        position,
        inventory,
        world,
        hit,
        mining,
        queued,
        auto,
        progress,
    ) in &mut bots
    {
        if held.left && !guarded.0.tool_guard.allows(held_item(inventory).as_ref()) {
            debug!("the held tool is down to the uses its guard keeps; mining stops");
            held.left = false;
            reports.0.let_go(LetGo::ToolGuard);
        }
        let down = held.left;
        let feet = position.y;
        let block_guard = &guarded.0.block_guard;
        // The bot's own picture of its world, read only while a mode that
        // lists blocks holds the button: at most three lookups a tick.
        let chunks = (down && !block_guard.is_off())
            .then(|| world.zip(worlds))
            .flatten()
            .and_then(|(name, worlds)| worlds.get(name));
        let chunks = chunks.as_ref().map(|world| world.read());
        let listed = |at: BlockPos| {
            block_guard.is_off() || {
                let kind = chunks
                    .as_ref()
                    .and_then(|chunks| chunks.get_block_state(at))
                    .map(|state| identifier(&state.as_block_kind().to_string()));
                block_guard.allows(kind.as_ref())
            }
        };
        let allowed = |at: BlockPos| may_mine(feet, block(at)) && listed(at);
        // What azalea is about to mine, and what it mines.
        if queued.is_some_and(|queued| !down || !allowed(queued.position)) {
            commands.entity(bot).remove::<MiningQueued>();
        }
        if let Some(mining) = mining
            && (!down || !allowed(mining.pos))
        {
            commands.trigger(SendGamePacketEvent::new(
                bot,
                ServerboundPlayerAction {
                    action: PlayerAction::AbortDestroyBlock,
                    pos: mining.pos,
                    direction: Face::Down,
                    seq: 0,
                },
            ));
            commands.entity(bot).remove::<Mining>();
            if let Some(mut progress) = progress {
                **progress = 0.0;
            }
        }
        // And whether it may start on what the bot looks at. Looking at
        // nothing is azalea's to handle: it stops by itself.
        let target = hit
            .and_then(|hit| hit.as_block_hit_result_if_not_miss())
            .map(|hit| hit.block_pos);
        let mine = down && target.is_none_or(allowed);
        match (mine, auto) {
            (true, false) => {
                commands.entity(bot).insert(LeftClickMine);
            }
            (false, true) => {
                commands.entity(bot).remove::<LeftClickMine>();
            }
            _ => {}
        }
    }
}

/// Whether `stack` is used over time: a use of it goes on for as long as
/// the button is held.
///
/// Food, potions and everything else that's consumed, and what blocks
/// attacks, say so in their components. The rest is Minecraft's own list
/// of items with a use duration.
#[must_use]
pub(super) fn used_over_time(stack: &ItemStack) -> bool {
    stack.get_component::<components::Consumable>().is_some()
        || stack.get_component::<components::BlocksAttacks>().is_some()
        || matches!(
            stack.kind(),
            ItemKind::Bow
                | ItemKind::Crossbow
                | ItemKind::Trident
                | ItemKind::Spyglass
                | ItemKind::GoatHorn
                | ItemKind::Brush
        )
}

/// Keeps a held right button going: the next use every
/// [`USE_EVERY_TICKS`] game ticks, or the button's end; see the
/// [module docs](self). It runs before azalea sends the tick's use.
pub(super) fn hold_right(
    mut commands: Commands,
    mut bots: Query<(Entity, &Reports, &mut Held, &Inventory), With<LocalEntity>>,
) {
    for (bot, reports, mut held, inventory) in &mut bots {
        let Some(hold) = held.right.as_mut() else {
            continue;
        };
        hold.since_use = hold.since_use.saturating_add(1);
        let now = inventory.held_item();
        let changed = *now != hold.used;
        if changed && hold.until_used {
            debug!("the item the right button was held for is used up");
            held.right = None;
            commands.entity(bot).remove::<StartUseItemQueued>();
            commands.trigger(SendGamePacketEvent::new(
                bot,
                actions::release_use_item_packet(),
            ));
            reports.0.let_go(LetGo::ItemUsed);
            continue;
        }
        // The use of the press itself goes out in the first tick after
        // it, so the next is due when four more have passed.
        if hold.since_use > USE_EVERY_TICKS && (changed || !used_over_time(now)) {
            commands.entity(bot).insert(StartUseItemQueued {
                hand: InteractionHand::MainHand,
                force_block: None,
            });
            hold.since_use = 1;
            hold.used = now.clone();
        }
    }
}

/// Queues one use for azalea's next game tick, as a tap of the right
/// button does.
///
/// # Errors
/// [`SessionError::NotInWorld`] if the bot lacks what azalea's use handler
/// needs: without it the use would stay queued.
pub(super) fn use_once(world: &mut World, bot: Entity) -> Result<(), SessionError> {
    let ready = world.get::<HitResultComponent>(bot).is_some()
        && world.get::<LookDirection>(bot).is_some()
        && world.get::<BlockStatePredictionHandler>(bot).is_some();
    if !ready {
        return Err(SessionError::NotInWorld);
    }
    actions::queue_use(world, bot)
}

/// Puts the right button down: one use now, and [`hold_right`] from the
/// next game tick on.
///
/// # Errors
/// [`SessionError::NotInWorld`] as for [`use_once`], or without an
/// inventory.
pub(super) fn press_right(
    world: &mut World,
    bot: Entity,
    until_used: bool,
) -> Result<(), SessionError> {
    let used = world
        .get::<Inventory>(bot)
        .ok_or(SessionError::NotInWorld)?
        .held_item()
        .clone();
    use_once(world, bot)?;
    world
        .get_mut::<Held>(bot)
        .ok_or(SessionError::NotInWorld)?
        .right = Some(RightHold {
        until_used,
        since_use: 0,
        used,
    });
    Ok(())
}

/// Lets go of the use button on the wire: a use that's still queued never
/// reached the server and is cancelled, then the release goes out, which a
/// server ignores for a bot that uses nothing (ADR-0011).
///
/// # Errors
/// [`SessionError::NotInWorld`] if the bot's entity is gone.
pub(super) fn let_go_of_use(world: &mut World, bot: Entity) -> Result<(), SessionError> {
    actions::cancel_queued_use(world, bot)?;
    world.trigger(SendGamePacketEvent::new(
        bot,
        actions::release_use_item_packet(),
    ));
    Ok(())
}
