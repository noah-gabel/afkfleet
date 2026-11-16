// An auction page's scan: the cheapest listing of one item, bought if it's
// under the limit. Each slot's price is read from its lore by a native helper.

fn on_menu_update() {
    let best = cheapest("minecraft:diamond");
    if best == () {
        return;
    }
    if best.price <= 1500 {
        menu.click(best.slot);
    }
}

fn cheapest(item) {
    let best = ();
    for slot in menu.slots {
        if slot.id != item {
            continue;
        }
        let price = slot.lore_number("Price");
        if price == () {
            continue;
        }
        if best == () || price < best.price {
            best = #{ slot: slot.index, price: price };
        }
    }
    best
}
