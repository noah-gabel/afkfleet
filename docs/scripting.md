# Scripted modes: the script reference

> **Draft: Phase 15 is being built, group by group, and no script runs yet.** This is the reference as a script author would read it, written with the plan for Phases 15 and 16 ([Plan.md](../Plan.md), [ADR-0036](adr/0036-scripted-modes.md), [ADR-0037](adr/0037-scripts-read-system-chat.md)). Part 1 is Phase 15's API. Part 2 is Phase 16's, inventory and menus. Names and numbers can still change while each group is built; whatever changes, changes here in the same pull request.

A scripted mode is a mode whose behaviour is a small program instead of a list of steps. The program is written in [Rhai](https://rhai.rs/book/), a small language that looks like JavaScript and Rust.

**A script is a virtual player at a vanilla client.** It presses the keys and the mouse buttons a player presses, looks where a player looks, and reads what a player sees on the screen. Each input behaves as it does in the vanilla client: holding the right button with a block in hand clicks again every 4 ticks, holding the left one on a block mines it. Nothing a player can't do is in the API.

**Whether a server allows what your script does is yours to check.** Many servers have rules about automated farming, trading and auction buying.

## Three things to know
1. **Hooks.** The script defines functions the runtime calls when something happens: the bot joined, it died, a timer is due, the server answered a command. A hook runs to its end at once; there is no `wait()`. To do something later, set a timer or hold a key for a time.
2. **One state object.** `state` holds whatever the script wants to remember between hooks: counters, flags, the step it's at.
3. **The game objects.** `keys`, `mouse`, `look`, `hotbar`, `vehicle`, `chat`, `timer`, `random` and `log` do things; `bot` and `world` say how things are.

Everything works the same in a hook and in the script's own functions.

```rhai
// Stays awake: a small turn every 45 seconds, and sometimes a jump.
fn on_join(first) {
    timer.every(45000, "move");
}

fn on_timer(name) {
    look.turn(random.between(-30, 30), random.between(-10, 10));
    if random.chance(30) {
        keys.jump.tap();
    }
}
```

## Part 1: the API of Phase 15

### Hooks
A script defines the hooks it needs and leaves the others out. A function whose name starts with `on_` and isn't in this table is refused when the mode is saved, so a misspelled hook can't go unnoticed.

| Hook | When it runs |
|---|---|
| `fn on_join(first)` | The bot is in the world after connecting. `first` is `true` for the first join since the bot was started, and `false` after a reconnect |
| `fn on_world_change()` | The server moved the bot to another world without a disconnect: a proxy's lobby and then the real server, or a portal |
| `fn on_death()` | The bot died. It can't act until it's back |
| `fn on_respawn()` | The bot lives again after a death |
| `fn on_timer(name)` | A timer the script set is due |
| `fn on_chat(key, values)` | The server printed a line that fits one of the mode's declared patterns (see [`chat`](#chat)) |
| `fn on_stop()` | The script stops in an orderly way: the bot is stopped, its mode is changed, or the mode was edited. It can change `state` and send one declared message; every key and button is released by the runtime right after it. It doesn't run after an error or a disconnect |

**Nothing is held at the start of `on_join`, `on_world_change`, `on_death` and `on_respawn`.** Each of them puts the bot somewhere new, so the runtime lets go of every key and button first, and the hook decides again.

**A join is early.** Right after a join the bot may not see the entities around it yet. `vehicle.mount` keeps looking for ten seconds for that reason; for anything else, set a short timer and look again.

### State
`state` is an object map. It's empty when the bot is started, and it's kept across reconnects, world changes, deaths and respawns. It's gone when the bot is stopped or its mode changes.

```rhai
fn on_join(first) {
    if first {
        state.joins = 0;
    }
    state.joins += 1;
}
```

- Set a property before reading it: reading one that was never set is an error with its line, so a typo shows.
- It holds numbers, text, `true` and `false`, arrays and maps, up to 64 entries and 4 KiB in all. A script that grows it past that stops at once, in the middle of its hook.

### Results
**Every action returns a result.** `result.ok` is `true` when the runtime took the action, and `result.reason` says why not otherwise. A simple script ignores it:

```rhai
keys.jump.tap();

if !mouse.left.tap().ok {
    log.info("Couldn't attack.");
}
```

| Reason | Meaning |
|---|---|
| `too_soon` | The action came faster than its limit allows (see [Limits](#limits)) |
| `dead` | The bot is dead |
| `not_in_world` | The bot is between worlds, or its session is ending |
| `leashed` | The bot is at the end of its leash, and this would take it further |
| `mining_guard` | The block is below the bot's feet |
| `tool_guard` | The held tool is nearly used up |
| `bad_value` | A number outside its range, an angle that isn't a number, an unknown kind |
| `too_many` | No room for another timer |
| `no_command` | `chat.expect` without a declared command sent before it in the same hook |

A release is never refused.

### `keys`
The keys of a vanilla keyboard.

| Key | Vanilla binding |
|---|---|
| `keys.forward`, `keys.back`, `keys.left`, `keys.right` | W, S, A, D |
| `keys.jump` | Space |
| `keys.sneak` | Shift |
| `keys.sprint` | Ctrl |
| `keys.drop` | Q: drops one of the held item |
| `keys.drop_stack` | Ctrl+Q: drops the held stack |
| `keys.swap_offhand` | F |

Every key has `tap()`. The first seven can also be held:

| Method | What it does |
|---|---|
| `tap()` | Press and let go: one jump, one step's worth of walking, one dropped item |
| `hold()` | Press and keep it down until `release()` |
| `hold_for(ms)` | Press, and let go by itself after `ms` milliseconds (50 to 600,000) |
| `release()` | Let go |
| `held` | Whether it's down now (a property, not a method) |

`keys.release_all()` lets go of every key.

```rhai
// Three seconds forward, sprinting.
keys.sprint.hold_for(3000);
keys.forward.hold_for(3000);
```

**The leash.** A bot walks at most a set number of blocks from where it was put: where it joined, respawned, changed worlds or was teleported by the server. At the end of the leash, a key that would take it further is released and returns `leashed`; walking back works. The radius is a setting of the mode (8 blocks unless changed), not of the script, and a script can't move the anchor.

### `mouse`
`mouse.left` and `mouse.right`, with the same four methods and `held`.

**Left button**
- `tap()` on an entity attacks it once. On anything else it swings the arm.
- `hold()` mines the block the bot looks at, and then the next one that appears there, as a held button does in vanilla. An entity in front of the bot is attacked once when the button goes down, never again while it's held.
- **The bot never mines a block below its feet,** which includes the one it stands on: the hold returns `mining_guard`, and a hold that's already down pauses while the bot looks at such a block.
- **The tool guard.** When the held tool has only a few uses left (2 unless the mode's setting says otherwise), mining stops, `held` turns `false`, and a new hold returns `tool_guard` until another item is in hand. `bot.held.durability` says how many uses are left.

**Right button**
- `tap()` uses the held item, or the block or entity the bot looks at.
- `hold()` keeps the button down. With an item that's used over time (a bow, a shield, food) it's one long use. With anything else it clicks again every 4 ticks, as the vanilla client does.
- `hold_until_used()` holds until the item is used up once (one piece of food eaten, one potion drunk) and then lets go by itself.

### `look`
| | |
|---|---|
| `look.at(yaw, pitch)` | Look in a direction, in degrees as the debug screen shows them: yaw −180 to 180, pitch −90 (up) to 90 (down) |
| `look.turn(yaw, pitch)` | Turn by that much from where the bot looks now |
| `look.at_position(x, y, z)` | Look at a point in the world |
| `look.at_entity(entity)` | Look at an entity `world` returned |
| `look.yaw`, `look.pitch` | Where the bot looks now |

### `hotbar`
`hotbar.select(n)` selects slot 1 to 9, as the number keys do. `hotbar.selected` is the selected slot.

### `vehicle`
- `vehicle.mount("minecart")` gets into the nearest free minecart within reach. A bot that already rides does nothing. With none in reach it keeps looking for ten seconds, then gives up. Whether it worked shows in `bot.riding` at a later hook.
- `vehicle.dismount()` gets out, as sneaking does.

Minecarts only, for now: fleet-mc keeps a bot's seat only there ([ADR-0026](adr/0026-a-bot-in-a-minecart.md)).

### `chat`
A script sends only messages its mode declares, each with a key. A message can have typed values:

| In the mode's settings | In the script |
|---|---|
| `home`: `/home farm` | `chat.send("home")` |
| `sell`: `/ah sell {price}`, `price` a whole number from 1 to 1,000,000 | `chat.send("sell", #{ price: 1500 })` |

- A value is a whole number in a range, a decimal number in a range with a set number of places, or one of a closed list of words.
- A command's name is always written out in the declaration, never a value. Whoever sets the mode on a bot needs the same rights as for a step mode that sends those commands.
- A script can't build text of its own.

**Reading the server's answer.** A script never sees chat as text. The mode declares patterns, each with a key, and a script gets the key and the numbers of a line that fits one:

| In the mode's settings | In the script |
|---|---|
| `balance`: `Balance: ${amount}`, a reply; `amount` an amount from 0 to 1,000,000,000 | `chat.expect("balance", 5000)` after the command; then `fn on_chat(key, values)` with `values.amount` |

- **Only the server's own lines count,** never what a player writes: not chat, not a private message, not `/me`. On a server whose plugins print player chat as server lines, the server's chat formats (in the app's server settings) tell those lines apart; set them before a script reads chat there.
- **The whole line must fit the pattern,** letter for letter. A pattern has no "any text" part. What changes in a line is a capture, and a capture is one of three kinds:

  | Capture | Reads | The script gets |
  |---|---|---|
  | `integer` | `42`, `-7` | A whole number |
  | `amount` | `1,234.56`, `12.5k`, `3M`, `2B`; the declaration says whether `.` or `,` is the decimal mark | A decimal number |
  | `word` | One of the words the declaration lists | That word |

  Every number has a range in the declaration, and a line whose number is outside it doesn't fit.
- **A reply counts only when the script asked.** `chat.expect(key, ms)` right after `chat.send` of a declared command opens a window of `ms` milliseconds (250 to 30,000) for that pattern, from the moment the command is sent. The first line that fits closes it. Without an open window, a reply pattern fits nothing.
- **A pattern can be declared to count at any time** instead. It then needs no `chat.expect`, the app marks the mode as riskier, and it's delivered at most once a second. Prefer a reply wherever a command causes the line.
- **A value from chat is a number or one of your own words.** It can go into a declared message's value, inside that value's range, and nowhere else: a recipient or a command is always written in the declaration.
- Every message sent, every window and every match is a line in the script's log.

### `timer`
| | |
|---|---|
| `timer.after(ms, name)` | Calls `on_timer(name)` once, `ms` milliseconds from now (250 to 86,400,000, which is 24 hours) |
| `timer.every(ms, name)` | Calls it every `ms` milliseconds (the same range), first after `ms` |
| `timer.cancel(name)` | Removes that timer |

A name holds one timer: setting it again replaces the old one. Timers wait while the bot is dead or between worlds, and they're cleared when it disconnects, so `on_join` is where a script starts them.

### `random` and `log`
- `random.between(low, high)` is a whole number from `low` to `high`, both included. `random.chance(percent)` is `true` that often.
- `log.info(text)` writes a line into the script's log, which the app shows on the bot's page: up to 200 characters, 20 lines a minute.

### `bot`: read-only
| | |
|---|---|
| `bot.health` | 0 to 20 |
| `bot.food` | 0 to 20 |
| `bot.x`, `bot.y`, `bot.z` | Where its feet are |
| `bot.dimension` | Such as `"minecraft:overworld"` |
| `bot.riding` | Whether it sits in a vehicle |
| `bot.held` | The item in its hand, or `()` for an empty hand: `id` (such as `"minecraft:iron_pickaxe"`), `count`, `durability` (uses left, or `()` for an item that doesn't wear) |
| `bot.looking_at` | What its crosshair is on within reach, or `()`: `kind` (`"block"` or `"entity"`), `id`, `x`, `y`, `z` |

### `world`: read-only
| | |
|---|---|
| `world.nearest(kind)` | The nearest entity of that kind within 16 blocks, or `()`: `kind`, `x`, `y`, `z`, `distance`, `occupied` |
| `world.nearest_free(kind)` | The same, skipping vehicles someone sits in |
| `world.block(x, y, z)` | The block's ID there, such as `"minecraft:lava"`, or `()` further than 4 blocks from the bot |

**A hook sees the world as it was when the hook started.** What its own actions changed shows at the next hook.

### Limits
A script is never faster than a step mode. The limits sit below the script, so no script can go around them.

| Action | At most |
|---|---|
| Each key, each look, hotbar, mount, dismount, drop, swap | Once every 250 ms |
| Right button `tap()` | Once every 200 ms (4 ticks) |
| Left button `tap()`, and holding or releasing the right button | Once every 500 ms |
| `chat.send` of plain chat | Once every 30 s |
| `chat.send` of a declared command | As fast as the bot's own chat rate allows, the server's setting that also paces what's typed in the app |

A script itself is limited too:

| | Limit |
|---|---|
| Size | 16 KiB |
| Work in one hook | 10,000 operations: far more than any example here uses, and a loop that never ends stops there |
| Work in one second | 30,000 operations |
| Functions, call depth | 64, 16 deep |
| A text, an array, a map | 1,024 characters, 256 items, 64 entries |
| Timers | 16, each 250 ms to 24 hours |
| Patterns, declared messages | 8, 16 |

**When a script hits a limit or fails,** it stops, every key and button is released, and the bot stays online doing nothing. The app shows the error with its line. It starts again when the bot is started again or its mode is saved or changed.

### Errors
- **When the mode is saved,** the server checks the script: its syntax, the hook names, every variable name, and that no variable or parameter is called like a game object. An error names its line and column.
- **A misspelled method** (`keys.forward.hodl()`) can only be found when that line runs: Rhai doesn't know a value's type before. The script stops there, with the line.
- `import` and `eval` aren't part of the language here.

### Examples
#### An AFK farm
```rhai
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
```

#### The minecart farm
```rhai
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
```

#### A cobblestone generator
```rhai
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
```

#### Paying out a balance
The mode declares two messages and one pattern:

| | Key | Text | Values and captures |
|---|---|---|---|
| Message | `balance` | `/balance` | |
| Message | `pay` | `/pay AfkMain {amount}` | `amount`: a decimal number from 1 to 1,000,000 with 2 places |
| Pattern, a reply | `balance` | `Balance: ${amount}` | `amount`: an amount from 0 to 1,000,000,000 |

```rhai
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
```

The recipient is text of the declaration: nothing the server prints can change who is paid. A line a player writes never reaches `on_chat`.

## Part 2: inventory and menus (Phase 16, planned)

> Phase 16 adds to Part 1 and changes nothing in it. A script that uses anything below needs agents on that phase's version.

A server's menu is a container with another look: a chest-shaped screen where a button is an item with a name and a few lines of lore. So a script reads slots and clicks slots, as a player does.

### Slots
A slot, wherever it comes from:

| | |
|---|---|
| `slot.index` | Its number in the screen it belongs to |
| `slot.empty` | Whether nothing is in it |
| `slot.id`, `slot.count`, `slot.durability` | As for `bot.held` |
| `slot.name` | The item's custom name as plain text, or `""` |
| `slot.lore` | Its lore lines as plain text |
| `slot.lore_number(label)` | The number on the lore line that starts with `label`, or `()`: `"Price: 12,500"`, `"Price: 12.5k"` and `"Price: 3M"` all read as numbers |
| `slot.lore_has(text)` | Whether a lore line contains `text` |

`number(text)` reads a number the same way out of any text. Names and lore are what the server sent, cleaned like chat: plain text, no formatting, bounded in length.

### `inventory`
The bot's own 36 slots, its armour and its offhand.

| | |
|---|---|
| `inventory.slots` | Every slot |
| `inventory.find(id)` | The first slot holding that item, or `()` |
| `inventory.count(id)` | How many of that item the bot carries |
| `inventory.open()` | Opens the inventory screen, as E does. It's then the open `menu` |

### `menu`
The screen that's open, if one is.

| | |
|---|---|
| `menu.open` | Whether a screen is open |
| `menu.kind` | Such as `"generic_9x6"`, `"merchant"`, `"anvil"`, `"enchantment"`, `"inventory"` |
| `menu.title` | Its title as plain text |
| `menu.slots` | The screen's own slots |
| `menu.inventory` | The bot's slots as this screen shows them |
| `menu.cursor` | The item on the cursor, or `()` |
| `menu.pending` | Whether a click is still unanswered |
| `menu.find(id)`, `menu.find_named(text)` | The first slot with that item, or whose name contains that text, or `()` |

| Click | Vanilla |
|---|---|
| `menu.click(slot)` | Left click |
| `menu.right_click(slot)` | Right click |
| `menu.shift_click(slot)` | Shift + left click |
| `menu.number_key(slot, n)` | Number key 1 to 9 over the slot |
| `menu.drop(slot)`, `menu.drop_stack(slot)` | Q and Ctrl+Q over the slot |
| `menu.button(n)` | A screen's own button: an enchantment, a stonecutter's recipe |
| `menu.select_trade(n)` | A villager's trade |
| `menu.rename(key, values)` | The anvil's name field, from a declared message |
| `menu.close()` | Esc |

**A menu opens** when the script sends a declared command, right-clicks a block or an entity, or calls `inventory.open()`, and the server answers with a screen. **While a screen is open, every key and mouse button is released,** as in a vanilla client.

### Hooks
| Hook | When it runs |
|---|---|
| `fn on_menu_open()` | A screen opened. A new screen with the same title as the last one is still a new screen |
| `fn on_menu_update()` | The open screen's content changed |
| `fn on_menu_close()` | The screen closed, by the script or by the server |
| `fn on_click(result)` | A click's fate is known: `result.slot`, `result.ok`, `result.reason` |

### A click never lands on the wrong thing
A hook sees the screen as it was when the hook started. Between that moment and the click, the server may have changed the screen. So every click is checked right before it's sent:

- the same screen is still open,
- its content hasn't changed since the hook saw it,
- the clicked slot still holds the item the hook saw.

If any of the three fails, nothing is sent, and the click returns the reason.

| Reason | Meaning |
|---|---|
| `no_menu` | No screen is open |
| `screen_changed` | Another screen is open than the one the hook saw |
| `content_changed` | The screen's content changed since the hook saw it |
| `slot_changed` | The slot holds something else now |
| `click_pending` | An earlier click on this screen has no answer yet |
| `screen_open` | For a key or a mouse button: a screen is open |
| `too_soon` | Clicks come faster than the click limit allows |

**One click at a time.** A screen takes no click while an earlier one is unanswered, so one listing is never bought twice. The answer arrives as `on_click`:

| `result.reason` | Meaning |
|---|---|
| (none, `ok` is `true`) | The server took the click and the screen moved on |
| `rejected` | The server put things back as they were |
| `unanswered` | No answer came in time. The screen takes no more clicks; close it and open it again |

Every click that wasn't sent, was rejected or stayed unanswered is a line in the script's log.

### Examples
#### An auction scan
```rhai
// Opens the auction house every 10 seconds and buys the cheapest diamond
// listing, if it costs 1,500 or less.
fn on_join(first) {
    if first {
        state.bought = 0;
    }
    timer.every(10000, "scan");
}

fn on_timer(name) {
    if !menu.open {
        chat.send("auction");
    }
}

fn on_menu_open() {
    look_for_a_bargain();
}

fn on_menu_update() {
    look_for_a_bargain();
}

fn on_click(result) {
    if result.ok {
        state.bought += 1;
    } else {
        log.info("Missed a listing: " + result.reason);
    }
    menu.close();
}

fn look_for_a_bargain() {
    if menu.pending {
        return;
    }
    let best = cheapest("minecraft:diamond");
    if best != () && best.price <= 1500 {
        menu.click(best.slot);
    }
}

// The cheapest listing of an item on this page: its slot and its price.
fn cheapest(item) {
    let best = ();
    for slot in menu.slots {
        if slot.id != item {
            continue;
        }
        let price = slot.lore_number("Price");
        if price == () {
            continue;
        }
        if best == () || price < best.price {
            best = #{ slot: slot.index, price: price };
        }
    }
    best
}
```

#### A sell macro
```rhai
// Every minute: opens the server's sell menu, moves every stack of slime
// balls into it and closes it, which sells them on this kind of server.
fn on_join(first) {
    timer.every(60000, "sell");
}

fn on_timer(name) {
    if inventory.count("minecraft:slime_ball") > 0 && !menu.open {
        chat.send("sell_menu");
    }
}

fn on_menu_open() {
    if menu.title != "Sell" {
        log.info("Not the sell menu: " + menu.title);
        menu.close();
        return;
    }
    move_next();
}

fn on_click(result) {
    move_next();
}

// Moves one stack into the menu, or closes it when none is left.
fn move_next() {
    for slot in menu.inventory {
        if slot.id == "minecraft:slime_ball" {
            menu.shift_click(slot.index);
            return;
        }
    }
    menu.close();
}
```
