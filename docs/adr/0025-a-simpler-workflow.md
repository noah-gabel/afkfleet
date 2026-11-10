# 0025. A simpler workflow

- **Status:** Accepted
- **Date:** 2026-11-09
- **Supersedes:** [ADR-0023](0023-local-autonomous-pipeline.md) (the local autonomous pipeline) and [ADR-0022](0022-deployment-and-phase-12-refinements.md)'s U9 and U11 (read-only subagents checked by the tree's fingerprint)
- **Related:** CLAUDE.md ("How we work", "A task"); `.claude/settings.json`; `scripts/settings.test.mjs`; `docs/threat-model.md`; ADR-0020 (U125, the CI watch); ADR-0022 (U2, the user's hosting stays private)

## Context
By 2026-11-09 the work ran through a heavy process. Each task got a written plan and up to two plan-review rounds, then up to three PR-review rounds by separate agents, their records under `target/review/`. Open questions went to subagents and then, by category, to the user, one at a time. A `PreToolUse` hook, the merge guard, decided when the AI could merge. Every subagent was checked by a fingerprint of the tree before and after. Group E (P12.8), a documentation tidy-up, spent three plan rounds and a PR round mostly on the guard's own shell scanner.

The user's decision (2026-11-09): less process and more autonomy, with the user reviewing and merging every pull request.

## Decision
**1. The AI decides.** It plans briefly in the chat and builds without waiting for approval. Every decision it would have asked about (design, dependencies, texts friends read, UX, and any exception a rule says needs "the user's approval") goes into the PR description under **Decided myself**, each with one line of why. Anything that weakens a security control (lints and `#[expect]`, the CSP, Tauri capabilities, rate limits, `deny.toml`, coverage gates) is listed under **Weakened** too.

**2. Only three things stop the AI and go to the user:** crossing a hard limit, an action that can't be undone, or a genuine conflict with a decision an ADR records as the user's.

**3. The hard limits,** stated in CLAUDE.md and, where Claude Code can enforce them, denied in `.claude/settings.json`:
- **Secrets:** never read, print, copy or list `secrets/`, any `.env*`, `*.key`, `*.pem` and vault key files, `target/dod-run/`, `target/dev-server/vault-key-*`, the desktop app's Credential Manager entries, or anything of the user's real host or real accounts.
- **GitHub:** never merge a PR, push to `main`, force-push, push a tag, create a release, or change the repository's settings, secrets, workflows' permissions or rulesets. Pushing its own branches and creating PRs is fine.
- **Real systems:** never connect to or act on the user's real server or real Microsoft accounts.
- **Privacy:** never name the user's hosting provider, host, address or DNS name in git, PRs or CI (ADR-0022, U2).
- **No paid services.**
- **Its own limits:** Claude Code's settings (.claude/settings*.json, ~/.claude/settings*.json) are the user's: propose a change under Decided myself in the PR description, and the user makes it.
- **Irreversible cleanups:** never delete `target/` as a whole (it holds `target/dod-run` and the dev vault key), and never remove Docker volumes, images or networks outside its own project names.

**4. Everything else is allowed without asking:** any build, test, Docker under the AI's own project names, package installs, and file changes anywhere in the repository, CLAUDE.md, `.claude/` and CI included, since the PR shows them. The technical rules stay as they were: Rust, frontend, testing, and security rules 1–9 and 11. Security rule 10 now reads "a weakening comes with an ADR and is listed under Weakened"; rule 12's secret files moved into the hard limits. The 7-day minimum age for a new dependency pin stays, as the AI's own rule; a security fix may be newer, noted in its PR.

**5. Subagents** are used when they help; each brief says read-only. There's no fingerprint check.

**6. One PR per task or group,** CI watched after every push as before (ADR-0020); the user reviews and merges every PR.

**Removed:**
- the merge guard: `.claude/hooks/merge-guard.mjs`, its `PreToolUse` entry in `.claude/settings.json` (both removed by the user, since the hooks folder was edit-denied to the AI), and `scripts/merge-guard.test.mjs`;
- the review agents `.claude/agents/plan-reviewer.md` and `pr-reviewer.md`, the review folder `target/review/` and its `verdict.json`;
- the tree fingerprint, `scripts/tree-fingerprint.mjs` and its test;
- CLAUDE.md's step 2's plan review, step 7's merge, step 10's question rules, the "Review pipeline" section and the `gh` allowlist.

**`.claude/settings.json`:** the allow list is broad (`Bash`, `PowerShell`, `Read`, `Edit`, `Write`, `Glob`, `Grep`, `NotebookEdit`, `WebFetch`, `WebSearch`); there's no ask list; the deny list holds exactly the hard limits Claude Code can enforce, each shell rule twinned for the PowerShell tool:
- secret paths, for reading and editing: `/secrets/**`, `**/.env*`, `**/*.key`, `**/*.pem`, `**/vault-key-*`, `/target/dod-run/**`;
- editing Claude Code's own settings, `/.claude/settings*.json` and `~/.claude/settings*.json` (an Edit rule covers Write too), so the AI can't loosen its own limits; and shell commands naming `secrets/`, `target/dod-run` or `vault-key-`, and `gh auth` (it prints the token);
- `gh pr merge`; pushes to `main`, force pushes, tag pushes (`--tags`, `--follow-tags`, `refs/tags/`, a `v…` refspec); `gh release`, `gh repo`, `gh secret`, `gh variable`, `gh ruleset`, `gh workflow enable` and `disable`; `gh api` with a method, a field or an input (its writes);
- the user's real systems: `just test-real-account`, `test-ms-account`, `test-managed-account`, every `dod-*`, `updater-sign`, and the `manual` nextest profile;
- `gh codespace` (paid);
- the irreversible cleanups `cargo clean`, `docker system prune` and `docker volume prune`.

*Added at the PR's review (the user's findings, 2026-11-10):* the settings rule, `*.pem`, and the cleanups.

`scripts/settings.test.mjs` pins that list whole, the broad allow, and that no hook remains; the threat model's row for a change merged without review cites it.

## Consequences
- **Easier:** a task goes from a short plan in the chat to a PR without stops; the user sees every decision, and every weakening, in one place, the PR.
- **Harder:** nothing reviews a change before the user does; the PR description carries the weight the review rounds did.
- **Accepted:** the deny rules read a command's text, so a command in another form (a script calling GitHub's API, a file read through another program) isn't seen; the hard limits hold by the AI keeping them. `main`'s ruleset still requires a pull request and CI's checks, and only the user merges.

## Alternatives considered
- **Keeping the pipeline, with fewer rounds:** the guard and the review agents were most of the cost, whatever the round limit.
- **Keeping the merge guard as a deny-only hook:** a deny rule on `gh pr merge` does the same, without a scanner to maintain.
- **Keeping the fingerprint check around subagents:** read-only briefs and the user's review of every PR cover what it caught, at a fraction of the cost.
