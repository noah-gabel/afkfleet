# 0040. `fleet-script` as built: an engine an instance, a sandbox tighter than planned, and what was measured

- **Status:** Accepted. Decided under ADR-0025; two points are put to the user at the end
- **Date:** 2026-11-17
- **Related:** Plan.md Phase 15 (P15.5 to P15.9, P15.31); [ADR-0036](0036-scripted-modes.md) (D3 to D6, D14, D18) and [ADR-0037](0037-scripts-read-system-chat.md) (C4, C6), whose decisions this builds; [the script reference](../scripting.md); ADR-0010 (time and randomness are parameters), ADR-0030 (fuzz targets); `deny.toml`; `crates/fleet-script`, `crates/fleet-fuzz/src/script.rs`

## Context
Group B builds the crate that holds Rhai: the sandboxed engine, the check at save, the instance that runs a script's hooks, the API's objects, the examples and the fuzz targets. ADR-0036 and ADR-0037 made the decisions; this records how they were built, what the user's reviewer found in a test project of their own against rhai 1.27.0, and what building it found beyond that.

**Everything below was checked against rhai 1.27.0 itself:** its source at the tag `v1.27.0` in Rhai's repository, and tests in this crate that run the real engine. Each fact has a test, so the next bump of the exact pin shows what changed.

## The reviewer's seven findings
| # | Finding | Verified | What was built |
|---|---|---|---|
| 1 | `Engine::on_progress` gets only an operation count; with one engine for all bots the callback can't tell whose hook runs | `src/api/events.rs`: `callback: impl Fn(u64) -> Option<Dynamic>` | **An engine an instance** (E1) |
| 2 | 0 means "no limit" for operations, expression depth and the sizes of texts, arrays and maps; the text limit counts bytes | `src/api/limits.rs`: each is stored as `NonZero…::new(n)`, and "the maximum length, in bytes" | A second's budget answers "don't run" at nothing left and hands out a `NonZeroU64`; fleet-core's constant is `MAX_STRING_BYTES` now, and the reference says bytes (E5) |
| 3 | A closure that captures `state` and is kept in it never lets the state be freed | Seen: `#{"f": Fn*("anon$…", <shared>)}` | **A script has no anonymous functions** (E3), and the state check still refuses a function pointer at any depth, judging a value's kind before reading inside it (E4) |
| 4 | `state.me = state` copies; Rhai checks an assigned value's nested size | Seen: `Size of object map too large` after a few rounds | D5's argument holds; the memory test shows it (below) |
| 5 | `impl AsRef<[Stmt]> for AST` is public without `internals` | `src/ast/ast.rs` | D14's refusal of anything outside a function works: `AsRef::<[_]>::as_ref(&ast).len()` |
| 6 | With `eval` disabled, `Fn("eval").call(..)` and `call(Fn("eval"), ..)` fail | Seen: `'eval' is a reserved keyword` | Both are tests, of the engine and of a running script |
| 7 | `Engine::on_var` is `#[deprecated]` to mark it volatile | `src/api/events.rs`; **`on_def_var` carries the same attribute** | One `#[expect(deprecated, reason = …)]` on the one function that sets both (E2) |

## Decisions

### E1. An engine an instance, over shared packages and one shared API module
Each `Instance` owns an `Engine::new_raw()` with Rhai's packages and the API's functions registered as shared modules that are built once for the process. The progress callback, the variable resolver and the operation limit are the instance's own, so one bot's budget, state check and game objects are never another's, on whatever thread.

**Measured** (the development PC, a debug build): 200 raw engines over a shared standard package take 64 µs to build, against 317 ms for 200 × `Engine::new()` (the reviewer's Linux release numbers: 34 µs against 86 ms). An instance, engine and all, takes about 2.2 KiB; 200 of one program take 0.42 MiB.

**The API's functions hold no state.** A function gets a small handle that points at its instance's turn (the facts, the limiter, the holds, the timers), which the instance's resolver makes when a script names an object. That's what lets one module serve every engine.

### E2. The packages are picked by hand
`Engine::new()` brings Rhai's language-core package, and with it **`sleep`, which blocks the thread that runs the hook:** a script could have parked a runtime thread for as long as it liked. Also `exit`, `parse_json`, blobs and bit fields. A script gets arithmetic, logic, text, numbers, arrays, maps and ranges, and a test holds that none of the others can be called. ADR-0036 left this choice to group B.

### E3. No anonymous functions *(tighter than ADR-0036's D3 planned)*
D3 keeps Rhai's closures in the build because the state needs Rhai's shared values. That stays. What goes is the syntax: `set_allow_anonymous_fn(false)`, so a script writes `list.filter(Fn("is_big"))` instead of `list.filter(|x| x > 1)`.

**Why: a script could stall its thread.** With `sync`, a shared value sits behind a lock, and Rhai's access to a locked one tries five times with 10 ms of sleep between (`locked_read` in `src/func/native.rs`), then fails with a "data race" error **that a script can catch.** A closure that captures a variable makes it shared; a method on that variable holds its lock while it calls the closure; the closure touches the variable. Measured: 51.6 ms for one such access. In a loop with `try` and `catch`, one hook's 10,000 operations would have held a runtime thread for about 100 seconds.

Without anonymous functions nothing captures, so **the state is a script's only shared value,** and E4 guards that one. It also ends finding 3 at its source: no script can make a state that holds itself.

**What it costs an author:** a named function where a closure would have been. No example in the reference used one.

### E4. The state: its own cell, never waited for, and stops that stick
- **The cell is fleet-script's own `Arc<RwLock<Dynamic>>`,** handed to Rhai as its shared value (Rhai's `Shared<Locked<Dynamic>>` is that type under `sync`). So the instance looks at the state with `try_read`, which never waits, where Rhai's own read would wait 50 ms.
- **Checked every 256 operations and after the hook,** as D5 asks: still a map, plain data at every depth (nothing, `true` and `false`, numbers, characters, texts, arrays and maps of those), at most 64 entries counting nested ones, at most 4 KiB. A value's kind is judged before anything is read from inside it, and a shared value inside it is refused without being locked. The check stops at the first entry too many, so it never goes deeper than 65 levels.
- **While a call on the state runs, the state is in use.** `state.list.filter(Fn("keep"))` holds its lock while `keep` runs. A check that finds it locked is skipped; the next one, or the one after the hook, sees what the call left. And **the resolver ends the hook at once if a script names `state` meanwhile** (`StateInUse`), instead of handing out a value Rhai would wait for.
- **A stop sticks.** Rhai's array functions wrap whatever error a function they call raises (the function pointer's call in `src/types/fn_ptr.rs`), and the wrapped one can be caught, even when the inner one is "terminated". So once one of the instance's guards has ended a hook, every further operation ends it again, and the instance reports that guard as why the script stopped, whatever the script made of the error on its way out.
- **A stopped or ended script's state is emptied.**

### E5. The budgets
- **A hook:** `set_max_operations` before each hook, to what the hook may run.
- **A second:** 30,000 operations in any one second, over all of a script's hooks. Operations are counted in slices of 50 ms and a second is judged over 21 of them, which errs towards refusing; a property test holds that no second, whenever it starts, holds more. A clock that goes back frees nothing.
- **Nothing left means the hook isn't run,** and the script stops for the second's budget. A budget of 0 never reaches Rhai. A hook that gets less than a hook's own budget and runs out of it stops for the second's.

### E6. A stack guard *(new)*
Rhai bounds a script's use of the thread's stack by counting levels: 16 calls, 16 levels of nesting in a function. A level's size depends on the build.

**Measured,** the deepest script those limits allow (six nested call arguments in each of 17 calls), on the development PC:

| | A release build | A debug build |
|---|---|---|
| One level of parentheses in each of 17 calls | 73 KiB | 460 KiB |
| One level of text interpolation in each | 112 KiB | 759 KiB |
| The deepest: six nested arguments in each | **309 KiB** | **1,797 KiB** |

A thread of the async runtime has 2 MiB. A debug build was within a few hundred KiB of the end, and **a stack that runs out ends the process,** with every bot of the agent.

**So the instance measures instead of counting:** at every operation it compares where the stack is with where it was when the hook started (the address of a local, taken with safe code), and past **1 MiB**, half a runtime thread's, it ends the hook with `Limit::Stack`. A release build never gets near; a debug build ends the most deeply nested scripts a little before Rhai's call limit would.

**For group D:** a hook must run on a thread with at least 2 MiB of stack: a runtime worker, or a test's thread. Not the main thread on Windows, which has 1 MiB.

### E7. Validation
`compile(&Script)` runs Rhai's compiler with strict variables against placeholder objects, then checks what Rhai's public API shows.

- **The optimizer is off.** With it on, Rhai copies a scope's constant into the program wherever it can prove the name constant: `"a" in state` then ran against the *placeholder* instead of the script's own state (seen: `Function not found: contains (Placeholder, …)`). With `OptimizationLevel::None` every use reaches the resolver; tests hold it for the state and for a game object that's passed on or kept in a variable.
- **An error's place where Rhai gives none.** Rhai's function metadata has no position without `internals`. A small scan of the text (`scan.rs`), which skips comments and texts as Rhai's tokenizer does, finds where each function is defined, the first thing outside every function, and the variables `for` and `catch` name.
- **`for keys in …` and `catch (state)` are refused** by that scan: Rhai's definition filter isn't asked for them, and the resolver's object would silently win over the loop's own value, as for a parameter.
- **Refused beyond D14's list:** an anonymous function (E3); a text, an array or a map written out past its limit (Rhai's own, as a syntax error).
- A script's source size and characters, its settings and declarations are the type `Script`'s own checks (group A) and aren't repeated.

### E8. What the limits are, exactly
- **A text is 1,024 bytes,** not characters (finding 2).
- **Calls: 16 below the hook.** Rhai's `set_max_call_levels(16)` lets the hook's functions nest 16 deep.
- **Nesting: 16 levels as Rhai counts them,** which in a script's terms is 4 `if`s or loops inside each other, 6 calls or parentheses, a sum of 13 terms, an `if … else if` chain of 11 branches; a `switch` isn't a chain. A test holds these numbers, and the reference says them.
- **Variables: 64 alive at once in a function** (`set_max_variables`). Not in ADR-0036's table; Rhai has no limit by default, and each variable can hold a value as large as the data limits allow.
- **A map's 64 entries are checked where Rhai checks them:** when a map is passed to a function or assigned as a value, not when an index assignment adds an entry. So a script's own (not the state's) map can pass 64 entries. What bounds it is the hook's operations and the limit on each value; the memory test measures it.

### E9. The API as a script sees it
- **Everything a script gets back is plain data.** A result is a map with `ok` and `reason` (empty when taken); an entity, a held item and what the crosshair is on are maps. So any of them can be kept in the state.
- **A wrong value is a result, a wrong program an error.** A number out of range or a text where a number belongs is `bad_value`. A key the mode doesn't declare, `chat.expect` for a pattern that isn't a reply, a hold of a key that only taps, `hold_until_used` on anything but the right button: these stop the script with their line (`Misuse`), as a misspelled method does.
- **Numbers:** whole and decimal numbers both go where a number belongs (`look.at(90, 10.5)`); a decimal one counts down to the next whole one where a whole one belongs (`world.block(bot.x, bot.y - 1, bot.z)`).
- **`chat.expect`** takes the last declared command the same hook sent, or answers `no_command`; a refused send (the bot is dead, a value out of range, an unknown key that was caught) counts as none.
- **`on_stop`** takes releases, state changes, log lines and one declared message; everything else answers `not_in_world`.
- **`log.info`** cuts a line at 200 characters and puts it on one line; the 21st line of a minute answers `too_soon`.

### E10. The instance's interface, for the runtime
- **`Instance::handle(event, moment, limiter)` and `Instance::wake(moment, limiter)`.** A `Moment` is the facts, the time, whether the bot can act (`Presence`) and, when its anchor is known, the leash. The bot's `ActionLimiter` is passed in and outlives the instance, so a new instance gains nothing on the limits.
- **`Action` is fleet-script's own type,** not fleet-core's `GameAction`, which gains its cases with their implementation in group C. Every action in an answer has passed the limiter, the presence, and for a press the leash and the guards.
- **Timers and `hold_for` are the instance's:** it says when it wants to be woken next; a wake lets go of what's due and runs `on_timer` for each due timer, in the order they were due, each as a hook of its own. Timers wait while the bot is dead or between worlds.
- **`let_go`** tells the script that the session below let go of an input (the leash, the tool guard, a used-up item), and **`disconnected`** clears the timers and the holds.
- **The outcome** is one of: ran, not defined, stopped (with the error, the hook and the line where Rhai has one), ended, or over.

### E11. The memory test, and its dev-dependency
`tests/memory.rs` is a test binary of its own that replaces the global allocator with **`peak_alloc` 0.3.0** (MIT, no dependencies, published 2025-06-23), which counts what the process holds and held at most. Our own code can't write an allocator: that needs `unsafe`. It's a dev-dependency of fleet-script alone.

**Measured** (a debug build, the development PC), each within one hook's 10,000 operations:

| A script that… | Held at most | Stops for |
|---|---|---|
| assigns its state to itself again and again | 0.06 MiB | A text's size, by Rhai's check of the assigned value |
| puts a largest array under ever new keys of the state | **0.08 MiB**, the worst of seven | The state's entries, at the first check |
| puts a longest text under ever new keys of the state | 0.03 MiB | The state's bytes |
| grows a map of its own with largest arrays | **2.86 MiB**, the worst of four | The hook's operations |

So a script that attacks its state never takes more than a tenth of a MiB, and one that grows its own variables under 3 MiB, gone with the hook. The test's bounds are 1 MiB and 8 MiB. After 2,000 stopped instances whose state held a function pointer with a copy of the state, and 2,000 that ended or were dropped, nothing is left: 0 bytes.

### E12. Fuzz targets
Five more in `fleet-fuzz` (fifteen in all), each a plain function whose properties its tests hold in `just check`:

| Target | Reads | Holds |
|---|---|---|
| `script_compile` | A script's text | No panic; an error's place is inside the text, its message one bounded line; what compiles holds the script it came from |
| `script_run` | A script, then events with pauses and facts | D18's properties: a hook within its budget and a second within its own; no two actions of a kind closer than its minimum; after any stop nothing held, no timer, no wake, an empty state; every message sent is one the mode declares, with that declaration's command |
| `script_api` | Calls of every API member with arguments of every kind and number | The same, and no result with a reason outside the closed list |
| `message_template` | A declared message's JSON, then values | A declaration reads back as itself and fits a chat message; whatever is given, what's rendered is that declaration's message |
| `chat_pattern` | A declared pattern's JSON, then a line | A declaration reads back as itself; a match gives exactly the declared captures, each a value its declaration allows |

Each property's check has a test that shows it failing, and with the limiter switched off in a trial, `script_run` failed on its seeds ("two LeftButton within 176ms"). The Fuzz workflow now also runs on pull requests that touch `crates/fleet-script`.

### E13. The two `deny.toml` exceptions, and the wrapper
As ADR-0036's D3 named them, pinned like the others:
- **A license exception for `tiny-keccak@2.0.2`** (CC0-1.0). It reaches Rhai through `ahash`'s compile-time seed, in a proc macro, so it runs at build time and is in no binary.
- **An `ignore` for RUSTSEC-2026-0249** (`smartstring` is unmaintained), with the removal trigger "a Rhai release without smartstring".
- **`rhai` is banned with the wrapper `fleet-script`,** so no other crate can depend on it; a trial with another wrapper named fails the check.

`fuzz/`'s check reads the same `deny.toml`, and `fuzz/Cargo.lock` gained the same crates. Both lockfiles also name `no-std-compat` and `spin` 0.5.2, which are never built: Rhai's `sync` names a feature of an optional dependency, and cargo records it.

## Put to the user
1. **Nesting of 16 levels is tight.** It's ADR-0036's number and unchanged. In practice: a log line with two interpolated calls inside an `if` didn't compile, and neither did `timer.after(random.between(…), …)` with an interpolated name inside an `if`. An author splits such a function in two. Rhai's own default for a release build is 32. Raising it is possible now that E6 bounds the stack by measuring; it would be a change to D3's table, so it's yours.
2. **E3 takes anonymous functions away,** which D3 didn't plan. I'd keep it: it's what makes the 50 ms waits impossible instead of unlikely.

## Alternatives considered
- **One engine for all, with the instance in the call's tag.** The resolver gets the tag; the progress callback doesn't (finding 1), so the state check and the second's budget couldn't know their bot.
- **Keeping anonymous functions and taking `try`/`catch` away.** A lock wait then ends the hook, once. But Rhai reads a locked shared value in places that raise nothing (showing it as text), so the waits would stay possible.
- **The state as a type of our own with setters,** checked at every write. No lock of Rhai's at all, but every method a map has would be ours to register, and D5 chose the shared map with the check in the progress callback.
- **A lower call limit** (Rhai's own debug default is 8) instead of E6. It halves a debug build's worst case and leaves it build-dependent; measuring bounds it in every build.
- **`Engine::new()` with `sleep` overridden.** One function fewer to forget is none; picking packages leaves nothing to override.
- **A counting allocator of our own.** `GlobalAlloc` is an `unsafe` trait, and the workspace forbids `unsafe`.

## Consequences
- **A new crate, `fleet-script`,** the only one that depends on Rhai, with a 90 % coverage gate; its own tests cover 98 % of its lines.
- **The language is smaller than Rhai's:** no anonymous functions, no `sleep`, nothing outside a function. The reference says so.
- **A script can't stall, overflow or outgrow its thread:** operations, the state, the stack and memory are each bounded by a measurement or a count that tests hold.
- **Three `deny.toml` entries,** two of them weaker rules, listed under Weakened.
- **For groups C and D:** `Action` maps onto `GameAction`'s new cases; the runner hands in the limiter, a snapshot, the presence and the leash; hooks run on runtime threads; compiling belongs in `spawn_blocking`.
