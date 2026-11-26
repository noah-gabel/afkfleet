//! What the bot sees of its inventory, its screen and its sign editor, as
//! fleet-core's types (Plan.md P16.8; ADR-0036, M1, M2, M11, M18;
//! ADR-0050, B1 to B4).
//!
//! An item's text is the server's: its name and each line of its lore are
//! rendered within a budget a little larger than what's kept, then
//! cleaned and cut by fleet-core. Its components reach a script only as
//! IDs and numbers; the whole of them is [`ItemData`], encoded here the
//! same way whatever order they came in.

use core::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};

use azalea::core::registry_holder::RegistryHolder;
use azalea::ecs::entity::Entity;
use azalea::ecs::world::World;
use azalea::entity::inventory::Inventory;
use azalea::inventory::components::EncodableDataComponent;
use azalea::inventory::item::MaxStackSizeExt as _;
use azalea::inventory::{DataComponentPatch, ItemStack, components};
use azalea::protocol::packets::game::c_merchant_offers::MerchantOffer;
use azalea::registry::builtin::{DataComponentKind, MenuKind};
use azalea::registry::{DataRegistry as _, Registry as _};
use azalea::world::{WorldName, Worlds};
use fleet_core::mc::MenuView;
use fleet_core::script::menu::{
    BotInventory, Item, ItemData, Lore, Screen, ScreenKind, TradeOffer,
};

use super::Menus;
use crate::events::render_within;
use crate::script::{floor, identifier, uses_left};

/// How many characters of an item's name are rendered before it's
/// cleaned: twice what's kept, so formatting the cleaning drops leaves
/// room, and no more.
const NAME_CHARS: usize = 2 * Item::MAX_NAME;
/// The same for a line of lore.
const LINE_CHARS: usize = 2 * Lore::MAX_LINE;
/// The same for a screen's title.
pub(super) const TITLE_CHARS: usize = 2 * Screen::MAX_TITLE;

/// The kind of screen azalea's `menu` registry names.
pub(super) const fn screen_kind(kind: MenuKind) -> ScreenKind {
    match kind {
        MenuKind::Generic9x1 => ScreenKind::Generic9x1,
        MenuKind::Generic9x2 => ScreenKind::Generic9x2,
        MenuKind::Generic9x3 => ScreenKind::Generic9x3,
        MenuKind::Generic9x4 => ScreenKind::Generic9x4,
        MenuKind::Generic9x5 => ScreenKind::Generic9x5,
        MenuKind::Generic9x6 => ScreenKind::Generic9x6,
        MenuKind::Generic3x3 => ScreenKind::Generic3x3,
        MenuKind::Crafter3x3 => ScreenKind::Crafter3x3,
        MenuKind::Anvil => ScreenKind::Anvil,
        MenuKind::Beacon => ScreenKind::Beacon,
        MenuKind::BlastFurnace => ScreenKind::BlastFurnace,
        MenuKind::BrewingStand => ScreenKind::BrewingStand,
        MenuKind::Crafting => ScreenKind::Crafting,
        MenuKind::Enchantment => ScreenKind::Enchantment,
        MenuKind::Furnace => ScreenKind::Furnace,
        MenuKind::Grindstone => ScreenKind::Grindstone,
        MenuKind::Hopper => ScreenKind::Hopper,
        MenuKind::Lectern => ScreenKind::Lectern,
        MenuKind::Loom => ScreenKind::Loom,
        MenuKind::Merchant => ScreenKind::Merchant,
        MenuKind::ShulkerBox => ScreenKind::ShulkerBox,
        MenuKind::Smithing => ScreenKind::Smithing,
        MenuKind::Smoker => ScreenKind::Smoker,
        MenuKind::CartographyTable => ScreenKind::CartographyTable,
        MenuKind::Stonecutter => ScreenKind::Stonecutter,
    }
}

/// What the bot sees of its inventory, its screen and its sign editor
/// right now: part of a hook's snapshot. A bot without azalea's inventory
/// sees an empty one.
pub(crate) fn view(world: &mut World, bot: Entity) -> MenuView {
    with_registries(world, bot, |registries| {
        let (Some(menus), Some(inventory)) = (world.get::<Menus>(bot), world.get::<Inventory>(bot))
        else {
            return MenuView::default();
        };
        MenuView {
            inventory: BotInventory::new(
                inventory
                    .inventory_menu
                    .slots()
                    .iter()
                    .map(|stack| item(stack, registries)),
            ),
            screen: screen(menus, inventory, registries),
            sign: menus.desk.open().copied(),
        }
    })
}

/// Runs `work` with the registries of the world the bot is in, which name
/// its enchantments; `None` outside a world.
pub(super) fn with_registries<T>(
    world: &World,
    bot: Entity,
    work: impl FnOnce(Option<&RegistryHolder>) -> T,
) -> T {
    let in_world = world
        .get::<WorldName>(bot)
        .zip(world.get_resource::<Worlds>())
        .and_then(|(name, worlds)| worlds.get(name));
    let read = in_world.as_ref().map(|world| world.read());
    work(read.as_ref().map(|world| &world.registries))
}

/// The open screen as the bot sees it, or `None` when none is open, or
/// when azalea's own picture of it isn't the screen the gate has open.
pub(super) fn screen(
    menus: &Menus,
    inventory: &Inventory,
    registries: Option<&RegistryHolder>,
) -> Option<Screen> {
    let open = menus.screen.as_ref()?;
    let (slots, state) = if open.kind == ScreenKind::Inventory {
        if inventory.container_menu.is_some() {
            return None;
        }
        (inventory.inventory_menu.slots(), menus.inventory_state)
    } else {
        if inventory.id != open.id.window().get() {
            return None;
        }
        (inventory.container_menu.as_ref()?.slots(), open.state)
    };
    Some(
        Screen::new(
            open.id,
            state,
            open.kind,
            &open.title,
            slots.iter().map(|stack| item(stack, registries)),
            item(&inventory.carried, registries),
        )
        .with_offers(open.offers.iter().cloned()),
    )
}

/// A merchant's trade as fleet-core keeps it, with its costs as the
/// merchant asks them now; `None` when an item in it can't be read.
pub(super) fn offer(
    offer: &MerchantOffer,
    registries: Option<&RegistryHolder>,
) -> Option<TradeOffer> {
    let cost = asked(offer);
    let cost = item(&ItemStack::from(cost), registries)?;
    let second = match &offer.cost_b {
        Some(second) => Some(item(
            &ItemStack::from(second.clone().into_item_stack()),
            registries,
        )?),
        None => None,
    };
    let result = item(&offer.result, registries)?;
    Some(TradeOffer::new(cost, second, result, offer.out_of_stock))
}

/// The first cost as a vanilla client shows it: the base count raised by
/// demand and moved by a special price, between 1 and a stack.
fn asked(offer: &MerchantOffer) -> azalea::inventory::ItemStackData {
    let mut cost = offer.base_cost_a.clone().into_item_stack();
    let base = cost.count;
    // Vanilla's `max(0, floor(base * demand * multiplier))`, in double
    // precision.
    let raised =
        floor(f64::from(base.saturating_mul(offer.demand)) * f64::from(offer.price_multiplier))
            .max(0);
    let most = cost.kind.max_stack_size().max(1);
    cost.count = base
        .saturating_add(raised)
        .saturating_add(offer.special_price_diff)
        .clamp(1, most);
    cost
}

/// The item in a slot as a script reads it; `None` for an empty slot.
pub(super) fn item(stack: &ItemStack, registries: Option<&RegistryHolder>) -> Option<Item> {
    let stack = stack.as_present()?;
    let count = NonZeroU32::new(u32::try_from(stack.count).ok()?)?;
    // azalea's own name of a kind it knows: always an identifier.
    let id = identifier(&stack.kind.to_string());
    // The name as the client shows it: a custom name, or else the item's
    // name, which plugins' buttons often set instead.
    let name = stack
        .get_component::<components::CustomName>()
        .map(|custom| render_within(&custom.name, NAME_CHARS).text)
        .or_else(|| {
            stack
                .get_component::<components::ItemName>()
                .map(|name| render_within(&name.name, NAME_CHARS).text)
        })
        .unwrap_or_default();
    let lore: Vec<String> = stack
        .get_component::<components::Lore>()
        .map(|lore| {
            lore.lines
                .iter()
                .take(Lore::MAX_LINES)
                .map(|line| render_within(line, LINE_CHARS).text)
                .collect()
        })
        .unwrap_or_default();
    let potion = stack
        .get_component::<components::PotionContents>()
        .and_then(|contents| contents.potion.map(|potion| potion.to_string()));
    let enchantments = stack
        .get_component::<components::Enchantments>()
        .map(|enchantments| named(enchantments.levels.iter(), registries))
        .unwrap_or_default();
    let stored = stack
        .get_component::<components::StoredEnchantments>()
        .map(|stored| named(stored.enchantments.iter(), registries))
        .unwrap_or_default();
    Some(
        Item::new(id, count, data(&stack.component_patch))
            .with_durability(uses_left(stack))
            .named(&name)
            .with_lore(lore)
            .with_potion(potion.as_deref())
            .with_enchantments(stored_refs(&enchantments))
            .with_stored_enchantments(stored_refs(&stored)),
    )
}

/// Enchantments with their identifiers, as the world's registries name
/// them; one the registry doesn't know is `minecraft:unknown`.
fn named<'a>(
    levels: impl Iterator<Item = (&'a azalea::registry::data::Enchantment, &'a i32)>,
    registries: Option<&RegistryHolder>,
) -> Vec<(String, i32)> {
    levels
        .map(|(enchantment, level)| (enchantment_name(*enchantment, registries), *level))
        .collect()
}

fn stored_refs(named: &[(String, i32)]) -> impl Iterator<Item = (&str, i32)> {
    named.iter().map(|(name, level)| (name.as_str(), *level))
}

/// An enchantment's identifier, as the server's own registry numbers it
/// (ADR-0036, M11), or `minecraft:unknown`.
fn enchantment_name(
    enchantment: azalea::registry::data::Enchantment,
    registries: Option<&RegistryHolder>,
) -> String {
    registries
        .and_then(|registries| {
            registries.protocol_id_to_identifier(
                azalea::registry::identifier::Identifier::new("minecraft:enchantment"),
                enchantment.protocol_id(),
            )
        })
        .map_or_else(
            || UNKNOWN.to_owned(),
            |name| identifier(&name.to_string()).as_str().to_owned(),
        )
}

/// What stands for an enchantment the registry doesn't know.
const UNKNOWN: &str = "minecraft:unknown";

/// Every data component of an item, in one encoding whatever order they
/// came in: each by its kind's number, in order, present or removed, with
/// its own encoding's length and bytes (ADR-0050, B3).
///
/// An enchantment list is a map with no order of its own, so it's written
/// sorted. A component that can't be encoded gets bytes no other item has,
/// so a click on it is refused rather than let through.
pub(super) fn data(patch: &DataComponentPatch) -> ItemData {
    let mut entries: Vec<_> = patch.iter().collect();
    entries.sort_by_key(|(kind, _)| kind.to_u32());
    let mut bytes = Vec::new();
    for (kind, component) in entries {
        bytes.extend_from_slice(&kind.to_u32().to_le_bytes());
        let Some(component) = component else {
            // Removed from the item's defaults.
            bytes.push(0);
            continue;
        };
        bytes.push(1);
        let encoded = encoded(kind, component, patch);
        bytes.extend_from_slice(
            &u32::try_from(encoded.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        bytes.extend_from_slice(&encoded);
    }
    ItemData::new(bytes)
}

/// One component's bytes, the same whatever order its own parts came in.
fn encoded(
    kind: DataComponentKind,
    component: &dyn EncodableDataComponent,
    patch: &DataComponentPatch,
) -> Vec<u8> {
    let sorted = match kind {
        DataComponentKind::Enchantments => patch
            .get::<components::Enchantments>()
            .map(|enchantments| sorted_levels(enchantments.levels.iter())),
        DataComponentKind::StoredEnchantments => patch
            .get::<components::StoredEnchantments>()
            .map(|stored| sorted_levels(stored.enchantments.iter())),
        DataComponentKind::BlockState => patch
            .get::<components::BlockState>()
            .map(|state| sorted_properties(state.properties.iter())),
        _ => None,
    };
    if let Some(sorted) = sorted {
        return sorted;
    }
    let mut encoded = Vec::new();
    if component.encode(&mut encoded).is_err() {
        // Unreadable: equal to nothing, itself included.
        let mut marked = vec![0xff];
        marked.extend_from_slice(&unique());
        return marked;
    }
    encoded
}

/// A block state's properties, each name and value with its length, by
/// name.
fn sorted_properties<'a>(properties: impl Iterator<Item = (&'a String, &'a String)>) -> Vec<u8> {
    let mut properties: Vec<_> = properties.collect();
    properties.sort_unstable();
    let mut bytes = Vec::new();
    for text in properties
        .into_iter()
        .flat_map(|(name, value)| [name, value])
    {
        bytes.extend_from_slice(&u32::try_from(text.len()).unwrap_or(u32::MAX).to_le_bytes());
        bytes.extend_from_slice(text.as_bytes());
    }
    bytes
}

/// An enchantment map's entries by the registry's numbers, in order.
fn sorted_levels<'a>(
    levels: impl Iterator<Item = (&'a azalea::registry::data::Enchantment, &'a i32)>,
) -> Vec<u8> {
    let mut levels: Vec<(u32, i32)> = levels
        .map(|(enchantment, level)| (enchantment.protocol_id(), *level))
        .collect();
    levels.sort_unstable();
    levels
        .into_iter()
        .flat_map(|(id, level)| id.to_le_bytes().into_iter().chain(level.to_le_bytes()))
        .collect()
}

/// Bytes that no other call gives: for a component that can't be encoded.
fn unique() -> [u8; 8] {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed).to_le_bytes()
}
