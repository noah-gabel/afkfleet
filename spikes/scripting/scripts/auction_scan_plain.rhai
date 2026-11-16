// The same scan without the native helper: the price is cut out of the lore
// with Rhai's own string functions. It shows what the helper saves.

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
        let price = price_of(slot.lore);
        if price == () {
            continue;
        }
        if best == () || price < best.price {
            best = #{ slot: slot.index, price: price };
        }
    }
    best
}

// "Price: 1,250" or "Price: 12.5k" as a whole number, or () without one.
fn price_of(lore) {
    for line in lore {
        if !line.starts_with("Price: ") {
            continue;
        }
        let text = line.sub_string(7);
        text.replace(",", "");
        let factor = 1;
        if text.ends_with("k") {
            factor = 1000;
            text = text.sub_string(0, text.len() - 1);
        } else if text.ends_with("M") {
            factor = 1000000;
            text = text.sub_string(0, text.len() - 1);
        }
        let dot = text.index_of(".");
        if dot < 0 {
            return parse_int(text) * factor;
        }
        let whole = parse_int(text.sub_string(0, dot));
        let tenths = parse_int(text.sub_string(dot + 1, 1));
        return whole * factor + tenths * factor / 10;
    }
    ()
}
