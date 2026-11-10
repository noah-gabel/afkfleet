# 0023. Local autonomous pipeline

- **Status:** Superseded by [ADR-0025](0025-a-simpler-workflow.md) (2026-11-09): the merge guard, the review agents and the review folder are gone; the user reviews and merges every pull request.
- **Date:** 2026-11-08
- **Related:** CLAUDE.md (steps 2–10, "Review pipeline", "Git & GitHub rules", the Definition of Done); ADR-0020 (U87, U125: CI watch); ADR-0022 (U9, U11: read-only, checked subagents; the 5,300-line groups); `.claude/hooks/merge-guard.mjs`, `.claude/agents/plan-reviewer.md`, `.claude/agents/pr-reviewer.md`, `.claude/settings.json`; `docs/threat-model.md`

## Context
Until now, every pull request waited for the user twice: once to approve its plan, and once to review and merge it. CLAUDE.md said "Only the user merges", allowed six `gh` commands, and every `gh` command and `git push` asked the user for approval, so each group waited for the user between its plan and its next group.

The user decided on 2026-11-08 to run the work autonomously, locally: the AI plans, has the plan reviewed, builds, has the change reviewed, and merges, with every review record kept in the gitignored `target/review/` and nothing about AI reviews on GitHub (no PR comments, labels, statuses or workflows). It changes rules that bind the AI (CLAUDE.md step 10, case 1), so the user approved it explicitly: "Only the user merges", the gh allowlist, "every gh command and git push asks for approval", and U9 for the review agents' inputs. The user approved the plan, then revised U1 during the build (below).

What Claude Code offers (v2.1.319 on this PC; its docs read on 2026-11-08):
- A `PreToolUse` hook sees each Bash and PowerShell call before it runs; exit 2 blocks it and shows stderr to the model. A hook that crashes or can't start lets the call through, unless it sets `"onFailure": "block"` (v2.1.295 on). Hooks from settings files also run inside subagents, whose input then carries `agent_id` and `agent_type`.
- Permission rules: deny beats ask beats allow, in every mode. Bash rules match the command's text after splitting compound commands, "and isn't a security boundary around the program" (the docs' words): `git -C . push` or `sh -c '…'` slip past a rule written for `git push`. `Write(path)` rules are accepted but never matched; `Edit(path)` rules cover every built-in tool that writes a file, and the file commands Claude Code recognizes in Bash.
- Auto mode's classifier blocks "merging a pull request no human has approved" by default, but an allow rule resolves a call before the classifier sees it.
- In auto mode a subagent runs in auto mode whatever its `permissionMode`; `omitClaudeMd: true` starts a subagent without the CLAUDE.md files (v2.1.271 on).

## Decision
### The review folder
`target/review/<branch with "/" → "-">/` holds `plan.md`, `plan-round-N.md`, `pr-round-N.md`, `reply-N.md` and `verdict.json` (`{"head": "<sha>", "verdict": "APPROVE" | "REVISE" | "NEEDS_HUMAN", "round": N, "findings": [...]}`). Only the main session writes there: it saves each reviewer's answer verbatim as the round's file and rewrites `verdict.json` after every PR round. The reviewers stay read-only; U9 and the fingerprint check stay as they are.

### The review agents
`.claude/agents/plan-reviewer.md` and `.claude/agents/pr-reviewer.md`: `model: opus`, `tools: Read, Grep, Glob, Bash`, `omitClaudeMd: true`. Their input is only the plan's path and the branch's name (plan-reviewer) or the branch's name (pr-reviewer), so their definitions carry Explore.md's read-only, secret and "Stay in this repository" rules instead of a brief: the change to U9 for the review agents' inputs. They read CLAUDE.md, the `.claude/rules` files, Plan.md and the ADRs from `origin/main` (`git show origin/main:<path>`), never the branch's own copy, and the earlier rounds and replies as files.
- **plan-reviewer** checks scope against Plan.md, the rules files the plan names, test-first and the 🔴 marks, the security rules, the planned size, and the open questions sorted into categories 1–3.
- **pr-reviewer** reads `git diff origin/main...HEAD` and the touched files, and checks the Definition of Done, the security and async rules, the error paths' tests, the docs, and every new or changed `.snap` file's content. It never builds or tests: `just check` already ran.
- Both return findings `F1…`: severity `blocker`, `should-fix` or `nit`, file and line, the problem, the CLAUDE.md rule or ADR it breaks, a suggested fix. **APPROVE** when no blocker or should-fix is open (nits never block); **REVISE** otherwise; **NEEDS_HUMAN** only for categories 1–3 or a dispute still open after one exchange.

### The correction loop
- Each blocker and should-fix is either fixed (a failing regression test first for a bug) or answered "disputed: <reason>" with evidence (a test, an ADR, a docs.rs link) in `reply-N.md`. A finding the main session can show is wrong is never obeyed; a dispute without evidence is never made.
- Each round is a new reviewer, given the earlier rounds and replies as files. From round 2 on it may block only on an unresolved finding, a dispute it rejects with evidence of its own, or what changed since the last reviewed SHA.
- A dispute still open after one exchange is NEEDS_HUMAN, with both positions summarized.
- Limits: 2 plan rounds and 3 PR rounds, then the work stops for the user.

### The flow, per task
Plan → plan review loop → the questions of categories 1–3 go to the user one at a time, and so do the "Decided without you" items marked security; the plan-reviewer's APPROVE settles the others → build (test first, `just check`, `just test-slow` and `just shots` where CLAUDE.md requires them) → PR review loop until APPROVE → `verdict.json` for HEAD → push, `gh pr create`, watch CI as before → if CI forces a change: fix, `just check`, a new pr-reviewer round on the delta, a new verdict for the new HEAD, push, watch → when CI is green and `verdict.json`'s head, HEAD and the pushed head are one SHA: `gh pr merge <n> --squash --match-head-commit <sha> --delete-branch` → one line: the PR's number and the merge commit.

A group planned past about **1,500 changed lines** (lockfiles, generated code and snapshots aside) is split at its plan review. This replaces the 5,300 lines of ADR-0020 and ADR-0022 from Phase 12's group E on.

When the work stops for the user, the last message starts with `NEEDS YOU:` and gives at most 5 lines: what's blocked, the options, the recommendation. The same stop also goes out as a push notification of under 200 characters (U1); the full text stays in the session.

### The merge guard
`.claude/hooks/merge-guard.mjs`, a `PreToolUse` hook on `Bash|PowerShell` in `.claude/settings.json` with `"onFailure": "block"`. It uses Node's own modules only. A subagent's `git push`, `gh pr create` or `gh pr merge` is refused (U2). Any other command holding a merge is refused, exit 2 with the reason on stderr, unless all of these hold, checked in this order (D11):
1. The command is exactly `gh pr merge <n> --squash --match-head-commit <sha>`, optionally with `--delete-branch`, flags in any order, alone (D1). `--admin` and `--auto` have their own refusals.
2. It runs inside this repository (D3), on a branch that isn't `main` or a detached HEAD, and `<sha>` is `git rev-parse HEAD`.
3. `git diff --name-status --no-renames -z origin/main...HEAD` touches no sensitive path (below); otherwise it prints `NEEDS_HUMAN: <paths>`.
4. `verdict.json` for this branch says APPROVE with head `<sha>`.
5. Origin's branch is at `<sha>`: `git ls-remote origin refs/heads/<branch>`, never the local `@{u}` copy (D4).
6. `gh pr view <n>`: open, into `main`, from this repository, on this branch at `<sha>` (D2).
7. `gh pr checks <n> --json name,bucket`: at least one check, every one `pass` (pending, skipping, cancelled and failed all refuse), and exit 0 (D7).

**Sensitive paths** (the hook's `SENSITIVE`, pinned whole by `scripts/merge-guard.test.mjs`; paths compare without case):
- Folders: `.github/`, `.claude/`, `.cargo/`, `.config/`, `deploy/`, `patches/`; `apps/desktop/src-tauri/capabilities/`, `apps/desktop/src-tauri/permissions/`; `apps/desktop/e2e/baselines/` (the `just shots` baselines).
- Files: `CLAUDE.md`, `deny.toml`, `biome.json`, `rust-toolchain.toml`, `justfile`, `pnpm-lock.yaml`, `pnpm-workspace.yaml`, `docs/threat-model.md`; `scripts/tree-fingerprint.mjs`, `scripts/threat-model-citations.mjs`, `scripts/merge-guard.test.mjs`, `scripts/coverage-gates.mjs`, `scripts/tauri-hardening.mjs`; `apps/desktop/src-tauri/tauri.conf.json`, `tauri.check.conf.json` and `build.rs` (D10: the check build's CSP, and the command list `just tauri-check` pins).
- Anywhere: every `clippy.toml` (the root's and four crates') and every `Cargo.lock` (the root's and three spikes').
- Every `Cargo.toml` whose dependency or lints sections differ between the merge base and HEAD (D6): `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, a target's and the workspace's, their subtables, dotted and inline forms; `[lints]` and `[workspace.lints]` with their subtables; and `[patch.*]` and `[replace]`, which swap a crate's source. A manifest added or deleted counts by its sections; one git can't show counts as changed.
- Every ADR (`docs/adr/NNNN-*.md`) modified, deleted or changed in type; a new ADR is fine.
- fleet-server's auth, vault and Microsoft accounts: every entry of the `auth`, `vault` and `msauth` coverage gates (`scripts/coverage-gates.mjs`; the auth gate's `crates/fleet-core/src/auth/` and `crates/fleet-api-types/src/secret.rs`, and the vault gate's `crates/fleet-startup/src/secret_file.rs`, included), and the auth code no gate measures: `crates/fleet-server/src/config/auth.rs`, `src/http/handlers/auth.rs`, `src/http/handlers/mfa.rs`. The rate-limit defaults, `crates/fleet-server/src/config/rate_limits.rs`.
- The control plane: fleet-server's `src/infra/pki/`, `src/app/enrollment.rs`, `src/app/agents/` and `src/grpc/`; fleet-agent's `src/managed/` and `src/enroll/`.
- `public_route`'s allowlist and the route-coverage test: both live in `crates/fleet-server/tests/routes.rs` (`outside_dev_mode_the_public_routes_are_exactly_the_allowlist`, the metrics port's one public route, every credentialed route's 401 and every step-up route's 403). `public_route` itself is in `crates/fleet-server/src/http/routes.rs`, an auth-gate entry. `crates/fleet-server/src/http/router.rs` registers every route and isn't listed, or every new endpoint would wait for the user: a public route added there fails the allowlist test unless `tests/routes.rs` changes too.

A test checks that the list covers every entry of the `auth`, `vault`, `msauth`, `control-plane` and `agent-control-plane` gates, so a module a later group adds to a gate can't be missed (D14).

### Permissions (`.claude/settings.json`)
Every Bash rule below has a PowerShell twin (`PowerShell(…)`, U3).
- **allow:** `git push -u origin p*`, `gh pr create *`, `gh pr view *`, `gh pr checks *`, `gh pr diff *`, `gh run list *`, `gh run view *`, `gh pr merge * --squash --match-head-commit *`, and `gh pr edit *` (U2).
- **ask** (they prompt even in auto mode; U2, D12): `gh pr edit` with `--add-label`, `--add-reviewer`, `--add-assignee`, `--add-project`, `--milestone` or `--base`; `gh pr comment`, `review`, `close`, `reopen`, `ready`, `lock`, `unlock`, `update-branch`, `revert`; `gh issue`, `label`, `release`, `repo`, `workflow`, `secret`, `variable`, `auth`, `gist`, `alias`, `extension`, `cache`, `project`; `gh run rerun`, `cancel`, `delete`.
- **deny:** the user's `git push * main*`, `git push --force*`, `git push -f*`, `gh pr merge * --admin*`, `gh pr merge * --auto*`, `gh api *` and `Edit(/.claude/hooks/**)` (which covers Write too); and U2's wider forms, wherever the flag or ref appears: `git push *:main*`, `git push *refs/heads/main*`, `git push *--force*`, `git push * -f*`, `git push *+*` (a forced refspec), `gh pr merge *--admin*`, `gh pr merge *--auto*`.
- The hook: `{"matcher": "Bash|PowerShell", "hooks": [{"type": "command", "command": "node", "args": ["${CLAUDE_PROJECT_DIR}/.claude/hooks/merge-guard.mjs"], "onFailure": "block"}]}`.

Every other rule stays: never push to `main`, never rewrite pushed history, stay in this repository.

### The user's decisions
- **U1 (revised during the build, 2026-11-08).** At the planning the user first let six files merge with a push notification instead of a stop; during the build they rejected that. Rule 10 stays as it is. `scripts/coverage-gates.mjs`, `scripts/tauri-hardening.mjs`, `crates/fleet-server/src/config/rate_limits.rs`, `crates/fleet-core/src/auth/`, `crates/fleet-api-types/src/secret.rs` and `crates/fleet-startup/src/secret_file.rs` are sensitive, and there is no notify mechanism. Instead, every `NEEDS YOU:` stop also sends a push notification (under 200 characters; it reaches the phone only while Remote Control is connected, and Claude Code skips it while the user is at the terminal). The user also added `.config/`, `deploy/`, `patches/`, `pnpm-workspace.yaml` and the file of `public_route`'s allowlist and the route-coverage test. The dependency fields of the `package.json` files stay off the list (`pnpm-lock.yaml` and `pnpm-workspace.yaml` are on it).
- **U2. Extra guards:** the hook refuses a subagent's push, PR creation and merge; the ask rules above, with `gh pr edit` allowed to run on its own; the wider push denies; and two Read denies, `target/dev-server/vault-key-*` and `target/dod-run/**`, in the user's untracked `.claude/settings.local.json` beside its four, not committed.
- **U3. PowerShell:** the PowerShell tool was available in the session, though CLAUDE.md called it off. The hook runs on it too, and every Bash rule has a PowerShell twin.
- **U4. README:** "This project is built with AI assistance (Claude Code). A human reviews and merges every pull request." became "This project is written by AI (Claude Code)."

### Points settled at the planning *(proposed by the AI, approved by the user at the plan review)*
Each makes the guard stricter than the user's text.
- **D1** The exact form, alone: anything else (`--subject`, `--body`, `-R`, `--merge`, a second command, `$(…)`, quotes, redirects, line breaks) is refused, naming what it found.
- **D2** PR `<n>` is this branch's open PR into `main`, from this repository, at `<sha>`.
- **D3** The merge runs inside this repository, so `gh` can't act on another one.
- **D4** Origin's head by `git ls-remote`; never `main` or a detached HEAD.
- **D5** The diff with renames off: a file moved out of `.github/` shows as its deletion; the status tells a new ADR from a changed one.
- **D6** Cargo.toml by its dependency, lints, patch and replace sections.
- **D7** "Every check passed" means at least one check, all `pass`, and exit 0.
- **D8** Fail closed: `onFailure: "block"`, and any error on a merge command refuses. If `node` can't start, every Bash and PowerShell call is blocked until it's fixed: visible, never silent. `.claude/settings.json` stays editable as the way out.
- **D9** No `if` filter: the hook runs on every Bash and PowerShell call and looks for `gh … pr … merge` in each command segment after removing quotes and escapes, without case. It over-matches rather than misses: `bash -c "gh pr merge 5"` is caught, and so is `echo gh pr merge`. Commit messages that name the command go through `git commit -F`.
- **D10** The exact list above, with the Tauri check build's config, `permissions/`, `build.rs`, the guard's own tests and the auth code no gate measures.
- **D11** The order of the checks, local before network; the first failure is the reason.
- **D12** `gh pr edit`'s flags that reach other people or retarget the PR still ask; labels too, since no AI review may appear on GitHub.
- **D13** The reviewers start without CLAUDE.md and read the rules from `origin/main`.
- **D14** Tests pin `.claude/settings.json`'s hook and its allow, ask and deny lists whole, pin the sensitive list whole, and check it against the coverage gates.
- **D15** One supply-chain row in `docs/threat-model.md`.

No open question went to a review agent: every one bound the AI, so it was the user's.

## Consequences
- **Easier:** a task goes from plan to `main` without waiting for a human, unless it touches what the user keeps for themselves. The guard refuses a merge for a reason it names, so a stop says what to do.
- **Harder:** a review that used to be the user's is now an AI's. Sensitive work still waits, and more of it than before is defined in one list that must be kept current: a new security module joins its coverage gate, and the gates' test then fails until it joins the list too.
- **Costs:** each Bash and PowerShell call starts Node once for the hook. A broken hook blocks every shell call until fixed (D8).
- **Revisit:** when Claude Code's hooks or permission rules change shape; when a new security module, workflow or config folder appears.

### Limits
- **Rules read text, not intent.** Permission rules match the command as written, and the docs call them no security boundary: `git -C . push`, `sh -c '…'`, an alias, an environment variable holding `gh`, or a Node or Python script that calls GitHub's API itself all slip past them. The hook over-matches merges, but it can't see a merge that never names `gh … pr … merge`. The guard stops mistakes and shortcuts, not a determined adversary.
- **The hook can be edited by a script.** `Edit(/.claude/hooks/**)` covers the built-in tools and the file commands Claude Code recognizes in Bash, not a script writing files. `.claude/settings.json` itself isn't denied: it is a protected path, so writing it prompts in the default modes and goes to the classifier in auto mode.
- **It replaces a built-in check.** The allow rule for `gh pr merge` resolves before auto mode's classifier, whose defaults block merging a PR no human approved. That is the point of this change; the hook takes the classifier's place.
- **It trusts files the main session writes.** `verdict.json` is written only by the main session, and the review agents are read-only by instruction; the fingerprint can't see inside `target/`. The main session rewrites `verdict.json` after every round, so a stray write by a reviewer doesn't survive to the merge.
- **The checks are read once.** They are read just before the merge; `--match-head-commit` pins the head between that read and the merge, and main's required status checks are the server-side backstop.
- **An AI reviews an AI.** The reviewers are separate instances with no memory of the build, but they share the builder's model family and can share its blind spots. A human sees only what the guard sends them.
- **The ruleset decides the merge.** If main's ruleset requires an approving review, `gh pr merge` fails, since the PRs are opened from the user's account, and the work stops with `NEEDS YOU:`.
- **Origin's view may be old.** The diff's base is the merge base with the local `origin/main`. A stale `origin/main` can only widen the diff, never narrow it.

## Alternatives considered
- **The user merges, as before:** the waiting this change ends.
- **GitHub-side review (a review bot, required reviews, labels or statuses):** the user wants nothing about AI reviews on GitHub.
- **Notify instead of stop for six files (U1's first version):** rejected by the user during the build; rule 10 stays.
- **A deny-list of merge flags instead of the exact form:** a flag nobody listed (`-R` to another repository, `--body`) would pass.
- **An `if` filter on the hook:** it is best-effort and wouldn't see `bash -c "gh pr merge …"`; running the hook on every shell call costs a Node start each.
- **Trusting `@{u}` for the pushed head:** a stale local copy could disagree with origin.

## After the merge, by the user
1. In main's ruleset, require ci.yml's 14 jobs as status checks: `fmt`, `clippy (ubuntu-latest)`, `clippy (windows-latest)`, `docs`, `test (ubuntu-latest)`, `test (windows-latest)`, `coverage`, `deny`, `stable-check`, `sqlx-offline`, `ts-types-fresh`, `tauri-build`, `frontend`, `e2e`. Not Screenshots or Stack: they run only on the PRs that touch their paths, and a required check that never runs blocks the merge for good. The guard still requires them whenever they run. Required approvals stay at 0.
2. Review and merge this PR (it touches `.claude/` and `CLAUDE.md`, so the guard would send it to the user anyway).
3. Restart Claude Code, so the hook, the rules and the agents load for certain.
