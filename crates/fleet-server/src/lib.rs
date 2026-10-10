//! The afkfleet server (Plan.md §2, Phase 6 on): users, roles and sessions,
//! encrypted Minecraft credentials, the desired state of every bot, modes,
//! chat history, the audit log, and the agents' control plane.
//!
//! # Layout
//! The library holds all the logic; `main.rs` (P6.11) only parses the
//! command line and the config and wires the adapters together.
//!
//! | Module | Holds | From |
//! |---|---|---|
//! | `config` | `server.toml` plus `AFKFLEET_SERVER__…` environment variables | P6.2 |
//! | `cli` | the commands: `serve`, `migrate`, `healthcheck`, later `user create-owner`, `pki`, `vault rotate-key`, `backup` | P6.11 |
//! | `app` | the services: one use case each, with its transaction and audit entry | P6.8 |
//! | `ports` | the repository and provider traits the services depend on | P6.4 |
//! | `infra` | the adapters: `system` (the real clock and the OS's randomness), `sqlite` (sqlx repositories), `crypto` (vault, tokens, passwords), `msauth` (azalea-auth) | group B, P6.3, P7, P9 |
//! | `http` | the router, middleware, extractors, handlers per resource, the error type and the WebSocket | P6.5 |
//! | `grpc` | enrollment, the control stream, the agent registry, the scheduler and the reconciler | P10 |
//!
//! Each module arrives with the task that first puts code in it (ADR-0015).
//!
//! # Layering
//! Calls go one way: `http` handlers → `app` services → `ports` → `infra`.
//! - **Handlers** only parse input, authorize, call a service and map the
//!   result: no business logic and no SQL.
//! - **Services** own the use cases, their transactions and their audit
//!   entries.
//! - **Ports** are traits; services hold them as `Arc<dyn …>`, so tests can
//!   swap the adapters.
//! - **`infra`** implements the ports. SQL lives only there, through sqlx's
//!   checked macros with bind parameters.

pub mod config;
pub mod infra;
pub mod ports;
