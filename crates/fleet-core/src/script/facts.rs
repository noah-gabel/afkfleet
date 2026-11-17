//! [`Snapshot`]: what a hook sees of the game, taken right before it runs
//! (ADR-0036, D12).

use core::fmt;
use core::str::FromStr;

use crate::mode::HotbarSlot;

/// Why text isn't an [`Identifier`]. It never holds the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IdentifierError {
    /// The text is empty, or its namespace or its path is.
    #[error("the identifier is empty")]
    Empty,
    /// The text is longer than [`Identifier::MAX_LEN`] characters.
    #[error("the identifier is {chars} characters long; the limit is {max}", max = Identifier::MAX_LEN)]
    TooLong {
        /// Its length in characters.
        chars: usize,
    },
    /// A character a registry identifier never has: anything but `a`–`z`,
    /// `0`–`9`, `_`, `-`, `.`, in the path also `/`, and the one `:`.
    #[error("the identifier holds a character it may not, at position {index}")]
    InvalidChar {
        /// The character's position, counted in characters.
        index: usize,
    },
}

/// A registry identifier, as Minecraft names a block, an item, an entity
/// kind or a dimension: `minecraft:iron_pickaxe`.
///
/// **Everything a server controls reaches a Phase 15 script as one of these
/// or as a number, never as free text** (ADR-0036, D12). The characters are
/// Minecraft's own rule: a namespace of `a`–`z`, `0`–`9`, `_`, `-` and `.`,
/// and a path of those and `/`. Text without a namespace is in `minecraft`,
/// as in vanilla, so a script writes `"minecart"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Identifier(String);

impl Identifier {
    /// The longest identifier, in characters, namespace and colon included.
    pub const MAX_LEN: usize = 128;
    /// The namespace of text that names none.
    pub const DEFAULT_NAMESPACE: &'static str = "minecraft";

    /// The identifier, always with its namespace: `minecraft:minecart`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The part before the colon.
    #[must_use]
    pub fn namespace(&self) -> &str {
        self.parts().0
    }

    /// The part after the colon.
    #[must_use]
    pub fn path(&self) -> &str {
        self.parts().1
    }

    /// The namespace and the path. Every identifier holds its one colon.
    fn parts(&self) -> (&str, &str) {
        self.0.split_once(':').unwrap_or((&self.0, ""))
    }
}

impl TryFrom<&str> for Identifier {
    type Error = IdentifierError;

    /// # Errors
    /// [`IdentifierError::Empty`], then [`IdentifierError::TooLong`] for
    /// the whole identifier, then [`IdentifierError::InvalidChar`] for the
    /// first such character; an empty namespace or path is
    /// [`IdentifierError::Empty`] too.
    fn try_from(text: &str) -> Result<Self, IdentifierError> {
        if text.is_empty() {
            return Err(IdentifierError::Empty);
        }
        // Up to the first colon is the namespace, if there is a colon.
        let colon = text.chars().position(|c| c == ':');
        // The length is the whole identifier's, with the namespace that
        // text without one gets.
        let added = if colon.is_some() {
            0
        } else {
            Self::DEFAULT_NAMESPACE.len() + 1
        };
        let chars = text.chars().count() + added;
        if chars > Self::MAX_LEN {
            return Err(IdentifierError::TooLong { chars });
        }
        let common =
            |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.');
        let invalid = text.chars().enumerate().position(|(index, c)| match colon {
            Some(colon) if index < colon => !common(c),
            Some(colon) if index == colon => false,
            _ => !(common(c) || c == '/'),
        });
        if let Some(index) = invalid {
            return Err(IdentifierError::InvalidChar { index });
        }
        let (namespace, path) = text
            .split_once(':')
            .unwrap_or((Self::DEFAULT_NAMESPACE, text));
        if namespace.is_empty() || path.is_empty() {
            return Err(IdentifierError::Empty);
        }
        Ok(Self(format!("{namespace}:{path}")))
    }
}

impl FromStr for Identifier {
    type Err = IdentifierError;

    fn from_str(text: &str) -> Result<Self, IdentifierError> {
        Self::try_from(text)
    }
}

impl fmt::Display for Identifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A point in a world isn't made of numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a position's coordinates must be numbers")]
pub struct PositionError;

/// A point in a world, in blocks. Its coordinates are numbers: never NaN
/// and never infinite, so every distance from it is a number too.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    x: f64,
    y: f64,
    z: f64,
}

impl Position {
    /// A position at these coordinates.
    ///
    /// # Errors
    /// [`PositionError`] if one isn't a finite number.
    pub fn try_new(x: f64, y: f64, z: f64) -> Result<Self, PositionError> {
        if x.is_finite() && y.is_finite() && z.is_finite() {
            Ok(Self { x, y, z })
        } else {
            Err(PositionError)
        }
    }

    /// East (positive) and west.
    #[must_use]
    pub const fn x(self) -> f64 {
        self.x
    }

    /// Up (positive) and down. For a bot, where its feet are.
    #[must_use]
    pub const fn y(self) -> f64 {
        self.y
    }

    /// South (positive) and north.
    #[must_use]
    pub const fn z(self) -> f64 {
        self.z
    }

    /// The distance to `other`, in blocks.
    #[must_use]
    pub fn distance_to(self, other: Self) -> f64 {
        self.horizontal_distance_to(other).hypot(other.y - self.y)
    }

    /// The distance to `other` on the ground, in blocks: height doesn't
    /// count.
    #[must_use]
    pub fn horizontal_distance_to(self, other: Self) -> f64 {
        (other.x - self.x).hypot(other.z - self.z)
    }
}

/// A block's place in a world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockPosition {
    /// East (positive) and west.
    pub x: i32,
    /// Up (positive) and down.
    pub y: i32,
    /// South (positive) and north.
    pub z: i32,
}

/// The item in a bot's hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldItem {
    /// What it is.
    pub id: Identifier,
    /// How many of it.
    pub count: u32,
    /// How many uses it has left; `None` for an item that doesn't wear.
    pub durability: Option<u32>,
}

/// What a bot's crosshair is on, within reach.
#[derive(Debug, Clone, PartialEq)]
pub enum LookingAt {
    /// A block.
    Block {
        /// What it is.
        id: Identifier,
        /// Where it is.
        at: BlockPosition,
    },
    /// An entity.
    Entity {
        /// What kind it is.
        kind: Identifier,
        /// Where it is.
        at: Position,
    },
}

/// What a script knows about its own bot.
#[derive(Debug, Clone, PartialEq)]
pub struct BotFacts {
    /// Health, 0 to 20.
    pub health: f32,
    /// Food, 0 to 20.
    pub food: u8,
    /// Where its feet are.
    pub position: Position,
    /// Where it looks, left and right, in degrees as the debug screen
    /// shows them.
    pub yaw: f32,
    /// Where it looks, up (negative) and down.
    pub pitch: f32,
    /// The world it's in, such as `minecraft:overworld`.
    pub dimension: Identifier,
    /// The kind of vehicle it sits in, if it rides.
    pub riding: Option<Identifier>,
    /// The item in its hand; `None` for an empty hand.
    pub held: Option<HeldItem>,
    /// The selected hotbar slot.
    pub selected: HotbarSlot,
    /// What its crosshair is on, within reach.
    pub looking_at: Option<LookingAt>,
}

/// An entity near the bot.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityFact {
    /// What kind it is.
    pub kind: Identifier,
    /// Where it is.
    pub position: Position,
    /// Whether someone sits in it. Always `false` for what isn't a vehicle.
    pub occupied: bool,
}

/// The blocks around a bot: every block within [`Blocks::RADIUS`] of the
/// one its feet are in, a cube of 9 by 9 by 9.
///
/// Each kind of block is held once, so a snapshot of a bot in a stone
/// tunnel is a few identifiers and 729 small numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocks {
    center: BlockPosition,
    /// Each kind of block in the cube, once.
    kinds: Vec<Identifier>,
    /// For each place in the cube, in a fixed order, its kind's place in
    /// `kinds`; `None` for a place the reader didn't know.
    cells: Vec<Option<u16>>,
}

impl Blocks {
    /// How far the cube reaches from its centre in each direction, in
    /// blocks.
    pub const RADIUS: i32 = 4;

    /// Reads the cube around `center`, asking `block_at` for each of its
    /// 729 places. `None` is a place that isn't known: not loaded, or
    /// outside the world.
    #[must_use]
    pub fn read(
        center: BlockPosition,
        mut block_at: impl FnMut(BlockPosition) -> Option<Identifier>,
    ) -> Self {
        let mut kinds: Vec<Identifier> = Vec::new();
        let mut cells = Vec::with_capacity(Self::CELLS);
        for dx in -Self::RADIUS..=Self::RADIUS {
            for dy in -Self::RADIUS..=Self::RADIUS {
                for dz in -Self::RADIUS..=Self::RADIUS {
                    // A place past the largest coordinate doesn't exist.
                    let place = center
                        .x
                        .checked_add(dx)
                        .zip(center.y.checked_add(dy))
                        .zip(center.z.checked_add(dz))
                        .map(|((x, y), z)| BlockPosition { x, y, z });
                    let kind = place.and_then(&mut block_at).and_then(|kind| {
                        let known = kinds.iter().position(|known| *known == kind);
                        let at = known.unwrap_or_else(|| {
                            kinds.push(kind);
                            kinds.len() - 1
                        });
                        // At most 729 kinds: one for each place.
                        u16::try_from(at).ok()
                    });
                    cells.push(kind);
                }
            }
        }
        Self {
            center,
            kinds,
            cells,
        }
    }

    /// How many places the cube has along one side.
    const SIDE: i64 = 2 * Self::RADIUS as i64 + 1;
    /// How many places the cube has.
    const CELLS: usize = 729;

    /// Where `at`'s cell is in `cells`, if `at` is in the cube: the
    /// order `read` fills them in.
    fn cell_of(&self, at: BlockPosition) -> Option<usize> {
        let radius = i64::from(Self::RADIUS);
        let offset = |place: i32, center: i32| {
            let offset = i64::from(place) - i64::from(center);
            (-radius..=radius)
                .contains(&offset)
                .then_some(offset + radius)
        };
        let dx = offset(at.x, self.center.x)?;
        let dy = offset(at.y, self.center.y)?;
        let dz = offset(at.z, self.center.z)?;
        usize::try_from((dx * Self::SIDE + dy) * Self::SIDE + dz).ok()
    }

    /// The block the cube is around.
    #[must_use]
    pub const fn center(&self) -> BlockPosition {
        self.center
    }

    /// The block at `at`; `None` further than [`Blocks::RADIUS`] from the
    /// centre in any direction, or where the reader knew none.
    #[must_use]
    pub fn get(&self, at: BlockPosition) -> Option<&Identifier> {
        let kind = (*self.cells.get(self.cell_of(at)?)?)?;
        self.kinds.get(usize::from(kind))
    }

    /// How many different kinds of block the cube holds.
    #[must_use]
    pub fn kinds(&self) -> usize {
        self.kinds.len()
    }
}

/// What a hook sees of the game: taken on the bot's host thread right
/// before the hook runs, and unchanged while it does (ADR-0036, D1, D12).
/// What the hook's own actions change shows in the next one.
///
/// It's bounded whatever a server sends: at most
/// [`Snapshot::MAX_ENTITIES`] entities within
/// [`Snapshot::ENTITY_RANGE`] blocks, nearest first, and the blocks within
/// [`Blocks::RADIUS`].
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    bot: BotFacts,
    entities: Vec<EntityFact>,
    blocks: Blocks,
}

impl Snapshot {
    /// The most entities a snapshot holds.
    pub const MAX_ENTITIES: usize = 64;
    /// How far from the bot an entity is still in the snapshot, in blocks.
    pub const ENTITY_RANGE: f64 = 16.0;

    /// A snapshot of `bot`, the entities among `entities` that are within
    /// range, nearest first and at most [`Snapshot::MAX_ENTITIES`], and
    /// `blocks`. Two entities at the same distance keep the order they
    /// came in.
    #[must_use]
    pub fn new(
        bot: BotFacts,
        entities: impl IntoIterator<Item = EntityFact>,
        blocks: Blocks,
    ) -> Self {
        let from = bot.position;
        let mut entities: Vec<EntityFact> = entities
            .into_iter()
            .filter(|entity| from.distance_to(entity.position) <= Self::ENTITY_RANGE)
            .collect();
        // A stable sort: two at one distance keep the order they came in.
        entities.sort_by(|one, other| {
            from.distance_to(one.position)
                .total_cmp(&from.distance_to(other.position))
        });
        entities.truncate(Self::MAX_ENTITIES);
        Self {
            bot,
            entities,
            blocks,
        }
    }

    /// The bot.
    #[must_use]
    pub const fn bot(&self) -> &BotFacts {
        &self.bot
    }

    /// The entities near the bot, nearest first.
    #[must_use]
    pub fn entities(&self) -> &[EntityFact] {
        &self.entities
    }

    /// The blocks around the bot.
    #[must_use]
    pub const fn blocks(&self) -> &Blocks {
        &self.blocks
    }

    /// The nearest entity of `kind`.
    #[must_use]
    pub fn nearest(&self, kind: &Identifier) -> Option<&EntityFact> {
        self.entities.iter().find(|entity| entity.kind == *kind)
    }

    /// The nearest entity of `kind` that nobody sits in.
    #[must_use]
    pub fn nearest_free(&self, kind: &Identifier) -> Option<&EntityFact> {
        self.entities
            .iter()
            .find(|entity| entity.kind == *kind && !entity.occupied)
    }

    /// How far `entity` is from the bot, in blocks.
    #[must_use]
    pub fn distance_to(&self, entity: &EntityFact) -> f64 {
        self.bot.position.distance_to(entity.position)
    }
}

/// A bot standing at `position` with nothing in its hand, for the tests of
/// this crate.
#[cfg(test)]
pub(crate) fn bot_at(position: Position) -> BotFacts {
    BotFacts {
        health: 20.0,
        food: 20,
        position,
        yaw: 0.0,
        pitch: 0.0,
        dimension: Identifier::try_from("overworld").unwrap(),
        riding: None,
        held: None,
        selected: HotbarSlot::FIRST,
        looking_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    fn id(text: &str) -> Identifier {
        Identifier::try_from(text).unwrap()
    }

    fn at(x: f64, y: f64, z: f64) -> Position {
        Position::try_new(x, y, z).unwrap()
    }

    const fn block(x: i32, y: i32, z: i32) -> BlockPosition {
        BlockPosition { x, y, z }
    }

    fn entity(kind: &str, x: f64, occupied: bool) -> EntityFact {
        EntityFact {
            kind: id(kind),
            position: at(x, 64.0, 0.0),
            occupied,
        }
    }

    // --- Identifiers ---

    #[rstest]
    #[case::with_its_namespace("minecraft:iron_pickaxe", "minecraft", "iron_pickaxe")]
    #[case::without_one("minecart", "minecraft", "minecart")]
    #[case::another_namespace("my_mod:machines/crusher.v2", "my_mod", "machines/crusher.v2")]
    #[case::dots_and_hyphens("a.b-c:d-e.f/g_1", "a.b-c", "d-e.f/g_1")]
    fn reads_an_identifier(#[case] text: &str, #[case] namespace: &str, #[case] path: &str) {
        let identifier = id(text);

        assert_eq!(identifier.namespace(), namespace);
        assert_eq!(identifier.path(), path);
        assert_eq!(identifier.as_str(), format!("{namespace}:{path}"));
        assert_eq!(identifier.to_string(), identifier.as_str());
        assert_eq!(text.parse::<Identifier>(), Ok(identifier));
    }

    #[test]
    fn a_name_without_a_namespace_is_minecrafts() {
        assert_eq!(id("minecart"), id("minecraft:minecart"));
        assert_ne!(id("minecart"), id("other:minecart"));
    }

    #[rstest]
    #[case::nothing("", IdentifierError::Empty)]
    #[case::no_namespace(":stone", IdentifierError::Empty)]
    #[case::no_path("minecraft:", IdentifierError::Empty)]
    #[case::only_a_colon(":", IdentifierError::Empty)]
    #[case::upper_case("Minecraft:stone", IdentifierError::InvalidChar { index: 0 })]
    #[case::upper_case_in_the_path("minecraft:Stone", IdentifierError::InvalidChar { index: 10 })]
    #[case::a_space("iron pickaxe", IdentifierError::InvalidChar { index: 4 })]
    #[case::a_second_colon("a:b:c", IdentifierError::InvalidChar { index: 3 })]
    #[case::a_slash_in_the_namespace("a/b:c", IdentifierError::InvalidChar { index: 1 })]
    #[case::a_section_sign("\u{a7}cstone", IdentifierError::InvalidChar { index: 0 })]
    #[case::a_line_break("stone\n", IdentifierError::InvalidChar { index: 5 })]
    #[case::an_umlaut("gr\u{fc}n", IdentifierError::InvalidChar { index: 2 })]
    fn refuses_what_no_registry_names(#[case] text: &str, #[case] error: IdentifierError) {
        assert_eq!(Identifier::try_from(text), Err(error));
    }

    #[test]
    fn an_identifier_is_at_most_128_characters() {
        let longest = format!("a:{}", "b".repeat(126));
        assert!(Identifier::try_from(longest.as_str()).is_ok());

        let longer = format!("a:{}", "b".repeat(127));
        assert_eq!(
            Identifier::try_from(longer.as_str()),
            Err(IdentifierError::TooLong { chars: 129 })
        );
        // The length is the whole identifier's, its namespace included.
        assert!(Identifier::try_from("b".repeat(118).as_str()).is_ok());
        assert_eq!(
            Identifier::try_from("b".repeat(119).as_str()),
            Err(IdentifierError::TooLong { chars: 129 })
        );
    }

    // --- Positions ---

    #[test]
    fn a_position_keeps_its_coordinates() {
        let position = at(1.5, 64.0, -3.25);

        assert_eq!(
            (position.x(), position.y(), position.z()),
            (1.5, 64.0, -3.25)
        );
    }

    #[rstest]
    #[case::not_a_number(f64::NAN, 0.0, 0.0)]
    #[case::endless(0.0, f64::INFINITY, 0.0)]
    #[case::endless_below(0.0, 0.0, f64::NEG_INFINITY)]
    fn a_position_is_made_of_numbers(#[case] x: f64, #[case] y: f64, #[case] z: f64) {
        assert_eq!(Position::try_new(x, y, z), Err(PositionError));
    }

    #[test]
    fn distances_are_in_blocks_and_the_ground_one_ignores_height() {
        let here = at(0.0, 64.0, 0.0);
        let there = at(3.0, 76.0, 4.0);

        assert!((here.distance_to(there) - 13.0).abs() < 1e-12);
        assert!((here.horizontal_distance_to(there) - 5.0).abs() < 1e-12);
        assert!((there.distance_to(here) - 13.0).abs() < 1e-12);
        assert!(here.distance_to(here).abs() < 1e-12);
    }

    // --- Blocks ---

    /// Stone below the feet's level, air above it, around (10, 64, -5).
    fn floor() -> Blocks {
        Blocks::read(block(10, 64, -5), |at| {
            Some(id(if at.y < 64 { "stone" } else { "air" }))
        })
    }

    #[test]
    fn the_cube_reaches_four_blocks_each_way() {
        let blocks = floor();

        assert_eq!(blocks.center(), block(10, 64, -5));
        assert_eq!(blocks.get(block(10, 63, -5)), Some(&id("stone")));
        assert_eq!(blocks.get(block(10, 64, -5)), Some(&id("air")));
        for corner in [block(6, 60, -9), block(14, 68, -1), block(6, 68, -1)] {
            assert!(blocks.get(corner).is_some(), "{corner:?}");
        }
        for outside in [
            block(5, 64, -5),
            block(15, 64, -5),
            block(10, 59, -5),
            block(10, 69, -5),
            block(10, 64, -10),
            block(10, 64, 0),
        ] {
            assert_eq!(blocks.get(outside), None, "{outside:?}");
        }
    }

    #[test]
    fn the_reader_is_asked_for_each_of_the_729_places_once() {
        let mut asked = Vec::new();
        let _ = Blocks::read(block(0, 0, 0), |at| {
            asked.push(at);
            None
        });

        assert_eq!(asked.len(), 729);
        asked.sort_by_key(|at| (at.x, at.y, at.z));
        asked.dedup();
        assert_eq!(asked.len(), 729);
        assert!(
            asked
                .iter()
                .all(|at| at.x.abs() <= 4 && at.y.abs() <= 4 && at.z.abs() <= 4)
        );
    }

    #[test]
    fn each_kind_of_block_is_held_once() {
        assert_eq!(floor().kinds(), 2);
        assert_eq!(Blocks::read(block(0, 0, 0), |_| None).kinds(), 0);
    }

    #[test]
    fn a_place_the_reader_didnt_know_has_no_block() {
        let blocks = Blocks::read(block(0, 64, 0), |at| (at.x >= 0).then(|| id("stone")));

        assert_eq!(blocks.get(block(1, 64, 0)), Some(&id("stone")));
        assert_eq!(blocks.get(block(-1, 64, 0)), None);
    }

    #[test]
    fn a_cube_at_the_edge_of_the_numbers_skips_what_doesnt_exist() {
        let edge = block(i32::MAX, i32::MIN, 0);
        let blocks = Blocks::read(edge, |_| Some(id("stone")));

        assert_eq!(blocks.get(edge), Some(&id("stone")));
        assert_eq!(
            blocks.get(block(i32::MAX - 4, i32::MIN + 4, 4)),
            Some(&id("stone"))
        );
        assert_eq!(blocks.get(block(i32::MAX - 5, i32::MIN, 0)), None);
        assert_eq!(blocks.get(block(i32::MIN, i32::MIN, 0)), None);
    }

    // --- The snapshot ---

    fn snapshot(entities: Vec<EntityFact>) -> Snapshot {
        Snapshot::new(bot_at(at(0.0, 64.0, 0.0)), entities, floor())
    }

    #[test]
    fn entities_come_nearest_first() {
        let seen = snapshot(vec![
            entity("zombie", 9.0, false),
            entity("minecart", 3.0, true),
            entity("minecart", -1.0, false),
            entity("cow", 5.0, false),
        ]);

        let order: Vec<f64> = seen
            .entities()
            .iter()
            .map(|entity| entity.position.x())
            .collect();
        assert_eq!(order, [-1.0, 3.0, 5.0, 9.0]);
        assert!((seen.distance_to(&seen.entities()[1]) - 3.0).abs() < 1e-12);
    }

    #[test]
    fn an_entity_beyond_16_blocks_isnt_in_the_snapshot() {
        let seen = snapshot(vec![
            entity("cow", 16.0, false),
            entity("cow", 16.001, false),
            entity("cow", -40.0, false),
        ]);

        assert_eq!(seen.entities().len(), 1);
        assert_eq!(seen.entities()[0].position.x(), 16.0);
    }

    #[test]
    fn a_snapshot_holds_the_64_nearest_entities() {
        let many = (0..200).map(|n| entity("cow", f64::from(n) * 0.05, false));

        let seen = snapshot(many.collect());

        assert_eq!(seen.entities().len(), 64);
        assert!(
            seen.entities()
                .iter()
                .all(|entity| entity.position.x() < 3.2)
        );
    }

    #[test]
    fn entities_at_one_distance_keep_their_order() {
        let seen = snapshot(vec![
            entity("cow", 2.0, false),
            entity("pig", 2.0, false),
            entity("sheep", -2.0, false),
        ]);

        let kinds: Vec<&str> = seen
            .entities()
            .iter()
            .map(|entity| entity.kind.path())
            .collect();
        assert_eq!(kinds, ["cow", "pig", "sheep"]);
    }

    #[test]
    fn the_nearest_of_a_kind_and_the_nearest_free_one() {
        let seen = snapshot(vec![
            entity("minecart", 1.0, true),
            entity("minecart", 2.0, true),
            entity("minecart", 4.0, false),
            entity("minecart", 6.0, false),
            entity("cow", 0.5, false),
        ]);
        let minecart = id("minecart");

        assert_eq!(
            seen.nearest(&minecart).map(|found| found.position.x()),
            Some(1.0)
        );
        assert_eq!(
            seen.nearest_free(&minecart).map(|found| found.position.x()),
            Some(4.0)
        );
        assert_eq!(seen.nearest(&id("boat")), None);
        assert_eq!(seen.nearest_free(&id("boat")), None);
    }

    #[test]
    fn when_every_vehicle_is_taken_none_is_free() {
        let seen = snapshot(vec![
            entity("minecart", 1.0, true),
            entity("minecart", 2.0, true),
        ]);

        assert!(seen.nearest(&id("minecart")).is_some());
        assert_eq!(seen.nearest_free(&id("minecart")), None);
    }

    #[test]
    fn a_snapshot_hands_out_what_it_was_given() {
        let bot = BotFacts {
            held: Some(HeldItem {
                id: id("iron_pickaxe"),
                count: 1,
                durability: Some(250),
            }),
            riding: Some(id("minecart")),
            looking_at: Some(LookingAt::Block {
                id: id("cobblestone"),
                at: block(0, 64, 1),
            }),
            ..bot_at(at(0.5, 64.0, 0.5))
        };

        let seen = Snapshot::new(bot.clone(), Vec::new(), floor());

        assert_eq!(seen.bot(), &bot);
        assert_eq!(seen.blocks(), &floor());
        assert_eq!(seen.entities(), []);
    }

    #[test]
    fn the_errors_never_hold_the_text() {
        assert_eq!(
            IdentifierError::InvalidChar { index: 4 }.to_string(),
            "the identifier holds a character it may not, at position 4"
        );
        assert_eq!(
            IdentifierError::TooLong { chars: 200 }.to_string(),
            "the identifier is 200 characters long; the limit is 128"
        );
        assert_eq!(
            PositionError.to_string(),
            "a position's coordinates must be numbers"
        );
    }

    // --- Properties ---

    proptest! {
        #[test]
        fn an_identifier_never_panics_and_is_always_whole(text in any::<String>()) {
            if let Ok(identifier) = Identifier::try_from(text.as_str()) {
                prop_assert!(!identifier.namespace().is_empty());
                prop_assert!(!identifier.path().is_empty());
                prop_assert_eq!(
                    identifier.as_str(),
                    format!("{}:{}", identifier.namespace(), identifier.path())
                );
                prop_assert_eq!(Identifier::try_from(identifier.as_str()), Ok(identifier.clone()));
                prop_assert!(identifier.as_str().is_ascii());
            }
        }

        #[test]
        fn every_name_of_the_grammar_is_an_identifier(text in "([a-z0-9_.-]{1,12}:)?[a-z0-9_./-]{1,24}") {
            prop_assert!(Identifier::try_from(text.as_str()).is_ok());
        }

        /// Whatever a server shows a bot: at most 64 entities, none beyond
        /// 16 blocks, never further before nearer.
        #[test]
        fn a_snapshot_is_bounded_whatever_it_is_given(
            entities in proptest::collection::vec((-40.0_f64..40.0, -40.0_f64..40.0, any::<bool>()), 0..150),
        ) {
            let given: Vec<EntityFact> = entities
                .into_iter()
                .map(|(x, z, occupied)| EntityFact {
                    kind: id("minecart"),
                    position: at(x, 64.0, z),
                    occupied,
                })
                .collect();

            let seen = snapshot(given.clone());

            prop_assert!(seen.entities().len() <= Snapshot::MAX_ENTITIES);
            let distances: Vec<f64> = seen.entities().iter().map(|e| seen.distance_to(e)).collect();
            prop_assert!(distances.iter().all(|distance| *distance <= Snapshot::ENTITY_RANGE));
            prop_assert!(distances.windows(2).all(|pair| pair[0] <= pair[1]));
            let in_range = given.iter().filter(|e| seen.distance_to(e) <= Snapshot::ENTITY_RANGE).count();
            prop_assert_eq!(seen.entities().len(), in_range.min(Snapshot::MAX_ENTITIES));
            if let Some(free) = seen.nearest_free(&id("minecart")) {
                prop_assert!(!free.occupied);
            }
        }

        /// The cube answers for exactly the places within its radius.
        #[test]
        fn the_cube_answers_exactly_within_its_radius(
            center in (-1000_i32..1000, -64_i32..320, -1000_i32..1000),
            offset in (-8_i32..=8, -8_i32..=8, -8_i32..=8),
        ) {
            let center = block(center.0, center.1, center.2);
            let blocks = Blocks::read(center, |_| Some(id("stone")));
            let place = block(center.x + offset.0, center.y + offset.1, center.z + offset.2);

            let inside = offset.0.abs() <= 4 && offset.1.abs() <= 4 && offset.2.abs() <= 4;

            prop_assert_eq!(blocks.get(place).is_some(), inside);
        }
    }
}
