// A farm that sells: it farms by holding the right button with its farm's
// item in hotbar slot 1. 30 to 60 seconds after it joins, then every 5
// minutes, it opens the sell menu, sells its Turtle Master potions and drops
// its one kind of junk, never anything else, so it never drops a tool. It
// waits 300 to 900 ms before every click, then selects the farm's item again
// and farms on: a right button held with a potion in hand would drink it.
fn junk() {
    "minecraft:rotten_flesh"
}

fn on_join(first) {
    farm();
    // Soon after every join: a farm that's kicked more often than every 5
    // minutes still sells.
    timer.after(random.between(30000, 60000), "sell");
}

fn farm() {
    hotbar.select(1);
    mouse.right.hold();
}

fn on_timer(name) {
    if name == "sell" {
        timer.after(300000, "sell");
        if !menu.open {
            mouse.right.release();
            chat.send("sell_menu");
        }
    } else if name == "step" {
        step();
    }
}

fn on_menu_open() {
    if menu.title != "Sell Items" {
        log.info(`Closed a screen it didn't open: ${menu.title}`);
        menu.close();
        return;
    }
    state.pressed = false;
    later();
}

fn on_click(result) {
    later();
}

fn on_menu_close() {
    farm();
}

fn later() {
    timer.after(random.between(300, 900), "step");
}

// One step in the sell menu: a potion in, the junk out, then Sell, then the
// close.
fn step() {
    if !menu.open || menu.title != "Sell Items" {
        return;
    }
    for n in menu.player {
        let slot = menu.slot(n);
        if turtle_master(slot) {
            act(menu.shift_click(slot));
            return;
        }
        if slot.id == junk() {
            act(menu.drop_stack(slot));
            return;
        }
    }
    let sell = menu.slot(49);
    if !state.pressed && sell.id == "minecraft:emerald" && sell.name == "Sell" {
        state.pressed = true;
        act(menu.click(sell));
        return;
    }
    menu.close();
}

// A click the hook was refused for is tried again at the next step.
fn act(result) {
    if !result.ok {
        later();
    }
}

fn turtle_master(slot) {
    slot.potion == "minecraft:turtle_master"
        || slot.potion == "minecraft:long_turtle_master"
        || slot.potion == "minecraft:strong_turtle_master"
}
