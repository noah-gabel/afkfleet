# 0021. testcontainers' unmaintained tar crate

- **Status:** Accepted
- **Date:** 2026-11-05
- **Related:** Plan.md §5 (Containers), P3.7; ADR-0011, ADR-0012, ADR-0019; `deny.toml`

## Context
RUSTSEC-2026-0440, issued on 2026-11-05, marks `astral-tokio-tar` unmaintained: Astral ended its maintenance on 2026-11-03, and every version is deprecated. It's an informational advisory, not a vulnerability, and it names no patched version; Astral points to `tar-codec` as a possible replacement. `deny.toml` makes an unmaintained crate an error anywhere in the graph (`unmaintained = "all"`), so `just deny` failed from that day on every branch, first on PR #93's docs-only commit.

- **Its reach.** `astral-tokio-tar` 0.6.4 comes in only through `testcontainers` 0.28.0, a dev-dependency of fleet-mc (the slow tests, P3.7) and fleet-integration (the managed check, ADR-0019). `cargo tree --workspace -e normal,build --target all -i astral-tokio-tar` finds nothing, so no shipped binary builds it. The tests only pull and run `itzg/minecraft-server`: none copies a file into or out of a container or builds an image.
- **No fix.** testcontainers 0.28.0 is its newest release (2026-08-06), and its `main` branch still depends on `astral-tokio-tar` 0.7.0.

Weakening a `deny.toml` rule needs the user's approval and an ADR (CLAUDE.md, security rule 10). The user approved this ignore on 2026-11-05, in a PR of its own from `main` rather than inside PR #93.

## Decision
`deny.toml`'s `[advisories] ignore` gains one entry, with its reason and its removal trigger in the entry itself:

| Advisory | Why it's acceptable | Removal trigger |
|---|---|---|
| RUSTSEC-2026-0440 (`astral-tokio-tar`) | Unmaintained, not vulnerable. It reaches only the container tests, through testcontainers, and never a shipped binary | Removed once a testcontainers release drops the crate; checked at every testcontainers bump |

`unmaintained = "all"` stays, so a new unmaintained crate anywhere else still fails. The entry names this one advisory, so a vulnerability reported against `astral-tokio-tar` later still fails `just deny`.

## Consequences
- **Easier:** `just deny` passes again on `main` and every branch. A PR that doesn't touch `deny.toml` goes green on a re-run of its job once this is merged, since CI tests a PR merged into `main`.
- **Harder:** the container tests depend on a crate that gets no more fixes. A vulnerability in it would come as an advisory of its own and fail `just deny` again.
- **To revisit:** at every testcontainers bump. Once a release drops the crate, the entry goes; cargo-deny warns about an ignore that matches nothing (ADR-0012).

## Alternatives considered
- **The same entry inside PR #93:** the user chose a PR of its own, keeping #93's review on its task.
- **Leaving `deny` red until testcontainers drops the crate:** no date is known, and every PR would merge past a red job.
- **Replacing testcontainers** and driving Docker another way: a dependency swap with its own planning, for a crate only the tests use.
- **`unmaintained = "workspace"`** (only direct dependencies): far broader than this one crate needs.
