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
| Minecraft chat components & translations | `azalea-chat`, `azalea-language` | `=0.16.0` (+mc26.1) | fleet-mc | Already in azalea's graph at the same pin. Only for fleet-mc's bounded renderer of server text: it names `PrimitiveOrComponent`, which azalea doesn't re-export, and looks up translation templates, because azalea's own rendering grows exponentially (ADR-0011). Bumped together with azalea |
| Microsoft / Minecraft auth | `azalea-auth` | `=0.16.0` (+mc26.1) | fleet-server | Device-code flow. Never use its file cache. fleet-mc reaches only `azalea::auth::sessionserver` and `certs`, through azalea's re-export, never the Microsoft flows; its clippy config bans them (ADR-0011) |
| Async runtime | `tokio` | 1.53.2 | runtime, mc, testkit, agent, server, client | `test-util` feature in dev. Clippy bans `unbounded_channel`; azalea's two mandated channels are the only exceptions (ADR-0011) |
| Cancellation, task tracking | `tokio-util` | 0.7.19 | runtime, mc, agent, server | `CancellationToken`, `TaskTracker`. fleet-runtime enables no features: it uses only `CancellationToken`, and its actors live in tokio's `JoinSet`, which reports panics (ADR-0013) |
| Stream adapters | `tokio-stream` | 0.1.19 | proto, agent, server | gRPC streams, broadcast → stream |
| Sink/Stream extension traits | `futures-util` | 0.3.34 | client, server | WebSocket split/send |
| Async fns in `dyn` traits | `async-trait` | 0.1.92 | server | Only for `Arc<dyn Port>`. Use generics + RPITIT elsewhere |
| Retry & backoff | `backon` | 1.6.0, dfo | core (policy), agent, client | Exponential backoff with jitter. The default features pull in a tokio sleeper |
| Rate limiting (keyed / in-process) | `governor` | 0.10.4, dfo | runtime (chat), server (per user) | fleet-runtime enables `std` only. The defaults add `quanta`, `dashmap` and `jitter`, which pulls rand 0.9 and getrandom 0.3 into normal dependencies. Without `quanta` there's no `DefaultClock`: the chat queue uses `direct_with_clock` with a tokio-backed clock, so paused time controls it (ADR-0013) |
| Rate limiting (HTTP middleware) | `tower_governor` | 0.8.0 | server | Per IP |
| TTL cache / single-flight | `moka` | 0.12.16 | server | MC token cache, WS tickets |
| Library errors | `thiserror` | 2.0.21 | all libraries | |
| Binary error reporting | `anyhow` | 1.0.104 | `main.rs` only | |
| Serialization | `serde`, `serde_json` | 1.0.229, 1.0.151 | all | |
| Configuration | `figment` | 0.10.19 | agent, server | TOML file + env; upstream is quiet but the crate is stable. It has no default features: the agent enables `toml` and `env`, and `test` (`Jail`) in its tests, whose one closure carries an approved `#[expect(clippy::result_large_err)]` (ADR-0014) |
| DTO & config validation | `garde` | 0.23.0 | api-types, agent, server | Domain value objects use hand-written constructors. It has no default features; members enable `derive` and the rules they use. The agent's config uses garde for its number ranges only and converts texts with the core constructors, so it reports every problem at once (ADR-0014) |
| Logging / tracing | `tracing`, `tracing-subscriber` | 0.1.44, 0.3.23 | all | `env-filter`, `json`. fleet-testkit's log capture uses `tracing` and `tracing-subscriber`; the redaction tests use it (ADR-0011) |
| Metrics | `metrics`, `metrics-exporter-prometheus` | 0.24.6, 0.18.3 (dfo) | runtime, agent, server | Internal port only. The exporter's default `push-gateway` brings its own TLS stack: enable `http-listener` only |
| IDs | `uuid` | 1.27.0, dfo | core, mc | v7, serde. fleet-mc only names `Uuid` in azalea's `AccountTrait` (ADR-0011) |
| Time | `chrono` | 0.4.45, dfo | core, runtime, server | Always UTC. No `clock` feature in core or runtime: time is passed in. The runtime derives `DateTime<Utc>` from tokio's clock, anchored at a wall time its caller passes in (ADR-0010, ADR-0013) |
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
| HTTP client | `reqwest` | **0.13.5**, dfo | client, server, desktop, mc | Feature `rustls` (aws-lc-rs + platform verifier), no native-tls. fleet-mc enables no features: it only names `reqwest::Proxy` in azalea's `AccountTrait` (ADR-0011) |
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
| Containers | `testcontainers` | 0.28.0, dfo | `itzg/minecraft-server`, for fleet-mc's slow tests (P3.7). The default `ring` feature turns on TLS for the Docker client, which the local socket and the Windows named pipe don't need; no feature is enabled |
| Time control | `tokio` `test-util` | | `start_paused`, `advance` |
| `log` records in tests | `log` | 0.4.34 | fleet-testkit dev only: its log capture's test emits a `log` record to prove that reqwest's and rustls's logs reach the redaction check through `tracing-log` (ADR-0011) |
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
| 1 | Connection drop / transient kick / session server unavailable | `Disconnected` / `ConnectionFailed` event; the account's join report (ADR-0011) | `Backoff` with exponential jittered delay (default 5 s → 5 min) → reconnect |
| 2 | Kick that won't heal (banned, not whitelisted, wrong version), or an account the session server refuses (banned from multiplayer, multiplayer disabled) | `DisconnectReason` classifier | `Failed(reason)`, alert the user, no retry until **Reset** |
| 3 | Duplicate login (a human logged into the account) | Classifier | `Paused`. **Never fight the human.** The user resumes it. |
| 4 | Flapping server | Circuit breaker (N failures within a window) | Open circuit with a long cool-down, then one half-open attempt |
| 5 | Expired or invalid session token | Auth error on join | Request one fresh token. If it fails again: `Failed(Auth)` and account marked "re-auth required" |
| 6 | Zombie session: azalea ECS panic or hang (azalea has no `catch_unwind`), frozen server or dead link | **Panic:** the runner's `AppExit` receiver fails at once. **Hang:** the Tick watchdog, no `Tick` for `watchdog_timeout` (30 s) while Online. **Frozen server or dead link:** the packet-liveness timeout, no packet for `packet_liveness_timeout` (30 s) while Online. Ticks are client-side and keep running then (ADR-0008 §5) | Tear down the session and treat it as a transient disconnect |
| 7 | Panic in the bot actor task | Supervisor sees `JoinError::is_panic()` | Restart the actor from its last spec. More than 5 restarts in 10 min → `Failed(CrashLoop)` |
| 8 | MC host thread hangs | Tick watchdog (row 6), and the thread's job queue stops answering | **Abandon** the thread and count it; it is never joined or respawned. The bot reconnects on a fresh thread. Above the abandoned-thread limit (`max_abandoned_threads`, default 3), fleet-mc refuses new host threads (`HostUnavailable`), and the agent process exits and Docker restarts it (ADR-0008 §5, ADR-0011) |
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

> Note (P2.9): The built `authorize()` refines this table (ADR-0010):
> - **Agents.** Only the Owner enrolls agents; viewing and disabling them is Admin+.
> - **Roles.** Only the Owner changes roles, so Admins manage Members by disabling and enabling them. Ownership transfer gets a permission together with a route.
> - **Modes.** Private modes follow the account rule. Shared modes are edited by their creator, while an Admin, and by the Owner. Built-in modes are read-only.
> - **The Owner's "everything"** leaves out: built-in modes, their own role and account status, another Owner, Owner invites, grants to themselves, and other users' personal resources.
> - **Server settings** get a permission when a route exists.

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
- **Branches:** one branch per task, `p<phase>/<task-id>-<slug>` (e.g. `p2/p2.6-bot-state-machine`). Phases 0 and 1 each use a single branch, `p0/foundation` and `p1/azalea-spike`. Phase 2 uses five group branches, one PR each (see the note under Phase 2). Phase 3 uses five group branches plus one task branch for P3.9 (see the note under Phase 3). Phase 4 uses five group branches (see the note under Phase 4). Phase 5 uses four group branches (see the note under Phase 5). Every branch ends in a PR that the user reviews and merges. The `Plan.md` checkbox is ticked in that same PR.
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
  >
  > **Known limit.** A duplicate login that a proxy or plugin reports as plain text has no key, so it classifies as transient. Decided in group B: P4.1 adds per-server conflict texts (see P2.6).
- [x] **P2.5** 🔴 Resilience policies:
  - `RetryPolicy` wraps `backon`'s exponential builder: base, factor, cap, jitter, and a reset after a stable period. Tests assert **bounds**, not exact values.
  - `CircuitBreaker` (closed, open, half-open) is pure, with time passed in.
  - Proptests: delay ≤ cap, and the breaker never lets an attempt through while open.

  > Note (P2.5) (ADR-0010):
  > - **Factor.** Fixed at 2; there's no config key for it.
  > - **Jitter.** backon adds its jitter after the cap (`d + d·U[0,1)`), so it's given `max / 2`. Jittered delays then stay ≤ `max`, which requires `max ≥ 2 × base`. The jitter is seeded from the injected RNG.
  > - **`FailureWindow`.** A pure counter for "N failures within a window". It backs the breaker, and later P4.7.
  > - **Breaker.** One per bot, and it only decides how long to wait. Appendix E's "circuit open too long → Failed" arrow is dropped.
- [x] **P2.6** 🔴 The **bot state machine**: `BotState`, `BotEvent`, `Effect`, and `transition(&state, event, now) -> Transition` (Appendix E). It needs example tests for every transition and proptest invariants:
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

  > Note (P2.6, built in group B): The user decided three questions on 2026-10-06 (ADR-0010):
  > - **Plain-text duplicate login.** No state-machine change: P4.1 adds per-server conflict texts. Until then, a keyless kick while Online backs off like any transient one, and a test pins that.
  > - **Paused and Failed are sticky.** `Start` and `Stop` are no-ops there; only `Resume`, `Reset` and `CrashLoop` leave them. So the first invariant holds for every other state, and the second holds for any event sequence without `Reset` or `Resume`.
  > - **Breaker effects.** `RecordFailure` (a session failed before the stable period, always right before `ScheduleRetry`), `RecordSuccess` (leaving Online after the stable period, whatever the exit) and `ResetBreaker` (`Start`, `Reset` or `Resume` (re)starts the bot).
  >
  > Also settled while building it (ADR-0010):
  > - **Attempts.** `Backoff{n}` and `ScheduleRetry{n}` name the attempt that failed, and the actor waits `RetryPolicy::delay(n)`, so the first retry waits 5–10 s. The `RetryPolicy` docs now count failures, too.
  > - **Session ends.** `ConnectFailed`, `Disconnected`, `WatchdogTimeout` and `SessionClosed` are handled alike in Connecting and Online. `AuthInvalid` while Online counts as transient, so a server can't drive reconnects without a backoff.
  > - **`Notify`** only on entering Paused or Failed. The actor publishes every state change, plus `Died`, for the app.
  > - **More proptest invariants:** session and mode effects balance; at most 2 connects per `Start`, `Reset`, `Resume` or `RetryDue`; `Notify` and the breaker effects match the state change.
- [x] **P2.7** 🔴 The mode model:
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

  > Note (P2.7, built in group C): The user decided five questions on 2026-10-06 (ADR-0010):
  > - **Limited steps.** A mode has at most one at-start and one repeating step each for `AttackFacingEntity` and `SendChat`. Otherwise two steps would get around the attack or chat interval, or fire together on every join.
  > - **Angles** are degrees. `Look`: yaw −180 to 180, pitch −90 to 90. `RotateRandom`: `max_yaw` 0 to 180, `max_pitch` 0 to 90, not both 0. NaN and ±∞ are rejected.
  > - **Errors.** `validate` returns the first `ModeError` in step order. `step` is the index into `steps`, and angle errors name their field.
  > - **New types fail closed.** An older server can't read a stored mode that uses a newer action or schedule type; the rollback promise covers new fields only.
  > - **Presets** are only the definitions `ModeDefinition::afk()` and `farm()`. Their names come with P5.1 and their fixed IDs with P11.1.
  >
  > Also settled while building it (ADR-0010):
  > - **JSON:** `{"steps":[{"action":{"type":"swing_arm"},"schedule":{"type":"every","interval_ms":20000,"jitter_ms":20000},"probability":100}]}`. Every field is required, and reading a `ModeDefinition` runs `validate`.
  > - **`HotbarSlot`** is a newtype for 0..=8, because azalea panics above 8 (ADR-0008 §8). An invalid slot fails while deserializing, like an invalid `ChatMessage`.
  > - **Whole milliseconds.** `validate` drops anything finer than a millisecond before checking, so a definition round-trips through its JSON exactly.
  > - **Snapshots** pin serde_json's own output: `afk_preset`, `farm_preset` and `all_variants` (every type and field name).
- [x] **P2.8** 🔴 `ModePlan`: a pure scheduler that takes a definition, an RNG and the current time and returns `(next_due, Vec<Action>)`. The runtime then only sleeps and executes. Tests use a seeded RNG.

  > Note (P2.8) (ADR-0010):
  > - **Return value.** `PlanTick { actions: Vec<PlannedAction>, next_due: Option<…> }`. `RotateRandom` is resolved to a relative `Turn`, and chat is kept separate for the P4.5 queue.
  > - **Timing.** Gaps are uniform in [interval, interval + jitter], the first run comes one gap after the start, and there's no catch-up.
  > - **Pitch.** After applying a `Turn`, the adapter (P3.6) clamps the pitch to [-90, 90].

  > Note (P2.8, built in group C) (ADR-0010):
  > - **API.** `ModePlan::start(&definition, now, rng) -> (ModePlan, PlanTick)` runs the at-start steps, and `tick(now, rng) -> PlanTick` runs every due step in step order. `PlannedAction` is `Game(GameAction)` or `Chat(ChatMessage)`.
  > - **`GameAction`** lives in `fleet_core::mode`, as the user decided; P2.10's `perform` takes it.
  > - **Rescheduling from `now`.** A step that ran is due again one gap after `now`, not after its old due time. So there's no catch-up, and two runs are never closer than the interval, even when the runtime wakes late. A skipped roll reschedules too.
  > - **No serde** on `ModePlan`, `PlanTick`, `PlannedAction` or `GameAction`.
- [x] **P2.9** 🔴 Authorization: `Role`, `GrantLevel`, `Permission`, `Actor`, `ResourceContext`, `authorize()`.
  - An **exhaustive matrix test** covers every role × grant × permission. It's generated and snapshotted with insta, so every change shows up in review.
  - Edge cases: an Admin acting on the Owner or another Admin, granting above your own level, deny by default.

  > Note (P2.9) (ADR-0010):
  > - **Admins.** An Admin's implicit Manage covers Members' accounts and the Admin's own, but not the Owner's or other Admins'.
  > - **Commands.** `CommandAllowlist` is a core type. Changing to a mode that contains a non-allowlisted command needs Manage.
  > - **Built-in modes.** `ResourceContext::Mode{owner: None}` is a built-in mode.

  > Note (P2.9, from the group A review): **Allowlist format.** Appendix A writes allowlist entries as `"/spawn"`, but `ChatMessage::command_name()` returns `spawn`, so the formats must match. Compare exactly and case-sensitively against the allowlist, so the check fails closed: `/Spawn`, `/minecraft:spawn` and `/spawn` followed by a zero-width space all need Manage.

  > Note (P2.9, built in group D): The user decided eight questions on 2026-10-06 (ADR-0010):
  > - **Private modes** follow the account rule: their owner, the Owner, and Admins for Members' modes see and edit them. Everyone else gets 404.
  > - **Shared modes** are visible to everyone. Their creator and the Owner edit them. Shared modes need Admin+ (Appendix B), so a creator who's demoted to Member can no longer edit theirs.
  > - **Grants.** Only Manage edits grants, so "never above your own level" always holds; a test pins it. Nobody grants to themselves, so an Admin can't turn implicit Manage into a grant that outlives a Member's promotion to Admin. A grant to an alt account is an accepted risk (threat model; P11.9 shows the grants at a promotion).
  > - **Roles.** Only the Owner changes roles, between Member and Admin, never their own or the Owner's. Admins disable and enable Members. Ownership transfer gets no permission until it has a route (P7.9).
  > - **Allowlist entries** keep the slash: `/` plus a command name without arguments, at most 64 entries. A missing key means an empty list, so every command needs Manage.
  > - **Invites.** Admins list all invites. Revoking follows creating: Member invites need Admin+, Admin invites the Owner, and Owner invites are never allowed.
  > - **Self-service routes** check `ResourceContext::Personal{owner}` with `Permission::UsePersonal`: allowed for the owner, 404 for everyone else, the Owner included.
  > - **Agents.** Only the Owner enrolls agents; viewing and disabling them stays Admin+. An agent receives session tokens for the bots assigned to it, so an Admin's own agent could otherwise get the Owner's. This deviates from §7.3.
  >
  > Also settled while building it (ADR-0010):
  > - **API.**
  >   - `ResourceContext` is `Global`, `Account{owner, grant}`, `Mode{owner, visibility}`, `User(UserRef)`, `Invite{role}` or `Personal{owner}`. `grant` is the actor's own grant, and a bot is authorized through its account.
  >   - `AuthzError` is `NotFound` (the actor can't even see it: 404), `Forbidden` (403) or `WrongResource` (a permission checked against the wrong kind of resource: a caller bug).
  >   - `Permission` is `Copy`. `SendChat` and `SetBotMode` carry a `CommandCheck` that only `CommandAllowlist::check` and `check_mode` create, so no chat text ends up in `Debug` output.
  > - **Server address and auto-start** need Manage (`ConfigureBot`): Appendix B's `PATCH /bots/{id}` needs Manage except for the mode.
  > - **The Owner's limits.** §7.3's "can do everything" doesn't cover these: editing built-in modes, changing their own role or disabling themselves, acting on another Owner, Owner invites, granting to themselves, and other users' personal resources.
  > - **Names.** `Role`, `GrantLevel` and `ModeVisibility` map to `users.role`, `account_grants.level` and `modes.visibility` through `as_str` and `FromStr`.
  > - **Not in `authorize`:** step-up (§7.2) is an authentication check in P7, and the account quota belongs to P9.4.
  > - **Matrix.** The snapshot `authorization_matrix` has a table per kind of resource: accounts by actor, owner and grant; the rest by actor and target. A separate test checks every permission against every other kind of resource.
- [x] **P2.10** 🔴 Minecraft ports, traits only:
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

  > Note (P2.10, from group C): `perform` takes `fleet_core::mode::GameAction`, which P2.8 already defines. Its hotbar slot is a `HotbarSlot`, so the port can't get a slot above 8 (ADR-0010).

  > Note (P2.10, built in group E): The user decided four questions on 2026-10-06 (ADR-0010):
  > - **Errors.** `SessionError` is `Closed`, `QueueFull`, `TimedOut` or `NotInWorld`; `ConnectError` is `HostUnavailable`, which the actor reports as `ConnectFailure::HostUnavailable`.
  > - **Event delivery.** Only `Chat` may be dropped when the consumer lags, and the adapter counts the drops. `Joined`, `Died`, `Disconnected` and `ConnectionFailed` are always delivered.
  > - **Liveness** is data only: `Liveness { last_tick, last_packet }`. The watchdog's comparison is P4.6's.
  > - **`ConnectParams`** is `{bot_id, server, credentials, connect_timeout}`. The `BotId` lets fleet-mc name host threads and tag its logs.
  >
  > Also settled while building it (ADR-0010):
  > - **Traits.** `MinecraftConnector` has the associated types `Session` and `Events`. The connector and the handle are `Send + Sync + 'static`, the events `Send + 'static`, and every future is `Send`. A doctest drives the ports from generic code and checks that.
  > - **`connect`** resolves once the session has started, before the bot reaches the server. The outcome arrives as an event, and a connect timeout as `ConnectionFailed(TimedOut)`.
  > - **`SessionEvents::next`** is cancel-safe and returns `None` after the session's one terminal event (`Disconnected` or `ConnectionFailed`). `Died` comes once per death.
  > - **`disconnect()`** returns `()`: the teardown always finishes, and calling it again is harmless. `liveness()` is synchronous.
  > - **`Liveness`** has public fields and no constructor. std can't make an `Instant` without `Instant::now()`, which `fleet-core`'s clippy config bans, so core can't test one; the fakes (P3.1) build them.
  > - **TDD.** Only the redaction test has a meaningful red phase: a stub `Debug` that printed the token failed it. The rest of the task is declarations.
- [x] **P2.11** A `thiserror` error enum per module, and crate- and module-level docs.

  > Note (P2.11): Error enums are built in each task, because each task tests its error paths first. P2.11 audits them and writes the crate docs (ADR-0010).

  > Note (P2.11, built in group E): **Audit result.**
  > - **Errors.** 15 enums, one per fallible module: the 13 from groups A–D plus `ConnectError` and `SessionError`. Each is `Copy`, its fields are positions, lengths, limits or the crate's own enums, and foreign errors are dropped instead of wrapped, so no message can echo input. Every variant has a test that asserts it, and every public fallible function has an `# Errors` section.
  > - **Fixes.**
  >   - A test pins the one composed message, `CommandAllowlistError::Invalid`.
  >   - The `GameAction` docs link to `SessionHandle::perform`.
  >   - The allowlist error's `index` fields say they're positions.
  >   - The `authz` docs list `WrongResource` (500).
  >   - One sentence in the `id` docs is fixed.
  > - **Crate docs.** The module list now includes `mc`, plus a "How it fits together" section and the conventions from ADR-0010.
  > - **Flagged:** serde_json's own errors quote their input (see P11.4).

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

> Note (P3):
> - **Five group branches, plus one task branch.** At the user's request, Phase 3 is built in group PRs like Phase 2. Each group has one branch and one commit per task, and the groups run in this order, each after the previous PR is merged:
>
>   | Group | Branch | Tasks |
>   |---|---|---|
>   | A | `p3/p3.1-testkit-fakes` | P3.1 |
>   | B | `p3/p3.2-p3.5-host-pool-and-events` | P3.2, P3.5 |
>   | C | `p3/p3.3-account-adapter` | P3.3, alone, so the Minecraft token handling gets a focused review |
>   | D | `p3/p3.4-p3.6-connector-actions-slow-suite` | P3.4, P3.7, P3.6, in that commit order: P3.6 can only be proven live, so the slow harness comes first |
>   | E | `p3/p3.8-teardown-and-wrap-up` | P3.8 and the phase wrap-up |
>   | — | `p3/p3.9-hold-use` | P3.9, its own task after group E |
> - **Decisions.** The user answered the Phase 3 plan's open questions on 2026-10-06. [ADR-0011](docs/adr/0011-fleet-mc-and-fleet-testkit-conventions-and-phase-3-refinements.md) records them; the notes below summarize what changes a task.
> - **azalea's advisories and licenses** are accepted (RUSTSEC-2023-0071, -2026-0118, -2026-0119) or excepted per crate (`minecraft_folder_path`, `socks5-impl`), with recorded reasons. The `deny.toml` entries and ADR-0012 land in group B, together with azalea.
> - **Dependencies.** Also approved: `uuid` and `reqwest` (no features) in fleet-mc, only to name types in azalea's `AccountTrait`; `tracing-subscriber` and `rstest` as fleet-mc dev-dependencies; `tokio` in fleet-testkit. `tokio-util` comes only if a task needs it.
> - **Lint guards.** Clippy bans `unbounded_channel`, with two approved `#[expect]`s for azalea's mandated channels (P3.4). fleet-mc's clippy config bans azalea's Microsoft login functions. A `scripts/` test checks that every crate-local `clippy.toml` carries the root's settings.
> - **Test servers.** Only local Docker servers: itzg in offline and online mode through testcontainers, with the image pinned in `deploy/compose.dev.yaml`. No public game servers.

- [x] **P3.1** 🔴 `fleet-testkit`: `FakeConnector` and `FakeSession`. They need to support:
  - scripted connect results
  - injected events (`Joined`, `Chat`, `Died`, `Disconnected(reason)`)
  - liveness timestamps that a test can advance or freeze
  - a log of performed actions and sent chat
  - "hang" (no more ticks) and "fail on action" modes

  Write tests for the fake itself.

  > Note (P3.1, from Phase 2): Liveness stamps are `std::time::Instant`. The fake stamps them with `tokio::time::Instant::now().into_std()`, so paused time works. Both stamps are set when the session is created (ADR-0010).

  > Note (P3.1, from the Phase 3 plan) (ADR-0011):
  > - **Types.** `FakeConnector`, `FakeSession`, `FakeEvents` and a test-side `SessionController` per session.
  > - **Liveness.** Both stamps follow tokio's clock until a test freezes one.
  > - **Hang** freezes both stamps, and every call fails with `TimedOut`.
  > - **The event contract** of ADR-0010 is enforced by the fake: one terminal event, then `None`; `Died` once until `respawn()`; only `Chat` is dropped, and counted.
  > - **No panics.** It's library code, so misuse returns an error.
- [x] **P3.2** 🔴 `McHostPool` spawns one host thread per session (ADR-0008 §2):
  - Each thread has a current-thread runtime and a `LocalSet`, and ends when its session ends.
  - Work reaches the thread over a bounded queue.
  - A hung thread is **abandoned** and counted, never joined or respawned. Test this with an injected job that never returns.

  > Note (P3.2, from the Phase 3 plan) (ADR-0011):
  > - **Thread names.** `mc-` plus the last 12 hex characters of the `BotId`, its random part. That's 15 bytes, Linux's limit, and the leading timestamp would make truncated names collide.
  > - **Limit.** Above `max_abandoned_threads` (default 3, Appendix A), the pool refuses new threads, so `connect()` returns `HostUnavailable`. The library never ends the process; the agent does (P5.3).
  > - **Diagnostics.** `live_threads()` and `abandoned_threads()` are public, for P3.8 and P4.9.
  > - **Test time.** These tests wait for a real OS thread, which tokio's paused clock would race. So they use real time with upper-bound timeouts that only fire on failure, and never a sleep. The hung job blocks on a std `sync_channel` that the test releases at the end.

  > Note (P3.2, from group B, the user's decisions) (ADR-0011):
  > - **Limit.** The pool refuses new threads once `abandoned_threads() >= max_abandoned_threads`, the same point where the agent exits (P5.3).
  > - **Counting.** `abandoned_threads()` only counts up, even when an abandoned thread ends after all; `live_threads()` goes down then. A pool's counts are shared by its clones.
  > - **No `CancellationToken`.** A host thread's jobs are tasks of its `JoinSet`. The stop signal ends the loop, and dropping the `JoinSet` and the `LocalSet` cancels them. That's a deliberate exception to the CLAUDE.md task rule, and `tokio-util` stays out.
  > - **Handles.** `HostThread` is `Clone`. The thread also ends when every clone is dropped (the job channel and the stop signal close), so an actor that panics or is aborted without `disconnect()` can't leave a bot running. A test covers it.
  > - **API.** `McConfig` holds the defaults (`max_abandoned_threads` is a `NonZeroUsize`, since 0 would refuse every thread). `spawn(bot_id)` returns a `HostThread` or a `SpawnError`, which becomes `ConnectError::HostUnavailable`. `run(job)` queues at once and fails with a `JobError`, which becomes the matching `SessionError`. A job whose caller has given up is skipped. `shutdown()` returns `Ended` or `Abandoned`, the first outcome on every later call; `ended()` resolves when the thread is gone.
  > - **Threads are never joined.** Joining would block the caller's runtime; the exit signal, sent last by the thread, says when it has ended.

  > Note (P3.2, from the group B review) (ADR-0011): **Threads dropped while hung count too.** When the last `HostThread` handle drops before a shutdown decided the outcome, the pool keeps the thread as an orphan: its exit signal and a deadline, the shutdown timeout after the drop. `spawn()` and `abandoned_threads()` settle the orphans first, without blocking or spawning: one that has ended is forgotten, and one past its deadline is counted as abandoned and logged at `warn`. So a hung session whose owner never called `disconnect()` still counts against the limit.
- [x] **P3.3** Account adapter: a custom `AccountTrait` for server-issued `SessionCredentials`, and offline accounts for dev and tests.

  > Note (P3.3, from the Phase 3 plan) (ADR-0011):
  > - **Session-server errors.** `InvalidSession` and `ForbiddenOperation` give `AuthRejected`. `Banned` and `MultiplayerDisabled` give a new permanent reason. An outage, HTTP error, rate limit, unknown response or timeout gives a new transient reason. That's a small fleet-core change in this task, which amends ADR-0010.
  > - **`refresh()`** is a no-op that returns `Ok(())`. An error would make azalea log a misleading Microsoft-flow error.
  > - **`join()`** runs on Bevy's IO pool, not on the host thread. It's bounded by a timeout, because azalea's HTTP client has none, and it reports to the session over a bounded channel.
  > - **The adapter doesn't check `expires_at`.**
  > - **Log redaction.** The tests capture every level from every target, azalea included, and assert that the token never appears. A failing assertion reports only the target and a count, never the captured lines or the token.

  > Note (P3.3, from group C, the user's decisions) (ADR-0010, ADR-0011):
  > - **New reasons.** `AccountRestricted{restriction}` (`Banned`, `MultiplayerDisabled`) is permanent, with the new kinds `PermanentKind::AccountBanned` and `MultiplayerDisabled`. `SessionServerFailed{failure}` (`Unreachable`, `RateLimited`, `TimedOut`, `Unexpected`) is transient.
  > - **`FailReason::Kicked` is now `FailReason::Permanent`**, since a session-server refusal isn't a kick.
  > - **On our timeout**, `join()` returns azalea's `Unknown("no answer within the session-join timeout")`. The timeout is `McConfig::session_join_timeout` (10 s).
  > - **Reports** go over a bounded channel of one, written with `try_send`: `account(credentials, bot_id, join_timeout)` returns azalea's `Account` and the receiver, which P3.4's pump turns into `EventSink::terminate`.
  > - **The log** is one `debug` line per failed join, with a fixed label, never azalea's error text.
  > - **Tests.** The join's bookkeeping takes the session-server future, so fast tests script it; the one call into azalea is covered by P3.7's online-mode scenario. The log capture also bridges `log` records (reqwest, rustls) through `tracing-log`; `log` is a new fleet-mc dev-dependency.
  > - **Dead code until P3.4.** `mod account` carries a temporary `#[expect(dead_code)]` in non-test builds, approved by the user; P3.4 removes it.
  > - **Found:** azalea logs the chat-signing private key at `trace` (`azalea_auth::certs`). It's a known limit in the threat model. P5.2 caps `azalea_auth` at `info`, like fleet-server does (P9.3).
- [x] **P3.4** `AzaleaConnector` implements `MinecraftConnector`:
  - azalea's auto-reconnect and auto-respawn are **disabled**
  - connect timeout
  - start, hosting model and executor as decided in ADR-0008 §1–3

  > Note (P3.4, from Phase 2): open questions, flagged and not yet decided:
  > - Appendix A has no config key for the connect timeout that `ConnectParams` carries.
  > - The account's `refresh()` fails fast, because the core state machine now requests the fresh token (ADR-0010).

  > Note (P3.4, from group E): `ConnectParams` carries the `BotId`, so host threads can be named after their bot, which helps when one is abandoned (ADR-0010).

  > Note (P3.4, from the Phase 3 plan) (ADR-0011):
  > - **Connect timeout.** It runs from `connect()` until `Joined`: resolve, TCP, login, session join and configuration. The default is 30 s, from the new key `[runtime] connect_timeout_secs` (Appendix A). Resolving happens inside the session, so a resolve error arrives as `ConnectionFailed`.
  > - **`refresh()`** is decided in P3.3.
  > - **The two azalea-mandated unbounded channels** carry the approved `#[expect(clippy::disallowed_methods, …)]`.
  > - **Diagnostics.** `live_worlds()` and `dropped_chat()` are public.
  > - **Fault injection.** A hook behind a fleet-mc cargo feature, off by default, adds a system to a session's App for the containment test (P3.7).

  > Note (P3.4, from group B):
  > - **Host-thread handles.** A host thread ends when every `HostThread` clone is dropped (P3.2), so no task on the thread itself may hold a clone, or the thread never ends without `disconnect()`.
  > - **Wiring the event bridge (P3.5).** P3.4 removes the temporary `#[expect(dead_code)]` on `mod events` and wires the producer side:
  >   - a pump on the host thread passes each event from azalea's `LocalPlayerEvents` channel to `EventSink::forward`
  >   - every App gets the `PacketLivenessPlugin`
  >   - the connect timeout, `AppExit` and the account's auth result go through `EventSink::terminate`
  >   - `disconnect()` calls `EventSink::close`, and `liveness()` reads the session's `LivenessStamps`
  >   - the connector shares one `EventCounters` between its sessions and exposes `ignored_action_bar()` next to `dropped_chat()`

  > Note (P3.4, from group D, the user's decisions) (ADR-0011):
  > - **Shape.** `AzaleaConnector::new(&McConfig)` owns its `McHostPool`. It exposes `pool()`, `live_worlds()`, `dropped_chat()` and `ignored_action_bar()`. `connect()` spawns the host thread, queues the session's driver with the new `HostThread::start` (a `JoinSet` task with no answer and no job timeout) and returns at once. `McSession` is the `SessionHandle`.
  > - **The driver** runs on the host thread for the whole session. Every wait before `Joined` (resolving, the join callback, the connect) also watches the stop signal, the connect deadline and azalea's runner.
  > - **Loop order** *(the user's notice)*: stop, auth reports, the connect deadline until joined, the runner's end, then events. So a server that keeps sending events before `Joined` can't starve the connect timeout. A test floods the loop with events and was red against an events-first order.
  > - **Endings.**
  >   - A resolve error is `ConnectionFailed(Unresolvable)`.
  >   - A runner end nobody asked for (an `Ok` too), a closed join callback or a closed event channel is `Disconnected(SessionCrashed)`.
  >   - A call that finds no `Client` or no components on the host thread is `NotInWorld`.
  > - **Teardown.** `disconnect()` closes the bridge first, so a normal teardown never reports a crash. Then it stops the driver, which exits azalea, waits up to `McConfig::app_exit_timeout` (2 s) for the runner and drops the `Client`. Then it waits up to that plus 1 s for the driver, and shuts the host thread down. It's idempotent from every clone.
  > - **The owner's bridge handle** is a non-counting `BridgeControl` (`phase`, `close`, `respawned`), so the rule that a dropped sink ends the session still holds.
  > - **The `Client`** lives in a slot that only host-thread jobs clone from. A drop guard empties it when the driver ends.
  > - **Stubs until P3.6/P3.7** *(the user's decision)*. `perform`, `send_chat` and `respawn` check the session state and run an empty host job; `respawn` already tells the bridge. P3.7 wires `send_chat`, and P3.6 the rest.
  > - **Fault injection.** `AzaleaConnector::with_app_hook(self, impl Fn(BotId, &mut azalea::app::App) + Send + Sync + 'static) -> Self`, behind the off-by-default `fault-injection` feature. *(The user's notice)* A build without `debug_assertions` and with the feature hits a `compile_error!`, so no release build can include the hook.
  > - **Tests.** The loopback tests (Plan §8 allows loopback outside the slow profile) cover a silent server timing out, a refused port, teardown, dropped handles and the host limit. The session loop, the pre-join waits, the App's plugins and executor, and `HostThread::start` have unit tests.
  > - **Found in CI on Linux: the first events could be lost.** azalea spawns the bot and reports a failed connect in the same frame, and the driver attached `LocalPlayerEvents` only after the join callback. So a fast refused connect on a loaded Linux runner ended as `TimedOut` at the connect timeout. Now an observer on `Add` of `LocalEntity` moves the one sender into the bot's component when azalea spawns it *(the user's decision)*. Unit tests and a Linux stress run (19 of 24 failed before, 24 of 24 pass after) cover it (ADR-0011).
- [x] **P3.5** 🔴 Map azalea events to `SessionEvent`, with unit tests on the pure mapping functions. Chat goes through the core sanitizer, with the sender taken only from where ADR-0008 §7 allows; kick reasons go through the core classifier input. `Tick` and server events such as `KeepAlive` only update the session's liveness timestamps (ADR-0008 §4–5).

  > Note (P3.5, from the group A review): **Logging chat.** Sanitized chat text keeps `\n`. Log it as a structured field (`?` or JSON), never with `%` (Display), so a server can't forge log lines.

  > Note (P3.5, from group E): **Event delivery.** The bounded bridge (ADR-0008 §4) may drop only `Chat` events, and it counts them. `Joined`, `Died`, `Disconnected` and `ConnectionFailed` are always delivered, at most one terminal event per session, after which `next()` returns `None` (ADR-0010).

  > Note (P3.5, from the Phase 3 plan) (ADR-0011):
  > - **Chat.** Action-bar (`overlay`) messages are dropped and counted. An unknown `ChatKind` falls back to `chat`. The text is what the vanilla client shows, without the chat-type decoration.
  > - **Lifecycle.** `Joined` is the first `Spawn` only. A `Died` before `Joined` is held until after it.
  > - **Liveness.** Received packets are observed through `ReceiveGamePacketEvent`, so `packet-event` stays off.
  > - **Terminal signals.** Other sources inject them into the bridge (the account's auth result, the connect timeout, `AppExit`), and the first one wins.

  > Note (P3.5, from group B, the user's decisions) (ADR-0011):
  > - **Chat kinds.** The server assigns the registry ids, and they're read in vanilla's order (azalea's `ChatKindKey::ALL`): 1 is an emote, 2 an incoming whisper, 4 `/say`. Everything else is `chat`: the echo of a whisper the bot sent, team chat, unknown ids and inline chat types. A server whose data packs reorder chat types can mislabel a kind; the text and the sender don't depend on it. That's a known limit.
  > - **Action bar.** Overlay messages are counted on their own (`ignored_action_bar`), apart from `dropped_chat`, and log nothing, so `dropped_chat` stays a signal of overload.
  > - **System messages** keep their whole sanitized text, and their sender is never guessed (see the chat-format note under P4.1).
  > - **Unknown azalea events** land in the mapping's catch-all and are logged at `debug`. Every azalea bump re-checks the variants (ADR-0003).
  > - **Dead code until P3.4.** Outside the tests, nothing calls the bridge's producer side yet (the mapping, `EventSink`, the liveness plugin). So `mod events` carries a temporary `#[expect(dead_code)]`, approved by the user, in non-test builds only. P3.4 removes it.
  > - **Shape.**
  >   - `McEvents` is the public `SessionEvents`. Its queue follows the fake's design (a `Mutex<VecDeque>` plus `Notify`): chat is dropped at the capacity (64, `McConfig::event_capacity`), and lifecycle events always go in.
  >   - `EventSink` maps and applies azalea's events, takes injected terminal events (`terminate`, the first one wins), learns of respawns (`respawned`) and closes the bridge.
  >   - `LivenessStamps` are atomic offsets that never move backwards. `PacketLivenessPlugin` stamps every `ReceiveGamePacketEvent` in `Update`.
  >   - The tests build kick reasons from JSON, through `serde_json` as a dev-dependency.

  > Note (P3.5, from the group B review, the user's decisions) (ADR-0011): **Bounded rendering of server text.**
  > - **The problem.** azalea's `FormattedText::to_string()` re-renders every argument of a translation. For an unknown key, the template is the key itself (or the JSON `fallback`), so a key like `%1$s%1$s%1$s%1$s`, nested, grows exponentially: a 507-byte system message renders to 16.7 million characters.
  > - **The renderer.** fleet-mc renders server text itself, at all six places the mapping used `to_string()`: system chat, player and disguised names and texts, and kick reasons.
  >   - It follows azalea-chat's `TranslatableComponent::read` rules: `%%`, `%s`, `%N$s`, known keys through `azalea_language::get`, else the fallback, else the key; a malformed template shows the key.
  >   - It walks the text by reference with an explicit stack, never cloning a subtree.
  >   - It stops after 4,096 characters or 16,384 steps, and reports whether it stopped. A step is a component visited, an argument substituted (whatever it renders to) or a character written, so templates of empty substitutions are paid for too.
  > - **Cut-off chat** goes through `IncomingChat::from_parts` with `truncated = true`. Only the text counts: a cut-off sender name is just capped. A kick keeps its class, because classifying uses only the top-level key.
  > - **Dependencies.** fleet-mc depends directly on `azalea-chat` (to name the argument type, which azalea doesn't re-export) and `azalea-language`, both `=0.16.0` and already in azalea's graph (§5).
  > - **Every azalea bump** re-checks the renderer against azalea-chat (ADR-0003).

  > Note (P3.5, from the group B review) (ADR-0011): **A dropped sink ends the session.** When the last `EventSink` clone is dropped, for example because the host thread is gone, a session that hasn't ended gets `Disconnected(SessionCrashed)`, logged at `warn`, so the actor never waits for events that can't come. Once the session ended or was closed, dropping the sink changes nothing.
- [x] **P3.6** 🔴 Map each `Action` to azalea calls: look, rotate, jump, sneak, swing, use item, attack facing entity (with a reach check), hotbar, respawn, chat.

  > Note (P3.6, from group C, the user's idea): Check whether azalea can hold right-click (use item) down continuously, the way `Sneak{on}` holds sneak. If it can, propose a `HoldUse{on}` action as an additive change to the mode model. It's a new action type, so older servers reject modes that use it (ADR-0010).

  > Note (P3.6, from group E, the user's decision): The adapter skips any `GameAction` with a non-finite angle and logs it, as a safety net behind mode validation. The `SessionHandle::perform` docs say so (ADR-0010).

  > Note (P3.6, from the Phase 3 plan) (ADR-0011):
  > - **Commit order.** In group D, P3.6 comes after P3.7, so its red tests run against a live server and check the effects through RCON. P3.7's "every action" scenario arrives with it.
  > - **HoldUse.** azalea 0.16.0 only has a one-shot use. Holding would send `UseItem` and later a raw `PlayerAction{ReleaseUseItem}`, for items with a use duration. This task verifies that on the test server and records the result in ADR-0011. The mode-model change is P3.9.

  > Note (P3.6, from group B): **Respawn.** After the respawn, the session calls `EventSink::respawned()`, so the bot's next death is reported again (P3.5).

  > Note (P3.6, from group D, the user's decisions) (ADR-0011):
  > - **Mapping.** Each action runs in one synchronous job on the host thread.
  >   - Components that azalea's `Client` methods would panic on (`LookDirection`, `PhysicsState`, the hit result, the entity index, the connection) are checked first; a missing one gives `NotInWorld`.
  >   - `Turn` reads the direction, wraps the yaw to −180..180 and clamps the pitch to −90..=90.
  >   - `AttackFacingEntity` takes azalea's target, which already respects the reach. With nothing in reach it returns `Ok` and logs at `debug`. Otherwise it writes the attack packet by hand (ADR-0008 §8), swings, and resets `TicksSinceLastAttack`.
  >   - `respawn()` sends `PerformRespawnEvent` and tells the bridge in the same job.
  >   - A non-finite angle is skipped, logged at `warn`, and returns `Ok`.
  > - **Tests.** Unit tests cover the turn, the finite check and the attack packet's bytes; they were red against stubs first. The new `slow_actions_scenario` was red against P3.4's stub `perform` and is green now. It checks through RCON:
  >   - the rotation after a look and after a turn
  >   - the `jump` statistic
  >   - sneaking on and off (an inline predicate)
  >   - `SelectedItemSlot`
  >   - a thrown snowball's `used` statistic
  >   - a pig hurt within reach and one untouched beyond it
  >   - a `/me` emote's echo
  >   - full health after a respawn, and a second `Died` after a second kill
  >
  >   Swings are seen by a watcher bot through a test-only system *(the user's decision)*. The containment scenario's action is now a look that RCON confirms.
  > - **HoldUse is feasible** *(a temporary probe, never committed; the user's decision)*. With a bow and arrows, `start_use_item` started drawing. Released after about 1 tick, it shot nothing; held for 25 ticks, it shot nothing yet. A raw `ServerboundPlayerAction{ReleaseUseItem}` (`pos` default, `direction` Down, `seq` 0) then shot one arrow: the bow's `used` statistic went to 1 and an arrow entity appeared. The packet encodes correctly through azalea's normal writer, so no hand-written bytes are needed. P3.9 can map `HoldUse{on}` to `start_use_item` and that release packet (ADR-0011).

  > Note (P3.6, from group E, the user's decision) (ADR-0011): **The respawn step was flaky.**
  > - **The failure.** About 4 local runs in 10 failed with "the second death didn't happen within 60s", from before group E's changes too.
  > - **The cause, as far as the runs show.** The step killed the bot as soon as the server reported full health after the respawn. A respawned player takes no damage, not even from `/kill`, until its client says it has loaded (`PlayerLoaded`), and azalea sends that only once the bot is in a loaded chunk again.
  > - **The fix.** The scenario's hook now also counts each time azalea adds its `HasClientLoaded` marker, and the step waits for that before the second kill. It then passed 8 runs in 8.
- [x] **P3.7** 🔴 Slow integration tests (`slow_*`, testcontainers + itzg, offline mode):
  - join and see the join message
  - send chat and see it echoed back
  - perform every action without an error
  - get kicked by RCON (`docker exec … rcon-cli kick`) and receive `Disconnected` with a reason
  - reconnect

  > Note (P3.7, from the Phase 3 plan) (ADR-0011):
  > - **A few scenario tests.** Each owns one container and runs several steps. nextest can't share a container between test processes, so a nextest test group serializes them. A fast test checks that the image and `VERSION` match the pins in `deploy/compose.dev.yaml`.
  > - **Online mode, added.** A local online-mode container: a garbage token gives `AuthRejected` within seconds, and an offline account gives `unverified_username`. Neither uses real credentials. Azalea's full login path runs under the log-redaction check (P3.3).
  > - **Fault containment, added.** A panic injected into one session's ECS ends it as `Disconnected(SessionCrashed)`, while a second session from the same pool keeps ticking, sends chat and performs an action.
  > - **ADR-0003's bump procedure** gains the test pin.

  > Note (P3.7, from group D, the user's decisions) (ADR-0011):
  > - **Layout.** One test binary, `crates/fleet-mc/tests/minecraft/`, holds the harness, the fast `pins` test and one module per scenario. Each `slow_` scenario starts its own container, and the nextest test group `minecraft` (one thread) runs them one at a time. `just test-slow` turns on the `fault-injection` feature, which the containment module needs; it's `cfg`-gated, so the binary and `pins` build without it.
  > - **Containers.** Each uses the compose pins (`tag@digest`) and the dev stack's environment, with `MEMORY` 1G, and waits for the image's healthcheck (5 min bound). Only the game port is published, on `127.0.0.1` and a random port, with `publish_all_ports` off, so RCON stays inside; `rcon-cli` runs through `exec`.
  > - **Found: a bot never sees its own join message.** The server broadcasts it before adding the new player; the spike also saw it through a second bot. So the offline scenario has a watcher, AfkBot2, that sees AfkBot1 join and rejoin. Both see AfkBot1's chat echo. That deviates from the approved plan's wording ("see `AfkBot1 joined the game`"), not from this task's.
  > - **Red, then green.** The chat steps failed against P3.4's stub `send_chat`, which P3.7 then wired to `client.chat`. All three scenarios pass in about 50 s together.
  > - **Online mode** *(as planned)*: a garbage token ends in `AuthRejected` within seconds, and an offline account is kicked with `unverified_username`. The marker token never shows in the TRACE capture of every target.
  > - **The log capture moved to fleet-testkit** *(the user's decision)*: `install()` and `check_absent()` return errors instead of panicking.
  > - **Failure output.** A failed wait shows the events the session sent meanwhile.

  > Note (P3.7, from group E, found by the user's real-account check; the user's decisions) (ADR-0010, ADR-0011): **Chat waits for signing.**
  > - **The bug.** `send_chat` returned `Ok` for chat that a server enforcing secure chat drops. azalea sets up the chat-signing session in the background after the join and sends chat unsigned until then.
  > - **The fix.** A plugin publishes each session's signing state: `NotNeeded` (offline account, offline-mode server, or a server that doesn't enforce secure chat, from its login packet), `Pending`, `Ready` or `Failed`.
  > - **Waiting.** A chat message, not a command, waits while signing is `Pending`, at most until the game state plus `McConfig::chat_signing_timeout` (10 s).
  > - **Refusing.** `Failed`, or still `Pending` at that deadline, gives the new `SessionError::ChatUnavailable`, logged at `warn` once. The session stays up.
  > - **The fake** gains `fail_chat`.
  > - **A key fetch that hangs** keeps chat unavailable until the next reconnect, since azalea never retries it.
  > - **Commands** are never signed by azalea. A server that enforces secure chat rejects those with message arguments, while `send_chat` returns `Ok` (found by the user's real-account check; flagged under P4.5 and P11.3).
- [x] **P3.8** 🔴 Clean-up test: after the full teardown from ADR-0008 §10 (not just `disconnect()`), the thread count and the number of live Worlds go back to baseline.

  > Note (P3.8, from the Phase 3 plan) (ADR-0011):
  > - **Baseline.** Taken after a warm-up join (P1.9).
  > - **Every platform.** `live_threads()` and `live_worlds()` must return to it.
  > - **Linux only.** The OS thread count from `/proc/self/status` must as well; Windows has no portable count without `unsafe` or a new crate.
  > - **Group E also adds:**
  >   - **The user's real-account check.** A `manual_` test in its own nextest profile, run by a `just` recipe. It reads `secrets/p1.8-account.txt`, which the user creates with the archived spike's `fetch-token`, and the AI never reads it. No error message ever includes the file's contents.
  >   - **A `slow-tests` CI workflow.** Weekly and on demand; not a required check.
  >   - **The threat model's B4 section.**

  > Note (P3.8, from group D): **testcontainers adds a thread.** Dropping a container from a current-thread runtime starts one process-wide cleanup thread, so the OS thread count's baseline must be taken after the container has started and been dropped once, or allow for that thread. `live_threads()` and `live_worlds()` don't count it.

  > Note (P3.8, from group E, the user's decisions) (ADR-0011):
  > - **"The full teardown"** is `McSession::disconnect()`, which follows ADR-0008 §10. "Not just `disconnect()`" means azalea's `Client::disconnect()`.
  > - **The scenario.** `slow_teardown_scenario` runs one warm-up cycle and three measured ones. Each cycle puts four bots online at once and ends them each way a session can end:
  >   - two with the full teardown
  >   - one kicked through RCON before its `disconnect()`
  >   - one whose handles are all dropped without `disconnect()` (P3.2)
  > - **Checks.**
  >   - After the warm-up and after every cycle, `live_threads()` and `live_worlds()` are 0 (they count only fleet-mc's own), and nothing is abandoned.
  >   - On Linux, the OS thread count reaches at most the baseline, within 30 s, longer than tokio's 10 s blocking-thread keep-alive. The baseline is taken after the warm-up, once no host thread is left in the OS's list.
  >   - Every check runs while the container is up, so the cleanup thread above never counts.
  > - **Red runs.**
  >   - A temporary, uncommitted break (the driver kept a `Client` clone) failed it on the Worlds.
  >   - In CI on Linux, a temporary commit parked one thread after the baseline. The run used a temporary `push` trigger on the branch, removed before the PR opened.
  > - **Found by that run: the baseline hid the parked thread.**
  >   - The first run passed when it had to fail. fleet-mc counts a host thread as ended just before its OS thread exits, so a warm-up thread that was still exiting inflated the baseline.
  >   - The baseline is now taken only once no thread named `mc-…` is left in `/proc/self/task`, and a failed check shows the threads by name.
  >   - The next run failed as it should ("8 OS threads … more than the 7 after the warm-up", the extra one under the test thread's name), and the revert passed.

  > Note (Phase 3 wrap-up, group E, the user's decisions) (ADR-0011):
  > - **The real-account check.**
  >   - `manual_real_account_scenario` joins the online-mode container with the user's real token, sends signed chat and sees its echo, and tears down.
  >   - The token must not appear in any log, and `PRIVATE KEY` only under `azalea_auth::certs` (azalea-auth 0.16.0 logs the certificate response at `trace`, which P5.2 caps).
  >   - Only the nextest `manual` profile includes `manual_` tests. Only the user runs `just test-real-account` (CLAUDE.md).
  >   - Errors name only a line number and the expected key, and a failed wait names only the step and event kinds. A rejected token says it may have expired, since `fetch-token` writes no expiry.
  >   - It also sends `/me` and expects the server to reject it: the known limit under P11.3. If the emote's echo arrives instead, the step fails with "azalea now signs commands: update this step and the P11 note".
  >   - The steps only record their results: the teardown and the log checks always run, and failed steps are reported after them. The first run failed at the signed chat (see P3.7's note) and skipped the log checks.
  >   - **The user's results** (2026-10-07, after the P3.7 fix):
  >     - passed: the join, the signed chat, the token absent from every log at every level, and `PRIVATE KEY` only under `azalea_auth::certs`
  >     - `/me` was rejected: the bot got one system message and stayed up, and the server logged at ERROR "Received unsigned command packet from AfkBot1, but the command requires signable arguments: …"
  > - **The `slow-tests` workflow** runs weekly (Mondays, 07:17 UTC) and on demand, on ubuntu; it isn't a required check.
  > - **The threat model's B4 section** is written. It adds two accepted risks: Minecraft servers aren't authenticated, and a server can grow a bot's memory.
  > - **The Phase 3 DoD** is checked in group E and again in P3.9's PR, which finishes the phase.
- [x] **P3.9** 🔴 `HoldUse{on}`: hold the use button down, the way `Sneak{on}` holds sneak (the user's idea, from P3.6). It's its own branch and PR, right after group E.
  - fleet-core: an additive `Action::HoldUse{on}` and `GameAction` variant, with validation and the mode snapshots updated. It's a new tag, so older servers reject modes that use it (ADR-0010).
  - fleet-mc: the mapping that P3.6 verified, with a live test.

  If P3.6 finds it isn't feasible, this task is closed with a note instead.

  > Note (P3.9, from group D): **Feasible.** P3.6's probe held a bow for 25 ticks with one `start_use_item` and shot the arrow on a raw `ServerboundPlayerAction{ReleaseUseItem}`, sent through azalea's normal packet writer. Releasing after about 1 tick shot nothing. Items without a use duration (block and entity clicks) would still need repeated packets (ADR-0011).

  > Note (P3.9, the user's decisions, 2026-10-07 and -08) (ADR-0010, ADR-0011):
  > - **Limits.** A repeating hold-use step needs at least 500 ms (`MIN_HOLD_USE_INTERVAL`), like an attack. Holding and letting go are limited apart, at most one at-start and one repeating step each, so a mode can shoot a bow again and again: draw at start, then every 2 s let go and draw again. It's never a command.
  > - **Order.** A use is queued for azalea's next game tick, but a release goes out at once, so a hold and a release in the same tick would reach the server the wrong way round.
  >   - `HoldUse{on: true}` and `UseItem` insert azalea's `StartUseItemQueued` directly, instead of calling `start_use_item`.
  >   - `HoldUse{on: false}` removes a use that's still queued, then always writes the release.
  >   - `UseItem` now returns `NotInWorld` once the bot has left its world, like the other actions; in game it behaves as before.
  > - **What ends a hold:** letting go, death and respawn, a hotbar slot change, the item finishing (food), and a disconnect, as the vanilla server handles them; only letting go is checked live. An at-start hold comes back after a reconnect but not after a respawn (flagged under P4.2). For a shield, look at the sky or into open air, since a use clicks a block or entity in reach.
  > - **Found: projectiles.** azalea doesn't move projectiles between the server's position updates, so right after a throw or a shot, a use can click the projectile in front of the bot instead of drawing. A temporary probe saw it. It's a known limit; the live step clears projectiles first.
  > - **Tests.**
  >   - Unit tests cover the release packet's bytes, and queuing and cancelling a use.
  >   - `slow_actions_scenario`'s `hold_use` step checks three things: on, then off right away, leaves nothing drawn; a full draw shoots nothing while held; the release shoots one arrow.
  >   - Red first: against a release that did nothing, and against an immediate release without the cancel, which shot an arrow from the bow it had left drawn.
  > - **Runs** (2026-10-08): `slow_actions_scenario` alone passed 10 of 10. Three full `just test-slow` runs passed every scenario (actions, offline, online, fault containment, teardown), 3 of 3 each.
  > - **A slot change and a hold in the same tick** *(from the PR review)*. An at-start "select a slot, then hold" runs in one tick on every join. azalea doesn't directly order its slot packet (`ensure_has_sent_carried_item`) against the use (`handle_start_use_item_queued`). If the slot packet reached the server after the use, it would end the hold.
  >   - The `hold_use` step now selects the bow's slot from slot 0 and holds with no wait between, waits a full draw, lets go, and expects one more arrow. The bow is only the instrument.
  >   - It passed 5 runs of 5 on its own, and one full `just test-slow` run.
  >   - A temporary probe, never committed, logged the bot's packets in 3 more runs. Both actions landed before the same tick, and in that tick `set_carried_item` went out about 0.05–0.1 ms before `use_item`.
  >   - That order is what azalea 0.16.0's schedule does today; no direct ordering constraint declares it. So the live step guards it at every azalea bump (ADR-0003).

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
**Introduces:** `governor`, `metrics`, `tokio-util` (only `CancellationToken`; Phase 3 didn't need it). Dev: `fleet-testkit`.

> Note (P4):
> - **Five group branches.** At the user's request, Phase 4 is built in group PRs like Phases 2 and 3. Each group has one branch and one commit per task, and the groups run in this order, each after the previous PR is merged:
>
>   | Group | Branch | Tasks |
>   |---|---|---|
>   | A | `p4/p4.1-p4.3-spec-and-credentials` | P4.1, P4.3 |
>   | B | `p4/p4.4-p4.5-chat-queue-and-mode-runner` | P4.5, then P4.4: mode chat goes through the queue. The runtime clock and the governor clock land in P4.5, their first user |
>   | C | `p4/p4.2-p4.6-bot-actor-and-watchdog` | P4.2, P4.6 |
>   | D | `p4/p4.7-supervisor-and-fleet` | P4.7 |
>   | E | `p4/p4.8-p4.9-metrics-chaos-and-wrap-up` | P4.9, P4.8 and the phase wrap-up |
>
>   The queue and the mode runner are built against the fake before the actor, so the actor PR wires real pieces instead of placeholders.
> - **Decisions.** The user answered the Phase 4 plan's open questions on 2026-10-08. [ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md) records them; the notes below summarize what changes a task.
> - **Dependencies.** `governor` without default features plus `std` (no quanta, dashmap or jitter, so no rand 0.9 or getrandom 0.3 in normal dependencies), `tokio-util` without features, `metrics`, and `chrono` in fleet-runtime for `DateTime` only. No `metrics-util`.
> - **Determinism.** `Fleet::new` takes a wall-clock anchor and a seed from its caller; the runtime never reads the wall clock or the OS's randomness. fleet-runtime's `clippy.toml` bans those entry points, so all time goes through tokio's clock, which paused-time tests control.

- [x] **P4.1** 🔴 `BotSpec` (account, server, mode, desired run state) and `BotSnapshot` (state, since, last disconnect reason, attempt, uptime).

  > Note (P4.1, from Phase 2): open question, flagged and not yet decided: fleet-proto and fleet-server also need to build a `BotSpec` (Appendix C `AssignBot`), but they may only depend on `fleet-core`.

  > Note (P4.1, from group B): **Conflict texts.** A bot's spec gets an optional list of kick texts that count as a duplicate login, empty by default. Some proxies kick with plain text when a human logs in; BungeeCord/Waterfall in online mode probably does. The classifier compares the sanitized kick message exactly against the list, so vanilla servers still go by translation key. The list's limits and config keys are decided here (ADR-0010).

  > Note (P4.1, from Phase 3, the user's request): **Chat format, flagged and not decided.** Next to the conflict texts, a server may later get a chat format for plugin chat that arrives as system messages, such as `[CLAN] Name : message`. It extracts the sender for display only:
  > - A parsed sender is marked as parsed from the text. Like every chat sender, it's never used for any permission or trigger decision.
  > - *(corrected in the group B review)* Chat senders are server-attributed, not verified. azalea 0.16 never verifies chat signatures, and fleet-mc shows a Player packet's `unsigned_content`, so even a Player packet's UUID and text are only what the server claims. A feature that needs a trustworthy sender must verify the signature and use the signed body, in its own ADR.
  > - fleet-mc keeps the whole sanitized text of system messages, so this stays possible (P3.5, ADR-0011).

  > Note (P4.1, from the Phase 4 plan, the user's decisions) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **Where.** `BotSpec` and `BotSnapshot` are pure data in `fleet_core::bot`, without serde, so fleet-proto and fleet-server can build them.
  > - **`BotSpec`** is `{ id, account, server, mode, desired, conflict_texts }`. The account is `BotAccount::Offline(McUsername)` or `BotAccount::Online(AccountId)`, and it never changes for a bot. The mode is the resolved `ModeDefinition`.
  > - **`BotSnapshot`** is `{ bot_id, state, since, last_disconnect }`; `attempt()` and `uptime(now)` are derived when read, so nothing goes stale between state changes.
  > - **Conflict texts.** `DisconnectReason::classify(&self)` stays unchanged, since fleet-mc calls it. `ConflictTexts::classify` applies the texts on top of it, for every kick that `classify()` calls Transient, whatever its key; a key that means Permanent, Conflict or AuthInvalid wins. `transition()` takes `&BotRules { retry, conflict_texts }` instead of `&RetryPolicy`.
  > - **Limits.** At most 16 texts of 1–1024 characters, compared exactly and case-sensitively, taken as written. A text with a character the kick sanitizer strips is rejected; errors name the index, never the text.
  > - **The chat format** is deferred to the server side (P11.9).
- [x] **P4.2** 🔴 `BotActor`:
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

  > Note (P4.2, from the group A review): **Half-open breaker.** `CircuitBreaker::permits` doesn't limit the half-open state to one attempt. The actor makes one attempt at a time and reports its outcome before it asks again.

  > Note (P4.2, from group B) (ADR-0010):
  > - **Breaker effects.** The actor runs `RecordFailure`, `RecordSuccess` and `ResetBreaker` on its `CircuitBreaker`, in order. Then for `ScheduleRetry{n}` it waits `RetryPolicy::delay(n)`, or until the breaker's cool-down ends if that's later.
  > - **Session events.** Only the current session's events reach `transition()`. Once `Disconnect` has run, the actor drops that session's events, and it sends `SessionClosed` for it only if no newer session has connected since.
  > - **Session requests.** Every `RequestSession` gets exactly one answer; a timeout becomes `SessionUnavailable{retryable: true}`.
  > - **Published events.** Every state change, plus `Died`, goes out as an event for the app (P4.7, P11). `Notify` is only for alerts that need a human.
  > - **Paused and Failed.** `Start` and `Stop` are no-ops there, so applying a spec's desired run state leaves them alone. A server change to a Paused or Failed bot takes effect at Resume or Reset.

  > Note (P4.2, from P3.9): **Not decided: re-apply holds after a respawn.** A death ends a `HoldUse` hold, and only a new join runs the at-start steps again. So an at-start hold comes back after a reconnect but not after a respawn (ADR-0011).

  > Note (P4.2, from the Phase 4 plan, the user's decisions) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **Holds after a respawn** aren't re-applied; that's a known limit. A repeating `HoldUse{on: true}` step does re-apply a hold after a death. P11.4 decides about "on respawn" steps.
  > - **A failed `respawn()`** is retried every 5 s while the session lives; the first failure logs at `warn`, the retries at `debug`. After 12 failed calls in a row the session ends with the new `DisconnectReason::RespawnFailed` (Transient, added to fleet-core in this task), so the bot reconnects and respawns on join.
  > - **Events.** `FleetEvent { bot_id, at, kind }` with `StateChanged(BotSnapshot)`, `Died`, `ChatReceived`, `ChatSent{ticket}` and `ChatFailed{ticket, reason}`, on one bounded `broadcast` per fleet.
  > - **The session-request timeout** is 30 s, a `RuntimeConfig` default.

  > Note (P4.2, from group B, the user's decisions) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **`FleetEvent` already exists** (P4.5), with every kind above plus `ModeChatSent{message}`.
  > - **Order.** At `StartMode` the actor opens the chat queue before it starts the mode runner. At `StopMode` or a disconnect it cancels the runner first and closes the queue after that, so mode chat never meets a closed queue. The runner and the queue's delivery get separate child tokens of the session's token.
  > - **Spans.** The actor spawns the queue's `ChatDelivery` and the `ModeRunner` instrumented with the bot's span; neither opens one of its own.

  > Note (P4.2, from group C, the user's decisions) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **Deviation: the inbox has no Start or Stop.** It takes one `BotCommand` per `Fleet` call: `UpdateSpec`, `Restart`, `Reset`, `Resume` and `SendChat` (with a `oneshot` reply). Start and Stop come only from the spec's desired state, when the actor starts and on every `UpdateSpec`, so the two can never disagree. `Restart` runs Stop then Start inside the actor, so a full inbox can't leave it half done, and it does nothing while the desired state is Stopped. "Stop during backoff" is an `UpdateSpec` with desired = Stopped.
  > - **API.** `BotActor::new(BotActorParts { spec, connector, credentials, retry, config, clock, rng, events, bucket, tickets, snapshot, breaker })` returns the actor and its `BotInbox` (`try_send`, `InboxError::{Full, Closed}`); `run(cancel)` returns an `ActorExit`. The caller owns the snapshot and breaker `watch`es: the breaker watch's current value is where the actor starts, and the actor writes a copy after each breaker effect. The `RetryPolicy` is passed in; `ResetBreaker` is `record_success()`, which leaves the breaker as a new one.
  > - **In-flight work.** The session request, the retry timer and the respawn retry are futures the actor owns and polls in its `select!`; leaving the state drops them, which cancels them. `connect()` is bounded by the connect timeout, a new `RuntimeConfig` field (Appendix A's key); running out is `ConnectFailed(TimedOut)`.
  > - **Teardown** runs as a task in the actor's `JoinSet`, so the actor keeps serving its inbox and timers. Each session has a generation; a finished teardown is `SessionClosed` only while no newer session has connected. It takes no timeout and no token of its own, because the port guarantees `disconnect()` finishes (the second exception in CLAUDE.md's task rule).
  > - **A server change or `Restart`** goes through `Stopping{restart}` while Connecting or Online. In AwaitingSession or Backoff it starts a new run at once, dropping the backoff, the pending request and the breaker's cool-down. A spec for another bot or account is ignored and logged at `error`.
  > - **Order.** The `select!` is biased: cancellation, finished tasks, inbox, session-request answer, retry timer, respawn retry, then the session's events last, so a chat flood can't hold up the rest. Before every input that ends a session (Restart, an `UpdateSpec` that changes the server or sets desired = Stopped, cancellation, a closed inbox, the respawn retry's last failure, a crash exit), the actor applies the session events that are already ready, at most `session_drain` = 67 (fleet-mc's 64 plus 3 lifecycle events). So a queued duplicate-login kick always pauses the bot first (§6 row 3).
  > - **Ending.** Cancellation or a closed inbox stops the bot through `transition()`, waits for every teardown and returns `ActorExit::Stopped`. A runner, delivery or teardown that panics is logged at `error` (the task, never the payload); the actor tears the session down without a state change and returns `ActorExit::TaskCrashed{task}`, so the last published state stays.
  > - **Respawn** retries 5 s after each failed call; `Closed` ends the retry at `debug` and never counts; each death warns on its first failure.
  > - **Events and logs.** `Notify` is published as the new `FleetEventKind::Alert(BotNotification)`, right after its `StateChanged`. No event for the starting state: the snapshot watch gets Stopped. `last_disconnect` changes only when a session ends on its own. State changes log at `info`; a session that ends on its own and is retried, a session-request timeout and a retryable credential error at `warn` (a kick's text only at `debug`); Paused at `warn`, Failed at `error`.
  > - **Tests.** fleet-testkit's fake session gained `fail_respawn`, `succeed_respawn` and `delay_disconnect`. Crashes come from a test-only wrapper in fleet-runtime's `tests/`, as for P4.8.

  > Note (P4.2, found and fixed in P4.7) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): **Reset and Resume ignored the desired state.** A Paused bot whose spec said desired = Stopped connected again on Resume, and a Failed one on Reset. Both now feed their event and then apply the desired state, as `UpdateSpec` does: with desired = Stopped the bot passes `AwaitingSession{1}` and ends Stopped, and the session request is dropped before the provider is ever asked. A regression test covers both.

  > Note (P4.2, found and fixed in P4.8) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): **A crashing actor stopped reading its inbox.** After its mode runner, chat delivery or a teardown panicked, the actor waited for its teardowns, which can take seconds, before it returned `TaskCrashed`. Meanwhile a `send_chat` forwarded to it ended `TimedOut` after 5 s, and a Reset, Resume or Restart was answered `Ok`, then lost with the actor: a resumed Paused bot came back Paused. The P4.8 plan found it.
  > - **The fix.** `crash()` now closes the inbox first, so the supervisor's next forward is refused at once (`Busy`), and drops the commands already queued, which answers a waiting `send_chat` with `Busy` too.
  > - **Known limit.** A command that reaches the inbox in the same instant as the crash is still lost, as with a panic of the actor itself. The chaos test treats a call in the same instant as a crash of that bot as possibly lost.
  > - **Test.** A regression test in `tests/fleet/supervisor.rs` first failed with `TimedOut` for `send_chat` and `Ok` for `resume`. It now checks that `send_chat`, `resume` and `restart` answer `Busy` without the clock moving.
- [x] **P4.3** 🔴 `SessionCredentialProvider` port:
  - In standalone mode it returns offline credentials.
  - In managed mode it asks the control plane (Phase 10).
  - On an expired token it refreshes once, then goes to `Failed(Auth)`.

  > Note (P4.3, from Phase 2): The "refresh once" is driven by the core state machine. The first `AuthInvalid` emits `RequestSession{fresh: true}`, and the provider must bypass any token cache for it. The second goes to `Failed(Auth)` (ADR-0010).

  > Note (P4.3, from the Phase 4 plan, the user's decisions) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): The port lives in `fleet_core::mc`, so fleet-testkit can provide its fake: `session(SessionRequest { bot_id, account, fresh })` returns `SessionCredentials` or `CredentialError { retryable }`. `OfflineCredentials` lives in fleet-runtime, the managed implementation in fleet-agent (P10.6). The runtime doesn't check `expires_at`.
- [x] **P4.4** 🔴 `ModeRunner` drives the core `ModePlan` through the `SessionHandle`.
  - A failed action is logged and skipped, never fatal.
  - It stops cleanly on disconnect or mode change.
  - Tests assert the timing on a paused clock.

  > Note (P4.4, from the Phase 4 plan, the user's decisions) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **Logging.** A failed action logs at `warn` for the first failure of each kind per session, then at `debug`.
  > - **A mode change** lets go first (`HoldUse{on:false}`, `Sneak{on:false}`), then starts the new plan with its at-start steps. The slot stays, and an equal definition changes nothing.

  > Note (P4.4, from group B, the user's decisions) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **Shape.** One `ModeRunner` task per Online session. The actor starts it at `StartMode` with a `CancellationToken` and a `watch::Receiver<ModeDefinition>`. A changed definition lets go and restarts the plan inside the same task, so the warn-once memory lasts the whole session. The runner ends at cancellation, when the session has ended (a call fails with `Closed`), or when the owner drops the `watch` sender.
  > - **Cancellation** interrupts a tick's actions; a mode change waits until they're done.
  > - **Logging.** Failed actions and refused mode chat (`RateLimited`, `QueueFull`) share the runner's warn-once log; the queue's delivery keeps its own for failed sends. *(the user's addition)* Mode chat that finds the queue closed (`NotOnline`) always logs at `debug`, so a disconnect can't produce a spurious warning. *(PR #15 review)* The same goes for a session call that fails with `Closed`: the runner stops at `debug`, and the delivery logs a mode-chat send the ended session refuses at `debug` too.
  > - **Timing.** The runner wakes at most once per sleep, and a late tick doesn't catch up (P2.8). tokio's timer rounds a deadline up to the next millisecond, which the jitter test allows for.
  > - **Tests** read log levels through a small level-recording subscriber in the test file: fleet-testkit's log capture doesn't record levels.
- [x] **P4.5** 🔴 Outbound chat queue:
  - bounded at 16, with one governor token bucket per bot
  - when the bucket or queue is full it returns `RateLimited` or `QueueFull` **instead of blocking**
  - user chat and mode chat share it
  - governor's clock is injected, so the tests run on controlled time (see §8)

  > Note (P4.5, from Phase 3, group E): **flagged, not decided.** `send_chat` can fail with `SessionError::ChatUnavailable` when an online session can't sign chat for a server that enforces it. The session stays up, and a later call may succeed. This task decides what the queue does then, for user chat and mode chat (ADR-0011).
  >
  > Mode chat with a command that takes message arguments (`/me`, `/msg`, `/tell`, `/w`, `/say`, `/teammsg`) is rejected by a server that enforces secure chat, while `send_chat` returns `Ok`. That's undecided here too; see P11.3's note.

  > Note (P4.5, from the Phase 4 plan, the user's decisions) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **Admission.** The bucket (one message per 3 s, burst 3) is checked when a message is queued. User chat while the bot isn't Online returns `NotOnline`.
  > - **Results.** `send_chat` returns a ticket once queued; the outcome comes later as `ChatSent` or `ChatFailed`. Queued messages fail with `ChatFailed{Disconnected}` on a disconnect. A failed send doesn't refund its token.
  > - **`ChatUnavailable`** drops the message: user chat gets `ChatFailed{ChatUnavailable}`, mode chat is logged at `debug` and skipped. No retry.
  > - **Mode chat** that's rate-limited or finds the queue full is skipped and logged.
  > - **Commands with message arguments** get nothing special here; P11.3's rule applies to mode chat too.

  > Note (P4.5, from the PR #14 review, the user's request): **Prove the chrono and rand bans.** fleet-runtime's `clippy.toml` lists chrono's and rand's clock and OS-randomness paths with `allow-invalid`, which would hide a path that doesn't resolve. When chrono and rand become fleet-runtime dependencies, show a red clippy run proving that each banned chrono and rand path fires. That means `chrono::Utc::now` and `Local::now`, rand's `rng`, `random`, `random_iter`, `random_range`, `random_bool`, `random_ratio`, `fill` and `make_rng`, and the `ThreadRng` and `SysRng` types. Use a temporary probe, never committed, with the features that make these paths exist enabled only for the probe, as ADR-0010's probe did (ADR-0013).

  > Note (P4.5, from group B, the user's decisions) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **Events now.** `FleetEvent { bot_id, at, kind }` is defined here with all its kinds, so the queue's `ChatDelivery` publishes `ChatSent` and `ChatFailed` on the fleet's `broadcast` itself, stamped by the `RuntimeClock`. A new kind, **`ModeChatSent{message}`**, reports mode chat that was sent; mode chat that isn't sent is only logged.
  > - **Tickets** come from one fleet-wide counter, `ChatTickets`, so a ticket never repeats while the agent runs, even across actor restarts.
  > - **`ChatFailure`** is `Disconnected`, `ChatUnavailable`, `TimedOut`, `SessionBusy` or `NotInWorld`. A message still queued at a close, or a send that finds the session `Closed`, is `Disconnected`.
  > - **Admission** reserves a queue slot before it takes a token, so a refused message uses up neither.
  > - **Mode chat that's refused** (`RateLimited`, `QueueFull`) or fails in the session logs at `warn` the first time per kind and session, then at `debug`. `ChatUnavailable` is always `debug`, since fleet-mc already warns once per session. *(PR #15 review)* So is `Closed`: it only means the session has ended, so a disconnect that catches mode chat in flight or still queued can't produce a spurious warning. The text is never logged.
  > - **The bucket** (`ChatBucket`) is shared and outlives the sessions, so a reconnect doesn't refill it, and the supervisor keeps it across actor restarts (P4.7).
  > - **Shape.** `ChatQueue::open(session, events, clock, cancel)` returns the session's `ChatDelivery`, which the actor spawns, and a `ModeChat` handle for the mode runner. `close()` cancels the delivery, which fails what's still queued. A send that's already running finishes, since every `SessionHandle` call has its own timeout. The settings are `RuntimeConfig` fields.
  > - **Naming.** The delivery task is `ChatDelivery`, not `ChatSender`: fleet-core's incoming chat already has a `ChatSender`.

  > Note (P4.5, from group B): **The bans are proven.** chrono arrived in P4.5 and rand in P4.4. A temporary probe, never committed, then enabled chrono's `clock` and rand's `thread_rng` (which brings `std_rng` and `sys_rng`) for fleet-runtime only and used each banned path. Clippy flagged all 12 with their reasons: `disallowed_methods` for `chrono::Utc::now`, `chrono::Local::now`, `rand::rng`, `random`, `random_iter`, `random_range`, `random_bool`, `random_ratio`, `fill` and `make_rng`, and `disallowed_types` for `rand::rngs::ThreadRng` and `SysRng`. The files were restored afterwards.
- [x] **P4.6** 🔴 Watchdog, while Online (fault table in ADR-0008 §5):
  - no `Tick` for `watchdog_timeout`: raise `WatchdogTimeout`, tear the session down and reconnect
  - no packet from the server for `packet_liveness_timeout`: tear the session down and treat it as a transient disconnect

  Both timeouts come from `[runtime]` in the agent config (Appendix A).

  > Note (P4.6, from group E, the user's decision): The session's `Liveness` holds the two stamps, and the comparison lives here. When both stamps are stale, the tick stall wins: `WatchdogTimeout`, not `LivenessTimeout`, because a hung host thread stops both (ADR-0010).

  > Note (P4.6, from the Phase 4 plan) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): The watchdog checks every 1 s, a `RuntimeConfig` default.

  > Note (P4.6, from group C, the user's decisions) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **In the actor.** The watchdog is a timer in the actor's `select!`, armed when the bot goes Online and dropped when it leaves. The first check comes 1 s after the join, and each check arms the next one a period later.
  > - **Stale** means at least the timeout old: with 30 s and a 1 s period, a stall is caught 30–31 s after the last tick or packet. The check reads tokio's clock as a std `Instant` and uses `saturating_duration_since`, never the banned `elapsed()`. A tick stall is `WatchdogTimeout`, a packet stall `Disconnected(LivenessTimeout)`; when both are stale the tick wins.
  > - **Ready session events go first,** as before every session-ending input (P4.2): a queued duplicate-login kick pauses the bot instead of a trip that would reconnect it.
  > - **Settings.** `watchdog_timeout` and `packet_liveness_timeout` (30 s each) are `RuntimeConfig` fields for Appendix A's keys; `watchdog_period` (1 s) is a default only, and a zero period counts as 1 ms. A trip is recorded in `last_disconnect` and logs at `warn` like any session that ends on its own.
  > - **Tests** are in `tests/bot_actor.rs`, next to the actor's helpers, instead of a separate file: sharing those helpers through `tests/common` would leave them unused in the other test files.
- [x] **P4.7** 🔴 `Supervisor` and `Fleet` handle:
  - actors run in a `JoinSet` or `TaskTracker`
  - panics are detected and the actor restarted, within the intensity limit
  - API: `apply(spec)`, `remove(id)`, `send_chat`, `snapshot_all`, `subscribe`
  - graceful shutdown: cancel, then disconnect every bot within the timeout

  > Note (P4.7, from Phase 2):
  > - **Restart limit.** "5 restarts in 10 min" uses the core `FailureWindow` and ends with the core `CrashLoop` event (ADR-0010).
  > - **Open question, flagged and not yet decided.** `Paused` (a human is playing) and `Failed` must survive an actor or agent restart. Otherwise a fresh `Start` kicks the human (§6 row 3).

  > Note (P4.7, from Phase 3): The abandoned-thread limit lives in fleet-mc: above it, `connect()` returns `HostUnavailable`. Ending the agent process at the limit is P5.3's job (ADR-0011).

  > Note (P4.7, from the Phase 4 plan, the user's decisions) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **Inside.** Actors run in tokio's `JoinSet` with child `CancellationToken`s. The `Fleet` handle talks to the supervisor task over a bounded queue with a reply timeout.
  > - **API.** Also `reset`, `resume`, `restart` (a no-op when desired = Stopped), `snapshot(id)` and `shutdown(timeout)`; bots start and stop only through the desired state. `apply(spec, restore: Option<StickyState>)` refuses a new bot above `max_bots` (`AtCapacity`) and a changed account (`AccountChanged`). `remove` returns once accepted; until the `Removed` event, `apply` for that bot returns `Busy`.
  > - **Restarts never skip the backoff.** The supervisor keeps each bot's spec, snapshot, restart window and a copy of its circuit breaker, which the actor publishes after each breaker effect. A pure `fleet_core::bot::restore` turns AwaitingSession, Connecting, Online and Backoff into `Backoff{attempt}` with `ScheduleRetry` (`Backoff{1}` after a stable Online); Paused, Failed and Stopped stay, and Stopping becomes Stopped. A panic isn't a breaker failure.
  > - **Crash loop.** The 6th panic within 10 min starts the actor once more with `CrashLoop` first. If it panics again, the supervisor publishes `Failed(CrashLoop)` itself and starts an actor only on Reset.
  > - **Agent restarts.** The server is the source of truth (P10.5); the standalone agent persists nothing, a documented limit.

  > Note (P4.7, from group B, the user's decision) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): **The chat bucket survives a restart.** The supervisor keeps each bot's `ChatBucket`, like its breaker copy, and hands the same one to a restarted actor, so a crash doesn't refill it. It also owns the fleet's `ChatTickets` and gives every actor a clone, so a ticket never repeats.

  > Note (P4.7, from group C, the user's decisions) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **A crashed task counts like a panic.** When the actor's mode runner, chat delivery or a teardown panics, `run()` returns `ActorExit::TaskCrashed{task}` instead of panicking itself. The supervisor counts that in the restart window exactly like a panic, so it restarts with the backoff and ends in `CrashLoop` after the 6th within 10 min. The actor leaves its last published state, so `restore()` works on it as after a real panic.
  > - **The actor's inbox** takes one `BotCommand` per `Fleet` call: `UpdateSpec`, `Restart`, `Reset`, `Resume` and `SendChat`. Bots start and stop only through the spec's desired state; `restart` is the `Restart` command, which the actor ignores while desired = Stopped.
  > - **The supervisor owns the `watch`es.** It creates each bot's snapshot and breaker `watch` and passes the senders in `BotActorParts`; the breaker watch's current value is where a new actor starts.
  > - **A restored starting point.** `BotActorParts` will need one for a restarted actor: the `Transition` from `fleet_core::bot::restore()` (e.g. Backoff with `ScheduleRetry`), or a sticky Paused or Failed from `apply`'s restore. Today the actor starts Stopped and applies the desired state. P4.7 decides what the actor publishes for a restored state.

  > Note (P4.7, from group D, the user's decisions) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **Shape.** `Fleet::new(FleetParts)` returns `(Fleet, Supervisor)` after checking the chat quota and every channel capacity against tokio's limits (`FleetSetupError`). The caller runs `supervisor.run(cancel)` in a task it owns; a cancelled token or every `Fleet` dropped shuts down within `RuntimeConfig::shutdown_timeout` (10 s).
  > - **Calls.** A full supervisor queue is `Busy` at once, no answer within 5 s is `TimedOut`, and an ended supervisor is `ShuttingDown`. One `BotCommand` per call by `try_send`; a refused one is `Busy` and stores no spec. `send_chat` returns `SendChatError { Fleet, Chat }`.
  > - **`apply`.** New: `AccountInUse` when a live bot's account clashes (`BotAccount::clashes_with`: offline names ignore ASCII case), `Busy` when only a removing bot holds it, `AtCapacity` at `max_bots` (every known bot counts until `Removed`). Known: `AccountChanged` compares exactly, an equal spec isn't forwarded, a restore is ignored. `StickyState` lives in `fleet_core::bot`.
  > - **Starting points.** `BotActorParts.start: Transition`: Stopped, a sticky state, `restore()`, or `CrashLoop` on the restored state. Only a start state that differs from the `watch` is published (a sticky start without an `Alert`); `last_disconnect` carries over. `restore()` of a stable Online also records a success.
  > - **Crashes.** A panic, a `TaskCrashed` and an unexpected exit (logged at `error`) count in the window; a restart logs at `warn` in the bot's span, the crash loop at `error`. After the crash-loop actor crashes too, calls act like a Failed actor's, and `reset` starts a new one. The window is cleared only by a `reset` while the bot is crash-looped: its actor was started for a crash loop, or it has none. *(PR #17 review)* That goes by the supervisor's own record, since a Reset can come before the crash-loop actor has published `Failed(CrashLoop)`.
  > - **Removal** publishes `StateChanged(Stopped)` only for a bot its actor stops through `transition()`; a Paused, Failed or crash-looped bot gets only `Removed`, so P10.5 keeps the server's stored state. While removing: `apply` is `Busy`, the other commands `UnknownBot`, a second `remove` `Ok`, and reads still show the bot.
  > - **Shutdown** returns `ShutdownReport { stopped, aborted, crashed }`; calls meanwhile are `ShuttingDown`, and an accepted removal still publishes `Removed`.
  > - **Tests** are the folder crate `tests/fleet/` (`main.rs`, `panicky.rs`, `supervisor.rs`).
- [x] **P4.8** 🔴 **Chaos property test.** Random sequences of these events, run with paused time:
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

  > Note (P4.8, from the Phase 4 plan, the user's decisions) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): A fixed 500 cases (`with_cases(500)`), as an exception to `PROPTEST_CASES`. Panics come from a test-only connector wrapper in fleet-runtime's `tests/`; the fakes stay panic-free. The storm invariant holds across actor panics too, with no exception for connects after a restart.

  > Note (P4.8, from group C, the user's decision) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): **Deliberate restarts start a new run.** A `Restart` or a server change starts a new run at once, in AwaitingSession and Backoff too, dropping the backoff and the breaker's cool-down, as Start, Reset and Resume already do. So the storm invariant must leave the connects of a deliberate new run out of its bound.

  > Note (P4.8, from group D, the user's decision) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): The chaos test becomes `tests/fleet/chaos.rs` in the fleet test crate and reuses `tests/fleet/panicky.rs`, whose `PanickyConnector` panics on chosen connects (the actor itself) or in a session's `perform` or `disconnect` (`TaskCrashed`).

  > Note (P4.8, from group E, the user's decisions) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **Cases.**
  >   - **Size.** 1–3 bots (`AfkBot1`–`AfkBot3`), 1–30 steps, and 0–90 s of paused time after each step. Settings are Appendix A's defaults.
  >   - **Faults:**
  >     - transient, permanent and duplicate-login kicks
  >     - failed connects (`Refused`, `Unresolvable`, `Other`, `HostUnavailable`), a connect that never joins (the test fails it after the connect timeout, as fleet-mc would), auth rejections, retryable, refused and stuck credentials
  >     - a hung session, a dead link, a tick stall, and teardowns of 0–20 s, so new sessions often connect while an old one still tears down
  >     - panics in connect, perform or teardown, and a crash burst of 1–8 panics on the next connects; at most 8 crashes per bot
  >   - **Calls:** spec updates (`afk` ↔ a cheap mode with a jump every 5 s and chat every 60 s, two placeholder servers, the desired state), Restart, Reset, Resume, `send_chat`, `snapshot`, `snapshot_all`, and remove plus re-add. The re-add works as P10.5 does: it retries on `Busy` until `Removed`, and restores a Paused or Failed last state.
  > - **Per-bot scripts in the test.** `panicky.rs` scripts connects and panics per bot and logs every connect and panic. `credentials.rs` (`ScriptedCredentials`) answers per bot. A "server" task joins each new session unless the bot's script says otherwise. fleet-testkit is unchanged.
  > - **The model.**
  >   - A fault counts only once it has landed: the event was queued, the scripted answer used, or the panic fired.
  >   - A bot leaves the class that must end Online when a non-transient fault lands, and comes back on a Reset (Failed) or Resume (Paused) answered `Ok`. A call in the same instant as a crash of its bot doesn't bring it back (see the P4.2 note).
  > - **Invariants:**
  >   - **No panic escapes:** the supervisor's task ends without a panic at the final shutdown.
  >   - **Healing:** a bot that should run, with only transient faults and at most 5 fired crashes, ends Online; with 6 or more it may end `Failed(CrashLoop)`.
  >   - **No storms,** on the events and the connect log:
  >     - `AwaitingSession{1}` only after a deliberate call.
  >     - `AwaitingSession{m≥2}` only after `Backoff{m−1}`, at least `bounds(m−1).0` later.
  >     - `Backoff{n}` keeps the attempt it came from, or is `Backoff{1}` after a stable Online.
  >     - Each connect follows its own published `Connecting`, so none while Paused or Failed. The fresh-token retry is allowed.
  >     - A `Lagged` on the test's receiver or the connector's fails the case.
  >   - **The API answers** before the clock moves and never `TimedOut`. `Busy` is allowed. The final shutdown answers within its timeout plus the reply timeout, and every call after it answers `ShuttingDown`.
  >   - **Metrics:** the bots gauge equals `snapshot_all` after settling and is 0 after the shutdown. The reconnect counter equals the published `Connecting`s with attempt > 1 or `auth_retried`.
  > - **Settling.** No new faults are injected; the scripted ones run out. It ends once every bot is in a state it stays in, with no stalled session left and no perform panic waiting on an Online bot. The cap is each scripted fault's worst wait (max backoff, breaker cool-down, timeouts, the slowest teardown) times the number of faults plus 3.
  > - **Probes** *(added during the build, the user's decision)*. Every bot the test holds and isn't removing gets a `send_chat` every 5 s, during the pauses and the settling. Any answer but `TimedOut`, `ShuttingDown` or `UnknownBot` passes, if it comes before the clock moves. Without them, the test passed all 500 cases with the crash-gap fix reverted (P4.2's note), because random steps rarely call a bot whose crashed actor is still tearing down. The probes cost about 1.5 s.
  > - **Coverage.**
  >   - The test runs exactly 500 cases (`Config::with_cases(500)`, plus any seeds in the regressions file), and `PROPTEST_CASES=1000` still runs 500.
  >   - It prints how many cases reached each target and fails if one is 0. Targets: a crash loop, a duplicate-login pause, the fresh-token retry, a sticky re-add, a teardown running at a connect, a tick trip, a packet trip, and *(the user's addition)* a probe while a session whose `perform` panicked is still inside its `disconnect()`, from the connector's own log.
  >   - A typical run: crash loop 51, pause 134, fresh token 92, sticky re-add 48, teardown at a connect 58, tick trip 163, packet trip 124, probe in a crash teardown 65. To lift the last one, perform panics and slow teardowns weigh 6 instead of 3 and 4, and the probes run every 5 s instead of only after each step.
  >   - Failures persist next to the file, as `tests/fleet/chaos.proptest-regressions`.
  > - **Red runs** (temporary, uncommitted breaks, each restored afterwards):
  >   - **Without the crash-gap fix:** "a Fleet call took time to answer", shrunk to one bot with `SlowTeardown(6)`, `Panic(Perform)` and a transient kick, all at pause 0. A probe reached the actor while it waited for its teardown.
  >   - **`restore()` returning `Backoff{1}` for every state:** "Backoff{1} after Connecting { attempt: 3, … }", shrunk to one bot with a retryable credential refusal, a server change, stuck credentials and a connect panic.
  >   - **`restore()` without `ScheduleRetry`:** "the fleet didn't settle within 5240s", shrunk to one bot with a crash burst of 1.
  > - **Time.** The chaos test takes about 7–7.7 s, and `just test fleet-runtime` (288 tests) about 7.9 s.
  > - **Helpers.** `awaiting`, `PAUSED` and `DUPLICATE_LOGIN` moved back from `harness.rs` to `supervisor.rs`, the only file that uses them.
- [x] **P4.9** Metrics: bots per state, reconnects, watchdog trips, actor restarts.

  > Note (P4.9, from Phase 3): Also export fleet-mc's diagnostics: live and abandoned host threads, live Worlds, and dropped chat (ADR-0011).

  > Note (P4.9, from Phase 3, group B): `abandoned_threads()`, `dropped_chat()` and `ignored_action_bar()` only count up, so they're counters; `live_threads()` and `live_worlds()` are gauges (ADR-0011).

  > Note (P4.9, from the Phase 4 plan, the user's decisions) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): The metrics are `afkfleet_bots{state}`, `afkfleet_bot_reconnects_total`, `afkfleet_watchdog_trips_total{kind}` and `afkfleet_actor_restarts_total`, with no `bot_id` label. Tests install a hand-written recorder per test with metrics' thread-local `set_default_local_recorder`. **fleet-mc's diagnostics move to P5.3:** fleet-runtime may depend only on fleet-core (§4), so the two notes above are the agent's job.

  > Note (P4.9, from group E, the user's decisions) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **The bots gauge** changes with every snapshot write:
  >   - The supervisor's `Entry` holds a `SnapshotOwner`, which isn't `Clone`. It counts the bot in its first state, and its `Drop` takes the bot out of its current one, so removing a bot or ending the supervisor does too.
  >   - Actors publish through a clonable `SnapshotPublisher`, which `BotActorParts.snapshot` now takes instead of the raw `watch::Sender`. Each publish moves the bot from the old state's label to the new one. Dropping a publisher changes nothing.
  >   - So no write can skip the gauge. P4.8's chaos test checks it against `snapshot_all` too.
  > - **What counts:**
  >   - **A reconnect:** every connect that isn't the first of a deliberate run (`attempt > 1 || auth_retried`), the fresh-token retry and failed connects included.
  >   - **A watchdog trip:** only one that ends the session, after the drained events. It's labelled `kind="tick"` or `kind="packet"`.
  >   - **An actor restart:** every actor the supervisor starts after a crash, the crash-loop start included. A new bot, a Reset, and a crash during removal, during a shutdown or after a crash loop don't count.
  > - **Labels.** The 8 `state` labels come from a private, exhaustive match in fleet-runtime with no catch-all arm: `stopped`, `awaiting_session`, `connecting`, `online`, `backoff`, `paused`, `failed`, `stopping`. A new `BotState` variant doesn't compile without one, and fleet-core is unchanged.
  > - **Registration.** `Fleet::new` describes the four metrics and registers every series at 0, in the recorder installed at that moment (see P5.3).
  > - **Dependency.** `metrics` 0.24.6, whose only dependency is `rapidhash`.
  > - **Tests:**
  >   - `tests/fleet/metrics.rs` uses a hand-written recorder (`tests/fleet/recorder.rs`). The helpers it shares with `supervisor.rs` moved to `tests/fleet/harness.rs`.
  >   - Three cases the `Fleet` can't reach are unit tests, with a second, `cfg(test)` recorder in `src/metrics.rs`: a crash of the crash-loop actor, the supervisor's own `Failed(CrashLoop)`, and a trip that the drained events overtake.
  >   - Two temporary mutations, never committed, showed that the "doesn't count" tests catch the bug: counting a trip before the drain, and counting every crash as a restart.

  > Note (Phase 4 wrap-up, group E) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **The Phase 4 DoD, checked on 2026-10-09:**
  >   - **Coverage ≥ 85 %:** `just cov` gives fleet-runtime 98.25 % of lines (4666/4749), and the workspace 98.94 %; every gate passes.
  >   - **The chaos test passes 500 cases:** it asserts the count itself, and still runs 500 with `PROPTEST_CASES=1000`.
  >   - **`just test fleet-runtime` under 30 s:** 288 tests in about 7.9 s, the chaos test about 7.5 s of it.
  > - **The phase's security list:**
  >   - **Every channel is bounded:** the supervisor queue, the actor inboxes and the chat queues are `mpsc::channel`s with a capacity from `RuntimeConfig`, checked against tokio's limits; the events are one bounded `broadcast`; `watch` and `oneshot` hold one value. clippy bans `unbounded_channel`.
  >   - **Chat only through a validated `ChatMessage`:** `Fleet::send_chat`, the queue and mode chat all take `fleet_core::chat::ChatMessage`.
  >   - **No `unwrap`:** none, nor `expect` or `panic!`, outside test modules; the lints deny them.
  > - **Later phases inherit** four new notes, in P5.3 and P5.6.

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

> Note (P5):
> - **Four group branches.** At the user's request, Phase 5 is built in group PRs like Phases 2–4. Each group has one branch and one commit per task, and the groups run in this order, each after the previous PR is merged:
>
>   | Group | Branch | Tasks |
>   |---|---|---|
>   | A | `p5/p5.1-p5.2-config-and-telemetry` | P5.1, P5.2. fleet-agent is a library only, without a binary or a placeholder command |
>   | B | `p5/p5.3-p5.4-wiring-and-shutdown` | P5.3, P5.4: `afkfleet-agent run`, `just dev-agent`, `deploy/dev/agent.toml` |
>   | C | `p5/p5.5-p5.6-healthcheck-and-docker` | P5.5, P5.6: the healthcheck exists for the container |
>   | D | `p5/p5.7-e2e-and-wrap-up` | P5.7, the DoD demo and the phase wrap-up |
>
>   Each later group asks its own implementation-level questions in its session.
> - **Decisions.** The user answered the Phase 5 plan's open questions on 2026-10-09. [ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md) records them; the notes below summarize what changes a task.
> - **Dependencies.** `figment` (`toml`, `env`; `test` for `Jail` in tests), `garde` with `derive`, `tracing-subscriber` with `env-filter` and `json`, and `log` as a fleet-agent dev-dependency (group A).

- [x] **P5.1** 🔴 Config (Appendix A):
  - loaded with figment from TOML plus `AFKFLEET_AGENT__…` env variables
  - validated with garde, with clear error messages
  - `[standalone]` and `[control_plane]` are mutually exclusive
  - standalone mode only allows **offline** accounts

  > Note (P5.1, from group C): `mode = "afk"` and `mode = "farm"` in `[[standalone.bots]]` map to `ModeDefinition::afk()` and `ModeDefinition::farm()`. Core has no lookup by name; this task adds it (ADR-0010).

  > Note (P5.1, from Phase 3): `[runtime]` gains `connect_timeout_secs` (default 30), which goes into `ConnectParams`, and `max_abandoned_threads` (default 3), which goes to `McHostPool` (Appendix A, ADR-0011).

  > Note (P5.1, from Phase 4) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): `[[standalone.bots]]` gains `conflict_texts = […]`, kick texts that count as a duplicate login; a missing key means an empty list (`ConflictTexts`, at most 16 entries of 1–1024 characters). P5 also supplies the runtime's wall-clock anchor and seed to `Fleet::new`.

  > Note (P5.1, from P4.7) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): `[runtime] max_bots` and `shutdown_timeout_secs` map to `RuntimeConfig::max_bots` and `shutdown_timeout`. Two `[[standalone.bots]]` entries whose accounts clash are refused with a clear message, using `BotAccount::clashes_with` (offline names compare ignoring ASCII case), the same rule as the runtime's `AccountInUse`.

  > Note (P5.1, from the Phase 5 plan, the user's decisions) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)):
  > - **Loading.** The file must exist at exactly the given path: figment's `Toml::file` would search parent directories and treat a missing file as empty, so `load` checks first and uses `Toml::file_exact`. `AFKFLEET_AGENT__…` variables override any key.
  > - **Unknown keys are errors,** in the file and in the environment (`deny_unknown_fields`, and no `flatten`, which would silently turn it off). `[standalone]` and `[control_plane]` are two optional fields, and exactly one must be present.
  > - **Optional keys.** Every `[runtime]` and `[retry]` key takes Appendix A's value when it's missing. `name` is required, and so are each bot's `username`, `server` and `mode`. `heartbeat_file` defaults to the OS temp directory's `afkfleet-agent.alive`, so `just dev-agent` works on Windows too.
  > - **Ranges:**
  >   - `[runtime]`: `max_bots` 1–1000, the watchdog and liveness timeouts 5–600 s, the connect timeout 5–300 s, `max_abandoned_threads` 1–100, the shutdown timeout 1–300 s.
  >   - `[retry]`: the base delay 1–3600 s, the max delay 1–86 400 s (and at least twice the base), `stable_after_secs` **60**–86 400 s, `circuit_failures` 1–1000, the circuit window and cool-down 1–86 400 s.
  >   - `stable_after_secs` starts at a minute, so a server that kicks the bot a few seconds after every join can't cause endless reconnects at the base delay, with the breaker never opening.
  > - **`name`** is the new `fleet_core::value::AgentName`: 1–64 ASCII letters, digits, `-`, `_` and `.`, starting with a letter or digit. P10 sends it to the server.
  > - **`heartbeat_file`** must be absolute. **`[control_plane]`** keys are typed and non-empty; P10 checks the URL and the files.
  > - **Bots.**
  >   - 1 to `max_bots` entries.
  >   - Accounts that clash are refused on the later entry.
  >   - Each entry is offline by construction: it only has a `username`.
  >   - Entries get no `BotId`; P5.3 mints one at every start.
  > - **Modes by name:** the new `fleet_core::mode::ModePreset { Afk, Farm }`, with exact lowercase names. Its error lists the valid names and never the input.
  > - **Errors.**
  >   - Parse errors (bad TOML, a wrong type, an unknown key) stop at the first. They name the key and the file or the environment variable; a TOML syntax error shows only its position.
  >   - Validation then lists every problem at once, sorted by key, without echoing values.
  >   - So texts are read as `String`s and required keys as `Option`s, and converted with the core constructors in the validation pass.

  > Note (P5.1, as built, group A) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)):
  > - **Shape.** `fleet_agent::config::load(path) -> Result<AgentConfig, ConfigError>`.
  >   - `AgentConfig { name, runtime: RuntimeConfig, mc: McConfig, retry, circuit, heartbeat_file, mode: AgentMode }`, where `AgentMode` is `Standalone(Vec<StandaloneBot>)` or `ControlPlane(ControlPlaneConfig)`.
  >   - `ConfigError` is `NotFound { path }`, `Parse(Box<ParseError>)` or `Invalid(Problems)`. Each `Problem` has a `KeyPath` and a `ProblemKind`.
  > - **Deviation: garde checks only the ranges.** The texts go through fleet-core's constructors in a second pass, so every problem is reported at once.
  > - **figment's error is never kept or printed.** Its own text shows a `default.` profile prefix, and for environment variables a key that isn't the variable's name. `ParseError` rebuilds the variable's name from the key path.
  > - **Environment limits.** A value that reads as a number can't fill a text key: `AFKFLEET_AGENT__NAME=123` fails as a wrong type, so quote it as `'"123"'`. `[[standalone.bots]]` can only be replaced as a whole.
  > - **A lint exception in tests** (the user's approval). figment's `Jail` fixes its closure's error type to `figment::Error`, which is larger than `clippy::result_large_err` allows, so every config test goes through one helper with an `#[expect]`.
  > - **Tests:** `tests/config.rs` (39 cases, each in `figment::Jail`) plus unit tests of the error conversion and the key mapping. They were red against stubs first, as were `ModePreset`'s and `AgentName`'s.
- [ ] **P5.2** Telemetry: pretty logs in dev and JSON in prod, an env filter, and a panic hook that logs through `tracing`.

  > Note (P5.2, from Phase 3, group B review): **azalea's log targets stay at `warn`** in the default filter. azalea_client's disconnect plugin formats kick reasons at `info`, with azalea's own rendering, which grows exponentially on hostile nested translations (P3.5) and panics on a `%0$s` placeholder, since it computes `d - 1` on an unsigned digit with overflow checks on. A disabled level never formats, so neither can happen (ADR-0011).

  > Note (P5.2, from Phase 3, group C, the user's decisions): **`azalea_auth` is capped at `info`**, whatever the operator's filter says. `azalea_auth::certs` logs the whole chat-signing certificate response at `trace`, private key included, in every online session. It's the same cap as fleet-server's (P9.3), and a test checks it (ADR-0011, threat model).

  > Note (P5.2, from the Phase 5 plan, the user's decisions) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)):
  > - **A new `[log]` section** (Appendix A): `format = "json" | "pretty"` (default `json`) and `filter` (EnvFilter syntax, default `info`). `AFKFLEET_AGENT__LOG__…` overrides them; `RUST_LOG` has no effect. Logs go to stdout.
  > - **JSON** puts the event's fields at the top level (`flatten_event`), with `span` and `spans`. CLAUDE.md forbids field names that would collide. **Pretty** is the single-line format, with colors only on a terminal.
  > - **azalea's levels.** The filter is `(EnvFilter ∧ azalea cap ∧ azalea_auth cap) ∨ panic target`:
  >   - EnvFilter gets `azalea=warn` in front unless the operator writes a plain `azalea` directive.
  >   - A separate cap holds every `azalea…` target at `warn` unless an operator directive names it, so a target-free span directive like `[mc_session]=debug` can't lift azalea.
  >   - `azalea_auth` stays at `info`.
  >   - Panic reports always get through.
  >   - One startup warning when the operator names an azalea target above `warn`.
  > - **The panic hook** replaces the default one and logs one `error` event with the location, the thread and the payload.
  >   - The payload is untrusted. It goes through fleet-core's new `text::sanitize_untrusted`, which makes each line break ` | `, and is cut at 1024 characters with ` [truncated]` appended.
  >   - A backtrace is included only when `RUST_BACKTRACE` asks for one.
  > - **The `log` bridge** (reqwest, rustls, hickory) is checked by a test: filters and caps see a `log` record's real target.
- [ ] **P5.3** Wiring: `McHostPool` + `AzaleaConnector` + `Fleet`, with the standalone spec source.

  > Note (P5.3, from Phase 3): When `McHostPool::abandoned_threads()` reaches `max_abandoned_threads`, the agent shuts down and exits with an error, so Docker restarts it (§6 row 8). The library never ends the process itself (ADR-0011).

  > Note (P5.3, from Phase 4) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **fleet-mc's diagnostics** are exported here, since fleet-runtime can't read them: `abandoned_threads()`, `dropped_chat()` and `ignored_action_bar()` as counters, `live_threads()` and `live_worlds()` as gauges.
  > - **Paused and Failed across an agent restart.** The standalone agent persists nothing, so a restart starts every bot again; that's a documented limit for a dev-only mode with offline accounts. In managed mode the server sends the sticky state (P10.5).

  > Note (P5.3, from P4.7) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): `Fleet::new` returns the handle and the `Supervisor`; the agent runs `supervisor.run(cancel)` in a task it owns and reports a `FleetSetupError` at startup.

  > Note (P5.3, from Phase 4, group E, the user's decisions) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **A real seed.** The agent passes `Fleet::new` a random seed from `getrandom`, never a fixed one, so two agents never jitter their reconnects in lockstep.
  > - **An unexpected end of the supervisor.** If the supervisor's task ends without a requested shutdown (a `JoinError` or an early return), the agent logs it at `error` and exits with an error code, so Docker restarts it, as at the abandoned-thread limit.
  > - **The recorder first.** The agent installs the Prometheus recorder before it calls `Fleet::new`. The `metrics` crate sends the descriptions and the 0-series to the recorder installed at that moment; anything registered earlier goes to the no-op recorder and is lost (P4.9).

  > Note (P5.3, from the Phase 5 plan, the user's decisions) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)):
  > - **`run` is generic** over the connector, an agent-side `HostDiagnostics` trait (fleet-mc's five numbers) and a shutdown future. Tests use fleet-testkit's `FakeConnector` and fakes on paused time, so the exits at the abandoned-thread limit and at the supervisor's end are tested.
  > - **Bot IDs.** Each standalone bot gets a random `BotId::new_v7(anchor, getrandom bytes)` at every start. One `info` line per bot logs its ID with its username, server and mode.
  > - **Metrics:** `install_recorder()` only, with no listener and no port; P12.4 adds the endpoint.
  > - **Exit codes,** listed in the README and in `--help`:
  >   - 0: clean shutdown after a signal
  >   - 1: startup error
  >   - 2: clap's usage errors
  >   - 3: abandoned-thread limit
  >   - 4: the supervisor ended unasked
  > - **Fleet events.** One task logs `ChatReceived` and `ModeChatSent` at `debug`, and `Lagged` at `debug` with its count. Nothing else.
  > - `run` refuses `[control_plane]` until P10.
- [ ] **P5.4** 🔴 Signals (Ctrl+C, SIGTERM) trigger a graceful shutdown within `shutdown_timeout`.

  > Note (P5.4, from P4.7) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): A signal calls `Fleet::shutdown(shutdown_timeout)`, or cancels the supervisor's token, which shuts down within `RuntimeConfig::shutdown_timeout`. `shutdown` returns a `ShutdownReport { stopped, aborted, crashed }` for the log.

  > Note (P5.4, from the Phase 5 plan, the user's decisions) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)): A signal calls `Fleet::shutdown(shutdown_timeout)` and logs the report. A second signal only logs that the shutdown is already running. Signals: SIGTERM and SIGINT on Linux, Ctrl+C and Ctrl+Break on Windows.
- [ ] **P5.5** 🔴 A `healthcheck` subcommand. The agent touches a heartbeat file every 10 s, and the check fails when the file is stale. This works in distroless images, which have no curl.

  > Note (P5.5, from the Phase 5 plan, the user's decisions) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)):
  > - **Heartbeat.** The file is touched every 10 s, and only when `fleet.snapshot_all()` returns `Ok`. `Busy`, `TimedOut` and `ShuttingDown` never touch it: a hung supervisor's queue fills with timed-out calls, after which every call answers `Busy` at once. A test covers that case.
  > - **`healthcheck --config <path>`** loads the same config. The file is stale when it's missing or 30 s old or more.
  > - It exits only 0 or 1, since Docker reserves 2. clap usage errors for `healthcheck` map to 1 too.
- [ ] **P5.6** `deploy/docker/agent.Dockerfile`:
  - cargo-chef
  - a nightly builder
  - runtime `gcr.io/distroless/cc-debian12:nonroot`

  Also `deploy/compose.dev.yaml` with an itzg server plus the agent.

  > Note (P5.6, from Phase 4, group E, the user's decision) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): The compose file gives the agent a `stop_grace_period` above `shutdown_timeout`, e.g. 15 s for the 10 s default, since `docker stop` kills a container after 10 s by default, before the fleet's graceful shutdown ends.
- [ ] **P5.7** 🔴 Slow end-to-end test:
  1. `compose up`, and all bots come Online.
  2. Restart the MC container; the bots reconnect within the policy.
  3. Stop the agent; the disconnect is clean.

  > Note (P5.7, from the Phase 5 plan, the user's decisions) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)):
  > - **Shape.** A `slow_` test in `crates/fleet-agent/tests/` drives `docker compose` through `std::process::Command`, with its own project name. It reads the agent's JSON logs and RCON `list`, and checks the exit code and the `ShutdownReport`.
  > - **The image is built first.** `just test-slow` builds it before the timed test, since a cold build can outlast the slow profile's 10 minutes.
  > - **Clean-up.** The test runs `down -v` for its own project before it starts, and in a guard that also runs when the test panics.

**DoD:** Demo with 5 bots AFK on a local server for 1 h, with one server restart in between. The logs show no errors except the expected disconnect warnings.

> Note (DoD, from the Phase 5 plan, the user's decision) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)): `just demo-agent` (logic in a script under `scripts/`) runs the demo and prints a summary:
> - warn and error lines by target and count
> - the state-change timeline
> - reconnect times after the restart
> - the shutdown report
>
> The AI runs it in group D and puts its output in the PR; the user can re-run it, for example after a Minecraft-version bump.

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

  > Note (P7.1, from the group A review): **IDs aren't secrets.** v7 IDs reveal their creation time. Invite codes and session tokens come from `getrandom`, never from an ID, and access is decided by `authorize()`, not by how hard an ID is to guess.

  > Note (P7.1, from group D): **One Owner.** The `users` migration adds a partial unique index on `role` for `'owner'`. `authorize()` also refuses to act on any other user with the Owner role (ADR-0010).
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

  > Note (P7.8, from group D) (ADR-0010):
  > - **Errors.** `AuthzError::NotFound` becomes 404 and `Forbidden` 403. `WrongResource` is a bug in the handler: 500, logged at `error`.
  > - **Self-service routes** (`/me/*`, `/me/sessions/{id}`, `/auth/logout`, `/events/ticket`) check `Permission::UsePersonal` on `ResourceContext::Personal{owner}`, so every authed handler calls `authorize()`. Someone else's session gives 404.
  > - **Step-up** (§7.2) is checked here or in the handler, not by `authorize()`.
- [ ] **P7.9** 🔴 Admin endpoints:
  - `GET /users`
  - `PATCH /users/{id}` (role, disabled). Changing either revokes all of that user's sessions.
  - `POST`, `GET` and `DELETE` on `/invites`

  > Note (P7.9, from group D) (ADR-0010):
  > - **Roles** change only through the Owner (`SetRole`, Member ↔ Admin), never on the Owner themselves or another Owner. Admins only disable and enable Members (`SetDisabled`), so Appendix B's "Admin+" for `PATCH /users/{id}` means the `disabled` field for Admins.
  > - **Ownership transfer** has no permission yet. It gets one together with a route.
  > - **Invites.** Admins create and revoke Member invites; Admin invites are the Owner's (`CreateInvite{role}` on `Global`, `RevokeInvite` on `ResourceContext::Invite{role}`). Admins list all invites.
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

  > Note (P9.1, from the group A review): **Account identity.** `McUsername` keeps its case, but Minecraft names are case-insensitive. Identify accounts by MC UUID; any lookup or comparison by name ignores case.
- [ ] **P9.2** 🔴 Vault extensions:
  - purpose-bound AAD (`account_id|ms_refresh`)
  - `vault rotate-key` CLI that re-encrypts transactionally
  - lookup by `key_id`
  - a test that a ciphertext swapped between two accounts fails to decrypt
- [ ] **P9.3** 🔴 `MicrosoftAuthProvider` port (`start_device_flow`, `poll`, `refresh`, `minecraft_session`), with an `azalea-auth` adapter and a fake.
  - Use azalea-auth's default client ID; a custom Azure app ID would need approval from Mojang.
  - **Never** use azalea's file cache.
  - At `trace`, azalea-auth logs Microsoft access and refresh tokens. `fleet-server` caps the `azalea_auth` log level at `info`, whatever the configured filter says.

  > Note (P9.3, from Phase 3, group C, the user's decisions): **The `azalea_auth` cap, in detail.**
  > - **What it covers.** At `trace`, azalea-auth 0.16 logs the Microsoft access token, the whole token response (refresh token included), the Xbox Live and XSTS auth responses, the Minecraft auth, ownership and profile responses, and the account cache. At `debug` it logs nothing secret.
  > - **One rule for both binaries.** fleet-server's filter caps `azalea_auth` at `info`, whatever the configured filter says. The agent has the same cap (P5.2), because azalea logs the chat-signing private key at `trace` in every online session.
  > - **Tested.** P9's redaction test checks the cap: with the configured filter at `trace`, nothing from `azalea_auth` below `info` gets through (ADR-0011, threat model).
- [ ] **P9.4** 🔴 Device-code flow:
  - `POST /accounts/link` returns `{flow_id, user_code, verification_uri, expires_at}`.
  - A background poller runs bounded and cancellable, with per-user and total caps.
  - `GET /accounts/link/{flow_id}` returns the status.
  - On success, check the profile and game ownership, store the encrypted token, and give the creator Manage.
  - Member quota from `accounts.max_per_member`.

  > Note (P9.4, from group D): Starting a flow checks `LinkAccount` (every role). `GET /accounts/link/{flow_id}` checks `UsePersonal` on `ResourceContext::Personal{owner}` with the flow's creator, so other users get 404 (ADR-0010).
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

  > Note (P9.6, from group D) (ADR-0010):
  > - **Permissions.** `ViewBot` for list and get, `DeleteAccount`, `RelinkAccount`, `ManageGrants` (list, revoke) and `Grant{level, grantee}` (create, change), all on `ResourceContext::Account{owner, grant}` with the actor's own grant.
  > - **Grants.** Only Manage edits grants, and nobody grants to themselves.
  > - **Flagged, not decided:** how a Member picks a grantee. Members can't list users, so the dialog (P9.8) needs a lookup that doesn't let them enumerate usernames.
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
- fleet-server's log filter caps `azalea_auth` at `info`, the same cap as the agent's (P5.2), and the redaction test checks it. At `trace`, azalea-auth logs Microsoft and Minecraft tokens, the refresh token and the account cache (P9.3).
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

  > Note (P10.3, from group D): **Only the Owner** creates enrollment tokens (`EnrollAgent`), not an Admin as step 1 says. An agent receives session tokens for the bots assigned to it, and P10.5 assigns bots to any agent with free capacity, so an Admin's own agent could otherwise get the Owner's. Viewing and disabling agents stays Admin+ (ADR-0010).
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

  > Note (P10.5, from Phase 4) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **Sticky state.** The server stores `bots.last_state` from `BotStatus` and sends a Paused or Failed state with `AssignBot`/`ReconcileFull`, so the agent's `Fleet::apply(spec, restore)` starts the bot there instead of kicking a human (§6 row 3).
  > - **Moving a bot** to another agent waits for the old agent's `Removed` event (or a timeout), so two agents never run the same account at once.

  > Note (P10.5, from P4.7, the user's decision) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): **No `Stopped` before `Removed` for a sticky bot.** A removed bot publishes `StateChanged(Stopped)` only if its actor stopped it through `transition()`. A Paused, Failed or crash-looped bot gets only `Removed`, so `bots.last_state` keeps the Paused or Failed the server sends with `AssignBot` on the next agent. Otherwise the new agent would connect and kick the human who's playing (§6 row 3), or retry a failed account.
- [ ] **P10.6** 🔴 Session grants: `SessionRequest{bot_id}` returns `SessionGrant`, but only if the bot is assigned to *this* agent; otherwise `SessionDenied`. On the agent this backs `SessionCredentialProvider`.

  > Note (P10.6, from Phase 4) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): The port is `fleet_core::mc`'s credentials provider, and the runtime doesn't check `expires_at`. The managed provider never hands out a token that expires within the connect timeout.
- [ ] **P10.7** 🔴 Agent client:
  - reconnects with backon
  - keeps bots running during outages
  - sends its full state on reconnect
  - forwards events through a bounded buffer that drops the oldest chat when full and **never blocks the bots**

  > Note (P10.7, from Phase 4, group B) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): The agent maps the runtime's `FleetEvent`s to `BotEvent`. `ChatSent` and `ChatFailed` carry the runtime's `ChatTicket`, which the agent maps back to `SendChat`'s request id. `ModeChatSent{message}` is outgoing mode chat, with no request id.

  > Note (P10.7, from P4.7) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): The agent maps `FleetEventKind::Removed` too, since P10.5's move to another agent waits for it.

  > Note (P10.7, from the PR #15 review): **An event can come before its ticket.** `ChatQueue::send` returns the ticket while the queue's delivery task may already be publishing `ChatSent` or `ChatFailed` for it, so on the agent the event can arrive before the `send_chat` reply. The agent must handle a ticket it hasn't mapped to a request id yet, for example by handling replies and events in one task, or by holding unknown tickets briefly.

  > Note (P10.7, from Phase 4, group C) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): `FleetEventKind::Alert(BotNotification)` comes right after the `StateChanged` to Paused or Failed it belongs to: an alert for a human. The agent maps it too.
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

  > Note (P11.1, from group C): The `modes` migration seeds the built-in modes `afk` and `farm`: fixed v7 IDs (ADR-0010), no owner, and the JSON of `ModeDefinition::afk()` and `farm()`.

  > Note (P11.1, from Phase 4, group B, the user's decision) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): **Outgoing mode chat** arrives as `ModeChatSent{message}` (P4.5), so `chat_messages` can record what a mode said, with direction out, next to user chat.

  > Note (P11.1, from Phase 4, the user's decision) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): **Conflict texts are stored per server** in managed mode: set once in the app for all bots on that server, and sent to agents as each bot's resolved `ConflictTexts` in its spec.
- [ ] **P11.2** 🔴 Bot endpoints:
  - `GET /bots`, `GET /bots/{id}`
  - `PATCH /bots/{id}` (server, mode, auto-start)
  - `POST /bots/{id}/start|stop|restart|reset|resume`

  Every call is authorized, audited and reconciled.

  > Note (P11.2, from the group A review): **SSRF.** `ServerAddress` allows loopback and private addresses (ADR-0010). An SSRF policy for the server address must check the resolved IP addresses, not the host string.

  > Note (P11.2, from group B): `start`, `stop` and `restart` change nothing for a Paused or Failed bot: the state machine ignores `Start` and `Stop` there (ADR-0010). Decide here what the API answers in that case.

  > Note (P11.2, from group D) (ADR-0010):
  > - **Switching a bot's mode** takes two checks: `ViewMode` on the mode, and `SetBotMode(allowlist.check_mode(&mode))` on the bot.
  > - **Server address and auto-start** need Manage (`ConfigureBot`).
  > - **Lifecycle calls** need `ControlBot`.
- [ ] **P11.3** 🔴 Chat:
  - `POST /bots/{id}/chat`:
    - takes a `ChatMessage`
    - `/commands` need Manage unless they're on the allowlist
    - rate-limited per bot and per user
    - audited
  - `GET /bots/{id}/chat?before=&limit=` with cursor pagination.

  > Note (P11.3, from group D): The handler checks `SendChat(allowlist.check(&message))` and then sends that same message. An allowlisted command passes with any arguments (threat model). Reading the history needs `ViewBot` (ADR-0010).

  > Note (P11.3, from Phase 3, group E, found by the user's real-account check): **flagged, not decided.**
  > - **The limit.** On a server that enforces secure chat, azalea's unsigned commands with message arguments (`/me`, `/msg`, `/tell`, `/w`, `/say`, `/teammsg`) are rejected.
  >   - The bot gets one system message and stays up.
  >   - The server logs an ERROR each time.
  >   - `send_chat` returns `Ok` anyway.
  > - **The options:**
  >   - sign commands, which needs the server's command tree to know which arguments are signable
  >   - refuse such commands on enforcing servers, with an error
  >
  > P4.5 decides the same for mode chat (ADR-0011).

  > Note (P11.3, from Phase 4) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **Mode chat follows this task's rule.** P4.5 does nothing special for commands with message arguments; whatever is decided here applies to mode chat too, through `ChatUnavailable`.
  > - **The per-bot chat limit.** §7.4 says it's configurable, and the agent's queue uses one message per 3 s, burst 3 as a default. Decide here where that setting lives (server settings, sent to agents).
- [ ] **P11.4** 🔴 Modes CRUD: `GET`, `POST`, `PUT`, `DELETE` on `/modes`. Core validation errors become field errors. Built-in modes are read-only.

  > Note (P11.4, from Phase 2): Editing a mode that's assigned to bots must re-run the command check for every one of them. A mode with a command outside the allowlist needs Manage on each bot (ADR-0010).

  > Note (P11.4, from group E): open question, flagged and not yet decided. `fleet-core`'s own errors never quote their input, but serde_json's do: an unknown action type or a bad field value in mode JSON comes back as e.g. ``unknown variant `…` `` or `invalid type: string "…"`, which can hold chat text. The existing rules already say: internal errors never reach clients, and chat is logged only at debug. How the server maps and logs these errors is decided here (ADR-0010).

  > Note (P11.4, from group C): `ModeDraft::validate` stops at the first error. Each `ModeError` becomes a field error at `steps[step]`: `AngleOutOfRange` names its field, and `DuplicateStep` its `LimitedStep` kind (ADR-0010).

  > Note (P11.4, from group D): Modes follow `authorize()` (ADR-0010):
  > - **Built-in** modes are read-only for everyone.
  > - **Private** modes are seen and edited by their owner, the Owner, and Admins for Members' modes. Everyone else gets 404, also in `GET /modes`.
  > - **Shared** modes are seen by everyone, and edited by their creator while they're an Admin, and by the Owner. Creating one needs Admin+.
  > - **Changing the visibility** needs the rights before and after the change, so an Admin can't share a Member's private mode.

  > Note (P11.4, from Phase 4, the user's request) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): **After a respawn none of the at-start setup runs again** (look, slot, sneak, hold); only a repeating step comes back. Decide here whether modes get "on respawn" steps (e.g. `/home`, then the setup). That needs a `SessionEvent::Respawned` port event, with which the actor's respawn retry can also confirm that the bot is alive again instead of trusting an `Ok`.
- [ ] **P11.5** 🔴 Live events:
  1. `POST /events/ticket` returns a single-use ticket valid for 30 s (moka).
  2. The client opens a WebSocket at `GET /events?ticket=…`.
  3. Events are **filtered per user at send time**.
  4. Limits: per-user connection cap, `Resync` on `Lagged`, ping/pong with an idle timeout, maximum frame size.

  > Note (P11.5, from the group A review): The ticket comes from `getrandom`, never from a v7 ID, which reveals its creation time.
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

  > Note (P11.9, from the group A review): **Chat console.** Render each message as its own block, with the sender outside the text, so a `\n[Server] …` inside a message can't pass as a separate line. The sanitizer keeps combining marks, so clip stacked ones (Zalgo text) with CSS overflow.

  > Note (P11.9, from group B): **Controls.** For paused or failed bots, the app shows Resume or Reset instead of Start and Stop (ADR-0010).

  > Note (P11.9, from group D, the user's request): **Promotions.** When a Member is promoted to Admin (P8.8), the app shows the existing grants on their accounts, and the server provides them, so the Owner can review and revoke them. Grants made before the promotion survive it, including one an Admin gave an alt account (ADR-0010, threat model).

  > Note (P11.9, from Phase 3, the user's request): **Parsed senders.** If P4.1's per-server chat format is built, the chat console shows a sender parsed from the text visibly differently from one a player packet named. Neither is verified: azalea 0.16 doesn't verify chat signatures, so every chat sender is only what the server claims, and none drives a permission or trigger decision (ADR-0011, threat model).

  > Note (P11.9, from Phase 4, the user's decision) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): **The chat format is parsed on the server.** It stays out of the bot's spec and the agent: the server already has each system message's whole sanitized text, so a per-server format can extract the sender for display here.

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
                                                 # each entry: "/" + one command name, no arguments; at most 64; missing = empty
```

`agent.toml`
```toml
name = "agent-1"

[runtime]
max_bots = 50
watchdog_timeout_secs = 30
packet_liveness_timeout_secs = 30
connect_timeout_secs = 30                # from connect() until the bot has joined (ADR-0011)
max_abandoned_threads = 3                # hung host threads before the agent exits (ADR-0011)
shutdown_timeout_secs = 10
# heartbeat_file = "/tmp/afkfleet-agent.alive"   # an absolute path; default: the OS temp directory's afkfleet-agent.alive (/tmp in Docker)

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
# mode = "afk"                           # "afk" or "farm"
# conflict_texts = []                    # kick texts that count as a duplicate login; at most 16
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

> Note (P2.9): `authorize()` refines these levels (ADR-0010):
> - `POST /agents/enrollment-tokens` is Owner-only.
> - `PATCH /users/{id}`: only the Owner changes roles; Admins disable and enable Members.
> - `DELETE /invites/{id}`: an Admin invite is the Owner's.
> - `PATCH /bots/{id}`: the server and auto-start need Manage, the mode Control, and a mode with a command off the allowlist Manage.
> - The `user` routes check `Personal`.

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

> Note (P2.6): The built state machine (`fleet_core::bot`) refines this chart. The changes are listed in ADR-0010:
> - state fields, the `CrashLoop` event and `Stopping{restart}`
> - sticky `Paused` and `Failed`
> - the breaker effects `RecordFailure`, `RecordSuccess` and `ResetBreaker`
> - no "circuit open too long" arrow
