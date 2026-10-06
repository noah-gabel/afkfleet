# afkfleet: Implementation Plan

> **Living document.** Work through it phase by phase with an AI assistant. The working rules for the assistant are in [`CLAUDE.md`](CLAUDE.md).
> - Mark a task done by ticking its box (`- [x]`).
> - Note deviations inline as `> Note (P4.3): …`.
> - Record design decisions as ADRs in `docs/adr/`.
> - Every task is built on its own branch (`p<phase>/<task-id>-<slug>`) and merged by the user through a pull request. Nobody commits to `main`!
>
> Created 2026-10-04. Crate versions are a **baseline** from that date. Phase 0 pins exact versions after re-checking each one on docs.rs.
>
> **Revised 2026-10-05**, before any code was written:
> - 2FA is mandatory for every user. The Owner/Admin-only wording and `require_mfa_for_admins` were removed, and a single-use setup token now covers first enrollment (§7.2).
> - The project is licensed under GPL-3.0-or-later.
> - No Dependabot or Renovate. Vulnerable dependencies are caught by `cargo deny` and `pnpm audit` in CI, including a weekly scheduled run; updates are done by hand.
> - No personal data in the repo (CLAUDE.md, security rule 11).
> - The `justfile` uses PowerShell 7 (`pwsh`), with recipes that also run in `sh`.
> - `missing_docs` lint and a `cargo doc` check were added.
> - The repo already exists, so P0.1 starts from a clone instead of `git init`.
> - Testing notes: red-phase stubs, snapshot review in the PR, clocks for `governor`/`moka`, `PROPTEST_CASES`, a `fleet-runtime` coverage gate.
> - The branch/PR workflow and `gh` rules are spelled out in `CLAUDE.md`.
>
> **Already done by the user (2026-10-05):**
> - Dev machine set up: Git, GitHub CLI, PowerShell 7, `just`, MSVC Build Tools, NASM, rustup, Node 24 + pnpm, Docker Desktop, Claude Code.
> - Public GitHub repo created with `README.md` and a GPL-3.0 `LICENSE`.
> - `main` protected by a ruleset; Actions and security settings configured.

## Contents
1. [Vision & scope](#1-vision--scope)
2. [Architecture overview](#2-architecture-overview)
3. [Architectural principles](#3-architectural-principles)
4. [Repository layout](#4-repository-layout)
5. [Crate & tool registry](#5-crate--tool-registry)
6. [Fault-tolerance model](#6-fault-tolerance-model)
7. [Security model](#7-security-model)
8. [Testing strategy](#8-testing-strategy)
9. [Phases](#9-phases)
10. [Future extension ideas](#10-future-extension-ideas)
11. [Appendix: config, API, protocol, schema, state machine](#11-appendix)

---

## 1. Vision & scope

**Goal.** A self-hosted fleet of Minecraft **Java Edition** AFK bots, one per Microsoft account. Each bot should:
- **stay online by itself.** It reconnects, heals itself, and never fights a human who logs into the same account.
- **run predefined modes**, such as `afk` (anti-AFK-kick), `farm` (look in a fixed direction and attack or use an item on a schedule), or custom action schedules.
- **use chat.** It receives messages, sends them, and streams them live to the app.
- **be managed centrally** through an admin server and a Windows desktop app built with Tauri. Friends can use the app with their own login and role.

**Non-goals.** The bots won't do any of the following:
- mining, building or pathfinding
- inventories or container GUIs
- combat logic beyond "attack what's in front of me"
- Bedrock Edition

There is also no public sign-up.

**Assumptions & constraints**
- **azalea and nightly Rust.** The Minecraft protocol comes from [`azalea`](https://github.com/azalea-rs/azalea), which needs **nightly Rust**.
- **One Minecraft version per azalea release.** Target servers must run that version, or ViaVersion. Upgrading Minecraft means upgrading azalea, the pinned nightly and the test-server `VERSION` in **one** change.
- **Platforms.** Production runs on a Linux host with Docker Compose. Development happens on Windows 11.
- **Scale.** v1 targets up to ~50 bots per agent and a handful of human users. Adding more agents or hosts must not need a redesign.
- **Server rules.** Only use the bots on servers whose rules allow AFK bots.

## 2. Architecture overview

```
 ┌────────────────────────────┐          ┌───────────────────── Linux host (Docker Compose) ─────────────────────┐
 │ Windows: Tauri desktop app │          │                                                                        │
 │  React UI  (packages/ui)   │  HTTPS   │  ┌───────────┐  HTTP   ┌─────────────────────────────────────┐        │
 │     │ invoke()             ├─────────▶│  │  Caddy    ├────────▶│ afkfleet-server                      │        │
 │  Rust core (src-tauri)     │  + WSS   │  │  :443 TLS │         │  axum REST + WebSocket       :8080   │        │
 │   fleet-client, keychain   │          │  └───────────┘         │  SQLite (sqlx) · vault · auth/RBAC   │        │
 └────────────────────────────┘          │                        │  tonic gRPC, mTLS            :7443   │        │
                                         │                        └──────────────────▲──────────────────┘        │
                                         │                       gRPC bidi stream    │  mTLS (agent dials out)    │
                                         │                        ┌──────────────────┴──────────────────┐        │
                                         │                        │ afkfleet-agent   (1..n)              │        │
                                         │                        │  Supervisor ── BotActor × N          │        │
                                         │                        │   (one tokio task per bot)           │        │
                                         │                        │  MC host threads (LocalSet)          │        │
                                         │                        │   └── azalea clients                 │        │
                                         │                        └──────────────────┬──────────────────┘        │
                                         └───────────────────────────────────────────┼───────────────────────────┘
                                                                                     ▼
                                                                         Minecraft Java servers
```

| Component | Owns | Never does |
|---|---|---|
| `afkfleet-server` | Users, roles, sessions; encrypted MC credentials; the **desired state** of every bot; modes; chat history; audit log; agent registry & scheduling | Speak the Minecraft protocol |
| `afkfleet-agent` | Running bots: connection lifecycle, modes, watchdog, reporting **actual state** & events | Persist long-lived secrets, make authorization decisions |
| Desktop app | Presentation; the local session (refresh token kept in the OS keychain) | Enforce business rules (the server decides) |

**Data flows**
- **Desired state:** user → REST → server DB → `ReconcileFull` / `AssignBot` / `UpdateBotSpec` → agent → bot actor.
- **Actual state & events:** bot actor → agent → gRPC stream → server (stores chat and status, then broadcasts) → WebSocket → app.
- **Credentials:** the Microsoft refresh token stays encrypted on the server. When a bot needs to join, its agent gets a **short-lived Minecraft session token** for that bot only.

## 3. Architectural principles
1. **Hexagonal architecture (ports & adapters).**
   - Domain logic depends on traits ("ports"), never on IO crates.
   - Adapters (azalea, sqlx, HTTP, gRPC) implement the ports at the edge.
   - Every port has a fake for tests.
2. **Functional core, imperative shell.** The bot lifecycle is a pure function `transition(&state, event, now) -> Transition { state, effects }`, and the actor only executes the effects. Retry policy, authorization, mode validation and mode scheduling work the same way.
3. **One actor per bot.** Each bot is one tokio task that owns its own state.
   - It communicates only through bounded channels: an `mpsc` inbox, a `watch` snapshot and `broadcast` events.
   - Bots share no mutable state.
4. **Desired-state reconciliation.** The server stores *what should run*. Agents keep moving *what does run* towards that and report back. Reconnects, restarts and crashes all resolve through this same loop.
5. **Parse, don't validate.** External input is turned into validated newtypes at the boundary (`ChatMessage`, `ServerAddress`, `McUsername`, …). Code further in can't construct invalid values.
6. **Fail small.** A fault in one bot never affects another. To get there:
   - All channels are bounded.
   - Every IO call has a timeout.
   - Shutdown follows a `CancellationToken` hierarchy.
   - Production code paths never panic.
7. **Explicit over magic.** There is no global state. Configuration is injected, and time and randomness can be injected so tests are deterministic.
8. **Open for extension.**

   | To add | Change |
   |---|---|
   | A mode | Data only |
   | An action | One enum variant plus one adapter mapping |
   | A frontend | One `ApiClient` implementation |
   | A storage backend | One repository implementation |

## 4. Repository layout

```
afkfleet/
├── CLAUDE.md · Plan.md · README.md · LICENSE (GPL-3.0) · SECURITY.md
├── .gitignore · .gitattributes · .editorconfig
├── .github/                   # workflows/, pull_request_template.md
├── Cargo.toml                 # workspace: members, [workspace.dependencies], [workspace.lints], profiles
├── rust-toolchain.toml        # dated nightly (required by azalea)
├── rustfmt.toml · clippy.toml · deny.toml · .config/nextest.toml
├── justfile                   # every dev command
├── package.json · pnpm-workspace.yaml · biome.json · tsconfig.base.json
├── crates/
│   ├── fleet-core/            # pure domain: ids, value objects, bot state machine, retry, modes, RBAC, MC ports
│   ├── fleet-mc/              # azalea adapter: the ONLY crate that depends on azalea
│   ├── fleet-runtime/         # bot actor, mode runner, watchdog, supervisor (generic over ports)
│   ├── fleet-agent/           # agent binary: config, wiring, control-plane client, standalone dev mode
│   ├── fleet-proto/           # .proto files, generated tonic/prost code, core <-> proto conversions
│   ├── fleet-api-types/       # HTTP/WS DTOs shared by server, client and app (exported to TS via ts-rs)
│   ├── fleet-server/          # server binary: HTTP API, auth, vault, persistence, gRPC control plane, CLI
│   ├── fleet-client/          # typed HTTP + WS client (used by the Tauri backend and integration tests)
│   └── fleet-testkit/         # fakes, fixtures, builders (dev-dependency only)
├── apps/
│   └── desktop/               # Tauri app: src-tauri/ (Rust, workspace member) + thin TS entry and TauriApiClient
├── packages/
│   └── ui/                    # the React app (routes, features, components, ApiClient interface); reusable for web
├── deploy/                    # Dockerfiles, compose files, Caddyfile, example configs
├── spikes/                    # throwaway experiments, NOT workspace members
└── docs/
    ├── adr/                   # architecture decision records: NNNN-title.md
    ├── threat-model.md
    └── runbook.md
```

Crates are created in the phase that first needs them, not up front.

**Crate dependencies.** These rules are enforced in review, and with `cargo tree` checks where that's possible.

| Crate | May depend on |
|---|---|
| `fleet-core` | Nothing internal; no IO, no tokio, no azalea |
| `fleet-runtime` | `fleet-core` |
| `fleet-mc` | `fleet-core`, **azalea** |
| `fleet-proto` | `fleet-core` |
| `fleet-api-types` | `fleet-core` |
| `fleet-agent` (bin) | `fleet-core`, `fleet-runtime`, `fleet-mc`, `fleet-proto` |
| `fleet-server` (bin) | `fleet-core`, `fleet-proto`, `fleet-api-types`, **azalea-auth** |
| `fleet-client` | `fleet-api-types` |
| `apps/desktop/src-tauri` | `fleet-client`, `fleet-api-types` |
| `fleet-testkit` | `fleet-core` (dev-dependency for everyone else) |

Binaries stay thin: `main.rs` parses the CLI and config and wires adapters together, and all logic lives in the crate's library part.

## 5. Crate & tool registry

**Rule: one crate per concern, used throughout the whole project.** A concern that isn't covered here needs the user's approval, an ADR, and an entry in this table and in the index in `CLAUDE.md`. All Rust crates are declared once in `[workspace.dependencies]`, when the first crate uses them (see the P0.3 note). Versions were pinned in P0.3 (2026-10-05) after checking crates.io and docs.rs. They're caret requirements, and `Cargo.lock` holds the exact pin (ADR-0009). **dfo** means `default-features = false` at workspace level.

### Rust: runtime, domain & infrastructure
| Concern | Crate | Version | Used in | Notes |
|---|---|---|---|---|
| Minecraft protocol & client | `azalea` | `=0.16.0` (+mc26.1) | fleet-mc | Needs nightly (ADR-0003). Disable its `AutoReconnectPlugin` and `AutoRespawnPlugin`. Runs only inside a `LocalSet` |
| Microsoft / Minecraft auth | `azalea-auth` | `=0.16.0` (+mc26.1) | fleet-server | Device-code flow. Never use its file cache |
| Async runtime | `tokio` | 1.53.2 | runtime, mc, agent, server, client | `test-util` feature in dev |
| Cancellation, task tracking | `tokio-util` | 0.7.19 | runtime, mc, agent, server | `CancellationToken`, `TaskTracker` |
| Stream adapters | `tokio-stream` | 0.1.19 | proto, agent, server | gRPC streams, broadcast → stream |
| Sink/Stream extension traits | `futures-util` | 0.3.34 | client, server | WebSocket split/send |
| Async fns in `dyn` traits | `async-trait` | 0.1.92 | server | Only for `Arc<dyn Port>`. Use generics + RPITIT elsewhere |
| Retry & backoff | `backon` | 1.6.0, dfo | core (policy), agent, client | Exponential backoff with jitter. The default features pull in a tokio sleeper |
| Rate limiting (keyed / in-process) | `governor` | 0.10.4 | runtime (chat), server (per user) | |
| Rate limiting (HTTP middleware) | `tower_governor` | 0.8.0 | server | Per IP |
| TTL cache / single-flight | `moka` | 0.12.16 | server | MC token cache, WS tickets |
| Library errors | `thiserror` | 2.0.21 | all libraries | |
| Binary error reporting | `anyhow` | 1.0.104 | `main.rs` only | |
| Serialization | `serde`, `serde_json` | 1.0.229, 1.0.151 | all | |
| Configuration | `figment` | 0.10.19 | agent, server | TOML file + env; upstream is quiet but the crate is stable |
| DTO & config validation | `garde` | 0.23.0 | api-types, agent, server | Domain value objects use hand-written constructors |
| Logging / tracing | `tracing`, `tracing-subscriber` | 0.1.44, 0.3.23 | all | `env-filter`, `json` |
| Metrics | `metrics`, `metrics-exporter-prometheus` | 0.24.6, 0.18.3 (dfo) | runtime, agent, server | Internal port only. The exporter's default `push-gateway` brings its own TLS stack: enable `http-listener` only |
| IDs | `uuid` | 1.27.0, dfo | core | v7, serde |
| Time | `chrono` | 0.4.45, dfo | core, server | Always UTC. No `clock` feature in core: time is passed in |
| CLI | `clap` | 4.6.7 | agent, server | derive |
| Hidden password prompt | `rpassword` | 7.5.4 | server CLI | |

### Rust: API, transport & persistence
| Concern | Crate | Version | Used in | Notes |
|---|---|---|---|---|
| HTTP framework + WebSocket server | `axum` | 0.8.9 | server | `ws` feature |
| Typed headers (and cookies later) | `axum-extra` | 0.12.6 | server | `TypedHeader<Authorization<Bearer>>` |
| Service abstraction | `tower` | 0.5.3 | server, client | |
| HTTP middleware | `tower-http` | 0.7.1 | server | request-id, trace, timeout, body limit, sensitive headers, set-header, catch-panic |
| OpenAPI | `utoipa`, `utoipa-axum` | 6.0.0, 0.3.0 | server | P0 picked 6.x (ADR-0009): released 2026-09-22, re-check at P6 |
| Database | `sqlx` | **0.9.0**, dfo | server | `runtime-tokio`, `sqlite`, `migrate`, `macros`, `chrono`, `uuid`; no TLS feature (SQLite); offline `.sqlx/` |
| gRPC | `tonic`, `tonic-prost` | **0.14.6** | proto, agent, server | Features `tls-aws-lc`, `tls-connect-info`. Never `tls-ring` or `tls-webpki-roots` |
| Protobuf | `prost` | 0.14.4 | proto | |
| Protobuf codegen | `tonic-prost-build`, `protox` | 0.14.6, 0.9.1 | proto (`build.rs`) | Pure Rust, no system `protoc` |
| TLS | `rustls` | 0.23.45 | agent, server, client, desktop | **Only the aws-lc-rs provider**, installed explicitly at startup. The default features select it |
| X.509 / CSR | `rcgen` | 0.14.10, dfo | server (CA, signing), agent (CSR) | `aws_lc_rs`, `pem`, `x509-parser`. The default is ring |
| HTTP client | `reqwest` | **0.13.5**, dfo | client, server, desktop | Feature `rustls` (aws-lc-rs + platform verifier), no native-tls |
| WebSocket client | `tokio-tungstenite` | 0.29.0, dfo | client | `connect`, `rustls-tls-native-roots`. 0.29 matches axum 0.8.9's `ws`, so only one tungstenite is built |
| Rust → TypeScript types | `ts-rs` | 12.0.1 | api-types | `chrono-impl`, `uuid-impl` |

### Rust: security
| Concern | Crate | Version | Used in | Notes |
|---|---|---|---|---|
| Password hashing | `argon2` | **0.6.0** | server | Argon2id, in `spawn_blocking` behind a semaphore |
| Authenticated encryption | `chacha20poly1305` | **0.11.0** | server (vault) | XChaCha20-Poly1305 |
| Hashing | `sha2` | **0.11.0** | server | Token hashes, certificate fingerprints |
| Constant-time comparison | `subtle` | 2.6.1 | server | |
| Encoding | `base64` | 0.23.1 | server, client | URL-safe, no padding |
| Secret wrappers | `secrecy` | 0.10.3 | core, mc, agent, server, client, desktop | Redacted `Debug` |
| Memory zeroing | `zeroize` | 1.9.0 | server, agent | |
| Secure randomness | `getrandom` | 0.4.3 | server | Tokens, keys, nonces: **always** use this |
| Non-security randomness | `rand` | **0.10.3**, dfo | core, runtime | Jitter, random look angles; seeded `StdRng` in tests. No OS randomness in core |
| TOTP 2FA | `totp-rs` | 6.0.0 | server | Feature `qr`. `gen_secret` uses rand's thread RNG, so P7 generates the secret bytes with `getrandom` instead (security rule 4) |
| Password strength | `zxcvbn` | 3.1.1 | server | |

### Rust: desktop
| Concern | Crate | Version | Used in | Notes |
|---|---|---|---|---|
| Desktop shell | `tauri`, `tauri-build` | **2.12.1** (≥ 2.11.1), 2.7.1 | desktop | 2.11.1 fixes CVE-2026-42184 (`is_local_url` origin bypass on Windows). `tauri-build` is the required build-dependency (approved in P0) |
| Open the device-code URL | `tauri-plugin-opener` | 2.7.0 | desktop | Called only from Rust |
| Single instance | `tauri-plugin-single-instance` | 2.5.2 | desktop | |
| Signed auto-update | `tauri-plugin-updater` | 2.13.1, dfo | desktop | Its default `rustls-tls` feature forces the ring provider; TLS comes from the workspace `reqwest` instead |
| OS credential store | `keyring-core` + `windows-native-keyring-store` | 1.0.0, 1.1.0 (keyring v4 family) | desktop | Refresh token storage |

### Rust: testing
| Concern | Crate | Version | Notes |
|---|---|---|---|
| Fixtures / parametrized tests | `rstest` | 0.27.0, dfo | `#[case]` tables. The default async-timeout and crate-renaming features aren't needed (ADR-0010) |
| Property testing | `proptest` | 1.11.0, dfo | State machine, parsers. Only `std`, which reads `PROPTEST_CASES`; no fork or timeout mode (ADR-0010) |
| Snapshot testing | `insta` | 1.49.0 | `json` and `redactions` features, enabled by the member that uses them (fleet-core: `json`) |
| Mocks | `mockall` | 0.15.0 | Only for interaction checks; put `#[automock]` above `#[async_trait]` |
| Containers | `testcontainers` | 0.28.0, dfo | `itzg/minecraft-server`. The default `ring` feature turns on TLS for the Docker client, which the local socket doesn't need |
| Time control | `tokio` `test-util` | | `start_paused`, `advance` |
| Fuzzing | `libfuzzer-sys`, `arbitrary` | 0.4.13, 1.4.2 | Driven by cargo-fuzz |

### Frontend (same one-library-per-concern rule)
| Concern | Package | Notes |
|---|---|---|
| Package manager / workspaces | pnpm | |
| Bundler & dev server | `vite` | |
| UI framework | `react`, `react-dom` 19 | |
| Language | `typescript` | `strict`, `noUncheckedIndexedAccess`, `exactOptionalPropertyTypes` |
| Server state & caching | `@tanstack/react-query` | |
| Routing | `@tanstack/react-router` | Type-safe routes |
| UI-only state | `zustand` | Small stores only |
| Schema validation | `zod` 4 | Forms |
| Forms | `react-hook-form`, `@hookform/resolvers` | |
| Styling | `tailwindcss` 4 | |
| Components | shadcn/ui (Radix UI primitives, copied into the repo) | |
| Icons | `lucide-react` | |
| Toasts | `sonner` | |
| Tauri bridge | `@tauri-apps/api` 2 | Only in `apps/desktop` |
| Unit / component tests | `vitest`, `@testing-library/react`, `@testing-library/user-event`, `jsdom` | |
| API mocking | `msw` | |
| E2E | `@playwright/test` | |
| Lint & format | `@biomejs/biome` | |

### Tools & infrastructure (not crates)
- **Rust tooling** (versions pinned in P0.3; tools for later phases are re-checked when they're introduced):
  - rustup with the dated nightly from `rust-toolchain.toml` (ADR-0003), plus stable 1.99.0 for the `stable-check` job
  - `just` 1.58.0
  - cargo-nextest 0.9.146, cargo-llvm-cov 0.9.1, cargo-deny 0.20.2, cargo-insta 1.49.0
  - Later: sqlx-cli **0.9.0** (P6), cargo-chef 0.1.78 (P5), cargo-mutants 27.1.0 and cargo-fuzz 0.13.2 (P13)
- **Frontend tooling:** pnpm 12.9.1 (`packageManager`) and Biome 2.5.15 from P0.8; every other package is pinned in the phase that introduces it.
- **Runtime & hosting:**
  - Docker + Docker Compose
  - Caddy 2 (TLS for the public API)
  - `itzg/minecraft-server` (test server)
- **Windows build dependencies:** MSVC Build Tools ("Desktop development with C++"), NASM (for aws-lc-rs). CMake isn't needed for non-FIPS builds.
- **Automation:** GitHub Actions (CI). No Dependabot or Renovate: `cargo deny` and `pnpm audit` flag vulnerable dependencies in CI (also on a weekly schedule), and dependency updates are done by hand in their own PR.

## 6. Fault-tolerance model

The bots run as one tokio task each, which is the user's decision. azalea needs a `LocalSet`, so the azalea clients themselves run on dedicated **MC host threads**: each has a current-thread runtime plus a `LocalSet`, managed by `fleet-mc`. Recovery is layered:

| # | Fault | Detected by | Recovery |
|---|---|---|---|
| 1 | Connection drop / transient kick | `Disconnected` / `ConnectionFailed` event | `Backoff` with exponential jittered delay (default 5 s → 5 min) → reconnect |
| 2 | Kick that won't heal (banned, not whitelisted, wrong version) | `DisconnectReason` classifier | `Failed(reason)`, alert the user, no retry until **Reset** |
| 3 | Duplicate login (a human logged into the account) | Classifier | `Paused`. **Never fight the human.** The user resumes it. |
| 4 | Flapping server | Circuit breaker (N failures within a window) | Open circuit with a long cool-down, then one half-open attempt |
| 5 | Expired or invalid session token | Auth error on join | Request one fresh token. If it fails again: `Failed(Auth)` and account marked "re-auth required" |
| 6 | Zombie session: azalea ECS panic or hang (azalea has no `catch_unwind`), frozen server or dead link | **Panic:** the runner's `AppExit` receiver fails at once. **Hang:** the Tick watchdog, no `Tick` for `watchdog_timeout` (30 s) while Online. **Frozen server or dead link:** the packet-liveness timeout, no packet for `packet_liveness_timeout` (30 s) while Online. Ticks are client-side and keep running then (ADR-0008 §5) | Tear down the session and treat it as a transient disconnect |
| 7 | Panic in the bot actor task | Supervisor sees `JoinError::is_panic()` | Restart the actor from its last spec. More than 5 restarts in 10 min → `Failed(CrashLoop)` |
| 8 | MC host thread hangs | Tick watchdog (row 6), and the thread's job queue stops answering | **Abandon** the thread and count it; it is never joined or respawned. The bot reconnects on a fresh thread. Above the abandoned-thread limit, the agent process exits and Docker restarts it (ADR-0008 §5) |
| 9 | Agent can't reach the server | gRPC stream error | Bots **keep running** on the last desired state. The agent reconnects with backoff, sends `Hello` with its actual state, and receives `ReconcileFull` |
| 10 | Agent crash or OOM | Docker healthcheck / exit code | `restart: unless-stopped`. The server marks the agent stale after the heartbeat timeout and shows its bots as `Unknown` until it reconciles |
| 11 | Server crash | Docker | Restart. SQLite WAL keeps the data durable and agents reconnect by themselves |
| 12 | Slow consumers (WebSocket, subscribers) | `broadcast` returns `Lagged` | Send a `Resync` message; the client refetches the snapshot |
| 13 | Overload | Bounded queues, argon2 semaphore, rate limits | Backpressure, `429` or `503` instead of running out of memory |

The attempt counter resets after a bot has been Online for a stable period (default 5 min).

**Known limit.** A Minecraft session token lives about 24 h. If the server is unreachable for longer than that, a bot that disconnects can't rejoin until the server is back.

**Escape hatch.** All runtime code is written against the `MinecraftConnector` port. If the spike or production shows that in-process isolation isn't enough, a **process-per-bot** connector can be added later without touching the runtime.

## 7. Security model

### 7.1 Assets, attackers, mitigations
The full analysis lives in `docs/threat-model.md`.

**Assets**, most critical first:
1. Microsoft refresh tokens (account takeover).
2. The vault master key and the CA private key.
3. User credentials and sessions.
4. Control over bots: they can chat and run commands as the player, possibly with operator rights.
5. Chat history and the audit log.

**Attackers:**
- Internet attackers probing the public API.
- A malicious or compromised friend account (insider).
- A stolen laptop with the app installed.
- A compromised agent host.
- Hostile content coming from Minecraft servers.

| Threat | Mitigation |
|---|---|
| Brute force / credential stuffing | Argon2id; per-IP and per-account throttling; lockout with exponential delay; mandatory 2FA for every user; invite-only registration |
| Token theft | Short-lived opaque access tokens; refresh-token rotation with reuse detection that revokes the whole family; tokens hashed at rest; refresh token only in the OS keychain, never in JS or localStorage |
| Privilege escalation by a friend | One pure `authorize()` for every route; per-account grants; Admins can't touch the Owner or other Admins; authorization-matrix test; route-coverage test; audit log |
| Bot abuse (`/op`, `/pay` …) | Slash commands need **Manage**, or must be on the allowlist; per-bot and per-user chat rate limits; every sent message is audited |
| Database or backup leak | MS tokens and TOTP secrets encrypted with XChaCha20-Poly1305, bound to their record by AAD; the key is stored outside the DB and the backups; passwords hashed with Argon2id; session tokens stored as SHA-256 |
| Rogue or compromised agent | mTLS with server-signed certificates; single-use enrollment tokens; fingerprint allowlist and revocation; an agent only ever gets session tokens for bots assigned to it |
| Malicious chat / XSS | Chat is sanitized plain text: control and format codes stripped, length capped. React escaping, no `dangerouslySetInnerHTML`, strict CSP |
| DoS | Rate limits, body-size limits, timeouts, bounded queues, an argon2 semaphore, WebSocket connection caps |
| Supply chain | `cargo deny` (advisories, licenses, sources, bans), `pnpm audit`, committed lockfiles, the minimal crate registry above |
| Information leakage | Generic error messages with request IDs; no stack traces; uniform login responses plus a dummy hash so usernames can't be enumerated; secrets redacted from logs |

### 7.2 Authentication
- **Passwords**
  - Argon2id. The baseline is OWASP's: m = 19 MiB, t = 2, p = 1, and it is configurable.
  - Hashing runs in `spawn_blocking`, behind a semaphore.
  - Passwords must be 12–128 characters and reach zxcvbn score ≥ 3.
  - For unknown usernames the server checks against a **dummy hash**, so the timing is the same as for real ones.
- **Tokens**
  - 256 bits from `getrandom`, encoded as base64url, with the prefix `afk_at_` (access) or `afk_rt_` (refresh) so secret scanners can spot them.
  - The DB stores only their **SHA-256 hash**.
  - Access tokens live 15 min.
  - Refresh tokens rotate on every use and belong to a *family*. Reusing an already rotated token revokes the whole family and logs the audit event `security.refresh_token_reuse`.
  - Refresh expiry is 7 days idle and 30 days absolute.
- **Immediate revocation.** Every request looks its session up in the DB, so logout, disabling a user and role changes take effect immediately.
- **2FA (TOTP, RFC 6238)**
  - The secret is encrypted with the vault.
  - The last used time step is stored so a code can't be replayed.
  - Users get 10 single-use recovery codes, stored hashed.
  - **2FA is mandatory for every user**, Owner and Admins included, and it can't be switched off by configuration.
  - Until a user has enrolled, the password step returns `mfa_setup_required` together with a single-use **setup token** (10 min). The setup token only works for `POST /me/mfa/enroll` and `POST /me/mfa/confirm`; after confirming, the user logs in normally. The password step never returns real tokens on its own.
  - Login is two steps: the password returns a single-use `mfa_token` (5 min), and that token plus the code returns the real tokens.
- **Registration only by invite**
  - Admins and above create invites; each is single-use and expires (default 72 h).
  - An invite carries a fixed role: Member, or Admin if the Owner created it.
- **Bootstrap.** `afkfleet-server user create-owner` (CLI, hidden password prompt). It refuses if an Owner already exists.
- **Step-up.** These actions require re-entering the password within the last 5 min:
  - changing the password
  - resetting 2FA (the user has to enroll again at the next login)
  - deleting an MC account
  - changing a role
  - rotating keys

### 7.3 Authorization
- **Global roles**
  - **Owner:** exactly one; can do everything.
  - **Admin:** manages Members, invites, agents and every account.
  - **Member:** manages their own accounts and the accounts they've been granted.
- **Per-account grants:** `View` < `Control` < `Manage`.
  - Whoever links an account owns it, which counts as Manage.
  - Owner and Admin implicitly have Manage on every account.

| Action | Requires |
|---|---|
| See bot status, chat history | View |
| Start / stop / restart, change mode, send plain chat, send allowlisted commands | Control |
| Send any `/command`, change server address, edit grants, re-link, delete account | Manage |
| Link a new Microsoft account | Member+ (within quota) |
| Create custom modes (private) / shared modes | Member+ / Admin+ |
| Manage Members, invites | Admin+ |
| Manage Admins, transfer ownership | Owner |
| Agents, audit log, server settings | Admin+ |

**How it's enforced**
- One function decides everything: `fleet_core::authz::authorize(&Actor, Permission, &ResourceContext) -> Result<(), AuthzError>`.
- It is pure, **denies by default**, and has an exhaustive table test.
- Nobody can grant more than their own level.
- If an actor can't even View a resource, the API answers **404** rather than 403, so it doesn't reveal that the resource exists.

### 7.4 Rate limits (defaults, all configurable)
| Scope | Default | Key |
|---|---|---|
| All requests | 20 req/s, burst 40 | Client IP |
| `POST /auth/login`, `/auth/login/mfa` | 5/min | IP. Also a per-username lockout after 5 failures: 1 min, doubling up to 1 h, stored in the DB |
| `POST /auth/register` | 5/hour | IP |
| `POST /auth/refresh` | 30/min | IP |
| Authenticated API | 10 req/s, burst 30 | User |
| Sending bot chat | 1 msg / 3 s, burst 3 per bot; 20/min per user | Bot, user |
| `SendChat` actions inside modes | Interval ≥ 30 s | Enforced by mode validation |
| WebSocket | 3 connections per user; ticket single-use, 30 s TTL | User |
| Microsoft link flows | 1 active per user, 3 active in total, 10/day per user | User |
| gRPC enrollment | 5/min | IP |

Responses use `429` with a `Retry-After` header. The client IP is the socket peer address. `X-Forwarded-For` is honored only when the peer is in `trusted_proxies` (Caddy).

### 7.5 Secrets & cryptography
- **Vault**
  - Cipher: XChaCha20-Poly1305.
  - Envelope: `version | key_id | nonce(24) | ciphertext`.
  - AAD: `record_id ‖ purpose`. A ciphertext can't be moved to another record or purpose without failing.
  - Master keys load from files (Docker secrets) into `SecretBox`.
  - `afkfleet-server vault rotate-key` re-encrypts everything inside one transaction.
- **CA private key.** Its own secret, separate from the vault key.
- **Agents.** They receive only short-lived Minecraft session tokens, held in memory as `SecretString`, and only for the bots assigned to them.
- **Never log secrets:**
  - `secrecy` types (their `Debug` is redacted).
  - `SetSensitiveRequestHeaders`.
  - Tests that assert `Debug` and log output contain no token material.
- **TLS everywhere:**
  - Caddy with Let's Encrypt in front of the API.
  - tonic with rustls mTLS for agents. Caddy terminates TLS, so the gRPC port is published **directly** by the server.
  - The app checks certificates against the system roots or a configured custom CA, and never accepts invalid ones.
- **One rustls crypto provider (aws-lc-rs)** for every crate, installed explicitly at process start.

### 7.6 Platform hardening
- **Rust:**
  - `unsafe_code = "forbid"` and no `#![feature]` in our crates.
  - A clippy deny-list (see P0.3).
  - Release builds use `overflow-checks = true` and `panic = "unwind"`, never `abort`.
- **HTTP:**
  - Security headers: `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `Cache-Control: no-store`, and HSTS at Caddy.
  - 64 KiB body limit and 15 s timeout.
  - **No CORS layer.** The app proxies every call through Rust, and the future website will be served from the same origin.
- **Tauri:**
  - Version ≥ 2.11.1.
  - Strict CSP: no remote origins, no `unsafe-eval`.
  - Capabilities grant only our own commands.
  - `withGlobalTauri: false`.
  - Devtools only in debug builds.
  - Signed updates.
  - No remote content is ever loaded.
- **Docker:**
  - Distroless non-root images with a read-only root filesystem.
  - `cap_drop: [ALL]` and `no-new-privileges`.
  - Only Caddy (80/443) and, if needed, the gRPC port are exposed.
  - Secrets come from Docker secrets; resources are limited.
- **Supply chain:** `cargo deny` and `pnpm audit` in CI (on every push and weekly), committed lockfiles, GitHub Actions pinned to full commit SHAs, and pinned toolchains.

## 8. Testing strategy

**The TDD loop for every task:**
1. 🔴 Write a failing test, run it, and check that it fails *for the right reason*.
2. 🟢 Write the minimal implementation that makes it pass.
3. 🔵 Refactor while the tests stay green.

| Level | Location | Covers | Tools |
|---|---|---|---|
| Unit | `#[cfg(test)] mod tests` at the bottom of the file | Pure logic: state machine, policies, validation, authz, crypto envelope | rstest, proptest, insta |
| Component | `crates/<crate>/tests/*.rs` | Actor with fake session; services on a temporary SQLite DB; handlers via `tower::ServiceExt::oneshot` | tokio `test-util`, `#[sqlx::test]`, fleet-testkit |
| Integration | `crates/<crate>/tests/*.rs` | In-process server + `fleet-client`; gRPC over loopback with test PKI; azalea against a Minecraft container (*slow*) | testcontainers, nextest `slow` profile |
| Frontend | `packages/ui/src/**/*.test.tsx` | Components, hooks, flows against a `FakeApiClient` or MSW | Vitest, Testing Library, MSW |
| E2E | `packages/ui/e2e/`, `deploy/` | UI flows (Playwright against the web build + MSW); Compose smoke test | Playwright, Docker Compose |

**Rules**
- **Deterministic.**
  - Anything time-based uses `#[tokio::test(start_paused = true)]` and `tokio::time::advance`.
  - `governor` and `moka` keep their own clocks, which paused tokio time doesn't control. Code that uses them takes an injectable clock so tests stay deterministic.
  - Randomness comes from a seeded `StdRng`.
  - No real `sleep`, and no network outside the `slow` profile.
- **Red means a failing assertion.** In Rust, a test that doesn't compile isn't "red". Write a compiling stub that returns a wrong value first, then show the failing assertion.
- **Every bug fix starts** with a regression test that fails.
- **Every public function** has a test for each of its error paths.
- **Fakes over mocks.** Fakes live in `fleet-testkit`; `mockall` is only for checking interactions.
- **Snapshots.** API JSON and error bodies are covered by insta snapshots. Never accept them blindly: the assistant reads each new snapshot before accepting it and lists it in the PR description, and the user reviews it in the PR diff.
- **Property tests** for state machines, parsers and conversions. CI runs them with `PROPTEST_CASES=1000`.
- **Coverage gates** (cargo-llvm-cov): `fleet-core` ≥ 90 %, `fleet-runtime` ≥ 85 %, auth and vault modules ≥ 90 %, workspace ≥ 80 %. Per-crate and per-module gates are checked by a small script over the llvm-cov JSON report.
- **Mutation testing** (cargo-mutants) on `fleet-core` and auth in Phase 13.
- **nextest profiles:**
  - `default`: fast, under ~60 s.
  - `slow`: tests named `slow_*`; needs Docker.
  - `ci`: JUnit output, no retries.

---

## 9. Phases

**Conventions**
- Each task has an ID `P<phase>.<n>`. 🔴 means *the test comes first*, strictly. Tasks without 🔴 (scaffolding, config, wiring) are verified by `just check`, CI or the task's demo, but any logic they add still gets tests.
- A phase is finished when every task is ticked **and** its Definition of Done (DoD) holds.
- **Branches:** one branch per task, `p<phase>/<task-id>-<slug>` (e.g. `p2/p2.6-bot-state-machine`). Phases 0 and 1 each use a single branch, `p0/foundation` and `p1/azalea-spike`. Phase 2 uses five group branches, one PR each (see the note under Phase 2). Every branch ends in a PR that the user reviews and merges. The `Plan.md` checkbox is ticked in that same PR.
- Phases are vertical slices:
  - **0–5** produce a working standalone bot.
  - **6–11** build the fully managed system with the app.
  - **12–13** make it production ready.

| Phase | Name | Milestone |
|---|---|---|
| 0 | Foundation & tooling | `just ci` green on an empty workspace |
| 1 | azalea spike | Minecraft layer de-risked (ADR-0008) |
| 2 | Domain core | All business rules, pure and tested |
| 3 | Minecraft adapter & testkit | Real bot joins a test server; fake for everything else |
| 4 | Bot runtime | Self-healing bots proven with chaos tests |
| 5 | Standalone agent | **First runnable product** (dev, offline accounts) |
| 6 | Server foundation | Secure HTTP skeleton, persistence, audit |
| 7 | AuthN & AuthZ | Invite-only login, 2FA, RBAC, rate limits |
| 8 | Desktop app foundation | Friends can log in through the app |
| 9 | Vault & Microsoft accounts | Real accounts linked securely |
| 10 | Control plane | Server ⇄ agent over mTLS, reconciliation |
| 11 | Bot operations & live events | Everyday management in the app |
| 12 | Deployment & operations | Reproducible production deployment |
| 13 | Hardening | Security & resilience proven |
| 14 | *Website (future)* | Not planned yet |

---

### Phase 0: Foundation & tooling
**Goal:** An empty but fully wired workspace in which every quality gate is active from day one.
**Prerequisites (done by the user):**
- Dev machine: Git, GitHub CLI, PowerShell 7, `just`, MSVC Build Tools, NASM, rustup, Node 24 + pnpm, Docker Desktop (WSL 2), Claude Code.
- GitHub: public repo with `README.md` and GPL-3.0 `LICENSE`, `main` ruleset, Actions and security settings.

**Introduces:** the tools cargo-nextest, cargo-llvm-cov, cargo-deny, cargo-insta and Biome. sqlx-cli comes in Phase 6. No runtime crates yet.

- [x] **P0.1** The repo already exists: it was created on GitHub with `README.md` and a GPL-3.0 `LICENSE`, then cloned. So there's no `git init`. Work on the branch `p0/foundation`. Add:
  - `.gitignore`: `target/`, `node_modules/`, `.env*`, `*.db*`, `secrets/`, `*.pem`, `*.key`, `.claude/settings.local.json`
  - `.gitattributes`: `* text=auto eol=lf`, plus binary file types
  - `.editorconfig`
  - `README.md`, rewritten as a skeleton:
    - what it is
    - **Status** (current phase and what works today)
    - prerequisites, quickstart, development
    - links to the architecture docs
    - license
    - a note that the bots are only for servers that allow them
    - a note that the project is built with AI assistance
  - `SECURITY.md`: report vulnerabilities through GitHub's private vulnerability reporting
  - `.github/pull_request_template.md` with the Definition of Done checklist from `CLAUDE.md`

  > Note (P0.1): `.claude/settings.json` was dropped from this task and from the §4 layout at the user's request; the user doesn't provide one. `CLAUDE.md` security rule 12 now states the secret-file rule as an instruction instead. `.claude/settings.local.json` stays in `.gitignore`.
- [x] **P0.2** Pick the target Minecraft version and the matching azalea release or git rev. Pin a **dated nightly** in `rust-toolchain.toml` with the components `rustfmt`, `clippy` and `llvm-tools-preview`. Record this in ADR-0003, including the bump procedure (nightly + azalea + test-server `VERSION` change together).

  > Note (P0.2):
  > - **azalea:** `=0.16.0` from crates.io, so Minecraft 26.1 (the user's choice). The current Minecraft release is 26.3, so 26.2/26.3 servers need ViaVersion/ViaBackwards.
  > - **Nightly:** pinned to `nightly-2026-08-21`, not the first candidate `nightly-2026-10-01`, which fails to compile azalea 0.16.0 (E0284 in `azalea-core`). A bisect found 08-21 as the last good and 08-22 as the first bad nightly.
  > - **Verification:** `spikes/toolchain-check/` builds on Windows and in `rust:1-bookworm`. It stays in the repo for future bumps.
  > - **Details:** see ADR-0003.
- [x] **P0.3** Write the workspace `Cargo.toml`:
  - `[workspace.package]`: edition 2024, `license = "GPL-3.0-or-later"`, `publish = false`, `repository` = the GitHub URL. Every member inherits them (`license.workspace = true`, `publish.workspace = true`, …).
  - `[workspace.dependencies]` with **every crate from §5**, each version checked on docs.rs. Crates are only *used* in their own phase.
    - Crates that `fleet-core` uses (`backon`, `rand`, `chrono`, `uuid`, …) get `default-features = false`, and members enable only the features they need. `backon`, for example, pulls in a tokio sleeper by default.
  - `[workspace.lints]`:
    ```toml
    [workspace.lints.rust]
    unsafe_code = "forbid"
    missing_docs = "warn"
    missing_debug_implementations = "warn"
    unreachable_pub = "warn"

    [workspace.lints.clippy]
    pedantic = { level = "warn", priority = -1 }
    unwrap_used = "deny"
    expect_used = "deny"
    panic = "deny"
    todo = "deny"
    unimplemented = "deny"
    dbg_macro = "deny"
    print_stdout = "deny"
    print_stderr = "deny"
    indexing_slicing = "deny"
    await_holding_lock = "deny"
    large_futures = "warn"
    ```
  - `[profile.release]` with `panic = "unwind"`, `overflow-checks = true` and `lto = "thin"`.

  > Note (P0.3):
  > - **Declared on first use.** The pinned nightly's cargo has a lint, `cargo::unused_workspace_dependencies` (warn by default), which fires once per declared but unused workspace dependency. All §5 crates up front meant 67 warnings. At the user's choice, `[workspace.dependencies]` holds only crates that are in use (none in Phase 0). The versions checked on docs.rs and crates.io, and the feature constraints, are pinned in §5 instead, and each phase copies its entries from there. No lint was changed.
  > - **Changes from the baseline:**
  >   - utoipa 6.0.0 / utoipa-axum 0.3.0
  >   - tokio-tungstenite 0.29.0 instead of 0.30, matching axum 0.8.9's `ws`
  >   - base64 0.23.1 and getrandom 0.4.3 are the current releases
  >   - `tauri-build` 2.7.1 added to the registry (approved)
  > - **Version policy:** caret requirements plus `Cargo.lock`, with `=` only for azalea (ADR-0009).
  > - `exclude = ["spikes"]`, so spikes nested under the workspace root can build on their own.
  > - `[workspace.package]` also sets `version = "0.1.0"`.
- [x] **P0.4** Write the remaining config files:
  - `clippy.toml`: `allow-unwrap-in-tests`, `allow-expect-in-tests`, `allow-panic-in-tests`, `allow-indexing-slicing-in-tests`, `allow-print-in-tests`.
  - `rustfmt.toml`: stable options only, `max_width = 100`.
- [x] **P0.5** Write `deny.toml`:
  - advisories: `deny`
  - license allowlist: MIT, Apache-2.0, BSD-2/3-Clause, ISC, Zlib, Unicode-3.0, MPL-2.0
  - bans: `openssl-sys`, `native-tls`; warn on duplicate versions
  - sources: crates.io plus the pinned azalea git repo only
  - every license on the allowlist must be compatible with GPL-3.0 (all of the above are)
  - our own crates are GPL-3.0-or-later and unpublished; `[licenses.private] ignore = true` skips them, so the allowlist applies only to dependencies

  > Note (P0.5):
  > - **Sources:** crates.io only, because azalea is pinned from crates.io (ADR-0003); no git source is allowed.
  > - **Stricter than the plan:**
  >   - `ring` is banned too, so "`cargo tree -i ring` empty or justified" is enforced; a justified exception needs a `wrappers` entry and an ADR.
  >   - `unsound = "all"` and `yanked = "deny"`.
  > - `[graph] targets` limits the check to Windows MSVC and Linux GNU, so mobile-only dependencies don't count.
  > - `unused-allowed-license = "allow"`: the allowlist is meant for future dependencies.
  > - **Heads-up for P3:** a dry run against azalea 0.16.0's graph found two problems. Before azalea enters the workspace, the user has to decide on exceptions (approval + ADR) or on an azalea bump.
  >   - **Licenses:** `minecraft_folder_path` (Unlicense) and `socks5-impl` (GPL-3.0-or-later) aren't on the allowlist.
  >   - **Advisories:** RUSTSEC-2026-0118/0119 in `hickory-proto` 0.25.2 (fixed only in 0.26.1), and RUSTSEC-2023-0071 in `rsa` (no fix).
- [x] **P0.6** Add `.config/nextest.toml` with the profiles `default` (excludes `test(/^slow_/)`), `slow` and `ci`.

  > Note (P0.6): The filter is `test(/(^|::)slow_/)`, not `test(/^slow_/)`. nextest matches the full test path, so a unit test is `tests::slow_join`, which `^slow_` would miss. Checked with a temporary `tests::slow_example`: `default` and `ci` skip it, `slow` selects only it.
- [x] **P0.7** Add a `justfile` with the recipes listed in `CLAUDE.md`. Set `set windows-shell := ["pwsh", "-NoLogo", "-NoProfile", "-Command"]` (PowerShell 7).
  - Every recipe must also run in `sh` on Linux CI: one command per line, no `&&`/`||`.
  - Set environment variables with just's `export`, e.g. `export RUSTDOCFLAGS := "-D warnings"`.
  - Anything more complex goes into a script under `scripts/`.
  - `check` also runs `cargo doc --no-deps --workspace`.

  > Note (P0.7):
  > - **Stubs.** Recipes whose tools arrive later (`gen`, `db-prepare`, `mc-up`/`mc-down`, `dev-*`, `ui-test`, `e2e`) print their phase and exit 1. That keeps the `CLAUDE.md` table in sync; each phase gives its recipes real bodies.
  > - **Extra checks.** `check` also runs doctests (nextest skips them) and the tests for `scripts/`.
  > - **CI building blocks.** `check` and `ci` are composed of building-block recipes (`fmt-check`, `clippy`, `docs`, `doctest`, `test-ci`, `stable-check`, `scripts-test`, `ui-check`, `ui-audit`), and each CI job runs one of them.
  > - **Coverage gates.** `scripts/coverage-gates.mjs` is plain Node (Node 24 is required everywhere anyway), with `node --test` tests. A gate whose files don't exist yet is skipped; the workspace gate never is. The auth and vault path patterns follow the server layout in `CLAUDE.md` and get confirmed in P7.
  > - **NASM.** `.cargo/config.toml` (a file not in the §4 layout) sets `AWS_LC_SYS_PREBUILT_NASM=0` for every cargo invocation, so a missing NASM fails the build instead of silently linking prebuilt objects (ADR-0009, P0.10).
- [x] **P0.8** Set up the frontend workspace:
  - `package.json`
  - `pnpm-workspace.yaml` (`apps/*`, `packages/*`)
  - `biome.json` (ignores `packages/ui/src/generated/`)
  - `tsconfig.base.json` (strict flags from §5)
- [x] **P0.9** 🔴 Create `crates/fleet-core` with one trivial test. Use it to prove that fmt, clippy, nextest and llvm-cov all run.
- [x] **P0.10** Check that the **single rustls provider (aws-lc-rs)** builds on Windows (MSVC) and in the Linux builder image, with `reqwest`, `tonic`, `rcgen` and `tokio-tungstenite` configured for it.
  - NASM is installed on the dev machine; CMake isn't needed for non-FIPS builds. Document this in the README, and make sure the Windows CI runner has NASM.
  - `cargo tree -i ring` must be empty or justified.
  - If it can't be done, move everything to `ring` instead and record that in an ADR. Never mix providers.

  > Note (P0.10):
  > - **Spike.** Verified with `spikes/tls-provider/`, which isn't a workspace member (the user's choice). reqwest 0.13.5, tonic 0.14.6, rcgen 0.14.10 and tokio-tungstenite 0.29.0, with the §5 features, install the aws-lc-rs provider and build their clients. That works on Windows (MSVC + NASM) and in `rust:1-bookworm`.
  > - **Dependency graph:**
  >   - `cargo tree -i ring --target all` prints nothing. `ring` appears in `Cargo.lock` only as an inactive optional dependency and is never compiled.
  >   - `openssl-sys`, `native-tls` and `webpki-roots` aren't in the graph at all.
  >   - `cargo deny` with the root `deny.toml` passes, so no license beyond the allowlist is needed. webpki-roots' CDLA-Permissive-2.0 is avoided.
  > - **Prebuilt NASM objects.** rustls's `aws_lc_rs` feature enables aws-lc's `prebuilt-nasm`, which silently links prebuilt objects when NASM isn't on PATH. `.cargo/config.toml` sets `AWS_LC_SYS_PREBUILT_NASM=0`; a build without NASM was confirmed to fail.
  > - **Not covered here.** `tauri-plugin-updater` and `testcontainers` default to ring, so they must use `default-features = false` (§5). The ring ban in `deny.toml` enforces it.
- [x] **P0.11** Set up CI (GitHub Actions) with these jobs:
  - `fmt`
  - `clippy` (`--all-targets -D warnings`)
  - `docs`: `cargo doc --no-deps --workspace` with `RUSTDOCFLAGS="-D warnings"`
  - `test` (nextest `ci` profile, `PROPTEST_CASES=1000`)
  - `coverage` (gates from §8, checked by the coverage script)
  - `deny`
  - `stable-check`: `cargo +stable check` on the crates that don't depend on azalea or azalea-auth
  - `frontend`: biome, `tsc --noEmit`, vitest

  These jobs are added in their own phase: `sqlx-offline`, `ts-types-fresh`, `slow-tests` (scheduled or manual), `tauri-build` (Windows runner), `compose-smoke`.

  For every workflow:
  - `permissions: contents: read` at the top.
  - Every third-party action is pinned to a full commit SHA, with the version in a comment. Updating them is a manual PR.
  - Only standard GitHub-hosted runners (free for public repos); no larger runners.

  Also run `deny` (and `pnpm audit` once the frontend exists) on a weekly `schedule`, so new security advisories show up even when nothing is pushed. There's no Dependabot.

  After the first CI run on the `p0/foundation` PR, the user adds the job names as required status checks in the `main` ruleset.

  > Note (P0.11):
  > - **Jobs call `just`.** Every job runs one `just` recipe (just and the cargo tools come from `taiki-e/install-action` at pinned versions), so CI and `just ci` can't drift apart.
  > - **Toolchains.** The nightly comes from `rust-toolchain.toml` via `rustup install`, so there's no toolchain action. The stable version comes from the justfile.
  > - **Windows.** `clippy` and `test` also run on `windows-latest`, with NASM from `ilammy/setup-nasm`. All other jobs run on `ubuntu-latest`.
  > - **frontend** also runs the `scripts/` tests and `pnpm audit`, because the frontend workspace exists since P0.8.
  > - **audit.yml** runs weekly (Mondays) and on demand: `just deny` and `just ui-audit`.
  > - **Job names for the required checks:** `fmt`, `clippy (ubuntu-latest)`, `clippy (windows-latest)`, `docs`, `test (ubuntu-latest)`, `test (windows-latest)`, `coverage`, `deny`, `stable-check`, `frontend`.
- [x] **P0.12** Add `docs/adr/0000-template.md` and these ADRs:

  | ADR | Topic |
  |---|---|
  | 0001 | Use ADRs |
  | 0002 | Hexagonal architecture & crate layout |
  | 0003 | azalea + pinned nightly |
  | 0004 | One tokio task per bot + MC host threads (revised by the spike to one host thread per bot, see ADR-0008) |
  | 0005 | SQLite via sqlx |
  | 0006 | Opaque tokens instead of JWT |
  | 0007 | Tauri Rust-side API proxy (keychain, no CORS) |

  > Note (P0.12): ADR-0009 ("Phase 0 dependency and tooling conventions") was added for the decisions taken while setting up Phase 0:
  > - the version policy and declaring dependencies on first use
  > - the single TLS provider, with the ring ban and NASM enforcement
  > - utoipa 6 and tokio-tungstenite 0.29
  > - the stricter cargo-deny settings
  > - CI calling `just`, and Node scripts
  >
  > ADR-0008 stays reserved for P1.10. ADR-0004 is `Proposed` until the spike confirms it.
- [x] **P0.13** Add a `docs/threat-model.md` skeleton based on §7.1.

**Security:**
- All lints active from the start.
- cargo-deny green.
- `.gitignore` covers secrets, keys and DB files.
- No personal data in any committed file (CLAUDE.md, security rule 11).

**DoD:**
- `just ci` passes locally on Windows and in CI.
- All ADRs are written.
- The `p0/foundation` PR is open, and its CI has run so the user can set required status checks.

---

### Phase 1: azalea spike (time-boxed, about 2 days)
**Goal:** De-risk the Minecraft layer before building on it.
- The code lives in `spikes/azalea/`, its own Cargo project and **not** a workspace member.
- It may be messy.
- It gets archived once ADR-0008 is written. **Done:** the spike is archived, and ADR-0008 holds the results.

**Introduces:** `azalea` (pinned), `tokio`.

> Note (P1):
> - **One branch.** At the user's request, the whole phase uses one branch, `p1/azalea-spike`, and one PR, like Phase 0.
> - **Spike-only dependencies.** With the user's approval, the spike also uses `tracing` and `tracing-subscriber` (to see azalea's own logs), plus `uuid` and `reqwest` (P1.8, because `AccountTrait` names `uuid::Uuid` and `reqwest::Proxy`). All four use their §5 versions. The workspace doesn't change.

- [x] **P1.1** `just mc-up` starts `itzg/minecraft-server` with:
  - `EULA=TRUE`, `ONLINE_MODE=FALSE`, `VERSION=<pinned>`
  - a flat world, low view distance
  - RCON enabled

  A bot joins it with `Account::offline`.

  > Note (P1.1):
  > - **Compose file.** `deploy/compose.dev.yaml` (project `afkfleet-dev`, service `minecraft`), image pinned by tag and digest. Phase 5 adds the agent to the same file.
  > - **Settings.** Bound to `127.0.0.1:25565` only. RCON is enabled with the image's random per-start password and an unpublished port; commands run through `docker compose … exec minecraft rcon-cli`. `MAX_PLAYERS` defaults to 60 (overridable with `MC_MAX_PLAYERS`) for the load tests.
  > - **Recipes.** `mc-down` also deletes the world (`--volumes`).
- [x] **P1.2** Run a client on a dedicated OS thread (current-thread runtime + `LocalSet`) and hand `(Client, events)` back to the multi-threaded runtime through a oneshot. Confirm that `Client` is `Send` and can be used from other threads.

  > Note (P1.2): `Client` is `Send + Sync + Clone` and works from tokio worker threads. Three start-up variants were compared; the hand-rolled join ("Variant C") won. `ClientBuilder` and Swarms can self-deadlock on teardown (an azalea bug). See ADR-0008 §1.
- [x] **P1.3** Find out how to build a client **without** `AutoReconnectPlugin` and `AutoRespawnPlugin`, and what failures look like:
  - how a connection failure shows up (`Event::ConnectionFailed`?)
  - what disconnect reasons look like, and whether translation keys are available for banned, not whitelisted, duplicate login, server full and outdated client

  > Note (P1.3):
  > - **Extra scenarios.** IP ban, idle timeout, server stop, `docker kill`, a frozen server (`docker pause`), and an older-version server.
  > - **Findings.** The frozen server showed that ticks keep going while the server is dead, so the watchdog also needs a packet-liveness timeout. azalea has no connect timeout. See ADR-0008 §5–6.
- [x] **P1.4** Chat:
  - receiving it, and telling player, system and whisper messages apart
  - getting the sender and the plain text
  - sending chat and `/commands`

  > Note (P1.4): For system messages, azalea guesses the sender by regex, so it can be spoofed. Only `Player`/`Disguised` packets carry a trustworthy sender. See ADR-0008 §7.
- [x] **P1.5** Actions:
  - look / set direction
  - jump
  - sneak
  - swing arm (is there an API for it?)
  - use item
  - attack the entity in view (how do you find it, and how do you check reach?)
  - select a hotbar slot
  - respawn after death

  > Note (P1.5):
  > - **Attack.** `Client::attack` in azalea 0.16.0 sends a mis-encoded packet and gets the bot kicked on 26.1. A raw-packet workaround (`attack_raw`) works.
  > - **Idle timer (extra test).** Rotation alone doesn't reset the server's idle timer. See ADR-0008 §8.
- [x] **P1.6** Inject a panic into a custom ECS system. What happens to that client, its event channel, and other clients on the same host thread? Use the result to confirm the `Tick`-based watchdog design.

  > Note (P1.6):
  > - **Extra tests.** A hang, a Swarm, and pool starvation, run in the Linux container.
  > - **Panics** are detected at once (the runner's `AppExit` receiver), so the Tick watchdog is only needed for hangs.
  > - **Hangs** can starve Bevy's process-wide compute pool and freeze every bot, which is why each App uses the single-threaded executor. See ADR-0008 §3 and §5.
- [x] **P1.7** Resources: compare one world per bot (`Client::join`) against Swarm shards at 10, 25 and 50 bots, measuring RSS, CPU and thread count. Choose a model, weighing in that one panic kills an entire shard.

  > Note (P1.7):
  > - **Method.** Measured in a Linux container (`--cpus 4`), at the user's choice. "One world per bot" uses Variant C instead of `Client::join` (P1.2).
  > - **Models.** The matrix added one host thread per bot (ADR-0004's alternative) and Bevy's single-threaded executor.
  > - **Choice.** One App and one host thread per bot, single-threaded executor: 222 MiB and 0.43 cores at 50 bots. Bevy's default multi-threaded executor needs ~7× the CPU. See ADR-0008 §2.
- [x] **P1.8** Write a custom `AccountTrait` that uses an externally supplied Minecraft access token, UUID and name, and stores the chat-signing certs. Compile-check it. The **user** may test it against a real online-mode server with their own account, running that step themselves. Real tokens never go into code, logs, commits or the AI chat.

  > Note (P1.8):
  > - **Local online-mode server.** At the user's request, the spike adds `spikes/azalea/compose.online.yaml` (online mode, secure profile enforced, `127.0.0.1:25567`), so the real-account test never touches a public server.
  > - **Checked without credentials.** A garbage token and an offline account were tested against it.
  > - **Real-account join.** That's the user's optional step: `fetch-token` (device code through azalea's re-exported auth, no cache file, token written to gitignored `secrets/`), then `account-join`. **Done by the user on 2026-10-06:** the bot spawned on the local online-mode server, its chat message was accepted with secure profiles enforced, and it disconnected cleanly.
  > - **Gotchas.** Auth failures raise no azalea event, `join()` does nothing by default, and azalea's `MicrosoftAccount` derives `Debug` over its token. See ADR-0008 §9.
- [x] **P1.9** Disconnect cleanly and check that no threads or tasks leak and that memory returns afterwards.

  > Note (P1.9):
  > - **Criterion.** At the user's approval, "memory returns" was checked as: RSS **plateaus** across cycles, and threads, fds and Worlds return to the post-warm-up baseline. glibc keeps freed memory, so RSS never drops back to the pre-join value.
  > - **Result.** 20 cycles of 25 bots in Linux, a fresh host thread per bot: threads and fds back to baseline every cycle, every World freed, RSS after teardown flat at ~110 MiB from cycle ~9. No leak.
  > - **Teardown.** `exit()`, drop every handle, close the host thread. `disconnect()` alone keeps the ECS running. See ADR-0008 §10.
- [x] **P1.10** Write **ADR-0008 "azalea integration"** with:
  - the answers
  - the chosen model
  - the gotchas found
  - API snippets that `fleet-mc` can reuse

  Update ADR-0004 to confirm or revise it.

  > Note (P1.10):
  > - **ADR-0008** also lists the open questions as answered or explicitly deferred (§11).
  > - **ADR-0004** is now Accepted, revised to one host thread per bot with the single-threaded executor.
  > - **Archived.** The spike stays in `spikes/azalea/` with an "Archived" banner, at the user's choice.
  > - **Plan changes.** ADR-0008 lists changes to P2.4, P2.7, P2.10, P3.2, P3.4, P3.5 and P4.6. The user accepted them in the Phase 1 review, and they were applied in the same PR, together with P3.1, P3.8, P9.3, the fault table (§6) and the agent config (Appendix A).

**Security:** never commit real account tokens. Use offline mode for everything except P1.8.

**DoD:** ADR-0008 written; open questions answered or explicitly deferred.

---

### Phase 2: Domain core (`fleet-core`)
**Goal:** Every business rule as pure, exhaustively tested Rust, with **no tokio and no IO**.
**Introduces:** `serde`, `thiserror`, `uuid`, `chrono`, `rand`, `backon`, `secrecy`. Dev: `rstest`, `proptest`, `insta`, `serde_json`.

> Note (P2):
> - **Five group branches.** At the user's request, Phase 2 is built in five PRs instead of one per task. Each group has one branch and one commit per task, and the groups run in this order, each after the previous PR is merged:
>
>   | Group | Branch | Tasks |
>   |---|---|---|
>   | A | `p2/p2.1-p2.5-values-and-policies` | P2.1–P2.5 |
>   | B | `p2/p2.6-bot-state-machine` | P2.6 |
>   | C | `p2/p2.7-p2.8-modes-and-scheduler` | P2.7–P2.8 |
>   | D | `p2/p2.9-authorization` | P2.9 |
>   | E | `p2/p2.10-p2.11-ports-errors-docs` | P2.10–P2.11 |
> - **Decisions.** The phase-wide conventions and the user's answers to the Phase 2 plan's open questions are in [ADR-0010](docs/adr/0010-fleet-core-conventions-and-phase-2-refinements.md):
>   - time (`DateTime<Utc>` passed in, `Instant` for liveness)
>   - injected randomness
>   - errors per task
>   - persistence formats
>
>   The notes below summarize the decisions that change a task.
> - **Dependencies.** `serde_json` is an extra dev-dependency, for the mode JSON tests. `rstest` and `proptest` are declared without default features (§5).
> - **DoD.** "No tokio or IO crates" is checked on normal dependencies: `cargo tree -p fleet-core -e normal`. Dev-only crates such as insta's tempfile don't ship.

- [x] **P2.1** 🔴 ID newtypes `UserId`, `AccountId`, `BotId`, `AgentId`, `ModeId`: uuid v7, `#[serde(transparent)]`, `Display`. They can't be confused with each other.

  > Note (P2.1):
  > - **Minting.** IDs are minted from caller data with `new_v7(created_at, [u8; 10])`; the server supplies `getrandom` bytes. uuid's `v7` feature stays off, because it pulls in getrandom.
  > - **Parsing.** Parsing and deserializing accept only v7 UUIDs.
  > - **serde.** It uses `try_from`/`into` `Uuid` instead of `transparent`: the same wire format, but validated (ADR-0010).
- [x] **P2.2** 🔴 Value objects with fallible constructors (`TryFrom<&str>`). Each one gets a test for every rejection case, plus a proptest that it **never panics on arbitrary input**.
  - `ServerAddress`: host or IP, optional port 1–65535 (default 25565), no scheme or path, ≤ 253 chars.
  - `ChatMessage`: 1–256 chars after trimming, no control characters, no `§`.
  - `McUsername`: 3–16 characters from `[A-Za-z0-9_]`.
  - `Username` (app login): 3–32 characters, normalized to lowercase.
  - Command detection: `ChatMessage::command_name()` for allowlist checks.

  > Note (P2.2) (ADR-0010):
  > - **`ChatMessage`** counts the 256 limit in UTF-16 code units, because vanilla checks the Java string length. It also rejects bidi controls.
  > - **`Username`** allows ASCII `[a-z0-9_.-]`, starting with a letter or digit.
  > - **`ServerAddress`:**
  >   - remembers whether a port was given
  >   - rejects non-ASCII hosts, `_`, a trailing dot and userinfo
  >   - requires the last label of a domain to start with a letter, so `0x7f000001` and `127.0.0.0x1` aren't read as IP addresses
  >   - needs brackets around IPv6 when a port follows
- [x] **P2.3** 🔴 `IncomingChat` sanitizer: strip format codes and control characters, cap the length, and keep the kind (player, system or whisper) and the sender. This turns untrusted server text into something safe to store and display.

  > Note (P2.3) (ADR-0010):
  > - **Kinds.** `chat`, `emote`, `whisper`, `announcement` and `system`; the spike saw all five (ADR-0008 §7).
  > - **Sender.** Only non-system kinds have one: a sanitized display name of at most 64 chars, plus an optional UUID.
  > - **Text.** It's capped at 1024 chars, with a `truncated` flag, and `\n` is kept. These are stripped from the text and from sender names:
  >   - other control characters
  >   - `§` pairs
  >   - bidi controls
  >   - invisible characters: the format characters that show no glyph (U+200B–U+200D, U+2060–U+2064, U+FEFF, U+00AD, …), U+2028/U+2029, the Hangul fillers, tags and every variation selector except U+FE0F; the ranges are in `text.rs`
  > - **Storage.** Chat is stored as columns, so there's no serde.
- [x] **P2.4** 🔴 `DisconnectReason` and a classifier that returns `Transient`, `Permanent(kind)`, `Conflict(DuplicateLogin)` or `AuthInvalid`. Table-driven tests use real kick messages and translation keys from the spike (ADR-0008 §6).

  > Note (P2.4): The input also covers these reasons (ADR-0010):
  > - `AuthRejected`, classified as `AuthInvalid`
  > - `SessionCrashed`, `WatchdogTimeout`, `LivenessTimeout` and `ConnectFailed` (including `HostUnavailable`), all transient
- [x] **P2.5** 🔴 Resilience policies:
  - `RetryPolicy` wraps `backon`'s exponential builder: base, factor, cap, jitter, and a reset after a stable period. Tests assert **bounds**, not exact values.
  - `CircuitBreaker` (closed, open, half-open) is pure, with time passed in.
  - Proptests: delay ≤ cap, and the breaker never lets an attempt through while open.

  > Note (P2.5) (ADR-0010):
  > - **Factor.** Fixed at 2; there's no config key for it.
  > - **Jitter.** backon adds its jitter after the cap (`d + d·U[0,1)`), so it's given `max / 2`. Jittered delays then stay ≤ `max`, which requires `max ≥ 2 × base`. The jitter is seeded from the injected RNG.
  > - **`FailureWindow`.** A pure counter for "N failures within a window". It backs the breaker, and later P4.7.
  > - **Breaker.** One per bot, and it only decides how long to wait. Appendix E's "circuit open too long → Failed" arrow is dropped.
- [ ] **P2.6** 🔴 The **bot state machine**: `BotState`, `BotEvent`, `Effect`, and `transition(&state, event, now) -> Transition` (Appendix E). It needs example tests for every transition and proptest invariants:
  - A `Stop` from any state ends in `Stopped`, and emits `Disconnect` if a session exists.
  - `Failed` and `Paused` never emit `Connect` before `Reset` or `Resume`.
  - There is never a `Connect` effect while `Connecting` or `Online`.
  - The attempt counter resets after the stable-online period.

  > Note (P2.6): Decided in the Phase 2 plan and built in group B (ADR-0010):
  > - **Signature.** `transition(&state, event, now, &RetryPolicy)`.
  > - **State fields.** `Online{since, attempt}`, `AwaitingSession{attempt, fresh}`, `Connecting{attempt, auth_retried}`, `Stopping{restart}`.
  > - **New event.** `CrashLoop`.
  > - **Auth.** The state machine owns "request one fresh token, then `Failed(Auth)`".
  > - **Stop.** Without a session, `Stop` goes straight to `Stopped`. The first invariant reads "Stop, then SessionClosed, ends in Stopped".
  > - **Unfitting events** are no-ops.
- [ ] **P2.7** 🔴 The mode model:
  - `Action` enum: `Look{yaw,pitch}`, `RotateRandom{max_yaw,max_pitch}`, `Jump`, `Sneak{on}`, `SwingArm`, `UseItem`, `AttackFacingEntity`, `SelectHotbarSlot(0..=8)`, `SendChat(ChatMessage)`.
  - `Schedule`: `AtStart` or `Every{interval, jitter}`, plus a `probability`.
  - `ModeDefinition` and `validate()`, with these limits:

    | Limit | Value |
    |---|---|
    | Actions per mode | ≤ 32 |
    | Interval | ≥ 250 ms |
    | Attack interval | ≥ 500 ms |
    | Chat interval | ≥ 30 s |
    | Yaw / pitch | Valid ranges |

  - Built-in presets as validated constants:
    - **`afk`**: a random small rotation every 45–120 s, plus a swing, jump or sneak more often than the server's idle timeout. Rotation alone doesn't reset that timer (ADR-0008 §8).
    - **`farm`**: a fixed look direction, a hotbar slot chosen at start, and attack every 0.65–0.8 s.
  - insta snapshots of the serialized presets, so stored modes stay compatible.

  > Note (P2.7): Decided in the Phase 2 plan and built in group C (ADR-0010):
  > - **`afk`:** `RotateRandom` ±30°/±10° every 45–120 s, plus `SwingArm` every 20–40 s.
  > - **`farm`:** hotbar slot 0 at start, plus an attack every 650–800 ms, with no Look.
  > - **JSON:** struct variants (`{"type":"select_hotbar_slot","slot":3}`), and `validate()` is `ModeDraft::validate`.
  > - **Limits:** probability is a percent from 1 to 100; interval and jitter are each ≤ 24 h; at most one `AtStart` chat step.
  > - **Commands:** `commands()` feeds the authorization check.
- [ ] **P2.8** 🔴 `ModePlan`: a pure scheduler that takes a definition, an RNG and the current time and returns `(next_due, Vec<Action>)`. The runtime then only sleeps and executes. Tests use a seeded RNG.

  > Note (P2.8) (ADR-0010):
  > - **Return value.** `PlanTick { actions: Vec<PlannedAction>, next_due: Option<…> }`. `RotateRandom` is resolved to a relative `Turn`, and chat is kept separate for the P4.5 queue.
  > - **Timing.** Gaps are uniform in [interval, interval + jitter], the first run comes one gap after the start, and there's no catch-up.
- [ ] **P2.9** 🔴 Authorization: `Role`, `GrantLevel`, `Permission`, `Actor`, `ResourceContext`, `authorize()`.
  - An **exhaustive matrix test** covers every role × grant × permission. It's generated and snapshotted with insta, so every change shows up in review.
  - Edge cases: an Admin acting on the Owner or another Admin, granting above your own level, deny by default.

  > Note (P2.9) (ADR-0010):
  > - **Admins.** An Admin's implicit Manage covers Members' accounts and the Admin's own, but not the Owner's or other Admins'.
  > - **Commands.** `CommandAllowlist` is a core type. Changing to a mode that contains a non-allowlisted command needs Manage.
  > - **Built-in modes.** `ResourceContext::Mode{owner: None}` is a built-in mode.
- [ ] **P2.10** 🔴 Minecraft ports, traits only:
  - `MinecraftConnector::connect(ConnectParams) -> (SessionHandle, SessionEvents)`
  - `SessionHandle` (clone, perform `Action`, send chat, disconnect, read liveness)
  - `SessionEvents::next()`
  - `SessionEvent` (`Joined`, `Chat`, `Died`, `Disconnected`, `ConnectionFailed`)
  - liveness: when the session last saw a `Tick` and last received a packet from the server. The session updates both in place instead of queueing events (ADR-0008 §4–5).
  - `SessionCredentials`, which holds the token as a `SecretString`

  Use RPITIT with `+ Send` futures.

  > Note (P2.10) (ADR-0010):
  > - **`connect`** returns a `Result`.
  > - **`SessionHandle`** gains `respawn()`. `perform` takes a `GameAction`, and chat goes through `send_chat`.
  > - **`disconnect()`** is the full ADR-0008 §10 teardown.
  > - **Liveness** stamps are `std::time::Instant`, set when the session is created, so they're never empty.
  > - **`SessionCredentials`** is `Offline` or `Online`.
- [ ] **P2.11** A `thiserror` error enum per module, and crate- and module-level docs.

  > Note (P2.11): Error enums are built in each task, because each task tests its error paths first. P2.11 audits them and writes the crate docs (ADR-0010).

**Security:**
- Every constructor rejects oversized input and control characters.
- `authorize` denies by default.
- Secrets are `SecretString` with redacted `Debug`.

**DoD:**
- `fleet-core` coverage ≥ 90 %.
- `cargo tree -p fleet-core` shows no tokio or IO crates.
- Proptests run ≥ 1,000 cases in CI.

---

### Phase 3: Minecraft adapter (`fleet-mc`) & test kit (`fleet-testkit`)
**Goal:** A tested azalea implementation of the Minecraft ports, plus a scriptable fake for every layer above it.
**Introduces:** `azalea`, `tokio`, `tokio-util`, `tracing`. Dev: `testcontainers`.

- [ ] **P3.1** 🔴 `fleet-testkit`: `FakeConnector` and `FakeSession`. They need to support:
  - scripted connect results
  - injected events (`Joined`, `Chat`, `Died`, `Disconnected(reason)`)
  - liveness timestamps that a test can advance or freeze
  - a log of performed actions and sent chat
  - "hang" (no more ticks) and "fail on action" modes

  Write tests for the fake itself.

  > Note (P3.1, from Phase 2): Liveness stamps are `std::time::Instant`. The fake stamps them with `tokio::time::Instant::now().into_std()`, so paused time works. Both stamps are set when the session is created (ADR-0010).
- [ ] **P3.2** 🔴 `McHostPool` spawns one host thread per session (ADR-0008 §2):
  - Each thread has a current-thread runtime and a `LocalSet`, and ends when its session ends.
  - Work reaches the thread over a bounded queue.
  - A hung thread is **abandoned** and counted, never joined or respawned. Test this with an injected job that never returns.
- [ ] **P3.3** Account adapter: a custom `AccountTrait` for server-issued `SessionCredentials`, and offline accounts for dev and tests.
- [ ] **P3.4** `AzaleaConnector` implements `MinecraftConnector`:
  - azalea's auto-reconnect and auto-respawn are **disabled**
  - connect timeout
  - start, hosting model and executor as decided in ADR-0008 §1–3

  > Note (P3.4, from Phase 2): open questions, flagged and not yet decided:
  > - Appendix A has no config key for the connect timeout that `ConnectParams` carries.
  > - The account's `refresh()` fails fast, because the core state machine now requests the fresh token (ADR-0010).
- [ ] **P3.5** 🔴 Map azalea events to `SessionEvent`, with unit tests on the pure mapping functions. Chat goes through the core sanitizer, with the sender taken only from where ADR-0008 §7 allows; kick reasons go through the core classifier input. `Tick` and server events such as `KeepAlive` only update the session's liveness timestamps (ADR-0008 §4–5).
- [ ] **P3.6** 🔴 Map each `Action` to azalea calls: look, rotate, jump, sneak, swing, use item, attack facing entity (with a reach check), hotbar, respawn, chat.
- [ ] **P3.7** 🔴 Slow integration tests (`slow_*`, testcontainers + itzg, offline mode):
  - join and see the join message
  - send chat and see it echoed back
  - perform every action without an error
  - get kicked by RCON (`docker exec … rcon-cli kick`) and receive `Disconnected` with a reason
  - reconnect
- [ ] **P3.8** 🔴 Clean-up test: after the full teardown from ADR-0008 §10 (not just `disconnect()`), the thread count and the number of live Worlds go back to baseline.

**Security:**
- Credentials are never logged.
- Every connect has a timeout.
- azalea faults stay contained in their host thread.

**DoD:**
- The slow suite passes against the pinned Minecraft version.
- `cargo tree -i azalea` shows `fleet-mc` as the only dependent.

---

### Phase 4: Bot runtime (`fleet-runtime`)
**Goal:** Self-healing bots, one supervised actor each, with modes and a watchdog. All of it is proven with fakes and paused time.
**Introduces:** `governor`, `metrics`. Dev: `fleet-testkit`.

- [ ] **P4.1** 🔴 `BotSpec` (account, server, mode, desired run state) and `BotSnapshot` (state, since, last disconnect reason, attempt, uptime).

  > Note (P4.1, from Phase 2): open question, flagged and not yet decided: fleet-proto and fleet-server also need to build a `BotSpec` (Appendix C `AssignBot`), but they may only depend on `fleet-core`.
- [ ] **P4.2** 🔴 `BotActor`:
  - `tokio::select!` over the inbox (`Start`, `Stop`, `UpdateSpec`, `SendChat`, `Reset`, `Resume`), session events, timers and cancellation.
  - It executes the core `Effect`s and computes retry delays with `RetryPolicy`.
  - It publishes a `watch` snapshot and `broadcast` events.

  Tests use `start_paused` and the fake. Scenarios:
  - the happy path
  - a transient kick, then backoff, then reconnect
  - a permanent kick ends in `Failed`
  - a duplicate login ends in `Paused`
  - `Stop` during backoff
  - changing the server reconnects
  - changing the mode does **not** reconnect

  > Note (P4.2, from Phase 2) (ADR-0010):
  > - **Clock.** The actor's clock derives `DateTime<Utc>` from tokio's `Instant`, anchored at startup, so paused time drives `transition`, the breaker and `ModePlan`.
  > - **Leaving a state** cancels that state's timer or session request.
  > - **`SessionClosed`** is sent once teardown has finished or timed out.
  > - **Restart** and a server change go through `Stopping{restart}`.
- [ ] **P4.3** 🔴 `SessionCredentialProvider` port:
  - In standalone mode it returns offline credentials.
  - In managed mode it asks the control plane (Phase 10).
  - On an expired token it refreshes once, then goes to `Failed(Auth)`.

  > Note (P4.3, from Phase 2): The "refresh once" is driven by the core state machine. The first `AuthInvalid` emits `RequestSession{fresh: true}`, and the provider must bypass any token cache for it. The second goes to `Failed(Auth)` (ADR-0010).
- [ ] **P4.4** 🔴 `ModeRunner` drives the core `ModePlan` through the `SessionHandle`.
  - A failed action is logged and skipped, never fatal.
  - It stops cleanly on disconnect or mode change.
  - Tests assert the timing on a paused clock.
- [ ] **P4.5** 🔴 Outbound chat queue:
  - bounded at 16, with one governor token bucket per bot
  - when the bucket or queue is full it returns `RateLimited` or `QueueFull` **instead of blocking**
  - user chat and mode chat share it
  - governor's clock is injected, so the tests run on controlled time (see §8)
- [ ] **P4.6** 🔴 Watchdog, while Online (fault table in ADR-0008 §5):
  - no `Tick` for `watchdog_timeout`: raise `WatchdogTimeout`, tear the session down and reconnect
  - no packet from the server for `packet_liveness_timeout`: tear the session down and treat it as a transient disconnect

  Both timeouts come from `[runtime]` in the agent config (Appendix A).
- [ ] **P4.7** 🔴 `Supervisor` and `Fleet` handle:
  - actors run in a `JoinSet` or `TaskTracker`
  - panics are detected and the actor restarted, within the intensity limit
  - API: `apply(spec)`, `remove(id)`, `send_chat`, `snapshot_all`, `subscribe`
  - graceful shutdown: cancel, then disconnect every bot within the timeout

  > Note (P4.7, from Phase 2):
  > - **Restart limit.** "5 restarts in 10 min" uses the core `FailureWindow` and ends with the core `CrashLoop` event (ADR-0010).
  > - **Open question, flagged and not yet decided.** `Paused` (a human is playing) and `Failed` must survive an actor or agent restart. Otherwise a fresh `Start` kicks the human (§6 row 3).
- [ ] **P4.8** 🔴 **Chaos property test.** Random sequences of these events, run with paused time:
  - transient and permanent kicks
  - connection failures
  - hangs
  - actor panics
  - spec updates

  Invariants:
  - No panic escapes.
  - Bots with desired = Running that only see transient faults end up Online.
  - Connect attempts stay inside the policy bounds (no reconnect storms).
  - The `Fleet` API always responds.
- [ ] **P4.9** Metrics: bots per state, reconnects, watchdog trips, actor restarts.

**Security:**
- Every channel is bounded.
- Chat only accepts a validated `ChatMessage`.
- No `unwrap` anywhere.

**DoD:**
- Coverage ≥ 85 %.
- The chaos test passes 500 cases.
- `just test fleet-runtime` takes under 30 s.

---

### Phase 5: Standalone agent (`fleet-agent`), the first runnable product
**Goal:** `afkfleet-agent run --config agent.toml` runs a few dev bots (offline mode) against a local server. It survives server restarts and shuts down cleanly.
**Introduces:** `clap`, `figment`, `garde`, `tracing-subscriber`, `anyhow`, `metrics-exporter-prometheus`.

- [ ] **P5.1** 🔴 Config (Appendix A):
  - loaded with figment from TOML plus `AFKFLEET_AGENT__…` env variables
  - validated with garde, with clear error messages
  - `[standalone]` and `[control_plane]` are mutually exclusive
  - standalone mode only allows **offline** accounts
- [ ] **P5.2** Telemetry: pretty logs in dev and JSON in prod, an env filter, and a panic hook that logs through `tracing`.
- [ ] **P5.3** Wiring: `McHostPool` + `AzaleaConnector` + `Fleet`, with the standalone spec source.
- [ ] **P5.4** 🔴 Signals (Ctrl+C, SIGTERM) trigger a graceful shutdown within `shutdown_timeout`.
- [ ] **P5.5** 🔴 A `healthcheck` subcommand. The agent touches a heartbeat file every 10 s, and the check fails when the file is stale. This works in distroless images, which have no curl.
- [ ] **P5.6** `deploy/docker/agent.Dockerfile`:
  - cargo-chef
  - a nightly builder
  - runtime `gcr.io/distroless/cc-debian12:nonroot`

  Also `deploy/compose.dev.yaml` with an itzg server plus the agent.
- [ ] **P5.7** 🔴 Slow end-to-end test:
  1. `compose up`, and all bots come Online.
  2. Restart the MC container; the bots reconnect within the policy.
  3. Stop the agent; the disconnect is clean.

**DoD:** Demo with 5 bots AFK on a local server for 1 h, with one server restart in between. The logs show no errors except the expected disconnect warnings.

---

### Phase 6: Server foundation (`fleet-server`, `fleet-api-types`)
**Goal:** An HTTP service skeleton that is secure by default, with persistence, an error model and an audit trail, but no business endpoints yet.
**Introduces:** `axum`, `axum-extra`, `tower`, `tower-http`, `sqlx`, `utoipa`, `utoipa-axum`, `ts-rs`, `serde_json`, `tokio-util`.

- [ ] **P6.1** Module layout (see `CLAUDE.md`): `config`, `app` (services), `ports`, `infra/{sqlite,crypto}`, `http/{router,middleware,extractors,handlers,error}`, `grpc`, `cli`.
- [ ] **P6.2** 🔴 Config:
  - figment + garde
  - secrets only via `*_file` paths
  - fail fast with a clear message
- [ ] **P6.3** SQLite setup:
  - `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5s`
  - a **write pool with 1 connection** plus a read pool
  - migrations embedded with `sqlx::migrate!` and run at startup
  - offline data in `.sqlx/`, and a CI job running `cargo sqlx prepare --check`
- [ ] **P6.4** 🔴 First migration and repositories for `users` and `audit_log`, tested with `#[sqlx::test]`.
- [ ] **P6.5** 🔴 `ApiError` maps each error to a status code and `{ "error": { "code", "message", "request_id", "fields"? } }`.
  - Internal errors are logged with the request ID and returned as a generic 500.
  - Every variant gets an insta snapshot.
- [ ] **P6.6** 🔴 Middleware stack, in this documented order:
  1. request-id (set and propagate)
  2. trace (no bodies, sensitive headers redacted)
  3. `CatchPanicLayer`, which turns a panic into a 500
  4. timeout (15 s)
  5. body limit (64 KiB)
  6. security headers

  Rate limiting is added in P7.
- [ ] **P6.7** 🔴 `GET /health/live` and `GET /health/ready` (DB ping), with no internal details in the response.
- [ ] **P6.8** 🔴 Audit service:
  - append-only: the trait has only `record` and `list`
  - each entry: actor, IP, action, target, outcome, metadata (no secrets), timestamp
- [ ] **P6.9** `fleet-api-types`:
  - DTOs with serde, garde and `ts-rs`
  - `#[serde(deny_unknown_fields)]` on request DTOs
  - `just gen` exports them to `packages/ui/src/generated/`
  - CI job `ts-types-fresh` fails on any diff
- [ ] **P6.10** OpenAPI via utoipa. `/api/openapi.json` is served only when `dev_mode = true`.
- [ ] **P6.11** CLI skeleton: `serve`, `migrate`, `healthcheck`.
- [ ] **P6.12** 🔴 Graceful shutdown: axum's `with_graceful_shutdown` plus a `CancellationToken`.

**Security:**
- No CORS layer.
- `Cache-Control: no-store` on API responses.
- No secrets in committed TOML.
- The DB file is only readable by the service user.

**DoD:**
- An integration test boots the app in-process on a temporary DB and passes the health checks.
- Snapshots reviewed.
- `just ci` green.

---

### Phase 7: Authentication & authorization
**Goal:** Invite-only login with 2FA, rotating tokens, central authorization and layered rate limits, all backed by adversarial tests.
**Introduces:** `argon2`, `getrandom`, `sha2`, `subtle`, `base64`, `zeroize`, `chacha20poly1305`, `totp-rs`, `zxcvbn`, `governor`, `tower_governor`, `rpassword`.

- [ ] **P7.1** 🔴 Migrations and repositories: `sessions` (with token families), `invites`, `login_attempts`, `user_mfa`.
- [ ] **P7.2** 🔴 `PasswordService`:
  - Argon2id with the configured parameters
  - runs in `spawn_blocking` behind a semaphore
  - dummy hash for unknown users
  - password policy (length, zxcvbn)
- [ ] **P7.3** 🔴 `TokenService`:
  - tokens from `getrandom`, with prefixes
  - SHA-256 hashing and constant-time comparison (`subtle`)
  - TTLs
  - refresh rotation, and **family revocation when an old token is reused**
- [ ] **P7.4** 🔴 Vault cipher: the envelope from §7.5 with `key_id` and AAD. TOTP secrets need it now; Phase 9 extends it. Tests cover tampering, the wrong key and the wrong AAD.
- [ ] **P7.5** 🔴 CLI `user create-owner`: hidden prompt with `rpassword`; refuses when an Owner already exists.
- [ ] **P7.6** 🔴 Endpoints:
  - `POST /auth/login`, `/auth/login/mfa`, `/auth/refresh`, `/auth/logout`, `/auth/register` (invite)
  - `GET /me`, `POST /me/password` (step-up)
  - `GET /me/sessions`, `DELETE /me/sessions/{id}`
- [ ] **P7.7** 🔴 2FA:
  - enroll returns the otpauth URI and a QR code as PNG data
  - confirm, then reset (with step-up), which forces a fresh enrollment at the next login
  - recovery codes
  - replay protection
  - mandatory for **every** user, not configurable: until they enroll, login returns `mfa_setup_required` plus a single-use setup token that only works for enroll and confirm (§7.2). That includes users who just registered with an invite.
  - tests: the setup token is rejected on every other endpoint, expires after 10 min and can't be reused
- [ ] **P7.8** 🔴 The `AuthUser` extractor (Bearer token via `axum-extra`) loads session and user and rejects disabled users. A `require(permission, resource)` helper calls `fleet_core::authz::authorize`.
- [ ] **P7.9** 🔴 Admin endpoints:
  - `GET /users`
  - `PATCH /users/{id}` (role, disabled). Changing either revokes all of that user's sessions.
  - `POST`, `GET` and `DELETE` on `/invites`
- [ ] **P7.10** 🔴 Rate limiting:
  - tower_governor per IP, globally and on auth routes, via `into_make_service_with_connect_info`
  - `trusted_proxies` CIDRs decide when `X-Forwarded-For` is honored
  - a periodic `retain_recent()` clean-up
  - governor limits per user
  - a per-username lockout stored in the DB
  - `429` with `Retry-After`
- [ ] **P7.11** 🔴 **Route-coverage test.**
  - Routes are registered only through `public_route(…)` or `authed_route(…)` helpers, which also record them in a registry.
  - The test calls every authed route without credentials and expects `401`.
  - It also checks that the public list is exactly the expected allowlist.
- [ ] **P7.12** 🔴 Security tests:
  - **User enumeration:** an unknown user and a wrong password give the same status and body.
  - **Lockout.**
  - **Refresh reuse:** reusing a refresh token revokes the family.
  - **Expired or disabled sessions.**
  - **Escalation:** a Member tries to become Admin, an Admin tries to change the Owner.
  - **Invites:** reuse and expiry.
  - **TOTP replay.**
- [ ] **P7.13** Audit events for:
  - login success and failure, logout
  - password and 2FA changes
  - role changes
  - invites created and used
  - token reuse

**DoD:**
- auth + vault coverage ≥ 90 %.
- Every security test is green.
- The threat model's auth section is updated.

---

### Phase 8: Desktop app foundation (Tauri + React)
**Goal:** A signed-in, role-aware desktop app that talks to the real server through a Rust-side API proxy. This is the first vertical slice that runs end to end.
**Introduces:**
- **Rust:** `tauri` (≥ 2.11.1), `tauri-plugin-single-instance`, `keyring-core` + `windows-native-keyring-store`, `reqwest`, `rustls`.
- **npm:** react, vite, typescript, TanStack Query & Router, zustand, zod, react-hook-form, tailwindcss, shadcn/ui, lucide-react, sonner, vitest, Testing Library, msw, `@tauri-apps/api`, biome.

- [ ] **P8.1** 🔴 `fleet-client`:
  - typed async methods for every endpoint (reqwest + rustls, timeouts)
  - one automatic, single-flight refresh on `401`
  - typed errors
  - integration tests against the in-process server
- [ ] **P8.2** Scaffold:
  - `apps/desktop` (Tauri v2; `src-tauri` is a workspace member)
  - `packages/ui` (Vite + React + strict TypeScript + Tailwind + shadcn/ui)
  - pnpm scripts for both
- [ ] **P8.3** Tauri hardening:
  - CSP: `default-src 'self'; img-src 'self' data:; style-src 'self'; connect-src ipc: http://ipc.localhost`. Relax it only with an ADR.
  - Capabilities allow only our commands.
  - `withGlobalTauri: false`.
  - Devtools only in debug builds.
  - The single-instance plugin.
- [ ] **P8.4** 🔴 Rust side:
  - `AppState` holds the server profile, the `fleet-client` and the session.
  - The **refresh token lives in the OS keychain**; the access token only in memory.
  - Typed commands are grouped per feature (`commands/auth.rs` …) and return `Result<T, CommandError { code, message }>`.
  - Server profile: a URL plus an optional custom CA PEM, validated. **Never accept invalid certificates.**
- [ ] **P8.5** 🔴 TypeScript side:
  - The `ApiClient` interface lives in `packages/ui/src/api/`.
  - `TauriApiClient` lives in `apps/desktop/src/` and is injected into `<App apiClient={…} />`.
  - `FakeApiClient` is for tests.
  - DTO types come only from `src/generated`.
- [ ] **P8.6** 🔴 App shell:
  - TanStack Router with an auth guard
  - role-aware navigation
  - an error boundary
  - toasts
  - a banner when the server is unreachable
- [ ] **P8.7** 🔴 Screens:
  - server setup
  - login, including the 2FA step
  - register with an invite
  - 2FA enrollment (QR code from the server), shown right after the first login, since 2FA is mandatory
  - profile: change password, list and revoke sessions
- [ ] **P8.8** 🔴 Admin screens:
  - users: change role, disable
  - invites: the code is shown **once**, with a copy button
- [ ] **P8.9** Playwright against the Vite web build with MSW, for login, 2FA and invite registration.

**Security:**
- Tokens never reach JavaScript.
- No `dangerouslySetInnerHTML`.
- Inputs are validated with zod before `invoke`, and the server validates them again.

**DoD:**
- A friend can install a dev build, register with an invite and log in with 2FA, and an Admin can manage users.
- Vitest and Playwright are green.

---

### Phase 9: Credential vault & Microsoft accounts
**Goal:** Users link their Microsoft accounts securely. Tokens are encrypted at rest and refreshed safely.
**Introduces:** `azalea-auth`, `moka`, `tauri-plugin-opener`.

- [ ] **P9.1** 🔴 Migrations and repositories:
  - `mc_accounts`: owner, MC UUID and name, kind, encrypted refresh token, `key_id`, status
  - `account_grants`
- [ ] **P9.2** 🔴 Vault extensions:
  - purpose-bound AAD (`account_id|ms_refresh`)
  - `vault rotate-key` CLI that re-encrypts transactionally
  - lookup by `key_id`
  - a test that a ciphertext swapped between two accounts fails to decrypt
- [ ] **P9.3** 🔴 `MicrosoftAuthProvider` port (`start_device_flow`, `poll`, `refresh`, `minecraft_session`), with an `azalea-auth` adapter and a fake.
  - Use azalea-auth's default client ID; a custom Azure app ID would need approval from Mojang.
  - **Never** use azalea's file cache.
  - At `trace`, azalea-auth logs Microsoft access and refresh tokens. `fleet-server` caps the `azalea_auth` log level at `info`, whatever the configured filter says.
- [ ] **P9.4** 🔴 Device-code flow:
  - `POST /accounts/link` returns `{flow_id, user_code, verification_uri, expires_at}`.
  - A background poller runs bounded and cancellable, with per-user and total caps.
  - `GET /accounts/link/{flow_id}` returns the status.
  - On success, check the profile and game ownership, store the encrypted token, and give the creator Manage.
  - Member quota from `accounts.max_per_member`.
- [ ] **P9.5** 🔴 `SessionTokenService`:
  - single-flight refresh per account (moka)
  - stores rotated refresh tokens atomically
  - caches the MC session token until shortly before it expires
  - on `invalid_grant`, marks the account `ReauthRequired`
- [ ] **P9.6** 🔴 Account endpoints:
  - list (only what the user may View)
  - get
  - delete (step-up + Manage)
  - grants CRUD (never above your own level)
  - re-link
- [ ] **P9.7** Offline accounts only when `accounts.allow_offline = true`. It's off by default and meant for dev.
- [ ] **P9.8** 🔴 App screens:
  - accounts list
  - **"Link Microsoft account" dialog**:
    - shows the code with a copy button
    - opens the verification URL through a Rust command that uses `tauri-plugin-opener`
    - shows the live status
  - sharing dialog (grants)
  - re-auth prompt

**Security:**
- Refresh tokens never leave the server.
- A test checks that logs are redacted.
- AAD-binding test.
- Per-user caps on link flows.

**DoD:**
- A real Microsoft account is linked end to end.
- The DB contains only ciphertext.
- `rotate-key` works on a copy of the DB.

---

### Phase 10: Control plane (agent ⇄ server, gRPC + mTLS)
**Goal:** Agents enroll securely, receive the desired state and report the actual state, and everything reconciles after any disconnect.
**Introduces:** `tonic`, `tonic-prost`, `prost`, `tonic-prost-build`, `protox`, `rcgen`, `tokio-stream`, `backon` (IO retries).

- [ ] **P10.1** 🔴 `fleet-proto`:
  - `proto/fleet/v1/*.proto` (Appendix C)
  - `build.rs` using protox and `tonic-prost-build`
  - core ⇄ proto conversions with round-trip proptests that reject invalid proto values
- [ ] **P10.2** 🔴 PKI:
  - `afkfleet-server pki init` creates the CA and the server certificate. The CA key is its own secret.
  - CSR signing with a fixed lifetime (default 60 days), with the agent ID in the SAN.
  - Fingerprints are stored in `agent_certs`.
- [ ] **P10.3** 🔴 Enrollment:
  1. An Admin calls `POST /agents/enrollment-tokens` and gets a single-use token, valid 15 min, stored hashed.
  2. `afkfleet-agent enroll` creates a key and a CSR (rcgen).
  3. It calls `AgentEnrollment.Enroll`. This RPC uses TLS without a client certificate and is rate-limited; the agent pins the CA fingerprint from the enrollment bundle.
  4. The agent stores its cert and key, readable only by itself.
- [ ] **P10.4** 🔴 `AgentControl.Connect` bidi stream. It requires:
  - a verified client certificate
  - an allowlisted fingerprint and an enabled agent
  - a version handshake: `Hello` gets either `Welcome` or a rejection
  - heartbeats with a timeout
  - message size limits
- [ ] **P10.5** 🔴 Server components:
  - `AgentRegistry`
  - `Scheduler`: in v1, the first agent with free capacity, labels optional
  - `Reconciler`: compares desired with reported state and sends `Assign`, `Unassign` or `UpdateSpec`
  - stale agent detection, which marks its bots `Unknown`
- [ ] **P10.6** 🔴 Session grants: `SessionRequest{bot_id}` returns `SessionGrant`, but only if the bot is assigned to *this* agent; otherwise `SessionDenied`. On the agent this backs `SessionCredentialProvider`.
- [ ] **P10.7** 🔴 Agent client:
  - reconnects with backon
  - keeps bots running during outages
  - sends its full state on reconnect
  - forwards events through a bounded buffer that drops the oldest chat when full and **never blocks the bots**
- [ ] **P10.8** 🔴 Server ingest:
  - status and chat are written in batches, in transactions roughly every 500 ms
  - **no per-tick writes**
  - events are published on the internal bus (`broadcast`)
- [ ] **P10.9** 🔴 Loopback tests with a test PKI. Each of these is rejected:
  - a revoked certificate
  - a certificate from the wrong CA
  - an agent that never enrolled

  And these must hold:
  - **Connection killed mid-stream:** the state reconciles.
  - **Agent restart:** no duplicate bots.
  - **Server restart:** agents resync.
- [ ] **P10.10** Admin endpoints: `GET /agents`, and `POST /agents/{id}/disable`, which revokes the fingerprint and drops the stream.

**Security:**
- A client certificate is required for `Connect`.
- An agent only gets session tokens for its own bots.
- Enrollment tokens are hashed and single-use.
- The gRPC port is published only when remote agents exist.

**DoD:** In managed mode, pressing Start in the app brings a bot with a real Microsoft account Online on a real server.

---

### Phase 11: Bot operations, modes & live events
**Goal:** Full day-to-day management from the app: start and stop, modes, live chat and history.
**Introduces:** `tokio-tungstenite`, `futures-util` (WebSocket client).

- [ ] **P11.1** 🔴 Migrations and repositories:
  - `bots`: 1:1 with an account; server address, mode, desired state, auto-start, assigned agent
  - `modes`: owner, visibility, definition JSON (validated by core), version
  - `chat_messages`
- [ ] **P11.2** 🔴 Bot endpoints:
  - `GET /bots`, `GET /bots/{id}`
  - `PATCH /bots/{id}` (server, mode, auto-start)
  - `POST /bots/{id}/start|stop|restart|reset|resume`

  Every call is authorized, audited and reconciled.
- [ ] **P11.3** 🔴 Chat:
  - `POST /bots/{id}/chat`:
    - takes a `ChatMessage`
    - `/commands` need Manage unless they're on the allowlist
    - rate-limited per bot and per user
    - audited
  - `GET /bots/{id}/chat?before=&limit=` with cursor pagination.
- [ ] **P11.4** 🔴 Modes CRUD: `GET`, `POST`, `PUT`, `DELETE` on `/modes`. Core validation errors become field errors. Built-in modes are read-only.

  > Note (P11.4, from Phase 2): Editing a mode that's assigned to bots must re-run the command check for every one of them. A mode with a command outside the allowlist needs Manage on each bot (ADR-0010).
- [ ] **P11.5** 🔴 Live events:
  1. `POST /events/ticket` returns a single-use ticket valid for 30 s (moka).
  2. The client opens a WebSocket at `GET /events?ticket=…`.
  3. Events are **filtered per user at send time**.
  4. Limits: per-user connection cap, `Resync` on `Lagged`, ping/pong with an idle timeout, maximum frame size.
- [ ] **P11.6** 🔴 Retention job: chat older than `chat.retention_days` is deleted. The audit log is kept longer (configurable).
- [ ] **P11.7** 🔴 Full-flow integration test: in-process server + in-process agent with `FakeConnector`.
  1. Start a bot via `fleet-client`.
  2. A WebSocket client sees `Online`.
  3. Chat flows in and out.
  4. Stop the bot.
- [ ] **P11.8** 🔴 Tauri: the WebSocket lives in Rust (`fleet-client` + reconnect) and is forwarded to the UI over a Tauri `Channel`. The UI patches the TanStack Query cache from the events.
- [ ] **P11.9** 🔴 UI:
  - **Dashboard:** a card per bot with a state badge, uptime, server, mode and last disconnect reason.
  - **Bot detail:**
    - controls and a mode selector
    - **live chat console** that renders plain text only
    - a send box that shows rate-limit feedback
    - an event timeline
  - **Mode editor:** an action list with validation.
  - **Admin:** an audit-log viewer and an agents view.

**DoD:** The user's real fleet runs for 24 h, managed from the app. Every event shows up live, and the logs contain no unhandled errors.

---

### Phase 12: Deployment & operations
**Goal:** A reproducible, hardened production deployment and a desktop app that can be distributed.
**Introduces:** `tauri-plugin-updater`. Tools: cargo-chef, Docker Compose, Caddy.

- [ ] **P12.1** Dockerfiles for the server and the agent:
  - multi-stage with cargo-chef
  - pinned nightly builder
  - distroless non-root runtime
  - `HEALTHCHECK` through the `healthcheck` subcommand
- [ ] **P12.2** `deploy/compose.yaml`:
  - **Caddy:** ports 80/443, Let's Encrypt, HSTS; proxies `/api` and WebSockets to `server:8080`.
  - **Server:** on the internal network. Port 7443 is published only when remote agents exist.
  - **Agents.**
  - **Docker secrets:** vault key, CA key/cert.
  - **Volumes.**
  - **Hardening:**
    - `read_only` + `tmpfs`
    - `cap_drop: [ALL]`
    - `security_opt: [no-new-privileges:true]`
    - CPU and memory limits
    - `restart: unless-stopped`
- [ ] **P12.3** 🔴 Backups:
  - a scheduled `VACUUM INTO` with retention, plus a `backup` CLI
  - a restore runbook
  - Backups contain only ciphertext. **Back up the vault key separately and securely.**
- [ ] **P12.4** A Prometheus metrics endpoint on an internal port, with the key metrics documented.
- [ ] **P12.5** Tauri release:
  - NSIS installer.
  - Updater key pair; the private key is kept **offline**.
  - An update manifest, e.g. hosted on GitHub Releases.
  - Optional Authenticode signing, which avoids SmartScreen warnings.
  - CI builds the installer on a Windows runner.
- [ ] **P12.6** `docs/runbook.md`:
  - install, configure, bootstrap the Owner
  - enroll an agent
  - rotate keys, renew certificates
  - **Minecraft version upgrade** (azalea + nightly + test-server bump)
  - restore a backup
  - incident response: revoke sessions, disable users and agents
- [ ] **P12.7** 🔴 Compose smoke test in CI, run nightly:
  1. `up`
  2. health checks pass
  3. create the Owner
  4. log in through `fleet-client`
  5. `down`

**DoD:** A fresh VPS gets to a running system by following only the runbook.

---

### Phase 13: Hardening
**Goal:** Prove the security and resilience claims.
**Introduces:** cargo-fuzz (`libfuzzer-sys`, `arbitrary`), cargo-mutants.

- [ ] **P13.1** 🔴 **Authorization matrix test:** every endpoint × {anonymous, Member without grant, View, Control, Manage, Admin, Owner} → the expected status. The table is generated and snapshotted.
- [ ] **P13.2** Fuzz targets:
  - value-object parsers
  - the chat sanitizer
  - mode definition deserialization + validation
  - proto → core conversions
  - the vault envelope parser
- [ ] **P13.3** cargo-mutants on `fleet-core` and auth/vault. Add tests until no relevant mutants survive.
- [ ] **P13.4** Load test: 200+ fake bots on one agent and 20 WebSocket clients. Measure CPU, RAM and event latency, then fix the hot spots.
- [ ] **P13.5** Chaos tests:
  - kill and restart the server, agent and MC containers
  - partition the network between agent and server
  - fill up the DB volume

  Check that everything recovers and nothing is corrupted.
- [ ] **P13.6** 🔴 Agent certificates renew automatically at 2/3 of their lifetime (`RenewCertificate` RPC). Warn when expiry gets close.
- [ ] **P13.7** Reviews:
  - threat model
  - dependencies (`cargo deny`, `cargo tree -d`, `pnpm audit`)
  - a secret scan of the repo history
- [ ] **P13.8** Self-review against the relevant parts of OWASP ASVS Level 2.

**DoD:** Every finding is either fixed or explicitly accepted in an ADR.

---

### Phase 14: Website (future idea, *not planned*)
The architecture is already prepared for it:
- `packages/ui` is platform-agnostic.
- All calls go through `ApiClient`.
- The server needs no CORS when the site is served from the same origin.

What would be left to do:
- An `HttpApiClient` built on `fetch`, with the build served by Caddy on the same origin.
- Sessions:
  - refresh token in an `HttpOnly; Secure; SameSite=Strict` cookie
  - access token in memory
  - CSRF protection (a custom header check or double-submit) for requests authenticated by cookie
  - cookies via `axum-extra`
- A web CSP, extra bot protection on the public login, and the existing Playwright suite reused (it already runs against the web build).

---

## 10. Future extension ideas
These are not scheduled. Each one needs an ADR before it starts.
- Chat triggers (pattern → response or action) as part of modes, with strict limits.
- Notifications on `Failed`, `Paused` or re-auth required (Discord webhook, desktop notification).
- Several server profiles in the desktop app.
- A process-per-bot connector for stronger isolation.
- A PostgreSQL repository implementation for larger installations.
- Scheduled modes (time windows), bot groups and bulk actions.
- Auto-eat and health awareness. This needs inventory knowledge, which is currently out of scope.

---

## 11. Appendix

### A. Example configuration
Secrets **never** go in these files. They're referenced through `*_file` paths (Docker secrets). Environment variables override any key: `AFKFLEET_SERVER__AUTH__ACCESS_TOKEN_TTL_SECS=600`, `AFKFLEET_AGENT__RUNTIME__MAX_BOTS=20`.

`server.toml`
```toml
dev_mode = false

[http]
bind = "0.0.0.0:8080"
trusted_proxies = ["172.30.0.0/24"]     # Caddy network; X-Forwarded-For is only trusted from here
request_timeout_secs = 15
max_body_bytes = 65536

[grpc]
bind = "0.0.0.0:7443"
ca_cert_file = "/run/secrets/fleet_ca_cert"
ca_key_file = "/run/secrets/fleet_ca_key"
server_cert_file = "/data/pki/server.crt"
server_key_file = "/data/pki/server.key"
agent_cert_lifetime_days = 60
heartbeat_interval_secs = 10
heartbeat_timeout_secs = 30

[database]
path = "/data/afkfleet.db"
backup_dir = "/data/backups"
backup_interval_hours = 24
backup_keep = 14

[auth]
access_token_ttl_secs = 900
refresh_token_idle_days = 7
refresh_token_absolute_days = 30
# 2FA is mandatory for every user and has no switch (§7.2)
argon2_memory_kib = 19456
argon2_iterations = 2
argon2_parallelism = 1
argon2_max_concurrent = 4
lockout_threshold = 5
invite_ttl_hours = 72

[rate_limits]
per_ip_per_sec = 20
per_ip_burst = 40
login_per_ip_per_min = 5
register_per_ip_per_hour = 5
refresh_per_ip_per_min = 30
per_user_per_sec = 10
per_user_burst = 30
chat_per_bot_interval_secs = 3
chat_per_bot_burst = 3
chat_per_user_per_min = 20
ws_connections_per_user = 3

[vault]
master_key_files = { 1 = "/run/secrets/vault_key_1" }
active_key_id = 1

[accounts]
max_per_member = 3
allow_offline = false                    # dev only

[chat]
retention_days = 14
command_allowlist = ["/spawn", "/home", "/afk"]   # usable with Control; every other /command needs Manage
```

`agent.toml`
```toml
name = "agent-1"

[runtime]
max_bots = 50
watchdog_timeout_secs = 30
packet_liveness_timeout_secs = 30
shutdown_timeout_secs = 10
heartbeat_file = "/tmp/afkfleet-agent.alive"

[retry]
base_delay_secs = 5
max_delay_secs = 300
stable_after_secs = 300
circuit_failures = 8
circuit_window_secs = 600
circuit_cooldown_secs = 900

[control_plane]                          # managed mode (production)
url = "https://fleet.example.com:7443"
ca_cert_file = "/run/secrets/fleet_ca_cert"
cert_file = "/data/agent.crt"
key_file = "/data/agent.key"

# [standalone]                           # dev only; mutually exclusive with [control_plane]
# [[standalone.bots]]
# username = "AfkBot1"                   # offline-mode account
# server = "localhost:25565"
# mode = "afk"
```

### B. REST API v1 (initial)
All routes are prefixed with `/api/v1`. The required level is checked through `authorize()`.

| Method & path | Auth | Purpose |
|---|---|---|
| `POST /auth/login` | public | Password step: returns `mfa_token`, or `mfa_setup_required` plus a setup token (never real tokens) |
| `POST /auth/login/mfa` | public | TOTP or recovery code step |
| `POST /auth/refresh` | public (refresh token) | Rotate tokens |
| `POST /auth/logout` | user | Revoke the current session |
| `POST /auth/register` | public (invite) | Create an account from an invite |
| `GET /me` · `POST /me/password` | user (step-up) | Profile, password change |
| `POST /me/mfa/enroll` · `/confirm` · `DELETE /me/mfa` | user or setup token (enroll/confirm); user + step-up (`DELETE`) | 2FA (`DELETE` resets it; enrollment is required again at the next login) |
| `GET /me/sessions` · `DELETE /me/sessions/{id}` | user | Session management |
| `GET /users` · `PATCH /users/{id}` | Admin+ | Role, disable |
| `POST /invites` · `GET /invites` · `DELETE /invites/{id}` | Admin+ | Invites |
| `POST /accounts/link` · `GET /accounts/link/{flow}` | Member+ | Microsoft device-code flow |
| `GET /accounts` · `GET/DELETE /accounts/{id}` | View / Manage | MC accounts |
| `GET/PUT/DELETE /accounts/{id}/grants[/{user}]` | Manage | Sharing |
| `GET /bots` · `GET/PATCH /bots/{id}` | View / Manage | Bot specs & state (`PATCH` mode only needs Control) |
| `POST /bots/{id}/start\|stop\|restart\|reset\|resume` | Control | Lifecycle |
| `POST /bots/{id}/chat` · `GET /bots/{id}/chat` | Control / View | Chat |
| `GET/POST /modes` · `PUT/DELETE /modes/{id}` | Member+ (own) / Admin+ (shared) | Modes |
| `GET /agents` · `POST /agents/enrollment-tokens` · `POST /agents/{id}/disable` | Admin+ | Agents |
| `GET /audit` | Admin+ | Audit log (cursor-paginated) |
| `POST /events/ticket` · `GET /events?ticket=` | user | WebSocket live events |
| `GET /health/live` · `GET /health/ready` | public (no prefix) | Health |

### C. Control-plane protocol sketch (`proto/fleet/v1/agent.proto`)
```proto
syntax = "proto3";
package fleet.v1;

service AgentEnrollment {                       // TLS server-auth only, rate-limited
  rpc Enroll(EnrollRequest) returns (EnrollResponse);   // token + CSR -> signed cert + CA
}

service AgentControl {                          // requires a verified client cert
  rpc Connect(stream AgentMessage) returns (stream ServerMessage);
  rpc RenewCertificate(RenewRequest) returns (RenewResponse);
}

message AgentMessage {
  oneof msg {
    Hello hello = 1;                  // agent_version, protocol_version, capacity, labels, running bots
    Heartbeat heartbeat = 2;
    BotStatus status = 3;             // bot_id, state, since, attempt, last_disconnect_reason
    BotEvent event = 4;               // chat in/out, joined, died, disconnected, watchdog, failed
    SessionRequest session_request = 5;
  }
}

message ServerMessage {
  oneof msg {
    Welcome welcome = 1;              // heartbeat interval, server version
    ReconcileFull reconcile = 2;      // full desired set for this agent
    AssignBot assign = 3;             // BotSpec
    UnassignBot unassign = 4;
    UpdateBotSpec update = 5;
    SendChat send_chat = 6;           // validated ChatMessage, request id
    SessionGrant session_grant = 7;   // username, uuid, mc_access_token, expires_at
    SessionDenied session_denied = 8;
    Reject reject = 9;                // incompatible version, disabled agent
  }
}
```

### D. Database tables (SQLite)
| Table | Key columns |
|---|---|
| `users` | id, username (unique, normalized), password_hash, role, disabled, created_at, password_changed_at |
| `user_mfa` | user_id, secret_ciphertext, key_id, last_used_step, recovery_code_hashes, enabled_at |
| `sessions` | id, user_id, family_id, access_hash, refresh_hash, access_expires_at, refresh_expires_at, last_used_at, revoked_at, ip, user_agent |
| `invites` | id, code_hash, role, created_by, expires_at, used_by, used_at |
| `login_attempts` | username, failures, locked_until, last_failure_at |
| `mc_accounts` | id, owner_user_id, mc_uuid (unique), mc_name, kind, refresh_ciphertext, key_id, status, created_at |
| `account_grants` | account_id, user_id, level, granted_by, granted_at |
| `bots` | id, account_id (unique), server_address, mode_id, desired_state, auto_start, agent_id, last_state, last_state_at |
| `modes` | id, owner_user_id (NULL = built-in), name, visibility, definition_json, version, updated_at |
| `agents` | id, name, enabled, labels, capacity, version, last_seen_at |
| `agent_certs` | fingerprint_sha256, agent_id, not_after, revoked_at |
| `enrollment_tokens` | token_hash, created_by, expires_at, used_at, agent_id |
| `chat_messages` | id, bot_id, direction, kind, sender, text, at |
| `audit_log` | id, at, actor_user_id, actor_ip, action, target_type, target_id, outcome, metadata_json |

### E. Bot state machine (`fleet_core::bot`)
**States:**
- `Stopped`
- `AwaitingSession{attempt}`
- `Connecting{attempt}`
- `Online{since}`
- `Backoff{attempt}`
- `Paused{reason}`
- `Failed{reason}`
- `Stopping`

**Events:**
- `Start`, `Stop`, `Reset`, `Resume`
- `SessionReady`, `SessionUnavailable{retryable}`
- `Joined`, `ConnectFailed(reason)`, `Disconnected(reason)`
- `RetryDue`, `WatchdogTimeout`, `Died`, `SessionClosed`

**Effects:**
- `RequestSession`, `Connect`, `Disconnect`
- `ScheduleRetry{attempt}`
- `StartMode`, `StopMode`
- `Respawn`
- `Notify(BotNotification)`

```
            Start               SessionReady              Joined
 Stopped ─────────▶ AwaitingSession ─────────▶ Connecting ─────────▶ Online ──Died──▶ (Respawn effect, stays Online)
    ▲                    │ SessionUnavailable      │ ConnectFailed          │ Disconnected / WatchdogTimeout
    │                    ▼ (retryable)             ▼ (transient)            ▼ (transient)
    │                 Backoff ◀────────────────────┴────────────────────────┘
    │                    │ RetryDue ──▶ AwaitingSession
    │
    └── Stopping ◀── Stop (from any state; emits Disconnect if a session exists)

 permanent kick / auth invalid twice / crash loop / circuit open too long ──▶ Failed ──Reset──▶ AwaitingSession
 duplicate login (a human is playing)                                      ──▶ Paused ──Resume──▶ AwaitingSession
```
