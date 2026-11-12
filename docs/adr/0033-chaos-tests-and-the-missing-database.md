# 0033. Chaos tests, and a server that refuses a missing database

- **Status:** Accepted
- **Date:** 2026-11-12
- **Related:** Plan.md Phase 13 (P13.5), §5, §6; ADR-0005 (SQLite's pragmas), ADR-0015 (rule 8, the database's opening), ADR-0019 (S23, S32, S40, S45, S63, S64, S67, S72, U16: the control plane and the loopback harness), ADR-0020 (S45, U20: retention), ADR-0022 (S2 to S8, S19, S25, S40: backups, the smoke tests, restore), ADR-0025, ADR-0027 (P13.6's renewal; group F), ADR-0031 (the time limit a test), ADR-0032 (the harness's options)

## Context
P13.5 asks for chaos tests: kill and restart the server's, the agent's and the Minecraft server's containers, partition the agent from the server, fill the database's volume, and check that everything recovers and nothing is corrupted. Earlier phases left it notes (S23, S32, S45, S63, S64, S67, U16's crash case), and the user added to the list when the group started:

- **Real faults, not mocks:** `SIGKILL` as well as a graceful stop, a real partition, a really full volume.
- **A kill in the middle of P13.6's renewal:** the server between signing and its commit, the agent between replacing its certificate file and swapping its identity. Afterwards the agent is on its old certificate or its new one, never locked out, and the server holds no certificate the agent can't use.
- **A kill during a backup and during a retention run:** a half-written backup never counts as one, and the next run finishes the job.
- **After every fault, not only "it came back":** SQLite's `integrity_check` passes; every write the API acknowledged is there with its audit entry; no bot ever runs on two agents, and every bot returns to its desired state; the vault opens every stored secret; a backup taken after recovery passes restore's own checks.
- **The full disk:** reads keep working, writes fail with the generic error and a log line, retention and backups fail loudly in their metrics, and everything resumes with no manual repair once there's room. Only a size-capped volume is ever filled, never Docker's real disk.
- **The missing database:** decide whether `serve` refuses to start in production when its database file is missing, instead of creating an empty one. Their reviewer recommended that it refuse.
- **Microsoft is always the fake.**

Everything below is decided under ADR-0025.

## Decision

### A missing database stops the start
- **Outside dev mode, `serve` never creates its database.** A missing file is exit 1, one `error` line, "the server can't start", before anything listens:
  ```text
  the database can't be opened: the database /data/afkfleet.db doesn't exist, and this command never creates one: on a first install, `afkfleet-server migrate` creates it (`setup.sh init` runs it); on a server that ran before, the volume that holds it isn't mounted at /data
  ```
  It names the file and the folder its volume belongs at, and both ways there.
- **Why:** a data volume that isn't mounted where it was (a renamed compose project, a removed volume, a new host without a restore) looks like a missing file. A server that created one there ran as an empty fleet: no user, every agent refused for good (exit 5, so their containers loop), every account and its sealed tokens gone from view, and nothing in the log but a normal start. The folder was already required (ADR-0015); a named volume always gives one, so that check never fired for this.
- **`migrate` is the one command that creates it.** `setup.sh init` runs it through the `maintenance` service after the PKI and before the server's first start (`ok: the database, in the data volume`), and stops there when it fails. A restored state has its database already, so `init` on one skips the step, as it skips the key and the PKI.
- **`user create-owner` holds the same rule.** It opened the database as `serve` did; an Owner created in a database the command made itself is the same empty fleet, with a login.
- **Dev mode keeps creating it,** for both, so `just dev-server` and `just dev-owner` need no step before them. The `dod-server` recipe, which isn't dev mode, runs `migrate` first.
- **An existing file is taken as it is and migrated forward,** an empty one too: that is what `migrate` leaves when it's stopped right after creating the file, and what an update starts from.
- **Restore is unchanged:** it puts the backup in place, runs `migrate`, and starts the server; on a new host `init` then finds the restored state.
- **In the code:** `sqlite::open_existing` beside `open` and `open_migrated`; `cli::open_database(config)` picks `open` in dev mode and `open_existing` otherwise, and `serve` and `create-owner` call it. `OpenError::MissingFile` says the two ways there for every command that never creates a database.
- *Rejected:* refusing an empty or unmigrated file too (it breaks nothing an operator does today, but makes an interrupted `migrate` need a second command where a start now finishes it); a marker file beside the database (one more thing a restore must carry); a setting to allow creation in production (the wrong default one line away); refusing in dev mode too.

### `afkfleet-server database check`
- **SQLite's own `integrity_check`, as a command.** The images hold no `sqlite3`, the chaos tests ask this after every fault, and an operator after a crash or a full disk needs the same answer.
- **Read-only, on a connection of its own:** beside a running server (`docker compose exec server …`) or with it stopped (`docker compose run --rm maintenance …`). It never creates, migrates or checkpoints, and reads no key. A `-wal` a killed server left is read through and stays for the next start.
- **Its exits:** 0 sound, one line on stdout; 1 it couldn't run (a missing database among the reasons, never an empty one that reads as sound); 2 usage; 3 damaged, with SQLite's findings on stderr, at most 100 lines, which name tables, indexes, pages and row numbers and never a row's content. A file SQLite can't read as a database at all (`SQLITE_CORRUPT`, `SQLITE_NOTADB`) is damage too.
- **It leaves two files beside a database nobody has open:** SQLite's `-wal`, holding nothing, and its `-shm`. Every connection to a WAL database makes them, a read-only one can't remove them, and the next start does. A test holds that the database's bytes don't change and nothing is written into the `-wal`.
- **Rule 8:** `PRAGMA integrity_check` goes through `sqlx::query_scalar!`, as the backup's `quick_check` does.
- *Rejected:* `foreign_key_check` beside it (no crash can break a foreign key that every connection enforces; it would be noise in the answer); opening the file `immutable` to leave nothing behind (it takes no lock, so a server starting meanwhile could make a sound database read as damaged); a busybox image with `sqlite3` in the tests alone (another image to pin, and nothing for an operator).

### Two tiers of faults
A container can be killed, but not at a chosen statement; and the production images can neither put the clock 40 days back, which a renewal needs, nor talk to anything but the real Microsoft. So the faults are in two places, both in fleet-integration, both real:

| | The chaos tests (`smoke::chaos`) | The crash cases (`crash`) |
|---|---|---|
| What runs | The production compose project, from both images, with a Minecraft server and real bots | The real server or agent in a child process of the test binary; the rest in the case's own process, on the loopback harness |
| The fault | Docker's: `kill --signal SIGKILL`, `stop`, a network disconnected and connected again, a volume written until it's full | The operating system's kill of the child (`SIGKILL`, or `TerminateProcess` on Windows) at an exact point |
| Needs | Docker; `just chaos`, and the Stack workflow nightly | Nothing: they run in `just check` and CI |

### The crash cases
- **The child is this test binary, started again** with a variable naming its role: `server` (serve's own wiring on the case's database and PKI), `agent` (one run of the managed agent from its identity files) or `backup` (the `backup` command's own function). The child's test is a no-op without the variable. It says where it is on stdout, one `CRASH …` line each, and never ends by itself.
- **A trip is where it stops for its kill:**
  - *A commit:* SQLite's commit, update and rollback hooks, set on the server's one write connection from outside, as any embedder of SQLite may. The commit hook runs before anything of the transaction is durable; for the n-th transaction that changed a named table, the connection's thread parks there for good. With `hold`, it waits for the case's word instead and lets the commit through, for a kill right after it.
  - *A file:* a thread of the child's that looks until a name is there or a file's bytes differ, for the steps that aren't in the database (a certificate file's replacement, a backup's temporary file).
  - *A refresh that never answers:* the case's own Microsoft stalls one call, before it saw the request or after it rotated the token.
- **Nothing of this is in the server's or the agent's code.** No fail point, no test-only setting, no feature.
- **Then the case kills the child and goes on in its own process:** the server serves again on the files the child left, or the agent runs again from them, and the loopback harness's checks apply as in every loopback case.
- **The case's own Microsoft** (`harness::microsoft`) rotates refresh tokens as the real one does and keeps which one it still takes in a file in the case's folder, so the server that dies and the one after it meet the same Microsoft.
- **Eleven cases:**

  | The kill | What must hold |
  |---|---|
  | The server, between signing a renewed certificate and its commit | Nothing of the renewal is stored; the agent keeps its certificate, comes back on it and asks again |
  | The server, right after that commit, the answer lost on its way | The server holds a certificate the agent never saw; the agent's next renewal revokes it |
  | The agent, once its certificate file is replaced, before any connection with the new one | It starts on the new certificate, whose hello revokes the old one; it never asks again |
  | The agent, while it replaces the file | It starts on the old certificate or the new one, never a part, and no temporary file stays |
  | The `backup` command, while its file is written | Only the temporary dot file is left; a backup is due at once, the next one finishes and passes restore's checks; a day later a backup removes what the kill left |
  | The server, before the second batch of a retention run commits | Exactly the first batch is gone; the next run finishes; each bot keeps its newest lines |
  | The server, before an ingest batch commits | None of the batch's lines is stored; the next ones are |
  | The server, then the agent with its memory, after a bot paused for a duplicate login (U16's crash case), the pause stored by the ingest's batch | Both hold the bot Paused; it never joins again by itself |
  | The same, the pause stored by the agent's next hello (it paused while cut off) | The same |
  | The server, in a refresh, before Microsoft saw the request (S64) | The account stays active; its bot joins again by itself |
  | The server, in a refresh, after Microsoft rotated the token | The stored token is spent: the account reads "link again", and nothing else is lost |

  In every renewal case no connection of the agent's is ever refused, and in the end the server holds exactly one certificate that isn't revoked: the one its stream presents.
- **"Between replacing the file and swapping the identity"** can't be told apart from outside from "after the swap, before the first hello with the new certificate": the identity in memory dies with the process either way. The case makes the second one certain: the agent's proxy lets exactly one connection through, the renewal runs on it, and no later connection reaches the server before the kill.
- **A graceful stop while a refresh hangs** (S64's other half) is fleet-server's own, in `tests/serve/control_plane.rs`, and stays there.

### The chaos tests on the real images
- **The world:** the smoke tests' `Stack` (the production compose files, both images, installed with its steps), with a compose file of the tests' own added last: a Minecraft server in the project on a network of its own, which the agents may dial and nothing else private or public; three bots on offline accounts, which join it for real; a second agent that holds no bot; the API on a fixed loopback port, so the Owner's session outlives the server's restarts.
- **Seven tests,** each its own project (`afkfleet-chaos-…`), removed with its volumes at the end:
  1. the server killed, then stopped;
  2. the agent killed, then stopped;
  3. the Minecraft server killed, then stopped;
  4. the agent partitioned from the server (S23, S32, S63, S67);
  5. the server restarted while an agent stays away (S45's placement gate);
  6. the database's and the backups' volumes filled;
  7. the database's volume lost.
- **The agents' retries are shortened, in the tests' own compose file alone:** a bot tries again after 20 s at most, where production waits up to 5 minutes, and the circuit breaker is out of reach, where production's opens after 8 failures for a quarter of an hour. A test takes its Minecraft server down twice within minutes and waits for every bot to be back; how the backoff grows and when the breaker opens are fleet-runtime's own tests.
- **No account on two agents** is read from the Minecraft server's own log: a second login of an account that's online ends the first with "logged in from another location". The second agent makes it a real question: nothing may ever move a bot to it.
- **The full disk is two `tmpfs` volumes with a size** (32 MiB for the database's, 16 MiB for the backups'), in Docker's memory. A `holder` container keeps them mounted for the test's whole life, since a `tmpfs` volume is gone when nothing mounts it. They're filled from inside it with `dd`, to the last byte, and freed by removing the two files it wrote. Nothing is ever written to Docker's disk to fill it.
- **`just chaos`** builds both images and runs the `chaos_…` tests in a nextest profile of their own, one stack at a time. The default, `ci` and `slow` profiles leave them out. The Stack workflow runs it nightly beside `test-stack`.

### After every fault
Both tiers check the same things, each with what it has:

| | The chaos tests | The crash cases |
|---|---|---|
| `integrity_check` | `database check`, through `maintenance` on the file as the kill left it, then beside the running server | `integrity::check` on the file as the kill left it |
| Acknowledged writes, with their audit entries | An account made through the API right before each fault, read back with its entry | The case's own writes before the kill, through the harness |
| No bot on two agents, every bot at its desired state | The Minecraft server's log; the API | The ledger of every session; the audit trail of assignments |
| The vault opens every secret | The Owner logs in anew with the same authenticator; `vault status` | The Owner logs in anew; `vault status` on the backup |
| A backup after recovery passes restore's checks | `backup`, then `restore`'s own steps at the test's end, and the fleet started on it | `BackupService::take`, then the backup opened as the database: its migrations, SQLite's check, `vault status` |

### One crate named again
`sqlx` becomes a dev-dependency of fleet-integration, with no feature of its own, for the child's hooks (`LockedSqliteHandle::set_commit_hook`, `set_update_hook`, `set_rollback_hook`, checked on docs.rs for 0.9.0). One edge in the lock, no crate.

## What the faults found

### The full disk: after it, the server could never write again. Fixed
- **What the test saw.** While both volumes were full, everything was as asked: reads worked, a write got the generic 500 with its request ID and an `error` line, `backup` ended with 1 and published nothing, a retention run and the schedule's backup failed in their metrics, the database stayed sound, and the server kept running. Then the room came back, and the next write failed with 500 too, on both architectures. So did every write after it: the server could read and never write again until it was restarted.
- **Why.** SQLite rolls a transaction back by itself after some errors, a full disk among them. sqlx doesn't see that. When the transaction is dropped it sends its own `ROLLBACK`, which fails ("no transaction is active"), and its count of open transactions on that connection stays at one; `Connection::is_in_transaction`'s docs say an automatic rollback may not show there. The server starts every write with `begin_with("BEGIN IMMEDIATE")`, which sqlx refuses on a connection it counts as inside a transaction (`InvalidSavePointStatement`), and the write pool has exactly that one connection, with no lifetime (ADR-0020, S39).
- **The failing tests first,** without a disk: SQLite's commit hook refuses one commit, which fails that `COMMIT` and rolls its transaction back, the same state. In fleet-server's `tests/store.rs`, the write after it failed with `InvalidSavePointStatement`, for an audited transaction and for a retention batch.
- **The fix,** in the write pool's `after_release` hook (`infra/sqlite/commits.rs`), which already settled the commit count there: a connection that comes back still counted as inside a transaction is pinged, so it has run everything it was handed, its `ROLLBACK` included; if it's still inside one then, the pool closes it and opens a new one for the next write. One `warn` says so: "the database's write connection was replaced: SQLite ended a transaction by itself, as it does when the disk is full". A transaction that was dropped and rolled back in the ordinary way costs one ping and keeps its connection.
- *Rejected:* a lifetime for the write connection (S39 took it away for the commit count, and a poisoned connection would still refuse writes until it ran out); asking SQLite itself whether a transaction is open (`sqlite3_get_autocommit` through the raw handle is `unsafe`); starting transactions without sqlx's `Transaction` (a rewrite of the store's write path for one error path).
- **Worth an upstream report:** a `ROLLBACK` that fails because SQLite already rolled back leaves sqlx's depth where it was. That's the user's to file.

### The crash cases: nothing in the server or the agent
Every case passed against the code as it was. Each was then shown to bite with a temporary mutation of that code, reverted from git right after:

| Mutation | Red |
|---|---|
| A renewal revokes the presented certificate at once | Three renewal cases: the agent is refused when it comes back on it |
| A hello revokes nothing | All four renewal cases: two certificates stay unrevoked |
| The agent never asks again after a failed renewal | The signing-kill case |
| The ingest doesn't write a bot's sticky crossing | The paused bot, stored by the ingest |
| A hello's states aren't stored | The paused bot, stored by the hello |
| A half-written backup counts as a finished one | The backup case |
| A refused refresh doesn't mark the account | The rotated-token case |

The retention case and the ingest-batch case pin that a batch is one transaction through the server's real path; nothing short of taking the transaction away turns them red.

Thirty runs of the first eight in a row, and ten of all eleven, had no failure.

### Three things the cases made visible, left as they are
- **A killed retention run's count isn't audited.** Its deletions are right, batch by batch, but the tally is in memory until the hourly `retention.purge` entry or a graceful stop (ADR-0020, U20). After a kill the entry misses what was deleted since the last one. The rows are gone either way, as the policy says; an entry a batch would be the per-batch audit U20 decided against.
- **A bot's state is never recorded backwards** (ADR-0019, S40): the server compares its receipt time with the stored one. The first draft of the harness ran the child on the real clock while the case's own clock stood 30 s ahead (the Owner's logins move it by their TOTP steps), and the child stored no state at all. That's the rule working. It also means a server whose clock is set back by minutes stores no bot's state until it has caught up; the runbook already asks for NTP on every host.
- **A kill between a backup's first and last step leaves its dot file for a day.** Pruning removes it with the first backup taken once it's a day old, with its journal. Until then it only takes room.

## Not covered, and why
- **A power loss or an OS crash** of the server's host: with `synchronous=NORMAL` the latest commits can roll back (ADR-0005), which S45's gate and ADR-0019's accepted risk are about. A process kill never loses a commit, and nothing here cuts the power.
- **The server's kill inside SQLite's own commit,** between two page writes: SQLite's write-ahead log is what makes that safe, and the kills here land before a commit, right after one, and at Docker's own moment in the middle of traffic.
- **A partition between the bots and their Minecraft server:** fleet-mc's liveness timeout and its tests (ADR-0011).
- **Microsoft's real answers:** never; the fake rotates tokens, and that is all the crash needs of it.

## Consequences
- An operator who updates sees no difference: the database exists. A first install through `setup.sh init` gains one step it runs itself. Whoever started the server by hand on an empty folder outside dev mode runs `migrate` once first; the error says so.
- A server whose volume is gone now stops with a line that says so, and stays stopped until a backup is restored.
- A server whose disk was full writes again once there's room. Its write connection is replaced after any transaction SQLite ended by itself, with one `warn`; a transaction dropped in the ordinary way costs the write connection one ping more than before.
- `database check` is one more command to keep: its help, the runbook's lines and the tests of both.
- The crash cases start a child process each, 6 s for the first nine together and 28 s each for the two refresh cases, in `just check` and CI.
- The harness's `Server` and `Agent` can run in a child, and its options grew by two (`backups`, `microsoft`); every loopback case runs as before.

## Alternatives considered
- **Fail points compiled into the binaries** (a `fail`-style crate behind a feature): the kill point would be a line of the production code, and the images under test would no longer be the ones released. SQLite's own hooks give the same point from outside.
- **Killing at random times in a loop** until every window was hit: no run says which window it tested, and a failure is hard to repeat.
- **Aborting the server's task in-process** (the loopback harness's `crash`): nothing is killed; sqlx rolls the transaction back and closes the connection, which is what a kill doesn't do. It stays for the agent's memory in U16's case, where the agent keeps nothing on disk.
- **Everything in containers:** no clock to set back, no fake Microsoft, and no way to stop a process at a statement.
- **A real disk image of a fixed size** for the full disk: it needs a file on Docker's own disk and a loop device; the `tmpfs` volumes need neither and can't outgrow their size.
- **The chaos tests in `test-stack`:** they add about half an hour and a Minecraft server to a recipe that's also run by hand after a change to the deployment; they get their own.
