## Goal
<!-- Task ID and what it is for, e.g. "P2.6: bot state machine". -->

## What changed

## Deviations from Plan.md
<!-- Every deviation also has a `> Note (Pn.m): …` under its task in Plan.md. Write "None" if there are none. -->

## Decided myself
<!-- Every decision the AI made that it once would have asked about (design, dependencies, texts friends read, UX, an exception a rule names), each with one line of why (ADR-0025). Write "None" if there are none. -->

## Weakened
<!-- Every change that weakens a security control: lints and `#[expect]`, the CSP, Tauri capabilities, rate limits, `deny.toml`, coverage gates. Write "None" if there are none. -->

## Snapshots
<!-- Every new or changed insta snapshot, each one read before it was accepted. Write "None" if there are none. -->

## Definition of Done
- [ ] The tests were written first; they cover the happy path and every error path, and all pass.
- [ ] `just check` is green, with no new warnings and no unexplained `#[expect]`.
- [ ] Non-test code has no `unwrap`/`expect`/`panic`/indexing, and no secrets appear in logs or `Debug`.
- [ ] New endpoints are authorized, rate-limited, audited (if they change state), validated, snapshot-tested and in the route-coverage test.
- [ ] Public items are documented (`missing_docs` and `cargo doc` are clean).
- [ ] Generated artifacts are refreshed if their inputs changed (`just gen`, `just db-prepare`).
- [ ] New or changed snapshots were read before accepting, and are listed in the PR description.
- [ ] `docs/development.md` reflects the change (setup, commands, config, behavior), and `README.md` its intro, quickstart or status line if they changed.
- [ ] The `Plan.md` checkbox is ticked, with a note for any deviation, and an ADR exists for any decision.
- [ ] The PR description lists **Decided myself** and **Weakened** (or "none").
- [ ] The work is on its task branch, pushed, with a PR open, and nothing was pushed to `main`.
