# 0002. Hexagonal architecture and crate layout

- **Status:** Accepted
- **Date:** 2026-10-05
- **Related:** Plan.md §2, §3, §4; CLAUDE.md ("Architecture rules")

## Context
The system has three deployables: the server, the agent and the desktop app. They share a domain: bots, accounts, modes, permissions.

The riskiest dependencies sit at the edges:
- azalea, which is nightly-only, supports one Minecraft version per release, and can panic inside its ECS
- the database
- the network protocols
- Tauri

The business rules need to be testable without any of them. Two rules matter most: the bot lifecycle and authorization.

## Decision
**Ports and adapters.**
- Domain logic depends only on traits ("ports").
- Adapters (azalea, sqlx, HTTP, gRPC, Tauri) implement those traits at the edge.
- Every port has a fake in `fleet-testkit`.

**Functional core, imperative shell.**
- The bot lifecycle is a pure function, `transition(&state, event, now) -> Transition { state, effects }`, and the actors only execute the effects.
- Retry policy, `authorize()`, mode validation and mode scheduling are pure in the same way.

**Crates.** Each crate is created in the phase that first needs it.

| Crate | Role | May depend on |
|---|---|---|
| `fleet-core` | Pure domain: IDs, value objects, state machine, policies, RBAC, Minecraft port traits | nothing internal; no IO, no tokio, no azalea |
| `fleet-runtime` | Bot actor, mode runner, watchdog, supervisor (generic over the ports) | `fleet-core` |
| `fleet-mc` | azalea adapter, the **only** crate that depends on `azalea` | `fleet-core`, `azalea` |
| `fleet-proto` | `.proto` files, generated code, core ⇄ proto conversions | `fleet-core` |
| `fleet-api-types` | HTTP/WS DTOs, exported to TypeScript | `fleet-core` |
| `fleet-agent` (bin) | Agent: config, wiring, control-plane client | `fleet-core`, `fleet-runtime`, `fleet-mc`, `fleet-proto` |
| `fleet-server` (bin) | HTTP API, auth, vault, persistence, gRPC control plane, CLI | `fleet-core`, `fleet-proto`, `fleet-api-types`, `azalea-auth` |
| `fleet-client` | Typed HTTP + WS client | `fleet-api-types` |
| `apps/desktop/src-tauri` | Tauri backend | `fleet-client`, `fleet-api-types` |
| `fleet-testkit` | Fakes, fixtures, builders (dev-dependency only) | `fleet-core` |

**Thin binaries.** `main.rs` parses the CLI and config and wires adapters together; all logic lives in the library part.

**Enforcement.** The dependency rules are checked in review and, where possible, with `cargo tree`. For example, `cargo tree -p fleet-core` must show no tokio or IO crates, and `cargo tree -i azalea` must show only `fleet-mc`.

## Consequences
- **Deterministic tests.** The domain and runtime are tested with fakes and paused time, without a Minecraft server or a database.
- **Swappable edges.** Replacing an edge means writing one adapter: a process-per-bot connector, a PostgreSQL repository, a web `ApiClient`.
- **Nightly is contained.** Only `fleet-mc` and the crates above it need nightly Rust. Everything else must also build on stable, which the `stable-check` CI job enforces.
- **Cost.** More crates and traits than a single binary would need, and conversions at every boundary (core ⇄ proto ⇄ DTO).

## Alternatives considered
- **One crate per binary with modules inside.** Fewer files, but nothing stops IO from leaking into the domain, and azalea's nightly requirement would spread to everything in the same crate.
- **Shared mutable state instead of actors and pure transitions.** Simpler at first, but harder to test, and one bot's fault can affect the others (Plan.md §6).
