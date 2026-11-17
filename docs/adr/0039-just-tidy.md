# 0039. `just tidy`: what a build leaves behind, and what is safe to remove

- **Status:** Accepted
- **Date:** 2026-11-17
- **Related:** CLAUDE.md, "A task" (step 3) and hard limit 3 (irreversible cleanups); ADR-0020 (U1: `target/dod-run`); ADR-0016 (the dev vault key in `target/dev-server`); ADR-0025; ADR-0030 (`fuzz/target`); ADR-0031 (`just mutants-clean`)

## Context
The development PC's disk ran full while Phase 15's group A was built. Cargo never removes what it built, `cargo clean` is forbidden here (it would take `target/dod-run` and the dev vault key with it), and until now the space came back only by hand.

**The user asked for a recipe** that removes only regenerable build output, written as a script with tests, after measuring what actually grows; never `target/` as a whole, `target/dod-run` or the dev vault key; run at the start of every group from now on.

## What was measured
The development PC, 2026-11-17, right after group A (one day of work since the folders were last emptied by hand). Cargo is the pinned nightly's, 1.100.0-nightly, whose build folder has one folder a package: `target/<profile>/build/<package>/<hash>/`, a **build unit**, with its `fingerprint/` and `out/`. There is no `deps/` folder any more.

| | Size | Grows with |
|---|---|---|
| `target/debug/incremental` | 48.2 GiB | Every build of our own crates; 668 folders |
| Our own crates' build units, all build folders | 55.0 GiB, 796 units | Every way a recipe builds a crate. `fleet-server` alone held 38 GiB in 395 units: **six variants of each of its test binaries,** built within one hour. Which command built which wasn't traced; the recipes do differ in what decides a unit's hash: the whole workspace or one crate (the features of shared dependencies), `--all-features` for clippy, fleet-mc's `fault-injection` for the slow tests, the stable toolchain for `stable-check` |
| Coverage: `target/llvm-cov-target`'s data and the reports in `target/` | 5.2 GiB in files tidy removes; 6.5 GiB with its dependencies' builds | Every `just cov`: 584 `.profraw` files, and a report of 120 to 250 MiB for each crate measured |
| `fuzz/target` | 2.8 GiB | Little: one build for all targets |
| `target/mutants` | Not there on this day; about 10 GB a shard after a run (ADR-0031) | Every `just mutants` |
| Every dependency's build units, `target/debug` | About 16 GiB | Slowly: a dependency's version or features must change |
| `target/release`, `target/doc`, everything else | 4.8 GiB, 0.2 GiB, under 0.1 GiB | Hardly |

**What "stale" can't mean.** No unit of our own crates is stale by anything a script can read: each of the six variants is the one its recipe would use again. So the rule is by kind, not by age.

## Decisions

### T1. What it removes
`scripts/tidy.mjs`, in the repository it sits in:
- **`incremental/` in every profile folder.**
- **The build units of this workspace's own crates** (`build/<crate>/` in every profile folder), every variant. The next build compiles our crates again, in minutes; that's the price, and the start of a group is when it costs least.
- **The same two in a build folder inside a build folder:** cargo-llvm-cov's (`target/llvm-cov-target`) and a target triple's (`fuzz/target/<triple>`), and the coverage data at such a folder's top (`*.profraw`, `*.profdata`, the profraw list).
- **The coverage reports at `target/`'s top, by name:** `coverage.json`, `coverage-<name>.json`, `cov-<name>.json`. A report for one crate is to be named that way.
- **The mutation runs' trees,** by calling `mutants-clean.mjs`'s own function, under its rules.

**Every dependency's build stays.** Those units cost the most to build again (azalea, aws-lc) and grow the least.

### T2. What it never touches
- **`target/dod-run` and `target/dev-server` are never entered,** not even listed, whatever they hold: skipped by name before any file-system call. A test dresses both up as build folders and counts the calls.
- **Never `target/` or `fuzz/target/` as a whole,** nor a finished binary, the docs, a log, a database, a git worktree or anything else that is no build output. It works from an allowlist: nothing is removed because of where it is, only because of what it is.
- **A folder is cargo's only by cargo's own marks:** `CACHEDIR.TAG` with the tag's signature for a build folder, `.cargo-lock` for a profile folder.
- **Links:** a `target/` or `fuzz/target/` that is a link or junction is refused; a linked folder where one would be removed is left and named; a link inside a removed folder is removed as an entry, never followed. As `dod-clean` and `mutants-clean` do.

### T3. A file that is held open
A build, a running `dev-server` or the app can hold a file. That entry is left and named, **its unit keeps its fingerprint** (removed last, and only when everything else in the unit went), and the rest is tidied. The exit code is 1, so it's seen.

### T4. Who the crates are
The package names come from the manifests (`Cargo.toml`'s `members`, each member's `[package]`, and `fuzz/Cargo.toml`), read by the script itself. `cargo metadata` would be the authority, but the script's tests run in CI's frontend job, which sets up no Rust toolchain; a test holds the list against this repository's `crates/`.

### T5. When it runs
**At the start of every group of work,** before the branch's first build (CLAUDE.md, "A task", step 3), and whenever the disk is short. `just tidy --dry-run` only measures. Not while a build runs.

## What it freed the first time
2026-11-17, before group B's first build: **108.5 GiB** by its own count (48.2 GiB incremental data, 55.0 GiB in 796 units, 5.2 GiB coverage; no mutation run was there). The disk's free space rose from 160 to 260 GiB: 99.7 GiB. The difference is the finished binaries in `target/debug`, which share their data with a unit's file and keep it.

## Alternatives considered
- **By age** (units older than some days). After group A everything was one day old, and the disk was full.
- **`cargo clean -p <crate>`.** It removes one profile at a time and knows nothing of coverage's folder; `cargo clean` itself is denied in Claude Code's settings, which a `-p` wouldn't get past.
- **`cargo-sweep`.** A new tool to install and pin; it works by access time or toolchain, neither of which tells the six variants apart.
- **Removing `fuzz/target` and `target/llvm-cov-target` whole.** Simpler, and it would build libFuzzer's and coverage's dependencies again each time. One rule for every build folder instead.
- **Fewer variants to begin with** (one feature set for every recipe). It would change what the recipes check; not this pull request's to decide.

## Consequences
- The first build after `just tidy` compiles the workspace's own crates again.
- `docs/development.md` and the commands table name the recipe; CLAUDE.md's "A task" starts with it.
- A cargo that lays its build folder out differently again leaves tidy with less to find, never with more: what it doesn't recognise, it leaves.
