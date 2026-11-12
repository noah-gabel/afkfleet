# 0032. The load test

- **Status:** Accepted
- **Date:** 2026-11-12
- **Related:** Plan.md Phase 13 (P13.4), §5, §8; ADR-0019 (S22, S57, S58, S72: the loopback harness, the session limits), ADR-0020 (S30, S31, S35, S40, S41, S43 to S45, S74, R57: the live events, the bus, the ingest, the retention), ADR-0022 (S9, the metrics ports), ADR-0025, ADR-0027 (group E), ADR-0031 (the time limit a test)

## Context
P13.4 asks for a load test: 200 or more fake bots on one agent and 20 WebSocket clients, with CPU, memory and an event's latency measured, and the hot spots fixed. Earlier phases left it notes: load the conductor and the ingest, count what a commit costs the open sockets, measure a retention run at the ingest's peak, scrape both metrics ports meanwhile, and measure how fast sessions are granted after a restart.

The pieces existed. fleet-integration's loopback harness (ADR-0019, S72) runs the real server through serve's own wiring and a real managed agent in one process, the agent's bots on fleet-testkit's fake connector, and every user through fleet-client. It had one user, the Owner, and the production rate limits.

Everything below is decided under ADR-0025.

## Decision

### Where it runs
- **A `load` module in fleet-integration's test binary,** on the loopback harness: one server, one agent with 200 bots on the fake connector, each bot an offline account of the Owner's, and 20 Members with a live events socket each, fleet-client's own. Nothing is mocked between a bot's session and a viewer's socket: the agent's conductor and outbox, mTLS on loopback, the server's ingest, its database, the bus, the sockets' projection and fleet-client's reader are the real ones.
- **Its tests are named `load_…`.** The default and `ci` nextest profiles leave them out, a `load` profile runs them alone, one at a time, and `scripts/nextest-config.test.mjs` holds both. `just load` runs that profile with the output shown: the report is the lines that start with `LOAD`.
- **It measures and prints; it asserts only what must hold at any load:** every line is stored or counted as dropped by the server, every stored line reaches each of its viewers, no socket is told to fetch everything again while chat flows, every session takes what it's handed, every scrape is answered, and a retention run fails no phase.
- **Not in `just check` or CI:** it takes about a minute and a quarter, and its numbers mean something only on a machine doing nothing else. `cargo nextest run -p fleet-integration --profile load --cargo-profile release --no-capture` runs it as a release build.

### The world it loads
- **200 bots on one agent** of capacity 250, started through the API as a user starts them.
- **20 Members,** each registered with an invite of the Owner's and logged in with 2FA, each given 20 of the bots, half at View and half at Control: 400 grants, two viewers a bot, the shape ADR-0020's S41 measured with.
- **The setup's own rate limits are lifted,** the others stay. The Owner creating 200 accounts from one address spends the production per-address limit (20 a second, 40 at once) in the first second; so do 20 registrations (5 an hour an address), the Members' logins (5 a minute an address) and 400 grants by name (10 an hour a user). The harness's server takes `Limits::LoadSetup` for these five. The limits on what the test loads (a bot's chat rate, its actions, the sockets a user may hold, the agent's connects) are the production ones.
- **Three more options of the harness, off for every other case:** the server's metrics port and the agent's metrics endpoint, each on a loopback port of its own; and the retention's runs left to the case (the task's own a day apart, each bot keeping 100 lines, the least a config allows), so a run starts when the case says and has work to do.

### What it measures
- **The setup:** how long 200 accounts, 200 starts and 20 Members with 400 grants take through the API.
- **An event's latency:** a bot hears a chat line (the fake session reports it) at a known instant; each viewer's socket notes when the `chat` frame with that text arrives. Steady, in a burst, and at 500, 1000, 2000 and 4000 lines a second.
- **The ingest's counts,** from the server's own series as its metrics port renders them: rows written, lines dropped.
- **Both metrics ports,** scraped every quarter second at 1000 lines a second.
- **A retention run** started while chat flows at 1000 lines a second, every bot about a hundred lines past its bound.
- **What commits cost the sockets:** every bot stopped through the API, then started again, with the sockets open. The server logs each read of the sockets' rights with its time, and each read a socket makes again because a commit landed meanwhile (two `debug` lines, added for this).
- **Restarts:** the agent's, with every bot running; then the server's and the agent's together.
- **The process:** CPU time and memory of the test's own process, read from the OS after each phase. The server, the agent, every client and the harness run in it, so both are upper bounds for the server's and the agent's together.

### Two new crates in the tests only, and two more named
- **`sysinfo` 0.40.1** (2026-10-30, MIT), a dev-dependency of fleet-integration alone, without default features and with `system` only: the processes, no disks, networks, GPUs or users. It reads `accumulated_cpu_time` and `memory` of one process. The two newer releases are a day old (2026-11-11), so the pin is 0.40.1. `Cargo.lock` gains `sysinfo`, `ntapi` 0.4.3 and `objc2-io-kit` 0.3.2 (macOS only, never built here); `windows` and `winapi` were there already. Reading `/proc` would cover Linux alone, and the development PC is Windows; a shell command a reading would add a process's start to what's measured.
- **fleet-integration now names `metrics-exporter-prometheus` and `tokio-util`,** both the workspace's own: the recorder the agent's main.rs installs for its endpoint, and the stop token a retention run takes. One edge each in the lock, no crate.

## What it measured
On the development PC (Windows, 8 threads), the test's runtime on one thread, 2026-11-12. Debug is `just load`; release is the same test built with `--cargo-profile release`.

**Setup:**

| | Debug | Release |
|---|---|---|
| 200 accounts and bots created | 2.6 s | 0.55 s |
| 200 starts asked, every bot live by then | 1.7 s | 0.37 s |
| 20 Members registered and logged in, 400 grants | 2.1 s | 1.1 s |

**Chat, a bot's line to its viewers' sockets** (debug / release):

| Rate, all bots together | Arrived | p50 | p95 | max | The server dropped |
|---|---|---|---|---|---|
| 20 lines/s | all | 265 / 251 ms | 495 / 482 ms | 514 / 506 ms | 0 |
| a burst of 1000 at once | all | 603 / 492 ms | 617 / 494 ms | 620 / 495 ms | 0 |
| 500 lines/s | all | 296 / 265 ms | 509 / 486 ms | 548 / 513 ms | 0 |
| 1000 lines/s | all | 329 / 278 ms | 557 / 499 ms | 592 / 529 ms | 0 |
| 2000 lines/s | 97 / 98 % | 377 / 294 ms | 615 / 506 ms | 659 / 549 ms | 3 / 2 % |
| 4000 lines/s | 53 / 50 % | 442 / 398 ms | 564 / 511 ms | 881 / 793 ms | 47 / 50 % |

- **The latency is the ingest's interval.** The server writes what waits every 500 ms (`INGEST_INTERVAL`), so a line waits up to that long: a quarter of a second in the middle, half a second at most, in a debug build as in a release one.
- **The drops begin where the design says:** one agent's stream may hold 1024 rows (`STREAM_ROWS`) and a tick writes 1000 (`BATCH_ROWS`), so about 2000 lines a second is what one agent can bring. Past it the server drops, counts every drop, and stores its 2000 a second.
- **Nothing was lost quietly:** at every rate the rows written plus the lines dropped are the lines sent, and the arrivals are the rows written times two viewers.
- **No socket fell behind:** no resync and no unreadable frame at any rate. The only disconnects in the run are the twenty of the server's own restart.

**The process** (server, agent, every client and the harness together):

| Phase | CPU, of one core, debug / release | Memory held, debug / release |
|---|---|---|
| idle: 200 bots online, 20 sockets open | 2 / 0 % | 72 / 54 MiB |
| 20 lines/s | 4 / 1 % | 71 / 54 MiB |
| 500 lines/s | 14 / 3 % | 74 / 56 MiB |
| 1000 lines/s | 19 / 4 % | 77 / 59 MiB |
| 2000 lines/s | 28 / 8 % | 82 / 64 MiB |
| 4000 lines/s | 37 / 11 % | 93 / 75 MiB |
| 200 stops and 200 starts through the API | 128 / 154 % | 100 / 81 MiB |

Each rate's share includes the two quiet seconds the test waits after its five of sending. The most memory any reading found was about 100 MiB in the debug run and 82 MiB in the release run.

**Both metrics ports at 1000 lines/s:** 30 scrapes each, every one answered. The server's answer is 9455 bytes, in 2.3 ms at the median and 4.2 ms at most (release 0.9 and 2.0 ms); the agent's is 3308 bytes, in 2.2 and 4.1 ms (release 0.7 and 1.6 ms).

**Chat on disk:** about 28,700 stored lines of about 20 characters grew the database and its write-ahead log by 6.9 MiB in the debug run and 3.1 MiB in the release run: at most 245 and 112 bytes a line. The log keeps its largest size, so what was already moved into the database counts twice; the true figure is nearer the lower one. At the default bound of 50,000 lines a bot, 200 bots' chat is of the order of 1 to 2 GB.

**Restarts** (debug / release):
- **The agent's, every bot running:** every viewer is shown its bots as last known 95 / 14 ms after the stop began, every bot is in a new session after 787 / 104 ms, and every viewer is shown it online as reported in the same instant. The server read the sockets' rights twice.
- **The server's and the agent's together:** the server is back after 80 / 38 ms, every bot in a new session after 430 / 95 ms. The new server granted the 200 sessions at about 570 / 3500 a second. Every socket is live again after 2.0 s in both builds, which is fleet-client's wait before it connects again.

## The two hot spots

### A retention run walked the bots one commit at a time: fixed
The chat-by-size phase deleted each bot's lines past its bound in a batch of its own, and pauses 100 ms after every committed batch (ADR-0020, S45). With every bot a little past its bound, which is the steady state of a busy fleet, a run over 200 bots was 200 batches: **21.7 s** in the load test, almost all of it pauses. At about 550 bots a run would not end within the minute between runs.

**A batch now takes rows of as many bots as fit.** The phase reads every bot's cut first, with no write open, then deletes through the cuts in batches of at most 1000 rows; a bot whose rows don't fit goes on in the next batch. The same run takes **1.3 s in 10 batches** (release 1.2 s).

What S45 and R57 decided still holds: the sockets see at most one commit a pause, a batch is still one write transaction of deletes of one table and at most 1000 rows, the write connection is free between batches, and a line written after its bot's cut was read still stays. The service still has one place that opens a batch (both loops go through one helper), which the source test of R57 checks. Two tests, red first: five bots three lines over share one batch (they took five), and four bots 600 lines over go in three batches (they took four).

**Chat didn't notice the run either way:** at 1000 lines a second every line arrived, at the same latency as without one.

### Every commit makes the sockets' rights be read again: measured, left as it is
While any socket has something to handle, a commit moves the count its rights are judged by, and the server reads the whole snapshot again: every bot, grant and user (ADR-0020, S30). One read serves every socket.

- **A read takes 5.7 ms in a debug build and 1.2 to 1.5 ms in a release build** at 200 bots, 400 grants and 22 users, about 2 µs a row.
- **It shows when one client makes many separate writes.** 200 stops through the API, 20 sockets open: 6.2 s in debug and 0.98 s in release, 4.9 ms a stop, against 1.8 ms for a start before any socket was open. A stop is about three commits; the rights were read 595 / 544 times, and the sockets did 828 / 161 reads again because a commit landed while they read (S43). The reads are between half and two thirds of that time.
- **It doesn't show in what a fleet does by itself.** The ingest commits twice a second. An agent's restart with 200 bots cost two reads. A retention run's commits cost a read only when a socket had an event to handle: 13 for a whole run.

**Left as it is.** The snapshot is what decides who sees which bot, a read is a millisecond, and the one case that multiplies it, a script writing hundreds of times a second through the API, is slowed from 2 ms a write to 5. Making it cheaper means knowing which commits can't change the rights, by a version of the access tables kept by triggers or by SQLite's update hook; a write that should move it and doesn't would leave a revoked viewer watching. That trade isn't worth a few milliseconds. `just load` prints the count and the time of the reads, so a fleet large enough to change this is seen there first.

## Not measured, and why
- **Microsoft's refresh rate after a restart** (ADR-0019's note: about 1.3 refreshes a second an agent). The load test's bots are offline accounts, which need no refresh, so the 570 sessions a second above are the server's own work alone. With Microsoft accounts the pace is Microsoft's and the server's deliberate one; measuring it needs real accounts, which only the user's manual checks have.
- **A cap on gRPC connections an address** (ADR-0019, S22's residual). Nothing here called for one: one agent holds one connection, and the connects' own limit was never reached.
- **A runtime with several threads,** as production has. The harness logs through a subscriber set for one thread, and the test runs everything on it; every number above is therefore one core's.

## Consequences
- **`just load` is how a change to the ingest, the bus, the sockets or the retention is weighed:** its report before and after.
- **The harness has Members now** (`Member::join`), grants (`Owner::grant`), a bot's own session events (`Ledger::emit`), the server's series (`Server::series`), both metrics ports and a retention run by hand, for any case that needs them.
- **The retention's chat-by-size phase commits less often:** a `retention.purge` summary's counts are the same, the batches a run's metric shows are fewer.
- **Two `debug` lines more** when sockets are open: one a read of the rights, one a read done again.
- **The numbers are one PC's:** they order the costs and show where the limits are; they don't size a host.

## Alternatives considered
- **The real binaries as processes, measured from outside:** the agent's binary has no fake connector, so 200 bots would need a Minecraft server able to hold them; that measures azalea and the Minecraft server more than the fleet's own code.
- **A benchmark crate with its own harness:** a second way to wire the server and the agent, to drift from serve's.
- **Lifting every rate limit for the test:** the limits on what is loaded are part of what's measured.
- **In CI, with thresholds:** the runners' speed varies more than the differences worth catching.
- **A shorter pause after a small retention batch,** instead of batches that span bots: the commits a second the sockets see would no longer be bounded, which is what the pause is for.
- **New metrics series for the rights' reads,** instead of `debug` lines: two more series on every server for a number only the load test reads.
