---
paths:
  - "crates/fleet-server/**"
---

# fleet-server
Moved from `CLAUDE.md` word for word; it points here.

## The database file
- **Only `migrate` creates the database outside dev mode** (ADR-0033). `serve` and `user create-owner` open it through `cli::open_database`, which refuses a missing file unless `dev_mode` is on; every other command opens it with `sqlite::open_migrated`, which never creates or migrates. A new command that opens the database takes one of the two, never `sqlite::open`.

## Writes
- **The write pool's `after_release` hook is part of the write path** (`infra/sqlite/commits.rs`): it settles the commit count, and it closes a write connection that comes back still counted as inside a transaction, which SQLite ended by itself (a full disk), so the pool opens a new one; without that, every later `BEGIN IMMEDIATE` on the pool's one connection is refused (ADR-0033). The write pool keeps no lifetime (ADR-0020, S39): sqlx would then close a connection without the hook.
- Writes go through `Store::write()`, a `WriteTx` on the one write connection. Its `commit(entry)` takes the audit entry, so nothing commits unaudited, except through `commit_unaudited(UnauditedWrite)`, whose named exemptions (`SessionTouch`, `SessionRotate`, `LoginAttemptsPurge`, the ingest's `EventBatch`, which writes only chat rows and a held bot's sticky crossing, U17) each have an ADR note, and through the retention task's own transaction, `Store::retention_batch()`, which can only delete, one table per batch, summed up in an hourly `retention.purge` entry (U20), and which only `app::retention` opens (R57); a new exemption needs the user's approval and its own note (ADR-0015, ADR-0016, ADR-0019, ADR-0020). Keep it short: no password hashing, crypto or network calls while it's open (ADR-0015).

**The gRPC port:**
- Its listener bounds each connection's age itself: 80 s, lifted only while an agent's welcomed stream lives. fleet-server's `clippy.toml` bans tonic's server-wide `max_connection_age`, its grace and `Server::timeout`, which would cut agents' streams (ADR-0019, U7).
- An agent's identity comes only from its client certificate's fingerprint and SAN, never from what it sends (ADR-0019).

Server module layout:
```
fleet-server/src/
  main.rs  lib.rs  config.rs  metrics.rs   # metrics.rs: ServerMetrics, every series and its fixed labels (P12.4)
  cli/        # serve, migrate, healthcheck, user create-owner, vault status, vault rotate-key, vault create-key, pki init, pki server-cert, agent create-enrollment-token, agent list, agent disable, agent release, backup, database check
  app/        # services: auth, users, accounts, backup, bots, modes, server_settings, agents (connections, renewal), certificates, enrollment, audit, events, event_tickets, retention, vault
  ports/      # repository and provider traits
  infra/      # sqlite/ (sqlx repos), crypto/ (vault, tokens, passwords), msauth/ (Microsoft's account flows over HTTP), pki/ (the CA, certificates, CSRs)
  http/       # router.rs (the API's router and the metrics port's), middleware/, extractors/, handlers/<resource>.rs, error.rs, openapi.rs, ws.rs
  grpc/       # listener.rs, status.rs, enrollment.rs, control/ (gate, session, grants, outbound, renew), registry.rs, reconciler.rs (the scheduler's choice is fleet-core's placement)
```
