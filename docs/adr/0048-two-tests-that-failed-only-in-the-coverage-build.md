# 0048. Two tests that failed only in the coverage build: the database's own last close, and a paused clock held over real sockets

- **Status:** Accepted. Decided under ADR-0025
- **Date:** 2026-11-25
- **Related:** Plan.md Phase 16, group A (the user's second commit); ADR-0015 (sqlx, time in tests), ADR-0017 (fleet-client's transport), ADR-0018 (`just dev-rotate-key` reads a `-wal` as a running server), ADR-0020 (U25: the live events socket over reqwest's upgrade); `crates/fleet-server/src/infra/sqlite/mod.rs`, `crates/fleet-server/tests/database.rs`, `crates/fleet-client/src/transport/mod.rs`

## Context
Two tests failed now and then in CI's `coverage` job alone, the slow, instrumented build that runs the whole workspace at once, and never in the plain test jobs or `just check`:

- **fleet-server's `database::a_closed_open_migrated_database_leaves_no_wal_or_shm`,** twice (main on 2026-11-10, PR #133 on 2026-11-22): after `Database::close()` returned, a `-wal` and a `-shm` were still beside the file. The reviewer's guess: SQLite removes them only at the last connection's close, and two pool connections closing at the same moment can each see the other still open.
- **fleet-client's `transport::tests::an_upgraded_connection_outlives_the_request_timeout`,** once (main on 2026-11-23, 9fa299b59): the request timed out after 0.4 s. The reviewer's guess: a real-socket test whose timeout is too tight for the instrumented build.

The user asked for the real cause of each and a deterministic fix, in the code for the first and with paused time or a test that doesn't depend on wall-clock speed for the second: no retries, no longer waits, each shown failing before its fix.

## W1. The database's last close is its own

### What was found
- **The failure reproduced** in the coverage build on Linux (`cargo llvm-cov nextest`, the pinned nightly, in a container under a project name of this run's own), with one CPU for eight runs at once: 2 failures in 1,200 runs, then 1 in 2,400. 1,200 runs on four CPUs, and 400 in a plain build on Windows, passed.
- **No handle outlived the close.** A diagnostic in the failing test listed the process's open file descriptors on the database, its `-wal` and its `-shm` right after `close()` returned: none. A second later both files were still there. So no connection was still open to remove them later: the last close skipped the removal.
- **It happens when two read connections are out as the close begins.** The diagnostic recorded the read pool's size and idle count at the close: the failure came at size 2 with none idle, which 250 passing runs also had, and never at any other size.
- **sqlx 0.9.0 closes such connections in their own return tasks, at the same moment.** A dropped `PoolConnection` spawns `return_to_pool` (sqlx-core `pool/connection.rs`), which starts with `if self.guard.pool.is_closed() { self.close().await; return false; }`. `Pool::close` marks the pool closed at once, then waits for every permit before it closes its idle connections one after another; but two return tasks that see the pool closed each close their own connection, side by side, each on its SQLite worker thread. sqlx-sqlite's worker drops its connection before it answers, so each close is complete when its task goes on, and nothing outlives `Pool::close`.
- **SQLite removes the files only at a close that finds no other connection open.** A closing connection takes an exclusive lock on the database to know it's the last; only then does it checkpoint and delete the `-wal` and `-shm` (`sqlite3WalClose`; wal.html: "When the last connection to a database closes, that connection does one last checkpoint and then deletes the WAL and its associated shared-memory file"). Two closes at once each see the other's lock, so neither removes them. That's the reviewer's guess, with the path that leads there: not the pool's own closes, which are one after another, but the return tasks'.
- **Why the coverage build:** the window is a test that hands two connections back right before the close, while both return tasks still wait for the runtime; the instrumented build on a loaded runner holds them there long enough.

### Decision
**`Database::close` ends with a connection of its own.** It closes the read pool, then the write pool, then opens one connection with the read pool's options (which never create the file), reads the schema so it has the WAL open, and closes it. Both pools have closed every connection of theirs by then, so this one is alone: its close takes the lock, checkpoints and removes both files. It's SQLite's own way to remove a WAL ("The only safe way to remove a WAL file is to open the database file using one of the sqlite3_open() interfaces then immediately close the database using sqlite3_close()", wal.html §4). It costs one open, one read and one close on the local disk, inside the bound every caller already puts on the close; a connection that can't open (the file is gone, the disk is full) leaves the files as a crash would, and the next open recovers them.

**It's in the code, not the test,** because the race is the server's: `serve`'s shutdown closes four read connections and the write connection, and a `-wal` left beside the dev database makes `just dev-rotate-key` refuse it as a running server (ADR-0018).

### Shown failing first
- **A test that forces the race:** `connections_handed_back_as_the_database_closes_leave_no_wal_or_shm` takes two read connections, reads with both, drops both and closes the database before the test's one thread runs their return tasks, 20 rounds a run. Before the fix it failed 30 runs of 30, most in the first round (Windows, a plain build). After it, 100 of 100 there.
- **The original test under the same conditions:** the coverage build on Linux, one CPU, eight runs at once. Before the fix 3 failures in 3,600 runs; after it, both tests 450 runs each in eight runs at once, 3,600 runs and 72,000 forced rounds, none failed.

### Alternatives considered
- **One read connection for `open_migrated`.** It fixes `vault status` alone; `serve`'s close has the same race.
- **Waiting until every connection is back before closing.** sqlx tells no caller when a return task ends; it would be a wait on a guess.
- **`PRAGMA wal_checkpoint(TRUNCATE)` and `journal_mode=DELETE` before the close.** TRUNCATE leaves both files; DELETE needs the database alone and rewrites its header, which the test's "file unchanged" (and a restore's checks) would read as a change.
- **Retrying the close, or the test.** Ruled out by the user.

## W2. A paused clock held still over real sockets

### What was found
- **The test ran the request on the wall clock with a 100 ms timeout.** Its server, a task on the test's own one-thread runtime, accepts the connection and answers the WebSocket handshake; the client must have the 101 within the transport's whole-request timeout, 100 ms, to show that the upgraded connection outlives it. On a loaded runner in an instrumented build, the process can stall for longer than that, and 0.4 s is a timer that fired late on a starved runtime.
- **Reproduced:** the same test with its server stalled for 0.3 s before the handshake fails with CI's exact error, `TimedOut(Some(reqwest::Error { kind: Request, source: TimedOut }))`.

### Decision
**The test runs on tokio's paused clock, and holds it still while the server works.** tokio moves a paused clock on to the next timer whenever the runtime waits with nothing to do, real sockets included, so paused time alone would fire the timeouts at once. It doesn't while a blocking task runs (`spawn_blocking`), so the test holds one, waiting on a channel, for as long as its server works:

- the server runs on a thread of its own, with tungstenite's blocking handshake and real sockets, bounded by their own timeouts (5 s);
- reqwest's whole-request timeout and the transport's own run on the paused clock, which moves only when the test says: once the connection is upgraded, `tokio::time::advance` moves it ten times the request timeout, then the server sends its ping and its close, and the client must read both;
- the hold ends when the test drops its sender, or after 5 s of the wall clock as a bound.

So no timeout sees how fast the machine runs, only the time the test advances. **Shown:** with the server stalled for 0.3 s before the handshake and 0.3 s after it, the new test passes, where the old one failed at the first stall; 300 runs of 300 in a stress run.

**The testing rules gain the pattern** (`.claude/rules/testing.md`): a test over real sockets whose subject is a timeout may run on a paused clock held still by a blocking task, so it depends on no machine's speed.

### Alternatives considered
- **A longer timeout.** Ruled out by the user, and it would only move the edge.
- **An in-memory pipe instead of sockets.** reqwest takes no stream of the caller's; the upgrade is what's tested.

## Consequences
- **Every close of the database opens one more connection,** alone, after the pools: a few milliseconds of local file work, inside the bounds `serve` and `migrate` already put on the close.
- **A new way to test a timeout over real sockets,** with its own rule; the old one (real time, short bounds) still holds for every other socket test.
