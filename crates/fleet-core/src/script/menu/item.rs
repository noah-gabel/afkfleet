//! [`Item`]: what a slot holds, with its text cleaned and its data
//! components as IDs and numbers (ADR-0036, M1, M3, M11).

use core::fmt;
use core::num::NonZeroU32;
use std::sync::Arc;

use super::text::{Lore, ServerText};
use crate::script::Identifier;

/// Every data component the server sent with an item, as fleet-mc encodes
/// them: compared whole by the click guard, and never read (ADR-0036, M3).
///
/// A script reads only an item's ID, count, durability, name, lore and the
/// components of [`Item::potion`] and [`Item::enchantments`]. Two listings
/// of one kind of item can differ anywhere else, so the guard compares
/// this too: a click meant for one never lands on the other.
///
/// **fleet-mc's encoding must be canonical:** the same components give the
/// same bytes, whatever order the server sent them in. A difference there
/// only refuses a click that would have been right (`slot_changed`), never
/// lets one through.
///
/// The bytes are shared, so a snapshot of a screen costs no copy of them.
/// `Debug` prints only their length.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct ItemData(Arc<[u8]>);

impl ItemData {
    /// An item's data components as fleet-mc encodes them.
    #[must_use]
    pub fn new(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self(bytes.into())
    }

    /// How many bytes the encoding has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the item has no data components.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for ItemData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ItemData({} bytes)", self.0.len())
    }
}

/// An item's enchantments, or an enchanted book's stored ones: each
/// enchantment's registry ID with its level (ADR-0036, M11).
///
/// Bounded, as everything a server can grow: at most
/// [`Enchantments::MAX`] of them, the first by ID, each level from 0 to
/// [`Enchantments::MAX_LEVEL`]. An entry whose ID isn't an [`Identifier`]
/// or whose level is outside that range is left out, never passed on as
/// text; an ID that comes twice keeps its highest level. What's kept
/// doesn't depend on the order the server sent them in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Enchantments(Vec<(Identifier, u8)>);

impl Enchantments {
    /// The most enchantments kept for one item.
    pub const MAX: usize = 16;
    /// The highest level kept: the most a command gives.
    pub const MAX_LEVEL: u8 = u8::MAX;

    /// Reads enchantments from each one's registry ID, as the world's
    /// registries name it, and its level.
    #[must_use]
    pub fn read<'a>(raw: impl IntoIterator<Item = (&'a str, i32)>) -> Self {
        // At most `MAX + 1` are held at any time, in order of ID, so a
        // server that sends a great many costs time in proportion and no
        // more memory.
        let mut kept: Vec<(Identifier, u8)> = Vec::with_capacity(Self::MAX + 1);
        for (name, level) in raw {
            // 0 to `MAX_LEVEL` is exactly what a `u8` holds.
            let Ok(level) = u8::try_from(level) else {
                continue;
            };
            let Ok(id) = Identifier::try_from(name) else {
                continue;
            };
            match kept.binary_search_by(|(known, _)| known.as_str().cmp(id.as_str())) {
                Ok(at) => {
                    if let Some((_, known)) = kept.get_mut(at) {
                        *known = (*known).max(level);
                    }
                }
                Err(at) => {
                    kept.insert(at, (id, level));
                    kept.truncate(Self::MAX);
                }
            }
        }
        Self(kept)
    }

    /// Each enchantment with its level, by ID.
    pub fn iter(&self) -> impl Iterator<Item = (&Identifier, u8)> {
        self.0.iter().map(|(id, level)| (id, *level))
    }

    /// The level of `id`, if the item has it.
    #[must_use]
    pub fn level(&self, id: &Identifier) -> Option<u8> {
        self.iter()
            .find(|(kept, _)| *kept == id)
            .map(|(_, level)| level)
    }

    /// How many there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// An item in a slot: what a script reads of it, and the whole of it for
/// the click guard (ADR-0036, M1, M3, M11).
///
/// **Two items are equal only when they're the same whole item:** the same
/// ID, count, durability, name, lore, components and [`ItemData`]. That's
/// the comparison the click guard makes (P16.6).
///
/// Its text is a server's, cleaned and bounded: a name of at most
/// [`Item::MAX_NAME`] characters and a [`Lore`]. Its components are IDs
/// and numbers, never text.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Item {
    id: Identifier,
    count: NonZeroU32,
    durability: Option<u32>,
    name: ServerText,
    lore: Lore,
    potion: Option<Identifier>,
    enchantments: Enchantments,
    stored_enchantments: Enchantments,
    data: ItemData,
}

impl Item {
    /// The longest name kept, in characters.
    pub const MAX_NAME: usize = 64;
    /// The items whose potion component a script reads (M11): a potion, a
    /// splash and a lingering potion, and a tipped arrow.
    pub const POTION_ITEMS: [&'static str; 4] = [
        "minecraft:potion",
        "minecraft:splash_potion",
        "minecraft:lingering_potion",
        "minecraft:tipped_arrow",
    ];

    /// An item of kind `id`, `count` of it, whose data components are
    /// `data`; without durability, name, lore or components until they're
    /// added.
    #[must_use]
    pub fn new(id: Identifier, count: NonZeroU32, data: ItemData) -> Self {
        Self {
            id,
            count,
            durability: None,
            name: ServerText::default(),
            lore: Lore::default(),
            potion: None,
            enchantments: Enchantments::default(),
            stored_enchantments: Enchantments::default(),
            data,
        }
    }

    /// The same item with how many uses it has left: `None` for one that
    /// doesn't wear.
    #[must_use]
    pub fn with_durability(self, durability: Option<u32>) -> Self {
        Self { durability, ..self }
    }

    /// The same item with the name the server shows for it, rendered as
    /// plain text and cleaned here.
    #[must_use]
    pub fn named(self, raw: &str) -> Self {
        Self {
            name: ServerText::clean(raw, Self::MAX_NAME),
            ..self
        }
    }

    /// The same item with its lore, each line rendered as plain text and
    /// cleaned here.
    #[must_use]
    pub fn with_lore<S: AsRef<str>>(self, raw: impl IntoIterator<Item = S>) -> Self {
        Self {
            lore: Lore::read(raw),
            ..self
        }
    }

    /// The same item with its potion's kind, as the world's registries name
    /// it. Kept only for one of [`Item::POTION_ITEMS`], and only when it's
    /// an [`Identifier`].
    #[must_use]
    pub fn with_potion(self, raw: Option<&str>) -> Self {
        let carries = Self::POTION_ITEMS.contains(&self.id.as_str());
        let potion = raw
            .filter(|_| carries)
            .and_then(|raw| Identifier::try_from(raw).ok());
        Self { potion, ..self }
    }

    /// The same item with its enchantments, read as [`Enchantments::read`]
    /// reads them.
    #[must_use]
    pub fn with_enchantments<'a>(self, raw: impl IntoIterator<Item = (&'a str, i32)>) -> Self {
        Self {
            enchantments: Enchantments::read(raw),
            ..self
        }
    }

    /// The same item with an enchanted book's stored enchantments, read as
    /// [`Enchantments::read`] reads them.
    #[must_use]
    pub fn with_stored_enchantments<'a>(
        self,
        raw: impl IntoIterator<Item = (&'a str, i32)>,
    ) -> Self {
        Self {
            stored_enchantments: Enchantments::read(raw),
            ..self
        }
    }

    /// What it is.
    #[must_use]
    pub const fn id(&self) -> &Identifier {
        &self.id
    }

    /// How many of it.
    #[must_use]
    pub const fn count(&self) -> NonZeroU32 {
        self.count
    }

    /// How many uses it has left; `None` for an item that doesn't wear.
    #[must_use]
    pub const fn durability(&self) -> Option<u32> {
        self.durability
    }

    /// Its name as the server shows it, or an empty text.
    #[must_use]
    pub const fn name(&self) -> &ServerText {
        &self.name
    }

    /// Its lore.
    #[must_use]
    pub const fn lore(&self) -> &Lore {
        &self.lore
    }

    /// Its potion's kind, such as `minecraft:turtle_master`.
    #[must_use]
    pub const fn potion(&self) -> Option<&Identifier> {
        self.potion.as_ref()
    }

    /// Its enchantments.
    #[must_use]
    pub const fn enchantments(&self) -> &Enchantments {
        &self.enchantments
    }

    /// An enchanted book's stored enchantments.
    #[must_use]
    pub const fn stored_enchantments(&self) -> &Enchantments {
        &self.stored_enchantments
    }

    /// Every data component the server sent with it.
    #[must_use]
    pub const fn data(&self) -> &ItemData {
        &self.data
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    pub(crate) fn id(text: &str) -> Identifier {
        Identifier::try_from(text).unwrap()
    }

    pub(crate) fn item(kind: &str, count: u32) -> Item {
        Item::new(
            id(kind),
            NonZeroU32::new(count).unwrap(),
            ItemData::default(),
        )
    }

    fn levels(enchantments: &Enchantments) -> Vec<(&str, u8)> {
        enchantments
            .iter()
            .map(|(id, level)| (id.as_str(), level))
            .collect()
    }

    #[test]
    fn a_new_item_has_its_kind_count_and_data_and_nothing_else() {
        let item = Item::new(
            id("diamond"),
            NonZeroU32::new(3).unwrap(),
            ItemData::new([1, 2]),
        );

        assert_eq!(item.id().as_str(), "minecraft:diamond");
        assert_eq!(item.count().get(), 3);
        assert_eq!(item.durability(), None);
        assert!(item.name().is_empty());
        assert_eq!(item.lore().lines(), []);
        assert_eq!(item.potion(), None);
        assert!(item.enchantments().is_empty());
        assert!(item.stored_enchantments().is_empty());
        assert_eq!(item.data().len(), 2);
    }

    #[test]
    fn a_name_is_cleaned_and_bounded() {
        let named = item("diamond", 1).named("\u{a7}6Cre\u{200b}ate\n");
        let long = item("diamond", 1).named(&"y".repeat(100));

        assert_eq!(named.name().as_str(), "Create");
        assert_eq!(long.name().as_str(), "y".repeat(Item::MAX_NAME));
    }

    #[test]
    fn lore_and_durability_are_kept() {
        let tool = item("iron_pickaxe", 1)
            .with_durability(Some(17))
            .with_lore(["Price: 500"]);

        assert_eq!(tool.durability(), Some(17));
        assert_eq!(tool.lore().lines()[0].as_str(), "Price: 500");
    }

    #[rstest]
    #[case::a_potion("potion")]
    #[case::a_splash_potion("splash_potion")]
    #[case::a_lingering_potion("lingering_potion")]
    #[case::a_tipped_arrow("tipped_arrow")]
    fn a_potions_kind_is_read_for_the_four_items(#[case] kind: &str) {
        let potion = item(kind, 1).with_potion(Some("minecraft:strong_turtle_master"));

        assert_eq!(
            potion.potion().map(Identifier::as_str),
            Some("minecraft:strong_turtle_master")
        );
    }

    #[rstest]
    #[case::another_item("diamond", Some("minecraft:turtle_master"))]
    #[case::a_glass_bottle("glass_bottle", Some("minecraft:water"))]
    #[case::no_kind("potion", None)]
    #[case::not_an_identifier("potion", Some("Turtle Master"))]
    #[case::formatting_in_it("potion", Some("\u{a7}cminecraft:turtle_master"))]
    #[case::empty("potion", Some(""))]
    fn a_potions_kind_is_left_out_anywhere_else(#[case] kind: &str, #[case] raw: Option<&str>) {
        assert_eq!(item(kind, 1).with_potion(raw).potion(), None);
    }

    #[test]
    fn enchantments_are_kept_by_id_with_their_levels() {
        let enchantments =
            Enchantments::read([("minecraft:sharpness", 5), ("minecraft:unbreaking", 3)]);

        assert_eq!(
            levels(&enchantments),
            [("minecraft:sharpness", 5), ("minecraft:unbreaking", 3)]
        );
        assert_eq!(enchantments.level(&id("sharpness")), Some(5));
        assert_eq!(enchantments.level(&id("mending")), None);
        assert_eq!(enchantments.len(), 2);
    }

    #[rstest]
    #[case::the_lowest(0, Some(0))]
    #[case::the_highest(255, Some(255))]
    #[case::below(-1, None)]
    #[case::above(256, None)]
    #[case::far_above(i32::MAX, None)]
    #[case::far_below(i32::MIN, None)]
    fn a_level_outside_0_to_255_is_left_out(#[case] level: i32, #[case] kept: Option<u8>) {
        let enchantments = Enchantments::read([("minecraft:sharpness", level)]);

        assert_eq!(enchantments.level(&id("sharpness")), kept);
    }

    #[rstest]
    #[case::a_name("Sharpness")]
    #[case::formatting("\u{a7}csharpness")]
    #[case::a_space("minecraft:sharp ness")]
    #[case::empty("")]
    #[case::a_colon_alone(":")]
    fn an_id_that_isnt_one_is_left_out(#[case] raw: &str) {
        let enchantments = Enchantments::read([(raw, 1), ("minecraft:mending", 1)]);

        assert_eq!(levels(&enchantments), [("minecraft:mending", 1)]);
    }

    #[test]
    fn an_id_twice_keeps_its_highest_level() {
        let enchantments = Enchantments::read([
            ("minecraft:unknown", 2),
            ("minecraft:unknown", 7),
            ("minecraft:unknown", 1),
        ]);

        assert_eq!(levels(&enchantments), [("minecraft:unknown", 7)]);
    }

    #[test]
    fn at_most_sixteen_are_kept_the_first_by_id() {
        // Twenty, given last to first.
        let names: Vec<String> = (0..20).rev().map(|n| format!("test:e{n:02}")).collect();

        let enchantments = Enchantments::read(names.iter().map(|name| (name.as_str(), 1)));

        let kept: Vec<&str> = enchantments.iter().map(|(id, _)| id.as_str()).collect();
        let expected: Vec<String> = (0..16).map(|n| format!("test:e{n:02}")).collect();
        assert_eq!(kept, expected);
    }

    #[test]
    fn a_book_keeps_its_stored_enchantments_apart() {
        let book = item("enchanted_book", 1).with_stored_enchantments([("minecraft:mending", 1)]);
        let sword = item("diamond_sword", 1).with_enchantments([("minecraft:sharpness", 5)]);

        assert!(book.enchantments().is_empty());
        assert_eq!(
            levels(book.stored_enchantments()),
            [("minecraft:mending", 1)]
        );
        assert_eq!(levels(sword.enchantments()), [("minecraft:sharpness", 5)]);
        assert!(sword.stored_enchantments().is_empty());
    }

    #[test]
    fn two_items_are_the_same_only_when_everything_is() {
        let listing = || {
            item("diamond", 1)
                .named("Diamond")
                .with_lore(["Price: 500"])
        };
        let with_data = |bytes: &[u8]| {
            Item::new(id("diamond"), NonZeroU32::MIN, ItemData::new(bytes))
                .named("Diamond")
                .with_lore(["Price: 500"])
        };

        assert_eq!(listing(), listing());
        assert_ne!(listing(), listing().with_lore(["Price: 5000"]));
        assert_ne!(listing(), listing().named("Diamonds"));
        assert_ne!(
            listing(),
            item("diamond", 2)
                .named("Diamond")
                .with_lore(["Price: 500"])
        );
        assert_ne!(listing(), listing().with_durability(Some(1)));
        assert_ne!(
            listing(),
            listing().with_enchantments([("minecraft:mending", 1)])
        );
        // Everything a script reads is equal; a component it can't read isn't.
        assert_eq!(with_data(&[1, 2, 3]), with_data(&[1, 2, 3]));
        assert_ne!(with_data(&[1, 2, 3]), with_data(&[1, 2, 4]));
    }

    #[test]
    fn data_is_shown_by_its_length_only() {
        assert_eq!(
            format!("{:?}", ItemData::new([0xde, 0xad, 0xbe, 0xef])),
            "ItemData(4 bytes)"
        );
        assert!(ItemData::default().is_empty());
    }

    proptest! {
        /// Any enchantments a server could send: at most sixteen, by ID,
        /// each ID once and an identifier, each level in its range, and
        /// the same whatever order they came in.
        #[test]
        fn any_enchantments_are_bounded_ordered_and_order_free(
            raw in proptest::collection::vec(
                ("[a-z:_ ]{0,12}|test:e[0-9]{1,2}", prop_oneof![-3_i32..260, any::<i32>()]),
                0..60,
            ),
        ) {
            let given: Vec<(&str, i32)> = raw.iter().map(|(name, level)| (name.as_str(), *level)).collect();
            let enchantments = Enchantments::read(given.iter().copied());

            // The model: every valid entry, each ID at its highest level,
            // the first sixteen by ID.
            let mut model = std::collections::BTreeMap::<String, i32>::new();
            for (name, level) in &given {
                if let Ok(valid) = Identifier::try_from(*name)
                    && (0..=i32::from(Enchantments::MAX_LEVEL)).contains(level)
                {
                    let highest = model.entry(valid.as_str().to_owned()).or_insert(*level);
                    *highest = (*highest).max(*level);
                }
            }
            let expected: Vec<(String, i32)> = model.into_iter().take(Enchantments::MAX).collect();
            let kept: Vec<(String, i32)> = enchantments
                .iter()
                .map(|(id, level)| (id.as_str().to_owned(), i32::from(level)))
                .collect();
            prop_assert_eq!(kept, expected);
            let mut reversed = given.clone();
            reversed.reverse();
            prop_assert_eq!(Enchantments::read(reversed), enchantments);
        }
    }
}
