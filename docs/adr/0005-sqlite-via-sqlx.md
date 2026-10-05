# 0005. SQLite via sqlx

- **Status:** Accepted
- **Date:** 2026-10-05
- **Related:** Plan.md §1 (scale), §5, Phase 6, Appendix D; CLAUDE.md, security rule 8

## Context
The server stores users, sessions, encrypted credentials, the desired state of every bot, modes, chat history and the audit log. The scale is small: a handful of users, up to about 50 bots per agent, one server instance. Production is one Linux host with Docker Compose. Operating effort matters more than horizontal scalability.

## Decision
We use **SQLite** through **sqlx 0.9**.
- **Pragmas:** `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5s`.
- **Pools:** a write pool with **one** connection, so SQLite writes are serialized explicitly instead of failing with `SQLITE_BUSY`; a separate read pool.
- **Queries:** only `sqlx::query!` / `query_as!` with bind parameters. They're checked at compile time against the schema, with offline data committed in `.sqlx/` and verified by CI (`cargo sqlx prepare --check`). SQL is never built from strings.
- **Migrations** are embedded with `sqlx::migrate!` and run at startup (and through `afkfleet-server migrate`).
- **Isolation:** access goes through repository traits (ports). Services own transactions.
- **Backups:** `VACUUM INTO` on a schedule plus a `backup` CLI. Secrets in backups are only ciphertext, because the vault (Phase 9) encrypts them before they're stored.

## Consequences
- **No database server to run.** The whole state is one file on a volume; backup and restore are file operations.
- **Fast, realistic tests.** `#[sqlx::test]` gives every test a fresh database with migrations applied.
- **One writer.** Write throughput is limited to one connection. Agent events are therefore batched (about every 500 ms, P10.8), and there are no per-tick writes.
- **No horizontal scaling.** The server can't scale out. A PostgreSQL repository implementation is the planned way out (Plan.md §10); the repository traits keep it contained.

## Alternatives considered
- **PostgreSQL.** More capable and scalable, but one more stateful service to run, secure and back up, with no need at this scale.
- **An ORM (Diesel, SeaORM).** More abstraction than needed. sqlx's checked raw SQL keeps queries explicit and reviewable.
