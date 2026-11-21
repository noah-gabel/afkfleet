---
paths:
  - "crates/**/*.rs"
  - "apps/desktop/src-tauri/**/*.rs"
  - "**/clippy.toml"
---

# Rust details
Moved from `CLAUDE.md` word for word; it points here.

## azalea and the Microsoft flows
Under CLAUDE.md's "Only `fleet-mc` may depend on `azalea`":
- `fleet-mc` may use only `azalea::auth::sessionserver` and `azalea::auth::certs`, through azalea's re-export, to join with a server-issued Minecraft token.
- The Microsoft flows (device code, Microsoft tokens, refresh, the Minecraft session) belong to `fleet-server` alone, in its own adapter (`infra/msauth`, reqwest with its own requests; ADR-0018). `fleet-mc`'s `clippy.toml` bans azalea's Microsoft flows (ADR-0011).
- Only `infra/msauth/client.rs` names reqwest's client types, through the one builder; fleet-server's `clippy.toml` bans them elsewhere, along with reqwest's unbounded body readers (ADR-0018).

## The test-only and start-up crates
- `fleet-integration` holds only tests: the real server and the real managed agent together, in-process (P10.9's loopback suite, Phase 10's DoD check, P11.7's full flow). Nothing depends on it, and everything it uses is a dev-dependency; it isn't in `stable_crates`, since azalea comes in through fleet-agent (ADR-0019, U19, U21).
- `fleet-startup` holds only the binaries' process start-up and lifecycle code: the config loader and its errors, the `[log]` types, the log layer, the panic hook, the stop signals (`OsSignals`) and the secret-file loader (`secret_file`, which reads every `*_file` secret of both binaries with one set of rules; ADR-0016). Internally it depends only on `fleet-core`. Its filter always caps `azalea_auth`, `tungstenite` and `tokio_tungstenite` at `info` and lets panic reports through; a binary only adds rules through `FilterRules` (ADR-0015, ADR-0020).

## fleet-script and Rhai (ADR-0036, ADR-0040)
- **Only `fleet-script` names a Rhai type.** Everything else uses its own: `Program`, `Instance`, `Action`, `CompileError`, `ScriptError`. fleet-core's `script` module holds what doesn't depend on the language.
- **The pin on `rhai` is exact** (`=1.27.0`). Before using any Rhai API, read it for that version on docs.rs or at the tag in Rhai's repository; fleet-script's tests of Rhai's own behaviour (the engine's and the compiler's `rhai_…` tests, the sandbox tests) are what a bump is checked against.
- **Never weaken the sandbox without an ADR:** no `unchecked` or `internals` feature, no package beyond the eight `engine.rs` picks (Rhai's language-core package has a `sleep` that blocks the thread), no anonymous functions, no optimizer for the compiler, no limit of 0 (to Rhai, 0 is "no limit").
- **The state is looked at only through `state.rs`,** with `try_read`: Rhai's own access to a locked shared value waits 50 ms.
- **A function of the API is registered in `api.rs` and described in `api/describe.rs`;** a test holds the two equal. It returns plain data (a map, a number, a text), never a type of ours, so a script can keep it in its state.
- **The examples are the files in `crates/fleet-script/scripts/`.** The reference shows them word for word; a test holds that.
- **A hook runs on a thread with at least 2 MiB of stack** (a runtime worker or a test's thread): the instance ends a hook at 1 MiB.
- **An instance is handed a `Stopwatch`** (fleet-core's port) and ends a hook at 5 ms and a second's hooks at 20 ms: a hook's operations don't bound its time (ADR-0040's amendment). fleet-script itself reads no clock; tests time a hook with fleet-testkit's `ManualStopwatch`, never with the OS's clock, except the three tests of the reviewer's cases, which assert why a hook stopped and never how long it took.

## fleet-mc and a scripted mode's inputs (ADR-0042)
- **`Held` on the bot is the one place that says which keys and buttons are down.** A game action only changes it; `script/tick.rs` writes azalea's key state from it each game tick. Don't call azalea's `walk`, `sprint`, `set_jumping`, `set_crouching` or `left_click_mine` for a script's input: they act a frame late, and the leash and the guards must act inside the tick.
- **The leash, the key state and the mining guards run before azalea's own systems in the same game tick,** and `connector::app`'s probe test pins it. A new system that touches what's held joins their chain in `script/mod.rs`; every system declares its place among azalea's, as for riding (ADR-0026).
- **Mining is behind three guards, each a decision of fleet-core's, applied together in `script/mouse.rs`'s `guard_mining`:** the floor (`may_mine`), the tool (`ToolGuard::allows`) and the blocks a mode lists (`BlockGuard::allows`; ADR-0042, G13). A block a guard holds back only pauses the hold; a new reason to hold a block back joins `allowed` there, so it's judged for what azalea queued, what it mines and what the crosshair is on alike.
- **Everything a snapshot holds is a registry identifier or a number** (`script/facts.rs`): never a name, lore or other text a server chose. A server's own name of a world that is no identifier becomes `minecraft:unknown`.
- **What a server can grow, has a bound:** the passengers index (512, swept), a pending world change and each let-go (one).

## Crate-local `clippy.toml` files
**Crate-local `clippy.toml` files** (`fleet-core`, `fleet-mc`, …) must repeat every setting of the root `clippy.toml`: the test allowances and the `disallowed-methods` bans. Clippy reads only the nearest file and doesn't merge them. `just scripts-test` fails when one is missing (`scripts/clippy-config.mjs`, ADR-0011).

## The four spawned tasks without an owner (CLAUDE.md's async rules)
- fleet-mc's host threads: their jobs belong to the thread's `JoinSet` and end with its stop signal (ADR-0011).
- the bot actor's teardown task: its `JoinSet` owns it, it runs to the end because the port guarantees `disconnect()` finishes, and an aborted actor aborts it too (ADR-0013).
- fleet-client's refresh and code-step tasks: once their request may reach the server, the request, the save and the adoption run in a task of their own, so a dropped caller can't lose a rotated refresh token or orphan a session. The caller awaits its handle; it runs to the end, bounded by its request's and its store calls' timeouts (ADR-0017).
- the desktop keychain's blocking calls: a call that outlives fleet-client's 5 s store timeout runs on in its `spawn_blocking` thread until the OS returns. It holds its entry's turn (a tokio mutex per origin, shared by every store of that entry, taken before the thread starts), so the next call on that entry starts only after it ends, and a call that timed out while waiting never runs (ADR-0017).
