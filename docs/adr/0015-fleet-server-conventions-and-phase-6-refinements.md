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

### The server's config (P6.1, P6.2; group A)
- **Modules arrive with their code** *(the user's decision)*. fleet-server starts as a library whose crate doc describes the whole P6.1 layout, which task fills each module, and the layering (`http` handlers → `app` services → `ports` → `infra`). Group A adds only `config`; the tree has no empty modules. The binary comes with the CLI in P6.11.
- **The agent's rules** through fleet-startup's loader, with the prefix `AFKFLEET_SERVER__`: the file must exist at exactly the given path, environment variables override any key, unknown keys are errors, parse errors stop at the first, and validation lists every problem at once, sorted by key, without echoing a value.
- **Only Phase 6's keys** *(the user's decision)*. Each later phase adds its own, so a key of a later phase stays an unknown key until then; Appendix A gets a note on which phase brings which key.

  | Key | Default | Rule |
  |---|---|---|
  | `dev_mode` | `false` | When it's on, one `warn` at startup *(the user's addition)* |
  | `[http] bind` | `127.0.0.1:8080` *(the user's decision)* | An IP address and a port other than 0 *(the user's decision)* |
  | `[http] request_timeout_secs` | 15 | 1–300 |
  | `[http] max_body_bytes` | 65536 | 1024–1048576, so the limit stays a guard when raised |
  | `[database] path` | none: required *(the user's decision)* | Absolute *(the user's addition)* |
  | `[log] format`, `filter` | `json`, `info` | As the agent's (ADR-0014) |

  - **`bind`'s default deviates from Appendix A's `0.0.0.0:8080`,** so a server started without the key never listens on the network; the container config sets `0.0.0.0:8080` itself. Port 0 is refused, since Caddy couldn't reach a port the OS picks and the healthcheck probes the configured one.
  - **`database.path`** has no default because Appendix A's `/data/afkfleet.db` is a container path, and it must be absolute because a relative path would resolve against wherever the server is started: a start from another directory would silently create a fresh, empty database.
- **The dev-mode warning** *(the user's choice of text)*: "dev mode is on: development-only features are enabled; never run a production server in dev mode". `ServerConfig::warn_if_dev_mode()` logs it; `serve` calls it right after logging starts (P6.11). It names no feature, so it stays true as later phases hang more on `dev_mode`.
- **`[log]`** is the agent's section *(the user's decision)*: `json` or `pretty`, an EnvFilter `filter`, `AFKFLEET_SERVER__LOG__…` overrides, no effect from `RUST_LOG`, stdout. The server's rules are fleet-startup's `NoRules`, so its filter is `(EnvFilter ∧ azalea_auth cap) ∨ panic reports`, with no azalea default and no startup warning.
- **Secrets: the rule and a test now, the loader in P7.4** *(the user's decision)*. Phase 6's config has no secret yet.
  - A key whose name has a `_`-separated part `key`, `token`, `password`, `secret`, `passphrase`, `pepper` or `credential`, singular or plural *(the user's list)*, must end in `_file` or `_files` (Appendix A's `master_key_files` is a map of files).
  - Keys that match but hold no secret go on an explicit allowlist, each with its reason, so a harmless key (P7's `access_token_ttl_secs`) never forces an awkward rename or a weaker test, and every exception shows in review *(the user's addition)*.
  - The test walks every key at every level through the field lists serde's derive passes to `deserialize_struct`, with a small walking deserializer in the test. Options and lists are entered, and a serde shape it doesn't know fails the walk, so no key can hide. It also asserts that the walk found Phase 6's keys, so it can't pass on an empty walk.
- *(as built, group A)*:
  - **Shape.** `config::load(path) -> Result<ServerConfig, ConfigError>`, with `ServerConfig { dev_mode, http: HttpConfig { bind, request_timeout, max_body_bytes }, database: DatabaseConfig { path }, log }`. `ConfigError`, `Problems` and `Problem` alias fleet-startup's types over the server's `ProblemKind` (`Missing`, `Empty`, `OutOfRange`, `NotAbsolute`, `SocketAddress`, `PortZero`, `LogFormat`, `LogFilter`).
  - **Tests,** red against stubs that compiled, then green: `tests/config.rs` (31 cases in `in_jail`), the secret-name rule and walk, and fleet-startup's `NoRules` tests (7 filters × 4 targets, panic reports, no defaults or warning). 40 of the 106 tests failed on assertions against the stubs.
  - **A probe found** that an unquoted `AFKFLEET_SERVER__HTTP__BIND=[::1]:8443` reads as text, so an IPv6 bind needs no quoting in the environment; the override test uses that form.

### Time and randomness (group B)
- **Injected ports** *(the user's decision)*. tokio's paused time doesn't move chrono's `Utc::now()`, and the server needs exact time in its rules (P7's TTLs and lockouts). fleet-core gets `system::{Clock, SecureRandom, RandomError}`: `Clock::now() -> DateTime<Utc>` and `SecureRandom::fill(&self, &mut [u8]) -> Result<(), RandomError>`, since getrandom can fail and nothing may panic. fleet-core still never reads a clock; it only names the traits.
- **Where the implementations live** *(the user's decisions)*: fleet-testkit has `ManualClock` (set and advance) and `SeededRandom` (a seeded `StdRng`); fleet-server's infra has `SystemClock` (chrono's `now`) and `OsRandom` (getrandom).
- **Guards for the randomness port**, which P7 uses for secrets *(the user's decisions)*:
  - The seeded fake exists only in fleet-testkit, a dev-dependency everywhere, so it can't be linked into a binary. A script under `scripts/`, run by `just scripts-test`, checks that no workspace member lists fleet-testkit outside its dev-dependencies. It reads the members from the root `Cargo.toml` and expands their globs, so a crate outside `crates/` (P8's `apps/desktop/src-tauri`) is covered too.
  - `OsRandom` is the only non-test implementation, and main.rs always wires it. The port's docs say every implementation must be cryptographically secure (security rule 4).
  - `crates/fleet-server/clippy.toml` repeats the root settings and bans chrono's `Utc::now` and `Local::now`, `SystemTime::now` and `elapsed`, `Instant::now` and `elapsed`, getrandom's free functions and rand's OS-seeded ones. `SystemClock` and `OsRandom` each carry one approved `#[expect(clippy::disallowed_methods, reason = …)]`.
- **Tests never hard-code `SeededRandom`'s bytes** or the IDs made from them, and snapshots redact them *(the user's addition)*: rand doesn't promise `StdRng`'s output across versions. Tests compare outputs with each other ("same seed, same result"), as fleet-core's do.
- *(as built, group B; the user's decisions in the group B plan)*:
  - **Milliseconds.** `Clock::now()` is documented to return whole milliseconds, and `SystemClock` and `ManualClock` both truncate. Milliseconds are what the database stores and what a v7 ID holds, so a time the server keeps in memory equals the same time loaded back.
  - **`RandomError::{Os { code: i32 }, Unavailable}`.** The OS's error number is context for the operator and never secret. Both traits take `Send + Sync + Debug` as supertraits, so services holding `Arc<dyn Clock>` can derive `Debug`.
  - **ID minting.** fleet-core gets a `V7Id` trait (just `new_v7`), which `define_id!` implements for every ID type, and `system::mint::<I: V7Id>(at, random) -> Result<I, MintError>`, with `MintError::{Random, Id}`. The caller reads its clock once and passes the time to `mint` and to the record, so a record's creation time equals the time inside its ID. `random_bytes::<N>()` is public for P7's tokens.
  - **A failing source for tests** is `SeededRandom::fail_next(error)`: it queues one failure, used in order, and a failed fill doesn't move the stream. No third implementation exists.
  - **The testkit guard is `just testkit-check`** *(the user's decision, amending "a script under `scripts/`, run by `just scripts-test`" above)*. Cargo answers instead of a TOML parser of ours: `cargo tree --workspace -e normal,build --target all -i fleet-testkit` must list nothing but fleet-testkit. Cargo's own resolution covers `workspace = true`, renames through `package =`, target-specific tables, build-dependencies and members anywhere in the workspace (P8's `src-tauri`), with no parser to maintain. `scripts/testkit-check.mjs` runs it and fails on any other package and on any line it can't read; its tests cover the parsing with fixtures (edge-kind headers, `(*)` markers, unreadable lines). It needs Rust, so `just check`, `just ci` and the `deny` CI job run it, not the frontend job. Shown red with a temporary normal dependency in fleet-server and a renamed, Unix-only build dependency in fleet-startup, neither committed.
  - **The bans go further than the list above** *(the user's decision)*: fleet-server's `clippy.toml` also bans uuid's self-minting functions (`Uuid::now_v1`, `now_v6`, `now_v7`, `new_v4`, `new_v7`, `uuid::Timestamp::now`), which read the clock or the OS themselves, and the types `rand::rngs::{ThreadRng, SysRng}` and `getrandom::SysRng`. `allow-invalid` marks only the paths that don't exist in fleet-server's own feature set.
  - **The probe.** A temporary probe, never committed, enabled chrono's `clock`, uuid's `std`, `v1`, `v4`, `v6` and `v7`, getrandom's `sys_rng`, rand with `thread_rng` and tokio's `sync` for fleet-server only, and used every banned path. Clippy flagged all 28 with their reasons: 4 std, 2 chrono, 4 getrandom, 8 rand, 6 uuid and the unbounded channel as `disallowed_methods`, and `ThreadRng` and `SysRng` as `disallowed_types`. rand's `SysRng` is getrandom's re-exported, so clippy names it `getrandom::SysRng`; a second run without the getrandom entry showed that the rand entry fires on its own. The files were restored afterwards.

### stable-check
fleet-startup and fleet-server join the justfile's `stable_crates` in group A, and fleet-api-types in group C, under P0.11's rule: the crates that don't depend on azalea or azalea-auth *(the user's decision)*. fleet-server leaves it in P9.3, when azalea-auth arrives.

### Notes for later groups
- **P6.3 (group B)** *(the user's decision)*: set sqlx's statement logging explicitly when configuring the connection, every statement at `debug` and slow statements at `warn`, since sqlx has logged every statement at `info` in some versions.
- **P6.11 (group E)** *(the user's decisions)*:
  - `serve` calls `warn_if_dev_mode()` right after logging starts.
  - When `bind` is an unspecified address (`0.0.0.0` or `::`), the healthcheck probes loopback (`127.0.0.1` or `::1`) on the same port: connecting to `0.0.0.0` only happens to work on Linux and fails on Windows.
  - The dev config needs an absolute `database.path`, which a committed file can't hold for every machine; how `just dev-server` provides it is group E's question.
- **P7.4:** the `*_file` loader (size cap, permissions, trailing newline) comes with its first user, and the secret-name test's allowlist takes P7's non-secret matches.
- **P7.10:** adds `[http] trusted_proxies`.
- **P9.3:** the `azalea_auth` cap already holds on the server; fleet-server leaves `stable_crates`.

### Dependencies
No new external crate in group A. All of these are already in `[workspace.dependencies]`:
- **fleet-startup:** fleet-core, figment (`toml`, `env`), serde, thiserror, tracing, tracing-subscriber (`env-filter`, `json`). Dev: fleet-testkit, rstest.
- **fleet-testkit** gains figment (`test`) and serde_json.
- **fleet-agent** gains fleet-startup and drops its normal figment dependency; its tests keep figment (`test`, `toml`).
- **fleet-server** (group A): fleet-startup, garde (`derive`), serde (`derive`), thiserror, tracing. Dev: fleet-testkit, figment (`test`), rstest, and tracing-subscriber *(the user's approval during the build, beyond the plan's list)*, so the dev-mode test can check the warning's level through fleet-startup's layer.

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
