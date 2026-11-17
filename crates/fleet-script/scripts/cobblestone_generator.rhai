// Mines the cobblestone in front of the bot with the pickaxes in slots 1 to 9,
// one after the other, and stops when the last one is nearly used up.
fn on_join(first) {
    if first {
        state.slot = 1;
    }
    hotbar.select(state.slot);
    look.at(0.0, 20.0);
    timer.every(1000, "mine");
}

fn on_timer(name) {
    if mouse.left.held {
        return; // still mining
    }
    let result = mouse.left.hold();
    if result.ok {
        return;
    }
    if result.reason == "tool_guard" || bot.held == () {
        next_pickaxe();
    }
}

fn next_pickaxe() {
    if state.slot == 9 {
        log.info("No pickaxe left.");
        timer.cancel("mine");
        return;
    }
    state.slot += 1;
    hotbar.select(state.slot);
}
