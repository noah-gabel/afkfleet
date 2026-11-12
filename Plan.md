# afkfleet: Implementation Plan

> **Living document.** Work through it phase by phase with an AI assistant. The working rules for the assistant are in [`CLAUDE.md`](CLAUDE.md).
> - Mark a task done by ticking its box (`- [x]`).
> - Note deviations inline as `> Note (P4.3): …`.
> - Record design decisions as ADRs in `docs/adr/`.
> - Every task is built on its own branch (`p<phase>/<task-id>-<slug>`) and merged through a pull request, which the user reviews and merges ([ADR-0025](docs/adr/0025-a-simpler-workflow.md)). Nobody commits to `main`!
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
│   ├── fleet-startup/         # the binaries' start-up code: config loader and errors, [log], log layer, panic hook, stop signals, secret-file loader
│   ├── fleet-testkit/         # fakes, fixtures, builders (dev-dependency only)
│   ├── fleet-integration/     # tests only: the real server with the real managed agent (P10.9, the DoD check, P11.7)
│   └── fleet-fuzz/            # the fuzz targets' bodies, run by fuzz/ (cargo-fuzz) and by its own tests (P13.2)
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
| `fleet-startup` | `fleet-core` |
| `fleet-agent` (bin) | `fleet-core`, `fleet-runtime`, `fleet-mc`, `fleet-proto`, `fleet-startup` |
| `fleet-server` (bin) | `fleet-core`, `fleet-proto`, `fleet-api-types`, `fleet-startup` (no azalea-auth since P9.3: its Microsoft adapter sends its own requests, [ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)) |
| `fleet-client` | `fleet-api-types` (its tests: `fleet-server`, `fleet-startup`, `fleet-core` and `fleet-testkit`, as dev-dependencies) |
| `apps/desktop/src-tauri` | `fleet-client`, `fleet-api-types` |
| `fleet-testkit` | `fleet-core` (dev-dependency for everyone else) |
| `fleet-integration` (tests only) | Nothing as a normal dependency; its tests: `fleet-agent`, `fleet-server`, `fleet-client`, `fleet-core`, `fleet-api-types`, `fleet-mc`, `fleet-runtime`, `fleet-startup` and `fleet-testkit`, as dev-dependencies. Nothing depends on it ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md), the user's choice, U19) |
| `fleet-fuzz` (test tooling) | `fleet-core`, `fleet-proto`, `fleet-api-types`, `fleet-server` and `fleet-agent`: what its targets read. Only `fuzz/`, the cargo-fuzz crate outside the workspace, depends on it ([ADR-0030](docs/adr/0030-fuzz-targets.md)) |

Binaries stay thin: `main.rs` parses the CLI and config and wires adapters together, and all logic lives in the crate's library part.

> Note (§4, Phase 6, the user's decision) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): **`fleet-startup`** holds only the binaries' process start-up code: the config loader with its key-path errors, the `[log]` types, the log layer and the panic hook. It came out of fleet-agent in Phase 6, group A, so the server reuses it instead of a second copy. Its filter always caps `azalea_auth` at `info` and lets panic reports through; a binary only adds rules (the agent's azalea rules). **Group E moved the stop signals there too** (the user's decision): `signals::{Signal, ShutdownSignals, OsSignals}` (SIGTERM and SIGINT, or Ctrl+C and Ctrl+Break), so both binaries share one implementation, and the crate's scope is now the binaries' process start-up and lifecycle.

> Note (§4, Phase 7, group A, the user's decision) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **`fleet_startup::secret_file`** reads every `*_file` secret of both binaries with one set of rules (regular file, owner-only mode on Unix, at most 64 KiB, one trailing newline removed; errors never show the content): the server's vault keys from P7.4, its PKI keys and the agent's `key_file` in P10. fleet-startup gains secrecy and zeroize for it.

## 5. Crate & tool registry

**Rule: one crate per concern, used throughout the whole project.** A concern that isn't covered here needs the user's approval, an ADR, and an entry in this table and in the index in `.claude/rules/dependencies.md`. All Rust crates are declared once in `[workspace.dependencies]`, when the first crate uses them (see the P0.3 note). Versions were pinned in P0.3 (2026-10-05) after checking crates.io and docs.rs. They're caret requirements, and `Cargo.lock` holds the exact pin (ADR-0009). **dfo** means `default-features = false` at workspace level.

### Rust: runtime, domain & infrastructure
| Concern | Crate | Version | Used in | Notes |
|---|---|---|---|---|
| Minecraft protocol & client | `azalea` | `=0.16.0` (+mc26.1) | fleet-mc | Needs nightly (ADR-0003). Disable its `AutoReconnectPlugin` and `AutoRespawnPlugin`. Runs only inside a `LocalSet` |
| Minecraft chat components & translations | `azalea-chat`, `azalea-language` | `=0.16.0` (+mc26.1) | fleet-mc | Already in azalea's graph at the same pin. Only for fleet-mc's bounded renderer of server text: it names `PrimitiveOrComponent`, which azalea doesn't re-export, and looks up translation templates, because azalea's own rendering grows exponentially (ADR-0011). Bumped together with azalea |
| Microsoft / Minecraft auth | `azalea-auth` | `=0.16.0` (+mc26.1) | none directly: only through azalea's crates | **Not used by fleet-server** (P9.3, the user's decision): azalea-auth checks no status, polls in a loop that drops every refusal, can panic on an unexpected answer and prints tokens in `Debug`, so fleet-server's adapter sends its own requests with azalea-auth 0.16.0's protocol (client ID, scope, fields, headers). `deny.toml` lets it in only through `azalea`, `azalea-client`, `azalea-inventory` and `azalea-protocol` ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)). fleet-mc reaches only `azalea::auth::sessionserver` and `certs`, through azalea's re-export, never the Microsoft flows; its clippy config bans them (ADR-0011) |
| Async runtime | `tokio` | 1.53.3 | runtime, mc, testkit, agent, server, client | `test-util` feature in dev (fleet-client since P11.8, U26, with `io-util` for its in-memory pipes). 1.53.3 is a security floor (2026-10-23): RUSTSEC-2026-0386 (`tokio::io::unix::AsyncFd`'s safe constructors, unsound with a descriptor the caller doesn't own; no member calls them, though `net` is on). Clippy bans `unbounded_channel`; azalea's two mandated channels are the only exceptions (ADR-0011). The agent builds a multi-threaded runtime after loading its config (ADR-0014). fleet-server enables `fs`, to create the database file and check its permissions, `time`, for the trace layer's latency from tokio's clock, and from P6.11 `rt`, `rt-multi-thread`, `macros`, `net` and `io-util`, for its runtimes, the listener and the healthcheck's TCP probe; fleet-startup enables `signal` and `macros` for the stop signals (ADR-0015) |
| Cancellation, task tracking | `tokio-util` | 0.7.19 | runtime, mc, agent, server | `CancellationToken`, `TaskTracker`. fleet-runtime and the agent enable no features: they use only `CancellationToken`, and their tasks live in tokio's `JoinSet`, which reports panics (ADR-0013, ADR-0014). fleet-server enables `rt` only, since P9.5: its token asks axum's server for the graceful shutdown (P6.12, ADR-0015), and the session-token service's refreshes run on a `TaskTracker` ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)) |
| Stream adapters | `tokio-stream` | 0.1.19, dfo | proto, agent, server | gRPC streams, broadcast → stream. Without default features (`time`). fleet-server (P10.4): each `Connect` answers from a `ReceiverStream` over its bounded queue, and the listener maps its accepted sockets with `StreamExt`. fleet-agent (P10.7, the user's choice U18): the outbox's `Connect` request body is a `Stream`, and the agent reads the server's answers with `StreamExt`. Already in Cargo.lock through tonic and sqlx ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)) |
| Sink/Stream extension traits | `futures-util` | 0.3.34 | client, server (dev) | WebSocket split/send. *Since P11.8 (G1, U25):* fleet-client's normal dependency with `sink`, the live events socket's `StreamExt` and `SinkExt` over tokio-tungstenite's `WebSocketStream`. Since P11.5 (D1, U11): fleet-server's dev-dependency with `sink` only, for its tests' `SinkExt`, and fleet-client's until G1 made it a normal one; fleet-server's own code needs none: one task per socket over axum's own `recv` and `send` (S35). Already in Cargo.lock through axum, tower and sqlx ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)) |
| Async fns in `dyn` traits | `async-trait` | 0.1.92 | server | Only for `Arc<dyn Port>`. Use generics + RPITIT elsewhere. First used by fleet-server's store ports (ADR-0015) |
| Retry & backoff | `backon` | 1.6.0, dfo | core (policy), agent, client | Exponential backoff with jitter. The default features pull in a tokio sleeper. The agent's reconnect waits come from fleet-core's `RetryPolicy::seeded_delay` (P10.7, S67), so backon's tokio sleeper isn't added |
| Rate limiting (keyed / in-process) | `governor` | 0.10.4, dfo | runtime (chat), server (HTTP: per IP, per route class, per user) | fleet-runtime and fleet-server enable `std` only. The defaults add `quanta`, `dashmap` and `jitter`, which pulls rand 0.9 and getrandom 0.3 into normal dependencies. Without `quanta` there's no `DefaultClock`: the chat queue uses `direct_with_clock` with a tokio-backed clock, so paused time controls it (ADR-0013); the server's keyed `hashmap_with_clock` limiters run on its `Clock` port, a monotonic clock in production and `ManualClock` in tests, behind its own axum middleware (ADR-0016) |
| TTL cache / single-flight | `moka` | 0.12.16 | none | **Not used.** P11.5's WebSocket tickets live in a map on the server's `Clock` port, as the login's challenges do (D1, S29): moka's own clock would need a real-time test, and it has no per-user bound ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)). Not the MC token cache either: P9.5's single-flight is a per-account tokio `Mutex`, since moka's coalescing lets a waiter re-run the init when the evaluating caller is dropped, a second refresh with a spent token ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)) |
| Library errors | `thiserror` | 2.0.21 | all libraries | |
| Binary error reporting | `anyhow` | 1.0.104 | `main.rs` only | Not declared yet: the agent's and the server's `main.rs` map their typed errors to exit codes themselves (the user's decision for the server, P6.11). It arrives with the first `main.rs` that uses it (ADR-0014, ADR-0015) |
| Serialization | `serde`, `serde_json` | 1.0.229, 1.0.151 | all | fleet-testkit's log buffer parses JSON log lines with `serde_json`; fleet-server reads the audit log's metadata JSON back with it, while fleet-core's `AuditMetadata` writes its JSON itself, byte for byte as `serde_json` would (ADR-0015) |
| Where serde stopped in a refused body | `serde_path_to_error` | 0.1.20 | server | Named only by the JSON extractor, to read the line and column out of axum's rejection, which wraps `serde_json`'s error in this crate's. It was in `Cargo.lock` already through axum's `json` ([ADR-0031](docs/adr/0031-mutation-testing.md)) |
| Configuration | `figment` | 0.10.19 | startup, testkit; agent and server tests | TOML file + env; upstream is quiet but the crate is stable. It has no default features: fleet-startup's loader enables `toml` and `env`, and fleet-testkit `test` for `jail::in_jail`, whose one closure carries the workspace's only approved `#[expect(clippy::result_large_err)]` (ADR-0014, ADR-0015) |
| DTO & config validation | `garde` | 0.23.0 | api-types, agent, server | Domain value objects use hand-written constructors. It has no default features; members enable `derive` and the rules they use. The agent's config uses garde for its number ranges only and converts texts with the core constructors, so it reports every problem at once (ADR-0014) |
| Logging / tracing | `tracing`, `tracing-subscriber` | 0.1.44, 0.3.23 | all | `env-filter`, `json`. fleet-startup builds both binaries' log layer (ADR-0015). fleet-testkit's log capture uses `tracing` and `tracing-subscriber`; the redaction tests use it (ADR-0011). Its log buffer is the writer for format and filter tests (ADR-0015) |
| Metrics | `metrics`, `metrics-exporter-prometheus` | 0.24.6, 0.18.3 (dfo) | runtime, agent, server | Internal port only. No features, ever: the default `push-gateway` brings its own TLS stack, and `http-listener` spawns unowned tasks with no timeouts, so each binary serves `GET /metrics` itself (ADR-0022, S9). Its metrics-util dependency always enables `storage`, which brings rand 0.9 and getrandom 0.3 into normal dependencies (both already in the graph, neither used for secrets) (ADR-0014) |
| IDs | `uuid` | 1.27.0, dfo | core, mc, server | v7, serde. fleet-mc only names `Uuid` in azalea's `AccountTrait` (ADR-0011). fleet-server binds IDs as `uuid::fmt::Hyphenated` text, never a plain `Uuid` (ADR-0015) |
| Time | `chrono` | 0.4.45, dfo | core, runtime, agent, server | Always UTC. No `clock` feature in core or runtime: time is passed in. The runtime derives `DateTime<Utc>` from tokio's clock, anchored at a wall time its caller passes in (ADR-0010, ADR-0013). The agent enables `now` only, for that one read at startup; `clock` would add local time zones (ADR-0014). fleet-server enables `now` only, for its `SystemClock`, the one wall-clock read behind the `Clock` port (ADR-0015) |
| CLI | `clap` | 4.6.7 | agent, server | derive. Both binaries handle clap's errors themselves, so `healthcheck`'s usage errors exit 1 (ADR-0014, ADR-0015) |
| Hidden password prompt | `rpassword` | 7.5.4 | server CLI | `user create-owner`'s prompt; `--password-stdin` is the non-interactive path (ADR-0016) |

### Rust: API, transport & persistence
| Concern | Crate | Version | Used in | Notes |
|---|---|---|---|---|
| HTTP framework + WebSocket server | `axum` | 0.8.9, dfo | server | `ws` feature. fleet-server enables only what it uses: `matched-path` (P6.6), so the trace layer logs the route's template, never the path a client sent, and `tokio` and `http1` (P6.11) for `axum::serve`; the error body is written with serde_json (ADR-0015). `ws` since P11.5 (D1, U11), which brings tokio-tungstenite and tungstenite 0.29.0, MIT and MIT OR Apache-2.0, released 2026-03-17: axum 0.8.9 pins them, and they're the only crates it added to Cargo.lock ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)) |
| Typed headers (and cookies later) | `axum-extra` | 0.12.6 | server | `TypedHeader<Authorization<Bearer>>` |
| Service abstraction | `tower` | 0.5.3, dfo | server, client | fleet-server lists its middleware stack with `ServiceBuilder` (no feature); its tests enable `util` for `ServiceExt::oneshot` (ADR-0015) |
| HTTP middleware | `tower-http` | 0.7.1 | server | fleet-server enables `catch-panic`, `timeout`, `limit`, `sensitive-headers` and `set-header`. Never `request-id`, which turns on uuid's `v4` and with it `rng`, nor `trace`, whose response hook can't see the route: the request-ID and trace layers are the server's own (P6.6). reqwest keeps its own 0.6.11 (ADR-0015) |
| OpenAPI | `utoipa`, `utoipa-axum` | 6.0.0, 0.3.0 | api-types, server | P0 picked 6.x (ADR-0009); re-checked in P6.10 (2026-10-11): still the newest, MIT/Apache-2.0. utoipa without default features: fleet-api-types and fleet-server enable `macros`, and fleet-api-types `preserve_order`, so properties keep the field order ts-rs writes and a test compares the two. utoipa-axum takes axum without default features. Cargo.lock gained only utoipa, utoipa-gen, utoipa-axum and its macro helper pastey (ADR-0015) |
| Database | `sqlx` | **0.9.0**, dfo | server | `runtime-tokio`, `sqlite-bundled`, `migrate`, `macros`, `uuid`; no TLS feature (SQLite); offline data in `crates/fleet-server/.sqlx/`. *RUSTSEC-2026-0459 (bundled SQLite before 3.54.0) is ignored until a sqlx release accepts `libsqlite3-sys` 0.39; that release's bump removes the ignore ([ADR-0024](docs/adr/0024-sqlite-zlib-and-caddy-advisories.md)).* Not `sqlite`: in 0.9 it also turns on extension loading, deserialize and unlock-notify. Not `chrono`: it would turn on chrono's `clock` for the whole workspace build, so times are converted in fleet-server. Not `json` (ADR-0015). *Since P13.5:* also a dev-dependency of fleet-integration, with no feature of its own, for the crash cases' child process: it sets SQLite's commit, update and rollback hooks on the server's write connection from outside, to stop a transaction right before it commits ([ADR-0033](docs/adr/0033-chaos-tests-and-the-missing-database.md)) |
| gRPC | `tonic`, `tonic-prost` | **0.14.6** | proto, agent, server | Features `tls-aws-lc`, `tls-connect-info`. Never `tls-ring` or `tls-webpki-roots`. Declared without default features (P10.1): fleet-proto enables `codegen` only, for the generated client and server; the agent and the server add the transport and TLS features in their groups ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)). Group B: fleet-server `server`, `router` (`add_service`) and `tls-aws-lc`, which turns on `tls-connect-info`, with `channel` for its loopback tests; fleet-agent `channel` and `tls-aws-lc`, with `server` and `router` for its fake port in tests. Group C: no server-wide `max_connection_age`, grace or `timeout`, which fleet-server's clippy config bans (U7); its listener bounds each connection's age itself, lifted while an agent's stream lives |
| Protobuf | `prost` | 0.14.4 | proto | Its defaults (`derive`, `std`). Already in Cargo.lock through testcontainers before P10.1 |
| Protobuf codegen | `tonic-prost-build`, `protox` | 0.14.6, 0.9.1 | proto (`build.rs`) | Pure Rust, no system `protoc`. tonic-prost-build without default features: no transport helpers, no `cleanup-markdown`. protox 0.9.1 over 0.10.0 (2026-10-07) is the user's choice at Phase 10's planning; no advisory names it. Build-only, with prost-build, prost-reflect, protox-parse, logos, miette and prost-types, which nothing uses at run time ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)) |
| TLS | `rustls` | 0.23.46 | agent, server, client, desktop | **Only the aws-lc-rs provider**, installed explicitly at startup. The default features select it. The agent's `main.rs` installs it before anything else runs (ADR-0014). 0.23.46 is a security floor (2026-10-14): RUSTSEC-2026-0346 (client session values' `Debug`), 0347 (ECH, never enabled here), 0348 (the `brotli` feature, never enabled here) and 0349 (a downgrade to classical key exchange on resumed client connections). Its certificate verifier, `rustls-webpki`, is locked at 0.103.16, a security floor (2026-10-21): RUSTSEC-2026-0382 (paths through distrusted emSign intermediates, which only a public trust store allows: the Microsoft adapter's and fleet-client's connections to publicly signed servers, never the control plane, whose own CA is the only anchor) |
| X.509 / CSR | `rcgen` | 0.14.10, dfo | server (CA, signing, `pki`; dev: the Microsoft adapter's certificate test), agent (key, CSR), client (dev: test certificates) | fleet-server: `aws_lc_rs`, `pem`, `x509-parser`, `zeroize` (Phase 10, group B: wipes a key pair's PKCS#8 copy; adds no crate); fleet-agent: `aws_lc_rs`, `pem`, `zeroize` (P10.3). The default is ring; `aws_lc_rs` turns on x509-parser's `verify-aws`, never its ring `verify`. **Every key is Ed25519 from getrandom bytes**, wrapped as PKCS#8 by `fleet_core::pki::ed25519_pkcs8`, never `KeyPair::generate`, which draws from aws-lc's own RNG (S10). rcgen 0.14 reads CSRs but no longer certificates (x509-parser below). fleet-client's TLS tests make their CA and server certificates with it at run time (`aws_lc_rs`, `pem`), since no key is ever committed ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)); fleet-server's Microsoft adapter test does the same from P9.3 ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)); P10's PKI in production from group B ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)) |
| X.509 parsing | `x509-parser` | 0.18.1, dfo | server, agent | fleet-agent (P13.6, [ADR-0027](docs/adr/0027-certificate-renewal-and-phase-13-refinements.md)): reads its own certificate's `notBefore` and `notAfter` after rustls checked it, for the renewal's timing and the warning before its end; Cargo.lock gained only the edge. fleet-server: reads the PKI's certificates: the CA flag, the server certificate's names and validity at serve's startup, and every field of the certificates the server issues in tests. The user's choice in Phase 10's group B (U5), since rcgen 0.14 no longer parses certificates. Already built through rcgen's `x509-parser` feature, so Cargo.lock gained no crate. Its errors are never passed on: they can quote their input ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)) |
| HTTP client | `reqwest` | **0.13.5**, dfo | client, server, desktop, mc | Feature `rustls` (aws-lc-rs + platform verifier), no native-tls. fleet-mc enables no features: it only names `reqwest::Proxy` in azalea's `AccountTrait` (ADR-0011). fleet-client enables none either, and builds its client with no redirects, no proxies, a 5 s connect and 20 s request timeout, and `tls_certs_only` for a custom CA; it reads each body itself, capped at 1 MiB ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)). fleet-server (P9.3) enables `form` only, for its Microsoft adapter: no redirects, no proxies, https only, a 5 s connect and 15 s request timeout, every body read through its own reader capped at 256 KiB. Its clippy config bans reqwest's unbounded readers and `reqwest::get`, and its client types outside the adapter's `client.rs` ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)) |
| WebSocket client | `tokio-tungstenite` | 0.29.0, dfo | client, server (dev) | `handshake` in fleet-client, `connect` in fleet-server's tests, never a TLS feature (*the user's choice at G's planning, U25:* fleet-client's socket is an HTTP/1.1 upgrade on its own reqwest client, framed by `WebSocketStream::from_raw_socket`, so its TLS is reqwest's; §5 first planned `connect` and `rustls-tls-native-roots`). 0.29 matches axum 0.8.9's `ws`, so only one tungstenite is built. Since P11.5 (D1, U11): fleet-server's dev-dependency with `connect` only, the tests' client over plain `ws://` on `127.0.0.1`; fleet-client's dev-dependency the same way, for the console's `events`, until G1 removed it: the console runs over fleet-client's own socket (R80). P11.8 opens the app's socket inside fleet-client with it as a normal dependency, so the token never leaves the client (U15); its TLS features stay G's question *(answered: U25, above)* ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)) |
| Rust → TypeScript types | `ts-rs` | 12.0.1, dfo | api-types | `serde-compat` only, so serde's renames reach the TypeScript; `chrono-impl` and `uuid-impl` come with the first DTO that holds a time or an ID. The export runs through `Config::new()`, never `#[ts(export)]` or the `TS_RS_*` variables (ADR-0015) |

### Rust: security
| Concern | Crate | Version | Used in | Notes |
|---|---|---|---|---|
| Password hashing | `argon2` | **0.6.0**, dfo | server | Argon2id, in `spawn_blocking` behind a semaphore. `alloc`, `password-hash`, `zeroize`; without the default `getrandom`, so salts come from the `SecureRandom` port through `hash_password_with_salt`. The dev profile compiles it and `blake2` at `opt-level = 3`: about 10 ms per hash instead of 228 ms, so every test hashes at the real baseline (ADR-0016) |
| Authenticated encryption | `chacha20poly1305` | **0.11.0**, dfo | server (vault) | XChaCha20-Poly1305. `alloc`, `zeroize`; without the default `getrandom`, so nonces come from the `SecureRandom` port (ADR-0016) |
| Hashing | `sha2` | **0.11.0** | server, client, agent | Token hashes, certificate fingerprints: fleet-client's `CustomCa::fingerprints` (P8.4, [ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)), the agent's CA pin (P10.3, the user's choice U4, [ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)) |
| Keyed hashing (HMAC) | `hmac` | 0.13.0 | server | HMAC-SHA256 keys of the lockout's `login_attempts` rows, so no row holds a typed name, and the derivation of their key from the vault's active key. `zeroize` wipes the keyed state. RustCrypto like sha2 0.11; already in Cargo.lock (ADR-0016) |
| Constant-time comparison | `subtle` | 2.6.1 | server | First used for the vault's duplicate-key check (ADR-0016). RustCrypto's own constant-time crate, `ctutils`, comes in through `digest` (sha2, hmac, argon2's blake2), poly1305 (chacha20poly1305), argon2's `phc` and azalea's `rsa`: it's locked at 0.4.4, a security floor (2026-10-23): RUSTSEC-2026-0385 (a branch in `Choice`'s mask generation on common targets, and in `u128` comparisons on 32-bit ones); 0.4.5 since 2026-10-24, which `crypto-bigint` 0.7.6 requires. `crypto-bigint` itself (through azalea's `rsa` and `crypto-primes`) is locked at 0.7.6, a security floor (2026-10-24): RUSTSEC-2026-0387 (timing variability in operations meant to be constant-time); 0.7.9 since 2026-10-25, when 0.7.6 to 0.7.8 were yanked "due to breaking changes in #1373" (its changelog: a fix to `Mul` for `NonZero<T>`, released as 0.7.9), with 0.7.6's fix kept |
| Encoding | `base64` | 0.23.1, dfo | server, client | URL-safe, no padding for tokens; standard with padding for the vault's key files (ADR-0016) and a custom CA's canonical PEM (fleet-client's `CustomCa::to_pem`, with `alloc`; P8.4). Without default features: `simd-unsafe`, its only unsafe code, stays off |
| Secret wrappers | `secrecy` | 0.10.3 | core, mc, agent, server, client, desktop, startup | Redacted `Debug`. fleet-server's `PasswordHash` holds the stored hash in a `SecretString` (ADR-0015); fleet-startup's secret-file loader returns secrets in it (ADR-0016) |
| Memory zeroing | `zeroize` | 1.9.0 | server, agent, startup | `Zeroizing` buffers for key files and passwords read from stdin (ADR-0016) |
| Secure randomness | `getrandom` | 0.4.3 | server, agent | Tokens, keys, nonces: **always** use this. The agent draws its runtime seed and its bot IDs' random bytes from it; no features (ADR-0014). In fleet-server only `OsRandom` calls it, behind the `SecureRandom` port (ADR-0015) |
| Non-security randomness | `rand` | **0.10.3**, dfo | core, runtime, testkit | Jitter, random look angles; seeded `StdRng` in tests. No OS randomness in core. fleet-testkit's `SeededRandom` uses `std_rng` (ADR-0015) |
| TOTP 2FA | `totp-rs` | 6.0.0 | server | No default features; `otpauth` and `zeroize`. No `qr` (its image codecs; group D, the user's choice), no `std` (wall-clock reads), no `gen_secret` (rand's thread RNG): the secret's bytes come from the `SecureRandom` port (security rule 4). `zeroize` adds the `zeroize_derive` proc macro (ADR-0016) |
| QR code | `qrcodegen` | 1.8.0 | server | The enrollment's QR code: its module grid, which fleet-server writes as an SVG itself (group D, the user's choice). No dependencies (ADR-0016) |
| Password strength | `zxcvbn` | 3.1.1, dfo | server | Without `derive_builder`. Runs inside the password service's blocking task; its hints map to fleet-core's own enums. It reads the wall clock itself for its "recent years" rule. The dev profile compiles it at `opt-level = 3` (ADR-0016) |

### Rust: desktop
| Concern | Crate | Version | Used in | Notes |
|---|---|---|---|---|
| Desktop shell | `tauri`, `tauri-build` | **2.12.3** (≥ 2.11.1), dfo; 2.7.2 | desktop | 2.11.1 fixes CVE-2026-42184 (`is_local_url` origin bypass on Windows). `tauri-build` is the required build-dependency (approved in P0). The desktop crate enables only `wry` and `compression`: never `dynamic-acl` (runtime `add_capability`), `devtools`, `common-controls-v6` (its TaskDialogIndirect import breaks Windows test executables, which lack the Common Controls v6 manifest), or the Linux-only `x11`/`dbus`; the CLI adds `custom-protocol` for `tauri build`. Dev only: `test`, for the ACL test ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| Open the device-code URL | `tauri-plugin-opener` | 2.7.0 (its build-dependency `tauri-plugin` 2.7.1 in Cargo.lock, so tauri-utils stays 2.10.1) | desktop | Called only from Rust: its free `open_url`, from one command without arguments, on the link fleet-client's `VerifiedLink` checked. Never registered, so the webview gets none of its commands ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)) |
| Single instance | `tauri-plugin-single-instance` | 2.5.3 | desktop | Registered first. No JavaScript API, so no capability grants it anything ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| Signed auto-update | `tauri-plugin-updater` | **2.14.1**, dfo (and `minisign-verify` 0.3.0 in Cargo.lock) | desktop | Its default `rustls-tls` feature forces the ring provider; TLS comes from the workspace `reqwest` 0.13 instead, and `system-proxy` and `zip` stay off too (`tauri-hardening.mjs` pins its features to none). Registered, but none of its commands is granted: two commands of the app's own check and install, against the committed public key, with `requireSignedVersion` (P12.5; [ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), U25, S27). Cargo.lock also lists its macOS-only `tar`, `filetime`, `osakit` and `objc2-osa-kit`, never built for Windows |
| OS credential store | `keyring-core` + `windows-native-keyring-store` | 1.0.0, 1.1.0 (keyring v4 family) | desktop | Refresh token storage: one entry per server origin, `Local` persistence, entries built from an explicit store (no global default). The Windows store without default features, so no `search` and no regex (P8.4, [ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |

### Rust: testing
| Concern | Crate | Version | Notes |
|---|---|---|---|
| Fixtures / parametrized tests | `rstest` | 0.27.0, dfo | `#[case]` tables. The default async-timeout and crate-renaming features aren't needed (ADR-0010) |
| Property testing | `proptest` | 1.11.0, dfo | State machine, parsers. Only `std`, which reads `PROPTEST_CASES`; no fork or timeout mode (ADR-0010) |
| Snapshot testing | `insta` | 1.49.0 | `json` and `redactions` features, enabled by the member that uses them (fleet-core: `json`; fleet-server: `json`, for the error responses' snapshots, plus the migrations' checksum snapshot) |
| Mocks | `mockall` | 0.15.0 | Only for interaction checks; put `#[automock]` above `#[async_trait]` |
| Containers | `testcontainers` | 0.28.0, dfo | `itzg/minecraft-server`, for fleet-mc's slow tests (P3.7) and fleet-integration's Phase 10 managed check, an online-mode one ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md), U21). The default `ring` feature turns on TLS for the Docker client, which the local socket and the Windows named pipe don't need; no feature is enabled. Its `astral-tokio-tar` is unmaintained (RUSTSEC-2026-0440, 2026-11-05), with no fix and no newer testcontainers; `deny.toml` ignores that one advisory, as it reaches only the tests, until a testcontainers release drops the crate ([ADR-0021](docs/adr/0021-testcontainers-unmaintained-tar-exception.md)) |
| A test's own CPU time and memory | `sysinfo` | 0.40.1, dfo | fleet-integration's load test alone (P13.4): `system` only, so no disks, networks, GPUs or users. Pinned to 0.40.1 of 2026-10-30, not the two releases of 2026-11-11 ([ADR-0032](docs/adr/0032-the-load-test.md)) |
| Time control | `tokio` `test-util` | | `start_paused`, `advance` |
| Independent IP-network check | `ipnet` | 2.12.2 | Dev only: fleet-server's property test compares its own `IpNet` (`[http] trusted_proxies`) and the /64 limiter key with it. Production code never uses it. Already in Cargo.lock through reqwest (ADR-0016) |
| In-memory gRPC transport | `hyper-util` | 0.1.21, dfo | Dev only: fleet-server's control-plane test hands tonic's client a tokio duplex pipe through `rt::TokioIo` (`tokio` only), so an agent's stream runs a virtual hour through the real router on paused time. The user's choice in Phase 10's group C (U6). Already in Cargo.lock through tonic, axum and reqwest ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)) |
| Temporary files | `tempfile` | 3.27.0 | fleet-server's database tests: a fresh folder per test, removed on drop; fleet-integration's cases keep a server's database, PKI and agents' identities in one (ADR-0019, U21). Already in the lockfile through figment and insta (ADR-0015) |
| `log` records in tests | `log` | 0.4.34 | fleet-server's one normal use: `log::LevelFilter`, which sqlx's statement-logging levels take; clippy bans its logging macros there, so all server logging stays on tracing (ADR-0015). Otherwise a dev-dependency. fleet-testkit: its log capture's test emits a `log` record to prove that reqwest's and rustls's logs reach the redaction check through `tracing-log` (ADR-0011). fleet-agent: its telemetry test proves that the log filter and azalea's caps see a `log` record's real target (ADR-0014) |
| Fuzzing | `libfuzzer-sys` | 0.4.13 | Only in `fuzz/`, the cargo-fuzz crate, a workspace of its own with its own `Cargo.lock`: never a dependency of a workspace member. The targets take bytes, so `arbitrary` (1.5.0 in `fuzz/Cargo.lock`) comes in only as libfuzzer-sys's own dependency ([ADR-0030](docs/adr/0030-fuzz-targets.md)) |

### Frontend (same one-library-per-concern rule)
| Concern | Package | Notes |
|---|---|---|
| Package manager / workspaces | pnpm | |
| Bundler & dev server | `vite` | 8.3.4, with `@vitejs/plugin-react` 6.1.2 (oxc on Vite 8, no Babel or SWC; approved in P8.2, [ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| UI framework | `react`, `react-dom` 19 | 19.3.0, with `@types/react` and `@types/react-dom` 19.3.0 (approved in P8.2) |
| Language | `typescript` | `strict`, `noUncheckedIndexedAccess`, `exactOptionalPropertyTypes`. 7.0.2, the Go port: a native `tsc`, but no JavaScript compiler API until 7.1, so no tool that needs it (typescript-eslint, ts-node, vite-plugin-checker); each tsconfig lists `vite/client` in `types` ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| Server state & caching | `@tanstack/react-query` | 5.104.1 (approved in P8.6): no retries and no refetch on focus or reconnect; no query or mutation ever carries a secret ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| Routing | `@tanstack/react-router` | 1.170.42 (approved in P8.6): type-safe routes defined in code (no router plugin), over a memory history ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)). Its router-core's `seroval` and `seroval-plugins` are locked at 1.6.12, within its `^1.6.7`, a security floor (2026-10-29): GHSA-f86p-h4j2-7qgj (code injection through a typed array's `constructor` property) and GHSA-9wx9-5pv9-3349 (script injection through RegExp sources in serialized output). router-core uses them only in its server-rendering modules, which the app never runs; 1.6.12 is the newest patch of the locked line, not 1.7 |
| UI-only state | `zustand` | Small stores only. 5.0.15 (approved in P8.6): one set per `App`, set by the watched client (session, banner) |
| Schema validation | `zod` 4 | 4.6.7 (approved in P8.5 at 4.6.5): the input checks every API call passes (`validated`), and the forms' schemas from P8.6 on ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)). 4.6.7 is a security floor (2026-10-24): GHSA-mrhx-c7xc-m3w5, memory exhausted while validating oversized constrained collections, fixed in 4.6.6; 4.6.7 is that line's newest patch, released 2026-10-12 |
| Forms | `react-hook-form`, `@hookform/resolvers` | 7.89.0 and 5.9.1 (approved in P8.7): zod schemas built on `api/schemas.ts`; a call carrying a secret is a direct call, never a mutation ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| Styling | `tailwindcss` 4 | 4.3.3, with `@tailwindcss/vite` 4.3.3 (approved in P8.2) |
| Components | shadcn/ui (Radix UI primitives, copied into the repo) | Style `radix-nova`, base color neutral, aliases through `packages/ui`'s package.json `#` imports. P8.6 copies Button, Input, Label, Textarea, Card and Alert from the registry's JSON (never the CLI, which installs packages itself), with `cn` 0.4.0, class-variance-authority 0.7.1 and the single Radix packages `@radix-ui/react-slot` 1.4.0 and `@radix-ui/react-label` 2.1.16 (never the `radix-ui` umbrella); tw-animate-css and shadcn's tailwind.css wait for a component that needs them. Before P8.6 it was only `components.json` and the theme tokens; the CLI never runs as a dependency ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| Fonts | `@fontsource-variable/geist`, `@fontsource/ibm-plex-mono` | 5.3.0 and 5.3.0 (approved in Phase 11's group L1, [ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), U93), OFL-1.1, about 267 KB whole. Imported in `styles.css`, never inlined (`assetsInlineLimit: 0`, checked by the bundle guard); the licences are in `THIRD-PARTY-NOTICES.md` |
| Icons | `lucide-react` | 1.53.0 (approved in P8.6) |
| Toasts | `sonner` | 2.0.8 (approved in P8.6), **patched**: `patches/sonner@2.0.8.patch` removes its `__insertCSS(…)` call, a `<style>` the release CSP blocks; its stylesheet comes through `styles.css`. A bump redoes the patch and runs `Toaster.test.tsx` ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| Drag and drop | `@dnd-kit/core`, `@dnd-kit/sortable`, `@dnd-kit/utilities` | 6.3.1, 10.0.0 and 3.2.2 (approved in Phase 11's group I, [ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), U69), with `@dnd-kit/accessibility` 3.1.1 and `tslib` 2.8.1. MIT, `tslib` 0BSD, about 18.7 KB gzipped. Pointer events, not HTML5 drag events, which Tauri's `dragDropEnabled` stops on Windows. No runtime `<style>`: positions go through React's `style` prop, which the release CSP allows |
| Tauri bridge | `@tauri-apps/api` 2 | 2.12.1 (approved in P8.5). Only in `apps/desktop`, and only `@tauri-apps/api/core`'s `invoke`, injected into `TauriApiClient`; Biome refuses every other Tauri import ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| Unit / component tests | `vitest`, `@testing-library/react`, `@testing-library/user-event`, `jsdom` | vitest 5.0.3, jsdom 29.1.1 (jsdom 30 needs Node ^24.15), @testing-library/react 16.3.3 with its peer `@testing-library/dom` 10.4.2 (approved in P8.2); `@testing-library/user-event` 14.6.7 arrived in P8.6. jsdom's `undici` is locked at 7.30.1, within its `^7.25.0`, a security floor (2026-10-24, test tooling only): GHSA-xjj6-f33q-929c (high, a denial of service through excessive content-encodings), GHSA-4qv8-2p9j-2685 (unbounded memory in the EventSource parser) and GHSA-f72v-6w45-q472 (a Subresource Integrity bypass) |
| API mocking | `msw` | 3.0.3 (approved in P8.9), with `@msw/playwright` 0.7.0: MSW's handlers on Playwright's routing, in the Playwright process only; nothing of MSW reaches any bundle ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| E2E | `@playwright/test` | 1.64.0 (approved in P8.9): the installed Microsoft Edge (`channel: "msedge"`), so no browser is downloaded; the screenshot baselines (P11.11) use the Chromium inside Playwright's Linux image of the same version, pinned by digest (see the pinned images below), never a download of their own; `@types/node` 24.19.2 for its config and specs ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| Lint & format | `@biomejs/biome` | |

### Tools & infrastructure (not crates)
- **Rust tooling** (versions pinned in P0.3; tools for later phases are re-checked when they're introduced):
  - rustup with the dated nightly from `rust-toolchain.toml` (ADR-0003), plus stable 1.99.0 for the `stable-check` job
  - `just` 1.58.0
  - cargo-nextest 0.9.146, cargo-llvm-cov 0.9.1, cargo-deny 0.20.2, cargo-insta 1.49.0
  - sqlx-cli **0.9.0** (P6), SQLite-only: `cargo install sqlx-cli --version 0.9.0 --locked --no-default-features --features sqlite`, the same in CI and `docs/development.md`
  - cargo-fuzz 0.13.2 (P13.2), re-checked on 2026-11-11: still the newest, from 2026-06-09. `just fuzz` and the Fuzz workflow need it; `just check` doesn't
  - cargo-mutants 27.1.0 (P13.3), re-checked on 2026-11-12: still the newest, from 2026-06-02. Only `just mutants` needs it; `just check` and CI don't
  - Later: cargo-chef 0.1.78 (P5)
- **Frontend tooling:** pnpm 12.9.1 (`packageManager`) and Biome 2.5.15 from P0.8; every other package is pinned in the phase that introduces it, exactly (no `^`), with `pnpm-lock.yaml` as the lock. From P8.6 `pnpm-workspace.yaml` sets `minimumReleaseAge: 4320`: pnpm resolves no release younger than 3 days and checks the whole lockfile at every install; exceptions only in `minimumReleaseAgeExclude`, with a comment and the user's approval. The Tauri CLI is `@tauri-apps/cli` 2.12.4, a dev-dependency of `apps/desktop` (approved in P8.2, [ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md); 2.12.4 for NSIS 3.13 and a clean uninstall, P12.5, [ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), U26).
- **Runtime & hosting:**
  - Docker + Docker Compose
  - Caddy 2 (TLS for the public API): the official `caddy:2.11.9-alpine`, pinned by multi-arch index digest in the compose files, the newest release at least 7 days old when chosen (P12.2; ADR-0022, U13), *except 2.11.9 itself, taken at 4 days old on 2026-11-09 for a remotely reachable Critical in 2.11.7's Go HTTP/2 server (ADR-0024)*; it serves the `probe` service's busybox `wget` and the smoke test's stand-in proxy too
  - `itzg/minecraft-server` (test server)
  - the images' base images (P5.6, P12.1), pinned by multi-arch index digest in `deploy/docker/agent.Dockerfile` and `server.Dockerfile`, the same in both (a test holds them equal): `rust:1.99.0-slim-trixie` (builder) and `gcr.io/distroless/cc-debian13:nonroot` (runtime). *The runtime's digest moved on 2026-11-09 for zlib's CVE-2026-76844 and CVE-2026-85091, a security exception to the 7-day rule ([ADR-0024](docs/adr/0024-sqlite-zlib-and-caddy-advisories.md)).*
  - the SFTP account's throwaway test image (P12.2, test-only): `rust:1.99.0-slim-trixie` again, with Debian 13's `openssh-server` from apt ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), U14)
  - **Grype** v0.121.0 (Apache-2.0), the weekly audit's image scanner, installed in CI as its release binary checked against its SHA-256, the newest release at least 7 days old when chosen (P12.1; ADR-0022, U12)
  - **ShellCheck** 0.11.0 (GPL-3.0) for `deploy/`'s POSIX sh scripts, in CI through `taiki-e/install-action` (P12.2; ADR-0022, U15)
  - the screenshot baselines' image (P11.11, group L4), pinned by multi-arch index digest in `deploy/docker/shots.Dockerfile`, the one place it's named: `mcr.microsoft.com/playwright:v1.64.0-noble`, its tag always `@playwright/test`'s exact version, so a Playwright bump moves the tag and the digest together and renews the baselines (`scripts/shots.test.mjs` holds the two equal; [ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), U97, S130)
- **Windows build dependencies:** MSVC Build Tools ("Desktop development with C++"), NASM (for aws-lc-rs). CMake isn't needed for non-FIPS builds.
- **Automation:** GitHub Actions (CI). `actions/upload-artifact` v7.0.2 and `actions/download-artifact` v8.0.2 (MIT), pinned by SHA, are approved for the release workflow alone, to hand files between its jobs (P12.5; [ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), U31). No Dependabot or Renovate: `cargo deny` and `pnpm audit` flag vulnerable dependencies in CI (also on a weekly schedule), and dependency updates are done by hand in their own PR.

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

> Note (§6, Phase 10, group F) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md), S63): **what a disconnected bot rejoins with while the server is unreachable.** The agent keeps each bot's last session the server granted it, in memory, and hands it out again only while it has no stream to the server, for a request that isn't fresh, while more than the connect timeout is left (an offline bot's name always). A bot whose session was rejected, which the server denied, or which the server stopped or removed waits for the server; so does every bot past its session's expiry (about 24 h). An agent that restarts meanwhile holds no sessions. Changes the server can't send meanwhile (a stop, a disabled owner or agent, a deleted account) reach the agent when it's back, as for connected bots.

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
  - **2FA is replaced, never removed** (group D, the user's decision): `POST /me/mfa/reset` (step-up) answers a setup token, and confirming a new authenticator with it swaps the secret and the recovery codes and revokes every other session of the user. Until then the old authenticator keeps working, so an account never stands without 2FA.
  - Login is two steps: the password returns a single-use `mfa_token` (5 min), and that token plus the code returns the real tokens.
- **Registration only by invite**
  - Admins and above create invites; each is single-use and expires (default 72 h).
  - An invite carries a fixed role: Member, or Admin if the Owner created it.
- **Bootstrap.** `afkfleet-server user create-owner` (CLI, hidden password prompt). It refuses if an Owner already exists.
- **Step-up.** These actions require re-entering the password within the last 5 min, and since v0.1.2 nothing else does ([ADR-0028](docs/adr/0028-a-session-is-enough.md)):
  - changing the password
  - replacing 2FA (`POST /me/mfa/reset`, group D: the new authenticator replaces the old one at confirm)

> Note (§7.2, Phase 13, P13.9, the user's decision) ([ADR-0028](docs/adr/0028-a-session-is-enough.md)): **a session is enough.** Once logged in with password and 2FA, the app never asks for the password again. The list above lost every entry but the two that replace a login credential: deleting an MC account, changing a role, disabling or enabling a user, creating or revoking an invite, approving or refusing a pending link, creating, changing or revoking a grant, and setting a bot's server address are plain authed routes, as are the later additions the notes below record (a command off the allowlist, the modes' `/with-commands` twins, deleting a mode, the servers' settings, disabling an agent, releasing its bots, leaving a shared account). Roles and grants are unchanged. The two that stay can lock a person out for good, so they keep asking (decided in the ADR, on the reviewer's recommendation). The notes below this one say how each step-up came; they're history now, except for the mechanism itself. Rotating keys never had a route (Phase 9, group A).

> Note (§7.2, Phase 10, group E) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the bots' writes,** decided per action (S47; decided by the review agent, approved by the user at the plan review). `PATCH /bots/{id} {server_address}` registers with `step_up_route`: the address sends the account, its mode's actions and later its chat to a server the person at the PC picks, and it outlasts the session. The mode switch (`PUT /bots/{id}/mode`) and the five lifecycle calls are plain authed routes: Control-level, nothing lost or widened, undone by the next call. A start or a resume may kick a human playing the account once, who pauses the bot again by logging in, so a step-up on one of them alone would protect nobody. The threat model records the unlocked PC's bots as an accepted risk.

> Note (§7.2, Phase 9, group E) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **the account writes.** `DELETE /accounts/{id}` (§7.2's "deleting an MC account"), `POST /accounts/{id}/approve` and `/refuse`, and every grant write (`POST /accounts/{id}/grants`, `PUT` and `DELETE /accounts/{id}/grants/{user}`) register with `step_up_route`, checked by the extractor only, as the admin writes are. Listing accounts and grants, and starting a link again (`POST /accounts/{id}/relink`), stay plain authed routes: linking again can't move an account to another Minecraft profile, and gives less than a new link, which needs none (decided by the review agent, approved by the user at the plan review).

> Note (§7.2, Phase 9, group A, the user's decision) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **rotating keys is the command line's only.** `afkfleet-server vault rotate-key` runs on the server's host, with the server stopped. A new key is a file the operator adds to the config, which no API request can do, so there's no route and no step-up to decide for it.

> Note (§7.2, Phase 8, group E, the user's decision) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)'s amendment, [ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)): **every admin write is a step-up action.** `PATCH /users/{id}` (a role or `disabled`), `POST /invites` and `DELETE /invites/{id}` register with `step_up_route`, checked by the extractor only, so an unlocked PC with a live Owner or Admin session can't change roles, disable users or mint invites without the password. Listing stays a plain authed route. The app sends the action and, on 403 `step_up_required`, asks for the password and retries once.

> Note (§7.2, Phase 7, the user's decision) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **step-up is a session stamp.** `POST /auth/step-up {password}` sets the session's `stepped_up_at`; a step-up action needs it at most 5 min old, otherwise it answers 403 `step_up_required`. A wrong password counts toward the lockout. No request body carries the current password.

> Note (§7.2, Phase 7, group D, the user's rule) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **every commit that acts on a verified password re-checks it.** Inside its own write transaction it re-reads the user's `password_changed_at` (and `disabled`), and the session where there is one, and refuses when they changed since the password was verified: login's code step, a 2FA confirm, step-up, and E's `/me/password` and anything later that verifies a password. A re-hash with the current parameters writes only the hash, never `password_changed_at`: it isn't a password change.

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
> - **Agents.** Only the Owner enrolls agents; viewing and disabling them is Admin+. *(Since Phase 11's group J, enrollment tokens come only from the server's host; ADR-0020, U76.)*
> - **Roles.** Only the Owner changes roles, so Admins manage Members by disabling and enabling them. Ownership transfer gets a permission together with a route.
> - **Modes.** Private modes follow the account rule. Shared modes are edited by their creator, while an Admin, and by the Owner. Built-in modes are read-only.
> - **The Owner's "everything"** leaves out: built-in modes, their own role and account status, another Owner, Owner invites, grants to themselves, and other users' personal resources.
> - **Server settings** get a permission when a route exists. *Planned at Phase 11's group H4 planning ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), U54, S75): `ViewServerSettings` and `ConfigureServerSettings`, both Admin+, every write with a step-up.*

> Note (§7.3, Phase 9, group E) (ADR-0010, [ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **as built,** the table's "Owner and Admin implicitly have Manage on every account" is ADR-0010's: an Admin's implicit Manage covers Members' accounts and their own, never the Owner's or another Admin's, which stay 404 for them without a grant (the user's reading of "Admins and the Owner see every account", group E). A pending link is approved or refused by someone above its owner by current role, the Owner for Admins' and Members' links and an Admin for Members' (`Permission::ReviewLink`); a grant never counts. Nobody gets a grant who already has implicit Manage on the account: its owner, the Owner, or an Admin on a Member's account.

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
| Bot calls that make its account join (start, restart, reset, resume, a server change) | 5/min, burst 5 | Bot |
| `SendChat` actions inside modes | Interval ≥ 30 s | Enforced by mode validation |
| WebSocket | 3 connections per user; ticket single-use, 30 s TTL | User |
| Microsoft link flows | 1 active per user, 3 active in total, 10/day per user | User |
| Grants created by username (`POST /accounts/{id}/grants`) | 10/hour | User |
| gRPC enrollment | 5/min | IP |
| gRPC `Connect` | 10/min | Client certificate (its fingerprint) |
| Fresh session requests on an agent's stream | 6/hour, burst 6 | Bot |

Responses use `429` with a `Retry-After` header. The client IP is the socket peer address.

> Note (§7.4, Phase 10, group F) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the per-bot limit on fresh sessions** is `[rate_limits] fresh_sessions_per_bot_per_hour` (1–60, 6 by default: a burst of 6, then one every 10 minutes), keyed by the bot, across streams and agents. Only a `SessionRequest` with `fresh` spends it, once the registry found the asking stream holding the bot assigned, and before the request is queued; past it, the answer is `SessionDenied { retryable: true }` and the session-token service isn't called. Each fresh session the server mints costs a Microsoft refresh, and Microsoft's 429 pauses every refresh, so a compromised agent could otherwise stall every account (S58, the user's approval at the plan review). Its refusals are counted in the minute's summary.

> Note (§7.4, Phase 11, group A) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S3, the user's approval at the plan review): **the per-bot chat rate** is the server's setting, `[rate_limits] chat_per_bot_interval_secs` (1–60, 3 by default) and `chat_per_bot_burst` (1–8, 3), sent to agents in each bot's spec, so the agent's bucket, which mode chat shares, keeps the same rate as the server's own limit on chat through the API (group B). The burst stops at 8 (the user's decision at PR #59's review, U2): vanilla's chat throttle adds 20 for each message and kicks once its count reaches 200, so 10 in one tick are kicked, and the agent enforces the rate too; a slow test proves a full burst of 8 isn't. A spec with another rate never refills the bucket: it keeps what it held, up to the new burst. A standalone agent uses the default. `chat_per_user_per_min` (1–60, 20) arrives with group B's routes.

> Note (§7.4, Phase 11, group B1) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S3): **the chat limits through the API**: `[rate_limits] chat_per_user_per_min` (1–60, 20 by default; a burst of 20, then one every 3 s), keyed by the user, then the bot's own chat rate, keyed by the bot, both spent after `authorize()` on `POST /bots/{id}/chat` and `/command` whatever then happens, so neither tells a stranger the bot exists; the pointer to `/command` and a missing step-up spend neither. The agent's own refusal at its rate (`RateLimited`, `QueueFull`) answers the same 429, its `Retry-After` the bot's interval. Both limits' refusals are counted in the minute's summary.

> Note (§7.4, Phase 11, group D1) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S29, S35, U12): **the WebSocket row as built.** `[rate_limits] ws_connections_per_user` (1–10, 3 by default) counts each user's open sockets: a place is taken before the upgrade and given back at every end, a panic included. One more is 429 `too_many_connections` with `Retry-After: 45`, counted in the minute's summary as `ws_connections`. A user holds as many unspent tickets as sockets (3 by default; one more evicts their oldest, R26 at PR #66's review); `POST /events/ticket` spends the authenticated API's per-user limit only.

> Note (§7.4, Phase 10, group E) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the per-bot limit** is `[rate_limits] bot_actions_per_bot_per_min` (1–60, 5 by default: a burst of 5, then one every 12 s), keyed by the bot and spent in the handler after `authorize()`, whatever the call's outcome, so a 429 never tells a stranger a bot exists. Start, restart, reset, resume and `PATCH` spend it; stop, the mode switch and the reads don't. Without it, the per-user limit (10/s) would let one account join ten times a second: the server's throttles and bans, and Mojang join spam from the agent's address (S52, the user's approval at the plan review). Its refusals are counted in the minute's summary.

> Note (§7.4, Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`Connect`'s limit** is `[rate_limits] connect_per_cert_per_min` (1–60, 10), keyed by the presented certificate's fingerprint and spent before any lookup or read; it answers `RESOURCE_EXHAUSTED` with `retry-after` and is counted in the minute's summary (S30). A call without a certificate is refused before it (S24). What an agent sends on its stream has no rate limit: HTTP/2's flow control and one message read at a time pace it (S25). `X-Forwarded-For` is honored only when the peer is in `trusted_proxies` (Caddy).

> Note (§7.4, Phase 9, group D, the user's decisions) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **The Microsoft link flows' caps.**
> - **Config:** `[rate_limits] link_flows_total` (1–16, 3) and `link_flows_per_user_per_day` (1–100, 10). "1 active per user" is a fixed rule: a new start replaces the user's waiting flow.
> - **Counting:** the daily cap counts the user's `account.start_link` entries in the audit log, in a rolling 24 hours, so it survives a restart. Refused starts aren't recorded.
> - **Answers:** the daily cap is 429 until enough starts are a day old; a start while the user's flow is finishing is 429 with 5 s; every place taken is 503 `busy` with `Retry-After: 30`.

> Note (§7.4, Phase 9, group E) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **grants by username** spend their own per-user limit, `[rate_limits] grants_by_name_per_user_per_hour` (1–100, 10 an hour, burst 10; the user's approval at the plan review), in the handler right after the step-up check, so a Manage holder can't guess names fast. Linking an account again counts toward the link flows' caps like a new link.

> Note (§7.4, Phase 7, group C, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
> - **"All requests"** includes the health checks and unmatched paths.
> - **Keys:** an IPv6 client is counted by its /64.
> - **The trusted proxies:** behind them, the client is the right-most `X-Forwarded-For` address that isn't itself a trusted proxy.
> - **The login routes share one bucket.**
> - **The lockout:** a locked login answers 429 before any password check and isn't counted. A streak is forgotten 24 h after its last failure. Unknown usernames lock exactly like real ones, and the typed name is never stored.
> - **Refusals:** a refused request is logged at `debug`, and the server logs one summary of refusals a minute.

### 7.5 Secrets & cryptography
- **Vault**
  - Cipher: XChaCha20-Poly1305.
  - Envelope: `version | key_id | nonce(24) | ciphertext`.
  - AAD: `record_id ‖ purpose`. A ciphertext can't be moved to another record or purpose without failing.
  - Master keys load from files (Docker secrets) into `SecretBox`.
  - `afkfleet-server vault rotate-key` re-encrypts everything inside one transaction.

  > Note (§7.5, Phase 9, group A, the user's decisions) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **`vault rotate-key` as built.**
  > - **What it re-encrypts:** every envelope under another key than `[vault] active_key_id`, and nothing already under it.
  > - **Where the crypto runs:** before the transaction, which swaps each row only while it still holds the envelope that was read, and rolls back if anything changed.
  > - **A bad envelope:** one that doesn't open stops it, with a Failure entry each.
  > - **The database:** it never creates or migrates it.
  > - **`vault status`** shows, without any key, what each key ID still seals.
  > - **The procedure** (stop, copy the database, add and activate the new key, rotate, check the status, drop the old key from the config but keep its file while older backups exist, start) is the runbook's [Rotating the vault key](docs/runbook.md#rotating-the-vault-key).
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
  - Security headers: `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `Cache-Control: no-store`, `Content-Security-Policy: default-src 'none'; frame-ancestors 'none'` and `X-Frame-Options: DENY` (the last two added in P6.6), on every response; HSTS at Caddy.
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
| E2E | `apps/desktop/e2e/`, `deploy/` | UI flows: Playwright in Edge, under the release CSP, against the e2e build (the desktop app's UI and TauriApiClient over a fetch-based `invoke`), with MSW's handlers in the Playwright process (P8.9); Compose smoke test | Playwright, MSW, Docker Compose |

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
- **Coverage gates** (cargo-llvm-cov): `fleet-core` ≥ 90 %, `fleet-runtime` ≥ 85 %, auth and vault modules ≥ 90 %, the Microsoft accounts' modules ≥ 90 % (`msauth`, P9.3, [ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)), the control plane ≥ 90 % (`control-plane`: fleet-server's `infra/pki/`, `app/enrollment.rs`, `app/agents/` and `grpc/`; Phase 10, the user's choice U9, [ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)), the agent's control plane ≥ 90 % (`agent-control-plane`: fleet-agent's `managed/` and `enroll/`; Phase 10, the user's choice U20), `fleet-client` ≥ 90 % (P8.1, [ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)), the desktop crate ≥ 90 % (P8.4, measured on Windows by `just cov-desktop`), workspace ≥ 80 %. Per-crate and per-module gates are checked by a small script over the llvm-cov JSON report.
- **Mutation testing** (cargo-mutants) on `fleet-core` and the auth and vault modules, from Phase 13: `just mutants`, before a release and after a larger change to them ([ADR-0031](docs/adr/0031-mutation-testing.md)).
- **nextest profiles:**
  - `default`: fast, under ~60 s.
  - `slow`: tests named `slow_*`; needs Docker.
  - `ci`: JUnit output, no retries.

---

## 9. Phases

**Conventions**
- Each task has an ID `P<phase>.<n>`. 🔴 means *the test comes first*, strictly. Tasks without 🔴 (scaffolding, config, wiring) are verified by `just check`, CI or the task's demo, but any logic they add still gets tests.
- A phase is finished when every task is ticked **and** its Definition of Done (DoD) holds.
- **Branches:** one branch per task, `p<phase>/<task-id>-<slug>` (e.g. `p2/p2.6-bot-state-machine`). Phases 0 and 1 each use a single branch, `p0/foundation` and `p1/azalea-spike`. Phase 2 uses five group branches, one PR each (see the note under Phase 2). Phase 3 uses five group branches plus one task branch for P3.9 (see the note under Phase 3). Phase 4 uses five group branches (see the note under Phase 4). Phase 5 uses four group branches (see the note under Phase 5). Phase 6 uses five group branches (see the note under Phase 6). Phase 7 uses five group branches (see the note under Phase 7). Phase 8 uses five group branches (see the note under Phase 8). Phase 9 uses six group branches (see the note under Phase 9). Phase 10 uses eight group branches (see the note under Phase 10). Every branch ends in a PR that the user reviews and merges. The `Plan.md` checkbox is ticked in that same PR.
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

  > Note (P0.1): Deviation: `.claude/settings.json` was dropped from this task and from the §4 layout at the user's request; `CLAUDE.md` security rule 12 states the secret-file rule as an instruction instead, and `.claude/settings.local.json` stays in `.gitignore`.
- [x] **P0.2** Pick the target Minecraft version and the matching azalea release or git rev. Pin a **dated nightly** in `rust-toolchain.toml` with the components `rustfmt`, `clippy` and `llvm-tools-preview`. Record this in ADR-0003, including the bump procedure (nightly + azalea + test-server `VERSION` change together).

  > Note (P0.2): azalea `=0.16.0` from crates.io, so Minecraft 26.1 (the user's choice; 26.2/26.3 servers need ViaVersion/ViaBackwards), on `nightly-2026-08-21`, since the first candidate `nightly-2026-10-01` fails to compile azalea 0.16.0; `spikes/toolchain-check/` stays in the repo for future bumps (ADR-0003).
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

  > Note (P0.3): Deviation: dependencies are declared on first use. The pinned nightly's `cargo::unused_workspace_dependencies` lint fires once per unused workspace dependency, so at the user's choice `[workspace.dependencies]` holds only crates in use. §5 pins the checked versions and feature constraints, each phase copies its entries from there, and no lint was changed.
  > Caret requirements plus `Cargo.lock`, `=` only for azalea, with utoipa 6 and tokio-tungstenite 0.29 (ADR-0009). Also `exclude = ["spikes"]`, so nested spikes build on their own, and `version = "0.1.0"` in `[workspace.package]`.
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

  > Note (P0.5): Deviation: crates.io is the only source, since azalea is pinned from crates.io (ADR-0003). Stricter than the plan: `ring` is banned too, plus `unsound = "all"` and `yanked = "deny"`; `[graph] targets` covers only Windows MSVC and Linux GNU, and `unused-allowed-license = "allow"`. The heads-up for P3 (azalea 0.16.0's two licenses and three advisories) is in ADR-0003.
- [x] **P0.6** Add `.config/nextest.toml` with the profiles `default` (excludes `test(/^slow_/)`), `slow` and `ci`.

  > Note (P0.6): Deviation: the filter is `test(/(^|::)slow_/)`, not `test(/^slow_/)`, because nextest matches the full test path, so a unit test is `tests::slow_join`.
- [x] **P0.7** Add a `justfile` with the recipes listed in `CLAUDE.md`. Set `set windows-shell := ["pwsh", "-NoLogo", "-NoProfile", "-Command"]` (PowerShell 7).
  - Every recipe must also run in `sh` on Linux CI: one command per line, no `&&`/`||`.
  - Set environment variables with just's `export`, e.g. `export RUSTDOCFLAGS := "-D warnings"`.
  - Anything more complex goes into a script under `scripts/`.
  - `check` also runs `cargo doc --no-deps --workspace`.

  > Note (P0.7): Recipes whose tools arrive later started as stubs that print their phase and exit 1. `check` also runs doctests and the `scripts/` tests, and `check` and `ci` are composed of building-block recipes, one per CI job (ADR-0009).
  > `scripts/coverage-gates.mjs` is plain Node, tested with `node --test`. *Confirmed in Phase 7, group A ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): the auth and vault gates are lists of path prefixes that fail on a prefix without files, required from group A on; each Phase 7 group adds its modules.*
  > `.cargo/config.toml` (a file not in the §4 layout) sets `AWS_LC_SYS_PREBUILT_NASM=0` for every cargo invocation, so a missing NASM fails the build (ADR-0009, P0.10).
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

  > Note (P0.10): Verified with `spikes/tls-provider/` (deleted in P12.8, U47; it stays in git history), not a workspace member (the user's choice), on Windows (MSVC + NASM) and in `rust:1-bookworm`. `cargo tree -i ring --target all` is empty, `openssl-sys`, `native-tls` and `webpki-roots` aren't in the graph, and `.cargo/config.toml` enforces NASM. `tauri-plugin-updater` and `testcontainers` default to ring, so they need `default-features = false` (§5), which the ring ban enforces.
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

  > Note (P0.11): Every job runs one `just` recipe, with just and the cargo tools from `taiki-e/install-action` at pinned versions and the nightly from `rust-toolchain.toml`. `clippy` and `test` also run on `windows-latest` (NASM from `ilammy/setup-nasm`). `frontend` also runs the `scripts/` tests and `pnpm audit`, and `audit.yml` runs `just deny` and `just ui-audit` weekly (Mondays) and on demand.
  > Job names for the required checks: `fmt`, `clippy (ubuntu-latest)`, `clippy (windows-latest)`, `docs`, `test (ubuntu-latest)`, `test (windows-latest)`, `coverage`, `deny`, `stable-check`, `frontend`.
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

  > Note (P0.12): ADR-0009 ("Phase 0 dependency and tooling conventions") was added for Phase 0's setup decisions: the version policy and declaring on first use, the single TLS provider, utoipa 6 and tokio-tungstenite 0.29, the stricter cargo-deny settings, and CI calling `just` with Node scripts. ADR-0008 stayed reserved for P1.10, and ADR-0004 `Proposed` until the spike confirmed it.
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

> Note (P1): One branch, `p1/azalea-spike`, and one PR, like Phase 0 (the user's request). Spike-only dependencies, approved by the user: `tracing` and `tracing-subscriber` (azalea's own logs), `uuid` and `reqwest` (P1.8, named by `AccountTrait`), all at their §5 versions; the workspace doesn't change.

- [x] **P1.1** `just mc-up` starts `itzg/minecraft-server` with:
  - `EULA=TRUE`, `ONLINE_MODE=FALSE`, `VERSION=<pinned>`
  - a flat world, low view distance
  - RCON enabled

  A bot joins it with `Account::offline`.

  > Note (P1.1): `deploy/compose.dev.yaml` (project `afkfleet-dev`, service `minecraft`, image pinned by tag and digest), bound to `127.0.0.1:25565` only. RCON uses the image's random per-start password on an unpublished port, through `docker compose … exec minecraft rcon-cli`. `MAX_PLAYERS` defaults to 60 (`MC_MAX_PLAYERS`) for the load tests, and `mc-down` also deletes the world (`--volumes`).
- [x] **P1.2** Run a client on a dedicated OS thread (current-thread runtime + `LocalSet`) and hand `(Client, events)` back to the multi-threaded runtime through a oneshot. Confirm that `Client` is `Send` and can be used from other threads.

  > Note (P1.2): `Client` is `Send + Sync + Clone` and works from tokio worker threads. Three start-up variants were compared; the hand-rolled join ("Variant C") won. `ClientBuilder` and Swarms can self-deadlock on teardown (an azalea bug). See ADR-0008 §1.
- [x] **P1.3** Find out how to build a client **without** `AutoReconnectPlugin` and `AutoRespawnPlugin`, and what failures look like:
  - how a connection failure shows up (`Event::ConnectionFailed`?)
  - what disconnect reasons look like, and whether translation keys are available for banned, not whitelisted, duplicate login, server full and outdated client

  > Note (P1.3): Extra scenarios: IP ban, idle timeout, server stop, `docker kill`, a frozen server (`docker pause`) and an older-version server. Ticks keep going while a frozen server is dead, so the watchdog also needs a packet-liveness timeout, and azalea has no connect timeout. See ADR-0008 §5–6.
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

  > Note (P1.5): `Client::attack` in azalea 0.16.0 sends a mis-encoded packet and gets the bot kicked on 26.1; a raw-packet workaround (`attack_raw`) works. Rotation alone doesn't reset the server's idle timer. See ADR-0008 §8.
- [x] **P1.6** Inject a panic into a custom ECS system. What happens to that client, its event channel, and other clients on the same host thread? Use the result to confirm the `Tick`-based watchdog design.

  > Note (P1.6): Also tested: a hang, a Swarm and pool starvation, in the Linux container. Panics are detected at once (the runner's `AppExit` receiver), so the Tick watchdog is only needed for hangs. Hangs can starve Bevy's process-wide compute pool and freeze every bot, so each App uses the single-threaded executor. See ADR-0008 §3 and §5.
- [x] **P1.7** Resources: compare one world per bot (`Client::join`) against Swarm shards at 10, 25 and 50 bots, measuring RSS, CPU and thread count. Choose a model, weighing in that one panic kills an entire shard.

  > Note (P1.7): Measured in a Linux container (`--cpus 4`, the user's choice). Deviation: "one world per bot" uses Variant C instead of `Client::join` (P1.2), and the matrix added one host thread per bot (ADR-0004's alternative) and Bevy's single-threaded executor. Chosen: one App and one host thread per bot with the single-threaded executor, 222 MiB and 0.43 cores at 50 bots; the default executor needs ~7× the CPU. See ADR-0008 §2.
- [x] **P1.8** Write a custom `AccountTrait` that uses an externally supplied Minecraft access token, UUID and name, and stores the chat-signing certs. Compile-check it. The **user** may test it against a real online-mode server with their own account, running that step themselves. Real tokens never go into code, logs, commits or the AI chat.

  > Note (P1.8): At the user's request, `spikes/azalea/compose.online.yaml` (online mode, secure profile enforced, `127.0.0.1:25567`) keeps the real-account test off public servers; a garbage token and an offline account were tested against it. The user's optional step, `fetch-token` (device code through azalea's re-exported auth, no cache file, token in the gitignored `secrets/`) then `account-join`, was **done by the user on 2026-10-06**: the bot spawned, its chat was accepted with secure profiles enforced, and it disconnected cleanly.
  > Gotchas: auth failures raise no azalea event, `join()` does nothing by default, and azalea's `MicrosoftAccount` derives `Debug` over its token. See ADR-0008 §9.
- [x] **P1.9** Disconnect cleanly and check that no threads or tasks leak and that memory returns afterwards.

  > Note (P1.9): Deviation, approved by the user: "memory returns" means RSS **plateaus** across cycles, and threads, fds and Worlds return to the post-warm-up baseline, since glibc keeps freed memory. 20 cycles of 25 bots in Linux showed no leak. Teardown is `exit()`, dropping every handle and closing the host thread; `disconnect()` alone keeps the ECS running. See ADR-0008 §10.
- [x] **P1.10** Write **ADR-0008 "azalea integration"** with:
  - the answers
  - the chosen model
  - the gotchas found
  - API snippets that `fleet-mc` can reuse

  Update ADR-0004 to confirm or revise it.

  > Note (P1.10): ADR-0008 lists the open questions as answered or explicitly deferred (§11); ADR-0004 is Accepted, revised to one host thread per bot with the single-threaded executor. The spike stays in `spikes/azalea/` with an "Archived" banner (the user's choice). The plan changes ADR-0008 lists (P2.4, P2.7, P2.10, P3.2, P3.4, P3.5, P4.6) were accepted in the Phase 1 review and applied in the same PR, together with P3.1, P3.8, P9.3, the fault table (§6) and the agent config (Appendix A).

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
> - **Decisions.** The phase-wide conventions (time, injected randomness, errors per task, persistence formats) and the user's answers to the Phase 2 plan's open questions are in [ADR-0010](docs/adr/0010-fleet-core-conventions-and-phase-2-refinements.md); the notes below summarize the decisions that change a task.
> - **DoD.** "No tokio or IO crates" is checked on normal dependencies: `cargo tree -p fleet-core -e normal`.

- [x] **P2.1** 🔴 ID newtypes `UserId`, `AccountId`, `BotId`, `AgentId`, `ModeId`: uuid v7, `#[serde(transparent)]`, `Display`. They can't be confused with each other.

  > Note (P2.1): IDs are minted with `new_v7(created_at, [u8; 10])` from server-supplied `getrandom` bytes (uuid's `v7` feature stays off), and parsing and deserializing accept only v7. Deviation: serde uses `try_from`/`into` `Uuid` instead of `transparent`: the same wire format, but validated (ADR-0010).
- [x] **P2.2** 🔴 Value objects with fallible constructors (`TryFrom<&str>`). Each one gets a test for every rejection case, plus a proptest that it **never panics on arbitrary input**.
  - `ServerAddress`: host or IP, optional port 1–65535 (default 25565), no scheme or path, ≤ 253 chars.
  - `ChatMessage`: 1–256 chars after trimming, no control characters, no `§`.
  - `McUsername`: 3–16 characters from `[A-Za-z0-9_]`.
  - `Username` (app login): 3–32 characters, normalized to lowercase.
  - Command detection: `ChatMessage::command_name()` for allowlist checks.

  > Note (P2.2): `ChatMessage` counts the 256 limit in UTF-16 code units and rejects bidi controls; `Username` allows ASCII `[a-z0-9_.-]`, starting with a letter or digit; `ServerAddress` remembers whether a port was given, rejects non-ASCII hosts, `_`, a trailing dot and userinfo, requires a domain's last label to start with a letter, and needs brackets around IPv6 when a port follows (ADR-0010).
- [x] **P2.3** 🔴 `IncomingChat` sanitizer: strip format codes and control characters, cap the length, and keep the kind (player, system or whisper) and the sender. This turns untrusted server text into something safe to store and display.

  > Note (P2.3): Kinds are `chat`, `emote`, `whisper`, `announcement` and `system` (ADR-0008 §7); only non-system kinds have a sender (a sanitized name of at most 64 chars, an optional UUID). The text is capped at 1024 chars with a `truncated` flag and keeps `\n`; other control characters, `§` pairs, bidi controls and invisible characters (ranges in `text.rs`) are stripped. Stored as columns, so no serde (ADR-0010).
- [x] **P2.4** 🔴 `DisconnectReason` and a classifier that returns `Transient`, `Permanent(kind)`, `Conflict(DuplicateLogin)` or `AuthInvalid`. Table-driven tests use real kick messages and translation keys from the spike (ADR-0008 §6).

  > Note (P2.4): The input also covers `AuthRejected` (classified `AuthInvalid`) and the transient `SessionCrashed`, `WatchdogTimeout`, `LivenessTimeout` and `ConnectFailed` (including `HostUnavailable`). Known limit: a plain-text duplicate login has no key and classifies as transient; P4.1 adds per-server conflict texts (see P2.6) (ADR-0010).
- [x] **P2.5** 🔴 Resilience policies:
  - `RetryPolicy` wraps `backon`'s exponential builder: base, factor, cap, jitter, and a reset after a stable period. Tests assert **bounds**, not exact values.
  - `CircuitBreaker` (closed, open, half-open) is pure, with time passed in.
  - Proptests: delay ≤ cap, and the breaker never lets an attempt through while open.

  > Note (P2.5): The factor is fixed at 2. backon's jitter, seeded from the injected RNG, is given `max / 2`, so jittered delays stay ≤ `max` (which requires `max ≥ 2 × base`). `FailureWindow` counts "N failures within a window" for the breaker and later P4.7. One breaker per bot, which only decides how long to wait; Appendix E's "circuit open too long → Failed" arrow is dropped (ADR-0010).
- [x] **P2.6** 🔴 The **bot state machine**: `BotState`, `BotEvent`, `Effect`, and `transition(&state, event, now) -> Transition` (Appendix E). It needs example tests for every transition and proptest invariants:
  - A `Stop` from any state ends in `Stopped`, and emits `Disconnect` if a session exists.
  - `Failed` and `Paused` never emit `Connect` before `Reset` or `Resume`.
  - There is never a `Connect` effect while `Connecting` or `Online`.
  - The attempt counter resets after the stable-online period.

  > Note (P2.6): Deviation: `transition(&state, event, now, &RetryPolicy)`, with the states `Online{since, attempt}`, `AwaitingSession{attempt, fresh}`, `Connecting{attempt, auth_retried}`, `Stopping{restart}`; *from Phase 11 (ADR-0020, U24, S1)* `Backoff{attempt, fresh}`. A new event `CrashLoop`; the state machine owns "request one fresh token, then `Failed(Auth)`"; unfitting events are no-ops; without a session, `Stop` goes straight to `Stopped`, so the first invariant reads "Stop, then SessionClosed, ends in Stopped" (ADR-0010).
  > The user's decisions (2026-10-06): a plain-text duplicate login backs off like any transient kick until P4.1's per-server conflict texts; Paused and Failed are sticky (`Start` and `Stop` are no-ops there, only `Resume`, `Reset` and `CrashLoop` leave them, so the first invariant holds for every other state and the second for any event sequence without `Reset` or `Resume`); the breaker effects are `RecordFailure`, `RecordSuccess` and `ResetBreaker`.
  > `Backoff{n}` and `ScheduleRetry{n}` name the failed attempt, so the first retry waits 5–10 s; `AuthInvalid` while Online counts as transient; `Notify` only on entering Paused or Failed; more proptest invariants: session and mode effects balance, at most 2 connects per `Start`, `Reset`, `Resume` or `RetryDue` (ADR-0010).
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

  > Note (P2.7): `afk` is `RotateRandom` ±30°/±10° every 45–120 s plus `SwingArm` every 20–40 s. Deviations: `farm` is hotbar slot 0 at start plus an attack every 650–800 ms, with no Look; the JSON uses struct variants (`{"type":"select_hotbar_slot","slot":3}`), every field required, and `validate()` is `ModeDraft::validate`, which reading a `ModeDefinition` runs. Probability is a percent from 1 to 100, interval and jitter are each ≤ 24 h, durations count in whole milliseconds, `HotbarSlot` is a 0..=8 newtype since azalea panics above 8 (ADR-0008 §8), and `commands()` feeds the authorization check (ADR-0010).
  > The user's decisions (2026-10-06): at most one at-start and one repeating step each for `AttackFacingEntity` and `SendChat`; angles in degrees (`Look` yaw −180 to 180, pitch −90 to 90; `RotateRandom` `max_yaw` 0 to 180, `max_pitch` 0 to 90, not both 0; NaN and ±∞ rejected); `validate` returns the first `ModeError` in step order; new types fail closed on older servers; the presets are only `ModeDefinition::afk()` and `farm()`, their names coming with P5.1 and their fixed IDs with P11.1 (ADR-0010).
- [x] **P2.8** 🔴 `ModePlan`: a pure scheduler that takes a definition, an RNG and the current time and returns `(next_due, Vec<Action>)`. The runtime then only sleeps and executes. Tests use a seeded RNG.

  > Note (P2.8): Deviation: `ModePlan::start(&definition, now, rng) -> (ModePlan, PlanTick)` runs the at-start steps and `tick(now, rng) -> PlanTick` every due step in step order, with `PlanTick { actions: Vec<PlannedAction>, next_due: Option<…> }`. `PlannedAction` is `Game(GameAction)` or `Chat(ChatMessage)`, chat kept apart for the P4.5 queue; `RotateRandom` becomes a relative `Turn`; `GameAction` lives in `fleet_core::mode` (the user's decision), and P2.10's `perform` takes it.
  > Gaps are uniform in [interval, interval + jitter], the first run comes one gap after the start, and a step that ran (or skipped its roll) is due again one gap after `now`, so there's no catch-up. The adapter (P3.6) clamps the pitch, and these types have no serde (ADR-0010).
- [x] **P2.9** 🔴 Authorization: `Role`, `GrantLevel`, `Permission`, `Actor`, `ResourceContext`, `authorize()`.
  - An **exhaustive matrix test** covers every role × grant × permission. It's generated and snapshotted with insta, so every change shows up in review.
  - Edge cases: an Admin acting on the Owner or another Admin, granting above your own level, deny by default.

  > Note (P2.9): An Admin's implicit Manage covers Members' accounts and the Admin's own, not the Owner's or other Admins'. `CommandAllowlist` is a core type, and changing to a mode with a non-allowlisted command needs Manage; `ResourceContext::Mode{owner: None}` is a built-in mode. From the group A review: allowlist entries keep the slash (`"/spawn"`, at most 64; a missing key is an empty list) and are compared exactly and case-sensitively, so `/Spawn`, `/minecraft:spawn` and `/spawn` followed by a zero-width space all need Manage (ADR-0010).
  > The user's decisions (2026-10-06): private modes follow the account rule (everyone else gets 404); shared modes are visible to everyone and edited by their creator and the Owner; only Manage edits grants and nobody grants to themselves (a grant to an alt account is an accepted risk, threat model; P11.9 shows the grants at a promotion); only the Owner changes roles, and Admins disable and enable Members; invites follow their role; self-service routes check `ResourceContext::Personal{owner}` with `Permission::UsePersonal`; only the Owner enrolls agents, a deviation from §7.3. *(Since Phase 11's group J, enrollment tokens come only from the server's host; ADR-0020, U76.)*
  > `AuthzError` is `NotFound` (404), `Forbidden` (403) or `WrongResource` (a caller bug); the server address and auto-start need Manage; the Owner's limits leave out built-in modes, their own role, another Owner, Owner invites, self-grants and others' personal resources; step-up (P7) and the account quota (P9.4) aren't in `authorize`; the snapshot `authorization_matrix` has a table per kind of resource (ADR-0010).
- [x] **P2.10** 🔴 Minecraft ports, traits only:
  - `MinecraftConnector::connect(ConnectParams) -> (SessionHandle, SessionEvents)`
  - `SessionHandle` (clone, perform `Action`, send chat, disconnect, read liveness)
  - `SessionEvents::next()`
  - `SessionEvent` (`Joined`, `Chat`, `Died`, `Disconnected`, `ConnectionFailed`)
  - liveness: when the session last saw a `Tick` and last received a packet from the server. The session updates both in place instead of queueing events (ADR-0008 §4–5).
  - `SessionCredentials`, which holds the token as a `SecretString`

  Use RPITIT with `+ Send` futures.

  > Note (P2.10): Deviations: `connect` returns a `Result` and resolves once the session has started (a connect timeout arrives as `ConnectionFailed(TimedOut)`); `SessionHandle` gains `respawn()`; `perform` takes `fleet_core::mode::GameAction` (its slot a `HotbarSlot`) and chat goes through `send_chat`. `ConnectParams` is `{bot_id, server, credentials, connect_timeout}`; `disconnect()` is the full ADR-0008 §10 teardown, returns `()` and is harmless to repeat; `SessionCredentials` is `Offline` or `Online` (ADR-0010).
  > The user's decisions (2026-10-06): `SessionError` is `Closed`, `QueueFull`, `TimedOut` or `NotInWorld`, and `ConnectError` is `HostUnavailable`; only `Chat` may be dropped, and counted, and `next()` is cancel-safe and returns `None` after the one terminal event; liveness is data only, `Liveness { last_tick, last_packet }` as `std::time::Instant`s set at creation, with public fields and no constructor (the fakes, P3.1, build them), compared in P4.6. The traits' associated types and `Send` futures are checked by a doctest (ADR-0010).
- [x] **P2.11** A `thiserror` error enum per module, and crate- and module-level docs.

  > Note (P2.11): Error enums are built in each task, which tests its error paths first; P2.11 audited them (one `Copy` enum per fallible module, never echoing input, every variant tested) and wrote the crate docs (ADR-0010).
  > Flagged: serde_json's own errors quote their input (see P11.4). *Decided in Phase 11, group C ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S12): serde refuses only a request's shape, as the extractor's bare 400 that never shows or logs its message; every value rule is a 422 at its field.*

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

> Note (P3): built in groups A–E, plus P3.9 on its own branch, one PR each; their decisions, the approved dependencies, the lint guards (the `unbounded_channel` ban, fleet-mc's ban on azalea's Microsoft login functions) and the local-only Docker test servers are in [ADR-0011](docs/adr/0011-fleet-mc-and-fleet-testkit-conventions-and-phase-3-refinements.md); azalea's advisories and licenses are accepted or excepted per crate in ADR-0012.

- [x] **P3.1** 🔴 `fleet-testkit`: `FakeConnector` and `FakeSession`. They need to support:
  - scripted connect results
  - injected events (`Joined`, `Chat`, `Died`, `Disconnected(reason)`)
  - liveness timestamps that a test can advance or freeze
  - a log of performed actions and sent chat
  - "hang" (no more ticks) and "fail on action" modes

  Write tests for the fake itself.

  > Note (P3.1): `FakeConnector`, `FakeSession`, `FakeEvents` and a test-side `SessionController` per session. Liveness stamps come from `tokio::time::Instant::now().into_std()`, set when the session is created and following tokio's clock until a test freezes one, so paused time works; hang freezes both and every call fails with `TimedOut`; the fake enforces ADR-0010's event contract, and misuse returns an error instead of panicking (ADR-0010, ADR-0011).
- [x] **P3.2** 🔴 `McHostPool` spawns one host thread per session (ADR-0008 §2):
  - Each thread has a current-thread runtime and a `LocalSet`, and ends when its session ends.
  - Work reaches the thread over a bounded queue.
  - A hung thread is **abandoned** and counted, never joined or respawned. Test this with an injected job that never returns.

  > Note (P3.2): Threads are named `mc-` plus the `BotId`'s last 12 hex characters (Linux's 15-byte limit). Once `abandoned_threads() >= max_abandoned_threads` (default 3, a `NonZeroUsize`), the pool refuses new threads and `connect()` returns `HostUnavailable`; the library never ends the process, the agent does (P5.3). `abandoned_threads()` only counts up, and a thread whose handles dropped while it hung counts once past its deadline (ADR-0011).
  > Threads are never joined: a thread also ends when every `HostThread` clone drops, and its exit signal says when. Its jobs are tasks of its `JoinSet`, with no `CancellationToken`, a deliberate exception to the CLAUDE.md task rule; the tests use real time with upper-bound timeouts, never a sleep (ADR-0011).
- [x] **P3.3** Account adapter: a custom `AccountTrait` for server-issued `SessionCredentials`, and offline accounts for dev and tests.

  > Note (P3.3): `InvalidSession` and `ForbiddenOperation` give `AuthRejected`; the new `AccountRestricted{restriction}` (`Banned`, `MultiplayerDisabled`) is permanent and `SessionServerFailed{failure}` transient, and `FailReason::Kicked` is now `FailReason::Permanent` (ADR-0010, ADR-0011). `refresh()` is a no-op returning `Ok(())`; `join()` runs on Bevy's IO pool under `McConfig::session_join_timeout` (10 s) and reports over a bounded channel of one; the adapter doesn't check `expires_at`; a failed join logs one `debug` line with a fixed label; the redaction tests capture every level from every target and never print the token.
  > Found: azalea logs the chat-signing private key at `trace` (`azalea_auth::certs`), a known limit in the threat model; P5.2 caps `azalea_auth` at `info`, like fleet-server does (P9.3).
- [x] **P3.4** `AzaleaConnector` implements `MinecraftConnector`:
  - azalea's auto-reconnect and auto-respawn are **disabled**
  - connect timeout
  - start, hosting model and executor as decided in ADR-0008 §1–3

  > Note (P3.4): The connect timeout runs from `connect()` until `Joined`, resolving included, 30 s by default from the new key `[runtime] connect_timeout_secs` (Appendix A); a resolve error is `ConnectionFailed(Unresolvable)`. `AzaleaConnector::new(&McConfig)` owns its `McHostPool`; the session's driver runs on its host thread and checks stop, auth reports and the connect deadline before events, so events can't starve the timeout (the user's notice). `ConnectParams` carries the `BotId`, to name host threads (ADR-0010).
  > `disconnect()` closes the bridge, stops the driver (`app_exit_timeout`, 2 s) and shuts the host thread down, idempotent from every clone; no task on the host thread holds a `HostThread` clone. azalea's two mandated unbounded channels carry approved `#[expect]`s; `live_worlds()`, `dropped_chat()` and `ignored_action_bar()` are public.
  > Fault injection: `with_app_hook`, behind the off-by-default `fault-injection` feature; a build with it and without `debug_assertions` hits a `compile_error!`. Found in CI on Linux: a fast refused connect could lose the first events, so an observer on `Add` of `LocalEntity` moves the event sender into the bot when azalea spawns it (ADR-0011).
- [x] **P3.5** 🔴 Map azalea events to `SessionEvent`, with unit tests on the pure mapping functions. Chat goes through the core sanitizer, with the sender taken only from where ADR-0008 §7 allows; kick reasons go through the core classifier input. `Tick` and server events such as `KeepAlive` only update the session's liveness timestamps (ADR-0008 §4–5).

  > Note (P3.5): Only `Chat` may be dropped, and is counted; lifecycle events are always delivered, with at most one terminal event per session, the first injected one winning (the account's auth result, the connect timeout, `AppExit`), and a dropped last `EventSink` ends the session as `Disconnected(SessionCrashed)` (ADR-0010, ADR-0011). `Joined` is the first `Spawn` only, a `Died` before it is held until after it, and received packets come through `ReceiveGamePacketEvent`, so `packet-event` stays off.
  > Chat is logged as a structured field (`?` or JSON), never with `%`; action-bar messages are counted apart (`ignored_action_bar`); chat kinds are read in vanilla's registry order (1 emote, 2 incoming whisper, 4 `/say`, else `chat`), a known limit when data packs reorder them; system messages keep their whole sanitized text and never get a guessed sender (see the chat-format note under P4.1); unknown azalea events are logged at `debug`.
  > Server text goes through fleet-mc's own bounded renderer (4,096 characters or 16,384 steps; cut-off chat is `truncated`), since azalea's `to_string()` grows nested translation keys exponentially; fleet-mc depends directly on `azalea-chat` and `azalea-language` `=0.16.0` for it, and every azalea bump re-checks the renderer and the event variants (ADR-0003, ADR-0011).
- [x] **P3.6** 🔴 Map each `Action` to azalea calls: look, rotate, jump, sneak, swing, use item, attack facing entity (with a reach check), hotbar, respawn, chat.

  > Note (P3.6): Each action runs in one synchronous host-thread job that first checks the components azalea's `Client` methods would panic on (a missing one is `NotInWorld`); `Turn` wraps the yaw and clamps the pitch to −90..=90; `AttackFacingEntity` takes azalea's reach-limited target (nothing in reach: `Ok`, logged at `debug`) and writes the attack packet by hand (ADR-0008 §8); a non-finite angle is skipped, logged at `warn`, and returns `Ok` (ADR-0010). `respawn()` tells the bridge (`EventSink::respawned()`) in the same job, and the live respawn step waits for azalea's `HasClientLoaded` before the second kill (ADR-0011).
  > HoldUse (the user's idea) is feasible: a probe held a bow with `start_use_item`, and a raw `ServerboundPlayerAction{ReleaseUseItem}` through azalea's normal writer shot the arrow; P3.9 builds it (ADR-0011).
- [x] **P3.7** 🔴 Slow integration tests (`slow_*`, testcontainers + itzg, offline mode):
  - join and see the join message
  - send chat and see it echoed back
  - perform every action without an error
  - get kicked by RCON (`docker exec … rcon-cli kick`) and receive `Disconnected` with a reason
  - reconnect

  > Note (P3.7): One test binary, `crates/fleet-mc/tests/minecraft/`: one container per `slow_` scenario, run one at a time by the nextest test group `minecraft`, and a fast `pins` test against `deploy/compose.dev.yaml`; only the game port is published, on `127.0.0.1`. Added: an online-mode scenario (a garbage token gives `AuthRejected`, an offline account `unverified_username`) and fault containment. A bot never sees its own join message, so a watcher bot, AfkBot2, sees it: a deviation from the approved plan's wording, not from this task's (ADR-0011).
  > Chat waits for signing (found by the user's real-account check): a chat message waits while signing is `Pending`, at most until the game state plus `McConfig::chat_signing_timeout` (10 s); `Failed`, or `Pending` at that deadline, gives the new `SessionError::ChatUnavailable`, logged at `warn` once, and the session stays up; a key fetch that hangs keeps chat unavailable until the next reconnect (ADR-0010, ADR-0011).
  > Commands are never signed by azalea. A server that enforces secure chat rejects those with message arguments, while `send_chat` returns `Ok` (found by the user's real-account check; flagged under P4.5 and P11.3). *Decided in Phase 11, group A ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S2): fleet-mc refuses such a command with `ChatUnavailable` before sending, wherever chat must be signed, judged by the server's command tree.*
- [x] **P3.8** 🔴 Clean-up test: after the full teardown from ADR-0008 §10 (not just `disconnect()`), the thread count and the number of live Worlds go back to baseline.

  > Note (P3.8): "The full teardown" is `McSession::disconnect()`, which follows ADR-0008 §10; "not just `disconnect()`" means azalea's `Client::disconnect()`. `slow_teardown_scenario` runs a warm-up and three cycles of four bots, ended by the full teardown, an RCON kick and dropped handles; after each, `live_threads()` and `live_worlds()` are 0 and nothing is abandoned (ADR-0011).
  > Linux only (Windows has no portable count without `unsafe` or a new crate): the OS thread count returns to the baseline within 30 s; the baseline is taken after the warm-up, once no `mc-…` thread is left in `/proc/self/task`, and every check runs while the container is up, so testcontainers' cleanup thread never counts (ADR-0011).

  > Note (Phase 3 wrap-up): The user's real-account check, `manual_real_account_scenario` (the nextest `manual` profile, run only by the user with `just test-real-account`), joins the online-mode container with the user's real token and sends signed chat. The user's results (2026-10-07): passed the join, the signed chat, the token absent from every log at every level, and `PRIVATE KEY` only under `azalea_auth::certs` (which P5.2 caps); `/me` was rejected (ADR-0011). *Changed in Phase 11, group A ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S2, the user's approval): the step, now `emote_refused`, expects fleet-mc's `ChatUnavailable` and a later signed chat's echo before any rejection or emote echo; its verdict is a pure function with default-profile tests. Only the user runs the check.*
  > The `slow-tests` workflow runs weekly (Mondays, 07:17 UTC) and on demand, not a required check; the threat model's B4 section adds two accepted risks (Minecraft servers aren't authenticated, and a server can grow a bot's memory). The Phase 3 DoD is checked in group E and again in P3.9's PR, which finishes the phase.
- [x] **P3.9** 🔴 `HoldUse{on}`: hold the use button down, the way `Sneak{on}` holds sneak (the user's idea, from P3.6). It's its own branch and PR, right after group E.
  - fleet-core: an additive `Action::HoldUse{on}` and `GameAction` variant, with validation and the mode snapshots updated. It's a new tag, so older servers reject modes that use it (ADR-0010).
  - fleet-mc: the mapping that P3.6 verified, with a live test.

  If P3.6 finds it isn't feasible, this task is closed with a note instead.

  > Note (P3.9): Feasible (P3.6's probe; items without a use duration would still need repeated packets). The user's decisions (2026-10-07 and -08): a repeating hold-use step needs at least 500 ms (`MIN_HOLD_USE_INTERVAL`), like an attack, and is never a command; holding and letting go are limited apart, at most one at-start and one repeating step each, so a mode can shoot a bow again and again (ADR-0010).
  > `HoldUse{on: true}` and `UseItem` insert azalea's `StartUseItemQueued` directly, and `{on: false}` removes a use that's still queued, then always writes the release, so a hold and a release in one tick keep their order; `UseItem` now returns `NotInWorld` once the bot has left its world. A hold ends at letting go, death and respawn, a slot change, the item finishing or a disconnect; an at-start hold comes back after a reconnect but not after a respawn (flagged under P4.2).
  > Known limits: right after a throw or a shot, a use can click the projectile instead; a slot change and a hold in the same tick rely on azalea 0.16.0's schedule, not a declared order, so the live `hold_use` step guards it at every azalea bump (ADR-0003, ADR-0011).
  > Note (bug fix, 2026-11-10) ([ADR-0026](docs/adr/0026-a-bot-in-a-minecart.md)): a bot sitting in a minecart clicked its own minecart whatever it faced, since azalea 0.16.0 has no riding. fleet-mc's `riding` module tracks the vehicle, keeps the bot at its seat and picks its target from there, for `UseItem`, `HoldUse` and `AttackFacingEntity`; `slow_riding_scenario` checks it live. Other vehicles are as before.

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

> Note (P4): built in five groups (A–E), one PR each; their decisions, which the user answered on 2026-10-08, are in [ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md), with the dependencies and the determinism rule: `Fleet::new` takes a wall-clock anchor and a seed from its caller, and the runtime never reads the wall clock or the OS's randomness (fleet-runtime's `clippy.toml` bans those entry points).

- [x] **P4.1** 🔴 `BotSpec` (account, server, mode, desired run state) and `BotSnapshot` (state, since, last disconnect reason, attempt, uptime).

  > Note (P4.1): `BotSpec { id, account, server, mode, desired, conflict_texts }` and `BotSnapshot { bot_id, state, since, last_disconnect }` are pure data in `fleet_core::bot`, without serde, so fleet-proto and fleet-server can build them; `attempt()` and `uptime(now)` are derived when read. The conflict texts, kick texts that count as a duplicate login for proxies that kick with plain text (ADR-0010), are at most 16 of 1–1024 characters, compared exactly, and `ConflictTexts::classify` applies them on top of the unchanged `DisconnectReason::classify` ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)).

  > Note (P4.1, the chat format): chat senders are server-attributed, never verified (ADR-0011). The display-only chat format for plugin chat went to the server side (P11.9), scheduled at Phase 11's group H planning ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S66): the per-server texts in H4 (S74–S78), the chat format in H7 (S79, S80, U55, U59).
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

  > Note (P4.2): Deviation: the inbox has no `Start` or `Stop`. It takes one `BotCommand` per `Fleet` call (`UpdateSpec`, `Restart`, `Reset`, `Resume`, `SendChat`), and the bot starts and stops only as the spec's desired state says, so the two can never disagree; "`Stop` during backoff" is an `UpdateSpec` with desired = Stopped ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)).

  > Note (P4.2): the actor executes the core effects and the breaker effects (ADR-0010), publishes every state change, `Died` and `Alert` as `FleetEvent`s, applies the ready session events before any input that ends a session, and tears sessions down off its loop ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)). From the group A review: `CircuitBreaker::permits` doesn't limit the half-open state to one attempt; the actor makes one attempt at a time and reports its outcome before it asks again.

  > Note (P4.2, later changes): holds aren't re-applied after a respawn (ADR-0011) until Phase 11's opt-in `on_respawn` schedule ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S20); an `Ok` from `respawn()` that the server doesn't confirm within 5 s counts as a failure (S23); the session-request timeout is 150 s, not 30 s (S1, PR #59's review). Found and fixed in P4.7 and P4.8 ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): Reset and Resume apply the desired state, and a crashing actor closes its inbox first.
- [x] **P4.3** 🔴 `SessionCredentialProvider` port:
  - In standalone mode it returns offline credentials.
  - In managed mode it asks the control plane (Phase 10).
  - On an expired token it refreshes once, then goes to `Failed(Auth)`.

  > Note (P4.3): the port lives in `fleet_core::mc`, so fleet-testkit provides its fake; `OfflineCredentials` is in fleet-runtime, the managed implementation in fleet-agent (P10.6). The "refresh once" is the core state machine's: the first `AuthInvalid` emits `RequestSession{fresh: true}`, which bypasses any token cache, and the second goes to `Failed(Auth)` (ADR-0010, [ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)).
- [x] **P4.4** 🔴 `ModeRunner` drives the core `ModePlan` through the `SessionHandle`.
  - A failed action is logged and skipped, never fatal.
  - It stops cleanly on disconnect or mode change.
  - Tests assert the timing on a paused clock.

  > Note (P4.4): one `ModeRunner` task per Online session; a failed action logs at `warn` for the first failure of each kind per session, then at `debug`; a mode change lets go first (`HoldUse{on:false}`, `Sneak{on:false}`), then restarts the plan in the same task; the runner wakes at most once per sleep, and a late tick doesn't catch up (P2.8) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)).
- [x] **P4.5** 🔴 Outbound chat queue:
  - bounded at 16, with one governor token bucket per bot
  - when the bucket or queue is full it returns `RateLimited` or `QueueFull` **instead of blocking**
  - user chat and mode chat share it
  - governor's clock is injected, so the tests run on controlled time (see §8)

  > Note (P4.5): admission takes a queue slot, then a token from the bucket (one message per 3 s, burst 3), which outlives the sessions and actor restarts; `send_chat` returns a ticket, and the outcome comes later as `ChatSent` or `ChatFailed`; `ChatUnavailable` drops the message; refused mode chat is skipped and logged ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)). A temporary probe proved that every ban on chrono's and rand's clock and OS-randomness paths in fleet-runtime's `clippy.toml` fires (PR #14's review, as ADR-0010's probe did).

  > Note (P4.5, from Phase 3): mode chat with a command that takes message arguments got nothing special here (ADR-0011); decided in Phase 11: fleet-mc refuses it with `ChatUnavailable`, so the mode's step is skipped at `debug` ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S2).
- [x] **P4.6** 🔴 Watchdog, while Online (fault table in ADR-0008 §5):
  - no `Tick` for `watchdog_timeout`: raise `WatchdogTimeout`, tear the session down and reconnect
  - no packet from the server for `packet_liveness_timeout`: tear the session down and treat it as a transient disconnect

  Both timeouts come from `[runtime]` in the agent config (Appendix A).

  > Note (P4.6): a timer in the actor's `select!` checks every 1 s while Online; a stamp is stale at its timeout (30 s each), so a stall is caught 30–31 s after the last tick or packet; when both are stale the tick stall wins (`WatchdogTimeout`), since a hung host thread stops both; the ready session events go first, so a queued duplicate-login kick pauses the bot instead (ADR-0010, [ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)).
- [x] **P4.7** 🔴 `Supervisor` and `Fleet` handle:
  - actors run in a `JoinSet` or `TaskTracker`
  - panics are detected and the actor restarted, within the intensity limit
  - API: `apply(spec)`, `remove(id)`, `send_chat`, `snapshot_all`, `subscribe`
  - graceful shutdown: cancel, then disconnect every bot within the timeout

  > Note (P4.7): actors run in tokio's `JoinSet` with child tokens; the API adds `reset`, `resume`, `restart`, `snapshot` and `shutdown`, and `apply(spec, restore: Option<StickyState>)`. Restarts never skip the backoff: the supervisor keeps each bot's spec, snapshot, restart window, breaker copy and `ChatBucket`, and `fleet_core::bot::restore` decides the restored state; a panic or a crashed task counts in the window (`FailureWindow`, ADR-0010), and the 6th within 10 min ends in `CrashLoop` ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)).

  > Note (P4.7): Paused and Failed survive an agent restart because the server is the source of truth (P10.5); the standalone agent persists nothing, a documented limit ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)). Above fleet-mc's abandoned-thread limit `connect()` returns `HostUnavailable`; ending the agent there is P5.3's job (ADR-0011).
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

  > Note (P4.8): a fixed 500 cases (`with_cases(500)`, an exception to `PROPTEST_CASES`) in `tests/fleet/chaos.rs`, with panics from the test-only `PanickyConnector`; the storm invariant leaves a deliberate new run's connects out of its bound; probes every 5 s catch the crash gap, and 8 coverage targets must each be reached ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)). Red runs, each a temporary break: the reverted crash-gap fix, `restore()` returning `Backoff{1}` for every state, and `restore()` without `ScheduleRetry` each failed with a shrunk one-bot case.

  > Note (P4.8, a bug found in Phase 6): the test's model missed a bot's last events, so on PR #23 the reconnect counter (4) disagreed with it (3); `settle()` now absorbs the events again after `snapshot_all()`, and CI's seed stays in `tests/fleet/chaos.proptest-regressions` ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)).
- [x] **P4.9** Metrics: bots per state, reconnects, watchdog trips, actor restarts.

  > Note (P4.9): `afkfleet_bots{state}`, `afkfleet_bot_reconnects_total`, `afkfleet_watchdog_trips_total{kind}` and `afkfleet_actor_restarts_total`, with no `bot_id` label; every snapshot write moves the bots gauge, and `Fleet::new` registers every series at 0 in the recorder installed at that moment (see P5.3) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)). Deviation: fleet-mc's diagnostics (counters and gauges, ADR-0011) moved to P5.3, since fleet-runtime may depend only on fleet-core.

> Note (Phase 4 wrap-up, group E): the DoD and the security list, checked on 2026-10-09: `just cov` gives fleet-runtime 98.25 % of lines; the chaos test passes 500 cases, also under `PROPTEST_CASES=1000`; `just test fleet-runtime` (288 tests) takes about 7.9 s; every channel is bounded, chat takes only a validated `ChatMessage`, and no `unwrap`, `expect` or `panic!` is outside test modules ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)).

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
**Introduces:** `clap`, `figment`, `garde`, `tracing-subscriber`, `metrics-exporter-prometheus`, `getrandom`, `rustls`. *(anyhow was planned here; group B didn't need it, so it arrives with the first `main.rs` that uses it, ADR-0014.)*

> Note (P5): built in four groups (A–D), one PR each; their decisions, which the user answered on 2026-10-09, are in [ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md), with the dependencies.

- [x] **P5.1** 🔴 Config (Appendix A):
  - loaded with figment from TOML plus `AFKFLEET_AGENT__…` env variables
  - validated with garde, with clear error messages
  - `[standalone]` and `[control_plane]` are mutually exclusive
  - standalone mode only allows **offline** accounts

  > Note (P5.1): `fleet_agent::config::load` reads the file at exactly the given path, then `AFKFLEET_AGENT__…` overrides; unknown keys are errors; missing `[runtime]` and `[retry]` keys take Appendix A's values, among them `connect_timeout_secs` and `max_abandoned_threads` (ADR-0011) and `max_bots` and `shutdown_timeout_secs` ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)); each bot may list `conflict_texts`, bots whose accounts clash are refused (`BotAccount::clashes_with`), and modes go by name (`ModePreset`, ADR-0010); `name` is an `AgentName` ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)).

  > Note (P5.1): Deviation: garde checks only the ranges; the texts go through fleet-core's constructors in a second pass, so validation lists every problem at once, sorted by key, without echoing values ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)).
- [x] **P5.2** Telemetry: pretty logs in dev and JSON in prod, an env filter, and a panic hook that logs through `tracing`.

  > Note (P5.2): a `[log]` section (`format` json or pretty, `filter`); azalea's targets stay at `warn` unless the operator names them, since azalea's kick rendering grows exponentially on hostile translations and can panic, and `azalea_auth` is capped at `info` whatever the filter says, since `azalea_auth::certs` logs the chat-signing private key at `trace` (ADR-0011, threat model); panic reports (`afkfleet::panic`) always get through, the payload sanitized and cut at 1024 characters ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)).
- [x] **P5.3** Wiring: `McHostPool` + `AzaleaConnector` + `Fleet`, with the standalone spec source.

  > Note (P5.3): `afkfleet-agent run` wires a generic `run`, tested on fakes on paused time; it seeds `Fleet::new` from `getrandom`, installs the Prometheus recorder first, samples fleet-mc's diagnostics into metrics every 5 s, and exits 3 at the abandoned-thread limit (ADR-0011) or 4 when the supervisor ends unasked; the standalone agent persists no Paused or Failed state ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md), [ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)).
- [x] **P5.4** 🔴 Signals (Ctrl+C, SIGTERM) trigger a graceful shutdown within `shutdown_timeout`.

  > Note (P5.4): the first signal (SIGTERM or SIGINT on Linux, Ctrl+C or Ctrl+Break on Windows) calls `Fleet::shutdown(shutdown_timeout)` with one deadline and exits 0 with the report; a later signal is only logged ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md), [ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)). Deviation: `run` takes a signal source (`ShutdownSignals`), not a shutdown future, since one future can't show the second signal.

  > Note (P5.4, from the manual test of PR #20, group C): Ctrl+C reaches `just`, PowerShell and `cargo` too, so `just dev-agent` has `[no-exit-message]`; it still exits non-zero, and the agent's own exit code is the one in "the agent stopped".
- [x] **P5.5** 🔴 A `healthcheck` subcommand. The agent touches a heartbeat file every 10 s, and the check fails when the file is stale. This works in distroless images, which have no curl.

  > Note (P5.5): a task the run owns touches the file every 10 s, only when `fleet.snapshot_all()` answers `Ok`, and never truncates or deletes it; `healthcheck --config <path>` loads the same config, exits only 0 or 1 (Docker reserves 2), and calls a file that's missing, 30 s old or more, or more than 30 s in the future unhealthy ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)).
- [x] **P5.6** `deploy/docker/agent.Dockerfile`:
  - cargo-chef
  - a nightly builder
  - runtime `gcr.io/distroless/cc-debian12:nonroot`

  Also `deploy/compose.dev.yaml` with an itzg server plus the agent.

  > Note (P5.6): Deviation: the runtime is `gcr.io/distroless/cc-debian13:nonroot`, not `cc-debian12`, whose images reached end of life in September 2026 under distroless's SUPPORT_POLICY.md (GoogleContainerTools/distroless#2129); the builder is `rust:1.99.0-slim-trixie`, so the binary's glibc matches ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)).

  > Note (P5.6): production is linux/arm64, so both base images are pinned by multi-arch index digest; `.dockerignore` is an allowlist; the compose agent, after `minecraft` in `deploy/compose.dev.yaml` since fleet-mc's `pins` test reads the first `image:` line, runs read-only with every capability dropped and a `stop_grace_period` of 20 s, above the worst-case shutdown of shutdown_timeout + reply_timeout + 1 s ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md), [ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)).
- [x] **P5.7** 🔴 Slow end-to-end test:
  1. `compose up`, and all bots come Online.
  2. Restart the MC container; the bots reconnect within the policy.
  3. Stop the agent; the disconnect is clean.

  > Note (P5.7): one `slow_compose_scenario` in `crates/fleet-agent/tests/slow_compose/` runs the dev stack as `afkfleet-e2e` with `deploy/compose.isolated.yaml` and five bots; it holds every reconnect wait to the retry policy's bounds and every bot to a deadline from the server's healthy moment, and shares `deploy/dev/stack-checks.json` with the demo ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)). No CI job builds the image on every PR: a PR that changes the Dockerfile, `.dockerignore`, `Cargo.lock` or `rust-toolchain.toml` runs the "Slow tests" workflow on its branch, which the user starts.

  > Note (P5.7): Deviation: the compose-file helpers stay in `tests/deploy.rs`; only the `stack-checks.json` reader moved to `tests/common/`.

**DoD:** Demo with 5 bots AFK on a local server for 1 h, with one server restart in between. The logs show no errors except the expected disconnect warnings.

> Note (DoD): `just demo-agent [minutes]` (`scripts/demo-agent.mjs`) runs the stack for 60 minutes by default, restarts the server at half time, records the agent's memory and CPU, and prints a summary with a verdict, PASS or FAIL; raw data stays under `target/demo-agent/`, and only `summary.txt` goes into a pull request. The Minecraft-version bump runs it (ADR-0003's verify step) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)).

> Note (DoD): Deviation: during a server restart azalea can log a TCP reset at ERROR; the e2e test and the demo allow it only between the agent's last line before the restart and the server's healthy moment, at most once per session closed there with `ConnectionClosed` (`expected_restart_errors` in `stack-checks.json`) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)).

> Note (DoD, the run and the Phase 5 wrap-up, group D): PASS on 2026-10-10, when the Goal was checked too: 5 bots (four on `afk`, AfkBot6 on `farm`) for an hour with one server restart at minute 30, every bot Online again 8–21 s later, no ERROR and only the 16 expected disconnect warnings, and a stop with exit code 0 (stopped 5, aborted 0, crashed 0); the agent's container peaked at 26.9 MiB and 6.59 % of one core (P12.2) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)).

---

### Phase 6: Server foundation (`fleet-server`, `fleet-api-types`)
**Goal:** An HTTP service skeleton that is secure by default, with persistence, an error model and an audit trail, but no business endpoints yet.
**Introduces:** `axum`, `axum-extra`, `tower`, `tower-http`, `sqlx`, `utoipa`, `utoipa-axum`, `ts-rs`, `serde_json`, `tokio-util`.

> Note (P6): built in five groups (A–E), one PR each; their decisions, which the user answered on 2026-10-10, are in [ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md), with the dependencies.
> The phase also added the crate fleet-startup (the binaries' shared start-up code) and time and randomness as injected ports (`Clock`, `SecureRandom`), with `ManualClock` and `SeededRandom` in fleet-testkit, which `just testkit-check` keeps a dev-dependency everywhere.

- [x] **P6.1** Module layout (see `CLAUDE.md`): `config`, `app` (services), `ports`, `infra/{sqlite,crypto}`, `http/{router,middleware,extractors,handlers,error}`, `grpc`, `cli`.

  > Note (P6.1): Deviation: modules arrive with their code; the crate doc describes the whole layout, the task that fills each module and the layering, so the tree has no empty modules ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).
- [x] **P6.2** 🔴 Config:
  - figment + garde
  - secrets only via `*_file` paths
  - fail fast with a clear message

  > Note (P6.2): the agent's rules through fleet-startup's loader, with `AFKFLEET_SERVER__…` overrides; only Phase 6's keys, so a key of a later phase is still an unknown key until then ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  >
  > | Key | Default | Rule |
  > |---|---|---|
  > | `dev_mode` | `false` | When it's on, the server logs one `warn` at startup |
  > | `[http] bind` | `127.0.0.1:8080` | An IP address and a port other than 0 |
  > | `[http] request_timeout_secs` | 15 | 1–300 (1–19 since P8.1, [ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
  > | `[http] max_body_bytes` | 65536 | 1024–1048576 |
  > | `[database] path` | required | Absolute |
  > | `[log] format`, `filter` | `json`, `info` | As the agent's (P5.2) |
  >
  > Deviation: `bind` defaults to `127.0.0.1:8080`, not Appendix A's `0.0.0.0:8080`, so a server started without it never listens on the network. A key whose name has a secret word must end in `_file` or `_files`, or be on the test's allowlist; the loader comes in P7.4.
- [x] **P6.3** SQLite setup:
  - `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5s`
  - a **write pool with 1 connection** plus a read pool
  - migrations embedded with `sqlx::migrate!` and run at startup
  - offline data in `.sqlx/`, and a CI job running `cargo sqlx prepare --check`

  > Note (P6.3): `infra::sqlite::open` prepares the file (a missing folder stops startup; a new file is `0600` on Unix, and one that group or others can use is refused), and `connect_options` sets every per-connection setting: `trusted_schema=OFF`, `query_only=ON` on the 4 read connections, and sqlx's statements at `debug`, slow ones at `warn` ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)). Deviation: the offline data lives in `crates/fleet-server/.sqlx/`; the `sqlx-offline` job runs `just migrations-check` and `just db-check`.
- [x] **P6.4** 🔴 First migration and repositories for `users` and `audit_log`, tested with `#[sqlx::test]`.

  > Note (P6.4): the schema conventions for every table: STRICT; v7 TEXT IDs and millisecond UTC TEXT times, each with a CHECK; CHECKs only on closed sets; append-only logs (`audit_log`, later `chat_messages`) keyed `INTEGER PRIMARY KEY AUTOINCREMENT` and ordered by ID only, with no trigger blocking DELETE, since P11.6 deletes old entries ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).
  > The store's `write()` is a `WriteTx` whose `commit(entry)` records the audit entry, so a change can't be committed unaudited (P7 adds named exemptions, see P7.1's note); the audit vocabulary lives in fleet-core's `audit`.
- [x] **P6.5** 🔴 `ApiError` maps each error to a status code and `{ "error": { "code", "message", "request_id", "fields"? } }`.
  - Internal errors are logged with the request ID and returned as a generic 500.
  - Every variant gets an insta snapshot.

  > Note (P6.5): thirteen codes, one `ApiError` variant each, each with a fixed message; a 401 sends `WWW-Authenticate: Bearer`, a 429 and a 503 `Retry-After`; `render_errors` writes the body with the request ID, from a marker the error leaves in the response's extensions; `internal` logs its sanitized source chain at `error`, `busy` at `warn` ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).
- [x] **P6.6** 🔴 Middleware stack, in this documented order:
  1. request-id (set and propagate)
  2. trace (no bodies, sensitive headers redacted)
  3. `CatchPanicLayer`, which turns a panic into a 500
  4. timeout (15 s)
  5. body limit (64 KiB)
  6. security headers

  Rate limiting is added in P7.

  > Note (P6.6): Deviations: the security headers sit outermost, so every response gets them (outermost first: sensitive headers, security headers, request ID, trace, `render_errors`, catch-panic, timeout, body limit); the request-ID and trace layers are the server's own, not tower-http's; and two security headers more than §7.6 listed, `Content-Security-Policy` and `X-Frame-Options` ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).

  > Note (P6.6): the request ID is a v7 `RequestId` the server mints after removing every `x-request-id` the client sent; `render_errors` gives every bare 4xx or 5xx the envelope; the trace span logs the route's template, never the path, query string, headers or body ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).
- [x] **P6.7** 🔴 `GET /health/live` and `GET /health/ready` (DB ping), with no internal details in the response.

  > Note (P6.7): both routes answer `200` with an empty body, without the `/api/v1` prefix; `ready` pings the read pool (`SELECT 1`) through `HealthService`, and a busy database answers 503, any other failure 500; the trace layer logs both routes at `debug` ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).
- [x] **P6.8** 🔴 Audit service:
  - append-only: the trait has only `record` and `list`
  - each entry: actor, IP, action, target, outcome, metadata (no secrets), timestamp

  > Note (P6.8): `app::audit::AuditService { clock, store }` stamps each entry with the `Clock`; `record(entry)` runs its own short write transaction for a failure or denial whose change was rolled back; the metadata's rules live in fleet-core's `AuditMetadata` ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).
  >
  > **Append-only** means the store has exactly two operations for the log, `AuditWrites::record` inside a `WriteTx` and `AuditReads::list` (P6.4's note); nothing updates or deletes an entry, and no trigger blocks P11.6's retention. *One exception since P11.6 ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), R50, R57):* the retention task deletes the log's oldest entries, a prefix by ID past `[audit] retention_days`, in a transaction of its own (`Store::retention_batch`) that nothing else opens; nothing ever updates an entry.
- [x] **P6.9** `fleet-api-types`:
  - DTOs with serde, garde and `ts-rs`
  - `#[serde(deny_unknown_fields)]` on request DTOs
  - `just gen` exports them to `packages/ui/src/generated/`
  - CI job `ts-types-fresh` fails on any diff

  > Note (P6.9): the error envelope's DTOs plus `SafeInt` (the only way a 64-bit integer crosses the API, within ±(2^53 − 1)), `Open<E>` (a value a later version may extend) and `BoundedText<MAX>`; `just gen` syncs one file per type, and `just gen-check` (CI's `ts-types-fresh`) compares ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)). Deviation: Phase 6 has no request DTO, so garde joins fleet-api-types with P7's first one.
- [x] **P6.10** OpenAPI via utoipa. `/api/openapi.json` is served only when `dev_mode = true`.

  > Note (P6.10): the schemas live in fleet-api-types next to ts-rs, a test keeps them equal to the TypeScript export, and routes register through utoipa-axum's `OpenApiRouter`; outside dev mode the path answers exactly like an unknown one ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)). Deviations: four text fields lose their description, since utoipa doesn't carry a doc comment onto a `schema_with` field; utoipa's `preserve_order` (the user's approval during the build) keeps the field order.
- [x] **P6.11** CLI skeleton: `serve`, `migrate`, `healthcheck`.

  > Note (P6.11): `afkfleet-server` follows the agent's shape (ADR-0014), with no anyhow and `--config <PATH>` on every command; `serve` warns in dev mode and takes an already-bound listener; `healthcheck` is our own HTTP/1.1 probe of `/health/ready` with one 3 s timeout, asking loopback for an unspecified bind; `just dev-server` passes the database path as a `$`-parameter under `target/dev-server/` ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).

  > Note (P6.11, from P13.5) ([ADR-0033](docs/adr/0033-chaos-tests-and-the-missing-database.md)): **outside dev mode `serve` never creates its database.** A missing file is exit 1 with an error naming the file and the folder its volume belongs at; only `migrate` creates one, which a first install runs before the server's first start. Dev mode still creates it. And one more command, `database check`: SQLite's `integrity_check`, read-only, exit 0 sound, 1 it couldn't run, 3 damaged.
- [x] **P6.12** 🔴 Graceful shutdown: axum's `with_graceful_shutdown` plus a `CancellationToken`.

  > Note (P6.12): the stop signals moved to fleet-startup; one deadline, `request_timeout + 5 s` from the first signal, bounds the drain and `Database::close()`; exit codes as the agent's: 0 stopped by a signal, 1 startup error, 2 usage error, 4 the server ended unasked or the database didn't close by the deadline, and 3 unused ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).

**Security:**
- No CORS layer.
- `Cache-Control: no-store` on API responses.
- No secrets in committed TOML.
- The DB file is only readable by the service user.

**DoD:**
- An integration test boots the app in-process on a temporary DB and passes the health checks.
- Snapshots reviewed.
- `just ci` green.

> Note (DoD, group E): checked point by point: `tests/serve/boot.rs` boots `serve::run` on a fresh temporary database, both health checks answer 200 and the healthcheck reports healthy; every snapshot of the phase was read before accepting; CI runs every recipe of `just ci` on the group E PR; the security list holds: no CORS layer, `Cache-Control: no-store` on every response, no secret in any committed TOML, and an owner-only database file on Unix ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).

---

### Phase 7: Authentication & authorization
**Goal:** Invite-only login with 2FA, rotating tokens, central authorization and layered rate limits, all backed by adversarial tests.
**Introduces:** `argon2`, `getrandom`, `sha2`, `subtle`, `base64`, `zeroize`, `chacha20poly1305`, `totp-rs`, `zxcvbn`, `governor`, `hmac`, `rpassword`; no `tower_governor` (group C, the user's decision).

> Note (P7): built in groups A–E, one PR each, with P7.1 and P7.6 spread over several groups and ticked in E; their decisions (the Phase 7 plan's questions, answered by the user on 2026-10-11, then each group's) are in [ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md).
> Security tests and audit actions ship with their feature, pure rules live in `fleet_core::auth`, tests drive the real router with `oneshot`, and the auth and vault coverage gates are lists of path prefixes, required from group A on.

- [x] **P7.1** 🔴 Migrations and repositories: `sessions` (with token families), `invites`, `login_attempts`, `user_mfa`.

  > Note (P7.1): each table arrived with its first user, in its own migration, under the schema conventions of P6.4's note: `sessions` (B), `login_attempts` (C), `user_mfa` (D), `invites` (E); ticked in E.
  > `WriteTx::commit` always records an audit entry; `commit_unaudited(reason)` takes only an enum of named exemptions (`SessionTouch` first, later `EventBatch`), each with its own ADR note, never free text ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md), [ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).

  > Note (P7.1): **IDs aren't secrets** (the group A review): v7 IDs reveal their creation time, so invite codes and session tokens come from `getrandom`, never from an ID, and access is decided by `authorize()`, not by how hard an ID is to guess.

  > Note (P7.1): **`0003_sessions.sql`** (B): one `sessions` row per login's token family, its `SessionId` the same across every refresh: `user_id` (no cascade), `access_hash` and `refresh_hash` (32-byte BLOBs, UNIQUE), `created_at`, `refreshed_at`, `last_used_at`, `stepped_up_at`, `revoked_at`; no expiry columns, no IP address or user agent.
  > `rotated_refresh_tokens (hash, session_id, rotated_at)` keeps every refresh token a session replaced, ON DELETE CASCADE ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).

  > Note (P7.1): **`0004_login_attempts.sql`** (C): one row per username with a running failure streak (`failures`, `last_failure_at`, `locked_until`), keyed by HMAC-SHA256 of the normalized `Username` under the lockout key (P7.10's note), never the name, since people type their password into that field; no `user_id`, so an unknown name's row looks exactly like a real user's. Purges commit with `UnauditedWrite::LoginAttemptsPurge` ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).

  > Note (P7.1): **One Owner** (group D): the `users` migration adds a partial unique index on `role` for `'owner'`, and `authorize()` also refuses to act on any other user with the Owner role (ADR-0010).

  > Note (P7.1): **`0005_user_mfa.sql`** (D): `user_mfa` holds confirmed enrollments only (`user_id`; `secret_envelope`, a vault envelope v1 of the 20-byte TOTP secret bound by its AAD to the user's ID and `totp_secret`; `last_used_step`; `confirmed_at`). `recovery_codes (hash, user_id, used_at)` stores plain SHA-256, safe only because each code has at least 80 bits, and isn't keyed, so P9's key rotation can't make the codes unusable.
  > The login challenges live in memory, not in a table, held as SHA-256 hashes, so a restart drops them ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).

  > Note (P7.1): **`0006_invites.sql`** (E), the last table: `code_hash` (SHA-256 of the `afk_iv_…` code), `role` (`member` or `admin` only), `created_by` and `used_by` (UNIQUE) without cascade, `expires_at` stored at creation, `used_at`, `revoked_at`, and belt CHECKs behind the services.
  > `ports::page` holds the shared `PageLimit` (1–100, 50 by default); `invite_state` is used, then revoked, then expired at `now >= expires_at` ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).
- [x] **P7.2** 🔴 `PasswordService`:
  - Argon2id with the configured parameters
  - runs in `spawn_blocking` behind a semaphore
  - dummy hash for unknown users
  - password policy (length, zxcvbn)

  > Note (P7.2): a full argon2 queue answers `busy` (503, `Retry-After`), like a busy database (Plan.md §6's overload row) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).

  > Note (P7.2): `Passwords` (`Argon2Passwords`) admits at most 4 × `argon2_max_concurrent` waiting callers, moves the hashing permit into the `spawn_blocking` closure, and checks unknown users against a dummy hash made at startup with the configured parameters. `NewPassword` is 12–128 `chars()` without control characters; zxcvbn (score ≥ 3) runs inside `hash`, before argon2.
  > `[auth]`'s argon2 ranges are checked in validation, not by garde, and no constructor below the floor exists, not even for tests; a stored hash of another algorithm or version, one that doesn't parse, or one above the config's bounds is `VerifyError::Corrupt` ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).
- [x] **P7.3** 🔴 `TokenService`:
  - tokens from `getrandom`, with prefixes
  - SHA-256 hashing and constant-time comparison (`subtle`)
  - TTLs
  - refresh rotation, and **family revocation when an old token is reused**

  > Note (P7.3): one `sessions` row per family, every replaced refresh hash kept in `rotated_refresh_tokens`, and a refresh replaces the access token too. Tokens are 32 bytes from `SecureRandom`, base64url behind `afk_at_` or `afk_rt_`, stored as SHA-256 and confirmed with `TokenHash::ct_eq`; expiry is computed from `created_at` and `refreshed_at` with the running config.
  > `refresh_decision` rotates (`commit_unaudited(SessionRotate)`), or for a replaced token of a live session revokes it and records `security.refresh_token_reuse` in the same commit; there's no grace window ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).
- [x] **P7.4** 🔴 Vault cipher: the envelope from §7.5 with `key_id` and AAD. TOTP secrets need it now; Phase 9 extends it. Tests cover tampering, the wrong key and the wrong AAD.

  > Note (P7.4): the `*_file` loader arrived here with its first user, the vault key. P6.2's secret-name test (a key with a part `key`, `token`, `password`, `secret`, `passphrase`, `pepper` or `credential` must end in `_file` or `_files`) lists keys that hold no secret, such as `[auth] access_token_ttl_secs`, on `NOT_SECRETS` in fleet-server's `config/raw.rs`, each with its reason ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).

  > Note (P7.4): the envelope v1 is `0x01 | key_id (u32 BE) | nonce (24) | ciphertext + tag`, its AAD `0x01 ‖ key_id ‖ record UUID (16) ‖ purpose name`, pinned by a committed golden envelope that's never regenerated (a real change means version 2). Key files hold padded base64 of exactly 32 bytes, their characters checked first (a byte-order mark gets its own error).
  > `[vault]` holds 1–16 `master_key_files` and an `active_key_id`; only `serve` reads them, through `fleet_startup::secret_file` (a regular file, `mode & 0o077 == 0` on Unix, not empty, at most 64 KiB, checked again on the opened handle). `just dev-server` creates `target/dev-server/vault-key-1` ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).
- [x] **P7.5** 🔴 CLI `user create-owner`: hidden prompt with `rpassword`; refuses when an Owner already exists.

  > Note (P7.5): `user create-owner --config <PATH> --username <NAME> [--password-stdin]` checks for an Owner before any prompt or stdin read. The prompt asks for the password twice, at most 3 rounds; `--password-stdin` takes one password of at most 1 KiB and is refused when stdin is a terminal; there's no password flag or variable, and both reads run in `spawn_blocking` (the group A review, fixed in group B).
  > Exit codes: 0 created, 1 refused or failed, 2 usage. The entry is `user.create` (`role`, `via`), and `just dev-owner username` runs the command against the dev database ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).

  > Note (P7.5, from P13.5) ([ADR-0033](docs/adr/0033-chaos-tests-and-the-missing-database.md)): `user create-owner` opens the database as `serve` does: outside dev mode a missing file is refused, never created.
- [x] **P7.6** 🔴 Endpoints:
  - `POST /auth/login`, `/auth/login/mfa`, `/auth/refresh`, `/auth/logout`, `/auth/register` (invite)
  - `GET /me`, `POST /me/password` (step-up)
  - `GET /me/sessions`, `DELETE /me/sessions/{id}`

  > Note (P7.6): the first request DTOs bring garde to fleet-api-types: each derives `Deserialize`, `Validate` and `#[serde(deny_unknown_fields)]`, tested for an unknown field and every rule. axum's JSON rejections (a malformed body, a wrong content type, an unknown or mistyped field) get their mapping to `bad_request`, `unsupported_media_type` or `validation_failed` here, with axum's `json` feature.
  > A DTO using garde's `pattern` rule gets the test that a value breaking it appears nowhere in the 422 body ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).

  > Note (P7.6): split between D (`login`, `login/mfa`, `refresh`, `logout`, plus `POST /auth/step-up`) and E (`register`, `/me`, `/me/password`, `/me/sessions`), ticked in E. `POST /auth/step-up {password}` stamps the session's `stepped_up_at`; a step-up action needs a stamp at most 5 min old (otherwise 403 `step_up_required`), and a wrong password counts toward the lockout ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).

  > Note (P7.6): D: the password step never hands out real tokens, only an mfa token (`afk_mt_`, 5 min, 3 wrong codes) or a setup token (`afk_st_`, 10 min). An unknown name, a wrong password and a disabled user are one counted 401 `invalid_credentials`; a name that isn't a valid `Username` is 422 before the lockout or any hashing, and the password field is capped at 1024 characters.
  > Login calls `LockoutService::begin` before any verify; the code step takes one `code` field; a hash made with other parameters is re-hashed best-effort, written in the `auth.login` success commit; refresh checks the read pool first; logout revokes its session; step-up is a plain authed route on the `Login` bucket; every 401 carries `WWW-Authenticate: Bearer`.
  > Deviation (the user's decision): "two concurrent stale logins of the same user both succeed" can't happen under the lockout's in-flight guard, so a cross-flow test pins the property instead ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).

  > Note (P7.6): E: `POST /auth/register {invite, username, password}` re-checks the invite and its creator in one transaction and answers 201 `{id, username, role}`, never tokens; every invite it can't use is one 422 on `invite`, a real one recorded as `user.create` Failure, and a refused new password is 422 "is too weak" with zxcvbn's hints. `GET /me` includes `failed_code_attempts` (group D's suggestion) and an optional `mfa_confirmed_at`.
  > A credential change revokes every other session through `revoke_all_for_user(user, except)`: a 2FA replacement (D) and `POST /me/password` (`step_up_route`). `GET /me/sessions` lists the live sessions in keyset pages; `DELETE /me/sessions/{id}` is 409 for the current one and 404 for someone else's ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).
- [x] **P7.7** 🔴 2FA:
  - enroll returns the otpauth URI and a QR code as PNG data
  - confirm, then reset (with step-up), which forces a fresh enrollment at the next login
  - recovery codes
  - replay protection
  - mandatory for **every** user, not configurable: until they enroll, login returns `mfa_setup_required` plus a single-use setup token that only works for enroll and confirm (§7.2). That includes users who just registered with an invite.
  - tests: the setup token is rejected on every other endpoint, expires after 10 min and can't be reused

  > Note (P7.7): **Deviation: the QR code is SVG, not PNG.** totp-rs's `qr` feature brings image codecs (some with SIMD and unsafe code), so the server draws qrcodegen's module grid as an SVG, sent as `data:image/svg+xml;base64,…`; the app shows it only through an `<img>` ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).

  > Note (P7.7): enroll and confirm take only a setup token (`Access::Setup`), and the registry test refuses a live setup token on every authed and step-up route; enroll may be repeated, its pending secret waiting in memory, and confirm stores its own step, so the confirming code can't log in afterwards. TOTP is SHA-1, 6 digits, 30 s steps, ±1 step, accepted only for a step after `last_used_step`; 10 recovery codes of 20 base32 characters; wrong codes count toward the lockout (group C's open question).
  > Deviation: replace, never reset. `POST /me/mfa/reset` (step-up) answers a setup token bound to the session, nothing changes until its confirm, and `DELETE /me/mfa` doesn't exist ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).
- [x] **P7.8** 🔴 The `AuthUser` extractor (Bearer token via `axum-extra`) loads session and user and rejects disabled users. A `require(permission, resource)` helper calls `fleet_core::authz::authorize`.

  > Note (P7.8): every refusal is the same 401 (`ApiError::Unauthorized`, a bare `WWW-Authenticate: Bearer`): no header, more than one `Authorization` header, another scheme, a malformed, unknown or expired token, a revoked session, a disabled user; a database failure is 503 or 500.
  > One joined lookup per request, decided by `access_decision`; `last_used_at` is touched at most once a minute (`commit_unaudited(SessionTouch)`, only the wait for the write connection bounded, at 250 ms); `Bearer` matches in any case, the spaces after it trimmed (RFC 6750) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).

  > Note (P7.8): `AuthzError::NotFound` becomes 404 and `Forbidden` 403; `WrongResource` is a bug in the handler: 500, logged at `error`. `From<AuthzError> for ApiError` already exists, so `require(…)` returns `authorize()`'s error with `?`.
  > Self-service routes check `Permission::UsePersonal` on `ResourceContext::Personal{owner}`, so every authed handler calls `authorize()`, and someone else's session gives 404; step-up is checked here or in the handler, not by `authorize()` (ADR-0010, [ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).
- [x] **P7.9** 🔴 Admin endpoints:
  - `GET /users`
  - `PATCH /users/{id}` (role, disabled). Changing either revokes all of that user's sessions.
  - `POST`, `GET` and `DELETE` on `/invites`

  > Note (P7.9): roles change only through the Owner (`SetRole`, Member ↔ Admin), never on the Owner themselves or another Owner; Admins only disable and enable Members (`SetDisabled`), so Appendix B's "Admin+" for `PATCH /users/{id}` means the `disabled` field for Admins.
  > Admins create and revoke Member invites and list all invites; Admin invites are the Owner's; ownership transfer gets a permission together with a route (ADR-0010).

  > Note (P7.9): `GET /users` (Admin+) lists keyset pages (`?before=<id>&limit=1..100`, 50 by default) with `mfa_enrolled` and `last_seen_at`. `PATCH /users/{id}` takes exactly one of `role` and `disabled` (garde has no rule across fields, so `Validate` by hand); a change revokes every session of the user, and a demotion to Member or disabling also their pending invites.
  > `POST /invites {role}` shows the `afk_iv_…` code only once, with at most 20 pending invites per creator; every admin write re-checks the acting session, the actor and the target in its transaction (`app::users::recheck`). Absent values are left out, never `null`, and every operation ID is unique ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).
- [x] **P7.10** 🔴 Rate limiting:
  - tower_governor per IP, globally and on auth routes, via `into_make_service_with_connect_info`
  - `trusted_proxies` CIDRs decide when `X-Forwarded-For` is honored
  - a periodic `retain_recent()` clean-up
  - governor limits per user
  - a per-username lockout stored in the DB
  - `429` with `Retry-After`

  > Note (P7.10): from Phase 6: this task adds `[http] trusted_proxies` with its validation and switches `serve` to `into_make_service_with_connect_info`; a limit answers `ApiError::RateLimited { retry_after }`, sent in whole seconds, rounded up.
  > A 429 that bypasses `ApiError` keeps its envelope and headers but is logged at `error` as a wiring bug, and the dev-only `GET /api/openapi.json` needs a rate-limit class too ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).

  > Note (P7.10): **Deviation: governor without tower_governor.** tower_governor 0.8.0 hard-codes governor's quanta clock, forces governor's and axum's default features, and rounds its wait down, so the server's own middleware runs governor's keyed limiters on its `Clock` port (`MonotonicClock` in production).
  > Limits per IP (every request, health checks and unmatched paths included; IPv6 by its /64), per route class (`Login` shared by `/auth/login` and `/auth/login/mfa`, `Refresh`, `Register`) and per user; `X-Forwarded-For` is read only from a trusted proxy, walked from the right; one real-socket test proves the peer address reaches the limiter ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).

  > Note (P7.10): the lockout: 5 failures (`[auth] lockout_threshold`, 3–20) lock for 1 min, each further one doubles the lock up to 1 h, and a streak expires 24 h after its last failure; while locked, 429 before any password check, not counted, and one attempt per name in flight. Rows are keyed by HMAC-SHA256 of the normalized `Username` under a key derived from the vault's active key, unknown names taking exactly the same path.
  > `security.lockout` is recorded at each lock. The housekeeping task runs every 60 s: `retain_recent()`, the purge of expired streaks, and one `info` summary of the refusals ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).

  > Note (P7.10): config: `[http] trusted_proxies` (at most 32 strict CIDRs: host bits zero, no bare addresses, no duplicates, no IPv4-mapped networks, no /0 since group D, since trusting every address lets any client choose its own through `X-Forwarded-For`, past every per-IP limit and into the audit log's IPs) and every `[rate_limits]` key but chat's and `ws_connections_per_user` ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  >
  > | Key | Default | Range |
  > |---|---|---|
  > | `per_ip_per_sec` / `per_ip_burst` | 20 / 40 | 1–1000 / 1–10000 |
  > | `login_per_ip_per_min` | 5 | 1–60 |
  > | `refresh_per_ip_per_min` | 30 | 1–600 |
  > | `register_per_ip_per_hour` | 5 | 1–100 |
  > | `per_user_per_sec` / `per_user_burst` | 10 / 30 | 1–1000 / 1–10000 |
- [x] **P7.11** 🔴 **Route-coverage test.**
  - Routes are registered only through `public_route(…)` or `authed_route(…)` helpers, which also record them in a registry.
  - The test calls every authed route without credentials and expects `401`.
  - It also checks that the public list is exactly the expected allowlist.

  > Note (P7.11): routes are registered through utoipa-axum's `OpenApiRouter`, so `public_route` and `authed_route` wrap it and every route keeps its `OpenAPI` entry; `GET /health/live` and `GET /health/ready` (plain `Router::route` since P6.7) move to `public_route` and the allowlist, and the dev-only `GET /api/openapi.json` is public, the allowlist test checking both modes ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).

  > Note (P7.11): `http::routes::Routes` takes routes only through `public_route(class, routes!(…))` and `authed_route(class, routes!(…))`, recording each method and path with its access and `RateClass`.
  > `tests/routes.rs` checks the public allowlist, calls every authed route without credentials (401), keeps the authed list from being empty with a test-only authed route, and checks that the registry equals the document's operations; a handler takes `AuthUser` before any body extractor ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).
- [x] **P7.12** 🔴 Security tests:
  - **User enumeration:** an unknown user and a wrong password give the same status and body.
  - **Lockout.**
  - **Refresh reuse:** reusing a refresh token revokes the family.
  - **Expired or disabled sessions.**
  - **Escalation:** a Member tries to become Admin, an Admin tries to change the Owner.
  - **Invites:** reuse and expiry.
  - **TOTP replay.**

  > Note (P7.12): each test shipped with the defense it attacks, red first: service-level tests in B (refresh reuse, a disabled user through `AuthUser`), C (lockout counting) and D (TOTP replay); end-to-end tests over HTTP in D (enumeration, lockout through login, expired sessions and refresh reuse) and E (escalation, disabling, invite reuse and expiry).
  > Every item is named with its tests in the threat model's B1 section ("P7.12's security tests"), whose citations `scripts/threat-model-citations.mjs` checks ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).

  > Note (P7.12): the accepted residual risks, under the threat model's "Accepted risks and known limits" ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **Keeping an account locked:** an attacker can fail once per lock (one failure an hour, once the lock reached its cap), while existing sessions keep working.
  > - **Stale hashes:** after an `argon2_*` change, users who don't log in keep their old hashes, so their names stay distinguishable by timing until they do.
  > - **TOTP guessing by a password holder:** at the default threshold of 5, about 33 wrong codes a day get through (5 free, then locks of 1, 2, 4, 8, 16 and 32 minutes, then one an hour). Each guess hits one of about 3 in 10^6 codes (the window's three steps), so about 1e-4 a day and 3.5 % a year for an attacker who has the password and never stops, while the real user stays locked out nearly all the time.
  > - **An account without 2FA** (an invitee who abandons setup) belongs to whoever first enrolls with its password.
  > - **Displacing a login:** a password holder can replace the user's live login challenge with a password step of their own (the same class as the lockout DoS).
- [x] **P7.13** Audit events for:
  - login success and failure, logout
  - password and 2FA changes
  - role changes
  - invites created and used
  - token reuse

  > Note (P7.13): a failed login for an unknown username never stores the typed name raw in the audit log: people sometimes type their password into the username field. Each phase lists its actions in one `audit_actions!` block in fleet-core ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).

  > Note (P7.13): each action arrived with its write: A `user.create`; B `security.refresh_token_reuse`; C `security.lockout`; D `auth.login`, `auth.logout`, `auth.step_up`, `mfa.enroll`, `mfa.reset`; E `user.set_role`, `user.set_disabled`, `user.change_password`, `invite.create`, `invite.use`, `invite.revoke`, `session.revoke`, and `user.create` through an invite.
  > Actions someone performs are `area.verb`, things the server detects `security.<noun>`, the outcome is never part of the name, and a snapshot of every name keeps merged names from being renamed or dropped ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).

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

> Note (P8): built in groups A–E, one PR each, with P8.7 spread over D and E and ticked in E; their decisions (group A's questions, answered by the user on 2026-10-12, then each group's) are in [ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md).

- [x] **P8.1** 🔴 `fleet-client`:
  - typed async methods for every endpoint (reqwest + rustls, timeouts)
  - one automatic, single-flight refresh on `401`
  - typed errors
  - integration tests against the in-process server

  > Note (P8.1): a refresh replaces the access token at once, so a request with a replaced one gets 401; the single-flight refresh then retries once with the current token, and only a 401 `unauthorized` for the current token from an authenticated route refreshes (never a 401 `invalid_credentials` from login or login/mfa, nor step-up's 403 `forbidden`, a wrong password).
  > No grace window: a lost refresh response, or a refused refresh, means logging in again. `expires_in` lets the app restart a timed-out mfa or setup step; refreshing never runs on a timer ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).

  > Note (P8.1): fleet-client sets a maximum response body size, since fleet-api-types caps each text a client reads but not how many entries a response holds ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).

  > Note (P8.1): request DTOs derive `Serialize` too (amending ADR-0015's request rule); the refresh token lives only in a `SessionStore`, the access and step tokens only in the client's memory, and `login` with a session held is `AlreadyLoggedIn`. An end never races a save (an epoch under a store gate), and once a request may reach the server, its save runs in a task of its own (the user's review of PR #34).
  > `ServerUrl` is an origin, plain http only for a literal loopback address; no redirects or proxies; `Trust::System`, or `Trust::CustomCa` alone. 5 s to connect, 20 s per request, a 1 MiB body cap, no automatic retries, the server's `[http] request_timeout_secs` capped at 19 s, and a required `fleet-client` coverage gate at 90 % ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)).
- [x] **P8.2** Scaffold:
  - `apps/desktop` (Tauri v2; `src-tauri` is a workspace member)
  - `packages/ui` (Vite + React + strict TypeScript + Tailwind + shadcn/ui)
  - pnpm scripts for both

  > Note (P8.2): Windows only: `apps/desktop/src-tauri` (crate `afkfleet-desktop`) is a workspace member that the Linux recipes exclude, covered by clippy and tests on windows-latest and a `tauri-build` job; doctests leave it out everywhere (tauri-build's stub `msvcrt.lib`); cargo deny runs two passes over the same `deny.toml` with no ignores (the Linux GTK3 stack failed it); the agent's image copies it but never builds it.
  > Identifier `io.github.noah-gabel.afkfleet`; TypeScript 7.0.2, shadcn/ui as only `components.json` and its theme tokens, Vitest, the Tauri CLI from npm; a placeholder icon that `just icons` renders ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)).
- [x] **P8.3** Tauri hardening:
  - CSP: `default-src 'self'; img-src 'self' data:; style-src 'self'; connect-src ipc: http://ipc.localhost`. Relax it only with an ADR.
  - Capabilities allow only our commands.
  - `withGlobalTauri: false`.
  - Devtools only in debug builds.
  - The single-instance plugin.

  > Note (P8.3): the CSP exactly, with `dangerousDisableAssetCspModification: true`, which makes the served header stricter, and no `devCsp`, since `tauri dev` sends no CSP; `withGlobalTauri: false` written out; `freezePrototype: true` (the user's addition; removing it needs an ADR). One capability, `main`, granting nothing (no `core:default`), and an empty `[default]` permission that turns the ACL on for app commands.
  > tauri has only `wry` and `compression` (no `devtools`, `dynamic-acl` or `common-controls-v6`); the single-instance plugin is registered first; a navigation guard allows only the app's own origin. `just tauri-check` and `tests/acl.rs` pin it, and a debug build checked it live ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)).
- [x] **P8.4** 🔴 Rust side:
  - `AppState` holds the server profile, the `fleet-client` and the session.
  - The **refresh token lives in the OS keychain**; the access token only in memory.
  - Typed commands are grouped per feature (`commands/auth.rs` …) and return `Result<T, CommandError { code, message }>`.
  - Server profile: a URL plus an optional custom CA PEM, validated. **Never accept invalid certificates.**

  > Note (P8.4): the keychain implements fleet-client's `SessionStore`, its blocking calls off the runtime; the profile reuses `ServerUrl` and `CustomCa::from_pem`; `main` installs the aws-lc-rs provider; `CommandError` maps `ClientError` with a default branch; no token reaches `AppState` ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)).

  > Note (P8.4): 20 commands in `commands/{server,auth,mfa,me,users,invites}.rs`, thin wrappers over `AppState`, their IPC types in fleet-api-types' `app` module; `CommandError { code, message, api?, fields? }` with a closed code; logout always answers Ok (`ended`, or `unconfirmed` with the error).
  > The profile is `server-profile.json` (version 1, a CA stored as canonical PEM, read with a 128 KiB cap; an unusable file starts the app `unreadable`). The keychain entry is keyed by origin: an identical profile is a no-op, and any other change logs out, clears the new origin's entry (`Client::discard_session`) and swaps under one `RwLock`.
  > keyring-core 1.0.0 over windows-native-keyring-store 1.1.0 (`Local`), calls on one entry in order through a tokio mutex per origin, a call outliving its 5 s timeout a new task exception; each command pinned in build.rs, `main.json`, `tauri-hardening.mjs` and `tests/ipc-contract.json`; `just cov-desktop` gates 90 % with `--no-rustc-wrapper` ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)).
- [x] **P8.5** 🔴 TypeScript side:
  - The `ApiClient` interface lives in `packages/ui/src/api/`.
  - `TauriApiClient` lives in `apps/desktop/src/` and is injected into `<App apiClient={…} />`.
  - `FakeApiClient` is for tests.
  - DTO types come only from `src/generated`.

  > Note (P8.5): UI code that branches on an error code always has a default branch that shows the message and the request ID, never an exhaustive switch, since an older app can receive a code added later; the same applies to every `Open<…>` field. The generated folder holds only ts-rs's files (`just gen` refuses otherwise), each type imported from its own file ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).

  > Note (P8.5): `ApiClient` has 20 methods typed by the generated types, failing with `ApiError { error: CommandError }` (`ipc` for Tauri's own refusals); `describeError` keeps a default branch on `api.code`. `validated(client)` checks every input with zod before `invoke`, and a fleet-api-types test keeps `limits.ts` equal to the Rust constants.
  > `FakeApiClient` is scripted per method; `TauriApiClient(invoke)` is tested against `ipc-contract.json`, the Rust ACL test's fixture. Biome refuses `@tauri-apps/*` in packages/ui and all but `@tauri-apps/api/core` in apps/desktop; packages/ui exports `./generated/*` ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)).
- [x] **P8.6** 🔴 App shell:
  - TanStack Router with an auth guard
  - role-aware navigation
  - an error boundary
  - toasts
  - a banner when the server is unreachable

  > Note (P8.6, P8.7): the banner shows on `Unreachable` and `TimedOut`; server setup explains `Certificate`; a 429 or 503 shows its `retry_after`; `expires_in` restarts a timed-out login or setup step; registering holds no session, so the login right after it never meets `AlreadyLoggedIn` ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)).

  > Note (P8.6): routes in code over a memory history, `/` the launch (server setup, login, Home or a start screen). Every call is watched: `unreachable`/`timed_out` raise the banner, and `no_session` ends a signed-in session once (login first, then the cache, then one toast); the banner checks every 10 s and waits a busy server's `retry_after`; no retries, no refetch on focus or reconnect.
  > Home and Server for everyone, Users and Invites for Admins and the Owner (an unknown role gets a Member's rights); an error boundary with a fixed text, Try again and Restart. No runtime styles: sonner is patched, and every Vitest test checks the document holds no `<style>` and no stylesheet; zod is jitless (`src/api/zod.ts`), since its JIT probe caused a CSP violation in the live check.
  > Dependencies are exact and at least 3 days old (`minimumReleaseAge: 4320` in `pnpm-workspace.yaml`) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)).
- [x] **P8.7** 🔴 Screens:
  - server setup
  - login, including the 2FA step
  - register with an invite
  - 2FA enrollment (QR code from the server), shown right after the first login, since 2FA is mandatory
  - profile: change password, list and revoke sessions

  > Note (P8.7): from Phase 7: the QR code is an SVG `data:` URI, shown only through `<img src="data:…">`; after setup the app asks for the next code; replacing the authenticator goes from the profile through step-up and `POST /me/mfa/reset` into enroll and confirm. Registering trims a pasted invite code and logs in straight into the 2FA setup.
  > The profile warns when `failed_code_attempts.count` is above 0 and when 3 or fewer recovery codes are left; the `current` session's row logs out through `POST /auth/logout` ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)).

  > Note (P8.7): D's part: one sign-in flow per `/login` and `/register`, a reducer whose state alone holds the password, the TOTP secret and the recovery codes; every call carrying a secret is a direct call, never a TanStack query or mutation. The code step allows 3 tries, then goes back to the password step, as its deadline does; recovery codes are shown once, behind a required "I've saved these codes"; server setup asks for a current custom CA again to keep it; a failed discard is `session_discard`.
  > After the user's review of PR #37: a server switch while signed in runs as a deliberate logout, late answers count only for their step, a running call holds its step's other actions, and `styles.css` hides Edge's own reveal button ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)).

  > Note (P8.7): E's part: `/profile`, with `/profile/password` and `/profile/2fa` beside it, both starting with "Confirm it's you" (`stepUp`); the warnings show on the profile and on Home; "Log out all other sessions" fetches every page and runs to the end. The wrong-code count leaves out the typos of the login that succeeded (a `LoginStepId`, [ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)'s amendment); every field message shows, one per line.
  > After the user's review of PR #38, a refusal's wait counts down on every form that sends a request, its submit disabled until the end (`useRetryWait`, `RetryWaitMessage`), and screen readers hear only its start and end ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)).
- [x] **P8.8** 🔴 Admin screens:
  - users: change role, disable
  - invites: the code is shown **once**, with a copy button

  > Note (P8.8): `permissions.ts` mirrors `authorize()`, its test checking every cell of fleet-core's authorization-matrix snapshot. The admin writes became `step_up_route`s (a server change, [ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)'s amendment): on 403 `step_up_required` the screen shows "Confirm it's you", steps up and sends the write once more (`useStepUpAction`).
  > Inline confirmations, no dialog; the invite's code shown once, with "Copy invitation" and "Copy code"; "Show more" on every list ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)).
- [x] **P8.9** Playwright against the Vite web build with MSW, for login, 2FA and invite registration.

  > Note (P8.9): the e2e build (`apps/desktop/e2e`) runs the real `TauriApiClient` over `fetchInvoke`, which posts each command to `/e2e/ipc/<command>`, answered by MSW in the Playwright process (`@msw/playwright`) from a fake that answers exactly `ipc-contract.json`'s commands; no service worker, and no MSW code or token in any bundle (`scripts/e2e-bundle-guard.mjs`). The installed Edge, under the release CSP with only `connect-src 'self'`.
  > Every browser spec freezes `Object.prototype` and fails on a CSP violation, a console error or an injected style, so every answer is a 200 with an envelope. Specs: login with the code step, the first login's 2FA setup, registering with an invite and the DoD's story; `just e2e` is in `check` and `ci`, with an `e2e` CI job on windows-latest ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)).

**Security:**
- Tokens never reach JavaScript.
- No `dangerouslySetInnerHTML`.
- Inputs are validated with zod before `invoke`, and the server validates them again.

**DoD:**
- A friend can install a dev build, register with an invite and log in with 2FA, and an Admin can manage users.
- Vitest and Playwright are green.

> Note (Phase 8's DoD): "A friend can install a dev build" moved to Phase 12's DoD (the user's decision): a friend's PC can reach a server only over HTTPS, which Phase 12 brings (Caddy), and the app allows plain http only to a loopback address. Phase 8 proved the rest on the user's PC by the PR's hands-on steps (the release exe against `just dev-server` over loopback), and Playwright runs the same story on every push ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)).

---

### Phase 9: Credential vault & Microsoft accounts
**Goal:** Users link their Microsoft accounts securely. Tokens are encrypted at rest and refreshed safely.
**Introduces:** `azalea-auth`, `moka`, `tauri-plugin-opener`.

> Note (P9): built in groups A–F, one PR each; their decisions are in [ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md).

> Note (P9): neither azalea-auth nor moka is introduced (the user's decisions in groups B and C): fleet-server's Microsoft adapter sends its own requests, and `deny.toml` keeps azalea-auth out of every member; the session-token service's single-flight is its own per-account lock ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)).

> Note (P9): **decide step-up per action**, the same way as the admin writes (the user's request): before building them, decide for each destructive or privilege-changing action of this phase whether it needs a step-up: unlinking or deleting a Microsoft account (already in §7.2's list), changing or revoking a grant, rotating keys through the API. The app reuses the admin screens' "send, and on `step_up_required` ask for the password and retry once" ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)).

- [x] **P9.1** 🔴 Migrations and repositories:
  - `mc_accounts`: owner, MC UUID and name, kind, encrypted refresh token, `key_id`, status
  - `account_grants`

  > Note (P9.1): **account identity:** `McUsername` keeps its case, but Minecraft names are case-insensitive. Identify accounts by MC UUID; any lookup or comparison by name ignores case (the group A review).

  > Note (P9.1): `mc_accounts` came in group A, `account_grants` in E, which ticked P9.1. `0008_mc_accounts.sql` holds both kinds: a Microsoft account unique by its profile's UUID, pending ones included, an offline one (no UUID, no envelope, always `active`) by its lowercased name; fleet-core's `AccountStatus` and `McUsername` check the values when a row is read. Deviation: no `key_id` column, since the envelope's header names its key ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)).

  > Note (P9.1): `account_grants` as built (`0010_account_grants.sql`): one grant per account and user, so no cap (S10), deleted with its account; `level` in `view`/`control`/`manage`; the owner never has a row ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)).
- [x] **P9.2** 🔴 Vault extensions:
  - purpose-bound AAD (`account_id|ms_refresh`)
  - `vault rotate-key` CLI that re-encrypts transactionally
  - lookup by `key_id`
  - a test that a ciphertext swapped between two accounts fails to decrypt

  > Note (P9.2): "lookup by `key_id`" is P7.4's `open`, plus `key_id_of` and `vault status`, a report of what each key ID seals; `vault rotate-key` re-encrypts only the envelopes under keys other than the active one, its crypto before the one transaction (ADR-0015's rule), and stops at an envelope that doesn't open; the swap tests are `a_refresh_token_moved_to_another_account_is_rejected` and `rotation_refuses_refresh_tokens_swapped_between_two_accounts_and_records_each`. CLI only: no API route for key rotation, so no step-up to decide ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)).
- [x] **P9.3** 🔴 `MicrosoftAuthProvider` port (`start_device_flow`, `poll`, `refresh`, `minecraft_session`), with an `azalea-auth` adapter and a fake.
  - Use azalea-auth's default client ID; a custom Azure app ID would need approval from Mojang.
  - **Never** use azalea's file cache.
  - At `trace`, azalea-auth logs Microsoft access and refresh tokens. `fleet-server` caps the `azalea_auth` log level at `info`, whatever the configured filter says.

  > Note (P9.3): **the `azalea_auth` cap:** at `trace`, azalea-auth 0.16 logs the Microsoft access token, the whole token response (refresh token included), the Xbox Live, XSTS and Minecraft responses and the account cache; at `debug` nothing secret. fleet-startup caps it at `info` for every binary since Phase 6, the agent too (P5.2), and P9's redaction test checks it on the server (ADR-0011, [ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)).

  > Note (P9.3): fleet-server, fleet-client and the desktop crate stay in `stable_crates`: the notes that they would leave it rested on fleet-server getting azalea-auth, which it doesn't (group B, the user's decision; [ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md), [ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)).

  > Note (P9.3): Deviation: no azalea-auth adapter (the user's decision, group B). azalea-auth 0.16.0 checks no HTTP status, can panic on an unexpected answer and prints tokens in `Debug`, so `infra::msauth::ReqwestMicrosoftAuth` sends its own requests with its protocol (the default client ID, scope, fields and headers), behind `ports::msauth` with one error enum per method, treating Microsoft's answers as bounded input; `just test-ms-account` runs the user's `manual_` checks ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)).
- [x] **P9.4** 🔴 Device-code flow:
  - `POST /accounts/link` returns `{flow_id, user_code, verification_uri, expires_at}`.
  - A background poller runs bounded and cancellable, with per-user and total caps.
  - `GET /accounts/link/{flow_id}` returns the status.
  - On success, check the profile and game ownership, store the encrypted token, and give the creator Manage.
  - Member quota from `accounts.max_per_member`.

  > Note (P9.4): starting a flow checks `LinkAccount` (every role); `GET /accounts/link/{flow_id}` checks `UsePersonal` on `ResourceContext::Personal{owner}` with the flow's creator, so other users get 404 (group D, ADR-0010).

  > Note (P9.4): against device-code phishing (the user's decisions): a Member's or an Admin's new link is `pending` until someone above them approves it (the Owner's own links are `active` at once), and no bot can use a pending account; a UUID already linked, pending ones included, can't be linked again by anyone (`InsertAccountError::Taken`), and the refusal names nobody; `account.link` records the creator, the Minecraft name and the UUID. A new link's status has no reason, and the link flow calls `minecraft_session` itself ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)).

  > Note (P9.4): as built in group D: the creator becomes the owner, which is Manage, with no grant row; `[accounts] max_per_member` (1–100, 3) counts every account a Member owns, checked at the start (409) and in the commit (`over_quota`); one flow per user, `[rate_limits] link_flows_total` (503 `busy`) and `link_flows_per_user_per_day` (429, counted from the audit log); an unknown, replaced or another user's flow is one 404, and the user code is only in the start's 201; the commit re-reads the creator and the session (`users::recheck`) and records `account.link`, every start `account.start_link`; shutdown cancels every flow; hands-on: `just test-ms-account link_flow` ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)).
- [x] **P9.5** 🔴 `SessionTokenService`:
  - single-flight refresh per account (moka)
  - stores rotated refresh tokens atomically
  - caches the MC session token until shortly before it expires
  - on `invalid_grant`, marks the account `ReauthRequired`

  > Note (P9.5): Deviation: no moka. Each account's slot has a tokio `Mutex` whose owned guard a task on the service's `TaskTracker` holds until the rotated token is stored, so a dropped caller never loses one. Order: a refresh, its store (`account.refresh`), then `minecraft_session`. `invalid_grant` marks `reauth_required`; NotOwned, NoProfile and the Xbox refusals mark a new `refused` with its `status_reason`. A session is cached until 10 minutes before it expires (`REUSE_MARGIN`). P10.6 wires it into serve ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)).
- [x] **P9.6** 🔴 Account endpoints:
  - list (only what the user may View)
  - get
  - delete (step-up + Manage)
  - grants CRUD (never above your own level)
  - re-link

  > Note (P9.6): permissions (group D, ADR-0010): `ViewBot` for list and get, `DeleteAccount`, `RelinkAccount`, `ManageGrants` (list, revoke) and `Grant{level, grantee}` (create, change), all on `ResourceContext::Account{owner, grant}` with the actor's own grant; only Manage edits grants, nobody grants to themselves, and the owner's Manage is ownership, with no grant row.

  > Note (P9.6): as built in group E (the user's U1–U4, the review agents' S1–S14): `GET /accounts`, `GET /accounts/awaiting-approval`, `GET` and `DELETE /accounts/{id}`, `POST /accounts/{id}/approve`, `/refuse` and `/relink`, and the grants under `/accounts/{id}/grants`; deleting, reviewing and every grant write need a step-up. Admins see every account they can view under ADR-0010, only the Owner all of them; a hidden account is 404 like an unknown one (U1). Reviewing is for someone above the owner: approving makes the account active, refusing deletes it with its grants and frees the quota slot. Deleting needs Manage, for any kind, including offline accounts while `allow_offline` is off. Grants are made by exact username, with one 422 (U4). Re-link is a second kind of P9.4's flow, without quota or step-up, refused for a pending or offline account (U2), and checks the profile's UUID. No hands-on check (U3) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)).
- [x] **P9.7** Offline accounts only when `accounts.allow_offline = true`. It's off by default and meant for dev.

  > Note (P9.7): as built in group D: `POST /accounts/offline {name}` for every role (`LinkAccount`), with no step-up and no approval, counted in the Member quota and recorded as `account.link`. With `allow_offline` off it answers 404 before reading the body, existing offline accounts are kept (serve warns at startup), and no bot may run on one (P10.5, P10.6). `deploy/dev/server.toml` turns it on ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)).
- [x] **P9.8** 🔴 App screens:
  - accounts list
  - **"Link Microsoft account" dialog**:
    - shows the code with a copy button
    - opens the verification URL through a Rust command that uses `tauri-plugin-opener`
    - shows the live status
  - sharing dialog (grants)
  - re-auth prompt

  > Note (P9.8): the link dialog says, in the user's words: "Only enter a code you started yourself here. Never enter a code someone sent you, it would hand them your Minecraft account." Home carries no warning, and shows Admins and the Owner how many links wait for their approval ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)).

  > Note (P9.8): asked of the screens by groups C–E: a text per status, `RefusalReason`, `LinkFlowState` and refusal of a start; starting a flow and a re-link is a direct call, since its answer carries the code; the threat model records the step-ups and the residuals ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)).

  > Note (P9.8): as built in group F (the user's U5–U15, the review agents' S1–S7): "Accounts" for every role, with the links waiting for approval as cards for Admins and the Owner, and a screen per account with its grants; the link screen has two steps, with the code held only in the screen's state. `open_verification_link`, a synchronous command without arguments, opens only the link the newest start answered with, checked by `fleet_client::VerifiedLink`; tauri-plugin-opener 2.7.0 isn't registered; the capability grows by exactly 15 `allow-` entries (35) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)).

**Security:**
- Refresh tokens never leave the server.
- A test checks that logs are redacted.
- fleet-server's log filter caps `azalea_auth` at `info`, the same cap as the agent's (P5.2), and the redaction test checks it. At `trace`, azalea-auth logs Microsoft and Minecraft tokens, the refresh token and the account cache (P9.3).

  > Note (Phase 9's security): fleet-server has no azalea-auth code, so the cap is a guard: `tests/telemetry.rs` checks it with the filter at `trace` from the config file or the environment, and `msauth::tests::no_secret_reaches_any_log_at_any_level` checks the adapter's own logs ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)).
- AAD-binding test.
- Per-user caps on link flows.

**DoD:**
- A real Microsoft account is linked end to end.
- The DB contains only ciphertext.
- `rotate-key` works on a copy of the DB.

> Note (Phase 9's DoD): how each is shown, in group F: the user's hands-on check in group F's PR linked a real account through the app against `just dev-server`; `at_rest::no_token_reaches_the_database_file_after_a_link_a_link_again_and_a_refresh` finds no token or code in the database's files; `at_rest::rotate_key_moves_a_linked_accounts_token_to_the_new_key_on_a_copy` runs the real binary's `vault rotate-key` on a copy ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)).

---

### Phase 10: Control plane (agent ⇄ server, gRPC + mTLS)
**Goal:** Agents enroll securely, receive the desired state and report the actual state, and everything reconciles after any disconnect.
**Introduces:** `tonic`, `tonic-prost`, `prost`, `tonic-prost-build`, `protox`, `rcgen`, `tokio-stream`, `backon` (IO retries).

> Note (P10): built in groups A–H, one PR each; their decisions are in [ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md). At the planning (2026-10-15), the user chose the phase boundary (U1): P11.1's `bots` and `modes` came with P10.5, its `chat_messages` with P10.8, all of P11.2 in group E, and the DoD's app part moved to Phase 11. The user also chose protox 0.9.1 over 0.10.0 (U2) and approved the review agents' S1–S9.

- [x] **P10.1** 🔴 `fleet-proto`:
  - `proto/fleet/v1/*.proto` (Appendix C)
  - `build.rs` using protox and `tonic-prost-build`
  - core ⇄ proto conversions with round-trip proptests that reject invalid proto values

  > Note (P10.1): as built in group A: package `fleet.v1` in `crates/fleet-proto/proto/fleet/v1/`, with the `AgentControl` service and its `Connect` stream (`RenewCertificate` waits for P13.6, S1). Nothing generated is committed; `tests/protocol/schema.rs` pins the compiled wire schema under `PROTOCOL_VERSION`, which is P0's "proto codegen check" (S9). `fleet_proto::generated` carries only `#[allow(clippy::pedantic)]`. `fleet_proto::control` holds the validated messages (S2): reading refuses what fleet-core's constructors refuse, and Minecraft's text is cleaned again (S6). Secrets stay out of `Debug` (S7) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).
- [x] **P10.2** 🔴 PKI:
  - `afkfleet-server pki init` creates the CA and the server certificate. The CA key is its own secret.
  - CSR signing with a fixed lifetime (default 60 days), with the agent ID in the SAN.
  - Fingerprints are stored in `agent_certs`.

  > Note (P10.2): `agent_certs` is keyed by fingerprint, with `agent_id` indexed but not unique, so P13.6's renewal can hold two certificates; the SAN's agent ID comes from the server's record, never the CSR (S1). As built in group B: `[grpc]` is optional (S13); every key is Ed25519 from `SecureRandom` (S10); the CA, server and agent profiles (S11); `pki init` writes exactly the four files and never overwrites one, and `pki server-cert` renews the server pair (S12); a CSR that asks for any extension or carries any attribute is refused; x509-parser is a direct dependency (U5) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).
- [x] **P10.3** 🔴 Enrollment:
  1. An Admin calls `POST /agents/enrollment-tokens` and gets a single-use token, valid 15 min, stored hashed. *Since Phase 11's group J only the server's host creates tokens, and the route is gone ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), U76).*
  2. `afkfleet-agent enroll` creates a key and a CSR (rcgen).
  3. It calls `AgentEnrollment.Enroll`. This RPC uses TLS without a client certificate and is rate-limited; the agent pins the CA fingerprint from the enrollment bundle.
  4. The agent stores its cert and key, readable only by itself.

  > Note (P10.3): **only the Owner** creates enrollment tokens (`EnrollAgent`), not an Admin as step 1 says: P10.5 assigns bots to any agent with free capacity, so an Admin's own agent could otherwise get the Owner's session tokens. Viewing and disabling agents stays Admin+ (group D, ADR-0010).

  > Note (P10.3): the enrollment service joined the proto here, and `EnrollRequest`, which holds the token, follows S7; the agent's key and certificate go on its writable `/data` volume, owner-only and written atomically (S1). As built in group B: `POST /api/v1/agents/enrollment-tokens`, the Owner's step-up route without a body (S16, S17); a token (`afk_et_`) lives 15 minutes, stored only as its SHA-256 and used at most once; from the host, `afkfleet-server agent create-enrollment-token` (the user's choice, U3); the bundle is `<token>.<CA fingerprint>` (S15).
  > `AgentEnrollment.Enroll` is rate-limited per IP before the body is read and checks in S21's order (S13, S14, S21), and the certificate goes back as PEM (R10). `afkfleet-agent enroll` reads the bundle from stdin only and checks the CA against the pin before sending anything (S18, S19, S20); on Windows the files inherit their folder's permissions (S20). PR #49's review: see R9 under P10.4. Deviation: the route takes no body, so rule 3's 422 has nothing to refuse ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).
- [x] **P10.4** 🔴 `AgentControl.Connect` bidi stream. It requires:
  - a verified client certificate
  - an allowlisted fingerprint and an enabled agent
  - a version handshake: `Hello` gets either `Welcome` or a rejection
  - heartbeats with a timeout
  - message size limits

  > Note (P10.4): **the agent notices a dead server too** (the user's note at the plan review): over a half-open connection (a server crash, a NAT timeout) the stream can hang without an error, so a connection that goes silent without closing must end within a bound. Decided as HTTP/2 keepalive with a timeout plus heartbeats (S23): group C tests the server's side, G the agent's reconnect and resync, and P10.9's loopback suite runs it end to end through a stalling proxy ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).

  > Note (P10.4): an agent's identity comes only from its certificate: the agent ID in its SAN, looked up by fingerprint in `agent_certs`, never `Hello`'s name. A `Hello` whose name differs from the agent's record is refused (the user's note at group B's plan review). A hello that doesn't convert gets `Reject` (S8). A connection needs a fingerprint that's neither revoked nor expired and an enabled agent (S1). `AgentControl` sits behind its own gate on the gRPC port (S13) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).

  > Note (P10.4): as built in group C: a call without a client certificate is refused first, then `[rate_limits] connect_per_cert_per_min` is spent before any lookup (S24, S30, R18); every admission failure is one `UNAUTHENTICATED`, and a known certificate's refusal is audited (R22, R23); the hello within 10 s, `protocol_version` equal to `PROTOCOL_VERSION`, then `Welcome` (R14); `[grpc] heartbeat_interval_secs` and `heartbeat_timeout_secs` with HTTP/2 keepalive (S23); 8 MiB from an agent, 128 MiB from the server (S25).
  > Every socket is cut at 80 s unless a welcomed stream holds it, replacing tonic's age and per-call timeout, which clippy now bans (S22, R9, U7); a stream's end is recorded as `agent.disconnect` (R19); U6's test runs on paused time. Deviation: the lift is taken before the slot is activated and `Welcome` queued (S22, R13) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).
- [x] **P10.5** 🔴 Server components:
  - `AgentRegistry`
  - `Scheduler`: in v1, the first agent with free capacity, labels optional
  - `Reconciler`: compares desired with reported state and sends `Assign`, `Unassign` or `UpdateSpec`
  - stale agent detection, which marks its bots `Unknown`

  > Note (P10.5): from Phase 4: the server stores `bots.last_state` and sends a Paused or Failed state with `AssignBot`/`ReconcileFull`, so the agent never kicks a human (§6 row 3); a sticky bot gets only `Removed`, never a `Stopped` first (P4.7, the user's decision); a bot that moves to another agent waits for the old agent's `Removed` (the "or a timeout" was dropped in group D) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)).

  > Note (P10.5): no bot runs on an offline account while `[accounts] allow_offline` is off (Phase 9, group D, the user's decision; [ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)). From the planning: `bots` and `modes` arrive here (U1); every account gets its bot (the user's note at the plan review); capacity is the hello's `AgentCapacity`, with no labels (S8); an agent's reports are untrusted (S6), and a session request names no account (S3) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).

  > Note (P10.5): the `AgentRegistry` (`grpc::registry`), as built in group C: one stream per agent, a second claim gets `Reject(AlreadyConnected)` (S26, R12); `end` and a 5 s sweep end the streams whose permission ended (S27, R13); `send` never waits, and a full queue (2048) ends the stream (S25); `admit` passes only the reports of bots held Assigned (S28, R21) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).

  > Note (P10.5): as built in group D (S32–S45, R24–R30): no `Scheduler` type. Placement is fleet-core's pure `placement::plan`, first fit in enrollment order, never rebalanced (S42), over `placement::placeable` (S33). The reconciler (`grpc::reconciler`, S37) passes at startup, every 30 s and on hints, and audits each change before any message (S36). A quiet agent's bots read `Unknown`, and no time moves a bot (S32, S41, S44). Two agents never run one account (S39, S43), and after a start nothing new is placed until every enabled agent is back or 90 s pass (S45). Sticky state stays in memory, written only in D's transactions (S40). PR #52's review added stream generations, fenced strays and the reconciler's watch (exit 4) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).
- [x] **P10.6** 🔴 Session grants: `SessionRequest{bot_id}` returns `SessionGrant`, but only if the bot is assigned to *this* agent; otherwise `SessionDenied`. On the agent this backs `SessionCredentialProvider`.

  > Note (P10.6): the managed provider never hands out a token that expires within the connect timeout ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)). Serve builds `SessionTokenService` here: `SessionRequest.fresh` maps to `session(account, fresh)`, a cached session is handed out only while more than 10 minutes are left, an offline account's grant carries its name, and a re-link during a refresh answers `ChangedMeanwhile`, retryable ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)). One exception to the mapping: an owner disabled while a grant waits comes back `OwnerDisabled`, retryable (U14).

  > Note (P10.6): a grant and a denial repeat the request's `SessionRequestId` and `bot_id`, and the agent drops a late answer (S3); the token reaches the wire only inside `OnlineSession` (S7); a grant goes only after `Welcome`, on the asking stream, to a bot it holds Assigned (`LiveAgents::held`), and fresh requests are bounded per bot (S28) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).

  > Note (P10.6): as built in group F (S57–S64, R39–R46, U14; each grant logged at `info`, the user's change at the plan review): the registry filters first (S57); `[rate_limits] fresh_sessions_per_bot_per_hour` (S58); the stream's grant queue runs at most 4 at once (R40); `app::agents::SessionGrantService` checks again in a write transaction that writes nothing (S59), over `MinecraftSessions` (R46); every denial is retryable but the service's own errors (S60); no audit entry (S61); the agent's `ManagedCredentials` (S62, S63); serve wires the service (S64) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).
- [x] **P10.7** 🔴 Agent client:
  - reconnects with backon
  - keeps bots running during outages
  - sends its full state on reconnect
  - forwards events through a bounded buffer that drops the oldest chat when full and **never blocks the bots**

  > Note (P10.7): from Phase 4: the agent maps the runtime's `FleetEvent`s to `BotEvent`: `ChatSent`/`ChatFailed`'s `ChatTicket` back to `SendChat`'s request ID, `ModeChatSent` to outgoing mode chat, and `Removed` and `Alert` too. A ticket's event can come before the `send_chat` reply (the PR #15 review) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)).

  > Note (P10.7): from groups A–F: `ChatSent`/`ChatFailed` carry the `ChatRequestId` (S3); the hello's capacity is `[runtime] max_bots` (S8); the identity `enroll` leaves goes straight into tonic's `Identity::from_pem`; the channel's keepalive and limits (S23, S25) and three kinds of end: stop, retry slowly, backoff (S22, S24, S26, S28); what the reconciler counts on (S33, S42, S44, S45); the SSRF check right before the dial, with `[control_plane] private_targets` empty by default, and the dev and compose configs listing what they need (S46, security); the lifecycle message applied in stream order (S49); the managed credentials (S62, S63, R44) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).

  > Note (P10.7): as built in group G (S65–S69, S71, R47–R58, U15, U18–U20): `managed::{identity, outbound, conductor, link}`, the rest of the names in ADR-0019's "As built (group G)"; `run` in managed mode loads the identity first (S68), exits 5 on a refusal and 4 on an unasked end; the outbound `Connect` body is a `tokio_stream::Stream` (U18); S46's check is in fleet-core's `net` and fleet-mc's `AzaleaConnector::with_targets` (S69), which is enough since azalea 0.16 ignores `ClientboundTransfer` and downloads no resource pack; `just dev-managed-agent` and `just dev-bots` (S71); U20's `agent-control-plane` gate at 90 %.
  > Deviations: no backon sleeper (the reconnect waits come from fleet-core's `RetryPolicy`, and the link sleeps with tokio); deterministic sweeps instead of proptest; `LinkTimings`, a seam for `tests/control`; `certificate::validity` for R52; one planned mutation that doesn't apply. The review of PR #56 fixed an overflow that turned a refusal into a reconnect, a busy removal that stopped a bot given again, and a heartbeat after a signal ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md), "The review of PR #56").

- [x] **P10.8** 🔴 Server ingest:
  - status and chat are written in batches, in transactions roughly every 500 ms
  - **no per-tick writes**
  - events are published on the internal bus (`broadcast`)

  > Note (P10.8): from the planning: `chat_messages` arrives here (U1), with `truncated` set to the agent's flag OR the server's own cut (S6); the batch needs a `commit_unaudited` exemption; a heartbeat carries no time (S5). An agent's presence is the session's, not the ingest's (S29). A bot's `last_state` is written in the hello's, the stream end's and the reconciler's transactions (S40), and since group G also in a batch's sticky crossings (U16) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).

  > Note (P10.8): as built in group G (S70, U16, U17): the batch writes only sticky crossings (U16); the registry's `Verdict::Held` hands a held report to `ports::ingest::Ingest::take`, which never waits (S28); `app::agents::ingest`'s `IngestService` and `IngestWriter` publish statuses at once and write chat every 500 ms, at most 1000 rows a batch, through `commit_unaudited(EventBatch)` (U17); the bus is `app::events::EventBus` (1024); serve's `end_streams_then_ingest` and `spawn_watched`; the full list of names is in ADR-0019's "As built (group G)".
  > Deviations: R54's baseline is the bot's previous admitted report, which equals the row's stored part in every path (S40, S48), and the marks are `ports::ingest::StickyMarks`, implemented by `AgentRegistry`, rather than `LiveAgents::take_sticky`; the in-memory test is a loopback one; the final flush's order is pinned on paused time. Since PR #56's review, a tick with no write is `Idle`, and a batch that stored nothing rolls back (U17); since group H, one budget of 4096 rows covers the queue, a kept batch and one in flight (S74) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).

- [x] **P10.9** 🔴 Loopback tests with a test PKI. Each of these is rejected:
  - a revoked certificate
  - a certificate from the wrong CA
  - an agent that never enrolled

  And these must hold:
  - **Connection killed mid-stream:** the state reconciles.
  - **Agent restart:** no duplicate bots.
  - **Server restart:** agents resync.

  > Note (P10.9): **an expired certificate** joins the rejections (S1), and **a connection that goes silent without closing** joins the cases that must hold (the user's note at the plan review, P10.4's note). Group C did the four rejections in `tests/grpc_control/identity.rs`; groups C–G tested each side over real mTLS on loopback; H's check against a local Minecraft container lists its network in `private_targets` (S46); G's residuals are in ADR-0019 and the threat model (U19) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).

  > Note (P10.9): as built in group H (U19, U21, U22, S72–S74): `crates/fleet-integration`, a test-only crate (U19), with `harness/`, `loopback/` and `managed_check/`; the cases `killed_mid_stream`, `agent_restart` (two), `server_restart` (either order), `silent_connection::a_silent_connection_frees_the_agent_and_it_reconnects_and_resyncs` and `outage` (two), each shown red by a mutation. A wait wakes on every log line and re-checks at most every 100 ms, always within `PATIENCE` (U23). Case 5 recorded a non-fresh request after a refused login, accepted then (U24) and changed in Phase 11's group A ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).
- [x] **P10.10** Admin endpoints: `GET /agents`, and `POST /agents/{id}/disable`, which revokes the fingerprint and drops the stream.

  > Note (P10.10): an agent's status is an `Open<…>` in its DTO, and a 64-bit number a `SafeInt` ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)). Disable revokes every fingerprint the agent holds, not one (S1), and `GET /agents` shows the current certificate's `not_after` and the capacity as a plain number, with no labels (S8) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).

  > Note (P10.10): as built in group C (S31, U3, U8): `GET /api/v1/agents` (Admin+, `ViewAgents`) gives keyset pages with each agent's `status` (`online`, `offline`, `expired`, `disabled`); `POST /api/v1/agents/{id}/disable` (Admin+, `DisableAgent`, step-up) revokes every unrevoked certificate and ends the live stream before it answers; from the host, `afkfleet-server agent list` and `agent disable` (`via` = `cli`). Deviation: no 422 test for the disable route, which has no body or query ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).

**Security:**
- A client certificate is required for `Connect`.
- An agent only gets session tokens for its own bots.
- Enrollment tokens are hashed and single-use.
- The gRPC port is published only when remote agents exist.

**DoD:** In managed mode, a bot with a real Microsoft account, started through `POST /bots/{id}/start`, goes Online on a real server and goes offline through `stop`.

> Note (Phase 10's DoD): the app's part moved to Phase 11 (the user's choice, U1). The check's agent lists the local Minecraft container's network in `[control_plane] private_targets` (S46) and calls `PATCH /bots/{id}`, `start` and `stop` through fleet-client ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).

> Note (Phase 10's DoD): the check is `managed_check::manual_managed_bot_goes_online_and_offline`, which the user runs with `just test-managed-account` (S73, U21, U22). **The user's first run (2026-10-18)** failed at setup, before anything reached Microsoft, and was fixed red first; **the second run (2026-10-18) passed every line**, in 55 s. **Phase 10's DoD is met** ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md), "As built (group H)").

---

### Phase 11: Bot operations, modes & live events
**Goal:** Full day-to-day management from the app: start and stop, modes, live chat and history.
**Introduces:** `tokio-tungstenite`, `futures-util` (WebSocket client).

> Note (P11): built in groups A–M, one PR each, some split at their plan reviews (B1–B2, C1–C3, D1–D2, G1–G2, H1–H8, L1–L4), K taking PR #86's follow-ups and U83 first; their decisions are in [ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md). At the planning (2026-10-19) the user approved the review agents' S1–S6 and added U1: the DoD checks with real accounts use a folder of their own, `target/dod-run/`, through recipes of their own, and a cleanup recipe deletes it when the user is done.

> Note (P11): **decide step-up per action**, the same way as the admin writes (the user's request): before building them, decide for each destructive or privilege-changing action of this phase whether it needs a step-up: deleting a bot, changing grants, deleting a shared mode, and anything else that loses data or widens access. The app reuses the admin screens' "send, and on `step_up_required` ask for the password and retry once" ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)).

> Note (P11): Phase 11's first group, as the user chose at PR #58's review (U24, [ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): after a refused login, a bot's next session request stays fresh through each backoff until a session arrives. Done in group A with fleet-core's `BotState::Backoff { attempt, fresh }` and the proto's `Backoff.fresh`, and P10.9's case 5 tightened. Residual: a deliberate restart, a server change or an agent restart after a refusal asks once without `fresh` ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S1).

- [x] **P11.1** 🔴 Migrations and repositories:
  - `bots`: 1:1 with an account; server address, mode, desired state, assigned agent
  - `modes`: owner, visibility, definition JSON (validated by core), version
  - `chat_messages`

  > Note (P11.1): built in Phase 10 (the user's choice, U1): `bots` and `modes` in group D (`0013_modes.sql`, `0014_bots.sql`; the columns in Appendix D), `chat_messages` in group G, which ticked P11.1. Every account has its bot, minted right after the account's ID (S35) and backfilled by the migration; `last_state_reason` (S40). The built-in modes `afk` and `farm` are seeded with fixed v7 IDs (ADR-0010), `01a14202-2800-7000-8000-000000000001` and `…0002` (`ModePreset::id()`, S38), no owner, and the JSON of `ModeDefinition::afk()` and `farm()` ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).

  > Note (P11.1): **no `auto_start`, ever** (the user's choice, U12; S33, S34): `desired_state` stays the only "should run" flag. `0014_bots.sql`'s comment ("Auto-start arrives with its first writer") stays as merged, since `migrations-check` forbids editing a merged migration ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).

  > Note (P11.1): `chat_messages` records what a mode said (`ModeChatSent{message}`, P4.5) with direction out. Conflict texts are stored per server and sent in each bot's spec as its resolved `ConflictTexts` ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)); built in group H4 (S66, S74–S78), in the table `server_settings`, keyed by `ServerKey` ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

  > Note (P11.1): **`chat_messages` as built** (`0016_chat_messages.sql`, `ports::chat`; S70): `id INTEGER PRIMARY KEY AUTOINCREMENT` (a bot's chat is ordered and paged by ID, which is receipt order; *since P11.3 commit order, which a user's line can lead by one ingest batch: Appendix D's note, U5*); `bot_id` (`ON DELETE CASCADE`), `direction`, `kind` (no CHECK), `sender_name`, `sender_uuid`, `text` (cleaned, S6), `truncated`, and `at`, the server's time of receipt (S5); indexed by `(bot_id, id)` and by `at`. `ChatWrites::append` stores a row only while the bot's row names the agent that reported it ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).
- [x] **P11.2** 🔴 Bot endpoints:
  - `GET /bots`, `GET /bots/{id}`
  - `PATCH /bots/{id}` (server, mode)
  - `POST /bots/{id}/start|stop|restart|reset|resume`

  Every call is authorized, audited and reconciled.

  > Note (P11.2): built in Phase 10's group E (U1), with fleet-client's bot methods; restart, reset and resume got a new `fleet.v1` oneof case (S3); the Tauri commands and the screens stayed in Phase 11 ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).

  > Note (P11.2): from earlier phases: a response's state, mode and disconnect reason are `Open<…>` ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)); a bot's DTO follows its account's field rule, and deleting an account deletes its bot ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)). From Phase 2 (ADR-0010): switching a mode takes `ViewMode` on the mode and `SetBotMode(allowlist.check_mode(&mode))` on the bot; the server address needs Manage (`ConfigureBot`) and the lifecycle calls `ControlBot`; `start`, `stop` and `restart` change nothing for a Paused or Failed bot; `ServerAddress` allows loopback and private addresses, so an SSRF policy must check the resolved IPs (the group A review).

  > Note (P11.2): from Phase 10's group D: reads go through `app::bots::BotService` after `authorize()`, and a bot's state reads `Reported`, `AwaitingReport`, `Unknown` or `Unplaced` with why (S41); every write that changes where a bot may run hints the reconciler (`Change::Store`); Resume and Reset of a sticky bot that isn't placed clear its stored `last_state`; `auto_start` never arrived (S34, U12); `link_flows/commit.rs`'s two clock reads became one first (U11) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).

  > Note (P11.2): as built in group E (U12, U13, S46–S56, R31–R38): `PATCH /bots/{id} {server_address}` (Manage, step-up), `PUT /bots/{id}/mode {mode_id}` and the five lifecycle calls (Control), each answering `BotResponse` with `Open<…>` enums (S54); start and stop store `desired_state` and answer 200 (S48, S50, S51); restart, and a resume or reset on an agent, are one-shot commands (202); anything else is a 409 with one of the four codes (U13); writes re-read, authorize again and commit with their entry (S53). SSRF: the server doesn't check where an address points; the agent does (S46).
  > Deviation (S47): the mode left `PATCH` for `PUT /bots/{id}/mode`, since only the server address needs a step-up; `PATCH` takes only a required `server_address`, and nothing clears it (R33, S55). No hands-on before G (ADR-0016, ADR-0017); PR #54's review tightened commands, unreachable agents and a write's answer ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).
- [x] **P11.3** 🔴 Chat:
  - `POST /bots/{id}/chat`:
    - takes a `ChatMessage`
    - `/commands` need Manage unless they're on the allowlist
    - rate-limited per bot and per user
    - audited
  - `GET /bots/{id}/chat?before=&limit=` with cursor pagination.

  > Note (P11.3): the cursors are `INTEGER` row IDs and cross the API as `SafeInt` ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)); the handler checks `SendChat(allowlist.check(&message))` and sends that same message, an allowlisted command passing with any arguments, and reading the history needs `ViewBot` (group D, ADR-0010); the bot routes' answer order (R38), and `[chat] command_allowlist` exists ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).

  > Note (P11.3): **a `SendChat` dies with its stream:** its `ChatSent` or `ChatFailed` may come on a later stream or never, so P11.3's wait for an answer is bounded, and one that never comes reads as unknown; `chat_messages` holds what bots received and what their modes sent, and a user's message is P11.3's to store (Phase 10, group G; [ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).

  > Note (P11.3): the agent's side, as built in group A: wherever chat must be signed, a command that can reach a `message` argument is refused with `ChatUnavailable` (S2). This settles the secure-chat limit Phase 3's real-account check found (ADR-0011), and it applies to mode chat too ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)). Phase 4 asked to "decide where that setting lives (server settings, sent to agents)": one chat rate per bot, `[rate_limits] chat_per_bot_interval_secs` and `chat_per_bot_burst` (burst 1–8, U2), is sent in each bot's spec (S3) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

  > Note (P11.3): sending, in group B1 (U3, S7–S9): `POST /bots/{id}/chat` (Control) and `POST /bots/{id}/command` (step-up, commands only, S8) answer 200 `sent` or 202 `pending` after waiting up to 6 s (S5). A user's line is stored with its sender (`chat_messages.sent_by`, U3), never changed after the send (S7), and audited without its text. The forbidden-character text names formatting codes (U4). Since PR #60's review, the wait counts from the request's arrival (U6), and the service checks the route itself (S10) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

  > Note (P11.3): reading, as built in B2, which ticked P11.3 (S6, S9, S11, U3, U5, U7): `GET /bots/{id}/chat?before=&limit=` answers `ChatPage`, newest first by ID, which is commit order (U5). Below Manage, a command shows as `/name` with `arguments_hidden` (S9). The sender's username shows only to viewers who see the account's owner, with `mine` on the viewer's own lines (U3). The text is a `BoundedText<3072>`, with a received line break shown as ` | ` (S11), and the kick message gets the same cap (U7) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).
- [x] **P11.4** 🔴 Modes CRUD: `GET`, `POST`, `PUT`, `DELETE` on `/modes`. Core validation errors become field errors. Built-in modes are read-only.

  > Note (P11.4): the rules it started from (ADR-0010): editing a mode re-runs the command check for every bot that uses it, and a command outside the allowlist needs Manage on each; `ModeDraft::validate` stops at the first error, and each `ModeError` is a field error at `steps[step]`; built-in modes are read-only; private ones are seen and edited by their owner, the Owner, and Admins for Members' modes; shared ones are seen by everyone and created by Admin+; a visibility change needs the rights before and after; serde_json's errors can quote chat text.
  > A bot's mode is switched by `PUT /bots/{id}/mode` (S47, S55; [ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)); "on respawn" steps were requested after Phase 4 ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)); a command off the allowlist in a mode needs a step-up (S4), and arguments are hidden below Manage (S9).

  > Note (P11.4): C's planning, in three PRs: C1 reading and creating, C2 editing and deleting, C3 "on respawn" steps (U8, S12–S20, R1–R5). Serde refuses only a shape (a bare 400), and every value rule is one 422 at the field's own path (S12); `GET /modes` gives keyset pages, and there's no `GET /modes/{id}` (R1); `fleet_core::mode::ModeName` is 1–32 characters, unique without ASCII case (S13, U8); what a viewer sees and may do comes from `authorize()`'s flags, and a command's arguments show only to those who could edit the mode (S14, S15).
  > `/with-commands` step-up twins and a step-up `DELETE` (S16); an edit checks every bot that uses the mode (S17); `PUT` carries the `version` (S18); 50 modes an owner (S19); an opt-in `on_respawn` schedule after a confirmed respawn (S20) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

  > Note (P11.4): as built in C1–C3 (S21–S27, R6, R7, U9, U10): an edit that changes nothing writes nothing, and a guarded `UPDATE` or delete that matches no row is the internal error (S21, R7); the `on_respawn` tag runs through fleet-core, the API, the stored JSON and the proto, and fleet-mc's `SessionEvent::Respawned` is confirmed from the Respawn's removal of `Dead` and the next load (S22, S27). Deviations: whose a mode is crosses as one open `ownership` instead of S14's two flags (R6); a new mode's ID and `created_at` are drawn before the write transaction (ADR-0015); the azalea bump check went into ADR-0003 ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

- [x] **P11.5** 🔴 Live events:
  1. `POST /events/ticket` returns a single-use ticket valid for 30 s (moka).
  2. The client opens a WebSocket at `GET /events?ticket=…`.
  3. Events are **filtered per user at send time**.
  4. Limits: per-user connection cap, `Resync` on `Lagged`, ping/pong with an idle timeout, maximum frame size.

  > Note (P11.5): the ticket comes from `getrandom`, never from a v7 ID, which reveals its creation time (the group A review). A WebSocket closes itself with a close frame on shutdown ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)). The bus is `app::events::EventBus`, which carries what agents reported, and P11.5 publishes the server's own changes ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)). Every live `Chat` and mode event is built per subscriber through the history's `chat_line` and `GET /modes`' `mode_response` (S6, S7, S9, S11, S14, S15, U3).

  > Note (P11.5): D's planning, in two PRs: D1 the socket and D2 the events (U11–U14, S28–S37, R8–R21). Deviation from item 1: tickets live in memory on the server's `Clock` (`afk_wt_`, 32 bytes, 30 s, single use), not in moka (S29). `GET /events` takes the session's bearer and the ticket, and every bad ticket is one 404 (S28, R10). A socket re-checks its session after every commit and ends with its access token (S31, R8). The limits: `[rate_limits] ws_connections_per_user`, ping 15 s, idle 45 s, client frames of at most 125 bytes, and `resync` at most every 5 s (S35, U12, U13). One flat `EventMessage` (S36); `just dev-bots`' `events` (S37) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

  > Note (P11.5): as built in D1 and D2, which ticked P11.5 (S38–S43, R22–R49, U15–U17): `http::ws::run` closes with 1001 at shutdown, 4001 when the session ends, 4000 without a pong and 4002 for a stuck send. `Client::events_handshake` lived behind fleet-client's `dev-console` feature until P11.8 (U15). The frames `bot`, `bot_removed`, `bot_died`, `bot_alert`, `chat`, `chat_sent`, `chat_failed`, `mode` and `mode_removed` are each built for their viewer from one rights snapshot, read again once the commit count moved (S41, S43). The server's hints follow every commit (S42). A flood on a hidden bot can show as a `resync`, an accepted risk (S40). `app/events.rs` joins the auth gate (U16), and D2 is one PR (U17) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

- [x] **P11.6** 🔴 Retention job: chat older than `chat.retention_days` is deleted. The audit log is kept longer (configurable).

  > Note (P11.6): what earlier phases asked of it: delete a session, revoked or not, once `created_at` + `refresh_token_absolute_days` has passed ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)); decide `secure_delete` ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)); bound chat by size too, not only by age, with the limit the user's ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)); delete in short batches, since a long delete holds up the ingest's ticks (S74); delete users' lines like any other chat (S6, S7).

  > Note (P11.6, from P13.4) ([ADR-0032](docs/adr/0032-the-load-test.md)): the chat-by-size phase no longer commits a batch a bot. It reads every bot's cut first, then deletes through the cuts in batches of at most 1000 rows that span bots, each still followed by the pause: the load test showed a run over 200 bots taking 21.7 s of pauses, 1.3 s since.

  > Note (P11.6): as built in group E (U18–U22, S44–S46, R47–R56): `cli::retention` runs `RetentionService::run_once` every 60 s, in batches of 1000 with a 100 ms pause (S45). In order: sessions with their rotated tokens, chat by age (`[chat] retention_days` 1–30, 14 by default, U21), chat by size (`[chat] max_lines_per_bot` 100–200,000, 50,000, U19), the audit log (`[audit] retention_days` 90–730, 365, U22), then unused invites and enrollment tokens past the audit retention (S46). `secure_delete = ON` on every connection (S44); one `retention.purge` entry with counts and cut-offs (U20); since PR #70's review, the batches commit through `Store::retention_batch` (R57–R63). P11.7 moved to group G (U18) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

- [x] **P11.7** 🔴 Full-flow integration test: in-process server + in-process agent with `FakeConnector`.
  1. Start a bot via `fleet-client`.
  2. A WebSocket client sees `Online`.
  3. Chat flows in and out.
  4. Stop the bot.

  > Note (P11.7): it lives in `crates/fleet-integration` beside `loopback/`, over the same harness (U19, [ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)). It was built in group G1, not E, over fleet-client's own reconnecting socket (U18; U15 unchanged), as `full_flow::a_started_bot_is_seen_online_chatting_both_ways_and_stopped_as_get_shows_it`, each step ending when the newest `bot` frame equals a `GET /bots/{id}` made for that frame (S58, R78) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).
- [x] **P11.8** 🔴 Tauri: the WebSocket lives in Rust (`fleet-client` + reconnect) and is forwarded to the UI over a Tauri `Channel`. The UI patches the TanStack Query cache from the events.

  > Note (P11.8): G's planning, in two PRs: G1 the socket in fleet-client, G2 the app's side, which ticked P11.8 (U25–U30, S53–S60, R73–R80). The socket is an HTTP/1.1 upgrade on fleet-client's own reqwest client (U25). `Client::live_events()` reconnects with a new ticket each time and 1–30 s of jittered backoff (S53, S54). fleet-startup caps `tungstenite` and `tokio_tungstenite` at `info` (S56, U27). No `dev-console` (R80, S57). One long-running `watch_events` command over a `Channel`; the UI patches the caches and shows a hint while a reconnect waits (U29, S59, S60, U30) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

  > Note (P11.8): as built in G1 and G2 (U25–U28, U31, S52–S65, R70–R86): `LiveEvents` documents its own rules; an open, signed-in app keeps its session until the 30-day end (U31); a stopped watch never restarts by itself (S61); `packages/ui/src/app/live.ts` patches the account caches and the bot, chat and mode keys (S62–S65). Deviation: `http1_only()` stays a guard with no test that could go red, since R75's premise doesn't hold today ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).
- [x] **P11.9** 🔴 UI:
  - **Dashboard:** a card per bot with a state badge, uptime, server, mode and last disconnect reason.
  - **Bot detail:**
    - controls and a mode selector
    - **live chat console** that renders plain text only
    - a send box that shows rate-limit feedback
    - an event timeline
  - **Mode editor:** an action list with validation.
  - **Admin:** an audit-log viewer and an agents view.

  > Note (P11.9): the chat console renders each message as its own block, with the sender outside the text, so a `\n[Server] …` inside a message can't pass as a separate line, and clips stacked combining marks (Zalgo text) with CSS overflow (the group A review). A paused or failed bot shows Resume or Reset instead of Start and Stop (ADR-0010). At a Member's promotion (P8.8), the Owner sees and can revoke the grants on their accounts, and at a demotion the dormant grants they hold ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)); the server's side is F's S50.

  > Note (P11.9): the agents view was to show the enrollment bundle once, a direct call, for the Owner to pipe into `afkfleet-agent enroll` (Phase 10, group B, U3; [ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)). *Superseded at group J's planning (the user's choice, U76): no token in the app, and the route is removed* ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

  > Note (P11.9): the agents view shows each agent's bots count (`AgentResponse.bots`), and releasing a disabled agent's bots becomes an API action (Admin or Owner, step-up, `agent.release` with `via` = `api`), with a text saying the operator vouches that the agent's host is stopped (Phase 10, group D; [ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)). *Corrected at group F's planning (the user's choice, U24): `via` = `http`; the route is S49's* ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

  > Note (P11.9): from Phase 10's group E: the four 409 codes' messages are fixed on the server (U13); Stop on a Paused or Failed bot frees its slot and keeps its state (S51); a 202 means sent, not done; every `Open<…>` has a default branch; `PATCH /bots/{id}` goes through `useStepUpAction`. From group G: `blocked` is a connect failure the UI explains, with a hint for the agent's operator (S46) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)).

  > Note (P11.9): from Phase 11's groups B and C: the send box sends to `/chat`, and on 403 `command_needs_step_up` it resends to `/command` through `useStepUpAction` (S8); it waits at most 6 s (S5) and shows the server's 422 texts (U4). The console shows a user's line as what the user asked (S7), a hidden command's mark outside its text (S9), the sender's name where the API gives one and `mine` (U3); it keeps the API's commit order (U5) and renders a line break as ` | `, as plain text (S11).
  > The mode editor binds to S12's types, takes its rights from the mode's flags (S14, S15), resends to the `/with-commands` twin (S16), sends `version` and handles the 409s and the cap (S17–S19), builds requests from the form (R5) and shows hotbar slots as 1 to 9 ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

  > Note (P11.9): parsed senders and the chat format: a sender parsed from the text is shown differently from one a player packet named, and neither is verified (Phase 3, ADR-0011); the chat format is parsed on the server, outside the bot's spec (Phase 4, [ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)). Both went to H4 (S66) and then H7 and H8: `ChatLine.parsed_sender`, parsed when a line is read (S79, S80), shown after a small icon, never in the primary color (U55) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

  > Note (P11.9): leaving a shared account: a View or Control grantee couldn't remove themselves (Phase 9, group F, the user's request; [ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)). Built in F as `DELETE /accounts/{id}/grants/me` (U23, S51; [ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

  > Note (P11.9): the backend, built in group F (U23, U24, S47–S51, R57–R69): `GET /audit` (S47, S48, R64–R66; a filter by target since J), `POST /agents/{id}/release` (step-up, `via` = `http`; S49, U24), the Owner's grants review through `GET /users/{id}/grants` and `GET /users/{id}/shared-accounts` (S50), leaving through `DELETE /accounts/{id}/grants/me` (U23, S51), and fleet-client's methods with `just dev-bots`' verbs (R67). Since PR #71's review, an audit row that doesn't read back is listed by its ID alone as `unreadable` (S52, U28, R70–R72, R79) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

  > Note (P11.9): the bot screens, planned as H1–H3 and a new H4 (U32–U52, S66–S72, R87–R97): Home as the dashboard (U35–U37), the controls (U38, U48), Settings (U40, U41), notices (U42), the timeline (U46), the console (U43, U44, U49), the send box (U45) and `[control_plane] public_targets` (U50). H1 built a read-only detail (R97, R98). H2 built the controls and Settings on S68's `unlisted_command` and S73's effect (R99–R111), with an `#[expect(clippy::struct_excessive_bools)]` on `ModeResponse` the user approved (U53). H3 built the chat, with HH:MM times (R112–R126). H5 built only the DoD's kit ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

  > Note (P11.9): per-server settings, planned as H4, H6 and H7 (U54–U60, S74–S80, R131–R136), with H7 split into H7 and H8 (S81, R137–R149). H4: conflict texts through `GET` and `POST /api/v1/server-settings` and `PUT` and `DELETE /api/v1/server-settings/{id}`, writes Admin+ with a step-up (U54), the reconciler sending each bot its server's texts. H6: the Minecraft servers screens, which open a new entry after Add and stay after Save (U56, U57, U61). H7: chat formats in the same routes (R135) and `POST /api/v1/server-settings/chat-format-preview` (U58). H8: `ChatLine.parsed_sender`, the same for every viewer, never `sender`, `sent_by` or `mine` (S79, R146, S81, R143, U55, R148, R149) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

  > Note (P11.9): the mode editor, built in group I (U62–U75, S82–S84, R157–R171): "Modes" in the nav and a screen per mode in three sections; drag and drop (`@dnd-kit`) with the keyboard and Move up and Move down; edit and delete only from the server's flags; the step-up twin through `useStepUpAction`; the three 409s with "Save as copy"; and "Make a copy". Before it, the TanStack client's default `networkMode` became `"always"` ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

  > Note (P11.9): the admin screens, built in group J, which ticked P11.9 (U76–U82, S85–S89, R172–R181): "Audit log" with a filter by target (`target_kind`, `target_id`, migration 0021; U78, U81, S89, correcting S48), "Agents", the Owner's grants review at `/admin/users/$id/grants` (U79), and "Leave" at View or Control (S86). Enrollment tokens are CLI only: `POST /agents/enrollment-tokens` and `EnrollAgent` are removed (U76). Seven commands, 66 grants (U77); one PR (U82) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

- [x] **P11.10** Dark mode: the app follows Windows' light or dark setting (`prefers-color-scheme` and its change event; the `.dark` class on `<html>`, set by the bundled script so the CSP allows it), and every screen is checked in both themes.

  > Note (P11.10): Phase 8 shipped the light theme only, with shadcn's dark tokens already under `.dark` ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)). Beyond the task, group K added the user's Light / Dark / Windows setting on Profile and `set_window_theme`, so the title bar follows it (67 grants); `scripts/theme-guard.mjs` refuses a hard-coded colour, and a Playwright sweep holds 36 rows at 3:1 in both themes (U84, U85, S92–S97; [ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).
- [x] **P11.11** UI design pass, before Phase 12 and once every screen exists:
  - **Direction first.** A short design brief, then 2–3 visual directions, each shown as screenshots of two or three real screens in light and dark. The user picks one before any implementation starts.
  - **Branding:**
    - the name or wordmark, the logo and the app icon (replacing P8.2's placeholder) at every Windows size, for the taskbar and P12.5's installer
    - a color palette mapped onto `styles.css`'s theme tokens for both themes; no color is hard-coded in a component
    - the typography, and the tone of the UI's texts
  - **Constraints:**
    - fonts and images are bundled: the CSP allows only 'self', so no Google Fonts or CDN
    - no runtime `<style>`: the style guard holds
    - WCAG AA contrast and visible focus rings, in both themes
  - **Every screen and state:** consistent layout, spacing and typography; the loading, empty and error states; the toasts, the banner and the error panel; the window's title and minimum size.
  - **Verification:**
    - before-and-after screenshots of every screen in both themes (P11.10) in the PR
    - Playwright screenshot tests in P8.9's suite, so a later change can't drift from the design

  > Note (P11.11): Phase 8's screens were functional first: shadcn's radix-nova with the light theme. How to keep screenshots stable was left to this task ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)). After H3, the user found the app looks poor throughout, and L's brief starts from that ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

  > Note (P11.11): L1 chose the direction (U86–U97, S98, R186–R191; [the brief](docs/design-brief.md)): Bridge with sidebar (U91); cool graphite and blue (U92); Geist and IBM Plex Mono (U93); the wordmark `afkfleet` (U94); the Orbit logo (U95); terse texts (U96); baselines from a pinned container (U97). Beyond the task: a Start or Stop button on each bot card, changing U35 for that one control (U88), and six code boxes (U86) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

  > Note (P11.11): L2 built the foundation (U98–U104, S99–S122, R192–R203): the palette, the fonts, the sidebar with no header, the shared parts, Orbit at 14 Windows sizes, the sign-in screens and Profile. The sweep enforces WCAG AA (text at 4.5:1, focus outlines at 3:1). The check exe is `target/app-check/afkfleet-check.exe`, the only file the AI launches (S99). The fonts' licence is in `THIRD-PARTY-NOTICES.md` (S110) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

  > Note (P11.11): L3 built Home, a bot's detail and Accounts (U105–U113, S123–S127, R204–R219). Beyond the task: a universal Back control (U111, U112), and suggestions behind "Share with" through `GET /accounts/{id}/grant-candidates`, a new limit and a 68th grant. This replaces ADR-0018's S6 for the picking, so any Member with an account can list enabled users' names, as `docs/threat-model.md` says (U110, S127) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

  > Note (P11.11): L4 finished the pass and ticked P11.11 (U118–U126, S128–S132, R226–R252): Modes, Minecraft servers and the admin screens are restyled. **The stable screenshots:** Playwright's Linux image, pinned by digest and run with no network (S130), compares every theme-sweep row in both themes to the pixel: 240 PNGs in `apps/desktop/e2e/baselines/` (`just shots`, `just shots-update`), in a workflow that isn't a required check (U118, U125). Deviation: the before-and-after shots are in `target/design-l4/` on the user's PC, not in the PR, which lists the baselines instead (R231, R242). PR #92's review hardened the container's run and added the certificate warning (U126) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)).

**DoD:**
- Pressing Start in the app brings a bot with a real Microsoft account Online on a real server (moved from Phase 10, the user's choice, U1 in ADR-0019).
- The user's real fleet runs for 24 h, managed from the app. Every event shows up live, and the logs contain no unhandled errors.

> Note (Phase 11's DoD): the first check runs on group H5's branch (`p11/p11-dod-kit`), with its own folder `target/dod-run/`, the `dod-*` recipes (the user's alone) and an agent that dials no public address (`public_targets = false`, U50). After the user presses Start, the Minecraft server's own `rcon-cli list` confirms the bot. The steps, in two parts (U51), are `docs/development.md`'s "The DoD's first check" (U1, U33, U52, S72, R122–R130) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), "As built (group H5)").

> Note (Phase 11's DoD): **the user's first check (2026-10-28) passed every step as expected:** with the user's own Microsoft account linked and its bot set to `127.0.0.1:25566`, Start pressed in the app brought the bot Online, the Minecraft server's own `rcon-cli list` showed the player, and the bot then stopped. **The first DoD item is met** ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), "The DoD's first check").

> Note (Phase 11's DoD): the 24-hour run (U127–U133, S133–S135, R256–R259): one bot on the local container, `public_targets = false` kept (U127, U128); a container restart at least 24 h after Start proves a renewed session (U129); JSON logs go through `scripts/dod-log.mjs` (S134, U132); `just dod-log-check` prints only counts and fixed messages (S135, U133); S133's step for a Minecraft-version bump went into ADR-0003. **The user's 24-hour run passed** (reported on 2026-11-05): `just dod-log-check` printed PASS, and the user reported each point as PASS. **The second DoD item is met, and with the first, Phase 11 is done.** U51's part two, which ends the DoD's folder, is the user's ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), "As built (group M)", "The DoD's 24-hour run").

---

### Phase 12: Deployment & operations
**Goal:** A reproducible, hardened production deployment and a desktop app that can be distributed.
**Introduces:** `tauri-plugin-updater`. Tools: cargo-chef, Docker Compose, Caddy (optional, [ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), U5).

> Note (P12, Phase 12's planning, 2026-11-05) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md)):
> - **Group branches** (four at the planning; at D's plan review D was split into D1 and D2, and E was added, [ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), R27, U35). Phase 12 is built in group PRs like Phases 2–11, each after the previous PR is merged: features first, then what packages them, then the release. Each group aims at about 5,300 lines; one that grows past it is split at its plan review. *From [ADR-0023](docs/adr/0023-local-autonomous-pipeline.md) (2026-11-08), a group aimed at about 1,500 changed lines and went through the local review pipeline; [ADR-0025](docs/adr/0025-a-simpler-workflow.md) (2026-11-09) replaced it: one PR per group, reviewed and merged by the user.*
>
>   | Group | Branch | Tasks |
>   |---|---|---|
>   | A | `p12/p12.3-start-screen-and-backups` | The start screen's way out (the user's finding, below), then P12.3 (ticked here) |
>   | B | `p12/p12.4-metrics` | P12.4 (ticked here) |
>   | C | `p12/p12-stack` | P12.1, P12.2 and P12.7: the images, the compose project with both proxy paths, the nightly smoke test, the setup script (ticked here) |
>   | D1 | `p12/p12.5-release` | PR #97's follow-ups, then P12.5: the installer, the updater, the maintainer's signing step and the rehearsal, the release workflow; the friends' and the maintainer's guides (ticked here) |
>   | D2 | `p12/p12.6-runbook` | P12.6 and the DoD's kit: the host's release bundle and `setup.sh fetch`, the operations on the volumes, dynamic DNS, the runbook finished, the Minecraft version bump in `docs/releasing.md` (ticked here) |
>   | E | `p12/tidy-up` | P12.8: the repository tidied for readability, with no behaviour change (ticked there) |
>
> - **The user's frame for the phase** (U1, U2):
>   - No paid services are required.
>   - Images are built for linux/arm64 in CI on GitHub's arm runners, and for amd64 too if it's cheap.
>   - HTTPS comes from Let's Encrypt over HTTP-01 on ports 80 and 443, with no Caddy plugin, for a domain name that points at the host; a free dynamic-DNS subdomain works. The token that updates its record stays on the host, never in the repository, in CI or on a command line. Let's Encrypt's IP-address certificates are the recorded fallback, not built.
>   - The installer is unsigned. The updater's private key stays offline, and releases live on GitHub Releases.
>   - Metrics stay on an internal port.
>   - The operator's hosting is never named anywhere (CLAUDE.md, rule 11). The AI never touches the host or its keys.
>   - CI secrets are the user's question first, and no production configuration dials public Minecraft servers until the user decides it (ADR-0020's U50, asked at C's planning).
> - **Beside other services** (U4, U5): the runbook is an operator's guide for any Linux host with Docker. afkfleet installs beside the host's other services in its own compose project, network and named volumes, with no host networking. It changes nothing on the host beyond what the runbook lists, and an uninstall section removes what it added. Caddy is optional, for hosts where 80 and 443 are free; otherwise afkfleet runs behind an existing reverse proxy, whose duties the runbook lists. The smoke test covers both.
> - **A short first install** (U8): at most about ten commands from a host with Docker, one setup script for the one-time steps (it checks the prerequisites, creates the vault key and the PKI with the right owners and modes, refuses to overwrite anything and prints what's next), updates in two commands, and every step saying what success looks like.
> - **Backups leave the host** (U3): they are pulled to an operator's PC over SFTP.
> - **Who decides what:** the review agents decide each group's open questions (CLAUDE.md's step 10). The user decides every dependency, CI permissions and secrets, U50 for production, the texts friends read (one table per group plan), looks and UX, and the first release tag. The DoD's steps on a real host are the user's alone.

> Note (P12, from Phase 11, group A) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S3, PR #59's review): **the server is deployed before its agents.** Phase 11 adds `BotSpec.chat_rate`, which an agent of that version requires: it refuses a spec without one, so an older server's assignments would be refused. Until the first release freezes the protocol (ADR-0019, S9), an upgrade updates the server first, then the agents; P12.6's runbook says so, and a later protocol change says which side goes first.

> Note (P12, from Phase 11's 24-hour run, the user's finding) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), "The DoD's 24-hour run"): **Phase 12's first group gives the start screen's wait a way out.** While the saved server doesn't answer, the start screen shows "Waiting for the server…" (with the banner's "Server unreachable. Retrying every 10 s.") and nothing else: a user can neither change the server nor log out, so a server that moved or is down for good leaves the app stuck (at the run's setup, the user had to start `dev-server` just to log out of it). The fix: while it waits, the screen also offers "Server settings" and "Log out on this PC", both working without the server; a local logout removes the stored session and says the server wasn't told. README's line on the start ("a session it can't use offers a way out") is corrected to match. Its texts are the user's to approve.
>
> *Planned in group A ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), S1, U7):* "Log out on this PC" reuses `logout`, which clears the stored session before it tries the server, asks nothing first, and disables "Server settings" while it runs; the texts were approved as drafted.

- [x] **P12.1** Dockerfiles for the server and the agent:
  - multi-stage with cargo-chef
  - pinned nightly builder
  - distroless non-root runtime
  - `HEALTHCHECK` through the `healthcheck` subcommand

  > Note (P12.1, from Phase 6, group E) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): the server's `HEALTHCHECK` runs `afkfleet-server healthcheck --config <the same file>`. It loads the whole config, so the container's `AFKFLEET_SERVER__…` variables apply to it too; it asks loopback when `bind` is `0.0.0.0`, and its own 3 s timeout fits a 5 s `--timeout`.

  > Note (P12.1, from Phase 5, group C, the user's decisions) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)):
  > - **Production is linux/arm64.** P12.1 builds the image for arm64, natively on the server or on GitHub's `ubuntu-24.04-arm` runner. The base images stay pinned by multi-arch index digest, and the Dockerfile names no architecture.
  > - **An image scan.** There's no Dependabot, and `cargo deny` covers only crates, so the weekly scheduled audit also scans the built agent image (e.g. Trivy). OS-level CVEs in the pinned base images then surface. Digest bumps stay deliberate PRs.
  > - **Labels.** The agent image has static OCI labels (source, licenses, title, description). `created`, `revision` and `version` change with every build, so they come through build args once CI publishes the image.

  > Note (P12.1, from Phase 10, group B) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the agent image runs `enroll` too.** `afkfleet-agent enroll --config …` reads the bundle from stdin (`docker run -i`), writes its key and certificate into the folder of `[control_plane] key_file` and `cert_file`, which must exist (the `/data` volume, S1), and never replaces them. The server image's `pki init` likewise needs its four folders to exist.

  > Note (P12.1, Phase 12's group C) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), S13, S14, U12, U16): **as built.**
  > - **`deploy/docker/server.Dockerfile`** follows the agent's: the same pinned bases and its chef and planner stages word for word, so `just images` builds both and the second reuses those layers. The image makes the folders compose mounts at, `/data`, `/backups`, `/keys`, `/ca` and `/tls`, owner-only and the nonroot user's, so a new named volume takes that owner; it has no `VOLUME`, runs as `USER 65532:65532`, exposes 8080 and 7443 (never a metrics port) and checks its health through `healthcheck` on the config path of its `CMD`. distroless carries the CA certificates reqwest needs. The agent's image gains `/data` (a managed agent's identity) and the same `USER`.
  > - **The vault key** is made inside the server's image by the new `afkfleet-server vault create-key` (S14), so a host needs only Docker.
  > - **Labels:** the four static OCI labels in both images; `created`, `revision` and `version` come with D's publishing.
  > - **CI:** the Stack workflow builds both images natively on `ubuntu-24.04-arm` and `ubuntu-24.04`, nightly, on demand and on the pull requests that touch `deploy/`, `.dockerignore`, `Cargo.lock` or `rust-toolchain.toml` (U16); the weekly audit scans both with Grype v0.121.0, failing on a HIGH or CRITICAL finding a newer package fixes (U12).
  > - **Tests:** `crates/fleet-server/tests/deploy.rs` and fleet-agent's pin both Dockerfiles against each other and against `deploy/prod/`'s configs, through `fleet_testkit::dockerfile`.
- [x] **P12.2** `deploy/compose.yaml`:
  - **Caddy:** ports 80/443, Let's Encrypt, HSTS; proxies `/api` and WebSockets to `server:8080` (as built: `/api/v1/*`, `/health/live` and `/health/ready`; the note below).
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

  > Note (P12.2, from Phase 6, group B, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **The database lives on a named volume, never a bind mount from a Windows folder.** Docker Desktop shows Windows files as mode 777, chmod can't change that, and the server refuses a database others can read.
  > - **A forgotten volume must fail a test, not lose data.** The image will likely pre-create the data folder so a named volume gets the nonroot owner, and then a missing mount is invisible to the server's folder check: the database would land in the container's throwaway layer. The compose setup and P12.7's smoke test prove the database survives a container restart.

  > Note (P12.2, from Phase 6, group E, the user's decision) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): **the server's `stop_grace_period` must exceed its shutdown deadline plus the 1 s runtime shutdown:** `request_timeout_secs` + 5 s + 1 s, so more than 21 s for the default 15 s request timeout. Otherwise Docker's SIGKILL replaces exit code 4 and the last line, "the server stopped".

  > Note (P12.2, from Phase 7, group A, the user's decision) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **secret files must be owner-only.** fleet-startup's loader refuses any file with group or other bits. Compose bind-mounts file secrets with the host file's owner and mode, and its uid/gid/mode settings don't apply to file-backed secrets. So the compose setup and the runbook (P12.6) `chown 65532:65532` and `chmod 600` every secret file, and P12.7's smoke test runs with them that way. Like the database, secrets never come from a Windows folder through Docker Desktop (shown as mode 777, which chmod can't change).

  > Note (P12.2, from Phase 5, group C, the user's decision) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)): Production is linux/arm64. The agent's memory and CPU limits come from group D's measured demo numbers, with headroom, not from the dev compose file's 512 MiB and 1 CPU.

  > Note (P12.2, from Phase 5, group D, the DoD demo) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)): The agent's container, measured with `docker stats` during the DoD demo on 2026-10-10. That's 7198 frames over the hour, with one server restart, for **5 bots** (four on `afk`, one on `farm`).
  > - **Memory:** peak 26.9 MiB (25 s after the restart), mean 21.9 MiB, p95 22.3 MiB.
  > - **CPU:** peak 6.59 % of one core, mean 4.06 %, p95 5.80 %.
  > - **Where:** x86_64 under Docker Desktop (AMD Ryzen 7 9800X3D, 8 CPUs). Memory should carry over to arm64 roughly; CPU percentages don't.
  > - **A baseline, not a total.** ADR-0008's measurement grew by about 4 MiB per bot (57 MiB at 10 bots, 222 MiB at 50). So P12.2 extrapolates from these numbers to the production bot count and adds headroom, rather than reading the 5-bot figures as limits.

  > Note (P12.2, from Phase 10, group B) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)):
  > - **The gRPC port stays unpublished** unless remote agents exist (S13); the container sets `[grpc] bind = "0.0.0.0:7443"`, and the server certificate names what agents dial (`pki server-cert --dns/--ip`).
  > - **The PKI's four files** are read through `secret_file`, so each is owner-only (the `chown 65532:65532` and `chmod 600` above, for the CA key and the server key as for the vault key). The CA key is a Docker secret; the server's pair may live on the data volume.
  > - **The agent's identity** lives on its `/data` volume, never a secret (S1); the CA certificate it pins may be a secret or a read-only file.
  > - **`pki init` runs once,** before the first start: a new CA would lock out every agent, so nothing in compose ever runs it again.

  > Note (P12.2, from Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): a managed agent in `compose.yaml` lists its Docker network in `[control_plane] private_targets` only if its bots dial a server on it, with that network's subnet pinned in compose: Docker also hands out 192.168.0.0/16 (U15). **Exit 5** means the server refused the agent for good, so `restart: on-failure` would restart it into the same refusal: P12.2 sets its policy (a restart limit, say), and the runbook says what to do.

  > Note (P12.2, from Phase 11, group D1) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S28): **Caddy must never log the live events socket's query string.** `GET /api/v1/events?ticket=…` carries a ticket: single use, 30 s, and only with its session's bearer token, but still a secret. Caddy writes no access log unless a `log` directive asks for one; if P12.2 adds one, its `query` filter deletes `ticket` from `request>uri`, and P12.7's smoke test checks the log holds none.

  > Note (P12.2, from Phase 11, group L3, PR #90's review) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), U110, S127): **the same goes for `q` on `GET /api/v1/accounts/*/grant-candidates`.** It's what a user typed into "Share with", sent as they type it, so a password pasted into the wrong field would be in it. A `log` directive's `query` filter deletes `q` on that route as it deletes `ticket`.

  > Note (P12.2, from Phase 12's planning, the user's decisions) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), U3, U4, U5, U8, R4): **beside other services, both proxy paths.**
  > - The compose project is afkfleet's own, with its own network and named volumes and no host networking. It touches nothing else on the host.
  > - Caddy is optional, for a host where 80 and 443 are free. Otherwise the server's HTTP port is reached through the host's existing reverse proxy, and the runbook lists what that proxy must do (TLS, WebSocket upgrades, `X-Forwarded-For` with `trusted_proxies`, HSTS, no `ticket` or `q` in its logs).
  > - The production server config sets `[database] backup_dir`. The backups' folder (owned by 65532, files 0600) is readable by the operator's SFTP account, which their PC pulls from.
  > - One setup script does the one-time steps; its form is group C's.

  > Note (P12.2, from Phase 12, group B) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), S10, U1, U5): **the metrics ports.** The production configs set `[metrics] bind = "0.0.0.0:9480"` for the server and `"0.0.0.0:9481"` for agents, reachable only on the project's own network: no `ports:` and no `EXPOSE` for either. Caddy and the host's existing proxy route only the server's 8080, never a metrics port.
  > Note (P12.2, from P13.5) ([ADR-0033](docs/adr/0033-chaos-tests-and-the-missing-database.md)): `setup.sh init` creates the database itself, with `migrate` through the `maintenance` service, after the PKI and before the server's first start; the server refuses to start without the file. A restored state has its database, so `init` on one skips the step.

  > Note (P12.2, Phase 12's group C) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), S14–S18, S21, U10, U18–U20, R16): **as built.**
  > - **The project:** `deploy/compose.yaml` (the server, the managed agent, and the `setup` and `probe` services under profiles of their own), plus one file per proxy path, picked by `COMPOSE_FILE` in the host's `.env`: `compose.edge.yaml` with `compose.caddy.yaml` (afkfleet's Caddy on 80 and 443) or alone (an existing proxy in a container), `compose.loopback.yaml` (a proxy on the host, at `127.0.0.1:8080`), and `compose.grpc.yaml` for remote agents, whose own project is `compose.agent.yaml`. The configs are `deploy/prod/`'s, the Caddyfile `deploy/Caddyfile`, and `deploy/setup.sh check|init|update` does the one-time steps (S18).
  > - **Settings in `.env`** (U18): `deploy/env.example` (not `.env.example`, R16) lists every one; compose passes `.env` to the server, the agent and `setup`, and an update never replaces it.
  > - **Deviations:**
  >   - **No Docker secrets** (S14): the vault key and the CA's key live on the `keys` volume, the CA certificate on `ca`, the server's pair on `tls`, each made inside the server's image as 65532 by `setup.sh init` (`vault create-key`, `pki init`) and mounted read-only into the server. A remote agent's CA certificate is a compose secret from a host file.
  >   - **Caddy's routes** are `/api/v1/*` (WebSockets included), `/health/live` and `/health/ready`; everything else is 404 (S16).
  >   - **`trusted_proxies`** is each path's proxy's own `/32` (S16), not a network; Appendix A's example reads so.
  >   - **The server's metrics port** binds the server's fixed address on `fleet` (`AFKFLEET_SERVER_FLEET_ADDRESS`), not `0.0.0.0` as group B's note says, so no proxy reaches it (U19).
  >   - **Caddy adds back `NET_BIND_SERVICE`** beside `cap_drop: [ALL]`: its pinned binary can't start without it (S21).
  >   - **The agent restarts `unless-stopped`,** like every service: an exit 5 loops at Docker's backoff, at most once a minute (S17).
  >   - **`setup.sh init` runs no `migrate`:** `serve` migrates at its start.
  >   - Caddy's error logger logs a 5xx's URI even without an access log, so the Caddyfile's filter, which deletes `ticket` and `q`, is on its global log (S16; the notes from Phase 11 above).
  > - **Limits** (`.env` settings, no swap): the server 512m, 1 CPU, 256 pids; the agent 512m, 1 CPU, 512 pids (50 bots from ADR-0008's numbers, about 1.9× headroom); Caddy 256m, 0.5 CPU, 128 pids. Grace periods: the server 30 s, the agent 20 s, Caddy 25 s.
  > - **The backups' host side:** the folder (`AFKFLEET_BACKUPS_DIR`, 65532, 700, in a root-owned folder) and `deploy/host/sftp-backups.sh` for the read-only SFTP account (S15), tested in a throwaway sshd container.
  > - **Production bots** dial public servers from the first start, and no private network (U10).
  > - **Tests:** `crates/fleet-server/tests/compose.rs` and both crates' `tests/deploy.rs` pin the files; P12.7's smoke tests run them.
- [x] **P12.3** 🔴 Backups:
  - a scheduled `VACUUM INTO` with retention, plus a `backup` CLI
  - a restore runbook
  - Backups contain only ciphertext. **Back up the vault key separately and securely.**

  > Note (P12.3, from Phase 11, group E) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S44, S45): a `VACUUM INTO` holds a read snapshot, so the retention's batches meanwhile grow the WAL: schedule the backup between its runs, or have both share a lock. A backup keeps deleted chat for the backup's own retention (`secure_delete` overwrites only the live file), and that retention needs a name apart from P11.6's. Whether to set `journal_size_limit` is decided here.

  > Note (P12.3, Phase 12's group A planning) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), S2–S8, R1–R3): **the design.**
  > - **The file:** `VACUUM INTO ?` with the path bound, on a read-only connection of its own. It's written into an owner-only dot file, synced, checked (the schema and `quick_check`), then published as `afkfleet-backup-YYYYMMDDTHHMMSSZ.db` by a hard link that never overwrites.
  > - **The schedule:** inside the retention task's loop, after a pass, when the newest backup's stamp is `backup_interval_hours` old. A stop cancels it.
  > - **The config:** off unless `[database] backup_dir` is set; `backup_keep` 1–60 and `backup_interval_hours` 1–168, their product at most 720 h. Pruning touches only the server's own names.
  > - **The command:** `afkfleet-server backup` takes one now and prints its path.
  > - **No audit entry,** and `journal_size_limit = 0` on every connection.
  > - **Restoring** follows the runbook's steps.
  >
  > If sqlx's macros can't read `quick_check` or `journal_size_limit`, the work stops for the user (rule 8).
  >
  > *Built in group A ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), "As built (group A)"):* sqlx's macros describe both reads, so no exception was needed. fleet-core's `backup` module holds the rules; `infra::sqlite::backup` the file, `app::backup` the service, `cli::retention` the schedule, `cli::backup` the command, `just dev-backup` a try on the dev database; `docs/runbook.md` starts with backups and restoring one. The off-host copy (U3) and the compose form come with group C.
- [x] **P12.4** A Prometheus metrics endpoint on an internal port, with the key metrics documented.

  > Note (P12.4, from Phase 7, group B, the user's decision) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): a counter of detected refresh-token reuses belongs among the key metrics, so alerting can pick up possible token theft without anyone reading `/audit` or the logs.

  > Note (P12.4, from Phase 10, group E) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): the per-bot limit's refusals (`bot_actions` in the minute's summary) belong with the other rate limits' counts, by limit.

  > Note (P12.4, from Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): `afkfleet_agent_connects_refused_total` (by reason) and `afkfleet_agent_identity_conflicts_total` belong among the key metrics: past the daily cap a disabled or revoked certificate still in use shows only at `debug` (S30), and a conflict is recorded once an hour (S26).

  > Note (P12.4, from Phase 10, group F) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): session grants and denials belong among the key metrics, e.g. `afkfleet_session_grants_total{kind,fresh}` and `afkfleet_session_denials_total{reason}` (S61, R43: a grant writes no audit entry, and its `info` line is the only other trail); the fresh-session limit's refusals (`fresh_sessions` in the minute's summary) go with the other rate limits' counts.

  > Note (P12.4, from Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): the outbox's counts (chat dropped, refusals, session requests dropped) and the ingest's (rows stored, skipped and dropped per agent; housekeeping's `chat_dropped`) are metrics to export.

  > Note (P12.4, from Phase 11, group E) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S45): the retention's rows deleted per phase, a run's time and batches, and the time of its last successful run belong among the key metrics: the task isn't watched, so that time is the alert for one that died or fails every run.

  > Note (P12.4, from Phase 12's group A planning) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), S6, R4): **the backups' metrics**: the last success, failures, a backup's duration and size. A backup writes no audit entry, so these and its log lines are its only trail. The `backup` command runs in a process of its own, so `serve` reads the last success at start from the newest backup's name.

  > Note (P12.4, Phase 12's group B) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), S9–S12, R5–R8): **as built.**
  > - **`[metrics] bind`** in both binaries, off unless set: an IP address and a port other than 0, and on the server not `[http]`'s or `[grpc]`'s port. One `info` line at startup says where it listens, or that metrics are off.
  > - **The server:** `crate::metrics`'s `ServerMetrics` records into each server's own recorder through typed methods with fixed labels, every series at 0 from the start. `GET /metrics` is a route of its own router (`metrics_routes`) behind the API's middleware, on a second listener that `serve` stops with the API's; the API's port answers 404 there.
  > - **The agent:** `metrics_endpoint`, a responder of its own (4 connections, 5 s each, a 2 KiB head, nothing from a request logged), and the outbox's three counters, sampled with the diagnostics every 5 s.
  > - **Totals only:** no label is per user, per bot or per agent. The ingest note's "per agent" stays in housekeeping's minute line, which names the agent (S11).
  > - **The docs:** the runbook's Metrics lists every key metric with its labels, how to read them from the host and what to watch.
- [x] **P12.5** Tauri release:
  - NSIS installer.
  - Updater key pair; the private key is kept **offline**.
  - An update manifest, hosted on GitHub Releases.
  - No Authenticode signing: the installer is unsigned, and README and the runbook explain Windows' "Windows protected your PC" ("More info", "Run anyway") ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), U1).
  - CI builds the installer on a Windows runner.

  > Note (P12.5, group D1) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), U25–U33, S22, S24, S27, R19–R26, "As built (group D1)"): **as built.**
  > - **The installer:** NSIS, per user (no administrator prompt), WebView2's download bootstrapper, publisher "afkfleet", unsigned. `just app-installer` refuses to start while a signing key's variable is set, builds with `--no-sign`, and prints the installer's SHA-256; CI's `tauri-build` job runs it on every pull request.
  > - **The updater:** `tauri-plugin-updater` 2.14.1 without default features, the committed public key (ID `D2C6CEB25F9F2CB0`; the private key and its password never left the user's PC), `releases/latest/download/latest.json`, `requireSignedVersion`, the passive installer. Its plugin commands are never granted: the app's own `check_update` and `install_update` are, and updates are on only in a release build whose updater points at the release's manifest alone (and in the rehearsal's debug build, whose updater points only at loopback): the policy reads the build's endpoints, not its identifier, so the check exe holds no real identifier. The UI checks at each start and on Profile's Updates, and installs only after "Install and restart".
  > - **The signing step and the rehearsal** are the user's alone: `just updater-sign` (the key by path, the password at a hidden prompt, into the CLI's environment alone) and `just update-rehearsal` (a throwaway key, a separate product on loopback).
  > - **The release workflow** (`.github/workflows/release.yml`): a tag vX.Y.Z on main; the guard requires CI green on that commit; each build job tests as Stack does and saves the images by ID, with no package permission; each push job, on a fresh runner that checks out `scripts/release.mjs` alone and runs no build tool, loads and checks them, then logs in, pushes `X.Y.Z-<arch>` and logs out (PR #98's review); the merge job makes `X.Y.Z` from the digests; the installer is built from scratch; a job with `contents: write` and no repository code drafts the release. The maintainer signs and publishes by `docs/releasing.md`.
  > - **Deviations:** README and the runbook don't explain SmartScreen: the friends' guide `docs/using-the-app.md` does (U34). Cargo.lock gains six entries, not two (four are macOS-only). The signing step prompts for the password itself instead of leaving it to the CLI's prompt (S24). The pushed images are checked by their platform and release labels instead of an image-ID comparison (R24). A tag the registry denies reading counts as absent (the first release's packages don't exist yet). The updater's on/off policy reads the build's endpoints instead of its identifier (S27, R20), as the check exe's guard (ADR-0020, U87) refused a binary holding the identifier constants.
- [x] **P12.6** `docs/runbook.md`:
  - install, configure, bootstrap the Owner
  - enroll an agent
  - rotate keys, renew certificates
  - **Minecraft version upgrade** (azalea + nightly + test-server bump)
  - restore a backup
  - incident response: revoke sessions, disable users and agents

  > Note (P12.6, from group D's planning, the user's change) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), U34, R29): **docs for three readers.** The runbook is the operators' guide alone: nothing about GitHub settings, tags, signing or the updater's key. The **Minecraft version upgrade** is the maintainer's code work, so it goes into `docs/releasing.md` (D2), with every note below about it. What friends do (installing the app, SmartScreen, "Connecting to a server, for friends") is in `docs/using-the-app.md`, written in D1. D2 moves README's operator material into the runbook, leaving a pointer.

  > Note (P12.6, from Phase 5, group D, the user's decision) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)): The **Minecraft version upgrade** section carries ADR-0003's bump procedure, whose verify step now runs `just ci`, `just test-slow` and `just demo-agent` (1 h), with the demo's `summary.txt` in the bump's pull request.

  > Note (P12.6, from Phase 11, group E) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), U19, S44, S45): **disk sizing** for chat (bots × `max_lines_per_bot` × up to 4.7 KB a line, plus 120,000 lines between two runs, plus the WAL); a lowered `max_lines_per_bot` or `retention_days` applies at the first run after the restart; the file doesn't shrink without a `VACUUM`; a full disk makes every run fail, since a delete needs WAL space; encrypt the data volume on the host, since `secure_delete` reaches only what SQLite writes.
  > Note (P12.6, from Phase 9, group B, the user's decisions) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)):
  > - **The manual Microsoft check:** `just test-ms-account` checks the Microsoft adapter with the operator's own account. It prints a link and a code, then pass or FAIL per step; `just test-ms-account declined_link` checks a refusal.
  > - **CA certificates in the server image:** reqwest's platform verifier reads the system's CA store on Linux, so the server image needs one, or every Microsoft call fails as `Certificate`. The agent image's distroless/cc base already has them.
  > - **The Minecraft version upgrade** carries ADR-0003's new steps. A new azalea crate that depends on azalea-auth is named by cargo-deny, and `deny.toml`'s wrapper list is updated with an ADR note. The adapter's endpoints, fields and headers are compared with the new azalea-auth's `auth.rs`.

  > Note (P12.6, from Phase 10, group B, the user's note at the plan review) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the server certificate's yearly renewal.** It expires after 365 days, and then every agent fails to connect. The runbook's steps: stop the server, move `server.crt` and `server.key` away, run `afkfleet-server pki server-cert --config … --dns … --ip …` with the names agents dial, start the server. The CA stays, so no agent enrolls again.

  > Note (P12.6, from Phase 10, group B) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **enrolling an agent:** `pki init` once (it prints the CA's fingerprint); a token from the server's host (`afkfleet-server agent create-enrollment-token`; the API's route is gone, [ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), U76); then pipe its bundle into `afkfleet-agent enroll` within 15 minutes. A token is single-use; exit 3 means the server enrolled an agent the host didn't store, which is disabled before enrolling again with a new token.

  > Note (P12.6, from Phase 10, group E) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **an agent next to a local Minecraft server** lists that network in `[control_plane] private_targets`, with a comment saying why; a bot whose last disconnect is `connect_failed` with `blocked` points at an address its agent refuses (S46). Listing a network opens it to every Manage holder of a bot on that agent (the threat model's accepted risk).

  > Note (P12.6, from Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **incident response for agents:** `afkfleet-server agent list` shows every agent; `agent disable --id …` disables a lost or stolen one for good from the server's host, beside the running server (its stream ends within 10 s), as the API's disable does. An agent whose `enroll` ended with exit 3 is disabled the same way.

  > Note (P12.6, from Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **time and certificates.** The server's clock decides when an agent's certificate has expired, and TLS checks it against the real time on both hosts, so both keep NTP. An agent whose certificate expired before P13.6's renewal enrolls again: disable it first, which frees its name. A restored backup can hold an agent that was disabled after it was taken: disable it again (S27).

  > Note (P12.6, from Phase 10, group F) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **session grants.**
  > - A bot that reads `Failed` with `session_denied` was refused for good by the session-token service (`reauth_required`, `refused`, an unusable stored token): the server's `info` or `error` line "a session request was denied" names the reason; fix the account (link it again), then Reset the bot.
  > - "a granted session expires within the connect timeout" on an agent means its clock or the server's is off by minutes: both hosts keep NTP.
  > - The shutdown's `warn` "session-token refreshes were still running…" and a later `reauth_required` mean a rotated refresh token was lost: link the account again.
  > - After an agent is compromised, every account whose bot it held counts as exposed until about 24 h after its last `bot.unassign`, `bot.detach` or `agent.disable`; the server's "a session was granted" lines narrow it where they're kept (S61).
  > - A bot stuck backing off while the minute's summary shows `fresh_sessions` refusals is past its fresh-session limit (S58).

  > Note (P12.6, from Phase 11, group C3) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S22, S23, S25):
  > - **An agent older than its server's modes** can't read a spec with a schedule it doesn't know (`on_respawn` for agents built before C3): it ends its stream as `unreadable` and reconnects in a loop, with the `error` line "a message from the server couldn't be read" naming the field, while its running bots keep their old specs. Update the agent; servers go first, as P12's note says.
  > - **A server rolled back past C3** reads a mode with `on_respawn` steps as `Corrupt`, so its bots are left alone and `GET /modes` fails: switch the bots off such modes, or delete them, before rolling back.
  > - **Respawns:** a `warn` "the server didn't confirm a respawn; retrying" means the server ignored a respawn request for 5 s; twelve in a row end the session `respawn_failed` and the bot joins again. The Minecraft version upgrade carries ADR-0003's check of azalea's `Dead` and `HasClientLoaded`, which the confirmation reads.

  > Note (P12.6, from Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)):
  > - **Connecting to a server, for friends** *(the user's note on the app's CA field)*: the runbook and the README get a section with the server address to enter; with Caddy and Let's Encrypt (P12.2's default) the app's CA field stays empty; for a server without a public domain, how the operator exports the HTTPS CA certificate (never the control plane's agent CA from `pki init`) and hands it out, and how friends compare its fingerprint in the app's server setup.
  > - **The agent's exit 5** and what to do (disable the agent and enroll it again, or fix its name), **its start errors** (each names its files and the fix), and **`private_targets`**' rules (at most 32 networks, each inside a private range; never checked in standalone mode).

  > Note (P12.6, from Phase 12's planning, the user's decisions) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), U1–U4, U8, S8, R4): **an operator's guide for any Linux host with Docker.**
  > - **It names no provider:** firewall steps are generic ("open 80 and 443 in your provider's firewall and the host's"), and so is keeping a dynamic-DNS record in step with the host's IP (its token in a root-only file on the host, read by the update, never on a command line).
  > - **The install:** about ten commands with one setup script, then two commands per update. Every step says what success looks like.
  > - **The uninstall** removes everything afkfleet added and nothing more.
  > - **The restore** follows S8's steps.
  > - **The backups' off-host copy** is the operator's PC pulling over SFTP.
  > - **Disk sizing** adds (`backup_keep` + 1) × the database's size and the WAL's peak during a backup.
  > - **Upgrades:** a backup is taken with the old server before an upgrade, and the key rotation's "copy the database" becomes `backup`.
  >
  > *Group A starts the runbook with "Backups and restore".*

  > Note (P12.6, from Phase 12, group B) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), S10): **the metrics** in the finished runbook: the compose form of reading them (from a container on the project's network, never through a published port), and the alerts group B's "Metrics" section starts with.

  > Note (P12.6, from Phase 10, group D) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **bots on a lost agent:** disable the agent, make sure its host and its bots are stopped, then `afkfleet-server agent release --id …`; a running server places the bots within 30 s. Nothing moves them by time (S32). **After restoring a backup:** an agent may run bots the database doesn't give it; they're fenced (`bot.stray`) until the agent removes them, and the agents' bots count shows them.

  > Note (P12.6, group D2) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), U38–U41, S28–S51, R30, R31, "As built (group D2)"): **as built.**
  > - **The runbook** (`docs/runbook.md`) is the operators' guide alone: 22 sections, every step with its success line, placeholders alone. README's operator material moved into it, each heading left with a pointer. `scripts/runbook-commands.mjs` ties each of its commands to the test that runs it, or its block says why none can.
  > - **The host's release files:** `afkfleet-deploy-X.Y.Z.tar.gz`, written byte for byte the same by `scripts/deploy-bundle.mjs` in the release's `bundle` job (`contents: read`), its `RELEASE` pinning both images by their index digests; `sh setup.sh fetch X.Y.Z`, and `update`'s refusal of a running server newer than the folder's release.
  > - **Operations on the volumes:** the `files` and `maintenance` services and `setup.sh`'s `restore`, `forget-aside`, `renew-cert`, `keys-backup` and `keys-restore`; the vault key's rotation and incident response as runbook commands; Docker Engine 28.0 or newer checked at the install (U40); the keys' copy leaving the host through the operator's own SSH login, compared by SHA-256 (U41).
  > - **Tests:** `smoke::ops::slow_ops_restore_after_a_crash` and `smoke::ops::slow_ops_new_host_from_the_keys_copy` in `just test-stack`, which CI runs with `setup.sh` itself; setup.sh's and files.sh's stub tests under dash.
  > - **The Minecraft version upgrade** is the maintainer's, in `docs/releasing.md` (U34): ADR-0003's steps, numbers kept, and ADR-0003 points there.
  > - **Deviations:** `put-back` and `tls-back` keep what they replace as `.undone-STAMP`, which `forget-aside` deletes too; the runbook's Owner line is marked as run by no test (it asks at a terminal; the tests use `--password-stdin`); more of the runbook's commands run in tests than S46 listed; PR #98's updater note was half right (a newer release without a Windows entry is `update_manifest`).
  > - **PR #99's review** (ADR-0022, "PR #99's review (group D2)"): a remote agent's folder takes `check` and `update` (no backup); a restore marks its stamp finished after the migration, and `forget-aside` refuses a stamp that isn't, whatever the server's health; a new host's restore ends with `init`; fetch and keys-restore hardened; the firewall, the keys' copy on the PC, a new host's backup staging, the docker group and the uninstall's images in the runbook; every safety check held by a test shown red by its mutation.
- [x] **P12.7** 🔴 Compose smoke test in CI, run nightly:
  1. `up`
  2. health checks pass
  3. create the Owner
  4. log in through `fleet-client`
  5. `down`

  > Note (P12.7, from Phase 7, group A, the user's decision) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): the smoke test creates the Owner with `docker compose run -T … user create-owner --username … --password-stdin`, since CI has no terminal for the prompt.

  > Note (P12.7, from Phase 7, group C, the user's note) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): the smoke test checks that a request through Caddy reaches the server with the client's real address (`[http] trusted_proxies` honored), so a missing or wrong `trusted_proxies` can't put every client into Caddy's one bucket unnoticed.

  > Note (P12.7, from Phase 11, group L3, PR #90's review) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S28, S127): when Caddy writes an access log, the smoke test opens the live events socket and asks `grant-candidates` for a match, then checks the log holds neither the `ticket` nor the `q` it sent (P12.2's notes).

  > Note (P12.7, from Phase 12's planning, the user's decision) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), U5): **both proxy paths** are smoke-tested: afkfleet's own Caddy, and afkfleet behind a stand-in for an existing reverse proxy.

  > Note (P12.7, from Phase 12, group B) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), S9, S10): the smoke test scrapes both metrics endpoints from inside the project's network, and checks that `/metrics` through Caddy and through the other proxy path reaches neither.

  > Note (P12.7, Phase 12's group C) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), S19, U5, U20, U21): **as built.** fleet-integration's `slow_smoke_with_caddy`, `slow_smoke_behind_a_container_proxy` and `slow_smoke_behind_a_host_proxy` bring the production project up from a copy of `deploy/`, with a `.env` and an override of their own (offline accounts allowed there alone, U20), under project names of their own. Beyond the five steps (the Owner created with `--password-stdin`, the login through fleet-client with the full 2FA flow), they check the client's real address and a forged `X-Forwarded-For`; no `ticket`, `q`, password, TOTP secret or recovery code in any log after forced 502s; both metrics ports from inside and neither through any path; `.env`'s settings in the server's environment; the database and a backup across a recreate; memory and pids under 75 % of the limits (U21); Caddy's one capability; the image alone refusing to start; and every exit clean. The Stack workflow runs them nightly on arm64 and amd64, with `deploy/setup.sh init` doing the setup there; `just test-stack` and `just test-slow` run them elsewhere, the test doing the same steps itself.

- [x] **P12.8** Tidy-up (group E, `p12/tidy-up`, after D2's merge; [ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), U35): the repository tidied for readability, with no behaviour change. Its plan first lists what it would move, merge, shorten or delete, each with why, for the user's approval. Candidates:
  - README's length and order;
  - an index for `docs/`;
  - stale notes, dead scripts or recipes, and outdated comments;
  - Plan.md's finished phases condensed, as long as every note an ADR or a comment points to stays findable.

  > Note (P12.8, as built) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), "Group E (P12.8)", U42–U52, S52–S58): one pull request (U42). README is a short intro with a quickstart and a status line (U44, U45, S52); its developer material is `docs/development.md` (U46), and `docs/README.md` indexes `docs/` (S53). The runbook gained what the DoD's run found and lost its repeats (S56); `docs/releasing.md` gained what the first release found (U52). Phases 0–11 above are summarised in place (U43, S55), every citation of Plan.md pinned by `scripts/plan-citations.test.mjs` (S54). `spikes/tls-provider/` is deleted (U47), and tasks keep `docs/development.md` current, README only its intro, quickstart and status (U48).
  > Deviation: the runbook lost about 5 % of its words, not the planned ~35 %: what S56 keeps word for word makes up most of its lines. U49's subagent write guard, built during the planning, was dropped with the merge guard when [ADR-0025](docs/adr/0025-a-simpler-workflow.md) replaced the pipeline, so this group is the tidy-up alone.

**DoD:**
- A clean afkfleet install (no earlier afkfleet state) on an existing Linux host that keeps its other services, following only the runbook.
- A friend on another PC installs the release build, registers with an invite and logs in with 2FA, over HTTPS against that system (moved from Phase 8's DoD).

> Note (Phase 12's DoD, the user's change at the planning) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), U6): **"A fresh VPS gets to a running system by following only the runbook" became the clean install above.** Both checks are the user's, on their own host, once group D is merged and the first release exists. The AI never touches the host.

> Note (Phase 12's DoD, group D2) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), S49): **the user's kit is `docs/dod/phase-12.md`:** part (a), the clean install by the runbook alone, and part (b), the friend's login by the friends' guide alone, each step with what success looks like and a place for its outcome. It's opened at the first release's tag; the AI never runs any of it.

> Note (Phase 12's DoD, passed) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), "The DoD's run"): **both parts passed on 2026-11-07 with v0.1.0,** run by the user. The gaps the run found in the guides went into P12.8.

---

### Phase 13: Hardening
**Goal:** Prove the security and resilience claims.
**Introduces:** cargo-fuzz (`libfuzzer-sys`, `arbitrary`), cargo-mutants.

> Note (P13, from Phase 12's DoD, the user's finding) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), "The DoD's run"): **the update download's URL scheme.** The app's own update download accepts any URL scheme `latest.json` names; release builds should require https, as defence in depth (the installer is still verified against the committed key).
>
> Note (P13, group A) ([ADR-0027](docs/adr/0027-certificate-renewal-and-phase-13-refinements.md)): **done with P13.6.** A release build refuses a download URL that isn't https before it sends anything (`update_invalid`), and a redirect to one; the rehearsal's debug build still downloads from loopback over http.

> Note (P13, Phase 13's planning, 2026-11-10) ([ADR-0027](docs/adr/0027-certificate-renewal-and-phase-13-refinements.md)): **the groups,** one pull request each, in this order, under [ADR-0025](docs/adr/0025-a-simpler-workflow.md):
>
> | Group | Branch | Tasks |
> |---|---|---|
> | A | `p13/p13.6-cert-renewal` | P13.6 and the update download's scheme, released as v0.1.2: an agent's certificate ends 60 days after its enrollment, so this goes first |
> | A2 | `p13/p13.9-no-step-up` | P13.9, the user's change to the order on 2026-11-11: right after A and before P13.1, and in v0.1.2 too, which wasn't tagged yet ([ADR-0028](docs/adr/0028-a-session-is-enough.md)) |
> | B | `p13/p13.1-authz-matrix` | P13.1 |
> | C | `p13/p13.2-fuzz` | P13.2 |
> | D | `p13/p13.3-mutants` | P13.3, after B, whose tests kill their share of the mutants first |
> | E | `p13/p13.4-load` | P13.4 |
> | F | `p13/p13.5-chaos` | P13.5, and the user's question whether `serve` should refuse a missing database file in production instead of creating an empty one (an unmounted data volume would otherwise start an empty fleet), without breaking a first install |
> | G | `p13/p13.7-reviews` | P13.7, P13.8 and the phase's DoD |

- [x] **P13.1** 🔴 **Authorization matrix test:** every endpoint × {anonymous, Member without grant, View, Control, Manage, Admin, Owner} → the expected status. The table is generated and snapshotted.

  > Note (P13.1, from Phase 9, group E) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): the cast steps up first, or every step-up route reads 403 `step_up_required`. The account routes need a pending account (review, grants, re-link) and a grantee with implicit Manage (the grant routes' 422 and 409).

  > Note (P13.1, from Phase 10, group E) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the bot routes** join the matrix: every lifecycle call, `PATCH` (step-up first), and the mode switch with a mode on the allowlist and one off it. The per-bot limit is spent only after `authorize()`, so a stranger's answer stays 404; the cast needs a fresh limit per row, or rows after the fifth read 429.

  > Note (P13.1, from Phase 12, group B) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), S9): the metrics router joins the matrix: `GET /metrics` on its own port for every cast, and on the API's port as 404.

  > Note (P13.1, from P13.9) ([ADR-0028](docs/adr/0028-a-session-is-enough.md)): only `POST /me/password` and `POST /me/mfa/reset` are step-up routes now, so the two notes above that have the cast step up first hold for those two alone: every other row of the matrix runs on a session that never stepped up, and `PATCH /bots/{id}` needs none.

  > Note (P13.1, as built) ([ADR-0029](docs/adr/0029-the-authorization-matrix.md)): **the matrix.**
  > - **Where:** `authz_matrix` in fleet-server's `accounts` test binary, on serve's own app. Its table is the snapshot `accounts__authz_matrix__authorization_matrix.snap`: 88 rows over the 68 registered routes, and a test that fails for a route without a row.
  > - **Every cell has a world of its own:** a fresh database and app, the same people and things, one request. So each cell's limits start full, and no cell meets another's writes.
  > - **A second block** asks the 25 rows under a bot or an account again about an Admin's account, as a Member, another Admin and the Owner: the seven casts can't show whose account it is.
  > - **Rules asserted beyond the snapshot:** no cell is a 400, 422, 429 or 5xx; a public route answers everyone alike; nobody gets anything but 401 elsewhere; a Member without a grant gets 404 under a bot or an account; a higher grant may do what a lower one may; each step-up route asks in one row and not in another; an Admin's account is 404 for a Member and another Admin.
  > - **What it found:** nothing: every cell reads as fleet-core's matrix and the handlers' docs say.
  > - **Deviations:** the restart row and the three chat rows end at their 409 (`bot_not_running`, `bot_not_online`) for a caller who may act, since nothing in-process runs a bot; the 409 comes after `authorize()`. The table shows one config: offline accounts on, `/spawn` allowlisted. The second block isn't in P13.1's text.
- [x] **P13.2** Fuzz targets:
  - value-object parsers
  - the chat sanitizer
  - mode definition deserialization + validation
  - proto → core conversions
  - the vault envelope parser

  > Note (P13.2, from Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): the `AgentMessage` target reaches `fleet_proto::control::read_hello`, and one target runs `fleet_proto::codec`'s check of a hello's bots over arbitrary bytes (S25).

  > Note (P13.2, from Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): fuzz `IpNet`'s parser and `PrivateTargets::problems`.

  > Note (P13.2, from Phase 11, group H7) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S80, R138): two targets for the chat formats, which read untrusted server text: `ChatFormats::try_new` over any list of strings (no panic, an error that never holds the text), and `sender_of` over any valid formats and any received system line (no panic, and a `Some` is a valid `McUsername` read from the text's first characters).

  > Note (P13.2, from Phase 12, group B) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), S9): a target for the agent's metrics responder: its request-head reader and router over arbitrary bytes.

  > Note (P13.2, as built) ([ADR-0030](docs/adr/0030-fuzz-targets.md)): **the targets.**
  > - **Two crates:** `crates/fleet-fuzz` holds each target's body, a plain function over bytes that panics when a property breaks, and its tests run all ten over their seeds and proptest's inputs, so `just check` holds the properties without a fuzzer. `fuzz/` is the cargo-fuzz crate, a workspace of its own with its own lockfile, one line a target.
  > - **Ten targets:** `value_objects`, `chat_sanitizer`, `chat_formats`, `chat_sender`, `private_targets`, `mode_definition`, `agent_message`, `server_message`, `vault_envelope`, `agent_metrics`. Each note above has its target; ADR-0030's table says what each holds besides never panicking.
  > - **Where they run:** `just fuzz [target] [seconds]`, natively on Windows too (with MSVC's AddressSanitizer runtime, which the script finds); the Fuzz workflow weekly and on pull requests that touch the targets.
  > - **What they found:** no defect in the code under test in 36.5 million inputs. Two in the fuzzing itself, both fixed: a property of `chat_formats` that an entry spelling the error's own wording broke, and that on Windows a panicking target leaves no input behind, so each target now keeps its own.
  > - **Deviations:** the proto conversions are two targets, one per direction, and the codec's check of a hello's bots runs inside `agent_message`. `arbitrary` isn't used: the targets cut their bytes themselves. `just deny` gains a third pass over `fuzz/Cargo.toml`, and `deny.toml` an exception for libfuzzer-sys's NCSA license.
- [x] **P13.3** cargo-mutants on `fleet-core` and auth/vault. Add tests until no relevant mutants survive.

  > Note (P13.3) ([ADR-0031](docs/adr/0031-mutation-testing.md)): **done.**
  > - **Two scopes:** `core`, all of fleet-core (about 1400 mutants), and `auth-vault`, the files the auth and vault coverage gates list outside it (about 830). Every mutant there is relevant: a missed one gets a test, or a line in `.cargo/mutants.toml` with why no test could see it.
  > - **`just mutants [scope] [shards]`** runs each shard in a git worktree of the last commit under `target/mutants/`, changed in place, so no ignored file is ever copied and the working tree isn't touched; it refuses uncommitted changes. Outside `check` and CI: about an hour for `core` and eight for `auth-vault` on the development PC.
  > - **What it found:** 40 missed mutants in fleet-core (one of them only in its second run) and 33 in the auth and vault modules. 59 got a test, among them the live events viewer's (a hint that joined a read already made), a socket slot's drop and the planner's strays; 14 change nothing and are left out with their reasons. One was a defect: a refused JSON body's `debug` line never had the line and column ADR-0016 decided on, which is fixed.
  > - **Deviations:** the mutants run on Windows, so code compiled on Unix alone shows as missed: the secret file's mode check became a function of the bits, tested on every platform. `serde_path_to_error` became a direct dependency of fleet-server (it was in the lock through axum). Seven mutants are found only because the tests then never end (a loop that never yields, which no bound inside a test can stop); nextest's default profile has no time limit, which the pull request proposes to change.
  > - **Two follow-ups from #113's review, in group E's first commit** (ADR-0031's amendment of 2026-11-12): every nextest profile ends a test after 10 minutes, chosen after measuring CI's runners (the slowest test there, the authorization matrix, took 37 to 68 s), so a test that never ends fails by name; and `just mutants-clean` removes the runs' worktrees, through git, and `target/mutants`, never anything else in `target/`.
- [x] **P13.4** Load test: 200+ fake bots on one agent and 20 WebSocket clients. Measure CPU, RAM and event latency, then fix the hot spots.

  > Note (P13.4) ([ADR-0032](docs/adr/0032-the-load-test.md)): **done.**
  > - **`just load`** runs fleet-integration's `load_` test alone, in a nextest profile of its own: the real server and a real managed agent in one process, 200 bots on the fake connector, and 20 Members, registered through invites with 2FA, each with 20 grants and a live events socket of fleet-client's. It prints its report as `LOAD` lines and asserts only what must hold at any load. Outside `check` and CI.
  > - **Measured** on the development PC, one thread, release build: idle with 200 bots online and 20 sockets open is 0 % of a core and 54 MiB for the whole process; chat reaches its viewers within the ingest's half second (p50 about 270 ms) up to 1000 lines a second at 4 % of a core; from about 2000 lines a second the server drops and counts what one agent's stream can't hold, and no socket ever resyncs.
  > - **Two hot spots.** A retention run committed one batch a bot, each with its 100 ms pause: 21.7 s for 200 bots. A batch now takes rows of as many bots as fit: 1.3 s. And while sockets have events to handle, every commit makes the server read the whole rights snapshot again, about 1.3 ms at this size; it shows only when one client makes hundreds of separate writes, and it's left as it is, with its numbers.
  > - **The earlier notes, answered in the ADR:** the conductor and the ingest are loaded and chat's stored size measured (at most 112 to 245 bytes a short line); what a commit costs the sockets is counted (the rights' reads and their time, the reads done again, no resync at the ingest's peak); a retention run at 1000 lines a second doesn't delay chat; both metrics ports answer a scrape in about 2 ms meanwhile; after a restart of both, 200 sessions are granted at 570 a second in a debug build.
  > - **Deviations:** Microsoft's refresh rate after a restart isn't measured (the bots are offline accounts, which need none), `GRANT_WORKERS` stays; no cap on gRPC connections an address was called for; everything ran on one thread. `sysinfo` 0.40.1 is a new dev-dependency of fleet-integration, for the process's CPU time and memory.

  > Note (P13.4, from Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): the gRPC port counts no connections per address (S22's residual); a cap, if the load test calls for one, goes into the listener's map of accepted sockets.
  > Note (P13.4, from Phase 10, group F) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): measure grant throughput after a restart of both server and agent (no cached sessions: about 1.3 refreshes a second per agent with `GRANT_WORKERS` = 4), and retune the constant if needed; a load test that asks fresh sessions raises `fresh_sessions_per_bot_per_hour` in its config (S57, S58).

  > Note (P13.4, from Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): load the conductor and the ingest too, and measure chat's stored size under that load (P11.6's note: retention must bound size, not only time).

  > Note (P13.4, from Phase 11, group D1) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S31, S35): the 20 WebSocket clients open real sockets with tickets, and the test measures what each commit costs them: every open socket checks its session again after each commit (S31), and from D2 on the rights snapshot is rebuilt (S30). A `resync` in the clients' logs means a socket fell more than the bus's 1024 events behind.
  > Note (P13.4, from Phase 11, group D2) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S40, S41, S43, R38): S41's test measured, in a debug build on Windows with 200 bots, 20 users and 400 grants, a rights rebuild at 2.3 ms (median; 2.8 ms at most) and one viewer's batched read of 200 hinted bots at 5.5 ms (11 ms at most), so a hint for all 200 costs 20 sockets about 115 ms of reads, one rebuild shared by all. The load test counts the rebuilds and their time, the steps a commit outran and read again (S43), and the resyncs at the ingest's peak, weighing its 1000-line batch against the bus of 1024 (S40); and it measures an event's latency at that peak and during a reconciler pass that writes many bots.

  > Note (P13.4, from Phase 11, group E) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S44, S45): measure a retention run at the ingest's peak: its batch times by phase, commits a second, WAL frames and checkpoints, the sockets' rebuilds and redone reads, the chat the ingest drops meanwhile, and the per-bot walk with 200 bots; retune the pause or the interval, and fall back to `secure_delete = FAST` if ON stalls the ingest.

  > Note (P13.4, from Phase 12, group B) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), S9): scrape both metrics endpoints during the load test, so a render under 200 bots and 20 sockets is measured too.
- [ ] **P13.5** Chaos tests:
  - kill and restart the server, agent and MC containers
  - partition the network between agent and server
  - fill up the DB volume

  Check that everything recovers and nothing is corrupted.

  > Note (P13.5, from Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): after a partition the server frees an agent within `[grpc] heartbeat_timeout_secs` (S23).

  > Note (P13.5, from Phase 10, group D) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): a partitioned agent's bots stay assigned to it and read `Unknown` (S32); after a server restart, nothing new is placed until every enabled agent is back or 90 s pass (S45).

  > Note (P13.5, from Phase 10, group F) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): a kill during a refresh is a crash: the account ends `active` or `reauth_required`, never corrupt, and a graceful stop never exits 4 because of Microsoft (S64); in a partition, disconnected bots rejoin with their kept grants while those last, and once the network heals every request goes to the server again (S63).

  > Note (P13.5, from Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): partitions (the agent notices a silent connection within its keepalive, S67) and kills of the server and the agent mid-batch (U16's crash case).
- [x] **P13.6** 🔴 Agent certificates renew automatically at 2/3 of their lifetime (`RenewCertificate` RPC). Warn when expiry gets close.

  > Note (P13.6, as built) ([ADR-0027](docs/adr/0027-certificate-renewal-and-phase-13-refinements.md)): **the renewal.**
  > - **The call** has no CSR: `RenewRequest` is empty, and the server signs the key of the certificate the call presents again, for the agent its row names. It passes `Connect`'s gate, limit and admission, needs no live stream, and answers the new certificate as PEM.
  > - **The old certificate stays valid** until the agent's first hello with a newer one, whose transaction revokes the older ones (`agent.connect`'s `certificates_revoked`); never a newer one, which the agent may hold already. A renewal asked again revokes the earlier answer. Audited as `agent.cert_renew`.
  > - **The agent** asks on its welcomed stream's connection once two thirds of its certificate's lifetime have passed, at once when it starts later, and every hour after a failure; checks the answer as `enroll` does; replaces `cert_file` in one step; swaps its identity and connects again. Its key stays. An attempt runs to its end, never raced against the stream. A server that doesn't know the call (`UNIMPLEMENTED`) leaves it running.
  > - **The warnings:** a certificate ends soon in its last 14 days, or its last quarter when it lives shorter. The server counts such agents (`afkfleet_agent_certificates`) and names each once a day (`warn`; `error` once expired); its own certificate's end is a gauge, and its last 30 days a `warn` at the start and once a day. The agent says its own once a day. The Agents screen's 14-day line says "It renews once connected on 0.1.2 or later.". The runbook gains "Agents' certificates".
  > - **Deviations:** `PROTOCOL_VERSION` stays 1 (a new call changes no message a peer reads). The server's warning is a line of its own once a day, not part of the housekeeping's minute line, which would repeat it 1440 times a day. x509-parser joins fleet-agent's dependencies. The version, 0.1.2, is this group's last commit, not a pull request of its own.

  > Note (P13.6, from Phase 10, group B, the user's note at the plan review) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the server certificate too.** It expires after 365 days, and then every agent fails to connect, so "warn when expiry gets close" covers it: a warning at startup and in the housekeeping line within 30 days of its `not_after`. Serve already logs an expired one at `error` (group B).

  > Note (P13.6, from Phase 10, group B) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): a renewal's answer carries the certificate as PEM, checked by the agent like `enroll`'s (R10): one section and nothing around it, a client certificate the pinned CA signed, for the agent's key.

  > Note (P13.6, from Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): `RenewCertificate` sits behind `Connect`'s gate and identity check, spends the same per-certificate bucket and answers `CERTIFICATE_REFUSED` alike (S24, S30). A stream ends at its certificate's `not_after`, so an agent renews before it; an expired certificate means enrolling again. The renewed certificate carries the same SAN and is found by its fingerprint, and revoking the old one shows a stale process as `revoked` refusals, under the same daily cap. Both certificates share the agent's one slot: to switch to the renewed one, the old stream ends first (S26).

  > Note (P13.6, from Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): the agent loads its identity once and never re-reads it, so a renewal swaps it under a running agent (a new endpoint and kept certificate), and adds the near-expiry warning S68 left out.
- [x] **P13.9** 🔴 **No password step-up but for the two credential changes** (the user's decision, 2026-11-11; group A2, in v0.1.2). Once logged in with password and 2FA, the app never asks for the password again: every `step_up_route` becomes an `authed_route`, and the app's password dialogs go. Roles and per-bot grants stay as they are.

  > Note (P13.9, as built) ([ADR-0028](docs/adr/0028-a-session-is-enough.md)): **what changed.**
  > - **The server:** twenty of the 22 step-up routes are authed routes, their handlers taking `AuthUser`. `POST /me/password` and `POST /me/mfa/reset` keep the step-up, and the mechanism (`POST /auth/step-up`, the 5-minute stamp, `SteppedUpUser`, `x-step-up`, `auth.step_up`) is untouched for them: both replace a login credential, so a hijacked session could lock its owner out for good and outlive its own end.
  > - **The twin routes** (`/command`, the modes' `/with-commands`) stay, without their step-up, and the plain routes still point there; the two codes keep their names, with messages that no longer speak of a password. Merging the twins is proposed, not done.
  > - **The clients:** fleet-client's and the bridge's docs and full-stack tests, the dev console (it never asks again after its login) and the integration harness.
  > - **The app:** no inline "Confirm it's you" but in the profile's two flows; `useStepUpAction` is `useWriteAction`; a command or a mode off the allowlist goes on to its twin within the same send or save; whatever asked a question first still does. A server older than the app reads "This server is older than the app and still asks for your password here. Ask its owner to update it.".
  > - **The records:** the threat model's rows and its accepted risk "A live session does everything its user may, without the password", CLAUDE.md's security rule 1, and notes on ADR-0016 to ADR-0020.
  > - **Deviations:** two routes keep asking, as the decision allowed. An Owner's or Admin's stolen session can now invite and register an account of the attacker's own; the ADR names it as the edge to revisit at P13.7 and P13.8.
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
- A web CSP, extra bot protection on the public login, and the existing Playwright suite reused (its specs drive the UI through its client; a web build adds an entry of its own).

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
- **An auto-stop timer per bot** *(the user's idea, recorded in Phase 11, group B2; after Phase 13)*:
  - **When:** a duration (hours and minutes), or a clock time in German local time (`Europe/Berlin`, summer and winter time from the time-zone rules, never a fixed UTC offset).
  - **What:** when it's due, the server sets the bot to Stopped as the Stop button does, audited with `via = timer`, and stored so it survives a restart.
  - **Edge times:** a clock time that doesn't exist (spring forward) fires at the next valid minute; one that exists twice (fall back) fires at its first occurrence.
  - **Needs:** a time-zone crate, a new dependency for the user's approval then.
- **A sell routine** *(the user's idea, recorded in Phase 11, group B2; after Phase 13)*, modelled on the FWM mod's sell macro (https://modrinth.com/mod/fwm-mod):
  - **The routine:** the user picks item types (e.g. `minecraft:slime_block`). The bot opens the server's sell menu with a configured command and keeps it open, and shift-clicks only stacks of those items from its inventory into it. It clicks the configured sell slot when the container is full, optionally also after a wait, and reopens the menu after a delay if it closes.
  - **A per-server profile:** the command, the expected raw menu title, its size and the sell slot; or a menu that sells when it's closed; or a server that sells by command only.
  - **Matching:** by item ID, optionally also by custom name or components.
  - **Safety:** a first recon step reports what the server's menu really sends. The bot stops and alerts when the menu doesn't match, and never clicks anything else.
  - **Stats:** optional, display-only sales stats from payout messages; chat never triggers an action.
  - **Built on** azalea's container support: the first GUI interaction the bots would do, so it lifts §1's non-goal "inventories or container GUIs" for this routine.

---

## 11. Appendix

### A. Example configuration
Secrets **never** go in these files. They're referenced through `*_file` paths (Docker secrets). Environment variables override any key: `AFKFLEET_SERVER__AUTH__ACCESS_TOKEN_TTL_SECS=600`, `AFKFLEET_AGENT__RUNTIME__MAX_BOTS=20`.

> Note (Appendix A, from Phase 6, group A, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): **`server.toml` grows with the phases.** Unknown keys are errors, and Phase 6 defines only `dev_mode`, `[http] bind`, `request_timeout_secs` and `max_body_bytes`, `[database] path` and `[log]`. Each later key arrives with its task: `trusted_proxies` (P7.10), `[auth]` (P7), `[rate_limits]` (P7.10), `[vault]` (P7.4, P9), `[accounts]` (P9), `[grpc]` (P10), `[chat]` (P11) and the `backup_*` keys (P12.3). The example below shows the full set.
> - `bind` defaults to `127.0.0.1:8080`, not the `0.0.0.0:8080` below, which is what a container sets.
> - `[database] path` is required and must be absolute.

> Note (Appendix A, Phase 7, group B, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **`[auth]`'s token keys** take the defaults below, within these ranges: `access_token_ttl_secs` 60–3600, `refresh_token_idle_days` 1–90, `refresh_token_absolute_days` 1–365, and the idle lifetime may not exceed the absolute one. They apply to existing sessions too: expiry is computed from each session's anchors with the running config. All three are on the secret-name test's allowlist.

> Note (Appendix A, Phase 7, group A, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
> - **`[auth]`'s argon2 keys** take the defaults below, within these ranges:
>   - `argon2_memory_kib`: 19456–1048576
>   - `argon2_iterations`: 2–10
>   - `argon2_parallelism`: 1–16
>   - `argon2_max_concurrent`: 1–64
>
>   Nothing can configure hashing below §7.2's baseline. The other `[auth]` keys arrive with groups B, C and E.
> - **`[vault]` is required.**
>   - **Its keys:** `master_key_files` maps 1–16 key IDs (1–4294967295) to absolute paths, and `active_key_id` must be one of them. `active_key_id` is on the secret-name test's allowlist: it holds an integer, not a key.
>   - **The files:** each holds standard padded base64 of exactly 32 bytes, owner-only on Unix.
>   - **Who reads them:** only `serve`.
>   - **The environment:** a key file can be given as `AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1=/path`.

> Note (Appendix A, Phase 7, group C, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
> - **`[http] trusted_proxies`** is empty by default and holds at most 32 CIDR networks, each its first address with a prefix length: a bare address, host bits, a duplicate, an IPv4-mapped IPv6 network or a prefix length of 0 is refused.
> - **`[auth] lockout_threshold`** takes 3–20.
> - **`[rate_limits]`** arrives with every key below but the chat ones and `ws_connections_per_user` (P11). The ranges are in P7.10's note; each is at least 1.
> - None of them needs a place on the secret-name test's allowlist.

> Note (Appendix A, Phase 8, group A, the user's decision) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)): **`[http] request_timeout_secs` is at most 19 s.** The desktop app's client gives up on a request after 20 s, so the server's request timeout must stay below that: a slower server answer could let a refresh commit after the client gave up, and the client's next refresh would then send the replaced token, which is reuse and ends the session. Validation refuses anything above 19.

`server.toml`
```toml
dev_mode = false

[http]
bind = "0.0.0.0:8080"
trusted_proxies = ["172.30.1.2/32"]     # the proxy's own address, set per proxy path by compose (ADR-0022, S16)
request_timeout_secs = 15                # 1–19: below the desktop client's 20 s
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

[metrics]                                # off unless set; internal only, never published or proxied
bind = "0.0.0.0:9480"                    # not port 0, nor [http]'s or [grpc]'s port

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
link_flows_total = 3                     # Microsoft link flows at once on the server
link_flows_per_user_per_day = 10         # counted from the audit log
grants_by_name_per_user_per_hour = 10    # grants created by username, per user
grant_candidates_per_user_per_hour = 60  # "Share with" suggestions read, per user, across their accounts
enroll_per_ip_per_min = 5                # Enroll calls on the gRPC port, per client IP
connect_per_cert_per_min = 10            # Connect calls on the gRPC port, per client certificate
bot_actions_per_bot_per_min = 5          # start, restart, reset, resume and server changes, per bot
fresh_sessions_per_bot_per_hour = 6      # fresh session requests on an agent's stream, per bot
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
retention_days = 14                      # 1–30
max_lines_per_bot = 50000                # each bot's newest lines kept; 100–200,000
command_allowlist = ["/spawn", "/home", "/afk"]   # usable with Control; every other /command needs Manage
                                                 # each entry: "/" + one command name, no arguments; at most 64; missing = empty

[audit]
retention_days = 365                     # 90–730: every entry holds a user's IP address
```

> Note (Appendix A, Phase 7, group E, the user's decision) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): `[auth] invite_ttl_hours` is 1 to 720 (30 days), 72 by default. An invite's expiry is stored when it's created, so a change doesn't move existing invites.

> Note (Appendix A, Phase 9, group D, the user's decisions) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **`[accounts]` arrives** with `max_per_member` (1–100, 3 by default), which counts every account a Member owns, and `allow_offline` (false by default; the dev config turns it on). `[rate_limits]` gains `link_flows_total` (1–16, 3) and `link_flows_per_user_per_day` (1–100, 10). None needs a place on the secret-name test's allowlist.

> Note (Appendix A, Phase 9, group E) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): `[rate_limits]` gains `grants_by_name_per_user_per_hour` (1–100, 10 by default, the user's approval at the plan review).

> Note (Appendix A, Phase 10, group B) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`[grpc]` as built** (P10.2's note): optional; without it the control plane is off. `heartbeat_interval_secs` and `heartbeat_timeout_secs` arrive with P10.4. `[rate_limits]` gains `enroll_per_ip_per_min` (1–60, 5 by default, S14). In `agent.toml`, `[control_plane] url` is `https://host:port` (no credentials, path or query), and its three files are absolute and distinct; the agent writes `cert_file` and `key_file` itself with `afkfleet-agent enroll`.

> Note (Appendix A, Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): `[grpc]` gains `heartbeat_interval_secs` (1–60, 10) and `heartbeat_timeout_secs` (2–300, 30); the timeout must be at least twice the interval, and an error names both keys. `[rate_limits]` gains `connect_per_cert_per_min` (1–60, 10).

> Note (Appendix A, Phase 10, group E) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): `[rate_limits]` gains `bot_actions_per_bot_per_min` (1–60, 5 by default, S52). **`[chat]` arrives** with `command_allowlist` alone (P2.9's rules: each entry `/` and one command name, no arguments; at most 64; a missing key means an empty list; an error names the entry's index, never its text); `retention_days` comes with P11.6 (R37). In `agent.toml`, `[control_plane] private_targets` (a CIDR list, empty by default) arrives with G (S46).

> Note (Appendix A, Phase 10, group F) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): `[rate_limits]` gains `fresh_sessions_per_bot_per_hour` (1–60, 6 by default, S58, the user's approval at the plan review).

> Note (Appendix A, Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`[control_plane] private_targets` as built** (S69): at most 32 IP networks in strict CIDR form (as `[http] trusted_proxies`), each inside one private range (loopback, RFC 1918, 100.64.0.0/10, fc00::/7), none a never-dialled address, none twice; each problem names its index, never its text. Empty by default, so a managed agent dials only public addresses; a parse error under `[standalone]`, which is never checked.

> Note (Appendix A, Phase 11, group A) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S3): `[rate_limits]` gains `chat_per_bot_interval_secs` (1–60, 3 by default) and `chat_per_bot_burst` (1–8, 3; U2), each bot's chat rate, which the server sends in its spec; an error names the key. `agent.toml` gets no chat key: a standalone agent's bots use the default.

> Note (Appendix A, Phase 11, group B1) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S3): `[rate_limits] chat_per_user_per_min` (1–60, 20 by default): the chat lines one user may send through the API in a minute, across bots.

> Note (Appendix A, Phase 11, group D1) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), U12, U13): `[rate_limits]` gains `ws_connections_per_user` (1–10, 3 by default). The socket's other bounds (U13) are constants, not settings.

> Note (Appendix A, Phase 11, group E) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), U19, U21, U22): `[chat]` gains `retention_days` (1–30, 14 by default) and `max_lines_per_bot` (100–200,000, 50,000), and a new `[audit]` section `retention_days` (90–730, 365); an error names the key. Unused invites and enrollment tokens are kept `[audit] retention_days` past their expiry (S46).

> Note (Appendix A, Phase 11, group H5) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), U50, R122): the agent's `[control_plane]` gains `public_targets` (a bool, `true` by default): with `false` a public address is refused like an unlisted private one (`blocked`), and the policy of a connector nobody configured refuses public addresses too. A parse error under `[standalone]`. `deploy/dod/agent.toml` sets it to `false`.

> Note (Appendix A, Phase 11, group L3) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), U110, S127): `[rate_limits]` gains `grant_candidates_per_user_per_hour` (1–600, 60 by default, all at once if need be; the user's approval at the plan review): how many times one user may read the suggestions behind "Share with" in an hour, one bucket across all their accounts.

> Note (Appendix A, Phase 12, group B) ([ADR-0022](docs/adr/0022-deployment-and-phase-12-refinements.md), S10): both configs gain `[metrics]` with one key, `bind`, off unless set: the address of `GET /metrics`, an IP address and a port other than 0; on the server not `[http] bind`'s or `[grpc] bind`'s port, whatever their addresses. In a container it's `0.0.0.0`, on the project's network only (P12.2).

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

[log]
format = "json"                          # or "pretty": one colored line per event, for development
filter = "info"                          # EnvFilter syntax; azalea stays at warn unless named, azalea_auth at info

[metrics]                                # off unless set; internal only, never published or proxied
bind = "0.0.0.0:9481"                    # an IP address and a port other than 0

[control_plane]                          # managed mode (production)
url = "https://fleet.example.com:7443"
ca_cert_file = "/run/secrets/fleet_ca_cert"
cert_file = "/data/agent.crt"
key_file = "/data/agent.key"
private_targets = []                     # private networks the bots may dial (at most 32, each inside a private range)
public_targets = true                    # whether the bots may dial public addresses

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
| `POST /me/mfa/enroll` · `/confirm` · `/reset` | setup token (enroll, confirm); user + step-up (reset) | 2FA: enroll answers a new secret, confirm stores it with 10 recovery codes; reset answers a setup token whose confirm replaces the authenticator and revokes the user's other sessions (group D) |
| `GET /me/sessions` · `DELETE /me/sessions/{id}` | user | Session management |
| `GET /users` · `PATCH /users/{id}` | Admin+ | Role, disable |
| `GET /users/{id}/grants` · `GET /users/{id}/shared-accounts` | Owner | The grants review at a role change: the grants the user holds (dormant ones marked), and their accounts a grant shares (P11.9) |
| `POST /invites` · `GET /invites` · `DELETE /invites/{id}` | Admin+ | Invites |
| `POST /accounts/link` · `GET /accounts/link/{flow}` | Member+ | Microsoft device-code flow |
| `GET /accounts` · `GET /accounts/awaiting-approval` · `GET/DELETE /accounts/{id}` | View / Manage | MC accounts; the pending links the caller may review |
| `POST /accounts/{id}/approve` · `/refuse` | above the owner | Review a pending link |
| `POST /accounts/{id}/relink` | Manage | Link the account again (a link flow) |
| `GET/POST /accounts/{id}/grants` · `PUT/DELETE /accounts/{id}/grants/{user}` | Manage | Sharing |
| `DELETE /accounts/{id}/grants/me` | a grantee without implicit Manage | Leave a shared account (P11.9) |
| `GET /accounts/{id}/grant-candidates?q=` | Manage | "Share with"'s suggestions: at most 8 enabled users' names that start with `q` and could be given a grant (P11.11, ADR-0020, U110, S127) |
| `GET /bots` · `GET/PATCH /bots/{id}` · `PUT /bots/{id}/mode` | View / Manage / Control | Bots and their state; the server address; the mode |
| `POST /bots/{id}/start\|stop\|restart\|reset\|resume` | Control | Lifecycle |
| `POST /bots/{id}/chat` · `POST /bots/{id}/command` · `GET /bots/{id}/chat` | Control (a command off the allowlist: Manage, and only through `/command`) / View | Chat |
| `GET/POST /modes` · `PUT/DELETE /modes/{id}` | Member+ (own) / Admin+ (shared) (`POST`, `PUT` with a command off the allowlist: their `/with-commands` twins) | Modes |
| `GET/POST /server-settings` · `PUT/DELETE /server-settings/{id}` | Admin+ | Each Minecraft server's settings: its conflict texts and chat formats (P11.9) |
| `POST /server-settings/chat-format-preview` | Admin+ | Which sender unsaved chat formats read from a line; nothing stored (P11.9) |
| `GET /agents` · `POST /agents/{id}/disable` · `POST /agents/{id}/release` | Admin+ | Agents; a disabled agent's bots released (P11.9). *Enrollment tokens have no route since Phase 11's group J ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), U76).* |
| `GET /audit` | Admin+ | Audit log (cursor-paginated), optionally narrowed to one target by `target_kind` and `target_id` (P11.9) |
| `POST /events/ticket` · `GET /events?ticket=` | user | WebSocket live events |
| `GET /health/live` · `GET /health/ready` | public (no prefix) | Health |
| `GET /api/openapi.json` | public, **only in dev mode** (no `/api/v1` prefix) | The OpenAPI document (P6.10); outside dev mode the path answers like any unknown route |

> Note (P2.9): `authorize()` refines these levels (ADR-0010):
> - `POST /agents/enrollment-tokens` is Owner-only. *Removed at Phase 11's group J ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), U76).*
> - `PATCH /users/{id}`: only the Owner changes roles; Admins disable and enable Members.
> - `DELETE /invites/{id}`: an Admin invite is the Owner's.
> - `PATCH /bots/{id}`: the server needs Manage; the mode (`PUT /bots/{id}/mode` since Phase 10, group E) Control, and a mode with a command off the allowlist Manage.
> - The `user` routes check `Personal`.

> Note (Appendix B, Phase 13, P13.9, the user's decision) ([ADR-0028](docs/adr/0028-a-session-is-enough.md)): **the table's "(step-up)" marks** are down to two, `POST /me/password` and `POST /me/mfa/reset`. The twenty other routes that carried one are plain authed routes; the notes below describe them as built, and wherever one says "step-up" for another route, this note replaces it. `command_needs_step_up` and `mode_needs_step_up` keep their names and still point to `/command` and the `/with-commands` twins, which take the request without a step-up.

> Note (Appendix B, Phase 7, the user's decision) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **`POST /auth/step-up`** (user) joins the table in group D. It takes `{password}` and stamps the session's `stepped_up_at`; the "(step-up)" routes need that stamp to be at most 5 min old, otherwise they answer 403 `step_up_required`.

> Note (Appendix B, Phase 9, group D, the user's decisions) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **`POST /accounts/link`** (every role) answers 201 `{flow_id, user_code, verification_uri, expires_at}`, the only time the code is shown. **`GET /accounts/link/{flow_id}`** is the flow's creator's alone (404 for everyone else) and answers `{status}` with each state's fields. P9.4's notes list both routes' answers. **`POST /accounts/offline {name}`** (every role, while `[accounts] allow_offline` is on; 404 otherwise) answers 201 `{id, name, kind, status}` (P9.7).

> Note (Appendix B, Phase 10, group B) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): *removed at Phase 11's group J ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), U76).* **`POST /agents/enrollment-tokens`** (the Owner, step-up, no body) answers 201 `{id, bundle, ca_fingerprint, expires_at}`, the only time the bundle is shown; 404 while `[grpc]` is off, 409 at 20 pending tokens (P10.3's note).

> Note (Appendix B, Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`GET /agents`** (Admin+) pages every agent with its status (`online`, `offline`, `expired`, `disabled`), and **`POST /agents/{id}/disable`** (Admin+, step-up, no body) answers 200 with the agent, disabled for good: every certificate revoked and its stream ended (P10.10's note).

> Note (Appendix B, Phase 10, group E) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the bot routes as built** (P11.2's note). `GET /bots` (keyset pages) and `GET /bots/{id}` answer `BotResponse`; `PATCH /bots/{id} {server_address}` (Manage, step-up) and `PUT /bots/{id}/mode {mode_id}` (Control) answer 200 with the bot. `start` and `stop` answer 200; `restart`, `reset` and `resume` 202 when sent to the bot's agent, 200 when a bot that runs nowhere had its state cleared, and otherwise 409 with `bot_unreachable`, `bot_not_running`, `bot_not_paused` or `bot_not_failed`. Only `PATCH` and `PUT` have a body. The answers follow the extractors' order: 401, the per-user 429, 403 `step_up_required`, a malformed path's 404, the body's 400/415/422, an unseen bot's 404, 403, the per-bot 429, 409 (R38).

> Note (Appendix B, Phase 11, group B1) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S4–S8): **the chat routes as built.** `POST /bots/{id}/chat {message}` (Control) sends plain chat or an allowlisted command; a command off the allowlist is never sent there: a Manage holder gets 403 `command_needs_step_up`, anyone else 403 `forbidden`. `POST /bots/{id}/command {message}` (step-up) sends a command, Control for an allowlisted one and Manage for any other; a line that isn't a command is 422. Both answer `ChatSendResponse {request_id, message_id, outcome}`: 200 `sent` when the bot's agent said the bot sent it, 202 `pending` when no answer came within 6 s (or the request timeout less 2 s; *counted from the request's arrival since PR #60's review*); otherwise 409 `bot_not_online`, `chat_unavailable` or `bot_unreachable`, or 429. The order: 401, the per-user 429, 403 `step_up_required`, a malformed path's 404, the body's 400/415/422, an unseen bot's 404, 403 `forbidden`, the pointer, the chat 429s, then the send's answer. `GET /bots/{id}/chat` comes with group B2.

> Note (Appendix B, Phase 11, group C1) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S12–S16, S19): **`GET /modes`** (authed) answers `ModePage {items, next}` under the list rule, each `ModeResponse {id, name, visibility, ownership, owner?, editable, deletable, visibility_editable, version, created_at, updated_at, steps}`, `owner` for Admins and the Owner only and a command's arguments only for a viewer who may edit the mode. **`POST /modes {name, visibility, steps}`** (authed) answers 201 with the mode; steps that send a command off the allowlist are 403 `mode_needs_step_up`, which **`POST /modes/with-commands`** (step-up) takes. The order: 401, the per-user 429, (the twin) 403 `step_up_required`, the body's 400/415/422, 403 `forbidden`, the pointer, then in the transaction the cap's 409 `conflict` and a taken name's 422.

> Note (Appendix B, Phase 11, group C2) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S16–S18, S21, R2, R3): **`PUT /modes/{id} {version, name, visibility, steps}`** (authed) answers 200 with the mode as it now is: at the next version, or unchanged when the edit shows the same as the stored mode (S21); steps that send a command off the allowlist, new, changed or kept, are 403 `mode_needs_step_up`, which **`PUT /modes/{id}/with-commands`** (step-up) takes. The order: 401, the per-user 429, (the twin) 403 `step_up_required`, a malformed path's 404, the body's 400/415/422 (`version` at least 1), an unseen mode's 404, 403 `forbidden` (`UpdateMode` with the mode's visibility, then with the requested one; a built-in mode for everyone), the pointer; then in the transaction the re-reads and `authorize()`, a no-op's 200, 409 `mode_changed` (a stale `version`), a taken name's 422, 409 `mode_in_use_by_others` (a bot that uses the mode, on which the caller couldn't set the edited one, S17), the write. **`DELETE /modes/{id}`** (step-up) answers 204: 401, the per-user 429, 403 `step_up_required`, a malformed path's 404, an unseen mode's 404, 403 `forbidden` (`DeleteMode`; a built-in mode for everyone), then in the transaction the re-reads and `authorize()` and 409 `mode_in_use` (any bot, whoever's).

> Note (Appendix B, Phase 11, group B2) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S6, S9, S11, U3): **`GET /bots/{id}/chat`** (View) answers `ChatPage {items, next}`: lines newest first, each `{id, at, direction, kind, text, truncated, arguments_hidden, sender?, parsed_sender?, sent_by?, mine}` (`parsed_sender` since H8: the name a chat format of the bot's server reads from a received system line, ADR-0020, S79), with `?before=<id>&limit=<1..100>` (50 by default). A command's arguments only for Manage holders, a user's line's sender only for viewers who see the account's owner; 400 for a query that doesn't parse, 422 for a value out of range, 404 for an unseen bot.

> Note (Appendix B, Phase 11, group D1) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S28, S29, S36, R10, R17): **the live events routes as built.** `POST /events/ticket` (authed, no body; `UsePersonal`) answers 200 `{ticket, expires_in}`: a ticket for one socket, bound to the session that asked, for 30 s. `GET /events?ticket=` (authed, with the same session's bearer token) upgrades to the WebSocket. Its answers, in order: 401, the per-user 429, 503 `busy` once the shutdown began, the upgrade's 405 or 400 (the ticket unspent), the query's 400, then the ticket taken: anything that isn't a live ticket of this session is one 404 `not_found`, as is a session that no longer counts; a store failure, or the shutdown meanwhile, is 503, the cap 429 `too_many_connections` (the shutdown's 503s and the order since PR #66's review). Neither route has a 422 (R17). The frames are `fleet_api_types::events::EventMessage`, in the `OpenAPI` document (the 101's `x-websocket-message`) and the module's docs.

> Note (Appendix B, Phase 9, group E) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **the account routes as built.** P9.6's notes list them. `POST /accounts/{id}/grants {username, level}` creates a grant by exact username (Members can't list users), and `PUT /accounts/{id}/grants/{user} {level}` changes one by user ID. Approving answers 200 with the account; refusing, deleting and revoking 204. `POST /accounts/{id}/relink` answers like `POST /accounts/link`, read through `GET /accounts/link/{flow_id}`.

> Note (Appendix B, Phase 7, group E, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
> - **Lists** (`GET /users`, `GET /invites`, `GET /me/sessions`) answer keyset pages: `?before=<id>&limit=<1..100>` (50 by default), newest first by ID, `{items, next}` with `next` left out on the last page. A query that doesn't parse is 400, a `limit` out of range 422.
> - **A path ID** that isn't a version 7 UUID is 404, like an unknown one.
> - **`POST /auth/register`** answers 201 `{id, username, role}`, never tokens. **`POST /invites`** answers 201 `{code, invite}`, the only time the code is shown. **`PATCH /users/{id}`** takes exactly one of `role` and `disabled`. **`DELETE /me/sessions/{id}`** is 409 for the request's own session.
> - **Times** in responses are `YYYY-MM-DDTHH:MM:SS.mmmZ` (UTC, milliseconds); a value that isn't there is left out, never `null`.

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

> Note (Appendix C, Phase 10, group A) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the protocol as built** (`crates/fleet-proto/proto/fleet/v1/`, pinned by the wire-schema snapshot) keeps the envelopes' cases and numbers above, and refines the sketch (decided by the review agents, approved by the user at the plan review):
> - **No `RenewCertificate` yet** (S1): it comes with P13.6, as a new RPC that breaks no agent. **`AgentEnrollment`** joins in P10.3, its first user.
> - **Times** are `optional int64` milliseconds since the Unix epoch (`*_unix_ms`), **IDs** 16 bytes, and there are no well-known types (S5). A heartbeat carries no time.
> - **`Hello`** is `name, agent_version, protocol_version, capacity, running` (snapshots), without labels (S8). **`BotStatus`** is a `BotSnapshot`. **`BotEvent`** has the bot, its time and one of `died`, `chat_received`, `chat_sent`, `chat_failed`, `mode_chat_sent`, `alert` and `removed`.
> - **Correlation** (S3): `SendChat` carries a `ChatRequestId` that `chat_sent` and `chat_failed` repeat; `SessionRequest` carries the agent's own number, which `SessionGrant` and `SessionDenied` repeat with the bot, and names no account.
> - **`AssignBot`** is the spec and the sticky state to restore; `ReconcileFull` a list of them. **`SessionGrant`** holds an `OnlineSession` (name, UUID, token, expiry) or an `OfflineSession` (name). **`Welcome`** carries only the heartbeat interval; P10.4 may refine it and `Reject`'s reasons.
> - **A bot's mode** is structured messages that mirror its steps (S4); no JSON crosses the wire.
> - **Restart, reset and resume** get a message with P11.2, in Phase 10's group E.

> Note (Appendix C, Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`Welcome`** carries the heartbeat interval and the agent's ID (`bytes agent_id = 2`), no server version. **`RejectReason`**: 1 `IncompatibleProtocol`, 2 `AgentDisabled`, 3 `NameMismatch`, 4 `InvalidHello`, 5 `AlreadyConnected`, 6 `CertificateRevoked`, 7 `CertificateExpired` (R14). A `Reject` answers a hello the server refuses, or is the last message of a stream whose permission ended; the stream then ends OK. An identity failure before the stream opens is one status, never a `Reject` (S24). The numbers freeze at the first release.

> Note (Appendix C, Phase 10, group D) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **a `ReconcileFull` may carry a spec with desired Stopped:** a bot the server is stopping before it unassigns it (S33, S42). Every welcome gets exactly one `ReconcileFull`, an empty one too. `fleet_proto::control::RECONNECT_DELAY_MAX` (30 s) bounds an agent's reconnect backoff, a constant only; the wire schema didn't change.

> Note (Appendix C, Phase 10, group E) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`ServerMessage` gains case 10,** `LifecycleCommand lifecycle = 10`: `LifecycleCommand { bytes bot_id = 1; Lifecycle command = 2; }` and `enum Lifecycle { LIFECYCLE_UNSPECIFIED = 0; LIFECYCLE_RESTART = 1; LIFECYCLE_RESET = 2; LIFECYCLE_RESUME = 3; }`. It's one-shot and never answered, and has no correlation ID: its effect comes back as the bot's `Status` (S49). Reading refuses a non-v7 or wrong-length ID, `UNSPECIFIED` and unknown values. The wire-schema snapshot shows it; `PROTOCOL_VERSION` stays 1 until the first release (S9).

> Note (Appendix C, Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`ConnectFailure` gains `CONNECT_FAILURE_BLOCKED = 6`** (S69): the agent refused the resolved address before dialling it. `fleet_proto::control` gains `KEEPALIVE_INTERVAL` (10 s) and `KEEPALIVE_TIMEOUT` (20 s), the agent's HTTP/2 keepalive, which the server's placement grace counts on (S45, S67); `WELCOME_TIMEOUT` bounds the agent's whole attempt, dialing included. `PROTOCOL_VERSION` stays 1 (S9).

> Note (Appendix C, Phase 11, group A) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S1, S3): **`BotState.Backoff` gains `bool fresh = 2`**: the next attempt asks for a fresh session (U24). **`BotSpec` gains `ChatRate chat_rate = 7`** (`ChatRate { uint32 interval_secs = 1; uint32 burst = 2; }`): the bot's chat rate, the server's `[rate_limits]` setting. Reading refuses a missing rate, a 0 as missing and a value past its range (60 s, 8 messages) as out of range, never clamped. The wire-schema snapshot shows both; `PROTOCOL_VERSION` stays 1 until the first release (S9).

> Note (Appendix C, Phase 13, group A) ([ADR-0027](docs/adr/0027-certificate-renewal-and-phase-13-refinements.md)): **`AgentControl` gains `RenewCertificate(RenewRequest {}) returns (RenewResponse {certificate_pem = 1, optional int64 not_after_unix_ms = 2})`,** as the sketch above had it; the request has no field. `fleet_proto::control` gains `RenewResponse`, `RENEW_SERVER_DEADLINE` (15 s) and `RENEW_CLIENT_DEADLINE` (20 s). The wire-schema snapshot shows it; `PROTOCOL_VERSION` stays 1.

> Note (Appendix C, Phase 10, group B) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`AgentEnrollment` as built** (`enrollment.proto`): `Enroll(EnrollRequest {token, name, csr_der}) returns (EnrollResponse {agent_id, certificate_pem, not_after_unix_ms})`. The answer carries no CA certificate: a trust anchor can't come from the server it vouches for, so the agent pins the bundle's fingerprint instead. The certificate is PEM text (R10), the CSR DER: each end gets what its reader takes. `EnrollRequest` is in `REDACTED` (S7).

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
| `bots` | id, account_id (unique), server_address, mode_id, desired_state, agent_id, last_state, last_state_at |
| `modes` | id, owner_user_id (NULL = built-in), name, visibility, definition_json, version, updated_at |
| `agents` | id, name, enabled, labels, capacity, version, last_seen_at |
| `agent_certs` | fingerprint_sha256, agent_id, not_after, revoked_at |
| `enrollment_tokens` | token_hash, created_by, expires_at, used_at, agent_id |
| `chat_messages` | id, bot_id, direction, kind, sender, text, at |
| `audit_log` | id, at, actor_user_id, actor_ip, action, target_type, target_id, outcome, metadata_json |

> Note (Appendix D, from Phase 6, group B, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): **How every table stores its values.** STRICT tables. Entity IDs are lowercase hyphenated version 7 UUID TEXT with a CHECK on the exact form; append-only logs (`audit_log`, `chat_messages`) use `INTEGER PRIMARY KEY AUTOINCREMENT` instead, ordered by ID. Times are `YYYY-MM-DDTHH:MM:SS.mmmZ` TEXT (UTC, milliseconds) with a CHECK on the shape. CHECKs pin only shapes and closed sets that never grow.

> Note (Appendix D, Phase 7, group B, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **`sessions` as built** has no `family_id`, `access_expires_at`, `refresh_expires_at`, `ip` or `user_agent`: a row is one family, expiry is computed from `created_at` and `refreshed_at`, and no client data is stored. It adds `refreshed_at`, `last_used_at`, `stepped_up_at` and `created_at`, and the replaced refresh tokens live in `rotated_refresh_tokens(hash, session_id, rotated_at)`.

> Note (Appendix D, Phase 7, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): Phase 7's tables arrive with their first user, each in its own migration: `sessions` in group B, with an extra `stepped_up_at` for step-up (§7.2); `login_attempts` in C; `user_mfa` in D, whose `secret_ciphertext` holds a vault envelope v1; `invites` in E. Each group designs the columns with its feature.

> Note (Appendix D, Phase 7, group D, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **`user_mfa` as built** is `user_id, secret_envelope, last_used_step, confirmed_at`: no `key_id` (the envelope's header has it) and no `enabled_at` (`confirmed_at`), and a row exists only for a confirmed enrollment. The recovery codes live in their own table, `recovery_codes(hash, user_id, used_at)`, instead of a `recovery_code_hashes` column. The login challenges (mfa and setup tokens) live in the server's memory, not in a table.

> Note (Appendix D, Phase 9, group A, the user's decisions) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **`mc_accounts` as built** (`0008_mc_accounts.sql`) is `id, owner_user_id, kind, mc_uuid, mc_name, refresh_envelope, status, created_at`.
> - **No `key_id`:** the envelope's header names its key.
> - **Offline accounts:** `refresh_envelope` (a vault envelope, purpose `ms_refresh`) and `mc_uuid` are NULL exactly for them. An offline account is always `active`.
> - **Uniqueness:** a Microsoft account is unique by `mc_uuid`, an offline account by `lower(mc_name)`.
> - **No CHECK** on `status`'s values or on the name's shape.
> - **`account_grants`** arrives with group E.

> Note (Appendix D, Phase 9, group E) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **`account_grants` as built** (`0010_account_grants.sql`) has the columns above, `PRIMARY KEY (account_id, user_id)`, `ON DELETE CASCADE` from `mc_accounts`, a CHECK on `level` (`view`, `control`, `manage`) and one against a grant to its own granter, and an index on `user_id`.

> Note (Appendix D, Phase 9, group C, the user's decisions) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **`0009_mc_accounts_status_reason.sql`** adds `status_reason TEXT`, set only with the new status `refused`: `not_owned`, `no_profile`, `xbox_banned`, `xbox_no_xbox_profile`, `xbox_country_unavailable`, `xbox_adult_verification`, `xbox_child_account`, `xbox_other` or `profile_mismatch`. No CHECK; fleet-core's `AccountStatus::from_columns` reads both columns together.

> Note (Appendix D, from Phase 10's planning) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`agents` has no `labels`** (S8): nothing in v1 schedules by them. `agents.capacity` holds the last capacity an agent reported, NULL until its first hello, with no range CHECK (ADR-0015's rule), read back through `AgentCapacity`. **`agent_certs.agent_id`** is indexed but not unique, so P13.6's renewal can hold two certificates per agent (S1). **`chat_messages`** gains a `truncated` column (S6). The tables arrive with their first users in Phase 10's groups: `agents`, `agent_certs` and `enrollment_tokens` in B, `bots` and `modes` in D, `chat_messages` in G.

> Note (Appendix D, Phase 10, group B) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`enrollment_tokens` as built** (`0012_enrollment_tokens.sql`) is `id, token_hash (unique), created_by (NULL for the server host's tokens, R1), created_at, expires_at, used_at, agent_id (unique)`, with CHECKs that `used_at` and `agent_id` are set together and that a token expires after it's created, and an index on `expires_at`. A used token keeps its row. `agents` and `agent_certs` arrived with P10.2 (`0011`; its note).

> Note (Appendix D, Phase 10, group D) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`modes`, `bots` and `releasing_bots` as built.**
> - **`modes`** (`0013`): `id, owner_user_id, name, visibility, definition_json, version, created_at, updated_at`; `visibility` is `private` or `shared`, a built-in (no owner) is `shared`, `definition_json` is a JSON object, `version` from 1, `updated_at` not before `created_at`; indexed by owner. Seeds `afk` and `farm` (S38).
> - **`bots`** (`0014`): `id, account_id (unique, ON DELETE CASCADE), server_address, mode_id, desired_state, agent_id, last_state, last_state_reason, last_state_at`; no `auto_start` (S34; never, U12 in group E) and no `created_at`; `desired_state` is `running` or `stopped`; `last_state` and `last_state_at` set together, `last_state_reason` only with them; indexed by mode and agent.
> - **`releasing_bots`** (`0015`, S39, S43, R30): `bot_id, agent_id, account_id, kind, mc_uuid, mc_name, created_at`, keyed by `(bot_id, agent_id)`, no foreign key to the bot or the account (either may be gone), indexed by agent: an agent that may still run a bot whose row no longer says so, with the Minecraft account it plays.

> Note (Appendix D, Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`chat_messages` as built** (`0016`): `id, bot_id, direction, kind, sender_name, sender_uuid, text, truncated, at`. Appendix D's `sender` is two columns, the name and the UUID a player message comes with; `at` is the server's time of receipt (S5). P11.1's note lists the rules.

> Note (Appendix D, Phase 11, group B1) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S6, S7, U3): **`chat_messages.sent_by`** (`0017_chat_sent_by.sql`): the afkfleet user who sent a line through the API, on `out` rows of the new kind `user` only (a CHECK), `REFERENCES users ON DELETE SET NULL`, so a deleted user's lines keep no sender; a partial index on the lines that have one. A user's line is appended in the send's audited transaction and never changes afterwards (S7). Reading refuses a `user` row whose text the constructor would change, and a sender that isn't a version 7 ID, as `Corrupt`.

> Note (Appendix D, Phase 11, group H4) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S74, S76, S78): **`server_settings` as built** (`0019_server_settings.sql`): `id`, `server` (the ServerKey's text, the host and an always-written port, unique and lowercase), `conflict_texts` (a JSON array, read back only exactly as the set writes it), `version`, `created_at`, `updated_at`. At most 100 rows, which a create counts; only a delete ends one. The fleet read takes every row in its snapshot: a row whose texts don't read back leaves its bots alone, one whose server doesn't fails the read. *As built in H7* (`0020_chat_formats.sql`, R139): `chat_formats`, a JSON array by CHECK, `'[]'` by default, the formats as written, in order, duplicates kept, read back only exactly so; the fleet read never reads it.

> Note (Appendix D, Phase 11, group B2, the user's choice, U5) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)): **`chat_messages` is in commit order, not quite receipt order.** With one write connection, ID order is commit order (ADR-0015). Since P11.3 a user's line commits at once, in its send's transaction, while received chat commits with the ingest's next batch (every 500 ms, later while the database is busy). So a user's line can get a lower ID than a message the server received a moment before it, and neighbouring rows' `at` can disagree with their ID order by about one batch. `0016_chat_messages.sql` says "the order the server received the messages in"; it stays as it is, since editing a migration on main changes its checksum. The code's docs and this note say it instead. The history (P11.3) pages by ID; the app's console (P11.9) keeps that order and never sorts by `at`.

> Note (Appendix D, Phase 7, group E, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **`invites` as built** is `id, code_hash, role, created_by, created_at, expires_at, used_by, used_at, revoked_at`: `role` is `member` or `admin` only, `used_by` is unique, a revoked or used invite keeps its row, and three CHECKs keep it consistent (a use and its time together, never both used and revoked, an expiry after the creation). `0007_audit_log_target.sql` indexes the audit log by target, action and ID, for `GET /me`'s count of wrong codes.

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

> Note (Appendix E, Phase 11, group A) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), U24, S1): **`Backoff{attempt, fresh}`.** A backoff entered from a fresh `AwaitingSession` stays fresh, and `RetryDue` asks as it says, until a session arrives: a refused login's requests never ask without `fresh`. Every other backoff is non-fresh. The list of states above stays the original sketch, without any state's fields (`AwaitingSession{attempt}` has no `fresh` there either), as the P2.6 note below says; the built fields are in P2.6's notes and ADR-0010, `Backoff`'s among them.

> Note (P2.6): The built state machine (`fleet_core::bot`) refines this chart. The changes are listed in ADR-0010:
> - state fields, the `CrashLoop` event and `Stopping{restart}`
> - sticky `Paused` and `Failed`
> - the breaker effects `RecordFailure`, `RecordSuccess` and `ResetBreaker`
> - no "circuit open too long" arrow
