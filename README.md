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
**Phase 3 (azalea adapter and test kit) is nearly done:** only P3.9, holding the use button, is left. The product itself isn't runnable yet. What exists:
- **From Phase 0:**
  - the Cargo workspace with all lints and the pinned toolchain
  - the quality gates: formatting, clippy, docs, tests, coverage gates, cargo-deny, Biome
  - CI and the architecture decision records
- **From Phase 1:**
  - a local Minecraft test server (`just mc-up`)
  - [ADR-0008](docs/adr/0008-azalea-integration.md), which records how afkfleet uses azalea: one isolated azalea App and thread per bot, the failure catalogue, the watchdog design, and workarounds for azalea bugs
  - the spike that produced the evidence, archived in [`spikes/azalea/`](spikes/azalea/)
- **From Phase 2:** [`crates/fleet-core`](crates/fleet-core/), the pure domain core. It has no IO and no tokio, and it's tested with seeded randomness and fixed times. It contains:
  - typed IDs, plus validated server addresses, names and chat messages
  - the sanitizer for chat received from servers
  - the disconnect classifier: retry, give up, or pause because a human logged in
  - retry backoff and the circuit breaker
  - the bot state machine
  - modes, the `afk` and `farm` presets, and their scheduler
  - authorization: roles, account grants and the command allowlist
  - the Minecraft ports that the azalea adapter will implement

  [ADR-0010](docs/adr/0010-fleet-core-conventions-and-phase-2-refinements.md) records its conventions and every decision made along the way.
- **From Phase 3 so far:**
  - [`crates/fleet-testkit`](crates/fleet-testkit/), scriptable fakes for the Minecraft ports. Tests of the bot runtime can script connects, emit server events, freeze a session's liveness or make it hang, and read what the bot did, all without a Minecraft server.
  - the first parts of [`crates/fleet-mc`](crates/fleet-mc/), the azalea adapter:
    - the host pool, which gives every bot its own thread and abandons a thread that hangs
    - the event bridge, which turns azalea's events into sanitized session events and never drops anything but chat
    - the account adapter, which logs in with the server-issued Minecraft token (never a Microsoft one) and never logs it. It tells a rejected token, an account the session server restricts and a session-server outage apart, so an outage is retried instead of failing the bot
    - the connector, which starts each bot's azalea session on its own thread, bounds connecting with a timeout, and tears a session down in a fixed order so nothing of it is left behind
    - the bot actions: look, turn, jump, sneak, swing, use the held item, attack what's in reach, pick a hotbar slot, respawn and chat, each checked against a real server
  - slow tests against local Minecraft servers in containers (`just test-slow`, and weekly on GitHub): joining, chat, kicks, reconnects, every action, online-mode logins that must never log the token, a crash in one bot that leaves another running, and a clean-up test that proves every way a bot's session ends leaves no thread and no World behind
  - a real-account check that only you run (`just test-real-account`): your real token joins a local online-mode server, sends signed chat, and never reaches a log
  - the threat model's analysis of what a hostile Minecraft server can do, with the test behind each mitigation ([`docs/threat-model.md`](docs/threat-model.md), B4)

  [ADR-0011](docs/adr/0011-fleet-mc-and-fleet-testkit-conventions-and-phase-3-refinements.md) records Phase 3's decisions, and [ADR-0012](docs/adr/0012-azalea-advisory-and-license-exceptions.md) records azalea's accepted advisories and license exceptions.

**Next:** P3.9, a `HoldUse` action that holds the use button down (for example to draw a bow), the last task of Phase 3. Then Phase 4, the bot runtime.

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
just mc-up     # local offline-mode Minecraft 26.1 test server on 127.0.0.1:25565 (needs Docker)
just mc-down   # stop it and delete its world
just test-slow # slow tests against local Minecraft containers, one at a time (needs Docker)
just test-real-account # the real-account check: you only (see below)
```
The test server runs in offline mode, so it's for local development only. RCON is enabled with a random password and isn't published; run commands with `docker compose --file deploy/compose.dev.yaml exec minecraft rcon-cli <command>`.

The slow tests also run on GitHub weekly and on demand (the `Slow tests` workflow); it isn't a required check.

**The real-account check** shows that a real Minecraft token joins a local online-mode server and sends signed chat, and that the token never reaches a log. It needs your real credentials, so only you run it; the AI never does.
1. Right before running, create `secrets/p1.8-account.txt` (gitignored) with the archived spike's `fetch-token`: `cd spikes/azalea`, then `cargo run -- fetch-token`, and sign in with the code it shows. The file holds no expiry, and a token lasts about a day. If the session server rejects it, fetch a new one.
2. Run `just test-real-account` (needs Docker and internet). It never prints the file's contents.
3. Report only the outcome, and delete the file afterwards.

When the test ends, its container is removed together with its volumes, which hold the account name in the server's usercache and logs. A killed run can leave the container behind; check `docker ps -a`.
`just --list` shows every recipe; [`CLAUDE.md`](CLAUDE.md) describes them and the project's working rules.

Every change goes through a pull request; nobody commits to `main`.

This project is built with AI assistance (Claude Code). A human reviews and merges every pull request.

## License
Copyright (C) 2026 the afkfleet contributors.

afkfleet is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version. See [`LICENSE`](LICENSE).
