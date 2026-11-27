# 0053. Level 2 on the wire, in the server and in the app, and what a bot sees

- **Status:** Accepted. Decided under ADR-0025, inside what ADR-0036 (M13, M14, M16, M17), ADR-0037 and ADR-0052 decided and the user asked for at group E's start (2026-11-27)
- **Date:** 2026-11-27
- **Related:** Plan.md Phase 16, group E (P16.15, P16.16, P16.20); [ADR-0036](0036-scripted-modes.md) (D13, M13, M14, M16, M17); [ADR-0044](0044-scripted-modes-on-the-wire-the-server-and-the-agent.md) (the wire and the API at level 1); [ADR-0045](0045-the-script-editor-and-a-bots-script-in-the-app.md) (the editor); [ADR-0046](0046-the-stopgaps-way-out-and-phase-15s-check.md) (C6, an older app's save; the last release's agent); [ADR-0050](0050-menus-in-fleet-core.md), [ADR-0051](0051-menus-in-fleet-mc.md), [ADR-0052](0052-menus-in-scripts-and-the-time-of-day.md) ("What group E inherits")

## Context
Group D gave scripts level 2 (screens, the inventory, clicks, signs, the time of day, `bot.name`) inside the agent, and saved every mode at level 1 so nothing reached an agent that couldn't run it. Group E takes level 2 across the wire, the server and the app, and adds the user's P16.20: on a bot's page, what it sees. The user named five things to be sure of: a level-2 mode never reaches the last release's agent (v0.2.1, level 1), shown live, and the app says why a bot waits; an older app never silently drops what it doesn't know; the app checks a message as fleet-core does; the editor knows level 2; the log's click lines are their own kind; and what a bot sees is bounded on both sides, never hangs, and shows a server's text as text.

## Decisions

### E1. The level a script needs is stamped at save
- **The server compiles a script at level 1 and, only when that answers `NeedsLevel`, at level 2** (fleet-script's `compile_lowest`), and stores the level that compiled. A farm stays at level 1, so an agent of the last release still runs it.
- **Declared signs and `bot.name` force level 2** whatever else the text uses: a sign is answered only through level 2's `sign`, and `bot` is a level-1 object whose `name` is a level-2 member that compiles at level 1 and stops the script when read. fleet-script's lexer notes `bot.name` (and `bot?.name`, inside a template's `${…}` too) as it reads the text, beside the level-2 objects named outside a member access or a definition.
- **The reasons travel with the level** (`LevelReason`: menus, signs, clock, bot_name; the desktop crate's `check_script` answers them as `ScriptCheck.needs`), so the editor says in words why a script needs level 2.

### E2. An older app never drops signs it doesn't show
- **`ModeScriptRequest.signs` is optional on the wire:** a body without it is the last release's app, which reads a level-2 mode without its signs. Its save of a mode that declares signs is refused at `script.signs` (422, `MODE_SIGNS_UNSEEN`: "update the app"), so the signs are never lost; a mode without signs saves as before, and this release's app always sends the field, `[]` taking signs away. The same shape as ADR-0046's C6 for a step mode's hidden arguments.
- **A new app reading an older server:** `ModeScript.signs` defaults to empty when a body leaves it out, and stays required in the schema (the precedent of `AuditPage.unreadable`).
- **The app checks a message, a sign's line and their values as fleet-core does,** the number-first rule included: a message may start with a number's value, never a word's, and a sign's line with either. One test of fleet-api-types holds the app's rule and fleet-core's equal on the same cases.

### E3. The editor's description at level 2
- **`ScriptApi::OFFERED` is the newest level.** Each object says its kind: a game object the script names (`game`), a map a call answers (`value`: `slot`, `time`, `input`) or the hooks (`hooks`); each member its level, and a method the map it answers (`answers`). The app offers only game objects for a name, and after a call that answers a map (`menu.slot(13).`, `clock.local(zone).`) that map's keys; a call's own parentheses may hold none of their own.
- **The editor says the level the server would stamp, and why** ("uses screens and the inventory", "uses the clock", "reads the bot's name", "declares signs"), and that only agents of this release run level 2. `sign.write` with a key the mode doesn't declare is marked, as `chat.send`'s is.
- **A mode's signs are a section of their own,** beside messages and chat patterns: a key, four lines and typed values; empty lines at a sign's end are left out of the request, one in the middle kept. The five level-2 examples (the sell macro, the auction scan, the farm that sells and both listers) are the editor's starter templates beside the four of level 1.

### E4. A click line is its own kind
- **`LogKind::Click`** (fleet-core; the wire's `SCRIPT_LOG_KIND_CLICK`, 10; the API's `click`), for every miss of a click, a screen action or a sign's answer, and for a refusal in a screen or a sign editor; `refused` stays chat's. The runtime writes what was clicked and its fate in words, with the name a script reads after it: `unclear` is "may have worked: the script checks the next screen", `rejected` "was put back by the server", `replaced` "wasn't answered: its screen closed or another opened first", `unanswered` "got no answer from the server, which may or may not have done it", and a click that wasn't sent says its reason's meaning from fleet-script's description. The app labels the line "Click".

### E5. What a bot sees: asked on demand, bounded on every side
- **`GET /api/v1/bots/{id}/screen`** for View on the bot, 404 below, as its chat. It reads only, so no audit entry. It spends the caller's own `[rate_limits] screen_reads_per_user_per_min` (12 unless set, 1 to 60), after `authorize()` passed, so a stranger spends nothing and learns nothing.
- **The server asks only a stream at script API level 2 or newer** (`ServerMessage::ReadScreen`): an agent of the last release would end its stream at a message it can't read, so its bot answers `agent_too_old` at once. A bot that isn't on a live agent answers `offline`. At most 8 questions wait on a stream, numbered by the server, and only that stream's answer for that bot settles one; a stream's end drops its questions. The service waits 5 s, then answers `no_answer`; a stream with 8 waiting, or an agent already looking at that bot, is 503 `busy`, never a wait.
- **The agent:** the conductor runs at most as many looks as it runs bots, each bounded at 3 s, and answers on an outbox class of its own (16, dropped at a stream's end, as the chat lane's answers are); the bot's actor runs one look at a time, bounded at 2 s, a second one `busy`, ended with the session's child token.
- **What crosses** is what a script reads: the screen's kind, title, grid and slots, the bot's 36 slots in it, the cursor, or the bot's own inventory when no screen is open, and the sign editor that waits, with its place and side. Never the guard's state (window, state ID, an item's whole data, trades). The server reads the answer through fleet-core's types, so every text is cleaned and bounded again there; an answer that breaks a rule (a slot twice, an identifier that isn't one) is refused as a whole.

### E6. "What it sees" on a bot's page
- **A card on every bot's page,** under the Script card: the main use is looking at a server's screen before a script for it exists. Its agent is asked only when Look is pressed, never on opening the page, as each look spends the viewer's limit and asks an agent; the answer is dropped when the page closes, as it's a moment's.
- **Each filled slot as a script names it:** its number, its row and column, and in a screen's own grid the `menu.at(row, column)` that names it; its item's ID and count, name, lore, potion, enchantments, stored enchantments and uses left, each as plain text a person can select and copy into a script. The empty slots read as ranges (`Empty: 0–3, 5–8`). Why it can't be said is a sentence: not in a world, an agent that needs this release, no answer within 5 s, and a kind this app doesn't know.
- **A bot waiting for an agent of this release** says its mode uses script API level 2, what that means, and to update an agent to this release.

### E7. The last release's agent, live
- **`slow_smoke_an_agent_of_the_last_release_is_sent_no_level_2_and_refuses_nothing`** (renamed from `…_is_sent_no_script_…`, since v0.2.1 runs level 1): the published v0.2.1 agent image, pinned by its index digest, beside this release's server. A level-2 mode is 409 `agent_too_old` on its bot and a new bot on it waits; what a bot of it sees is `agent_too_old` within the server's bound and the waiting bot's `offline`; a level-1 mode runs on it; its stream never ends and its log holds no warning.

## Consequences
- **Level 2 needs agents of this release.** An operator updates the server first, then each agent (the runbook's order); until then a bot on a level-2 mode waits and says why, and a level-1 mode runs anywhere.
- **The last release's app can't save a mode with signs,** and says to update; everything else it saves as before.
- **A new rate-limit class and one new config key,** `[rate_limits] screen_reads_per_user_per_min`.
- **Every bot's page has one more card,** so every screenshot of a bot's page changed.

## Alternatives considered
- **Stamp every script at the newest level.** Rejected: a farm would stop reaching the last release's agents for no reason of its own.
- **Drop an older app's missing `signs` as "none".** Rejected: that silently deletes what a level-2 mode declares, the user's first rule. Reading it as "keep the stored ones" was the other way; refusing is ADR-0046's precedent, and the app's message says what to do.
- **Send `ReadScreen` to every stream.** Rejected: a v0.2.1 agent ends its stream at a message it can't read, which would cost every bot on it a reconnect.
- **Read what a bot sees on opening its page, or every few seconds.** Rejected: each read asks an agent and spends the viewer's limit; the user asked for a refresh button.
- **Show the card only on a scripted mode's bot.** Rejected: the screen is looked at to write the script, before the bot runs one.
