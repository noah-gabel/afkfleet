// Every 10 hours, and at every join: asks for the balance and pays what's
// above 10,000 to the main account, which the declared message names.
fn on_join(first) {
    ask();
    timer.every(36000000, "balance");
}

fn on_timer(name) {
    ask();
}

fn ask() {
    if chat.send("balance").ok {
        chat.expect("balance", 5000);
    }
}

fn on_chat(key, values) {
    let over = values.amount - 10000;
    if over >= 1 {
        chat.send("pay", #{ amount: over });
    }
}
