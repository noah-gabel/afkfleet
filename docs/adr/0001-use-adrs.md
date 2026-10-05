# 0001. Record architecture decisions

- **Status:** Accepted
- **Date:** 2026-10-05
- **Related:** Plan.md (living document), CLAUDE.md ("Record")

## Context
afkfleet is built phase by phase, largely with AI assistance, and reviewed by one maintainer. Decisions made in one phase constrain the later ones: the crate layout, the TLS provider, the token model. If the reasons for a decision live only in a chat or a commit message, they get lost, and later work either repeats the discussion or quietly undoes the decision.

## Decision
We record every design decision as an Architecture Decision Record in `docs/adr/`.
- **Format.** Files are named `NNNN-short-title.md` and numbered in order. They follow [`0000-template.md`](0000-template.md): Status, Date, Related, then Context, Decision, Consequences and Alternatives considered.
- **When to write one:**
  - any design decision
  - any deviation from `Plan.md` that changes the design
  - every new dependency or crate swap
  - every change to a security control: lints, CSP, Tauri capabilities, rate limits, cargo-deny rules, coverage gates (CLAUDE.md, security rule 10)
- **Status values:** `Proposed`, `Accepted`, `Superseded by NNNN`, `Deprecated`.
- **Accepted ADRs aren't rewritten.** A changed decision gets a new ADR that supersedes the old one, and the old one's status is updated. Fixing typos and adding links is fine.
- **Reserved numbers.** Numbers named in `Plan.md` are reserved for their task, even when later ADRs are written first. For example, ADR-0008 is written in P1.10.
- **Reading and writing.** The ADR is written in the same pull request as the change it describes. `Plan.md` refers to ADRs by number.

## Consequences
- Every decision has one place that explains it, and the PR diff shows the decision next to the code.
- Writing an ADR costs a little time for each decision. Small, obvious choices go into a `> Note (Pn.m)` in `Plan.md` instead.

## Alternatives considered
- **Decisions only in `Plan.md`.** The plan describes *what* to build. Mixing in the history of *why* would make it much harder to read.
- **Decisions in PR descriptions.** They're hard to find later, and they aren't versioned with the code.
