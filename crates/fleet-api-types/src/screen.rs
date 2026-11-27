//! What a bot sees, on demand (Plan.md P16.20; ADR-0036, M14):
//! `GET /api/v1/bots/{id}/screen`'s answer.
//!
//! - **Each slot as a script names it:** its number (`menu.slot(n)`,
//!   `inventory.slot(n)`), and in a grid its row and column from the top
//!   left (`menu.at(row, column)`). A screen's own slots and the bot's 36
//!   in it are told apart, as `menu.slots` and `menu.player` are.
//! - **Each item as a script reads it:** its registry ID, count, name, lore
//!   and components (a potion's kind, enchantments with their levels), and
//!   the uses it has left. Every text is a Minecraft server's, cleaned and
//!   bounded by fleet-core when the agent's answer was read, and shown as
//!   plain text.
//! - **Without a screen open,** the bot's own inventory, numbered as its
//!   inventory screen numbers it: what a script reads in `inventory`.

use fleet_core::script::menu::{BotInventory, Enchantments, Item, Screen, SignSide, Slot};
use fleet_core::script::{BlockPosition, Identifier};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use utoipa::{PartialSchema as _, ToSchema};

use crate::int::integer_schema;
use crate::open::{Open, OpenEnum};
use crate::text::BoundedText;

/// The most characters of a registry identifier (`Identifier::MAX_LEN`).
pub const SCREEN_ID_MAX_CHARS: usize = Identifier::MAX_LEN;
/// The most characters of a screen's kind, as `menu.kind` reads it.
pub const SCREEN_KIND_MAX_CHARS: usize = 32;
/// The most characters of a screen's title (`Screen::MAX_TITLE`).
pub const SCREEN_TITLE_MAX_CHARS: usize = Screen::MAX_TITLE;
/// The most characters of an item's name (`Item::MAX_NAME`).
pub const ITEM_NAME_MAX_CHARS: usize = Item::MAX_NAME;
/// The most characters of a lore line (`Lore::MAX_LINE`).
pub const LORE_LINE_MAX_CHARS: usize = fleet_core::script::menu::Lore::MAX_LINE;

/// What the answer says of the bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BotScreenKind {
    /// It has a screen open: `screen` holds it.
    Screen,
    /// It has none open: `inventory` holds its own slots.
    Inventory,
    /// It isn't in a world on a live agent.
    Offline,
    /// Its agent runs a version before script API level 2, which can't be
    /// asked: it needs the next release.
    AgentTooOld,
    /// Its agent didn't answer in time.
    NoAnswer,
}

impl OpenEnum for BotScreenKind {
    const ALL: &'static [Self] = &[
        Self::Screen,
        Self::Inventory,
        Self::Offline,
        Self::AgentTooOld,
        Self::NoAnswer,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Screen => "screen",
            Self::Inventory => "inventory",
            Self::Offline => "offline",
            Self::AgentTooOld => "agent_too_old",
            Self::NoAnswer => "no_answer",
        }
    }
}

/// `GET /bots/{id}/screen`: what the bot sees, or why it can't be said.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
pub struct BotScreenResponse {
    /// What it says of the bot.
    #[ts(as = "BotScreenKind")]
    #[schema(value_type = BotScreenKind)]
    pub kind: Open<BotScreenKind>,
    /// `screen`: the screen it has open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub screen: Option<BotScreenView>,
    /// `inventory`: its own slots, numbered as `inventory.slots` numbers
    /// them: its armour 5 to 8, its 36 slots 9 to 44, its offhand 45.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub inventory: Option<Vec<BotScreenSlot>>,
    /// `screen`, `inventory`: the sign editor that waits for its answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub sign: Option<BotScreenSign>,
}

/// A screen a bot has open.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
pub struct BotScreenView {
    /// Its kind, as `menu.kind` reads it: `generic_9x3`, `anvil`.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<SCREEN_KIND_MAX_CHARS>::schema)]
    pub kind: BoundedText<SCREEN_KIND_MAX_CHARS>,
    /// Its title, as plain text.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<SCREEN_TITLE_MAX_CHARS>::schema)]
    pub title: BoundedText<SCREEN_TITLE_MAX_CHARS>,
    /// A grid's rows, as `menu.rows`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = integer_schema::<0, { u8::MAX as u32 }>, nullable = false)]
    pub rows: Option<u8>,
    /// A grid's columns, as `menu.columns`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = integer_schema::<0, { u8::MAX as u32 }>, nullable = false)]
    pub columns: Option<u8>,
    /// Its own slots, in order, as `menu.slots` numbers them.
    pub slots: Vec<BotScreenSlot>,
    /// The bot's 36 slots in it, as `menu.player` numbers them: four rows
    /// of nine, the hotbar last.
    pub player: Vec<BotScreenSlot>,
    /// The item on the cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub cursor: Option<BotScreenItem>,
}

/// A slot, as a script names it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
pub struct BotScreenSlot {
    /// Its number.
    #[schema(schema_with = integer_schema::<0, { u16::MAX as u32 }>)]
    pub index: u16,
    /// Its row in its grid, from 0 at the top.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = integer_schema::<0, { u8::MAX as u32 }>, nullable = false)]
    pub row: Option<u8>,
    /// Its column in its grid, from 0 at the left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = integer_schema::<0, { u8::MAX as u32 }>, nullable = false)]
    pub column: Option<u8>,
    /// What it holds; nothing when it's empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub item: Option<BotScreenItem>,
}

/// An item, as a script reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, ToSchema)]
pub struct BotScreenItem {
    /// Its registry identifier: `minecraft:potion`.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<SCREEN_ID_MAX_CHARS>::schema)]
    pub id: BoundedText<SCREEN_ID_MAX_CHARS>,
    /// How many.
    #[schema(schema_with = integer_schema::<1, { u32::MAX }>)]
    pub count: u32,
    /// The name the client shows, as plain text.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<ITEM_NAME_MAX_CHARS>::schema)]
    pub name: BoundedText<ITEM_NAME_MAX_CHARS>,
    /// Its lore's lines, as plain text.
    #[ts(as = "Vec<String>")]
    #[schema(schema_with = lore_schema)]
    pub lore: Vec<BoundedText<LORE_LINE_MAX_CHARS>>,
    /// A potion's kind: `minecraft:turtle_master`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = BoundedText::<SCREEN_ID_MAX_CHARS>::schema, nullable = false)]
    pub potion: Option<BoundedText<SCREEN_ID_MAX_CHARS>>,
    /// Its enchantments.
    pub enchantments: Vec<BotScreenEnchantment>,
    /// An enchanted book's stored enchantments.
    pub stored_enchantments: Vec<BotScreenEnchantment>,
    /// The uses it has left, for an item that wears.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = integer_schema::<0, { u32::MAX }>, nullable = false)]
    pub durability: Option<u32>,
}

/// An enchantment and its level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct BotScreenEnchantment {
    /// Its registry identifier: `minecraft:sharpness`.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<SCREEN_ID_MAX_CHARS>::schema)]
    pub id: BoundedText<SCREEN_ID_MAX_CHARS>,
    /// Its level.
    #[schema(schema_with = integer_schema::<0, { u8::MAX as u32 }>)]
    pub level: u8,
}

/// A sign editor that waits for its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct BotScreenSign {
    /// The sign's block: x.
    #[schema(schema_with = coordinate_schema)]
    pub x: i32,
    /// The sign's block: y.
    #[schema(schema_with = coordinate_schema)]
    pub y: i32,
    /// The sign's block: z.
    #[schema(schema_with = coordinate_schema)]
    pub z: i32,
    /// Whether it writes the sign's front; its back otherwise.
    pub front: bool,
}

impl BotScreenResponse {
    /// An answer of `kind` that shows nothing of the bot: offline, an old
    /// agent, no answer.
    #[must_use]
    pub fn unseen(kind: BotScreenKind) -> Self {
        Self {
            kind: kind.into(),
            screen: None,
            inventory: None,
            sign: None,
        }
    }

    /// `screen` the bot has open, with `sign`'s editor if one waits.
    #[must_use]
    pub fn of_screen(screen: &Screen, sign: Option<(BlockPosition, SignSide)>) -> Self {
        let grid = screen.grid();
        Self {
            kind: BotScreenKind::Screen.into(),
            screen: Some(BotScreenView {
                kind: BoundedText::new(screen.kind().as_str()),
                title: BoundedText::new(screen.title().as_str()),
                rows: grid.map(|grid| grid.rows),
                columns: grid.map(|grid| grid.columns),
                slots: screen.own().map(slot).collect(),
                player: screen.player().map(slot).collect(),
                cursor: screen.cursor().map(item),
            }),
            inventory: None,
            sign: sign.map(sign_of),
        }
    }

    /// The bot's own `inventory`, with no screen open, and `sign`'s editor
    /// if one waits.
    #[must_use]
    pub fn of_inventory(inventory: &BotInventory, sign: Option<(BlockPosition, SignSide)>) -> Self {
        Self {
            kind: BotScreenKind::Inventory.into(),
            screen: None,
            inventory: Some(inventory.slots().map(slot).collect()),
            sign: sign.map(sign_of),
        }
    }
}

fn sign_of((at, side): (BlockPosition, SignSide)) -> BotScreenSign {
    BotScreenSign {
        x: at.x,
        y: at.y,
        z: at.z,
        front: side == SignSide::Front,
    }
}

fn slot(slot: &Slot) -> BotScreenSlot {
    let place = slot.place();
    BotScreenSlot {
        index: slot.index(),
        row: place.map(|place| place.row),
        column: place.map(|place| place.column),
        item: slot.item().map(item),
    }
}

fn enchantments(enchantments: &Enchantments) -> Vec<BotScreenEnchantment> {
    enchantments
        .iter()
        .map(|(id, level)| BotScreenEnchantment {
            id: BoundedText::new(id.as_str()),
            level,
        })
        .collect()
}

fn item(item: &Item) -> BotScreenItem {
    BotScreenItem {
        id: BoundedText::new(item.id().as_str()),
        count: item.count().get(),
        name: BoundedText::new(item.name().as_str()),
        lore: item
            .lore()
            .lines()
            .iter()
            .map(|line| BoundedText::new(line.as_str()))
            .collect(),
        potion: item
            .potion()
            .map(|potion| BoundedText::new(potion.as_str())),
        enchantments: enchantments(item.enchantments()),
        stored_enchantments: enchantments(item.stored_enchantments()),
        durability: item.durability(),
    }
}

/// Lore lines: texts of at most [`LORE_LINE_MAX_CHARS`] characters.
fn lore_schema() -> RefOr<Schema> {
    utoipa::openapi::schema::ArrayBuilder::new()
        .items(BoundedText::<LORE_LINE_MAX_CHARS>::schema())
        .into()
}

/// A block's coordinate: any `i32`.
fn coordinate_schema() -> RefOr<Schema> {
    ObjectBuilder::new()
        .schema_type(Type::Integer)
        .minimum(Some(i32::MIN))
        .maximum(Some(i32::MAX))
        .into()
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;

    use fleet_core::script::menu::{ItemData, ScreenKind};
    use serde_json::json;

    use super::*;

    fn potion() -> Item {
        Item::new(
            Identifier::try_from("minecraft:potion").unwrap(),
            NonZeroU32::new(3).unwrap(),
            ItemData::new(Vec::new()),
        )
        .named("Potion of the Turtle Master")
        .with_lore(["Price: 1,250", "Seller: AfkBot2"])
        .with_potion(Some("minecraft:turtle_master"))
    }

    fn sword() -> Item {
        Item::new(
            Identifier::try_from("minecraft:diamond_sword").unwrap(),
            NonZeroU32::MIN,
            ItemData::new(Vec::new()),
        )
        .with_durability(Some(1500))
        .with_enchantments([("minecraft:sharpness", 5)])
    }

    #[test]
    fn a_screens_slots_have_their_numbers_rows_and_columns_as_a_script_names_them() {
        let chest = Screen::shown(
            ScreenKind::Generic9x3,
            "Auction House",
            (0..63).map(|n| (n == 13).then(potion).or_else(|| (n == 54).then(sword))),
            Some(sword()),
        );

        let answer = BotScreenResponse::of_screen(&chest, None);

        let screen = answer.screen.unwrap();
        assert_eq!(answer.kind, BotScreenKind::Screen.into());
        assert_eq!(
            (screen.kind.as_str(), screen.title.as_str()),
            ("generic_9x3", "Auction House")
        );
        assert_eq!((screen.rows, screen.columns), (Some(3), Some(9)));
        assert_eq!(screen.slots.len(), 27);
        let listing = &screen.slots[13];
        assert_eq!(
            (listing.index, listing.row, listing.column),
            (13, Some(1), Some(4))
        );
        let item = listing.item.as_ref().unwrap();
        assert_eq!(
            (item.id.as_str(), item.count, item.name.as_str()),
            ("minecraft:potion", 3, "Potion of the Turtle Master")
        );
        assert_eq!(item.lore[1].as_str(), "Seller: AfkBot2");
        assert_eq!(
            item.potion.as_ref().map(BoundedText::as_str),
            Some("minecraft:turtle_master")
        );
        assert_eq!(screen.player.len(), 36);
        let first_of_the_bots = &screen.player[0];
        assert_eq!(
            (
                first_of_the_bots.index,
                first_of_the_bots.row,
                first_of_the_bots.column
            ),
            (27, Some(0), Some(0))
        );
        assert_eq!(screen.cursor.unwrap().durability, Some(1500));
        assert_eq!(answer.inventory, None);
    }

    #[test]
    fn without_a_screen_the_bots_own_slots_are_numbered_as_its_inventory_screen_numbers_them() {
        let inventory = BotInventory::new((0..46).map(|n| (n == 36).then(sword)));

        let answer = BotScreenResponse::of_inventory(
            &inventory,
            Some((
                BlockPosition {
                    x: -3,
                    y: 64,
                    z: 12,
                },
                SignSide::Back,
            )),
        );

        let slots = answer.inventory.unwrap();
        assert_eq!(answer.kind, BotScreenKind::Inventory.into());
        assert_eq!(slots.first().map(|slot| slot.index), Some(5));
        assert_eq!(slots.last().map(|slot| slot.index), Some(45));
        let hotbar = slots.iter().find(|slot| slot.index == 36).unwrap();
        let sword = hotbar.item.as_ref().unwrap();
        assert_eq!(
            sword
                .enchantments
                .iter()
                .map(|enchantment| (enchantment.id.as_str(), enchantment.level))
                .collect::<Vec<_>>(),
            [("minecraft:sharpness", 5)]
        );
        assert_eq!(
            answer.sign,
            Some(BotScreenSign {
                x: -3,
                y: 64,
                z: 12,
                front: false
            })
        );
    }

    #[test]
    fn an_unseen_bot_says_why_and_nothing_else() {
        let answer = BotScreenResponse::unseen(BotScreenKind::AgentTooOld);

        assert_eq!(
            serde_json::to_value(&answer).unwrap(),
            json!({ "kind": "agent_too_old" })
        );
    }

    #[test]
    fn every_open_name_is_its_serde_name() {
        for kind in BotScreenKind::ALL {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                json!(kind.as_str()),
                "{kind:?}"
            );
        }
    }
}
