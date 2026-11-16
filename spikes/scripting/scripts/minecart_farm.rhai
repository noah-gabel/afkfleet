// The minecart farm, written with helper functions: the spike's check that
// the game objects and the state object work inside a script's own functions.

fn on_join(first) {
    if first {
        state.swings = 0;
    }
    get_in();
    timer.every(600, "swing");
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

// Gets into the nearest free minecart, and tries again in a second if the
// bot isn't seated yet.
fn get_in() {
    if bot.riding {
        return;
    }
    vehicle.mount("minecart");
    timer.after(1000, "get_in");
}

// One attack, counted, with the sword in slot 1.
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
