# Scripted modes: the script reference

This is the reference for script authors: Admins and the Owner, who write scripted modes in the app's mode editor. Part 1 is the API of Phase 15, API level 1. Part 2 is Phase 16's, API level 2: the inventory, a server's menus and signs, and the time of day. The design behind it is in [ADR-0036](adr/0036-scripted-modes.md) and [ADR-0037](adr/0037-scripts-read-system-chat.md), and how it was built in ADR-0040 to [ADR-0046](adr/0046-the-stopgaps-way-out-and-phase-15s-check.md).

**The tables of each object's members, of the hooks and of the reasons are generated** from the one description of the API that the editor's completion and hover read too (`crates/fleet-script/src/api/describe.rs`): `just gen` writes them, and a test holds them equal. Change a member there, never in a table here.

A scripted mode is a mode whose behaviour is a small program instead of a list of steps. The program is written in [Rhai](https://rhai.rs/book/), a small language that looks like JavaScript and Rust.

**A script is a virtual player at a vanilla client.** It presses the keys and the mouse buttons a player presses, looks where a player looks, and reads what a player sees on the screen. Each input behaves as it does in the vanilla client: holding the right button with a block in hand clicks again every 4 ticks, holding the left one on a block mines it. Nothing a player can't do is in the API.

**Whether a server allows what your script does is yours to check.** Many servers have rules about automated farming, trading and auction buying.

**A scripted mode runs only on an agent that runs scripts:** an agent of this release or later. The server never sends one to an older agent; a bot whose agent is older waits, and its page says the agent must be updated.

## Three things to know
1. **Hooks.** The script defines functions the runtime calls when something happens: the bot joined, it died, a timer is due, the server answered a command. A hook runs to its end at once; there is no `wait()`. To do something later, set a timer or hold a key for a time.
2. **One state object.** `state` holds whatever the script wants to remember between hooks: counters, flags, the step it's at.
3. **The game objects.** `keys`, `mouse`, `look`, `hotbar`, `vehicle`, `chat`, `timer`, `random` and `log` do things; `bot` and `world` say how things are. At API level 2, `inventory`, `menu`, `sign` and `clock` join them (Part 2).

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
A script defines the hooks it needs and leaves the others out. A function whose name starts with `on_` and isn't in this table is refused when the mode is saved, so a misspelled hook can't go unnoticed. The last five are API level 2's (Part 2).

<!-- generated table: hooks -->
| Hook | When it runs |
|---|---|
| `fn on_join(first)` | The bot is in the world after connecting; first is false after a reconnect. |
| `fn on_world_change()` | The server moved the bot to another world without a disconnect. |
| `fn on_death()` | The bot died. |
| `fn on_respawn()` | The bot lives again after a death. |
| `fn on_timer(name)` | A timer the script set is due. |
| `fn on_chat(key, values)` | The server printed a line that fits one of the mode's declared patterns. |
| `fn on_stop()` | The script stops in an orderly way. |
| `fn on_menu_open()` | A screen opened: a server's, or the inventory. Everything held was let go. |
| `fn on_menu_update()` | The open screen's content changed: at most once a game tick. |
| `fn on_menu_close()` | The screen closed. |
| `fn on_click(result)` | What became of a click, a button, a trade or a name: ok, reason and slot. |
| `fn on_sign_open()` | The server opened a sign editor. |
<!-- end of generated table -->

- **`on_join(first)`:** `first` is `true` for the first join since the bot was started, and `false` after a reconnect.
- **`on_world_change()`:** a proxy's lobby and then the real server, or a portal.
- **`on_death()`:** the bot can't act until it's back.
- **`on_stop()`** runs when the bot is stopped or removed, its agent shuts down, its mode is changed, or the mode was edited. It can change `state` and send one declared message, which goes out at once, ahead of anything still waiting in the bot's chat queue; every key and button is released by the runtime right after it. It doesn't run after an error or a disconnect, and a bot whose session doesn't answer is stopped without it after two seconds.

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

- Set a property before reading it: reading one that was never set is an error with its line, so a typo shows. (`state.joins += 1` reads it too; the error then names the sum, `+`, at that line.)
- **It holds plain data:** nothing (`()`), `true` and `false`, numbers, text, and arrays and maps of those. A result, an entity and a held item are maps, so they can be kept. A function (`Fn("name")`) or a game object (`keys`, `keys.jump`, `bot`) can't: a script that puts one into its state stops.
- **Up to 64 entries and 4 KiB in all.** Every entry of a map and every item of an array counts, at any depth; a number counts 8 bytes, a text and a key their bytes. A script that grows its state past that stops at once, in the middle of its hook.
- `state = …` doesn't replace it: a script whose state isn't a map any more stops.
- **Don't use `state` inside a function that a method of something in `state` calls.** `state.list.filter(Fn("keep"))` runs `keep` while the list is in use, and if `keep` reads or writes `state`, the script stops. Take a copy first: `let list = state.list;`.

### Results
**Every action returns a result.** `result.ok` is `true` when the runtime took the action, and `result.reason` says why not otherwise. A simple script ignores it:

```rhai
keys.jump.tap();

if !mouse.left.tap().ok {
    log.info("Couldn't attack.");
}
```

<!-- generated table: reasons -->
| Reason | Meaning |
|---|---|
| `too_soon` | The action came faster than its limit allows (see Limits). |
| `dead` | The bot is dead. |
| `not_in_world` | The bot is between worlds, or its session is ending. |
| `leashed` | The bot is at the end of its leash, and this would take it further. |
| `mining_guard` | The block reaches below the bot's feet. |
| `tool_guard` | The held tool is nearly used up. |
| `block_guard` | The block isn't one the mode lists for mining. |
| `bad_value` | A number outside its range, an angle that isn't a number, an unknown kind. |
| `too_many` | No room for another timer. |
| `no_command` | chat.expect without a declared command sent before it in the same hook. |
| `screen_open` | A screen is open: keys, buttons, looks, the hotbar and vehicles wait, and the inventory doesn't open. |
| `no_menu` | No screen is open. |
| `screen_changed` | Another screen is open than the one the hook saw, or than the one the slot was read on. |
| `content_changed` | The screen's content changed since the hook saw it. |
| `slot_changed` | A slot the click touches holds something else than the hook or the slot saw. |
| `click_pending` | An earlier action's fate hasn't come yet. |
| `unanswered` | No answer came within 5 s: the server may or may not have done it. The screen takes no click until it's opened again. |
| `no_sign` | No sign editor waits for an answer. |
| `sign_changed` | Another sign editor is open than the one the hook saw. |
| `sign_answered` | The sign editor was answered already. |
| `rejected` | The server corrected the screen: it put things back or did otherwise. A plugin's button answers so though it acted; judge by the screen that follows. |
| `unclear` | The server corrected the screen after a click on a result slot (a trade, an anvil's, a craft's), a button, a trade or a name: it may have done it, once or more. Judge by the screen. |
| `replaced` | The screen closed, or another opened, before the answer: the server may have done it. |
<!-- end of generated table -->

A release is never refused.

### `keys`
The keys of a vanilla keyboard.

<!-- generated table: keys -->
| Member | What it is or does |
|---|---|
| `keys.forward` | W: walk forward. |
| `keys.back` | S: walk back. |
| `keys.left` | A: strafe left. |
| `keys.right` | D: strafe right. |
| `keys.jump` | Space. |
| `keys.sneak` | Shift. |
| `keys.sprint` | Ctrl. |
| `keys.drop` | Q: drops one of the held item. Tap only. |
| `keys.drop_stack` | Ctrl+Q: drops the held stack. Tap only. |
| `keys.swap_offhand` | F: swaps the held item with the offhand's. Tap only. |
| `keys.release_all()` | Lets go of every key. |
<!-- end of generated table -->

Every key has `tap()`, and the first seven can also be held. Each of them is an input:

<!-- generated table: input -->
| On a key or a button | What it is or does |
|---|---|
| `tap()` | Press and let go. |
| `hold()` | Press and keep down until release(). |
| `hold_for(ms)` | Press, and let go by itself after ms milliseconds (50 to 600,000). |
| `hold_until_used()` | The right button only: hold until the item is used up once. |
| `release()` | Let go. Never refused. |
| `held` | Whether it's down now. |
<!-- end of generated table -->

Holding one of the three keys that only tap (`keys.drop.hold()`) is a mistake no value excuses: the script stops there, with the line.

```rhai
// Three seconds forward, sprinting.
keys.sprint.hold_for(3000);
keys.forward.hold_for(3000);
```

**The leash.** A bot walks at most a set number of blocks from where it was put: where it joined, respawned, changed worlds or was teleported by the server. At the end of the leash, a direction key that would take it further, by where it looks, is released and returns `leashed`; walking back and along the edge works. The radius is a setting of the mode (8 blocks unless changed), not of the script, and a script can't move the anchor.

### `mouse`

<!-- generated table: mouse -->
| Member | What it is or does |
|---|---|
| `mouse.left` | Attack; held on a block, mine. |
| `mouse.right` | Use. |
<!-- end of generated table -->

Each button is an input, with the same methods and `held` as a key.

**Left button**
- `tap()` on an entity attacks it once. On anything else it swings the arm.
- `hold()` mines the block the bot looks at, and then the next one that appears there, as a held button does in vanilla. An entity in front of the bot is attacked once when the button goes down, never again while it's held.
- **The bot never mines a block that reaches below its feet:** the one it stands on, whatever its shape, everything under it, and the blocks beside it at that height. The hold returns `mining_guard`, and a hold that's already down pauses while the bot looks at such a block. So aim at feet level or higher from solid ground: a bot standing on a slab, on soul sand or on a carpet mines at eye level only, and so does one in mid-jump.
- **The tool guard.** When the held tool has only a few uses left (2 unless the mode's setting says otherwise), mining stops, `held` turns `false`, and a new hold returns `tool_guard` until another item is in hand. `bot.held.durability` says how many uses are left.
- **The block guard.** A held button mines whatever the crosshair lands on next. Once a block is gone, that's the block behind it, exactly as in vanilla: at a cobblestone generator it's the wall behind the generator's block while the next one forms, and on a real farm that wall may hold the lava. So a mode can list the blocks its script mines: a setting of the mode, up to 16 block IDs such as `cobblestone`. On any other block a hold pauses, and goes on by itself once the bot looks at a listed block again; a new hold on a block that isn't listed returns `block_guard`. A mode that lists nothing mines any block. The three guards answer in this order: `mining_guard`, `tool_guard`, `block_guard`.
- **A block the server won't let the bot break** (a spawn protection, a claim) is mined to its end, put back by the server, and mined again, for as long as the button is held. Nothing is gained and the tool isn't worn, and a script can't tell it from a block that grows back.

**Right button**
- `tap()` uses the held item, or the block or entity the bot looks at.
- `hold()` keeps the button down. With an item that's used over time (a bow, a shield, food) it's one long use. With anything else it clicks again every 4 ticks, as the vanilla client does.
- `hold_until_used()` holds until the item is used up once (one piece of food eaten, one potion drunk) and then lets go by itself.

### `look`

<!-- generated table: look -->
| Member | What it is or does |
|---|---|
| `look.at(yaw, pitch)` | Look in a direction, in degrees: yaw -180 to 180, pitch -90 (up) to 90 (down). |
| `look.turn(yaw, pitch)` | Turn by that much from where the bot looks now. |
| `look.at_position(x, y, z)` | Look at a point in the world. |
| `look.at_entity(entity)` | Look at an entity world returned. |
| `look.yaw` | Where the bot looks, left and right. |
| `look.pitch` | Where the bot looks, up and down. |
<!-- end of generated table -->

Angles are in degrees as the debug screen shows them: yaw −180 to 180, pitch −90 (up) to 90 (down).

### `hotbar`

<!-- generated table: hotbar -->
| Member | What it is or does |
|---|---|
| `hotbar.select(n)` | Select slot 1 to 9. |
| `hotbar.selected` | The selected slot, 1 to 9. |
<!-- end of generated table -->

The slots are numbered 1 to 9, as the number keys are.

### `vehicle`

<!-- generated table: vehicle -->
| Member | What it is or does |
|---|---|
| `vehicle.mount(kind)` | Get into the nearest free vehicle of that kind within reach: "minecart". |
| `vehicle.dismount()` | Get out. |
<!-- end of generated table -->

- `vehicle.mount("minecart")` gets into the nearest free minecart within reach, passing over one someone sits in. A bot that already rides does nothing. With none in reach it keeps looking for ten seconds, then gives up. Whether it worked shows in `bot.riding` at a later hook.
- `vehicle.dismount()` gets out, as sneaking does.

Minecarts only, for now: fleet-mc keeps a bot's seat only there ([ADR-0026](adr/0026-a-bot-in-a-minecart.md)).

### `chat`

<!-- generated table: chat -->
| Member | What it is or does |
|---|---|
| `chat.send(key, values)` | Send the declared message key; values is a map of its values, left out for none. |
| `chat.expect(key, ms)` | After a declared command in the same hook: read the reply pattern key for ms milliseconds (250 to 30,000). |
<!-- end of generated table -->

A script sends only messages its mode declares, each with a key. A message can have typed values:

| In the mode's settings | In the script |
|---|---|
| `home`: `/home farm` | `chat.send("home")` |
| `sell`: `/ah sell {price}`, `price` a whole number from 1 to 1,000,000 | `chat.send("sell", #{ price: 1500 })` |

- A value is a whole number in a range, a decimal number in a range with a set number of places, or one of a closed list of words.
- **A decimal number where a whole one is declared counts down** to the next whole number, as everywhere else in the API, and **a decimal value with places counts down at its last place,** never to the nearest: a message never sends more than the script gave. A decimal's binary noise doesn't count (`1234.56 - 1000` sends 234.56, not 234.55). The range is judged on what's sent.
- **A message may start with a number's value** (`{price}` alone, for an anvil's name box): no number is a command. A word's value may not come first.
- A command's name is always written out in the declaration, never a value. Whoever sets the mode on a bot needs the same rights as for a step mode that sends those commands.
- Below the right to edit the mode, a declared command shows as its `/` and name alone, as a step's chat does: a command can hold a password.
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
- Every message sent, every window and every match is a line in the script's log, written by the runtime: a message that wasn't sent with the reason, a window that opened, one that ran out, one that never opened because its command wasn't sent, a match with its values, and a `chat.send` or `chat.expect` that was refused. They don't count against the script's own 20 lines a minute; the lines about what was refused have 20 a minute of their own.
- **A window opens only when the server really got the command.** The bot's chat queue may refuse it (full, or the bot's chat rate used up), or the session may be unable to send it: no window opens then, and the log says why.
- **Tested on a real server:** a player who writes the reply's exact text as chat, as a whisper or with `/me`, who dies to an item renamed to it or joins under a name as close to it as a name can be, never makes the balance example pay, even inside its window; the server's own line does, inside the window alone.

### `timer`

<!-- generated table: timer -->
| Member | What it is or does |
|---|---|
| `timer.after(ms, name)` | Call on_timer(name) once, ms milliseconds from now (250 to 86,400,000). |
| `timer.every(ms, name)` | Call on_timer(name) every ms milliseconds, first after ms. |
| `timer.cancel(name)` | Remove that timer. |
<!-- end of generated table -->

A name holds one timer: setting it again replaces the old one. A script holds at most 16. Timers wait while the bot is dead or between worlds, and they're cleared when it disconnects, so `on_join` is where a script starts them. Timers that are due at the same moment run one after the other, in the order they were set, and other bots' hooks may run in between: on an agent whose scripts keep it busy, a timer's hook can begin some milliseconds late.

### `random` and `log`

<!-- generated table: random -->
| Member | What it is or does |
|---|---|
| `random.between(low, high)` | A whole number from low to high, both included. |
| `random.chance(percent)` | true that often, in percent. |
<!-- end of generated table -->

<!-- generated table: log -->
| Member | What it is or does |
|---|---|
| `log.info(text)` | Write a line: up to 200 characters, 20 lines a minute. |
<!-- end of generated table -->

The log shows on the bot's page in the app.

### `bot`: read-only

<!-- generated table: bot -->
| Member | What it is or does |
|---|---|
| `bot.name` | The bot's own player name, as its account has it. API level 2. |
| `bot.health` | Health, 0 to 20. |
| `bot.food` | Food, 0 to 20. |
| `bot.x` | Where its feet are: east (positive) and west. |
| `bot.y` | Where its feet are: up (positive) and down. |
| `bot.z` | Where its feet are: south (positive) and north. |
| `bot.dimension` | The world it's in, such as "minecraft:overworld". |
| `bot.riding` | Whether it sits in a vehicle. |
| `bot.held` | The item in its hand, or () for an empty hand: id, count, durability. |
| `bot.looking_at` | What its crosshair is on within reach, or (): kind, id, x, y, z. |
<!-- end of generated table -->

`bot.held` is a map of `id` (such as `"minecraft:iron_pickaxe"`), `count` and `durability` (uses left, or `()` for an item that doesn't wear). `bot.looking_at` is a map of `kind` (`"block"` or `"entity"`), `id`, `x`, `y` and `z`.

### `world`: read-only

<!-- generated table: world -->
| Member | What it is or does |
|---|---|
| `world.nearest(kind)` | The nearest entity of that kind within 16 blocks, or (): kind, x, y, z, distance, occupied. |
| `world.nearest_free(kind)` | The same, skipping vehicles someone sits in. |
| `world.block(x, y, z)` | The block's ID there, or () further than 4 blocks from the bot. |
<!-- end of generated table -->

An entity is a map of `kind`, `x`, `y`, `z`, `distance` and `occupied`. **A hook sees the world as it was when the hook started.** What its own actions changed shows at the next hook.

### Limits
A script is never faster than a step mode. The limits sit below the script, so no script can go around them.

| Action | At most |
|---|---|
| Each key, each look, hotbar, mount, dismount, drop, swap | Once every 250 ms |
| Right button `tap()` | Once every 200 ms (4 ticks) |
| Left button `tap()`, and holding or releasing the right button | Once every 500 ms |
| `chat.send` of plain chat | Once every 30 s |
| `chat.send` of a declared command | As fast as the bot's own chat rate allows, the server's setting that also paces what's typed in the app |
| A click, a screen's button, a trade, an anvil's name, a sign's answer | Once a game tick (50 ms), one at a time: the next after the last one's fate (Part 2) |
| `menu.close()`, `inventory.open()` | Never limited |

A script itself is limited too:

| | Limit |
|---|---|
| Size | 16 KiB |
| Work in one hook | 10,000 operations: far more than any example here uses, and a loop that never ends stops there |
| Work in one second | 30,000 operations, over all of a script's hooks |
| Time in one hook | 5 ms. A hook that counts, compares and calls the API takes a few microseconds; a hook that runs a loop to its 10,000 operations about a fifth of a millisecond. What takes long is calling a function of your own through an array's method (`list.map(Fn("f"))`) many thousand times, or building texts and arrays past their limits |
| Time in one second | 20 ms, over all of a script's hooks |
| Functions, call depth | 64; a hook's functions call each other 16 deep |
| Nesting inside a function | 32 levels as Rhai counts them: 9 `if`s or loops inside each other, 14 calls or parentheses inside each other, a sum of 29 terms, an `if … else if` chain of 27 branches (use `switch` for more). Give a deep part a function of its own |
| A text, an array, a map | 1,024 bytes (letters outside ASCII take two or more), 256 items, 64 entries |
| Variables in one function | 64 |
| State | 64 entries and 4 KiB (see [State](#state)) |
| Timers | 16, each 250 ms to 24 hours |
| Patterns, declared messages | 8, 16 |

**When a script hits a limit or fails,** it stops, every key and button is released, and the bot stays online doing nothing. The app shows the error with its line; a limit has no line, so the app names the hook that ran. It starts again when the bot is started again or its mode is saved or changed. A script that loops, recurses or fills its state stops by itself, and the other bots on its agent go on: tested with hundreds of scripts at their budgets beside step-mode bots.

**What the language leaves out.** It's Rhai without a few things:
- **No anonymous functions** (`|x| x + 1`). Give the function a name and pass `Fn("name")`: `list.filter(Fn("is_big"))`.
- **Nothing outside a function:** a script is functions only, so no `let` or `const` at the top. A value every function needs is a function that returns it, or lives in `state`.
- **No `import`, no `eval`, no `sleep`, no clock.** A script's only output is its actions and its log: `print` and `debug` go nowhere.
- **No raw strings** (`#"…"#`): write a text in quotes, or in backticks across lines.
- **No variable, parameter, loop variable or caught error named like a game object** or `state`.

### Errors
- **When the mode is saved,** the server checks the script: its syntax, the hook names and their parameters, every variable name, that no variable or parameter is called like a game object, that nothing stands outside a function, and the limits for functions and nesting. An error names its line and column. The app's editor runs the same check while you type.
- **A misspelled method** (`keys.forward.hodl()`) can only be found when that line runs: Rhai doesn't know a value's type before. The script stops there, with the line. The editor marks one it can tell from the API's description.
- **A wrong value is a result, a wrong program an error.** A number out of range or a text where a number belongs returns `bad_value`, and the script goes on. A key the mode doesn't declare (`chat.send("nope")`), `chat.expect` for a pattern that isn't a reply, a hold of a key that only taps: these stop the script with their line.
- A script can catch its own errors with `try` and `catch`. It can't catch its way past a limit: the work and the time in a hook, the state's limits and the depth of its calls end the hook whatever it catches. (One exception that gains a script nothing: a text or an array that grows past its limit inside a function called through `Fn(…)` by an array's method can be caught. The hook's work and time still end it.)

### Converting a step mode
**"Convert to script"** in the mode editor writes a step mode as the script that does what its steps do, for an Admin or the Owner. Nothing changes until the mode is saved, and "Keep the steps" goes back. Every line of the script is marked with the step it does, named as the editor names it (`// while it's online, step 2`).

- **Each step is a line of a hook:** a step when it joins in `on_join`, one after it respawns in `on_respawn`, and a step while it's online on a timer of its own, set in `on_join` and named after it (`online_2`). Steps next to each other on one schedule without jitter share a timer. A jitter is a random part of the gap, drawn again before each run. A probability is a `random.chance` around the step's line; a run it skips is due again all the same.
- **Each action is the script's call that does it:** `look.at`, `look.turn`, `keys.jump.tap()`, `keys.sneak.hold()` and `release()`, `mouse.right.tap()`, `mouse.right.hold()` and `release()`, and `hotbar.select`.
- **A chat step becomes a declared message,** keyed by its step (`join_1`), and the script sends it by its key: its text is never in the script. Below the right to edit the mode it shows as its `/` and name, as the step did. A brace in its text is written doubled. A step whose command's name holds a brace, or whose text holds an invisible character, can't be declared: the editor says so at the step.
- **An on-respawn chat goes at most once every 30 seconds,** as the step did.

**What a script does differently.** The editor lists what applies to the mode's own steps when it converts:

| A step | In a script |
|---|---|
| Swing the arm | A swing is a left click, which hits an entity under the crosshair, so the converted step swings only while the crosshair is on none |
| Attack what's in front | A left click on nothing would swing, so the converted step clicks only while the crosshair is on an entity, as the step attacked only one |
| A swing more often than every 0.5 s, a use more often than every 0.25 s | At most that often: a left click's limit, and a timer's shortest |
| Two steps that do the same kind of thing | They share that kind's limit: a run that comes too soon after the other's is skipped. Two at the same moment (both when it joins, both after it respawns): the second |
| Hold right-click | A held right button with an item that isn't used over time uses it again every 4 ticks, as in vanilla |
| A gap longer than 24 hours (an interval plus its jitter) | At most 24 hours, a timer's longest |

And in every script, whatever the steps were: the runtime lets go of every key and button when the bot dies, respawns or changes worlds (a step mode lets go after a respawn only when it has steps for it), and a timer waits while the bot is dead and runs right after the respawn, where a step mode's run while dead did nothing.

**A mode that repeats more than 16 steps on schedules of their own** can't be converted: a script has 16 timers. Put repeating steps with one interval and no jitter next to each other, and they share one.

**Getting into the nearest minecart** was a step until 0.2.1 ([ADR-0034](adr/0034-a-stopgap-action-into-the-nearest-minecart.md)); a script does it with `vehicle.mount("minecart")`, which passes over a minecart someone sits in. A step mode that still had it is converted on 0.2.1 before the update: the release after it refuses to start while one is stored (the runbook's "Updating").

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

The mode lists `cobblestone` as the one block it mines. While the next block forms, the crosshair is on the wall behind the generator: the hold pauses there, and a new hold returns `block_guard`, which the script answers by trying again a second later.

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

## Part 2: inventory, menus, signs and the time of day (Phase 16)

Phase 16 adds to Part 1 and changes nothing in it. **What's here is API level 2:** the objects `inventory`, `menu`, `sign` and `clock`, `bot.name`, and the hooks `on_menu_open`, `on_menu_update`, `on_menu_close`, `on_click` and `on_sign_open`. A script that uses any of them needs agents of this release or later, and the server never sends it to an older one. **The server saves each script at the lowest level that runs it:** a script that uses none of these stays at level 1, so the last release's agents still run it, and declaring a sign needs level 2 whatever the script uses. Under the editor, the app says which level a script is and why. In a level-1 script these names are the script's own, as they always were: `let menu = 3;` still works there, and naming the object `menu` is refused when the mode is saved, with the level it needs.

A server's menu is a container with another look: a chest-shaped screen where a button is an item with a name and a few lines of lore. So a script reads slots and clicks slots, as a player does. **To see a screen as a script reads it,** open it with the bot and press Look in "What it sees" on the bot's page: each slot with its number, its row and column, and its item's ID, name, lore and components, as text to copy.

### Slots
A slot is a map of plain data: a script can keep one in its state. `menu.slot(n)` and `inventory.slot(n)` read one; a list of slots is a list of their numbers, which a script reads one by one:

```rhai
for n in menu.slots {
    let slot = menu.slot(n);
    if slot.id == "minecraft:diamond" {
        log.info(`A diamond in slot ${n}.`);
    }
}
```

<!-- generated table: slot -->
| Member | What it is or does |
|---|---|
| `slot.index` | Its number in the screen, or in the inventory. |
| `slot.row` | Its row in a grid, from 0 at the top; () outside one. |
| `slot.column` | Its column in a grid, from 0 at the left; () outside one. |
| `slot.screen` | The screen it was read on (menu.screen); () for the inventory's own. |
| `slot.empty` | Whether it holds nothing. |
| `slot.id` | The item's registry ID; () when empty. |
| `slot.count` | How many; 0 when empty. |
| `slot.name` | The name the client shows: the custom name, or the item's. |
| `slot.lore` | The lore's lines, plain text: the first ones, while a value holds them. menu.lore_has and lore_number read them all. |
| `slot.durability` | The uses left; () for an item that doesn't wear. |
| `slot.potion` | A potion's, a splash or lingering potion's or a tipped arrow's kind, such as "minecraft:turtle_master"; (). |
| `slot.enchantments` | A map of each enchantment's ID to its level. |
| `slot.stored_enchantments` | An enchanted book's, the same way. |
<!-- end of generated table -->

- **Names and lore are what the server sent, cleaned like chat:** plain text, no formatting, one line each, a name up to 64 characters and lore up to 16 lines of 128. A slot's map keeps its lore lines while they fit in one of Rhai's values (1,024 bytes of text in all); `menu.lore_has(slot, text)` and `menu.lore_number(slot, label)` read every line.
- **A slot's components are registry IDs and numbers, never text.** A vanilla potion's kind is in no name and no lore: `slot.potion` is how a script tells a Turtle Master from a water bottle.
- **Numbers as servers write them:** `menu.number(text)` reads the first number in a text, `"Price: 12,500"`, `"12.5k"`, `"3M"` and `"2B"` alike, or gives `()`. A digit glued to a letter belongs to a word (`AfkBot1` is no number), and a first number that can't be read reads as nothing: a later one never stands in for it. `menu.lore_number(slot, label)` reads the first number after `label` on the first lore line that starts with it. Both take `","` as a last value for servers that write `1.234,56`.
- **`bot.name`** is the bot's own player name, for a script several bots share: a listing whose seller line holds it is the bot's own (`menu.lore_has(slot, "Seller: " + bot.name)`).

### `inventory`
<!-- generated table: inventory -->
| Member | What it is or does |
|---|---|
| `inventory.slots` | Every slot's number: the armour 5 to 8, the main inventory 9 to 35, the hotbar 36 to 44, the offhand 45. |
| `inventory.slot(n)` | Slot n, or (). |
| `inventory.lore_has(slot, text)` | Whether a lore line of the bot's slot holds text, as on menu. |
| `inventory.lore_number(slot, label)` | The first number after label in the bot's slot's lore, as on menu. |
| `inventory.find(id)` | The first slot that holds the item id, or (). |
| `inventory.count(id)` | How many of the item id the bot carries. |
| `inventory.hotbar(n)` | The hotbar slot of number key n, 1 to 9, or (). |
| `inventory.open()` | E: opens the inventory screen; only with no screen open. Nothing goes to the server. |
<!-- end of generated table -->

### `menu`
<!-- generated table: menu -->
| Member | What it is or does |
|---|---|
| `menu.open` | Whether a screen is open. |
| `menu.screen` | Which screen it is: a number new at every opening, or () with none. |
| `menu.kind` | Its kind: generic_9x3, anvil, merchant, inventory, …; () with none. |
| `menu.title` | Its title, as plain text. |
| `menu.rows` | A grid's rows; () for a screen that isn't one. |
| `menu.columns` | A grid's columns; () for a screen that isn't one. |
| `menu.slots` | The numbers of the screen's own slots, from 0: for n in menu.slots { let slot = menu.slot(n); … }. |
| `menu.player` | The numbers of the bot's 36 slots in this screen: four rows of nine, the hotbar last. |
| `menu.slot(n)` | Slot n of the screen, its own or the bot's, or (). |
| `menu.at(row, column)` | A grid's slot by its row and column, from 0 at the top left, or (). |
| `menu.find(id)` | The first of the screen's own slots that holds the item id, or (). |
| `menu.cursor` | The item on the cursor, or (). |
| `menu.trades` | How many trades a merchant offers; 0 for any other screen. |
| `menu.trade(n)` | Trade n, from 0: a map of cost_id, cost_count, second_id, second_count, result_id, result_count, result_name, result_potion, result_enchantments, result_stored_enchantments, out_of_stock; or (). |
| `menu.pending` | Whether an action of the script's still waits for its fate. |
| `menu.lore_has(slot, text)` | Whether a lore line of the slot (its number, or a slot read on this screen) holds text. An empty text is in none. |
| `menu.lore_number(slot, label)` | The first number after label on the slot's first lore line that starts with it, or (); lore_number(slot, label, ",") for a comma as the decimal mark. |
| `menu.number(text)` | The first number in text as servers write amounts (1,250.5, 2k, 1.5M, 3B), or (); number(text, ",") for a comma as the decimal mark. |
| `menu.click(slot)` | A left click on a slot: its number, or a slot a hook read. Its fate comes in on_click. |
| `menu.right_click(slot)` | A right click on a slot. |
| `menu.shift_click(slot)` | Shift and a left click: moves the stack to the other part of the screen. |
| `menu.swap(slot, n)` | Number key n over a slot: swaps it with hotbar slot n, 1 to 9. |
| `menu.drop(slot)` | Q over a slot: drops one item. |
| `menu.drop_stack(slot)` | Ctrl+Q over a slot: drops the stack. |
| `menu.button(n)` | A screen's own button: an enchanting table's 0 to 2, a stonecutter's or a loom's. |
| `menu.select_trade(n)` | Selects a merchant's trade n, from 0. |
| `menu.rename(key, values)` | Sets the anvil's name to a declared message: rename(key) without values. |
| `menu.close()` | Esc: closes the screen. Never limited, never refused while one is open. |
<!-- end of generated table -->

- **Slots are the screen's own numbers:** a chest's own first, from 0, then the bot's 36, the hotbar last. A grid has rows and columns from 0 at the top left: the chests, the 3-by-3 screens, the shulker box and the hopper; `menu.at(menu.rows - 1, 4)` is the middle of the bottom row. Any other screen has neither.
- **A screen opens** when the script sends a declared command, right-clicks a block or an entity, or calls `inventory.open()`, and the server answers with one. **A new screen is a new screen,** whatever its title: `menu.screen` is new at every opening.
- **While a screen is open, nothing else of the player's moves:** keys, buttons, looks, the hotbar and vehicles answer `screen_open`, as a vanilla player's hands are on the screen. A release is never refused.
- **A screen that opens lets go of every key and button,** as in a vanilla client, and the script hears it as a let-go: `held` is `false` again. A script that defines `on_menu_open` handles the screen itself, and holds again what it wants once the screen has closed (`on_menu_close`). **A script that doesn't define it, at level 1 or 2, has a server's screen closed at once and what it held pressed again,** past the same limits as any press, so a farm that holds a button from `on_join` farms on. A press the limits refuse then stays up, and the script's log says so. The bot's own inventory is only opened by a script, and never closed for it.
- **A merchant's trades** are read one by one: `menu.trades` is how many, `menu.trade(n)` one, as a flat map.

### Hooks
| Hook | When it runs |
|---|---|
| `fn on_menu_open()` | A screen opened: a server's, or the inventory the script opened. Everything held was let go |
| `fn on_menu_update()` | The open screen's content changed: at most once a game tick (50 ms), however often it changes |
| `fn on_menu_close()` | The screen closed, by the script or by the server |
| `fn on_click(result)` | What became of a click, a screen's button, a trade or a name the script sent: `result.ok`, `result.reason` and `result.slot` |
| `fn on_sign_open()` | The server opened a sign editor |

### A click never lands on the wrong thing
A hook sees the screen as it was when the hook started. Between that moment and the click, the server may have changed it. So every click is checked twice: in the hook, against what the hook sees, and right before it's sent, against the screen as it is then:

- a screen is open (`no_menu`), and it's the screen the hook saw (`screen_changed`);
- its content hasn't changed since the hook saw it: its state, its cursor, a trade at its place (`content_changed`);
- every slot the click touches still holds the whole item the hook saw: its ID, count, name, lore and every component (`slot_changed`).

**A slot read in an earlier hook** names what the script decided on: `menu.click(slot)` goes out only if this hook sees the same item on the same screen there, and answers `slot_changed` or `screen_changed` otherwise. A slot's number alone clicks whatever this hook sees there.

**One action at a time.** A click, a screen's button, a trade or an anvil's name waits for its fate before the next may go (`click_pending`), and the screen takes at most one a game tick (`too_soon`, see Limits). Closing is never limited and never waits.

### What becomes of a click
Every action a hook was given `ok` for gets exactly one `on_click(result)`: the reason it wasn't sent after all, or its fate. `result.ok` is `true` only for a fate of `ok`.

| `result.reason` | What came |
|---|---|
| (none) | Nothing: the server did what a vanilla client predicts |
| `rejected` | The server corrected the screen: it put things back, or did otherwise. **A plugin's buttons answer so though they acted:** a plugin cancels the click on its own button and puts the item back, then does its work |
| `unclear` | The server corrected the screen after a click on a result slot (a trade's, an anvil's, a craft's) or after a screen's button, a trade or a name: it may have done what was asked, once or more. A shift-click on a villager's result trades as long as the payment lasts, and an anvil asks the levels the server decides |
| `replaced` | The screen closed, or another opened, before the answer: the server may have done it |
| `unanswered` | No answer came within 5 seconds: the server may have done it. The screen takes no click until it's opened again; close it and open it anew |
| `screen_changed`, `slot_changed`, … | It wasn't sent after all: the screen changed between the hook and the moment of sending |

**A fate never decides a repeat. Judge a step by the screen that follows it:** the confirm screen that opens, the listing that's gone, the items in the inventory. A script that clicks again because a click came back `rejected`, `unclear`, `replaced` or `unanswered` can buy twice. Every example below reads the next screen or the inventory instead.

**Every click that wasn't sent, came back other than `ok` or got no answer is a line in the script's log,** written by the runtime, and so is every click a hook was refused: the click limit bounds them, so none is ever dropped. The app labels them "Click": what was clicked, what came of it in words, and the name `on_click` reads, as in "a click on slot 13 may have worked: the script checks the next screen (unclear)".

**What a server can notice.** After every click the bot asks the server for its statistics, as a vanilla client's statistics screen does: the server's answer to that request is how the runtime knows the click was handled. A vanilla client never asks for statistics while a container is open, so a server's anti-cheat could notice it. Whether that matters is yours to weigh per server; try a new server with one bot first.

### Signs
<!-- generated table: sign -->
| Member | What it is or does |
|---|---|
| `sign.open` | Whether a sign editor waits for its answer. |
| `sign.x` | The sign's block: x. |
| `sign.y` | The sign's block: y. |
| `sign.z` | The sign's block: z. |
| `sign.side` | "front" or "back". |
| `sign.write(key, values)` | Answers with a declared sign: write(key) without values. Once an editor. |
| `sign.cancel()` | Answers with nothing written. Once an editor. |
<!-- end of generated table -->

A script answers a sign editor the server opens, with a sign the mode declares and nothing else: up to four lines of fixed text and typed values, as declared messages have, a line at most 80 characters as Paper takes them. A line may start with a value (the price alone on its line) and may be empty. `sign.cancel()` writes four empty lines. **Only the editor the server opened last is answered, and only once:** an editor the script doesn't answer is never answered, and a new screen, a death or a world change ends it. A mode declares its signs in the mode editor's Signs section, beside its messages, each with a key, its lines and its values.

### The time of day
<!-- generated table: clock -->
| Member | What it is or does |
|---|---|
| `clock.local(zone)` | The time in an IANA time zone such as "Europe/Berlin", summer time included: a time map. |
<!-- end of generated table -->

<!-- generated table: time -->
| Member | What it is or does |
|---|---|
| `time.year` | The year. |
| `time.month` | 1 to 12. |
| `time.day` | The day of the month, 1 to 31. |
| `time.hour` | 0 to 23. |
| `time.minute` | 0 to 59. |
| `time.second` | 0 to 59. |
| `time.weekday` | 1 for Monday to 7 for Sunday. |
| `time.offset` | Minutes ahead of UTC: 60 in Berlin's winter, 120 in its summer. |
<!-- end of generated table -->

The time is the hook's own moment, the same one its timers count from. Summer time comes from IANA's time-zone database, compiled into the agent: a zone is a name such as `"Europe/Berlin"`, never a fixed offset, so the clock follows the switch on the last Sunday of March and of October by itself. On the spring day 02:00 to 02:59 never shows in Berlin, and on the autumn day it shows twice, the second time with `offset` 60. **A name that is no zone stops the script with its line,** as a message the mode doesn't declare does.

```rhai
// Only between 14:00 and 19:00 German time.
fn in_window() {
    let now = clock.local("Europe/Berlin");
    now.hour >= 14 && now.hour < 19
}

fn on_timer(name) {
    if in_window() {
        chat.send("balance");
    }
}
```

### Examples
These run against the test server's copy of an auction plugin (the harness), whose screens are named as they are here.

#### A sell macro
The mode declares `sell_menu`: `/sell`.

```rhai
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
```

#### An auction scan
The mode declares `auction`: `/ah`.

```rhai
// Every 30 seconds: opens the auction house and buys the cheapest diamond
// listed for 1,500 or less, on its Confirm Purchase screen. A purchase is
// judged by what follows it, never by a click's fate: the plugin answers
// every button "rejected" though it acted, so the diamonds in the inventory
// say what was bought. Each Confirm Purchase screen is confirmed at most
// once, and only for the listing picked, so no listing is bought twice.
fn on_join(first) {
    if first {
        state.bought = 0;
    }
    state.wanted = ();
    state.confirmed = false;
    state.before = 0;
    timer.every(30000, "scan");
}

fn on_timer(name) {
    if name == "scan" {
        if !menu.open {
            chat.send("auction");
        }
    } else if name == "pick" {
        pick();
    } else if name == "confirm" {
        confirm();
    }
}

fn on_menu_open() {
    if menu.title == "Auction House" {
        later("pick");
    } else if menu.title == "Confirm Purchase" {
        state.confirmed = false;
        later("confirm");
    } else {
        menu.close();
    }
}

fn on_menu_close() {
    // The purchase's screen closed after its confirm: what the inventory
    // holds now says whether it bought.
    if state.wanted != () && state.confirmed {
        let now = inventory.count("minecraft:diamond");
        if now > state.before {
            state.bought += 1;
            log.info(`Bought ${now - state.before} diamonds for ${state.wanted.price}.`);
        } else {
            log.info("The purchase didn't go through.");
        }
        state.wanted = ();
        state.confirmed = false;
    }
}

fn later(name) {
    timer.after(random.between(300, 900), name);
}

// On the auction house: the cheapest diamond within the budget.
fn pick() {
    if !menu.open || menu.title != "Auction House" {
        return;
    }
    let best = ();
    let price = 0.0;
    for n in menu.slots {
        let slot = menu.slot(n);
        if slot.id != "minecraft:diamond" || slot.row > 4 {
            continue;
        }
        let asked = menu.lore_number(slot, "Price");
        if asked == () || asked > 1500 {
            continue;
        }
        if best == () || asked < price {
            best = slot;
            price = asked;
        }
    }
    if best == () {
        menu.close();
        return;
    }
    state.wanted = #{ price: price, seller: seller_of(best) };
    if !menu.click(best).ok {
        state.wanted = ();
        menu.close();
    }
}

// On Confirm Purchase: confirms only the listing picked, once.
fn confirm() {
    if !menu.open || menu.title != "Confirm Purchase" || state.wanted == () || state.confirmed {
        return;
    }
    let button = menu.slot(11);
    let same = menu.slot(13).id == "minecraft:diamond"
        && menu.lore_number(13, "Price") == state.wanted.price
        && menu.lore_has(13, state.wanted.seller);
    if !same || button.id != "minecraft:lime_stained_glass_pane" || button.name != "Confirm" {
        log.info("Not the listing picked; nothing bought.");
        state.wanted = ();
        menu.close();
        return;
    }
    state.before = inventory.count("minecraft:diamond");
    state.confirmed = menu.click(button).ok;
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
```

#### A farm that sells
The mode declares `sell_menu`: `/sell`. The junk it drops is one item ID the script names; it never drops anything else.

```rhai
// A farm that sells: it farms by holding the right button with its farm's
// item in hotbar slot 1. Every 5 minutes it opens the sell menu, sells its
// Turtle Master potions and drops its one kind of junk, never anything else,
// so it never drops a tool. It waits 300 to 900 ms before every click, then
// selects the farm's item again and farms on: a right button held with a
// potion in hand would drink it.
fn junk() {
    "minecraft:rotten_flesh"
}

fn on_join(first) {
    farm();
    timer.every(300000, "sell");
}

fn farm() {
    hotbar.select(1);
    mouse.right.hold();
}

fn on_timer(name) {
    if name == "sell" {
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
```

#### An auction lister
The mode declares `auction`: `/ah`, `sell`: `/ah sell {price}` with `price` a whole number from 100 to 1,000,000, and `expired`: `/ah expired`.

```rhai
// An auction lister: 10 to 30 seconds after it joins, then every 10
// minutes, with a Turtle Master potion in its hotbar, it reads the potions'
// prices on the auction house's first page, skips its own listings, and
// lists one potion at the middle of the three cheapest with
// "/ah sell {price}", confirming it on its screen once. Then it collects its
// expired listings.
//
// The declared price's range starts at 100: that's the floor a bait listing
// can't push the price below. A price under it is refused (bad_value), and
// nothing is sent.
fn on_join(first) {
    state.task = "";
    state.done = ();
    // Soon after every join: a bot that's kicked more often than every 10
    // minutes still lists.
    timer.after(random.between(10000, 30000), "list");
}

fn on_timer(name) {
    if name == "list" {
        timer.after(600000, "list");
        if state.task == "" && !menu.open && potion_key() != () {
            state.task = "prices";
            chat.send("auction");
        }
    } else if name == "step" {
        step();
    }
}

fn on_menu_open() {
    later();
}

fn on_click(result) {
    later();
}

fn on_menu_update() {
    later();
}

fn on_menu_close() {
    if state.task == "listing" {
        // The confirm's screen closed: on to the expired listings.
        state.task = "expired";
        chat.send("expired");
    } else if state.task == "expired" {
        state.task = "";
    }
}

fn later() {
    timer.after(random.between(300, 900), "step");
}

fn step() {
    if !menu.open || state.done == menu.screen {
        return;
    }
    let title = menu.title;
    if title == "Auction House" && state.task == "prices" {
        list();
    } else if title == "Confirm Listing" && state.task == "listing" {
        let button = menu.slot(11);
        if button.id == "minecraft:lime_stained_glass_pane" && button.name == "Confirm"
            && turtle_master(menu.slot(13))
        {
            // Once a screen: the plugin lists and closes it.
            state.done = menu.screen;
            menu.click(button);
        } else {
            menu.close();
        }
    } else if title == "Expired Listings" && state.task == "expired" {
        for n in menu.slots {
            let slot = menu.slot(n);
            if slot.row < 5 && !slot.empty {
                menu.click(slot);
                return;
            }
        }
        menu.close();
    } else {
        log.info(`Closed a screen it didn't expect: ${title}`);
        state.task = "";
        menu.close();
    }
}

// Reads the price, closes the auction house and lists one potion.
fn list() {
    let price = middle_price();
    let key = potion_key();
    menu.close();
    if price == () || key == () {
        log.info("Fewer than three potions listed by others; nothing listed.");
        state.task = "";
        return;
    }
    hotbar.select(key);
    state.task = "listing";
    if !chat.send("sell", #{ price: price }).ok {
        log.info(`Not listed at ${price}: below the floor.`);
        state.task = "";
    }
}

// The middle of the three cheapest Turtle Master potions others list.
fn middle_price() {
    let prices = [];
    for n in menu.slots {
        let slot = menu.slot(n);
        if slot.row < 5 && turtle_master(slot) && !menu.lore_has(slot, "Seller: " + bot.name) {
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

// The number key of a hotbar slot that holds a Turtle Master potion.
fn potion_key() {
    for key in 1..10 {
        if turtle_master(inventory.hotbar(key)) {
            return key;
        }
    }
    ()
}

fn turtle_master(slot) {
    slot.potion == "minecraft:turtle_master"
        || slot.potion == "minecraft:long_turtle_master"
        || slot.potion == "minecraft:strong_turtle_master"
}
```

#### An auction lister with the price in an anvil
For a server whose auction house takes a listing's price in an anvil it opens: the item to list is clicked in the inventory, and the price is typed into the anvil's name box and confirmed with the item in its result slot. The anvil may show an enchantment cost that needs no levels: it's the plugin's display. The mode declares `auction`: `/ah`, and `price`: `{price}` alone, a whole number from 100 to 1,000,000.

```rhai
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
```
