# 0046. The stopgap's way out and Phase 15's check: a step mode converted to a script, the reference generated, and the examples and the spoofs on a real server

- **Status:** Accepted. Decided under ADR-0025
- **Date:** 2026-11-22
- **Related:** Plan.md Phase 15, group G (P15.27 to P15.29, P15.36); [ADR-0034](0034-a-stopgap-action-into-the-nearest-minecart.md) (the stopgap), [ADR-0036](0036-scripted-modes.md) (D4, D7, D13, D17, D18), [ADR-0037](0037-scripts-read-system-chat.md) (C4, C9), [ADR-0040](0040-fleet-script-as-built.md) to [ADR-0045](0045-the-script-editor-and-a-bots-script-in-the-app.md), whose decisions this builds on; `crates/fleet-script/src/convert.rs`, `crates/fleet-script/src/api/reference.rs`, `apps/desktop/src-tauri/src/script_convert.rs`, `crates/fleet-integration/tests/integration/slow_scripts/`, `crates/fleet-integration/tests/integration/smoke/chaos/last_release.rs`

## Context
Groups A to F built scripted modes everywhere. Group G closes the phase: the way out of the stopgap `enter_minecart` that ADR-0034 added for one release, "Convert to script" for every step mode (D17), the records (the reference out of draft with its tables generated, the threat model's rows), and the phase's check on a real server (D18, C9). The user's reviewer named what they'd look at closely: the converter tested against the plan it replaces, schedule for schedule, for every step kind; a chat step's arguments hidden below the right to edit after the conversion as before it; `enter_minecart` refused in a new mode and kept in a stored one; the balance example beside a second player's every spoof, live, repeated; the phase's definition of done point by point.

## Decisions

### C1. The converter is fleet-script's, and writes each step as a line of a hook
`fleet_script::convert(&ModeDefinition)` answers a `Conversion`: fleet-core's checked `Script` and the notes of what the script does differently. It lives in fleet-script, the crate that knows the language: no other crate writes Rhai.

- **Each step is a line, marked as the mode editor names it:** `// while it's online, step 2`, by its section ("when it joins", "while it's online", "after it respawns") and its place there. A converted script outlives its steps, so its names are the ones the user saw.
- **A step at start is a line of `on_join`,** which runs at every join, as the steps at start do; **on respawn, of `on_respawn`;** **a repeating step gets a timer of its own,** set in `on_join` after the steps at start and named after it (`online_2`): `timer.every` without jitter; with jitter, a `timer.after` of the interval and `random.between(0, jitter)`, set again at each run, so each gap is drawn as the plan draws it.
- **Repeating steps next to each other on one schedule without jitter share a timer.** They're always due together and run in step order, as in the plan; steps that aren't next to each other keep their own, since a shared timer could run them out of step order beside a third one due at the same moment. A script has 16 timers: a mode that needs more is refused with the count (`TooManyTimers`).
- **A probability is `random.chance(p)` around the step's line;** a run it skips is due again, as in the plan.
- **An on-respawn chat goes at most once every 30 seconds,** as a step mode keeps it (ADR-0020, S24): a flag in `state`, set with a timer of 30 s that clears it, and cleared at every join.
- **The calls:** `look.at`, `look.turn` (a maximum with decimals as `random.between(-305, 305) / 10.0`, so the maximum itself can come up), `keys.jump.tap()`, `keys.sneak.hold()` and `release()`, `mouse.right.tap()`, `mouse.right.hold()` and `release()`, `hotbar.select(slot + 1)`, `vehicle.mount("minecart")` for `enter_minecart`, and `chat.send(key)`.
- **A swing and an attack are a left click,** which on an entity attacks and on anything else swings (ADR-0042, G4). So a converted swing clicks only while the crosshair is on no entity, and a converted attack only while it's on one: each does what its step said and never the other. The helpers `swing()` and `attack()` are written only when a step uses them.
- **A chat step becomes a declared message,** keyed by its step (`join_1`), its braces doubled so it's sent as written. The script names it by its key: the text is never in the script, and below the right to edit the mode the message shows as its `/` and name, as the step did (ADR-0044, E6). A step whose command's name holds a brace, or whose text holds an invisible character, is no declaration (ADR-0036, D8): `ConvertError::Message` names the step.
- **The settings are the defaults:** a leash of 8 blocks (a converted step walks nowhere), a tool guard of 2 (a tap meets no guard), no blocks listed.

### C2. What a script does differently is a note at its step
| Note | When |
|---|---|
| `SwingSkipsEntities` | Every swing (C1) |
| `Slower` | A swing more often than a left click's 500 ms, a use more often than a timer's 250 ms: the interval is raised to it |
| `HoldRepeats` | Holding the use button: a held right button uses an item that isn't used over time again every 4 ticks (ADR-0042, G5) |
| `MountSkipsOccupied` | `enter_minecart`: the mount passes over an occupied minecart, the stopgap's known limit |
| `GapCapped` | An interval and its jitter past 24 hours, a timer's longest: the jitter is cut |
| `SharesLimit` | Two steps of one kind of action whose runs can meet under one limit below the script: both at start or both on respawn (the later one is refused), or two repeating ones, or a repeating one and one on respawn. A step at start and a repeating one never meet: the first run is a whole interval after the join |

**What every script does differently, whatever its steps,** isn't a note but the reference's: the runtime lets go of everything at a death, a respawn and a world change, where a step mode lets go at a respawn only when it has steps for it; and a timer waits while the bot is dead and runs right after the respawn, where a step mode's run while dead did nothing.

### C3. Tested against the plan it replaces, schedule for schedule
`tests/suite/convert.rs` runs the step mode's own `ModePlan` and the converted script's `Instance` over the same session on the suite's fake world: a join, the repeating runs, and deaths and respawns between two of them. The plan's actions are turned into what a script does for each (a swing off an entity a left tap, an attack on one, a look a look, a chat its text) and put through what a script's inputs do anyway: the limiter below a script, and a held key or button that a hold doesn't press again, which the runtime lets go of at a join, a death and a respawn. The script must then have done exactly that, at the same milliseconds and in the same order.

- **Every kind of step on every schedule:** the eleven actions, sneaking and the use button both ways, chat both plain and a command: at start, repeating at the step's own shortest interval (raised where C2 says), and on respawn over three deaths, two of them inside an on-respawn chat's 30 s; both with the crosshair on an entity and on nothing.
- **A mode of every kind at once,** over three deaths.
- **Any step mode:** a property test over modes of up to ten steps of every kind on every schedule; 40 runs of its 64 cases each, 2,560 modes, before it was trusted.
- **Randomness by its spread:** a jittered gap stays in its range and spreads over it, a probability runs as often as it says (1,000 joins, 300 runs), a turn stays within its maxima and reaches the decimals.
- **The text itself:** a whole converted script is held word for word, so the reviewer reads what a user reads.
- **Found by it:** a repeating hold pressed again at every run in the plan, and once in a script, since a held button can't be pressed again (vanilla's rule, and fleet-script's); the comparison now models the script's own inputs. And the harness rounded a jittered due time down to the millisecond, after which the plan's tick found nothing due and the loop never ended: it ticks at the exact moment now.
- **Red first:** every test failed against a stub that wrote an empty script (30 of 34; the three that passed are an empty mode, a step without notes and `farm` without notes).

### C4. The app converts on its own Rust side
**`convert_to_script`**, the desktop crate's command, reads a step mode's steps as the server reads them (`ModeSteps`: a broken rule is invalid input at its field), converts them and answers the script as the editor reads a mode its viewer may edit (`ModeScript::of(script, true)`), with the notes, or a `problem` at its field (`steps[1].action.message`, or `steps` for too many timers). It's local and microseconds of text: no network, nothing stored, nothing logged. A chat step can hold a password, so the UI calls it directly, never through a cached query (frontend.md's list).

- **In the editor:** "Convert to script" under a step mode's name, for whoever writes scripts. The script fills the form exactly as a loaded scripted mode does; a notice lists each note, with each step named as the editor names it, and what every script does differently. "Keep the steps" puts the form back as it was; Save sends the script. A step that keeps the steps from being a script shows its text at its field.
- **Every place frontend.md lists for a new command,** with the IPC contract's entry run by the ACL test.
- **The e2e fake answers its story's steps with the real converter's answer** (`apps/desktop/e2e/fake/converted.json`), which a test of the desktop crate holds equal to the command's own; other steps get a problem at `steps`. The fake writes no Rhai of its own.

### C5. The stopgap goes in the next release
- **A new mode with `enter_minecart` is a 422** at the step's action, `steps[i].action.type`, with a text that names the way out: "is going away in the next release: a scripted mode gets in with vehicle.mount("minecart"), and Convert to script turns a step mode into one". **So is an edit that brings it into a mode that had none.** A mode that has it keeps it through an edit, is still set on bots and keeps running; agents and the wire are unchanged.
- **The app offers it only on a step that has it,** marked "(going away)" with a hint that points at the conversion.
- **The next release's first task,** as ADR-0034 set out: the server names every mode that still uses it and refuses to start until none does; the wire's case 11 is reserved; fleet-mc's code goes.

### C6. A scripted mode stays a script
An app of the last release reads a scripted mode as a mode without steps (it never knew `script`), and an Admin who saved it there would have replaced the script with nothing. No editor of this release turns a script into steps: a mode's kind is set when it's made, and the conversion goes the other way. So **the server refuses an edit that turns a scripted mode into steps** (422 at `steps`, "can't replace a scripted mode's script: an app older than the server shows the mode without it; update the app"), and the runbook's update order puts the app third, before anyone writes scripts.

### C7. The reference's tables are generated
`docs/scripting.md` marks each table that comes from the description (`<!-- generated table: keys -->` to `<!-- end of generated table -->`): each object's members, the input's, the hooks and the reasons. `fleet_script::with_tables` writes them anew from `API` and `reason_meaning` (the reasons' meanings, now in the description beside their names), leaving every other line; `just gen` writes the reference with it (fleet-script's `reference` example), `just gen-check` fails while it differs, and the suite's `the_references_tables_are_the_descriptions` holds it in `just check`. The reference is out of draft: the prose stays around each table, and a new section, "Converting a step mode", is the one each converted script's header points to.

### C8. The phase's check on a real server
fleet-integration's `slow_scripts`: the real server and the real managed agent on fleet-mc's azalea connector, against the pinned offline test server, each case a container of its own. The reference's examples are set through the API as the editor's templates set them, as written.

| Case | What the server shows |
|---|---|
| The AFK farm | The join's look (90°, 10°); an iron golem without AI two blocks west takes five hits whose four gaps take 4.3 to 6.5 s by the bot's damage statistic (the timer's 1.2 s make 4.8); after a death, the look again |
| The minecart farm | Seated in the free minecart, past a nearer one a pig sits in; again after a death; out of the seat on the Nether's roof, and in again back beside it |
| The cobblestone generator | A real generator, built once the bot keeps its chunks loaded: nine cobblestone mined with the nine pickaxes of 3 uses, one block each before the tool guard; the wall behind never; then the script's own "No pickaxe left." |
| A player's chat, whisper and `/me` (P15.36) | Each the reply's exact text, `Balance: $50,000`, put before the bot within 2 s of a window's opening, in the bot's history: no match, no `/pay` |
| A death and a join (P15.36) | The second player slain by an armour stand's stick renamed to the reply, and a player joining as `Balance_500`, within 4 s: no match, no `/pay` |
| The server's own line (P15.36) | `tellraw` of the reply outside a window: no match; inside the next one: the match, and `/pay AfkMain 40000.00`, what's above 10,000 |

- **Each spoof is put before the bot inside a window of the script's own,** at a join the test causes: the first window opens while the bots come online, so the rounds start at the second, after a kick, which the bot answers by joining again (an operator's kick is transient). A line's time is proved by when it was put, against the runner's own line for the window's opening, on a server that relays a line at once, in a window of 5 s; the bot's history then holds it.
- **Repeated before they were trusted:** see "What was measured".
- **An agent of the last release:** `slow_smoke_an_agent_of_the_last_release_is_sent_no_script_and_refuses_nothing`, on the chaos world's stack with its second agent the image v0.1.3 published, pinned by its index digest. This release's agent is stopped, disabled and released, so the three bots go to the old one and run there; a scripted mode can't be set on one of them (409 `agent_too_old`), a new bot on it waits for a newer agent and never reaches the old one, the old agent's stream never ends and its log holds no warning or error. The old agent reads this release's agent config and compose file unchanged: neither changed since v0.1.3.

### C5, as built: the removal (Phase 16, group A, 2026-11-25)
The release after 0.2.1 removes the stopgap as C5 and ADR-0034 planned, the tag v0.2.1 checked first:
- **fleet-core** has no `enter_minecart` any more: such a step's JSON is an unknown action. A script's `vehicle.mount` keeps the machinery (`riding/enter.rs`), now always past a minecart someone sits in; its unit tests and the live `slow_mount_scenario` (once `slow_enter_minecart_scenario`) run on the mount.
- **The wire's case 11 is reserved** (`reserved 11; reserved "enter_minecart";`): an older server's step with it reads as an action that isn't set, and the whole spec is refused.
- **`serve` refuses to start while a stored mode holds it,** right after the database opens and before anything reads a mode: "the server can't start" with `N stored modes still hold enter_minecart, which this release removed: "<name>" (<id>), …; convert each to a script on the release before this one, then update again`. fleet-core can't read such a mode any more, so the server finds them in the stored JSON with SQLite's own `json_each` (`ModeReads::with_retired_action`); a scripted mode has no steps and is never one. This release adds no migration, so going back to convert is the release before, unchanged.
- **The API's kind and the app's entry go:** a request with it is a body of the wrong shape (400). The converter loses its `MountSkipsOccupied` note, which only that step had.
- **The way back, as 0.3.0 ships it** (its version PR, 2026-11-28): "the release before, unchanged" is v0.2.1's two images named by digest in `.env`, which the bundle's image lines take over its own pins (ADR-0022), over 0.3.0's compose files, which are 0.2.1's. No restore: no release since 0.1.0 has added a migration, and a restore would run 0.3.0's images again. `fetch` only moves forward, so its refusal of an older release now points at the runbook's "Going back" instead of a restore. The step exists from 0.1.3 on and only 0.2.1 converts it, so a host on 0.1.3 with such a mode goes through 0.2.1.

## What was measured
On this PC (Windows 11, Docker Desktop), 2026-11-22, against the pinned test server (26.1):

| Case | Passed | Took, each |
|---|---|---|
| The AFK farm | 6 of 6 | 23.2 to 23.9 s |
| The minecart farm | 6 of 6 | 18.6 to 19.1 s |
| The cobblestone generator | 6 of 6 | 47.0 to 50.1 s |
| A player's chat, whisper and `/me` | 6 of 6 | 85.4 to 85.6 s |
| A death and a join | 6 of 6 | 51.4 to 51.9 s |
| The server's own line | 6 of 6 | 29.6 to 29.8 s |
| An agent of the last release | 6 of 6 | 28.0 to 30.6 s |

- **The runs:** once in the whole `just test-slow` (26 of 26 passed), then five times in a row with nextest's `--stress-count 5` (8 of 8 each time, 1,433 s). The cases stay well inside the profile's 10 minutes; the longest, a player's three lines, puts each in a window of its own.
- **The converter, against the plan:** the property over any step mode passed 40 runs of 64 cases before it was trusted (2,560 modes), and the suite's 35 tests of it pass at every `just check`.
- **Before the fixes the runs found:** the generator was built before the bot kept its chunks loaded, and its blocks never formed; reading the bot's history at every wake hit its rate limit (429); and the history's own time of a line came seconds late, so a spoof's time is proved by when it was put.

## Deviations from the plan
- **P15.27 says "any step mode".** A mode that repeats more than 16 steps on schedules of their own isn't converted: a script has 16 timers (C1). The editor says so at the steps, with what to change.
- **The converted script names a step by the editor's section and place,** not by its place in the stored list (C1).
- **C6 is new:** the update order showed that an older app could empty a scripted mode, so an edit can't turn a script into steps.

## Known limits
- **What a script does differently** (C2) is shown when converting and kept in the reference; the script's own lines say only which step they do.
- **A converted swing at an entity doesn't swing at all,** where the step swung: a left click would hit it.
- **The stopgap stays one release more,** with its known limits (ADR-0034).

## Alternatives considered
- **The converter on the server, behind a route.** A new route, its rate limit, audit and matrix rows, for a computation that needs nothing of the server's and that the server checks again at save anyway. Rejected for the app's own command.
- **The converter in the UI, in TypeScript.** A second writer of Rhai, kept equal to nothing. Rejected.
- **One timer for every repeating step of one interval.** Fewer timers, but steps due at one moment could run out of their order. Rejected for neighbours only.
- **A converted swing as a plain left click.** It would hit a player or a villager in front of the bot, which the step never did. Rejected.
- **Refusing every save of a mode with `enter_minecart`.** It would block a friend from changing an interval before they convert. Rejected: a mode that has it keeps it until the next release.
- **The reference's tables hand-written, with a test that they name every member.** The summaries would still drift from the editor's hover. Rejected for one source.
- **Proving a spoof's time by the server's receipt of the line.** The ingest's batches make it seconds late. Rejected for the time the spoof was put, against the window's own line.
