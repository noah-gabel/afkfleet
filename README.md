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
**Phase 5 (the standalone agent) is in progress.** Its first runnable product, `afkfleet-agent`, runs a few offline-mode bots against a local server (see [Quickstart](#quickstart)). What exists:
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
- **From Phase 3:**
  - [`crates/fleet-testkit`](crates/fleet-testkit/), scriptable fakes for the Minecraft ports. Tests of the bot runtime can script connects, emit server events, freeze a session's liveness or make it hang, and read what the bot did, all without a Minecraft server.
  - [`crates/fleet-mc`](crates/fleet-mc/), the azalea adapter:
    - the host pool, which gives every bot its own thread and abandons a thread that hangs
    - the event bridge, which turns azalea's events into sanitized session events and never drops anything but chat
    - the account adapter, which logs in with the server-issued Minecraft token (never a Microsoft one) and never logs it. It tells a rejected token, an account the session server restricts and a session-server outage apart, so an outage is retried instead of failing the bot
    - the connector, which starts each bot's azalea session on its own thread, bounds connecting with a timeout, and tears a session down in a fixed order so nothing of it is left behind
    - the bot actions: look, turn, jump, sneak, swing, use the held item, hold the use button (to draw and shoot a bow, for example), attack what's in reach, pick a hotbar slot, respawn and chat, each checked against a real server. On a server that requires signed chat, an online bot's chat waits until it can be signed, and fails instead of being silently dropped
  - slow tests against local Minecraft servers in containers (`just test-slow`, and weekly on GitHub): joining, chat, kicks, reconnects, every action, online-mode logins that must never log the token, a crash in one bot that leaves another running, and a clean-up test that proves every way a bot's session ends leaves no thread and no World behind
  - a real-account check that only you run (`just test-real-account`): your real token joins a local online-mode server, sends signed chat, and never reaches a log
  - the threat model's analysis of what a hostile Minecraft server can do, with the test behind each mitigation ([`docs/threat-model.md`](docs/threat-model.md), B4)

  [ADR-0011](docs/adr/0011-fleet-mc-and-fleet-testkit-conventions-and-phase-3-refinements.md) records Phase 3's decisions, and [ADR-0012](docs/adr/0012-azalea-advisory-and-license-exceptions.md) records azalea's accepted advisories and license exceptions.
- **From Phase 4:** [`crates/fleet-runtime`](crates/fleet-runtime/), the bot runtime. It's written against the Minecraft ports, so its tests run it on the test kit's fakes with paused time, and it never reads the clock or the OS's randomness itself. It contains:
  - one actor per bot that drives the bot state machine. It gets the session credentials, connects, runs the bot's mode and respawns it after a death. When a session ends it reconnects, waiting longer after each failure, and longer still while the circuit breaker is open
  - pausing when a human logs into the bot's account, and failing on a kick that won't heal, so a bot never fights the human or keeps hitting a server that banned it
  - the mode runner, which plays the bot's mode on its schedule
  - the outbound chat queue, which user chat and mode chat share: bounded, rate-limited per bot, and answering at once when it's full instead of waiting
  - the watchdog, which ends a session that stops ticking or stops getting packets, so the bot reconnects
  - the supervisor and its `Fleet` API. It runs every bot's actor and restarts one that crashes without skipping the backoff. A bot that crashes 6 times in 10 minutes fails. Every call is answered at once, if only with "busy", and a shutdown stops every bot within a timeout
  - four metrics: bots per state, reconnects, watchdog trips and actor restarts
  - a chaos test: 500 random runs of kicks, failed connects, hangs, crashes and API calls, which check that bots heal, never reconnect in a storm, never connect while a human plays, and that the API always answers

  [ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md) records Phase 4's decisions.
- **From Phase 5 so far:** [`crates/fleet-agent`](crates/fleet-agent/) and its `afkfleet-agent` binary:
  - the config: `agent.toml` plus `AFKFLEET_AGENT__…` environment variables. A typo is an error with its key, and every problem is listed at once
  - logging: JSON lines or a pretty format, a filter that keeps azalea quiet and its auth logs safe, and a panic hook that logs through it
  - the wiring of the azalea adapter to the bot runtime. Each bot gets a new ID at every start, logged with its name. The agent exports the adapter's numbers as metrics and exits so Docker can restart it when too many host threads hang
  - a graceful shutdown on SIGTERM or SIGINT (Ctrl+C or Ctrl+Break on Windows): every bot leaves the server within the shutdown timeout
  - a heartbeat file, touched every 10 s while the fleet answers, and an `afkfleet-agent healthcheck` command that checks it without a shell or curl
  - a Docker image (distroless, nonroot, building natively on amd64 and arm64) with a `HEALTHCHECK`, and an agent in the dev compose stack: `just stack-up`
  - an end-to-end test of the compose stack (`just test-slow`): every bot comes Online, reconnects within its retry policy after the server restarts, and leaves the server cleanly when the agent stops, with no error and only the expected warnings in the log

  [ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md) records Phase 5's decisions.

**Next:** the rest of Phase 5: a one-hour demo with five bots and one server restart.

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
Run three bots against a local offline-mode server (needs Docker):
```sh
just mc-up      # the local Minecraft 26.1 server on 127.0.0.1:25565
just dev-agent  # afkfleet-agent with deploy/dev/agent.toml
```
The bots join within a few seconds; the log shows each bot's ID with its name. Ctrl+C stops the agent: each bot leaves the server, and the last line reports how many stopped. `just mc-down` stops the server and deletes its world.

Or run the server and the agent both in Docker: `just stack-up` builds the agent's image and starts three other bots (see [Running the agent in Docker](#running-the-agent-in-docker)).

On Windows, Ctrl+C reaches every process in the console, `just`, PowerShell and `cargo` included, not only the agent. So the prompt can come back before the agent's last lines, and `just` may report the recipe as failed even when the agent stopped cleanly. The agent's own exit code is the one in its last line, "the agent stopped".

### Running the agent
```sh
afkfleet-agent run --config agent.toml
```
- **The config** is a TOML file ([`deploy/dev/agent.toml`](deploy/dev/agent.toml) is an example; Plan.md's Appendix A lists every key and its default). `AFKFLEET_AGENT__…` environment variables override any key, with `__` between the parts, e.g. `AFKFLEET_AGENT__LOG__FILTER=debug`. Unknown keys are errors.
- **Standalone mode** (`[standalone]`) is for development only: its bots use offline accounts, which only an offline-mode server accepts. Managed mode (`[control_plane]`) arrives in Phase 10.
- **Logs** go to stdout, as JSON lines by default or as one readable line per event with `[log] format = "pretty"`. A config error is printed to stderr before logging starts.
- **Stopping:** SIGTERM or SIGINT (Ctrl+C or Ctrl+Break on Windows) stops every bot within `[runtime] shutdown_timeout_secs` (10 s by default); a bot that takes longer is aborted. A second signal changes nothing: the shutdown is already running.
- **Exit codes**, also listed by `afkfleet-agent run --help`:

  | Code | Meaning |
  |---|---|
  | 0 | Stopped by a signal |
  | 1 | Startup error: the config, logging, or the fleet's setup |
  | 2 | Usage error |
  | 3 | Too many Minecraft host threads hung (the abandoned-thread limit); the agent shut its bots down first, so Docker can restart it |
  | 4 | The fleet's supervisor ended unasked, or didn't end when asked |
- **Heartbeat:** while its fleet answers, the agent touches its heartbeat file right after it starts and then every 10 s. The file is `[runtime] heartbeat_file`, an absolute path; by default it's `afkfleet-agent.alive` in the OS's temp directory, which is `/tmp` on Linux. A hung fleet stops the beats even while the process lives on. The agent never truncates or deletes the file.
- **Healthcheck:** `afkfleet-agent healthcheck --config agent.toml` loads the same config (environment variables included) and prints one line, such as `healthy: the heartbeat is 4 s old`. It works without a shell or curl, so it runs in the distroless image. Exit codes, also listed by `afkfleet-agent healthcheck --help`:

  | Code | Meaning |
  |---|---|
  | 0 | Healthy: the heartbeat file was touched less than 30 s ago |
  | 1 | Unhealthy: the file is missing, 30 s old or more, unreadable, or its time is more than 30 s in the future. Also a usage or config error, which goes to stderr as for `run`. Docker reserves 2, so it's never used |

### Running the agent in Docker
[`deploy/docker/agent.Dockerfile`](deploy/docker/agent.Dockerfile) builds the agent's image from the repository root: a builder with the pinned nightly and cargo-chef, and a distroless runtime (`gcr.io/distroless/cc-debian13:nonroot`) without a shell. The base images are pinned by multi-arch digest, so the same file builds natively on linux/amd64 and linux/arm64 (production runs on arm64). `.dockerignore` lets only the Rust workspace into the build.
```sh
just stack-up   # build afkfleet-agent:dev, start the server and the agent, wait until both are healthy
docker compose --file deploy/compose.dev.yaml logs -f agent   # the agent's JSON logs
just mc-down    # stop both and delete the server's world
```
- **The config** belongs at `/etc/afkfleet/agent.toml`. The image runs `run --config /etc/afkfleet/agent.toml`, and its `HEALTHCHECK` (every 10 s) runs `healthcheck --config /etc/afkfleet/agent.toml`. If you run the agent with another `--config` path, override the `HEALTHCHECK` with the same path, or it checks another config's heartbeat file. `AFKFLEET_AGENT__…` variables apply to both either way.
- **The compose agent** uses [`deploy/dev/agent.compose.toml`](deploy/dev/agent.compose.toml): AfkBot4–8 (four on `afk`, AfkBot6 on `farm`) against the compose server at `minecraft:25565`, with JSON logs. They aren't `just dev-agent`'s bots, so both agents can run at once. It starts once the server is healthy.
- **Stopping:** `docker stop` sends SIGTERM and kills the container once its grace period is over. The agent's worst-case shutdown is `shutdown_timeout_secs` plus 6 s (the reply timeout and the runtime's own shutdown), so the grace period must be longer: the compose file sets 20 s for the 10 s default. Otherwise Docker's kill replaces the exit code and the last line. The server gets 60 s: itzg's runner turns SIGTERM into `stop`, which saves the world, and Docker's default 10 s could kill it first.
- **Hardening** in the compose file: the nonroot user, a read-only root filesystem with a 1 MiB tmpfs at `/tmp` for the heartbeat file, no capabilities, `no-new-privileges`, 512 MiB of memory and 1 CPU, and a restart after a non-zero exit (`on-failure`).

## Development
```sh
pnpm install   # frontend tooling (Biome)
just check     # format, lints, docs, tests: run before every commit
just ci        # everything CI runs
just mc-up     # local offline-mode Minecraft 26.1 test server on 127.0.0.1:25565 (needs Docker)
just mc-down   # stop it (and the Docker agent) and delete its world
just dev-agent # run the agent with deploy/dev/agent.toml against that server
just stack-up  # build the agent's image and run the server and the agent in Docker
just test-slow # pull the server image, build the agent's image, then the slow tests one at a time (needs Docker)
just test-real-account # the real-account check: you only (see below)
```
The test server runs in offline mode, so it's for local development only. RCON is enabled with a random password and isn't published; run commands with `docker compose --file deploy/compose.dev.yaml exec minecraft rcon-cli <command>`.

The slow tests also run on GitHub weekly and on demand (the `Slow tests` workflow); it isn't a required check.

The compose e2e test runs the Docker stack under a project of its own, `afkfleet-e2e`, with [`deploy/compose.isolated.yaml`](deploy/compose.isolated.yaml) on top, which unpublishes the server's port, so it can run beside `just mc-up`. It judges the agent by [`deploy/dev/stack-checks.json`](deploy/dev/stack-checks.json): the retry windows and the warnings a server restart may cause. It needs the image `just test-slow` builds first; run on its own, it stops at once and says so. Its stack is removed when it ends, and a killed run's leftovers are removed by the next run, or by `docker compose -p afkfleet-e2e down -v`.

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
