// An auction lister for a server whose auction house takes the price in an
// anvil, written the safe way:
// - every button is clicked by its slot, and only while that slot holds the
//   item and the name expected, so a listing renamed "Create" above the
//   buttons is never clicked;
// - the potion is found by its kind, wherever it is in the inventory;
// - every step is judged by the screen that follows it, never by a click's
//   fate alone: the plugin answers its buttons "rejected" or "unclear"
//   though they did their work;
// - an unexpected screen closes the menu and logs why, and a watchdog ends
//   a listing that takes longer than a minute.
//
// The flow: /ah, Create (the last row's third slot), the potion, Confirm
// Item, the price in the anvil "Preis eingeben", then Confirm Listing. The
// declared price's range starts at 100, the floor a bait listing can't push
// the price below. It lists 10 to 30 seconds after it joins, then every 10
// minutes.
fn on_join(first) {
    state.listing = false;
    // Soon after every join: a bot that's kicked more often than every 10
    // minutes still lists.
    timer.after(random.between(10000, 30000), "list");
}

fn on_timer(name) {
    if name == "list" {
        timer.after(600000, "list");
        start();
    } else if name == "watchdog" {
        stop("it took longer than a minute");
    } else if name == "price_taken" {
        if state.listing && menu.open && menu.title == "Preis eingeben" {
            stop("the price was refused");
        }
    } else if name == "step" {
        step();
    }
}

fn start() {
    if state.listing || menu.open || !carries_potion() {
        return;
    }
    state.listing = true;
    state.price = ();
    state.done = ();
    state.named = ();
    state.final = false;
    timer.after(60000, "watchdog");
    chat.send("auction");
}

fn stop(why) {
    log.info(`Listing stopped: ${why}.`);
    state.listing = false;
    timer.cancel("watchdog");
    if menu.open {
        menu.close();
    }
}

fn on_menu_open() {
    if state.listing {
        later();
    }
}

fn on_click(result) {
    if state.listing {
        later();
    }
}

fn on_menu_close() {
    if state.listing && state.final {
        log.info(`Listed a potion for ${state.price}.`);
        state.listing = false;
        timer.cancel("watchdog");
    }
}

fn later() {
    timer.after(random.between(300, 900), "step");
}

// One step, by the screen that's open now.
fn step() {
    // Between two screens, or this screen's step is done: the next screen
    // says whether it worked.
    if !state.listing || !menu.open || state.done == menu.screen {
        return;
    }
    let title = menu.title;
    if title == "Auction House" {
        create();
    } else if title == "Create Listing" {
        select_potion();
    } else if title == "Confirm Item" {
        confirm_item();
    } else if title == "Preis eingeben" {
        enter_price();
    } else if title == "Confirm Listing" {
        confirm_listing();
    } else {
        stop(`an unexpected screen, ${title}`);
    }
}

// Clicks `slot` as this screen's step; one the hook was refused is tried
// again at the next step.
fn press(slot) {
    if menu.click(slot).ok {
        state.done = menu.screen;
    } else {
        later();
    }
}

// Whether `slot` holds `id` named `name`.
fn holds(slot, id, name) {
    slot.id == id && slot.name == name
}

fn create() {
    if state.price == () {
        state.price = middle_price();
        if state.price == () {
            stop("fewer than three potions listed by others");
            return;
        }
    }
    let button = menu.at(menu.rows - 1, 2);
    if !holds(button, "minecraft:emerald", "Create") {
        stop("no Create button where it belongs");
        return;
    }
    press(button);
}

fn select_potion() {
    for n in menu.player {
        let slot = menu.slot(n);
        if turtle_master(slot) {
            press(slot);
            return;
        }
    }
    stop("no Turtle Master potion in the inventory");
}

fn confirm_item() {
    let button = menu.slot(11);
    if !turtle_master(menu.slot(13))
        || !holds(button, "minecraft:lime_stained_glass_pane", "Confirm")
    {
        stop("Confirm Item shows something else");
        return;
    }
    press(button);
}

// The anvil: first the price as its name, then its confirm.
fn enter_price() {
    if state.named != menu.screen {
        if menu.rename("price", #{ price: state.price }).ok {
            state.named = menu.screen;
        } else {
            stop(`the price ${state.price} is below the floor`);
        }
        return;
    }
    let button = menu.slot(2);
    if !holds(button, "minecraft:lime_dye", "Bestätigen") {
        stop("the anvil holds no confirm");
        return;
    }
    press(button);
    timer.after(3000, "price_taken");
}

fn confirm_listing() {
    let button = menu.slot(11);
    if menu.lore_number(13, "Price") != state.price
        || !holds(button, "minecraft:lime_stained_glass_pane", "Confirm")
    {
        stop("Confirm Listing shows another price");
        return;
    }
    state.final = true;
    press(button);
}

// The middle of the three cheapest Turtle Master potions others list.
fn middle_price() {
    let prices = [];
    for n in menu.slots {
        let slot = menu.slot(n);
        if slot.row < menu.rows - 1 && turtle_master(slot)
            && !menu.lore_has(slot, "Seller: " + bot.name)
        {
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

fn carries_potion() {
    for n in inventory.slots {
        if turtle_master(inventory.slot(n)) {
            return true;
        }
    }
    false
}

fn turtle_master(slot) {
    slot.potion == "minecraft:turtle_master"
        || slot.potion == "minecraft:long_turtle_master"
        || slot.potion == "minecraft:strong_turtle_master"
}
