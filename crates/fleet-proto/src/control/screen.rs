//! What a bot sees, on demand (Plan.md P16.20; ADR-0036, M14): the server's
//! [`ScreenRequest`] and the agent's [`ScreenAnswer`].
//!
//! - **What crosses is [`Seen`]:** the bot's inventory, the screen it has
//!   open and the sign editor that waits, as a script reads them (P16.5),
//!   without what only the click guard reads: a screen's window, opening and
//!   state, an item's data components, a merchant's trades.
//! - **Reading rebuilds it through fleet-core's constructors,** so a name,
//!   a lore line and a title are cleaned and bounded as fleet-core bounds
//!   them, a slot gets its place as a script names it, and enchantments are
//!   read as fleet-core reads a server's. A slot outside its screen, one
//!   given twice, an unknown kind of screen and an item ID that isn't an
//!   identifier are refused: the agent sends only what fleet-core made.

use core::num::{NonZeroU32, NonZeroU64};

use fleet_core::id::BotId;
use fleet_core::mc::MenuView;
use fleet_core::script::menu::{
    BotInventory, Item, ItemData, Screen, ScreenKind, ServerText, SignSide,
};
use fleet_core::script::{BlockPosition, Identifier};

use crate::convert::{enum_from_wire, id_from_wire, id_to_wire, required};
use crate::generated as wire;
use crate::{ConvertError, Problem};

/// The server's number for a screen request: from 1, and the answer
/// repeats it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScreenRequestId(NonZeroU64);

impl ScreenRequestId {
    /// Wraps a request number.
    #[must_use]
    pub const fn new(number: NonZeroU64) -> Self {
        Self(number)
    }

    /// Returns the request number.
    #[must_use]
    pub const fn get(self) -> NonZeroU64 {
        self.0
    }
}

/// A user asks what a bot sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRequest {
    /// The request's number.
    pub id: ScreenRequestId,
    /// The bot.
    pub bot_id: BotId,
}

/// What a bot sees, or why its agent can't say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenAnswer {
    /// The request's number.
    pub id: ScreenRequestId,
    /// The bot.
    pub bot_id: BotId,
    /// The answer.
    pub outcome: ScreenOutcome,
}

/// The answer to a screen request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenOutcome {
    /// What the bot sees.
    Seen(Box<Seen>),
    /// Why the agent can't say.
    Unseen(ScreenUnseen),
}

/// Why an agent can't say what a bot sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScreenUnseen {
    /// The bot isn't in a world: offline, connecting, or between worlds.
    NotOnline,
    /// The agent is looking for this bot already, or for as many bots as it
    /// looks for at once.
    Busy,
    /// The bot's session didn't answer in time.
    TimedOut,
    /// The agent doesn't run the bot.
    UnknownBot,
}

impl ScreenUnseen {
    /// Every reason, in the wire's order.
    pub const ALL: [Self; 4] = [
        Self::NotOnline,
        Self::Busy,
        Self::TimedOut,
        Self::UnknownBot,
    ];
}

/// A sign editor that waits for its answer: where its sign is, and which
/// side it writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SeenSign {
    /// The sign's block.
    pub at: BlockPosition,
    /// The side the editor writes.
    pub side: SignSide,
}

/// What a bot sees, as it crosses the wire: a [`MenuView`] without what only
/// the click guard reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    /// The bot's own inventory.
    pub inventory: BotInventory,
    /// The screen it has open: window 0, opening 0 and state 0, and no
    /// trades, whatever the agent's was.
    pub screen: Option<Screen>,
    /// The sign editor that waits for its answer.
    pub sign: Option<SeenSign>,
}

/// How many slots of window 0 a [`BotInventory`] is read from: the crafting
/// result and grid, the armour, the 36 slots and the offhand.
const INVENTORY_SLOTS: u16 = 46;

impl Seen {
    /// What `view` shows of the bot: its items without their data
    /// components, and its screen without its window, opening, state and
    /// trades.
    #[must_use]
    pub fn of(view: &MenuView) -> Self {
        let item = |slot: Option<&fleet_core::script::menu::Slot>| {
            slot.and_then(fleet_core::script::menu::Slot::item)
                .map(plain)
        };
        Self {
            inventory: BotInventory::new(
                (0..INVENTORY_SLOTS).map(|index| item(view.inventory.slot(index))),
            ),
            screen: view.screen.as_ref().map(|screen| {
                let kind = screen.kind();
                Screen::shown(
                    kind,
                    screen.title().as_str(),
                    (0..kind.slots()).map(|index| item(screen.slot(index))),
                    screen.cursor().map(plain),
                )
            }),
            sign: view.sign.map(|editor| SeenSign {
                at: editor.at(),
                side: editor.side(),
            }),
        }
    }
}

/// `item` without its data components, as [`Seen`] holds it.
fn plain(item: &Item) -> Item {
    Item::new(item.id().clone(), item.count(), ItemData::new(Vec::new()))
        .with_durability(item.durability())
        .named(item.name().as_str())
        .with_lore(item.lore().lines().iter().map(ServerText::as_str))
        .with_potion(item.potion().map(Identifier::as_str))
        .with_enchantments(levels(item.enchantments()))
        .with_stored_enchantments(levels(item.stored_enchantments()))
}

/// Enchantments as fleet-core reads a server's.
fn levels(
    enchantments: &fleet_core::script::menu::Enchantments,
) -> impl Iterator<Item = (&str, i32)> {
    enchantments
        .iter()
        .map(|(id, level)| (id.as_str(), i32::from(level)))
}

// --- Writing -------------------------------------------------------------------------

impl From<ScreenRequest> for wire::ScreenRequest {
    fn from(request: ScreenRequest) -> Self {
        Self {
            id: request.id.get().get(),
            bot_id: id_to_wire(request.bot_id),
        }
    }
}

impl From<ScreenAnswer> for wire::ScreenAnswer {
    fn from(answer: ScreenAnswer) -> Self {
        use wire::screen_answer::Answer;
        Self {
            id: answer.id.get().get(),
            bot_id: id_to_wire(answer.bot_id),
            answer: Some(match answer.outcome {
                ScreenOutcome::Seen(seen) => Answer::Seen(Box::new(seen_to_wire(&seen))),
                ScreenOutcome::Unseen(why) => Answer::Unseen(
                    match why {
                        ScreenUnseen::NotOnline => wire::ScreenUnseen::NotOnline,
                        ScreenUnseen::Busy => wire::ScreenUnseen::Busy,
                        ScreenUnseen::TimedOut => wire::ScreenUnseen::TimedOut,
                        ScreenUnseen::UnknownBot => wire::ScreenUnseen::UnknownBot,
                    }
                    .into(),
                ),
            }),
        }
    }
}

fn seen_to_wire(seen: &Seen) -> wire::Seen {
    wire::Seen {
        inventory: slots_to_wire(seen.inventory.slots()),
        screen: seen.screen.as_ref().map(|screen| {
            let kind = screen.kind();
            wire::SeenScreen {
                kind: kind.as_str().to_owned(),
                title: screen.title().as_str().to_owned(),
                slots: slots_to_wire((0..kind.slots()).filter_map(|index| screen.slot(index))),
                cursor: screen.cursor().map(item_to_wire),
            }
        }),
        sign: seen.sign.map(|sign| wire::SeenSign {
            x: sign.at.x,
            y: sign.at.y,
            z: sign.at.z,
            front: sign.side == SignSide::Front,
        }),
    }
}

fn slots_to_wire<'a>(
    slots: impl Iterator<Item = &'a fleet_core::script::menu::Slot>,
) -> Vec<wire::SeenSlot> {
    slots
        .filter_map(|slot| {
            slot.item().map(|item| wire::SeenSlot {
                index: u32::from(slot.index()),
                item: Some(item_to_wire(item)),
            })
        })
        .collect()
}

fn enchantments_to_wire(
    enchantments: &fleet_core::script::menu::Enchantments,
) -> Vec<wire::SeenEnchantment> {
    enchantments
        .iter()
        .map(|(id, level)| wire::SeenEnchantment {
            id: id.as_str().to_owned(),
            level: u32::from(level),
        })
        .collect()
}

fn item_to_wire(item: &Item) -> wire::SeenItem {
    wire::SeenItem {
        id: item.id().as_str().to_owned(),
        count: item.count().get(),
        name: item.name().as_str().to_owned(),
        lore: item
            .lore()
            .lines()
            .iter()
            .map(|line| line.as_str().to_owned())
            .collect(),
        potion: item
            .potion()
            .map(|potion| potion.as_str().to_owned())
            .unwrap_or_default(),
        enchantments: enchantments_to_wire(item.enchantments()),
        stored_enchantments: enchantments_to_wire(item.stored_enchantments()),
        durability: item.durability(),
    }
}

// --- Reading -------------------------------------------------------------------------

impl TryFrom<wire::ScreenRequest> for ScreenRequest {
    type Error = ConvertError;

    fn try_from(request: wire::ScreenRequest) -> Result<Self, ConvertError> {
        Ok(Self {
            id: screen_number("ScreenRequest.id", request.id)?,
            bot_id: id_from_wire("ScreenRequest.bot_id", &request.bot_id)?,
        })
    }
}

impl TryFrom<wire::ScreenAnswer> for ScreenAnswer {
    type Error = ConvertError;

    fn try_from(answer: wire::ScreenAnswer) -> Result<Self, ConvertError> {
        use wire::screen_answer::Answer;
        const UNSEEN: &str = "ScreenAnswer.unseen";
        let outcome = match required("ScreenAnswer.answer", answer.answer)? {
            Answer::Seen(seen) => ScreenOutcome::Seen(Box::new(seen_from_wire(*seen)?)),
            Answer::Unseen(why) => ScreenOutcome::Unseen(match enum_from_wire(UNSEEN, why)? {
                wire::ScreenUnseen::NotOnline => ScreenUnseen::NotOnline,
                wire::ScreenUnseen::Busy => ScreenUnseen::Busy,
                wire::ScreenUnseen::TimedOut => ScreenUnseen::TimedOut,
                wire::ScreenUnseen::UnknownBot => ScreenUnseen::UnknownBot,
                wire::ScreenUnseen::Unspecified => {
                    return Err(ConvertError::new(UNSEEN, Problem::Missing));
                }
            }),
        };
        Ok(Self {
            id: screen_number("ScreenAnswer.id", answer.id)?,
            bot_id: id_from_wire("ScreenAnswer.bot_id", &answer.bot_id)?,
            outcome,
        })
    }
}

/// A screen request's number, from 1.
fn screen_number(field: &'static str, number: u64) -> Result<ScreenRequestId, ConvertError> {
    NonZeroU64::new(number)
        .map(ScreenRequestId::new)
        .ok_or(ConvertError::new(field, Problem::Missing))
}

fn seen_from_wire(seen: wire::Seen) -> Result<Seen, ConvertError> {
    let inventory = items_from_wire("Seen.inventory", seen.inventory, 5..INVENTORY_SLOTS)?;
    let screen = seen
        .screen
        .map(|screen| {
            const KIND: &str = "SeenScreen.kind";
            let kind = ScreenKind::ALL
                .into_iter()
                .find(|kind| kind.as_str() == screen.kind)
                .ok_or(ConvertError::new(KIND, Problem::Unknown))?;
            let items = items_from_wire("SeenScreen.slots", screen.slots, 0..kind.slots())?;
            let cursor = screen.cursor.as_ref().map(item_from_wire).transpose()?;
            Ok::<_, ConvertError>(Screen::shown(kind, &screen.title, items, cursor))
        })
        .transpose()?;
    Ok(Seen {
        inventory: BotInventory::new(inventory),
        screen,
        sign: seen.sign.map(|sign| SeenSign {
            at: BlockPosition {
                x: sign.x,
                y: sign.y,
                z: sign.z,
            },
            side: if sign.front {
                SignSide::Front
            } else {
                SignSide::Back
            },
        }),
    })
}

/// The items of `slots`, by their numbers, which must be in `numbers`, each
/// once: every slot from 0 to the range's end, empty where none is given.
fn items_from_wire(
    field: &'static str,
    slots: Vec<wire::SeenSlot>,
    numbers: core::ops::Range<u16>,
) -> Result<Vec<Option<Item>>, ConvertError> {
    let mut items: Vec<Option<Item>> = vec![None; usize::from(numbers.end)];
    let max = usize::from(numbers.end - numbers.start);
    if slots.len() > max {
        return Err(ConvertError::new(
            field,
            Problem::TooMany {
                count: slots.len(),
                max,
            },
        ));
    }
    for slot in slots {
        let index = u16::try_from(slot.index)
            .ok()
            .filter(|index| numbers.contains(index))
            .ok_or(ConvertError::new(field, Problem::OutOfRange))?;
        let place = items
            .get_mut(usize::from(index))
            .ok_or(ConvertError::new(field, Problem::OutOfRange))?;
        if place.is_some() {
            return Err(ConvertError::new(field, Problem::DuplicateSlot));
        }
        *place = Some(item_from_wire(&required("SeenSlot.item", slot.item)?)?);
    }
    Ok(items)
}

fn item_from_wire(item: &wire::SeenItem) -> Result<Item, ConvertError> {
    let id = Identifier::try_from(item.id.as_str())
        .map_err(|_| ConvertError::new("SeenItem.id", Problem::NotAnIdentifier))?;
    let count =
        NonZeroU32::new(item.count).ok_or(ConvertError::new("SeenItem.count", Problem::Missing))?;
    let level = |level: u32| i32::try_from(level).unwrap_or(i32::MAX);
    Ok(Item::new(id, count, ItemData::new(Vec::new()))
        .with_durability(item.durability)
        .named(&item.name)
        .with_lore(&item.lore)
        .with_potion((!item.potion.is_empty()).then_some(item.potion.as_str()))
        .with_enchantments(
            item.enchantments
                .iter()
                .map(|enchantment| (enchantment.id.as_str(), level(enchantment.level))),
        )
        .with_stored_enchantments(
            item.stored_enchantments
                .iter()
                .map(|enchantment| (enchantment.id.as_str(), level(enchantment.level))),
        ))
}
