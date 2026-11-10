# CLAUDE.md: afkfleet

## Project
afkfleet is a self-hosted fleet of **Minecraft Java AFK bots**. It has three parts:
- **Bots:** Rust on `azalea`. Each bot is one actor that heals itself and runs modes such as `afk` and `farm`.
- **Admin server:** axum + SQLite. It talks to the bot agents over gRPC with mTLS.
- **Desktop app:** Tauri v2 for Windows with a React/TypeScript UI, shared with friends. It has roles, invite-only registration and mandatory 2FA for every user.

**Priorities, in this order: security → stability → clean architecture → features.**

**Repository:** public on GitHub, licensed under GPL-3.0-or-later (`LICENSE`). `main` is protected by a ruleset (pull request required, squash merge only, no force pushes). The user reviews and merges every pull request (ADR-0025).

Where things are:
- **Roadmap and design:** `Plan.md`. It holds the phases and these key sections:
  - §5 crate registry
  - §6 fault tolerance
  - §7 security model
  - Appendix: config, API, protocol, schema, state machine
- **Decisions:** `docs/adr/`
- **Threat model:** `docs/threat-model.md`. Its "Verified by" cells cite tests as Rust paths (`module::test_name`); `scripts/threat-model-citations.mjs` checks that each exists (`just scripts-test`). Rename a cited test only together with the threat model.
- **Operations:** `docs/runbook.md`
- **Project overview and status:** `README.md` (kept current, see "A task", step 6)
- **Topic rules:** `.claude/rules/` holds the rules for parts of the work: the full commands table (`commands.md`), dependencies (`dependencies.md`), Rust details (`rust.md`), fleet-server (`server.md`), tests (`testing.md`) and the frontend (`frontend.md`). Each loads when you open a file it covers; read the ones a task touches before you plan it. Older ADRs and Plan.md notes cite some of these rules as "CLAUDE.md's …".

## How we work (ADR-0025)
1. **You decide.** Plan briefly in the chat, then build without waiting for approval. Every decision you'd once have asked about (design, dependencies, texts friends read, UX, or an exception a rule here or in `.claude/rules/` says needs "the user's approval") goes into the PR description under **Decided myself**, each with one line of why. Anything that weakens a security control (lints and `#[expect]`, the CSP, Tauri capabilities, rate limits, `deny.toml`, coverage gates) is listed under **Weakened** too, so the user can't miss it.
2. **Only these stop you** and go to the user: crossing a hard limit below, an action that can't be undone, or a genuine conflict with a decision an ADR records as the user's. Say in a few lines what's blocked, the options and your recommendation. Where a rule here, in `.claude/rules/` or in an older ADR still says to ask, wait for the user or stop, decide instead and record it under **Decided myself**.
3. **Hard limits.** `.claude/settings.json` denies what Claude Code can enforce; the rest is yours to keep.
   - **Secrets:** never read, print, copy or list `secrets/`, any `.env*`, `*.key`, `*.pem` and vault key files (`target/dev-server/vault-key-*`), anything inside `target/dod-run/` (the DoD checks' folder: the user's real account, sealed, its key, PKI and logs), the desktop app's Windows Credential Manager entries (service `io.github.noah-gabel.afkfleet`; tests use `io.github.noah-gabel.afkfleet.test-<process id>` and delete their entry), or anything of the user's real host or real accounts. When development needs a secret file, a `just` recipe generates it into a gitignored folder (the dev vault key: `just dev-server`); never write or print one yourself. Never ask the user to paste tokens, passwords, keys or real account data into the chat.
   - **GitHub:** never merge a pull request, push to `main`, force-push, push a tag, create a release, or change the repository's settings, secrets, workflows' permissions or rulesets (no `gh pr merge`, `gh release`, `gh repo`, `gh secret`, `gh api` writes). Pushing your own branches (`git push -u origin <branch>`) and opening PRs is fine.
   - **Real systems:** never connect to or act on the user's real server or real Microsoft accounts: never `just test-real-account`, `just test-ms-account`, `just test-managed-account`, any `just dod-*`, `just updater-sign` or the `manual` nextest profile.
   - **Privacy:** never name the user's hosting provider, host, address or DNS name anywhere in git, PRs or CI (ADR-0022, U2).
   - **No paid services.**
   - **Your own limits:** Claude Code's settings (`.claude/settings*.json`, `~/.claude/settings*.json`) are the user's: propose a change under **Decided myself** in the PR description, and the user makes it.
   - **Irreversible cleanups:** never delete `target/` as a whole (it holds `target/dod-run` and the dev vault key), and never remove Docker volumes, images or networks outside your own project names (so never `cargo clean`, `docker system prune` or `docker volume prune`).
4. **Everything else is allowed without asking:** any build, test, Docker under your own project names, package installs, and file changes anywhere in this repository, `CLAUDE.md`, `.claude/` and CI included: the PR shows them. Stay in this repository: library and crate sources are read on docs.rs or the crate's own repository on the web, never from the cargo registry or the toolchain folders.
5. **One PR per task or group;** the user reviews and merges it. CI is watched after every push (step 8 below).
6. **Subagents** when they help. Each brief says the subagent is read-only (it creates, edits, moves or deletes no file, and runs no command that writes, installs, builds or changes git state) and stays in this repository.

## A task
1. **Find the current phase:** the first phase with unchecked tasks. Don't work on later phases unless the user asks.
2. **Plan it in the chat:** the task's goal, its acceptance criteria and your approach, in a few lines; then build.
3. **Branch** from an up-to-date `main` (`git switch main`, `git pull`): `p<phase>/<task-id>-<slug>`, e.g. `p2/p2.6-bot-state-machine`, or the group branch Plan.md names. One task or group per branch; if it needs an unmerged PR, say so in its description.
4. **TDD.**
   1. Write the tests first.
   2. **Run them and show they fail for the right reason.** A Rust test that doesn't compile doesn't count. First add a stub with the real signature that compiles and returns a wrong value (an empty `Vec`, a default value, a fixed error variant), so the run ends in a failed assertion. `todo!()` is not a stub: it's banned by the lints.
   3. Implement the minimum to make them pass.
   4. Refactor.
   5. Everything green.

   Tasks marked 🔴 in `Plan.md` are strictly test-first. Unmarked tasks (scaffolding, config, wiring) are verified by `just check`, CI or the task's demo, but any logic they add still gets tests.
5. **Check.** Run `just check`. Also run `just test-slow` if you touched `fleet-mc`, Docker files or anything container-based, and `just shots` if you touched `packages/`, `apps/desktop/` or the screenshots' own files (ADR-0020, U125).
6. **Record.**
   - Tick the box in `Plan.md`, and note any deviation under the task as `> Note (P4.3): …`.
   - Write an ADR (`docs/adr/NNNN-title.md`) for any design decision.
   - Update `README.md` whenever setup steps, commands, configuration or user-visible behavior change. When a phase is finished, update its **Status** section.
   - Keep `.claude/rules/commands.md` in sync with the `justfile`.
7. **Commit, push, open the PR.** Small commits with messages like `P2.6: add Stop transition`. Push with `git push -u origin <branch>`, then `gh pr create`. Title: `P2.6: <task name>`. Body: the goal, what changed, deviations, every new or changed snapshot, **Decided myself**, **Weakened** (or "none"), and the Definition of Done checklist below. When later commits change what it lists, update it with `gh pr edit <n> --body-file <file>`.
8. **Watch CI after every push** (ADR-0020). Straight after it, in a background Bash: `sleep 30; gh pr checks <n> --watch --fail-fast --interval 30 > target/ci.txt 2>&1; echo "exit $?"`. Exit 0 is all green; anything else means act now. Never pipe the watch: `| tail` prints nothing until the last job ends, so a red job sits unseen behind a slow one.
   - `gh pr checks <n>` lists the red jobs; during the run, `gh run view --job <id>` shows a job's annotations, and its recipe can run locally.
   - `gh run view --log-failed` opens only when the whole run has ended, and a push cancels the running one. If you still need a log, wait for the run's end before pushing.
   - While a job is red for a reason already explained (a `deps/` PR waiting for its merge), watch without `--fail-fast`, or it stops at that job and hides the rest.
   - A job red in CI and green here isn't explained until its log says why: CI's runners are slower than this PC, and some of what they check is fetched fresh.
9. **Stay in scope.** Keep changes to the task; propose refactors outside it in the PR.
10. **Finish the phase:** when a phase is done, check its **Definition of Done** point by point and report.

## Commands
The full table, with every recipe and how to write one, is in `.claude/rules/commands.md`, which loads when you open the `justfile` or `scripts/`. The ones most tasks need:
- `just check`: formatting, clippy, docs, nextest, doctests, the generated-code and hardening checks, the `scripts/` tests, biome, `tsc`, Vitest and `just e2e`. **Run it before saying a task is done.**
- `just test [crate]`: nextest for the workspace or one crate.
- `just test-slow`: the container tests (needs Docker), after touching `fleet-mc`, Docker files or anything container-based.
- `just fmt`; `just gen` after changing a DTO; `just db-prepare` after changing a query or adding a migration.
- `just cov` (the coverage gates) and, on Windows, `just cov-desktop` (the desktop crate's gate).

**The user only; the AI never runs them:** `just test-real-account`, `just test-ms-account`, `just test-managed-account`, every `just dod-*` recipe, `just updater-sign` (the release's signature, with the maintainer's private key; ADR-0022, S24), `just update-rehearsal` (installs and updates an app on the user's PC; R20), and the `manual` nextest profile. `just dev-bots` needs a terminal, so the AI doesn't run it either. Nor does the AI run `just dev-app` or launch an exe from `just app-build`: the only file it launches is `target/app-check/afkfleet-check.exe`, built by `just app-build-check`, under its own identifier, so it can never reach the user's running app (ADR-0020, U87, S99).

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
- Writes go through `Store::write()`, a `WriteTx` on the one write connection, whose `commit(entry)` takes the audit entry. Its few named exemptions, each with an ADR note, and the rule for a new one, are in `.claude/rules/server.md`.

**Shared types:**
- Every API DTO lives in `fleet-api-types` (serde + garde + ts-rs + utoipa). A test keeps its `OpenAPI` schemas equal to the TypeScript export.
- Never hand-write a TypeScript type that mirrors a Rust DTO.

**Config:**
- figment, with secrets only through `*_file` paths.
- Binaries stay thin: `main.rs` wires things up; logic lives in the library part.

## Crate registry: one crate per concern
**Adding or swapping a dependency is yours to decide:** list it under **Decided myself**, write an ADR, and update Plan.md §5 and the registry in `.claude/rules/dependencies.md`. A new pin is at least 7 days old; a security fix may be newer, noted in the PR.
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

Don't silence lints with `#[allow]`. If an exception is truly needed, use `#[expect(lint, reason = "…")]`, listed under **Weakened** in the PR.

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
- **Every spawned task has an owner** (`JoinSet` / `TaskTracker`) and a child `CancellationToken`. No fire-and-forget `tokio::spawn`. There are four approved exceptions, each owned and ended in its own way: fleet-mc's host threads, the bot actor's teardown task, fleet-client's refresh and code-step tasks, and the desktop keychain's blocking calls (ADR-0011, ADR-0013, ADR-0017; the details are in `.claude/rules/rust.md`).
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
10. **No silent weakening.** A change that weakens lints, the CSP, Tauri capabilities, rate limits, cargo-deny rules or coverage gates comes with an ADR and is listed under **Weakened** in its PR.
11. **Nothing secret or personal in git.** The repo is public.
    - Never commit secrets, `.env` files, databases, keys or real account data.
    - Never commit personal data either: real names, email addresses, Windows usernames or home paths (`C:\Users\…`), IP addresses, real server addresses or domains, Minecraft account names or UUIDs.
    - Never name the user's hosting, in git, PRs or CI: their provider, their host, its address or DNS name, or that they use any particular cloud or DNS service. Steps that depend on a provider are written generically ("open 80 and 443 in your provider's firewall and the host's"); the runbook is a guide for any Linux host with Docker (the user's rule, ADR-0022, U2).
    - Examples and tests use placeholders: `example.com`, `192.0.2.10`, `AfkBot1`, `C:\path\to\afkfleet`.
    - The one exception is the repository's own GitHub URL (e.g. `repository` in `Cargo.toml`). It's public anyway.

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
  - `governor` (rate limits) and `moka` (caches with expiry) keep their own clocks, which paused tokio time doesn't control. Inject a clock for them (governor supports custom clocks), or say under **Decided myself** how else they're tested.
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
- **Never delete, `#[ignore]`, or weaken a failing test to get to green.** Fix the code; a test that is itself wrong is fixed with its reason under **Decided myself**.
- Coverage gates: `fleet-core` ≥ 90 %, `fleet-runtime` ≥ 85 %, auth and vault modules ≥ 90 %, the Microsoft accounts' modules ≥ 90 % (`msauth`), the control plane ≥ 90 % (`control-plane`: fleet-server's `infra/pki/`, `app/enrollment.rs`, `app/agents/` and `grpc/`; ADR-0019), the agent's control plane ≥ 90 % (`agent-control-plane`: fleet-agent's `managed/` and `enroll/`; ADR-0019, U20), `fleet-client` ≥ 90 %, the desktop crate ≥ 90 % (Windows, `just cov-desktop`), workspace ≥ 80 %.

## Frontend rules
The frontend's rules (structure, data and state, safety, quality, and every place a new Tauri command changes) are in `.claude/rules/frontend.md`, which loads when you open a file under `packages/` or `apps/desktop/`. Read it before planning frontend work.
- A call whose request or answer carries a secret is a direct call, never a TanStack query or mutation (ADR-0017, ADR-0018); the full list is in `.claude/rules/frontend.md`.

## Environment
- **Dev machine:** Windows 11, already set up by the user.
  - **Your shell tool is Git Bash.** Write POSIX shell commands. Claude Code's PowerShell tool may be there too; don't use it for work. The settings' deny rules cover it as well.
  - The `justfile` runs recipes with **PowerShell 7** (`pwsh`) on Windows and `sh` on Linux.
  - Docker Desktop (WSL 2 backend) runs the Minecraft test server and the compose stack. It must be running for `just mc-up`, `just test-slow` and `just shots`; if Docker isn't reachable, ask the user to start it.
- **Toolchain:** `rust-toolchain.toml` pins a **dated nightly**, and rustup installs it automatically.
  - Change it only in a deliberate Minecraft-version bump.
  - That bump changes nightly, azalea and the test server's `VERSION` together, as in `docs/releasing.md`.
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
- [ ] The PR description lists **Decided myself** and **Weakened** (or "none").
- [ ] The work is on its task branch, pushed, with a PR open, and nothing was pushed to `main`.
