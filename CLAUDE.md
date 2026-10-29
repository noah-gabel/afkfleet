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
- **Threat model:** `docs/threat-model.md`. Its "Verified by" cells cite tests as Rust paths (`module::test_name`); `scripts/threat-model-citations.mjs` checks that each exists (`just scripts-test`). Rename a cited test only together with the threat model.
- **Operations:** `docs/runbook.md`
- **Project overview and status:** `README.md` (kept current, see step 6 below)
- **Topic rules:** `.claude/rules/` holds the rules for parts of the work, moved here from this file word for word: the full commands table (`commands.md`), dependencies (`dependencies.md`), Rust details (`rust.md`), fleet-server (`server.md`), tests (`testing.md`) and the frontend (`frontend.md`). Each loads when you open a file it covers. Every plan names the `.claude/rules` files for the areas it touches, reads them before planning, and lists them in its Context, so the plan review can see it. Older ADRs and Plan.md notes cite some of these rules as "CLAUDE.md's …" (its time rule, its wait line, its "A new command changes …" line, its exemption list).

## How to work with Plan.md
1. **Find the current phase.** It's the first phase with unchecked tasks. Don't work on later phases unless the user asks.
2. **Pick the task.** Take the next unchecked task, or the one the user names (e.g. "do P4.3"). Before coding, restate its goal and acceptance criteria in 2–4 lines.
3. **Branch.**
   - Start from an up-to-date `main`: `git switch main`, then `git pull`.
   - Create the task branch `p<phase>/<task-id>-<slug>`, e.g. `p2/p2.6-bot-state-machine`. Phases 0 and 1 each used one branch for the whole phase (`p0/foundation`, `p1/azalea-spike`). From Phase 2 on, a phase's work goes in group branches, one PR each, named in the note under the phase in Plan.md; a group split at its plan review gets the branches its planning names.
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
   - Keep `.claude/rules/commands.md` in sync with the `justfile`.
7. **Commit, push, open a PR.**
   - Small commits with messages like `P2.6: add Stop transition`.
   - Push only the task branch: `git push -u origin <branch>`.
   - Open the PR with `gh pr create`. Title: `P2.6: <task name>`. Body: goal, what changed, deviations, every new or changed snapshot, and the Definition of Done checklist from below.
   - Then **stop and wait**. The user reviews and merges.
8. **Stay in scope.** Keep changes to the task. Propose refactors outside it; don't do them silently.
9. **Finish the phase.** When a phase is done, check its **Definition of Done** point by point and report.
10. **Open questions.** Don't improvise around them. Each open question goes to its own read-only subagent, given the options neutrally. The subagent reads the whole project and returns one decision: the option, why, the rejected ones, the later phases it affects and how hard it is to change. The plan lists them under "Decided without you" (S1, S2, …), each marked "security" where it touches security or permissions, for the user's review. Names follow the naming rules and are the AI's. Only these go to the user directly, one at a time:
    1. A decision whose chosen option would add, loosen or except a rule that binds the AI (this file's rules, lints, `#[expect]`, clippy and deny bans, coverage gates, CI, dependencies, manual tests, permissions).
    2. A genuine conflict between decisions the ADRs record as the user's, or between such a decision and the code, where every sensible option overturns one of them.
    3. Matters of taste (looks, texts friends read, UX).

    If at least one sensible option respects every rule and every recorded decision, the subagent decides, and the decision goes into "Decided without you". When unsure whether a question is (1)–(3), ask.

## Git & GitHub rules
- **Never:** merge a PR, push to `main`, force-push, or rewrite history that's already pushed. The ruleset on `main` blocks most of this anyway; don't try to work around a rejection.
- **Allowed `gh` commands:** `gh pr create`, `gh pr view`, `gh pr diff`, `gh pr checks`, `gh run list`, `gh run view` (including `--log-failed`). Nothing else: no `gh pr merge`, `gh api`, `gh repo …`, `gh secret`, `gh auth`, `gh workflow`, `gh release`, `gh gist`.
- Every `gh` command and every `git push` asks the user for approval. That's intended; don't look for ways around it.
- **When CI fails on your PR:** read the log (`gh run view --log-failed`), fix it on the same branch and push again.
- **Stay in this repository.** Never read, change or run anything in other folders or other repositories.
- **Never ask the user to paste tokens, passwords, keys or real account data** into the chat. Steps that need real credentials (P1.8, P9 account linking) are done by the user.

## Commands
The full table, with every recipe and how to write one, is in `.claude/rules/commands.md`, which loads when you open the `justfile` or `scripts/`. The ones most tasks need:
- `just check`: formatting, clippy, docs, nextest, doctests, the generated-code and hardening checks, the `scripts/` tests, biome, `tsc`, Vitest and `just e2e`. **Run it before saying a task is done.**
- `just test [crate]`: nextest for the workspace or one crate.
- `just test-slow`: the container tests (needs Docker), after touching `fleet-mc`, Docker files or anything container-based.
- `just fmt`; `just gen` after changing a DTO; `just db-prepare` after changing a query or adding a migration.
- `just cov` (the coverage gates) and, on Windows, `just cov-desktop` (the desktop crate's gate).

**The user only; the AI never runs them:** `just test-real-account`, `just test-ms-account`, `just test-managed-account`, every `just dod-*` recipe, and the `manual` nextest profile. `just dev-bots` needs a terminal, so the AI doesn't run it either.

## Architecture rules
**Crates and dependencies** (full table in Plan.md §4):
- `fleet-core` is pure: no IO, no tokio, no azalea. It contains domain types, the bot state machine, policies, `authorize()` and the Minecraft port traits.
- **Only `fleet-mc` may depend on `azalea`. No member depends on `azalea-auth` directly**: it enters only through azalea's own crates, and a `deny.toml` wrapper entry enforces it (ADR-0018).
- azalea code runs only on the MC host threads in `fleet-mc` (current-thread runtime + `LocalSet`). Never call azalea from a normal tokio task.

**Bot lifecycle:**
- Every lifecycle change goes through `fleet_core::bot::transition()`. Actors execute the effects it returns and never decide state themselves.
- Disconnect handling follows the core classifier. Never reconnect against a duplicate login: that means a human is playing.

**Ports:**
- In the runtime, ports are generic traits with `fn …(&self) -> impl Future<Output = T> + Send`.
- Server ports used as `Arc<dyn Trait>` take `#[async_trait]`. When mocking them, put `#[automock]` **above** `#[async_trait]`.

**Server layering:** `http/handlers` → `app` services → `ports` → `infra`.
- Handlers only parse input, authorize, call a service and map the result. No business logic, no SQL.
- Services own use cases, transactions and audit entries.
- Writes go through `Store::write()`, a `WriteTx` on the one write connection, whose `commit(entry)` takes the audit entry. Its few named exemptions, each with the user's approval and an ADR note, and the rule for a new one, are in `.claude/rules/server.md`.

**Shared types:**
- Every API DTO lives in `fleet-api-types` (serde + garde + ts-rs + utoipa). A test keeps its `OpenAPI` schemas equal to the TypeScript export.
- Never hand-write a TypeScript type that mirrors a Rust DTO.

**Config:**
- figment, with secrets only through `*_file` paths.
- Binaries stay thin: `main.rs` wires things up; logic lives in the library part.

## Crate registry: one crate per concern
**Never add a dependency or swap a crate without asking the user first.** If they approve, write an ADR and update Plan.md §5 and the registry in `.claude/rules/dependencies.md`.
- Before using any crate API, **check it on docs.rs for the pinned version**. These crates changed recently: sqlx 0.9, tonic 0.14 (+ `tonic-prost`), rand 0.10, argon2 0.6, chacha20poly1305 0.11, sha2 0.11, reqwest 0.13, keyring v4, azalea.
- The registry's tables (every Rust crate and npm package allowed, by concern) and how to declare a dependency are in `.claude/rules/dependencies.md`.


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
- **Every spawned task has an owner** (`JoinSet` / `TaskTracker`) and a child `CancellationToken`. No fire-and-forget `tokio::spawn`. There are four exceptions:
  - four approved exceptions, each owned and ended in its own way: fleet-mc's host threads, the bot actor's teardown task, fleet-client's refresh and code-step tasks, and the desktop keychain's blocking calls (ADR-0011, ADR-0013, ADR-0017; the details are in `.claude/rules/rust.md`).
- **`tokio::select!` branches must be cancel-safe**, or have a comment explaining why it's fine.
- **Overload is an explicit error.** When a queue is full or a rate limit is hit, return `QueueFull`, `RateLimited` or `429`. Never wait forever.

## Security rules (non-negotiable)
1. **Every HTTP route** is registered with `public_route` (allowlist), `authed_route`, `step_up_route` (an authed route that needs the password entered again within 5 minutes; its handler takes `SteppedUpUser`) or `setup_route` (TOTP enrollment with a setup token; its handler takes `SetupToken`). A route that needs a fresh step-up registers with `step_up_route`, never `authed_route`. Authed handlers call `authorize()`. If the actor can't View a resource, answer **404**.
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
12. **Never read secret files:** `secrets/`, `.env*`, `*.pem`, `*.key` or `target/dev-server/vault-key-*`. Never read, list or print anything inside `target/dod-run/` either: the DoD checks' folder holds the user's real account, sealed, its vault key, its PKI, the agent's identity and their logs, and only the user's `dod-*` recipes touch it (the user's choice, ADR-0020, U1, U33). Never read or print the desktop app's Windows Credential Manager entries either (service `io.github.noah-gabel.afkfleet`, one per server origin); tests use a test-only service, `io.github.noah-gabel.afkfleet.test-<process id>`, and delete their entry (ADR-0017). When development needs a secret file, a `just` recipe generates it into a gitignored folder; never write or print one yourself. The dev vault key, for example, is created by `just dev-server` itself as `target/dev-server/vault-key-1`, next to the dev database it encrypts (ADR-0016). Any other one goes into the gitignored `secrets/` folder, through a recipe the user runs, except the DoD checks' own, which their recipes keep in `target/dod-run/` (U1).

## Testing conventions
**Where tests go**
- Unit tests: `#[cfg(test)] mod tests` at the bottom of the file.
- Component and integration tests: `crates/<crate>/tests/*.rs`.
- Slow tests (containers, real Minecraft) have names starting with `slow_`.
- Manual tests need the user's real credentials and have names starting with `manual_`. **Only the user runs them**, with `just test-real-account` (fleet-mc), `just test-ms-account` (fleet-server's Microsoft check) or `just test-managed-account` (fleet-integration's Phase 10 check). The AI never runs those recipes or the `manual` nextest profile, and no other profile includes them.

**How to write them**
- Names describe behavior, e.g. `login_with_wrong_password_returns_401_and_counts_attempt`.
- Arrange / Act / Assert, one behavior per test. Use `rstest` `#[case]` for tables.
- **Time:** `#[tokio::test(start_paused = true)]` plus `tokio::time::advance`. Never a real `sleep`.
  - **Never paused time with sqlx.** It runs each SQLite connection on a worker thread, so the runtime looks idle while it waits, and paused time would jump ahead and fire its timeouts early. A test that waits for a pool timeout lowers it (`DatabaseOptions`) instead (ADR-0015).
  - `governor` (rate limits) and `moka` (caches with expiry) keep their own clocks, which paused tokio time doesn't control. Inject a clock for them (governor supports custom clocks), or ask the user before testing them another way.
  - The approved real-time exceptions (the `manual_` tests that wait on Microsoft, fleet-integration's one wait helper, tests over real sockets, the desktop keychain's tests) are in `.claude/rules/testing.md`.
- **Randomness:** seeded `StdRng`.
- **The server's time and randomness** come through fleet-core's `Clock` and `SecureRandom` ports. Tests use fleet-testkit's `ManualClock` and `SeededRandom` (`fail_next` for a failing source) and never hard-code `SeededRandom`'s bytes or the IDs made from them; snapshots redact them (ADR-0015).
- **Property tests:** proptest's default is 256 cases. CI sets `PROPTEST_CASES=1000`.

**Doubles and fixtures**
- Fakes come from `fleet-testkit`. Use `mockall` only to assert interactions.
- DB tests use `#[sqlx::test]`, which gives each test a fresh SQLite with migrations applied.
- Config tests, log tests and HTTP tests have helpers of their own: see `.claude/rules/testing.md`.

**Assertions**
- Snapshots use `insta` with redactions for IDs and timestamps. **Never accept them blindly.** You can't use the interactive `cargo insta review`, so:
  1. Run the tests and read every `.snap.new` file.
  2. Accept with `cargo insta accept` only when the content is right.
  3. List every new or changed snapshot in the PR description. The user reviews them in the PR diff.
- Every error path of a public function has a test. Every bug fix starts with a failing regression test.

**Hard rules**
- `unwrap` and `expect` are allowed in tests, via `clippy.toml`.
- **Never delete, `#[ignore]`, or weaken a failing test to get to green.** Fix the code or ask.
- Coverage gates: `fleet-core` ≥ 90 %, `fleet-runtime` ≥ 85 %, auth and vault modules ≥ 90 %, the Microsoft accounts' modules ≥ 90 % (`msauth`), the control plane ≥ 90 % (`control-plane`: fleet-server's `infra/pki/`, `app/enrollment.rs`, `app/agents/` and `grpc/`; ADR-0019), the agent's control plane ≥ 90 % (`agent-control-plane`: fleet-agent's `managed/` and `enroll/`; ADR-0019, U20), `fleet-client` ≥ 90 %, the desktop crate ≥ 90 % (Windows, `just cov-desktop`), workspace ≥ 80 %.

## Frontend rules
The frontend's rules (structure, data and state, safety, quality, and every place a new Tauri command changes) are in `.claude/rules/frontend.md`, which loads when you open a file under `packages/` or `apps/desktop/`. Read it before planning frontend work.
- A call whose request or answer carries a secret is a direct call, never a TanStack query or mutation (ADR-0017, ADR-0018); the full list is in `.claude/rules/frontend.md`.

## Environment
- **Dev machine:** Windows 11, already set up by the user.
  - **Your shell tool is Git Bash.** Claude Code's PowerShell tool is turned off in the user's settings. Write POSIX shell commands.
  - The `justfile` runs recipes with **PowerShell 7** (`pwsh`) on Windows and `sh` on Linux.
  - Docker Desktop (WSL 2 backend) runs the Minecraft test server and the compose stack. It must be running for `just mc-up` and `just test-slow`; if Docker isn't reachable, ask the user to start it.
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
