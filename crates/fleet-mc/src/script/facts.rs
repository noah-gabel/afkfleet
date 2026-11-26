//! What a hook sees: the snapshot, read on the host thread (Plan.md P15.13;
//! ADR-0036, D12).
//!
//! [`observe`] reads the bot's own state, the entities within a snapshot's
//! range and the blocks around the bot out of azalea's picture of the
//! world, into fleet-core's types. **Everything a server controls arrives
//! as a registry identifier or a number:** an entity's, a block's and an
//! item's kind are azalea's own names of kinds it knows; a dimension's name
//! is the server's, so one that is no identifier becomes
//! `minecraft:unknown`. No name, lore or sign text is read.
//!
//! It's a job on the session's host thread, taken right before a hook: a
//! few hundred lookups, no IO.

use std::collections::HashMap;

use azalea::BlockPos;
use azalea::core::entity_id::MinecraftEntityId;
use azalea::core::hit_result::HitResult;
use azalea::ecs::entity::Entity;
use azalea::ecs::query::Without;
use azalea::ecs::world::World;
use azalea::entity::indexing::EntityIdIndex;
use azalea::entity::inventory::Inventory;
use azalea::entity::metadata::Health;
use azalea::entity::{EntityKindComponent, LocalEntity, LookDirection, Position};
use azalea::interact::pick::HitResultComponent;
use azalea::inventory::{ItemStackData, components};
use azalea::local_player::Hunger;
use azalea::registry::builtin::BlockKind;
use azalea::world::{WorldName, Worlds};
use fleet_core::mc::{Observation, SessionError};
use fleet_core::mode::HotbarSlot;
use fleet_core::script::{
    BlockPosition, Blocks, BotFacts, EntityFact, HeldItem, Identifier, LookingAt, Snapshot,
};

use super::tick::feet;
use super::{Arriving, Placement};
use crate::riding::{Passengers, Riding};

/// What stands for a kind or a world that has no identifier here.
const UNKNOWN: &str = "minecraft:unknown";

/// `name` as an identifier, or [`UNKNOWN`] if it isn't one.
pub(crate) fn identifier(name: &str) -> Identifier {
    Identifier::try_from(name)
        .or_else(|_| Identifier::try_from(UNKNOWN))
        // `UNKNOWN` is one; this keeps the function total without a panic.
        .unwrap_or_else(|_| unreachable_identifier())
}

/// Never reached: [`UNKNOWN`] is an identifier, which a test holds.
fn unreachable_identifier() -> Identifier {
    Identifier::try_from("a").unwrap_or_else(|_| unreachable_identifier())
}

/// The item in the bot's hand as a script reads it; `None` for an empty
/// hand.
#[must_use]
pub(crate) fn held_item(inventory: &Inventory) -> Option<HeldItem> {
    let stack = inventory.held_item().as_present()?;
    Some(HeldItem {
        // azalea's own name of a kind it knows: always an identifier.
        id: Identifier::try_from(stack.kind.to_string().as_str()).ok()?,
        count: u32::try_from(stack.count).ok()?,
        durability: uses_left(stack),
    })
}

/// How many uses `stack` has left; `None` for an item that doesn't wear.
#[must_use]
pub(crate) fn uses_left(stack: &ItemStackData) -> Option<u32> {
    if stack.get_component::<components::Unbreakable>().is_some() {
        return None;
    }
    let most = stack.get_component::<components::MaxDamage>()?.amount;
    let worn = stack
        .get_component::<components::Damage>()
        .map_or(0, |damage| damage.amount);
    Some(u32::try_from(most.saturating_sub(worn)).unwrap_or(0))
}

/// The block `at` in fleet-core's coordinates.
pub(crate) const fn block(at: BlockPos) -> BlockPosition {
    BlockPosition {
        x: at.x,
        y: at.y,
        z: at.z,
    }
}

/// Takes a snapshot of the game as `bot` sees it.
///
/// # Errors
/// [`SessionError::NotInWorld`] if the bot isn't placed in a world: its
/// entity is gone, it's between worlds, or it lacks a part of what a
/// snapshot holds.
pub(crate) fn observe(world: &mut World, bot: Entity) -> Result<Observation, SessionError> {
    let gone = SessionError::NotInWorld;
    let placement = *world.get::<Placement>(bot).ok_or(gone)?;
    if placement.arriving != Arriving::Placed {
        return Err(gone);
    }
    let position = world.get::<Position>(bot).and_then(feet).ok_or(gone)?;
    let entities = entities_near(world, bot, position);

    let look = *world.get::<LookDirection>(bot).ok_or(gone)?;
    let inventory = world.get::<Inventory>(bot).ok_or(gone)?;
    let index = world.get::<EntityIdIndex>(bot);
    // The kind of an entity the bot knows by the server's ID.
    let kind_of = |entity: Entity| {
        world
            .get::<EntityKindComponent>(entity)
            .map(|kind| identifier(&kind.to_string()))
    };
    let riding = world.get::<Riding>(bot).map(|riding| {
        index
            .and_then(|index| index.get_by_minecraft_entity(riding.vehicle))
            .and_then(kind_of)
            .unwrap_or_else(|| identifier(UNKNOWN))
    });
    let name = world.get::<WorldName>(bot);
    let chunks = name
        .zip(world.get_resource::<Worlds>())
        .and_then(|(name, worlds)| worlds.get(name));
    let chunks = chunks.as_ref().map(|world| world.read());
    // Each kind's name is made once a snapshot, however many blocks of it.
    let mut names: HashMap<BlockKind, Identifier> = HashMap::new();
    let mut block_at = |at: BlockPosition| {
        let state = chunks
            .as_ref()?
            .get_block_state(BlockPos::new(at.x, at.y, at.z))?;
        let kind = state.as_block_kind();
        Some(
            names
                .entry(kind)
                .or_insert_with(|| identifier(&kind.to_string()))
                .clone(),
        )
    };
    let looking_at = match world.get::<HitResultComponent>(bot).map(|hit| &**hit) {
        Some(HitResult::Block(hit)) if !hit.miss => {
            let at = block(hit.block_pos);
            block_at(at).map(|id| LookingAt::Block { id, at })
        }
        Some(HitResult::Entity(hit)) => kind_of(hit.entity)
            .zip(world.get::<Position>(hit.entity).and_then(feet))
            .map(|(kind, at)| LookingAt::Entity { kind, at }),
        Some(HitResult::Block(_)) | None => None,
    };
    let center = BlockPosition {
        x: floor(position.x()),
        y: floor(position.y()),
        z: floor(position.z()),
    };
    let blocks = Blocks::read(center, &mut block_at);

    let facts = BotFacts {
        health: world.get::<Health>(bot).map_or(0.0, |health| **health),
        // A bot azalea has no hunger for yet is as full as one that joined.
        food: world
            .get::<Hunger>(bot)
            .map_or(20, |hunger| u8::try_from(hunger.food.min(20)).unwrap_or(20)),
        position,
        yaw: look.y_rot(),
        pitch: look.x_rot(),
        dimension: name.map_or_else(|| identifier(UNKNOWN), |name| identifier(&name.to_string())),
        riding,
        held: held_item(inventory),
        selected: HotbarSlot::try_from(inventory.selected_hotbar_slot).unwrap_or(HotbarSlot::FIRST),
        looking_at,
    };
    let anchor = placement.anchor;
    Ok(Observation {
        facts: Snapshot::new(facts, entities, blocks),
        anchor,
        menu: crate::menu::view(world, bot),
    })
}

/// The block a coordinate is in. A position holds only numbers, and one
/// past the largest block is the largest block.
pub(crate) fn floor(coordinate: f64) -> i32 {
    let block = coordinate.floor();
    if block >= f64::from(i32::MAX) {
        i32::MAX
    } else if block <= f64::from(i32::MIN) {
        i32::MIN
    } else {
        // Whole and within `i32`: the conversion is exact.
        format!("{block:.0}").parse().unwrap_or(0)
    }
}

/// The entities within a snapshot's range of `from`, each with whether
/// someone sits in it. The bot itself isn't among them.
fn entities_near(
    world: &mut World,
    bot: Entity,
    from: fleet_core::script::Position,
) -> Vec<EntityFact> {
    let mut others = world.query_filtered::<(
        &Position,
        &EntityKindComponent,
        Option<&MinecraftEntityId>,
    ), Without<LocalEntity>>();
    let passengers = world.get::<Passengers>(bot);
    others
        .iter(world)
        .filter_map(|(position, kind, id)| {
            let position = feet(position)?;
            // Before a kind's name is made: most entities are far away.
            (from.distance_to(position) <= Snapshot::ENTITY_RANGE).then(|| EntityFact {
                kind: identifier(&kind.to_string()),
                position,
                occupied: id
                    .zip(passengers)
                    .is_some_and(|(id, passengers)| passengers.is_occupied(*id)),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_for_what_has_none_is_an_identifier_itself() {
        assert_eq!(identifier(UNKNOWN).as_str(), UNKNOWN);
        assert_eq!(identifier("Not A Name!").as_str(), UNKNOWN);
        assert_eq!(identifier("stone").as_str(), "minecraft:stone");
        assert_eq!(identifier("lobby:hub_1").as_str(), "lobby:hub_1");
    }

    #[test]
    fn a_coordinate_is_in_the_block_below_or_at_it() {
        assert_eq!(floor(0.5), 0);
        assert_eq!(floor(-0.5), -1);
        assert_eq!(floor(64.0), 64);
        assert_eq!(floor(-64.0), -64);
        assert_eq!(floor(1e300), i32::MAX);
        assert_eq!(floor(-1e300), i32::MIN);
    }
}
