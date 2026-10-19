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
│   ├── fleet-startup/         # the binaries' start-up code: config loader and errors, [log], log layer, panic hook, stop signals, secret-file loader
│   ├── fleet-testkit/         # fakes, fixtures, builders (dev-dependency only)
│   └── fleet-integration/     # tests only: the real server with the real managed agent (P10.9, the DoD check, P11.7)
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

Binaries stay thin: `main.rs` parses the CLI and config and wires adapters together, and all logic lives in the crate's library part.

> Note (§4, Phase 6, the user's decision) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): **`fleet-startup`** holds only the binaries' process start-up code: the config loader with its key-path errors, the `[log]` types, the log layer and the panic hook. It came out of fleet-agent in Phase 6, group A, so the server reuses it instead of a second copy. Its filter always caps `azalea_auth` at `info` and lets panic reports through; a binary only adds rules (the agent's azalea rules). **Group E moved the stop signals there too** (the user's decision): `signals::{Signal, ShutdownSignals, OsSignals}` (SIGTERM and SIGINT, or Ctrl+C and Ctrl+Break), so both binaries share one implementation, and the crate's scope is now the binaries' process start-up and lifecycle.

> Note (§4, Phase 7, group A, the user's decision) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **`fleet_startup::secret_file`** reads every `*_file` secret of both binaries with one set of rules (regular file, owner-only mode on Unix, at most 64 KiB, one trailing newline removed; errors never show the content): the server's vault keys from P7.4, its PKI keys and the agent's `key_file` in P10. fleet-startup gains secrecy and zeroize for it.

## 5. Crate & tool registry

**Rule: one crate per concern, used throughout the whole project.** A concern that isn't covered here needs the user's approval, an ADR, and an entry in this table and in the index in `CLAUDE.md`. All Rust crates are declared once in `[workspace.dependencies]`, when the first crate uses them (see the P0.3 note). Versions were pinned in P0.3 (2026-10-05) after checking crates.io and docs.rs. They're caret requirements, and `Cargo.lock` holds the exact pin (ADR-0009). **dfo** means `default-features = false` at workspace level.

### Rust: runtime, domain & infrastructure
| Concern | Crate | Version | Used in | Notes |
|---|---|---|---|---|
| Minecraft protocol & client | `azalea` | `=0.16.0` (+mc26.1) | fleet-mc | Needs nightly (ADR-0003). Disable its `AutoReconnectPlugin` and `AutoRespawnPlugin`. Runs only inside a `LocalSet` |
| Minecraft chat components & translations | `azalea-chat`, `azalea-language` | `=0.16.0` (+mc26.1) | fleet-mc | Already in azalea's graph at the same pin. Only for fleet-mc's bounded renderer of server text: it names `PrimitiveOrComponent`, which azalea doesn't re-export, and looks up translation templates, because azalea's own rendering grows exponentially (ADR-0011). Bumped together with azalea |
| Microsoft / Minecraft auth | `azalea-auth` | `=0.16.0` (+mc26.1) | none directly: only through azalea's crates | **Not used by fleet-server** (P9.3, the user's decision): azalea-auth checks no status, polls in a loop that drops every refusal, can panic on an unexpected answer and prints tokens in `Debug`, so fleet-server's adapter sends its own requests with azalea-auth 0.16.0's protocol (client ID, scope, fields, headers). `deny.toml` lets it in only through `azalea`, `azalea-client`, `azalea-inventory` and `azalea-protocol` ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)). fleet-mc reaches only `azalea::auth::sessionserver` and `certs`, through azalea's re-export, never the Microsoft flows; its clippy config bans them (ADR-0011) |
| Async runtime | `tokio` | 1.53.2 | runtime, mc, testkit, agent, server, client | `test-util` feature in dev. Clippy bans `unbounded_channel`; azalea's two mandated channels are the only exceptions (ADR-0011). The agent builds a multi-threaded runtime after loading its config (ADR-0014). fleet-server enables `fs`, to create the database file and check its permissions, `time`, for the trace layer's latency from tokio's clock, and from P6.11 `rt`, `rt-multi-thread`, `macros`, `net` and `io-util`, for its runtimes, the listener and the healthcheck's TCP probe; fleet-startup enables `signal` and `macros` for the stop signals (ADR-0015) |
| Cancellation, task tracking | `tokio-util` | 0.7.19 | runtime, mc, agent, server | `CancellationToken`, `TaskTracker`. fleet-runtime and the agent enable no features: they use only `CancellationToken`, and their tasks live in tokio's `JoinSet`, which reports panics (ADR-0013, ADR-0014). fleet-server enables `rt` only, since P9.5: its token asks axum's server for the graceful shutdown (P6.12, ADR-0015), and the session-token service's refreshes run on a `TaskTracker` ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)) |
| Stream adapters | `tokio-stream` | 0.1.19, dfo | proto, agent, server | gRPC streams, broadcast → stream. Without default features (`time`). fleet-server (P10.4): each `Connect` answers from a `ReceiverStream` over its bounded queue, and the listener maps its accepted sockets with `StreamExt`. fleet-agent (P10.7, the user's choice U18): the outbox's `Connect` request body is a `Stream`, and the agent reads the server's answers with `StreamExt`. Already in Cargo.lock through tonic and sqlx ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)) |
| Sink/Stream extension traits | `futures-util` | 0.3.34 | client, server | WebSocket split/send |
| Async fns in `dyn` traits | `async-trait` | 0.1.92 | server | Only for `Arc<dyn Port>`. Use generics + RPITIT elsewhere. First used by fleet-server's store ports (ADR-0015) |
| Retry & backoff | `backon` | 1.6.0, dfo | core (policy), agent, client | Exponential backoff with jitter. The default features pull in a tokio sleeper. The agent's reconnect waits come from fleet-core's `RetryPolicy::seeded_delay` (P10.7, S67), so backon's tokio sleeper isn't added |
| Rate limiting (keyed / in-process) | `governor` | 0.10.4, dfo | runtime (chat), server (HTTP: per IP, per route class, per user) | fleet-runtime and fleet-server enable `std` only. The defaults add `quanta`, `dashmap` and `jitter`, which pulls rand 0.9 and getrandom 0.3 into normal dependencies. Without `quanta` there's no `DefaultClock`: the chat queue uses `direct_with_clock` with a tokio-backed clock, so paused time controls it (ADR-0013); the server's keyed `hashmap_with_clock` limiters run on its `Clock` port, a monotonic clock in production and `ManualClock` in tests, behind its own axum middleware (ADR-0016) |
| TTL cache / single-flight | `moka` | 0.12.16 | server | WS tickets (P11). Not the MC token cache: P9.5's single-flight is a per-account tokio `Mutex`, since moka's coalescing lets a waiter re-run the init when the evaluating caller is dropped, a second refresh with a spent token ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)) |
| Library errors | `thiserror` | 2.0.21 | all libraries | |
| Binary error reporting | `anyhow` | 1.0.104 | `main.rs` only | Not declared yet: the agent's and the server's `main.rs` map their typed errors to exit codes themselves (the user's decision for the server, P6.11). It arrives with the first `main.rs` that uses it (ADR-0014, ADR-0015) |
| Serialization | `serde`, `serde_json` | 1.0.229, 1.0.151 | all | fleet-testkit's log buffer parses JSON log lines with `serde_json`; fleet-server reads the audit log's metadata JSON back with it, while fleet-core's `AuditMetadata` writes its JSON itself, byte for byte as `serde_json` would (ADR-0015) |
| Configuration | `figment` | 0.10.19 | startup, testkit; agent and server tests | TOML file + env; upstream is quiet but the crate is stable. It has no default features: fleet-startup's loader enables `toml` and `env`, and fleet-testkit `test` for `jail::in_jail`, whose one closure carries the workspace's only approved `#[expect(clippy::result_large_err)]` (ADR-0014, ADR-0015) |
| DTO & config validation | `garde` | 0.23.0 | api-types, agent, server | Domain value objects use hand-written constructors. It has no default features; members enable `derive` and the rules they use. The agent's config uses garde for its number ranges only and converts texts with the core constructors, so it reports every problem at once (ADR-0014) |
| Logging / tracing | `tracing`, `tracing-subscriber` | 0.1.44, 0.3.23 | all | `env-filter`, `json`. fleet-startup builds both binaries' log layer (ADR-0015). fleet-testkit's log capture uses `tracing` and `tracing-subscriber`; the redaction tests use it (ADR-0011). Its log buffer is the writer for format and filter tests (ADR-0015) |
| Metrics | `metrics`, `metrics-exporter-prometheus` | 0.24.6, 0.18.3 (dfo) | runtime, agent, server | Internal port only. The exporter's default `push-gateway` brings its own TLS stack: enable `http-listener` only. Until P12.4 serves the endpoint, the agent enables no features and only installs the recorder. Its metrics-util dependency always enables `storage`, which brings rand 0.9 and getrandom 0.3 into normal dependencies (both already in the graph, neither used for secrets) (ADR-0014) |
| IDs | `uuid` | 1.27.0, dfo | core, mc, server | v7, serde. fleet-mc only names `Uuid` in azalea's `AccountTrait` (ADR-0011). fleet-server binds IDs as `uuid::fmt::Hyphenated` text, never a plain `Uuid` (ADR-0015) |
| Time | `chrono` | 0.4.45, dfo | core, runtime, agent, server | Always UTC. No `clock` feature in core or runtime: time is passed in. The runtime derives `DateTime<Utc>` from tokio's clock, anchored at a wall time its caller passes in (ADR-0010, ADR-0013). The agent enables `now` only, for that one read at startup; `clock` would add local time zones (ADR-0014). fleet-server enables `now` only, for its `SystemClock`, the one wall-clock read behind the `Clock` port (ADR-0015) |
| CLI | `clap` | 4.6.7 | agent, server | derive. Both binaries handle clap's errors themselves, so `healthcheck`'s usage errors exit 1 (ADR-0014, ADR-0015) |
| Hidden password prompt | `rpassword` | 7.5.4 | server CLI | `user create-owner`'s prompt; `--password-stdin` is the non-interactive path (ADR-0016) |

### Rust: API, transport & persistence
| Concern | Crate | Version | Used in | Notes |
|---|---|---|---|---|
| HTTP framework + WebSocket server | `axum` | 0.8.9, dfo | server | `ws` feature. fleet-server enables only what it uses: `matched-path` (P6.6), so the trace layer logs the route's template, never the path a client sent, and `tokio` and `http1` (P6.11) for `axum::serve`; the error body is written with serde_json (ADR-0015) |
| Typed headers (and cookies later) | `axum-extra` | 0.12.6 | server | `TypedHeader<Authorization<Bearer>>` |
| Service abstraction | `tower` | 0.5.3, dfo | server, client | fleet-server lists its middleware stack with `ServiceBuilder` (no feature); its tests enable `util` for `ServiceExt::oneshot` (ADR-0015) |
| HTTP middleware | `tower-http` | 0.7.1 | server | fleet-server enables `catch-panic`, `timeout`, `limit`, `sensitive-headers` and `set-header`. Never `request-id`, which turns on uuid's `v4` and with it `rng`, nor `trace`, whose response hook can't see the route: the request-ID and trace layers are the server's own (P6.6). reqwest keeps its own 0.6.11 (ADR-0015) |
| OpenAPI | `utoipa`, `utoipa-axum` | 6.0.0, 0.3.0 | api-types, server | P0 picked 6.x (ADR-0009); re-checked in P6.10 (2026-10-11): still the newest, MIT/Apache-2.0. utoipa without default features: fleet-api-types and fleet-server enable `macros`, and fleet-api-types `preserve_order`, so properties keep the field order ts-rs writes and a test compares the two. utoipa-axum takes axum without default features. Cargo.lock gained only utoipa, utoipa-gen, utoipa-axum and its macro helper pastey (ADR-0015) |
| Database | `sqlx` | **0.9.0**, dfo | server | `runtime-tokio`, `sqlite-bundled`, `migrate`, `macros`, `uuid`; no TLS feature (SQLite); offline data in `crates/fleet-server/.sqlx/`. Not `sqlite`: in 0.9 it also turns on extension loading, deserialize and unlock-notify. Not `chrono`: it would turn on chrono's `clock` for the whole workspace build, so times are converted in fleet-server. Not `json` (ADR-0015) |
| gRPC | `tonic`, `tonic-prost` | **0.14.6** | proto, agent, server | Features `tls-aws-lc`, `tls-connect-info`. Never `tls-ring` or `tls-webpki-roots`. Declared without default features (P10.1): fleet-proto enables `codegen` only, for the generated client and server; the agent and the server add the transport and TLS features in their groups ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)). Group B: fleet-server `server`, `router` (`add_service`) and `tls-aws-lc`, which turns on `tls-connect-info`, with `channel` for its loopback tests; fleet-agent `channel` and `tls-aws-lc`, with `server` and `router` for its fake port in tests. Group C: no server-wide `max_connection_age`, grace or `timeout`, which fleet-server's clippy config bans (U7); its listener bounds each connection's age itself, lifted while an agent's stream lives |
| Protobuf | `prost` | 0.14.4 | proto | Its defaults (`derive`, `std`). Already in Cargo.lock through testcontainers before P10.1 |
| Protobuf codegen | `tonic-prost-build`, `protox` | 0.14.6, 0.9.1 | proto (`build.rs`) | Pure Rust, no system `protoc`. tonic-prost-build without default features: no transport helpers, no `cleanup-markdown`. protox 0.9.1 over 0.10.0 (2026-10-07) is the user's choice at Phase 10's planning; no advisory names it. Build-only, with prost-build, prost-reflect, protox-parse, logos, miette and prost-types, which nothing uses at run time ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)) |
| TLS | `rustls` | 0.23.46 | agent, server, client, desktop | **Only the aws-lc-rs provider**, installed explicitly at startup. The default features select it. The agent's `main.rs` installs it before anything else runs (ADR-0014). 0.23.46 is a security floor (2026-10-14): RUSTSEC-2026-0346 (client session values' `Debug`), 0347 (ECH, never enabled here), 0348 (the `brotli` feature, never enabled here) and 0349 (a downgrade to classical key exchange on resumed client connections) |
| X.509 / CSR | `rcgen` | 0.14.10, dfo | server (CA, signing, `pki`; dev: the Microsoft adapter's certificate test), agent (key, CSR), client (dev: test certificates) | fleet-server: `aws_lc_rs`, `pem`, `x509-parser`, `zeroize` (Phase 10, group B: wipes a key pair's PKCS#8 copy; adds no crate); fleet-agent: `aws_lc_rs`, `pem`, `zeroize` (P10.3). The default is ring; `aws_lc_rs` turns on x509-parser's `verify-aws`, never its ring `verify`. **Every key is Ed25519 from getrandom bytes**, wrapped as PKCS#8 by `fleet_core::pki::ed25519_pkcs8`, never `KeyPair::generate`, which draws from aws-lc's own RNG (S10). rcgen 0.14 reads CSRs but no longer certificates (x509-parser below). fleet-client's TLS tests make their CA and server certificates with it at run time (`aws_lc_rs`, `pem`), since no key is ever committed ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)); fleet-server's Microsoft adapter test does the same from P9.3 ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)); P10's PKI in production from group B ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)) |
| X.509 parsing | `x509-parser` | 0.18.1, dfo | server | Reads the PKI's certificates: the CA flag, the server certificate's names and validity at serve's startup, and every field of the certificates the server issues in tests. The user's choice in Phase 10's group B (U5), since rcgen 0.14 no longer parses certificates. Already built through rcgen's `x509-parser` feature, so Cargo.lock gained no crate. Its errors are never passed on: they can quote their input ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)) |
| HTTP client | `reqwest` | **0.13.5**, dfo | client, server, desktop, mc | Feature `rustls` (aws-lc-rs + platform verifier), no native-tls. fleet-mc enables no features: it only names `reqwest::Proxy` in azalea's `AccountTrait` (ADR-0011). fleet-client enables none either, and builds its client with no redirects, no proxies, a 5 s connect and 20 s request timeout, and `tls_certs_only` for a custom CA; it reads each body itself, capped at 1 MiB ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)). fleet-server (P9.3) enables `form` only, for its Microsoft adapter: no redirects, no proxies, https only, a 5 s connect and 15 s request timeout, every body read through its own reader capped at 256 KiB. Its clippy config bans reqwest's unbounded readers and `reqwest::get`, and its client types outside the adapter's `client.rs` ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)) |
| WebSocket client | `tokio-tungstenite` | 0.29.0, dfo | client | `connect`, `rustls-tls-native-roots`. 0.29 matches axum 0.8.9's `ws`, so only one tungstenite is built |
| Rust → TypeScript types | `ts-rs` | 12.0.1, dfo | api-types | `serde-compat` only, so serde's renames reach the TypeScript; `chrono-impl` and `uuid-impl` come with the first DTO that holds a time or an ID. The export runs through `Config::new()`, never `#[ts(export)]` or the `TS_RS_*` variables (ADR-0015) |

### Rust: security
| Concern | Crate | Version | Used in | Notes |
|---|---|---|---|---|
| Password hashing | `argon2` | **0.6.0**, dfo | server | Argon2id, in `spawn_blocking` behind a semaphore. `alloc`, `password-hash`, `zeroize`; without the default `getrandom`, so salts come from the `SecureRandom` port through `hash_password_with_salt`. The dev profile compiles it and `blake2` at `opt-level = 3`: about 10 ms per hash instead of 228 ms, so every test hashes at the real baseline (ADR-0016) |
| Authenticated encryption | `chacha20poly1305` | **0.11.0**, dfo | server (vault) | XChaCha20-Poly1305. `alloc`, `zeroize`; without the default `getrandom`, so nonces come from the `SecureRandom` port (ADR-0016) |
| Hashing | `sha2` | **0.11.0** | server, client, agent | Token hashes, certificate fingerprints: fleet-client's `CustomCa::fingerprints` (P8.4, [ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)), the agent's CA pin (P10.3, the user's choice U4, [ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)) |
| Keyed hashing (HMAC) | `hmac` | 0.13.0 | server | HMAC-SHA256 keys of the lockout's `login_attempts` rows, so no row holds a typed name, and the derivation of their key from the vault's active key. `zeroize` wipes the keyed state. RustCrypto like sha2 0.11; already in Cargo.lock (ADR-0016) |
| Constant-time comparison | `subtle` | 2.6.1 | server | First used for the vault's duplicate-key check (ADR-0016) |
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
| Signed auto-update | `tauri-plugin-updater` | 2.13.1, dfo | desktop | Its default `rustls-tls` feature forces the ring provider; TLS comes from the workspace `reqwest` instead |
| OS credential store | `keyring-core` + `windows-native-keyring-store` | 1.0.0, 1.1.0 (keyring v4 family) | desktop | Refresh token storage: one entry per server origin, `Local` persistence, entries built from an explicit store (no global default). The Windows store without default features, so no `search` and no regex (P8.4, [ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |

### Rust: testing
| Concern | Crate | Version | Notes |
|---|---|---|---|
| Fixtures / parametrized tests | `rstest` | 0.27.0, dfo | `#[case]` tables. The default async-timeout and crate-renaming features aren't needed (ADR-0010) |
| Property testing | `proptest` | 1.11.0, dfo | State machine, parsers. Only `std`, which reads `PROPTEST_CASES`; no fork or timeout mode (ADR-0010) |
| Snapshot testing | `insta` | 1.49.0 | `json` and `redactions` features, enabled by the member that uses them (fleet-core: `json`; fleet-server: `json`, for the error responses' snapshots, plus the migrations' checksum snapshot) |
| Mocks | `mockall` | 0.15.0 | Only for interaction checks; put `#[automock]` above `#[async_trait]` |
| Containers | `testcontainers` | 0.28.0, dfo | `itzg/minecraft-server`, for fleet-mc's slow tests (P3.7) and fleet-integration's Phase 10 managed check, an online-mode one ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md), U21). The default `ring` feature turns on TLS for the Docker client, which the local socket and the Windows named pipe don't need; no feature is enabled |
| Time control | `tokio` `test-util` | | `start_paused`, `advance` |
| Independent IP-network check | `ipnet` | 2.12.2 | Dev only: fleet-server's property test compares its own `IpNet` (`[http] trusted_proxies`) and the /64 limiter key with it. Production code never uses it. Already in Cargo.lock through reqwest (ADR-0016) |
| In-memory gRPC transport | `hyper-util` | 0.1.21, dfo | Dev only: fleet-server's control-plane test hands tonic's client a tokio duplex pipe through `rt::TokioIo` (`tokio` only), so an agent's stream runs a virtual hour through the real router on paused time. The user's choice in Phase 10's group C (U6). Already in Cargo.lock through tonic, axum and reqwest ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)) |
| Temporary files | `tempfile` | 3.27.0 | fleet-server's database tests: a fresh folder per test, removed on drop; fleet-integration's cases keep a server's database, PKI and agents' identities in one (ADR-0019, U21). Already in the lockfile through figment and insta (ADR-0015) |
| `log` records in tests | `log` | 0.4.34 | fleet-server's one normal use: `log::LevelFilter`, which sqlx's statement-logging levels take; clippy bans its logging macros there, so all server logging stays on tracing (ADR-0015). Otherwise a dev-dependency. fleet-testkit: its log capture's test emits a `log` record to prove that reqwest's and rustls's logs reach the redaction check through `tracing-log` (ADR-0011). fleet-agent: its telemetry test proves that the log filter and azalea's caps see a `log` record's real target (ADR-0014) |
| Fuzzing | `libfuzzer-sys`, `arbitrary` | 0.4.13, 1.4.2 | Driven by cargo-fuzz |

### Frontend (same one-library-per-concern rule)
| Concern | Package | Notes |
|---|---|---|
| Package manager / workspaces | pnpm | |
| Bundler & dev server | `vite` | 8.3.4, with `@vitejs/plugin-react` 6.1.2 (oxc on Vite 8, no Babel or SWC; approved in P8.2, [ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| UI framework | `react`, `react-dom` 19 | 19.3.0, with `@types/react` and `@types/react-dom` 19.3.0 (approved in P8.2) |
| Language | `typescript` | `strict`, `noUncheckedIndexedAccess`, `exactOptionalPropertyTypes`. 7.0.2, the Go port: a native `tsc`, but no JavaScript compiler API until 7.1, so no tool that needs it (typescript-eslint, ts-node, vite-plugin-checker); each tsconfig lists `vite/client` in `types` ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| Server state & caching | `@tanstack/react-query` | 5.104.1 (approved in P8.6): no retries and no refetch on focus or reconnect; no query or mutation ever carries a secret ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| Routing | `@tanstack/react-router` | 1.170.42 (approved in P8.6): type-safe routes defined in code (no router plugin), over a memory history ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| UI-only state | `zustand` | Small stores only. 5.0.15 (approved in P8.6): one set per `App`, set by the watched client (session, banner) |
| Schema validation | `zod` 4 | 4.6.5 (approved in P8.5): the input checks every API call passes (`validated`), and the forms' schemas from P8.6 on ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| Forms | `react-hook-form`, `@hookform/resolvers` | 7.89.0 and 5.9.1 (approved in P8.7): zod schemas built on `api/schemas.ts`; a call carrying a secret is a direct call, never a mutation ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| Styling | `tailwindcss` 4 | 4.3.3, with `@tailwindcss/vite` 4.3.3 (approved in P8.2) |
| Components | shadcn/ui (Radix UI primitives, copied into the repo) | Style `radix-nova`, base color neutral, aliases through `packages/ui`'s package.json `#` imports. P8.6 copies Button, Input, Label, Textarea, Card and Alert from the registry's JSON (never the CLI, which installs packages itself), with `cn` 0.4.0, class-variance-authority 0.7.1 and the single Radix packages `@radix-ui/react-slot` 1.4.0 and `@radix-ui/react-label` 2.1.16 (never the `radix-ui` umbrella); tw-animate-css and shadcn's tailwind.css wait for a component that needs them. Before P8.6 it was only `components.json` and the theme tokens; the CLI never runs as a dependency ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| Icons | `lucide-react` | 1.53.0 (approved in P8.6) |
| Toasts | `sonner` | 2.0.8 (approved in P8.6), **patched**: `patches/sonner@2.0.8.patch` removes its `__insertCSS(…)` call, a `<style>` the release CSP blocks; its stylesheet comes through `styles.css`. A bump redoes the patch and runs `Toaster.test.tsx` ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| Tauri bridge | `@tauri-apps/api` 2 | 2.12.1 (approved in P8.5). Only in `apps/desktop`, and only `@tauri-apps/api/core`'s `invoke`, injected into `TauriApiClient`; Biome refuses every other Tauri import ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| Unit / component tests | `vitest`, `@testing-library/react`, `@testing-library/user-event`, `jsdom` | vitest 5.0.3, jsdom 29.1.1 (jsdom 30 needs Node ^24.15), @testing-library/react 16.3.3 with its peer `@testing-library/dom` 10.4.2 (approved in P8.2); `@testing-library/user-event` 14.6.7 arrived in P8.6 |
| API mocking | `msw` | 3.0.3 (approved in P8.9), with `@msw/playwright` 0.7.0: MSW's handlers on Playwright's routing, in the Playwright process only; nothing of MSW reaches any bundle ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| E2E | `@playwright/test` | 1.64.0 (approved in P8.9): the installed Microsoft Edge (`channel: "msedge"`), so no browser is downloaded; `@types/node` 24.19.2 for its config and specs ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
| Lint & format | `@biomejs/biome` | |

### Tools & infrastructure (not crates)
- **Rust tooling** (versions pinned in P0.3; tools for later phases are re-checked when they're introduced):
  - rustup with the dated nightly from `rust-toolchain.toml` (ADR-0003), plus stable 1.99.0 for the `stable-check` job
  - `just` 1.58.0
  - cargo-nextest 0.9.146, cargo-llvm-cov 0.9.1, cargo-deny 0.20.2, cargo-insta 1.49.0
  - sqlx-cli **0.9.0** (P6), SQLite-only: `cargo install sqlx-cli --version 0.9.0 --locked --no-default-features --features sqlite`, the same in CI and the README
  - Later: cargo-chef 0.1.78 (P5), cargo-mutants 27.1.0 and cargo-fuzz 0.13.2 (P13)
- **Frontend tooling:** pnpm 12.9.1 (`packageManager`) and Biome 2.5.15 from P0.8; every other package is pinned in the phase that introduces it, exactly (no `^`), with `pnpm-lock.yaml` as the lock. From P8.6 `pnpm-workspace.yaml` sets `minimumReleaseAge: 4320`: pnpm resolves no release younger than 3 days and checks the whole lockfile at every install; exceptions only in `minimumReleaseAgeExclude`, with a comment and the user's approval. The Tauri CLI is `@tauri-apps/cli` 2.12.2, a dev-dependency of `apps/desktop` (approved in P8.2, [ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)).
- **Runtime & hosting:**
  - Docker + Docker Compose
  - Caddy 2 (TLS for the public API)
  - `itzg/minecraft-server` (test server)
  - the agent image's base images (P5.6), pinned by multi-arch index digest in `deploy/docker/agent.Dockerfile`: `rust:1.99.0-slim-trixie` (builder) and `gcr.io/distroless/cc-debian13:nonroot` (runtime)
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
- **Step-up.** These actions require re-entering the password within the last 5 min:
  - changing the password
  - replacing 2FA (`POST /me/mfa/reset`, group D: the new authenticator replaces the old one at confirm)
  - deleting an MC account
  - changing a role
  - disabling or enabling a user, creating or revoking an invite (Phase 8, group E)
  - approving or refusing a pending link, and creating, changing or revoking a grant (Phase 9, group E)
  - setting a bot's server address (Phase 10, group E)
  - rotating keys

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
> - **Agents.** Only the Owner enrolls agents; viewing and disabling them is Admin+.
> - **Roles.** Only the Owner changes roles, so Admins manage Members by disabling and enabling them. Ownership transfer gets a permission together with a route.
> - **Modes.** Private modes follow the account rule. Shared modes are edited by their creator, while an Admin, and by the Owner. Built-in modes are read-only.
> - **The Owner's "everything"** leaves out: built-in modes, their own role and account status, another Owner, Owner invites, grants to themselves, and other users' personal resources.
> - **Server settings** get a permission when a route exists.

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

> Note (§7.4, Phase 11, group A) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S3, the user's approval at the plan review): **the per-bot chat rate** is the server's setting, `[rate_limits] chat_per_bot_interval_secs` (1–60, 3 by default) and `chat_per_bot_burst` (1–10, 3), sent to agents in each bot's spec, so the agent's bucket, which mode chat shares, keeps the same rate as the server's own limit on chat through the API (group B). The burst stops at 10 because the agent enforces it too, which keeps every allowed rate under vanilla's spam kick. A spec with another rate gives the bot a full bucket at it; a standalone agent uses the default. `chat_per_user_per_min` (1–60, 20) arrives with group B's routes.

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
  > - **The procedure** (stop, copy the database, add and activate the new key, rotate, check the status, drop the old key from the config but keep its file while older backups exist, start) is in the README.
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
  > - **Coverage gates.** `scripts/coverage-gates.mjs` is plain Node (Node 24 is required everywhere anyway), with `node --test` tests. A gate whose files don't exist yet is skipped; the workspace gate never is. The auth and vault path patterns follow the server layout in `CLAUDE.md` and get confirmed in P7. *Confirmed in Phase 7, group A ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): the auth and vault gates are lists of path prefixes that fail on a prefix without files, required from group A on; each Phase 7 group adds its modules.*
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
  > - **Commands** are never signed by azalea. A server that enforces secure chat rejects those with message arguments, while `send_chat` returns `Ok` (found by the user's real-account check; flagged under P4.5 and P11.3). *Decided in Phase 11, group A ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S2): fleet-mc refuses such a command with `ChatUnavailable` before sending, wherever chat must be signed, judged by the server's command tree.*
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
  >   - It also sends `/me` and expects the server to reject it: the known limit under P11.3. If the emote's echo arrives instead, the step fails with "azalea now signs commands: update this step and the P11 note". *Changed in Phase 11, group A ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S2, the user's approval): the step, now `emote_refused`, expects fleet-mc's `ChatUnavailable` and a later signed chat's echo before any rejection or emote echo; its verdict is a pure function with default-profile tests. Only the user runs the check.*
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
  > Mode chat with a command that takes message arguments (`/me`, `/msg`, `/tell`, `/w`, `/say`, `/teammsg`) is rejected by a server that enforces secure chat, while `send_chat` returns `Ok`. That's undecided here too; see P11.3's note. *Decided in Phase 11 ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S2): fleet-mc refuses it with `ChatUnavailable`, so the mode's step is skipped at `debug`.*

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

  > Note (P4.8, a bug found in Phase 6, the user's decisions) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): **The model missed a bot's last events.**
  > - **Found.** CI's coverage job on PR #23 hit a case where `afkfleet_bot_reconnects_total` was 4 and the test's model 3. The case failed on Windows too, so it wasn't a platform difference.
  > - **The cause was the test, not the runtime.** `settle()` absorbed the events first, then awaited the probes and `snapshot_all()`. In this case a bot's retry fell due in the instant the settle loop woke up. The bot then went `AwaitingSession{7}` → `Connecting{7}` → Online while the loop awaited, and the snapshots already showed Online, so `settle()` returned without the model ever seeing those three changes. Attempt 7 was a real reconnect, a connect the bot made on its own, so the counter's 4 was right; the model's storm checks had skipped those changes too.
  > - **The fix.** `settle()` absorbs the events again right after `snapshot_all()`, before it judges the snapshots. The actor publishes each snapshot and its event in one synchronous step, so every state the snapshots show is absorbed. Neither the runtime nor what the metric counts changes.
  > - **The regression test.** CI's seed is in `tests/fleet/chaos.proptest-regressions`, which proptest replays before the 500 new cases. The test failed on it (4 against 3) before the fix and passes after it.
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
**Introduces:** `clap`, `figment`, `garde`, `tracing-subscriber`, `metrics-exporter-prometheus`, `getrandom`, `rustls`. *(anyhow was planned here; group B didn't need it, so it arrives with the first `main.rs` that uses it, ADR-0014.)*

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
> - **Dependencies.** `figment` (`toml`, `env`; `test` for `Jail` in tests), `garde` with `derive`, `tracing-subscriber` with `env-filter` and `json`, and `log` as a fleet-agent dev-dependency (group A). Group B adds `clap` (`derive`), `getrandom`, `metrics-exporter-prometheus` (no features), `rustls` (its defaults: aws-lc-rs), chrono's `now`, tokio's `rt-multi-thread` and `signal`, and fleet-testkit as a dev-dependency.

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
- [x] **P5.2** Telemetry: pretty logs in dev and JSON in prod, an env filter, and a panic hook that logs through `tracing`.

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

  > Note (P5.2, as built, group A) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)):
  > - **Shape.**
  >   - `fleet_agent::telemetry::init(&LogConfig)` installs the global subscriber and the `log` bridge, the startup warning and the panic hook. It returns `TelemetryError::AlreadyInstalled` when a subscriber or logger is already there.
  >   - `telemetry::layer(config, ansi, writer)` builds the layer, so tests can write into a buffer.
  >   - The config gains `LogConfig { format: LogFormat, filter: LogFilter }`. `LogFilter` reads each directive with tracing-subscriber's own `Directive` parser and refuses a bad one by its index, without echoing it.
  > - **Found in the build: azalea's default only lowers** (the user's decision).
  >   - The `azalea=warn` in front of the operator's directives is more specific than their global level, so `filter = "off"` or `"error"` still let azalea's warnings through, and so did a filter without a global level.
  >   - The default is now `warn`, or the operator's global level if that's lower (`off` when there's none).
  > - **Found: EnvFilter applies a span directive's target to the span too.** So `azalea[x]=debug` means azalea's own spans named `x`. Inside such a span EnvFilter enables every target at that level, and the caps still hold: `azalea_auth` stays at `info` and azalea at what's named.
  > - **Panic reports** have the target `afkfleet::panic`, a name no crate prefix shares, with the fields `thread`, `location`, `payload` and `backtrace`.
  > - **fleet-core** makes `text` a public module that exposes only `sanitize_untrusted` and `UntrustedText`; the chat sanitizer's own items stay private.
  > - **Tests.**
  >   - Unit tests run the real layer in JSON into a buffer:
  >     - 11 filters × 5 probe targets, including the span cases
  >     - panic reports getting through `off`, `afkfleet::panic=off` and a filter without a global level
  >     - the startup warning
  >     - the JSON shape and the pretty format with and without colors
  >     - the panic helpers
  >   - `tests/telemetry_init.rs` covers the second `init` and the `log` bridge (`azalea_auth::certs` at `debug` is dropped, `info` is kept, `reqwest` at `debug` is kept).
  >   - `tests/panic_hook.rs` panics on a named thread and checks the one event.
  >   - Each was red against stubs first.
- [x] **P5.3** Wiring: `McHostPool` + `AzaleaConnector` + `Fleet`, with the standalone spec source.

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

  > Note (P5.3, as built, group B) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)):
  > - **Shape.**
  >   - The binary is `afkfleet-agent` (`crates/fleet-agent/src/main.rs`); `fleet_agent::cli` holds its clap structs, so they're testable and group C can add `healthcheck`.
  >   - `fleet_agent::run::run(RunParts { config, connector, diagnostics, anchor, seed }) -> Outcome { exit, report }`. `Exit` gives the exit code: `StartupFailed` 1, `AbandonedLimit` 3, `SupervisorFailed` 4 (0 comes with P5.4).
  >   - `fleet_agent::diagnostics::HostDiagnostics` has one method, `sample() -> HostSample` (fleet-mc's five numbers), implemented for `AzaleaConnector`, which owns its `McHostPool`. main.rs passes one connector as both the connector and the diagnostics.
  > - **main.rs** (the user's decisions):
  >   - The order: `Cli::try_parse` (clap's own output and code), `config::load` (before any runtime exists: it reads a file), `telemetry::init`, the aws-lc-rs provider, the Prometheus recorder, a multi-threaded tokio runtime, `run`, `Runtime::shutdown_timeout(1 s)`, then the last line, "the agent stopped" with `exit_code`.
  >   - Errors before logging exists go to stderr as one plain message through `writeln!` (no `#[expect(clippy::print_stderr)]`) and exit 1. Every later startup error is one `error` event, "the agent can't start", and exits 1.
  >   - No anyhow: main.rs maps typed errors to codes itself.
  >   - The anchor is `Utc::now()` (chrono's `now`), the seed `getrandom::u64()`.
  >   - The Prometheus handle isn't kept: nothing serves it until P12.4, and with no histograms the recorder needs no upkeep.
  > - **One deadline for a shutdown** (the user's decision): shutdown_timeout + reply_timeout from its start.
  >   - `Fleet::shutdown` answers `Ok(report)`: the supervisor's task is joined by the deadline.
  >   - `Busy` or `ShuttingDown`: a `warn`, the supervisor's token is cancelled (it then shuts down without a report), and its task is joined by the deadline.
  >   - `TimedOut` has used up the deadline: the token is cancelled, and the run exits 4 at once.
  >   - A supervisor that doesn't end by the deadline, or panics, is an `error` and exits 4. **Exit code 4 also means "didn't end when asked".**
  >   - A bot the fleet refuses at startup is logged at `error` with its ID and username; the fleet shuts down the same way, and the run exits 1 (or 4).
  > - **Diagnostics:** sampled at once and every 5 s (a constant; missed ticks are skipped). The counters `afkfleet_mc_abandoned_threads_total`, `afkfleet_mc_dropped_chat_total` and `afkfleet_mc_ignored_action_bar_total` take fleet-mc's totals with `absolute()`; `afkfleet_mc_host_threads` (abandoned threads that still run included) and `afkfleet_mc_worlds` are gauges. All are described and registered at 0 right after `Fleet::new`.
  > - **Logs.**
  >   - `run` enters the root span `agent{agent=…}` itself, so the bots' spans nest in it; main.rs logs its own lines in a span of the same name. fleet-mc's `mc_session` spans are created on host threads and stay roots.
  >   - `info`: "starting a standalone bot" (`bot_id`, `username`, `server`, `mode`), "the agent is running" (`bots`), "the agent stopped" (`exit_code`, plus `stopped`, `aborted` and `crashed` with a report).
  >   - `error`: "the abandoned-thread limit is reached; shutting down" (`abandoned`, `limit`); "the fleet's supervisor ended unasked" (`panicked`). A `JoinError` is never logged: its text carries the panic payload, which is untrusted and already logged, sanitized, by the panic hook.
  >   - The event task logs "the bot received chat" (`kind`, `sender`, `text`, `truncated`), "the bot's mode sent chat" (`text`) and a lag (`skipped`) at `debug`. Chat text is a plain string field, so both formats escape its line breaks.
  > - **Dev:** `deploy/dev/agent.toml` runs AfkBot1 and AfkBot2 on `afk` and AfkBot3 on `farm` against `127.0.0.1:25565`, with pretty logs; `just dev-agent` runs it.
  > - **Tests.**
  >   - `tests/run/`: the IDs and the startup lines, the span nesting, every startup error, the limit, the sampling period and the metrics, on fleet-testkit's `FakeConnector`, a fake diagnostics source and paused time.
  >   - Unit tests: the specs, the watch, the event log, the metrics, and `finish` against a real `Fleet` whose supervisor a test task holds, runs, drops or panics. Each `finish` case checks the exit code and the paused time it ends at (15 s for `TimedOut` and a hung supervisor).
  >   - `tests/cli.rs` runs the binary: a missing or invalid config exits 1 with the plain message on stderr, a usage error exits 2, `run --help` lists the codes, `--version`. Each run has its own deadline, and a guard kills the child.
  >   - 48 tests were red against stubs first.
  > - **Until P5.4,** the binary has no graceful stop: a signal ends it the default way.
- [x] **P5.4** 🔴 Signals (Ctrl+C, SIGTERM) trigger a graceful shutdown within `shutdown_timeout`.

  > Note (P5.4, from P4.7) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): A signal calls `Fleet::shutdown(shutdown_timeout)`, or cancels the supervisor's token, which shuts down within `RuntimeConfig::shutdown_timeout`. `shutdown` returns a `ShutdownReport { stopped, aborted, crashed }` for the log.

  > Note (P5.4, from the Phase 5 plan, the user's decisions) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)): A signal calls `Fleet::shutdown(shutdown_timeout)` and logs the report. A second signal only logs that the shutdown is already running. Signals: SIGTERM and SIGINT on Linux, Ctrl+C and Ctrl+Break on Windows.

  > Note (P5.4, as built, group B) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)):
  > - **Deviation: a signal source, not a shutdown future** (the user's decision). One future can't show the second signal, so `run` takes a `fleet_agent::signals::ShutdownSignals` with a cancel-safe `recv() -> Signal`.
  >   - `OsSignals::install()` registers SIGTERM and SIGINT (Unix) or Ctrl+C and Ctrl+Break (Windows). main.rs calls it inside the runtime, which tokio's signal handling needs, and before the fleet starts, so an early signal isn't lost. An error there is a startup error (exit 1).
  >   - A stream that ends never makes a signal up: the source then waits forever, and so does the tests' channel fake once its sender is gone.
  >   - Other platforms fail to build with a `compile_error!`.
  > - **The first signal** logs "shutting down" with `signal` ("SIGTERM", "SIGINT", "Ctrl+C", "Ctrl+Break") and shuts down with P5.3's single deadline. The run ends with `Exit::Stopped`, code 0, even if bots had to be aborted. P5.3's fallbacks keep the code: `Busy` or `ShuttingDown` still give 0 once the supervisor ends; `TimedOut` or a hung supervisor give 4.
  > - **A later signal** only logs "already shutting down" with `signal`, while the fleet shuts down and while its supervisor is awaited; the deadline doesn't move.
  > - **Tests**, all red first on assertions, against stubs that compiled: a source that installed the real handlers but never yielded (so a signal was swallowed instead of ending the process), names that were wrong, and a watch and shutdown that ignored signals.
  >   - Unit tests: the names, the source's stream merging (an ended stream leaves the other working; with both ended, nothing comes), the watch, and `finish` after a signal (each fallback, and repeats while the fleet shuts down and while the supervisor is awaited).
  >   - `tests/run/signals.rs`: a signal stops the bots with exit 0 and the report; a bot slower than the shutdown timeout is aborted at 10 s, still exit 0; a second signal is only logged and doesn't change the 8 s shutdown; a signal sent before the start is handled once the agent runs; a source that ends never stops the agent.
  >   - Unix only: `tests/signals.rs` (one test: `OsSignals` receives SIGTERM, then SIGINT, sent to its own process with `kill`) and `tests/cli.rs`'s SIGTERM test (the binary with one bot at `127.0.0.1:1`, `kill -TERM` after "the agent is running", exit 0 and "the agent stopped" with the report as the last line). Windows' Ctrl+C and Ctrl+Break can't be sent without unsafe code; `just dev-agent` exercises them.
  >   - The Unix tests were run red and green in a Linux container, since the dev machine is Windows.
  > Note (P5.4, from the manual test of PR #20, group C): On Windows, Ctrl+C during `just dev-agent` stopped the agent cleanly ("the agent stopped" with `exit_code=0`), but `just` then printed "error: recipe `dev-agent` failed … with exit code 1": Ctrl+C reaches `just`, PowerShell and `cargo` too. The recipe now has `[no-exit-message]`, so `just` prints no message of its own; it still exits non-zero, and the agent logs its own errors. The README says the agent's exit code is the one in "the agent stopped".
- [x] **P5.5** 🔴 A `healthcheck` subcommand. The agent touches a heartbeat file every 10 s, and the check fails when the file is stale. This works in distroless images, which have no curl.

  > Note (P5.5, from the Phase 5 plan, the user's decisions) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)):
  > - **Heartbeat.** The file is touched every 10 s, and only when `fleet.snapshot_all()` returns `Ok`. `Busy`, `TimedOut` and `ShuttingDown` never touch it: a hung supervisor's queue fills with timed-out calls, after which every call answers `Busy` at once. A test covers that case.
  > - **`healthcheck --config <path>`** loads the same config. The file is stale when it's missing or 30 s old or more.
  > - It exits only 0 or 1, since Docker reserves 2. clap usage errors for `healthcheck` map to 1 too.

  > Note (P5.5, as built, group C) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md); the user answered group C's questions on 2026-10-09):
  > - **Shape.**
  >   - `fleet_agent::heartbeat` has `PERIOD` (10 s), `STALE_AFTER` (30 s), the `Heartbeat` port (`touch()`) and `FileHeartbeat`. A touch opens the file with `create(true).write(true).truncate(false)` and sets its modification time, in `spawn_blocking`. It never truncates or deletes the file, so a wrong path can't destroy data.
  >   - `RunParts` gains `heartbeat: H`; main.rs passes `FileHeartbeat` on `heartbeat_file`, and tests pass a counting fake.
  >   - `fleet_agent::healthcheck::check(path, now) -> Health` (`Fresh`, `Stale`, `Future`, `Missing` or `Unreadable`), with `exit_code()` and the one line it prints.
  >   - `afkfleet-agent healthcheck --config <PATH>`. Its help lists codes 0 and 1. `cli::exit_code` turns clap's usage error into 1 when the first argument is `healthcheck`; `--help` and `--version` stay 0.
  > - **The beat** is a task the run owns, with its own child token. It starts once "the agent is running" (a start that fails never beats) and is cancelled when the shutdown starts.
  >   - It beats at once, then every 10 s, with missed ticks skipped. Each touch ends before the next beat.
  >   - Logs, once per streak: `warn` "the fleet didn't answer the heartbeat" (`error`), then `info` "the fleet answers the heartbeat again"; `warn` "the heartbeat file can't be touched" (`file`, `error`), then `info` "the heartbeat file is touched again" (`file`). A wrong path shows up within a second of the start.
  > - **The check** (the user's decisions):
  >   - One line on stdout either way: `healthy: the heartbeat is N s old`, `unhealthy: the heartbeat file is N s old`, `unhealthy: no heartbeat file at <path>`, or `unhealthy: the heartbeat file at <path> can't be read: <error>`.
  >   - stderr is left for errors that keep the check from running: a config that can't be loaded (the same text as `run`'s, exit 1) and usage errors.
  >   - **A time in the future** counts as age 0 up to 30 s ahead, so a small clock correction never fails the check. Further ahead gives `unhealthy: the heartbeat file's time is N s in the future`: a healthy agent's next beat fixes it, and a hung agent can't look healthy for the length of a big backward jump.
  > - **Tests**, red against stubs that compiled (a touch that did nothing, a beat that only waited for its token, a check that always answered healthy, clap's codes unchanged), then green:
  >   - `healthcheck`'s unit tests: the 30 s limits in both directions, the lines and codes, a missing file, and on Unix a path through a file (`ENOTDIR`).
  >   - The beat's unit tests, on a real `Fleet` and paused time: a beat at once and every 10 s; **no beat from a held supervisor through 70 beats, past the point where its full queue answers `Busy` at once**, with one `warn`; beats coming back with an `info`; a file streak; cancelling while a snapshot waits.
  >   - `tests/run/heartbeat.rs`: the first beat right after "the agent is running", none once the shutdown starts, none after a failed start.
  >   - `tests/heartbeat.rs`: `FileHeartbeat` on real files: it creates the file, moves an old time to now, keeps the content, and fails without its directory.
  >   - `tests/cli.rs`: the binary's healthcheck for a fresh, a stale and a missing file, a missing config, a usage error (exit 1) and `--help` (exit 0). The Unix SIGTERM test also waits for its configured heartbeat file before it sends the signal, so the wiring is checked end to end.
  >   - The Unix-only tests ran red and green in a Linux container.
  >   - The shared test fleet of `run`'s unit tests moved into `run::testing`, used by `finish`'s tests and the beat's.
- [x] **P5.6** `deploy/docker/agent.Dockerfile`:
  - cargo-chef
  - a nightly builder
  - runtime `gcr.io/distroless/cc-debian12:nonroot`

  Also `deploy/compose.dev.yaml` with an itzg server plus the agent.

  > Note (P5.6, from Phase 4, group E, the user's decision) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): The compose file gives the agent a `stop_grace_period` above `shutdown_timeout`, e.g. 15 s for the 10 s default, since `docker stop` kills a container after 10 s by default, before the fleet's graceful shutdown ends.

  > Note (P5.6, from group B, the user's decision) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)): The agent's worst-case shutdown is shutdown_timeout + reply_timeout, plus main.rs's 1 s runtime-shutdown bound. So `stop_grace_period` must be above that: **20 s** for the 10 s default, not the 15 s above. Otherwise Docker's SIGKILL replaces exit code 4 and the last line, "the agent stopped".

  > Note (P5.6, as built, group C) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md); the user answered group C's questions on 2026-10-09):
  > - **Deviation: `gcr.io/distroless/cc-debian13:nonroot`, not `cc-debian12`.** Under distroless's SUPPORT_POLICY.md, the Debian 12 `cc` images reached end of life in September 2026 (GoogleContainerTools/distroless#2129), so they get no more updates. Its README still has a Debian 12 table; group C wrongly gave the README as the reason, and group D corrected it. The builder is Debian 13 too (`rust:1.99.0-slim-trixie`), so the binary's glibc matches.
  > - **Production is linux/arm64** (the user's requirement).
  >   - Both base images are pinned by their multi-arch index digest, never by one platform's, and nothing in the Dockerfile names an architecture, so it builds natively on amd64 and arm64.
  >   - deny.toml's `[graph] targets` gains `aarch64-unknown-linux-gnu`. `cargo deny check` stays clean, with no `ring` on aarch64.
  >   - The arm64 builder stage was built under QEMU as a smoke check; a full arm64 build is P12.1's.
  > - **`deploy/docker/agent.Dockerfile`:**
  >   - `chef` installs cargo-chef 0.1.78 (`cargo install --locked`, with the image's stable toolchain), then copies `rust-toolchain.toml` alone and runs `rustup toolchain install`. Both are cached layers before any source, and the toolchain file stays the nightly's only pin.
  >   - `planner` runs `cargo chef prepare`. `builder` cooks the dependencies, then builds `--release --locked --package fleet-agent --bin afkfleet-agent`.
  >   - The runtime has static OCI labels (title, description, source, licenses), the binary at `/usr/local/bin/afkfleet-agent`, `CMD ["run", "--config", "/etc/afkfleet/agent.toml"]`, and `HEALTHCHECK --interval=10s --timeout=5s --start-period=30s --retries=3` through `healthcheck` with the same path. A comment and the README say that another `--config` path needs the same override on the HEALTHCHECK.
  > - **`.dockerignore`** is an allowlist: the workspace manifests, `rust-toolchain.toml`, `.cargo/config.toml` and `crates/`. After them, `.gitignore`'s secret patterns are excluded again at any depth.
  > - **`deploy/compose.dev.yaml`** gains the `agent` service after `minecraft`, since fleet-mc's `pins` test reads the first `image:` line:
  >   - `afkfleet-agent:dev`, built with `pull_policy: build`
  >   - started once the server is healthy, so the bots don't use up their retries while its first start builds the world
  >   - `deploy/dev/agent.compose.toml` mounted read-only at `/etc/afkfleet/agent.toml`
  >   - `stop_grace_period: 20s` and `restart: on-failure`
  >   - `read_only` with a 1 MiB tmpfs at `/tmp`, `cap_drop: [ALL]`, `no-new-privileges`, 512 MiB and 1 CPU
  > - **`deploy/dev/agent.compose.toml`** runs AfkBot4 and AfkBot5 on `afk` and AfkBot6 on `farm` at `minecraft:25565`, with JSON logs, so it never logs out `just dev-agent`'s AfkBot1–3.
  > - **`just stack-up`** builds the image and starts both services, waiting until they're healthy; `just mc-down` stops both.
  > - `RUNTIME_SHUTDOWN` (1 s) moved from main.rs to `fleet_agent::run`, so a test can read it.
  > - **`tests/deploy.rs`** (fast):
  >   - Both dev configs load, inside `Jail` with an empty environment.
  >   - Every compose bot joins `minecraft:25565`, and none clashes with a dev bot.
  >   - The grace period exceeds the compose config's shutdown and reply timeouts plus `RUNTIME_SHUTDOWN`. With 15 s, the test failed.
  >   - Compose mounts the config at `/etc/afkfleet/agent.toml`, and the Dockerfile's `ENTRYPOINT`, `CMD` and `HEALTHCHECK` use that path.
  >   - The approved `in_jail` helper moved to `tests/common/jail.rs`; it's still one `#[expect]`.
  > - **Demo:** `just stack-up` brought both services up healthy (`healthy: the heartbeat is 5 s old`), and AfkBot4–6 came online. `docker compose stop agent` ended in 0.4 s with exit code 0 and "the agent stopped" (`stopped: 3`) last, with no `warn` or `error` line.
- [x] **P5.7** 🔴 Slow end-to-end test:
  1. `compose up`, and all bots come Online.
  2. Restart the MC container; the bots reconnect within the policy.
  3. Stop the agent; the disconnect is clean.

  > Note (P5.7, from the Phase 5 plan, the user's decisions) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)):
  > - **Shape.** A `slow_` test in `crates/fleet-agent/tests/` drives `docker compose` through `std::process::Command`, with its own project name. It reads the agent's JSON logs and RCON `list`, and checks the exit code and the `ShutdownReport`.
  > - **The image is built first.** `just test-slow` builds it before the timed test, since a cold build can outlast the slow profile's 10 minutes.
  > - **Clean-up.** The test runs `down -v` for its own project before it starts, and in a guard that also runs when the test panics.

  > Note (P5.7, from group C, the user's decision) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)): **No CI job builds the image on every PR.** Instead, group D's PR, and every later PR that changes the Dockerfile, `.dockerignore`, `Cargo.lock` or `rust-toolchain.toml`, runs the "Slow tests" workflow on its branch before it's merged. The user starts it under Actions → Slow tests → Run workflow, so the image build is checked in CI.

  > Note (P5.7, as built, group D) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md); the user answered group D's questions on 2026-10-09):
  > - **Shape.** `crates/fleet-agent/tests/slow_compose/` (`main.rs`, `docker.rs`, `logs.rs`, `checks.rs`) holds one `slow_compose_scenario`. It runs the dev stack as the project `afkfleet-e2e`, with the new `deploy/compose.isolated.yaml` on top (`ports: !reset []` on minecraft), so it can run beside `just mc-up`. `!reset` needs Compose 2.24, which the test checks first.
  >   - Every docker call runs from the repository root with a deadline.
  >   - A guard runs `down -v --timeout 10` when the test ends or panics, and the same runs before the start, which covers a run nextest killed.
  > - **Five bots.** `agent.compose.toml` gains AfkBot7 and AfkBot8 on `afk` (AfkBot6 stays on `farm`), so `just stack-up`, the test and the demo run the same five.
  > - **The image.** `just test-slow` pulls the server image and builds the agent's (`compose build agent`) first. It then sets `AFKFLEET_E2E_IMAGE_BUILT=1` through a `$`-parameter; without it, the test stops at once, so it never tests an old image. The stack starts with `up --wait --no-build`.
  > - **The steps:**
  >   1. Every bot is Online (state lines and RCON `list`) within connect + bounds(1).max + connect + 1 s of "the agent is running": 71 s with the defaults.
  >   2. `restart --no-deps minecraft`.
  >      - **The healthy moment** is the end of the first good probe after the server's new StartedAt. It's polled from `docker inspect` at once, since Docker keeps only five probes.
  >      - **Every bot is Online again** by bounds(n).max + connect + bounds(n+1).max + connect + 1 s from that moment, where n is its last Backoff before then. That allows one more failed attempt, since an attempt in flight when the probe passes can still fail.
  >      - **Every wait** from `Backoff { attempt: n }` to the next `AwaitingSession` in the whole log lies in bounds(n), with 100 ms of slack below and 1 s above. A failure says whether the breaker may have been open.
  >      - **The agent didn't restart:** its StartedAt and restart count are unchanged, and "the agent is running" appears once.
  >   3. `stop agent`:
  >      - exit code 0
  >      - the last line is "the agent stopped" with exit_code 0, stopped 5, aborted 0 and crashed 0
  >      - no WARN or ERROR from "shutting down" on
  >      - RCON's list drops every bot within 5 s
  >
  >   Then the whole log must hold no ERROR and only the expected warnings, and every offending line is printed in full. Every check also asserts that it saw data (5 bots, a wait per bot after the restart, a probe, the bots on RCON's list).
  > - **Shared numbers.** `deploy/dev/stack-checks.json` is shared with the demo. It holds:
  >   - the expected warnings (target, message prefix, why)
  >   - the bounds(n) table up to the capped window
  >   - the connect timeout and the agent's grace period
  >
  >   `tests/deploy.rs` checks every number against `agent.compose.toml` and `compose.dev.yaml`. It also checks that the override unpublishes the port, and that minecraft's `stop_grace_period` is at least 30 s. It's now 60 s, so a stop is graceful: Docker's default 10 s could kill the server, and the bots would see reset sockets.
  > - **The budget.** Before each deadline, elapsed + deadline + the agent's grace period + 60 s of teardown must fit the slow profile's limit, 600 s, read from `.config/nextest.toml`.
  > - **Clocks.** The test judges only by Docker-VM timestamps (the agent's log, StartedAt, the probes). Its waits wake on the agent's next line (a `logs --follow` child) or after 250 ms, and never sleep.
  > - **Found in the runs:**
  >   - The restart took about 3 s.
  >   - The bots saw `ConnectionClosed`, not the "Server closed" kick, so azalea's "Got disconnect packet" didn't appear. It stays expected, since a slower stop sends it.
  >   - The server took joins a few seconds before its first good probe.
  > - **Connection resets** (found by the demo's first run, after the runs above; the user's decisions). vanilla's shutdown sometimes resets a bot's socket before its kick, and azalea logs that at ERROR. The whole-log check allows that one error:
  >   - only between the agent's last line before the restart and the server's healthy moment
  >   - no more often than bot sessions ended there with `ConnectionClosed`
  >
  >   It's `expected_restart_errors` in `stack-checks.json`, shared with the demo; the DoD note explains it. `tests/deploy.rs` checks that every expected line names its target, prefix and reason. The checks' new unit tests were red against a stub first.
  > - **Deviation:** the compose-file helpers stay in `tests/deploy.rs`, now for any service, since the scenario reads the agent's grace period from `stack-checks.json`. Only the `stack-checks.json` reader moved to `tests/common/`.
  > - **Tests:**
  >   - 52 fast unit tests of the helpers, red against stubs first.
  >   - 6 new tests in `deploy.rs`, red against placeholder data first.
  >   - The scenario was red against stub parsers ("every bot Online within 71s; last seen: Online in the log: {}"), and once against `stop_signal: SIGKILL` (exit 137). Then it passed in about 45 s.

**DoD:** Demo with 5 bots AFK on a local server for 1 h, with one server restart in between. The logs show no errors except the expected disconnect warnings.

> Note (DoD, from the Phase 5 plan, the user's decision) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)): `just demo-agent` (logic in a script under `scripts/`) runs the demo and prints a summary:
> - warn and error lines by target and count
> - the state-change timeline
> - reconnect times after the restart
> - the shutdown report
>
> The AI runs it in group D and puts its output in the PR; the user can re-run it, for example after a Minecraft-version bump.

> Note (DoD, from group C, the user's decision): The demo also records the agent's peak memory and CPU (`docker stats`), so P12.2's limits come from a measured value with headroom, not a guess.

> Note (DoD, as built, group D) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md); the user answered group D's questions on 2026-10-09):
> - **`just demo-agent [minutes]`** runs `scripts/demo-agent.mjs`, which uses Node's built-ins only and is tested in `scripts/demo-agent.test.mjs`. It runs 60 minutes by default:
>   - It builds the agent's image, then runs the stack as the project `afkfleet-demo`, with `compose.isolated.yaml` on top.
>   - It restarts the server at half time (`restart --no-deps minecraft`) and stops the agent at the end.
>   - Last, it removes the stack (`down -v --timeout 10`), also after Ctrl+C or an error, once the logs are saved.
> - **The summary:**
>   - where it was measured: `docker info`'s architecture, CPUs, OS and memory, plus the CPU model from `lscpu`, else `/proc/cpuinfo`, else "unknown"
>   - the agent's memory and CPU from a streamed `docker stats`: the peak, with its time relative to the restart, the mean and the p95. These are never judged.
>   - warn and error lines by target and count
>   - the state-change timeline
>   - each bot's reconnect after the restart
>   - the shutdown report
> - **The verdict** is PASS (exit 0) or FAIL (exit 1). A FAIL lists each failed criterion with its lines:
>   - an ERROR line, except the connection resets a server restart may cause (below)
>   - a warning `stack-checks.json` doesn't expect, or a line that isn't the agent's JSON
>   - a bot not Online again by its deadline (the e2e test's formula, from the server's healthy moment)
>   - an agent restart
>   - a shutdown that isn't exit 0 with aborted 0 and crashed 0
>
>   A run under 60 minutes says "not the DoD run".
> - **Too short to judge** (exit 2):
>   - Before the start: the minimum comes from `stack-checks.json`. Half the run must hold attempt 3's reconnect deadline plus the agent's stop, which is 7 minutes with the defaults.
>   - After the restart: if the deadlines it observes, plus the stop, don't fit in the time left, it saves the logs and stops.
> - **Raw data** goes under `target/demo-agent/<UTC time>/`: `agent.log`, `minecraft.log`, `stats.jsonl` and `summary.txt`. Only `summary.txt` goes into a pull request, since `minecraft.log` holds the bots' container IPs.
> - **Deviation, found in the first run** (the user's decisions): azalea logs a TCP reset at ERROR (`azalea_client::plugins::connection`, "Error reading packet from Client: IoError { … ConnectionReset …").
>   - **Why it happens.** A server that closes a socket with unread client data sends a TCP reset, and during vanilla's shutdown that can arrive before the "Server closed" kick. In the 8-minute check run, 2 bots got the kick and 3 were closed without it, one of them with a reset.
>   - **The agent recovers as designed:** `ConnectionClosed`, a backoff, and back Online in about 25 s.
>   - **The allowance.** `stack-checks.json` gains `expected_restart_errors` with that one entry. The e2e test and the demo accept it only:
>     - between the agent's last line before the restart and the server's healthy moment, both by the Docker VM's clock
>     - no more often than bot sessions ended in that window with `ConnectionClosed`, since each reset ends one live session, and azalea's line carries no bot ID
>
>     Every other ERROR, and this one at any other time, still fails. The summary reports how many were allowed.
> - **The Minecraft-version bump** runs it: ADR-0003's verify step gains `just demo-agent` (1 h), with its `summary.txt` in the bump's pull request, and P12.6's runbook carries the step.

> Note (DoD, the run, group D): `just demo-agent` ran on 2026-10-10 with its default 60 minutes. Its `summary.txt` is in the PR. **PASS:**
> - 5 bots (four on `afk`, AfkBot6 on `farm`) came Online within a second, and the server restarted once, at minute 30.
> - Every bot got the "Server closed" kick, and every bot was Online again 8–21 s later, well inside its deadline (attempts 2 and 3).
> - The log holds no ERROR, and only the 16 expected disconnect warnings:
>   - 8 "the session ended; the bot connects again"
>   - 5 of azalea's "Got disconnect packet"
>   - 3 of azalea's "failed to create connection"
> - The agent never restarted. Its stop gave exit code 0 and stopped 5, aborted 0, crashed 0.
> - The agent's container peaked at 26.9 MiB and 6.59 % of one core (P12.2's note has the numbers).

> Note (Phase 5 wrap-up, group D) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)):
> - **The Phase 5 Goal, checked on 2026-10-10:**
>   - **"`afkfleet-agent run --config agent.toml` runs a few dev bots (offline mode) against a local server":** `just dev-agent` runs three bots on the host (group B), and the compose agent runs five in Docker (`just stack-up`, group C; five since group D).
>   - **"It survives server restarts":**
>     - The e2e test restarts the server, holds every wait between attempts to the retry policy and every bot to its deadline, and checks that the agent itself didn't restart.
>     - The hour-long demo did the same at minute 30.
>   - **"It shuts down cleanly":** in the e2e test and the demo, the stop gave exit code 0 and the report (stopped 5, aborted 0, crashed 0), and the server dropped every bot at once.
> - **The Phase 5 DoD:** the demo above. It ran 5 bots, four of them AFK and one farming (the user's decision), for an hour against the local server, with one restart. Its logs show no error, only the expected disconnect warnings.
>   - **Deviation:** a restart can cause one error the DoD didn't foresee, azalea's connection reset. The e2e test and the demo accept it only during the restart, at most once per session closed there (the DoD note above). It didn't occur in this run.
> - **Every task, P5.1–P5.7, is ticked.** The crates the phase introduces are in, except `anyhow`, which arrives with the first `main.rs` that needs it (ADR-0014).
> - **Later phases inherit:**
>   - P12.2: the measured memory and CPU, as a baseline per bot
>   - P12.6: the demo in the Minecraft-version bump
>   - the notes for P10, P11.1, P12.1 and P12.4 that ADR-0014 lists

---

### Phase 6: Server foundation (`fleet-server`, `fleet-api-types`)
**Goal:** An HTTP service skeleton that is secure by default, with persistence, an error model and an audit trail, but no business endpoints yet.
**Introduces:** `axum`, `axum-extra`, `tower`, `tower-http`, `sqlx`, `utoipa`, `utoipa-axum`, `ts-rs`, `serde_json`, `tokio-util`.

> Note (P6):
> - **Five group branches.** At the user's request, Phase 6 is built in group PRs like Phases 2–5. Each group has one branch and one commit per task, and the groups run in this order, each after the previous PR is merged:
>
>   | Group | Branch | Tasks |
>   |---|---|---|
>   | A | `p6/p6.1-p6.2-layout-and-config` | The fleet-startup extraction, P6.1, P6.2. fleet-server is a library only, without a binary |
>   | B | `p6/p6.3-p6.8-persistence-and-audit` | P6.3, P6.4, P6.8, plus the time and randomness ports, the `sqlx-offline` job and `just db-prepare` |
>   | C | `p6/p6.5-p6.9-api-types-and-errors` | P6.9 first (ApiError's body DTO lives in fleet-api-types), then P6.5; the `ts-types-fresh` job and `just gen` |
>   | D | `p6/p6.6-p6.7-middleware-and-health` | P6.6, P6.7 |
>   | E | `p6/p6.10-p6.12-cli-and-wrap-up` | P6.10, P6.11, P6.12, the DoD integration test and the phase wrap-up |
>
>   Each later group asks its own implementation-level questions in its session.
> - **Decisions.** The user answered the Phase 6 plan's open questions on 2026-10-10. [ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md) records them; the notes below summarize what changes a task.
> - **fleet-startup** (the user's decision). A new crate holds the binaries' start-up code: the config loader with its key-path errors, the `[log]` types, the log layer and the panic hook (§4). Group A's first commit moves it out of fleet-agent with no change in behavior, and fleet-agent's tests pass with only import changes. Its filter always caps `azalea_auth` at `info` and lets panic reports through. A binary adds its own rules through `FilterRules`, which can't weaken those two; only the agent's `azalea=warn` rules stay in fleet-agent.
> - **Time and randomness are injected ports** (the user's decision), from group B on. fleet-core gets `system::{Clock, SecureRandom, RandomError}`, fleet-testkit `ManualClock` and `SeededRandom`, and fleet-server's infra `SystemClock` and `OsRandom`:
>   - The seeded fake exists only in fleet-testkit, a dev-dependency everywhere, and a script under `scripts/` checks that no workspace member has fleet-testkit as a normal dependency. `OsRandom` is the only other implementation, and main.rs always wires it. The port's docs say every implementation must be cryptographically secure (security rule 4).
>   - `crates/fleet-server/clippy.toml` bans reading the clock or the OS's randomness directly; `SystemClock` and `OsRandom` each carry one approved `#[expect(clippy::disallowed_methods)]`.
>   - Tests never hard-code `SeededRandom`'s bytes or the IDs made from them, and snapshots redact them: rand doesn't promise `StdRng`'s output across versions.
>
>   As built (group B, the user's decisions, [ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
>   - **fleet-core `system`:** `Clock::now()` returns whole milliseconds (the precision the database stores and a v7 ID holds); `SecureRandom::fill`; `RandomError::{Os { code }, Unavailable}`; `random_bytes::<N>()`; and `mint::<I: V7Id>(at, random)`, where `define_id!` implements fleet-core's new `V7Id` trait for every ID type. The caller reads its clock once, so a record's creation time equals its ID's.
>   - **fleet-testkit:** `ManualClock` (`new`, `set`, `advance`, all truncating to ms) and `SeededRandom` (`fail_next` queues one failure that doesn't move the stream).
>   - **fleet-server `infra::system`:** `SystemClock` and `OsRandom`, each with its approved `#[expect]`.
>   - **Deviation: the testkit guard is `just testkit-check`, not a TOML-parsing script** (the user's decision). It asks Cargo: `cargo tree --workspace -e normal,build --target all -i fleet-testkit` must list nothing but fleet-testkit, so `workspace = true`, renames, target-specific tables and members anywhere are covered with no parser to maintain. `scripts/testkit-check.mjs` reads cargo's output and fails on any line it can't read; `just check`, `just ci` and the `deny` CI job run it.
>   - **The bans go further than planned** (the user's decision): uuid's self-minting functions and the `SysRng`/`ThreadRng` types too. A temporary probe, never committed, proved that all 28 entries fire with their reasons.
> - **stable-check.** fleet-startup and fleet-server join `stable_crates` in group A, fleet-api-types in group C. P9.3 removes fleet-server when azalea-auth arrives.
> - **Dependencies (group B, the user's approval).** New in `[workspace.dependencies]`: sqlx (0.9.0, no default features), async-trait (0.1.92) and tempfile (3.27.0, already in the lockfile). fleet-server gains fleet-core, async-trait, chrono (`now`), getrandom, log (only for sqlx's `LevelFilter`), secrecy, serde_json, sqlx (`runtime-tokio`, `sqlite-bundled`, `macros`, `migrate`, `uuid`), tokio (`fs`) and uuid; in dev, insta, proptest (`std`), tempfile and tokio (`macros`, `rt`). fleet-testkit gains chrono and rand (`std_rng`). fleet-core gains nothing.
> - **Dependencies (group C, the user's approval).** New in `[workspace.dependencies]`, each without default features: ts-rs (12.0.1), axum (0.8.9) and tower (0.5.3), plus fleet-api-types by path. fleet-api-types uses fleet-core, serde (`derive`), thiserror and ts-rs (`serde-compat`); in dev, rstest, serde_json and tempfile. fleet-server gains fleet-api-types and axum (no features); in dev, tower (`util`), and insta gains `json`. Cargo.lock gains only ts-rs, ts-rs-macros and termcolor. ts-rs's `chrono-impl` and `uuid-impl` come with the first DTO that holds a time or an ID.
> - **Dependencies (group D, the user's approval).** New in `[workspace.dependencies]`: tower-http (0.7.1; it has no default features). fleet-server gains tower-http (`catch-panic`, `timeout`, `limit`, `sensitive-headers`, `set-header`), tower as a normal dependency (no features, for `ServiceBuilder`), axum's `matched-path` and tokio's `time`; in dev, tokio's `test-util`. fleet-core gains the `RequestId` type, no dependency. Cargo.lock gains only tower-http 0.7.1, next to reqwest's 0.6.11.
> - **Dependencies (group E, the user's approval).** New in `[workspace.dependencies]`: utoipa (6.0.0, without default features) and utoipa-axum (0.3.0). fleet-api-types gains utoipa (`macros`, and `preserve_order`, the user's approval during the build); fleet-server gains utoipa (`macros`), utoipa-axum, clap (`derive`) and tokio-util, axum's `tokio` and `http1`, and tokio's `rt`, `rt-multi-thread`, `macros`, `net` and `io-util`; fleet-startup gains tokio (`signal`, `macros`; in dev `rt`, `sync`, `time`, `test-util`), and fleet-agent drops tokio's `signal`, which fleet-startup now enables. Cargo.lock gains only utoipa, utoipa-gen, utoipa-axum and its macro helper pastey. No anyhow, and no hyper, hyper-util or reqwest as a direct dependency.
> - **Dependencies.** Group A adds no external crate. fleet-startup uses figment (`toml`, `env`), serde, thiserror, tracing and tracing-subscriber (`env-filter`, `json`); fleet-testkit gains figment (`test`) and serde_json; fleet-server uses fleet-startup, garde (`derive`), serde (`derive`), thiserror and tracing.

- [x] **P6.1** Module layout (see `CLAUDE.md`): `config`, `app` (services), `ports`, `infra/{sqlite,crypto}`, `http/{router,middleware,extractors,handlers,error}`, `grpc`, `cli`.

  > Note (P6.1, as built, group A) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **Deviation: modules arrive with their code** (the user's decision). `crates/fleet-server` starts as a library whose crate doc describes the whole layout, each module with the task that fills it, and the layering (`http` handlers → `app` services → `ports` → `infra`). Group A adds `config` (P6.2); every other module arrives with the group that first puts code in it, so the tree has no empty modules.
  > - **No binary yet.** `afkfleet-server` arrives with the CLI in P6.11 (group E).
  > - fleet-server joins the justfile's `stable_crates` (P9.3 takes it out again).
- [x] **P6.2** 🔴 Config:
  - figment + garde
  - secrets only via `*_file` paths
  - fail fast with a clear message

  > Note (P6.2, from the Phase 6 plan, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **The agent's rules,** through fleet-startup's loader: the file must exist at exactly the given path, `AFKFLEET_SERVER__…` variables override any key, unknown keys are errors in the file and in the environment, parse errors stop at the first, and validation lists every problem at once, sorted by key, without echoing a value.
  > - **Only Phase 6's keys.** Each later phase adds its own, so a key of a later phase is still an unknown key until then (Appendix A's note).
  >
  >   | Key | Default | Rule |
  >   |---|---|---|
  >   | `dev_mode` | `false` | When it's on, the server logs one `warn` at startup |
  >   | `[http] bind` | `127.0.0.1:8080` | An IP address and a port other than 0 |
  >   | `[http] request_timeout_secs` | 15 | 1–300 (1–19 since P8.1, [ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)) |
  >   | `[http] max_body_bytes` | 65536 | 1024–1048576 |
  >   | `[database] path` | required | Absolute |
  >   | `[log] format`, `filter` | `json`, `info` | As the agent's (P5.2) |
  > - **Deviation: `bind` defaults to `127.0.0.1:8080`,** not Appendix A's `0.0.0.0:8080`, so a server started without it never listens on the network. The container config sets `0.0.0.0:8080` itself (P12).
  > - **Port 0 is refused:** a server on a port the OS picks can't be reached by Caddy, and the healthcheck probes the configured port.
  > - **`database.path` is required and absolute:** `/data/afkfleet.db` is a container path, and a relative path would resolve against wherever the server is started, so a start from another directory would silently create a fresh, empty database.
  > - **The dev-mode warning:** "dev mode is on: development-only features are enabled; never run a production server in dev mode". It names no feature, so it stays true as later phases hang more on `dev_mode`.
  > - **Secrets: the rule and a test now, the loader in P7.4.** A key whose name has a `_`-separated part `key`, `token`, `password`, `secret`, `passphrase`, `pepper` or `credential` (singular or plural) must end in `_file` or `_files`, or be on the test's allowlist with a reason. The test walks every key through serde's own field lists, so a new key can't escape it.
  > - **`[log]`** is the agent's section from fleet-startup. The server's filter is `(EnvFilter ∧ azalea_auth cap) ∨ panic reports`, through fleet-startup's `NoRules`: no azalea default and no startup warning.

  > Note (P6.2, as built, group A) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **Shape.** `fleet_server::config::load(path) -> Result<ServerConfig, ConfigError>`, with `ServerConfig { dev_mode, http: HttpConfig { bind, request_timeout, max_body_bytes }, database: DatabaseConfig { path }, log: LogConfig }`, `HttpConfig::DEFAULT_BIND`, `ENV_PREFIX` and `ServerConfig::warn_if_dev_mode()`.
  >   - `ConfigError`, `Problems` and `Problem` are aliases of fleet-startup's generic types over the server's `ProblemKind`: `Missing`, `Empty`, `OutOfRange`, `NotAbsolute`, `SocketAddress`, `PortZero`, `LogFormat` and `LogFilter`. No message echoes a value.
  >   - `fleet_startup::telemetry::NoRules` is the server's `FilterRules`: no defaults, a cap that lets everything through, no warning.
  > - **A dependency edge beyond the plan's list** (the user's approval): `tracing-subscriber` as a fleet-server dev-dependency, so the dev-mode test checks the warning's level through fleet-startup's layer.
  > - **Tests**, red against stubs that compiled (a `validate` that returned fixed defaults, a `warn_if_dev_mode` that did nothing, a `NoRules` cap that blocked everything, a key walker that found nothing): 40 of 106 failed on assertions, then all passed.
  >   - `tests/config.rs` (31 cases, each through `fleet_testkit::jail::in_jail`): the defaults, every key, environment overrides, the bind forms, the dev-mode warning on and off, a missing file, unknown keys in the file and the environment, a later phase's section, a wrong type, the database path, bad binds and port 0, the range edges, bad log settings, and every problem at once without values.
  >   - Unit tests: the secret-name rule (14 names), the walk finding every Phase 6 key, no key holding a secret, the allowlist's entries, and the problem list's text.
  >   - fleet-startup: `NoRules` across 7 filters and 4 targets (`azalea_auth` stays at `info` under `trace`), panic reports under `off`, and no defaults or warning.
- [x] **P6.3** SQLite setup:
  - `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5s`
  - a **write pool with 1 connection** plus a read pool
  - migrations embedded with `sqlx::migrate!` and run at startup
  - offline data in `.sqlx/`, and a CI job running `cargo sqlx prepare --check`
  > Note (P6.3, from group A, the user's decision) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): **sqlx's statement logging is set explicitly** when the connection is configured: every statement at `debug`, slow statements at `warn`. sqlx has logged every statement at `info` in some versions, which the default `info` filter would let through on every request.

  > Note (P6.3, as built, group B, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **Shape.** `infra::sqlite::open(&DatabaseConfig, DatabaseOptions) -> Result<Database, OpenError>` is the startup call: there's no binary yet, so "run at startup" means this function, which group E's `serve` and `migrate` call. It prepares the file, then `Database::connect` builds the write pool, runs every pending migration and builds the read pool.
  > - **One function owns every per-connection setting,** `connect_options(base, PoolRole, &DatabaseOptions)`, for production and tests alike: `foreign_keys=ON`, `busy_timeout=5s`, `synchronous=NORMAL`, `trusted_schema=OFF` (the user's addition: no function with side effects runs from the schema), statements at `debug` and slow ones (from 250 ms) at `warn`, and `create_if_missing(false)`. The write connection adds WAL; the 4 read connections add `query_only=ON`, so a write through them fails instead of bypassing the single writer. Both pools wait at most 5 s for a connection.
  > - **The file is open()'s job:** a missing folder stops startup (`MissingFolder`, nothing created); open() creates a missing file itself, `0600` on Unix, so sqlx never makes one with default permissions; and on Unix a database, `-wal` or `-shm` that group or others can use stops startup with the path, the fix (`chmod 600`) and the Docker Desktop bind-mount case in the message. That covers Phase 6's "only the service user reads the DB file".
  > - **Migration errors** an operator must understand get their own variants: `EditedMigration { version }` and `NewerSchema { version }`.
  > - **Timeouts** are documented in the module instead of a wrapper per query: the acquire timeout, `busy_timeout` and P6.6's request timeout bound every query; migrations are unbounded on purpose.
  > - **Deviation: the offline data lives in `crates/fleet-server/.sqlx/`,** prepared from the crate without `--workspace`: only fleet-server uses sqlx, the Docker allowlist already includes `crates/`, and prepare compiles only fleet-server, never azalea. `SQLX_OFFLINE = "true"` in `.cargo/config.toml` pins every build to it. `just db-prepare` and `just db-check` (in `check` and `ci`) rebuild `target/sqlx-prepare.db` from the migrations, through an absolute URL, since rustc runs the macros in the workspace root.
  > - **The `sqlx-offline` job** installs sqlx-cli pinned and SQLite-only, fetches `main`'s tip, and runs `just migrations-check` (a migration on `main` may never be modified, deleted or renamed) and `just db-check`.
  > - **Tests** (`tests/database.rs`, `tests/database_logging.rs`, real time, never paused) were red against compiling stubs first: 15 of the 20 that run on Windows failed; the 3 Unix-only permission tests run in Linux CI.
- [x] **P6.4** 🔴 First migration and repositories for `users` and `audit_log`, tested with `#[sqlx::test]`.

  > Note (P6.4, as built, group B, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **Schema conventions for every table:** STRICT; entity IDs as lowercase hyphenated v7 TEXT with a CHECK on the exact canonical form; times as `YYYY-MM-DDTHH:MM:SS.mmmZ` TEXT with a CHECK on the shape; CHECKs on closed sets only (`role`, `disabled`, `outcome`), never on sets later phases extend (`action`, `target_type`); append-only logs (`audit_log`, later `chat_messages`) keyed `INTEGER PRIMARY KEY AUTOINCREMENT` and ordered by ID only. IDs and times convert in one place (`infra/sqlite/convert.rs`); IDs are bound as `uuid::fmt::Hyphenated`, never as a plain `Uuid`, which sqlx would bind as a BLOB.
  > - **Two migrations,** `0001_users.sql` (Appendix D in full, `username = lower(username)`, an algorithm-neutral PHC shape on `password_hash`, `password_changed_at` = `created_at` for a new user, and the partial unique index on `role` for `'owner'` from P7.1's note) and `0002_audit_log.sql` (`actor_user_id` references `users` without cascade, `actor_ip` canonical, target type and ID both set or both NULL, metadata NULL or a JSON object). No trigger blocks DELETE: P11.6 deletes old entries.
  > - **The unit of work:** `ports::store::Store` hands out read handles (`users()`, `audit()`) and `write()`, a `WriteTx` started with `BEGIN IMMEDIATE`. Its `commit(entry)` records the audit entry and commits, so a change can't be committed unaudited; dropping it rolls back. There is no unaudited commit (P7 adds one with named exemptions, see P7.1's note). `StoreError::{Busy, Corrupt { table, column, rowid }, Unstorable { table, column }, Backend(source)}` names no sqlx type; `Corrupt` never carries the value.
  > - **Users:** `insert`, `get` and `find_by_username`. `UsernameTaken` and `OwnerExists` are mapped from SQLite's code 2067 and its message, pinned by tests. `User` and `NewUser` live in `ports::users`; `PasswordHash` wraps a `SecretString` and checks the PHC shape the CHECK enforces.
  > - **The audit vocabulary lives in fleet-core** (`audit`): `AuditAction` (a validated dotted name; each phase lists its constants in one `audit_actions!` block, group B none), `AuditOutcome`, `AuditTarget` with a tolerant `RecordedTarget`, and `AuditMetadata`, which never refuses an entry for its size: a text value is cut to the room left (ending in `…`), every free slot keeps room for the largest integer or boolean entry, and a `const` assertion checks that invariant, so the JSON never exceeds 4 KiB and recording never fails on metadata (the PR #25 review). `SECRET_WORDS` moved there from the config test, which now uses it too.
  > - **Tests,** red first against compiling stubs (37 of 144 fleet-server tests failed, and 24 of the 27 store tests against migrations without their CHECKs): `tests/store.rs` (`#[sqlx::test]`, opened through `Database::connect` so the production settings apply), `tests/database.rs` (an edited migration, a full row through `open()`), `tests/migrations.rs` (the checksum snapshot) and the unit tests, including two property tests on the time conversion.
- [x] **P6.5** 🔴 `ApiError` maps each error to a status code and `{ "error": { "code", "message", "request_id", "fields"? } }`.
  - Internal errors are logged with the request ID and returned as a generic 500.
  - Every variant gets an insta snapshot.

  > Note (P6.5, as built, group C, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **Thirteen codes,** one `ApiError` variant each, every code the plan names, so the envelope and the TypeScript union are complete in Phase 6; later phases add only domain-specific codes. `bad_request` 400, `unauthorized` 401, `forbidden` 403, `not_found` 404, `method_not_allowed` 405, `timeout` 408, `conflict` 409, `payload_too_large` 413, `unsupported_media_type` 415, `validation_failed` 422 (with `fields`), `rate_limited` 429, `internal` 500, `busy` 503.
  > - **The message is a fixed sentence per code** (`ErrorCode::message()` in fleet-api-types, e.g. "The server is busy. Try again shortly."), never runtime data; a case that needs other words gets its own code.
  > - **Headers:** a 401 always sends `WWW-Authenticate: Bearer`, with no realm and no error detail. A 429 sends `Retry-After` from the error's own delay, in whole seconds, rounded up. A 503 sends `Retry-After: 1` (`BUSY_RETRY_AFTER`), so the app has one retry rule for both.
  > - **Conversions:** `StoreError::Busy` becomes `busy`, and `Corrupt`, `Unstorable` and `Backend` become `internal`. `AuthzError::NotFound` and `Forbidden` become 404 and 403, and `WrongResource` becomes `internal`, logged at `error` (P7.8's note). A garde `Report` becomes `validation_failed`, one field error per broken rule in garde's order and path notation (`steps[0].angle`).
  > - **The request ID.** `IntoResponse` can't see the request, so `ApiError::into_response` sets the status and headers and leaves a private marker in the response's extensions, with an empty body. The `render_errors` middleware, which group D places inside the request-ID layer, reads the ID from the request's `x-request-id` (`REQUEST_ID_HEADER`) as a `BoundedText<64>`, takes the marker, logs, and writes the body with `serde_json::to_vec`. A response without a marker passes unchanged.
  >   - A missing, empty or non-text ID is a wiring bug: the body says `"unknown"` and it's logged at `error`.
  >   - If writing the body ever failed (it can't for these types), a fixed, hard-coded `internal` body with `request_id: "unknown"` is sent and the failure logged, never serde's text; a test checks that the fixed body equals what serde would write.
  > - **Logging:** `internal` at `error`, with `request_id` and the whole source chain joined as "a: b: c" (e.g. "the database failed: disk I/O error"), cleaned by `sanitize_untrusted` and cut at 1024 characters; `busy` at `warn`, with `request_id`; 4xx codes not at all, since group D's trace layer logs every request's status.
  > - **Tests**, red against stubs that compiled (every variant 500 and `internal`, conversions that all gave `internal`, a middleware that passed responses through): 51 of 57 failed on assertions, then all passed. 13 snapshots, one per variant, hold the status, the `content-type`, `retry-after` and `www-authenticate` headers and the body, through the middleware with a fixed request ID; they were read before accepting. A garde test struct (length and range rules) proves that the 422 body names rules, never values: `hunter2-secret`, `7654321` and `12345` appear nowhere in it, and garde's texts fit their caps uncut.
- [x] **P6.6** 🔴 Middleware stack, in this documented order:
  1. request-id (set and propagate)
  2. trace (no bodies, sensitive headers redacted)
  3. `CatchPanicLayer`, which turns a panic into a 500
  4. timeout (15 s)
  5. body limit (64 KiB)
  6. security headers

  Rate limiting is added in P7.

  > Note (P6.6, from group C, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **`render_errors` goes inside the request-ID layer,** so every `ApiError` gets its ID. Errors that don't pass through `ApiError` (tower-http's timeout and body-limit responses, 405, the 404 fallback) must end up in the same `ErrorResponse` envelope with a `request_id`, e.g. by mapping them to `ApiError` or by having the middleware wrap any error status without a marker. A 405 keeps axum's `Allow` header. An integration test sends one request for each error the router can produce and asserts that every response has the JSON body with a `request_id`, so a missing body is caught, not just safe.
  > - **The request ID is always generated by the server.** tower-http's `SetRequestIdLayer` keeps an `x-request-id` the client already sent, so the client's header is removed or overwritten before the ID is set; otherwise a client could choose the ID that gets logged and echoed back (fake or colliding IDs, log injection). The tests send a request with its own `x-request-id` and assert that the logged and returned ID is a fresh server-generated one. The ID fits `BoundedText<64>` unchanged: at most 64 characters, none that `sanitize_untrusted` strips.

  > Note (P6.6, as built, group D, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **Deviation: the order.** The security headers sit outermost, so every response gets them, including those the layers below make themselves: a refused request, a timeout, a body-limit 413, a panic.
  >   - **The stack, outermost first:** sensitive headers, security headers, request ID, trace, `render_errors`, catch-panic, timeout (`[http] request_timeout_secs`, 408), body limit (`[http] max_body_bytes`), then the router with its 404 and 405 fallbacks.
  >   - `http::middleware::apply(routes, &HttpConfig, RequestIds)` adds them, and its module docs give each layer's reason.
  > - **The request ID** is a version 7 `RequestId` (a new `define_id!` in fleet-core), minted with `system::mint` from the `Clock` and `SecureRandom` ports: lowercase hyphenated, 36 characters.
  >   - **Deviation: the layer is the server's own,** not tower-http's `request-id`, which turns on uuid's `v4` and `rng`.
  >   - It removes every `x-request-id` the client sent, then sets the new ID on the request and on the response, replacing any value a handler set.
  >   - If minting fails, the request is refused with the internal error (`request_id: "unknown"`) and the cause is logged at `error`; the handler never runs.
  > - **Deviation: two security headers more than §7.6 listed.** Every response carries `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `Cache-Control: no-store`, `Content-Security-Policy: default-src 'none'; frame-ancestors 'none'` and `X-Frame-Options: DENY`. They're set with `overriding`, so a handler can't weaken them.
  > - **Sensitive headers:** `Authorization`, `Proxy-Authorization` and `Cookie` on requests, and `Set-Cookie` on responses, are marked before anything else runs.
  > - **Deviation: the trace layer is the server's own,** not tower-http's `trace`, whose response hook can't see the route.
  >   - An `info` span `request` carries `request_id`, `method` and `route`. `method` is one of the nine standard methods or `"<other>"`. `route` is axum's `MatchedPath` template or `"<unmatched>"`, never the path a client sent or its query string.
  >   - One line, "request finished", carries `status`, `latency_ms` (a float, to the microsecond) and, for an error, `code`. It's logged at `info`, or at `debug` for `/health/…`.
  >   - It logs no header and no body. A test sends credentials and a JSON body with marker values, and no marker appears in any line at `debug`.
  > - **The safety net:** `render_errors` gives every 4xx or 5xx without a marker the envelope.
  >   - The code comes from the status. A status without a code of its own keeps its status and says `bad_request` (4xx) or `internal` (5xx). A bare 422 has `fields: []`.
  >   - The old body is replaced, and `Content-Length` and `Content-Encoding` are removed. Every other header stays, such as axum's `Allow` and a limiter's `Retry-After`.
  >   - A missing `WWW-Authenticate: Bearer` (401) or `Retry-After: 1` (503) is added.
  >   - Every bare error status is logged at `error` as a wiring bug, except the timeout's 408 and the body limit's 413.
  >   - The 404 and 405 fallbacks and the panic handler return `ApiError` themselves.
  > - **A panic** answers with the internal error, whose source is the fixed "a request handler panicked". The payload reaches the log only through fleet-startup's panic hook, whose line carries the request span. That's tested with a stand-in hook in a test binary of its own, since the hook is process-wide.
  > - **Group C's test changed:** `a_response_without_an_api_error_passes_unchanged` sent a bare 418 and expected it unchanged. The safety net now wraps a 418, so the test sends a 201 (`a_success_without_an_api_error_passes_unchanged`), and the 418 is one of the safety-net cases.
  > - **Tests**, red against compiling stubs: pass-through ID and trace layers, no safety net, no security or sensitive headers, no timeout or body limit, and tower-http's default panic response. 53 of 110 failed on assertions, then all passed.
  >   - `tests/middleware.rs` sends one request per error: 404, 405, 408, 413 by its length and when read, a panic, and a failed mint. It also sends one per bare status. Each gets a snapshot, 16 in all, read before accepting.
  >   - The timeout's tests run on paused time.
- [x] **P6.7** 🔴 `GET /health/live` and `GET /health/ready` (DB ping), with no internal details in the response.

  > Note (P6.7, as built, group D, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **Shape.** `http::router::router(AppState, &HttpConfig, RequestIds)` holds the two routes, without the `/api/v1` prefix (Appendix B), behind P6.6's stack.
  >   - The handlers live in `http::handlers::health`.
  >   - `ready` calls `app::health::HealthService::ready()`, which calls the new `Store::ping()`.
  >   - `ping()` is `SELECT 1` (`query_scalar!`) on the read pool, so it never competes with the one write connection.
  > - **Answers.** Both checks answer `200` with an empty body.
  >   - `live` checks nothing else.
  >   - A failed `ready` answers like any store failure, through `From<StoreError>`. A busy database (no read connection within the acquire timeout) gives `busy`: 503, `Retry-After: 1`, logged at `warn`. Any other failure gives `internal`: 500, its source chain logged at `error` with the request ID.
  >   - No response holds anything of the cause.
  > - **Bounded like every query:** the pools' 5 s acquire timeout, `busy_timeout` and the request timeout. There's no extra timeout.
  > - **Logging.** The trace layer logs both routes at `debug`, so a probe every few seconds doesn't flood the log. A failed check is still logged by `render_errors`.
  > - **Tests**, red against a `ping` that always failed and handlers that answered 500: 10 of 44 failed on assertions, then all passed.
  >   - `tests/health.rs` runs in real time against a database from `#[sqlx::test]`. It covers 200 and `HEAD` for both routes, 405 with `Allow`, `live` with a closed database, `ready` busy (every read connection held, a 1 s acquire timeout), and `ready` with a closed database.
  >   - It has 4 snapshots, read before accepting.
  >   - `tests/store.rs` checks the ping: it succeeds, it doesn't wait for a held write connection, it's busy while every read connection is taken, and it's a backend error on a closed database.
- [x] **P6.8** 🔴 Audit service:
  - append-only: the trait has only `record` and `list`
  - each entry: actor, IP, action, target, outcome, metadata (no secrets), timestamp

  > Note (P6.8, as built, group B, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **`app::audit::AuditService { clock, store }`:** `entry(action, outcome)` returns a `NewAuditEntry` stamped with the `Clock` (milliseconds), with the builder methods `actor`, `ip`, `target` and `metadata`; `record(entry)` runs its own short write transaction (`write()`, then `commit(entry)`), for a failure or denial whose change was rolled back; `list(page)` reads through the read pool. A use case that changes state builds its entry here and passes it to `WriteTx::commit`, so every timestamp comes from one clock.
  > - **Append-only** means the store has exactly two operations for the log, `AuditWrites::record` inside a `WriteTx` and `AuditReads::list` (P6.4's note); nothing updates or deletes an entry, and no trigger blocks P11.6's retention.
  > - **No secrets:** the metadata's rules live in fleet-core's `AuditMetadata` (P6.4's note), checked when it's built.
  > - **Tests** (`tests/audit_service.rs`, `ManualClock`, IDs minted from `SeededRandom` and compared with each other), red against a stub that stamped the epoch, recorded nothing and listed nothing: 5 of 6 failed.
- [x] **P6.9** `fleet-api-types`:
  - DTOs with serde, garde and `ts-rs`
  - `#[serde(deny_unknown_fields)]` on request DTOs
  - `just gen` exports them to `packages/ui/src/generated/`
  - CI job `ts-types-fresh` fails on any diff

  > Note (P6.9, as built, group C, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **What's in it.** The error envelope's DTOs (`ErrorResponse`, `ErrorBody`, `FieldError`, `ErrorCode`; P6.5) and three building blocks for every later DTO:
  >   - **`SafeInt`**: the only way a 64-bit integer crosses the API. It's a range-checked `i64` within ±(2^53 − 1), the integers JavaScript reads exactly; reading a number outside that range fails. In TypeScript it's `export type SafeInt = number;`. `SafeInt::MAX` is the crate's own constant, and a test checks that it equals `AuditMetadata::MAX_INT`. Its serde is written by hand, since ts-rs can't read serde's `try_from`/`into` and warns about them on every build.
  >   - **`Open<E>`**: a response value a later version may extend, such as `ErrorCode`. A value this version doesn't know still reads back, as text cleaned by `sanitize_untrusted` and cut at 64 characters; only a non-string fails. The server can build one only from a known value. `E` implements `OpenEnum` (`ALL`, `as_str`), and a field holding one carries `#[ts(as = "E")]`, so the TypeScript union lists exactly the known values.
  >   - **`BoundedText<MAX>`**: response text, cleaned by `sanitize_untrusted` and cut at `MAX` characters both when built and when read, never failing on content. It has no TypeScript type of its own, so a field without `#[ts(type = "string")]` doesn't compile.
  > - **Requests and responses.** Responses derive `Serialize` and `Deserialize` without `deny_unknown_fields`, so `fleet-client` reads them and a newer server's extra field doesn't break an older app; each round-trips in a test. Requests are `Deserialize` + garde + `deny_unknown_fields`, which the crate docs state. **Deviation:** Phase 6 has no request DTO, so garde joins fleet-api-types with P7's first one.
  > - **The export.** `typescript::export(scratch)` writes the root DTOs through ts-rs's `export_all` with `Config::new()` (which ignores the `TS_RS_*` variables) into an empty scratch folder, and refuses one that isn't empty. No DTO carries `#[ts(export)]`, so tests never write files. One file per type, no index file.
  >   - `just gen` (`typescript::write`) exports into a fresh temp folder inside `target/gen/`, then syncs `packages/ui/src/generated/` with it. The sync deletes only `.ts` files that start with ts-rs 12.0.1's exact header, and stops before deleting anything, naming the entry, if the folder holds any other file or a subfolder.
  >   - `just gen-check` (`typescript::check`), in `just check` and `just ci`, compares the folder with a fresh export and names every missing, extra or changed file. CI's `ts-types-fresh` job (ubuntu, Rust only) runs it.
  > - **`packages/ui/src/generated/`** holds only the generated files until P8.2 scaffolds `packages/ui` around it. With no package.json there, pnpm ignores it (checked: `pnpm -r ls` lists only the root). Biome already skips it, and `.gitattributes` marks it `linguist-generated`.
  > - fleet-api-types joins `stable_crates`.
  > - **Tests**, red against stubs that compiled (a `SafeInt` that took any value, text that wasn't cleaned, an `Open` that knew nothing, empty messages and names, an export, comparison and sync that did nothing): 54 of 81 failed on assertions, then all passed. Among them: a sync over a previous real ts-rs export (a stale `Probe.ts` included) proves that it recognizes ts-rs's real header, and every generated file is checked for `bigint`. After the implementation, the coverage report showed error paths without a test; their tests followed in their own commit, and one found a bug: `sync` noticed a subfolder in the scratch folder only after it had deleted and written files. It now checks both folders before changing anything.
- [x] **P6.10** OpenAPI via utoipa. `/api/openapi.json` is served only when `dev_mode = true`.

  > Note (P6.10, as built, group E, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **The schemas live in fleet-api-types,** always on like ts-rs: the DTOs derive `ToSchema` next to `TS`. `SafeInt` is a component of its own (an integer within ±(2^53 − 1)), as it's a type of its own in TypeScript. A `BoundedText<MAX>` field carries `#[schema(schema_with = BoundedText::<MAX>::schema)]` (a string with `maxLength` MAX; utoipa's derive can't read a const generic, so a field without it doesn't compile), an `Open<E>` field `#[schema(value_type = E)]`, and an optional field `#[schema(nullable = false)]`. `openapi::components()` lists the root DTOs, like the TypeScript export.
  > - **Deviation: four fields lose their description.** utoipa doesn't carry a doc comment onto a `schema_with` field, so the text fields have no description in the document; the TypeScript keeps it.
  > - **A test keeps the two exports equal:** a test-only renderer turns every component into the TypeScript declaration ts-rs writes and compares them (comments stripped, whitespace collapsed); the component names must equal the exported files; and what TypeScript can't express is checked directly: `SafeInt`'s range, every text field's cap against its constant, an open field's enum against `E::ALL`. The renderer fails on any construct it doesn't handle, and a test proves it on 15 of them. **Deviation: utoipa's `preserve_order`** (the user's approval during the build), since utoipa wrote properties alphabetically and ts-rs in field order.
  > - **The document:** routes are registered through utoipa-axum's `OpenApiRouter` with `#[utoipa::path]` on each handler, so a route can't exist without its entry. Each operation lists its own statuses (`live` 200; `ready` 200, 500, and 503 with `Retry-After`) plus a `default` response with `ErrorResponse`. `info`: title "afkfleet", the crate's version, GPL-3.0-or-later, no contact and no servers. The document doesn't list itself.
  > - **Serving:** `router()` takes `ApiDocs::{Served, Hidden}` (`ApiDocs::for_dev_mode`). In dev mode `GET /api/openapi.json` answers the document as JSON, written per request, behind the whole middleware stack; otherwise the route doesn't exist and the path answers exactly like an unknown one.
  > - **Tests,** red against compiling stubs: in fleet-api-types 9 of 26 failed (an empty component list, `SafeInt` without its range, text without its cap); in fleet-server 8 of 9 (no route in dev mode, no schemas). One snapshot holds the whole document, read before accepting.
- [x] **P6.11** CLI skeleton: `serve`, `migrate`, `healthcheck`.

  > Note (P6.11, from group A, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **The dev-mode warning.** `serve` calls `ServerConfig::warn_if_dev_mode()` right after logging starts.
  > - **The healthcheck's address.** When `bind` is an unspecified address (`0.0.0.0` or `::`), as the container config will set, the healthcheck probes loopback (`127.0.0.1` or `::1`) on the same port instead: connecting to `0.0.0.0` only happens to work on Linux and fails on Windows.
  > - **The dev config's database path must be absolute,** and a committed file can't hold one that works on every machine. How `just dev-server` provides it is group E's question, e.g. a `$`-parameter built from `justfile_directory()`.

  > Note (P6.11, from group D) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): `serve` builds `http::router::router` with `RequestIds` over `SystemClock` and `OsRandom` and the `Database` as the health service's store. The healthcheck probes `/health/ready`, whose only success is `200` with an empty body.

  > Note (P6.11, as built, group E, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **The agent's shape** (ADR-0014): `afkfleet-server`; `fleet_server::cli` holds the clap structs and each command's logic; main.rs handles clap's errors (a `healthcheck` usage error exits 1), loads the config before any runtime, writes errors from before logging to stderr as one line, and **uses no anyhow**. `--config <PATH>` is required on every command.
  > - **`serve`:** logging, `warn_if_dev_mode()`, a multi-threaded runtime, then inside it the stop signals and the bound listener, then `serve::run`, which opens the database (with its migrations) and serves `serve::app` over `SystemClock`, `OsRandom` and the database. Startup errors are one `error`, "the server can't start", with the whole chain; the last line, "the server stopped", carries the exit code.
  > - **A free port without weakening the config:** `serve::run` takes an already-bound tokio `TcpListener`. main.rs binds `[http] bind`; tests bind `127.0.0.1:0`. The port-0 rule is unchanged.
  > - **`migrate`** logs like `serve`: "the database is migrated" (`info`, with `path`) or "the database can't be migrated" (`error`). It opens the database through `open()` and closes it with the new `Database::close_within(DATABASE_CLOSE)` (5 s). Exit codes 0, 1, 2.
  > - **`healthcheck`:** our own HTTP/1.1 GET over tokio's `TcpStream` on a current-thread runtime, `Connection: close`, one 3 s timeout for the whole exchange, at most 16 KiB read, no TLS. Healthy only on `HTTP/1.1 200` with an empty body; an unspecified bind is asked on loopback on its port. One line on stdout, exit 0 or 1; no logging. It loads the whole config, like the agent's, so it needs the server's environment variables too. The tests reuse `healthcheck::get` as their HTTP client.
  > - **`just dev-server`** runs `deploy/dev/server.toml` (dev mode, pretty logs, default bind, no database path) with `$AFKFLEET_SERVER__DATABASE__PATH`, a `$`-parameter defaulting to `target/dev-server/afkfleet.db` under `justfile_directory()`; Node creates the folder first.
  > - **Tests,** red against compiling stubs (35 of 185 failed): the command line, the probe and its parser, `close_within`, the dev config, and the real binary: missing and invalid configs, usage errors, each command's help, the version, `migrate` twice and into a missing folder, `healthcheck` with nothing listening, against an in-process server, and through an unspecified bind.
- [x] **P6.12** 🔴 Graceful shutdown: axum's `with_graceful_shutdown` plus a `CancellationToken`.

  > Note (P6.12, from group B) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): `Database::close()` closes the read pool, then the write pool, and waits for every checked-out connection, so shutdown bounds it with a timeout.

  > Note (P6.12, as built, group E, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **The signals moved to fleet-startup** (§4) in a pure-move commit: `Signal`, `ShutdownSignals`, `OsSignals` and their tests. fleet-agent re-exports them and keeps its own test sources; fleet-server's tests have their own `ChannelSignals`.
  > - **One deadline, `request_timeout + 5 s`, from the first signal; no new key.** The drain may take up to `[http] request_timeout_secs`, which the timeout layer already enforces per request; the database closes with the rest, at least `DATABASE_CLOSE` (5 s). A later signal is only logged.
  > - **Connections:** `axum::serve(…).with_graceful_shutdown(token.cancelled_owned())`. axum spawns each connection itself and never aborts them, so at the drain limit the server stops waiting, logs one `warn` and closes the database; by then the timeout layer has cut every handler, so a leftover connection holds no database connection, and the runtime's 1 s shutdown ends it.
  > - **Exit codes, as the agent's:** 0 stopped by a signal (even with connections cut at the drain limit), 1 startup error, 2 usage error, 4 the server ended unasked or the database didn't close by the deadline; 3 is unused.
  > - **`serve::drive`** holds the sequence, generic over the server's and the close's futures, with biased `select!`s whose cancel-safety is commented.
  > - **Tests,** red against a `drive` that returned at once (13 of 29 failed): its timing on paused time with stand-in futures; `serve` in real time over sockets and SQLite (an in-flight request finishing while new connections are refused, an idle keep-alive connection, a held database connection failing at the deadline); and on Unix the real binary stopping on SIGTERM with exit 0, "the server stopped" last. That test finds a free port, then binds it, and retries only when the server exits 1 with the bind error for that port before listening, at most 3 times (the user's addition).

**Security:**
- No CORS layer.
- `Cache-Control: no-store` on API responses.
- No secrets in committed TOML.
- The DB file is only readable by the service user.

**DoD:**
- An integration test boots the app in-process on a temporary DB and passes the health checks.
- Snapshots reviewed.
- `just ci` green.

> Note (DoD, group E): checked point by point.
> - **The integration test** (`tests/serve/boot.rs`) runs `serve::run` with the binary's wiring on a fresh temporary database and a `127.0.0.1:0` listener: `GET /health/live` and `/health/ready` answer 200 with an empty body over a real connection, the healthcheck reports healthy, and a signal stops it with exit 0, the database closed and every migration applied. A temporary break in the wiring, never committed, made it fail.
> - **Snapshots:** every snapshot of the phase was read before accepting: the migrations' checksums (group B), 13 error responses (group C), 16 middleware and 4 health responses (group D), and the OpenAPI document (group E).
> - **`just ci`:** CI runs every recipe of `just ci`, one per job, on the group E PR.
> - **Security:** no CORS layer; `Cache-Control: no-store` on every response; no secret in any committed TOML (Phase 6's config has none, and the secret-name test guards every key); the database file is owner-only on Unix, and the server refuses one others can read.

---

### Phase 7: Authentication & authorization
**Goal:** Invite-only login with 2FA, rotating tokens, central authorization and layered rate limits, all backed by adversarial tests.
**Introduces:** `argon2`, `getrandom`, `sha2`, `subtle`, `base64`, `zeroize`, `chacha20poly1305`, `totp-rs`, `zxcvbn`, `governor`, `hmac`, `rpassword`; no `tower_governor` (group C, the user's decision).

> Note (P7):
> - **Five group branches.** At the user's request, Phase 7 is built in group PRs like Phases 2–6. Each group has one branch and one commit per task, and the groups run in this order, each after the previous PR is merged:
>
>   | Group | Branch | Tasks |
>   |---|---|---|
>   | A | `p7/p7.2-p7.5-passwords-vault-owner` | P7.2, P7.4 (with the `*_file` loader), P7.5; `fleet_core::auth` with the password length rule |
>   | B | `p7/p7.3-p7.8-sessions-and-auth-user` | P7.1's `sessions` (with `stepped_up_at`), P7.3, P7.8 |
>   | C | `p7/p7.10-p7.11-routes-and-rate-limits` | P7.1's `login_attempts`, P7.11, P7.10 |
>   | D | `p7/p7.6-p7.7-login-and-2fa` | P7.1's `user_mfa` (and wherever the login challenges live), P7.7; P7.6's `login`, `login/mfa`, `refresh`, `logout`; `/me/mfa/*`; `POST /auth/step-up` |
>   | E | `p7/p7.6-p7.9-accounts-admin-wrap-up` | P7.1's `invites`; P7.6's `register`, `/me`, `/me/password`, `/me/sessions`; P7.9; the DoD, the threat model and the wrap-up |
>
>   - **The guards come before any endpoint.** Every new endpoint needs its rate-limit class and route-coverage entry from day one (security rule 3), so the guards (B, C) come before the first endpoint (D).
>   - **Tasks spread over groups:** P7.1 and P7.6 are ticked in E.
>   - Each later group asks its own implementation questions in its session.
> - **Decisions.** The user answered the Phase 7 plan's open questions on 2026-10-11, one at a time. [ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md) records them; the notes below summarize what changes a task.
> - **Security tests and audit events ship with the feature** (the user's decision).
>   - **Audit actions:** every new write adds its action to Phase 7's one `audit_actions!` block in the group that adds the write:
>     - A: create-owner
>     - B: token reuse
>     - C: the lockout
>     - D: login, logout, 2FA
>     - E: password, roles, invites
>   - **Service-level P7.12 tests** go in the service's group:
>     - B: refresh reuse, a disabled user through `AuthUser`
>     - C: lockout counting
>     - D: TOTP replay
>   - **End-to-end HTTP tests** go in the endpoint's group:
>     - D: enumeration, lockout through login, expired sessions and refresh reuse through `/auth/refresh`
>     - E: escalation, disabling, invite reuse and expiry
>   - **E only checks:** the DoD, each P7.12 item named with its test, the threat model's auth section, and the P7.12 and P7.13 ticks.
> - **Pure rules live in `fleet_core::auth`** (the user's decision): the lockout schedule, session expiry, the refresh outcome, TOTP acceptance and replay, invite validity, the step-up window and the password length rule, with `DateTime<Utc>` passed in. Each group adds the rules its feature needs. The crypto stays in fleet-server.
> - **Tests drive the real router with `oneshot`** (the user's decision):
>   - the full stack and the limiter
>   - `#[sqlx::test]` SQLite, `ManualClock` and `SeededRandom`
>   - the peer address as axum's `ConnectInfo`
>   - one real-socket test for `into_make_service_with_connect_info`
>
>   fleet-client's full-stack tests stay P8.1's.
> - **Coverage gates as lists of path prefixes** (the user's decision).
>   - **Auth:** fleet-core's `auth`, `app/auth`, `app/users`, `app/invites`, `infra/crypto/` except the vault, `http/extractors/`, the rate-limit middleware and the route registry.
>   - **Vault:** the vault and the `*_file` loader.
>   - **Dead prefixes fail:** a listed prefix that matches no file in the report fails the gate.
>   - **Adding prefixes:** each group adds a prefix only when it creates that module.
>   - **Required from A on:** both gates are `required` from group A on.
> - **Dependencies (group A, the user's approval).**
>   - **New in `[workspace.dependencies]`:**
>     - argon2 0.6.0, no default features; `alloc`, `password-hash`, `zeroize`
>     - chacha20poly1305 0.11.0, no default features; `alloc`, `zeroize`
>     - zxcvbn 3.1.1, no default features
>     - rpassword 7.5.4
>     - base64 0.23.1, no default features, so no `simd-unsafe`
>     - subtle 2.6.1
>     - zeroize 1.9.0
>   - **fleet-server:** all of them, plus tokio's `sync`.
>   - **fleet-startup:** secrecy and zeroize, and tempfile in dev.
>   - **The profile:** `[profile.dev.package]` compiles argon2, blake2 and zxcvbn at `opt-level = 3`.
> - **Dependencies (group C, the user's approval).**
>   - **No tower_governor:** 0.8.0 hard-codes governor's quanta clock, forces governor's and axum's default features on the workspace, and rounds its wait down. fleet-server uses the workspace's governor with `std` only, behind its own middleware.
>   - **New in `[workspace.dependencies]`:** hmac 0.13.0 (fleet-server enables `zeroize`), and ipnet 2.12.2, a fleet-server dev-dependency only. Both were already in Cargo.lock, which gains only the direct edges.

- [x] **P7.1** 🔴 Migrations and repositories: `sessions` (with token families), `invites`, `login_attempts`, `user_mfa`.

  > Note (P7.1, from the Phase 7 plan, the user's decision) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **each table arrives with its first user,** in its own migration, so its columns are designed together with the feature:
  > - `sessions` with `stepped_up_at` (B)
  > - `login_attempts` (C)
  > - `user_mfa`, and wherever the `mfa_token` and setup token live (D)
  > - `invites` (E)
  >
  > The task is ticked in E.

  > Note (P7.1, from Phase 6, group B, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **Follow the schema conventions** of P6.4's note: STRICT, canonical TEXT IDs and millisecond TEXT times with their CHECKs, CHECKs only on closed sets.
  > - **Unaudited writes.** `WriteTx::commit` always records an audit entry. The first write that needs none (a session's `last_used_at`) adds `commit_unaudited(reason)`, whose reason is an enum of named exemptions (`SessionTouch`, later `EventBatch`), each with its own ADR note, never free text.

  > Note (P7.1, from the group A review): **IDs aren't secrets.** v7 IDs reveal their creation time. Invite codes and session tokens come from `getrandom`, never from an ID, and access is decided by `authorize()`, not by how hard an ID is to guess.

  > Note (P7.1, as built, group B) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **`0003_sessions.sql`.**
  > - **`sessions`**, one row per login's token family, its ID (`SessionId`) the same across every refresh: `user_id` (references `users`, no cascade), `access_hash` and `refresh_hash` (32-byte BLOBs, UNIQUE), `created_at`, `refreshed_at`, `last_used_at`, `stepped_up_at` and `revoked_at` (NULL until set). No expiry columns: `fleet_core::auth` computes them from `created_at` and `refreshed_at` with the running config. No IP address or user agent.
  > - **`rotated_refresh_tokens`** `(hash, session_id, rotated_at)`: every refresh token a session replaced, ON DELETE CASCADE, so reuse of any of them is recognized while the session exists.
  > - **Indexes:** `sessions(user_id)`, `sessions(created_at)` (P11.6's cleanup), `rotated_refresh_tokens(session_id)`.
  > - **The repository** has what B uses: insert, the joined lookup by access hash, and inside the write transaction the lookup by refresh hash (current or rotated), rotate, revoke and the conditional touch. `WriteTx::commit_unaudited(UnauditedWrite)` arrives with its first exemption, `SessionTouch`.
  > - **Tests,** red against compiling stubs: 21 of 26 repository tests and 3 of 8 `TokenHash` tests failed on assertions, then all passed.

  > Note (P7.1, group C, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **`0004_login_attempts.sql`.**
  > - **One row per username with a running failure streak,** keyed by `username_key`: HMAC-SHA256 of the normalized `Username` under a lockout key derived from the vault's active key (P7.10's note), a 32-byte BLOB. Never the name, since people type their password into that field.
  > - **Columns:** `failures` (`>= 1`), `last_failure_at`, `locked_until` (NULL until a lock). No `user_id`, so an unknown name's row looks exactly like a real user's.
  > - **Index** on `last_failure_at`, for the purge of expired streaks.
  > - **The repository:** `find` (read pool and transaction), `upsert`, `delete`, and `purge_stale(failed_by, now, limit)` (a `rowid IN (SELECT … LIMIT ?)` subquery). Purge batches commit with the new exemption `UnauditedWrite::LoginAttemptsPurge`.
  > - **Tests,** red against compiling stubs: 14 of 18 failed, then all passed.

  > Note (P7.1, from group D): **One Owner.** The `users` migration adds a partial unique index on `role` for `'owner'`. `authorize()` also refuses to act on any other user with the Owner role (ADR-0010).

  > Note (P7.1, group D, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **`0005_user_mfa.sql`.**
  > - **The login challenges live in memory,** not in a table: the mfa token and the setup token are held as SHA-256 hashes, at most one login challenge and one replace token per user, and a restart drops them (P7.7's note).
  > - **`user_mfa`** holds confirmed enrollments only, so a user has 2FA exactly when their row exists: `user_id` (the key, references `users`, no cascade), `secret_envelope` (a vault envelope v1 of the 20-byte TOTP secret, 65 bytes, bound by its AAD to the user's ID and `totp_secret`), `last_used_step` (`>= 0`) and `confirmed_at`. No `key_id` column: the envelope's header carries it.
  > - **`recovery_codes`** `(hash, user_id, used_at)`: plain SHA-256 of the normalized code, safe only because each code has at least 80 bits from `SecureRandom` (100 in fact). Not keyed, since a key derived from the vault's active key would make every code unusable after P9's key rotation. A used code keeps its row; indexed on `user_id`.
  > - **The repository:** `find` (read pool and transaction), `find_unused_code`, `insert` (a second first enrollment is refused), `replace`, the conditional `advance_step` (only a later step, and only while `confirmed_at` is unchanged), `replace_codes` and the conditional `use_code`. The sessions gain `refresh_hash_exists`, `revoke_all_for_user(user, except)`, `find` and `stamp_step_up`; the users `get` in a transaction and `set_password_hash`, which never touches `password_changed_at`.
  > - **Tests,** red against compiling stubs: 27 of 39 failed, then all passed. The migrations' snapshot gained line 5.

  > Note (P7.1, group E, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **`0006_invites.sql`**, the last of the four tables.
  > - **Columns:** `id`; `code_hash` (SHA-256 of the `afk_iv_…` code, a 32-byte BLOB, UNIQUE); `role` (`member` or `admin` only: an Owner invite can't exist); `created_by` and `used_by` referencing `users` without cascade, `used_by` UNIQUE (one invite per user); `created_at` and `expires_at` (stored at creation from `[auth] invite_ttl_hours`, so a config change doesn't move it); `used_by`, `used_at` and `revoked_at` NULL until set. A used or revoked invite keeps its row.
  > - **Belt CHECKs behind the services:** a user and the time of the use come together; an invite is never both used and revoked; it expires after its creation (the fixed time form compares as text). Each has a refusal test.
  > - **Indexes** on `created_by` (the cap and revoking a creator's pending invites) and `expires_at` (P11.6's cleanup).
  > - **The repository** (`ports::invites`, `infra::sqlite::invites`): `find` and `find_by_code_hash` on the read pool and in the transaction, `list` (newest first, every state, joined with the people's names, one row more than the page), `insert`, `count_pending_by`, and the conditional `use_invite`, `revoke` and `revoke_pending_by` (each only while unused, unrevoked and unexpired; the last with `RETURNING`). `ports::page` holds the shared `PageLimit` (1–100, 50 by default), `Page` and `Paged::split`.
  > - **fleet-core:** `InviteId`, `AuditTarget::Invite` (`invite`) and `auth::invite` (`InviteTtl` 1–720 h, 72 by default; `invite_state`: used, then revoked, then expired at `now >= expires_at`; `MAX_PENDING_INVITES` = 20 and `pending_cap_reached`).
  > - **Tests,** red against compiling stubs (a rule that kept every invite pending and accepted every lifetime, a target named like a session, a page that never split, a repository that found and wrote nothing): fleet-core 18 of 25 rule tests and the two target tests, fleet-server 6 of 11 page tests and 13 of 20 repository tests failed on assertions. The repository tests that passed check the table's CHECKs and foreign keys, which the real migration already enforces, and an unknown lookup. Then all passed. Snapshots, read before accepting: the serialized IDs gained `invite`, the migrations' line 6.
- [x] **P7.2** 🔴 `PasswordService`:
  - Argon2id with the configured parameters
  - runs in `spawn_blocking` behind a semaphore
  - dummy hash for unknown users
  - password policy (length, zxcvbn)

  > Note (P7.2, from Phase 6, group C, the user's decision) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): a full argon2 queue answers `busy` (503, `Retry-After`), like a busy database (Plan.md §6's overload row).

  > Note (P7.2, group A, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **Ports and adapters.** `ports::passwords::Passwords` with `hash(&NewPassword, &Username)`, `verify` and `verify_unknown_user`, implemented by `infra::crypto::passwords::Argon2Passwords`.
  > - **The queue.**
  >   - **Admission:** at most 4 × `argon2_max_concurrent` callers wait (`try_acquire`); past that, `busy` at once.
  >   - **The hashing permit** is taken with `acquire_owned()` and moved into the `spawn_blocking` closure. A cancelled caller never frees a slot while argon2 still runs.
  >   - **Tests use a blocking stand-in** for both rules.
  > - **The dummy hash** is made at startup with the configured parameters, through a private path without the strength check. `verify_unknown_user` takes the real check's queue, permit and verify call, and always answers "wrong password".
  > - **`fleet_core::auth::NewPassword(SecretString)`.**
  >   - **Length:** 12–128 `chars()`, no trimming and no normalization.
  >   - **Control characters** are refused.
  >   - **Errors:** `NewPasswordError::{TooShort, TooLong, ControlCharacter}`, carrying only the limits.
  > - **zxcvbn** runs inside `hash`, in the same blocking task, before argon2, with score ≥ 3.
  >   - **User inputs:** the username, "afkfleet" and "minecraft".
  >   - **Weak passwords:** `HashError::Weak` carries `fleet_core::auth::{WeakWarning, WeakSuggestion}`, our own enums, mapped from zxcvbn's by exhaustive `match`es.
  > - **`[auth]`'s argon2 keys.**
  >   - **Ranges:** memory 19456–1048576 KiB, iterations 2–10, parallelism 1–16, max_concurrent 1–64.
  >   - **No constructor below the floor exists,** not even for tests: every test hashes at the baseline, and the dev profile compiles argon2, blake2 and zxcvbn at `opt-level = 3`. That's 10 ms per hash instead of 228 ms, measured.
  > - **The hash format:** argon2id v19 with the configured m/t/p, a 16-byte salt from `SecureRandom` and a 32-byte output.
  >   - **What `verify` accepts:** argon2id v19 with in-range parameters.
  >   - **`VerifyError::Corrupt`** covers anything else: another algorithm or version, a string that doesn't parse, or m/t/p above the config's upper bounds.

  > Note (P7.2, as built, group A) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **`[auth]`'s ranges are checked in validation, not by garde.** `RawAuth`'s fields drop the `argon2_` prefix (clippy's `struct_field_names`) and serde names the keys. garde would report the field's name instead of the key: 0.23 parses `rename` but never applies it. So validation checks each value against `Argon2Params`' bounds, reports every problem under its key in garde's words ("lower than …", "greater than …"), and then builds `Argon2Params::try_new`, the type's only checked constructor besides `Default`.
  > - **Tests,** red against compiling stubs (a `NewPassword` that accepted anything, empty texts and `ALL` lists, a `try_new` without checks, config that ignored the keys, a service that hashed nothing, a queue that was always busy, mappings to one variant):
  >   - fleet-core: 19 of 29 failed on assertions
  >   - fleet-server: 68 of 77 failed on assertions
  >
  >   Then all passed. The queue's tests use a blocking stand-in and bound every wait at 10 s.
- [x] **P7.3** 🔴 `TokenService`:
  - tokens from `getrandom`, with prefixes
  - SHA-256 hashing and constant-time comparison (`subtle`)
  - TTLs
  - refresh rotation, and **family revocation when an old token is reused**

  > Note (P7.3, group B, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **One `sessions` row per family,** with the current access and refresh hashes; every replaced refresh hash stays in `rotated_refresh_tokens`, so reuse of any of them is caught. A refresh replaces the access token too.
  > - **The tokens:** 32 bytes from `SecureRandom`, base64url without padding, behind `afk_at_` or `afk_rt_`. Stored as SHA-256 of the whole text (a 32-byte BLOB). A presented token must be its prefix plus 43 base64url characters before it's hashed; after the indexed lookup, `TokenHash::ct_eq` confirms the match.
  > - **Expiry from anchors:** access = `refreshed_at` + `access_token_ttl_secs`, idle = `refreshed_at` + `refresh_token_idle_days`, both capped at `created_at` + `refresh_token_absolute_days`, from the running config. Expired at `now >= expiry`.
  > - **The refresh rule** (`fleet_core::auth::refresh_decision`), inside the write transaction: unknown, revoked or expired → 401; a replaced token of a live session → reuse (revoke + `security.refresh_token_reuse` in the same commit, one `warn`); the current token of a disabled user → 401; otherwise rotate (`commit_unaudited(SessionRotate)`). No grace window; reuse in a dead session records nothing.
  > - **The reuse entry:** Failure, no actor, the client's IP, target the session's user, metadata `session_id` and `rotated_at` (fleet-core's new typed `AuditMetadata::id` and `time`).
  > - **`TokenService::prepare`** makes a session before any transaction; D's login inserts it and commits with `auth.login`.

  > Note (P7.3, as built, group B) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **The pieces:** `fleet_core::auth::{SessionPolicy, SessionTimes, refresh_decision}`; `ports::tokens::{Tokens, TokenKind, MintedToken}` and `infra::crypto::tokens::Sha256Tokens` (sha2 with `zeroize`); `app::auth::TokenService` with `prepare` and `refresh`; `[auth]`'s three token keys.
  > - **Tests,** red against compiling stubs (rules that allowed everything, typed metadata that added nothing, an adapter that minted nothing, config that ignored the keys, a service that refused every refresh): fleet-core 20 of 25 rule tests and 5 metadata tests plus the names snapshot; fleet-server 11 adapter, 8 config and 13 of 21 service tests failed on assertions. The tests that passed against the stubs check refusals and `Debug`. Then all passed.
- [x] **P7.4** 🔴 Vault cipher: the envelope from §7.5 with `key_id` and AAD. TOTP secrets need it now; Phase 9 extends it. Tests cover tampering, the wrong key and the wrong AAD.

  > Note (P7.4, from Phase 6, group A, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **The `*_file` loader comes here,** with its first user, the vault key: how a secret file is read into a `SecretBox` (a size cap, its permissions, a trailing newline) is decided and tested in this task.
  > - **The secret-name test** (P6.2) already checks every config key: a name with a part `key`, `token`, `password`, `secret`, `passphrase`, `pepper` or `credential` must end in `_file` or `_files`. Keys that match but hold no secret, such as `[auth] access_token_ttl_secs`, go on the test's allowlist (`NOT_SECRETS` in fleet-server's `config/raw.rs`), each with its reason.

  > Note (P7.4, group A, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **The envelope, version 1:** `0x01 | key_id (u32 BE) | nonce (24) | ciphertext + tag`.
  >   - **AAD:** `0x01 ‖ key_id ‖ record UUID (16) ‖ purpose name` (`VaultPurpose::TotpSecret` = `totp_secret`).
  >   - **A committed golden envelope** pins the format. It's never regenerated: a real change means version 2.
  > - **Key files:** standard padded base64 of exactly 32 bytes; a 33-byte key is a wrong length, not "not base64".
  > - **`[vault]`:** `master_key_files` holds 1–16 entries, with u32 IDs and absolute paths. `active_key_id` must be one of them.
  >   - **Duplicate keys** are refused (`subtle`).
  >   - **Who reads the files:** `serve` reads them before the runtime starts and stops with exit 1 on a bad one; `migrate` and `healthcheck` never read them.
  >   - **Not held yet:** the vault is built and dropped until D's MFA service holds it.
  > - **The loader is `fleet_startup::secret_file`,** shared with the agent and P10.
  >   - **Checks:**
  >     - stat first, so anything but a regular file is refused without opening it
  >     - then every check again on the opened handle: a regular file, `mode & 0o077 == 0` on Unix, not empty, at most 64 KiB
  >   - **Reading** is from that handle, capped.
  >   - **Trailing newline:** one trailing `\n` or `\r\n` is removed.
  >   - **Errors** name the file, its mode and the fix, never the content.
  > - **The dev key:** `just dev-server` creates `target/dev-server/vault-key-1` when it's missing and passes it through `AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1`. CLAUDE.md's rule 12 lists it among the files never to read.

  > Note (P7.4, as built, group A) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **Characters before length** (the user's decision during the build). base64 0.23.1's `decode_slice` checks that the whole input fits before it looks at any character, so a 45-character text with a stray space would have been "the wrong length". So the key decoding checks the characters first:
  >   - **A byte-order mark** (UTF-8 `EF BB BF`, UTF-16 `FF FE` or `FE FF`) gets its own error, "starts with a byte-order mark; save it as plain text without BOM". Windows editors and Windows PowerShell 5.1's `>` add one.
  >   - **Any byte outside the standard alphabet and `=`** is "not base64".
  >   - **Then `decode_slice`** decides the padding, the trailing bits and the length; a 33-byte key is still `WrongLength`.
  > - **No copy of a key.** The key decodes straight into its `SecretBox<[u8; 32]>`, and `XChaChaVault::new` consumes the loaded keys: each goes into its cipher (wiped on drop through chacha20poly1305's `zeroize` feature) by reference, and the loaded copy is dropped (wiped) right after. The loader allocates its buffer once at full size, so reading never leaves a copy behind.
  > - **`VaultError::TooLarge`** joins the port's errors: XChaCha20-Poly1305 refuses plaintexts beyond about 256 GiB, and `seal` returns a `Result`, so nothing may panic there. No stored secret comes near it.
  > - **base64 stays without `alloc`.** The tests encode with `encode_slice` into a fixed buffer, and the serve tests' key is a constant (checked with Node).
  > - **Tests,** red against compiling stubs (a loader that returned nothing, keys that loaded nothing, a vault that sealed nothing and opened nothing, validation that accepted anything):
  >   - fleet-startup: 15 of 16 failed on Windows; the Unix-only cases (modes, a symlink, a FIFO read on its own thread with a 5 s bound) run in Linux CI
  >   - fleet-server: 49 failed, including the real binary refusing a bad key file
  >   - the dev script's tests: 7 of 8 (the mode case is Unix-only)
  >
  >   Then all passed. The byte-flip test asserts the envelope's length first, so it can't pass on an empty one. The golden envelope was made once by a temporary test, never committed.
- [x] **P7.5** 🔴 CLI `user create-owner`: hidden prompt with `rpassword`; refuses when an Owner already exists.

  > Note (P7.5, group A, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **The command:** `user create-owner --config <PATH> --username <NAME> [--password-stdin]`.
  >   - **The Owner check** runs before any prompt or stdin read.
  >   - **The prompt:** the password twice, at most 3 rounds, with zxcvbn's hints.
  >   - **`--password-stdin`:** one password, capped at 1 KiB, with no retry; refused when stdin is a terminal.
  >   - **Neither available:** without a terminal and without `--password-stdin`, it exits 1. There's no password flag or variable.
  >   - **Exit codes:** 0 created; 1 refused or failed; 2 usage.
  > - **`just dev-owner username`** runs it against the dev database.
  > - **The audit entry:** `user.create` (Success, target the new user, metadata `role` and `via`, no actor or IP). E's register shares the action.
  > - **The naming rule for audit actions:**
  >   - Actions someone performs are `area.verb`.
  >   - Things the server detects are `security.<noun>`.
  >   - The outcome never goes into the name.
  >   - An insta snapshot of every name keeps merged names from being renamed.

  > Note (P7.5, as built, group A) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **The pieces:**
  >   - `UserReads::owner_exists`, an `EXISTS` query
  >   - `app::users::UserService::create_owner`: the hash first, outside the transaction, then the insert and its `user.create` entry in one commit
  >   - `CreatedVia::Cli` for `via`
  >   - `cli::user` with a `PasswordPrompt` trait (rpassword's terminal, a stand-in in tests) and a `PasswordInput` of the prompt or stdin
  > - **No unwiped copies of a password:** a typed password is copied into a `SecretString` of exactly its length (so boxing it never reallocates) and the original is zeroized; stdin is read into a `Zeroizing` buffer allocated once at full size.
  > - **The output:** "the Owner "alice" is created" on stdout. Every refusal is one `afkfleet-server: …` line on stderr. A round's refusal (a mismatch, the length rule, a weak password with its hints) is a plain line before the next prompt.
  > - **The log:** `info` "the Owner is created" with the user's ID; a failure of the database or the password service is one `error` with its chain.
  > - **Tests,** red against compiling stubs (no Owner check, a service that refused everything, a command that did nothing): 26 failed on assertions, then all passed.
  >   - In-crate: the rounds through a stand-in prompt, `--password-stdin` through a stand-in stdin, and prompts and stdin that fail the test when they're used.
  >   - The real binary: `--password-stdin` creates the Owner with one audit entry, and a second run exits 1 with its stdin still open and unwritten.

  > Note (P7.5, from the group A review, fixed in group B) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **the reads never block the runtime.** The prompt and the `--password-stdin` read run in `spawn_blocking`, each moving its owned source onto the blocking thread and back; the Owner check still comes first. Two current-thread tests failed (the reads blocked the runtime for their 5 s bound), then passed.
- [x] **P7.6** 🔴 Endpoints:
  - `POST /auth/login`, `/auth/login/mfa`, `/auth/refresh`, `/auth/logout`, `/auth/register` (invite)
  - `GET /me`, `POST /me/password` (step-up)
  - `GET /me/sessions`, `DELETE /me/sessions/{id}`

  > Note (P7.6, from Phase 6, group C, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **The first request DTOs.** garde joins fleet-api-types here. Each request DTO derives `Deserialize`, `Validate` and `#[serde(deny_unknown_fields)]`, with tests for an unknown field and every rule; a handler turns garde's report into `validation_failed` with `?` (`From<garde::Report> for ApiError` exists).
  > - **axum's JSON rejections** get their mapping here: which of `bad_request`, `unsupported_media_type` and `validation_failed` a malformed body, a wrong content type and an unknown or mistyped field answer with. axum's `json` feature arrives with them.
  > - **A DTO that uses garde's `pattern` rule** (its `regex` feature) gets the test that a value breaking it appears nowhere in the 422 body.

  > Note (P7.6, from the Phase 7 plan, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **Split between D and E.** D builds `login`, `login/mfa`, `refresh` and `logout`, plus `POST /auth/step-up`. E builds `register`, `/me`, `/me/password` and `/me/sessions`. The task is ticked in E.
  > - **Step-up.** `POST /auth/step-up {password}` stamps the session's `stepped_up_at`.
  >   - A step-up action needs a stamp at most 5 min old; otherwise it answers 403 `step_up_required`.
  >   - A wrong password counts toward the lockout.
  > - **Open for D: re-hashing.** A stored hash made with older `argon2_*` parameters verifies with different timing than the dummy hash. Whether a successful login re-hashes it with the current parameters, and which audit action that write records, is group D's question.
  > - **Open for D: an oversized login.** Login's request DTO gets a generous upper bound on the password field, so an oversized login is refused before any hashing. The bound itself is D's question.
  > - **Open for D and E: the 422 for weak passwords.** `HashError::Weak` carries `WeakWarning` and `WeakSuggestion`; how a 422 exposes them is D's and E's question.

  > Note (P7.6, group D, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **The password step** never hands out real tokens: `{status: "mfa_required" | "mfa_setup_required", token, expires_in}`, an mfa token (`afk_mt_`, 5 min, 3 wrong codes) or a setup token (`afk_st_`, 10 min). An unknown name, a wrong password and a disabled user are one 401 `invalid_credentials`, counted toward the lockout and audited (`auth.login` Failure; no target for an unknown name, which is never stored). A name that isn't a valid `Username` is 422 before the lockout or any hashing; the password field is capped at 1024 characters.
  > - **The code step** takes one `code` field: 6 digits are a TOTP code, 20 base32 characters a recovery code. Wrong codes count toward the lockout; the streak is cleared only when tokens are issued. Its commit re-reads the user (disabled: counted; password changed: not counted) and accepts the step or uses the code conditionally (an authenticator replaced meanwhile: `mfa_changed`, not counted). Answer: `{access_token, refresh_token, access_expires_in, refresh_expires_in}`.
  > - **Re-hashing** a stored hash whose variant, version, m, t or p differ from today's: best-effort at the password step, written in the `auth.login` success commit (`rehashed`), never touching `password_changed_at`.
  > - **Refresh** checks the read pool first, so an unknown token never takes the write connection; a refusal is 401 `unauthorized`.
  > - **Logout** revokes the request's session (`auth.logout`, target the session), 204.
  > - **Step-up** is a plain authed route on the Login bucket: a wrong password is 403 `forbidden`, counted; its commit re-reads the session and the user (401 when gone or disabled, 403 uncounted when the password changed), stamps exactly that session and clears the streak (`auth.step_up`), 204.
  > - **`invalid_credentials`** (401) joins the error codes; every 401 carries `WWW-Authenticate: Bearer`, whatever its code.

  > Note (P7.6, as built, group D) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **The pieces:** `app::login::LoginService` (`password_step`, `code_step`, `step_up`); `TokenService::logout` and the refresh's read-pool check; `IssuedTokens` with the seconds left; `Passwords::{needs_rehash, rehash_verified}`; the handlers in `http::handlers::auth`; fleet-api-types' `auth` DTOs (`LoginRequest`, `LoginResponse` with the open `LoginStatus`, `LoginMfaRequest`, `TokenResponse`, `RefreshRequest`, `StepUpRequest`); the actions `auth.login`, `auth.logout`, `auth.step_up`. `serve` builds the password service before the app (its dummy hash is async work); a failure is a new start error.
  > - **Deviation, one test (the user's decision during the build):** "two concurrent stale logins of the same user both succeed" can't happen: the lockout's in-flight guard holds the name from a step's `begin` to its commit (a second login gets 429), and a password step between another's two steps replaces its challenge. The test that pins the property instead runs across flows: a 2FA replacement's confirm still succeeds after a login re-hashed the password meanwhile; and a second attempt while one is in flight is 429.
  > - **Tests,** the login tests in `tests/auth/` (one binary: login, refresh, logout, step-up), red against compiling stubs (a login service that refused everything, a logout that revoked nothing, a refresh without the read-pool check, a re-hash that never ran, a 401 header for `unauthorized` only, DTO rules that accepted anything, tokens without their lifetimes): 51 of the 73 new tests failed on assertions. The ones that passed check 422s, shapes, `Debug`, registrations, limits and refusals a do-nothing stub satisfies too. Then all passed. Changes landing between a step's checks and its commit come from a store wrapper that applies a queued change right before the next write transaction opens.

  > Note (P7.6, from group D, for E; to ask the user): **failed code attempts on `/me`.** A wrong code means someone has the password, so `/me` could show the failed code attempts (`auth.login` Failure, `step` = `code`) since the user's last login, and P8 show them as a warning.

  > Note (P7.6, from group D, for E; the user's decision) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **a credential change revokes every other session.** A 2FA replacement does (D), and E's `/me/password` must too, through `SessionWrites::revoke_all_for_user(user, except)`, which `PATCH /users/{id}` reuses for all of a user's sessions.

  > Note (P7.6, group E, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **`POST /auth/register {invite, username, password}`** (public, `Register`): the cheapest checks first, the invite then the username on the read pool, then the strength check and the hash, then one transaction that re-checks the invite and its creator (enabled, still allowed to `CreateInvite` its role), inserts the user and uses the invite conditionally. 201 `{id, username, role}`, never tokens: the first login answers `mfa_setup_required`.
  > - **Its refusals:** every invite it can't use is one 422 on `invite` ("isn't valid: it's unknown, used, revoked or expired; ask for a new invite"); a real one is recorded as `user.create` Failure (no actor, the IP, target the invite, `reason` `used` | `revoked` | `expired` | `creator_unauthorized`), every attempt, while an unknown or malformed code never is. A taken name is 422 on `username` ("is taken") and leaves the invite pending, a race at the insert included. A success records `invite.use` and `user.create` (`via` = `invite`, `invite_id`) in one commit, actor the new user.
  > - **A refused new password** (register and `/me/password`) is 422 on its field: "is too weak", then one entry per zxcvbn warning and suggestion in its own words; the length and control-character rule's own message comes first, from garde.
  > - **`GET /me`:** `{id, username, role, created_at, password_changed_at, mfa_confirmed_at?, recovery_codes_left, failed_code_attempts: {count, last_at?}}`, the wrong codes after the previous successful login (or since the account's creation), over the new index `0007_audit_log_target.sql`.
  > - **`POST /me/password {new_password}`** (`step_up_route`, `Login`): the same password is 422 ("is the current password"); the commit re-checks the session (live, the user's: else 401), the user (enabled: else 401) and the stamp (within 5 minutes, and `password_changed_at` not after it: else 403 `step_up_required`, nothing written), then writes the hash and `password_changed_at`, revokes every other session and clears this session's stamp (`user.change_password` with `sessions_revoked`). 204.
  > - **`GET /me/sessions`** lists the live sessions (not revoked, refresh not expired) newest first in keyset pages, `{id, created_at, last_used_at, expires_at, current}`; **`DELETE /me/sessions/{id}`** ends another one (`session.revoke`), the current one is 409 (logout ends it), and someone else's, an ended or an unknown one is 404.

  > Note (P7.6, as built, group E) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **The pieces:** `InviteService::{code_hash, check, check_in}` with `InviteCheck` and `Unusable`; `UserService::{register, me, change_password}`; `TokenService::{list_sessions, revoke_own}`; the queries `AuditReads::failed_code_attempts`, `MfaReads::count_unused_codes`, `UserWrites::change_password`, `SessionReads::list_live` and `SessionWrites::clear_step_up`; `http::handlers::me` and register in `auth`; fleet-api-types' `RegisterRequest`, `RegisterResponse`, `MeResponse`, `FailedCodeAttempts`, `PasswordChangeRequest`, `SessionResponse`, `SessionPage`; `AuditAction::{INVITE_USE, USER_CHANGE_PASSWORD, SESSION_REVOKE}`.
  > - **`mfa_confirmed_at` is optional** on the wire: a user who logged in always has an authenticator, but a profile without one reads as left out rather than failing.
  > - **An over-long invite code** fails its garde rule with the same message as every other bad invite, so a client can't tell it from an unknown one.
  > - **Tests,** red against compiling stubs (rules that accepted anything, queries that found and wrote nothing, services that refused everything): fleet-api-types 3 rule tests, the queries 7 of 8, the HTTP tests 25 of 35 failed on assertions. The ones that passed check refusals a do-nothing stub gives too (the step-up 403, rule 422s, a 401, the unknown-invite 422, a 404, the limits). Then all passed. Snapshots, read before accepting: the audit-action names (three added), the migrations' line 7, `registered`, `register_weak`, `me`, `sessions_page` and the OpenAPI document.

  > Note (P7.6, from group C, for group D) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **Login and the lockout:**
  >   - Login parses the typed name as a `Username` at the boundary; how an invalid name answers is D's question.
  >   - It calls `LockoutService::begin` before the dummy or the real verify, holds the `Attempt` through its transaction, puts `record_failure` into the `auth.login` failure commit and `clear` into the success commit, and maps `LockoutError` to `ApiError` with `?`.
  >   - Step-up's wrong password counts the same way.
  > - **Routes and the client:** handlers take `ClientIp` for their audit entries; the login routes register with `RateClass::Login`, refresh with `Refresh` (E's register with `Register`).
  > - **Open: refresh without the write connection.** `/auth/refresh` could check on the read pool whether the presented refresh hash exists at all (current or rotated) before opening the write transaction, so an unknown token is a 401 without taking the one write connection. That's safe because a token reaches a client only after its commit, so the check never misses one; only known tokens take the race-safe path inside the transaction. D's question (the user's note).
- [x] **P7.7** 🔴 2FA:
  - enroll returns the otpauth URI and a QR code as PNG data
  - confirm, then reset (with step-up), which forces a fresh enrollment at the next login
  - recovery codes
  - replay protection
  - mandatory for **every** user, not configurable: until they enroll, login returns `mfa_setup_required` plus a single-use setup token that only works for enroll and confirm (§7.2). That includes users who just registered with an invite.
  - tests: the setup token is rejected on every other endpoint, expires after 10 min and can't be reused

  > Note (P7.7, from group C, open for D; the user's note) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **2FA codes and the lockout.** Whether wrong 2FA codes (and recovery codes) count toward the per-username lockout, and how many code attempts one `mfa_token` allows, are D's questions. Per-IP limits alone don't stop code guessing from many addresses; `Attempt::record_failure` can count a wrong code like a wrong password.

  > Note (P7.7, group D, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **Deviation: the QR code is SVG, not PNG.** totp-rs's `qr` feature brings image codecs (image, moxcms, png and more, some with SIMD and unsafe code). qrcodegen builds the module grid, and the server writes it as an SVG with nothing but a white background path and one black path of unit squares (ECC Medium, a 4-module quiet zone), sent as `data:image/svg+xml;base64,…`. The app shows it only through an `<img>`.
  > - **Setup tokens only** for enroll and confirm (`Authorization: Bearer afk_st_…`), under a new registry kind `Access::Setup`. Enroll may be repeated (each call replaces the pending secret, which waits in memory); confirm before enroll is 409; a wrong or malformed code is 422 on `code` and keeps the token; confirm takes the token once the code matched, so a second concurrent confirm gets 401. The response of enroll: `{otpauth_uri, qr_svg, secret}`.
  > - **TOTP:** SHA-1, 6 digits, 30 s steps, ±1 step, a 20-byte secret from `SecureRandom`, issuer "afkfleet", label `afkfleet:<username>`. A code is accepted only for a step strictly after `last_used_step`; confirm stores its own step, so the confirming code can't log in afterwards.
  > - **Recovery codes:** 10, each 20 base32 characters (100 bits), shown once as `abcd-efgh-ijkl-mnop-qrst`, typed back in any case with or without `-` and spaces, stored as plain SHA-256.
  > - **Replace, never reset:** `POST /me/mfa/reset` (step-up) answers 200 `{setup_token, expires_in}`, bound to the session that asked; nothing changes until its confirm, which swaps the authenticator and the codes, revokes the user's other sessions (`mfa.reset` with `sessions_revoked`) and re-checks that this session is live, the user enabled and the password unchanged. `DELETE /me/mfa` doesn't exist.
  > - **The vault** joins `AppState` through the 2FA service; the secret's AAD names the user's ID and `totp_secret`.

  > Note (P7.7, as built, group D) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **The pieces:** `fleet_core::auth::{totp, recovery, challenge}` (the window and the replay rule, `RecoveryCode`, `SecondFactor`, the lifetimes); `ports::{totp, qr, recovery_codes}` with `infra::crypto::{totp::TotpRs, recovery_codes::Sha256RecoveryCodes}` and `infra::qr::SvgQr`; `app::challenges::Challenges` and `app::mfa::MfaService`; the extractors `SetupToken`, `SteppedUpUser` and `ValidJson`; `Routes::{setup_route, step_up_route}` with the security schemes and `x-step-up`; `ErrorCode::StepUpRequired`; fleet-api-types' `SecretText` and the 2FA DTOs; `VaultKeys::from_keys`, the in-memory half of `load`.
  > - **The registry test** calls every authed, step-up and setup route without credentials (401), every authed and step-up route with a live setup token (401), every setup route with an access token (401), and every step-up route with a live session that never stepped up (403 `step_up_required`); a step-up route whose handler forgets the check is caught. Each operation's security and `x-step-up` match its access.
  > - **Tests,** red against compiling stubs (a store that found no challenge, a service that refused everything, adapters that matched, drew and hashed nothing, an extractor that skipped the stamp, a body extractor that answered 400 to everything, a registry that documented nothing): 68 of the 88 new server tests, 37 of the 74 new rule tests in fleet-core, and 8 of the 13 new tests in fleet-api-types failed on assertions. The ones that passed check refusals, pinned values and registration, which the stubs satisfy too. Then all passed. New snapshots, read before accepting: the audit-action names (`mfa.enroll`, `mfa.reset`), the 403 `step_up_required` envelope, the OpenAPI document and the SVG of the test URI.
- [x] **P7.8** 🔴 The `AuthUser` extractor (Bearer token via `axum-extra`) loads session and user and rejects disabled users. A `require(permission, resource)` helper calls `fleet_core::authz::authorize`.

  > Note (P7.8, group B, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **Every refusal is the same 401** (`ApiError::Unauthorized`, a bare `WWW-Authenticate: Bearer`): no header, more than one `Authorization` header, another scheme, a malformed, unknown or expired token, a revoked session, a disabled user. A database failure isn't a refusal: 503 or 500.
  > - **One lookup per request:** the session joined with its user on the read pool, decided by `fleet_core::auth::access_decision` (revoked, then expired, then disabled).
  > - **The touch:** `last_used_at` at most once a minute (`touch_due`), a conditional write committed with `commit_unaudited(SessionTouch)`. Only the wait for the write connection is bounded (250 ms); the touch never fails a request, and a skipped one is logged at `debug`.
  > - **Logging:** each refusal is one `debug` line with its reason (and the session's and user's IDs once found); no token, hash or header value is ever logged.
  > - **Step-up:** `AuthUser::stepped_up_at` and `fleet_core::auth::is_stepped_up` (5 minutes); D adds `step_up_required` with its first step-up route.
  > - **Wiring:** `AppState` carries the `TokenService`, built by `serve` from `[auth]`.

  > Note (P7.8, as built, group B) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **The scheme:** headers 0.4's `Authorization<Bearer>` matches `Bearer` in any case and trims the spaces after it, as RFC 6750's `"Bearer" 1*SP b64token` allows (the user's decision during the build); the token's own shape check does the rest.
  > - **Tests,** through a test-only router behind the real middleware stack, red against compiling stubs (an extractor that didn't count the headers, a service that skipped the decision and the touch): 7 of 15 failed on assertions, then all passed. The test that the held write connection doesn't hold up a request passed against the stub too, which never touched.

  > Note (P7.8, from group D) (ADR-0010):
  > - **Errors.** `AuthzError::NotFound` becomes 404 and `Forbidden` 403. `WrongResource` is a bug in the handler: 500, logged at `error`.
  > - **Self-service routes** (`/me/*`, `/me/sessions/{id}`, `/auth/logout`, `/events/ticket`) check `Permission::UsePersonal` on `ResourceContext::Personal{owner}`, so every authed handler calls `authorize()`. Someone else's session gives 404.
  > - **Step-up** (§7.2) is checked here or in the handler, not by `authorize()`.

  > Note (P7.8, from Phase 6, group C) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): `From<AuthzError> for ApiError` already exists, as the note above maps it, so `require(…)` returns `authorize()`'s error with `?`. The 401 answer, `ApiError::Unauthorized`, always sends `WWW-Authenticate: Bearer` with no error detail, so it never tells whether a token existed.
- [x] **P7.9** 🔴 Admin endpoints:
  - `GET /users`
  - `PATCH /users/{id}` (role, disabled). Changing either revokes all of that user's sessions.
  - `POST`, `GET` and `DELETE` on `/invites`

  > Note (P7.9, from group D) (ADR-0010):
  > - **Roles** change only through the Owner (`SetRole`, Member ↔ Admin), never on the Owner themselves or another Owner. Admins only disable and enable Members (`SetDisabled`), so Appendix B's "Admin+" for `PATCH /users/{id}` means the `disabled` field for Admins.
  > - **Ownership transfer** has no permission yet. It gets one together with a route.
  > - **Invites.** Admins create and revoke Member invites; Admin invites are the Owner's (`CreateInvite{role}` on `Global`, `RevokeInvite` on `ResourceContext::Invite{role}`). Admins list all invites.

  > Note (P7.9, group E, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **`GET /users`** (Admin+): keyset pages (`?before=<id>&limit=1..100`, 50 by default, newest first by v7 ID; `{items, next}`, `next` left out on the last page), each user `{id, username, role, disabled, created_at, mfa_enrolled, last_seen_at?}`; `last_seen_at` is the newest `last_used_at` of any of their sessions.
  > - **`PATCH /users/{id}`** takes exactly one of `role` and `disabled` (422 otherwise) and answers the user as the list shows them. A value equal to the current one, decided on the row the transaction re-reads, writes nothing. A change revokes every session of the user (`user.set_role` with `role`, `previous_role`, `sessions_revoked`; `user.set_disabled` with `disabled`, `sessions_revoked`); a demotion to Member or disabling also revokes their pending invites, one `invite.revoke` each with `reason` `creator_demoted` or `creator_disabled`.
  > - **Invites:** `POST /invites {role}` answers 201 `{code, invite}`, the code `afk_iv_…` (32 bytes, `TokenKind::Invite`) shown only there; `[auth] invite_ttl_hours` (default 72, 1 to 720) sets `expires_at` at creation; a creator holds at most 20 pending invites (409 beyond, counted inside the transaction). `GET /invites` lists every invite in its state (`pending`, `used`, `revoked`, `expired`) with `created_by` and `used_by` as `{id, username}`. `DELETE /invites/{id}` revokes a pending one (`invite.revoke`); a used one is 409; a revoked or expired one 204 again without a second entry.
  > - **Every admin write re-checks in its transaction** (`app::users::recheck`): the acting session live and theirs, the actor enabled, then `authorize()` again on the fresh role and target. A dead session or disabled actor is 401; a refusal is what `authorize()` answers on the fresh facts (an Admin demoted meanwhile gets 404 on a Member, since a Member can't see other users).
  > - **The API's new building blocks:** fleet-api-types' `Timestamp` (`YYYY-MM-DDTHH:MM:SS.mmmZ`, written and read by hand; chrono joins the crate with no features), IDs as fleet-core's typed IDs on the wire as strings, `PageQuery<I>` with its own `IntoParams`, and `bool` fields (the renderer test gained `boolean`). Absent values are left out, never `null`. A path ID that isn't a v7 UUID is 404 (`ValidPath`); a query that doesn't parse is 400 and a rule broken 422 (`ValidQuery`, over axum's `query` feature, the user's approval).

  > Note (P7.9, as built, group E) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **The pieces:** `UserReads::{list, listed}` and `UserWrites::{set_role, set_disabled}`; `app::users::{ActingUser, recheck, UserChange, UserService::{list, user_ref, set}}`; `app::invites::InviteService`; `http::handlers::{users, invites}` with explicit operation IDs; `http::extractors::{ValidPath, ValidQuery}`; `AppState` gains both services; `AuditAction::{USER_SET_ROLE, USER_SET_DISABLED, INVITE_CREATE, INVITE_REVOKE}`.
  > - **garde has no rule across fields,** so `UserPatchRequest` implements `Validate` by hand, reporting on the request as a whole (an empty path).
  > - **`PageQuery` uses no serde `default`:** it would bound the ID type by `Default`; a missing `Option` reads as `None` anyway.
  > - **Unique operation IDs:** utoipa names an operation after its function, so `users::list` and `invites::list` were both `list`; each new operation names its own, and a test keeps every operation ID unique (red on the duplicate first).
  > - **Tests,** red against compiling stubs (services that listed and found nothing and refused every change, repositories that read and wrote nothing, a `Timestamp` that wrote nothing and read anything, rules that accepted everything, extractors that refused everything or skipped garde, a prefix and a config key that were ignored): fleet-api-types 26 tests, the users' repository 6 of 6, config 3, the token prefix 2, the extractors 3 of 6, the HTTP tests 33 of 46 failed on assertions. The ones that passed check refusals the handler makes before the service is called. Then all passed. Snapshots, read before accepting: the audit-action names (four added), `users_page`, `user_patched`, `invite_created`, `invites_page` and the OpenAPI document.
- [x] **P7.10** 🔴 Rate limiting:
  - tower_governor per IP, globally and on auth routes, via `into_make_service_with_connect_info`
  - `trusted_proxies` CIDRs decide when `X-Forwarded-For` is honored

  > Note (P7.10, from Phase 6, group A) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): `[http] trusted_proxies` doesn't exist yet: the server's config has only Phase 6's keys, so this task adds it, with its validation.
  - a periodic `retain_recent()` clean-up
  - governor limits per user
  - a per-username lockout stored in the DB
  - `429` with `Retry-After`

  > Note (P7.10, from Phase 6, group C) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): a limit answers with `ApiError::RateLimited { retry_after }`, the limiter's delay, which is sent in whole seconds, rounded up.

  > Note (P7.10, from Phase 6, group D) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): a 429 that tower_governor answers itself, without `ApiError`, still gets the envelope from `render_errors`' safety net and keeps its `Retry-After` and rate-limit headers, but it's logged at `error` as a wiring bug, so the limiter's error handler should return `ApiError::RateLimited`. Whether the health routes count against the global limit is this task's question.

  > Note (P7.10, from Phase 6, group E) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): the dev-only `GET /api/openapi.json` needs a rate-limit class too, like every route. The limiter needs the client's address: `serve` passes the router to `axum::serve` directly, so this task switches to `into_make_service_with_connect_info` there.

  > Note (P7.10, from the Phase 7 plan, the user's decision; open for group C) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): the one real-socket test sends a request over a real connection to a route with the per-IP limit, and fails if the peer address doesn't reach the limiter. `oneshot` sets `ConnectInfo` itself, so it can't catch a missing `into_make_service_with_connect_info`.

  > Note (P7.10, group C, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **Deviation: governor without tower_governor.** tower_governor 0.8.0 hard-codes governor's quanta clock, forces governor's and axum's default features, and rounds its wait down. The server's own middleware runs governor's keyed limiters, and every refusal is `ApiError::RateLimited` with the limiter's wait, rounded up to whole seconds.
  > - **The limits:**
  >   - **Per IP:** every request, health checks and unmatched paths included, in the middleware stack right inside `render_errors`.
  >   - **Per route class,** given at registration (P7.11): `Login` (one bucket shared by `/auth/login` and `/auth/login/mfa`), `Refresh`, `Register`; `Standard` spends nothing more. A route layer, so a 404 or 405 spends none.
  >   - **Per user:** every authed request, in `AuthUser`, right after authentication.
  >   - **Keys:** an IPv4 client by its address, an IPv6 client by its /64; only the key is masked, the audit log keeps the full address.
  > - **The client** (`ClientIp`, a request extension and an extractor for D's handlers): the canonical peer address. Only when it's in `[http] trusted_proxies` is `X-Forwarded-For` read: all its headers as one list, walked from the right past trusted proxies to the first untrusted address. A bad entry stops at the last trusted hop; all trusted means the left-most. No `Forwarded`, no `X-Real-IP`. A request without `ConnectInfo` is a 500, logged as a wiring bug.
  > - **The limiters' clock** is the server's `Clock` port: a new `MonotonicClock` in production, only for the limiters, and the same `ManualClock` as everything else in tests. Stored and audited times keep `SystemClock`.
  > - **The lockout** (`app::lockout::LockoutService`):
  >   - **The rule** (`fleet_core::auth::lockout`): 5 failures (`[auth] lockout_threshold`, 3–20) lock for 1 min, each further one doubles the lock up to 1 h, and a streak expires 24 h after its last failure.
  >   - **While locked:** 429 with the rest of the lock, before any password check, not counted. One attempt per name in flight; a concurrent one gets 429 with `Retry-After: 1`.
  >   - **Keys:** rows keyed by HMAC-SHA256 of the normalized `Username` under a key derived from the vault's active key; unknown names take exactly the same path.
  >   - **`security.lockout`:** recorded in the failing attempt's transaction at each lock, with an `info` line.
  > - **The housekeeping** (`cli::housekeeping`), the server's first long-lived background task: every 60 s it runs `retain_recent()`, purges expired streaks in batches of 1000 (each committed with `UnauditedWrite::LoginAttemptsPurge`), and logs one `info` summary of the refusals since its last run. A refused request's own line is at `debug`. It belongs to `serve`'s `JoinSet` and stops at the first signal.
  > - **Config:** `[http] trusted_proxies` (at most 32 strict CIDRs: host bits zero, no bare addresses, no duplicates, no IPv4-mapped networks, no /0 since group D) and every `[rate_limits]` key but chat's and `ws_connections_per_user`:
  >
  >   | Key | Default | Range |
  >   |---|---|---|
  >   | `per_ip_per_sec` / `per_ip_burst` | 20 / 40 | 1–1000 / 1–10000 |
  >   | `login_per_ip_per_min` | 5 | 1–60 |
  >   | `refresh_per_ip_per_min` | 30 | 1–600 |
  >   | `register_per_ip_per_hour` | 5 | 1–100 |
  >   | `per_user_per_sec` / `per_user_burst` | 10 / 30 | 1–1000 / 1–10000 |
  > - **Tests,** each written first and red against compiling stubs:
  >   - the lockout rule: 25 of 40
  >   - `IpNet`: 33 of 46
  >   - config: 19 of 21
  >   - the limiter and the client's address: 19 of 33
  >   - the router-level limits: 13 of 16
  >   - the HMAC, the derivation and `MonotonicClock`: 9 of 13
  >   - the lockout service, P7.12's lockout counting: 16 of 19
  >   - the housekeeping: 5 of 8
  >   - the real-socket test, with `serve` stripped of `into_make_service_with_connect_info`: it saw a 500
  >   - the housekeeping's stop, with a token the shutdown never cancels: it hit its 30 s bound
  >
  >   Then all passed. The ones that passed against the stubs check allowances, refusals or `Debug` that a do-nothing stub satisfies too. A proptest cross-checks `IpNet` and the /64 key with `ipnet`. The 429 response and the audit-action names have snapshots, read before accepting.

  > Note (P7.10, from the group C review, fixed in group D's first commit) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **no /0 proxy.** `[http] trusted_proxies` accepted `0.0.0.0/0` and `::/0`. Trusting every address lets any client choose its own address through `X-Forwarded-For`, which bypasses every per-IP limit (login's included) and forges the audit log's IPs. `IpNet` now refuses a prefix length of 0 for both families with `IpNetError::Everything` ("a prefix length of 0 would trust every address; list only your proxies' own networks"). The unit tests that had used /0 as a valid network moved to the refusal table or to /1 networks, and the proptests draw lengths from 1. Tests, red first: 6 failed (two parse cases, the message test, the no-echo test and one config case per family), then all passed.
- [x] **P7.11** 🔴 **Route-coverage test.**
  - Routes are registered only through `public_route(…)` or `authed_route(…)` helpers, which also record them in a registry.
  - The test calls every authed route without credentials and expects `401`.
  - It also checks that the public list is exactly the expected allowlist.

  > Note (P7.11, from Phase 6, group D) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): `GET /health/live` and `GET /health/ready` exist since P6.7, registered with plain `Router::route`. They move to `public_route` and belong on the public allowlist.

  > Note (P7.11, from Phase 6, group E) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): routes are registered through utoipa-axum's `OpenApiRouter` (`routes!` with `#[utoipa::path]`), so `public_route` and `authed_route` wrap it and every route keeps its `OpenAPI` entry. `GET /api/openapi.json` exists only in dev mode: it's public, and the allowlist test checks both modes.

  > Note (P7.11, from the Phase 7 plan, the user's decision; open for group C) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): the route-coverage test must not pass while the authed list is empty. One way: a test-only authed route proves the 401 check, and D's and E's routes join the test automatically.

  > Note (P7.11, as built, group C) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **`http::routes`:** `Routes` wraps utoipa-axum's `OpenApiRouter`. Its only ways in are `public_route(class, routes!(…))` and `authed_route(class, routes!(…))`, each taking the route's `RateClass` (`Standard`, `Login`, `Refresh`, `Register`) and recording every method and path as a `RouteEntry { method, path, access, class }`.
  > - **The router:** `http::router::api_routes` registers the production routes (health live and ready, public, `Standard`), and `Routes::finish` adds the dev-only `GET /api/openapi.json` (public, `Standard`, recorded, not in the document) and the middleware stack. `router()` is `api_routes(Routes::new()).finish(…)`.
  > - **`tests/routes.rs`:**
  >   - The public routes are exactly the allowlist outside dev mode and in it.
  >   - A test-only authed route whose handler takes `AuthUser` keeps the authed list from being empty, and every authed route answers 401 with `WWW-Authenticate: Bearer` without credentials.
  >   - An authed route whose handler forgets `AuthUser` is caught.
  >   - The registry equals the document's operations.
  > - **Tests,** red against a registry that recorded nothing: 8 of 9 failed, then all passed.
  > - **For D and E:** a handler takes `AuthUser` before any body extractor, or its request without credentials isn't a 401 and the route-coverage test fails.
- [x] **P7.12** 🔴 Security tests:
  - **User enumeration:** an unknown user and a wrong password give the same status and body.
  - **Lockout.**
  - **Refresh reuse:** reusing a refresh token revokes the family.
  - **Expired or disabled sessions.**
  - **Escalation:** a Member tries to become Admin, an Admin tries to change the Owner.
  - **Invites:** reuse and expiry.
  - **TOTP replay.**

  > Note (P7.12, from the Phase 7 plan, the user's decision) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **each test ships with what it attacks,** written first, so it's red before its defense exists.
  > - **Service-level tests:**
  >   - B: refresh reuse revokes the family; a disabled user's session is rejected by `AuthUser`
  >   - C: lockout counting
  >   - D: TOTP replay
  > - **End-to-end tests over HTTP:**
  >   - D: enumeration (same status and body); lockout through login; expired sessions and refresh reuse through `/auth/refresh`
  >   - E: escalation; disabling through `PATCH /users/{id}`; invite reuse and expiry
  >
  > E names each item with its test and ticks the task.

  > Note (P7.12, from group C, for E and the threat model; the user's note) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **Lockout counting** is `tests/lockout_service.rs` (group C): the fifth failure locks for a minute, each further one doubles the lock up to an hour, a locked name is refused without being counted, and an unknown name locks exactly like a real one.
  > - **An accepted residual risk the threat model names:** the lockout lets an attacker keep one account locked by failing once per lock (one failure an hour, once the lock reached its cap), while existing sessions keep working.

  > Note (P7.12, group D, for E's naming of each item) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **Enumeration:** `tests/auth/login.rs`'s `an_unknown_name_a_wrong_password_and_a_disabled_user_answer_alike` (same status, headers and body but the request ID) and `the_typed_name_is_stored_nowhere`.
  > - **Lockout through login:** `the_fifth_wrong_password_locks_the_name_and_the_right_one_waits` and `wrong_codes_count_toward_the_lockout` there, and `step_up.rs`'s `wrong_step_up_passwords_lock_the_name_for_login_too`.
  > - **Expired sessions and refresh reuse through `/auth/refresh`:** `tests/auth/refresh.rs`'s `reusing_a_replaced_refresh_token_revokes_the_whole_session`, `a_session_idle_for_seven_days_cant_refresh` and `a_session_past_thirty_days_cant_refresh_however_often_it_did`.
  > - **TOTP replay:** `tests/mfa_service.rs`'s `a_totp_code_is_accepted_once_and_no_older_code_after_it` and `the_confirming_code_cant_log_in_afterwards`, and `tests/mfa_repository.rs`'s conditional-write tests.

  > Note (P7.12, from group D, for E's threat model; the user's notes) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **accepted residual risks** to list next to the lockout DoS:
  > - **Stale hashes:** after an `argon2_*` change, users who don't log in keep their old hashes, so their names stay distinguishable by timing until they do.
  > - **TOTP guessing by a password holder:** at the default threshold of 5, about 33 wrong codes a day get through (5 free, then locks of 1, 2, 4, 8, 16 and 32 minutes, then one an hour). Each guess hits one of about 3 in 10^6 codes (the window's three steps), so about 1e-4 a day and 3.5 % a year for an attacker who has the password and never stops, while the real user stays locked out nearly all the time.
  > - **An account without 2FA** (an invitee who abandons setup) belongs to whoever first enrolls with its password.
  > - **Displacing a login:** a password holder can replace the user's live login challenge with a password step of their own (the same class as the lockout DoS).

  > Note (P7.12, group E) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **every item is named with its tests** in the threat model's B1 section (`docs/threat-model.md`, "P7.12's security tests"), whose citations `scripts/threat-model-citations.mjs` checks against the crates. E's own: escalation (`a_member_cant_make_themselves_an_admin`, `an_admin_cant_change_the_owner`, `an_admin_cant_disable_another_admin`, `an_admin_cant_change_any_role`, `nobody_makes_another_owner`, `an_admin_cant_invite_an_admin`, `nobody_invites_an_owner`), disabling through `PATCH /users/{id}` (`an_admin_disables_a_member_who_is_refused_from_then_on`), invite reuse (`an_invite_registers_once_and_its_reuse_is_refused_and_recorded`, `two_registrations_with_one_invite_create_one_user`) and expiry (`an_invite_is_refused_from_its_expiry_on`), each red against its stub first. The accepted residual risks of groups C, D and E are under the threat model's "Accepted risks and known limits".
- [x] **P7.13** Audit events for:
  - login success and failure, logout
  - password and 2FA changes
  - role changes
  - invites created and used
  - token reuse

  > Note (P7.13, from Phase 6, group B, the user's decision) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): a failed login for an unknown username never stores the typed name raw in the audit log: people sometimes type their password into the username field. Each phase lists its actions in one `audit_actions!` block in fleet-core.

  > Note (P7.13, from the Phase 7 plan, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **Each action arrives with its write:**
  >   - A: `user.create`
  >   - B: token reuse
  >   - C: the lockout
  >   - D: login, logout, 2FA
  >   - E: password, roles, invites
  >
  >   E ticks the task.
  > - **The naming rule:**
  >   - Actions someone performs are `area.verb` in the imperative (`auth.login`, `user.set_role`, `invite.create`, `invite.use`).
  >   - Things the server detects are `security.<noun>` (`security.refresh_token_reuse`).
  >   - The outcome is never part of the name.
  >   - Metadata values come from closed enums.
  >   - A snapshot of every name keeps merged names from being renamed or dropped.

  > Note (P7.13, as built, group D) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **`auth.login`, `auth.logout`, `auth.step_up`, `mfa.enroll` and `mfa.reset`.** `auth.login` Success when tokens are issued (actor and target the user, `session_id`, `method`, `rehashed` only when true); Failure on every counted refusal and on a code step refused because the password or the authenticator changed (no actor, target the user or none, `step` and `reason`). `auth.logout` and `auth.step_up` target the new `AuditTarget::Session`; a step-up's Failure carries `reason`. `mfa.enroll` records a first confirm, `mfa.reset` a replacement's with `sessions_revoked`. Starting a replacement, 422s and 429s record nothing. The names snapshot gained all five, read before accepting.

  > Note (P7.13, as built, group C) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **`security.lockout`**, recorded in the failing attempt's own transaction each time a failure starts a lock. Outcome Failure; no actor; the client's IP; target the user when the name is a user's, none otherwise (the typed name is never stored); metadata `failures` and `locked_until`. The names snapshot gained it, read before accepting.

  > Note (P7.13, as built, group E) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **`user.set_role`, `user.set_disabled`, `user.change_password`, `invite.create`, `invite.use`, `invite.revoke` and `session.revoke`,** plus `user.create` through an invite (Success, `via` = `invite`) and its Failure for a real invite that can't be used. Every entry whose commit revokes sessions carries `sessions_revoked`; the pending invites a demotion or disabling revokes get one `invite.revoke` each with `reason`. The names snapshot gained all seven, read before accepting.

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

> Note (P8):
> - **Five group branches.** At the user's request, Phase 8 is built in group PRs like Phases 2–7. Each group has one branch and one commit per task, and the groups run in this order, each after the previous PR is merged:
>
>   | Group | Branch | Tasks |
>   |---|---|---|
>   | A | `p8/p8.1-fleet-client` | P8.1 |
>   | B | `p8/p8.2-p8.3-scaffold-and-hardening` | P8.2, P8.3 |
>   | C | `p8/p8.4-p8.5-bridge` | P8.4, P8.5 |
>   | D | `p8/p8.6-p8.7-shell-and-auth-screens` | P8.6; P8.7's server setup, login with 2FA, register and enrollment |
>   | E | `p8/p8.7-p8.9-profile-admin-wrap-up` | P8.7's profile; P8.8; P8.9; the DoD, threat model B2/B7 and README |
>
>   - **P8.7 is spread over D and E** and ticked in E.
>   - **Hands-on checks:** where only the user can check something on Windows (the window, the keychain, the phone's authenticator, an installed dev build), the group's plan names the exact steps.
>   - Each later group asks its own implementation questions in its session.
> - **Decisions.** The user answered group A's questions on 2026-10-12, one at a time, and amended its plan in review. [ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md) records them; the notes below summarize what changes a task.

- [x] **P8.1** 🔴 `fleet-client`:
  - typed async methods for every endpoint (reqwest + rustls, timeouts)
  - one automatic, single-flight refresh on `401`
  - typed errors
  - integration tests against the in-process server

  > Note (P8.1, from Phase 7, group B, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **A refresh replaces the access token at once.** A request sent with an access token that a refresh has since replaced gets a 401. fleet-client's single-flight refresh checks whether the token it used is still the current one: if a refresh already replaced it, it retries once with the current token instead of refreshing again (no needless second rotation). Only a 401 for the current token triggers a refresh.
  > - **No grace window.** A refresh whose response got lost leaves the client with a replaced refresh token; sending it again is reuse, which revokes the session, so the user logs in again.

  > Note (P8.1, from Phase 7, group D, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **Which 401 refreshes:** the client refreshes only on a 401 with the code `unauthorized` from an authenticated route. A 401 `invalid_credentials` from login or login/mfa never triggers a refresh or a logout, and a 403 `forbidden` from step-up means a wrong password, not a lost session. A refused refresh (401 `unauthorized` from `/auth/refresh`) means logging in again.
  > - **`expires_in`:** the app uses it on the mfa and setup tokens to restart a timed-out step itself (an expired token answers like a wrong code). Refreshing stays on a 401 only, never on a timer.

  > Note (P8.1, from Phase 6, group C, the user's decision) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): **A maximum response body size.** fleet-api-types caps each text a client reads (`BoundedText`, `Open`), but not how many entries a response holds (a `fields` array with a million entries), so fleet-client sets a maximum body size on its HTTP client, which bounds every response, not only errors.

  > Note (P8.1, group A, the user's decisions) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - **The API types:** request DTOs derive `Serialize` too, leaving an empty `Option` out (an amendment of ADR-0015's request rule), and fleet-api-types re-exports the IDs its DTOs carry, so §4's table holds.
  > - **The tokens:**
  >   - The refresh token lives only in a `SessionStore` port (`MemoryStore` here, the keychain in P8.4); a failed save clears it.
  >   - The access token lives only in memory; a request without one refreshes first.
  >   - The password step's and a 2FA replacement's tokens stay inside the client, whose methods take only codes.
  >   - `login` with a session held is `AlreadyLoggedIn`.
  > - **The session ends** on a refused refresh, a 401 after the retry, an empty store, and every logout, even an unreachable server's; never on a transient failure. **An end never races a save:** an epoch under a store gate drops, and revokes on the server, any save that started before an end. Logout ends locally first, then revokes with the tokens it took.
  > - **The server and its certificates:**
  >   - `ServerUrl` is an origin; plain http works only for a literal loopback address.
  >   - No redirects, no proxies.
  >   - `Trust::System`, or `Trust::CustomCa` alone (`tls_certs_only`), read strictly by `CustomCa::from_pem`.
  > - **Bounds:**
  >   - 5 s to connect, 20 s per request and 5 s per store call.
  >   - A 1 MiB body cap.
  >   - No automatic retries: the error carries `retry_after`.
  >   - **The server's `[http] request_timeout_secs` is capped at 19 s** (Appendix A's note).
  > - **Tests:**
  >   - `serve::app_with` runs the production wiring over a `ManualClock` and `SeededRandom`.
  >   - The interleavings run through a crate-private transport seam.
  >   - A required `fleet-client` coverage gate at 90 %.

  > Note (P8.1, as built, group A) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - **The pieces:**
  >   - The client: `fleet_client::{Client, ClientConfig, LoginStep, ServerUrl, Trust, CustomCa, SessionStore, MemoryStore, ClientError, ApiFailure}`, every `/api/v1` route plus `health_live` and `health_ready`.
  >   - fleet-server: `serve::{ServerPorts, app_with}`.
  >   - fleet-api-types: the request DTOs' `Serialize` and the ID re-exports.
  > - **Tests,** red against compiling stubs first: fleet-api-types 2 of 3, the config's range case, fleet-client 101 of 132, the gates script's new check. Then all passed.
  >   - The integration tests run the real server on 127.0.0.1:0: every route; the refresh rules (a transparent refresh, eight requests and one rotation, no grace window, the idle expiry); logout after a restart.
  >   - The TLS tests use rcgen certificates and a rustls server on a thread.
  > - **A dropped caller never stops a save** (the user's review of PR #34): once a refresh's or a code step's request may reach the server, the request, the save and the adoption run in a task of their own, which holds the refresh lock and the refresh-out mark until it ends. So a cancelled caller can't lose a rotated refresh token (whose next use would be reuse) or orphan a session. A task that ends without an answer is `ClientError::Interrupted`.
  > - **Lockfile:** the build gains rcgen, pem and yasna. Cargo.lock also lists x509-parser and its parsing crates, the optional dependency behind rcgen's weak feature `x509-parser?/verify-aws`, which nothing enables or builds; `cargo deny check` passes.
- [x] **P8.2** Scaffold:
  - `apps/desktop` (Tauri v2; `src-tauri` is a workspace member)
  - `packages/ui` (Vite + React + strict TypeScript + Tailwind + shadcn/ui)
  - pnpm scripts for both

  > Note (P8.2, group B, the user's decisions) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - **Windows only.** `apps/desktop/src-tauri` (crate `afkfleet-desktop`) is a workspace member, but on Linux it would need WebKitGTK, so every workspace-wide Rust recipe adds `--exclude afkfleet-desktop` there. Windows covers it: clippy and test on windows-latest, and a new `tauri-build` job (docs, stable-check, `just app-build`). Coverage never measures it.
  > - **Doctests leave the desktop crate out on every platform.** tauri-build's static VC++ runtime puts a stub `msvcrt.lib` on its link path, and cargo passes every build script's link paths to every doctest, so fleet-core's doctests failed to link. The desktop crate has no doctests.
  > - **cargo deny in two passes**, same `deny.toml`, no ignores:
  >   - the workspace without the desktop crate, on every target
  >   - the desktop crate alone, on Windows
  >
  >   The real all-targets run failed only on the Linux GTK3 stack (glib 0.18's RUSTSEC-2024-0429, proc-macro-error's RUSTSEC-2024-0370, target-lexicon's `Apache-2.0 WITH LLVM-exception`). A new desktop target joins the second pass.
  > - **Docker:** the agent's image copies `apps/desktop/src-tauri` too, because cargo can't load the workspace without it. Nothing there builds it.
  > - **Identity:** identifier `io.github.noah-gabel.afkfleet`, productName `afkfleet`.
  > - **The frontend:**
  >   - TypeScript 7.0.2.
  >   - shadcn/ui is only `components.json` (radix-nova, `#` subpath imports) plus its theme tokens; its packages arrive with the first component.
  >   - Vitest with a smoke test.
  >   - The Tauri CLI from npm.
  >   - `apps/desktop` renders `<App />` from `@afkfleet/ui` on Vite's port 1420.
  > - **The icon** is an original placeholder SVG; `just icons` renders `icon.ico` from it.
  > - **Recipes:** `just dev-app`, `just app-build [--debug]`, `just icons` and `just ui-test` work; `just e2e` arrives in P8.9.
  > - **Tests,** red first against stubs:
  >   - `scripts/icons.test.mjs`: 4 of 4
  >   - the UI's smoke test, which found no heading in an empty `App`
  >
  >   Then all passed.
- [x] **P8.3** Tauri hardening:
  - CSP: `default-src 'self'; img-src 'self' data:; style-src 'self'; connect-src ipc: http://ipc.localhost`. Relax it only with an ADR.
  - Capabilities allow only our commands.
  - `withGlobalTauri: false`.
  - Devtools only in debug builds.
  - The single-instance plugin.

  > Note (P8.3, group B, the user's decisions) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - **The CSP, exactly,** with `dangerousDisableAssetCspModification: true`: Tauri appends no hashes or nonces, which makes the served header stricter, not looser. Tauri writes the header from a hash map, so its four directives come in an order that varies between runs.
  > - **No `devCsp`:** `tauri dev` on desktop loads Vite's server directly, and Tauri sends no CSP there. The real CSP is checked in a `tauri build --debug` exe.
  > - **`withGlobalTauri: false`** is written out.
  > - **`freezePrototype: true`** (the user's addition); removing it needs an ADR.
  > - **Capabilities:** one, `main`, granting nothing; no `core:default`.
  >   - build.rs passes `const COMMANDS` (empty) to `AppManifest::commands`.
  >   - An empty `[default]` permission in `permissions/default.toml` turns the ACL on for app commands. Without an app manifest Tauri lets every app command through, which the ACL test showed.
  > - **Devtools:** no `devtools` feature or window setting. tauri has only `wry` and `compression`: `dynamic-acl` is out, and `common-controls-v6` is out too (the user's choice), since its TaskDialogIndirect import kept the ACL test's exe from starting.
  > - **Single instance:** the plugin is registered first, and a second launch restores, shows and focuses the window.
  > - **A navigation guard** (the user's addition): an internal plugin's `on_navigation` hook allows only `http://tauri.localhost`, and `devUrl`'s origin in dev builds.
  > - **Pinned:**
  >   - `just tauri-check` (`scripts/tauri-hardening.mjs`; in `check`, `ci` and CI's deny job) fails on any change to the security settings, the windows, the dev URL, the capability, the command list, the default permission or tauri's resolved features.
  >   - `tests/acl.rs` shows on the mock runtime that ungranted app and core commands are refused.
  > - **Tests,** red first against stubs: `navigation` 14 of 19, `focus` 2 of 2, the ACL's app-command case, and the hardening script 18 of 25. Then all passed.
  > - **Checked live** in a debug build over WebView2's DevTools protocol: the page loads, no CSP violations, the guard keeps the window on the app, `window.open` opens nothing, and a second launch exits at once.
- [x] **P8.4** 🔴 Rust side:
  - `AppState` holds the server profile, the `fleet-client` and the session.
  - The **refresh token lives in the OS keychain**; the access token only in memory.
  - Typed commands are grouped per feature (`commands/auth.rs` …) and return `Result<T, CommandError { code, message }>`.
  - Server profile: a URL plus an optional custom CA PEM, validated. **Never accept invalid certificates.**
  > Note (P8.4, from group A) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - **The keychain implements fleet-client's `SessionStore`**, doing its blocking calls off the runtime.
  > - **The server profile reuses `ServerUrl` and `CustomCa::from_pem`.**
  > - **`main` installs the aws-lc-rs provider** before building the client.
  > - **`CommandError` maps `ClientError`** with a default branch (`ApiFailure::code` is `Open`).
  > - **No token reaches `AppState`:** fleet-client holds the access token and the step tokens itself.

  > Note (P8.4, group C, the user's decisions) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - **Every endpoint now:** 20 commands in `commands/{server,auth,mfa,me,users,invites}.rs`, each a thin wrapper over an `AppState` method. The IPC types (`CommandError`, the login step, the logout outcome, the server profile's request, view and states) live in fleet-api-types' `app` module and are exported by `just gen`; the `OpenAPI` comparison leaves them out.
  > - **`CommandError { code, message, api?, fields? }`:**
  >   - a closed code
  >   - its fixed message
  >   - only for `api`: the server's status, open code, message, request ID, fields and `retry_after_secs`
  >   - the rules a local check broke
  >
  >   Logout always answers Ok: `ended`, or `unconfirmed` with the error.
  > - **The server profile:**
  >   - It's validated by `ServerUrl` and `CustomCa::from_pem`, and stored as `server-profile.json` (version 1) in the local app-data folder. A CA is stored as canonical PEM re-encoded from DER (`CustomCa::to_pem`), and the file is written atomically and read with a 128 KiB cap.
  >   - A file that can't be used starts the app without a profile, `unreadable` with its reason.
  >   - The view is the origin plus each CA certificate's SHA-256 fingerprint.
  >   - `set_server_profile` checks `health_live` before changing anything; `check_server` calls `health_ready`.
  > - **Switching:**
  >   - The keychain entry is keyed by origin, and a stored session resumes only under the identical profile.
  >   - An identical profile is a no-op (`change: "none"`). Any other change logs out of the old server, then clears the new origin's entry unsent (`Client::discard_session`), saves and swaps (`change: "switched"`).
  >   - One `RwLock`: every command holds it shared, and the switch exclusively, after its health check.
  > - **The keychain:**
  >   - keyring-core 1.0.0 over windows-native-keyring-store 1.1.0, `Local` persistence.
  >   - Calls on one entry run in order: a tokio mutex per origin (shared by every store of that entry, after the user's review of PR #36) taken before `spawn_blocking`, its guard moved in, so a call that timed out waiting never runs. A call that outlives the 5 s timeout runs on in its thread: a new task exception in CLAUDE.md.
  >   - keyring-core's errors become content-free `KeychainError`s.
  > - **fleet-client:** `CustomCa::to_pem` and `fingerprints`; `from_pem` also refuses a CA whose canonical PEM would pass 64 KiB; `Client::discard_session`.
  > - **Pinned:**
  >   - Each command is in build.rs, `main.json` and `tauri-hardening.mjs`.
  >   - `permissions/autogenerated/` is committed and pinned byte for byte.
  >   - `tests/ipc-contract.json` lists every command's exact args and expected answer, checked by the ACL test on the mock runtime.
  > - **Coverage:** `just cov-desktop` on Windows (CI's `tauri-build` job), with a required 90 % gate. It runs with `--no-rustc-wrapper`, since cargo-llvm-cov 0.9.1's wrapper leaves the lib's unit-test binary uninstrumented on Windows (found in the spike).
  > - **Tests:**
  >   - The commands run against the in-process server (`serve::app_with`).
  >   - The keychain's ordering tests run on real time (a deviation like sqlx's).
  >   - One test uses the real Credential Manager under a test-only service.

  > Note (P8.4, as built, group C) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - **Tests,** red first against compiling stubs:
  >   - fleet-api-types' `app`: 1 of 14, on the stub's empty messages (a second failure was the test's own field-order mistake, fixed)
  >   - fleet-client: 7 of 7 `CustomCa` and 4 of 4 `discard`
  >   - the desktop crate: 94 of 129, the 35 that passed being the existing tests and checks a do-nothing stub also satisfies
  >   - `tauri-hardening`: 10 of 34
  >   - `coverage-gates`: 7 of 23
  >
  >   The keychain's ordering test also failed against a std mutex taken inside the blocking call, as the user predicted. Then all passed.
  > - **Coverage:** 94.95 % of the desktop gate's lines on the dev machine, 95.17 % in CI. After the user's review of PR #36, a prefix gate also fails on a listed file with executable lines and none covered ("uninstrumented test binary?"), and the keychain's turn is per entry (one per origin, shared by every store of it).
  > - **Lockfile:** it gains only keyring-core and windows-native-keyring-store.
- [x] **P8.5** 🔴 TypeScript side:
  - The `ApiClient` interface lives in `packages/ui/src/api/`.
  - `TauriApiClient` lives in `apps/desktop/src/` and is injected into `<App apiClient={…} />`.
  - `FakeApiClient` is for tests.
  - DTO types come only from `src/generated`.

  > Note (P8.5, from Phase 6, group C, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **Error codes.** UI code that branches on an error code always has a default branch that shows the message and the request ID, never an exhaustive switch that assumes the union is complete: an older app can receive a code added later. The same applies to every `Open<…>` field.
  > - **The generated folder** holds only ts-rs's files: `just gen` refuses to touch it if it holds anything else. Import each type from its own file (`../generated/ErrorResponse`); there's no index file.

  > Note (P8.5, group C, the user's decisions) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - **`ApiClient`:** 20 methods typed by the generated types. Failures are `ApiError { error: CommandError }`; a rejection that isn't a `CommandError` (Tauri's own refusals) becomes `ipc`. `describeError` switches exhaustively on `code` and keeps a default branch on `api.code`.
  > - **zod in a `validated(client)` wrapper,** which `App` applies: every input is checked before `invoke`, and a failure is `invalid_input` with garde-style fields. `limits.ts` mirrors the Rust constants, and a fleet-api-types test keeps them equal.
  > - **`FakeApiClient`** is scripted per method and records calls; `UnavailableApiClient` serves the web entry until P8.9.
  > - **`TauriApiClient(invoke)`** has `invoke` injected and is tested with Vitest in apps/desktop against `ipc-contract.json`, the same fixture the Rust ACL test uses.
  > - **Biome's `noRestrictedImports`:** no `@tauri-apps/*` in packages/ui, and only `@tauri-apps/api/core` in apps/desktop.
  > - **Dependencies:** `@tauri-apps/api` 2.12.1, zod 4.6.5, vitest 5.0.3 (apps/desktop dev), all exact.

  > Note (P8.5, as built, group C) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - **Tests,** red first:
  >   - the UI: 25 of 31 against stubs
  >   - `TauriApiClient`: 4 of 24, against a renamed command, a dropped argument and no rejection mapping
  >   - `tests/limits.rs`: on a missing constant
  >
  >   Then all passed. One test runs the validating wrapper with `Object.prototype` frozen, as the webview does.
  > - **packages/ui** exports `./generated/*`, so apps/desktop imports each type from its own file.
- [x] **P8.6** 🔴 App shell:
  - TanStack Router with an auth guard
  - role-aware navigation
  - an error boundary
  - toasts
  - a banner when the server is unreachable
  > Note (P8.6, P8.7, from group A) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - The banner shows on `Unreachable` and `TimedOut`.
  > - Server setup explains `Certificate`.
  > - A 429 or 503 shows its `retry_after`.
  > - `expires_in` restarts a timed-out login or setup step.
  > - Registering holds no session, so the login right after it never meets `AlreadyLoggedIn`.

  > Note (P8.6, group D, the user's decisions) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - **Dependencies,** exact and at least 3 days old: TanStack Router 1.170.42 and Query 5.104.1, sonner 2.0.8, zustand 5.0.15, lucide-react 1.53.0, `cn` 0.4.0, class-variance-authority 0.7.1, `@radix-ui/react-slot` 1.4.0 and `@radix-ui/react-label` 2.1.16 (never the `radix-ui` umbrella), user-event 14.6.7 (dev). `pnpm-workspace.yaml` sets `minimumReleaseAge: 4320` for every install from now on.
  > - **No runtime styles:** sonner is patched not to inject its `<style>` (the release CSP blocks it), and every Vitest test ends with a check that the document holds no `<style>` and no stylesheet at all.
  > - **shadcn's six components** (Button, Input, Label, Textarea, Card, Alert) are copied from the registry's JSON, never through the CLI, each with its edits in its header.
  > - **The router:** routes in code over a memory history. `/` is the launch: no profile or an unreadable one leads to server setup; `me()` then leads Home, to login on `no_session`, or to a start screen (waiting while unreachable; otherwise the error with Try again, Server settings and Log out).
  > - **Every call is watched:** `unreachable`/`timed_out` raise the banner, `certificate` its own banner state, and `no_session` ends a signed-in session once (only for a call of the current sign-in, never at launch, inside the sign-in flow or during a logout): to login first, then the cache is emptied, then one toast. A success of a call that started after the banner went up takes it down and refetches.
  > - **The banner** checks every 10 s, never two at once, and waits a busy server's `retry_after` (at least 10 s); "Retry now" checks at once.
  > - **QueryClient:** no retries, no refetch on focus or reconnect; `me` is fresh for 60 s.
  > - **Role-aware navigation:** Home and Server for everyone; Users and Invites (placeholders until group E) for Admins and the Owner, behind a role guard that answers "not found". An unknown role gets a Member's rights.
  > - **The error boundary** shows a fixed text with Try again and Restart; an API error its text, any other error only its name.
  > - **Logout** from the header: login first, then the cache; a warning when the server couldn't be told; a failed call stays signed in.

  > Note (P8.6, as built, group D) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - **Tests,** red first against compiling stubs: the style guard 3 of 4, sonner's guard against the unpatched package, the stores and the watched client 23 of 32, the QueryClient's defaults 1 of 2, the shell (launch, session end, header, router, nav, banner, error screen) 64 of 67. Then all passed. The 3 that passed are what a stub also does (two privileges, the error component's wiring).
  > - **A keychain entry the app can't read** (`tests/bridge/auth.rs`, the real `KeychainStore` on keyring-core's mock): `me()` answers `store`, `logout()` removes it, and the next login works. It pins existing behaviour, so it was shown red through a temporary change to fleet-client's logout (skipping the clear after a failed load), then reverted.
  > - **Found while building:** a check that settled before React rendered flipped `checking` twice in one batch, so the banner's next wait never started; the wait now chains from the check itself.
  > - **Found in the live check** (a `just app-build --debug` exe over WebView2's DevTools protocol): zod's JIT probe (`new Function`) caused a CSP violation at every start. zod is now jitless (`src/api/zod.ts`, the only module that may import zod), with a red-first guard test; after it, no violation and no `<style>` on any screen walked.
- [x] **P8.7** 🔴 Screens:
  - server setup
  - login, including the 2FA step
  - register with an invite
  - 2FA enrollment (QR code from the server), shown right after the first login, since 2FA is mandatory
  - profile: change password, list and revoke sessions

  > Note (P8.7, from Phase 7, group D, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **The QR code is an SVG `data:` URI.** The app shows it only through `<img src="data:…">`, never by inserting the SVG as HTML, so nothing in it could ever run. P8.3's CSP already allows `img-src data:`.
  > - **After setup, the app asks for the next code:** the code that confirmed the authenticator can't log in.
  > - **Replacing the authenticator** goes from the profile through step-up and `POST /me/mfa/reset` straight into enroll and confirm with the setup token it answers, in the same session; until confirm, the old authenticator keeps working.

  > Note (P8.7, from Phase 7, group E, the user's decisions) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)):
  > - **Registering:** the screen trims spaces and line breaks around a pasted invite code before sending it, since chat apps often add them; the server's shape check stays strict. After a successful registration the app logs in right away with the credentials just entered and goes straight into the 2FA setup, so a new account never sits without 2FA (the threat model's "an account without 2FA").
  > - **The profile:** when `failed_code_attempts.count` is above 0, the app shows a warning that names `last_at` and suggests changing the password. When `recovery_codes_left` is 3 or fewer, it warns and points to the 2FA replacement, which issues 10 new codes.
  > - **The session list:** the row marked `current` shows "Log out", which calls `POST /auth/logout`; `DELETE /me/sessions/{id}` answers 409 for it.

  > Note (P8.7, group D's part: server setup, login with 2FA, register, enrollment; the user's decisions) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - **One sign-in flow** per `/login` and `/register`, a reducer inside one component, so the password, the TOTP secret and the recovery codes live only in its state. Every call that carries a secret in its request or its answer (here `login`, `register`, `loginMfa`, `mfaEnroll`, `mfaConfirm`; in E `stepUp`, `changePassword`, `createInvite`) is a direct call, never a TanStack query or mutation (CLAUDE.md).
  > - **The code step:** whitespace removed; 3 tries ("Wrong code. 2 tries left.", "… 1 try left."), the third and the step's deadline back to the password step (username kept, password dropped); a code after the deadline is never sent; a last-minute warning. A 422 shows at the field and doesn't count.
  > - **The setup step:** the QR code only through `<img>` and only as `data:image/svg+xml;base64,`; the key in groups of 4; "New QR code"; only the newest one works.
  > - **Recovery codes:** shown once, "Copy all" (the clipboard may keep them), a required "I've saved these codes"; then the flow logs in again with the kept password and asks for the next code. A failed login there goes back to the password step.
  > - **Registering:** the invite trimmed, the password typed twice with "show" toggles; then a login with the same credentials straight into the setup, or the login form with "Your account was created. Log in to set up 2FA."
  > - **A session still held at login:** the same user (case-insensitive) opens Home; another offers "Continue as …" or "Log out"; one that can't be checked offers "Log out". Every way into the app starts a new signed-in generation.
  > - **Server setup:** the address and an optional CA (pasted, or read from a file of at most 64 KiB); a current custom CA starts checked with its fingerprints and must be given again to keep it; signed in, a warning before a save logs out. `none` keeps the session; `switched`, `profile_save` and the new `session_discard` go to the login.
  > - **`session_discard`:** after the old server's logout, a failed discard of the new origin's stored session no longer answers `store`, so `store` from `set_server_profile` always means nothing changed.

  > Note (P8.7, as built, group D) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - **Tests,** red first: the flow's reducer 9 of 11; the field components 3 of 4; the login, register and server screens 57 of 57 against the placeholders; the desktop crate 2 of 134 against a `session_discard()` stub; the UI failed to compile until it knew the new code, by design. A test that a store refused before the guard leaves a live session untouched pins existing behaviour and was shown red through a temporary change that logged out first; the frozen-prototype flow test, through a temporary shadowing `toString` assignment.
  > - **E's part:** the profile (password change, sessions) and the admin screens replace group D's placeholders; P8.7 is ticked there.

  > Note (P8.7, the user's review of PR #37) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - **A server switch while signed in runs as a deliberate logout:** `logging_out` from before the call until its answer, so a call answering `no_session` meanwhile shows no "session ended"; `none` and an error that changed nothing go back to signed in.
  > - **Late answers count only for their step:** every step has an id, each action names the step it was issued in, and the reducer drops the rest. A late answer never signs in or navigates once the flow left its step or route.
  > - **While a call runs,** its step's Cancel, links, "Log out" and "Continue as" wait, and a second submit does nothing. "New QR code" waits too, and hides the old secret and QR code while the new one loads.
  > - **Edge's own reveal button:** WebView2 adds Edge's password-reveal (and clear) button to inputs, so a password field showed two eye icons; `styles.css` hides `input::-ms-reveal` and `input::-ms-clear`, checked in the debug exe (two icons before, one after).
  > - **Smaller:** the setup secret is requested once per step, also under StrictMode; a step's deadline counts from when its request was sent; a failed login after registering lands on a real `/login` (with its notice and username), whose Register link works.
  > - **Tests,** red first: 13 flow tests and 2 switch tests against the old code, and the reducer's 11 of 13 against the old reducer. 8 more pin behaviour the old code had (double submits, "Continue as"'s guard, the generation rule, `me`'s invalidation at every way in, the setup step's deadline check), shown red through temporary mutations of those guards.

  > Note (P8.7, group E's part: the profile; the user's decisions) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - **The layout:** `/profile` holds the account, the warnings, "Change password", "Replace authenticator" with the recovery codes left, and the sessions. `/profile/password` and `/profile/2fa` are routes of their own beside it, so leaving a flow drops its password, secret and codes.
  > - **Step-up first.** Both flows start with "Confirm it's you" (`stepUp`, a direct call; the password lives only in `StepUpForm` until it answers).
  >   - **The password:** then the new password, typed twice, before the step-up's 5 minutes end, counted from when it was sent (`STEP_UP_WINDOW_SECS`, mirrored in `limits.ts` and checked by `tests/limits.rs`).
  >   - **The authenticator:** `mfaReset` straight into the 2FA setup, then the new recovery codes and "Finish", without a new login.
  >   - `step_up_required` or a deadline goes back to the password with "That step ended. Enter your password again."
  > - **The warnings:** wrong 2FA codes since the previous login (naming the last), and 3 or fewer recovery codes left. They show on the profile and on Home, where every sign-in lands; on Home "Dismiss" hides one until the next sign-in.
  > - **The wrong-code count leaves out the typos of the login that succeeded** (a server change). The password step mints a `LoginStepId`, which the code step's `auth.login` entries record as `login_step_id`, and `GET /me` leaves out the newest success's own step ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)'s amendment).
  > - **Sessions:**
  >   - "Show more".
  >   - Another session's "Log out" runs at once; this session's is the header's logout.
  >   - "Log out all other sessions" asks first, then fetches every page the server lists. It runs to the end even when the screen is left, and reports on the screen or in a toast.

  > Note (P8.7, as built, group E) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - **Shared with the sign-in flow:** `hooks/useStepGuard` (the step-id and pending guards), `useEnrollment`, `nextSetup`, `isStepGone`, and `SetupStep`'s and `RecoveryCodesStep`'s texts as props.
  > - **Found while building:** `showFieldErrors` kept only a field's last message, so a weak password showed only zxcvbn's last hint. Every message now shows, one per line (red first).
  > - **Tests,** red first:
  >   - the UI against compiling stubs: 52 of the new and changed tests (the reducer 4 of 7, the notices store 2 of 3, navigation 2 of 10, the router 4 of 12, the overview 9 of 11, the sessions 9 of 9, the password flow 14 of 14, the 2FA flow 8 of 8); the rest are what a stub also does
  >   - the server's wrong-code tests 3 of 3 against the old query
  >   - `tests/limits.rs` on the missing constant
  >   - `formErrors` 1 of 3
  >
  >   Tests that pin existing behaviour were shown red through temporary mutations: the login step's metadata and its error path, two `formErrors` cases, and the sessions' "every page" and "runs to the end" cases.

  > Note (P8.7, the user's review of PR #38) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)): **A refusal's wait counts down.** A 429 (or a 503 busy) showed "Try again in N s." as static text, with the submit still enabled, so every click during the wait was refused again.
  > - **The countdown:** every form that sends a request shows the wait counting down (`useRetryWait`, `RetryWaitMessage`). Those are login, the code step, the 2FA setup, registering, the step-up (the profile's and the admin prompt's), the new password and the server form. The form keeps its submit disabled until the wait reaches zero, then says once "You can try again now."
  > - **Screen readers** hear the start (the refusal, with the whole wait in words) and the end (a status), never each second: the ticking number is hidden from them.
  > - **A login that ran by itself** (after registering, or after the recovery codes) hands its wait to the login form's notice, which counts it down.
  > - **After a reload** the countdown is gone, and the next try shows the server's remaining wait again; the server enforces it.
  > - **Tests,** red first with fake timers against a stub hook: the hook 4 of 6, the message 2 of 3, and each form 10 of 10.
- [x] **P8.8** 🔴 Admin screens:
  - users: change role, disable
  - invites: the code is shown **once**, with a copy button

  > Note (P8.8, group E, the user's decisions) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - **Only what the server would allow.** `permissions.ts` mirrors `authorize()` (`userActions`, `inviteRoles`, `canRevoke`). Its test reads fleet-core's authorization-matrix snapshot itself and checks every cell of its Users, CreateInvite and RevokeInvite tables, so a rule change fails the UI's test.
  > - **Step-up for the admin writes** (a server change). `PATCH /users/{id}`, `POST /invites` and `DELETE /invites/{id}` are `step_up_route`s, checked by the extractor only ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)'s amendment). The screens send the write; on 403 `step_up_required` they show "Confirm it's you" at the row or the form, step up and send it once more (`useStepUpAction`).
  > - **Inline confirmations** (no dialog) for a role change, disabling and revoking an invite, each naming what it ends; enabling runs at once.
  > - **The invite's code** is shown once, in the screen's state only, with the clipboard note on both copy buttons:
  >   - "Copy invitation": the server, the code, its end with its time zone, and a custom CA's fingerprints with "Ask me for the CA file"
  >   - "Copy code"
  > - **Paging:** "Show more" on every list.

  > Note (P8.8, as built, group E) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)): **tests,** red first:
  > - the server's step-up tests 5 of 5 against the old registration
  > - fleet-client's and the desktop's admin flows (4 and 3) until they stepped up
  > - the 38 admin tests that fail without their stamps
  > - the UI 33 of 38 against all-refusing mirrors and the placeholder screens; the other 5 read the snapshot's tables and check refusals
- [x] **P8.9** Playwright against the Vite web build with MSW, for login, 2FA and invite registration.

  > Note (P8.9, group E, the user's decisions) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - **The e2e build** (`apps/desktop/e2e`): the desktop app's UI with its real `TauriApiClient`, over `fetchInvoke`, which sends each command's args as JSON to `/e2e/ipc/<command>`. MSW's handlers answer in the Playwright process through `@msw/playwright`, from a fake of the Rust side and the server that answers exactly `ipc-contract.json`'s commands. There is no service worker, and no MSW code or token in any bundle.
  > - **The browser and the CSP:**
  >   - The installed Edge (`channel: "msedge"`): nothing is downloaded.
  >   - `vite preview` serves the release CSP with only `connect-src 'self'`.
  >   - Every browser spec freezes `Object.prototype`, and fails on a CSP violation, a console error or an injected style.
  > - **Never shipped:** Biome refuses any `e2e` import in `apps/desktop/src`. `just e2e` builds both shipped bundles and runs `scripts/e2e-bundle-guard.mjs` before Playwright.
  > - **The specs:**
  >   - login with the code step
  >   - the first login's 2FA setup
  >   - registering with an invite
  >   - the DoD's story: an Admin invites; the invitee registers, sets up 2FA and logs in; the Admin finds them in the user list
  >
  >   `just e2e` is in `check` and `ci`, and a new `e2e` CI job runs on windows-latest.
  > - **Dependencies,** exact devDependencies of apps/desktop: `@playwright/test` 1.64.0, `msw` 3.0.3, `@msw/playwright` 0.7.0 and `@types/node` 24.19.2. None runs an install script. vitest's `@vitest/mocker` names msw ^2 as an optional peer, which pnpm reports as unmet; vitest runs only in Node here and never loads it.

  > Note (P8.9, as built, group E) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)):
  > - **17 specs:** 5 in Edge (two login specs, the first login, registering, the story) and 12 in Node (`fetchInvoke` through `TauriApiClient`, the CSP and its served header, the fake against the contract).
  > - **Red first:** the bundle guard's script 3 of 4 against a stub. The specs were written with the code, so each was shown red through a temporary mutation:
  >   - a `<style>`, a `console.error` and an `eval` in the e2e entry, each caught (the style count with a `style-src-elem` violation, the console, a `script-src` violation)
  >   - the story, with the fake skipping the step-up
  >   - `e2eCsp` returning its input: 1 of 2
  >   - `fetchInvoke` resolving everything: 6 of 7
  >   - the fake missing a command: 1 of 3
  >   - the real guard, with the marker added to the shipped desktop bundle
  > - **Every answer is a 200 with an envelope:** the browser logs a failed request as a console error, which the guards would count.

**Security:**
- Tokens never reach JavaScript.
- No `dangerouslySetInnerHTML`.
- Inputs are validated with zod before `invoke`, and the server validates them again.

**DoD:**
- A friend can install a dev build, register with an invite and log in with 2FA, and an Admin can manage users.
- Vitest and Playwright are green.

> Note (Phase 8's DoD, group E, the user's decision) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)): **"A friend can install a dev build" moves to Phase 12's DoD.** A friend's PC can reach a server only over HTTPS, which Phase 12 brings (Caddy), and the app allows plain http only to a loopback address. Phase 8 proves the rest on the user's PC, by the PR's hands-on steps: the release exe against `just dev-server` over loopback, an invite created in the app, registering, setting up 2FA, logging in, and an Admin managing users. Playwright runs the same story on every push.

---

### Phase 9: Credential vault & Microsoft accounts
**Goal:** Users link their Microsoft accounts securely. Tokens are encrypted at rest and refreshed safely.
**Introduces:** `azalea-auth`, `moka`, `tauri-plugin-opener`.

> Note (P9, Phase 9, group B, the user's decision) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **azalea-auth isn't introduced.** fleet-server's Microsoft adapter sends its own requests, and `deny.toml` keeps azalea-auth out of every member. Group B adds reqwest (`form`) and rustls to fleet-server, and rcgen to its dev-dependencies.

> Note (P9, Phase 9, group C, the user's decision) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **moka isn't introduced either.** The session-token service's single-flight is its own per-account lock, and group C adds no dependency: only tokio-util's `rt` feature on fleet-server, for a `TaskTracker`.

> Note (P9):
> - **Six group branches.** At the user's request, Phase 9 is built in group PRs like Phases 2–8. Each group has one branch and one commit per task, and the groups run in this order, each after the previous PR is merged:
>
>   | Group | Branch | Tasks |
>   |---|---|---|
>   | A | `p9/p9.1-p9.2-accounts-table-and-vault` | P9.1's `mc_accounts`; P9.2 |
>   | B | `p9/p9.3-microsoft-auth-provider` | P9.3, alone, so the Microsoft token handling gets a focused review |
>   | C | `p9/p9.5-session-token-service` | P9.5 |
>   | D | `p9/p9.4-p9.7-link-flow-and-offline` | P9.4, P9.7 |
>   | E | `p9/p9.1-p9.6-grants-and-account-endpoints` | P9.1's `account_grants`; P9.6 |
>   | F | `p9/p9.8-app-screens-wrap-up` | P9.8; the DoD, the threat model and the README |
>
>   - **P9.1 is spread over A and E** and ticked in E: each table arrives with its first user.
>   - **New dependencies** arrive with their group, each approved with its version, license and release date: azalea-auth in B, moka in C, tauri-plugin-opener in F.
>   - **Hands-on checks** use the user's own Microsoft account; each group's PR names exactly what the user does and should see.
>   - Each later group asks its own implementation questions in its session.
> - **Decisions.** The user answered group A's questions on 2026-10-14, one at a time. [ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md) records them; the notes below summarize what changes a task.

> Note (P9, from Phase 8, group E, the user's request) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)): **Decide step-up per action, the same way as the admin writes.** Phase 8 put role changes, disabling users and invites behind step-up, so an unlocked PC with a live session can't do them without the password. Before building them, decide for each destructive or privilege-changing action of this phase whether it needs a step-up: unlinking or deleting a Microsoft account (already in §7.2's list), changing or revoking a grant, rotating keys through the API. The app reuses the admin screens' "send, and on `step_up_required` ask for the password and retry once".

- [x] **P9.1** 🔴 Migrations and repositories:
  - `mc_accounts`: owner, MC UUID and name, kind, encrypted refresh token, `key_id`, status
  - `account_grants`

  > Note (P9.1, from the group A review): **Account identity.** `McUsername` keeps its case, but Minecraft names are case-insensitive. Identify accounts by MC UUID; any lookup or comparison by name ignores case.

  > Note (P9.1, Phase 9, group A, the user's decisions) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **`mc_accounts` is built in group A, `account_grants` in E**, which ticks P9.1.
  > - **One table for both kinds** (`0008_mc_accounts.sql`). An offline account has no UUID and no envelope. A Microsoft account is unique by its profile's UUID, pending ones included; an offline account by its lowercased name.
  > - **No `key_id` column:** the envelope's header names its key, as in `user_mfa`.
  > - **No CHECK** on the status's values (`pending`, `active`, `reauth_required`, and `refused` since group C) or the name's shape; fleet-core's `AccountStatus` and `McUsername` check them when a row is read back. An offline account is always `active`.
  > - **Core types:** `fleet_core::account::{AccountKind, AccountStatus}` and `fleet_core::value::McUuid`, a non-nil UUID in lowercase hyphenated form.
  > - **The repository** has only what A uses: `insert` (`Taken` for a linked account) and `find`.

  > Note (P9.1, Phase 9, group E) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **`account_grants` as built** (`0010_account_grants.sql`) is `account_id, user_id, level, granted_by, granted_at`, as Appendix D has it.
  > - **One grant per account and user** (`PRIMARY KEY (account_id, user_id)`), so changing a grant rewrites its row, and no cap is needed (S10 in ADR-0018's group E). The rows go with their account (`ON DELETE CASCADE`); users are never deleted.
  > - **CHECKs:** the ID and time shapes, `level` in `view`/`control`/`manage` (a closed set, as `users.role`), and `user_id <> granted_by`, a belt behind `authorize()`'s ban on granting to oneself. The owner never has a row.
  > - **The repository** (`ports::grants`, `infra::sqlite::grants`): `GrantReads::list` (a keyset page by grantee ID, with both names) and `GrantWrites::{find, insert, set_level, delete}`; a second grant is `InsertGrantError::Exists`. Statuses are the services' business, not the table's.
- [x] **P9.2** 🔴 Vault extensions:
  - purpose-bound AAD (`account_id|ms_refresh`)
  - `vault rotate-key` CLI that re-encrypts transactionally
  - lookup by `key_id`
  - a test that a ciphertext swapped between two accounts fails to decrypt

  > Note (P9.2, Phase 9, group A, the user's decisions) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)):
  > - **"Lookup by `key_id`"** is P7.4's `open`, which already finds the key the header names, plus `key_id_of` (the header, read without a key) and **`vault status`**. That's a report of what each key ID seals, which reads no key file and exits 3 when an envelope names a key that isn't configured or is malformed.
  > - **`vault rotate-key`** re-encrypts only the envelopes under other keys than the active one.
  >   - The crypto runs before the transaction (ADR-0015's rule). The one transaction swaps each row only while it holds the envelope that was read, and rolls back if anything changed.
  >   - An envelope that doesn't open stops it, with one `vault.rotate_key` Failure entry each.
  >   - Neither vault command creates or migrates the database.
  > - **The swap test** is `a_refresh_token_moved_to_another_account_is_rejected` (the vault) and `rotation_refuses_refresh_tokens_swapped_between_two_accounts_and_records_each` (the rotation).
  > - **CLI only:** no API route for key rotation, so no step-up to decide.
- [x] **P9.3** 🔴 `MicrosoftAuthProvider` port (`start_device_flow`, `poll`, `refresh`, `minecraft_session`), with an `azalea-auth` adapter and a fake.
  - Use azalea-auth's default client ID; a custom Azure app ID would need approval from Mojang.
  - **Never** use azalea's file cache.
  - At `trace`, azalea-auth logs Microsoft access and refresh tokens. `fleet-server` caps the `azalea_auth` log level at `info`, whatever the configured filter says.

  > Note (P9.3, from Phase 3, group C, the user's decisions): **The `azalea_auth` cap, in detail.**
  > - **What it covers.** At `trace`, azalea-auth 0.16 logs the Microsoft access token, the whole token response (refresh token included), the Xbox Live and XSTS auth responses, the Minecraft auth, ownership and profile responses, and the account cache. At `debug` it logs nothing secret.
  > - **One rule for both binaries.** fleet-server's filter caps `azalea_auth` at `info`, whatever the configured filter says. The agent has the same cap (P5.2), because azalea logs the chat-signing private key at `trace` in every online session.
  > - **Tested.** P9's redaction test checks the cap: with the configured filter at `trace`, nothing from `azalea_auth` below `info` gets through (ADR-0011, threat model).

  > Note (P9.3, from Phase 6, group A, the user's decisions) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)):
  > - **The cap already exists.** fleet-server's log layer comes from fleet-startup, which caps `azalea_auth` at `info` for every binary, outside the rules a binary can add, since Phase 6. P9.3 adds no second cap; P9's redaction test still checks this one on the server.
  > - **stable-check.** fleet-server is in the justfile's `stable_crates` since Phase 6, because it didn't depend on azalea-auth yet. When azalea-auth arrives here, fleet-server leaves `stable_crates`, as the rule says (P0.11).
  > Note (P9.3, from Phase 8, group A) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)): **fleet-client leaves `stable_crates` with fleet-server,** since fleet-server is its dev-dependency and `stable-check` builds `--all-targets`.
  > Note (P9.3, from Phase 9, group A's planning) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **the desktop crate leaves `stable_crates` too.** `apps/desktop/src-tauri` also dev-depends on fleet-server, and `stable-check` includes it off Linux.
  > Note (P9.3, Phase 9, group B, the user's decision) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **correction to the three notes above: fleet-server, fleet-client and the desktop crate stay in `stable_crates`.** They rested on fleet-server getting azalea-auth, which it doesn't; the justfile's rule (crates that don't depend on azalea or azalea-auth) still holds for all three.
  > Note (P9.3, Phase 9, group B, the user's decisions) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)):
  > - **No azalea-auth adapter.** azalea-auth 0.16.0 checks no HTTP status, so `invalid_grant`, a 429, an Xbox refusal and a missing profile look alike. Its only poll is a loop that drops every refusal, it can panic on an unexpected answer, and its types print tokens in `Debug`. `infra::msauth::ReqwestMicrosoftAuth` sends its own requests with azalea-auth 0.16.0's protocol: the default client ID, scope, fields and headers. `deny.toml` keeps azalea-auth out of every member, and the `azalea_auth` cap stays as a guard.
  > - **The port** (`ports::msauth`) has one error enum per method.
  >   - `poll` makes one try and returns `Pending`, `SlowDown`, `Declined`, `Expired` or `Authorized`.
  >   - `refresh` returns the token to store every time; a 200 without one keeps the token sent (RFC 6749 §6). Only `invalid_grant` is `RefreshError::InvalidGrant`.
  >   - `minecraft_session` runs Xbox Live, XSTS, the Minecraft login, the entitlements (none at all is `NotOwned`) and the profile (`NOT_FOUND` is `NoProfile`), with named `XboxRefusal`s.
  >   - Failures: `Unreachable`, `Certificate`, `TimedOut`, `RateLimited`, `Unavailable` and `Unexpected {endpoint, rule}`. An unknown OAuth code is named only when it matches `[a-z_]{1,64}`.
  > - **Microsoft's answer is input.**
  >   - Bounded numbers: interval 1–60 s (5 s when missing); device code 60 s–1 h; access token 60 s–1 day; Minecraft token 60 s–2 days.
  >   - Every token is 1 B–16 KiB of visible ASCII, which is the stored refresh token's upper bound.
  >   - Every body is at most 256 KiB.
  >   - The verification link must be https to `www.microsoft.com` or `microsoft.com`.
  >   - Expiries come from the `Clock`, read just before the request.
  > - **The client:** 5 s connect, 15 s per request, 20 s per call (60 s for `minecraft_session`), no redirects, no proxy, https only, no retries. A failed call logs one `debug` line without any value.
  > - **Tests without Microsoft:** a `#[cfg(test)]` constructor against an axum fake, raw TCP servers and an rcgen-certified TLS server. The fake for later groups lives in the `tests/accounts/` binary. Coverage has a new `msauth` gate at 90 %.
  > - **The provider:** `serve` installs aws-lc-rs as the rustls provider in main.rs's `start()`, never in the library's `serve::run`.
  > - **Hands-on:** `just test-ms-account` runs two `manual_` tests the user runs with their own account, the only tests with a real sleep.
- [x] **P9.4** 🔴 Device-code flow:
  - `POST /accounts/link` returns `{flow_id, user_code, verification_uri, expires_at}`.
  - A background poller runs bounded and cancellable, with per-user and total caps.
  - `GET /accounts/link/{flow_id}` returns the status.
  - On success, check the profile and game ownership, store the encrypted token, and give the creator Manage.
  - Member quota from `accounts.max_per_member`.

  > Note (P9.4, from Phase 9, group C) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): a new link's status has no reason, and the link flow calls `minecraft_session` itself; the session-token service serves linked accounts only.

  > Note (P9.4, from group D): Starting a flow checks `LinkAccount` (every role). `GET /accounts/link/{flow_id}` checks `UsePersonal` on `ResourceContext::Personal{owner}` with the flow's creator, so other users get 404 (ADR-0010).

  > Note (P9.4, from Phase 9, group A, the user's decisions on device-code phishing) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)):
  > - **New links wait for approval.** A Member's or an Admin's new account is `pending` until someone above them approves it (a Member's: an Admin or the Owner; an Admin's: the Owner). The Owner's own links are `active` at once. No bot can use a pending account.
  > - **One link per Minecraft account.** A UUID that's already linked, pending ones included, can't be linked again by anyone (`InsertAccountError::Taken`); the refusal names nobody.
  > - **`account.link`** records the creator, the Minecraft name and the UUID.

  > Note (P9.4, Phase 9, group D, the user's decisions) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)):
  > - **Ownership is Manage.** The creator becomes the account's owner, which `authorize()` already treats as Manage. E adds no grant row for the owner.
  > - **The quota,** `[accounts] max_per_member` (1–100, 3 by default), counts every account a Member owns, of any kind and status; Admins and the Owner have none. It's checked when a flow starts (409 `conflict`) and again in the commit's transaction (the flow ends `over_quota`).
  > - **The caps:**
  >   - One flow per user, a fixed rule: a new start replaces the user's waiting flow, whose code is then never polled again, but not one Microsoft already approved (429, `Retry-After: 5`).
  >   - `[rate_limits] link_flows_total` (1–16, 3) users at once on the server: one more is 503 `busy` with `Retry-After: 30`.
  >   - `[rate_limits] link_flows_per_user_per_day` (1–100, 10) starts in any 24 hours, counted from the `account.start_link` entries in the audit log, so a restart doesn't reset them: 429 until enough of them are a day old.
  >   - The cheapest check comes first, and Microsoft is asked last. When Microsoft fails the start, it's 503 with 30 s for a passing failure and 500 for `Certificate` or `Unexpected`.
  > - **`GET /accounts/link/{flow_id}`** answers `waiting` (with `expires_at`), `finishing`, `declined`, `expired`, `taken`, `over_quota`, `refused` (with `reason`), `failed` or `linked` (with `account_id`, `account_status` and `name`). An unknown or replaced flow, another user's, and a result 15 minutes after its end are all the same 404. The user code is only in the start's 201 answer.
  > - **Failures:** a passing poll failure waits for the next poll, and a 429 also slows the polls by 5 s. After the approval, a passing failure of the Minecraft session or of the database is retried after 5 s doubling to a minute, for at most 5 minutes and never past the access token's expiry. `Certificate` and `Unexpected` end the flow at once.
  > - **The commit** reads the creator and the session that started the flow again (`users::recheck`): a revoked session, a logout or a disabled creator stores nothing, while a refreshed session still links. The role read there decides `pending` or `active`. It records `account.link` Success, or a Failure for `taken` and `over_quota`; every accepted start records `account.start_link`.
  > - **Shutdown** cancels every flow at once; only a commit that started finishes. A restart loses the flows. A Microsoft client that can't be built stops serve's start (`StartError::Microsoft`).
  > - **Tests** wait through a pacing port (`ports::pace`), whose fake moves the manual clock by each wait; `app/link_flows/` joins the `msauth` coverage gate.
  > - **Hands-on:** `just test-ms-account link_flow`.
- [x] **P9.5** 🔴 `SessionTokenService`:
  - single-flight refresh per account (moka)
  - stores rotated refresh tokens atomically
  - caches the MC session token until shortly before it expires
  - on `invalid_grant`, marks the account `ReauthRequired`

  > Note (P9.5, Phase 9, group C, the user's decisions) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)):
  > - **No moka:** each account's slot has a tokio `Mutex`, whose owned guard a task on the service's `TaskTracker` holds until the rotated token is stored. A dropped or timed-out caller never loses a rotated token.
  > - **Order:** a refresh, the rotated token's store (`account.refresh`), then `minecraft_session`; nothing is handed out unless the store succeeded. No Microsoft access token is kept.
  > - **A failed store** keeps the token in the slot for the next request, which stores it first. It's discarded only when the row's envelope changed or the row is gone.
  > - **Who gets a session:** an active Microsoft account whose owner isn't disabled (`OwnerDisabled`), read on every request.
  > - **Statuses:** `invalid_grant` marks `reauth_required`. NotOwned, NoProfile and the Xbox refusals mark a new `refused` with its reason in a new `status_reason` column (`0009`). A profile with another UUID marks `refused`/`profile_mismatch` with `security.account_profile_mismatch`. A renamed profile's name is handed out and stored (`account.rename`). Every change is a compare-and-set and audited (`account.set_status`).
  > - **Cache:** until 10 minutes before expiry (`REUSE_MARGIN`), from the `Clock`. A fresh request reuses only a session whose minting started after it arrived.
  > - **Backoff:** an account backs off 5 s doubling to 5 min after a failure; a 429 pauses every account 30 s doubling to 10 min. Only calls to Microsoft wait; cached sessions are still handed out.
  > - **Wait bound:** 2 minutes; `with_wait_bound` can only shorten it.
  > - **Not wired yet:** P10.6 builds it in serve. No hands-on check in C.
- [x] **P9.6** 🔴 Account endpoints:
  - list (only what the user may View)
  - get
  - delete (step-up + Manage)
  - grants CRUD (never above your own level)
  - re-link

  > Note (P9.6, from Phase 9, group D) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)):
  > - **The owner's Manage is ownership:** no grant row for the owner, and a grant to oneself stays refused.
  > - **The quota counts every account** a Member owns, so deleting one frees its slot. Deleting must also work for offline accounts while `accounts.allow_offline` is off, so a Member with leftover offline accounts can free those slots (P9.7).
  > - **A refused pending link** still fills the Member's quota: either refusing it deletes it, or the Member can delete it themselves. E decides which.
  > - **Reuse D's DTO enums** (`AccountStatus`, `RefusalReason`) for the account list.

  > Note (P9.6, from group D) (ADR-0010):
  > - **Permissions.** `ViewBot` for list and get, `DeleteAccount`, `RelinkAccount`, `ManageGrants` (list, revoke) and `Grant{level, grantee}` (create, change), all on `ResourceContext::Account{owner, grant}` with the actor's own grant.
  > - **Grants.** Only Manage edits grants, and nobody grants to themselves.
  > - **Flagged, not decided:** how a Member picks a grantee. Members can't list users, so the dialog (P9.8) needs a lookup that doesn't let them enumerate usernames.

  > Note (P9.6, from Phase 9, group C) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **re-link** must check that the profile's UUID is the account's, and sets `active` (clearing a `refused` reason) together with the new envelope, which makes a refresh in flight drop its rotated token. A `refused` or `reauth_required` account comes back through re-link; E decides whether approve or refuse may touch them.

  > Note (P9.6, from Phase 9, group A, the user's decisions on device-code phishing) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **approving and refusing a pending link** get endpoints here, and this group decides whether each needs a step-up. Admins and the Owner see every account with who linked it and when.
  > *(Group E, the user's reading: every account they can view under ADR-0010. Only the Owner sees all of them; the Owner's and other Admins' accounts stay 404 for Admins.)*

  > Note (P9.6, Phase 9, group E) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **as built.** The user's choices (U1–U4) and the review agent's decisions the user approved at the plan review (S1–S14):
  > - **Routes:** `GET /accounts` (what the caller may see), `GET /accounts/awaiting-approval` (the pending links the caller may review, for F's Home), `GET /accounts/{id}`, `DELETE /accounts/{id}`, `POST /accounts/{id}/approve`, `/refuse` and `/relink`, `GET` and `POST /accounts/{id}/grants`, `PUT` and `DELETE /accounts/{id}/grants/{user}`. Deleting, reviewing and every grant write need a step-up.
  > - **What each viewer sees:** the account's own facts and their own `level`; `owner` and `linked_at` only for Admins, the Owner and Manage holders. A hidden account is 404 like an unknown one (U1).
  > - **Reviewing:** someone above the owner by current role, only a pending account (409 otherwise). Approving makes it active (`account.approve`); refusing deletes it with its grants, freeing the quota slot and the UUID (`account.refuse`).
  > - **Deleting:** Manage, any status and kind, offline ones too while `allow_offline` is off (`account.delete`).
  > - **Grants:** created by exact username; an unknown name, a disabled user and anyone with implicit Manage on the account get one 422 on `username`, "can't be given a grant: unknown, disabled, or already has access; check the name" (U4), recorded as a `grant.create` Failure with its reason, never the name. A second grant or a pending account is 409. Changing and revoking go by user ID; a raise for someone who has implicit Manage by now is 409; no cap. `grant.create`, `grant.set_level`, `grant.revoke`.
  > - **Re-link:** a second kind of P9.4's flow, sharing its slot, caps, daily count, retries and shutdown, without a quota and without a step-up. A pending or offline account is 409 (U2). The commit asks `authorize()` again, refuses another profile (`refused`/`profile_mismatch`, an `account.relink` Failure) and otherwise swaps the envelope, sets `active` and stores the profile's current name (`account.relink`).
  > - **No hands-on check** (U3): re-link makes the same Microsoft calls P9.4's check covered.
- [x] **P9.7** Offline accounts only when `accounts.allow_offline = true`. It's off by default and meant for dev.

  > Note (P9.7, Phase 9, group D, the user's decisions) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)):
  > - **`POST /accounts/offline {name}`,** an authed route for every role (`LinkAccount`), without a step-up. The name follows `McUsername` (3–16 of letters, digits and `_`); the answer is 201 `{id, name, kind, status}`, always `active`, with no approval. A name taken in any case is 409 and names nobody.
  > - **It counts toward the Member quota** (409 when full), like every account, and is recorded as `account.link` with `kind` offline, or a Failure for `taken` and `over_quota`.
  > - **With `allow_offline` off,** the route answers 404 like an unknown one, before the body is read; it stays registered and in the `OpenAPI` document. Turning the switch off again keeps the existing offline accounts untouched, and serve warns at startup with how many are kept. No bot may run on one while it's off (P10.5, P10.6).
  > - **The dev config** (`deploy/dev/server.toml`) turns it on, for the offline-mode test server.
- [x] **P9.8** 🔴 App screens:
  - accounts list
  - **"Link Microsoft account" dialog**:
    - shows the code with a copy button
    - opens the verification URL through a Rust command that uses `tauri-plugin-opener`
    - shows the live status
  - sharing dialog (grants)
  - re-auth prompt

  > Note (P9.8, from Phase 9, group C) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): the account list and the re-auth prompt need a text per `RefusalReason` and per status; `xbox_other`'s code is in the audit log only.

  > Note (P9.8, from Phase 9, group D) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)):
  > - **Starting a flow is a direct call** (`POST /accounts/link` answers the user code, a secret); reading its status may be a query.
  > - **A text per `LinkFlowState`,** and for each refusal of the start: 409 (quota), 429 (daily cap, or the user's flow finishing), 503 (every place taken, or Microsoft away).
  > - **fleet-client's methods** for the two routes come with the dialog, with the Tauri commands and `ApiClient`.

  > Note (P9.8, from Phase 9, group A, the user's decisions on device-code phishing) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)):
  > - **The link dialog** says, in the user's words: "Only enter a code you started yourself here. Never enter a code someone sent you, it would hand them your Minecraft account." Home carries no warning.
  > - **Home** shows Admins and the Owner how many links wait for their approval, so none is missed.

  > Note (P9.8, from Phase 9, group E) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)):
  > - **Home's count** reads `GET /accounts/awaiting-approval` (pages of 100 until `next` is gone); a Member gets an empty list. Approve and refuse, delete, and the grant writes go through `useStepUpAction`; starting a re-link is a direct call (its answer carries the code) and reuses the link dialog.
  > - **Texts** for: the 409s (not pending; a pending or offline account linked again; a grant that exists; a pending account shared; a raise for someone with implicit Manage), the grant's 422 (the user's text, U4), the by-name limit's 429, and `profile_mismatch` when linking again. A View or Control grantee's list has no owner column.
  > - **fleet-client's methods**, the Tauri commands and `ApiClient` for the eleven routes come with the screens.
  > - **The threat model** gains: approving, refusing, deleting and the grant writes need a step-up; names a Manage holder can probe, bounded by the per-user limit (an accepted risk); someone at the Owner's unlocked PC can still start a link that's active at once; an Admin's linked row can be deleted only by the Owner.

  > Note (P9.8, Phase 9, group F) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **as built.** The user's choices (U5–U15) and the review agents' decisions the user approved at the plan review (S1–S7):
  > - **Where:** "Accounts" in the navigation for every role (`/accounts`): one card per account; above them, for Admins and the Owner, the links waiting for their approval as cards with Approve and Refuse. Each account has its own screen (`/accounts/<id>`) with its facts, the re-auth prompt, "Who has access" (grants by exact username, level picker, Revoke) and Delete. Cards show the re-auth prompt with "Link again" for Manage holders.
  > - **The link screen** is its own route (`/accounts/link`, `/accounts/<id>/link-again`), in two steps: the warning and "Get a code", then the code with "Copy code", Microsoft's page (it opens by itself once), and the status, read every 2 s until it ends. The code lives only in the screen's state: starting is a direct call, and the code goes when the flow ends.
  > - **The opener:** `open_verification_link`, a synchronous command without arguments (on the main thread; one approved `#[expect(clippy::needless_pass_by_value)]`), opens only the link the newest start answered with, checked when it arrived by `fleet_client::VerifiedLink`, a copy of group B's rule compared case by case with the server's. A bad link drops the whole answer (`invalid_verification_link`). tauri-plugin-opener 2.7.0 isn't registered. The capability grows by exactly 15 `allow-` entries (35).
  > - **Home** shows Admins and the Owner how many links wait (an alert, and a badge on Accounts), read when Home or Accounts mounts, after the user's own writes, and when the window regains focus; no timer.
  > - **Offline accounts** have a form behind its own button on Accounts.
  > - **Texts:** the user's table at the plan review; a start's 429 and 503 have one text each. Long waits count down in minutes, or hours and minutes, in every form.
  > - **Tests:** fleet-client and the bridge reach the routes without a Microsoft fake (refusals before Microsoft, offline accounts, pending rows written to the store); the 201s over fleet-client's scripted transport; the held link in-crate; two e2e stories.

**Security:**
- Refresh tokens never leave the server.
- A test checks that logs are redacted.
- fleet-server's log filter caps `azalea_auth` at `info`, the same cap as the agent's (P5.2), and the redaction test checks it. At `trace`, azalea-auth logs Microsoft and Minecraft tokens, the refresh token and the account cache (P9.3).

  > Note (Phase 9's security, group B) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): fleet-server has no azalea-auth code, so the cap is a guard. `tests/telemetry.rs` checks it with the filter at `trace` from the config file or the environment, and `msauth::tests::no_secret_reaches_any_log_at_any_level` checks the adapter's own logs.
- AAD-binding test.
- Per-user caps on link flows.

**DoD:**
- A real Microsoft account is linked end to end.
- The DB contains only ciphertext.
- `rotate-key` works on a copy of the DB.

> Note (Phase 9's DoD, group F) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **how each is shown.**
> - **A real account linked end to end:** the user's hands-on check in group F's PR, through the app against `just dev-server`, ending with the account deleted in the app and `target/dev-server` wiped.
> - **Only ciphertext:** `at_rest::no_token_reaches_the_database_file_after_a_link_a_link_again_and_a_refresh` reads the database's files, open (with the WAL) and closed, after a link, a link again and a refresh with the fake, and finds no token or code.
> - **`rotate-key` on a copy:** `at_rest::rotate_key_moves_a_linked_accounts_token_to_the_new_key_on_a_copy` runs the real binary's `vault rotate-key` and `vault status` on a copy with a linked account; the copy then works with the new key alone, and the original stays under the old one.

---

### Phase 10: Control plane (agent ⇄ server, gRPC + mTLS)
**Goal:** Agents enroll securely, receive the desired state and report the actual state, and everything reconciles after any disconnect.
**Introduces:** `tonic`, `tonic-prost`, `prost`, `tonic-prost-build`, `protox`, `rcgen`, `tokio-stream`, `backon` (IO retries).

> Note (P10, Phase 10's planning, 2026-10-15) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)):
> - **Eight group branches.** Phase 10 is built in group PRs like Phases 2–9: one branch and one commit per task, in this order, each after the previous PR is merged:
>
>   | Group | Branch | Tasks |
>   |---|---|---|
>   | A | `p10/p10.1-fleet-proto` | P10.1, alone: the wire contract and the trust boundary for agent input |
>   | B | `p10/p10.2-p10.3-pki-and-enrollment` | P10.2, P10.3: how an agent gets its identity |
>   | C | `p10/p10.4-p10.10-connect-and-agents` | P10.4, P10.5's `AgentRegistry`, P10.10: who may stay connected, and the switch that ends it; P10.9's rejections are tested here |
>   | D | `p10/p10.5-p11.1-bots-scheduler-reconciler` | P11.1's `bots` and `modes`; the rest of P10.5 (ticked here) |
>   | E | `p10/p11.2-bot-endpoints` | P11.2 |
>   | F | `p10/p10.6-session-grants` | P10.6, alone: the one place Minecraft tokens leave the server |
>   | G | `p10/p10.7-p10.8-agent-client-and-ingest` | P10.7, P10.8, P11.1's `chat_messages` (P11.1 ticked here) |
>   | H | `p10/p10.9-loopback-wrap-up` | P10.9, the DoD, the threat model and the README |
>
>   - **New dependencies** arrive with their group, each shown to the user with its version, license and release date: prost, tonic, tonic-prost, tonic-prost-build and protox in A; rcgen in production code and tonic's TLS features in B; tokio-stream in C; backon's tokio sleeper in G.
>   - **Hands-on checks:** none in A. B and C are local (`just dev-server`, `just dev-agent`). H's DoD check uses the user's real Microsoft account and needs Docker for a local online-mode Minecraft server (as in P1.8), unless the user points it at their own; one machine is enough. It follows Phase 9 group D's manual-test pattern: a vault key drawn for the run and kept in memory, a temporary folder deleted at every end, and only the new test's filter.
>   - **Approvals later groups ask for:** G, a new `commit_unaudited` exemption for the batched writes (`EventBatch`, which ADR-0015 anticipated); B, how the Owner gets an enrollment token while the agents screen is P11.9's.
> - **The phase boundary** *(the user's choice, U1)*. P10.5, P10.6 and P10.8 need P11.1's tables, and the DoD needed P11.2 and P11.9. P11.1's `bots` and `modes` (with the built-in seeds) come with P10.5, their first reader; its `chat_messages` with P10.8, its first writer; all of P11.2, with fleet-client's bot methods, in group E. The DoD's app part moves to Phase 11 (see the DoD below).
> - **protox 0.9.1** *(the user's choice, U2)*, §5's pin, over 0.10.0 (2026-10-07).
> - **Decided by the review agents** *(approved by the user at the plan review)*, S1–S9 in ADR-0019: renewal stays in P13.6 (S1); the validated messages live in fleet-proto (S2); `ChatRequestId` and the agent's session request numbers (S3); the mode as structured messages (S4); times as Unix milliseconds and IDs as 16 bytes (S5); Minecraft text cleaned again, validated values strict (S6); secrets out of `Debug` (S7); no agent labels, `AgentCapacity` 1–1000 (S8); the wire-schema snapshot as P0's "proto codegen check" (S9).

- [x] **P10.1** 🔴 `fleet-proto`:
  - `proto/fleet/v1/*.proto` (Appendix C)
  - `build.rs` using protox and `tonic-prost-build`
  - core ⇄ proto conversions with round-trip proptests that reject invalid proto values

  > Note (P10.1, Phase 10, group A) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **as built.**
  > - **The protocol** is `crates/fleet-proto/proto/fleet/v1/` (`agent`, `bot`, `chat`, `mode`, `common`), package `fleet.v1`: the `AgentControl` service with its `Connect` stream. `RenewCertificate` waits for P13.6 (S1); the enrollment service and its messages come with P10.3, their first user. Appendix C's note lists the refinements.
  > - **The build** compiles the files with protox and generates the client and the server with tonic-prost-build into `OUT_DIR`, without transport helpers. Nothing generated is committed. The build script also renders the compiled wire schema, which `tests/protocol/schema.rs` pins with insta under `PROTOCOL_VERSION` (1): P0's "proto codegen check" (S9).
  > - **`fleet_proto::generated`** carries only `#[allow(clippy::pedantic)]`, the one group of lints the generated code trips.
  > - **`fleet_proto::control`** holds the validated messages (S2): `AgentMessage` (`Hello`, the heartbeat, a snapshot, `BotEvent`, `SessionRequest`) and `ServerMessage` (`Welcome`, a full reconcile, `Assignment`, unassign, update, `SendChat`, `SessionGrant`, `SessionDenied`, `RejectReason`), each read with `TryFrom` from its generated envelope and written back with `TryFrom`. The leaf conversions cover `BotSpec`, `ModeDefinition`, `BotState`, `BotSnapshot`, `StickyState`, `BotNotification`, `DisconnectReason`, `IncomingChat` and `SessionCredentials`.
  > - **Reading refuses** what fleet-core's constructors refuse, a missing field, an enum value or oneof this version doesn't know, an ID that isn't 16 bytes or not v7, a nil profile UUID, a time outside years 0–9999, a count of 0, more than 1000 bots or a bot twice in a hello or a full reconcile, a heartbeat interval outside 1 s–5 min, and a token that isn't 1 B–16 KiB of visible ASCII. A validated text must arrive exactly as its type keeps it: a server address in its written form, a chat message trimmed. Minecraft's text is cleaned again instead (S6). `ConvertError { field, index, problem }` never holds a value.
  > - **Secrets** (S7): `OnlineSession` has no derived `Debug`; its own prints the expiry only, its `Drop` wipes the token, and the conversion moves the token into a `SecretString`. A test fails on any field named like a secret outside a redacted message.
  > - **fleet-core** gains `id::ChatRequestId` and `value::{AgentCapacity, AgentVersion}`.
  > - **Tests,** each red first against compiling stubs with the real signatures (`tests/protocol/`, 164 tests, then the exhaustive enum tables); the cases a stub happened to satisfy were shown red by temporary mutations, each reverted and checked by checksum (ADR-0019).
- [x] **P10.2** 🔴 PKI:
  - `afkfleet-server pki init` creates the CA and the server certificate. The CA key is its own secret.
  - CSR signing with a fixed lifetime (default 60 days), with the agent ID in the SAN.
  - Fingerprints are stored in `agent_certs`.

  > Note (P10.2, from Phase 10's planning, S1) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **room for P13.6's renewal.** `agent_certs` is keyed by fingerprint, and `agent_id` is indexed but not unique: a renewing agent holds two certificates. The agent ID in the SAN comes from the server's record, never from the CSR.

  > Note (P10.2, Phase 10, group B) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **as built.**
  > - **`[grpc]` is optional** (S13): `bind` (`127.0.0.1:7443` by default; not port 0 nor `[http] bind`'s port), the four files `ca_cert_file`, `ca_key_file`, `server_cert_file` and `server_key_file` (required once the section is there, absolute, all different), and `agent_cert_lifetime_days` (1–365, 60). Without the section the control plane is off.
  > - **Every key is Ed25519** (S10): 32 bytes from the `SecureRandom` port, wrapped as PKCS#8 v1 by `fleet_core::pki::ed25519_pkcs8` (RFC 8410's fixed prefix), never drawn by rcgen or aws-lc. A CSR carries only an Ed25519 key.
  > - **The profiles** (S11, `infra::pki::profiles`): the CA (CN `afkfleet CA`, ten years, CA with path length 0, keyCertSign), the server (CN `afkfleet server`, one year, serverAuth, the names given as `--dns` and `--ip`) and agents (CN `afkfleet agent`, `agent_cert_lifetime_days` read when signing, clientAuth, the one SAN `urn:uuid:<agent ID>`). Serials are 16 bytes from `SecureRandom`; a certificate is valid from an hour before the clock's time, in whole seconds, and never outside 1970–9999.
  > - **`afkfleet-server pki init`** writes exactly the four `[grpc]` files and never overwrites one (S12): every target and its temp file are checked first, then each file is written to a `create_new` temp file (0600 on Unix) and published with `hard_link`, keys first and the CA certificate last; a failure removes only this run's files. It prints the CA's fingerprint as openssl does, the server certificate's names and expiry, never a key. **`pki server-cert`** issues a new server pair from the existing CA for the yearly renewal, under the same rule: move the old pair away first.
  > - **Signing** (`infra::pki`): a CSR's self-signature is checked (rcgen, x509-parser with aws-lc) and only its public key is used; a request asking for any extension, or carrying any attribute, is refused, and its subject is ignored. Attributes are refused since the PR #49 review: rcgen reads extensions through x509-parser's `requested_extensions()`, which sees only the first extension request, so a second one could ask for what the check never sees (`CsrRefusal::Attributes`). `AgentIssuer` is the CA behind `ports::pki::AgentCertificates`, which P10.3's service calls in `spawn_blocking`.
  > - **Serve's startup check** (`infra::pki::load`, wired in with P10.3): the four files through `secret_file`, exactly one certificate or key each, the CA certificate a CA's, the CA key signing a probe certificate rustls's client verifier accepts, the server key the server certificate's, and the server certificate one the CA signed for a server. An expired server certificate doesn't stop serve.
  > - **`agents` and `agent_certs`** (`0011_agents.sql`): `agent_certs` keyed by the certificate's SHA-256 (32 bytes), `agent_id` indexed and not unique; an enabled agent's name is unique ignoring case. Nothing writes them before P10.3.
  > - **x509-parser** is a direct dependency of fleet-server (the user's choice, U5): rcgen 0.14 no longer reads certificates, and the startup check reads the CA flag and the server certificate's names and validity.
  > - **Tests,** red first against compiling stubs: fleet-core's fingerprint and PKCS#8 (RFC 8410's example key), the config, the repository, `infra::pki` (RFC 8032's test key, every profile field read back with x509-parser, the validity edges with rustls's verifiers, CSR refusals, the startup check's refusals, the no-clobber writer with a file appearing midway) and the `pki` commands, in-process and through the binary. Tests that passed against a stub were shown red by a temporary mutation, each reverted and checked by checksum.
- [x] **P10.3** 🔴 Enrollment:
  1. An Admin calls `POST /agents/enrollment-tokens` and gets a single-use token, valid 15 min, stored hashed.
  2. `afkfleet-agent enroll` creates a key and a CSR (rcgen).
  3. It calls `AgentEnrollment.Enroll`. This RPC uses TLS without a client certificate and is rate-limited; the agent pins the CA fingerprint from the enrollment bundle.
  4. The agent stores its cert and key, readable only by itself.

  > Note (P10.3, from group D): **Only the Owner** creates enrollment tokens (`EnrollAgent`), not an Admin as step 1 says. An agent receives session tokens for the bots assigned to it, and P10.5 assigns bots to any agent with free capacity, so an Admin's own agent could otherwise get the Owner's. Viewing and disabling agents stays Admin+ (ADR-0010).

  > Note (P10.3, from Phase 10, group A) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)):
  > - **The enrollment service and its messages** join the proto here, their first user, with the wire schema's snapshot. `EnrollRequest` holds the enrollment token, so it follows S7: listed in the build script's `REDACTED`, a `Debug` in `fleet_proto::redact` that never prints it, a `Drop` that wipes it, and a `SecretString` at the conversion.
  > - **The agent's key and certificate** go on its writable volume (`/data`, Appendix A), never a Docker secret, owner-only and written atomically (S1).
  > - **To ask the user:** how the Owner gets a token while the agents screen is P11.9's.

  > Note (P10.3, Phase 10, group B) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **as built.**
  > - **Tokens** (S16, S17): `POST /api/v1/agents/enrollment-tokens` is the Owner's, a step-up route (class Standard, plus the per-user limit) without a body. It answers 201 `{id, bundle, ca_fingerprint, expires_at}`; 404 while `[grpc]` is off (after the step-up check, as `POST /accounts/offline` does); 409 when 20 tokens are pending. A token (`afk_et_` and 43 characters) lives 15 minutes, is stored only as its SHA-256 (`enrollment_tokens`, `0012`), and is used at most once, by a conditional update inside the enrollment's transaction. `enrollment_token.create` is audited with the Owner and the IP, `via` = `http`.
  > - **From the server's host** (the user's choice, U3): `afkfleet-server agent create-enrollment-token --config …` creates the same token without the API. It never creates or migrates the database, and reads only the CA certificate, never its key. Its audit entry has no actor and no IP, `via` = `cli`. The bundle alone goes to stdout and its log lines to stderr (fleet-startup's new `init_on`), so `$bundle = …` takes the bundle whole.
  > - **The bundle** (S15) is one line of 146 characters, `<token>.<CA fingerprint>`, the fingerprint SHA-256 over the CA certificate's DER in openssl's form.
  > - **`AgentEnrollment.Enroll`** on the gRPC port (S13, S14, S21): TLS with the server certificate; a client certificate is optional, but one that's presented must chain to the CA. A gate spends `[rate_limits] enroll_per_ip_per_min` (1–60, 5) before the body is read. Requests decode at most 8 KiB and answers encode at most 16 KiB. The call's 15 s deadline covers the token, the name, the CSR's check and the signing, which write nothing (`DEADLINE_EXCEEDED`); a refusal's record and the enrollment's transaction then run to their end, bounded by the write pool's acquire timeout and SQLite's busy timeout, and the answer is sent (the PR #49 review: sqlx finishes a COMMIT already sent when its future is dropped). The checks run in S21's order: the conversion, the token (one `UNAUTHENTICATED` for every problem), the name (`ALREADY_EXISTS`), the CSR (`INVALID_ARGUMENT`), then one transaction (the token again, the agent, its certificate's fingerprint, the token used, `enrollment_token.use` and `agent.enroll`). A real token that can't enroll is audited as an `agent.enroll` Failure, at most 10 per token. Every answer carries `x-request-id`, every refusal a fixed message.
  > - **The certificate goes back as PEM** (`EnrollResponse.certificate_pem`, R10, decided by a review agent during the build): the agent can't PEM-encode DER without a new dependency, and the server holds rcgen's PEM of the DER it fingerprints.
  > - **`afkfleet-agent enroll --config …`** (S18, S19, S20): it reads the bundle from stdin only (a terminal is refused, at most 1 KiB, through a duplicate of stdin's handle, never std's buffered `Stdin`) and checks the CA file against the pin before anything is sent. An existing identity or a `.tmp` lock is refused. The Ed25519 key comes from getrandom and is written owner-only before the call, through the handle that created its `.tmp` file, never its path. `Enroll` runs over TLS that trusts only the pinned CA (5 s to connect, 5 s for the handshake, 20 s for the call, 30 s in all). The certificate is checked (one PEM section and nothing around it, a client certificate the pinned CA signed, the agent's key) before both files are linked, never replacing anything. Exit 3 means the server enrolled the agent but the host stored nothing, or may have: once the channel is up, running out of time, a status without the server's `x-request-id` (a broken connection's `UNAVAILABLE`, tonic's `CANCELLED` at the agent's deadline) and the server's `INTERNAL` can't tell (`client::Unclear`, the PR #49 review). `client::call` itself refuses any scheme but `https`. `[control_plane] url` must be `https://host:port`, and its three files absolute and distinct. `run` still refuses `[control_plane]` until P10.7, pointing to `enroll`.
  > - **serve** reads the PKI before the runtime, like the vault's keys, binds `[grpc] bind` next to `[http] bind`, and stops both servers within the one shutdown deadline. Without `[grpc]`, it logs that the control plane is off. An expired server certificate is an `error`, and the API keeps running.
  > - **Windows** (S20): the agent's files, like `pki init`'s, get no owner-only mode there; they inherit their folder's permissions, and both commands say so on Windows.
  > - **Dev:** `just dev-server` creates `target/dev-server/pki/` once (`scripts/dev-pki.mjs`, never overwriting) and turns `[grpc]` on; `just dev-enrollment-token` and `just dev-enroll [dir]` (`deploy/dev/agent.managed.toml`) enroll a dev agent. `dev-enroll` is one line, `scripts/dev-enroll.mjs`, which creates the folder and starts the agent on its own stdin: every recipe line gets just's stdin, so an earlier line could take the piped bundle.
  > - **The PR #49 review** (the user's) also gave `infra::pki::files::NewFile` a `Debug` that shows the path and the length, never the bytes, and corrected the listener's docs: with the grace set, tonic 0.14.6 cuts a connection at 80 s with no GOAWAY at 60 s (R9's note under P10.4). Each fix has a test that was red first, or, for the docs, none (ADR-0019).
  > - **Deviations:** the route takes no body, so rule 3's 422 has nothing to refuse; `PkiLoadError::File` no longer repeats its cause in its own message, and `pki server-cert` prints the whole cause chain.
  > - **Tests,** red first against compiling stubs; those a stub happened to satisfy, and those written after their code, were shown red by a temporary mutation, each reverted and checked by checksum (ADR-0019).
- [x] **P10.4** 🔴 `AgentControl.Connect` bidi stream. It requires:
  - a verified client certificate
  - an allowlisted fingerprint and an enabled agent
  - a version handshake: `Hello` gets either `Welcome` or a rejection
  - heartbeats with a timeout
  - message size limits

  > Note (P10.4, from Phase 10, group A) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)):
  > - **A hello that doesn't convert** (S8: a capacity outside 1–1000, among the rest) gets `Reject`; P10.4 decides the rule for `protocol_version`, which fleet-proto sends as `PROTOCOL_VERSION`, and may refine `Welcome` and `RejectReason`.
  > - **A connection needs** a fingerprint that's neither revoked nor expired, and an enabled agent (S1). P10.9's rejections (revoked, another CA, never enrolled, expired) are tested in this group.
  > - **The agent notices a dead server too** *(the user's note at the plan review)*. Over a half-open connection (a server crash, a NAT timeout) the stream can hang without an error, so the agent would never reconnect. C decides between HTTP/2 keepalive with a timeout, on the agent's channel and on the server for its own check, and a server heartbeat the agent times out on. The loopback suite tests it: a connection that goes silent without closing (e.g. through a test proxy) ends within the bound. C tests the server's side, G the agent's reconnect and resync.

  > Note (P10.4, from Phase 10, group B, the user's note at the plan review) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **an agent's identity comes only from its certificate:** the agent ID in its SAN, looked up by the certificate's fingerprint in `agent_certs`, never `Hello`'s name. A `Hello` whose name differs from the agent's record is refused, so an agent can't appear under another agent's name in logs or `GET /agents`.

  > Note (P10.4, from Phase 10, group B) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)):
  > - **The gRPC port** (`grpc::listener`) serves `AgentEnrollment` alone, behind its gate (`grpc::enrollment`). `AgentControl` joins it behind a gate of its own that refuses a call without a verified client certificate (`peer_certs()`): the listener only verifies a certificate that's presented (S13).
  > - **`max_connection_age`** (60 s plus 20 s of grace) bounds every connection while `Enroll` is the port's only service. With the grace set, tonic 0.14.6 sends no GOAWAY at 60 s: it sleeps for the age plus the grace and then closes the connection forcefully, a hard cut at 80 s, calls in flight included. C replaces it with a bound for connections without a client certificate, or records the residual risk (R9). Removing the grace isn't a fix either, since tonic then polls the finished timeout future again *(the user's note at the PR #49 review)*.
  > - **`agents.enabled`** and the rule that an enabled agent's name is unique, ignoring case, come from `0011`; disabling an agent frees its name for a new enrollment.

  > Note (P10.4, Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **as built.**
  > - **The gate** (S24, S30): a call without a client certificate is `UNAUTHENTICATED` before anything else, with no request ID minted. Then the ID, then `[rate_limits] connect_per_cert_per_min` (1–60, 10), keyed by the presented certificate's fingerprint (`RESOURCE_EXHAUSTED` with `retry-after`), before anything is looked up. The certificates come from the listener's own `TlsConnectInfo<Connection>` (R18).
  > - **Admission** (`app::agents::AgentConnectionService`, behind `ports::agent_connections`, R22): the fingerprint's row in `agent_certs`, then revoked, expired by the server's clock (`now >= not_after`), disabled, and only then the SAN, which must be one `urn:uuid:` naming the row's agent. Every failure is one `UNAUTHENTICATED` "certificate refused". A refusal of a certificate the server knows is an `agent.connect` Failure with its reason and fingerprint, at most 10 per agent in a rolling 24 hours; a certificate no agent holds is an `error` line, never audited and never parsed (R23).
  > - **The handshake:** the first message within 10 s (`HELLO_TIMEOUT`), read by `fleet_proto::control::read_hello` (a hello, then `protocol_version` equal to `PROTOCOL_VERSION`, then the conversion), then the name byte for byte (`Reject(NameMismatch)`, audited like the identity refusals). The hello's transaction checks the certificate and the agent again, records the capacity, the version and `last_seen_at` (never backwards) and commits `agent.connect`; then `Welcome {heartbeat_interval, agent_id}`. `RejectReason` gains 3 to 7 (R14). Every answer is in `grpc::control`'s table, one fixed message each, with the call's request ID.
  > - **Liveness** (S23): `[grpc] heartbeat_interval_secs` (1–60, 10) and `heartbeat_timeout_secs` (2–300, 30, at least twice the interval). The port's HTTP/2 keepalive pings a connection it read nothing from for an interval and closes it when the ping isn't answered by the timeout. A stream ends `DEADLINE_EXCEEDED` when no message that converts arrives within the timeout; a message that decodes but doesn't convert is dropped and keeps nothing alive.
  > - **Sizes** (S25): 8 MiB from an agent, 128 MiB from the server. `fleet_proto::codec::BoundedCodec` counts a hello's bots before prost builds them (more than 1000 is `OUT_OF_RANGE`), and answers a message it can't decode with `INVALID_ARGUMENT`, never prost's `INTERNAL`, so a session tells a malformed message from a failed transport.
  > - **The connections' age** (S22, R9): every accepted socket is a `grpc::listener::Bounded`, cut at 80 s unless a welcomed stream holds its `Lift`; the age starts again when that stream ends. tonic's own age and per-call timeout are gone, and fleet-server's clippy config bans them (U7).
  > - **A stream's end:** its certificate's `not_after` (`Reject(CertificateExpired)`), the heartbeat timeout, a second hello (`FAILED_PRECONDITION`), a message over the limit (`OUT_OF_RANGE`), the agent's close or reset, or the shutdown (`UNAVAILABLE`). At its end the stream's switch is thrown, so nothing queued goes out any more, the slot is freed and the last answer sent at once; then the end is recorded (`last_seen_at` and `agent.disconnect` with its reason, R19) and the lift dropped. A close or a failed transport within one heartbeat interval of the deadline is recorded `heartbeat_timeout`: keepalive closes a silent connection then, and tonic may read that as the agent's own end. After an accept error that isn't one connection's (`EMFILE`, `ENFILE`), the port waits 1 s (`ACCEPT_PAUSE`) before it accepts again, as axum does; tonic would try again at once. serve ends every session at the signal and waits for them before the database closes, so no stream holds tonic's drain.
  > - **Deviations:** the lift is taken before the slot is activated and `Welcome` queued, so the age can't cut the connection between them (S22 and R13 had Welcome, then the lift). A message on a welcomed stream that can't be decoded ends it `INVALID_ARGUMENT` "a message couldn't be read" (`protocol_error`), a row S24's table didn't have. A reset is recorded as `closed`: tonic reads a request's `CANCEL` as the stream's end.
  > - **Tests:** `tests/grpc_control/` over real TLS on `127.0.0.1` (P10.9's four rejections, every row of the answers table, a silent stream, a silent connection with and without a call through a proxy that stops forwarding, the age), U6's test on paused time (an agent's stream lives a virtual hour through the real router over in-memory pipes while a connection without a certificate is cut at 80 s), and a serve test in which one signal ends an agent's stream and the server, with no drain warning.
- [x] **P10.5** 🔴 Server components:
  - `AgentRegistry`
  - `Scheduler`: in v1, the first agent with free capacity, labels optional
  - `Reconciler`: compares desired with reported state and sends `Assign`, `Unassign` or `UpdateSpec`
  - stale agent detection, which marks its bots `Unknown`

  > Note (P10.5, from Phase 4) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)):
  > - **Sticky state.** The server stores `bots.last_state` from `BotStatus` and sends a Paused or Failed state with `AssignBot`/`ReconcileFull`, so the agent's `Fleet::apply(spec, restore)` starts the bot there instead of kicking a human (§6 row 3).
  > - **Moving a bot** to another agent waits for the old agent's `Removed` event (or a timeout), so two agents never run the same account at once.

  > Note (P10.5, from P4.7, the user's decision) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): **No `Stopped` before `Removed` for a sticky bot.** A removed bot publishes `StateChanged(Stopped)` only if its actor stopped it through `transition()`. A Paused, Failed or crash-looped bot gets only `Removed`, so `bots.last_state` keeps the Paused or Failed the server sends with `AssignBot` on the next agent. Otherwise the new agent would connect and kick the human who's playing (§6 row 3), or retry a failed account.
  > Note (P10.5, from Phase 9, group D, the user's decision) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **no bot runs on an offline account while `[accounts] allow_offline` is off.** The offline accounts kept from before stay untouched (serve warns at startup), and the scheduler refuses to assign their bots until the switch is on again.

  > Note (P10.5, from Phase 10's planning) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)):
  > - **The tables** *(the user's choice, U1)*: P11.1's `bots` and `modes` (with the built-in seeds) arrive in this group, their columns designed with the scheduler and the reconciler (`agent_id`, `last_state`, `last_state_at`, and how `Unknown` is shown).
  > - **Every account gets its bot** *(the user's note at the plan review)*. Bots are 1:1 with accounts, but nothing creates a bot row yet, and there's no `POST /bots`. D decides how: created together with the account, on each path that creates one (a Microsoft link's commit, an offline account, and the review if that matters), plus a backfill for the accounts that already exist. The DoD check then needs only the moved P11.2 routes: `PATCH` sets the server address on the existing row, and `start` starts it. Deleting an account deletes its bot (P9.6's note), and the reconciler unassigns it.
  > - **Capacity** (S8): the hello's `AgentCapacity`, with no labels in v1. Free capacity also counts the bots unassigned whose `Removed` hasn't arrived, since the runtime counts a bot until then. An agent that comes back with a lower capacity has bots moved off it.
  > - **What an agent reports** is untrusted (S6): an event or a status for a bot that isn't assigned to that agent is refused. A session request names no account; the server takes it from its own assignment (S3).

  > Note (P10.5, Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the `AgentRegistry` as built** (`grpc::registry`); the task is ticked in D with the rest.
  > - **One stream per agent** (S26): a session claims its agent's slot after its hello's checks and before the hello's transaction; a second claim while the slot is taken gets `Reject(AlreadyConnected)`, the live stream untouched, and isn't audited (R12). The slot frees when the session drops its lease, before its last answer and before its end is recorded, so an agent that saw its stream end can connect again at once. A message on the live stream after a refused claim is an identity conflict: an `error` line and `security.agent_identity_conflict` (the refused connection's IP and fingerprint, `live_ip`, `connected_at`, `refused_at`), at most once an hour per agent.
  > - **Ending** (S27, R13): `end` throws a live stream's switch, so nothing queued goes out any more and `send` answers `NotConnected`, and its session sends `Reject(AgentDisabled | CertificateRevoked | CertificateExpired)` at once; or it makes a claim fail its activation, so a stream whose permission ended during its hello is never welcomed. A sweep every 5 s (`SWEEP_INTERVAL`) reads every admissible certificate once and ends the streams whose certificate stopped admitting them; it's skipped with no stream, a failed read keeps every stream, one stream whose rows can't be read is a `warn` line while the others are judged, a disabled agent's stream ends `AgentDisabled` whatever its certificates' state, and it never ends a newer stream of the same agent. The registry implements `ports::live_agents::LiveAgents` for the app.
  > - **Sending** (S25): `send` never waits; over `MAX_SERVER_MESSAGE_BYTES` (`fleet_proto::control::server_message_len`) is `TooLarge` and the stream stays; a full queue (2048) is `QueueFull` and ends the stream `RESOURCE_EXHAUSTED`.
  > - **Reports** (S28): `admit` passes every converted message after `Welcome`. A status or an event for a bot not `Assigned` is dropped and counted (housekeeping logs one line per agent, R21); a `Releasing` bot passes only its `Removed`, which ends the hold; a session request gets `SessionDenied { retryable: true }`. The holds (`hold`, `release`) stay empty until this task's reconciler fills them; a hello's running bots aren't held either, and are the reconciler's to read.
  > - **For D:** fill the holds from the server's own assignments, never from what an agent says; treat `QueueFull` and `NotConnected` as "resync on the next connect"; an agent goes stale when its stream ends, and its bots then follow the stale path (a disabled agent never sends their `Removed`); compute `Unknown` when reading, from `last_seen_at` and the registry.
  > - **For E:** the lifecycle messages go through `send`; any new message from an agent goes through `admit`.
  > - **For H:** a restart while the old connection is still open: the new process is refused `AlreadyConnected` until the old stream ends, then welcomed, with no conflict recorded.

  > Note (P10.5, Phase 10, group D) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the scheduler, the reconciler and stale agents as built.** Decided by the review agents and approved at the plan review: S32–S45; reconciliations R24–R30.
  > - **No `Scheduler` type:** the choice is fleet-core's pure `placement::plan` (S42): live agents in enrollment order (the server-minted agent ID), first fit, never rebalanced; which bots run is `placement::placeable` (S33), offline accounts' bots only while `[accounts] allow_offline` is on.
  > - **The reconciler** (`grpc::reconciler`, S37) is one task on serve's `JoinSet`: a pass at startup, every 30 s, on a hint (the registry's welcomes, `Settled` reports and ends; the accounts' and users' services after their commits; a bounded queue, one `warn` when full) and at the nearest bound. A pass commits each change of a bot's agent in its own audited transaction (`bot.assign`, `bot.unassign`, `bot.detach`, `bot.stray`, S36) before any message: a welcome gets one `ReconcileFull`, always, and other streams one message per bot.
  > - **Stale agents** (S32, S41): nothing is written when an agent goes quiet; its bots keep their agent and read `Unknown` (`fleet_core::bot::shown`, computed when read by `app::bots::BotService`). No time ever moves a bot: it leaves its agent only by that agent's `Removed` of a bot the server released (a `Removed` of one it never released frees nothing, S44), a hello of its that doesn't list it, or `afkfleet-server agent release --id` of a disabled agent. The "(or a timeout)" of the Phase 4 note above is dropped.
  > - **Two agents never run one account:** a bot is placed only when no live stream holds it or a bot of the same Minecraft account and no release record names either (S39: a deleted account's bot an agent may still run; S43: a hello's bot it wasn't given, `bot.stray`). Right after a start, nothing new is placed until every enabled agent is back or 90 s pass (S45).
  > - **Capacity and moves** (S42): free capacity counts Assigned, Releasing and open records; an agent back with less keeps the bots it runs first, then those not paused or failed, then the lowest IDs, and the rest move after their resting report; a bot that stops qualifying is sent desired Stopped and unassigned after it rests or 60 s (S33).
  > - **Sticky state** (S40): the last report a stream sent of each bot is kept in memory and written only in transactions D commits anyway (the hello, the stream's end, `bot.unassign`, `bot.detach`), never backwards; `AssignBot` and `ReconcileFull` carry the newer of memory and the row.
  > - **Deviations:** the registry sends the hints (the session's `control_service` would need an eighth parameter, which clippy refuses); `ReconcileFull` may carry a spec with desired Stopped (a bot being stopped before it's unassigned).
  > - **PR #52's review** (ADR-0019): a `Removed` note carries its stream's generation and is dropped once a newer stream of its agent is welcomed; a hello's strays are fenced in the hello's own transaction; serve watches the reconciler and stops (exit 4) if it ends unasked; `account.delete` records the bot's agent; a bot that doesn't read back is left alone, and every other bot reconciled; an assignment its agent never got is sent again.
- [x] **P10.6** 🔴 Session grants: `SessionRequest{bot_id}` returns `SessionGrant`, but only if the bot is assigned to *this* agent; otherwise `SessionDenied`. On the agent this backs `SessionCredentialProvider`.

  > Note (P10.6, from Phase 4) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): The port is `fleet_core::mc`'s credentials provider, and the runtime doesn't check `expires_at`. The managed provider never hands out a token that expires within the connect timeout.

  > Note (P10.6, from Phase 9, group C, the user's decisions) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **serve builds `SessionTokenService` here**, over D's `ReqwestMicrosoftAuth`, with a child of serve's shutdown token, and calls its `shutdown()` within serve's shutdown deadline (it waits for refreshes in flight).
  > - `SessionRequest.fresh` maps to `session(account, fresh)`; `SessionTokenError::retryable()` maps to `CredentialError::retryable`.
  > - A cached session is handed out only while more than 10 minutes are left, so it never expires within the agent's connect timeout.
  > - An offline account is `SessionTokenError::Offline`: the grant carries its name instead.
  >
  > Note (P10.6, Phase 10, group F, the user's choice, U14) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **one exception to the mapping.** The session-token service's flight reads the owner again once it holds the account's lock, so an owner disabled while a grant waits for an earlier refresh comes back as `OwnerDisabled`, which the grants answer retryable, as they do the same owner found by their own check. `SessionTokenError::retryable()` stays as it is, and every other error keeps the mapping. An offline account never reaches the service: the grants build its grant themselves.

  > Note (P10.6, from Phase 9, group E) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): an account linked again (P9.6) during a refresh answers `ChangedMeanwhile`, which is retryable, and the retry uses the new token; a test here should show it end to end.

  > Note (P10.6, from Phase 10, group A) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)):
  > - **Correlation** (S3): a grant and a denial repeat the request's `SessionRequestId`, the agent's own number from 1, and its `bot_id`. The agent's managed provider drops an answer to a request it no longer waits for (the actor dropped it, or its 30 s passed), so a late grant can't answer a later `fresh` request.
  > - **The grant** is `fleet_proto::control::SessionGrant` over core's `SessionCredentials`; the token reaches the wire only inside `OnlineSession`, whose `Debug` never prints it (S7). This group's `log_capture` test covers a grant and a denial at every level of every target (tonic, h2, hyper and rustls included).

  > Note (P10.6, from Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the registry's `DenySession` arm is F's to replace** (S28). A grant goes only after `Welcome`, to the identity the certificate gave, and only on the stream that asked: a request's answer must not reach a later stream of the same agent, whose `SessionRequestId`s start at 1 again, so F answers on the asking session's own queue (its lease's generation) rather than `send` by agent. The token is checked once it's in hand, and queued grants die with their stream. A compromised agent could send `fresh: true` requests in a loop to force Microsoft refreshes, and Microsoft's 429 pauses every refresh (ADR-0018): F bounds them per bot and answers `SessionDenied { retryable: true }` when its own queue is full.
  > Note (P10.6, from Phase 10, group D) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **a grant goes only to a bot the asking stream holds Assigned** (`LiveAgents::held`), never one held Releasing or listed in an unreconciled hello; offline bots get their (offline) session from the server too.

  > Note (P10.6, Phase 10, group F) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **as built.** Decided by the review agents and approved at the plan review: S57–S64; reconciliations R39–R46; the user's choice U14, and the user's change at the plan review that each grant is logged at `info`.
  > - **The registry filters first** (S57): a `SessionRequest` passes (`Verdict::Session`) only while the stream is reconciled, not ended and holds the bot Assigned; any other is denied at once, retryable, and counted in housekeeping's per-agent line (`RefusedReports::sessions_denied`). A fresh one then spends `[rate_limits] fresh_sessions_per_bot_per_hour` (1–60, 6; S58), and past it is denied retryable without reaching the token service.
  > - **The stream's grant queue** (`grpc/control/grants.rs`, S57, R40): one waiting request per bot, a newer one replacing it (and aborting its bot's running one, whose token flight goes on, on the service's own tracker); at most `GRANT_WORKERS` (4) at once, on the session's `JoinSet`, which its loop polls beside the reads; at most 1000 held (`GrantBounds`), one more bot's request denied retryable. The stream's end drops what waits and aborts what runs.
  > - **The grant** (`app::agents::SessionGrantService` behind `ports::session_grants::SessionGrants`, S59): the stream's own certificate by its fingerprint (fleet-core's `admit`), the bot's row naming the agent, fleet-core's `placeable`; the account from that row (S3); the session from `app::session_tokens::MinecraftSessions` (R46; `SessionTokenService` implements it) with the request's `fresh`, or an offline account's name; then the same check inside a write transaction that writes nothing, the account unchanged, and the answer queued by `LiveAgents::answer_session` (the asking stream's generation, not ended, holding the bot Assigned, in one critical section) before the transaction is dropped. `control_service` takes `ControlPorts { connections, grants, clock }`.
  > - **Denials** (S60, U14): every one the grants make themselves is retryable; the service's errors keep `retryable()` except `OwnerDisabled`. The wire is unchanged. Each denial is one line at its level with a fixed `reason`; each grant one `info` line (request, agent, bot, number, kind, fresh, expiry); never a token, a name or a UUID. No audit entry (S61).
  > - **serve** (S64): `app_with` creates the shutdown token (`App::shutdown`); the service is built over the one Microsoft adapter with a child of it, while `[grpc]` is on, and stopped by `stop_session_tokens` within the drain, refreshes still running then left with one `warn` (`SESSION_TOKENS_LEFT`).
  > - **The agent** (S62, S63): `fleet_agent::managed::credentials::ManagedCredentials` over a `SessionLink` (G implements it, calls `answer`, `link_ended`, `forget` and `forget_all`): numbers from 1, answers matched by number and bot, late ones dropped, a link without a stream or a stream's end failing at once, a token within the connect timeout of its expiry refused, kept grants used only without a stream. Not wired into `run` yet: G's.
  > - **Deviations:** the service is `app/agents/session_grants.rs` (the plan's `grants.rs`; `app::grants` is the accounts' sharing) and the token seam `MinecraftSessions` lives beside `SessionTokenService` in `app/session_tokens` rather than in `ports/` (its error is that service's); the grant is converted for the wire inside `answer_session`, before the registry's lock, while the fence is open (CPU only); the service-level checks live in `tests/grpc_control/grant_checks.rs` (over the loopback harness's scripted token service) instead of a binary of their own; no NoServer case among the checks, since no route clears a server.
- [x] **P10.7** 🔴 Agent client:
  - reconnects with backon
  - keeps bots running during outages
  - sends its full state on reconnect
  - forwards events through a bounded buffer that drops the oldest chat when full and **never blocks the bots**

  > Note (P10.7, from Phase 4, group B) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): The agent maps the runtime's `FleetEvent`s to `BotEvent`. `ChatSent` and `ChatFailed` carry the runtime's `ChatTicket`, which the agent maps back to `SendChat`'s request id. `ModeChatSent{message}` is outgoing mode chat, with no request id.

  > Note (P10.7, from P4.7) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): The agent maps `FleetEventKind::Removed` too, since P10.5's move to another agent waits for it.

  > Note (P10.7, from the PR #15 review): **An event can come before its ticket.** `ChatQueue::send` returns the ticket while the queue's delivery task may already be publishing `ChatSent` or `ChatFailed` for it, so on the agent the event can arrive before the `send_chat` reply. The agent must handle a ticket it hasn't mapped to a request id yet, for example by handling replies and events in one task, or by holding unknown tickets briefly.

  > Note (P10.7, from Phase 4, group C) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): `FleetEventKind::Alert(BotNotification)` comes right after the `StateChanged` to Paused or Failed it belongs to: an alert for a human. The agent maps it too.

  > Note (P10.7, from Phase 10, group A) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)):
  > - **The mapping:** a `FleetEvent` becomes `fleet_proto::control::BotEvent`, a `StateChanged` an `AgentMessage::Status`. `ChatSent` and `ChatFailed` carry the `ChatRequestId` the server's `SendChat` named (S3), and `ChatFailedReason` covers both the runtime's `ChatFailure` and the refusals of `send_chat` (`NotOnline`, `QueueFull`, `RateLimited`, and the fleet's `UnknownBot`, `Busy`/`TimedOut` as `AgentBusy`, `ShuttingDown`), mapped with exhaustive matches.
  > - **Capacity** (S8): the hello's capacity is `[runtime] max_bots`, and a test keeps the agent config's range equal to `AgentCapacity::MIN` and `MAX`.
  > - **A dead server** *(the user's note at the plan review)*: the agent notices a stream that hangs without an error (P10.4's note) and reconnects and resyncs; the loopback suite tests it.

  > Note (P10.7, from Phase 10, group B) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the identity `enroll` leaves:** `cert_file` holds exactly the PEM the agent checked (one certificate the pinned CA signed for client authentication), and `key_file` the PKCS#8 PEM of its Ed25519 key, both owner-only on Unix. They go straight into tonic's `Identity::from_pem`; the client checks at start that they match each other and the pinned CA, as `enroll` did. `run`'s refusal of `[control_plane]` ("managed mode runs from P10.7") goes away here.

  > Note (P10.7, from Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)):
  > - **The channel** (S23, S25): HTTP/2 keepalive every 10 s with a 20 s timeout, while idle too; heartbeats on `Welcome`'s interval; decoding up to `MAX_SERVER_MESSAGE_BYTES`, encoding up to `MAX_AGENT_MESSAGE_BYTES`; `Welcome` or `Reject` awaited for `WELCOME_TIMEOUT` (30 s) from the call's start. `Welcome.agent_id` is the ID the agent logs.
  > - **Three kinds of end** (S24): *stop* on `UNAUTHENTICATED`, `Reject(NameMismatch)` and the permission Rejects (`AgentDisabled`, `CertificateRevoked`, `CertificateExpired`), with an exit code of its own that `run`'s help lists, stopping every bot on `AgentDisabled` so two agents never run one account; *retry slowly* on `Reject(IncompatibleProtocol)` and `Reject(InvalidHello)`; *retry with backoff* on everything else, `AlreadyConnected` and `RESOURCE_EXHAUSTED`'s `retry-after` included.
  > - **An idle channel's `UNAVAILABLE`** is routine: the port cuts a connection 80 s after its last stream ended (S22).
  > - **`Reject(AlreadyConnected)`** means its own old stream still lives, after a restart for instance: retry with backoff; the old one ends within the heartbeat timeout (S26). The reports the server passes (statuses and events of assigned bots) are the ingest's to read (S28).
  > Note (P10.7, from Phase 10, group D) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **what the reconciler counts on** (S33, S42, S44, S45): the agent removes a bot only on `Unassign` or its omission from a `ReconcileFull`; a spec with desired Stopped stops the bot and keeps it; a bot starts in its `restore`; a `Busy` apply is retried after the bot's `Removed`, and a `Removed` is never dropped; the hello's snapshot is sent only after the previous stream's instructions are applied; an `Unassign` of a bot it doesn't run is answered with `Removed`; the reconnect backoff stays within `fleet_proto::control::RECONNECT_DELAY_MAX` (30 s).

  > Note (P10.7, from Phase 10, group E) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)):
  > - **The SSRF check** (S46, security): right before it dials the address of a server-sent spec, the agent classifies the socket fleet-mc resolved (`ResolvedAddr.socket`, SRV included), the one azalea dials. *Never:* unspecified, link-local (169.254.169.254 included), multicast, broadcast, reserved, benchmarking and documentation addresses, also inside IPv4-mapped, NAT64, 6to4 and IPv4-compatible forms. *Private:* loopback, RFC 1918, carrier-grade NAT and unique local, allowed only inside `[control_plane] private_targets`, a CIDR list, empty by default (`IpNet` moves to fleet-core for it). *Public:* allowed. A refusal is the new `ConnectFailure::Blocked`, transient and never dialled. Standalone mode is never checked. fleet-mc changes, so `just test-slow` runs.
  > - **The dev and compose configs** *(the user's note at the plan review)*: `private_targets` is empty by default, so a managed agent refuses loopback and LAN servers. `deploy/dev/agent.managed.toml` (loopback) and the compose agent, whose `minecraft:25565` resolves to a Docker bridge address in `172.16.0.0/12`, list the networks they need, each with a comment saying why, so the hands-on checks don't fail on the SSRF check.
  > - **The lifecycle message** (S49): match `ServerMessage::Lifecycle` exhaustively and apply it in stream order. For a bot the fleet runs, call exactly one `Fleet::{restart, reset, resume}`, without filtering by state; drop one for any other bot at `debug`. A command never applies, removes or respecs a bot, and is never answered: its effect comes back as the bot's `Status`. Drop the fleet's refusals; commands die with their stream.

  > Note (P10.7, from Phase 10, group F) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the managed credentials** (S62, S63, R44): build one `fleet_agent::managed::credentials::ManagedCredentials` per process, with `RuntimeClock::new(anchor)`, the runtime's `connect_timeout` and its `session_request_timeout`, as the fleet's `FleetParts.credentials`. Implement `SessionLink::send` on a lane that refuses when full (never the event buffer that drops chat), answering `NoStream` whenever no welcomed stream exists, and raise `LinkGeneration` for each welcomed stream. The stream's reader hands every `SessionGrant` and `SessionDenied` to `answer`, and every stream end calls `link_ended(generation)`. Call `forget(bot)` on an `Unassign`, a bot left out of a `ReconcileFull`, its `Removed` and a spec with desired Stopped, and `forget_all()` on `UNAUTHENTICATED` or a permission `Reject`, before stopping the bots. A replaced request gets no answer and times out retryable; every retry is a new number.

  > Note (P10.7, Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **as built** (S65–S69, R47–R58; the user's choices U15, U18–U20).
  > - **`run` in managed mode:** main.rs loads and checks the identity (`managed::identity`, S68) after logging and the TLS provider, before the recorder, the runtime and the signals: any problem is exit 1, its text naming the files and the fix; one `info` line shows both fingerprints. `StartupError::ManagedMode` is gone. The run builds one `RuntimeClock`, the `Outbox`, `ManagedCredentials<Outbox>` as the fleet's credentials, subscribes to the fleet's events before its supervisor runs, and starts the conductor and the link beside the supervisor, the chat logger and the heartbeat (which starts at once).
  > - **The conductor** (`managed::conductor`, S65): one task owns every Fleet call; the reader's six instructions go through its FIFO (`INBOUND_QUEUE`, 2048), the session answers straight to the credentials. The hello barrier (R48), the instructions' table, the exhaustive answers (`mapping`), the ticket map (`ChatRequests`, `max_bots × chat_queue`), the retry tick (`RETRY_TICK`, 1 s) and the lag resync as S65 and R47 say.
  > - **The outbox** (`managed::outbound`, S66): one `std` mutex; the `Connect` request body `Outbound` is a `tokio_stream::Stream` (U18) with the idle heartbeat (R50); the classes, their caps (`Caps::of`; chat 64 per bot and 1024 in all), the send order, `prepare`, `open`, `welcomed`, `reconciled` (R49) and `ended` (R51). `Outbox` is the credentials' `SessionLink`.
  > - **The link** (`managed::link`, S67): `endpoint` with the keepalive, `Link::run` with the attempt (R52's check, the dial, the barrier, the hello, `Connect`, `Welcome`), the reader, `ends` (stop, slow, backoff; `retry-after`) and `backoff` (fleet-core's `RetryPolicy::seeded_delay`, a splitmix64 seed sequence from the OS's seed). `LinkTimings` carries the welcome timeout and the keepalive: the protocol's constants in the agent, shorter in `tests/control`. A refusal forgets every kept grant before the stream's end is noted; the run stops every bot and exits 5 (`Exit::Refused`); an unasked end of the conductor or the link is exit 4.
  > - **S46's check** (S69): fleet-core's `net` (`IpNet` moved there; `classify`, `TargetPolicy`, `PrivateTargets`), fleet-mc's `AzaleaConnector::with_targets` (strictest by default) and the check in `Driver::drive` right after `resolve`; `ConnectFailure::Blocked` (wire 6, `"blocked"`). `[control_plane] private_targets` is read into `AgentMode::target_policy`; main.rs passes it to the connector. One check is enough today: azalea 0.16 ignores `ClientboundTransfer` (the configuration state's handler only logs it at `debug`, the game state's has an empty body) and never downloads resource packs (both states only emit a `ResourcePackEvent`, whose one consumer, `accept_resource_packs`, answers `Accepted` and `SuccessfullyLoaded` without fetching), checked on docs.rs's source of azalea-client 0.16.0 and azalea 0.16.0.
  > - **U20's gate** `agent-control-plane` measured 93.8 % of lines over `managed/` and `enroll/` (Windows, `--no-rustc-wrapper`, nothing excluded), so it's required at 90 % (`scripts/coverage-gates.mjs`, its pin, §8, CLAUDE.md).
  > - **The dev tooling** (S71): `just dev-managed-agent` and `just dev-bots`, the console in `crates/fleet-client/examples/dev-bots/` (tested from fleet-client's `client` binary through `#[path]`). `deploy/dev/agent.managed.toml` lists exactly `127.0.0.1/32` (R58); the compose agent stays standalone with U15's comment, which resolves E's note on its Docker network.
  > - **Deviations:**
  >   - *No backon sleeper* (S67's option C): the reconnect waits come from fleet-core's `RetryPolicy`, backon inside, and the link sleeps with tokio; Phase 10's dependency note expected backon's tokio sleeper in G.
  >   - *The agent's property tests are deterministic sweeps* (seeds, orders and subsets), since fleet-agent has no `proptest` dev-dependency, and adding one wasn't in the plan.
  >   - *`LinkTimings`*, a small seam the plan didn't name, so `tests/control` sees a missing welcome and a silent connection within seconds; the agent always runs the protocol's constants.
  >   - *`certificate::validity`*, enroll's chain check without the key, for R52's check before each attempt; `enroll::chain` stayed private (nothing outside needed it).
  >   - *One planned mutation doesn't apply:* `prepare` after the snapshot (the conductor pushes no event while it awaits the snapshot, so both orders are one). `forget_all` after `link_ended`, listed here as untestable at first, became testable in the review below.
  > - **Tests,** red first against compiling stubs where the code came after them, the rest through mutations, each reverted and checked by checksum (the PR lists them): fleet-core's `net` and `seeded_delay`; fleet-proto's wire value and the timing pin; fleet-mc's driver unit, `connector.rs` and the slow offline scenario's target step; fleet-agent's config, identity, outbox, conductor, link (`ends`, `backoff`), run (`watch`, `finish`), the CLI, `tests/managed` (paused time, a real fleet on the fake connector) and the new real-time `tests/control` (U19: the managed run against a fake `AgentControl` server over mTLS on loopback, with a stalling proxy); fleet-client's console.

  > Note (P10.7, the review of PR #56) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md), "The review of PR #56"):
  > - **Fixed:** an overflowing outbox no longer turns the server's refusal into a reconnect (a stop stays a stop, so the grants are wiped and every bot stops); a busy removal no longer stops a bot the next hello's reconcile gives again (the barrier, a reconcile and an assignment clear it; the tick removes before it applies); the run's end closes the stream the aborted link left open (`Outbox::close`), so no heartbeat follows a signal.
  > - **Observable now:** `forget_all` and `link_ended` each log at `debug`, so the refusal tests prove the wipe comes first; a certificate expired by the agent's own clock reads as that, never as the server's refusal (the link's line, the shutdown's, exit 5's help); `private_targets` refuses a metadata or other never-dialled entry as `NeverDialled`; `link::endpoint` refuses a URL that isn't `https`.
  > - **Tests added:** the end kinds end to end, R52 on the run's clock, the conductor at capacity, a lag's synthesized `Removed`, a timed-out apply ending the batch, and the console's paging, lost session, arguments and tokens. `tests/control` runs the protocol's timings but in the two tests that measure them.

- [x] **P10.8** 🔴 Server ingest:
  - status and chat are written in batches, in transactions roughly every 500 ms
  - **no per-tick writes**
  - events are published on the internal bus (`broadcast`)

  > Note (P10.8, from Phase 10's planning) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)):
  > - **P11.1's `chat_messages`** arrives in this group, its first writer *(the user's choice, U1)*, with a `truncated` column: the agent's flag OR the server's own cut (S6). The rebuilt text is what's stored.
  > - **The batched writes** need a new `commit_unaudited` exemption (`EventBatch`, which ADR-0015 anticipated): the user's approval, and its own ADR note.
  > - **A heartbeat** carries no time: the server stamps `last_seen_at` with its own `Clock` (S5).

  > Note (P10.8, from Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **an agent's presence isn't the ingest's** (S29): the session writes `agents.capacity`, `version` and `last_seen_at` at the hello and `last_seen_at` at the stream's end, and nothing per message; `EventBatch` never carries these columns.

  > Note (P10.8, from Phase 10, group D) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **nor is a bot's `last_state`** (S40): it's written only in the hello's, the stream end's and the reconciler's transactions. *Amended in group G (U16):* the batch also writes a bot's sticky crossings; see the next note.

  > Note (P10.8, Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **as built** (S70, U16, U17).
  > - **What "status" the batch writes** *(the user's choice, U16; S40 revised)*: only sticky crossings. A bot's `last_state`, `last_state_reason` and `last_state_at` are written in the batch when its reported state entered or left Paused or Failed, or changed its reason; every other status stays in the registry's memory, as S40 has it, and S40's three transactions stay. Group D's note above ("nor is a bot's `last_state`") is amended to this.
  > - **The tap** (S28): the registry's `Verdict::Held` covers a status of a bot a reconciled, unended stream holds assigned, or an event of such a bot other than `Removed`. The session hands a held report to `ports::ingest::Ingest::take`, which never waits; a refusal is counted per agent, and housekeeping's line gains `chat_dropped`. `ControlPorts` gains `ingest`.
  > - **`app::agents::ingest`:** `ingest(store, marks, bus, timings, bounds)` gives the session's `IngestService` and serve's `IngestWriter`. Statuses, deaths, alerts and chat answers are published on the bus at once; chat rows wait in a bounded queue (`INGEST_QUEUE` 4096, `STREAM_ROWS` 1024 per stream through a semaphore share). The writer ticks every `INGEST_INTERVAL` (500 ms): a bounded wait for the write connection (`INGEST_WAIT`, 250 ms), at most `BATCH_ROWS` (1000) guarded inserts, U16's fenced updates, `commit_unaudited(EventBatch)`, then each stored row published with its ID. An idle tick writes nothing. A busy store keeps the batch, first at the next tick; any other failure drops it, with one `error` per run of failures and a `warn` with the rows lost on recovery.
  > - **The bus** (`app::events`): `EventBus`, a `broadcast` of `Arc<LiveEvent>` (`EVENT_BUS_CAPACITY` 1024), created in `app_with` (`App.events`) for P11.5.
  > - **serve:** the ingest is built in `control_plane_services` and carried in `GrpcRoutes` and `GrpcPort`. At the stop, `end_streams_then_ingest` ends the streams first and only then stops the writer, whose final flush (each batch's wait bounded, a busy one dropped with one `warn`) runs before the database closes. The writer is watched like the reconciler: `spawn_reconciler` became `spawn_watched` with `task_watch` and `Watched::{Reconciler, Ingest}`; an unasked end is exit 4 (`INGEST_ENDED`).
  > - **`UnauditedWrite::EventBatch`** *(the user's approval, U17)*: its doc in `ports/store.rs` gives the scope; CLAUDE.md's exemption list names it.
  > - **Deviations:**
  >   - *R54's baseline:* a bot is marked when an admitted status's sticky part differs from its previous admitted report's, not from a copy of the row the reconciler read. At the settle, the previous one is the hello's report, which the hello's transaction stored (S40); a resume's or reset's clear takes the report out in its own transaction (S48), so the next one is compared with nothing, as the cleared row is; a crossing is written or marked again. So it is the row's stored part in every path, without a read. The marks are `ports::ingest::StickyMarks` (`any`, `take`, `remark`), which `AgentRegistry` implements, rather than `LiveAgents::take_sticky`.
  >   - *The in-memory test* that a full ingest never holds up a stream's reads is the loopback test `chat_past_the_streams_share_is_dropped_counted_and_the_stream_goes_on`: `Ingest::take` is synchronous and never waits, and the barrier after the dropped row is still answered.
  >   - *The final flush's order* is pinned on paused time through `end_streams_then_ingest`, red first against a stub that stopped the writer first, since a real-socket test can't time a report between the signal and the streams' end.
  > - **Tests,** red first against compiling stubs where a stub fails; the rest through mutations, each reverted and checked by checksum (the PR lists them): the registry's verdicts and marks (`grpc/registry.rs`), `tests/ingest_service.rs`, `tests/event_batch_scope.rs` (U17's scope pin: every table and column around a batch; the writer also updating `agent_id`, `server_address`, `mode_id` or `desired_state` turned it red, one run each), `tests/grpc_control/ingest.rs` over loopback (U19), the log check, serve's watch, stop order and flush.
  > Note (P10.8, the review of PR #56) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md), "The review of PR #56"): the writer commits only what it wrote: a tick whose marks the fence refused, with no chat, is `Idle`, and a batch that stored nothing rolls back (U17). The stop's last batch, busy with no chat waiting, no longer says chat was lost. Two tests pin U16's order (the marks taken only inside the open write transaction; a resume's clear landing while a tick waits is never undone, with the real registry), and the scope pin compares all three `last_state*` columns of every bot; its table list stays fixed, as the test's header says why.

  > Note (P10.8, from Phase 10, group H) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md), S74): **4096 rows in all.** One row budget covers the rows queued, kept by a busy database or being written, so a kept or in-flight batch no longer lets the queue refill past S70's 4096, and the stop's flush commits at most U17's 6 batches; a `const` assertion ties `INGEST_QUEUE` to that 6. Before, up to 5096 rows could be held.

- [x] **P10.9** 🔴 Loopback tests with a test PKI. Each of these is rejected:
  - a revoked certificate
  - a certificate from the wrong CA
  - an agent that never enrolled

  And these must hold:
  - **Connection killed mid-stream:** the state reconciles.
  - **Agent restart:** no duplicate bots.
  - **Server restart:** agents resync.

  > Note (P10.9, from Phase 10's planning) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **an expired certificate** joins the rejections (S1), and **a connection that goes silent without closing** joins the cases that must hold *(the user's note at the plan review, P10.4's note)*. The rejections are tested in group C, which builds `Connect`; the rest of the suite in H.

  > Note (P10.9, Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the four rejections are done,** in `tests/grpc_control/identity.rs`: a revoked certificate gets the answer of one no agent holds (`a_revoked_certificate_is_refused_like_an_unknown_one`); a certificate from another CA and an expired one fail TLS's handshake, before any call (`a_client_certificate_from_another_ca_fails_the_handshake`, `an_expired_certificate_fails_the_handshake`); an agent that never enrolled is refused (`an_agent_that_never_enrolled_is_refused`). The server's side of a connection that goes silent is in `liveness.rs`. For H: the silent connection runs end to end through the suite's stalling proxy (`tests/grpc_control/proxy.rs`): the server frees the agent within `heartbeat_timeout`, and the agent reconnects within 30 s and resyncs.
  > Note (P10.9, from Phase 10, group D) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the reconciler's loopback tests** are in `tests/grpc_control/reconcile.rs`; H adds a server restart with agents coming back in either order, and checks the threat model's B3 rows that cite D's tests.
  > Note (P10.9, from Phase 10, group E) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the bots' loopback tests** are in `tests/grpc_control/reconcile.rs` too: a bot started through its service is placed and, stopped, taken off its agent; a paused bot's stop frees its slot and keeps it paused; a command reaches the stream that holds its bot once and never a later one; a start of a fenced bot places it nowhere. H checks the threat model's B1, B3, B4 and B7 rows that cite E's tests. **H's check against a local Minecraft container** lists that container's network in the agent's `[control_plane] private_targets`, with a comment saying why *(the user's note at the plan review)*, or the SSRF check refuses the connect (S46).
  > Note (P10.9, from Phase 10, group F) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the grants' loopback tests** are in `tests/grpc_control/grants.rs` and `grant_checks.rs`, the log test in `logs.rs`, the real service's in `tests/accounts/session_tokens/grants.rs`, serve's in `tests/serve/control_plane.rs` and `shutdown.rs`. H adds, with G's managed agent: a server down while a bot is kicked (it rejoins with its kept grant, and a fresh request waits), and a server restart whose agents' bots get their sessions once reconciled; and checks the threat model's B3 rows that cite F's tests.

  > Note (P10.9, from Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **for H** (U19). G tests each side against a scripted peer over real mTLS on loopback: fleet-server's `tests/grpc_control/ingest.rs`, fleet-agent's `tests/control/`. A real agent against a real server stays H's question to the user (fleet-agent dev-depending on fleet-server, or a new test crate), for H's cases, the DoD check and P11.7. The threat model gained G's agent and ingest rows; H reviews them with its own. G's residuals are in ADR-0019 and the threat model: tonic's buffers hold the identity's key PEM unwiped; a notice handed to tonic is lost when its connection breaks (no acknowledgement); a power loss can undo the last batch (U16).

  > Note (P10.9, Phase 10, group H) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **as built** (the user's choices U19, U21 and U22; S72–S74, decided by the review agents and approved at the plan review).
  > - **Where:** `crates/fleet-integration`, a test-only crate (U19), one test binary, `tests/integration/`.
  >   - `harness/`: serve's wiring over a `ManualClock` and `SeededRandom` (`app_with`, then serve's `open` and `grpc_port`, made public, then `serve`); agents enrolled for real and run in-process over a `Ledger` connector, one fake per run, which fails a case that ever runs one account twice; one stalling proxy per agent; the Owner through fleet-client with 2FA; one log that wakes the waits.
  >   - `loopback/`: the cases.
  >   - `managed_check/`: Phase 10's DoD check (below).
  > - **The cases**, in nextest's default profile, each shown red by a mutation:
  >   - `killed_mid_stream::a_connection_cut_mid_stream_reconciles_what_changed_meanwhile`: the kept bot runs on untouched, the bot stopped meanwhile stops and is unassigned after its `Removed`, the one started meanwhile is placed.
  >   - `agent_restart::a_restarted_agent_runs_its_bots_once`: no move, no other agent.
  >   - `agent_restart::a_restart_while_the_old_connection_lives_is_refused_until_it_ends_and_records_no_conflict`: C's note for H.
  >   - `server_restart::agents_resync_after_a_server_restart_in_either_order`: D's note; S45's gate holds a new bot until both are back.
  >   - `silent_connection::a_silent_connection_frees_the_agent_and_it_reconnects_and_resyncs`: C's note, the one case on short timings.
  >   - `outage::a_bot_kicked_while_the_server_is_down_rejoins_with_its_kept_grant_and_a_fresh_request_waits` and `outage::bots_get_their_sessions_once_reconciled_after_a_server_restart`: F's note.
  > - **Deviations:**
  >   - The limiters measure real intervals.
  >   - A wait wakes on every log line and re-checks in-process state at most every 100 ms, always within `PATIENCE`, never a fixed delay before an assertion: the user's choice at PR #58's review (U23), now in CLAUDE.md's time rule.
  >   - Case 2b's old stream ends by the server's heartbeat timeout, not by a cut.
  >   - Case 5 records how the code behaves: after a refused login, the request after the backoff isn't fresh; with no stream nothing is kept, so it waits for the server. Accepted for now (U24); Phase 11's first group changes it (P11's note).
  > - **The review of PR #58** (the user's choices U23 and U24):
  >   - *The managed check:* the account's name and UUID pass only in the two received-chat lines at `debug` (fleet-mc's bridge and the agent's event log), and every token of the server's is found by its shape (prefix and random part), seen or not; the enrollment token and the refresh token are recorded right away; a setup step that fails is a FAIL line with a fixed text, never a panic carrying a command's output; teardown never panics; the steps stop at 520 s, and the whole check ends within 580 s (a `const` assertion keeps 20 s of the profile's 600 s).
  >   - *The harness:* each start binds the gRPC port afresh and the proxies dial wherever it listens now, and the API's listener is bound once with each serve on a clone, so a restart never frees a port another process could take; the proxy pauses after an accept error and forgets ended connections; `RECONNECT_MAX` is fleet-proto's constant (a dev-dependency, the user's request).
  >   - *The cases:* every case ends by checking the assignment trail (no second `bot.assign` of a bot without an unassignment or a detach in between); case 3 asserts each first reconcile's bots exactly (when the high agent is back first, the low one's first reconcile also carries the new bot: its welcome opens the gate, and fleet-core's `plan` places waiting bots on a welcomed agent in the same pass); case 4 times from the stream end after the silence; case 5 caps its bot's retries at 10 s; case 6 counts every denial line.
  > - **Follow-ups from G's review** are commit 1: dev-bots labels its IDs; a tick that wrote nothing is `Idle`; the rig's `Gate` keeps one lock; the busy-removal test asserts the hello and the deferral; the conductor warns once per wait; `AgentKey::from_pem` and `certificate::check` take exactly one PEM section; the ingest holds 4096 rows in all (S74).
- [x] **P10.10** Admin endpoints: `GET /agents`, and `POST /agents/{id}/disable`, which revokes the fingerprint and drops the stream.

  > Note (P10.10, from Phase 6, group C) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): a response value that a later version may extend, such as an agent's status, is an `Open<…>` in its DTO, and a 64-bit number is a `SafeInt`.

  > Note (P10.10, from Phase 10's planning) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **disable revokes every fingerprint** the agent holds, not one (S1). `GET /agents` shows the current certificate's `not_after`, so an expiry is visible before P13.6's renewal, and the capacity as a plain number; there are no labels (S8).

  > Note (P10.10, Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **as built** (S31).
  > - **`GET /api/v1/agents`** (Admin+, `ViewAgents`; class Standard): Appendix B's keyset pages of every agent, newest first: `AgentPage {items, next}` of `AgentResponse {id, name, enabled, status, capacity?, version?, connected_since?, last_seen_at?, not_after?, created_at}`. `status` is an `Open<AgentStatus>` (`online`, `offline`, `expired`, `disabled`) from fleet-core's rule (`agent::status`): disabled, else expired when no unrevoked certificate is valid, else online while its stream lives; `not_after` is the latest of its unrevoked certificates; `last_seen_at` the later of the stored time and the live stream's. The capacity is a plain number (1–1000).
  > - **`POST /api/v1/agents/{id}/disable`** (Admin+, `DisableAgent`, step-up; class Standard): authorized before anything is read, then one transaction (the acting user re-read and authorized again, `enabled` off, every unrevoked certificate revoked, `agent.disable` with `name`, `certificates_revoked` and `via`), then the live stream ended through `LiveAgents::end` before the answer: 200 with the agent. Disabled already: 200 as it is, no entry. It works with `[grpc]` off. Unknown or malformed IDs are 404.
  > - **From the host** (U3, U8): `afkfleet-server agent list --config …` (tab-separated after a header, `-` for nothing, the state `enabled`, `expired` or `disabled`: the host sees no stream) and `agent disable --config … --id …` (`via` = `cli`, no actor; exit 0 disabled now or before, 1 unknown or a failure, 2 usage). Neither creates or migrates the database; both work while serve runs, whose sweep ends a disabled agent's stream within one 5 s sweep, or two (up to 10 s) when the disable lands between a stream's hello transaction and its activation. `just dev-agents` and `just dev-disable-agent id` run them on dev-server's database.
  > - **Deviation:** no 422 test for the disable route: it has no body or query, and a malformed path ID is 404, as P10.3's token route has no 422 test either. fleet-client's agents methods come with P11.9.
  > - **For P11.9:** the agents view pages through `GET /agents` with a text for each status and a default branch (it's an `Open<…>`), disables through `useStepUpAction` with a text saying it's final, and shows `expired` apart from `disabled`; the token stays a direct call. The audit viewer shows `agent.connect` Failures with their reason and IP; how each reason reads on screen is the user's to decide. **For D:** a bots count may join `AgentResponse` (additive), and a new status goes in through `Open`; a disabled agent's bots never get a `Removed` from it, so they follow the stale path.

**Security:**
- A client certificate is required for `Connect`.
- An agent only gets session tokens for its own bots.
- Enrollment tokens are hashed and single-use.
- The gRPC port is published only when remote agents exist.

**DoD:** In managed mode, a bot with a real Microsoft account, started through `POST /bots/{id}/start`, goes Online on a real server and goes offline through `stop`.

> Note (Phase 10's DoD, from Phase 10's planning, the user's choice, U1) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the app's part moved to Phase 11.** The DoD read "pressing Start in the app brings a bot with a real Microsoft account Online on a real server", which needs P11.9's screens. The user does this check by hand in group H, with their own account; it needs Docker for a local online-mode Minecraft server (as in P1.8), unless they point it at their own. P10.9's loopback suite is the rest of the proof.

> Note (Phase 10's DoD, from group E, the user's note at the plan review) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): the check's agent lists the local Minecraft container's network in `[control_plane] private_targets` (S46), with a comment saying why, or its SSRF check refuses the connect. The check calls `PATCH /bots/{id}` (after a step-up), `POST /bots/{id}/start` and `stop` through fleet-client (`patch_bot`, `start_bot`, `stop_bot`).

> Note (Phase 10's DoD, group H) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md), S73, U21, U22): **the check is written; the user runs it** with `just test-managed-account`: `managed_check::manual_managed_bot_goes_online_and_offline` in fleet-integration, Phase 9 group D's pattern (a vault key drawn for the run and kept in memory, a temporary folder deleted at every end, only this test's filter, nothing secret on a command line, in a variable or in a log). Its agent lists exactly `127.0.0.1/32`, the container's published port, with a comment saying why. Its steps stop at 520 s, and the whole check ends within 580 s (PR #58's review). Its outcome is recorded here once the user has run it. **The user's first run (2026-10-18)** failed at setup, before anything reached Microsoft: the check's `agent.toml` put `heartbeat_file` at the top level, where the agent's loader refuses it as an unknown key (it belongs under `[runtime]`), and the log check counted two migration comments naming a token's format (`afk_iv_…`, `afk_et_…`), which sqlx logs at `debug`, as server tokens. Fixed, red first: the TOML's `[runtime]`; the token check now looks for a token's shape, its prefix and the 43 characters of its random part; and two default-profile tests now run the check's own config, setup, login, teardown and log check on the real path but for Microsoft and Docker (`managed_check::tests::the_agents_config_loads_as_the_check_writes_it`, `managed_check::tests::the_checks_setup_login_and_teardown_pass_without_an_account`). **The user's second run (2026-10-18) passed every line**, in 55 s: started and enrolled, logged in with 2FA, the link started, linked and active, the bot listed (`unplaced`, `no_server`), the Minecraft server started, the server set after a step-up, started, Online, stopped, offline (`unplaced`, `stopped`), the agent, the server and the container stopped or removed, the folder deleted, and no secret in any log. The bot's states, for information: its first read already showed a backoff after a kick, then connecting, then Online (the check shows a kick's kind, never its key or text). **Phase 10's DoD is met.**

---

### Phase 11: Bot operations, modes & live events
**Goal:** Full day-to-day management from the app: start and stop, modes, live chat and history.
**Introduces:** `tokio-tungstenite`, `futures-util` (WebSocket client).

> Note (P11, Phase 11's planning, 2026-10-19) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md)):
> - **Thirteen group branches.** Phase 11 is built in group PRs like Phases 2–10: one branch and one commit per task, in this order, each after the previous PR is merged. Backend first; each screen comes after the endpoints it needs.
>
>   | Group | Branch | Tasks |
>   |---|---|---|
>   | A | `p11/u24-p11.3-fresh-requests-and-agent-chat` | U24 (ADR-0019), then P11.3's agent side: commands that need a signature, and the chat rate in each bot's spec |
>   | B | `p11/p11.3-chat-endpoints` | P11.3's API (ticked here) |
>   | C | `p11/p11.4-modes` | P11.4 |
>   | D | `p11/p11.5-live-events` | P11.5 |
>   | E | `p11/p11.6-p11.7-retention-and-full-flow` | P11.6, P11.7 |
>   | F | `p11/p11.9-admin-endpoints` | P11.9's backend: `GET /audit`, `agent release` over the API, the grants review at a role change |
>   | G | `p11/p11.8-events-in-the-app` | P11.8 |
>   | H | `p11/p11.9-bot-screens` | P11.9's dashboard and bot detail; the DoD's first check |
>   | I | `p11/p11.9-mode-editor` | P11.9's mode editor |
>   | J | `p11/p11.9-admin-screens` | P11.9's admin screens (ticked here) |
>   | K | `p11/p11.10-dark-mode` | P11.10 |
>   | L | `p11/p11.11-design-pass` | P11.11 |
>   | M | `p11/p11-wrap-up` | The threat model, the README, the DoD's 24-hour run |
>
>   - **P11.3 is spread over A and B**, and **P11.9 over F, H, I and J**, each ticked in its last group, as P10.5 was over C and D. Each group aims at Phase 10's F or H in size (about 5,300 lines); one that grows past it is split at its plan review.
>   - **New dependencies** arrive with their group, each shown to the user with its version, license and release date: axum's `ws` feature (tokio-tungstenite and tungstenite 0.29.0, which axum 0.8.9 pins), tokio-tungstenite as fleet-server's dev-dependency, and futures-util and moka only if D needs them; tokio-tungstenite as fleet-integration's dev-dependency in E; tokio-tungstenite and futures-util in fleet-client in G, its TLS features G's question; single Radix packages in H, I and J; bundled fonts in L.
>   - **Who decides what:** the review agents decide each group's open questions (CLAUDE.md's step 10); the user decides P11.6's size bound (E), P11.11's stable screenshots and visual direction (L), every text friends read (one table per group plan), the looks and UX questions, and whether the chat console names who sent each line (B).
> - **The DoD's two checks** are the user's, with their own accounts. The first ("pressing Start in the app …") in H, once the bot screens exist; the second (24 h) in M, after the design pass. Each manual check gets default-profile tests that run its own setup, config and log check without the account (Phase 10's lesson).
> - **The DoD checks' own data folder** *(the user's addition at the plan review, U1)*. Linking a real account stores its Microsoft refresh token in the server's database, sealed by a vault key file beside it. So both checks use one folder of their own, `target/dod-run/` (the database, the vault key, the PKI, the agent's identity, the logs), through recipes of their own that H adds, never the everyday `target/dev-server/`, so the account linked in H stays for M. They log at the default `info`, so no chat text reaches the disk. A cleanup recipe deletes the whole folder when the user is done, with a default-profile test that it removes everything inside, touches nothing outside, follows no link and refuses any other path. The steps say: unlink the accounts in the app first, then run the cleanup, then optionally end the app's consent in the Microsoft account. This mirrors production, where the key file lives on the server's host; `target/` is never committed.
> - **Decided by the review agents** *(approved by the user at the plan review)*, S1–S6 in ADR-0020: U24 in fleet-core's state (S1); commands that need a signature refused where chat must be signed (S2); one chat rate per bot, the server's, sent in its spec (S3); a step-up route for commands off the allowlist (S4); the send's bounded answer (S5); a user's message stored as a chat row, audited without its text (S6). The chat limits' ranges (S3) are the user's approval at the plan review (rule 10).

> Note (P11, from Phase 8, group E, the user's request) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)): **Decide step-up per action, the same way as the admin writes.** Before building them, decide for each destructive or privilege-changing action of this phase whether it needs a step-up: deleting a bot, changing grants, deleting a shared mode, and anything else that loses data or widens access. The app reuses the admin screens' "send, and on `step_up_required` ask for the password and retry once".

> Note (P11, from Phase 10, group H, the user's choice at PR #58's review, U24) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **Phase 11's first group, first:** after a refused login, a bot's next session request stays fresh until a fresh grant arrives, so a backoff never asks without `fresh` and the server never hands out the cached session that was just refused. It gets its own test, and P10.9's case 5 (`outage::a_bot_kicked_while_the_server_is_down_rejoins_with_its_kept_grant_and_a_fresh_request_waits`) is then tightened to expect it.
>
> *Done in group A ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S1):* fleet-core's `BotState::Backoff { attempt, fresh }` carries a fresh request through each backoff until a session arrives, and `restore` keeps it; the proto's `Backoff` gains `fresh = 2`. A fresh session that's refused again fails the bot `Failed(Auth)`. The agent's "a session request couldn't be sent" line names `fresh`. Tests: fleet-core's `transition::a_refused_logins_requests_stay_fresh_through_its_backoffs_until_a_session_arrives` and the properties beside it, the runtime's `bot_actor::a_refused_logins_requests_stay_fresh_through_its_backoffs_until_a_grant_arrives` and `supervisor::a_crash_during_a_fresh_backoff_keeps_the_next_request_fresh`, the chaos test's stricter rule and its target "fresh retry after a backoff", and case 5, which now asserts every state and unsent request after the refusal fresh, the server's one grant after the outage fresh, the next `Connecting` with `auth_retried: true` and no fresh-limit refusal. *Residual:* a deliberate restart, a server change or an agent restart after a refusal asks once without `fresh`.

- [x] **P11.1** 🔴 Migrations and repositories:
  - `bots`: 1:1 with an account; server address, mode, desired state, assigned agent
  - `modes`: owner, visibility, definition JSON (validated by core), version
  - `chat_messages`

  > Note (P11.1, from Phase 10's planning, the user's choice, U1) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **built in Phase 10.** `bots` and `modes` come in Phase 10's group D, with P10.5's scheduler and reconciler, their first readers; `chat_messages` in group G, with P10.8's ingest, its first writer, which ticks P11.1. Each table keeps the notes below. The per-server conflict texts stay in Phase 11: Phase 10's specs carry an empty list, and the per-server storage arrives later in a table of its own.

  > Note (P11.1, from group C): The `modes` migration seeds the built-in modes `afk` and `farm`: fixed v7 IDs (ADR-0010), no owner, and the JSON of `ModeDefinition::afk()` and `farm()`.

  > Note (P11.1, Phase 10, group D) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`bots` and `modes` as built** (`0013_modes.sql`, `0014_bots.sql`; the columns in Appendix D). Every account has its bot: created with it on both paths that create one, minted right after the account's ID (S35), and the migration gives every existing account one (its ID is the account's with the last hex digit's high bit flipped; no server, `afk`, stopped). No `auto_start` yet: it arrives with its first writer, P11.2's `PATCH` (S34). `last_state_reason` holds a paused or failed state's reason (S40). The seeds' IDs are `01a14202-2800-7000-8000-000000000001` (`afk`) and `…0002` (`farm`), `ModePreset::id()` (S38). Ticked in G, with `chat_messages`.

  > Note (P11.1, Phase 10, group E, the user's choice, U12) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **no `auto_start`, ever.** `desired_state` stays the only "should run" flag: a bot runs whenever it should and can (S33), and `start` and `stop` set it. The column never arrives and `PATCH` carries none. `0014_bots.sql`'s comment ("Auto-start arrives with its first writer") stays as merged: `migrations-check` forbids editing a merged migration.

  > Note (P11.1, from Phase 4, group B, the user's decision) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): **Outgoing mode chat** arrives as `ModeChatSent{message}` (P4.5), so `chat_messages` can record what a mode said, with direction out, next to user chat.

  > Note (P11.1, from Phase 4, the user's decision) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): **Conflict texts are stored per server** in managed mode: set once in the app for all bots on that server, and sent to agents as each bot's resolved `ConflictTexts` in its spec.

  > Note (P11.1, Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`chat_messages` as built** (`0016_chat_messages.sql`), which ticks P11.1; its writer is P10.8's ingest (S70).
  > - **Columns:** `id INTEGER PRIMARY KEY AUTOINCREMENT` (a bot's chat is ordered and paged by ID, which is receipt order); `bot_id`, `ON DELETE CASCADE` from `bots`, so a bot's chat goes with its account; `direction`, `in` or `out`; `kind`, fleet-core's `ChatKind` name for what a bot received and `mode` for what its mode sent, with no CHECK (P11.3 adds kinds, and reading refuses one it doesn't know); `sender_name` and `sender_uuid`, a player message's sender as the Minecraft server claims it (Appendix D's `sender`, in two columns); `text`, fleet-core's rebuilt, cleaned text (S6); `truncated`, the agent's flag or the server's own cut; `at`, the server's time of receipt, never the agent's (S5). CHECKs: a UUID only with a name, and an outgoing row has no sender and no cut. Indexed by `(bot_id, id)` for P11.3's pages and by `at` for P11.6's retention.
  > - **`ports::chat`:** `ChatWrites::append` stores a row only while the bot's row names the agent that reported it (`INSERT … SELECT … WHERE EXISTS` in the batch's write transaction), and answers `None` otherwise without failing the transaction, so a bot deleted or moved since its message arrived never fails a batch; `ChatReads::page` is newest first, `before` exclusive, one row more than the page. Reading rebuilds each entry through fleet-core's constructors, and a row they refuse is `StoreError::Corrupt` naming its column.
  > - **Tests,** red first against a stub that stored nothing and found nothing (11 of 12; the CHECK test, which the migration satisfies, was shown red by dropping the `direction` CHECK, reverted and checked by checksum). The migrations' checksum snapshot gains `16 chat messages`.
- [x] **P11.2** 🔴 Bot endpoints:
  - `GET /bots`, `GET /bots/{id}`
  - `PATCH /bots/{id}` (server, mode)
  - `POST /bots/{id}/start|stop|restart|reset|resume`

  Every call is authorized, audited and reconciled.

  > Note (P11.2, from Phase 10's planning, the user's choice, U1) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **built in Phase 10's group E,** right after the reconciler, so "reconciled" is the real one, with fleet-client's bot methods for P10.9's tests and the DoD check. Its open notes (SSRF, the answer for a Paused or Failed bot, and P11's step-up decision for `PATCH` and the five lifecycle calls) are decided there. Restart, reset and resume need a message the protocol doesn't have yet: group E adds it to `fleet.v1` as a new oneof case (S3). The Tauri commands, `ApiClient` and the bot screens stay in Phase 11.

  > Note (P11.2, from Phase 6, group C) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): the bot's state, mode and disconnect reason in a response are values a later version may extend, so their DTO fields are `Open<…>`.

  > Note (P11.2, from the group A review): **SSRF.** `ServerAddress` allows loopback and private addresses (ADR-0010). An SSRF policy for the server address must check the resolved IP addresses, not the host string.

  > Note (P11.2, from group B): `start`, `stop` and `restart` change nothing for a Paused or Failed bot: the state machine ignores `Start` and `Stop` there (ADR-0010). Decide here what the API answers in that case.

  > Note (P11.2, from group D) (ADR-0010):
  > - **Switching a bot's mode** takes two checks: `ViewMode` on the mode, and `SetBotMode(allowlist.check_mode(&mode))` on the bot.
  > - **Server address and auto-start** need Manage (`ConfigureBot`).
  > - **Lifecycle calls** need `ControlBot`.

  > Note (P11.2, from Phase 9, group E) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): a bot's DTO follows its account's field rule: who owns it, and since when, only for Admins, the Owner and Manage holders. Deleting an account (P9.6) deletes its bot with it.

  > Note (P11.2, from Phase 10, group D) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)):
  > - **Reads** go through `app::bots::BotService` (`view`, `list`), after `authorize()`; a bot's state reads `Reported`, `AwaitingReport`, `Unknown` (its agent has no live stream) or `Unplaced` with why (S41).
  > - **Every write that changes where a bot may run** (`PATCH`, start, stop, Resume, Reset, a mode's change) hints the reconciler (`Change::Store`) after its commit; the 30 s pass catches a lost hint.
  > - **`auto_start`** arrives with `PATCH` by `ALTER TABLE … ADD COLUMN` (S34); what auto-start does for users is asked at E's planning.
  > - **Resume and Reset of a sticky bot that isn't placed** clear its stored `last_state` in the same transaction: the reconciler restores the stored one.
  > - **Before E:** `link_flows/commit.rs`'s two clock reads become one, in its own PR (U11).

  > Note (P11.2, Phase 10, group E) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **as built.** The user's choices: no auto-start (U12; P11.1's note) and the four 409 texts (U13). Decided by the review agents and approved at the plan review: S46–S56; reconciliations R31–R38.
  > - **Routes** (class Standard; Appendix B): `GET /bots` (keyset pages, newest first: the bots of the accounts the caller may View, filtered in SQL and each checked again with `authorize(ViewBot)`), `GET /bots/{id}`, `PATCH /bots/{id} {server_address}` (Manage, step-up), `PUT /bots/{id}/mode {mode_id}` (`ViewMode` on the mode, `SetBotMode(check_mode)` on the bot) and `POST /bots/{id}/start|stop|restart|reset|resume` (Control). Each answers `BotResponse {id, account, server_address?, mode_id, desired_state, status, agent?}`: the account under its field rule, `agent` for Admins and the Owner only, and `status` flat, with `kind` (`reported`, `awaiting_report`, `unknown`, `unplaced`) and the fields of its kind; every enum is an `Open<…>` (S54).
  > - **Deviation (S47):** the mode left `PATCH` for `PUT /bots/{id}/mode`, since a step-up is per route and only the server address needs one. `PATCH` takes only a required `server_address`, and nothing clears it (R33, S55).
  > - **The lifecycle** (S48, S50, S51): start and stop store `desired_state` and answer 200 whatever the bot reports; a bot that can't run yet reads unplaced, with why. Stopping a Paused or Failed bot frees its slot and keeps its state. Restart, and resume or reset of a bot on an agent, are one-shot commands (`ServerMessage::Lifecycle`), recorded and then sent only to the stream that holds the bot Assigned: 202 with the bot. Resume or reset of a bot that runs nowhere clears its stored state and the server's kept report in one transaction: 200. Anything else is 409 `bot_not_running`, `bot_not_paused`, `bot_not_failed` or `bot_unreachable` (fleet-core's `bot::judge_command`). A command whose send fails after its commit is 409 `bot_unreachable`, and its entry stays.
  > - **Writes** re-read the actor, the bot, its account's grant and the mode in their transaction and authorize again, write only the changed column (never `agent_id`), commit with their entry (`bot.set_server`, `bot.set_mode`, `bot.start`, `bot.stop`, `bot.restart`, `bot.reset`, `bot.resume`; S53) and hint `Change::Store` after the commit. A write that changes nothing answers 200 with no entry and no hint. An unknown or hidden mode is one 422 on `mode_id`; a mode with a command off the allowlist needs Manage (403).
  > - **Also:** the per-bot limit (§7.4's note, S52); `[chat] command_allowlist`, read by the mode switch (R37); `Assignments::assign` returns the row's stored state, from which a new placement's restore comes; fleet-client's `bots`, `bot`, `patch_bot`, `set_bot_mode`, `start_bot`, `stop_bot`, `restart_bot`, `reset_bot` and `resume_bot` (S56).
  > - **SSRF** (S46): the server doesn't check where an address points; the agent will, right before it dials (P10.7's note). The `PATCH`'s OpenAPI description says so.
  > - **Hands-on:** none before G. Without the managed agent no bot runs through the server, the app has no bot screens until P11.9, and calling the API by hand would need curl or a password on a command line (ADR-0016, ADR-0017). H's DoD check calls these routes for real.
  > - **PR #54's review** (ADR-0019): a command goes only to the stream of the generation found holding its bot, checked and queued in one critical section; a bot whose agent isn't online (disabled, its certificates revoked or expired, before its stream ends) is 409 `bot_unreachable`; a resume or reset puts the kept report back unless its commit lands (a caller dropped mid-commit included), and refuses when its clear changes nothing; a write's answer asks `authorize()` again (404 once the caller may no longer see the bot); `server_address` shows only to Control holders and above.
- [ ] **P11.3** 🔴 Chat:
  - `POST /bots/{id}/chat`:
    - takes a `ChatMessage`
    - `/commands` need Manage unless they're on the allowlist
    - rate-limited per bot and per user
    - audited
  - `GET /bots/{id}/chat?before=&limit=` with cursor pagination.

  > Note (P11.3, from Phase 6, group C, the user's decision) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): the cursors (chat's `before`, and `GET /audit`'s) are `INTEGER` row IDs, so they cross the API as `SafeInt`, never a plain `i64`.

  > Note (P11.3, from group D): The handler checks `SendChat(allowlist.check(&message))` and then sends that same message. An allowlisted command passes with any arguments (threat model). Reading the history needs `ViewBot` (ADR-0010).

  > Note (P11.3, Phase 11, group A) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S2, S3): **P11.3's agent side as built** (P11.3 is ticked in group B, with the API).
  > - **Secure chat (S2):** wherever fleet-mc's `needs_signing` holds, a command whose name can reach a `message` argument in the server's command tree is refused before sending, with `ChatUnavailable`, even once chat is signed: azalea signs no command. The tree is read by `command_tree::MessageCommands`, one backward pass, each node at most once, a redirect to the root not followed, indexes out of range ignored; until a tree arrives commands go out as before. User chat gets `ChatFailed{ChatUnavailable}`; mode chat is skipped at `debug`; fleet-mc warns once per session, never naming the command. No wire change.
  > - **The chat rate (S3):** `fleet_core::chat::ChatRate` (interval 1–60 s, burst 1–10, 3 s and 3 by default), the server's `[rate_limits] chat_per_bot_interval_secs` and `chat_per_bot_burst`, sent in each bot's spec (`BotSpec.chat_rate`, wire field 7) by the reconciler. The agent's runtime builds each bot's bucket from its spec and swaps it, full, for every clone when a spec brings another rate; the same rate keeps it. The runtime's fleet-wide `chat_interval` and `chat_burst` and `FleetSetupError::ChatQuota` are gone; their zero-interval check is `ChatRate`'s. A standalone agent uses the default. The HTTP limits that spend it come with the API (B).

  > Note (P11.3, from Phase 3, group E, found by the user's real-account check): **flagged, not decided.** *Decided in Phase 11, group A: see the note above (S2).*
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
  > - **The per-bot chat limit.** §7.4 says it's configurable, and the agent's queue uses one message per 3 s, burst 3 as a default. Decide here where that setting lives (server settings, sent to agents). *Decided in Phase 11, group A ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S3): the server's `[rate_limits]`, sent in each bot's spec.*

  > Note (P11.3, from Phase 10, group E) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`[chat] command_allowlist` exists** (`ServerConfig.chat`, P2.9's rules; the mode switch reads it), and `BotService` has it; `retention_days` stays P11.6's. The bot routes answer in extractor order (R38): 401, the per-user 429, a malformed path's 404, the body's 400/422, an unseen bot's 404, then 403; the chat route follows it, and re-reads the actor and the grant in its transaction as the bot writes do.

  > Note (P11.3, from Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **a `SendChat` dies with its stream.** The agent applies one only while the stream that carried it lives, and its `ChatSent` or `ChatFailed` may come on a later stream or never: the outbox keeps answers across an outage, bounded, and drops those of bots the next reconcile doesn't give. P11.3's wait for an answer is bounded, and one that never comes reads as unknown. `chat_messages` holds what bots received and what their modes sent; a user's message is P11.3's to store.
- [ ] **P11.4** 🔴 Modes CRUD: `GET`, `POST`, `PUT`, `DELETE` on `/modes`. Core validation errors become field errors. Built-in modes are read-only.

  > Note (P11.4, from Phase 2): Editing a mode that's assigned to bots must re-run the command check for every one of them. A mode with a command outside the allowlist needs Manage on each bot (ADR-0010).

  > Note (P11.4, from group E): open question, flagged and not yet decided. `fleet-core`'s own errors never quote their input, but serde_json's do: an unknown action type or a bad field value in mode JSON comes back as e.g. ``unknown variant `…` `` or `invalid type: string "…"`, which can hold chat text. The existing rules already say: internal errors never reach clients, and chat is logged only at debug. How the server maps and logs these errors is decided here (ADR-0010).

  > Note (P11.4, from Phase 10, group E) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **a bot's mode is switched by `PUT /bots/{id}/mode {mode_id}`** (S47, S55): `ViewMode` on the mode (an unknown or hidden one is one 422 on `mode_id`, so modes can't be probed), then `SetBotMode(check_mode)` on the bot (a command off the allowlist needs Manage, 403), both again in the transaction (`ModeWrites::find`). Editing a mode bots use re-runs that check for each (the Phase 2 note above); whether a mode's edit or deletion needs a step-up is decided here, under P11's step-up note.

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

  > Note (P11.5, from Phase 6, group E, the user's addition) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): **a WebSocket must close itself on shutdown.** Once upgraded, it isn't bound by the request timeout, and the graceful shutdown waits for every open connection, so each WebSocket handler watches the shutdown `CancellationToken` and closes its connection with a close frame when it fires. Otherwise one open app makes every shutdown wait the full deadline.

  > Note (P11.5, from Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the bus** is `app::events::EventBus` (`App.events`, made in `app_with`): a `broadcast` of `Arc<LiveEvent>` (1024) with statuses, deaths, alerts and chat answers at once, and chat after its commit with its row ID. A subscriber that lags gets `Lagged` and resyncs from the bots' views. It carries what an agent reported, not what the server decided: P11.5 publishes the shown-state changes it needs itself (assignments, a bot's desired state).
- [ ] **P11.6** 🔴 Retention job: chat older than `chat.retention_days` is deleted. The audit log is kept longer (configurable).

  > Note (P11.6, from Phase 7, group B, the user's decision) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **sessions are cleaned up here too.** A session, with its `rotated_refresh_tokens` rows (ON DELETE CASCADE), is deleted once `created_at` + the current `refresh_token_absolute_days` has passed, whether it was revoked or not. A revoked session is unusable anyway, and keeping its rotation history until then lets an investigation after a reuse see the family's timeline. `sessions(created_at)` is indexed for it.

  > Note (P11.6, from Phase 6, group B) ([ADR-0015](docs/adr/0015-fleet-server-conventions-and-phase-6-refinements.md)): decide `secure_delete` here, so deleted chat and old audit entries are overwritten instead of lingering in free pages.

  > Note (P11.6, from Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)) *(the user's note at G's plan review)*: **retention must also bound size,** for example the newest N rows per bot, not only `retention_days`: up to about 2000 rows a second of up to 1024 characters can reach the ingest, and every bot on one Minecraft server stores its own copy. The limit itself is the user's decision here. `chat_messages` has the `(at)` and `(bot_id, id)` indexes both need.

  > Note (P11.6, from Phase 10, group H) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md), S74): a retention delete holds the write connection; meanwhile the ingest's ticks find it busy and keep their batch, which now holds up to 1000 of the 4096 places, so more chat drops while a long delete runs. Delete in short batches, as the lockout purge does (1000).
- [ ] **P11.7** 🔴 Full-flow integration test: in-process server + in-process agent with `FakeConnector`.
  1. Start a bot via `fleet-client`.
  2. A WebSocket client sees `Online`.
  3. Chat flows in and out.
  4. Stop the bot.

  > Note (P11.7, from Phase 10, group H) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md), U19): **it lives in `crates/fleet-integration`**, a module of its one test binary beside `loopback/`, over the same harness: `World` (the server through serve's wiring, the Owner through fleet-client with 2FA), agents enrolled for real and run over the `Ledger` connector, and the log the waits wake on. The WebSocket client it needs arrives with P11.5 (`tokio-tungstenite`), a new dev-dependency of the crate then.
- [ ] **P11.8** 🔴 Tauri: the WebSocket lives in Rust (`fleet-client` + reconnect) and is forwarded to the UI over a Tauri `Channel`. The UI patches the TanStack Query cache from the events.
- [ ] **P11.9** 🔴 UI:

  > Note (P11.9, from Phase 10, group D) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): the agents view shows each agent's bots count (`AgentResponse.bots`: its bots and open release records); releasing a disabled agent's bots becomes an API action (Admin or Owner, step-up, `agent.release` with `via` = `api`), with a text saying the operator vouches the agent's host is stopped. The texts for it and for the shown states are the user's.

  - **Dashboard:** a card per bot with a state badge, uptime, server, mode and last disconnect reason.
  - **Bot detail:**
    - controls and a mode selector
    - **live chat console** that renders plain text only
    - a send box that shows rate-limit feedback
    - an event timeline
  - **Mode editor:** an action list with validation.
  - **Admin:** an audit-log viewer and an agents view.

  > Note (P11.9, from Phase 10, group E) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the bot screens.**
  > - **Texts:** the four 409 codes' messages are fixed on the server (U13); the controls' labels and the shown states' texts are the user's, asked here.
  > - **Stop on a Paused or Failed bot** is allowed: it frees the bot's slot and keeps its state (S51). Whether the controls offer it beside Resume or Reset is this task's UX.
  > - **A 409** (`bot_unreachable`, `bot_not_running`, `bot_not_paused`, `bot_not_failed`) shows its message and refetches the bot. A 202 means sent, not done: the result arrives as the bot's status.
  > - **Every `Open<…>`** (`desired_state`, `status.kind`, `state`, `reason`, `why`, the last disconnect's kinds) has a default branch. `server_address` is there only for Control holders and above, `agent` only for Admins and the Owner.
  > - **Step-up:** `PATCH /bots/{id}` goes through `useStepUpAction`; the mode switch, the lifecycle calls and the reads don't need one. A per-bot 429 shows its `Retry-After`.

  > Note (P11.9, from Phase 10, group B, the user's choice, U3) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **enrollment tokens in the agents view.** `POST /agents/enrollment-tokens` exists (the Owner's, step-up, no body); the view shows the bundle once, for the Owner to pipe into `afkfleet-agent enroll`. The bundle is a secret for 15 minutes, so the call is a direct one through `useApiClient()`, never a TanStack query or mutation (CLAUDE.md). `afkfleet-server agent create-enrollment-token` stays for the server's host.

  > Note (P11.9, from the group A review): **Chat console.** Render each message as its own block, with the sender outside the text, so a `\n[Server] …` inside a message can't pass as a separate line. The sanitizer keeps combining marks, so clip stacked ones (Zalgo text) with CSS overflow.

  > Note (P11.9, from group B): **Controls.** For paused or failed bots, the app shows Resume or Reset instead of Start and Stop (ADR-0010).

  > Note (P11.9, from group D, the user's request): **Promotions.** When a Member is promoted to Admin (P8.8), the app shows the existing grants on their accounts, and the server provides them, so the Owner can review and revoke them. Grants made before the promotion survive it, including one an Admin gave an alt account (ADR-0010, threat model).

  > Note (P11.9, from Phase 9, group F, the user's request) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **leaving a shared account.** A View or Control grantee can't remove themselves from a shared account; only a Manage holder revokes a grant. A "leave" action could come here.

  > Note (P11.9, from Phase 9, group E) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **demotions too.** A grant can't be made to someone with implicit Manage, but one made before a promotion stays dormant, and comes back when that Admin is demoted, so the review at a role change shows the grants the user holds as well as those on their accounts.

  > Note (P11.9, from Phase 3, the user's request): **Parsed senders.** If P4.1's per-server chat format is built, the chat console shows a sender parsed from the text visibly differently from one a player packet named. Neither is verified: azalea 0.16 doesn't verify chat signatures, so every chat sender is only what the server claims, and none drives a permission or trigger decision (ADR-0011, threat model).

  > Note (P11.9, from Phase 4, the user's decision) ([ADR-0013](docs/adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)): **The chat format is parsed on the server.** It stays out of the bot's spec and the agent: the server already has each system message's whole sanitized text, so a per-server format can extract the sender for display here.


  > Note (P11.9, from Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`blocked`** is a new connect failure: the agent refused the server's address before dialling it, a never-dialled address or a private one its config doesn't list (S46); the UI says so, with a hint for the agent's operator. The console merges chat by its row ID (the bus's `Chat { id }`).
- [ ] **P11.10** Dark mode: the app follows Windows' light or dark setting (`prefers-color-scheme` and its change event; the `.dark` class on `<html>`, set by the bundled script so the CSP allows it), and every screen is checked in both themes.

  > Note (P11.10, from Phase 8, group D, the user's decision) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)): Phase 8 ships the light theme only (sonner's `theme="light"`). `packages/ui/src/styles.css` already holds shadcn's dark tokens under `.dark`.
- [ ] **P11.11** UI design pass, before Phase 12 and once every screen exists:
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

  > Note (P11.11, from Phase 8, group D, the user's request): Phase 8's screens are functional first: shadcn's radix-nova components with the light theme, no design of their own yet.

  > Note (P11.11, from Phase 8, group E, the user's addition) ([ADR-0017](docs/adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)): **Stable screenshots.** The screenshot tests need a stable rendering, but P8.9's suite drives the installed Edge, whose version moves with Windows and the runner image. P11.11 decides then how to keep screenshots stable, for example one fixed OS with a pixel tolerance, or a pinned browser whose download is verified.

**DoD:**
- Pressing Start in the app brings a bot with a real Microsoft account Online on a real server (moved from Phase 10, the user's choice, U1 in ADR-0019).
- The user's real fleet runs for 24 h, managed from the app. Every event shows up live, and the logs contain no unhandled errors.

---

### Phase 12: Deployment & operations
**Goal:** A reproducible, hardened production deployment and a desktop app that can be distributed.
**Introduces:** `tauri-plugin-updater`. Tools: cargo-chef, Docker Compose, Caddy.

- [ ] **P12.1** Dockerfiles for the server and the agent:
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
- [ ] **P12.3** 🔴 Backups:
  - a scheduled `VACUUM INTO` with retention, plus a `backup` CLI
  - a restore runbook
  - Backups contain only ciphertext. **Back up the vault key separately and securely.**
- [ ] **P12.4** A Prometheus metrics endpoint on an internal port, with the key metrics documented.

  > Note (P12.4, from Phase 7, group B, the user's decision) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): a counter of detected refresh-token reuses belongs among the key metrics, so alerting can pick up possible token theft without anyone reading `/audit` or the logs.

  > Note (P12.4, from Phase 10, group E) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): the per-bot limit's refusals (`bot_actions` in the minute's summary) belong with the other rate limits' counts, by limit.

  > Note (P12.4, from Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): `afkfleet_agent_connects_refused_total` (by reason) and `afkfleet_agent_identity_conflicts_total` belong among the key metrics: past the daily cap a disabled or revoked certificate still in use shows only at `debug` (S30), and a conflict is recorded once an hour (S26).

  > Note (P12.4, from Phase 10, group F) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): session grants and denials belong among the key metrics, e.g. `afkfleet_session_grants_total{kind,fresh}` and `afkfleet_session_denials_total{reason}` (S61, R43: a grant writes no audit entry, and its `info` line is the only other trail); the fresh-session limit's refusals (`fresh_sessions` in the minute's summary) go with the other rate limits' counts.

  > Note (P12.4, from Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): the outbox's counts (chat dropped, refusals, session requests dropped) and the ingest's (rows stored, skipped and dropped per agent; housekeeping's `chat_dropped`) are metrics to export.
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

  > Note (P12.6, from Phase 5, group D, the user's decision) ([ADR-0014](docs/adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)): The **Minecraft version upgrade** section carries ADR-0003's bump procedure, whose verify step now runs `just ci`, `just test-slow` and `just demo-agent` (1 h), with the demo's `summary.txt` in the bump's pull request.
  > Note (P12.6, from Phase 9, group B, the user's decisions) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)):
  > - **The manual Microsoft check:** `just test-ms-account` checks the Microsoft adapter with the operator's own account. It prints a link and a code, then pass or FAIL per step; `just test-ms-account declined_link` checks a refusal.
  > - **CA certificates in the server image:** reqwest's platform verifier reads the system's CA store on Linux, so the server image needs one, or every Microsoft call fails as `Certificate`. The agent image's distroless/cc base already has them.
  > - **The Minecraft version upgrade** carries ADR-0003's new steps. A new azalea crate that depends on azalea-auth is named by cargo-deny, and `deny.toml`'s wrapper list is updated with an ADR note. The adapter's endpoints, fields and headers are compared with the new azalea-auth's `auth.rs`.

  > Note (P12.6, from Phase 10, group B, the user's note at the plan review) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the server certificate's yearly renewal.** It expires after 365 days, and then every agent fails to connect. The runbook's steps: stop the server, move `server.crt` and `server.key` away, run `afkfleet-server pki server-cert --config … --dns … --ip …` with the names agents dial, start the server. The CA stays, so no agent enrolls again.

  > Note (P12.6, from Phase 10, group B) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **enrolling an agent:** `pki init` once (it prints the CA's fingerprint); a token from the Owner (`POST /agents/enrollment-tokens`, later the app's agents view) or from the server's host (`afkfleet-server agent create-enrollment-token`); then pipe its bundle into `afkfleet-agent enroll` within 15 minutes. A token is single-use; exit 3 means the server enrolled an agent the host didn't store, which is disabled before enrolling again with a new token.

  > Note (P12.6, from Phase 10, group E) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **an agent next to a local Minecraft server** lists that network in `[control_plane] private_targets`, with a comment saying why; a bot whose last disconnect is `connect_failed` with `blocked` points at an address its agent refuses (S46). Listing a network opens it to every Manage holder of a bot on that agent (the threat model's accepted risk).

  > Note (P12.6, from Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **incident response for agents:** `afkfleet-server agent list` shows every agent; `agent disable --id …` disables a lost or stolen one for good from the server's host, beside the running server (its stream ends within 10 s), as the API's disable does. An agent whose `enroll` ended with exit 3 is disabled the same way.

  > Note (P12.6, from Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **time and certificates.** The server's clock decides when an agent's certificate has expired, and TLS checks it against the real time on both hosts, so both keep NTP. An agent whose certificate expired before P13.6's renewal enrolls again: disable it first, which frees its name. A restored backup can hold an agent that was disabled after it was taken: disable it again (S27).

  > Note (P12.6, from Phase 10, group F) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **session grants.**
  > - A bot that reads `Failed` with `session_denied` was refused for good by the session-token service (`reauth_required`, `refused`, an unusable stored token): the server's `info` or `error` line "a session request was denied" names the reason; fix the account (link it again), then Reset the bot.
  > - "a granted session expires within the connect timeout" on an agent means its clock or the server's is off by minutes: both hosts keep NTP.
  > - The shutdown's `warn` "session-token refreshes were still running…" and a later `reauth_required` mean a rotated refresh token was lost: link the account again.
  > - After an agent is compromised, every account whose bot it held counts as exposed until about 24 h after its last `bot.unassign`, `bot.detach` or `agent.disable`; the server's "a session was granted" lines narrow it where they're kept (S61).
  > - A bot stuck backing off while the minute's summary shows `fresh_sessions` refusals is past its fresh-session limit (S58).

  > Note (P12.6, from Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)):
  > - **Connecting to a server, for friends** *(the user's note on the app's CA field)*: the runbook and the README get a section with the server address to enter; with Caddy and Let's Encrypt (P12.2's default) the app's CA field stays empty; for a server without a public domain, how the operator exports the HTTPS CA certificate (never the control plane's agent CA from `pki init`) and hands it out, and how friends compare its fingerprint in the app's server setup.
  > - **The agent's exit 5** and what to do (disable the agent and enroll it again, or fix its name), **its start errors** (each names its files and the fix), and **`private_targets`**' rules (at most 32 networks, each inside a private range; never checked in standalone mode).
- [ ] **P12.7** 🔴 Compose smoke test in CI, run nightly:
  1. `up`
  2. health checks pass
  3. create the Owner
  4. log in through `fleet-client`
  5. `down`

  > Note (P12.7, from Phase 7, group A, the user's decision) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): the smoke test creates the Owner with `docker compose run -T … user create-owner --username … --password-stdin`, since CI has no terminal for the prompt.

  > Note (P12.7, from Phase 7, group C, the user's note) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): the smoke test checks that a request through Caddy reaches the server with the client's real address (`[http] trusted_proxies` honored), so a missing or wrong `trusted_proxies` can't put every client into Caddy's one bucket unnoticed.

**DoD:**
- A fresh VPS gets to a running system by following only the runbook.
- A friend on another PC installs the release build, registers with an invite and logs in with 2FA, over HTTPS against that system (moved from Phase 8's DoD).

---

### Phase 13: Hardening
**Goal:** Prove the security and resilience claims.
**Introduces:** cargo-fuzz (`libfuzzer-sys`, `arbitrary`), cargo-mutants.

- [ ] **P13.1** 🔴 **Authorization matrix test:** every endpoint × {anonymous, Member without grant, View, Control, Manage, Admin, Owner} → the expected status. The table is generated and snapshotted.

  > Note (P13.1, from Phase 9, group E) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): the cast steps up first, or every step-up route reads 403 `step_up_required`. The account routes need a pending account (review, grants, re-link) and a grantee with implicit Manage (the grant routes' 422 and 409).

  > Note (P13.1, from Phase 10, group E) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the bot routes** join the matrix: every lifecycle call, `PATCH` (step-up first), and the mode switch with a mode on the allowlist and one off it. The per-bot limit is spent only after `authorize()`, so a stranger's answer stays 404; the cast needs a fresh limit per row, or rows after the fifth read 429.
- [ ] **P13.2** Fuzz targets:
  - value-object parsers
  - the chat sanitizer
  - mode definition deserialization + validation
  - proto → core conversions
  - the vault envelope parser

  > Note (P13.2, from Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): the `AgentMessage` target reaches `fleet_proto::control::read_hello`, and one target runs `fleet_proto::codec`'s check of a hello's bots over arbitrary bytes (S25).

  > Note (P13.2, from Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): fuzz `IpNet`'s parser and `PrivateTargets::problems`.
- [ ] **P13.3** cargo-mutants on `fleet-core` and auth/vault. Add tests until no relevant mutants survive.
- [ ] **P13.4** Load test: 200+ fake bots on one agent and 20 WebSocket clients. Measure CPU, RAM and event latency, then fix the hot spots.

  > Note (P13.4, from Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): the gRPC port counts no connections per address (S22's residual); a cap, if the load test calls for one, goes into the listener's map of accepted sockets.
  > Note (P13.4, from Phase 10, group F) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): measure grant throughput after a restart of both server and agent (no cached sessions: about 1.3 refreshes a second per agent with `GRANT_WORKERS` = 4), and retune the constant if needed; a load test that asks fresh sessions raises `fresh_sessions_per_bot_per_hour` in its config (S57, S58).
  > Note (P12.6, from Phase 10, group D) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **bots on a lost agent:** disable the agent, make sure its host and its bots are stopped, then `afkfleet-server agent release --id …`; a running server places the bots within 30 s. Nothing moves them by time (S32). **After restoring a backup:** an agent may run bots the database doesn't give it; they're fenced (`bot.stray`) until the agent removes them, and the agents' bots count shows them.

  > Note (P13.4, from Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): load the conductor and the ingest too, and measure chat's stored size under that load (P11.6's note: retention must bound size, not only time).
- [ ] **P13.5** Chaos tests:
  - kill and restart the server, agent and MC containers
  - partition the network between agent and server
  - fill up the DB volume

  Check that everything recovers and nothing is corrupted.

  > Note (P13.5, from Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): after a partition the server frees an agent within `[grpc] heartbeat_timeout_secs` (S23).

  > Note (P13.5, from Phase 10, group D) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): a partitioned agent's bots stay assigned to it and read `Unknown` (S32); after a server restart, nothing new is placed until every enabled agent is back or 90 s pass (S45).

  > Note (P13.5, from Phase 10, group F) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): a kill during a refresh is a crash: the account ends `active` or `reauth_required`, never corrupt, and a graceful stop never exits 4 because of Microsoft (S64); in a partition, disconnected bots rejoin with their kept grants while those last, and once the network heals every request goes to the server again (S63).

  > Note (P13.5, from Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): partitions (the agent notices a silent connection within its keepalive, S67) and kills of the server and the agent mid-batch (U16's crash case).
- [ ] **P13.6** 🔴 Agent certificates renew automatically at 2/3 of their lifetime (`RenewCertificate` RPC). Warn when expiry gets close.

  > Note (P13.6, from Phase 10, group B, the user's note at the plan review) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the server certificate too.** It expires after 365 days, and then every agent fails to connect, so "warn when expiry gets close" covers it: a warning at startup and in the housekeeping line within 30 days of its `not_after`. Serve already logs an expired one at `error` (group B).

  > Note (P13.6, from Phase 10, group B) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): a renewal's answer carries the certificate as PEM, checked by the agent like `enroll`'s (R10): one section and nothing around it, a client certificate the pinned CA signed, for the agent's key.

  > Note (P13.6, from Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): `RenewCertificate` sits behind `Connect`'s gate and identity check, spends the same per-certificate bucket and answers `CERTIFICATE_REFUSED` alike (S24, S30). A stream ends at its certificate's `not_after`, so an agent renews before it; an expired certificate means enrolling again. The renewed certificate carries the same SAN and is found by its fingerprint, and revoking the old one shows a stale process as `revoked` refusals, under the same daily cap. Both certificates share the agent's one slot: to switch to the renewed one, the old stream ends first (S26).

  > Note (P13.6, from Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): the agent loads its identity once and never re-reads it, so a renewal swaps it under a running agent (a new endpoint and kept certificate), and adds the near-expiry warning S68 left out.
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
trusted_proxies = ["172.30.0.0/24"]     # Caddy network; X-Forwarded-For is only trusted from here
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
retention_days = 14
command_allowlist = ["/spawn", "/home", "/afk"]   # usable with Control; every other /command needs Manage
                                                 # each entry: "/" + one command name, no arguments; at most 64; missing = empty
```

> Note (Appendix A, Phase 7, group E, the user's decision) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): `[auth] invite_ttl_hours` is 1 to 720 (30 days), 72 by default. An invite's expiry is stored when it's created, so a change doesn't move existing invites.

> Note (Appendix A, Phase 9, group D, the user's decisions) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **`[accounts]` arrives** with `max_per_member` (1–100, 3 by default), which counts every account a Member owns, and `allow_offline` (false by default; the dev config turns it on). `[rate_limits]` gains `link_flows_total` (1–16, 3) and `link_flows_per_user_per_day` (1–100, 10). None needs a place on the secret-name test's allowlist.

> Note (Appendix A, Phase 9, group E) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): `[rate_limits]` gains `grants_by_name_per_user_per_hour` (1–100, 10 by default, the user's approval at the plan review).

> Note (Appendix A, Phase 10, group B) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`[grpc]` as built** (P10.2's note): optional; without it the control plane is off. `heartbeat_interval_secs` and `heartbeat_timeout_secs` arrive with P10.4. `[rate_limits]` gains `enroll_per_ip_per_min` (1–60, 5 by default, S14). In `agent.toml`, `[control_plane] url` is `https://host:port` (no credentials, path or query), and its three files are absolute and distinct; the agent writes `cert_file` and `key_file` itself with `afkfleet-agent enroll`.

> Note (Appendix A, Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): `[grpc]` gains `heartbeat_interval_secs` (1–60, 10) and `heartbeat_timeout_secs` (2–300, 30); the timeout must be at least twice the interval, and an error names both keys. `[rate_limits]` gains `connect_per_cert_per_min` (1–60, 10).

> Note (Appendix A, Phase 10, group E) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): `[rate_limits]` gains `bot_actions_per_bot_per_min` (1–60, 5 by default, S52). **`[chat]` arrives** with `command_allowlist` alone (P2.9's rules: each entry `/` and one command name, no arguments; at most 64; a missing key means an empty list; an error names the entry's index, never its text); `retention_days` comes with P11.6 (R37). In `agent.toml`, `[control_plane] private_targets` (a CIDR list, empty by default) arrives with G (S46).

> Note (Appendix A, Phase 10, group F) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): `[rate_limits]` gains `fresh_sessions_per_bot_per_hour` (1–60, 6 by default, S58, the user's approval at the plan review).

> Note (Appendix A, Phase 10, group G) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`[control_plane] private_targets` as built** (S69): at most 32 IP networks in strict CIDR form (as `[http] trusted_proxies`), each inside one private range (loopback, RFC 1918, 100.64.0.0/10, fc00::/7), none a never-dialled address, none twice; each problem names its index, never its text. Empty by default, so a managed agent dials only public addresses; a parse error under `[standalone]`, which is never checked.

> Note (Appendix A, Phase 11, group A) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S3): `[rate_limits]` gains `chat_per_bot_interval_secs` (1–60, 3 by default) and `chat_per_bot_burst` (1–10, 3), each bot's chat rate, which the server sends in its spec; an error names the key. `agent.toml` gets no chat key: a standalone agent's bots use the default.

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

[control_plane]                          # managed mode (production)
url = "https://fleet.example.com:7443"
ca_cert_file = "/run/secrets/fleet_ca_cert"
cert_file = "/data/agent.crt"
key_file = "/data/agent.key"
private_targets = []                     # private networks the bots may dial (at most 32, each inside a private range)

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
| `GET /users` · `PATCH /users/{id}` | Admin+ (`PATCH`: step-up) | Role, disable |
| `POST /invites` · `GET /invites` · `DELETE /invites/{id}` | Admin+ (`POST`, `DELETE`: step-up) | Invites |
| `POST /accounts/link` · `GET /accounts/link/{flow}` | Member+ | Microsoft device-code flow |
| `GET /accounts` · `GET /accounts/awaiting-approval` · `GET/DELETE /accounts/{id}` | View / Manage (`DELETE`: step-up) | MC accounts; the pending links the caller may review |
| `POST /accounts/{id}/approve` · `/refuse` | above the owner (step-up) | Review a pending link |
| `POST /accounts/{id}/relink` | Manage | Link the account again (a link flow) |
| `GET/POST /accounts/{id}/grants` · `PUT/DELETE /accounts/{id}/grants/{user}` | Manage (writes: step-up) | Sharing |
| `GET /bots` · `GET/PATCH /bots/{id}` · `PUT /bots/{id}/mode` | View / Manage (`PATCH`: step-up) / Control | Bots and their state; the server address; the mode |
| `POST /bots/{id}/start\|stop\|restart\|reset\|resume` | Control | Lifecycle |
| `POST /bots/{id}/chat` · `GET /bots/{id}/chat` | Control / View | Chat |
| `GET/POST /modes` · `PUT/DELETE /modes/{id}` | Member+ (own) / Admin+ (shared) | Modes |
| `GET /agents` · `POST /agents/enrollment-tokens` · `POST /agents/{id}/disable` | Admin+ | Agents |
| `GET /audit` | Admin+ | Audit log (cursor-paginated) |
| `POST /events/ticket` · `GET /events?ticket=` | user | WebSocket live events |
| `GET /health/live` · `GET /health/ready` | public (no prefix) | Health |
| `GET /api/openapi.json` | public, **only in dev mode** (no `/api/v1` prefix) | The OpenAPI document (P6.10); outside dev mode the path answers like any unknown route |

> Note (P2.9): `authorize()` refines these levels (ADR-0010):
> - `POST /agents/enrollment-tokens` is Owner-only.
> - `PATCH /users/{id}`: only the Owner changes roles; Admins disable and enable Members.
> - `DELETE /invites/{id}`: an Admin invite is the Owner's.
> - `PATCH /bots/{id}`: the server needs Manage; the mode (`PUT /bots/{id}/mode` since Phase 10, group E) Control, and a mode with a command off the allowlist Manage.
> - The `user` routes check `Personal`.

> Note (Appendix B, Phase 7, the user's decision) ([ADR-0016](docs/adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)): **`POST /auth/step-up`** (user) joins the table in group D. It takes `{password}` and stamps the session's `stepped_up_at`; the "(step-up)" routes need that stamp to be at most 5 min old, otherwise they answer 403 `step_up_required`.

> Note (Appendix B, Phase 9, group D, the user's decisions) ([ADR-0018](docs/adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)): **`POST /accounts/link`** (every role) answers 201 `{flow_id, user_code, verification_uri, expires_at}`, the only time the code is shown. **`GET /accounts/link/{flow_id}`** is the flow's creator's alone (404 for everyone else) and answers `{status}` with each state's fields. P9.4's notes list both routes' answers. **`POST /accounts/offline {name}`** (every role, while `[accounts] allow_offline` is on; 404 otherwise) answers 201 `{id, name, kind, status}` (P9.7).

> Note (Appendix B, Phase 10, group B) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`POST /agents/enrollment-tokens`** (the Owner, step-up, no body) answers 201 `{id, bundle, ca_fingerprint, expires_at}`, the only time the bundle is shown; 404 while `[grpc]` is off, 409 at 20 pending tokens (P10.3's note).

> Note (Appendix B, Phase 10, group C) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **`GET /agents`** (Admin+) pages every agent with its status (`online`, `offline`, `expired`, `disabled`), and **`POST /agents/{id}/disable`** (Admin+, step-up, no body) answers 200 with the agent, disabled for good: every certificate revoked and its stream ended (P10.10's note).

> Note (Appendix B, Phase 10, group E) ([ADR-0019](docs/adr/0019-control-plane-conventions-and-phase-10-refinements.md)): **the bot routes as built** (P11.2's note). `GET /bots` (keyset pages) and `GET /bots/{id}` answer `BotResponse`; `PATCH /bots/{id} {server_address}` (Manage, step-up) and `PUT /bots/{id}/mode {mode_id}` (Control) answer 200 with the bot. `start` and `stop` answer 200; `restart`, `reset` and `resume` 202 when sent to the bot's agent, 200 when a bot that runs nowhere had its state cleared, and otherwise 409 with `bot_unreachable`, `bot_not_running`, `bot_not_paused` or `bot_not_failed`. Only `PATCH` and `PUT` have a body. The answers follow the extractors' order: 401, the per-user 429, 403 `step_up_required`, a malformed path's 404, the body's 400/415/422, an unseen bot's 404, 403, the per-bot 429, 409 (R38).

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

> Note (Appendix C, Phase 11, group A) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), S1, S3): **`BotState.Backoff` gains `bool fresh = 2`**: the next attempt asks for a fresh session (U24). **`BotSpec` gains `ChatRate chat_rate = 7`** (`ChatRate { uint32 interval_secs = 1; uint32 burst = 2; }`): the bot's chat rate, the server's `[rate_limits]` setting. Reading refuses a missing rate, a 0 as missing and a value past its range (60 s, 10 messages) as out of range, never clamped. The wire-schema snapshot shows both; `PROTOCOL_VERSION` stays 1 until the first release (S9).

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

> Note (Appendix E, Phase 11, group A) ([ADR-0020](docs/adr/0020-bot-operations-and-phase-11-refinements.md), U24, S1): **`Backoff{attempt, fresh}`.** A backoff entered from a fresh `AwaitingSession` stays fresh, and `RetryDue` asks as it says, until a session arrives: a refused login's requests never ask without `fresh`. Every other backoff is non-fresh.

> Note (P2.6): The built state machine (`fleet_core::bot`) refines this chart. The changes are listed in ADR-0010:
> - state fields, the `CrashLoop` event and `Stopping{restart}`
> - sticky `Paused` and `Failed`
> - the breaker effects `RecordFailure`, `RecordSuccess` and `ResetBreaker`
> - no "circuit open too long" arrow
