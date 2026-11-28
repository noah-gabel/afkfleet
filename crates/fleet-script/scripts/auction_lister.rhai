// An auction lister: 10 to 30 seconds after it joins, then every 10
// minutes, with a Turtle Master potion in its hotbar, it reads the potions'
// prices on the auction house's first page, skips its own listings, and
// lists one potion at the middle of the three cheapest with
// "/ah sell {price}", confirming it on its screen once. Then it collects its
// expired listings.
//
// The declared price's range starts at 100: that's the floor a bait listing
// can't push the price below. A price under it is refused (bad_value), and
// nothing is sent.
fn on_join(first) {
    state.task = "";
    state.done = ();
    state.on = "";
    // Soon after every join: a bot that's kicked more often than every 10
    // minutes still lists.
    timer.after(random.between(10000, 30000), "list");
}

fn on_timer(name) {
    if name == "list" {
        timer.after(600000, "list");
        // With no screen open, a listing that didn't finish 10 minutes ago
        // is over.
        if !menu.open && potion_key() != () {
            state.task = "prices";
            chat.send("auction");
        }
    } else if name == "step" {
        step();
    }
}

fn on_menu_open() {
    state.on = menu.title;
    later();
}

fn on_click(result) {
    later();
}

fn on_menu_update() {
    later();
}

fn on_menu_close() {
    // Only the confirm's own close moves on: the auction house, which it
    // closes itself before it lists, closes too.
    if state.task == "listing" && state.on == "Confirm Listing" {
        state.task = "expired";
        chat.send("expired");
    } else if state.task == "expired" && state.on == "Expired Listings" {
        state.task = "";
    }
}

fn later() {
    timer.after(random.between(300, 900), "step");
}

fn step() {
    if !menu.open || state.done == menu.screen {
        return;
    }
    let title = menu.title;
    if title == "Auction House" && state.task == "prices" {
        list();
    } else if title == "Confirm Listing" && state.task == "listing" {
        let button = menu.slot(11);
        if button.id == "minecraft:lime_stained_glass_pane" && button.name == "Confirm"
            && turtle_master(menu.slot(13))
        {
            // Once a screen: the plugin lists and closes it.
            state.done = menu.screen;
            menu.click(button);
        } else {
            menu.close();
        }
    } else if title == "Expired Listings" && state.task == "expired" {
        for n in menu.slots {
            let slot = menu.slot(n);
            if slot.row < 5 && !slot.empty {
                menu.click(slot);
                return;
            }
        }
        menu.close();
    } else {
        log.info(`Closed a screen it didn't expect: ${title}`);
        state.task = "";
        menu.close();
    }
}

// Reads the price, closes the auction house and lists one potion.
fn list() {
    let price = middle_price();
    let key = potion_key();
    menu.close();
    if price == () || key == () {
        log.info("Fewer than three potions listed by others; nothing listed.");
        state.task = "";
        return;
    }
    hotbar.select(key);
    state.task = "listing";
    if !chat.send("sell", #{ price: price }).ok {
        log.info(`Not listed at ${price}: below the floor.`);
        state.task = "";
    }
}

// The middle of the three cheapest Turtle Master potions others list.
fn middle_price() {
    let prices = [];
    for n in menu.slots {
        let slot = menu.slot(n);
        if slot.row < 5 && turtle_master(slot) && !menu.lore_has(slot, "Seller: " + bot.name) {
            let price = menu.lore_number(slot, "Price");
            if price != () {
                prices.push(price);
            }
        }
    }
    if prices.len() < 3 {
        return ();
    }
    prices.sort();
    prices[1].floor()
}

// The number key of a hotbar slot that holds a Turtle Master potion.
fn potion_key() {
    for key in 1..10 {
        if turtle_master(inventory.hotbar(key)) {
            return key;
        }
    }
    ()
}

fn turtle_master(slot) {
    slot.potion == "minecraft:turtle_master"
        || slot.potion == "minecraft:long_turtle_master"
        || slot.potion == "minecraft:strong_turtle_master"
}
