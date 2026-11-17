// Looks at the spot where the mobs arrive and swings the sword every 1.2 seconds.
fn on_join(first) {
    hotbar.select(1);
    look.at(90.0, 10.0);
    timer.every(1200, "swing");
}

fn on_respawn() {
    hotbar.select(1);
    look.at(90.0, 10.0);
}

fn on_timer(name) {
    mouse.left.tap();
}
