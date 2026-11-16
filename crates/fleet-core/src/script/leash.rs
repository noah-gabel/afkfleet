//! [`Leash`]: how far a scripted bot walks from where it was put
//! (ADR-0036, D10).

use super::facts::Position;
use super::input::Key;
use super::settings::LeashRadius;

/// What a game tick's movement means for the leash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeashTick {
    /// The bot is within the radius.
    Inside,
    /// The bot is beyond the radius and didn't get further this tick: it's
    /// walking back, standing, or being carried sideways.
    Returning,
    /// The bot is beyond the radius and got further this tick: every
    /// movement key is let go.
    Stop,
}

/// The leash: a scripted bot walks at most a mode's radius from where it
/// was put, on the ground (ADR-0036, D10).
///
/// - **The anchor** is where the bot was put: at a join, a respawn, a
///   world change ([`put_at`](Self::put_at)), and after a server teleport
///   of more than [`Leash::TELEPORT_RESET`] blocks
///   ([`teleported`](Self::teleported)). No action of a script moves it.
/// - **The rule is about getting further, not about being outside.**
///   Beyond the radius, a tick that takes the bot further lets go of every
///   movement key ([`tick`](Self::tick)), and a key that would walk it
///   further is refused when it's pressed
///   ([`would_go_further`](Self::would_go_further)). Walking back always
///   works.
/// - **Height doesn't count:** a fall or a ladder isn't walking away.
///
/// These are the decisions only. fleet-mc applies [`tick`](Self::tick) on
/// the bot's host thread every game tick, below the script and the
/// runtime, so a stalled runtime can't let a bot walk on. It can't hold a
/// bot in place either: water, pistons and minecarts still carry it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Leash {
    radius: LeashRadius,
    anchor: Position,
}

impl Leash {
    /// A server teleport of more than this many blocks puts the bot
    /// somewhere new, and the anchor with it. A shorter one is a
    /// correction of where the bot already was.
    pub const TELEPORT_RESET: f64 = 8.0;

    /// A leash of `radius` around `at`, where the bot was put.
    #[must_use]
    pub const fn new(radius: LeashRadius, at: Position) -> Self {
        Self { radius, anchor: at }
    }

    /// The radius.
    #[must_use]
    pub const fn radius(&self) -> LeashRadius {
        self.radius
    }

    /// Where the bot was put.
    #[must_use]
    pub const fn anchor(&self) -> Position {
        self.anchor
    }

    /// The bot was put at `at`: by a join, a respawn or a world change.
    pub fn put_at(&mut self, at: Position) {
        self.anchor = at;
    }

    /// The server moved the bot from `from` to `to`. Returns whether that
    /// was far enough to count as being put somewhere new, in which case
    /// `to` is the anchor now.
    pub fn teleported(&mut self, from: Position, to: Position) -> bool {
        let moved = from.distance_to(to) > Self::TELEPORT_RESET;
        if moved {
            self.anchor = to;
        }
        moved
    }

    /// Whether `at` is beyond the radius.
    #[must_use]
    pub fn is_beyond(&self, at: Position) -> bool {
        self.anchor.horizontal_distance_to(at) > f64::from(self.radius.blocks())
    }

    /// What a game tick that moved the bot from `before` to `after` means.
    #[must_use]
    pub fn tick(&self, before: Position, after: Position) -> LeashTick {
        if !self.is_beyond(after) {
            return LeashTick::Inside;
        }
        let was = self.anchor.horizontal_distance_to(before);
        let is = self.anchor.horizontal_distance_to(after);
        if is > was + FURTHER {
            LeashTick::Stop
        } else {
            LeashTick::Returning
        }
    }

    /// Whether a bot at `at` that looks along `yaw` would walk further
    /// beyond the radius with `keys` down: what a press of a direction key
    /// is refused for (`leashed`).
    ///
    /// Only the four direction keys walk; jump and sprint alone go
    /// nowhere, and opposite keys cancel. Within the radius nothing is
    /// refused: the tick that crosses it is [`tick`](Self::tick)'s. A yaw
    /// that isn't a number is refused beyond the radius, since nothing can
    /// be said about where it leads.
    #[must_use]
    pub fn would_go_further(
        &self,
        at: Position,
        yaw: f32,
        keys: impl IntoIterator<Item = Key>,
    ) -> bool {
        // How far forward and how far to the left the keys lead.
        let (mut forward, mut left) = (0.0_f64, 0.0_f64);
        for key in keys {
            match key {
                Key::Forward => forward += 1.0,
                Key::Back => forward -= 1.0,
                Key::Left => left += 1.0,
                Key::Right => left -= 1.0,
                Key::Jump
                | Key::Sneak
                | Key::Sprint
                | Key::Drop
                | Key::DropStack
                | Key::SwapOffhand => {}
            }
        }
        if (forward == 0.0 && left == 0.0) || !self.is_beyond(at) {
            return false;
        }
        if !yaw.is_finite() {
            return true;
        }
        // Minecraft's yaw: 0 looks south (+z), 90 west (−x). Looking along
        // it, forward is (−sin, cos) on the ground and left is (cos, sin).
        let (sin, cos) = f64::from(yaw).to_radians().sin_cos();
        let (x, z) = (left * cos - forward * sin, forward * cos + left * sin);
        // Further means along the line from the anchor to the bot.
        let outward = (at.x() - self.anchor.x()) * x + (at.z() - self.anchor.z()) * z;
        outward > FURTHER
    }
}

/// How much further counts as further: more than the rounding of a
/// position's arithmetic, far less than a tick's walk.
const FURTHER: f64 = 1e-6;

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    fn at(x: f64, z: f64) -> Position {
        Position::try_new(x, 64.0, z).unwrap()
    }

    fn high(x: f64, y: f64, z: f64) -> Position {
        Position::try_new(x, y, z).unwrap()
    }

    /// A leash of 8 blocks around (0, 0).
    fn leash() -> Leash {
        Leash::new(LeashRadius::DEFAULT, at(0.0, 0.0))
    }

    /// Minecraft's directions: yaw 0 looks south (+z), 90 west (−x),
    /// 180 north (−z), −90 east (+x).
    const SOUTH: f32 = 0.0;
    const WEST: f32 = 90.0;
    const NORTH: f32 = 180.0;
    const EAST: f32 = -90.0;

    #[test]
    fn a_leash_starts_where_the_bot_was_put() {
        let leash = Leash::new(LeashRadius::try_from(16).unwrap(), at(100.0, -50.0));

        assert_eq!(leash.anchor(), at(100.0, -50.0));
        assert_eq!(leash.radius().blocks(), 16);
    }

    #[rstest]
    #[case::at_the_anchor(0.0, 0.0, false)]
    #[case::inside(5.0, 5.0, false)]
    #[case::at_the_radius(8.0, 0.0, false)]
    #[case::just_beyond(8.01, 0.0, true)]
    #[case::beyond_diagonally(6.0, 6.0, true)]
    #[case::far(-100.0, 0.0, true)]
    fn beyond_the_radius_is_further_than_it_on_the_ground(
        #[case] x: f64,
        #[case] z: f64,
        #[case] beyond: bool,
    ) {
        assert_eq!(leash().is_beyond(at(x, z)), beyond);
    }

    #[test]
    fn height_doesnt_count() {
        let leash = leash();

        assert!(!leash.is_beyond(high(7.0, 300.0, 0.0)));
        assert!(!leash.is_beyond(high(0.0, -60.0, 0.0)));
        // A fall beyond the radius isn't walking further.
        assert_eq!(
            leash.tick(high(9.0, 80.0, 0.0), high(9.0, 60.0, 0.0)),
            LeashTick::Returning
        );
    }

    #[rstest]
    #[case::walking_inside(at(1.0, 0.0), at(1.2, 0.0), LeashTick::Inside)]
    #[case::walking_out_but_still_inside(at(7.5, 0.0), at(7.9, 0.0), LeashTick::Inside)]
    #[case::reaching_the_radius(at(7.8, 0.0), at(8.0, 0.0), LeashTick::Inside)]
    #[case::crossing_it(at(7.9, 0.0), at(8.1, 0.0), LeashTick::Stop)]
    #[case::getting_further(at(9.0, 0.0), at(9.2, 0.0), LeashTick::Stop)]
    #[case::getting_further_sideways(at(9.0, 0.0), at(9.0, 2.0), LeashTick::Stop)]
    #[case::standing_beyond(at(9.0, 0.0), at(9.0, 0.0), LeashTick::Returning)]
    #[case::walking_back(at(9.0, 0.0), at(8.8, 0.0), LeashTick::Returning)]
    #[case::walking_back_inside(at(8.1, 0.0), at(7.9, 0.0), LeashTick::Inside)]
    #[case::carried_far_in_one_tick(at(0.0, 0.0), at(50.0, 0.0), LeashTick::Stop)]
    fn a_tick_that_takes_the_bot_further_beyond_the_radius_stops_it(
        #[case] before: Position,
        #[case] after: Position,
        #[case] verdict: LeashTick,
    ) {
        assert_eq!(leash().tick(before, after), verdict);
    }

    #[test]
    fn a_bot_put_somewhere_new_is_leashed_there() {
        let mut leash = leash();

        leash.put_at(at(1000.0, 1000.0));

        assert_eq!(leash.anchor(), at(1000.0, 1000.0));
        assert!(!leash.is_beyond(at(1005.0, 1000.0)));
        assert!(leash.is_beyond(at(0.0, 0.0)));
        assert_eq!(leash.radius(), LeashRadius::DEFAULT);
    }

    #[rstest]
    #[case::a_small_correction(at(3.0, 0.0), at(3.5, 0.0), false)]
    #[case::exactly_eight_blocks(at(0.0, 0.0), at(8.0, 0.0), false)]
    #[case::more_than_eight_blocks(at(0.0, 0.0), at(8.5, 0.0), true)]
    #[case::far(at(0.0, 0.0), at(5000.0, -200.0), true)]
    #[case::straight_up(high(0.0, 64.0, 0.0), high(0.0, 80.0, 0.0), true)]
    fn a_server_teleport_of_more_than_8_blocks_moves_the_anchor(
        #[case] from: Position,
        #[case] to: Position,
        #[case] moved: bool,
    ) {
        let mut leash = leash();

        assert_eq!(leash.teleported(from, to), moved);

        assert_eq!(leash.anchor(), if moved { to } else { at(0.0, 0.0) });
    }

    /// The bot stands 9 blocks east of the anchor, beyond the radius.
    #[rstest]
    #[case::forward_facing_away(EAST, &[Key::Forward], true)]
    #[case::forward_facing_the_anchor(WEST, &[Key::Forward], false)]
    #[case::back_facing_away(EAST, &[Key::Back], false)]
    #[case::back_facing_the_anchor(WEST, &[Key::Back], true)]
    #[case::left_facing_south_is_east(SOUTH, &[Key::Left], true)]
    #[case::right_facing_south_is_west(SOUTH, &[Key::Right], false)]
    #[case::left_facing_north_is_west(NORTH, &[Key::Left], false)]
    #[case::right_facing_north_is_east(NORTH, &[Key::Right], true)]
    #[case::diagonally_away(EAST, &[Key::Forward, Key::Left], true)]
    #[case::diagonally_back(WEST, &[Key::Forward, Key::Right], false)]
    #[case::opposite_keys_cancel(EAST, &[Key::Forward, Key::Back], false)]
    #[case::sprinting_away(EAST, &[Key::Sprint, Key::Forward], true)]
    #[case::jump_alone_goes_nowhere(EAST, &[Key::Jump], false)]
    #[case::sprint_alone_goes_nowhere(EAST, &[Key::Sprint], false)]
    #[case::sneak_alone_goes_nowhere(EAST, &[Key::Sneak], false)]
    #[case::no_key(EAST, &[], false)]
    #[case::a_yaw_that_isnt_a_number(f32::NAN, &[Key::Forward], true)]
    #[case::an_endless_yaw(f32::INFINITY, &[Key::Back], true)]
    #[case::a_yaw_that_isnt_a_number_without_a_direction(f32::NAN, &[Key::Jump], false)]
    fn beyond_the_radius_a_key_that_walks_further_is_refused(
        #[case] yaw: f32,
        #[case] keys: &[Key],
        #[case] refused: bool,
    ) {
        let beyond = at(9.0, 0.0);

        assert_eq!(
            leash().would_go_further(beyond, yaw, keys.iter().copied()),
            refused
        );
    }

    #[test]
    fn within_the_radius_no_key_is_refused() {
        let leash = leash();

        for yaw in [SOUTH, WEST, NORTH, EAST, f32::NAN] {
            for key in Key::ALL {
                assert!(
                    !leash.would_go_further(at(7.9, 0.0), yaw, [key]),
                    "{yaw} {key:?}"
                );
                assert!(
                    !leash.would_go_further(at(8.0, 0.0), yaw, [key]),
                    "{yaw} {key:?}"
                );
            }
        }
    }

    /// Walking along the radius' edge, neither nearer nor further, is
    /// what the tick judges; a key that leads sideways isn't refused.
    #[test]
    fn a_key_that_leads_neither_away_nor_back_isnt_refused() {
        // East of the anchor, facing south: forward is along the circle.
        assert!(!leash().would_go_further(at(9.0, 0.0), SOUTH, [Key::Forward]));
    }

    proptest! {
        /// Walking back is always allowed: beyond the radius, the key
        /// that leads straight to the anchor is never refused, and a tick
        /// that brings the bot nearer never stops it.
        #[test]
        fn walking_back_is_always_allowed(
            yaw in -180.0_f32..180.0,
            distance in 8.01_f64..200.0,
            step in 0.01_f64..5.0,
            radius in 1_u8..=64,
        ) {
            let leash = Leash::new(LeashRadius::try_from(radius).unwrap(), at(0.0, 0.0));
            let distance = distance.max(f64::from(radius) + 0.01);
            // Looking along `yaw`, forward is (−sin, cos) on the ground;
            // the bot stands `distance` behind the anchor, looking at it.
            let (sin, cos) = f64::from(yaw).to_radians().sin_cos();
            let here = at(distance * sin, -distance * cos);
            let nearer = at((distance - step) * sin, -(distance - step) * cos);

            prop_assert!(leash.is_beyond(here));
            prop_assert!(!leash.would_go_further(here, yaw, [Key::Forward]));
            prop_assert!(leash.would_go_further(here, yaw, [Key::Back]));
            prop_assert_ne!(leash.tick(here, nearer), LeashTick::Stop);
            prop_assert_eq!(leash.tick(nearer, here), LeashTick::Stop);
        }

        /// Whatever a bot does, a tick stops it exactly when it ends
        /// beyond the radius and further than it began.
        #[test]
        fn a_tick_stops_exactly_what_goes_further_beyond_the_radius(
            before in (-30.0_f64..30.0, -30.0_f64..30.0),
            after in (-30.0_f64..30.0, -30.0_f64..30.0),
            radius in 1_u8..=64,
        ) {
            let anchor = at(0.0, 0.0);
            let leash = Leash::new(LeashRadius::try_from(radius).unwrap(), anchor);
            let (before, after) = (at(before.0, before.1), at(after.0, after.1));
            let (was, is) = (
                anchor.horizontal_distance_to(before),
                anchor.horizontal_distance_to(after),
            );

            let verdict = leash.tick(before, after);

            if is <= f64::from(radius) {
                prop_assert_eq!(verdict, LeashTick::Inside);
            } else if is > was + 1e-6 {
                prop_assert_eq!(verdict, LeashTick::Stop);
            } else if is < was - 1e-6 {
                prop_assert_eq!(verdict, LeashTick::Returning);
            }
            prop_assert_eq!(verdict == LeashTick::Inside, !leash.is_beyond(after));
        }
    }
}
