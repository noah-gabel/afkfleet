# 0036. Scripted modes: a virtual vanilla client in Rhai, and menus after it

- **Status:** Accepted. The user approved the plan and answered its eight questions on 2026-11-16 ("The user's answers", at the end); the decisions they changed are marked. Reading chat is [ADR-0037](0037-scripts-read-system-chat.md). Amended 2026-11-25 with the user's additions for Phase 16 (M11 to M15)
- **Date:** 2026-11-16
- **Related:** Plan.md Phases 15 and 16, §10; [the script reference](../scripting.md) (draft); [ADR-0037](0037-scripts-read-system-chat.md) (scripts read system chat); ADR-0008 (one host thread per bot), ADR-0010 (the mode model and its limits), ADR-0011 (`UseItem`, `HoldUse`), ADR-0013 (the mode runner), ADR-0017 (the app's CSP, no runtime styles), ADR-0019 (a mode on the wire), ADR-0020 (S20 to S27: respawn; S80: chat formats), ADR-0025, ADR-0026 (riding), ADR-0030 (fuzz targets), ADR-0032 (the load test), ADR-0034 (the stopgap this replaces); `spikes/scripting/`

## Context
A mode is a list of steps today: an action, a schedule, a probability. It can't react: it doesn't know whether the bot sits in its minecart, which world it's in or what it holds. ADR-0034 added one stopgap action for one such case and said scripted modes would replace it.

**What the user asked for,** in three rounds at the plan's review:
- Modes written as scripts in Rhai, sandboxed, validated at save and again on the agent, with an editor in the app.
- **The API is a virtual vanilla client:** every vanilla keybinding as an input, the mouse, looking, the screens and facts, each behaving as in vanilla.
- **Its first goal is to be clean and easy:** few concepts, the same method names everywhere, every action returning success or a reason, errors with line numbers, a complete example for each kind of mode the user runs.
- Hooks for joins, world changes without a disconnect, deaths, respawns and timers; state the author controls, kept across reconnects.
- Mining by holding the left button, with guards.
- Every held input released by the runtime whenever a script stops; today's rate limits enforced below the script; a safety net for walking.
- Chat never triggers an action. *(Changed by the user's answer 2: a script reads system lines by declared patterns, [ADR-0037](0037-scripts-read-system-chat.md).)*
- Engine-neutral: the API's types and limits in fleet-core, fleet-mc only translating.
- **A second phase in the same plan:** inventory and menus, for a sell macro and an auction scan on any server, with clicks that can't land on the wrong thing and never buy twice.

**Decided under ADR-0025.** Every decision below is mine unless it says it's the user's. The eight questions at the end were the user's; their answers are there, and each decision an answer or their reviewer's notes changed says so.

## What was measured
`spikes/scripting` holds the code; `cargo run --release -- all` prints every line below, and `spikes/scripting/linux.sh all` the same in `rust:1-bookworm`. rhai 1.27.0 with `sync`, `no_module`, `no_time`, `no_custom_syntax`; one thread; a release build; the development PC (Windows 11) and a Linux container on it with 4 CPUs.

### Numbers
| | Windows | Linux |
|---|---|---|
| A loop of 1,000,000 additions (6,000,009 operations) | 118 ms, 50.8 million operations a second | 121 ms, 49.5 million |
| Compiling a script of 16,803 bytes (95 functions) | 574 µs | 465 µs |
| An empty hook | 96 ns | 60 ns |
| The minecart farm's swing (42 operations, five API calls) | 2.8 µs | 2.2 µs |
| Scanning a 54-slot auction page with a native number helper, picking the cheapest | 53 µs, 1,437 operations | 47 µs, 1,437 |
| The same scan with Rhai's own string functions | 235 µs, 5,271 operations | 224 µs, 5,271 |
| A busy loop, ended by a budget of 5,000 operations | 102 µs | 119 µs |

### Both time models at 200 bots, 10 seconds
Each bot runs the minecart farm's swing every 250 ms.

| | Hooks that run to their end, one thread | A thread a bot with a blocking `wait()` |
|---|---|---|
| Threads | +0 | +200 |
| Memory, Windows | +1.9 MiB | +6.5 MiB |
| Memory, Linux | below what the reading shows | +3.0 to +4.4 MiB |
| Virtual memory, Linux | +0 | +365 MiB in one run and +4,435 MiB in the other |
| CPU idle | none measured | none measured |
| CPU at a hook every 250 ms a bot (8,000 hooks) | 26 ms in hooks on Windows, 19 ms on Linux: about 0.3 % of a core | 110 ms on Windows, 220 ms on Linux: 1.1 % and 2.2 % of a core |
| A busy loop stopped | by the budget, after 102 µs | by a flag the progress hook reads, 66 µs after it's set |
| All 200 stopped | nothing to stop | 9 ms on Windows, 18 ms on Linux |

**Read honestly:** the thread model is affordable at 200 bots. Its memory and CPU are small next to the 200 host threads the bots already have (ADR-0008). The numbers favour hooks by a factor of four to seven in CPU, but they don't rule threads out. The decision below rests on three other things.

### Facts about Rhai 1.27.0
Each line is the spike's output; the text after the bar is Rhai's own.

| Checked | Result |
|---|---|
| A game object inside a hook, through a variable resolver | Works |
| A game object inside the script's own function, through a variable resolver | Works |
| The same through constants in the call's scope | `Variable not found: keys (line 1, position 38) in call to function 'walk'`: Rhai's functions don't see the scope |
| State as a custom type with an indexer, from the resolver | `Cannot assign to indexer of constant (line 1, position 21)`: what a resolver returns is a constant |
| State as a **shared** object map, from the resolver | `state.n = 1`, `state.n += 1` and `state["n"] += 1` work in hooks and in the script's own functions, and the value is kept from one hook to the next |
| State as `this` | Works in the hook and in a helper called as `this.more()`; a helper called plainly fails with `'this' not bound (line 1, position 63)` |
| `state = 5` with the shared map | Accepted: the state is then `5`. The runtime must check it |
| A state grown past `set_max_map_size(64)` through the shared map | Accepted: 100 entries. Rhai's size limit doesn't hold there, so the runtime must check it |
| `keys = 1` | `Cannot modify constant keys (line 1, position 16)` |
| `let keys = 1` with a definition filter | `Forbidden variable name: keys (line 1, position 20)`, at compile time |
| A parameter named `keys` | Accepted, and the resolver's object wins over the argument. Validation must refuse it |
| Strict variables, compiled against placeholder constants, run with the resolver | Works: the actions reach the call's own turn, none reach the placeholder |
| A misspelled object with strict variables | `Undefined variable: kees (line 2, position 3)`, at compile time |
| A misspelled method | Compiles. At run time: `Function not found: hodl (input) (line 2, position 16)` |
| `import` | `'import' is a reserved keyword (line 2, position 3)`, at compile time |
| `eval`, as a disabled symbol | `reserved keyword 'eval' is disabled (line 2, position 3)`, at compile time |
| A syntax error | `Unexpected ';' (line 3, position 11)` |
| A busy loop, also inside `try`/`catch` | `Too many operations`, without a position; the `catch` doesn't run |
| Endless recursion at 16 call levels | `Stack overflow`, without a position |
| A text and an array that grow | `Length of string too large`, `Size of array/BLOB too large` |
| A result ignored, and one tested with `.ok` | Both work |
| The proposed object names | Rhai keeps none of them for itself |

Two more facts from the build:
- **Rhai marks `Engine::on_var` as volatile:** "This API is NOT deprecated, but it is considered volatile and may change in the future."
- **`cargo deny` with this repository's rules fails on Rhai's graph twice:** `tiny-keccak` 2.0.2 is licensed CC0-1.0, which the allowlist doesn't hold (it comes in through `ahash` → `const-random` → `const-random-macro`, a proc macro, so it runs at build time only); and `smartstring` 1.0.1 is unmaintained (RUSTSEC-2026-0249, its repository archived on 2026-05-03). Rhai's `compact_str` feature adds `compact_str` and keeps `smartstring` in the graph.

### CodeMirror 6 under the app's CSP
One page, served with the exact `csp` string of `tauri.conf.json` as its header, opened in the installed Edge 157 with Playwright. `codemirror` 6.0.2, `@codemirror/view` 6.43.15, `state` 6.7.6, `language` 6.13.1, `autocomplete` 6.20.4, `lint` 6.9.8, `commands` 6.11.1, `style-mod` 4.1.4. The editor has line numbers, a small Rhai tokenizer through `StreamLanguage`, highlighting, a theme that reads CSS variables, a completion list opened by the test, and a lint marker.

| | In the document | In a shadow root (`root: shadowRoot`) |
|---|---|---|
| CSP violations | One: `style-src-elem`, inline, from the bundle | **None** |
| The console | "Applying inline style violates the following Content Security Policy directive 'style-src 'self''" | Nothing |
| The editor's own layout (`display`) | `block`: unstyled | `flex`: styled |
| The theme's colour from a CSS variable | Not applied | Applied: custom properties cross the shadow boundary |
| Highlighting | One colour | Four colours |
| The completion list, the lint markers | Shown, unstyled | Shown, styled |
| `<style>` elements in the head | 1 (blocked) | 0 |
| `document.adoptedStyleSheets` | 0 | 0 |
| The shadow root's `adoptedStyleSheets` | | 1 |

**Why:** `style-mod` creates a `<style>` element when the root has a head, and a constructed stylesheet filled with `insertRule` when it's a shadow root (`if (!root.head && root.adoptedStyleSheets && win.CSSStyleSheet)`). Edge enforces `style-src` on the element and not on the constructed sheet. CodeMirror's own `style` attributes are set through the CSS object model, which the CSP allows, as ADR-0017 found for React.

The page was a throwaway and isn't committed: it would have brought npm packages into the repository before they're chosen. To repeat it: bundle a page that mounts `new EditorView({ parent, root })` once with `parent` in the document and once with `parent = host.attachShadow({ mode: "open" })` and `root` that shadow root; serve it with the header `Content-Security-Policy:` and the `csp` value of `apps/desktop/src-tauri/tauri.conf.json`; listen for `securitypolicyviolation` on the document; read `getComputedStyle(parent.querySelector(".cm-editor")).display`.

**Not checked yet:** the built app. Edge is WebView2's engine, and the e2e suite relies on that for every other page; group F starts the check exe with the editor before it's done, as the frontend rules ask for any new UI dependency.

## Decisions for Phase 15

### D1. How time passes: hooks run to their end; there is no `wait()`
A script defines hooks. The runtime calls one when its event happens, the hook runs to its end under an operation budget, and time passes between hooks. A sequence is written with `hold_for(ms)` and named timers.

**Why, with the numbers above not deciding it:**
1. **A waiting script is deaf.** With `wait()`, the bot's script sits in one call for as long as it waits. A death, a world change or a menu update that arrives meanwhile can't run a hook on the same state: it must wait for the script to return, or stop it from outside. With hooks, every event is handled when it happens.
2. **Tests.** A hook is a function of the event, the facts and the time: it's tested like `ModePlan`, with paused time and property tests, and it's fuzzed without threads. A thread that sleeps needs real time or a clock threaded through every wait, and the testing rules forbid a real sleep.
3. **One way to stop.** A hook ends by itself, bounded by its budget. A thread must be told to stop and joined, and a native call that doesn't return can't be interrupted.

**What it costs the author:** "walk three seconds, then jump" is `keys.forward.hold_for(3000)` and a timer, not three lines with a wait between them. The reference's examples show the pattern, and the user confirmed the model at the plan's review.

**What follows from it:**
- **A hook can't wait for an answer.** So it gets a snapshot of facts taken right before it runs, and its actions are applied after it returns. A script sees what its own action did at its next hook.
- **Hooks run on the bot's runner task,** as `ModePlan::tick` does. The worst case of one hook is its budget: 10,000 operations, 0.2 to 0.45 ms at the rates measured. *(Corrected by [ADR-0040](0040-fleet-script-as-built.md)'s amendment, A1: that holds for plain operations only; a hook is bounded in time too, at 5 ms.)* That's a bounded computation, not blocking IO; compiling runs in `spawn_blocking` on the server and the agent.

**Alternatives considered:**
- **A thread a bot with a blocking `wait()`.** Measured above; affordable, rejected for the three reasons.
- **A hybrid, hooks plus a `wait()` on a thread only for scripts that use it.** Two models to test and to explain, and the deafness stays for those scripts.
- **A fixed `on_tick` every game tick.** 4,000 hook calls a second at 200 bots for scripts that mostly have nothing to do. Timers of 250 ms and up, and events, cover it.
- **Another engine with coroutines** (Lua, where a `wait()` yields without a thread). Rhai is the user's requirement, and it's pure Rust with no `unsafe` of ours and no C build.

### D2. Where the code lives
| Crate | Holds |
|---|---|
| `fleet-core` | Engine-neutral types and rules: the script's source and settings, message templates, facts, inputs and the hold tracker, the action limiter, the leash, the mining and tool guards' decisions, hook events. No Rhai type |
| `fleet-script` (new) | Rhai. The sandboxed engine, validation, the instance that runs hooks, the API's objects, the API's description, the examples. Pure and synchronous: no tokio, no IO |
| `fleet-runtime` | `ScriptRunner`, beside `ModeRunner`: events, snapshots, timers, actions through the limiter, releases. The script's state belongs to the bot's actor |
| `fleet-mc` | Translation only: the new `GameAction` cases, the facts reader, the leash's and the guards' tick systems, the world-change event |
| `fleet-server`, `fleet-agent` | Use `fleet-script` to validate, at save and before a run |

- **Only `fleet-script` depends on `rhai`,** enforced by a `deny.toml` wrapper entry, as azalea-auth is (ADR-0018).
- **Why a crate of its own, not fleet-runtime:** the server validates at save and must not depend on the bot runtime; the fuzz targets want the engine alone.
- **Coverage gate:** 90 %, as fleet-core's neighbours.

### D3. The sandbox
- **rhai `=1.27.0`,** published 2026-10-21, 26 days before this plan; MIT OR Apache-2.0.
- **Features:** `sync` (the engine and a compiled script are shared between bots), `no_module` (no `import`, no module resolver in the build at all), `no_time` (no clock but the runtime's), `no_custom_syntax`. Never `unchecked`, which removes every limit, nor `internals`.
- **Closures stay on.** The plan's first draft had `no_closure`; the state object needs Rhai's shared values, which that feature removes (D5).
- **`eval`** is a disabled symbol; `print` and `debug` go nowhere.
- **Limits** (proposed; group D's load test confirms or changes the two budgets):

| | Limit | Why this number |
|---|---|---|
| Script size | 16 KiB | Compiles in about half a millisecond; the longest example is under 2 KiB |
| Operations a hook | 10,000 | Seven times the auction scan with the helper, twice the scan without it; at most about 0.45 ms *(of plain operations; ADR-0040's amendment, A1, adds 5 ms a hook and 20 ms a second)* |
| Operations a second | 30,000 | A scan every game tick for a second; 200 bots at this ceiling are 12 to 27 % of one core |
| Call depth | 16 | Rhai's native stack use grows with it |
| Expression depth | 32, and 16 inside functions *(32 there too: the user's answer, ADR-0040's amendment, A3)* | |
| Functions | 64 | |
| Text, array, map | 1,024 characters, 256 items, 64 entries | |
| State | 64 entries and 4 KiB, checked by the runtime **while a hook runs** (every 256 operations) and after it | Rhai's own limits don't hold through the shared map (measured); D5 says why after the hook is too late |
| Timers | 16, each 250 ms to 24 hours | Today's shortest and longest interval (ADR-0010); the user's case needs 10 hours |
| Log | 200 characters a line, 20 lines a minute | |

- **A script that fails or hits a limit stops.** Everything is released, the bot stays online and idle, and the error with its line reaches the app. It starts again at the next Start or when its mode is saved or changed. It isn't restarted at the next join: a script that fails at every join would otherwise fail forever.
- **A limit's error has no line** (measured), so the app names the hook that ran instead.

**The two exceptions Rhai needs in `deny.toml`,** to be made in group B's pull request and listed there under Weakened:
- A license exception for `tiny-keccak@2.0.2` (CC0-1.0, a public-domain dedication, build time only).
- An `ignore` for RUSTSEC-2026-0249 (`smartstring` is unmaintained), with the removal trigger "a Rhai release without smartstring". It's the type behind Rhai's short strings, so it runs inside the sandbox; the fuzz targets cover it, and it's the price of the Rhai requirement.

**Alternatives considered:**
- **`Engine::new_raw()` with packages picked by hand.** Fewer built-in functions to reason about. Left for group B to decide with the fuzzer's help; the standard library has no IO once `no_module` and `no_time` are on.
- **An interrupt through `on_progress`.** Needed only for a thread model. With hooks, the operation budget ends a busy loop by itself.

### D4. The API's shape: a virtual vanilla client, clean and easy
The reference draft is [docs/scripting.md](../scripting.md). Its rules:

- **Three concepts:** hooks, one `state` object, the game objects.
- **Inputs are named after their vanilla bindings and behave as in vanilla.** `keys` and `mouse` are the hand; `look` is the mouse's movement; `hotbar` is the number keys.
- **Four method names for every input:** `tap()`, `hold()`, `hold_for(ms)`, `release()`, and the property `held`.
- **Every action returns a result** with `ok` and `reason`, the reason from a closed list. A release is never refused.
- **Everything works in the script's own functions** as in a hook (measured), so a long script reads as ordinary functions.
- **Errors carry a line and a column** wherever Rhai gives one.

**How the objects reach a script:** a variable resolver (`Engine::on_var`) answers for the object names with handles built from the call's own context. It's the one mechanism the spike found that works inside a script's functions. Rhai calls that API volatile, so the pin is exact, and fleet-script's tests of "an object inside a helper function" fail at the first bump that changes it.

**The object names are reserved.** A definition filter refuses `let keys` at compile time, and validation refuses a parameter with such a name (measured: the resolver would silently win).

**Vanilla's inputs, and where each is:**

| Vanilla | API | Phase |
|---|---|---|
| W, A, S, D, Space, Shift, Ctrl | `keys.forward`, `back`, `left`, `right`, `jump`, `sneak`, `sprint` | 15 |
| Left button | `mouse.left`: an attack per click on an entity; held on a block, mining | 15 |
| Right button | `mouse.right`: use; held, a use every 4 ticks or one long use; `hold_until_used()` | 15 |
| Mouse movement | `look` | 15 |
| 1 to 9, the scroll wheel | `hotbar.select(n)` | 15 |
| Q, Ctrl+Q, F | `keys.drop`, `keys.drop_stack`, `keys.swap_offhand` | 15 |
| T and `/` | `chat.send(key, values)`: declared messages only (D8) | 15 |
| E, Esc | `inventory.open()`, `menu.close()` | 16 |
| Clicks in a screen | `menu.click` and its kinds | 16 |
| What the screen shows | `bot`, `world`; `inventory`, `menu` | 15; 16 |

**Left out, and why:**

| Vanilla | Why not |
|---|---|
| Pick block (middle button) | Does nothing useful outside creative mode |
| Dragging in a screen, double-click to collect | The same result with single clicks; two more packet shapes to get exactly right |
| The recipe book | Servers limit it separately (Paper's `recipe-spam-limit`); crafting works by clicks |
| Typing free chat, tab completion | No free text is ever built (D8) |
| Signs, books, command blocks | Text entry. Later, through templates |
| The beacon's and the horse's screens, dialogs | No use named; each needs packets of its own |
| The player list, advancements, the perspective, screenshots | Nothing of them reaches the server |
| Placing blocks, fishing with bite detection, steering boats and horses, elytra | The user's list for later (Plan.md §10) |

**Two things that work without being built:** a right-click with a block in hand places it, as in vanilla, and holding the right button at a classic AFK fishing farm works. Neither gets an API, a guard or a test of its own in these phases, and the reference says so.

**The examples are files.** Each of the five lives once in fleet-script, is compiled and run against the fake world in its tests, appears in the reference and is offered in the editor as a starter template. One description of the API, exported by fleet-script, feeds the reference's tables and the editor's autocomplete.

**Alternatives considered:**
- **Plain functions** (`hold_forward()`, `bot_health()`). No resolver needed. Rejected: dozens of names instead of a few objects with four methods, and no autocomplete after a dot.
- **An action that fails raises an error.** Every simple script would need `try`. Rejected for a result one can ignore.
- **Hotbar slots 0 to 8,** as a step mode's JSON has them. Rejected for the numbers on the keys; Phase 16's slot numbers are a screen's own and are documented as such.

### D5. State
- **`state` is a shared object map** the resolver hands out: readable and writable everywhere in the script (measured).
- **Empty at Start. Kept** across reconnects, world changes, deaths and respawns. **Gone** at Stop and when the bot's mode changes or is saved.
- **The runtime checks it while a hook runs and after it:** still a map, at most 64 entries and 4 KiB. Otherwise the script stops with that error.
  - **Why not only after the hook** (the user's reviewer, 2026-11-16): an assignment copies its value, so `state.a = state.b` doubles what the state holds, and one hook's 10,000 operations could grow it to fill the agent's 512 MiB and take every bot on that agent down before the check ran.
  - **So the size is checked in Rhai's progress callback,** every 256 operations, and the script stops there. Between two checks a script can add at most what 256 operations add, each bounded by Rhai's own limits for one text, array or map.
  - **Tested in group B:** a script that tries it never takes more than a few MiB, measured.
- **Reading a property that was never set is an error** (`set_fail_on_invalid_map_property`), so a typo shows with its line instead of reading as nothing.
- It lives in memory on the agent. An agent's restart is a Start for every bot's script.

**Alternatives considered:**
- **State as `this`,** Rhai's documented way. A helper function loses it unless it's called as `this.helper()` (measured). Rejected: it breaks "complex modes read as ordinary functions".
- **`state.get("n")` and `state.set("n", 1)`.** Works with constants, reads badly.
- **Stored on the server,** to survive an agent's restart. A write path from every bot for a counter; not asked.

### D6. Hooks
`on_join(first)`, `on_world_change()`, `on_death()`, `on_respawn()`, `on_timer(name)`, `on_stop()`, and `on_chat(key, values)` for a declared pattern's match ([ADR-0037](0037-scripts-read-system-chat.md)).

- **`on_reconnect` isn't a hook.** It's `on_join(false)`: one way to say each thing.
- **A world change** is a second login, or a respawn packet without a death, on a connection that stays open. Today fleet-mc reports neither. A proxy that sends a bot through a lobby produces the first, a portal the second.
- **Everything is released before `on_join`, `on_world_change`, `on_death` and `on_respawn`.** Each puts the bot somewhere new.
- **Timers** wait while the bot is dead or between worlds; a repeating one doesn't catch up. They're cleared at a disconnect, so `on_join` starts them.
- **`on_stop`** runs at an orderly stop only, with the same budget; it may change state and send one declared message. Its error is logged and changes nothing.
- **A function named `on_…` that isn't a hook,** or a hook with the wrong number of parameters, is refused at save.

### D7. Limits below the script
One `ActionLimiter` in fleet-core sits between a script's actions and the session. Each input and each action kind has its minimum, today's: 250 ms in general, 200 ms for a use, 500 ms for an attack and for holding the use button, 30 s for plain chat. A refused action returns `too_soon` and is counted in a metric. A release always passes, and doesn't count as a press.

**A declared command goes by the bot's own chat rate** (the user's answer 4): the limiter passes it, and the bot's chat queue paces it as it paces a command typed in the app (ADR-0020, S3). Plain chat keeps 30 s.

The limiter is per bot and outlives a hook, a reconnect and a mode change, as ADR-0020's 30 s chat memory does, so no sequence of events gets around a minimum.

### D8. Chat: declared messages with typed values
- **A mode declares its messages,** up to 16, each with a key: fixed text, or a template with values. A value is an integer in a range, a decimal in a range with a fixed number of places, or one of up to 32 literal texts.
- **The command's name is literal text,** never a value. So `ModeDefinition::commands()` lists the names as it does for steps, and the allowlist check for setting a mode on a bot is unchanged: Control when every command is on the allowlist, Manage otherwise.
- **The longest text a template can produce is checked at save** against chat's 256 characters; a rendered message goes through `ChatMessage`'s own validation again.
- **A script names a key and gives values.** A value out of range returns `bad_value`; an unknown key is an error with its line.
- **Nothing a script reads reaches chat as text.** A script can't put a position, a name or lore into a message. A number a pattern read from chat can fill a typed value, inside that value's range ([ADR-0037](0037-scripts-read-system-chat.md), C6).

**Alternatives considered:**
- **Fixed messages only** (the plan's first draft). `/ah sell <price>` would be impossible, the user's finding.
- **Free text built by the script.** Any text a server shows (a name, lore) could then be sent as a command by the bot. Rejected.

### D9. Releasing held inputs
- **`GameAction::ReleaseAll`:** every key up, both buttons up, mining stopped. Idempotent.
- **Sent at every exit:** Stop, a mode change or save, an error, a limit, a death, a world change, and when the runner's task ends for any other reason, a panic included (the actor owns the task and sends it).
- **A new session starts with nothing held;** fleet-mc's test pins it.
- **Not relied on alone for walking:** the leash (D10) runs on the host thread whatever the runtime does.

### D10. Walking's safety net
**The leash.** fleet-mc checks it every game tick, below script and runtime.
- **The anchor** is where the bot was put: its position at a join, a respawn, a world change, and after a server teleport of more than 8 blocks. Only fleet-mc sets it; no action moves it.
- **Beyond the radius** (horizontal distance), a tick that takes the bot further from the anchor releases every movement key and reports it. Walking back is allowed: the rule is about getting further, not about being outside.
- **A press is judged too** *(group A)*: beyond the radius, a direction key that would lead further, by where the bot looks, returns `leashed`; one that leads back or along the edge is taken. The plan had every key refused until the bot was inside again, which would have kept it from walking back.
- **It applies to a riding bot too,** since movement keys steer some vehicles.
- **The radius is a mode setting** next to the script, 1 to 64 blocks, 8 by default; no "off" (the user's answer 5).

**What else can go wrong, and what covers it:**

| Risk | Covered by |
|---|---|
| Walking off an edge: fall damage, death, the void | The leash doesn't. A script can hold `keys.sneak`, which stops at edges in vanilla; group C checks live that azalea's physics does the same. `on_death` and `on_respawn` exist |
| Lava, fire, cactus, drowning | Nothing below the script. `world.block` lets a script look; `bot.health` lets it react |
| Water currents, pistons, minecarts carrying the bot away | The leash stops its own walking further; it can't hold the bot in place |
| Portals | A world change releases everything and sets a new anchor; a script that walks back in loops through `on_world_change`, bounded by the limiter |
| Pressure plates, tripwires, trampled farmland | Nothing. The leash's default radius keeps it small |
| Hunger from sprinting and jumping | Nothing: no auto-eat. `bot.food` is a fact, and `hold_until_used()` eats |
| Mobs | Nothing new |
| Unloaded chunks | The leash's 64 blocks at most stay well inside a server's view distance |
| Anti-cheat | azalea's physics imitates vanilla's; ice, slime, ladders, bubble columns and knockback are the known places for differences. Inputs go through azalea's own key states, and rates stay under today's limits. Whether a server's anti-cheat accepts a bot is the user's to judge per server |

**Alternatives considered:**
- **azalea's pathfinder** (`goto`). It decides the route, can dig and place by its own rules, and is a much larger thing to bound. The user asked for keys.
- **A leash in the runtime.** A runtime bug or a stalled task would then let a bot walk on. Rejected for the host thread.
- **A script that may move the anchor.** Then the leash holds nothing.

### D11. Mining is in Phase 15
The user's decision that it's in; mine that it fits this phase, in group C.

- **Why here:** left-hold is one more input with the same hold and release machinery; its guards are per-tick checks in fleet-mc beside the leash; the tool's durability comes with the held-item fact; a cobblestone generator needs nothing of Phase 16.
- **azalea 0.16.0 has hold-to-mine** (docs.rs): `Client::left_click_mine(enabled)`, "When enabled, the bot will mine any block that it is looking at if it is reachable", with `is_mining()` and `start_mining(position)`.
- **As vanilla:** held on a block, it mines block after block. An entity under the crosshair when the button goes down is attacked once, never again during the hold.
- **Guard 1, the floor:** never a block that reaches below the bot's feet: only a block whose bottom is at the feet's height or above is mined. That includes the one it stands on and everything under it. A hold that meets such a block pauses; a new hold on one returns `mining_guard`.
  - *Sharpened in group A:* the plan's wording was "whose top is at or below the bot's feet". A bot on a slab, on soul sand or on farmland has its feet inside that block's cube, below its top, and would have mined the block it stands on. The price: the blocks beside the bot at that height stay too, and so does everything at feet level while the bot is in the air or on a carpet.
- **Guard 2, the tool:** when the held item has a durability and only N uses are left, mining stops and the hold ends; a new hold returns `tool_guard`. N is a mode setting, 2 by default, 0 to turn it off (the user's answer 6: on, at 2 uses left). `bot.held.durability` is the fact.
- **Both guards are decisions in fleet-core,** applied by fleet-mc each tick before azalea's mining system runs.
- **Known limits to state after the live tests:** azalea predicts the broken block itself, so a break the server refuses (spawn protection, a claim) must be seen to resync; the guards say nothing about blocks beside or above the bot.

### D12. Facts
A snapshot fleet-mc builds on the host thread right before a hook:
- **The bot:** health, food, position, dimension, whether it rides and what, the held item (ID, count, durability), what the crosshair is on.
- **Entities:** at most 64 within 16 blocks, nearest first: kind, position, whether a vehicle has a passenger.
- **Blocks:** the IDs within 4 blocks of the bot.

**Passengers are new.** azalea doesn't track other entities' passengers (ADR-0026); fleet-mc reads `SetPassengers` into an index, which ADR-0034 left out on purpose. `vehicle.mount` then takes the nearest free minecart, by the click on its ID and with the patience ADR-0034 measured as necessary.

**Everything a server controls arrives as a registry identifier or a number,** never as free text, in this phase. Custom names and lore come in Phase 16, sanitized.

**The bot's-eyes panel isn't in these phases.** The fact types and the reader are the part it shares; the panel needs a wire message, live events, a screen and a decision on who may see a bot's position. A phase of its own, after these two (the user's answer 7).

### D13. A scripted mode in storage and on the wire
- **A mode's body is steps or a script.** In JSON a scripted mode has `script` (the source and its settings: leash, tool guard, messages) and no `steps`, so a server older than Phase 15 can't read it, as with every new step type (ADR-0010). The runbook's "Going back" covers it.
- **On the wire** `ModeDefinition` gains a `script` field. An agent before Phase 15 would skip it and read a mode with no steps, a mode that does nothing, which is exactly the silent failure to avoid.
- **So `Hello` gains `script_api`,** the API level an agent runs: absent or 0 before Phase 15, 1 for Phase 15, 2 for Phase 16. **The server never sends a scripted mode to an agent below the level the mode needs.** Setting such a mode on a bot whose agent is too old answers 409 with its own code, a bot isn't placed on such an agent, and the bot's page says its agent must be updated.
- **A mode records the level it was validated at;** Phase 16's objects make it level 2.
- **The agent validates again** with its own engine and limits, and refuses per bot (`ModeRefused` with the error and its line), never by ending its stream as ADR-0034's case does.

### D14. Validation
At save, in `spawn_blocking`, the server:
1. checks size and characters;
2. compiles with strict variables against placeholder objects (syntax, undefined variables, `import`, `eval`);
3. reads the functions (`AST::iter_functions`): hook names and parameter counts, no parameter named like an object;
4. checks the settings and every message template.

The answer is 422 with line, column and Rhai's message, shown at the line in the editor.

**What validation can't find:** a misspelled method, and anything that depends on a value. Rhai resolves methods when the line runs. The script then stops with that line, the bot's page shows it, and the examples are tested so the starter templates never do.

**Top-level statements** are never run (hooks are called with `eval_ast(false)`). Rhai 1.27.0's public API has no direct way to tell whether a script has any; group B refuses them at save if it finds one, and otherwise the reference says a script is functions only.

### D15. Who may write a scripted mode
**The user's decision** (answer 1): Admins and the Owner create and edit scripted modes; a Member runs a shared one on bots they control. It's stricter than step modes (any Member writes private ones) because a sandbox is new code that hostile input reaches, and because a script moves a bot. A server setting can widen it later without a migration.

### D16. The editor
**CodeMirror 6, mounted in a shadow root,** with the CSP unchanged (measured above).
- **Why CodeMirror:** line numbers, error markers at a line, highlighting and above all autocomplete, which the user asked for.
- **What it needs:** the packages named in the measurement, each pinned at a version at least 7 days old; a Rhai tokenizer of our own through `StreamLanguage` (no Rhai language package is needed); a theme made only of the app's colour tokens.
- **What changes in the rules:** `.claude/rules/frontend.md` says a constructed stylesheet is blocked. Measured, Edge blocks the `<style>` element and not a constructed sheet inside a shadow root. The rule gets that one exception by name, and the style guards keep refusing everything in the document (`document.adoptedStyleSheets` stays 0). The CSP itself isn't touched.
- **Open for group F:** the focus outline (the app's one rule doesn't reach into a shadow root, so the host element shows it).
- **Verified in the built app, not only in Edge** (the user's reviewer, 2026-11-16): group F runs `just app-build-check` and starts `target/app-check/afkfleet-check.exe` with the editor open before the group is done. If WebView2 there disagrees with Edge, the textarea below is the fallback.

**Alternatives considered:**
- **CodeMirror in the document with a nonce.** The app's CSP is a fixed string, so a nonce would be a public constant that any injected `<style>` could carry. Rejected.
- **A textarea with line numbers and the server's markers.** No dependency and no CSP question, and the fallback if the check exe disagrees with Edge. Without highlighting, and autocomplete would be a list of our own to build.
- **Monaco.** Megabytes, workers and injected styles.

### D17. The stopgap
- **Phase 15 adds "Convert to script"** for any step mode: every step becomes a timer or a hook line, `enter_minecart` becomes `vehicle.mount("minecart")`, a chat step becomes a declared message. The converter is tested against the plan it replaces, schedule for schedule.
- **`enter_minecart` is marked as going away:** a mode that holds it keeps running, saving a new one with it is refused with a text that points at the conversion.
- **Removed one release later** (the user's answer 8), as ADR-0034 set out: the server refuses to start while a stored mode still uses it, naming each; the wire's case 11 is reserved.

### D18. Tests
- **Property tests** on the instance: a hook never exceeds its budget; no sequence of events makes two actions of a kind closer than its minimum; after any stop, nothing is held; state survives exactly what D5 says.
- **Fuzz targets:** `script_compile` (any bytes: no panic, a bounded time, an error position inside the source), `script_run` (a script and a list of events against a fake world: the properties above), `script_api` (any arguments to every API function), `message_template`.
- **Live,** on the test server: walking and the leash, a hold and its release, sneaking at an edge, mounting past an occupied minecart, the cobblestone generator with both guards, a dimension change as the world change, the right-hold's cadence in ticks. Each new scenario repeated, as ADR-0034's were.
- **The load test** (ADR-0032) with 200 scripted bots.
- **A proxy** with a lobby isn't in the test stack. The world change is tested with a second login injected into fleet-mc's app, and live with a dimension change. Whether a proxy joins the slow suite is group C's to say after that.

## Decisions for Phase 16: inventory and menus
Phase 16 adds to Phase 15 and changes none of it. What Phase 15 leaves open for it: the snapshot, the events and the limiter's classes take new parts; the item fact grows a name and lore; templates, the script's log and its way into the app exist; a screen the server opens is closed at once in Phase 15 and handed to the script from API level 2.

### M1. Slots, with text from the server
A slot is an item's ID, count, durability, custom name and lore lines. **Name and lore are the first free text from a server that a script reads.** They go through the chat sanitizer in fleet-core, as plain text, bounded: a name to 64 characters, lore to 16 lines of 128.

### M2. The menu snapshot
The open screen's kind, size, title, slots, the bot's own slots as the screen shows them, the cursor's item, **the window ID and the state ID.** azalea 0.16.0 keeps them in its `Inventory` component (docs.rs): `id`, "The ID of the container that's currently open", and `state_id`, "An identifier used by the server to track client inventory desyncs", which "only changes when the server sends a new container update".

**A new screen is a new screen.** `on_menu_open` fires for every screen the server opens, whatever its title; the window ID tells them apart.

### M3. A click is sent only if the screen is still the one the script saw
A click carries what the hook saw: the window ID, the state ID, the slot and that slot's item. fleet-mc compares on the host thread at the moment of sending:

1. the same window ID, or the answer is `screen_changed`;
2. the same state ID, or `content_changed`;
3. the same item in the slot, or `slot_changed`. **The same item is the whole item** (the user's reviewer, 2026-11-16): its ID, count, custom name, lore and data components, not the ID alone. Two listings of one kind of item differ only in name, lore and components, and a click meant for one must not land on the other.

Only then one `ServerboundContainerClick` goes out, with azalea's own prediction of the result. The comparison is a pure function in fleet-core with property tests; fleet-mc only feeds it.

### M4. One click at a time, and what "answered" means
**A screen takes no click while an earlier one on it is unanswered.** That's what keeps one listing from being bought twice.

As far as the protocol's documentation says, a click has had no acknowledgement of its own since Minecraft 1.17: a server answers only where its result differs from what the client predicted, and a plugin that cancels a click on a button sends the slot back. So "answered" can't be assumed, and none of this is taken on trust:

- **Group A measures first:** what follows a click on a vanilla chest, on the bot's own inventory and on a plugin's button, in packets and in ticks.
- **Proposed until then:** a click is answered by an update of that window with a newer state ID, by the window's close, or by its replacement. Without one within a bound, the script gets `unanswered`, and the screen takes no further click until it's opened anew.
- **A rejection** (the server put things back) reaches the script as `rejected`.

Every click that wasn't sent, was rejected or stayed unanswered is a line in the script's log and in the app.

### M5. The click limit
A class of its own in the limiter, **sized from measurements in group A:** how fast a vanilla client clicks when a person clicks fast and shift-clicks a stack, and what a server accepts. One fact so far, from Paper's documentation: its packet limiter kicks above 500 packets a second over 7 seconds, and it has no limit for clicks alone; anti-cheat plugins have their own.

A vanilla client's own rate can't be read off a bot. The harness's plugin logs every click's time, and the user can join the local test server with their own client and click: a step only they run, and optional.

### M6. Number helpers
`slot.lore_number(label)` and `number(text)` are native functions in fleet-script over a parser in fleet-core: thousands separators, decimals, the suffixes k, M and B. It's the parser Phase 15 builds for a chat pattern's amounts ([ADR-0037](0037-scripts-read-system-chat.md), C3). Measured: the scan with the helper costs 1,437 operations, without it 5,271. The parser is fuzzed.

### M7. Special screens
- **Villager trades:** `menu.select_trade(n)`, with the offers as facts.
- **The anvil:** `menu.rename(key, values)`, the text from a declared template (D8).
- **Buttons:** `menu.button(n)` is the enchanting table's choice, and with the same packet the stonecutter's and the loom's.
- Crafting tables, furnaces and chests are ordinary menus.

### M8. While a screen is open
Every key and mouse button is released, as a vanilla client's are, and the limiter answers `screen_open` for them. It also keeps anti-cheat's checks for moving with an open inventory quiet.

### M9. The harness
A Paper server in the slow suite with a small test plugin, built in Docker: an auction menu whose listings carry prices in their lore, a sell menu, and a second player who can buy a listing first. **Java is a new language in the repository,** test-only, never shipped. **Pinned and built like the rest of the supply chain** (the user's reviewer, 2026-11-16): the Paper server's download and the plugin's build toolchain (the JDK image, the build tool and every dependency) are pinned by checksum, and the plugin is built without network, from what was fetched and verified before. If Paper has no build for the pinned Minecraft version when group A starts, the group says what works instead.

**Live tests:** the auction script and the sell script of the reference; the other player buying a listing between the scan and the click; a long run that counts double buys and unlogged misses, both of which must be zero. Trades, the anvil and the enchanting table run on the vanilla test server.

### M10. New rows for the threat model
Lore steers a script's clicks, so a server, or a player who can list an item, steers a bot's purchases: a price written to be misread by the number helper; a bait listing; a screen that changes between the scan and the click (M3's case); a title that imitates another menu. The script's author decides what to trust; the runtime guarantees only that a click lands on the item the script saw.

## Amendment: the user's additions for Phase 16 (2026-11-25)
The user added five things at Phase 16's start, for the groups after A to build. Each is recorded at its task in Plan.md; what's left open by them is decided here under ADR-0025.

### M11. A slot's data components, as registry IDs and numbers (P16.4, P16.5, P16.12)
**A slot also carries a small closed set of data components, never as text:**

| | |
|---|---|
| `slot.potion` | The potion's kind as a registry ID, such as `"minecraft:turtle_master"`, `"minecraft:long_turtle_master"` or `"minecraft:strong_turtle_master"`, for a potion, a splash potion, a lingering potion and a tipped arrow; `()` for any other item and for a potion without a kind |
| `slot.enchantments` | A map of each enchantment's ID to its level, such as `#{ "minecraft:sharpness": 5 }`: a tool's, a weapon's, an armour piece's |
| `slot.stored_enchantments` | The same for an enchanted book's stored enchantments |

- **Why:** a vanilla potion's kind is in no name and no lore: the client writes "Potion of the Turtle Master" from the kind itself. Without it a script can't tell a Turtle Master from a water bottle, and two listings of one kind of item differ only there.
- **From azalea 0.16.0** (docs.rs): `components::PotionContents { potion: Option<Potion>, .. }` (its custom colour, custom name and custom effects are left out), `components::Enchantments { levels: HashMap<Enchantment, i32> }` and `components::StoredEnchantments { enchantments: HashMap<Enchantment, i32> }`. `Enchantment` is an ID in a registry the server sends, resolved to its identifier through the world's registries; an ID the registry doesn't know reads `"minecraft:unknown"`, as a world's name does (ADR-0042).
- **Bounded,** as everything a server can grow: at most 16 enchantments a slot (vanilla has 42 in all, an item rarely more than 8), each level 0 to 255 (the most a command gives); a slot with more, or a level past that, keeps the first 16 by ID and leaves the rest out.
- **The same item is still the whole item** (M3): the comparison before a click covers every component, these and all others.

### M12. `bot.name` (P16.12)
**The bot's own player name**, as its account has it: no text from the server. A script shared by several bots reads it to skip its own auction listings by their seller line (`slot.lore_has("Seller: " + bot.name)`).

### M13. Two more examples, run live on the harness (P16.12, P16.17)
- **A farm that sells:** it farms by holding its farm item's button, and every few minutes it opens the sell menu, sells its Turtle Master potions, drops one listed kind of junk (one item ID the mode names, never "everything else", so it never drops its tools), waits `random.between(300, 900)` ms before every click, then selects the farm's item again and goes back to farming: a held right button with a potion in hand would drink it. The wait is a timer per click: hooks still run to their end (D1).
- **An auction lister:** it reads the Turtle Master listings' prices from their lore (`slot.lore_number("Price")`), skips its own listings (M12), takes the middle of the three cheapest, lists one potion at that price with `/ah sell {price}` (a declared message, D8), confirms on the confirm screen, and collects its expired listings. **The declared price range's minimum is the floor a bait listing can't push it below,** and the example says so in a comment: a price below it is refused (`bad_value`) and nothing is sent.
- **The harness holds what both need** (P16.1): the auction plugin's `/ah sell <price>` from the hand with a confirm screen, pages with a next-page button, a seller line in each listing's lore, real potions among the listings with their potion component, expired listings to collect, a sell menu, and a second player who buys.

### M14. What the bot sees, on demand (P16.20, a task of its own)
**A bot's page shows, on demand, what the bot sees in the screen it has open:** each slot's item, count, name, lore and the components of M11, as plain text. An author needs it to write a script for a new server.
- **Who may see it: View on the bot** (`ViewBot`), the level that already sees its chat history and live events; below it the bot is 404, as everywhere. A screen holds the server's text, as chat does, and nothing of the mode: a declared message's arguments never reach a screen.
- **How:** `GET /api/v1/bots/{id}/screen`, authed, not audited (it only reads), with a rate-limit class of its own (each read asks the agent), its rows in the authorization matrix and the route-coverage test, and the 401, 404, 422 and 429 tests every endpoint has. The agent answers from P16.5's snapshot on its stream within a bound; a bot offline or without a screen answers so. Text is sanitized and bounded as M1 has it, components are IDs and numbers as M11 has them. The app's bot page gets a panel with a refresh button, plain text only.
- **More than a small task,** so it's P16.20, in group E beside the wire and the app at level 2.

### M15. A decimal where a declared message wants a whole number (P16.12; ADR-0040, E9)
**Today a decimal number given for a declared message's whole-number value is refused as `bad_value`;** the user's reviewer had to write `.floor().to_int()` in a payout script. The reference says so now, with that workaround.

**Decided: it counts down to the next whole number,** as a decimal does wherever the API takes a whole one (ADR-0040, E9: `world.block(bot.x, bot.y - 1, bot.z)`), and the range is judged on the result. **A decimal value with set places counts down at its last place too,** instead of rounding to the nearest: a payout never sends more than the script gave, which rounding up could. Built with group D, for every API level: a value refused before is sent, and a value with more places than declared is written down rather than up.

## Consequences
- **Two phases, thirteen groups,** each a pull request, each after the last one's merge.
- **A new crate and a new dependency** with two `deny.toml` exceptions, an API Rhai calls volatile, and an unmaintained crate inside the sandbox.
- **A new language for authors.** A misspelled method is found only when it runs.
- **Agents must be updated before a scripted mode is used;** the server enforces it instead of the runbook asking for it.
- **The app's rule against runtime styles gets one named exception,** with the CSP unchanged.
- **Scripts can walk and mine.** The leash and the guards bound what a mistake costs; they don't make a bot safe on a server its owner doesn't know.
- **What a server allows** (automated farming, trading, auction buying) is checked by the user for each server, and the reference tells every author the same.

## The user's answers
The plan ended with eight questions that were the user's. Their answers, of 2026-11-16:

1. **Who may write scripted modes?** Admins and the Owner write them; a Member runs a shared one on bots they control (D15).
2. **May scripts read chat?** **Yes, and in Phase 15:** system messages by declared patterns, never player chat, under six guards the user set. The plan had recommended inventory and menu state only, with patterns as Phase 16's last group on a yes; that group is gone. The user's case: every 10 hours, and at every join, a script sends `/balance`, reads the reply, and pays what's above a threshold to their main account with `/pay`. The guards, the design and where each part is built are [ADR-0037](0037-scripts-read-system-chat.md).
3. **Message templates with typed values?** Yes (D8).
4. **How often may a script send a command?** A templated command goes by the bot's own chat rate; plain chat keeps 30 s (D7).
5. **The leash's default.** 8 blocks, 1 to 64 per mode, no "off" (D10).
6. **The tool guard's default.** On, at 2 uses left (D11).
7. **The bot's-eyes panel.** A phase of its own, after these two (D12).
8. **When does `enter_minecart` go?** Marked in Phase 15, removed one release later (D17).

**Also the user's, with the answers:** timers must allow intervals of at least 10 hours. They reach 24 hours (D3).

**Four notes from the user's reviewer,** each recorded at its decision and in its group's task:
- **Group B:** the state's size is checked while a hook runs, not only after it (D5; P15.7).
- **Group F:** the editor is verified in the built app, not only in Edge (D16; P15.24).
- **Phase 16, group A:** the Paper server and the plugin's toolchain are pinned by checksum, and the plugin is built without network (M9; P16.1).
- **Phase 16, group B:** "the same item in the slot" is the whole item (M3; P16.6).

**Mine, where an answer left a detail open:**
- **A declared command without values goes by the chat rate too,** as one with values: the answer names templated commands, and both are commands the mode declares.
- **Chat reading is built in Phase 15's groups,** not as a group of its own (ADR-0037, C1).

## Alternatives considered for the whole
- **More step types instead of scripts** (a condition, a branch). Every new need is a new type, a wire case, an editor state. ADR-0034 was the first, and it already needed patience, a click by ID and a look that doesn't stick.
- **Chat triggers** (Plan.md §10's first idea). Text from players would move bots. The user's rule is the opposite.
- **One phase for everything.** Inventory and menus double the API and bring the first free text from a server; Phase 15 is useful without them.
