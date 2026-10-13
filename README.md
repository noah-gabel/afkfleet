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
**Phase 8 (the desktop app) is done.** The agent, `afkfleet-agent`, runs a few offline-mode bots against a local server (see [Quickstart](#quickstart)). The server, `afkfleet-server`, runs the secure HTTP API with invite-only accounts, mandatory 2FA and central authorization that every later phase builds on (see [Running the server](#running-the-server)). The Windows desktop app signs in to it, sets up 2FA, manages the user's own account and sessions, and lets Admins manage users and invites (see [the desktop app](#development)). What exists:
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
- **From Phase 5:** [`crates/fleet-agent`](crates/fleet-agent/) and its `afkfleet-agent` binary:
  - the config: `agent.toml` plus `AFKFLEET_AGENT__…` environment variables. A typo is an error with its key, and every problem is listed at once
  - logging: JSON lines or a pretty format, a filter that keeps azalea quiet and its auth logs safe, and a panic hook that logs through it
  - the wiring of the azalea adapter to the bot runtime. Each bot gets a new ID at every start, logged with its name. The agent exports the adapter's numbers as metrics and exits so Docker can restart it when too many host threads hang
  - a graceful shutdown on SIGTERM or SIGINT (Ctrl+C or Ctrl+Break on Windows): every bot leaves the server within the shutdown timeout
  - a heartbeat file, touched every 10 s while the fleet answers, and an `afkfleet-agent healthcheck` command that checks it without a shell or curl
  - a Docker image (distroless, nonroot, building natively on amd64 and arm64) with a `HEALTHCHECK`, and an agent in the dev compose stack: `just stack-up`
  - an end-to-end test of the compose stack (`just test-slow`): every bot comes Online, reconnects within its retry policy after the server restarts, and leaves the server cleanly when the agent stops, with only the expected warnings in the log and no error but a connection reset during the restart
  - a one-hour demo (`just demo-agent`): five bots and one server restart, with a summary, a verdict and the agent's measured memory and CPU

  [ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md) records Phase 5's decisions.

- **From Phase 6:** [`crates/fleet-server`](crates/fleet-server/) and its `afkfleet-server` binary, an HTTP service skeleton that is secure by default, with persistence, an error model and an audit trail, but no business endpoints yet, plus [`crates/fleet-startup`](crates/fleet-startup/) and [`crates/fleet-api-types`](crates/fleet-api-types/):
  - the binaries' shared start-up code (config loader, logging, panic hook, stop signals), moved out of the agent
  - the server's config: `server.toml` plus `AFKFLEET_SERVER__…` environment variables, with every problem listed at once
  - the server's clock and secure randomness behind ports, so tests control both, and a lint that refuses any other read of either
  - the SQLite database: one write connection and four read-only ones, a database file only the server's user can read, migrations at startup or through `afkfleet-server migrate`, and SQL checked at compile time against committed offline data (`just db-prepare`)
  - the first tables, `users` and `audit_log`, with strict types and CHECKs, and a store whose every write transaction commits together with its audit entry
  - the audit service: entries stamped by the clock, with details that can't hold secrets
  - the API's shared types, exported to TypeScript for the app (`just gen`), and described by an OpenAPI document that only dev mode serves, which a test keeps equal to the TypeScript
  - the error model: every failed request answers with one JSON envelope, a code with its status and a fixed message, and the request ID; an internal error's details go only to the log
  - the middleware stack: a request ID the server mints for every request, security headers on every response, one log line per request without headers or bodies, the request timeout and body limit from the config, and the error envelope for every error, a panic's included
  - the health endpoints, `GET /health/live` and `GET /health/ready` (a database ping): `200` with an empty body, revealing nothing about the server, and an `afkfleet-server healthcheck` command that asks the second without a shell or curl
  - a graceful shutdown on SIGTERM or SIGINT (Ctrl+C or Ctrl+Break on Windows): in-flight requests finish and the database closes within one deadline
  - an integration test that boots the server in-process on a fresh database, passes both health checks over a real connection and stops it cleanly

  [ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md) records Phase 6's decisions.

- **From Phase 7:** authentication and authorization in the server: invite-only accounts, mandatory 2FA, rotating tokens, one central `authorize()` and layered rate limits:
  - the password service: Argon2id at OWASP's baseline or above, with a zxcvbn strength check (score ≥ 3, the username and the app's words count against a password), behind a bounded queue
  - the vault's cipher (XChaCha20-Poly1305, bound to its record and purpose), with the keys read at startup from owner-only files
  - `afkfleet-server user create-owner`, which creates the one Owner
  - sessions: one per login, with opaque access tokens (15 minutes) and rotating refresh tokens (7 days idle, 30 days at most), stored only as SHA-256 hashes; reusing a replaced refresh token revokes the whole session and is recorded in the audit log
  - the `AuthUser` extractor, which looks up the session and its user on every request, so a revocation, a disabled user or a role change counts at once, and `require()`, which authorizes through the one `authorize()`
  - the route registry: every route is registered as public, authed, step-up or setup, with its rate-limit class, and a test calls every route that needs credentials without them expecting 401
  - rate limits per client IP (every request, health checks and unknown paths included), per route class (login, refresh, register) and per user, answered with 429 and `Retry-After`; behind a trusted proxy, the client is the right-most untrusted `X-Forwarded-For` address
  - the per-username lockout: 5 failed logins lock a name for a minute, doubling up to an hour, the same for names that belong to no user; the name itself is never stored
  - the server's housekeeping task, which keeps the limiters' memory bounded, purges expired lockout rows and logs one summary of refused requests a minute
  - login in two steps: the password, then a code from an authenticator app or a recovery code; the password step never hands out real tokens, a user without 2FA gets a setup token for enrolling first, and every refusal is the same answer
  - 2FA (TOTP), mandatory for every user: enrollment with a QR code, 10 single-use recovery codes, replay protection, and replacing an authenticator without ever standing without one
  - refreshing, logging out, and stepping up (the password again, for sensitive actions within 5 minutes)
  - registration only by invite: Admins and the Owner create single-use invites that expire, a friend registers with one and sets up 2FA at the first login, and an attempt with an invite that was already used is recorded
  - the user's own routes: the profile, with the wrong 2FA codes typed since the last login; a new password after stepping up, which ends every other session; the list of live sessions, and ending one
  - the admins' routes: the list of users; changing a role (only the Owner) or disabling a user, which ends every session of theirs; the list of invites and revoking one. Each of these writes is authorized again inside its own transaction
  - the threat model's analysis of the API's authentication and authorization, with the tests behind each mitigation and the accepted risks ([`docs/threat-model.md`](docs/threat-model.md), B1); a script checks that every test it cites exists

  [ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md) records Phase 7's decisions.
- **From Phase 8 (the desktop app):** group A's [`crates/fleet-client`](crates/fleet-client/), the typed client of the API that the app's Rust side will use:
  - one method per endpoint, sending and reading exactly the shared DTOs
  - the session kept out of every caller's hands: the access token in memory, the refresh token in a store (the OS keychain in the app), the login's step tokens in memory
  - the server's refresh rules: one refresh at a time and only on a 401, no grace window, and a session that ends on a refused refresh, a revoked session or a logout, which never races a refresh in flight; a refresh or a login that reached the server finishes saving even when its caller gives up
  - certificates always checked, against the system's roots or only a configured CA; plain HTTP only to this computer's loopback address
  - every request bounded (5 s to connect, 20 s in all), every answer capped at 1 MiB

  Group B's desktop app scaffold: [`apps/desktop`](apps/desktop/) (Tauri v2, its Rust side a workspace member, Windows only) around [`packages/ui`](packages/ui/) (React 19, Vite, strict TypeScript 7, Tailwind CSS 4 and shadcn/ui's theme). For now the window shows only the app's name; `just dev-app` runs it with hot reload and `just app-build` builds the exe. It's hardened before any feature arrives:
  - P8.3's strict CSP, served exactly as written
  - no global Tauri API and a frozen `Object.prototype`
  - a capability that grants nothing, so every future command needs an explicit grant
  - devtools only in debug builds
  - one instance at a time
  - a guard that keeps the window on the app itself

  `just tauri-check` fails if any of that changes.

  Group C's bridge, the app's Rust side: 20 commands, one per endpoint plus the server profile, which talk to the server only through fleet-client:
  - the server profile: its origin (https, or plain http only to this computer's loopback address) and optionally your own CA's certificates. It's checked against the server before it's saved, and a broken saved profile asks for the server again
  - the refresh token in the Windows Credential Manager, one entry per server. A switch to another server logs out first and never carries a session over
  - no token ever reaches the UI; every error has a code the UI handles

  And its TypeScript side: the UI's `ApiClient` (in `packages/ui`), which checks every input against the server's limits before sending it, the desktop app's `TauriApiClient` behind it, and a `FakeApiClient` for tests. One contract file lists every command's exact arguments, and both sides test against it.

  Group D's app shell, in `packages/ui`:
  - at start, the app checks the saved server and the stored session, then opens Home, the login or the server setup; a session it can't use offers a way out (try again, server settings, log out)
  - a banner while the server can't be reached (it checks every 10 s, or as long as a busy server asks) or presents a certificate the app refuses
  - navigation by role: Home and Server for everyone, the admin screens (users and invites, arriving with group E) only for Admins and the Owner
  - a session that ends while you use the app takes you to the login once, with one message; "Log out" in the header
  - an error screen instead of a blank window

  And its first screens:
  - server setup: the server's address and, optionally, your own CA's certificates (pasted or from a file), whose fingerprints the app shows for you to compare with the server's admin's; changing the server while logged in warns that it logs you out
  - logging in with a code from your authenticator app or a recovery code
  - the first login's 2FA setup: a QR code (or its key, to type in), then 10 recovery codes shown once, then the next code from your app
  - registering with an invite, which goes straight into the 2FA setup

  And group E's:
  - **the profile:**
    - your account, its password and its authenticator
    - a warning on Home and in the profile when wrong 2FA codes were entered since your previous login, or when 3 or fewer recovery codes are left
    - changing the password and replacing the authenticator, each after entering the password again
    - your sessions, each of which can be logged out, and "Log out all other sessions" for a lost PC
  - **the admin screens:**
    - users: make a Member an Admin or the reverse (the Owner), disable or enable (Admins for Members, the Owner for both)
    - invites: list, revoke, and a new one whose code is shown once, with "Copy invitation" (server, code, expiry and the CA's fingerprints) and "Copy code"

    Each row offers only what the server would allow, every change that ends something asks first, and every admin write needs the password within the last 5 minutes.
  - **Playwright tests** in Microsoft Edge: the app's UI, with its real API client, under the release CSP, against a fake of the server (`just e2e`). They cover logging in, the first 2FA setup, registering, and an Admin inviting someone who then registers.

  No password, 2FA secret, recovery code or invite code is kept anywhere but the screen that needs it, never in the UI's caches, and no token ever reaches the UI. The threat model's "Desktop app" section describes the app's boundaries, including a stolen or shared PC with a live session. A friend on another PC needs the server over HTTPS, which Phase 12 brings.

  [ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md) records Phase 8's decisions.

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
- Node 24 and pnpm (the version in `package.json`'s `packageManager`)
- The WebView2 Runtime, for the desktop app (built into Windows 11). The desktop app builds only on Windows; on Linux every workspace-wide recipe leaves it out.
- Docker Desktop (WSL 2 backend), for the Minecraft test server and slow tests
- Cargo tools:
  ```sh
  cargo +stable install --locked cargo-nextest@0.9.146 cargo-llvm-cov@0.9.1 cargo-deny@0.20.2 cargo-insta@1.49.0
  cargo install sqlx-cli --version 0.9.0 --locked --no-default-features --features sqlite
  ```
  sqlx-cli is pinned and SQLite-only, exactly as CI installs it; `just check` needs it for `db-check`.

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

### Running the server
```sh
afkfleet-server serve --config server.toml
afkfleet-server migrate --config server.toml       # migrate and exit; serve migrates at startup too
afkfleet-server healthcheck --config server.toml   # ask a running server whether it's ready
afkfleet-server user create-owner --config server.toml --username alice   # create the Owner
```
- **The config** is a TOML file ([`deploy/dev/server.toml`](deploy/dev/server.toml) is the development one; Plan.md's P6.2 note and Appendix A list the keys). `AFKFLEET_SERVER__…` environment variables override any key, e.g. `AFKFLEET_SERVER__LOG__FILTER=debug`. Unknown keys are errors, and every problem is listed at once.
  - `[database] path` is required and must be absolute.
  - `[http] bind` defaults to `127.0.0.1:8080` and can't use port 0.
  - `[http] request_timeout_secs` is 15 by default, at most 19: the desktop app gives up on a request after 20 s, so the server must answer first, or a refresh could go through after the app gave up and end the session at its next refresh.
  - `[http] trusted_proxies` lists the proxies whose `X-Forwarded-For` the server believes, as CIDR networks (`["172.30.0.0/24"]`, at most 32): none by default. Each network is written as its first address with a prefix length; a bare address or one with host bits set is refused. A prefix length of 0 (`0.0.0.0/0`, `::/0`) is refused too: trusting every address would let any client pick its own address through `X-Forwarded-For`, past every per-IP limit (login's included) and into the audit log. In the environment, it's a list: `AFKFLEET_SERVER__HTTP__TRUSTED_PROXIES='["172.30.0.0/24"]'`.
  - `[auth]`'s `argon2_memory_kib`, `argon2_iterations`, `argon2_parallelism` and `argon2_max_concurrent` default to 19456, 2, 1 and 4, and can't go below that baseline.
  - `[auth]`'s session lifetimes: `access_token_ttl_secs` 900 (60 to 3600), `refresh_token_idle_days` 7 (1 to 90) and `refresh_token_absolute_days` 30 (1 to 365), with idle at most absolute. A change applies to existing sessions too, at the next restart.
  - `[auth] lockout_threshold`: how many failed logins lock a username, 5 by default (3 to 20).
  - `[auth] invite_ttl_hours`: how long an invite lives, 72 by default (1 to 720). An invite's expiry is fixed when it's created, so a change doesn't move existing invites.
  - `[rate_limits]`: `per_ip_per_sec` 20 and `per_ip_burst` 40 for every request per client IP, `login_per_ip_per_min` 5 (login and its 2FA step together), `refresh_per_ip_per_min` 30, `register_per_ip_per_hour` 5, and `per_user_per_sec` 10 with `per_user_burst` 30 for every authed request per user. Each is at least 1; Plan.md's P7.10 note lists the upper bounds.
  - `[vault]` is required: `master_key_files = { 1 = "/run/secrets/vault_key_1" }` (1 to 16 key IDs, absolute paths) and `active_key_id = 1`. A key file can also come from the environment, as `AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1=/path`.
- **The vault's keys** are read by `serve` at startup, before anything else: each file holds 32 random bytes as standard base64 with padding (`openssl rand -base64 32`). On Unix it must be readable only by the server's user (`chmod 600`). A bad key file, two IDs with the same key or an active key without a file stops the server with exit 1, naming the file but never its content. `migrate`, `healthcheck` and `user create-owner` never read the keys. Back them up separately: the database holds only ciphertext.
- **The Owner** is created once, from the command line: `user create-owner` refuses at once when an Owner exists. Otherwise it asks for the password twice at the terminal (hidden; at most three tries), or reads it once from stdin with `--password-stdin`, for scripts. Prefer the prompt; in a script, pipe the password straight from a password manager (`<password-manager-command> | afkfleet-server user create-owner … --password-stdin`). If a password file is unavoidable, delete it right after. A password has 12 to 128 characters and must pass the strength check; a weak one is refused with hints. There's no password flag or variable. Exit codes, also listed by `afkfleet-server user create-owner --help`: 0 created, 1 refused or failed, 2 usage error.
- **Rate limits** answer `429` with the code `rate_limited` and `Retry-After` in whole seconds. An IPv6 client is counted by its /64. Without `trusted_proxies`, the client is always the connection's peer: behind a reverse proxy such as Caddy, list the proxy's network, or every client shares the proxy's one limit. A refused request is logged at `debug`; once a minute, one `info` line counts the refusals per limit.
- **Logging in** takes two steps: `POST /api/v1/auth/login {"username", "password"}` answers `{"status", "token", "expires_in"}`, an mfa token (`mfa_required`, 5 minutes) or, for a user without 2FA, a setup token (`mfa_setup_required`, 10 minutes), never real tokens. `POST /api/v1/auth/login/mfa {"mfa_token", "code"}` takes a code from the authenticator app or a recovery code and answers the session's tokens. An unknown username, a wrong password or code, a disabled user and a used-up or expired token all get the same 401 `invalid_credentials`; a username that isn't a valid one, or a password over 1024 characters, is 422. One mfa token takes 3 wrong codes. A stored hash made with older `argon2_*` parameters is re-hashed at the next login.
- **Sessions:** `POST /api/v1/auth/refresh {"refresh_token"}` answers a new pair and ends the old one (a refused refresh is 401 `unauthorized`: log in again), and `POST /api/v1/auth/logout` ends the request's session. `POST /api/v1/auth/step-up {"password"}` allows sensitive actions in this session for 5 minutes: changing the password, replacing 2FA, and every admin write (a role change, disabling or enabling a user, creating or revoking an invite). Without it they answer 403 `step_up_required`; a wrong password is 403 `forbidden` and counts toward the lockout.
- **The lockout:** after `lockout_threshold` failed logins for one username, it's locked for a minute, and each further failure doubles the lock up to an hour. A locked login answers 429 before the password is checked, and that doesn't count as a failure. A streak is forgotten 24 h after its last failure. Rows are keyed by an HMAC of the name under a key derived from the vault's, so the database never holds a typed name.
- **2FA** (TOTP) is mandatory for every user, the Owner included, and can't be switched off. A setup token (`Authorization: Bearer afk_st_…`, 10 minutes) works only for `POST /api/v1/me/mfa/enroll`, which answers a new secret as an otpauth URI, a QR code (an SVG `data:` URI) and base32 for typing in, and `POST /api/v1/me/mfa/confirm {"code"}`, which stores it and answers 10 recovery codes, shown once. Enroll may be repeated; a wrong code is 422 and the token stays usable. Authenticator apps get SHA-1, 6 digits and 30-second steps. To replace an authenticator, step up and call `POST /api/v1/me/mfa/reset`: it answers a setup token, the current authenticator keeps working until that token's confirm, and the confirm revokes every other session.
- **Invites and registration:** an Admin creates a Member invite and the Owner an Admin invite with `POST /api/v1/invites {"role"}` (after a step-up), which answers its code (`afk_iv_…`) this one time; a creator holds at most 20 pending invites. `GET /api/v1/invites` lists every invite in its state, and `DELETE /api/v1/invites/{id}` (after a step-up) revokes a pending one. The invitee calls `POST /api/v1/auth/register {"invite", "username", "password"}`, which answers 201 with the new user and no tokens: their first login answers `mfa_setup_required`. Every invite that can't be used gets the same 422 on `invite`; a taken name is 422 on `username`; a weak password is 422 with zxcvbn's hints, one field entry each. Disabling or demoting an invite's creator revokes their pending invites.
- **The user's own routes:** `GET /api/v1/me` answers the profile, the recovery codes left and the wrong 2FA codes typed since the previous login, without the typos of the newest login's own step. `POST /api/v1/me/password {"new_password"}` needs a step-up within 5 minutes; it ends every other session and this session's step-up. `GET /api/v1/me/sessions` lists the live sessions, and `DELETE /api/v1/me/sessions/{id}` ends another one (the current one ends through logout).
- **The admins' routes:** `GET /api/v1/users` lists the users, with whether each has 2FA and when they were last seen. `PATCH /api/v1/users/{id}` takes one change, after a step-up: `{"role"}` (only the Owner, between `member` and `admin`) or `{"disabled"}` (the Owner, or an Admin for a Member); either ends every session of the user.
- **Lists** answer pages, newest first: `?limit=` (1 to 100, 50 by default) and `?before=` with the `next` of the page before. Times are UTC with milliseconds (`2027-01-15T08:00:00.000Z`), and a value that isn't there is left out.
- **Request bodies** are JSON: a missing or other content type is 415, malformed JSON or an unknown, missing or mistyped field 400 without details, and a broken rule 422 with one entry per field; no answer or log line repeats what was sent.
- **Dev mode** (`dev_mode = true`) serves the OpenAPI document at `GET /api/openapi.json` and logs a warning at startup. Never run a production server in dev mode. Outside it, that path answers 404 like any unknown route.
- **The database** opens at startup: its folder must exist, a missing file is created (readable only by the server's user on Unix), and every pending migration runs. `migrate` does just that and exits 0, or 1 with the reason in the log, so you can migrate before starting a new version. On Unix, the server refuses a database, `-wal` or `-shm` file other users can read.
- **Logs** go to stdout, as JSON lines by default or as one readable line per event with `[log] format = "pretty"`. A config error is printed to stderr before logging starts.
- **Stopping:** SIGTERM or SIGINT (Ctrl+C or Ctrl+Break on Windows) starts a graceful shutdown with one deadline, `[http] request_timeout_secs` plus 5 s (20 s by default). The server stops accepting connections and lets in-flight requests finish within the request timeout, then closes the database within the rest. A second signal changes nothing. In Docker, the grace period must be longer than the deadline plus 1 s (more than 21 s by default), or Docker's kill replaces the exit code and the last line, "the server stopped".
- **Exit codes** of `serve`, also listed by `afkfleet-server serve --help`:

  | Code | Meaning |
  |---|---|
  | 0 | Stopped by a signal |
  | 1 | Startup error: the config, logging, the address or the database |
  | 2 | Usage error |
  | 4 | The server ended unasked, or didn't finish its shutdown by the deadline (the database didn't close) |
- **Healthcheck:** `afkfleet-server healthcheck --config server.toml` loads the same config, environment variables included, so give it the server's variables too. It asks `GET /health/ready` on `[http] bind`, or on loopback with the same port when the bind is `0.0.0.0` or `::`, and prints one line, such as `healthy: /health/ready on 127.0.0.1:8080 answered 200`. It needs no shell or curl. Exit codes:

  | Code | Meaning |
  |---|---|
  | 0 | Healthy: `/health/ready` answered 200 with an empty body |
  | 1 | Unhealthy: another answer, no answer within 3 s, or none at all. Also a usage or config error, which goes to stderr. Docker reserves 2, so it's never used |
- **In development,** `just dev-server` runs it with `deploy/dev/server.toml` (dev mode, pretty logs, `127.0.0.1:8080`), its database at `target/dev-server/afkfleet.db` and its vault key at `target/dev-server/vault-key-1`, which it creates on the first run and never overwrites. `just dev-server C:\path\to\afkfleet\other.db C:\path\to\afkfleet\other-key` uses other absolute paths. `just dev-owner alice` creates the dev database's Owner. To run the healthcheck against it, set `AFKFLEET_SERVER__DATABASE__PATH` and `AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1` to the same paths.

The server has no TLS of its own: Caddy fronts it (Phase 12).

## Development
```sh
pnpm install   # the frontend packages and tools (Biome, Vite, the Tauri CLI)
just check     # format, lints, docs, tests: run before every commit
just ci        # everything CI runs
just mc-up     # local offline-mode Minecraft 26.1 test server on 127.0.0.1:25565 (needs Docker)
just mc-down   # stop it (and the Docker agent) and delete its world
just dev-agent # run the agent with deploy/dev/agent.toml against that server
just dev-server # run the server with deploy/dev/server.toml (dev mode), its database and vault key in target/dev-server/
just dev-owner alice # create the dev database's Owner, the password typed at the terminal
just stack-up  # build the agent's image and run the server and the agent in Docker
just test-slow # pull the server image, build the agent's image, then the slow tests one at a time (needs Docker)
just demo-agent # the Phase 5 demo: the Docker agent's five bots for an hour with one server restart
just test-real-account # the real-account check: you only (see below)
just db-prepare # after changing a query or adding a migration: refresh crates/fleet-server/.sqlx/
just gen       # after changing an API type: refresh packages/ui/src/generated/
just dev-app   # run the desktop app with Vite's dev server (hot reload)
just app-build # build the desktop app's exe: target/release/afkfleet-desktop.exe
just ui-test   # the frontend's Vitest tests
just e2e       # the Playwright tests in the installed Edge (part of just check)
just cov-desktop # the desktop crate's coverage and its 90 % gate (Windows only)
just icons     # after changing apps/desktop/src-tauri/icons/icon.svg: render icon.ico
```
**The desktop app** (Phase 8 on): `apps/desktop` is the Tauri app, with its Rust side in `apps/desktop/src-tauri` (a workspace member, Windows only) and a thin frontend entry that renders the UI from `packages/ui`. `just dev-app` starts Vite's dev server on `localhost:1420` and opens the window; frontend changes reload at once, and Rust changes rebuild the app. `just app-build` builds the release exe with the frontend embedded (no installer yet); `just app-build --debug` builds a debug exe the same way, with devtools. The app's icon is rendered from `icons/icon.svg`, an original placeholder: change the SVG, then run `just icons`. The app's hardening (the CSP, the capabilities, the global API, devtools, tauri's features, the commands and their generated permissions in `src-tauri/permissions/autogenerated/`) is pinned by `just tauri-check`, part of `just check`; changing it means changing `scripts/tauri-hardening.mjs` too, with an ADR. A new command goes into build.rs's `COMMANDS`, `capabilities/main.json`, the script, and `src-tauri/tests/ipc-contract.json`. `pnpm install` applies `patches/`: sonner is patched so it adds no `<style>` element, which the release CSP blocks (every Vitest test checks that nothing does). It also refuses any npm release younger than 3 days (`minimumReleaseAge` in `pnpm-workspace.yaml`). `just e2e` builds the e2e entry (`apps/desktop/e2e`: the app's UI and its real `TauriApiClient`, over HTTP to a fake of the server that MSW runs inside Playwright), checks that neither shipped bundle holds any of it, and runs Playwright in the installed Microsoft Edge (nothing is downloaded) under the release CSP; every spec fails on a CSP violation, a console error or an injected style.
**Trying the app:** run `just dev-server` and, once per fresh database, `just dev-owner <name>`; then `just dev-app` (or a `just app-build --debug` exe) and enter `http://127.0.0.1:8080` as the server. The first login sets up 2FA with your phone's authenticator app. To try registering, create an invite in the Invites screen (it asks for your password first), log out, and register with its code.
**The desktop app's data:** the server profile is `%LOCALAPPDATA%\io.github.noah-gabel.afkfleet\server-profile.json` (an origin and, optionally, a CA's certificates; no secrets). The refresh token is a generic credential in the Windows Credential Manager named `<server origin>.io.github.noah-gabel.afkfleet`, e.g. `http://127.0.0.1:8080/.io.github.noah-gabel.afkfleet` for `just dev-server`. Logging out removes it. To reset the app by hand, delete that file and that credential (Control Panel, Credential Manager, Windows Credentials).
**The API's TypeScript types** (Phase 6 on) are generated from the Rust DTOs in `crates/fleet-api-types` with ts-rs and committed in `packages/ui/src/generated/`; never edit them by hand. After changing a DTO, run `just gen` and commit the result; `just check` (`gen-check`) and CI's `ts-types-fresh` job fail while they're stale. `just gen` exports into a fresh folder in `target/` first and deletes only files ts-rs generated: if the folder holds anything else, it stops and changes nothing.
**The server's database** (Phase 6 on): every build checks fleet-server's SQL against the offline query data committed in `crates/fleet-server/.sqlx/`, never against a live database (`SQLX_OFFLINE` in `.cargo/config.toml`). After changing a query or adding a migration, run `just db-prepare`, which rebuilds a throwaway database in `target/` from the migrations and regenerates that data, and commit it; `just check` and CI's `sqlx-offline` job fail while it's stale. A migration is never edited once it's on `main`, because sqlx checksums it: add a new one instead. `just migrations-check` and the `sqlx-offline` job enforce that.
The test server runs in offline mode, so it's for local development only. RCON is enabled with a random password and isn't published; run commands with `docker compose --file deploy/compose.dev.yaml exec minecraft rcon-cli <command>`.

The slow tests also run on GitHub weekly and on demand (the `Slow tests` workflow); it isn't a required check.

The compose e2e test runs the Docker stack under a project of its own, `afkfleet-e2e`, with [`deploy/compose.isolated.yaml`](deploy/compose.isolated.yaml) on top, which unpublishes the server's port, so it can run beside `just mc-up`. It judges the agent by [`deploy/dev/stack-checks.json`](deploy/dev/stack-checks.json): the retry windows, the warnings a server restart may cause, and the one error it may cause, a connection reset, allowed only during the restart. It needs the image `just test-slow` builds first; run on its own, it stops at once and says so. Its stack is removed when it ends, and a killed run's leftovers are removed by the next run, or by `docker compose -p afkfleet-e2e down -v`.

**The demo** (`just demo-agent`, or `just demo-agent 8` for a short check) runs the same stack as the project `afkfleet-demo` for an hour and restarts the server at half time. It then prints a summary: the agent's memory and CPU (peak, mean and 95th percentile, from `docker stats`, with the machine they were measured on), the warnings and errors by target, every state change, each bot's reconnect, and the shutdown report. It ends with PASS, or FAIL and every failed criterion: an error, a warning [`stack-checks.json`](deploy/dev/stack-checks.json) doesn't expect, a bot not back by its deadline, an agent restart, or an unclean shutdown. The one error that file allows is a connection reset while the server restarts, at most one per session closed there: vanilla's shutdown sometimes resets a socket before its kick, and azalea logs that at ERROR. The raw logs and the summary are saved under `target/demo-agent/`; only `summary.txt` belongs in a pull request, since the server's log holds container IPs. Run it after a Minecraft-version bump (ADR-0003).

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
