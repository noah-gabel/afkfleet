//! What a seated bot looks at: vanilla's pick, from the seat's eyes
//! (ADR-0026).
//!
//! azalea's own `pick` does the same for a player on foot, but it can't be
//! called with other eyes than the bot's own position gives (its options'
//! fields are private), and its port of vanilla's `getEntityHitResult` has
//! the own vehicle's branch commented out ("TODO: don't pick the entity
//! we're riding on"). So this is that function again, from azalea's public
//! pieces, with the branch as azalea's comment gives it:
//!
//! - The nearest block on the line of sight bounds the search for entities.
//! - An entity whose box holds the eyes is picked at once.
//! - Otherwise the nearest entity the line of sight crosses is picked, but
//!   never the own vehicle: that one is only taken when something was
//!   already picked at the eyes themselves. So a passenger clicks through
//!   its minecart onto what's behind it.
//! - The nearer of block and entity wins, and one beyond its own range (a
//!   block's or an entity's) is a miss.
//!
//! Adapted from azalea-client 0.16.0's plugins/interact/pick.rs (MIT).
//! azalea's notice is in the repository's `THIRD-PARTY-NOTICES.md`.

use azalea::core::aabb::Aabb;
use azalea::core::direction::Direction;
use azalea::core::hit_result::{BlockHitResult, EntityHitResult, HitResult};
use azalea::core::position::Vec3;
use azalea::ecs::entity::Entity;
use azalea::entity::metadata::{ArmorStandMarker, InGround};
use azalea::entity::{LookDirection, view_vector};
use azalea::interact::pick::{MaybePickableEntityQuery, pick_block};
use azalea::physics::collision::entity_collisions::{AabbQuery, get_entities};
use azalea::world::World;

/// A bot in its seat.
#[derive(Debug, Clone)]
pub(super) struct Seated {
    /// The bot's entity, which is never picked.
    pub(super) bot: Entity,
    /// The entity the bot sits in.
    pub(super) vehicle: Entity,
    /// Where a seated player's eyes are.
    pub(super) eye: Vec3,
    /// Where the bot looks.
    pub(super) look: LookDirection,
    /// A seated player's box.
    pub(super) body: Aabb,
    /// How far the bot reaches for a block.
    pub(super) block_range: f64,
    /// How far the bot reaches for an entity.
    pub(super) entity_range: f64,
}

/// What `seated` looks at in `world`.
pub(super) fn pick(
    seated: &Seated,
    world: &World,
    aabb_query: &AabbQuery,
    pickable_query: &MaybePickableEntityQuery,
) -> HitResult {
    let reach = seated.block_range.max(seated.entity_range);
    let block = pick_block(seated.look, seated.eye, &world.chunks, reach);
    // Nothing behind the block it looks at can be picked.
    let reach = if block.miss {
        reach
    } else {
        block.location.distance_squared_to(seated.eye).sqrt()
    };
    let line = view_vector(seated.look) * reach;
    let is_pickable = |entity: Entity| {
        pickable_query.get(entity).is_ok_and(|(marker, in_ground)| {
            marker != Some(&ArmorStandMarker(true)) && in_ground != Some(&InGround(true))
        })
    };
    let candidates = get_entities(
        world,
        Some(seated.bot),
        &seated.body.expand_towards(line).inflate_all(1.0),
        &is_pickable,
        aabb_query,
    );
    let entity = pick_entity(
        candidates,
        seated.eye,
        seated.eye + line,
        reach * reach,
        seated.vehicle,
    );
    nearer(block, entity, seated)
}

/// The entity of `candidates` that eyes at `eye` see along the line to
/// `end`, within a squared distance of `reach_squared`; see the
/// [module docs](self) for `vehicle`'s rule. azalea's `pick_entity` with
/// that rule in place.
#[must_use]
fn pick_entity(
    candidates: impl IntoIterator<Item = (Entity, Aabb)>,
    eye: Vec3,
    end: Vec3,
    reach_squared: f64,
    vehicle: Entity,
) -> Option<EntityHitResult> {
    let mut picked = None;
    // How far the picked entity is, squared; nothing farther is picked.
    let mut nearest_squared = reach_squared;
    // Vanilla asks whether that distance is 0: whether something was picked
    // at the eyes themselves.
    let mut at_the_eyes = false;
    for (candidate, aabb) in candidates {
        let enters_at = aabb.clip(eye, end);
        if aabb.contains(eye) {
            picked = Some(EntityHitResult {
                location: enters_at.unwrap_or(eye),
                entity: candidate,
            });
            nearest_squared = 0.0;
            at_the_eyes = true;
        } else if let Some(location) = enters_at {
            let distance_squared = eye.distance_squared_to(location);
            if distance_squared < nearest_squared || at_the_eyes {
                if candidate == vehicle {
                    if at_the_eyes {
                        picked = Some(EntityHitResult {
                            location,
                            entity: candidate,
                        });
                    }
                } else {
                    picked = Some(EntityHitResult {
                        location,
                        entity: candidate,
                    });
                    nearest_squared = distance_squared;
                    // A square is never below 0.
                    at_the_eyes = distance_squared <= 0.0;
                }
            }
        }
    }
    picked
}

/// The nearer of `block` and `entity`, or a miss if it's beyond its range.
#[must_use]
fn nearer(block: BlockHitResult, entity: Option<EntityHitResult>, seated: &Seated) -> HitResult {
    let block_distance_squared = block.location.distance_squared_to(seated.eye);
    match entity {
        Some(entity)
            if entity.location.distance_squared_to(seated.eye) < block_distance_squared =>
        {
            within_range(HitResult::Entity(entity), seated.eye, seated.entity_range)
        }
        _ => within_range(HitResult::Block(block), seated.eye, seated.block_range),
    }
}

/// `hit`, or a miss at its place if that's `range` or more from `eye`.
#[must_use]
fn within_range(hit: HitResult, eye: Vec3, range: f64) -> HitResult {
    let location = hit.location();
    if location.closer_than(eye, range) {
        hit
    } else {
        let direction = Direction::nearest(location - eye);
        HitResult::new_miss(location, direction, location.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use azalea::BlockPos;
    use azalea::ecs::world::World as Ecs;

    /// Eyes at the origin of the tests' boxes, looking south (+z).
    const EYE: Vec3 = Vec3::new(0.5, 1.5, 0.5);
    const END: Vec3 = Vec3::new(0.5, 1.5, 5.0);
    const REACH_SQUARED: f64 = 4.5 * 4.5;

    /// Three entities: the vehicle, and two others.
    fn entities() -> [Entity; 3] {
        let mut ecs = Ecs::new();
        [
            ecs.spawn_empty().id(),
            ecs.spawn_empty().id(),
            ecs.spawn_empty().id(),
        ]
    }

    /// A box one block wide and high around the line of sight, from `z` on
    /// for `depth`.
    fn across(z: f64, depth: f64) -> Aabb {
        Aabb {
            min: Vec3::new(0.0, 1.0, z),
            max: Vec3::new(1.0, 2.0, z + depth),
        }
    }

    fn picked(candidates: &[(Entity, Aabb)], vehicle: Entity) -> Option<(Entity, Vec3)> {
        pick_entity(candidates.iter().copied(), EYE, END, REACH_SQUARED, vehicle)
            .map(|hit| (hit.entity, hit.location))
    }

    // --- The entity on the line of sight ---

    #[test]
    fn the_nearest_entity_on_the_line_is_picked_where_the_line_enters_it() {
        let [vehicle, near, far] = entities();

        let hit = picked(
            &[(far, across(3.0, 1.0)), (near, across(2.0, 0.5))],
            vehicle,
        );

        assert_eq!(hit, Some((near, Vec3::new(0.5, 1.5, 2.0))));
    }

    #[test]
    fn nothing_on_the_line_is_nothing_picked() {
        let [vehicle, beside, _] = entities();
        let beside_the_line = Aabb {
            min: Vec3::new(2.0, 1.0, 2.0),
            max: Vec3::new(3.0, 2.0, 3.0),
        };

        assert_eq!(picked(&[(beside, beside_the_line)], vehicle), None);
        assert_eq!(picked(&[], vehicle), None);
    }

    #[test]
    fn an_entity_beyond_the_reach_is_not_picked() {
        let [vehicle, far, _] = entities();

        let hit = pick_entity([(far, across(3.0, 1.0))], EYE, END, 2.0 * 2.0, vehicle);

        assert_eq!(hit, None);
    }

    #[test]
    fn an_entity_whose_box_holds_the_eyes_is_picked_at_the_eyes() {
        let [vehicle, around, behind] = entities();

        let hit = picked(
            &[(behind, across(2.0, 1.0)), (around, across(0.0, 1.0))],
            vehicle,
        );

        // The line leaves the box without entering it, so there's no point
        // where it enters, and vanilla takes the eyes.
        assert_eq!(hit.map(|(entity, _)| entity), Some(around));
    }

    // --- The own vehicle ---

    #[test]
    fn the_vehicle_on_the_line_is_looked_through() {
        let [vehicle, behind, _] = entities();

        let hit = picked(
            &[(vehicle, across(1.0, 0.5)), (behind, across(2.0, 1.0))],
            vehicle,
        );

        assert_eq!(hit, Some((behind, Vec3::new(0.5, 1.5, 2.0))));
    }

    #[test]
    fn the_vehicle_alone_on_the_line_is_nothing_picked() {
        let [vehicle, _, _] = entities();

        assert_eq!(picked(&[(vehicle, across(1.0, 0.5))], vehicle), None);
    }

    /// Whatever order the candidates come in.
    #[test]
    fn the_vehicle_is_looked_through_when_it_comes_after_what_is_behind_it() {
        let [vehicle, behind, _] = entities();

        let hit = picked(
            &[(behind, across(2.0, 1.0)), (vehicle, across(1.0, 0.5))],
            vehicle,
        );

        assert_eq!(hit, Some((behind, Vec3::new(0.5, 1.5, 2.0))));
    }

    #[test]
    fn a_vehicle_whose_box_holds_the_eyes_is_picked() {
        let [vehicle, _, _] = entities();

        let hit = picked(&[(vehicle, across(0.0, 1.0))], vehicle);

        assert_eq!(hit.map(|(entity, _)| entity), Some(vehicle));
    }

    /// Vanilla's rule: with something picked at the eyes, the vehicle on the
    /// line is taken after all.
    #[test]
    fn the_vehicle_on_the_line_replaces_what_was_picked_at_the_eyes() {
        let [vehicle, around, _] = entities();

        let hit = picked(
            &[(around, across(0.0, 1.0)), (vehicle, across(2.0, 1.0))],
            vehicle,
        );

        assert_eq!(hit, Some((vehicle, Vec3::new(0.5, 1.5, 2.0))));
    }

    // --- Block or entity ---

    fn seated(vehicle: Entity, bot: Entity) -> Seated {
        Seated {
            bot,
            vehicle,
            eye: EYE,
            look: LookDirection::new(0.0, 0.0),
            body: across(0.0, 1.0),
            block_range: 4.5,
            entity_range: 3.0,
        }
    }

    fn block_at(z: f64, miss: bool) -> BlockHitResult {
        let location = Vec3::new(0.5, 1.5, z);
        BlockHitResult {
            location,
            miss,
            direction: Direction::North,
            block_pos: BlockPos::from(location),
            inside: false,
            world_border: false,
        }
    }

    fn entity_at(entity: Entity, z: f64) -> EntityHitResult {
        EntityHitResult {
            location: Vec3::new(0.5, 1.5, z),
            entity,
        }
    }

    #[test]
    fn an_entity_in_front_of_the_block_wins() {
        let [vehicle, bot, pig] = entities();

        let hit = nearer(
            block_at(3.0, false),
            Some(entity_at(pig, 2.0)),
            &seated(vehicle, bot),
        );

        assert_eq!(hit, HitResult::Entity(entity_at(pig, 2.0)));
    }

    #[test]
    fn a_block_in_front_of_the_entity_wins() {
        let [vehicle, bot, pig] = entities();

        let hit = nearer(
            block_at(2.0, false),
            Some(entity_at(pig, 2.5)),
            &seated(vehicle, bot),
        );

        assert_eq!(hit, HitResult::Block(block_at(2.0, false)));
    }

    #[test]
    fn without_an_entity_the_block_is_picked() {
        let [vehicle, bot, _] = entities();

        let hit = nearer(block_at(4.0, false), None, &seated(vehicle, bot));

        assert_eq!(hit, HitResult::Block(block_at(4.0, false)));
    }

    /// An entity is reached at 3 blocks, a block at 4.5.
    #[test]
    fn an_entity_beyond_the_entity_range_is_a_miss() {
        let [vehicle, bot, pig] = entities();

        let hit = nearer(
            block_at(5.0, true),
            Some(entity_at(pig, 4.0)),
            &seated(vehicle, bot),
        );

        assert!(hit.miss(), "{hit:?}");
    }

    #[test]
    fn a_block_beyond_the_block_range_is_a_miss() {
        let [vehicle, bot, _] = entities();
        let mut seated = seated(vehicle, bot);
        seated.block_range = 2.0;

        let hit = nearer(block_at(3.0, false), None, &seated);

        assert!(hit.miss(), "{hit:?}");
    }

    #[test]
    fn a_miss_stays_a_miss() {
        let [vehicle, bot, _] = entities();

        let hit = nearer(block_at(5.0, true), None, &seated(vehicle, bot));

        assert!(hit.miss(), "{hit:?}");
    }
}
