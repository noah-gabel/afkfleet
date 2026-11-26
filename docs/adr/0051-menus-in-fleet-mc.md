# 0051. Menus in fleet-mc: each window's state, azalea's gaps put right, the guard at sending, the barrier, and the user's auction flow

- **Status:** Accepted. Decided under ADR-0025, inside what ADR-0036 (M1 to M18, R1, R2) and ADR-0050 decided
- **Date:** 2026-11-26
- **Related:** Plan.md Phase 16, group C (P16.8 to P16.11); [ADR-0036](0036-scripted-modes.md); [ADR-0050](0050-menus-in-fleet-core.md) ("What the next groups inherit"); [ADR-0049](0049-the-paper-harness.md) (the harness); [ADR-0042](0042-scripted-inputs-in-fleet-mc.md) (the inputs); [ADR-0026](0026-a-bot-in-a-minecart.md) (riding); `crates/fleet-core/src/mc/menu.rs`, `crates/fleet-mc/src/menu/`, `crates/fleet-mc/tests/minecraft/screens.rs`, `harness/paper/`

## Context
Group B built the menu model in fleet-core: `ClickGate`, the guard, `SignDesk`. Group C feeds it from azalea 0.16.0 on the session's host thread, sends what it allows, and gives the runtime a way in. Group A had measured the protocol (ADR-0036's amendment); azalea's own code was read at its 0.16.0 release's commit on GitHub, never from the local registry. What it showed decided most of what follows:
- **azalea keeps one state ID** for every window, set only from a slot packet of the open window, never from a whole content.
- **azalea drops the cursor:** `SetCursorItem` is only logged, and a whole content's cursor is ignored. `SetPlayerInventory` does nothing.
- **azalea's click prediction has gaps.** It reads a number key as a slot of the screen, not the hotbar. A right click on a stack leaves the whole stack. A stack a drop empties stays at count 0. It knows no slot that refuses an item.
- **Three components hold unordered maps:** the enchantments, the stored enchantments, and a block state's properties.

## Decisions

### C1. The port: an observation's `MenuView`, five events, one call
- **`Observation.menu: MenuView`:** the bot's `BotInventory`, the open `Screen`, and the `SignEditor` still waiting for its answer. It's read with the snapshot, on the host thread.
- **`SessionEvent`** gets `MenuOpened`, `MenuUpdated` and `MenuClosed` (each with its `ScreenId`), `Clicked(ClickResult)` and `SignOpened(SignEditor)`. The bot's own inventory opening and its own close are events too, so whoever reads them sees every screen open and close once.
- **`SessionHandle::menu(MenuCommand) -> Result<(), MenuError>`:** one call for `Act(MenuAction)`, `Close(ScreenId)`, `OpenInventory` and `AnswerSign { editor, answer }`. `MenuError` is `Session(SessionError)`, `Refused(MenuRefusal)` or `Sign(SignRefusal)`, so a script's reason stays apart from the session's. One call, not four, because each is the same kind of thing: a decision made on the host thread against what is open at that moment.
- **The runtime's actor reads the events and logs them at `debug`;** group D gives them to a script.

### C2. The events stay bounded, whatever a server sends
- A screen that opens drops what's unread of the screens before it, all but the close of the one whose opening was read.
- An update is pending once a screen.
- A screen that closes before its opening was read takes the opening and its update along, and reports nothing.
- A sign editor replaces one unread.
- **A fate is always delivered, and no action goes out while one is unread** (`ClickPending`). So at most a close, an opening, an update, a fate and a sign editor wait at once. A test floods the bridge with 10,000 screens and editors and finds at most three events.

### C3. Each window's own state ID (P16.8)
The state a click carries is the one the server last gave **its window**, taken from the packets a vanilla client applies to that window:
- window 0: a whole content always; a slot packet of a hotbar slot always; any other slot packet only while no server screen is open, as a vanilla client applies them;
- the open screen: its whole content and its slot packets.

A packet that isn't applied gives no state. A state never runs ahead of the content it describes, which the guard relies on. Live, a chest's click went out with the chest's state 2 while window 0 was at 7 (`slow_screens_on_vanilla`).

### C4. What azalea leaves out is put right on its `Inventory`
fleet-mc's reader runs in `Update`, after azalea's handlers ran in `PreUpdate`, and goes through the frame's packets in order:
- **the cursor:** `SetCursorItem`, and the cursor of a whole content of the screen it belongs to;
- **`SetPlayerInventory`:** into window 0, and into the open screen's copy of the bot's slots;
- **a hotbar slot of window 0:** also into the open screen's copy, since a vanilla client's screens show the bot's own slots, not a copy.

Its system is ordered before azalea's `path_found_listener`, the only azalea system of `Update` that reads the inventory. fleet-mc never uses the pathfinder, but the ambiguity check (`no_schedule_runs_systems_in_an_undeclared_order`) holds every system to a declared place.

### C5. Reading an item and a screen
- **The name** is the custom name, or else the item's name, as the client shows it.
- **The text** (each lore line, the first 16; a screen's title) is rendered within a budget of twice what fleet-core keeps, and four steps a character. That bounds a screen of 90 slots of hostile text. Formatting the cleaning drops still leaves room.
- **The components:** the potion's kind, and the enchantments by **the world's own registry**, so the server's numbers become their names, or `minecraft:unknown`. Live: `minecraft:efficiency` and `minecraft:unbreaking` from a vanilla enchanting table.
- **A merchant's first cost** is computed as the client shows it (raised by demand, moved by a special price), in double precision.
- **`ItemData`:** every component by its kind's number, in order, present or removed, each with its length and bytes. The three map components are written sorted. A component that can't be encoded gets bytes no other item has, so a click on it is refused rather than let through.
- **A screen is only read when azalea's own picture agrees with the gate's:** the same window, or no server screen for the inventory. Otherwise there is none, and the guard says `no_menu`.

### C6. Sending, at the moment of sending (P16.9, P16.10)
- **An action:**
  1. In one job on the host thread: refused if a fate is unread.
  2. The screen as it is now.
  3. azalea's prediction on a copy of its inventory.
  4. `ClickGate::send` with the predicted slots: the guard and the gate decide.
  5. Then the packet, **fleet-mc's own `ServerboundContainerClick`** (azalea's handler mirrored, with the window's own state), the statistics request right behind it, and azalea's copy set to the prediction, as a vanilla client shows it.
  
  A button, a trade and a name are their own packets with the barrier behind; they predict nothing.
- **The prediction is azalea's, with three gaps put right:**
  - a number key swaps the slot with the hotbar slot it names;
  - a right click on a stack leaves the other half;
  - an emptied stack is empty.
  
  The rest is azalea's. Live on vanilla, a left, right and shift click in a chest or a furnace, a number key and a drop came back `ok`. A shift-click on a merchant's or an anvil's result came back `rejected`: azalea puts the result elsewhere in the bot's slots than the server does, and the server's correction puts it right. A script reads `rejected` there, though the trade happened.
- **A close:** fleet-mc's own `ServerboundContainerClose` for the window the hook saw (0 for the inventory), then azalea's copy closed as the client closes it. An action pending on the screen is `replaced`.
- **The inventory opens without a packet,** as a vanilla client's does, and only with no screen open (`screen_open`).
- **A sign is answered only through `SignDesk::answer`:** one `ServerboundSignUpdate`, never anything else.

### C7. Time
The gate's clock is monotonic: an `Instant` taken when the bot's menus begin, given to fleet-core as that much after the Unix epoch. A clock set back can't make an action unanswered early or late. `expire` runs every game tick while an action is pending.

### C8. Keys and buttons while a screen is open (M8)
- **A screen opening lets go of every key and the left button.** The right button is let go, with the release of its use, only if it was held. A screen that opens sends nothing for a bot that held nothing.
- **While a screen is open, a press, a tap, a hold until used, a mount or a dismount does nothing** and answers `Ok`. The runtime's limiter refuses them with `screen_open` first (group D); this keeps the rule whatever calls.
- **A sign editor holds nothing back.** A server never sees a client's sign screen, and a script that never answers one would otherwise hold its keys up for good.
- **Until group D closes a server's screen at once for a script at API level 1** (ADR-0036's plan), such a script's keys stay up while a server's screen is open, as a person's do.

### C9. Sign editors (M16, P16.10)
- **The opening:** a sign editor closes the open screen on the client, with nothing sent, as a vanilla client's sign screen takes the screen's place. Then `SignDesk::opened`.
- **It ends unanswered** when a screen opens, the bot dies or the bot changes worlds. fleet-mc never answers one by itself. Live on Paper, an editor nobody answered was never answered, not when a screen ended it (`a_sign_nobody_answers`).

### C10. Getting out of a minecart (a fix found by this group's stress run)
`GameAction::Dismount` tapped sneak for one game tick. When azalea catches up on ticks, both inputs reach the server between two of its ticks and the dismount is lost. It happened once in ten runs of `slow_mouse_scenario`. **Sneak now stays down each tick until the server has the bot off, for at most a second** (`GettingOut`), and a screen that opens ends it.

### C11. The harness gets the user's server's auction flow (M17, P16.11)
- `/ah <query>` searches: a listing's kind, its name or its potion holds the query.
- **Create** is in the auction house's last row (slot 47, row 5, column 2), an emerald named "Create", beside the other buttons, which keep their slots.
- **Create Listing**: a click on an item in the player's own inventory selects it. Then **Confirm Item**. Then a sign the plugin opens with Paper's `openVirtualSign` (nothing is built), whose first line is the price, read in `UncheckedSignChangeEvent`. Then the final **Confirm Listing**, `/ah sell`'s own. Each screen opens on the tick after its click, as the plugin's others do.
- `harness sign <player>` opens a sign nobody asked for. `harness-sign-open` and `harness-sign` log lines say what came back.
- **The live test drives the flow with fleet-mc's own calls:**
  - it searches;
  - it finds Create by its slot and checks the trap listing renamed "Create" in the rows above;
  - it finds the potion by its component;
  - it answers the sign with a declared sign;
  - it finds the listing in `harness listings` and no purchase in the log.

### C12. The four fates, live on both servers
- `ok`: a chest's or the sell menu's slot.
- `rejected`: a furnace's result slot on vanilla, which takes nothing and which azalea doesn't know; a plugin's cancelled button on Paper.
- `replaced`: the bot closes the screen before the reply.
- `unanswered`: no reply in 5 s.

The last two need a server that doesn't answer in time. The test freezes the server's container (`docker pause`) right after a keep-alive came, so the server's 15 s keep-alive check never sees the pause. A screen stuck after `unanswered` refused the next click, and the late reply answered nothing.

**Nothing on Paper reacted to the statistics requests.** There was one a click, nine in the Paper scenario, nine replies, no kick and no warning about the bot. At R2's limit they are 20 a second beside the clicks' 20, a sixth of Paper's packet limiter.

## What group D inherits
- **Feed the events to the script:** `on_menu_open`, `on_menu_update`, `on_menu_close`, `on_click` and `on_sign_open`, from the `MenuView` of the snapshot taken for the hook. A `MenuOpened` means everything held was let go.
- **Read every `Clicked` before the next action:** the session refuses one while a fate is unread.
- **`Limited::Click` before every `MenuCommand::Act` and `AnswerSign`;** `screen_open` for keys and buttons while a screen is open.
- **API level 1:** close a server's screen at once, as ADR-0036 planned.
- **The trap of a result slot:** a shift-click on a merchant's or an anvil's result is `rejected` though it worked. The reference says so, or the runtime's prediction learns the server's order.

## Consequences
- **fleet-mc depends on `chrono`** (the workspace's, no clock feature) to name the time the gate takes.
- **fleet-mc sends its own click, close and sign packets** instead of azalea's events. An azalea bump re-reads azalea's click handler and its prediction (the three gaps of C6), and the slow scenarios hold the result.
- **The residual risk ADR-0050 named stays:** a plugin that sends statistics unasked would answer the pending action early. Its fate would be decided before the server handled it, and the reply that is really its own would then answer the next one early, one ahead for the rest of the session. The guard still holds every click to the screen, state and item its hook saw, but a script could send the next click onto a screen whose correction hasn't come yet. Nothing in vanilla or Paper does this. A reply with no barrier out answers nothing and can be noticed; one while a barrier is out can't be told from the real one.
- **A statistics request while a container is open is something a vanilla client never sends:** its statistics screen replaces the container. An anti-cheat plugin looking for exactly that could tell the bot apart. None is known to; this is for the user to weigh per server, as ADR-0036's consequences already ask.

## Alternatives considered
- **azalea's `ContainerClickEvent`, with its `state_id` set from fleet-mc's before each click,** as group A's measurements did. Rejected: its prediction can't be put right before the gate decides, and the number key's packet carries the key, not the slot the prediction needs.
- **A model of the screens of fleet-mc's own, apart from azalea's `Inventory`.** Rejected for now: the rest of fleet-mc (the held item, the tool guard, the right button) reads azalea's, and two models could disagree. Putting azalea's right in place keeps one.
- **Every event delivered, as lifecycle events are.** Rejected: a server that opens screens in a loop would grow the queue without end.
- **A sign editor holding the keys up, as a vanilla client's sign screen does.** Rejected for C8's reason.
- **A one-tick sneak, tapped twice.** Rejected: two ticks can bunch as one did. Holding until the server lets the bot off is the vanilla player's own way.
