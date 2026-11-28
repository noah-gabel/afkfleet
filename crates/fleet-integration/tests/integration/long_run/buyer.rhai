// The long run's buyer (Plan.md P16.18; ADR-0054): the auction scan's way,
// as fast as the limits let a script go. Each step comes 250 ms after the
// event before it, a timer's shortest wait: a click's fate comes back
// sooner than the click limit's game tick. It pages through the auction
// house, forth to the last page and back, and on each page picks the
// cheapest diamond listed for 1,000 or less, then confirms it on its
// Confirm Purchase screen: once a screen, and only the listing picked.
//
// Every buy it meant and didn't make is a line of its log naming the price,
// whatever stopped it: the listing gone, a confirm showing another, a screen
// opened unasked, a click not sent, a disconnect, a stop. A step is judged
// by the screen that follows it, never by a click's fate; a confirm by the
// diamonds in the inventory.
fn budget() {
    1000.0
}

fn on_join(first) {
    if first {
        state.wanted = ();
        state.phase = "";
        state.before = 0;
        state.patience = 1500;
        state.answered = 0;
    }
    state.forth = true;
    if state.phase == "confirmed" {
        // Its confirm may have gone through before the disconnect.
        timer.after(state.patience, "judge");
    } else {
        missed("the bot was disconnected");
        state.phase = "";
    }
    timer.after(1000, "open");
}

fn on_stop() {
    log.info(`Answered ${state.answered} clicks.`);
    if state.wanted != () {
        missed(`the bot was stopped, ${state.phase}`);
    }
}

fn on_timer(name) {
    if name == "open" {
        if !menu.open && state.phase != "confirmed" && !chat.send("auction").ok {
            timer.after(1000, "open");
        }
    } else if name == "step" {
        step();
    } else if name == "no_confirm" {
        if state.phase == "picked" {
            missed("no confirm screen came");
            state.phase = "";
            later();
        }
    } else if name == "judge" {
        judge();
    }
}

fn later() {
    timer.after(250, "step");
}

fn on_menu_open() {
    let title = menu.title;
    if title == "Auction House" {
        if state.phase == "picked" {
            missed("the auction house opened again");
            state.phase = "";
        }
        later();
    } else if title == "Confirm Purchase" {
        later();
    } else {
        if state.phase == "picked" {
            missed(`an unexpected screen, ${title}`);
            state.phase = "";
        }
        menu.close();
    }
}

fn on_menu_update() {
    // A page redrawn while it has nothing waiting: the listings changed.
    if state.phase == "" {
        later();
    }
}

fn on_menu_close() {
    if state.phase == "confirmed" {
        timer.after(state.patience, "judge");
    } else if state.phase == "" {
        timer.after(500, "open");
    }
    // Picked: the auction house closes when Confirm Purchase opens; the
    // confirm, or no_confirm, decides.
}

fn on_click(result) {
    let sent = result.ok || result.reason == "rejected" || result.reason == "unclear"
        || result.reason == "replaced" || result.reason == "unanswered";
    if sent {
        state.answered += 1;
    }
    if result.reason == "unanswered" {
        // The screen takes no click until it's opened again. After a
        // confirm the server may still buy it: the inventory says, later.
        if state.phase == "confirmed" {
            state.patience = 12000;
        } else if state.phase == "turning" {
            state.phase = "";
        }
        menu.close();
    } else if state.phase == "turning" {
        state.phase = "";
        later();
    } else if state.phase == "picked" {
        if !sent {
            missed(`its click wasn't sent, ${result.reason}`);
            state.phase = "";
            later();
        }
    } else if state.phase == "confirmed" {
        if !sent {
            missed(`its confirm wasn't sent, ${result.reason}`);
            state.phase = "";
            menu.close();
        }
    } else {
        later();
    }
}

fn missed(why) {
    if state.wanted != () {
        log.info(`Missed ${state.wanted.price} from ${state.wanted.seller}: ${why}.`);
    }
    state.wanted = ();
}

// One step, by the screen that's open now. Never while a click waits for
// its fate: that fate comes, and the step after it.
fn step() {
    if !menu.open {
        timer.after(500, "open");
        return;
    }
    if menu.pending {
        return;
    }
    let title = menu.title;
    if title == "Auction House" {
        if state.phase == "" {
            scan();
        }
    } else if title == "Confirm Purchase" {
        confirm();
    }
}

// On the auction house: the cheapest diamond within the budget on this
// page, or the next page.
fn scan() {
    let best = ();
    let price = 0.0;
    for n in 0..45 {
        let slot = menu.slot(n);
        if slot.id != "minecraft:diamond" {
            continue;
        }
        let asked = menu.lore_number(slot, "Price");
        if asked == () || asked > budget() {
            continue;
        }
        if best == () || asked < price {
            best = slot;
            price = asked;
        }
    }
    if best == () {
        turn();
        return;
    }
    state.wanted = #{ price: price, seller: seller_of(best) };
    let picked = menu.click(best);
    if picked.ok {
        state.phase = "picked";
        timer.after(3000, "no_confirm");
    } else {
        missed(`its click was refused, ${picked.reason}`);
        later();
    }
}

// Forth to the last page, then back to the first.
fn turn() {
    let next = menu.slot(53);
    let previous = menu.slot(45);
    let forth = next.id == "minecraft:arrow" && next.name == "Next page";
    let back = previous.id == "minecraft:arrow" && previous.name == "Previous page";
    if state.forth && !forth {
        state.forth = false;
    } else if !state.forth && !back {
        state.forth = true;
    }
    let button = if state.forth { next } else { previous };
    if (state.forth && forth) || (!state.forth && back) {
        if menu.click(button).ok {
            state.phase = "turning";
        } else {
            later();
        }
    }
}

// On Confirm Purchase: confirms only the listing picked, once.
fn confirm() {
    if state.phase != "picked" {
        // A confirm it no longer waits for: never confirmed.
        if state.phase != "confirmed" {
            menu.close();
        }
        return;
    }
    let button = menu.slot(11);
    let same = menu.slot(13).id == "minecraft:diamond"
        && menu.lore_number(13, "Price") == state.wanted.price
        && menu.lore_has(13, state.wanted.seller);
    if !same || button.id != "minecraft:lime_stained_glass_pane" || button.name != "Confirm" {
        missed("the confirm showed another listing");
        state.phase = "";
        menu.close();
        return;
    }
    state.before = inventory.count("minecraft:diamond");
    let confirmed = menu.click(button);
    if confirmed.ok {
        state.phase = "confirmed";
        state.patience = 1500;
    } else {
        missed(`its confirm was refused, ${confirmed.reason}`);
        state.phase = "";
        menu.close();
    }
}

// After a confirm: the inventory says whether it bought.
fn judge() {
    if state.phase != "confirmed" {
        return;
    }
    if inventory.count("minecraft:diamond") <= state.before {
        missed("it didn't go through");
    }
    state.wanted = ();
    state.phase = "";
    if !menu.open {
        timer.after(500, "open");
    }
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
