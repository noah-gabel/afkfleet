// Every minute, while it carries Turtle Master potions: opens the server's
// sell menu, moves every potion into it with a shift-click, presses Sell and
// closes it. Each step waits 300 to 900 ms, and each is judged by what the
// screen shows next, never by a click's fate alone: a plugin's button answers
// "rejected" though it did its work.
fn on_join(first) {
    timer.every(60000, "sell");
}

fn on_timer(name) {
    if name == "sell" {
        if !menu.open && carries_potions() {
            chat.send("sell_menu");
        }
    } else if name == "step" {
        step();
    }
}

fn on_menu_open() {
    if menu.title == "Sell Items" {
        state.pressed = false;
        later();
    }
}

fn on_click(result) {
    later();
}

fn later() {
    timer.after(random.between(300, 900), "step");
}

// One step in the sell menu: a potion in, then Sell, then the close.
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

fn carries_potions() {
    for n in inventory.slots {
        if turtle_master(inventory.slot(n)) {
            return true;
        }
    }
    false
}
