// Every 30 seconds: opens the auction house and buys the cheapest diamond
// listed for 1,500 or less, on its Confirm Purchase screen. A purchase is
// judged by what follows it, never by a click's fate: the plugin answers
// every button "rejected" though it acted, so the diamonds in the inventory
// say what was bought. Each Confirm Purchase screen is confirmed at most
// once, and only for the listing picked, so no listing is bought twice.
fn on_join(first) {
    if first {
        state.bought = 0;
    }
    state.wanted = ();
    state.confirmed = false;
    state.before = 0;
    timer.every(30000, "scan");
}

fn on_timer(name) {
    if name == "scan" {
        if !menu.open {
            chat.send("auction");
        }
    } else if name == "pick" {
        pick();
    } else if name == "confirm" {
        confirm();
    }
}

fn on_menu_open() {
    if menu.title == "Auction House" {
        later("pick");
    } else if menu.title == "Confirm Purchase" {
        state.confirmed = false;
        later("confirm");
    } else {
        menu.close();
    }
}

fn on_menu_close() {
    // The purchase's screen closed after its confirm: what the inventory
    // holds now says whether it bought.
    if state.wanted != () && state.confirmed {
        let now = inventory.count("minecraft:diamond");
        if now > state.before {
            state.bought += 1;
            log.info(`Bought ${now - state.before} diamonds for ${state.wanted.price}.`);
        } else {
            log.info("The purchase didn't go through.");
        }
        state.wanted = ();
        state.confirmed = false;
    }
}

fn later(name) {
    timer.after(random.between(300, 900), name);
}

// On the auction house: the cheapest diamond within the budget.
fn pick() {
    if !menu.open || menu.title != "Auction House" {
        return;
    }
    let best = ();
    let price = 0.0;
    for n in menu.slots {
        let slot = menu.slot(n);
        if slot.id != "minecraft:diamond" || slot.row > 4 {
            continue;
        }
        let asked = menu.lore_number(slot, "Price");
        if asked == () || asked > 1500 {
            continue;
        }
        if best == () || asked < price {
            best = slot;
            price = asked;
        }
    }
    if best == () {
        menu.close();
        return;
    }
    state.wanted = #{ price: price, seller: seller_of(best) };
    if !menu.click(best).ok {
        state.wanted = ();
        menu.close();
    }
}

// On Confirm Purchase: confirms only the listing picked, once.
fn confirm() {
    if !menu.open || menu.title != "Confirm Purchase" || state.wanted == () || state.confirmed {
        return;
    }
    let button = menu.slot(11);
    let same = menu.slot(13).id == "minecraft:diamond"
        && menu.lore_number(13, "Price") == state.wanted.price
        && menu.lore_has(13, state.wanted.seller);
    if !same || button.id != "minecraft:lime_stained_glass_pane" || button.name != "Confirm" {
        log.info("Not the listing picked; nothing bought.");
        state.wanted = ();
        menu.close();
        return;
    }
    state.before = inventory.count("minecraft:diamond");
    state.confirmed = menu.click(button).ok;
}

// A listing's seller line, as its lore holds it.
fn seller_of(slot) {
    for line in slot.lore {
        if line.starts_with("Seller: ") {
            return line;
        }
    }
    "Seller: "
}
