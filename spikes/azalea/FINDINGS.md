# Findings (Phase 1 azalea spike)

Short results per task. ADR-0008 is the curated record; this file keeps the evidence behind it. All runs used azalea `0.16.0+mc26.1`, `nightly-2026-08-21`, and the dev server from `deploy/compose.dev.yaml` (`itzg/minecraft-server:2026.9.2-java25`, vanilla 26.1, offline mode).

## P1.1 Local test server
- `just mc-up` pulls the pinned image, starts vanilla 26.1 and is healthy after about 40 s on the first run (the server jar is downloaded once per world volume).
- Only `127.0.0.1:25565` is published. RCON listens inside the container; `docker compose … exec minecraft rcon-cli <cmd>` works without knowing the random password.
- `LEVEL_TYPE=minecraft:flat` works. The log shows `ERROR: No key layers in MapLike[{}]` because itzg writes `generator-settings={}`, but the default flat preset is used anyway: grass at y −61 and bedrock at y −64 (checked with `execute if block`).
- **In Git Bash, set `MSYS_NO_PATHCONV=1`** before `docker compose exec … <path>`. Otherwise `/data/…` gets rewritten to a Windows path.
- `cargo run -- join 15`: `AfkBot1` logs in and spawns at `(2.5, -60.0, 5.5)`, and `rcon-cli list` shows `AfkBot1`. After `exit()` the process ends.

## P1.2 A client on a dedicated MC host thread
Setup (`src/host.rs`, `src/session.rs`): one OS thread with a current-thread runtime and a `LocalSet`; jobs arrive as `Send` closures over a bounded queue, run as `spawn_local`, and reply over a oneshot. Three ways to start a bot on it:

| Variant | How | Plugins | Gives us |
|---|---|---|---|
| A `join` | `Client::join` | fixed set: auto-reconnect and auto-respawn **on** | `(Client, UnboundedReceiver<Event>)` |
| B `builder` | `ClientBuilder` + handler that hands out the `Client` on `Init` | our choice | `Client` via handler; `start()` future |
| C `custom` | `Client::join`'s 20 lines rebuilt from public pieces: `App` + our plugins → `start_ecs_runner` (`#[doc(hidden)]`) → `StartJoinServerEvent` → `LocalPlayerEvents(tx)` → `Client::new` | our choice | `(Client, UnboundedReceiver<Event>, oneshot::Receiver<AppExit>)` |

Results (`cargo run -- host <variant> 3`, then `exit-race`, `host-builder-exit`):
- **`Client` is `Send + Sync + Clone`** (compile-time assert). `set_direction` and `position()` called from a tokio *worker* thread work, and the host thread sees the new direction. We still keep azalea calls on host threads (CLAUDE.md): every call takes the ECS `RwLock`, and that lock is held for a whole Update/GameTick.
- **The handoff works** for all three: three bots on one host thread, ~20 ticks/s each; the oneshot delivers the client to the multi-threaded runtime.
- **Nested `LocalSet` works.** B's `start()` creates its own `LocalSet` inside our host task, and three builders on one host thread run fine.
- **`exit()` tears down A and C cleanly.** The runner returns `AppExit::Success`, the World is cleared (0 entities), and the event channel closes. With C, `exit()` from another thread worked on the first call in 12 of 12 trials.
- **B (and therefore every `Swarm`) can deadlock on teardown. This is an azalea bug.**
  - Where: in `SwarmBuilder::start`, the client-handler loop holds `ecs.write()` and, when an event arrives for a bot whose state component is gone (the World was just cleared by `exit()`), logs `first_bot.username()`. `username()` reads the same ECS on the same thread, and `parking_lot` locks aren't reentrant: **self-deadlock** (`azalea/src/swarm/builder.rs` ~555–580).
  - Effect: the host thread hangs forever while holding the write lock, so any later `exit()` or getter from another thread hangs too.
  - Frequency: a race. In `host-builder-exit`, `start()` didn't return in 2 of 4 runs where `exit()` came from another thread, and 1 of 2 even without nesting; `exit-race builder` hung on its first trial.
- **Getters panic outside the game.** `client.username()` panics (`Our client is missing a required component: GameProfileComponent`) before login and after `exit()`. Keep the name ourselves; call getters only while the bot is known to be in a world.
- **`packet-event` floods the channel.** With nobody reading for 3 s, 61 events were dropped from a 256-slot channel; nearly all were `Event::Packet`. The bridge now counts packets and only forwards them on request. `fleet-mc` should build azalea without `packet-event`.
- `Init` is sent as soon as `LocalPlayerEvents` is inserted, **before** the TCP connect, so `ConnectionFailed` always comes after `Init`.
- tokio's `num_alive_tasks` doesn't count `LocalSet` tasks; our own counter only sees our jobs, not azalea's internal `spawn_local`s.

**Conclusion:** Variant C. It disables plugins like B, avoids B's teardown deadlock, and is the only variant that reports how the runner ended (`AppExit`, or the sender dropped on a panic). The cost is relying on `#[doc(hidden)] start_ecs_runner`, acceptable because azalea is pinned exactly and the ADR-0003 bump procedure re-checks it.
