# 0038. `chrono` is unmaintained: ignored for now, and what would end that

- **Status:** Accepted. Decided under ADR-0025. Whether and when the workspace moves to another time crate was left to the user, who answered on 2026-11-18: no move for now (C3)
- **Date:** 2026-11-17
- **Related:** Plan.md §5 (Time: `chrono`); ADR-0010 (time is a parameter in fleet-core), ADR-0013, ADR-0014, ADR-0015 (where a clock is read); ADR-0012, ADR-0021, ADR-0024 (the other ignored advisories); ADR-0035 (a dependency with an advisory is handled first); `deny.toml`

## Context
On 2026-11-17 `just deny` failed on `main`, with nothing changed in the workspace, on a new advisory: **RUSTSEC-2026-0543, "chrono is unmaintained; all versions are affected".**

**What it is, checked on the day:**
- **A notice, not a vulnerability.** RustSec lists it as informational, "unmaintained", with no patched version. It was reported on 2026-09-14 and issued on 2026-11-17.
- **Filed by chrono's own maintainer.** Its reference is chrono's issue 1768, "Soft-deprecating chrono and chrono-tz", opened on 2026-01-22: the maintainer is winding maintenance down, calls the API dated and recommends `jiff`. On 2026-11-17 he confirmed there that he filed the advisory. No successor and no fork is named; a new maintainer would be at his discretion.
- **`deny.toml` treats an unmaintained crate as an error** anywhere in the graph (`unmaintained = "all"`), which is why it fails the check.

**Where chrono is here:**
- **It's the workspace's time type** (Plan.md §5): `DateTime<Utc>` in fleet-core's values and rules, in the runtime, the agent, the server, the API's types and the app's Rust side. It's built without default features; only the agent and the server enable `now`, each for its one clock read.
- **azalea depends on it too:** `azalea-auth` and `azalea-client` 0.16.0, so it stays in the graph whatever the workspace does, until an azalea release drops it.

## Decision

### C1. The advisory is ignored, by its one ID
`deny.toml` ignores RUSTSEC-2026-0543 with its reason and its removal trigger.

**Why that's acceptable now:**
- **Nothing is wrong with the code today.** The notice says nobody will fix what is found later; it names no fault.
- **What chrono does here is narrow.** UTC instants, arithmetic on them and timestamps as text. No local time zones (`clock` is off everywhere) and no time zone database (`chrono-tz` isn't in the lockfile). The text it parses is timestamps: what serde reads for the API's types, the server's own database rows, and in tests Docker's output.
- **A vulnerability in chrono would be a new advisory** with its own ID, and fails the check again. The ignore names this one notice.

### C2. Removal trigger
**Once neither the workspace nor azalea depends on chrono.** With `unused-ignored-advisory = "deny"` (ADR-0024), the ignore then fails the check by itself and is removed.

### C3. Moving to another time crate: not for now *(the user's answer, 2026-11-18)*
**The user's answer:** no move for now. azalea keeps chrono in the graph anyway (`azalea-auth` and `azalea-client` 0.16.0), so moving the workspace's own crates wouldn't remove the advisory, only add a second time crate beside it. **It's looked at again when azalea drops chrono, or when an advisory names a real fault in chrono.** The rule below stays: nothing new deepens the dependency.

**What was put to them:**

It's a change to Plan.md §5's registry and touches every crate: the domain types, the stored and wire formats' timestamps, ts-rs's and utoipa's and sqlx's support for the type, and every test that builds a time. It isn't urgent (C1), and it shouldn't ride in a pull request whose job is to make `main` green again.

**Proposed at the time:** a task of its own after Phase 15, with a short comparison of `jiff` and `time` first: which the crates we depend on (sqlx, ts-rs, utoipa, serde) support, what the stored formats need, and what azalea does. Until then nothing new should deepen the dependency: no `chrono-tz`, no new feature of chrono's.

## Alternatives considered
- **Moving now.** See C3: large, and not what an advisory pull request is for.
- **Setting `unmaintained` to a weaker level.** It would let every other unmaintained crate through unseen. One ID is the narrowest exception.
- **Waiting for a successor.** None is named, and `main` is red meanwhile, which hides every other advisory.

## Consequences
- **`deny.toml` holds one more ignore,** listed under Weakened in its pull request.
- **chrono gets no fixes any more.** A fault found in it stays until the workspace has moved; C1 says why that's bearable for what it does here, and C3 when a move is looked at again.
- **azalea's bumps check it too:** when azalea drops chrono, the workspace's own move is all that's left.
