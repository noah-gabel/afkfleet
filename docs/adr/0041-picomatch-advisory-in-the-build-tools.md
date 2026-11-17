# 0041. `picomatch`'s advisory in the build tools: ignored by its one ID, and how `pnpm audit` ignores

- **Status:** Accepted. Decided under ADR-0025
- **Date:** 2026-11-17
- **Related:** Plan.md §5 (Bundler & dev server: `vite`; tests: `vitest`); ADR-0017 (the frontend's dependencies, `minimumReleaseAge`); ADR-0012, ADR-0021, ADR-0024, ADR-0038 (the other ignored advisories); ADR-0035 (a dependency with an advisory is handled first); `pnpm-workspace.yaml`

## Context
On 2026-11-17 `just ui-audit` (`pnpm audit`) failed on `main`, on the push that merged Phase 15's group B. The same lockfile had passed the same check on that pull request's last run less than an hour earlier: nothing of ours changed, an advisory was published.

**What it is, read on the day:**
- **GHSA-fc5q-39xc-vjpf** (CVE-2026-112366), "picomatch vulnerable to ReDoS through single-branch repeated extglobs", rated high.
- **Affected: every release,** `<= 4.0.7`, and 4.0.7 is the newest on the registry. **Patched versions: none.**
- **The fault:** a glob *pattern* of a certain shape (a repeated extglob with one branch, such as `+(*a)`) is turned into a regular expression that backtracks without end on a short crafted text. Whoever can hand picomatch such a pattern can keep the process busy.

**Where picomatch is here:**
- **In the build and test tools only:** `vite` 8.3.4, `vitest` 5.0.3, `tinyglobby` 0.2.17 and `fdir` 6.5.0, all through `devDependencies`. `pnpm audit --prod` is clean: nothing the app ships contains it. It never runs in the desktop app, the server or the agent.
- **The patterns it reads are our own:** the configs of Vite, Vitest and Playwright in this repository and those tools' built-in defaults. None of this repository's configs holds an extglob. No pattern comes from a user, the network or a Minecraft server.
- **Vite's dev server** matches request paths against its fixed deny patterns. It listens on the loopback address only (no `host` is set), during `just dev-app`.

## Decision

### N1. The advisory is ignored, by its one ID
`pnpm-workspace.yaml` gains pnpm 12's `audit.ignore` with `GHSA-fc5q-39xc-vjpf`, its reason and its removal trigger in a comment above it.

**Why that's acceptable now:**
- **To use the fault, someone must supply a pattern.** Here that means changing a config file in this repository or a tool's own source, and whoever can do that already runs code in the build.
- **The worst outcome is a tool that hangs:** a build, a test run or a dev server on a developer's PC or a CI runner, which a job's timeout ends. No data is read or changed, and nothing reaches a user's PC or the server.
- **No fixed release can be taken.** An override to an older picomatch gains nothing: every release is affected.

### N2. Removal trigger
**Once a fixed picomatch is released and locked.** Then the entry goes, in the pull request that takes the fix (an override, or the tools' own bump). The fix may be younger than `minimumReleaseAge`'s three days only through `minimumReleaseAgeExclude` (ADR-0017).

**pnpm doesn't say when an ignored advisory matches nothing,** as `cargo deny` does (`unused-ignored-advisory`, ADR-0024). So the entry is looked at by hand, at every bump of vite, vitest or pnpm; `pnpm audit` names it in every run until then (N3).

### N3. How `pnpm audit` ignores, from now on
- **Only `audit.ignore` in `pnpm-workspace.yaml`,** one GHSA ID a line, each under a comment with its reason and its removal trigger, and each with a note in an ADR and under **Weakened** in its pull request, as for `deny.toml`.
- **Never a broader switch:** no `--ignore-unfixable` (it would pass over every advisory without a fix, unseen), no `--audit-level` or `audit.level` (a whole severity), no `--prod` in `just ui-audit` (the build tools run on our PCs and CI's runners and stay audited).
- With an ignored advisory, `pnpm audit` still names it in every run: "1 ignored: 1 high".

## Alternatives considered
- **Waiting for a fix with `main` red.** A red `main` hides every other advisory, and every pull request's `frontend` job fails meanwhile.
- **Auditing only what ships (`--prod`).** It would have passed, and would stop checking the tools that build the app and run on our machines.
- **Replacing the tools.** picomatch is inside Vite and Vitest themselves, and they're Plan.md §5's choice.

## Consequences
- `just ui-audit` is green again and prints the ignored advisory in every run.
- The first ignored npm advisory; the Rust side has had them since ADR-0012.
- Checked by a trial: with another ID in the list, the audit fails again on this advisory.
