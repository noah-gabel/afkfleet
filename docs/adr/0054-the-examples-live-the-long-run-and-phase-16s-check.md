# 0054. The examples live on the harness, the long run, and Phase 16's check

- **Status:** Accepted. Decided under ADR-0025, inside what ADR-0036 (M3, M4, M9, M13, M17 as amended, R1, R2) and ADR-0049 to ADR-0053 decided and the user asked for at group F's start (2026-11-27)
- **Date:** 2026-11-27
- **Related:** Plan.md Phase 16, group F (P16.17 to P16.19); [ADR-0036](0036-scripted-modes.md); [ADR-0046](0046-the-stopgaps-way-out-and-phase-15s-check.md) (C8, Phase 15's check, whose shape this follows); [ADR-0049](0049-the-paper-harness.md) (the harness); [ADR-0051](0051-menus-in-fleet-mc.md), [ADR-0052](0052-menus-in-scripts-and-the-time-of-day.md) (G4, G5), [ADR-0053](0053-level-2-on-the-wire-in-the-app-and-what-a-bot-sees.md); `crates/fleet-integration/tests/integration/slow_trading/`, `long_run/`, `harness/paper.rs`, `harness/trades.rs`; `harness/paper/plugin/`

## Context
Group F closes Phase 16: the five level-2 examples run live, a long run, and the phase's definition of done. The user named what to be sure of:
1. **The examples run as a friend runs them.** Each is saved through the API as its starter template and sent by the server to a real agent, with no edits but the values a template asks for, and each case passes six runs in a row. The second lister goes through the price anvil with the trap, a price the plugin refuses and the watchdog. The farm counts its potions and the held item. The scan meets the other player buying its listing between the scan and the click, buys nothing in its place and logs the miss.
2. **The long run counts from the harness's own records:** double buys, silent misses and clicks onto another screen (title, window, state), under the other player buying, listings changing, a screen opened unasked, the server frozen past a fate's 5 s and a disconnect; every fate counted and every miss with its reason; Paper's log without a kick or a warning about the bot. The 10-minute limit a test stays.
3. **Nothing is left behind, whichever way a script ends** (a stop, a limit, a disconnect, a mode change mid-click), checked on the harness.
4. The DoD point by point.
5. What changes since v0.2.1, for the release notes.

## Decisions

### F1. The examples, live, as a friend runs them
fleet-integration's `slow_trading::examples`, one case an example, in `just test-slow`. Each saves the example's draft through the API exactly as the editor's starter template fills the script (the template fills only the script; the mode's name is the author's, here the example's ID: a name is at most 32 characters, and the anvil lister's title is longer). The real server stamps it level 2 and sends it to the real managed agent on fleet-mc's azalea connector, which runs it against the Paper harness beside a second player on an idle step mode. A case is judged by the harness's records (F3), never by the bot's word alone.

| Case | What the harness shows |
|---|---|
| The sell macro | Its first minute's sale: the three Turtle Master potions shift-clicked in, Sell once, the menu closed; the pickaxe and the wheat (which the menu would buy) left |
| The auction scan | The cheapest diamond within 1,500 bought once. The other player buys the listing it picked on its confirm screen: its confirm is refused (`GONE`), it logs "The purchase didn't go through." The other player buys the listing it will pick the tick after its auction house opens: its click meets the listing gone, the plugin draws the next one in that slot, which it never clicks, and it logs the miss |
| The farm that sells | Farming with its shield up in hotbar slot 1; its three potions sold, none drunk (no `harness-consume`, no effect), its junk dropped and nothing else, both tools kept; then farming on with the shield up |
| The auction lister | Bait below its floor: nothing listed, "Not listed at 20.0: below the floor."; then the middle of the three cheapest others list (50, 400, 500), never its own 150; its expired listing collected |
| The anvil lister | A price the plugin refuses (`harness cap`): the anvil stays and it stops with its line. A step that never answers (`harness stall`): its watchdog ends it. A screen opened unasked in the middle of the flow: it stops and closes it. Then the listing through the anvil at the middle price; Create clicked by its slot each time, the trap renamed "Create" never; each stop closed the screen it stopped on |

### F2. What the live runs found, each fixed with a failing test first
- **A script's end left its screen open** (the user's point 3). A script that hit a limit or failed, and one that ended in order while its session lived on (its mode changed or edited), released its keys but left a server's screen open, a click perhaps still pending. The instance now closes the screen its last hook saw before the release, unless `on_stop` closed it; the release stays last, which the runner's held mark relies on. Red first in fleet-script's suite and fleet-runtime's fleet tests, and live: `ways_out` failed at the mode change without it.
- **The listers listed 10 minutes after every join, and the farm sold 5 minutes after:** `timer.every` restarts at each join, so a bot kicked more often than that never listed or sold, and a lister's case couldn't fit a test's 10 minutes. They now try 10 to 30 s (the listers) or 30 to 60 s (the farm) after a join, then every 10 or 5 minutes from the last try.
- **The auction scan stayed stuck when its pick was gone before its click.** The plugin refused the click and redrew the page in place; no hook of the example heard that, so the auction house stayed open, every later scan was skipped and nothing was logged. The scan now expects Confirm Purchase within 3 s of its click; still on the auction house then, it logs the miss with the price and closes. A confirm it no longer waits for is closed unconfirmed, where it would have read the price of nothing and stopped.
- **The auction lister took its own close of the auction house for Confirm Listing's,** sent `/ah expired` at once, and its screen replaced Confirm Listing in the same tick: nothing was ever listed. The fake world of the suite never delivered that close; the runtime does, for every close. It now remembers the title of the screen that opened, and only Confirm Listing's close moves it on; a listing whose screen never came no longer holds every later try back.

The reference shows each example as its file has it.

### F3. The harness's records and controls (test-only)
- **Each click's line says what it met and what it was sent onto:** the window and state the server had when it handled it, the state its packet carried (`sent=`), and the listing the slot held, all before any handler ran. No Bukkit event says a window, a state or a packet's state, so the plugin reads them from Paper's own objects (Paper runs Mojang's names): the player's `containerMenu`, and a Netty handler before Paper's `packet_handler` that notes every container click packet as the network reads it. A packet whose window the server no longer had open reaches no handler and is logged as `harness-ignored`. If the objects can't be read, the plugin warns, which fails every case's log check.
- **More records:** a drop (`harness-drop`), a drink (`harness-consume`), a click on a listing that's gone (`harness-gone`), a snipe, an intrusion, a stall, a kick inside a click; `harness state <player>` answers the screen open, the cursor, the held slot and item, a raised hand, sneaking and the items carried.
- **Controls that put each race on a known tick:** `snipe` (the other player buys the cheapest listing of an item, or the one on a confirm screen, the tick after the bot opens that screen), `live` (every open auction house redrawn when the market changes, as most plugins do), `cap` (the price anvil's highest), `stall` (Confirm Item never answers), `intrude` (a screen opened the tick after the bot opens a title), `kickon` (a kick inside the bot's next click), `count` (listings of an item at a price or less: `listings` outgrows one RCON packet, which the harness's client reads alone).
- **Why server-side:** a race timed from the test through RCON lands anywhere in a 300 to 900 ms wait; the plugin's own tick never misses it, so every run meets the same race.

### F4. Every way out, mid-click (the user's point 3)
`slow_trading::ways_out`: a test script that holds sneak and the right button with a shield, opens the sell menu and clicks there every second. For a click to wait for its fate, the server is frozen (`docker pause`) before it goes out; a variant loops for good 300 ms after a click, only while that click waits. Each way, then the harness's state:

| Way | How | Then |
|---|---|---|
| A mode change | Another mode set through the API while a click waits | The sell menu closed by the bot, nothing held, nothing on the cursor |
| A limit | The loop hits the operations' limit while a click waits | The same; the bot stays online, idle |
| A disconnect | A kick inside the bot's click (`harness kickon`); it joins again | No screen, nothing held: a rejoin presses nothing again |
| A stop | The bot stopped through the API while a click waits | The menu closed, the bot gone |

### F5. The long run (P16.18)
- **Its own recipe and profile:** `just long-run`, fleet-integration's `long_` tests in the nextest `long` profile, which no other profile includes; seven parts of 500 clicks, each a test under the 10-minute limit with a Paper harness of its own (`fits` holds each part's worst case to it: its steps get 300 s). The Slow tests workflow runs it after `just test-slow`.
- **The buyer** (`long_run/buyer.rhai`): the auction scan's way as fast as the limits let a script go, written for the run. It pages forth to the last page and back, picks the cheapest diamond for 1,000 or less on each page, confirms it only on its Confirm Purchase screen and only if that shows the listing picked, judges a confirm by the diamonds in its inventory, never acts while a click waits for its fate, and logs every buy it meant and didn't make, naming the price. The reference's scan as written runs every 30 s with 300 to 900 ms between steps: thousands of clicks would take hours.
- **How fast a script can click:** each of the buyer's steps comes 250 ms after the event before it, about 3.5 clicks a second. A click's fate comes back on loopback within the same game tick, so a click from the hook that hears it is `too_soon` under the click limit (R2), and a timer's shortest wait is 250 ms. The first live part, clicking from `on_click`, had nearly every second click refused that way; the run's parts are sized from the paced rate.
- **The market:** five pages it can't afford, one diamond within its budget kept listed at a price no listing shares (the ledger tells listings apart by price), topped up at every change of the market, and every open auction house redrawn at every change. One at a time, since each buy needs a command to open the auction house again and the bot sends one every 3 s at most: with three, a part of 600 clicks spent 288 of its 300 s.
- **The chaos, at counts of the buyer's clicks:** the market changing every 40 clicks; the other player buying first, on the auction house and on the confirm, twice each; a screen opened unasked in the middle of a purchase, twice, and anywhere, once; the server frozen until a click's fate comes back `unanswered`, twice (bounded at 12 s, below Paper's 15 s keep-alive check, which a longer pause can trip); a kick. Then an orderly stop, whose `on_stop` names a buy still open and says how many fates the script heard.
- **The ledger** (`harness/trades.rs`), from the harness's records against the runner's line for each click (window, state, slot, the item the hook saw) and the script's log; each received click is matched to the first sent one after the last match with its window and slot, since the harness hears a subsequence of what was sent:
  - **a double buy** is a purchase no pick meant (by the price the script saw), or a second for one;
  - **a silent miss** is a pick that bought nothing without a line naming its price;
  - **a click onto another screen** is one whose packet carried another window or state than the script saw, or one never asked for.
  - Beside them it counts what isn't the script's to prevent: a click sent onto the state the script saw that met a newer one, the server having redrawn the page in flight (the server applies it anyway, then sends the whole screen; the buyer's confirm check catches a wrong listing), and packets the server dropped because their window was gone.
  - **Every fate:** each fate other than `ok` is a click line of the script's log; `ok` is the rest of the fates the script heard, which its `on_stop` counts; a click whose session ended first (the kick) got no fate at all, and is counted so.
  - **The statistics request behind every click:** a fate comes only from its reply (R1), so every sent click that got one had it. Paper's log is read whole at each part's end.
  - **Its own tests,** each count broken in turn, fail: the counting is held as the definition of done is.
- **Each part prints `LONG` lines** and fails on a double buy, a silent miss, a click onto another screen, or a kick or warning about either player in Paper's log that it didn't cause.

## What was measured
On this PC (Windows 11, Docker Desktop), 2026-11-27, against the harness (Paper 26.1.2).

### The live cases, six runs in a row
nextest's `--stress-count 6` over the six cases, 36 of 36 passed (3,309 s, 551 s an iteration):

| Case | Passed | Took, each |
|---|---|---|
| The sell macro | 6 of 6 | 70.8 to 71.2 s |
| The auction scan | 6 of 6 | 102.1 to 102.5 s |
| The farm that sells | 6 of 6 | 53.2 to 53.9 s |
| The auction lister | 6 of 6 | 62.7 to 63.2 s |
| The anvil lister | 6 of 6 | 231.2 to 231.6 s |
| Every way out | 6 of 6 | 30.4 to 30.6 s |

Two runs before it didn't count: one found `ways_out`'s rejoin race below, and one was cut when the PC went to sleep while a harness started (it slept 11 hours; its start's bound had passed when it woke). The run that counts kept the PC awake while it lasted.

### The long run
`just long-run`, all seven parts in one run (1,452 s, each part 206 to 209 s, about 199 s of it clicking):

| | Each part | All seven |
|---|---|---|
| Clicks asked / sent / received by the harness | 500 to 501 / 498 to 500 / as sent | 3,501 / 3,495 / 3,495 |
| Fates | `rejected` 496 to 498, `unanswered` 2, `ok` 0 (every button here is a plugin's, which answers `rejected`) | `rejected` 3,479, `unanswered` 14, no fate because a kick ended the session first 2 |
| Not sent | 0 to 2, all `content_changed` (a redraw reached the bot between its hook and the sending) | 6 |
| Picks / bought / misses | 13 to 15 / 10 / 3 to 5 | 94 / 70 / 24 |
| Double buys, silent misses, clicks onto another screen | 0, 0, 0 | **0, 0, 0** |
| Sent onto the state seen, met a newer one | 0 or 1 | 2, both page turns |
| Packets no handler got | 0 | 0 |
| Kicks or warnings about a player in Paper's log not caused by the part | none | none |

- **Every miss has its line,** with its reason: an unexpected screen (the intrusion in the middle of a purchase), "it didn't go through" (the other player bought the listing on the confirm screen first: the plugin refused the buyer's confirm, `GONE`), "no confirm screen came" (the listing gone between the pick and its click, twice in 7 parts). A snipe on the auction house mostly came before the buyer's step: with the page redrawn at once, the buyer never picked the listing gone.
- **Each freeze lasted until a click came back `unanswered`:** 5.3 to 5.7 s.
- **The statistics request behind every click:** every sent click got its fate but the two a kick cut off, and Paper's log held no kick or warning about either player but the operator's kick each part caused.
- **On CI's runners** (the Slow tests workflow on group F's branch, run 46139264132, ubuntu-latest): all seven parts passed in 1,428 s, each 202 to 209 s, about 190 s of it clicking. 3,502 clicks asked, sent and received; `rejected` 3,486, `unanswered` 14, no fate after a kick 2; 91 picks, 70 bought, 21 misses each with its line; **double buys 0, silent misses 0, clicks onto another screen 0**; 2 met a page redrawn in flight. Its `just test-slow` step took 45 minutes (the six live cases 35 to 236 s each), its `just long-run` step 25, inside the workflow's 120.

### Found by the runs and fixed in the tests
- `ways_out` once in two iterations: `harness state` answered as soon as Paper had the player back after the kick, and the sell menu the next way opened reached a client still joining. It now waits for the script's own join.
- The long run's first buyer clicked from the hook that hears a fate, and every second click was `too_soon`; with three listings within its budget, a part of 600 clicks spent 288 of its 300 s on the commands that reopen the auction house. F5 says what changed.

## Phase 16's definition of done
**Met.** Each point, with what shows it:
- **The reference's sell macro and auction scan run against the harness as written:** each a case of `slow_trading::examples`, saved through the API as its starter template, six runs in a row; so do the farm that sells and both listers (F1). The reference shows them as their files have them, F2's fixes included.
- **In the long run no listing is bought twice:** none of the buyer's 70 purchases over 3,495 clicks was one no pick meant, nor a second for one pick, counted from the harness's purchases against the listing each pick's hook saw (F5). Under it, the harness's market takes a listing off in the same step it sells it, on the server's main thread.
- **No failed or missed buy lacks its log line:** each of the 24 picks that bought nothing has a line of the buyer's naming its price and why; the live auction scan logs the listing bought before its click and the one bought on its confirm screen (F1).
- **A click is never sent to a screen other than the one the script saw:** none of the 3,495 clicks the harness received in the long run carried another window or state than its hook saw, read off the network before the server handled it; two met a page redrawn in flight, which the server applies anyway, and the buyer's confirm check holds what follows. Under it: fleet-core's guard and its properties (ADR-0050), fleet-fuzz's `click_guard` (ADR-0052, G9), fleet-mc's `slow_screens_on_paper` (a click onto a new screen of the same title refused, nothing sent), and the anvil lister, which never clicks the trap renamed "Create" (F1).

Beside the definition of done, the user's third point: **nothing is left behind** after a stop, a limit, a disconnect or a mode change in the middle of a click (F4, six runs in a row), after F2's first fix.

## Consequences
- **`just test-slow` takes about 9 minutes more** for the six live cases, and **`just long-run` about 25 minutes,** which the Slow tests workflow runs after it.
- **The harness's plugin reads Paper's internals** through reflection for two things no event says; a Paper bump checks they still read (a warning fails every case).
- **The examples changed** as F2 lists; a friend who started from an earlier template keeps their copy as it was: the reference shows the new one.

## Alternatives considered
- **The reference's auction scan as the long run's script.** Rejected: its 30 s period and 300 to 900 ms waits give a few hundred clicks an hour.
- **Races timed from the test through RCON.** Rejected for F3's reason.
- **The long run in `just test-slow`.** Rejected: it would add its whole length to every slow run; a recipe of its own keeps `test-slow` for the cases and still runs on CI.
- **A packet's state read on the bot's side alone** (fleet-mc's log). Rejected: the definition of done counts from the harness's records; the bot's side is what the script saw, the harness's what was sent and met.
- **Comparing only the server's state at a click** with the script's. Rejected: a redraw in flight would read as a click sent onto another screen, which the guard never sends; the packet's own state tells the two apart.
