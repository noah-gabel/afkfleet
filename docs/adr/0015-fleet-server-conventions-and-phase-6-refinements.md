# 0015. fleet-server conventions and Phase 6 refinements

- **Status:** Accepted
- **Date:** 2026-10-10
- **Related:** Plan.md Phase 6 (P6.1–P6.12), §4, §5, §7.2, §7.5, Appendix A; ADR-0002, ADR-0005, ADR-0010, ADR-0011, ADR-0014

## Context
Phase 6 builds the server foundation: `fleet-server`, a secure HTTP skeleton with persistence, an error model and an audit trail, and `fleet-api-types`, the DTOs shared with the app. It's the second binary, after the agent (ADR-0014), and the first that keeps state and reads time and randomness for its own records.

The server needs much of what the agent already built: a config loaded from a TOML file plus prefixed environment variables, with errors that name each key and never echo a value; the `[log]` section; JSON or pretty logs with the `azalea_auth` cap, which P9.3 requires on the server too; and a panic hook. It also needs a way to read the time and random bytes that tests can control.

Before writing any code, the Phase 6 plan listed every question the plan left open. The user answered them on 2026-10-10, one at a time, and this ADR records the answers. If a later group's review changes a decision, that group's PR amends this ADR.

## Decision
### Grouping
One branch and one PR per group, one commit per task, in this order *(the user's choice: five groups)*:

| Group | Branch | Tasks |
|---|---|---|
| A | `p6/p6.1-p6.2-layout-and-config` | The fleet-startup extraction, P6.1, P6.2. fleet-server is a library only, without a binary |
| B | `p6/p6.3-p6.8-persistence-and-audit` | P6.3, P6.4, P6.8, plus the time and randomness ports, the `sqlx-offline` job and `just db-prepare` |
| C | `p6/p6.5-p6.9-api-types-and-errors` | P6.9 first, since ApiError's body DTO lives in fleet-api-types, then P6.5; the `ts-types-fresh` job and `just gen` |
| D | `p6/p6.6-p6.7-middleware-and-health` | P6.6, P6.7 |
| E | `p6/p6.10-p6.12-cli-and-wrap-up` | P6.10, P6.11, P6.12, the DoD integration test and the phase wrap-up |

Each later group asks its own implementation-level questions in its session. Known ones: anyhow in the server's `main.rs` and the HTTP client for the healthcheck and the DoD test (E), the request-ID format (D), how ts-rs exports while `packages/ui` doesn't exist yet (C), and sqlx's offline data in every CI job that compiles the server, and Biome against `.sqlx/*.json` (B).

### fleet-startup, the binaries' shared start-up code
- **A new crate** *(the user's decision)*, `crates/fleet-startup`, holds only what a process needs to start:
  - `config`: the generic loader `extract::<R, K>(path, env_prefix)` (the file must exist at exactly that path; `Toml::file_exact`; `Env::prefixed(prefix).split("__")`), the errors `ConfigError<K>`, `Problems<K>`, `Problem<K>`, `KeyPath` and `ParseError`, and the `[log]` types
  - `telemetry`: the JSON or pretty log layer (`layer_with`), `init_with`, the filter and the panic hook (`PANIC_TARGET`)

  It depends internally only on fleet-core, for `sanitize_untrusted`. Its `//!` header says that it holds only start-up code.
- **Not "bootstrap"** *(the user's naming)*. Plan.md §7.2 and P12.6 already use "bootstrap" for creating the first Owner, and a crate of that name the agent also links would invite account code into it.
- **A pure move first** *(the user's decision)*. Group A's first commit moves the code out of fleet-agent with no change in behavior. fleet-agent re-exports everything under its old paths (`ConfigError`, `Problems` and `Problem` become type aliases over its own `ProblemKind`), so its tests pass with only import changes. The 46 unit tests of the moved code moved with it, under the same names.
- **The filter** is `(EnvFilter ∧ the binary's cap ∧ azalea_auth cap) ∨ panic reports`, the agent's tree and text unchanged.
  - **The `azalea_auth` cap at `info` and the panic passthrough are fleet-startup's** *(the user's decision)*, applied outside anything a binary passes in, so no binary can weaken them. P9.3's cap on the server is this one.
  - **A binary only adds rules,** through `trait FilterRules { env_defaults, cap, warn_at_startup }` *(the user's choice over an options struct or two public set-up steps)*. `env_defaults` returns directives that go in front of the operator's, so the operator's filter always applies. `init_with` keeps the agent's order: the subscriber, then `warn_at_startup`, then the panic hook.
  - The agent's `AzaleaRules` returns its existing azalea default, azalea cap and startup warning, which stay in fleet-agent.
- **Shared test helpers move to fleet-testkit** *(the user's decisions)*:
  - `jail::in_jail`, figment's `Jail` for every config test. The workspace keeps exactly one approved `#[expect(clippy::result_large_err)]` (ADR-0014).
  - `log_buffer::LogBuffer`, a writer for a log layer a test builds itself. It replaces the agent's two copies of `Capture` in a second commit, so the move stays separate from that mechanical change. fleet-testkit is library code that never panics, so `json_lines()` and `lines_with()` return a `Result` whose error, `NotJson`, names only the line's position. Its lock is poison-tolerant and its text lossy.
  - `log_capture` (ADR-0011) and `LogBuffer` don't overlap, so both stay, with module docs that say when to use which: `log_capture` for redaction checks across the whole process, `LogBuffer` for what one layer prints. Since tests print a `LogBuffer`'s text when they fail, it's never used to look for real secrets.

### Time and randomness (group B)
- **Injected ports** *(the user's decision)*. tokio's paused time doesn't move chrono's `Utc::now()`, and the server needs exact time in its rules (P7's TTLs and lockouts). fleet-core gets `system::{Clock, SecureRandom, RandomError}`: `Clock::now() -> DateTime<Utc>` and `SecureRandom::fill(&self, &mut [u8]) -> Result<(), RandomError>`, since getrandom can fail and nothing may panic. fleet-core still never reads a clock; it only names the traits.
- **Where the implementations live** *(the user's decisions)*: fleet-testkit has `ManualClock` (set and advance) and `SeededRandom` (a seeded `StdRng`); fleet-server's infra has `SystemClock` (chrono's `now`) and `OsRandom` (getrandom).
- **Guards for the randomness port**, which P7 uses for secrets *(the user's decisions)*:
  - The seeded fake exists only in fleet-testkit, a dev-dependency everywhere, so it can't be linked into a binary. A script under `scripts/`, run by `just scripts-test`, checks that no workspace member lists fleet-testkit outside its dev-dependencies. It reads the members from the root `Cargo.toml` and expands their globs, so a crate outside `crates/` (P8's `apps/desktop/src-tauri`) is covered too.
  - `OsRandom` is the only non-test implementation, and main.rs always wires it. The port's docs say every implementation must be cryptographically secure (security rule 4).
  - `crates/fleet-server/clippy.toml` repeats the root settings and bans chrono's `Utc::now` and `Local::now`, `SystemTime::now` and `elapsed`, `Instant::now` and `elapsed`, getrandom's free functions and rand's OS-seeded ones. `SystemClock` and `OsRandom` each carry one approved `#[expect(clippy::disallowed_methods, reason = …)]`.
- **Tests never hard-code `SeededRandom`'s bytes** or the IDs made from them, and snapshots redact them *(the user's addition)*: rand doesn't promise `StdRng`'s output across versions. Tests compare outputs with each other ("same seed, same result"), as fleet-core's do.

### stable-check
fleet-startup and fleet-server join the justfile's `stable_crates` in group A, and fleet-api-types in group C, under P0.11's rule: the crates that don't depend on azalea or azalea-auth *(the user's decision)*. fleet-server leaves it in P9.3, when azalea-auth arrives.

### Dependencies
No new external crate in group A. All of these are already in `[workspace.dependencies]`:
- **fleet-startup:** fleet-core, figment (`toml`, `env`), serde, thiserror, tracing, tracing-subscriber (`env-filter`, `json`). Dev: fleet-testkit, rstest.
- **fleet-testkit** gains figment (`test`) and serde_json.
- **fleet-agent** gains fleet-startup and drops its normal figment dependency; its tests keep figment (`test`, `toml`).
- **fleet-server** (group A): fleet-startup, garde (`derive`), serde (`derive`), thiserror, tracing. Dev: fleet-testkit, figment (`test`), rstest.

## Consequences
- **Easier:**
  - The server gets the agent's tested config errors and logging without a second implementation, so a fix to either reaches both binaries.
  - The `azalea_auth` cap and the panic passthrough are enforced in one place for both binaries.
  - Time and randomness are deterministic in every server test, and a direct clock or OS-randomness read in fleet-server fails clippy.
- **Harder:**
  - One more crate, and fleet-agent's config types are aliases over generic ones.
  - fleet-testkit, still a dev-dependency only, carries figment's `Jail` and serde_json for its helpers.
  - The agent's tests read JSON lines through a `Result` (`.unwrap()`), since fleet-testkit can't panic.

## Alternatives considered
- **Copying the agent's code into fleet-server.** About 1,000 lines, including the error texts that must never echo a value and the panic sanitizing, would drift between two copies.
- **Copying now and extracting later.** The same drift until then, and a second migration.
- **fleet-server depending on fleet-agent.** Against §4, and it would pull azalea into the server.
- **The name `fleet-bootstrap`, `fleet-telemetry`, `fleet-shell` or `fleet-bin-support`.** "Bootstrap" already means the first Owner; the others name only part of the crate or nothing specific.
- **An options struct, or two public set-up steps, instead of `FilterRules`.** The struct would move the agent's warning after the panic hook; with two steps, a binary could forget the hook.
- **A third copy of `Capture`, or moving it in the extraction commit.** The first keeps three implementations; the second mixes the mechanical `.unwrap()` changes into the move.
- **Time and randomness passed as arguments, or read directly.** Arguments spread clock reads into every handler; direct reads make expiry and lockout rules untestable.
- **The ports in fleet-server, with fleet-testkit depending on it.** A dev-dependency cycle in which fleet-server's own unit tests would see a second copy of the traits.
- **No lint, or the adapters in fleet-startup.** Review alone misses a stray `Utc::now()`; adapters in fleet-startup would give the agent's start-up crate the server's clock and getrandom.
- **fleet-server outside stable-check until P9.** Nightly-only code could creep into the server for three phases.
