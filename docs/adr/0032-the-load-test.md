# 0032. The load test

- **Status:** Accepted; the test is still growing (see "Still to come")
- **Date:** 2026-11-12
- **Related:** Plan.md Phase 13 (P13.4), §5, §8; ADR-0019 (S57, S58, S72: the loopback harness, the session limits), ADR-0020 (S30, S31, S35, S40, S41, S43, S44, S74: the live events, the bus, the ingest), ADR-0022 (S9), ADR-0025, ADR-0027 (group E), ADR-0031 (the time limit a test)

## Context
P13.4 asks for a load test: 200 or more fake bots on one agent and 20 WebSocket clients, with CPU, memory and an event's latency measured, and the hot spots fixed. Earlier phases left it notes: load the conductor and the ingest, count what a commit costs the open sockets, measure a retention run at the ingest's peak, scrape both metrics ports meanwhile, and measure how fast sessions are granted after a restart.

The pieces existed. fleet-integration's loopback harness (ADR-0019, S72) runs the real server through serve's own wiring and a real managed agent in one process, the agent's bots on fleet-testkit's fake connector, and every user through fleet-client. It had one user, the Owner, and the production rate limits.

Everything below is decided under ADR-0025.

## Decision

### Where it runs
- **A `load` module in fleet-integration's test binary,** on the loopback harness: one server, one agent with 200 bots on the fake connector, each bot an offline account of the Owner's, and 20 Members with a live events socket each, fleet-client's own. Nothing is mocked between a bot's session and a viewer's socket: the agent's conductor and outbox, mTLS on loopback, the server's ingest, its database, the bus, the sockets' projection and fleet-client's reader are the real ones.
- **Its tests are named `load_…`.** The default and `ci` nextest profiles leave them out, a `load` profile runs them alone, one at a time, and `scripts/nextest-config.test.mjs` holds both. `just load` runs that profile with the output shown: the report is the lines that start with `LOAD`.
- **It measures and prints; it asserts only what must hold at any load:** every line is stored or counted as dropped by the server, every stored line reaches each of its viewers, no socket is told to fetch everything again, and every session takes what it's handed.
- **Not in `just check` or CI:** it takes about a minute and its numbers mean something only on a machine doing nothing else.

### The world it loads
- **200 bots on one agent** of capacity 250, started through the API as a user starts them.
- **20 Members,** each registered with an invite of the Owner's and logged in with 2FA, each given 20 of the bots, half at View and half at Control: 400 grants, two viewers a bot, the shape ADR-0020's S41 measured with.
- **The setup's own rate limits are lifted,** the others stay. The Owner creating 200 accounts from one address spends the production per-address limit (20 a second, 40 at once) in the first second; so do 20 registrations (5 an hour an address), the Members' logins (5 a minute an address) and 400 grants by name (10 an hour a user). The harness's server takes `Limits::LoadSetup` for these five. The limits on what the test loads (a bot's chat rate, its actions, the sockets a user may hold, the agent's connects) are the production ones.

### What it measures
- **The setup:** how long 200 accounts, 200 starts and 20 Members with 400 grants take through the API, and when every bot is live.
- **An event's latency:** a bot hears a chat line (the fake session reports it) at a known instant; each viewer's socket notes when the `chat` frame with that text arrives.
- **The ingest's counts,** read from the server's own series as its metrics port renders them: rows written, lines dropped.
- **The sockets:** `ready` fetches, resyncs, frames a client couldn't read, disconnects.
- **The process:** CPU time and memory of the test's own process, read from the OS. The server, the agent, all 21 clients and the harness run in it, in a debug build, so both are upper bounds for the server's and the agent's together.

### One new crate, in the tests only
**`sysinfo` 0.40.1** (2026-10-30, MIT), a dev-dependency of fleet-integration alone, without default features and with `system` only: the processes, no disks, networks, GPUs or users. It reads `accumulated_cpu_time` and `memory` of one process. The two newer releases are a day old (2026-11-11), so the pin is 0.40.1. `Cargo.lock` gains `sysinfo`, `ntapi` 0.4.3 and `objc2-io-kit` 0.3.2 (macOS only, never built here); `windows` and `winapi` were there already. Reading `/proc` would cover Linux alone, and the development PC is Windows; a shell command a reading would add a process's start to what's measured.

## What it measured
On the development PC (Windows, 8 threads), a debug build, the test's runtime on one thread, 2026-11-12:

**Setup:** 200 accounts and bots in 2.5 s, their 200 starts asked in 1.7 s and every bot live by then; 20 Members with 400 grants in 2.1 s. That took 5.3 s and 2.5 s of CPU, a little over one core.

**Chat, a bot's line to its viewers' sockets:**

| Rate, all bots together | Arrived | p50 | p95 | max | The server dropped |
|---|---|---|---|---|---|
| 20 lines/s | all | 264 ms | 489 ms | 514 ms | 0 |
| a burst of 1000 at once | all | 616 ms | 627 ms | 630 ms | 0 |
| 500 lines/s | all | 294 ms | 509 ms | 549 ms | 0 |
| 1000 lines/s | all | 328 ms | 553 ms | 583 ms | 0 |
| 2000 lines/s | 96 % | 378 ms | 610 ms | 664 ms | 4 % |
| 4000 lines/s | 50 % | 458 ms | 592 ms | 874 ms | 50 % |

- **The latency is the ingest's interval.** The server writes what waits every 500 ms (`INGEST_INTERVAL`), so a line waits up to that long; a quarter of a second in the middle.
- **The drops begin where the design says:** one agent's stream may hold 1024 rows (`STREAM_ROWS`), and a tick writes 1000 (`BATCH_ROWS`), so about 2000 lines a second is what one agent can bring. Past it the server drops, counts every drop, and stores exactly its 2000 a second.
- **Nothing was lost quietly:** at every rate the rows written plus the lines dropped are the lines sent, and the arrivals are the rows written times two viewers.
- **No socket fell behind:** no resync, no disconnect, no unreadable frame, at any rate.

**The process** (server, agent, 21 clients and the harness together):

| Phase | CPU, of one core | Memory held |
|---|---|---|
| idle: 200 bots online, 20 sockets open | 1 % | 71 MiB |
| 20 lines/s | 6 % | 72 MiB |
| 500 lines/s | 13 % | 74 MiB |
| 1000 lines/s | 20 % | 77 MiB |
| 2000 lines/s | 28 % | 82 MiB |
| 4000 lines/s | 37 % | 94 MiB |

Each rate's share includes the two quiet seconds the test waits after its five of sending.

**No hot spot so far.** Chat, the heaviest path, stays within one thread's reach in a debug build at twice the rate the server accepts.

## Still to come
What a commit costs the open sockets (the rights' rebuilds and the reads done again); a retention run at the ingest's peak; both metrics ports scraped under load; a restart of the server and the agent; the same numbers in a release build and on a runtime with several threads, as production has; chat's stored bytes a line.

## Consequences
- **`just load` is how a change to the ingest, the bus or the sockets is weighed:** its report before and after.
- **The harness has Members now** (`Member::join`), grants (`Owner::grant`) and a bot's own session events (`Ledger::emit`), for any case that needs a second user.
- **The numbers are one PC's,** in a debug build: they order the costs; they don't size a host.

## Alternatives considered
- **The real binaries as processes, measured from outside:** the agent's binary has no fake connector, so 200 bots would need a Minecraft server able to hold them; that measures azalea and the Minecraft server more than the fleet's own code.
- **A benchmark crate with its own harness:** a second way to wire the server and the agent, to drift from serve's.
- **Lifting every rate limit for the test:** the limits on what is loaded are part of what's measured.
- **In CI, with thresholds:** the runners' speed varies more than the differences worth catching.
