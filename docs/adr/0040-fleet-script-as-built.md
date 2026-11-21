# 0040. `fleet-script` as built: an engine an instance, a sandbox tighter than planned, and what was measured

- **Status:** Accepted. Decided under ADR-0025. The two points put to the user are answered, and a hook is bounded in time, in the [amendment](#amendment-2026-11-18-a-hooks-time-and-the-users-answers) of 2026-11-18; the point that amendment put to them is answered at its end (2026-11-21: scripts keep `try` and `catch`). A raw string is refused since the amendment of 2026-11-21 (A6), which the fuzzer prompted on group E's CI.
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
Rhai bounds a script's use of the thread's stack by counting levels: 16 calls, 16 levels of nesting in a function. A level's size depends on the build. *(The amendment's A3 raises the nesting to 32 and has the numbers for it.)*

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
- **A name in an error is cut at 200 characters,** like Rhai's own message. Rhai puts no bound on a name's length, so a function named `on_` and 400 letters is refused as no hook with the first 200 characters of its name (E12's finding).
- A script's source size and characters, its settings and declarations are the type `Script`'s own checks (group A) and aren't repeated.

### E8. What the limits are, exactly
- **A text is 1,024 bytes,** not characters (finding 2).
- **Calls: 16 below the hook.** Rhai's `set_max_call_levels(16)` lets the hook's functions nest 16 deep.
- **Nesting: 16 levels as Rhai counts them,** which in a script's terms is 4 `if`s or loops inside each other, 6 calls or parentheses, a sum of 13 terms, an `if … else if` chain of 11 branches; a `switch` isn't a chain. A test holds these numbers, and the reference says them. *(32 levels since the amendment's A3: 9, 14, 29 and 27.)*
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
  - *Changed in group D ([ADR-0043](0043-the-script-runner-and-hooks-on-the-async-workers.md), R9):* **a wake runs one due timer, the one that was due first,** and asks to be woken again at once while another is due. The timers still run in the order they were due, each as a hook of its own; what changed is that no call on an instance takes longer than one hook's 5 ms. Before, four timers due together held their thread for up to the second's 20 ms.
- **`let_go`** tells the script that the session below let go of an input (the leash, the tool guard, a used-up item), and **`disconnected`** clears the timers and the holds.
- **The outcome** is one of: ran, not defined, stopped (with the error, the hook and the line where Rhai has one), ended, or over.
- *Added in group D:* **a call also says what it refused** (`Ran::refused`): each kind of thing a hook asked for and each reason once, with how often. The script got every refusal as a result; this is the same told to the runtime, for the script's log and its counters (ADR-0043, R7).

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

**Its first finding, on CI on 2026-11-17.** The workflow's first run on this branch passed; its second, on the same tree after `main` was merged in, failed in `script_compile` after 38 seconds: a function whose name starts with `on_` and is 246 characters long. It's refused as no hook, as it should be, but the error carried the whole name, so the line an author reads was longer than the bound the target holds. No panic of ours, no limit passed: a script's text is at most 16 KiB, so the name was too. The name is now cut like every other (E7). The input is a test of `compile` and, byte for byte, a seed of the target, so `just check` runs it.

### E13. The two `deny.toml` exceptions, and the wrapper
As ADR-0036's D3 named them, pinned like the others:
- **A license exception for `tiny-keccak@2.0.2`** (CC0-1.0). It reaches Rhai through `ahash`'s compile-time seed, in a proc macro, so it runs at build time and is in no binary.
- **An `ignore` for RUSTSEC-2026-0249** (`smartstring` is unmaintained), with the removal trigger "a Rhai release without smartstring".
- **`rhai` is banned with the wrapper `fleet-script`,** so no other crate can depend on it; a trial with another wrapper named fails the check.

`fuzz/`'s check reads the same `deny.toml`, and `fuzz/Cargo.lock` gained the same crates. Both lockfiles also name `no-std-compat` and `spin` 0.5.2, which are never built: Rhai's `sync` names a feature of an optional dependency, and cargo records it.

## Put to the user
**Answered on 2026-11-18** (the amendment's A3 and A4): 1. raise it to 32; 2. anonymous functions stay off.

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

## Amendment (2026-11-18): a hook's time, and the user's answers
The user's reviewer built this crate as merged and timed it, and the user answered the two points above and ADR-0038's. Built as the first commit of group C's pull request.

### A1. A hook is bounded in time as well as in operations
**The finding.** ADR-0036 counted on a hook's operations bounding its time: 10,000 operations, "at most about 0.45 ms" (D1, D3's table). The reviewer's hooks, each run to those 10,000 operations (a release build, Linux):

| A hook that… | Took |
|---|---|
| runs a plain loop | 0.38 ms |
| calls `a.map(Fn("id"))` over 30 items, in a loop | 41 ms |
| calls `[1].map(Fn("bomb"))` in a loop, where `bomb` builds a text of 1 MiB with `replace` or splits a text of 1 KiB, and catches the refusal | 106 to 120 ms |

**Why operations don't bound time:**
- **An operation's cost varies by a factor of several hundred.** A call of a script's function through an array's method is one operation and costs about a hundred plain ones.
- **Rhai builds a value past its limit before it refuses it** (a text of 1 MiB from one of 1 KiB, an array of 1,024 from a text), and inside a function that an array's method calls, the refusal can be caught (A2), so a loop pays for it again and again.

**So ADR-0036's numbers are corrected:** a hook's operations bound a hook of *plain* operations to about 0.1 to 0.45 ms. They don't bound a hook's time. Group D runs hooks on the async runtime's threads, where 120 ms is far too long.

**What's built:**
- **`fleet_core::script::Stopwatch`,** a port: `elapsed()`, monotonic. An instance is given one when it's made (`Instance::new(program, seed, stopwatch)`). The real one, which reads the OS's monotonic clock, belongs to whoever runs scripts (group D); fleet-testkit has `ManualStopwatch`, which stands still or moves by a fixed step at each reading, so a test says how long a hook took and gets the same answer on any machine. fleet-script itself still reads no clock.
- **Read in Rhai's progress callback, every 8 operations,** and once at a hook's start and end. A hook past its time ends like one past the stack guard: the stop sticks (E4), so a script can't catch its way past it.
- **Two limits of their own,** in fleet-core's `limits` and beside the operation budgets in the same 50 ms slices (E5):

  | Limit | | Stops the script with |
  |---|---|---|
  | A hook | **5 ms** | `Limit::HookTime` |
  | A second's hooks, together | **20 ms,** 2 % of one core | `Limit::TimePerSecond` |

  With nothing left of the second's time, the hook isn't run at all, as for operations. A hook that gets less than a hook's own time and runs past it stops for the second's.
- **A gap between two readings counts for at most 1 ms** (`limits::LONGEST_STRETCH`). No step of a script takes that long, so a longer gap is the host's: the thread was set aside, or the whole process was (a container at its CPU limit stands still for up to 100 ms). A hook pays 1 ms for each such gap and no more. That's what keeps the two limits low without stopping a script that did nothing wrong on a busy host: a hook would have to be set aside five times to meet its limit, and a normal hook is over before it's set aside twice.
- **A stopwatch that goes back counts as one that stands still.**

**Why these numbers.** A hook of plain operations that runs to its 10,000 takes 0.1 to 0.4 ms in a release build, so 5 ms is twelve to fifty times that; the minecart farm's swing takes 3 µs (ADR-0036). A second of plain operations at its 30,000 takes about 1.4 ms, a fourteenth of 20 ms. In a debug build a plain hook at its budget took 1.1 ms. Like the two operation budgets, they're proposals until group D's load test (P15.19).

**Measured** (the development PC, with the OS's clock; the same three scripts as tests of the suite):

| | Without the bound, a debug build | With it, a debug build | With it, a release build |
|---|---|---|---|
| A plain loop | | 1.15 ms, ended by its operations | 0.10 ms, ended by its operations |
| `map` over 30 items, in a loop | about 500 ms | 5.1 ms | 5.0 ms |
| The text of 1 MiB, caught, in a loop | about 450 ms | 5.1 ms | 5.0 ms |
| The text split into 1,024, caught, in a loop | about 450 ms | 5.2 ms | 5.0 ms |

**What bounds the overshoot:** a hook ends at the first reading past its time, so it runs at most 8 operations longer. Where each of those is one of the slowest a script has (the text of 1 MiB), the 1 ms cap can undercount a stretch; a release build stayed within 0.05 ms of the limit in every case above.

**Tests:** the suite's `time` module (each limit at its exact edge, the second's budget, the cap, a stopwatch that goes back, a caught end, `on_stop`, and the reviewer's three cases against the OS's clock, asserting why each stops and printing how long it took); the budget's own tests and a property test that no second holds more time than its budget; and the fuzz target `script_run`, whose runs are timed by a stopwatch of 0 to 127 µs a reading and held to both limits.

**Not changed:** `Outcome::Ran` still reports operations only. A hook's time is the runner's to measure for its metrics (P15.18, P15.19).

### A2. A caught data-size error can't be made to end the hook
The user asked for it "if Rhai allows it". **It doesn't,** in 1.27.0:
- A script's `catch` takes an error when the *outer* error says it's catchable (`Stmt::TryCatch` in `src/eval/stmt.rs`: `Err(err) if !err.is_catchable() => Err(err)`). "Data too large" isn't. But a function called through a function pointer has its error wrapped (`_call_with_extra_args` in `src/types/fn_ptr.rs`: `ErrorInFunctionCall`), and the wrapper is catchable whatever is inside.
- **Nothing tells the host that an error was raised or caught.** The progress callback gets an operation count; there is no hook on errors without the `debugging` feature, and the size check itself (after a native call returns) isn't ours to replace.
- So the instance can make its *own* stops stick (E4), because it knows when it stopped a hook. It can't know that Rhai refused a value.

**What holds instead:** such a loop is bounded by the hook's operations and now by its time (A1), and what it built is freed at once (E11's memory test). Outside a function pointer's call the refusal can't be caught and stops the script as before; a test holds both.

**The three ways to close it, none taken:**
- **No `try` and `catch` at all** (`disable_symbol("try")`). Every error would then end the hook, which is what ADR-0036's D3 says in so many words ("a script that fails or hits a limit stops"). But the reference tells authors they can catch their own errors, so it's the user's to decide: put below.
- **No function pointers.** E3 tells authors to write `list.filter(Fn("is_big"))`.
- **Our own copies of every array method that takes a function,** unwrapping the error. Some twenty functions to keep equal to Rhai's at every bump.

### A3. Nesting inside a function: 32 levels *(the user's answer to point 1)*
Rhai's own default for a release build. `limits::MAX_FUNCTION_EXPRESSION_DEPTH` is 32; E8's bullet and the reference now read: **9 `if`s or loops inside each other, 14 calls or parentheses, a sum of 29 terms, an `if … else if` chain of 27 branches.**

**The stack guard (E6) is what makes it safe, and now it's needed in earnest.** The deepest script Rhai's limits allow, measured without the guard on a thread large enough (the development PC):

| | A release build | A debug build |
|---|---|---|
| Before: 6 nested arguments in each of 17 calls | 309 KiB | 1,797 KiB |
| Now: 14 nested arguments in each of 17 calls | **624 KiB** | **3,580 KiB** |

A debug build would now run past a runtime thread's 2 MiB. **The test the user asked for** (`the_most_deeply_nested_script_never_overflows_a_runtime_threads_stack`): on a thread of exactly 2 MiB, for each of nine ways to nest a call, the deepest nesting that compiles is found by compiling there, in each of as many calls as the call limit lets run, and run through an instance. Each one runs, or ends with `Limit::Stack`; a stack that ran out would end the whole test binary. In a release build all nine run. In a debug build five end with `Limit::Stack` (blocks, loops, arguments, texts, sums) and four run.

**The guard reads the stack at every operation,** and every level of nesting is at least one, so between two readings the stack grows by one level's frames (tens of KiB in a debug build), far less than the 1 MiB between the guard's limit and the thread's end.

### A4. Anonymous functions stay off *(the user's answer to point 2)*
E3 stands.

### Put to the user
**Answered on 2026-11-21** (A5).

- **A script's `try` and `catch` (A2).** Keeping them means a script can catch a refusal of Rhai's own inside a function pointer's call (a data size, the call depth) and go on until its operations or its time end the hook; nothing is lost by it but that hook's 5 ms at most. Taking them away would make every error stop the script, and take from an author the way to survive a mistake of their own (an index past an array's end, a missing property) on a farm that runs for days. I'd keep them.

### A5. Scripts keep `try` and `catch` *(the user's answer, 2026-11-21)*
The user's answer to PR #126: **scripts keep them; the time bound ends a caught loop within its 5 ms.** A2 stands as built, and none of its three ways to close the gap is taken.

- **What a script can catch:** its own mistakes (an index past an array's end, a property never set), and one refusal of Rhai's that gains it nothing: a text, an array or a map past its limit inside a function that an array's method calls.
- **What it can't catch its way past:** a hook's operations and time, a second's, the state's limits, the stack guard and the depth of its calls. Each of the instance's own stops sticks (E4), so the next thing a script does after a `catch` ends the hook again.
- **Tests that hold it:** the suite's `time` module (a caught end, and the reviewer's three cases against the OS's clock), `a_script_cant_catch_its_way_past_the_stack_guard`, and the sandbox tests of a caught data-size error.
- **The reference** says the same in "Errors", unchanged.

## Amendment (2026-11-21): no raw strings
### A6. A raw string is refused at its own place *(found by the fuzzer on group E's CI)*
`script_compile` found, on PR #128's CI, a script whose compile error was placed at column 41 of an empty line. The cause is Rhai 1.27.0's tokenizer (read in its source, `parse_raw_string_literal`): in a raw string, `#"…"#`, a line break right after a `"` is taken without starting a new line, so every place after it is a line too high and its column runs on from the line before. The scan that finds places (E3's) didn't read raw strings either, so its places after one could be wrong too.

- **So a script holds no raw string:** `compile` refuses one with `CompileErrorKind::RawString` at its `#`, the first in the text, wherever it is (in code or in a text's `${ … }`; never a `#{` map or a `#` inside a text or a comment). A mistake Rhai finds before it comes first, at its own place, which a raw string later in the text can't have moved; any place Rhai gives at or after one is replaced by the raw string's. The scan now runs before Rhai's parse, reads raw strings whole, and judges nothing else.
- **Nothing is lost:** quotes and backticks write every text a raw string can, and no example uses one. It tightens the language, so the sandbox is no weaker.
- **The other position quirks the source shows don't move a place outside the text:** a lone carriage return (which `ScriptSource` refuses), an empty piece of a text in backticks placed at no position, and `\${` counted one column short.
- **Tests:** `a_raw_string_is_refused_at_its_place`, `a_mistake_before_a_raw_string_comes_first`, `a_map_and_a_hash_in_a_text_are_no_raw_strings`, the scan's `it_finds_the_first_raw_string` and `a_raw_string_is_skipped_whole`, and the input as a `script_compile` seed (`RAW_STRING`), which the old code failed exactly as CI did ("column 13 of 0").
- **The reference** lists raw strings with what the language leaves out.
