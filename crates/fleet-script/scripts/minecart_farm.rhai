// A farm the bot works from a minecart. Wherever the server puts the bot
// (a join, a lobby and back, a respawn), it gets into the nearest free
// minecart first.
fn on_join(first) {
    if first {
        state.swings = 0;
    }
    get_in();
    timer.every(600, "swing");
}

fn on_world_change() {
    get_in();
}

fn on_respawn() {
    get_in();
}

fn on_timer(name) {
    if name == "get_in" {
        get_in();
    } else if name == "swing" {
        swing();
    }
}

// Gets into the minecart, and looks again in a second until the bot is seated.
fn get_in() {
    if bot.riding {
        return;
    }
    vehicle.mount("minecart");
    timer.after(1000, "get_in");
}

// One attack with the sword in slot 1, counted.
fn swing() {
    if !bot.riding {
        return;
    }
    hotbar.select(1);
    look.at(90.0, 0.0);
    if mouse.left.tap().ok {
        state.swings += 1;
    }
}
