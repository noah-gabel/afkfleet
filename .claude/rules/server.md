---
paths:
  - "crates/fleet-server/**"
---

# fleet-server
Moved from `CLAUDE.md` word for word; it points here.

## Writes
- Writes go through `Store::write()`, a `WriteTx` on the one write connection. Its `commit(entry)` takes the audit entry, so nothing commits unaudited, except through `commit_unaudited(UnauditedWrite)`, whose named exemptions (`SessionTouch`, `SessionRotate`, `LoginAttemptsPurge`, the ingest's `EventBatch`, which writes only chat rows and a held bot's sticky crossing, U17) each have an ADR note, and through the retention task's own transaction, `Store::retention_batch()`, which can only delete, one table per batch, summed up in an hourly `retention.purge` entry (U20), and which only `app::retention` opens (R57); a new exemption needs the user's approval and its own note (ADR-0015, ADR-0016, ADR-0019, ADR-0020). Keep it short: no password hashing, crypto or network calls while it's open (ADR-0015).

**The gRPC port:**
- Its listener bounds each connection's age itself: 80 s, lifted only while an agent's welcomed stream lives. fleet-server's `clippy.toml` bans tonic's server-wide `max_connection_age`, its grace and `Server::timeout`, which would cut agents' streams (ADR-0019, U7).
- An agent's identity comes only from its client certificate's fingerprint and SAN, never from what it sends (ADR-0019).

Server module layout:
```
fleet-server/src/
  main.rs  lib.rs  config.rs
  cli/        # serve, migrate, healthcheck, user create-owner, vault status, vault rotate-key, pki init, pki server-cert, agent create-enrollment-token, agent list, agent disable, agent release, backup
  app/        # services: auth, users, accounts, bots, modes, server_settings, agents, enrollment, audit, events, event_tickets, retention, vault
  ports/      # repository and provider traits
  infra/      # sqlite/ (sqlx repos), crypto/ (vault, tokens, passwords), msauth/ (Microsoft's account flows over HTTP), pki/ (the CA, certificates, CSRs)
  http/       # router.rs, middleware/, extractors/, handlers/<resource>.rs, error.rs, openapi.rs, ws.rs
  grpc/       # listener.rs, status.rs, enrollment.rs, control/ (gate, session, grants, outbound), registry.rs, reconciler.rs (the scheduler's choice is fleet-core's placement)
```
