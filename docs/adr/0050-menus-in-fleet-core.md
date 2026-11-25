# 0050. Menus in fleet-core: a server's text, the screen's snapshot, the click guard, one click at a time, and the sign desk

- **Status:** Accepted. Decided under ADR-0025, inside what ADR-0036 decided and the user added (M1 to M18, R1, R2)
- **Date:** 2026-11-25
- **Related:** Plan.md Phase 16, group B (P16.4 to P16.7, and the sign's part of M16); [ADR-0036](0036-scripted-modes.md) ("Decisions for Phase 16", its amendments); [ADR-0037](0037-scripts-read-system-chat.md) (C3: the amount reader); ADR-0010 (the text rules, time passed in); [ADR-0042](0042-scripted-inputs-in-fleet-mc.md) (facts are identifiers and numbers); [ADR-0049](0049-the-paper-harness.md) (the harness); `crates/fleet-core/src/script/menu/`, `script/amount.rs`, `script/limiter.rs`, `script/template.rs`

## Context
Phase 16 lets a script read the bot's inventory and a server's screens and click in them. Its group B builds fleet-core's part, pure and tested before anything reads a packet. fleet-mc (group C) then feeds these types from azalea, and fleet-script (group D) shows them to a script. ADR-0036 fixed the rules:
- **M1:** name and lore are cleaned as chat is, and bounded.
- **M3:** a click goes out only onto the screen, the state and the whole item the hook saw.
- **M4, R1:** one click at a time, with four fates decided at the reply to a barrier.
- **M11:** a small closed set of components, as IDs and numbers.
- **M16:** sign input.
- **M18:** slots by position.
- **R2:** a click a game tick.

This records how group B built them, and what was left to decide.

## Decisions

### B1. A server's text: `ServerText` and `Lore` (M1)
- **Cleaned with the chat sanitizer, as a sender's name is:** no formatting code, no control character, line breaks included, no text-direction mark, no invisible character, cut on a character boundary. Each text is one line wherever it's shown, and cleaning it again changes nothing. Property tests run it over any `String`, and over text made only of what hides or reorders.
- **Bounds:** a name to 64 characters, lore to 16 lines of 128 (M1). **A screen's title to 128** (decided here), longer than a name: servers with their own GUIs build titles out of many private-use glyphs, and a script compares a title, so cutting it short would make two different titles equal.
- **An empty lore line is a line,** as on the screen.

### B2. The components a script reads (M11)
- **A potion's kind is kept only for the four items** (a potion, a splash potion, a lingering potion, a tipped arrow) and only when it's an identifier. Anything else gives `None`.
- **Enchantments: identifiers only, levels 0 to 255.** An entry with an ID that isn't an identifier, or with a level outside that range, is left out. At most 16 are kept, the first by ID. An ID that comes twice keeps its highest level.
- **What's kept doesn't depend on the server's order.** The reader holds at most 17 entries at any time, so a server that sends thousands costs time in proportion and no memory.

### B3. The whole item: `ItemData` (M3)
- **An item also carries every data component the server sent, as fleet-mc encodes them,** shared and never read. Two items are equal only when their ID, count, durability, name, lore, read components and this encoding all are, and that equality is the guard's comparison. So two listings of one kind of item that differ only in a component no script reads are still two items.
- **fleet-mc's encoding must be canonical** (P16.8): the same components give the same bytes, whatever order they came in. A difference there only refuses a right click as `slot_changed`; it never lets a wrong one through.
- **Exact bytes, not a hash:** no collision argument is needed, and since the bytes are shared, a snapshot copies none of them. `Debug` prints only the length.

### B4. Screens (M2, M18)
- **`ScreenKind` is azalea 0.16.0's 25 menu kinds and the inventory,** with the own slots azalea's `Menu` lays out. For example, the crafter has 9: azalea leaves out its result slot. A kind added in a Minecraft bump fails fleet-mc's match on azalea's enum, so it can't slip through.
- **`ScreenId` is the window and which opening of the session it is.** A server counts its windows round from 1 to 100, and a plugin may reopen a screen under the old number. Since "a new screen is a new screen" (M2), the guard compares the opening too. Only `ClickGate` mints a `ScreenId`.
- **A `Screen` always has exactly its kind's slots,** whatever the server sent: a missing slot is empty, and anything past the last is never read. It numbers its own slots first and the bot's 36 after them. The exception is the inventory, whose offhand is slot 45, after the 36.
- **A screen also has its cursor, and a merchant its trades.** A merchant keeps at most 64 trades (decided; a villager has about ten).
- **`BotInventory` is window 0's slots 5 to 45:** the armour, the 36 and the offhand. The 2-by-2 crafting grid shows only in the open inventory screen.
- **Rows and columns (M18):** the chests, the 3-by-3 screens, the shulker box and the hopper are grids, and the bot's 36 slots are four rows of nine in every screen. `Screen::at(row, column)` finds a grid's own slot.

### B5. The click guard (M3; P16.6)
**`MenuAction::guard` is a pure decision**, made on the screen open at the moment of sending. It checks, in order:
1. A screen is open, or the action is refused as `no_menu`.
2. It's the opening the hook saw, or `screen_changed`.
3. Its state and its cursor are as the hook saw them, or `content_changed`. For a trade, the trade at its place must also be as seen.
4. Every slot the click touches holds the whole item the hook saw, or `slot_changed`. A click touches its slot, and a number key also touches the hotbar slot it swaps with.

- **The cursor is part of the content** (decided). What a click does depends on the cursor, and the server sends the cursor alone, without a state ID.
- **A shift-click's destination isn't compared slot by slot.** The state covers what the server changed, and the gate below keeps the bot's own clicks from overlapping.
- **Tests:**
  - every whole-item difference, each as its own case;
  - two properties against a model, one with two random screens and one with a screen made from the seen one by a few changes;
  - the gate's session property (B6).

### B6. One click at a time: `ClickGate` (M4, R1; P16.6)
- **One action pending a bot,** not just a screen: a second one is `click_pending` on any screen. That's stricter than R1's "one click at a time a screen", and simpler to hold. It costs nothing, because a plugin's next screen comes after the reply (R1).
- **The fate is decided at the barrier's reply,** from what came for the action's window between the send and the reply:
  - `replaced` if the screen closed or another opened;
  - else `rejected` if a correction came: the whole content, a slot it predicted, or the cursor;
  - else `ok`.

  `unanswered` comes at `expire` after 5 s. Every action sent gets exactly one fate.
- **The barrier is counted, not named.** The gate counts barriers out and replies back, and the server answers them in order. A late reply belongs to its own, unanswered click and never answers the next, and a reply when no barrier is out answers nothing.
- **The count is never reset, not even when a screen reopens:** a late reply would then answer the next click early. This trades liveness for safety. A server that never answered the barrier would leave every click `unanswered`, which the log shows; it would never cause a buy that isn't waited for.
- **After `unanswered`, the screen takes nothing** (`unanswered`) until a screen is opened again. It can still be closed.
- **A screen's button, a trade and an anvil's name are actions like a click:** one at a time, each with a barrier. Their fate says only that the server handled them, since azalea predicts no slot for them.
- **A close is guarded by its screen** (`no_menu`, `screen_changed`). It's never pending and never limited, as a release isn't, and an action pending on the screen it closes becomes `replaced`. The inventory opens only when no screen is open (`screen_open`).
- **What's left:** a plugin that sends statistics unasked would answer a click early. Nothing in vanilla does. Group C can change the barrier without changing the gate (R1).
- **The session property:** random sequences of opens, closes, updates, looks, clicks, corrections, replies and waits. It asserts that a click goes out only onto what its hook saw, never while another is pending and never onto a stuck screen, and that each click gets exactly one fate, in order. A temporary assertion showed its cases send three clicks and more.

### B7. Sign input (M16)
- **`SignLine`: at most 80 UTF-16 units.** Paper 26.1.2's `Paper.maxSignLength` is 80 by default, read with `javap` in the harness image, and Paper cuts a longer line to 80 code points. Vanilla's packet reads up to 384. A line can't hold what a declaration can't (a control character, `§`, a text-direction mark, an invisible character), and it may be empty.
- **`SignTemplate` holds a sign to P15.2's rules.** It reuses the message template's steps: resolving a text's names, writing the values, assembling and measuring a text. Those moved out of `MessageTemplate` with its behaviour unchanged, and its tests held it.
  - 1 to 4 lines, at most 8 values for the whole sign, each value used once on it.
  - Each line's longest text is checked when it's declared, and `render` puts each finished line through `SignLine` again (`RenderError::SignLine`).
  - Two differences from a message, since a sign's line is never a command: **a line may start with a value** (the price alone on its line), and **a line may be empty.**
- **`SignDesk` answers only the editor the server opened last, and only once.** Its refusals are `no_sign`, `sign_changed` and `sign_answered`.
  - **A screen opening, a death or a world change ends the editor unanswered** (decided). A vanilla client's sign screen is replaced then, and the vanilla client would send its lines at that moment. Ours sends nothing, as M16 has it.
  - **`cancel` writes four empty lines** (decided), as a vanilla client sends for a sign left empty. Vanilla's Esc would send back the sign's text as the server set it, but no declaration holds that text, so the bot never sends it.
- **A sign editor's opening closes the open screen for the gate:** a vanilla client's sign screen takes the container's place. Group C wires that.

### B8. Numbers (M6; P16.7)
- **`find_amount` reads the first number in a text, or none,** with P15.30's reader:
  - **A digit right after an ASCII letter or digit, the decimal mark or the separator is part of something else.** `AfkBot1` is a name, and `.5` is no number. A letter of another script doesn't glue: price tags in other alphabets need no space before their digits.
  - **A minus is a sign only when no word comes before it.** `-250` and `Change:-250` are negative, and `Tier-2` is a word.
  - **If the first number can't be read, the answer is `None`:** too long, or past the largest. A later number never stands in for it, so a bait text such as `Price: 1000000000000000000 or 5` reads as nothing.
- **`Lore::number(label)` reads the first line that starts with the label,** leading spaces aside and with case counted, then finds the first number after the label. If that line holds no number, no later line is read instead.
- **`Lore::has(text)`:** an empty text is in no line, so a script's `lore_has("")` is never true by accident.
- **The decimal mark is the caller's.** How a script names it is group D's (P16.12).

### B9. The click limit (R2; P16.7)
**`Limited::Click` is one a game tick, 50 ms, with no burst.** It's one class for everything a screen answers: a click, a screen's button, a trade, an anvil's name, and a sign's answer. A close isn't limited. The user's own client measurement (ADR-0049, H5) is optional and hasn't come in, so R2 stands.

### What the next groups inherit
- **Group C (fleet-mc):**
  - Feed the gate and the desk from what the server sends: an opened screen, a close, a whole content, a slot, the cursor, the barrier's reply, a sign editor. Call `expire` every tick.
  - Send a `MenuAction` only after `ClickGate::send` allowed it, on the host thread, with the screen as it is then and azalea's predicted slots, and with its barrier right behind it.
  - Keep each window's own state ID (P16.8), and encode `ItemData` canonically.
  - Take an item's name as the client shows it: its custom name, or else its item name (plugins' buttons often use the latter).
  - Answer a sign only through `SignDesk::answer`. On a screen opening, a death or a world change, end both the screen and the editor.
- **Group D (fleet-script, fleet-runtime):**
  - `MenuRefusal`, `ClickFate` and `SignRefusal` join a script's results. Their texts are the reference's.
  - A mode's declared signs join its `Script` at level 2, with the wire and the server in P16.15.
  - `Limited::Click` goes into the turn.
  - M15's counting down goes into the template's writing of values, shared by messages and signs.

## Consequences
- fleet-core has the whole menu model before any packet is read, and every rule in it is a pure function or a small state machine with property tests.
- `RenderError` has a sixth reason, `SignLine`, and `Limited` a twelfth kind, `Click`; nothing outside fleet-core matched either exhaustively.
- The script's own refusal list (`Refusal`) is unchanged until group D. The reference's generated tables don't change in this group.

## Alternatives considered
- **A hash of an item's components** instead of the bytes: rejected. It needs a collision argument and saves nothing, since the bytes are shared.
- **One pending click a screen, as R1 words it:** rejected for one a bot. Two pending clicks on two screens would need two barriers matched to two screens, and nothing gains from it.
- **Resetting the barrier count when a screen opens:** rejected. A late reply would answer the next click before the server handled it, which is the double buy M4 exists to prevent.
- **Echoing the sign's text on cancel,** as vanilla's Esc does: rejected. It sends a server's text back, which no declaration holds.
- **A digit after a letter starts a number** (`x2 Diamonds` reads 2): rejected. `Seller: AfkBot1` would read 1, and lore steers purchases.
- **Taking the next number when the first can't be read:** rejected for the bait case in B8.
