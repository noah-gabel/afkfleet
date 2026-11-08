# CLAUDE.md: afkfleet

## Project
afkfleet is a self-hosted fleet of **Minecraft Java AFK bots**. It has three parts:
- **Bots:** Rust on `azalea`. Each bot is one actor that heals itself and runs modes such as `afk` and `farm`.
- **Admin server:** axum + SQLite. It talks to the bot agents over gRPC with mTLS.
- **Desktop app:** Tauri v2 for Windows with a React/TypeScript UI, shared with friends. It has roles, invite-only registration and mandatory 2FA for every user.

**Priorities, in this order: security → stability → clean architecture → features.**

**Repository:** public on GitHub, licensed under GPL-3.0-or-later (`LICENSE`). `main` is protected by a ruleset (pull request required, squash merge only, no force pushes). The AI merges a task's PR itself, only through the merge guard (step 7); a PR the guard sends to the user waits for the user's merge (ADR-0023).

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
2. **Pick the task and plan it.** Take the next unchecked task, or the one the user names (e.g. "do P4.3"). Before coding, restate its goal and acceptance criteria in 2–4 lines. Write the plan to `target/review/<slug>/plan.md` and run the plan review until APPROVE (see "Review pipeline" below); questions of categories 1–3 (step 10) and the "Decided without you" items marked security go to the user, one at a time.
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
5. **Check.** Run `just check`. Also run `just test-slow` if you touched `fleet-mc`, Docker files or anything container-based, and `just shots` if you touched `packages/`, `apps/desktop/` or the screenshots' own files (the user's choice, ADR-0020, U125).
6. **Record.**
   - Tick the box in `Plan.md`.
   - Note any deviation under the task as `> Note (P4.3): …`.
   - Write an ADR (`docs/adr/NNNN-title.md`) for any design decision.
   - Update `README.md` whenever setup steps, commands, configuration or user-visible behavior change. When a phase is finished, update its **Status** section.
   - Keep `.claude/rules/commands.md` in sync with the `justfile`.
7. **Commit, review, push, open a PR, merge.**
   - Small commits with messages like `P2.6: add Stop transition`. A message that names the merge command goes through `git commit -F <file>`: the merge guard reads every shell command's text (ADR-0023, D9).
   - Run the PR review until APPROVE, then write `verdict.json` for HEAD (see "Review pipeline" below).
   - Push only the task branch, always in this form: `git push -u origin <branch>`.
   - Open the PR with `gh pr create`. Title: `P2.6: <task name>`. Body: goal, what changed, deviations, every new or changed snapshot, and the Definition of Done checklist from below. Nothing about the AI review goes into it, or anywhere on GitHub. When a later round changes what the body lists, update it with `gh pr edit <n> --body-file <file>`.
   - **Watch CI after every push** (the user's rule, approved 2026-11-03, ADR-0020). Straight after it, in a background Bash: `sleep 30; gh pr checks <n> --watch --fail-fast --interval 30 > target/ci.txt 2>&1; echo "exit $?"`. Its output stays in this repository, in the gitignored `target/`. Exit 0 is all green; anything else means act now (see "When CI fails" below). Never pipe the watch: `| tail` prints nothing until the last job ends, so a red job sits unseen behind a slow one.
   - If CI forces a change: fix it, `just check`, a new pr-reviewer round on the delta, a new `verdict.json` for the new HEAD, push, watch.
   - **Merge** when CI is green and `verdict.json`'s head, HEAD and the pushed head are one SHA, with exactly `gh pr merge <n> --squash --match-head-commit <sha> --delete-branch`, alone. The merge guard checks it first and refuses anything else. Then report one line: the PR's number and the merge commit.
   - The guard's `NEEDS_HUMAN` (a sensitive path), a review's NEEDS_HUMAN, a red CI you can't explain, or a review past its round limit stops the work for the user (see "Review pipeline").
8. **Stay in scope.** Keep changes to the task. Propose refactors outside it; don't do them silently.
9. **Finish the phase.** When a phase is done, check its **Definition of Done** point by point and report.
10. **Open questions.** Don't improvise around them. Each open question goes to its own read-only subagent, given the options neutrally. The subagent reads the whole project and returns one decision: the option, why, the rejected ones, the later phases it affects and how hard it is to change. The plan lists them under "Decided without you" (S1, S2, …), each marked "security" where it touches security or permissions. The plan-reviewer's APPROVE settles the ones not marked security; the ones marked security go to the user, one at a time (ADR-0023). Names follow the naming rules and are the AI's. Only these go to the user directly, one at a time:
    1. A decision whose chosen option would add, loosen or except a rule that binds the AI (this file's rules, lints, `#[expect]`, clippy and deny bans, coverage gates, CI, dependencies, manual tests, permissions).
    2. A genuine conflict between decisions the ADRs record as the user's, or between such a decision and the code, where every sensible option overturns one of them.
    3. Matters of taste (looks, texts friends read, UX).

    If at least one sensible option respects every rule and every recorded decision, the subagent decides, and the decision goes into "Decided without you". When unsure whether a question is (1)–(3), ask.

    Every subagent brief repeats "Stay in this repository". Library and crate sources are read on docs.rs or the crate's own repository on the web, never from the cargo registry, the toolchain folders or any other folder on this PC outside this repository.

    **Every subagent is read-only, and checked** (the user's rule, ADR-0022, U9):
    - Its brief says so: it creates, edits, moves or deletes no file, and runs no command that writes, installs, builds or changes git state.
    - Before it starts, run `node scripts/tree-fingerprint.mjs` and keep its output; after it finishes, run it again and compare. With several subagents in flight, compare after each. It hashes git's HEAD, refs, index with its flags, hooks and stash, tracked changes and every untracked file by content, and the ignored paths outside `target/` and `node_modules/` by path, size and mtime, never opening them; it can't see changes inside those two folders.
    - Any change you didn't make stops the work and goes to the user.
    - The two review agents, `plan-reviewer` and `pr-reviewer`, get only the plan's path and the branch name, or the branch name: their definitions in `.claude/agents/` carry the read-only, secret and "Stay in this repository" sentences instead of a brief (ADR-0023). The fingerprint check runs around them as around every subagent; run `git fetch origin` before the first fingerprint, since they read `origin/main`.

## Review pipeline (ADR-0023)
The work runs on its own from plan to merge, with every review record kept locally. Nothing about AI reviews appears on GitHub: no PR comments, labels, statuses or workflows.
- **The folder:** `target/review/<branch with "/" → "-">/` holds `plan.md`, `plan-round-N.md`, `pr-round-N.md`, `reply-N.md` and `verdict.json` (`{"head": "<sha>", "verdict": "APPROVE" | "REVISE" | "NEEDS_HUMAN", "round": N, "findings": [...]}`). Only the main session writes there: it saves each reviewer's answer verbatim as the round's file, and rewrites `verdict.json` after every PR round.
- **The reviewers:** `plan-reviewer` (input: the plan's path and the branch name) and `pr-reviewer` (input: the branch name), in `.claude/agents/`. They read the rules from `origin/main`, and return findings `F1…` (blocker, should-fix or nit, with the rule each breaks) and APPROVE, REVISE or NEEDS_HUMAN. APPROVE means no blocker or should-fix is open; nits never block.
- **The loop:** each blocker and should-fix is fixed (a failing regression test first for a bug) or answered "disputed: <reason>" in `reply-N.md` with evidence (a test, an ADR, a docs.rs link). Never obey a finding you can show is wrong; never dispute without evidence. Each round is a new reviewer, given the earlier rounds and replies as files; from round 2 on it blocks only on unresolved findings, disputes it rejects with evidence of its own, or what changed since the last reviewed SHA. A dispute still open after one exchange is NEEDS_HUMAN, with both positions. At most 2 plan rounds and 3 PR rounds, then stop for the user.
- **Size:** a group planned past about 1,500 changed lines (lockfiles, generated code and snapshots aside) is split at its plan review.
- **The merge guard** (`.claude/hooks/merge-guard.mjs`, a `PreToolUse` hook on Bash and PowerShell that fails closed) refuses a subagent's push, PR creation or merge, and refuses a merge unless it is step 7's exact command, run here on the task branch at `<sha>`, `verdict.json` approves `<sha>`, origin's branch and the PR's head are `<sha>`, PR `<n>` is this branch's open PR into `main`, and every check passed. A diff that touches a sensitive path gets `NEEDS_HUMAN: <paths>`: the list is the hook's `SENSITIVE` and ADR-0023's (the rules, CI, the toolchain, dependencies and lockfiles, the Tauri hardening, `deploy/`, the threat model, changed ADRs, the coverage gates, the route allowlist, and the security code). Never look for a way around a refusal.
- **Stopping for the user:** the last message starts with `NEEDS YOU:` and gives at most 5 lines: what's blocked, the options, your recommendation. Send the same stop as a push notification of under 200 characters; the full text stays in the session.
- **Success:** one line with the PR's number and the merge commit.

## Git & GitHub rules
- **Never:** push to `main`, force-push, rewrite history that's already pushed, or merge a PR in any form but step 7's (never `--admin` or `--auto`). The ruleset on `main` blocks most of this anyway; don't try to work around a rejection, the merge guard's included.
- **Allowed `gh` commands:** `gh pr create`, `gh pr view`, `gh pr diff`, `gh pr checks`, `gh pr edit` (title and body), `gh run list`, `gh run view` (including `--log-failed`), and `gh pr merge` in step 7's form alone. Nothing else: no `gh api`, `gh repo …`, `gh secret`, `gh auth`, `gh workflow`, `gh release`, `gh gist`, no PR comments, reviews or labels.
- `.claude/settings.json` lets exactly these run without asking: `git push -u origin p…`, the `gh` commands above, and the merge in step 7's form. Every other `gh` command that writes to GitHub asks the user, in auto mode too; its denies (pushes to `main`, force pushes, `--admin`, `--auto`, `gh api`, edits to `.claude/hooks/`) hold in every mode. Every rule has a twin for the PowerShell tool (ADR-0023). Don't look for ways around them.
- **When CI fails on your PR:** fix it on the same branch, push again and watch again (step 7); a new pr-reviewer round and a new verdict come before the merge.
  - `gh pr checks <n>` lists the red jobs. During the run, `gh run view --job <id>` shows a job's annotations, Playwright's errors among them, and its recipe can run locally.
  - `gh run view --log-failed` opens only when the whole run has ended, and a push cancels the running one (`cancel-in-progress`). If you still need a log, wait for the run's end (`gh pr checks <n> --watch`) before pushing.
  - While a job is red for a reason already explained, such as a `deps/` PR still waiting for its merge, watch without `--fail-fast`, or it stops at that job and hides the rest.
  - A job red in CI and green here isn't explained until its log says why. Don't guess: CI's runners are slower than this PC, and some of what they check is fetched fresh.
- **Stay in this repository.** Never read, change or run anything in other folders or other repositories.
- **Never ask the user to paste tokens, passwords, keys or real account data** into the chat. Steps that need real credentials (P1.8, P9 account linking) are done by the user.

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
    - Never name the user's hosting, in git, PRs or CI: their provider, their host, its address or DNS name, or that they use any particular cloud or DNS service. Steps that depend on a provider are written generically ("open 80 and 443 in your provider's firewall and the host's"); the runbook is a guide for any Linux host with Docker (the user's rule, ADR-0022, U2).
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
  - **Your shell tool is Git Bash.** Write POSIX shell commands. Claude Code's PowerShell tool may be there too; don't use it for work. The merge guard and every permission rule cover it as well (ADR-0023, U3).
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
- [ ] The work is on its task branch, pushed, with a PR open, and nothing was pushed to `main`.
