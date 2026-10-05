# 0004. One tokio task per bot, azalea on MC host threads

- **Status:** Proposed. The azalea spike confirms or revises it in P1.10 (ADR-0008).
- **Date:** 2026-10-05
- **Related:** Plan.md §3.3, §6, Phase 1, Phase 3, Phase 4; ADR-0002

## Context
An agent runs up to about 50 bots. A fault in one bot must never affect another (Plan.md §3.6, "fail small"). azalea has two constraints:
- it needs a `LocalSet`, so its clients can't simply run on the multi-threaded tokio runtime
- it has no `catch_unwind` around its ECS, so a panic or hang inside azalea takes down whatever shares its executor

## Decision
**Bot actors.** Every bot is one actor: a tokio task on the agent's multi-threaded runtime that owns all of that bot's state.
- It communicates only through bounded channels: an `mpsc` inbox, a `watch` snapshot and `broadcast` events.
- Bots share no mutable state.
- A `Supervisor` owns the actors (`JoinSet` / `TaskTracker`). It restarts an actor that panics, at most 5 times in 10 minutes before `Failed(CrashLoop)`.

**MC host threads.** The azalea clients run on a small pool of OS threads managed by `fleet-mc` (`McHostPool`).
- Each thread has a current-thread runtime and a `LocalSet`.
- A new connection goes to the least-loaded thread through a bounded queue.
- The actor talks to its session only through the `MinecraftConnector` / `SessionHandle` / `SessionEvents` ports.

**Recovery layers:**

| Fault | Recovery |
|---|---|
| azalea hangs or its ECS panics | Watchdog: no `Tick` within `watchdog_timeout` while Online → tear down and reconnect |
| A bot actor panics | Supervisor restart |
| A host thread dies | The pool respawns it; its bots see `Disconnected` and follow the normal backoff path |

## Consequences
- **Async-native actors.** They use tokio timers and channels, so they're fully testable with fakes and paused time.
- **Contained blast radius, with one open question.** A panic inside azalea affects at most the bots on one host thread. How many bots share a thread, and whether one panic kills only one client or a whole shard, is still open: P1.6 and P1.7 measure it.
- **Escape hatch.** If in-process isolation turns out not to be enough, a process-per-bot connector can implement the same port without touching the runtime.

## Alternatives considered
- **All azalea clients on the main runtime.** Not possible: azalea needs a `LocalSet`.
- **One OS thread per bot.** Strongest isolation within the process, but 50 runtimes and threads per agent. P1.7 measures the cost before this is rejected for good.
- **One process per bot.** Strongest isolation of all, but heavier: memory, process management, and IPC for every event. Kept as the escape hatch.
- **Swarm shards (several bots in one azalea `Swarm`).** Fewer resources, but one panic kills the whole shard. P1.7 compares it.
