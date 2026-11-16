//! The two guards of mining: never the floor, and never a tool to its end
//! (ADR-0036, D11).
//!
//! Both are decisions only. fleet-mc applies them on the bot's host thread
//! each game tick, before azalea's own mining runs, so they hold whatever a
//! script or the runtime does.

use super::facts::{BlockPosition, HeldItem};
use super::settings::ToolGuard;

/// How far below its feet a block's bottom may be and still count as level
/// with them: enough for the rounding of a position a server sent, far less
/// than the thinnest block a bot stands on.
const LEVEL: f64 = 1e-4;

/// Whether a bot whose feet are at height `feet_y` may mine `block`: only a
/// block whose bottom is at its feet's height or above.
///
/// So it never mines the block it stands on, nor anything under it, and
/// can't dig itself into a hole or through a farm's floor.
///
/// **Stricter than "a block below the feet",** on purpose: a bot that
/// stands on a slab, on soul sand or on farmland has its feet inside that
/// block's cube, above its bottom. The block's top is then above the feet,
/// but its bottom is below them, and it isn't mined. The same holds for the
/// blocks beside it at that height, and while the bot is in the air.
///
/// A height that isn't a number allows nothing.
#[must_use]
pub fn may_mine(feet_y: f64, block: BlockPosition) -> bool {
    feet_y.is_finite() && f64::from(block.y) >= feet_y - LEVEL
}

impl ToolGuard {
    /// Whether a bot may go on mining with `held` in its hand.
    ///
    /// Not when the item wears and has only as many uses left as the guard
    /// keeps, or fewer. An empty hand and an item that doesn't wear always
    /// may; so does everything when the guard is off.
    #[must_use]
    pub fn allows(self, held: Option<&HeldItem>) -> bool {
        let left = held.and_then(|item| item.durability);
        self.is_off() || left.is_none_or(|left| left > u32::from(self.uses_left()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::Identifier;
    use proptest::prelude::*;
    use rstest::rstest;

    const fn block_at(y: i32) -> BlockPosition {
        BlockPosition { x: 3, y, z: -7 }
    }

    fn tool(durability: Option<u32>) -> HeldItem {
        HeldItem {
            id: Identifier::try_from("iron_pickaxe").unwrap(),
            count: 1,
            durability,
        }
    }

    fn guard(uses: u8) -> ToolGuard {
        ToolGuard::try_from(uses).unwrap()
    }

    /// The bot stands on the block at y = 63: its feet are at 64.
    #[rstest]
    #[case::the_block_it_stands_on(63, false)]
    #[case::the_block_under_that(62, false)]
    #[case::far_below(-64, false)]
    #[case::at_its_feet(64, true)]
    #[case::at_its_eyes(65, true)]
    #[case::above_its_head(66, true)]
    fn a_bot_on_the_ground_mines_nothing_below_its_feet(#[case] y: i32, #[case] may: bool) {
        assert_eq!(may_mine(64.0, block_at(y)), may);
    }

    /// Feet inside a block's cube: the block itself and what's beside it
    /// at that height stay.
    #[rstest]
    #[case::on_a_bottom_slab(63.5, 63, false)]
    #[case::on_a_bottom_slab_the_block_above(63.5, 64, true)]
    #[case::on_soul_sand(63.875, 63, false)]
    #[case::on_farmland(63.9375, 63, false)]
    #[case::on_a_carpet(64.0625, 64, false)]
    #[case::on_a_carpet_at_its_eyes(64.0625, 65, true)]
    #[case::in_a_jump(64.42, 64, false)]
    #[case::in_a_jump_at_its_eyes(64.42, 65, true)]
    #[case::falling_past(60.3, 61, true)]
    #[case::falling_past_the_block_its_in(60.3, 60, false)]
    fn feet_inside_a_blocks_cube_dont_free_that_block(
        #[case] feet: f64,
        #[case] y: i32,
        #[case] may: bool,
    ) {
        assert_eq!(may_mine(feet, block_at(y)), may);
    }

    #[rstest]
    #[case::a_hair_above_the_blocks_bottom(64.00001, 64, true)]
    #[case::a_hair_below(63.99999, 64, true)]
    #[case::more_than_a_hair_above(64.001, 64, false)]
    fn a_servers_rounding_of_the_height_doesnt_stop_mining_at_the_feet(
        #[case] feet: f64,
        #[case] y: i32,
        #[case] may: bool,
    ) {
        assert_eq!(may_mine(feet, block_at(y)), may);
    }

    #[rstest]
    #[case::not_a_number(f64::NAN)]
    #[case::endless(f64::INFINITY)]
    #[case::endless_below(f64::NEG_INFINITY)]
    fn a_height_that_isnt_a_number_allows_nothing(#[case] feet: f64) {
        for y in [i32::MIN, -64, 0, 64, 320, i32::MAX] {
            assert!(!may_mine(feet, block_at(y)), "{y}");
        }
    }

    #[rstest]
    #[case::plenty_left(2, Some(250), true)]
    #[case::one_more_than_kept(2, Some(3), true)]
    #[case::as_many_as_kept(2, Some(2), false)]
    #[case::fewer(2, Some(1), false)]
    #[case::none_left(2, Some(0), false)]
    #[case::an_item_that_doesnt_wear(2, None, true)]
    #[case::a_larger_guard(100, Some(100), false)]
    #[case::a_larger_guard_with_one_more(100, Some(101), true)]
    fn the_tool_guard_stops_at_the_uses_it_keeps(
        #[case] uses: u8,
        #[case] durability: Option<u32>,
        #[case] may: bool,
    ) {
        assert_eq!(guard(uses).allows(Some(&tool(durability))), may);
    }

    #[test]
    fn an_empty_hand_may_always_mine() {
        assert!(guard(2).allows(None));
        assert!(guard(100).allows(None));
    }

    #[test]
    fn a_guard_that_is_off_lets_a_tool_be_mined_to_its_end() {
        for durability in [Some(250), Some(1), Some(0), None] {
            assert!(
                ToolGuard::OFF.allows(Some(&tool(durability))),
                "{durability:?}"
            );
        }
    }

    #[test]
    fn the_guard_unless_set_keeps_two_uses() {
        assert!(ToolGuard::default().allows(Some(&tool(Some(3)))));
        assert!(!ToolGuard::default().allows(Some(&tool(Some(2)))));
    }

    proptest! {
        /// Wherever a bot's feet are: it never mines a block that reaches
        /// below them, and always may mine one that's wholly above.
        #[test]
        fn nothing_that_reaches_below_the_feet_is_ever_mined(
            feet in -64.0_f64..320.0,
            y in -70_i32..330,
        ) {
            let bottom = f64::from(y);

            if bottom < feet - 1e-3 {
                prop_assert!(!may_mine(feet, block_at(y)));
            }
            if bottom >= feet {
                prop_assert!(may_mine(feet, block_at(y)));
            }
        }

        /// A tool the guard watches is never left with fewer uses than it
        /// keeps by one more block: mining stops while more are left.
        #[test]
        fn the_guard_allows_exactly_while_more_uses_are_left_than_it_keeps(
            uses in 0_u8..=100,
            left in 0_u32..300,
        ) {
            let allowed = guard(uses).allows(Some(&tool(Some(left))));

            prop_assert_eq!(allowed, uses == 0 || left > u32::from(uses));
        }
    }
}
