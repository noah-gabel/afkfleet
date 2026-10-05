# afkfleet

A self-hosted fleet of **Minecraft Java Edition AFK bots**, one per Microsoft account, managed from a Windows desktop app.

- **Bots** stay online by themselves. They reconnect, heal themselves, and back off when a human logs into the same account.
- **Modes** such as `afk` (anti-AFK-kick) and `farm` (look in one direction and attack or use an item on a schedule).
- **Chat**: bots receive and send chat, and stream it live to the app.
- **Central management**: an admin server with invite-only registration, roles and mandatory 2FA, plus a Tauri desktop app for you and your friends.

> **Use responsibly.** Only run the bots on servers whose rules allow AFK bots. afkfleet isn't affiliated with Mojang or Microsoft.

## Disclosure

afkfleet is **a hobby project** I'm building for my friends with Claude Code. I use Claude to code faster and follow production standards to keep the risk of vulnerabilities low, but the code hasn't been professionally audited: use it at your own risk. A lot of the structure is deliberately overkill for a hobby project; it's also an experiment in how far strict tooling can keep AI-written code safe.

There's no support, and I don't take feature requests. Security reports are welcome, though: see [SECURITY.md](SECURITY.md).

## Status
**Phase 0 (Foundation & tooling) is complete.** It's under review in its pull request. Nothing is runnable yet. What exists:
- the Cargo workspace with all lints
- the pinned toolchain
- the quality gates: formatting, clippy, docs, tests, coverage gates, cargo-deny, Biome
- CI and the architecture decision records

**Next:** Phase 1, a time-boxed spike that de-risks the Minecraft layer.

**Minecraft version:** Java Edition **26.1** (azalea 0.16.0, see [ADR-0003](docs/adr/0003-azalea-and-pinned-nightly.md)). Servers on newer versions need ViaVersion/ViaBackwards.

The roadmap and the progress of every phase are in [`Plan.md`](Plan.md).

## Architecture
| Component | Technology | Role |
|---|---|---|
| Agent | Rust, [`azalea`](https://github.com/azalea-rs/azalea) | Runs the bots: one supervised actor per bot |
| Server | Rust, axum, SQLite | Users, roles, encrypted credentials, desired bot state; talks to agents over gRPC with mTLS |
| Desktop app | Tauri v2, React, TypeScript | The UI for everyone with an account |

Further reading:
- [`Plan.md`](Plan.md): roadmap, architecture, security model, fault tolerance, appendices
- [`docs/adr/`](docs/adr/): architecture decision records
- [`docs/threat-model.md`](docs/threat-model.md): assets, attackers and mitigations
- `docs/runbook.md`: operations (written in Phase 12)

## Prerequisites (development, Windows 11)
- Git, [PowerShell 7](https://learn.microsoft.com/powershell/scripting/install/installing-powershell-on-windows) (`pwsh`) and [`just`](https://github.com/casey/just)
- [rustup](https://rustup.rs). Run `rustup install` once in the repository to install the nightly pinned in `rust-toolchain.toml`. Don't set a `rustup override` for this directory: it would take precedence over the pinned toolchain.
- MSVC Build Tools ("Desktop development with C++")
- [NASM](https://www.nasm.us) on `PATH`, needed to build `aws-lc-rs` (the TLS crypto provider). CMake isn't needed.
- Node 24 and pnpm
- Docker Desktop (WSL 2 backend), for the Minecraft test server and slow tests
- Cargo tools:
  ```sh
  cargo +stable install --locked cargo-nextest@0.9.146 cargo-llvm-cov@0.9.1 cargo-deny@0.20.2 cargo-insta@1.49.0
  ```

## Quickstart
There's nothing to run yet. The first runnable product is the standalone agent in Phase 5.

## Development
```sh
pnpm install   # frontend tooling (Biome)
just check     # format, lints, docs, tests: run before every commit
just ci        # everything CI runs
```
`just --list` shows every recipe; [`CLAUDE.md`](CLAUDE.md) describes them and the project's working rules.

Every change goes through a pull request; nobody commits to `main`.

This project is built with AI assistance (Claude Code). A human reviews and merges every pull request.

## License
Copyright (C) 2026 the afkfleet contributors.

afkfleet is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version. See [`LICENSE`](LICENSE).
