# 0052. Menus in scripts and the runtime, the time of day, and a fate that never misleads

- **Status:** Accepted. Decided under ADR-0025, inside what ADR-0036 (M1 to M18, R1, R2), ADR-0050 and ADR-0051 decided and the user asked for on 2026-11-26
- **Date:** 2026-11-26
- **Related:** Plan.md Phase 16, group D (P16.12 to P16.14, P16.21); [ADR-0036](0036-scripted-modes.md) (its M17 amended for the user's correction of 2026-11-26); [ADR-0038](0038-chrono-is-unmaintained.md) (no `chrono-tz`); [ADR-0040](0040-fleet-script-as-built.md) (the sandbox's limits); [ADR-0043](0043-the-script-runner-and-hooks-on-the-async-workers.md) (the runner); [ADR-0049](0049-the-paper-harness.md) (the harness); [ADR-0050](0050-menus-in-fleet-core.md), [ADR-0051](0051-menus-in-fleet-mc.md) ("What group D inherits"); `crates/fleet-script/src/api/menu.rs`, `api/clock.rs`, `clock.rs`, `turn/menu.rs`; `crates/fleet-runtime/src/script/runner.rs`, `feed.rs`; `crates/fleet-fuzz/src/menu.rs`

## Context
Groups B and C built the menu model in fleet-core and fed it from azalea in fleet-mc. Group D shows it to a script and runs it: the objects, the hooks, the runner's part, the examples, the fuzz targets. The user added four things to be sure of and a task (P16.21, the time of day in a named zone), and at the group's start corrected one fact: on their server a listing's price is typed into an anvil the plugin opens, titled "Preis eingeben", not written on a sign. This records what group D decided where ADR-0036 and its amendments left it open, and what the live runs showed.

## Decisions

### G1. API level 2, and its names at level 1
- **`ScriptApiLevel::V2` is the newest.** A script uses level 2 by naming `inventory`, `menu`, `sign` or `clock`, defining one of the five menu hooks, reading `bot.name`, or declaring signs.
- **A script is compiled against its own level's objects and hooks.** At level 1 the new names are the script's own, as they were before Phase 16: no stored script breaks, and a level-1 script that names the object `menu` or defines `on_menu_open` is refused at save with `NeedsLevel { name, level: 2 }` and its place. `bot.name` (a member of a level-1 object) stops a level-1 script with `needs level 2`.
- **Until the server stamps the level a script needs (P16.15), a mode is saved at level 1:** fleet-api-types' `ModeScriptRequest::script` stamps `V1` (it stamped `NEWEST`), and so does "Convert to script". The editor's data (`ScriptApi`, the app's autocomplete and starter templates) carries level 1's objects, members, reasons and examples only, so the app offers nothing a save would refuse. An agent says it runs level 2 (`Hello.script_api`), and a standalone agent's own modes run at the newest level.

### G2. What a script reads: maps, lists of numbers, and lore that fits
- **A slot is a map of plain data:** its number, its row and column, the screen's opening it was read on, `empty`, and its item's ID, count, name, lore, durability, potion and both kinds of enchantments. A trade is one flat map. A script can keep either in its state.
- **A list is of slot numbers, never of maps** (`for n in menu.slots { let slot = menu.slot(n); … }`), and **a trade is read by its number** (`menu.trades`, `menu.trade(n)`). Rhai counts a value's map entries and its text over everything nested in it, against the sandbox's 64 entries and 1,024 bytes a value (ADR-0036, D3): an array of 27 slot maps is over both. Measured first, in the suite: the planned `menu.slots` of maps stopped a script reading an ordinary chest.
- **A slot's map keeps its lore lines while they fit in one value;** `menu.lore_has(slot, text)` and `menu.lore_number(slot, label)` (and the same on `inventory`) read every line from what the hook sees. A server sends up to 16 lines of 128 characters, and a seller's line further down must never be missed. A slot is at most 45 map entries and 1,024 bytes; a trade at most 42 entries. The `menu_snapshot` fuzz target holds that a script reads anything a server sends to its end.
- **The number helper is `menu.number(text)`,** not the global `number(text)` ADR-0036's M6 named: every game function hangs on an object (D4), the level comes with the object, and a Phase 15 script may define its own `number`. Both helpers take `","` as a last value for servers that write `1.234,56` (B8 left the mark to group D).

### G3. A click: checked in the hook and at sending, one at a time
- **In the hook,** against the screen the hook sees: a value first (`bad_value`: a slot the screen doesn't have, a button or a trade it doesn't offer), then the bot can act, a screen is open (`no_menu`), no earlier action waits for its fate (`click_pending`), and the click limit (`too_soon`, `Limited::Click`, one a game tick). At the moment of sending, the session's gate and guard decide again (ADR-0050).
- **A slot read in an earlier hook names what the script decided on:** `menu.click(slot)` goes out only if this hook sees the same item, as a script reads it, on the same screen there; `slot_changed` or `screen_changed` otherwise. That covers the auction case where a timer's click comes 300 to 900 ms after the hook that chose the listing. A slot's number alone clicks what this hook sees.
- **A close and opening the inventory are never limited,** as a release isn't; a close is never refused while a screen is open.
- **While a screen is open, keys, buttons, looks, the hotbar and vehicles answer `screen_open`** (the user's point 4; M8). A release never is.

### G4. What becomes of an action: one `on_click` each, and `unclear`
- **Every action a hook was given `ok` for gets exactly one `on_click(result)`:** its fate, or the reason the session didn't send it after all (the runner queues that outcome and delivers it before anything else). A send the session didn't answer in time waits for its fate up to 10 s (`FATE_PATIENCE`), then is `unanswered`; a fate that comes after that is dropped.
- **A new fate, `unclear`:** a correction after a click on a screen's result slot (`ScreenKind::result_slot`: the inventory's and the crafting table's crafts, the anvil, grindstone, merchant, cartography table, smithing table, loom and stonecutter; not a furnace's output, which consumes nothing), or after a screen's button, a trade or a name, which predict no slot at all.
- **Why not the preferred way, a prediction put right so the click comes back `ok`:** a shift-click on a villager's result repeats the trade while the payment lasts, an anvil asks for the levels the server computes, and a plugin's anvil puts its own item there. A prediction exact for all of it would be a second copy of the server's trade rules in fleet-mc that no test could hold complete. So the user's fallback holds: **a click that may have gone through never reads `rejected`.**
- **Measured live** (`screens::slow_screens_on_vanilla`, `slow_screens_on_paper`):
  - On vanilla, a villager's result shift-clicked: `unclear`, the bread taken and the emerald paid. An anvil's result: `unclear`, the sword named.
  - On Paper, the user's price anvil: the name `unclear` (Paper answers it with the anvil's picture, though the plugin then reads the text), a confirm the plugin refused `unclear` with the anvil still open, the price's name `unclear`, and the confirm that worked `unclear`.
  - A plugin's button on a chest slot stays `rejected` (it predicts its slot), though the plugin acts, as #138 saw for Create.
- **So the reference's central rule:** a fate never decides a repeat; a step is judged by the screen that follows it, or the inventory. Every example does that, and the auction scan confirms each Confirm Purchase screen once.

### G5. A screen opening: a let-go for every script, and level 1 pressed again
- **Every script hears a screen opening as a let-go** (the user's point 1): what it held is let go in the instance too, so `held` tells the truth.
- **A script that defines `on_menu_open` handles the screen.** **One that doesn't, at level 1 or 2, has a server's screen closed at once and what it held pressed again** past the same limiter, leash and guards as any press, a `hold_for` for what was left of it, a hold until used as one. The bot's own inventory is never closed for it.
- **Why pressed again rather than only told:** a level-1 script has no hook that hears of a screen, so a farm that holds its button from `on_join` would stand idle, believing correctly that it holds nothing. A person closes the screen and holds the button again; the runtime does the same. A press the limiter refuses (the use button within 500 ms of its last press) stays up, and the script's log says so.

### G6. The runner (P16.13)
- **The menu events join `Life`,** bounded as fleet-mc's bridge bounds them (ADR-0051, C2): a screen that opens drops what's unread of the screens before it but the close of one whose opening was read; a screen that closes before its opening was read is never heard of; an update waits once; a sign editor replaces one unread; a fate is always kept. At most an opening, an update, a close, a fate and a sign editor wait at once.
- **`on_menu_update` runs at most once a game tick:** an update within 50 ms of the last waits for the tick's end, once.
- **Every miss is a line in the script's log** (the user's point 4): a click not sent (with the session's reason), a fate other than `ok`, and every refusal of a click, a screen command or a sign in a hook. They're written with the runtime's own lines, never dropped: the click limit bounds them. Until the app's trade lines get a kind of their own (P16.16) they go as `refused` lines.
- **`bot.name`** comes from the credentials the bot connected with, never from the server.

### G7. Declared messages and signs
- **M15, as the user added it:** a decimal for a whole number counts down to the next one, and a decimal value with places counts down at its last place, never to the nearest. **A decimal's binary noise doesn't count:** the number is first rounded six places past its last (`1234.56 - 1000.0` is 234.55999999999995 and sends 234.56, not 234.55). The range is judged on what's sent.
- **A message may start with a number's value** (`{price}` alone, for the anvil's name box). The rule it loosens exists so a value can't make a message a command; a number never starts with `/`. A word's value first stays refused.
- **Declared signs join `Script`** (`signs`, at most 16, each a `SignTemplate`, only at level 2). `ScriptDraft.signs` is left out of JSON when empty, so a stored mode without signs reads and writes as before. The wire and the API take them in P16.15.

### G8. The time of day (P16.21)
- **`clock.local(zone)`** answers a map of `year`, `month`, `day`, `hour`, `minute`, `second`, `weekday` (1 Monday to 7 Sunday, ISO 8601) and `offset` (minutes ahead of UTC), from the hook's own moment.
- **jiff 1.0.1, its bundled data only** (`default-features = false`, `std`, `tzdb-bundle-always`): no `tz-system`, `tzdb-zoneinfo` or `tzdb-concatenated`, so nothing is read from the system and an agent's container needs no zone files. `jiff-core` is held at 1.0.1 (1.0.2 was two days old). **Why jiff:** ADR-0038 rules out `chrono-tz`, which chrono's maintainer is winding down with chrono itself and who recommends jiff. chrono stays the workspace's time type; jiff converts one timestamp, in fleet-script alone.
- **An unknown zone stops the script with its line,** as a message the mode doesn't declare does: a zone is written into the program, and no value at run time excuses a typo. `bad_value` would hand a script `()`, whose `hour` then fails with a less clear message.
- **Tests:** both of Berlin's switch days with the hour skipped and the one shown twice, midnight and the weekday's change at Berlin's midnight, other zones, names that are no zone; and properties against the data: Berlin follows the EU rule written out apart (1997 to 2037) with its clock UTC plus its offset, and every zone in the build reads every moment from 1900 to 2100.

### G9. Fuzz targets and the budget (P16.14)
- **`menu_snapshot`:** a screen, its trades and an inventory from any bytes, including text of thousands of characters, 24 lore lines and 24 enchantments of any level. What's kept stays in its bounds, and a level-2 script reads every slot, trade and field to its end.
- **`number_helpers`:** the first number is finite or none and the same each time, and a lore's number is its label's line's.
- **`click_guard`:** any sequence of screens, corrections, a hook's view, clicks, replies and waits. A click goes out only onto the screen, state, cursor and items the hook saw, never while another waits, and every click gets exactly one fate.
- **The fuzz runner turns AddressSanitizer's fake stack frames off:** `menu_snapshot`'s first run on CI's Linux runner showed that in them fleet-script's stack guard ends every hook at its first operation ([ADR-0030](0030-fuzz-targets.md)'s amendment of 2026-11-26).
- **The operation budget on real pages:** a full page of 45 listings costs the auction scan 1,939 operations, the lister 2,538 and the anvil lister 2,765, of a hook's 10,000. ADR-0036 measured 1,437 for a scan on a smaller synthetic page; the budgets stand.

### G10. The harness's price step is an anvil (the user's correction, 2026-11-26)
- **"Preis eingeben"** opens on the tick after Confirm Item:
  - paper named "Preis" in its left input, the text box's starting text;
  - a lime dye named "Bestätigen" in its result slot;
  - an enchantment cost of 1 shown, which needs no levels.
  
  A click on the result reads the anvil's rename text: a whole number from 1 to 1,000,000,000 opens Confirm Listing, anything else is refused and the anvil stays open. Nothing of the anvil's goes back into the player's inventory.
- **Live with a bot at level 0** (`xp query`), as a fresh account is: the user's observation holds for the bot.
- **The sign editor, `harness sign` and their tests stay** (other servers take a price on a sign); the flow no longer reaches a sign.
- **The second lister example** names the price with `menu.rename("price", #{ price })`, and clicks the result only while it holds the lime dye named "Bestätigen". A price the plugin refuses leaves the anvil open: the example sees that three seconds later and stops, logging why.

### G11. The Caddy smoke test waits for the site's certificate
#138's flaky first login was Caddy answering a handshake before it had its site's certificate. Reproduced on purpose with the pinned image: a handshake right after the root appears fails with `tlsv1 alert internal error`. Caddy writes its root about 80 ms before it obtains the site's certificate. **`caddy_root` now waits for Caddy's own "certificate obtained successfully" line for the site,** waking at each of Caddy's lines, on both proxy paths.
- **Why the line rather than retrying the refusal:** the line is the event that ends the race, so the wait is exact and the smoke tests' form (every wait wakes on an event). A retry of TLS failures would also hide a real certificate fault until its bound ran out.

## What group E inherits
- **The wire's values for the five hooks** (fleet-proto's `ScriptHook` maps them to "no hook" until then) and a log kind of its own for the click lines (P16.16's trade lines).
- **The server stamps the level a script needs:** compile at level 1, and at level 2 when that refuses with `NeedsLevel`. Then `ScriptApi`'s `OFFERED` becomes level 2, and the app learns the description's maps (`slot`, `time`) as values, not game objects (its `GAME_OBJECTS` filters only `input` and `hooks` today).
- **Declared signs on the wire and in the API,** and the five starter templates (`EXAMPLES`' level-2 ones).

## Consequences
- **A script can buy and sell.** The guard holds a click to what the script saw. That doesn't make a server's market safe: the reference tells authors to judge a step by what follows, and what a server allows stays the user's to check.
- **One new dependency,** jiff 1.0.1 with jiff-core and jiff-tzdb, in fleet-script; chrono stays (ADR-0038, C3). A tzdb update comes with a jiff bump.
- **A sixth fate.** `ClickFate::Unclear` joins fleet-core; `Refusal` grows to 23 reasons, the script's one closed list.
- **What a server can notice:** after every click the bot asks for its statistics, which a vanilla client never does with a container open (ADR-0051's consequences). The reference says so, and that a new server is best tried with one bot first.

## Alternatives considered
- **An array of slot maps** (`for slot in menu.slots`). Rejected: over Rhai's per-value limits for any real chest (G2). Raising the sandbox's limits instead was the other way, and weakens what bounds a script's memory.
- **Lore helpers on the slot's map** (`slot.lore_has(…)`, as ADR-0036's M12 wrote it). Rejected: a map can't hold all of a server's lore, and a helper on the map would read only what fit.
- **Only telling a level-1 script of the screen.** Rejected for G5's reason; the user's farm would stop.
- **`rejected` kept for vanilla's result slots,** with the reference warning. Rejected: the user's rule is that a click that may have gone through never reads so.
- **`chrono-tz`** for the zones. Rejected by ADR-0038's rule. **A zone's rule written by hand** (the EU's) would cover Berlin alone and silently go wrong for any other zone or a future rule change.
- **An unknown zone answering `bad_value`.** See G8.
