//! [`Screen`] and [`BotInventory`]: the open screen and the bot's own
//! inventory, every slot in them (ADR-0036, M2, M18; P16.5).

use crate::mode::HotbarSlot;

use super::item::Item;
use super::text::ServerText;

/// A window's number, as the server gives it: `0` is the bot's own
/// inventory, every other screen gets its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(i32);

impl WindowId {
    /// The bot's own inventory.
    pub const INVENTORY: Self = Self(0);

    /// The window the server numbered `id`.
    #[must_use]
    pub const fn new(id: i32) -> Self {
        Self(id)
    }

    /// The number.
    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }
}

/// The state a window's content was in when the server last sent it: a
/// click names it, and the server takes a click with an older one as stale
/// (ADR-0036, M2, R1). fleet-mc keeps each window's own (P16.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StateId(u32);

impl StateId {
    /// The state the server numbered `id`.
    #[must_use]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// The number.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// One opening of a screen: its window and which opening of the session it
/// is (ADR-0036, M2; ADR-0050).
///
/// **A new screen is a new screen,** whatever its title, and even when the
/// server gives it the number of one before: a server counts its windows
/// round from 1 to 100, and a plugin may open a screen under the number
/// of the one it replaces. So a screen is told apart by its opening, which
/// only [`ClickGate::opened`](super::ClickGate::opened) and
/// [`ClickGate::open_inventory`](super::ClickGate::open_inventory) hand out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScreenId {
    window: WindowId,
    opening: u64,
}

impl ScreenId {
    /// The opening `opening` of the session, in window `window`.
    pub(crate) const fn new(window: WindowId, opening: u64) -> Self {
        Self { window, opening }
    }

    /// The window the server opened it in.
    #[must_use]
    pub const fn window(self) -> WindowId {
        self.window
    }
}

/// What a screen is: one of the screens a vanilla client knows, or the
/// bot's own inventory (ADR-0036, M2).
///
/// The screens are the server's built-in `menu` registry, as azalea 0.16.0
/// names them (`MenuKind`); the inventory isn't in it, since a client
/// opens its own without asking the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScreenKind {
    /// The bot's own inventory, opened with E.
    Inventory,
    /// A chest-shaped screen of one row.
    Generic9x1,
    /// Two rows.
    Generic9x2,
    /// Three rows: a chest, a barrel, an ender chest.
    Generic9x3,
    /// Four rows.
    Generic9x4,
    /// Five rows.
    Generic9x5,
    /// Six rows: a double chest.
    Generic9x6,
    /// A dispenser or a dropper.
    Generic3x3,
    /// A crafter.
    Crafter3x3,
    /// An anvil.
    Anvil,
    /// A beacon.
    Beacon,
    /// A blast furnace.
    BlastFurnace,
    /// A brewing stand.
    BrewingStand,
    /// A crafting table.
    Crafting,
    /// An enchanting table.
    Enchantment,
    /// A furnace.
    Furnace,
    /// A grindstone.
    Grindstone,
    /// A hopper.
    Hopper,
    /// A lectern.
    Lectern,
    /// A loom.
    Loom,
    /// A villager's or a wandering trader's trades.
    Merchant,
    /// A shulker box.
    ShulkerBox,
    /// A smithing table.
    Smithing,
    /// A smoker.
    Smoker,
    /// A cartography table.
    CartographyTable,
    /// A stonecutter.
    Stonecutter,
}

/// How a screen's own slots are laid out when they're a grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Grid {
    /// How many rows.
    pub rows: u8,
    /// How many columns.
    pub columns: u8,
}

/// Where a slot is in its grid, counted from the top left (ADR-0036, M18).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Place {
    /// The row, from 0 at the top.
    pub row: u8,
    /// The column, from 0 at the left.
    pub column: u8,
}

/// How many slots of the bot's own a screen shows: the 27 of the main
/// inventory and the 9 of the hotbar.
const PLAYER_SLOTS: u16 = 36;
/// The bot's own slots are four rows of nine in every screen.
const PLAYER_GRID: Grid = Grid {
    rows: 4,
    columns: 9,
};

impl ScreenKind {
    /// Every kind, in a fixed order: the inventory, then the `menu`
    /// registry's own order.
    pub const ALL: [Self; 26] = [
        Self::Inventory,
        Self::Generic9x1,
        Self::Generic9x2,
        Self::Generic9x3,
        Self::Generic9x4,
        Self::Generic9x5,
        Self::Generic9x6,
        Self::Generic3x3,
        Self::Crafter3x3,
        Self::Anvil,
        Self::Beacon,
        Self::BlastFurnace,
        Self::BrewingStand,
        Self::Crafting,
        Self::Enchantment,
        Self::Furnace,
        Self::Grindstone,
        Self::Hopper,
        Self::Lectern,
        Self::Loom,
        Self::Merchant,
        Self::ShulkerBox,
        Self::Smithing,
        Self::Smoker,
        Self::CartographyTable,
        Self::Stonecutter,
    ];

    /// The kind as a script reads it in `menu.kind`: the registry's path,
    /// such as `generic_9x6`, and `inventory` for the bot's own.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inventory => "inventory",
            Self::Generic9x1 => "generic_9x1",
            Self::Generic9x2 => "generic_9x2",
            Self::Generic9x3 => "generic_9x3",
            Self::Generic9x4 => "generic_9x4",
            Self::Generic9x5 => "generic_9x5",
            Self::Generic9x6 => "generic_9x6",
            Self::Generic3x3 => "generic_3x3",
            Self::Crafter3x3 => "crafter_3x3",
            Self::Anvil => "anvil",
            Self::Beacon => "beacon",
            Self::BlastFurnace => "blast_furnace",
            Self::BrewingStand => "brewing_stand",
            Self::Crafting => "crafting",
            Self::Enchantment => "enchantment",
            Self::Furnace => "furnace",
            Self::Grindstone => "grindstone",
            Self::Hopper => "hopper",
            Self::Lectern => "lectern",
            Self::Loom => "loom",
            Self::Merchant => "merchant",
            Self::ShulkerBox => "shulker_box",
            Self::Smithing => "smithing",
            Self::Smoker => "smoker",
            Self::CartographyTable => "cartography_table",
            Self::Stonecutter => "stonecutter",
        }
    }

    /// How many slots of its own the screen has, as azalea 0.16.0's `Menu`
    /// lays them out: the inventory's are its crafting result and grid,
    /// its armour and its offhand.
    #[must_use]
    pub const fn own_slots(self) -> u16 {
        match self {
            Self::Beacon | Self::Lectern => 1,
            Self::Enchantment | Self::Stonecutter => 2,
            Self::Anvil
            | Self::BlastFurnace
            | Self::Furnace
            | Self::Grindstone
            | Self::Merchant
            | Self::Smoker
            | Self::CartographyTable => 3,
            Self::Loom | Self::Smithing => 4,
            Self::BrewingStand | Self::Hopper => 5,
            Self::Generic9x1 | Self::Generic3x3 | Self::Crafter3x3 => 9,
            Self::Inventory | Self::Crafting => 10,
            Self::Generic9x2 => 18,
            Self::Generic9x3 | Self::ShulkerBox => 27,
            Self::Generic9x4 => 36,
            Self::Generic9x5 => 45,
            Self::Generic9x6 => 54,
        }
    }

    /// How many slots the screen has in all: its own and the bot's 36.
    #[must_use]
    pub const fn slots(self) -> u16 {
        self.own_slots() + PLAYER_SLOTS
    }

    /// Where the bot's own 36 slots start in the screen's numbering: right
    /// after the screen's own, except in the inventory, where its offhand
    /// comes after them.
    #[must_use]
    pub const fn player_start(self) -> u16 {
        match self {
            // The crafting result and grid (0 to 4) and the armour (5 to
            // 8); the offhand is 45.
            Self::Inventory => 9,
            _ => self.own_slots(),
        }
    }

    /// How the screen's own slots are laid out, if they're a grid: the
    /// chests, the 3-by-3 screens, the shulker box and the hopper
    /// (ADR-0036, M18).
    #[must_use]
    pub const fn grid(self) -> Option<Grid> {
        let (rows, columns) = match self {
            Self::Generic9x1 => (1, 9),
            Self::Generic9x2 => (2, 9),
            Self::Generic9x3 | Self::ShulkerBox => (3, 9),
            Self::Generic9x4 => (4, 9),
            Self::Generic9x5 => (5, 9),
            Self::Generic9x6 => (6, 9),
            Self::Generic3x3 | Self::Crafter3x3 => (3, 3),
            Self::Hopper => (1, 5),
            Self::Inventory
            | Self::Anvil
            | Self::Beacon
            | Self::BlastFurnace
            | Self::BrewingStand
            | Self::Crafting
            | Self::Enchantment
            | Self::Furnace
            | Self::Grindstone
            | Self::Lectern
            | Self::Loom
            | Self::Merchant
            | Self::Smithing
            | Self::Smoker
            | Self::CartographyTable
            | Self::Stonecutter => return None,
        };
        Some(Grid { rows, columns })
    }

    /// Where slot `index` of this kind of screen is, if it's in a grid: the
    /// screen's own, or the bot's 36.
    fn place_of(self, index: u16) -> Option<Place> {
        let start = self.player_start();
        if (start..start + PLAYER_SLOTS).contains(&index) {
            return PLAYER_GRID.place(index - start);
        }
        // Outside the bot's slots, every slot of a grid is one of the
        // grid's: no kind with a grid has an offhand.
        self.grid()?.place(index)
    }
}

impl Grid {
    /// Where the `at`-th slot of the grid is, counted along the rows.
    fn place(self, at: u16) -> Option<Place> {
        let columns = u16::from(self.columns);
        let row = u8::try_from(at.checked_div(columns)?).ok()?;
        let column = u8::try_from(at.checked_rem(columns)?).ok()?;
        (row < self.rows).then_some(Place { row, column })
    }

    /// The number in the grid of the slot at `place`, if it's inside.
    fn index(self, place: Place) -> Option<u16> {
        (place.row < self.rows && place.column < self.columns)
            .then(|| u16::from(place.row) * u16::from(self.columns) + u16::from(place.column))
    }
}

/// One slot of a screen or of the bot's inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    index: u16,
    place: Option<Place>,
    item: Option<Item>,
}

impl Slot {
    /// Its number in the screen it belongs to: what a click names.
    #[must_use]
    pub const fn index(&self) -> u16 {
        self.index
    }

    /// Where it is in its grid, if it's in one (ADR-0036, M18).
    #[must_use]
    pub const fn place(&self) -> Option<Place> {
        self.place
    }

    /// What it holds; `None` when it's empty.
    #[must_use]
    pub const fn item(&self) -> Option<&Item> {
        self.item.as_ref()
    }
}

/// One of a merchant's trades (ADR-0036, M7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeOffer {
    cost: Item,
    second_cost: Option<Item>,
    result: Item,
    out_of_stock: bool,
}

impl TradeOffer {
    /// A trade of `cost`, and `second_cost` if it asks for two items, for
    /// `result`; the costs' counts as the merchant asks them now.
    #[must_use]
    pub const fn new(
        cost: Item,
        second_cost: Option<Item>,
        result: Item,
        out_of_stock: bool,
    ) -> Self {
        Self {
            cost,
            second_cost,
            result,
            out_of_stock,
        }
    }

    /// What it costs.
    #[must_use]
    pub const fn cost(&self) -> &Item {
        &self.cost
    }

    /// The second item it costs, if any.
    #[must_use]
    pub const fn second_cost(&self) -> Option<&Item> {
        self.second_cost.as_ref()
    }

    /// What it gives.
    #[must_use]
    pub const fn result(&self) -> &Item {
        &self.result
    }

    /// Whether it can't be traded until the merchant restocks.
    #[must_use]
    pub const fn out_of_stock(&self) -> bool {
        self.out_of_stock
    }
}

/// The screen that's open, as it was at one moment: its every slot, its
/// kind, title and cursor, and which screen and state it is (ADR-0036, M2;
/// P16.5).
///
/// It's bounded whatever a server sends: exactly
/// [`ScreenKind::slots`] slots, a title of at most [`Screen::MAX_TITLE`]
/// characters, and for a merchant at most [`Screen::MAX_OFFERS`] trades.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    id: ScreenId,
    state: StateId,
    kind: ScreenKind,
    title: ServerText,
    slots: Vec<Slot>,
    cursor: Option<Item>,
    offers: Vec<TradeOffer>,
}

impl Screen {
    /// The longest title kept, in characters.
    pub const MAX_TITLE: usize = 128;
    /// The most trades a merchant's screen keeps.
    pub const MAX_OFFERS: usize = 64;

    /// The screen `id` of kind `kind` in state `state`, with the title the
    /// server gave it, rendered as plain text and cleaned here, and the
    /// cursor's item.
    ///
    /// `items` are its slots in the screen's own numbering, from 0: the
    /// first [`ScreenKind::slots`] are taken, and a slot that isn't given
    /// is empty.
    #[must_use]
    pub fn new(
        id: ScreenId,
        state: StateId,
        kind: ScreenKind,
        title: &str,
        items: impl IntoIterator<Item = Option<Item>>,
        cursor: Option<Item>,
    ) -> Self {
        Self {
            id,
            state,
            kind,
            title: ServerText::clean(title, Self::MAX_TITLE),
            slots: slots_of(kind, 0..kind.slots(), items),
            cursor,
            offers: Vec::new(),
        }
    }

    /// The same screen with a merchant's trades, in the merchant's order:
    /// the first [`Screen::MAX_OFFERS`], and none for any other kind.
    #[must_use]
    pub fn with_offers(self, offers: impl IntoIterator<Item = TradeOffer>) -> Self {
        let offers = if self.kind == ScreenKind::Merchant {
            offers.into_iter().take(Self::MAX_OFFERS).collect()
        } else {
            Vec::new()
        };
        Self { offers, ..self }
    }

    /// Which screen it is.
    #[must_use]
    pub const fn id(&self) -> ScreenId {
        self.id
    }

    /// The state its content was in.
    #[must_use]
    pub const fn state(&self) -> StateId {
        self.state
    }

    /// What it is.
    #[must_use]
    pub const fn kind(&self) -> ScreenKind {
        self.kind
    }

    /// Its title.
    #[must_use]
    pub const fn title(&self) -> &ServerText {
        &self.title
    }

    /// The item on the cursor.
    #[must_use]
    pub const fn cursor(&self) -> Option<&Item> {
        self.cursor.as_ref()
    }

    /// A merchant's trades, in its order.
    #[must_use]
    pub fn offers(&self) -> &[TradeOffer] {
        &self.offers
    }

    /// The slot numbered `index`.
    #[must_use]
    pub fn slot(&self, index: u16) -> Option<&Slot> {
        self.slots.get(usize::from(index))
    }

    /// The screen's own slots, in order: what `menu.slots` shows.
    pub fn own(&self) -> impl Iterator<Item = &Slot> {
        let start = self.kind.player_start();
        self.slots
            .iter()
            .filter(move |slot| !(start..start + PLAYER_SLOTS).contains(&slot.index))
    }

    /// The bot's own 36 slots as the screen shows them, in order: what
    /// `menu.inventory` shows.
    pub fn player(&self) -> impl Iterator<Item = &Slot> {
        let start = self.kind.player_start();
        self.slots
            .iter()
            .filter(move |slot| (start..start + PLAYER_SLOTS).contains(&slot.index))
    }

    /// How its own slots are laid out, if they're a grid.
    #[must_use]
    pub const fn grid(&self) -> Option<Grid> {
        self.kind.grid()
    }

    /// Its own slot at `row` and `column` of its grid, counted from the top
    /// left; `None` outside the grid, or when it has none (ADR-0036, M18).
    #[must_use]
    pub fn at(&self, row: u8, column: u8) -> Option<&Slot> {
        let index = self.grid()?.index(Place { row, column })?;
        self.slot(index)
    }

    /// The number of the bot's hotbar slot `slot` in this screen: what a
    /// number key swaps with.
    #[must_use]
    pub fn hotbar(&self, slot: HotbarSlot) -> u16 {
        self.kind.player_start() + HOTBAR_START + u16::from(slot.get())
    }
}

/// Where the hotbar starts among the bot's 36 slots: after the main
/// inventory's 27.
const HOTBAR_START: u16 = 27;

/// The slots numbered `indexes` of a screen of kind `kind`, filled from
/// `items` in the screen's own numbering: what isn't given is empty, and
/// what's given past the screen's last slot is never read.
fn slots_of(
    kind: ScreenKind,
    indexes: core::ops::Range<u16>,
    items: impl IntoIterator<Item = Option<Item>>,
) -> Vec<Slot> {
    let mut items = items.into_iter();
    (0..kind.slots())
        .map(|index| (index, items.next().flatten()))
        .filter(|(index, _)| indexes.contains(index))
        .map(|(index, item)| Slot {
            index,
            place: kind.place_of(index),
            item,
        })
        .collect()
}

/// The bot's own inventory, whether a screen is open or not: its armour,
/// its 36 slots and its offhand, numbered as its inventory screen numbers
/// them (ADR-0036, M2; P16.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotInventory {
    slots: Vec<Slot>,
}

impl BotInventory {
    /// The first slot a script reads: the helmet's. Before it are the
    /// crafting result and grid, which only the open inventory screen
    /// shows.
    const FIRST: u16 = 5;

    /// The inventory from window 0's slots in its own numbering, from 0:
    /// the first 46 are taken, and a slot that isn't given is empty.
    #[must_use]
    pub fn new(items: impl IntoIterator<Item = Option<Item>>) -> Self {
        let kind = ScreenKind::Inventory;
        Self {
            slots: slots_of(kind, Self::FIRST..kind.slots(), items),
        }
    }

    /// Its armour, its 36 slots and its offhand, in order: what
    /// `inventory.slots` shows.
    pub fn slots(&self) -> impl Iterator<Item = &Slot> {
        self.slots.iter()
    }

    /// The slot numbered `index`, if a script reads it.
    #[must_use]
    pub fn slot(&self, index: u16) -> Option<&Slot> {
        self.slots.get(usize::from(index.checked_sub(Self::FIRST)?))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::script::menu::item::tests::item;
    use proptest::prelude::*;
    use rstest::rstest;

    pub(crate) fn screen_id(window: i32, opening: u64) -> ScreenId {
        ScreenId::new(WindowId::new(window), opening)
    }

    /// A double chest whose slot `n` holds `n + 1` stones, the bot's own
    /// slots empty.
    fn double_chest() -> Screen {
        Screen::new(
            screen_id(3, 1),
            StateId::new(7),
            ScreenKind::Generic9x6,
            "Auction House",
            (1..=54).map(|n| Some(item("stone", n))),
            None,
        )
    }

    fn indexes<'a>(slots: impl Iterator<Item = &'a Slot>) -> Vec<u16> {
        slots.map(Slot::index).collect()
    }

    #[rstest]
    #[case::inventory(ScreenKind::Inventory, "inventory", 10, None)]
    #[case::one_row(ScreenKind::Generic9x1, "generic_9x1", 9, Some((1, 9)))]
    #[case::two_rows(ScreenKind::Generic9x2, "generic_9x2", 18, Some((2, 9)))]
    #[case::a_chest(ScreenKind::Generic9x3, "generic_9x3", 27, Some((3, 9)))]
    #[case::four_rows(ScreenKind::Generic9x4, "generic_9x4", 36, Some((4, 9)))]
    #[case::five_rows(ScreenKind::Generic9x5, "generic_9x5", 45, Some((5, 9)))]
    #[case::a_double_chest(ScreenKind::Generic9x6, "generic_9x6", 54, Some((6, 9)))]
    #[case::a_dispenser(ScreenKind::Generic3x3, "generic_3x3", 9, Some((3, 3)))]
    #[case::a_crafter(ScreenKind::Crafter3x3, "crafter_3x3", 9, Some((3, 3)))]
    #[case::an_anvil(ScreenKind::Anvil, "anvil", 3, None)]
    #[case::a_beacon(ScreenKind::Beacon, "beacon", 1, None)]
    #[case::a_blast_furnace(ScreenKind::BlastFurnace, "blast_furnace", 3, None)]
    #[case::a_brewing_stand(ScreenKind::BrewingStand, "brewing_stand", 5, None)]
    #[case::a_crafting_table(ScreenKind::Crafting, "crafting", 10, None)]
    #[case::an_enchanting_table(ScreenKind::Enchantment, "enchantment", 2, None)]
    #[case::a_furnace(ScreenKind::Furnace, "furnace", 3, None)]
    #[case::a_grindstone(ScreenKind::Grindstone, "grindstone", 3, None)]
    #[case::a_hopper(ScreenKind::Hopper, "hopper", 5, Some((1, 5)))]
    #[case::a_lectern(ScreenKind::Lectern, "lectern", 1, None)]
    #[case::a_loom(ScreenKind::Loom, "loom", 4, None)]
    #[case::a_merchant(ScreenKind::Merchant, "merchant", 3, None)]
    #[case::a_shulker_box(ScreenKind::ShulkerBox, "shulker_box", 27, Some((3, 9)))]
    #[case::a_smithing_table(ScreenKind::Smithing, "smithing", 4, None)]
    #[case::a_smoker(ScreenKind::Smoker, "smoker", 3, None)]
    #[case::a_cartography_table(ScreenKind::CartographyTable, "cartography_table", 3, None)]
    #[case::a_stonecutter(ScreenKind::Stonecutter, "stonecutter", 2, None)]
    fn each_kind_has_azaleas_slots_and_its_grid(
        #[case] kind: ScreenKind,
        #[case] name: &str,
        #[case] own: u16,
        #[case] grid: Option<(u8, u8)>,
    ) {
        assert_eq!(kind.as_str(), name);
        assert_eq!(kind.own_slots(), own);
        assert_eq!(kind.slots(), own + 36);
        assert_eq!(
            kind.grid(),
            grid.map(|(rows, columns)| Grid { rows, columns })
        );
        if let Some(grid) = kind.grid() {
            assert_eq!(u16::from(grid.rows) * u16::from(grid.columns), own);
        }
    }

    #[test]
    fn every_kind_is_listed_once() {
        let mut names: Vec<&str> = ScreenKind::ALL.iter().map(|kind| kind.as_str()).collect();
        names.sort_unstable();
        names.dedup();

        assert_eq!(names.len(), ScreenKind::ALL.len());
    }

    #[test]
    fn a_screens_own_slots_come_first_and_the_bots_after() {
        let chest = double_chest();

        assert_eq!(indexes(chest.own()), (0..54).collect::<Vec<u16>>());
        assert_eq!(indexes(chest.player()), (54..90).collect::<Vec<u16>>());
        assert_eq!(chest.slot(0).unwrap().item(), Some(&item("stone", 1)));
        assert_eq!(chest.slot(53).unwrap().item(), Some(&item("stone", 54)));
        assert_eq!(chest.slot(54).unwrap().item(), None);
        assert_eq!(chest.slot(89).unwrap().item(), None);
        assert_eq!(chest.slot(90), None);
    }

    #[test]
    fn the_inventorys_offhand_comes_after_the_bots_slots() {
        let inventory = Screen::new(
            screen_id(0, 1),
            StateId::new(1),
            ScreenKind::Inventory,
            "",
            (0..46).map(|_| None),
            None,
        );

        let mut own = (0..9).collect::<Vec<u16>>();
        own.push(45);
        assert_eq!(indexes(inventory.own()), own);
        assert_eq!(indexes(inventory.player()), (9..45).collect::<Vec<u16>>());
        assert_eq!(ScreenKind::Inventory.player_start(), 9);
        assert_eq!(ScreenKind::Generic9x3.player_start(), 27);
    }

    #[test]
    fn a_screen_has_its_kinds_slots_whatever_it_was_given() {
        let few = Screen::new(
            screen_id(1, 1),
            StateId::new(1),
            ScreenKind::Hopper,
            "",
            [Some(item("stone", 1))],
            None,
        );
        let many = Screen::new(
            screen_id(1, 1),
            StateId::new(1),
            ScreenKind::Hopper,
            "",
            (1..=500).map(|n| Some(item("stone", n))),
            None,
        );

        assert_eq!(few.slot(0).unwrap().item(), Some(&item("stone", 1)));
        assert_eq!(few.slot(1).unwrap().item(), None);
        assert_eq!(indexes(few.own()).len() + indexes(few.player()).len(), 41);
        assert_eq!(many.slot(40).unwrap().item(), Some(&item("stone", 41)));
        assert_eq!(many.slot(41), None);
    }

    #[test]
    fn a_grids_slots_have_their_row_and_column() {
        let chest = double_chest();
        let place = |index| chest.slot(index).unwrap().place();

        assert_eq!(place(0), Some(Place { row: 0, column: 0 }));
        assert_eq!(place(8), Some(Place { row: 0, column: 8 }));
        assert_eq!(place(9), Some(Place { row: 1, column: 0 }));
        assert_eq!(place(49), Some(Place { row: 5, column: 4 }));
        assert_eq!(place(53), Some(Place { row: 5, column: 8 }));
    }

    #[test]
    fn the_bots_slots_are_four_rows_of_nine_the_hotbar_last() {
        let chest = double_chest();
        let place = |index| chest.slot(index).unwrap().place();

        assert_eq!(place(54), Some(Place { row: 0, column: 0 }));
        assert_eq!(place(80), Some(Place { row: 2, column: 8 }));
        assert_eq!(place(81), Some(Place { row: 3, column: 0 }));
        assert_eq!(place(89), Some(Place { row: 3, column: 8 }));
    }

    #[test]
    fn a_screen_that_isnt_a_grid_has_no_places_of_its_own() {
        let anvil = Screen::new(
            screen_id(2, 1),
            StateId::new(1),
            ScreenKind::Anvil,
            "Repair & Name",
            [],
            None,
        );

        assert!(anvil.own().all(|slot| slot.place().is_none()));
        assert_eq!(
            anvil.slot(3).unwrap().place(),
            Some(Place { row: 0, column: 0 })
        );
        assert_eq!(anvil.grid(), None);
        assert_eq!(anvil.at(0, 0), None);
    }

    #[rstest]
    #[case::the_top_left(0, 0, Some(0))]
    #[case::the_middle_of_the_bottom_row(5, 4, Some(49))]
    #[case::the_bottom_right(5, 8, Some(53))]
    #[case::below_the_last_row(6, 0, None)]
    #[case::right_of_the_last_column(0, 9, None)]
    #[case::far_out(255, 255, None)]
    fn a_grids_slot_is_found_by_row_and_column(
        #[case] row: u8,
        #[case] column: u8,
        #[case] index: Option<u16>,
    ) {
        assert_eq!(double_chest().at(row, column).map(Slot::index), index);
    }

    #[rstest]
    #[case::a_hopper(ScreenKind::Hopper, 0, 0, Some(0))]
    #[case::a_hoppers_last(ScreenKind::Hopper, 0, 4, Some(4))]
    #[case::a_hoppers_second_row(ScreenKind::Hopper, 1, 0, None)]
    #[case::a_dispensers_middle(ScreenKind::Generic3x3, 1, 1, Some(4))]
    #[case::a_dispensers_fourth_column(ScreenKind::Generic3x3, 0, 3, None)]
    fn each_grid_has_its_own_width(
        #[case] kind: ScreenKind,
        #[case] row: u8,
        #[case] column: u8,
        #[case] index: Option<u16>,
    ) {
        let screen = Screen::new(screen_id(1, 1), StateId::new(1), kind, "", [], None);

        assert_eq!(screen.at(row, column).map(Slot::index), index);
    }

    #[rstest]
    #[case::a_double_chest(ScreenKind::Generic9x6, 0, 81)]
    #[case::a_double_chests_last(ScreenKind::Generic9x6, 8, 89)]
    #[case::a_chest(ScreenKind::Generic9x3, 0, 54)]
    #[case::an_anvil(ScreenKind::Anvil, 2, 32)]
    #[case::the_inventory(ScreenKind::Inventory, 0, 36)]
    #[case::the_inventorys_last(ScreenKind::Inventory, 8, 44)]
    fn a_hotbar_slot_has_its_number_in_each_screen(
        #[case] kind: ScreenKind,
        #[case] hotbar: u8,
        #[case] index: u16,
    ) {
        let screen = Screen::new(screen_id(1, 1), StateId::new(1), kind, "", [], None);

        assert_eq!(screen.hotbar(HotbarSlot::try_from(hotbar).unwrap()), index);
    }

    #[test]
    fn a_title_is_cleaned_and_bounded() {
        let shown = Screen::new(
            screen_id(1, 1),
            StateId::new(1),
            ScreenKind::Generic9x3,
            "\u{a7}8Auction\u{202e} House\n",
            [],
            None,
        );
        let long = Screen::new(
            screen_id(1, 1),
            StateId::new(1),
            ScreenKind::Generic9x3,
            &"\u{e001}".repeat(300),
            [],
            None,
        );

        assert_eq!(shown.title().as_str(), "Auction House");
        assert_eq!(long.title().as_str().chars().count(), Screen::MAX_TITLE);
    }

    #[test]
    fn a_screen_keeps_what_it_was_given() {
        let chest = Screen::new(
            screen_id(4, 9),
            StateId::new(12),
            ScreenKind::Generic9x3,
            "Chest",
            [],
            Some(item("diamond", 2)),
        );

        assert_eq!(chest.id(), screen_id(4, 9));
        assert_eq!(chest.id().window(), WindowId::new(4));
        assert_eq!(chest.state(), StateId::new(12));
        assert_eq!(chest.state().get(), 12);
        assert_eq!(chest.kind(), ScreenKind::Generic9x3);
        assert_eq!(chest.cursor(), Some(&item("diamond", 2)));
        assert_eq!(WindowId::INVENTORY.get(), 0);
    }

    fn offer(n: u32) -> TradeOffer {
        TradeOffer::new(item("emerald", n), None, item("bread", 1), false)
    }

    #[test]
    fn a_merchant_keeps_its_trades_in_order_and_at_most_64() {
        let merchant = Screen::new(
            screen_id(5, 1),
            StateId::new(1),
            ScreenKind::Merchant,
            "Farmer",
            [],
            None,
        )
        .with_offers((1..=100).map(offer));

        assert_eq!(merchant.offers().len(), Screen::MAX_OFFERS);
        assert_eq!(merchant.offers()[0], offer(1));
        assert_eq!(merchant.offers()[63], offer(64));
        let trade = &merchant.offers()[0];
        assert_eq!(trade.cost(), &item("emerald", 1));
        assert_eq!(trade.second_cost(), None);
        assert_eq!(trade.result(), &item("bread", 1));
        assert!(!trade.out_of_stock());
    }

    #[test]
    fn only_a_merchant_has_trades() {
        let chest = double_chest().with_offers([offer(1)]);

        assert_eq!(chest.offers(), []);
    }

    #[test]
    fn the_bots_inventory_shows_its_armour_its_slots_and_its_offhand() {
        let inventory = BotInventory::new((1..=46).map(|n| Some(item("stone", n))));

        assert_eq!(indexes(inventory.slots()), (5..46).collect::<Vec<u16>>());
        // The crafting grid isn't read.
        assert_eq!(inventory.slot(4), None);
        assert_eq!(inventory.slot(5).unwrap().item(), Some(&item("stone", 6)));
        assert_eq!(inventory.slot(5).unwrap().place(), None);
        assert_eq!(
            inventory.slot(9).unwrap().place(),
            Some(Place { row: 0, column: 0 })
        );
        assert_eq!(
            inventory.slot(44).unwrap().place(),
            Some(Place { row: 3, column: 8 })
        );
        assert_eq!(inventory.slot(45).unwrap().item(), Some(&item("stone", 46)));
        assert_eq!(inventory.slot(45).unwrap().place(), None);
        assert_eq!(inventory.slot(46), None);
    }

    #[test]
    fn an_inventory_given_nothing_is_empty() {
        let inventory = BotInventory::new([]);

        assert_eq!(inventory.slots().count(), 41);
        assert!(inventory.slots().all(|slot| slot.item().is_none()));
    }

    proptest! {
        /// Whatever a server sends, a screen has its kind's slots, each
        /// once and in order, split into its own and the bot's 36.
        #[test]
        fn any_screen_has_exactly_its_kinds_slots(
            kind in proptest::sample::select(ScreenKind::ALL.to_vec()),
            given in 0_usize..200,
        ) {
            let screen = Screen::new(
                screen_id(1, 1),
                StateId::new(1),
                kind,
                "",
                (0..given).map(|_| Some(item("stone", 1))),
                None,
            );

            let all: Vec<u16> = (0..kind.slots()).collect();
            let mut seen = indexes(screen.own());
            seen.extend(indexes(screen.player()));
            seen.sort_unstable();
            prop_assert_eq!(seen, all);
            prop_assert_eq!(screen.player().count(), 36);
            prop_assert_eq!(screen.own().count(), usize::from(kind.own_slots()));
            for index in 0..kind.slots() {
                prop_assert_eq!(screen.slot(index).map(Slot::index), Some(index));
                prop_assert_eq!(
                    screen.slot(index).unwrap().item().is_some(),
                    usize::from(index) < given
                );
            }
            prop_assert_eq!(screen.slot(kind.slots()), None);
        }

        /// Every slot of a grid is found where its place says.
        #[test]
        fn a_slots_place_finds_the_slot(kind in proptest::sample::select(ScreenKind::ALL.to_vec())) {
            let screen = Screen::new(screen_id(1, 1), StateId::new(1), kind, "", [], None);

            for slot in screen.own() {
                prop_assert_eq!(slot.place().is_some(), kind.grid().is_some());
                if let Some(place) = slot.place() {
                    prop_assert_eq!(screen.at(place.row, place.column), Some(slot));
                }
            }
        }
    }
}
