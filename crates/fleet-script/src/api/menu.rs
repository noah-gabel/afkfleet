//! `inventory`, `menu`, `sign`, and a slot's own helpers: the bot's
//! inventory, a server's screens and its sign editors (Plan.md P16.12;
//! ADR-0036, M1 to M18; ADR-0052). API level 2.
//!
//! **Everything a script reads is plain data:** a slot is a map of its
//! place and its item, a trade a flat map, so a script can keep any of them
//! in its state. A slot read in one hook can be clicked in a later one: the
//! click goes out only if that hook sees the same item on the same screen
//! there ([`Target::Read`]).
//!
//! **A list is of numbers, never of maps,** and a slot's map keeps its lore
//! lines while they fit: Rhai counts a value's map entries and its text
//! over everything nested in it, and the sandbox takes 64 entries and
//! 1,024 bytes a value (ADR-0036, D3). A slot is at most 45 entries (13
//! fields and 16 enchantments of each kind), a trade at most 42, so neither
//! a list of them nor what a server sends can make a value the script can't
//! hold. The lore helpers read every line, from what the hook sees
//! (ADR-0052).

use fleet_core::script::DecimalMark;
use fleet_core::script::amount::find_amount;
use fleet_core::script::menu::{
    BotInventory, ClickKind, Enchantments, Item, Lore, Screen, ScreenId, Slot, TradeOffer,
};
use rhai::{Array, Dynamic, EvalAltResult, FuncRegistration, INT, Map, Module};

use super::{Registered, Returns, int, misuse, result, strict, text, values, whole};
use crate::error::Misuse;
use crate::turn::{ItemView, SharedTurn, Target, kept_lore, lock, number_key};

/// `inventory`: the bot's own slots.
#[derive(Debug, Clone)]
pub(super) struct Inventory(pub(super) SharedTurn);
/// `menu`: the screen that's open.
#[derive(Debug, Clone)]
pub(super) struct Menu(pub(super) SharedTurn);
/// `sign`: the sign editor the server opened.
#[derive(Debug, Clone)]
pub(super) struct Sign(pub(super) SharedTurn);

/// The first hotbar slot in the inventory's own numbering: 36 to 44.
const HOTBAR: u16 = 36;

/// A map of enchantments: each ID to its level.
fn levels(enchantments: &Enchantments) -> Dynamic {
    let map: Map = enchantments
        .iter()
        .map(|(id, level)| (id.as_str().into(), int(level)))
        .collect();
    Dynamic::from_map(map)
}

/// What a script reads of an item, into `map`: an empty slot's ID, potion
/// and durability are `()`, its count 0, its name empty, and it has no
/// lore and no enchantments.
fn item_fields(map: &mut Map, item: Option<&Item>) {
    let text_of = |text: &str| Dynamic::from(text.to_owned());
    map.insert(
        "id".into(),
        item.map_or(Dynamic::UNIT, |item| text_of(item.id().as_str())),
    );
    map.insert(
        "count".into(),
        int(item.map_or(0, |item| item.count().get())),
    );
    map.insert(
        "name".into(),
        item.map_or_else(|| text_of(""), |item| text_of(item.name().as_str())),
    );
    let lore: Array = item
        .map(|item| kept_lore(item).into_iter().map(Dynamic::from).collect())
        .unwrap_or_default();
    map.insert("lore".into(), Dynamic::from_array(lore));
    map.insert(
        "durability".into(),
        item.and_then(Item::durability).map_or(Dynamic::UNIT, int),
    );
    map.insert(
        "potion".into(),
        item.and_then(Item::potion)
            .map_or(Dynamic::UNIT, |potion| text_of(potion.as_str())),
    );
    let none = Enchantments::read(core::iter::empty());
    map.insert(
        "enchantments".into(),
        levels(item.map_or(&none, Item::enchantments)),
    );
    map.insert(
        "stored_enchantments".into(),
        levels(item.map_or(&none, Item::stored_enchantments)),
    );
}

/// An item as a script reads it: the cursor's, a trade's.
fn item(item: Option<&Item>) -> Dynamic {
    let Some(item) = item else {
        return Dynamic::UNIT;
    };
    let mut map = Map::new();
    item_fields(&mut map, Some(item));
    Dynamic::from_map(map)
}

/// A slot as a script reads it: its number, its row and column in a grid
/// (`()` outside one), the screen it was read on (`()` for the inventory
/// outside a screen), whether it's empty, and its item's fields.
fn slot(slot: &Slot, screen: Option<ScreenId>) -> Dynamic {
    let mut map = Map::new();
    map.insert("index".into(), int(slot.index()));
    let place = slot.place();
    map.insert(
        "row".into(),
        place.map_or(Dynamic::UNIT, |place| int(place.row)),
    );
    map.insert(
        "column".into(),
        place.map_or(Dynamic::UNIT, |place| int(place.column)),
    );
    map.insert("screen".into(), screen.map_or(Dynamic::UNIT, opening));
    map.insert("empty".into(), Dynamic::from_bool(slot.item().is_none()));
    item_fields(&mut map, slot.item());
    Dynamic::from_map(map)
}

/// A screen's opening as a script's number. A session opens far fewer
/// than `i64::MAX` screens.
fn opening(screen: ScreenId) -> Dynamic {
    INT::try_from(screen.opening()).map_or(Dynamic::UNIT, Dynamic::from_int)
}

/// The numbers of `found`, as a script loops over them.
fn numbers<'a>(found: impl Iterator<Item = &'a Slot>) -> Array {
    found.map(|found| int(found.index())).collect()
}

/// A trade as a script reads it: one flat map, its cost's and its second
/// cost's ID and count, its result's ID, count, name, potion and both kinds
/// of enchantments, and whether it's out of stock.
fn trade(offer: &TradeOffer) -> Dynamic {
    let mut map = Map::new();
    let text_of = |text: &str| Dynamic::from(text.to_owned());
    map.insert("cost_id".into(), text_of(offer.cost().id().as_str()));
    map.insert("cost_count".into(), int(offer.cost().count().get()));
    let second = offer.second_cost();
    map.insert(
        "second_id".into(),
        second.map_or(Dynamic::UNIT, |second| text_of(second.id().as_str())),
    );
    map.insert(
        "second_count".into(),
        int(second.map_or(0, |second| second.count().get())),
    );
    let result = offer.result();
    map.insert("result_id".into(), text_of(result.id().as_str()));
    map.insert("result_count".into(), int(result.count().get()));
    map.insert("result_name".into(), text_of(result.name().as_str()));
    map.insert(
        "result_potion".into(),
        result
            .potion()
            .map_or(Dynamic::UNIT, |potion| text_of(potion.as_str())),
    );
    map.insert("result_enchantments".into(), levels(result.enchantments()));
    map.insert(
        "result_stored_enchantments".into(),
        levels(result.stored_enchantments()),
    );
    map.insert(
        "out_of_stock".into(),
        Dynamic::from_bool(offer.out_of_stock()),
    );
    Dynamic::from_map(map)
}

/// A text of a map's.
fn field_text(map: &Map, key: &str) -> Option<String> {
    map.get(key).and_then(text)
}

/// A whole number of a map's.
fn field_int(map: &Map, key: &str) -> Option<i64> {
    map.get(key)?.as_int().ok()
}

/// `()` as `None`, a value as `Some(read(value))`; an unreadable one as
/// `Err`.
fn optional<T>(
    value: Option<&Dynamic>,
    read: impl FnOnce(&Dynamic) -> Option<T>,
) -> Result<Option<T>, ()> {
    match value {
        None => Err(()),
        Some(value) if value.is_unit() => Ok(None),
        Some(value) => read(value).map(Some).ok_or(()),
    }
}

/// A map of enchantments as a script holds it.
fn enchantments_of(value: Option<&Dynamic>) -> Option<Vec<(String, u8)>> {
    let map = value?.as_map_ref().ok()?;
    map.iter()
        .map(|(id, level)| {
            let level = u8::try_from(level.as_int().ok()?).ok()?;
            Some((id.to_string(), level))
        })
        .collect()
}

/// The item a slot's map says it held: `Ok(None)` for an empty slot,
/// `Err` for a map that holds no slot.
fn read_item(map: &Map) -> Result<Option<ItemView>, ()> {
    let empty = map
        .get("empty")
        .and_then(|empty| empty.as_bool().ok())
        .ok_or(())?;
    if empty {
        return Ok(None);
    }
    read_held(map).map(Some).ok_or(())
}

/// The item of a slot's map that isn't empty; `None` for a map that holds
/// none.
fn read_held(map: &Map) -> Option<ItemView> {
    let lore = map
        .get("lore")?
        .as_array_ref()
        .ok()?
        .iter()
        .map(text)
        .collect::<Option<Vec<String>>>()?;
    Some(ItemView {
        id: field_text(map, "id")?,
        count: u32::try_from(field_int(map, "count")?).ok()?,
        name: field_text(map, "name")?,
        lore,
        durability: optional(map.get("durability"), |value| {
            u32::try_from(value.as_int().ok()?).ok()
        })
        .ok()?,
        potion: optional(map.get("potion"), text).ok()?,
        enchantments: enchantments_of(map.get("enchantments"))?,
        stored_enchantments: enchantments_of(map.get("stored_enchantments"))?,
    })
}

/// What a script names to click: a slot's number, or a slot it read.
fn target(value: &Dynamic) -> Target {
    if let Some(index) = whole(value) {
        return u16::try_from(index).map_or(Target::Bad, Target::Index);
    }
    let Ok(map) = value.as_map_ref() else {
        return Target::Bad;
    };
    let index = field_int(&map, "index").and_then(|index| u16::try_from(index).ok());
    let screen = optional(map.get("screen"), |value| {
        u64::try_from(value.as_int().ok()?).ok()
    });
    match (index, screen, read_item(&map)) {
        (Some(index), Ok(screen), Ok(item)) => Target::Read {
            index,
            screen,
            item,
        },
        _ => Target::Bad,
    }
}

/// A decimal mark as a script names it: `"."` or `","`.
fn mark(value: &Dynamic) -> Result<DecimalMark, Box<EvalAltResult>> {
    match text(value).as_deref() {
        Some(".") => Ok(DecimalMark::Point),
        Some(",") => Ok(DecimalMark::Comma),
        _ => Err(misuse(Misuse::UnknownDecimalMark).into()),
    }
}

/// A number a script reads, or `()` for none.
fn amount(found: Option<f64>) -> Dynamic {
    found.map_or(Dynamic::UNIT, Dynamic::from_float)
}

/// The whole lore of the slot a script names on `screen`: by its number,
/// or a slot it read on this very screen. `None` for a slot this screen
/// doesn't have, an empty one, or one read on another screen.
fn lore_on<'a>(screen: &'a Screen, named: &Dynamic) -> Option<&'a Lore> {
    let named = target(named);
    if named.read_elsewhere(Some(screen.id().opening())) {
        return None;
    }
    screen.slot(named.index()?)?.item().map(Item::lore)
}

/// The whole lore of the bot's own slot a script names: by its number, or
/// a slot it read from `inventory`.
fn lore_in(inventory: &BotInventory, named: &Dynamic) -> Option<Lore> {
    let named = target(named);
    if named.read_elsewhere(None) {
        return None;
    }
    inventory
        .slot(named.index()?)?
        .item()
        .map(|item| item.lore().clone())
}

/// `lore_has(slot, text)` on `lore`.
fn lore_has(lore: Option<&Lore>, wanted: &Dynamic) -> bool {
    text(wanted).is_some_and(|wanted| lore.is_some_and(|lore| lore.has(&wanted)))
}

/// `lore_number(slot, label)` on `lore`, with `mark`.
fn lore_number(lore: Option<&Lore>, label: &Dynamic, mark: DecimalMark) -> Dynamic {
    amount(text(label).and_then(|label| lore.and_then(|lore| lore.number(&label, mark))))
}

/// The screen the hook sees, read by `read`; `otherwise` with none.
fn screen<T>(menu: &Menu, otherwise: T, read: impl FnOnce(&Screen) -> T) -> T {
    lock(&menu.0).screen().map_or(otherwise, read)
}

/// `inventory`, `menu`, `sign` and a slot's helpers.
pub(super) fn register(m: &mut Module, r: &mut Registered) {
    m.set_custom_type::<Inventory>("inventory");
    m.set_custom_type::<Menu>("menu");
    m.set_custom_type::<Sign>("sign");
    register_inventory(m, r);
    register_menu(m, r);
    register_menu_reads(m, r);
    register_menu_actions(m, r);
    register_sign(m, r);
    register_lore(m, r);
}

/// `inventory`.
fn register_inventory(m: &mut Module, r: &mut Registered) {
    property!(m, r, "inventory", "slots", |inventory: &mut Inventory| {
        let turn = lock(&inventory.0);
        turn.view()
            .map(|view| numbers(view.inventory.slots()))
            .unwrap_or_default()
    });
    method!(
        m,
        r,
        "inventory",
        "slot",
        |inventory: &mut Inventory, index: Dynamic| {
            let index = whole(&index).and_then(|index| u16::try_from(index).ok());
            let turn = lock(&inventory.0);
            index
                .and_then(|index| {
                    turn.view()?
                        .inventory
                        .slot(index)
                        .map(|found| slot(found, None))
                })
                .unwrap_or(Dynamic::UNIT)
        }
    );
    method!(
        m,
        r,
        "inventory",
        "find",
        |inventory: &mut Inventory, id: Dynamic| {
            let Some(id) = text(&id) else {
                return Dynamic::UNIT;
            };
            let turn = lock(&inventory.0);
            turn.view()
                .and_then(|view| {
                    view.inventory
                        .slots()
                        .find(|found| found.item().is_some_and(|item| item.id().as_str() == id))
                })
                .map_or(Dynamic::UNIT, |found| slot(found, None))
        }
    );
    method!(
        m,
        r,
        "inventory",
        "count",
        |inventory: &mut Inventory, id: Dynamic| {
            let Some(id) = text(&id) else {
                return 0;
            };
            let turn = lock(&inventory.0);
            turn.view().map_or(0, |view| {
                view.inventory
                    .slots()
                    .filter_map(Slot::item)
                    .filter(|item| item.id().as_str() == id)
                    .map(|item| INT::from(item.count().get()))
                    .sum::<INT>()
            })
        }
    );
    method!(
        m,
        r,
        "inventory",
        "hotbar",
        |inventory: &mut Inventory, key: Dynamic| {
            let index = whole(&key)
                .filter(|key| (1..=9).contains(key))
                .and_then(|key| u16::try_from(key - 1).ok())
                .map(|key| HOTBAR + key);
            let turn = lock(&inventory.0);
            index
                .and_then(|index| {
                    turn.view()?
                        .inventory
                        .slot(index)
                        .map(|found| slot(found, None))
                })
                .unwrap_or(Dynamic::UNIT)
        }
    );
    method!(m, r, "inventory", "open", |inventory: &mut Inventory| {
        result(lock(&inventory.0).open_inventory())
    });
}

/// `menu`'s properties and readers.
fn register_menu(m: &mut Module, r: &mut Registered) {
    property!(m, r, "menu", "open", |menu: &mut Menu| screen(
        menu,
        false,
        |_| true
    ));
    property!(m, r, "menu", "screen", |menu: &mut Menu| screen(
        menu,
        Dynamic::UNIT,
        |open| opening(open.id())
    ));
    property!(m, r, "menu", "kind", |menu: &mut Menu| screen(
        menu,
        Dynamic::UNIT,
        |open| Dynamic::from(open.kind().as_str().to_owned())
    ));
    property!(m, r, "menu", "title", |menu: &mut Menu| screen(
        menu,
        Dynamic::UNIT,
        |open| Dynamic::from(open.title().as_str().to_owned())
    ));
    property!(m, r, "menu", "rows", |menu: &mut Menu| screen(
        menu,
        Dynamic::UNIT,
        |open| open.grid().map_or(Dynamic::UNIT, |grid| int(grid.rows))
    ));
    property!(m, r, "menu", "columns", |menu: &mut Menu| screen(
        menu,
        Dynamic::UNIT,
        |open| open.grid().map_or(Dynamic::UNIT, |grid| int(grid.columns))
    ));
    property!(m, r, "menu", "slots", |menu: &mut Menu| screen(
        menu,
        Array::new(),
        |open| numbers(open.own())
    ));
    property!(m, r, "menu", "player", |menu: &mut Menu| screen(
        menu,
        Array::new(),
        |open| numbers(open.player())
    ));
}

/// `menu`'s readers of one slot, the cursor, the trades and numbers.
fn register_menu_reads(m: &mut Module, r: &mut Registered) {
    method!(m, r, "menu", "slot", |menu: &mut Menu, index: Dynamic| {
        let index = whole(&index).and_then(|index| u16::try_from(index).ok());
        screen(menu, Dynamic::UNIT, |open| {
            index
                .and_then(|index| open.slot(index))
                .map_or(Dynamic::UNIT, |found| slot(found, Some(open.id())))
        })
    });
    method!(
        m,
        r,
        "menu",
        "at",
        |menu: &mut Menu, row: Dynamic, column: Dynamic| {
            let place = whole(&row)
                .and_then(|row| u8::try_from(row).ok())
                .zip(whole(&column).and_then(|column| u8::try_from(column).ok()));
            screen(menu, Dynamic::UNIT, |open| {
                place
                    .and_then(|(row, column)| open.at(row, column))
                    .map_or(Dynamic::UNIT, |found| slot(found, Some(open.id())))
            })
        }
    );
    method!(m, r, "menu", "find", |menu: &mut Menu, id: Dynamic| {
        let Some(id) = text(&id) else {
            return Dynamic::UNIT;
        };
        screen(menu, Dynamic::UNIT, |open| {
            open.own()
                .find(|found| found.item().is_some_and(|item| item.id().as_str() == id))
                .map_or(Dynamic::UNIT, |found| slot(found, Some(open.id())))
        })
    });
    property!(m, r, "menu", "cursor", |menu: &mut Menu| screen(
        menu,
        Dynamic::UNIT,
        |open| item(open.cursor())
    ));
    property!(m, r, "menu", "trades", |menu: &mut Menu| screen(
        menu,
        0,
        |open| { INT::try_from(open.offers().len()).unwrap_or(INT::MAX) }
    ));
    method!(m, r, "menu", "trade", |menu: &mut Menu, index: Dynamic| {
        let index = whole(&index).and_then(|index| usize::try_from(index).ok());
        screen(menu, Dynamic::UNIT, |open| {
            index
                .and_then(|index| open.offers().get(index))
                .map_or(Dynamic::UNIT, trade)
        })
    });
    property!(m, r, "menu", "pending", |menu: &mut Menu| lock(&menu.0)
        .awaits_fate());
    method!(
        m,
        r,
        "menu",
        "number",
        |_menu: &mut Menu, written: Dynamic| amount(
            text(&written).and_then(|written| find_amount(&written, DecimalMark::Point))
        )
    );
    // The same name with a second parameter: the decimal mark.
    FuncRegistration::new("number")
        .in_global_namespace()
        .with_purity(true)
        .with_volatility(true)
        .set_into_module(
            m,
            |_menu: &mut Menu, written: Dynamic, given: Dynamic| -> Returns<Dynamic> {
                let mark = mark(&given)?;
                Ok(amount(
                    text(&written).and_then(|written| find_amount(&written, mark)),
                ))
            },
        );
}

/// `menu`'s actions: clicks, a button, a trade, a name, the close.
fn register_menu_actions(m: &mut Module, r: &mut Registered) {
    let kinds: [(&'static str, ClickKind); 5] = [
        ("click", ClickKind::Left),
        ("right_click", ClickKind::Right),
        ("shift_click", ClickKind::Shift),
        ("drop", ClickKind::Drop),
        ("drop_stack", ClickKind::DropStack),
    ];
    for (name, kind) in kinds {
        r.push(("menu", name));
        FuncRegistration::new(name)
            .in_global_namespace()
            .with_purity(true)
            .with_volatility(true)
            .set_into_module(m, move |menu: &mut Menu, at: Dynamic| {
                result(lock(&menu.0).click(&target(&at), Some(kind)))
            });
    }
    method!(
        m,
        r,
        "menu",
        "swap",
        |menu: &mut Menu, at: Dynamic, key: Dynamic| result(
            lock(&menu.0).click(&target(&at), whole(&key).and_then(number_key))
        )
    );
    method!(
        m,
        r,
        "menu",
        "button",
        |menu: &mut Menu, button: Dynamic| result(
            lock(&menu.0).press_button(whole(&button).and_then(|button| u8::try_from(button).ok()))
        )
    );
    method!(
        m,
        r,
        "menu",
        "select_trade",
        |menu: &mut Menu, index: Dynamic| result(
            lock(&menu.0).select_trade(whole(&index).and_then(|index| u8::try_from(index).ok()))
        )
    );
    method!(m, r, "menu", "rename", |menu: &mut Menu, key: Dynamic| {
        strict(lock(&menu.0).rename(text(&key).as_deref(), Some(&[])))
    });
    // The same name with a second parameter: the declared message's values.
    FuncRegistration::new("rename")
        .in_global_namespace()
        .with_purity(true)
        .with_volatility(true)
        .set_into_module(m, |menu: &mut Menu, key: Dynamic, given: Dynamic| {
            strict(lock(&menu.0).rename(text(&key).as_deref(), values(&given).as_deref()))
        });
    method!(m, r, "menu", "close", |menu: &mut Menu| result(
        lock(&menu.0).close_screen()
    ));
}

/// `sign`.
fn register_sign(m: &mut Module, r: &mut Registered) {
    let editor = |sign: &Sign| lock(&sign.0).view().and_then(|view| view.sign);
    property!(m, r, "sign", "open", move |sign: &mut Sign| editor(sign)
        .is_some());
    property!(m, r, "sign", "x", move |sign: &mut Sign| editor(sign)
        .map_or(Dynamic::UNIT, |editor| int(editor.at().x)));
    property!(m, r, "sign", "y", move |sign: &mut Sign| editor(sign)
        .map_or(Dynamic::UNIT, |editor| int(editor.at().y)));
    property!(m, r, "sign", "z", move |sign: &mut Sign| editor(sign)
        .map_or(Dynamic::UNIT, |editor| int(editor.at().z)));
    property!(m, r, "sign", "side", move |sign: &mut Sign| editor(sign)
        .map_or(Dynamic::UNIT, |editor| Dynamic::from(
            editor.side().as_str()
        )));
    method!(m, r, "sign", "write", |sign: &mut Sign, key: Dynamic| {
        strict(lock(&sign.0).write_sign(text(&key).as_deref(), Some(&[])))
    });
    // The same name with a second parameter: the declared sign's values.
    FuncRegistration::new("write")
        .in_global_namespace()
        .with_purity(true)
        .with_volatility(true)
        .set_into_module(m, |sign: &mut Sign, key: Dynamic, given: Dynamic| {
            strict(lock(&sign.0).write_sign(text(&key).as_deref(), values(&given).as_deref()))
        });
    method!(m, r, "sign", "cancel", |sign: &mut Sign| result(
        lock(&sign.0).cancel_sign()
    ));
}

/// The lore helpers, on `menu` and on `inventory`: every line of the slot
/// as the hook sees it, never only what the slot's map kept.
fn register_lore(m: &mut Module, r: &mut Registered) {
    method!(
        m,
        r,
        "menu",
        "lore_has",
        |menu: &mut Menu, named: Dynamic, wanted: Dynamic| {
            screen(menu, false, |open| lore_has(lore_on(open, &named), &wanted))
        }
    );
    method!(
        m,
        r,
        "menu",
        "lore_number",
        |menu: &mut Menu, named: Dynamic, label: Dynamic| {
            screen(menu, Dynamic::UNIT, |open| {
                lore_number(lore_on(open, &named), &label, DecimalMark::Point)
            })
        }
    );
    // The same name with a third parameter: the decimal mark.
    FuncRegistration::new("lore_number")
        .in_global_namespace()
        .with_purity(true)
        .with_volatility(true)
        .set_into_module(
            m,
            |menu: &mut Menu, named: Dynamic, label: Dynamic, given: Dynamic| -> Returns<Dynamic> {
                let mark = mark(&given)?;
                Ok(screen(menu, Dynamic::UNIT, |open| {
                    lore_number(lore_on(open, &named), &label, mark)
                }))
            },
        );
    method!(
        m,
        r,
        "inventory",
        "lore_has",
        |inventory: &mut Inventory, named: Dynamic, wanted: Dynamic| {
            let turn = lock(&inventory.0);
            let lore = turn
                .view()
                .and_then(|view| lore_in(&view.inventory, &named));
            lore_has(lore.as_ref(), &wanted)
        }
    );
    method!(
        m,
        r,
        "inventory",
        "lore_number",
        |inventory: &mut Inventory, named: Dynamic, label: Dynamic| {
            let turn = lock(&inventory.0);
            let lore = turn
                .view()
                .and_then(|view| lore_in(&view.inventory, &named));
            lore_number(lore.as_ref(), &label, DecimalMark::Point)
        }
    );
}

#[cfg(test)]
mod tests {
    use fleet_core::script::Identifier;
    use fleet_core::script::menu::ItemData;

    use super::*;

    fn potion() -> Item {
        Item::new(
            Identifier::try_from("minecraft:potion").unwrap(),
            core::num::NonZeroU32::new(2).unwrap(),
            ItemData::new(Vec::new()),
        )
        .named("Turtle Master")
        .with_lore(["Price: 1,250", "Seller: AfkBot1"])
        .with_potion(Some("minecraft:turtle_master"))
    }

    #[test]
    fn an_item_reads_back_as_the_item_a_script_saw() {
        let item = potion();
        let mut map = Map::new();
        map.insert("empty".into(), Dynamic::FALSE);
        item_fields(&mut map, Some(&item));

        assert_eq!(read_item(&map), Ok(Some(ItemView::of(&item))));
    }

    #[test]
    fn an_empty_slot_reads_back_as_empty_and_a_map_of_the_scripts_own_as_none() {
        let mut empty = Map::new();
        empty.insert("empty".into(), Dynamic::TRUE);
        item_fields(&mut empty, None);
        let mut own = Map::new();
        own.insert("empty".into(), Dynamic::FALSE);
        own.insert("id".into(), Dynamic::from("minecraft:potion"));

        assert_eq!(read_item(&empty), Ok(None));
        assert_eq!(read_item(&own), Err(()));
        assert_eq!(read_item(&Map::new()), Err(()));
    }

    #[test]
    fn a_target_is_a_slots_number_or_a_slot_a_script_read() {
        assert_eq!(target(&Dynamic::from_int(12)), Target::Index(12));
        assert_eq!(target(&Dynamic::from_float(12.7)), Target::Index(12));
        assert_eq!(target(&Dynamic::from_int(-1)), Target::Bad);
        assert_eq!(target(&Dynamic::from_int(70_000)), Target::Bad);
        assert_eq!(target(&Dynamic::from("12")), Target::Bad);
        assert_eq!(target(&Dynamic::from_map(Map::new())), Target::Bad);
    }

    #[test]
    fn a_slots_map_keeps_whole_lore_lines_while_its_text_fits_in_a_value() {
        let long = potion().with_lore((0..16).map(|n| format!("{n:02}{}", "x".repeat(126))));
        let short = potion();

        let kept = kept_lore(&long);
        let text: usize = kept.iter().map(String::len).sum::<usize>()
            + long.id().as_str().len()
            + long.name().as_str().len()
            + long.potion().map_or(0, |potion| potion.as_str().len());

        assert_eq!(kept.len(), 7);
        assert!(
            text <= fleet_core::script::limits::MAX_STRING_BYTES,
            "{text}"
        );
        assert!(
            kept.iter()
                .enumerate()
                .all(|(n, line)| line.starts_with(&format!("{n:02}")))
        );
        assert_eq!(kept_lore(&short), ["Price: 1,250", "Seller: AfkBot1"]);
    }
}
