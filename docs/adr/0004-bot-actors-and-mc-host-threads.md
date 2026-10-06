# 0004. One tokio task per bot, azalea on MC host threads

- **Status:** Accepted, revised by the azalea spike (P1.10). The host-thread part changed: **one host thread per bot** instead of a shared pool. Details and evidence are in [ADR-0008](0008-azalea-integration.md).
- **Date:** 2026-10-05
- **Related:** Plan.md §3.3, §6, Phase 1, Phase 3, Phase 4; ADR-0002, ADR-0008

## Context
An agent runs up to about 50 bots. A fault in one bot must never affect another (Plan.md §3.6, "fail small"). azalea has two constraints:
- it needs a `LocalSet`, so its clients can't simply run on the multi-threaded tokio runtime
- it has no `catch_unwind` around its ECS, so a panic or hang inside azalea takes down whatever shares its executor

The Phase 1 spike measured both (ADR-0008, P1.6 and P1.7):
- A panic stays inside one bot's Bevy App.
- A **hang** blocks the whole OS thread that runs the App. With Bevy's default multi-threaded executor it can also starve the process-wide compute pool and freeze **every** bot.
- One App per bot costs about 4 MiB and under 1 % of a core per bot with Bevy's single-threaded executor. Giving every bot its own thread adds little on top.

## Decision
**Bot actors** (unchanged). Every bot is one actor: a tokio task on the agent's multi-threaded runtime that owns all of that bot's state.
- It communicates only through bounded channels: an `mpsc` inbox, a `watch` snapshot and `broadcast` events.
- Bots share no mutable state.
- A `Supervisor` owns the actors (`JoinSet` / `TaskTracker`). It restarts an actor that panics, at most 5 times in 10 minutes before `Failed(CrashLoop)`.

**MC host threads** (revised). Every bot session runs on **its own** OS thread, owned by `fleet-mc`.
- The thread has a current-thread runtime and a `LocalSet`, and runs one azalea App (its own Bevy World) with **Bevy's single-threaded executor**.
- A connect starts the thread. Session teardown (`exit()`, then dropping every handle) ends it.
- The actor talks to its session only through the `MinecraftConnector` / `SessionHandle` / `SessionEvents` ports. Every azalea call runs on the session's host thread, reached over a bounded queue.
- No Swarms.

**Recovery layers:**

| Fault | Detected by | Recovery |
|---|---|---|
| Panic in azalea's ECS (or ours) | the runner's `AppExit` receiver fails **at once** | Drop the session (World, socket, thread), then transient disconnect → backoff |
| Hang in azalea's ECS | no `Tick` for `watchdog_timeout` while Online, and the host thread no longer answers | **Abandon** the thread (it can't be killed) and reconnect on a fresh one. Count abandoned threads; above a small limit, exit the process and let Docker restart it |
| Server frozen or link silently dead | no packet / `KeepAlive` for 30 s while Online (ticks don't stop, they're client-side) | Tear down, transient disconnect |
| A bot actor panics | Supervisor sees `JoinError::is_panic()` | Restart from the last spec |

## Consequences
- **Async-native actors.** They use tokio timers and channels, so they're fully testable with fakes and paused time.
- **Blast radius of one bot.** A panic or a hang affects exactly one bot. A hang leaks one thread until the process restarts.
- **Cost.** At 50 bots: 222 MiB, 0.43 cores and 58 threads (P1.7). That's comfortable for a small VPS.
- **`McHostPool` (P3.2) changes** from "N threads, least-loaded placement, respawn" to "a thread per session, abandon on hang". Its tests change with it; Plan.md P3.2 was updated in the Phase 1 PR.
- **Escape hatch.** If in-process isolation turns out not to be enough (e.g. frequent hangs), a process-per-bot connector can implement the same port without touching the runtime.

## Alternatives considered
- **All azalea clients on the main runtime.** Not possible: azalea needs a `LocalSet`.
- **A small pool of shared host threads** (this ADR's original decision). About the same cost (195 MiB and 0.34 cores at 50 bots on 4 threads), but a hang freezes every bot on its thread.
- **One process per bot.** Strongest isolation of all, but heavier: memory, process management, and IPC for every event. Kept as the escape hatch.
- **Swarm shards (several bots in one azalea `Swarm`).** Cheapest in memory, but one panic kills the whole shard, and a Swarm's teardown can self-deadlock (an azalea bug, ADR-0008).
