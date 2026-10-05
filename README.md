# afkfleet

A self-hosted fleet of **Minecraft Java Edition AFK bots**, one per Microsoft account, managed from a Windows desktop app.

- **Bots** stay online by themselves. They reconnect, heal themselves, and back off when a human logs into the same account.
- **Modes** such as `afk` (anti-AFK-kick) and `farm` (look in one direction and attack or use an item on a schedule).
- **Chat**: bots receive and send chat, and stream it live to the app.
- **Central management**: an admin server with invite-only registration, roles and mandatory 2FA, plus a Tauri desktop app for you and your friends.

> **Use responsibly.** Only run the bots on servers whose rules allow AFK bots. afkfleet isn't affiliated with Mojang or Microsoft.

## Status
**Phase 0: Foundation & tooling** is in progress. The workspace, quality gates and CI are being set up; nothing is runnable yet.

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
- [rustup](https://rustup.rs). The pinned nightly toolchain in `rust-toolchain.toml` is installed automatically. Don't set a `rustup override` for this directory: it would take precedence over the pinned toolchain.
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
