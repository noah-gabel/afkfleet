# 0031. Mutation testing

- **Status:** Accepted
- **Date:** 2026-11-12
- **Related:** Plan.md Phase 13 (P13.3), §5, §8; ADR-0010 (fleet-core's tests), ADR-0016 (the auth and vault coverage gates, the JSON rejections), ADR-0020 (S43, R47: the live events viewer), ADR-0025, ADR-0027 (group D), ADR-0029

## Context
P13.3 asks for cargo-mutants on fleet-core and on the auth and vault modules, with tests added until no relevant mutant survives. A mutant is the code with one small change: an operator swapped, a condition negated, a function's body replaced by a fixed value. cargo-mutants builds each one and runs the tests; a mutant no test fails on is *missed*, and shows something the tests don't hold. The coverage gates (Plan.md §8) say which lines the tests run; this says which lines they check.

Three things about the tool, cargo-mutants 27.1.0, tried on the development PC, shaped how it runs here:
- **Its copy of the tree took ignored files along.** By default it tests each mutant in a copy of the source tree. Here that copy went into `node_modules`, which git ignores, and stopped at pnpm's linked folders there (Windows' os error 1314). So it doesn't leave out what git ignores, and the repository's ignored folders hold secrets and the DoD run. No copy was ever finished, and nothing secret was copied.
- **One run at a time is slow.** fleet-core has about 1400 mutants and the auth and vault modules about 830; fleet-server's whole suite runs for each of the latter.
- **A mutated run leaves files behind:** proptest writes a failure's seed beside the source file, and a run that's cut off leaves its mutant in the source.

Everything below is decided under ADR-0025.

## Decision

### What is mutated
- **`core`: all of fleet-core.**
- **`auth-vault`: the files the `auth` and `vault` coverage gates list outside fleet-core** (`scripts/coverage-gates.mjs`): fleet-server's auth, 2FA, lockout, token, rate limit, extractor, route registry, live events and vault files, fleet-startup's secret-file loader and fleet-api-types' secret type. The script reads the gates' own lists, so a module that joins a gate joins the mutants.
- **"Relevant" is every mutant in those scopes.** None is waved through as unimportant: each missed one gets a test, or a line in `.cargo/mutants.toml` saying why no test could see it.

### How it runs: `just mutants [scope] [shards]`
- **Each shard runs in a git worktree of the last commit,** `target/mutants/tree-<k>`, changed in place (`--in-place --shard`), four at once by default. A worktree holds tracked files only, so nothing ignored is ever copied, and the working tree is never touched. The run tests HEAD: `scripts/mutants.mjs` refuses to start over uncommitted changes, so nobody reads a result for code that wasn't in it.
- **A tree is reset before and after its run** (tracked files back to the commit, untracked files removed, both in the tree alone; its `target/` stays). The script only resets a folder that is this repository's registered worktree and its own top level; anything else at that path stops it.
- **The tests are the mutated package's own, all of them** (cargo-mutants' default), with nextest, in a profile of their own, `mutants`: the test profile without debug info, so every mutant's build is shorter and smaller.
- **The verdict:** each shard's counts, every missed mutant and every one whose tests ran out of time, then PASS or FAIL. It fails on a missed mutant, and on a shard whose exit code isn't the one its lists stand for (a failed baseline, a usage error).
- **A mutant whose tests run out of time counts as found.** Its tests never end, which no green run hides.
- **It's outside `check` and `ci`,** and there is no workflow for it. `core` takes about an hour here and `auth-vault` about eight, at four shards on eight threads, and a job on GitHub's runners ends after six. It runs before a release, and after a larger change to these modules.

### The mutants left out
`exclude_re` in `.cargo/mutants.toml` holds one regular expression per equivalent mutant, with its reason beside it. A mutant's name is `path:line:column: what changes[ in function]`; most are matched without their position, so they stay matched when lines move. Two of fleet-core's can't be told from a neighbour without it and are pinned to a line and column: when that line moves, the mutant shows up as missed again and its entry moves with it.

Fourteen are left out: twelve of fleet-core's and two of the auth and vault modules'. The kinds:
- **A bound that another check holds anyway** (a name one character too long is refused by the separator too; the backup name's shape check is repeated by the comparison at the end).
- **Arithmetic that can't differ** (`|` and `^` over two values that share no bit; the larger of two equal lengths).
- **A second look no input reaches:** the recovery code's constant-time comparison comes after the store matched it by hash already.
- **The secret file's `check` as a whole:** on Windows it repeats what `read` checks around it, and its one check of its own, the permission bits, is compiled on Unix alone.

### What it needs
cargo-mutants 27.1.0 (2026-06-02, the newest; MIT), installed like the other cargo tools. Each tree's build is about 10 GB.

## What the runs found

### fleet-core
1403 mutants at first: 1090 caught, 272 unviable (they don't compile), 2 timed out, **39 missed**.
- **27 got a test.** The audit metadata's budget to its last byte, its largest integer, a control character's escape and a stored integer's bounds; three planner cases (a welcome's records, a stray with its own record, another agent's untouchable bot); an angle whose maximum isn't sampled; and accessors nothing asserted (`is_empty`, `as_str`, a `Display`, a redacted `Debug` compared whole).
- **12 change nothing** and are left out.
- **The confirmation run found one more** (1391 mutants: 1116 caught, 1 missed): in the planner's `let_foreign_go`, another agent's record of *any* bot counted as the listed bot's. No fixed case told the two apart; the first run most likely caught it through a property test's random cases. It has its own test now, seen failing with the mutant applied by hand.
- **The two that time out** are loops that never end once their condition is mutated (`int_len`, `FailureWindow::record`).

### The auth and vault modules
834 mutants: 522 caught, 273 unviable, 6 timed out, **33 missed**.
- **One was a defect.** Four mutants of the JSON extractor's `position` survived because it never found anything: axum hands serde's error over inside `serde_path_to_error`'s, whose `source` skips it. So a refused body's `debug` line never had the line and column ADR-0016 decided on, except for text after a whole value. `position` now reads the wrapped error by its type, and the extractor has tests for every kind of refusal, with its status and its line.
- **Four were in the live events viewer,** where a wrong answer shows a user stale or missing bots: a hint that joined a read already made (its bot would be sent from views read before the event), the removal of a bot that reads as nothing, sent once and only when it was seen, and a hint judged on rights a commit outran, which must wait only when those rights hide one of its bots.
- **One let a user keep sockets past their cap:** when one of several socket slots was dropped, forgetting the user's whole count went unnoticed.
- **The rest were counts, reasons and forms nothing asserted:** a replace token's purge, the rotation's counts in its audit entry (the test rotated one secret of each kind, so a fixed 1 passed), the limiters' key count, the open sockets, the bearer header's and the path's refusal reasons, the setup token's `require`, two `Debug` forms, and the secret file's limit.
- **The secret file's mode check became a function of the bits.** Its five mutants were compiled away on Windows, where the mutants run. `open_to_others` is tested on every platform now, and only its wiring is Unix's.
- **The secret file's bounded read became a function over any source.** The read asks for one byte more than the limit, to tell a larger file from one exactly that long; asking for the limit alone went unnoticed, because the metadata check in front refuses a larger file first. `read_bounded` is tested with a source a byte past the limit, as a file that grew between the check and the read: refused, never cut.
- **2 change nothing** and are left out.
- **Every one of the 33 was run again** against the new tests, with the other mutants of its function or file: none is missed.
- **The six that time out** are in the live events' rights and socket loop (`LiveRights::fresh` twice, the socket's `checked` and `resynced`, the viewer's `gate` and `judge`). `judge`'s fails one of the new tests at once now. One of the others was applied by hand, `gate` answering that the rights are current without reading them: the socket's loop then never yields, the `auth` tests' `events::chat` cases never end, and their own bound of 10 s per wait can't fire in a runtime that's kept busy.

**One dependency became direct:** `serde_path_to_error` 0.1.20 (2025-09-15, MIT OR Apache-2.0), fleet-server's alone, to name the type axum wraps serde's error in. It was in `Cargo.lock` already, through axum's `json`; the lock gains one edge and no crate.

## 2026-11-12: a time limit a test, and `just mutants-clean`
Two follow-ups from the pull request's review (the user's, #113), in group E's first commit.

### Every nextest profile ends a test after 10 minutes
**Measured first,** on GitHub's runners: twelve jobs of three runs (#112's last, `main` after it, #113's), every test's time read from the jobs' logs.

| Job | Slowest test | The whole run |
|---|---|---|
| `test` (ubuntu-latest, profile `ci`) | 48.0 to 59.6 s | 201 to 233 s |
| `test` (windows-latest, profile `ci`) | 52.5 to 68.0 s | 255 to 317 s |
| `coverage` (ubuntu-latest, the default profile, instrumented) | 41.8 to 62.3 s | 167 to 221 s |
| `tauri-build` (the desktop crate's coverage, windows-latest) | 8.0 to 10.6 s | 42 to 53 s |

The slowest is always the authorization matrix (ADR-0029), but in one coverage job, where a loopback case was (41.8 s to the matrix's 36.5 s). After it: fleet-integration's restart case at 42 s in every job that runs it (it waits real time), fleet-runtime's chaos property at 12.5 to 34.4 s, the outage case at 23 to 29 s and the renewal case at 21 to 22 s. No other test takes 11 s.

**The limit:** `slow-timeout = { period = "60s", terminate-after = 10 }` in the default and the `ci` profile, as the `slow` and `manual` profiles had it already. A custom profile inherits the default's setting; each says it again, and `scripts/nextest-config.test.mjs` holds the line in all four, so dropping one can't lift the limit for the profile CI runs. A test that runs out of time fails, labelled `TIMEOUT` with its name (nextest's default, `on-timeout = "fail"`, which the test holds too). On Windows nextest ends the test's process at once; on Unix it sends SIGTERM, and SIGKILL 10 s later.

**Why 10 minutes:** almost nine times the slowest test measured (68 s), and a hang costs it once, since tests run side by side. Less would cut close elsewhere: the matrix grows with every route, and on the development PC under four mutation shards fleet-server's suite took 165 to 215 s where it takes 46 s alone.

**Seen working** with the mutant from above applied by hand (`Viewer::gate` answering without reading), on the `auth` tests' `events::chat` cases, under each profile: the cases that never end were stopped at 600 s and reported as `TIMEOUT [ 600.15s]` by name, four in one run and six in the other, and each run ended after 601 s with exit code 100. (The two runs went at once, which is most likely why other cases of theirs failed at once and not the same ones.)

**A test still bounds its own waits.** The limit is for code that stops yielding, where no timer inside the test's process can fire.

**For `just mutants`:** in the `auth-vault` scope a mutant whose tests never end is now a failed test after 10 minutes, so it counts as caught; cargo-mutants' own limit there was 14 to 18 minutes. In `core` cargo-mutants' limit, 102 s, still comes first, and such a mutant stays "timed out".

### `just mutants-clean`
**It removes exactly what the runs keep** (`scripts/mutants-clean.mjs`): every registered worktree named `target/mutants/tree-<k>`, through `git worktree remove --force`, then `target/mutants` itself. Nothing else in `target/` is touched.
- **`--force`,** because a tree always holds its build, and a run that was cut off leaves a mutant in its source. git also forgets a tree whose folder was deleted by hand.
- **It refuses** a `target/` or a `target/mutants` that is a link or junction, and any other registered worktree inside `target/mutants`, removing nothing: only its owner should say that one may go.
- **It stops at a tree git can't remove** (a locked one, or one a run still holds), with git's reason.
- **The folder is walked with lstat,** as `dod-clean` walks its own: a link inside is removed as an entry, never followed.
- **Not while `just mutants` runs:** the run would lose its trees.

## Consequences
- **A missed mutant fails `just mutants`.** Whoever runs it writes the test or the line in `.cargo/mutants.toml`.
- **A run needs a commit.** Work in progress is committed first, as a WIP commit if need be.
- **The trees stay** between runs, so the next one builds little: about 10 GB each. `just mutants-clean` removes them and the reports (since 2026-11-12, above).
- **Code compiled on Unix alone shows as missed here,** since the mutants run on Windows: it gets a platform-free function to test, as the secret file's mode check did, or a recorded reason.
- **Seven mutants are found only because the tests then never end** (two loops in fleet-core, five in the live events). A loop that never yields is beyond any bound a test sets for its own waits; only a limit outside the test's process ends it. nextest's default profile had none, so in CI such a defect would have ended at the job's own timeout, without naming the test; since 2026-11-12 every profile ends a test after 10 minutes (above).

## Alternatives considered
- **cargo-mutants' own copy of the tree, told to skip ignored files:** what the copy holds would then hang on an option and its default in the next version of the tool; a worktree is exactly the tracked files, and git keeps it that way.
- **`--in-place` in the working tree:** one run at a time, and the work in the tree stops for hours; a run cut off leaves a mutant in the source.
- **Narrowing each mutant's tests to the test binaries that cover its file:** faster, but a list to keep, and a mutant "missed" by a narrowed run says nothing until the whole suite has run on it anyway.
- **A workflow on GitHub:** the hours above, for a check that changes only when these modules do.
- **Leaving out whole kinds of mutants** (every `Debug`, every log reason): the two `Debug` forms here are the ones that keep keys out of logs, and the reasons are what an operator reads; each was a line of test.
- **Parsing the position from serde's message** instead of naming `serde_path_to_error`: the message quotes the request's own text.
