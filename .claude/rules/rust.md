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

## Crate-local `clippy.toml` files
**Crate-local `clippy.toml` files** (`fleet-core`, `fleet-mc`, …) must repeat every setting of the root `clippy.toml`: the test allowances and the `disallowed-methods` bans. Clippy reads only the nearest file and doesn't merge them. `just scripts-test` fails when one is missing (`scripts/clippy-config.mjs`, ADR-0011).

## The four spawned tasks without an owner (CLAUDE.md's async rules)
- fleet-mc's host threads: their jobs belong to the thread's `JoinSet` and end with its stop signal (ADR-0011).
- the bot actor's teardown task: its `JoinSet` owns it, it runs to the end because the port guarantees `disconnect()` finishes, and an aborted actor aborts it too (ADR-0013).
- fleet-client's refresh and code-step tasks: once their request may reach the server, the request, the save and the adoption run in a task of their own, so a dropped caller can't lose a rotated refresh token or orphan a session. The caller awaits its handle; it runs to the end, bounded by its request's and its store calls' timeouts (ADR-0017).
- the desktop keychain's blocking calls: a call that outlives fleet-client's 5 s store timeout runs on in its `spawn_blocking` thread until the OS returns. It holds its entry's turn (a tokio mutex per origin, shared by every store of that entry, taken before the thread starts), so the next call on that entry starts only after it ends, and a call that timed out while waiting never runs (ADR-0017).
