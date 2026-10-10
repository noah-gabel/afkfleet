# CLAUDE.md: afkfleet

## Project
afkfleet is a self-hosted fleet of **Minecraft Java AFK bots**. It has three parts:
- **Bots:** Rust on `azalea`. Each bot is one actor that heals itself and runs modes such as `afk` and `farm`.
- **Admin server:** axum + SQLite. It talks to the bot agents over gRPC with mTLS.
- **Desktop app:** Tauri v2 for Windows with a React/TypeScript UI, shared with friends. It has roles, invite-only registration and mandatory 2FA for every user.

**Priorities, in this order: security → stability → clean architecture → features.**

**Repository:** public on GitHub, licensed under GPL-3.0-or-later (`LICENSE`). `main` is protected by a ruleset (pull request required, squash merge only, no force pushes). Only the user merges.

Where things are:
- **Roadmap and design:** `Plan.md`. It holds the phases and these key sections:
  - §5 crate registry
  - §6 fault tolerance
  - §7 security model
  - Appendix: config, API, protocol, schema, state machine
- **Decisions:** `docs/adr/`
- **Threat model:** `docs/threat-model.md`
- **Operations:** `docs/runbook.md`
- **Project overview and status:** `README.md` (kept current, see step 6 below)

## How to work with Plan.md
1. **Find the current phase.** It's the first phase with unchecked tasks. Don't work on later phases unless the user asks.
2. **Pick the task.** Take the next unchecked task, or the one the user names (e.g. "do P4.3"). Before coding, restate its goal and acceptance criteria in 2–4 lines.
3. **Branch.**
   - Start from an up-to-date `main`: `git switch main`, then `git pull`.
   - Create the task branch `p<phase>/<task-id>-<slug>`, e.g. `p2/p2.6-bot-state-machine`. Phases 0 and 1 each use one branch for the whole phase: `p0/foundation` and `p1/azalea-spike`. Phase 2 uses five group branches, one PR each, with one commit per task (Plan.md, the note under Phase 2). Phase 3 does the same with five group branches, plus one task branch for P3.9 (Plan.md, the note under Phase 3). Phase 4 uses five group branches (Plan.md, the note under Phase 4). Phase 5 uses four group branches (Plan.md, the note under Phase 5). Phase 6 uses five group branches (Plan.md, the note under Phase 6).
   - One task, or one Phase 2 group, per branch. If the previous PR isn't merged yet, stop and tell the user instead of stacking branches.
4. **TDD.**
   1. Write the tests first.
   2. **Run them and show they fail for the right reason.** A Rust test that doesn't compile doesn't count. First add a stub with the real signature that compiles and returns a wrong value (an empty `Vec`, a default value, a fixed error variant), so the run ends in a failed assertion. Show that output. `todo!()` is not a stub: it's banned by the lints.
   3. Implement the minimum to make them pass.
   4. Refactor.
   5. Everything green.

   Tasks marked 🔴 in `Plan.md` are strictly test-first. Unmarked tasks (scaffolding, config, wiring) are verified by `just check`, CI or the task's demo, but any logic they add still gets tests.
5. **Check.** Run `just check`. Also run `just test-slow` if you touched `fleet-mc`, Docker files or anything container-based.
6. **Record.**
   - Tick the box in `Plan.md`.
   - Note any deviation under the task as `> Note (P4.3): …`.
   - Write an ADR (`docs/adr/NNNN-title.md`) for any design decision.
   - Update `README.md` whenever setup steps, commands, configuration or user-visible behavior change. When a phase is finished, update its **Status** section.
   - Keep the commands table below in sync with the `justfile`.
7. **Commit, push, open a PR.**
   - Small commits with messages like `P2.6: add Stop transition`.
   - Push only the task branch: `git push -u origin <branch>`.
   - Open the PR with `gh pr create`. Title: `P2.6: <task name>`. Body: goal, what changed, deviations, every new or changed snapshot, and the Definition of Done checklist from below.
   - Then **stop and wait**. The user reviews and merges.
8. **Stay in scope.** Keep changes to the task. Propose refactors outside it; don't do them silently.
9. **Finish the phase.** When a phase is done, check its **Definition of Done** point by point and report.
10. **Ask on ambiguity.** If the plan is ambiguous or conflicts with reality (an API changed, the spike found something), stop and ask. Don't improvise around it.

## Git & GitHub rules
- **Never:** merge a PR, push to `main`, force-push, or rewrite history that's already pushed. The ruleset on `main` blocks most of this anyway; don't try to work around a rejection.
- **Allowed `gh` commands:** `gh pr create`, `gh pr view`, `gh pr diff`, `gh pr checks`, `gh run list`, `gh run view` (including `--log-failed`). Nothing else: no `gh pr merge`, `gh api`, `gh repo …`, `gh secret`, `gh auth`, `gh workflow`, `gh release`, `gh gist`.
- Every `gh` command and every `git push` asks the user for approval. That's intended; don't look for ways around it.
- **When CI fails on your PR:** read the log (`gh run view --log-failed`), fix it on the same branch and push again.
- **Stay in this repository.** Never read, change or run anything in other folders or other repositories.
- **Never ask the user to paste tokens, passwords, keys or real account data** into the chat. Steps that need real credentials (P1.8, P9 account linking) are done by the user.

## Commands (`justfile`, created in P0.7; keep this table in sync)
Recipes run in **PowerShell 7** (`pwsh`) on Windows and in `sh` on Linux CI, so every recipe line must work in both:
- one command per line; no `&&` or `||`
- set environment variables with just's `export` or a `$`-parameter, never with shell syntax (`$env:X` or `X=…`)
- anything more complex goes into a small script under `scripts/`

| Command | What it does |
|---|---|
| `just check` | `cargo fmt --check`, clippy (`--all-targets -D warnings`), `cargo doc --no-deps --workspace` with `RUSTDOCFLAGS="-D warnings"`, nextest default profile, doctests, `db-check`, `testkit-check`, the `scripts/` tests, and biome + `tsc --noEmit` once the frontend exists. **Run before saying a task is done.** |
| `just test [crate]` | nextest for the workspace or one crate |
| `just test-slow` | Pulls the server image and builds the agent's image, then runs the nextest `slow` profile with fleet-mc's test-only `fault-injection` feature, one Minecraft container or compose stack at a time (needs Docker) |
| `just test-real-account` | **The user only; the AI never runs it.** nextest `manual` profile: the user's real token from `secrets/p1.8-account.txt` joins a local online-mode container and sends signed chat (needs Docker) |
| `just cov` | cargo-llvm-cov with the coverage gates (per-crate and per-module gates are checked by a script over the JSON report) |
| `just deny` | `cargo deny check` |
| `just fmt` | `cargo fmt` + biome format |
| `just gen` | Export ts-rs types to `packages/ui/src/generated/`; proto codegen check |
| `just db-prepare` | Rebuilds the throwaway `target/sqlx-prepare.db` from the migrations and runs `cargo sqlx prepare -- --all-targets` in `crates/fleet-server`, writing the offline query data to `crates/fleet-server/.sqlx/` (needs sqlx-cli 0.9.0). Run it after changing a query or adding a migration |
| `just db-check` | The same database, then `cargo sqlx prepare --check`: fails if `.sqlx/` is missing or differs from a query (the `sqlx-offline` job) |
| `just migrations-check` | Fails if a migration that exists on `origin/main` was modified, deleted or renamed. Needs an up-to-date `origin/main`; CI fetches it |
| `just mc-up` / `just mc-down` | Local offline-mode Minecraft server (`itzg/minecraft-server`); `mc-down` stops the agent too and deletes the world |
| `just stack-up` | Builds the agent's image and starts the local server plus the agent (`deploy/dev/agent.compose.toml`) in Docker, waiting until both are healthy |
| `just dev-server` / `just dev-agent` | Run server / agent with the configs in `deploy/dev/` |
| `just demo-agent [minutes]` | Phase 5's demo: the compose stack (project `afkfleet-demo`) for 60 minutes, or `minutes`, with one server restart at half time; prints a summary and a verdict, saves its logs under `target/demo-agent/` (needs Docker) |
| `just dev-app` | `pnpm tauri dev` |
| `just ui-test` / `just e2e` | Vitest / Playwright |
| `just ci` | Everything CI runs |
| `just testkit-check` | Fails if any workspace member depends on fleet-testkit outside its dev-dependencies (`cargo tree`, through `scripts/testkit-check.mjs`; ADR-0015) |
| `just fmt-check` · `clippy` · `docs` · `doctest` · `test-ci` · `stable-check` · `db-check` · `migrations-check` · `testkit-check` · `scripts-test` · `ui-check` · `ui-audit` | The building blocks of `check` and `ci`. Each CI job runs one of them, so local and CI runs can't drift apart |

Recipes for tools that arrive in later phases (`gen`, `dev-server`, `dev-app`, `ui-test`, `e2e`) print the phase they arrive in and exit with an error until then.

## Architecture rules
**Crates and dependencies** (full table in Plan.md §4):
- `fleet-core` is pure: no IO, no tokio, no azalea. It contains domain types, the bot state machine, policies, `authorize()` and the Minecraft port traits.
- **Only `fleet-mc` may depend on `azalea`; only `fleet-server` may depend on `azalea-auth`.**
  - `fleet-mc` may use only `azalea::auth::sessionserver` and `azalea::auth::certs`, through azalea's re-export, to join with a server-issued Minecraft token.
  - The Microsoft flows (device code, Microsoft tokens, the account cache) belong to `fleet-server` alone. `fleet-mc`'s `clippy.toml` bans them (ADR-0011).
- azalea code runs only on the MC host threads in `fleet-mc` (current-thread runtime + `LocalSet`). Never call azalea from a normal tokio task.
- `fleet-startup` holds only the binaries' process start-up code: the config loader and its errors, the `[log]` types, the log layer and the panic hook. Internally it depends only on `fleet-core`. Its filter always caps `azalea_auth` at `info` and lets panic reports through; a binary only adds rules through `FilterRules` (ADR-0015).

**Bot lifecycle:**
- Every lifecycle change goes through `fleet_core::bot::transition()`. Actors execute the effects it returns and never decide state themselves.
- Disconnect handling follows the core classifier. Never reconnect against a duplicate login: that means a human is playing.

**Ports:**
- In the runtime, ports are generic traits with `fn …(&self) -> impl Future<Output = T> + Send`.
- Server ports used as `Arc<dyn Trait>` take `#[async_trait]`. When mocking them, put `#[automock]` **above** `#[async_trait]`.

**Server layering:** `http/handlers` → `app` services → `ports` → `infra`.
- Handlers only parse input, authorize, call a service and map the result. No business logic, no SQL.
- Services own use cases, transactions and audit entries.
- Writes go through `Store::write()`, a `WriteTx` on the one write connection. Its `commit(entry)` takes the audit entry, so nothing commits unaudited. Keep it short: no password hashing, crypto or network calls while it's open (ADR-0015).

**Shared types:**
- Every API DTO lives in `fleet-api-types` (serde + garde + ts-rs).
- Never hand-write a TypeScript type that mirrors a Rust DTO.

**Config:**
- figment, with secrets only through `*_file` paths.
- Binaries stay thin: `main.rs` wires things up; logic lives in the library part.

Server module layout:
```
fleet-server/src/
  main.rs  lib.rs  config.rs
  cli/        # serve, migrate, healthcheck, user create-owner, pki, vault rotate-key, backup
  app/        # services: auth, users, accounts, bots, modes, agents, audit, events
  ports/      # repository and provider traits
  infra/      # sqlite/ (sqlx repos), crypto/ (vault, tokens, passwords), msauth/ (azalea-auth adapter)
  http/       # router.rs, middleware/, extractors/, handlers/<resource>.rs, error.rs, ws.rs
  grpc/       # enrollment.rs, control.rs, registry.rs, scheduler.rs, reconciler.rs
```

## Crate registry: one crate per concern
**Never add a dependency or swap a crate without asking the user first.** If they approve, write an ADR and update Plan.md §5 and this list.
- Add dependencies only in `[workspace.dependencies]`; members use `dep.workspace = true`. Declare a crate there when the first member uses it, with the version and feature constraints from Plan.md §5. Cargo warns about unused workspace dependencies.
- **Default features in `fleet-core`:** disable them and enable only what you need. Some crates pull in IO or tokio by default (for example `backon` with its tokio sleeper). `cargo tree -p fleet-core` must show no tokio and no IO crates.
- Every crate inherits `license` (`GPL-3.0-or-later`) and `publish = false` from `[workspace.package]` (`license.workspace = true`, `publish.workspace = true`).
- Before using any crate API, **check it on docs.rs for the pinned version**. These crates changed recently: sqlx 0.9, tonic 0.14 (+ `tonic-prost`), rand 0.10, argon2 0.6, chacha20poly1305 0.11, sha2 0.11, reqwest 0.13, keyring v4, azalea.

**Rust crates**

| Area | Concern | Crate |
|---|---|---|
| Domain & runtime | Minecraft | `azalea` (fleet-mc only) |
| | Chat components, translations | `azalea-chat`, `azalea-language` (fleet-mc only, for its bounded renderer) |
| | MS auth | `azalea-auth` (server only) |
| | Async runtime | `tokio` |
| | Cancellation | `tokio-util` (`CancellationToken`, `TaskTracker`) |
| | Streams | `tokio-stream` |
| | Sink/Stream traits | `futures-util` |
| | dyn async ports | `async-trait` |
| | Retry | `backon` |
| | Rate limits | `governor`, `tower_governor` |
| | Cache / single-flight | `moka` |
| Errors, config, observability | Errors | `thiserror` (libraries), `anyhow` (`main.rs` only) |
| | Serialization | `serde`, `serde_json` |
| | Config | `figment` |
| | Validation | `garde` (DTOs/config); value objects use constructors in core |
| | Logging | `tracing`, `tracing-subscriber` |
| | Metrics | `metrics`, `metrics-exporter-prometheus` |
| | IDs | `uuid` v7 |
| | Time | `chrono` (UTC) |
| | CLI | `clap` |
| | Password prompt | `rpassword` |
| API, transport, persistence | HTTP | `axum` (+ws), `axum-extra`, `tower`, `tower-http` |
| | OpenAPI | `utoipa`, `utoipa-axum` |
| | DB | `sqlx` (sqlite) |
| | gRPC | `tonic`, `tonic-prost`, `prost`, `tonic-prost-build`, `protox` |
| | TLS | `rustls` (**aws-lc-rs provider only**) |
| | PKI | `rcgen` |
| | HTTP client | `reqwest` (rustls) |
| | WS client | `tokio-tungstenite` |
| | TS types | `ts-rs` |
| Security | Password hashing | `argon2` |
| | Encryption | `chacha20poly1305` |
| | Hashing | `sha2` |
| | Constant-time | `subtle` |
| | Encoding | `base64` |
| | Secrets | `secrecy`, `zeroize` |
| | Secure randomness | `getrandom` |
| | Non-security randomness | `rand` |
| | 2FA | `totp-rs` |
| | Password strength | `zxcvbn` |
| Desktop | Shell | `tauri` ≥ 2.11.1, `tauri-build` |
| | Plugins | `tauri-plugin-opener`, `tauri-plugin-single-instance`, `tauri-plugin-updater` |
| | Keychain | `keyring-core` + `windows-native-keyring-store` |
| Tests | Testing crates | `rstest`, `proptest`, `insta`, `mockall`, `testcontainers`, tokio `test-util`, `log` (dev only: fleet-testkit's log capture and fleet-agent's telemetry, each to prove the `log` bridge; fleet-server's one normal use is sqlx's `LevelFilter`, and its logging macros are banned there), `tempfile` (fleet-server's database tests) |
| | Fuzzing | `libfuzzer-sys`, `arbitrary` |

**Frontend packages**

| Concern | Package |
|---|---|
| Package manager | pnpm |
| Build | `vite` |
| UI framework | `react` 19 |
| Language | `typescript` (strict) |
| Server state | `@tanstack/react-query` |
| Routing | `@tanstack/react-router` |
| UI state | `zustand` |
| Validation | `zod` |
| Forms | `react-hook-form` + `@hookform/resolvers` |
| Styling | `tailwindcss` |
| Components | shadcn/ui |
| Icons | `lucide-react` |
| Toasts | `sonner` |
| Tests | `vitest`, `@testing-library/react`, `msw`, `@playwright/test` |
| Lint & format | `@biomejs/biome` |
| Tauri bridge | `@tauri-apps/api` (`apps/desktop` only) |

## Rust rules
**Idioms**
- Edition 2024. Use iterators and `?`.
- Use newtypes for IDs and validated values ("parse, don't validate").
- Constructors are `new`, `try_new` or `TryFrom`; use a builder for more than 3 optional parameters.
- Mark pure functions that return values `#[must_use]`.
- Take `&str` / `&[T]` parameters. No `clone()` just to satisfy the borrow checker without thinking it through.

**Banned in non-test code** (the lints enforce this):
- `unwrap`, `expect`
- `panic!`, `todo!`, `unimplemented!`
- direct indexing or slicing; use `.get()`
- `as` casts that can truncate; use `TryFrom`

Don't silence lints with `#[allow]`. If an exception is truly needed, use `#[expect(lint, reason = "…")]`, and only with the user's approval.

**Crate-local `clippy.toml` files** (`fleet-core`, `fleet-mc`, …) must repeat every setting of the root `clippy.toml`: the test allowances and the `disallowed-methods` bans. Clippy reads only the nearest file and doesn't merge them. `just scripts-test` fails when one is missing (`scripts/clippy-config.mjs`, ADR-0011).

**The one pre-approved exception: generated code.** The prost/tonic output in `fleet-proto` is included in a dedicated module, e.g. `pub mod generated`, with a comment saying it's generated. That module may carry `#[allow(missing_docs, unreachable_pub, clippy::pedantic, …)]`. Use `#[allow]` there, not `#[expect]`, because generated code may or may not trigger each lint. Never put hand-written code in that module.

**Never:** `unsafe`, or `#![feature(...)]`. The nightly toolchain exists only because of azalea.

**Errors**
- One `thiserror` enum per module or boundary.
- Variants carry context (IDs, kinds), not free-form `String`s.
- Convert at boundaries with `From` or `map_err`.
- Internal errors never reach API clients: they get a generic message plus a `request_id`.
- `anyhow` is allowed only in `main.rs`.

**Logging (`tracing`)**
- Use structured fields (`bot_id = %id`) and a span per request or bot.
- Levels:
  - `error`: a human must act
  - `warn`: a fault was recovered
  - `info`: lifecycle events
  - `debug`: details
- Chat content is logged only at `debug`.
- The agent's JSON lines put an event's fields next to the line's own keys, and a duplicate key in one line is ambiguous (most tools keep only one value). So no field may be named `timestamp`, `level`, `target`, `message` (other than tracing's own), `span`, `spans` or `fields`, and no span field `name` (ADR-0014).

**Documentation:** every public item has a `///` doc (including `# Errors`), and every module has a `//!` header explaining its role. This is enforced:
- `missing_docs` is a workspace lint, and CI denies warnings, so an undocumented public item fails the build.
- `just check` runs `cargo doc` with warnings denied, so broken doc links fail too.

## Async & concurrency rules
- **Never block the runtime.** CPU-heavy work (argon2, crypto on large data) goes to `spawn_blocking`. No `std::thread::sleep`, and no blocking file IO in async code.
- **Every network or IO await has a timeout** (`tokio::time::timeout`), or a comment explaining why it doesn't need one.
- **Channels are always bounded.** No `unbounded_channel`: clippy's `disallowed-methods` bans it. The only exceptions are the two channels azalea's API forces on `fleet-mc`, each marked with an approved `#[expect]` (ADR-0011).
- **Never hold a `std::sync::Mutex` guard across `.await`.** Prefer message passing to shared locks.
- **Every spawned task has an owner** (`JoinSet` / `TaskTracker`) and a child `CancellationToken`. No fire-and-forget `tokio::spawn`. There are two exceptions:
  - fleet-mc's host threads: their jobs belong to the thread's `JoinSet` and end with its stop signal (ADR-0011).
  - the bot actor's teardown task: its `JoinSet` owns it, it runs to the end because the port guarantees `disconnect()` finishes, and an aborted actor aborts it too (ADR-0013).
- **`tokio::select!` branches must be cancel-safe**, or have a comment explaining why it's fine.
- **Overload is an explicit error.** When a queue is full or a rate limit is hit, return `QueueFull`, `RateLimited` or `429`. Never wait forever.

## Security rules (non-negotiable)
1. **Every HTTP route** is registered with `public_route` (allowlist) or `authed_route`. Authed handlers call `authorize()`. If the actor can't View a resource, answer **404**.
2. **Validate at the boundary.**
   - DTOs: garde + `#[serde(deny_unknown_fields)]`.
   - Domain values: `fleet-core` constructors.
   - Never trust the client, and that includes our own app.
3. **Every new endpoint** needs all of these:
   - a rate-limit class
   - an audit entry, if it changes state
   - tests for 401, 403/404, 422 and 429
   - a place in the route-coverage test
4. **Secrets**
   - Keep them in `SecretString` / `SecretBox` and load them from files.
   - They never appear in logs, errors, panics, URLs, or `Debug` output. The only exception in URLs is the single-use WebSocket ticket.
   - Use `getrandom` for every secret. Compare secrets and hashes in constant time with `subtle`.
5. **Passwords** go only through the password service (Argon2id). **Tokens** are stored only as SHA-256 hashes.
6. **Microsoft refresh tokens and TOTP secrets** are stored only through the vault (AAD-bound).
   - Agents never receive Microsoft tokens.
   - They only receive short-lived Minecraft session tokens, and only for bots assigned to them.
7. **Text from Minecraft servers is untrusted.** Sanitize it in core, store it as plain text, render it as text.
8. **SQL** only through the `sqlx::query!` / `query_as!` macros with bind parameters. Never build SQL from strings. The approved exceptions are fixed string literals, never formatted and without input (ADR-0015): `begin_with("BEGIN IMMEDIATE")` for write transactions, and, in tests, a pragma read sqlx's macros can't describe (`PRAGMA journal_mode`).
9. **TLS**
   - Never turn off certificate checks; no `danger_*` APIs.
   - Use exactly one rustls provider (aws-lc-rs), installed at startup.
   - The agent gRPC port uses mTLS; Caddy only fronts the HTTP API.
10. **No silent weakening.** Lints, CSP, Tauri capabilities, rate limits, cargo-deny rules and coverage gates change only with the user's explicit approval and an ADR.
11. **Nothing secret or personal in git.** The repo is public.
    - Never commit secrets, `.env` files, databases, keys or real account data.
    - Never commit personal data either: real names, email addresses, Windows usernames or home paths (`C:\Users\…`), IP addresses, real server addresses or domains, Minecraft account names or UUIDs.
    - Examples and tests use placeholders: `example.com`, `192.0.2.10`, `AfkBot1`, `C:\path\to\afkfleet`.
    - The one exception is the repository's own GitHub URL (e.g. `repository` in `Cargo.toml`). It's public anyway.
12. **Never read secret files:** `secrets/`, `.env*`, `*.pem` or `*.key`. When development needs a secret file (e.g. a dev vault key), write a `just` recipe that generates it into the gitignored `secrets/` folder and ask the user to run it.

## Testing conventions
**Where tests go**
- Unit tests: `#[cfg(test)] mod tests` at the bottom of the file.
- Component and integration tests: `crates/<crate>/tests/*.rs`.
- Slow tests (containers, real Minecraft) have names starting with `slow_`.
- Manual tests need the user's real credentials and have names starting with `manual_`. **Only the user runs them**, with `just test-real-account`. The AI never runs that recipe or the `manual` nextest profile, and no other profile includes them.

**How to write them**
- Names describe behavior, e.g. `login_with_wrong_password_returns_401_and_counts_attempt`.
- Arrange / Act / Assert, one behavior per test. Use `rstest` `#[case]` for tables.
- **Time:** `#[tokio::test(start_paused = true)]` plus `tokio::time::advance`. Never a real `sleep`.
  - **Never paused time with sqlx.** It runs each SQLite connection on a worker thread, so the runtime looks idle while it waits, and paused time would jump ahead and fire its timeouts early. A test that waits for a pool timeout lowers it (`DatabaseOptions`) instead (ADR-0015).
  - `governor` (rate limits) and `moka` (caches with expiry) keep their own clocks, which paused tokio time doesn't control. Inject a clock for them (governor supports custom clocks), or ask the user before testing them another way.
- **Randomness:** seeded `StdRng`.
- **The server's time and randomness** come through fleet-core's `Clock` and `SecureRandom` ports. Tests use fleet-testkit's `ManualClock` and `SeededRandom` (`fail_next` for a failing source) and never hard-code `SeededRandom`'s bytes or the IDs made from them; snapshots redact them (ADR-0015).
- **Property tests:** proptest's default is 256 cases. CI sets `PROPTEST_CASES=1000`.

**Doubles and fixtures**
- Fakes come from `fleet-testkit`. Use `mockall` only to assert interactions.
- Config tests run inside figment's `Jail` through `fleet_testkit::jail::in_jail`, which carries the workspace's one approved `#[expect(clippy::result_large_err)]`. Don't write a second helper (ADR-0015).
- Tests of what a log layer prints write into a `fleet_testkit::log_buffer::LogBuffer`. Checks that a secret never reaches a log use `fleet_testkit::log_capture` instead.
- DB tests use `#[sqlx::test]`, which gives each test a fresh SQLite with migrations applied.
- HTTP:
  - **Handler tests:** use `tower::ServiceExt::oneshot`.
  - **Full-stack tests:** run `fleet-client` against an in-process server on `127.0.0.1:0`.

**Assertions**
- Snapshots use `insta` with redactions for IDs and timestamps. **Never accept them blindly.** You can't use the interactive `cargo insta review`, so:
  1. Run the tests and read every `.snap.new` file.
  2. Accept with `cargo insta accept` only when the content is right.
  3. List every new or changed snapshot in the PR description. The user reviews them in the PR diff.
- Every error path of a public function has a test. Every bug fix starts with a failing regression test.

**Hard rules**
- `unwrap` and `expect` are allowed in tests, via `clippy.toml`.
- **Never delete, `#[ignore]`, or weaken a failing test to get to green.** Fix the code or ask.
- Coverage gates: `fleet-core` ≥ 90 %, `fleet-runtime` ≥ 85 %, auth and vault modules ≥ 90 %, workspace ≥ 80 %.

## Frontend rules
**Structure**
- `packages/ui` doesn't know about the platform: it **never imports `@tauri-apps/*`**.
- `apps/desktop` implements `ApiClient` as `TauriApiClient` and injects it.
- Organize by feature: `src/features/<feature>/{components,hooks,routes}` with tests alongside. Shared primitives go in `src/components/ui` (shadcn).

**Data and state**
- Components use hooks (`useBots()`, …) built on TanStack Query over the injected `ApiClient`. **Never call `fetch` or `invoke` inside a component.**
- TanStack Query holds server state; small `zustand` stores hold UI-only state.
- Forms use react-hook-form with zod schemas that mirror the server's limits. The server stays authoritative.
- API types come only from `src/generated/` (ts-rs). **Never edit generated files**; run `just gen`. Biome ignores `src/generated/`.

**Safety**
- Never use `dangerouslySetInnerHTML`, `eval` or `new Function`.
- Chat is rendered as plain text.
- External links open only through the Rust-side opener command.

**Quality**
- TypeScript strict; no `any`; no non-null `!` without a comment.
- Tests use Testing Library (query by role or label), with a `FakeApiClient` or MSW standing in for the API. Playwright covers flows.

## Environment
- **Dev machine:** Windows 11, already set up by the user.
  - **Your shell tool is Git Bash.** Claude Code's PowerShell tool is turned off in the user's settings. Write POSIX shell commands.
  - The `justfile` runs recipes with **PowerShell 7** (`pwsh`) on Windows and `sh` on Linux.
  - Docker Desktop (WSL 2 backend) runs the Minecraft test server and the compose stack. It must be running for `just mc-up` and `just test-slow`; if Docker isn't reachable, ask the user to start it.
  - Tauri needs MSVC Build Tools ("Desktop development with C++") and WebView2 (built into Windows 11). Both are installed.
  - aws-lc-rs needs NASM, which is installed and on `PATH`. CMake isn't needed (it's only for FIPS builds).
  - Installed: Git, GitHub CLI, PowerShell 7, `just`, rustup, Node 24 + pnpm, Docker Desktop. Cargo tools (nextest, llvm-cov, deny, insta) may still need installing in Phase 0. sqlx-cli 0.9.0 is installed SQLite-only: `cargo install sqlx-cli --version 0.9.0 --locked --no-default-features --features sqlite`.
- **Toolchain:** `rust-toolchain.toml` pins a **dated nightly**, and rustup installs it automatically.
  - Change it only in a deliberate Minecraft-version bump.
  - That bump changes nightly, azalea and the test server's `VERSION` together, as in `docs/runbook.md`.
- **Production:** Linux + Docker Compose (`deploy/`).
- Files use LF line endings (`.gitattributes`; the user's git has `core.autocrlf = false`).

## Definition of Done (every task)
- [ ] The tests were written first; they cover the happy path and every error path, and all pass.
- [ ] `just check` is green, with no new warnings and no unexplained `#[expect]`.
- [ ] Non-test code has no `unwrap`/`expect`/`panic`/indexing, and no secrets appear in logs or `Debug`.
- [ ] New endpoints are authorized, rate-limited, audited (if they change state), validated, snapshot-tested and in the route-coverage test.
- [ ] Public items are documented (`missing_docs` and `cargo doc` are clean).
- [ ] Generated artifacts are refreshed if their inputs changed (`just gen`, `just db-prepare`).
- [ ] New or changed snapshots were read before accepting, and are listed in the PR description.
- [ ] `README.md` reflects the change (setup, commands, config, behavior, phase status).
- [ ] The `Plan.md` checkbox is ticked, with a note for any deviation, and an ADR exists for any decision.
- [ ] The work is on its task branch, pushed, with a PR open. Nothing is merged, and nothing was pushed to `main`.
