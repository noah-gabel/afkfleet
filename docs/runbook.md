# Runbook

An operator's guide to running afkfleet on a Linux host with Docker, beside whatever else the host runs. It names no hosting provider: where a step depends on yours, it says so in general terms.

> **Work in progress (Phase 12).** This guide grows with Phase 12's groups ([ADR-0022](adr/0022-deployment-and-phase-12-refinements.md)): this first part covers backups, restoring one and the metrics, for the binaries. The install with Docker Compose, keeping a backup off the host, updates, the uninstall and the rest of operations follow, and P12.6 finishes it.

## Backups

The server backs up its database by itself once `[database] backup_dir` is set ([P12.3](../Plan.md); ADR-0022, S2–S5). README's [Running the server](../README.md#running-the-server) lists the keys.

**What a backup is:**
- One SQLite file, `afkfleet-backup-YYYYMMDDTHHMMSSZ.db`, named for the second it was taken (UTC), in `backup_dir`. While it's being written it's the dot file `.afkfleet-backup-YYYYMMDDTHHMMSSZ.db.partial`: never copy that one.
- A consistent copy (`VACUUM INTO`), checked before it counts (this server's migrations, and SQLite's `quick_check`), readable only by the server's user, and never written over another file.
- The `backup_keep` newest are kept (14 by default), one every `backup_interval_hours` (24 by default), at most 720 hours between them.

**What a backup holds:**
- Every secret only sealed: TOTP secrets and Microsoft refresh tokens under the vault's keys, passwords as Argon2id hashes, session tokens as SHA-256 hashes.
- In plain text: usernames, the audit log (with the addresses users came from) and the bots' chat. Chat that the retention deleted lives on in the backups taken before it went, for up to `backup_keep` × `backup_interval_hours`. Treat a backup as private data.
- **Never the keys.** The vault's key files, the CA's key and the server's key are kept apart: the server refuses a `backup_dir` that holds any of them. Back them up yourself, separately and securely (offline, for example). A backup without the vault key that sealed it can't open its secrets.

**Taking one now:** `afkfleet-server backup --config server.toml` takes one beside a running server, or with it stopped. It prints the new file's path alone on stdout, so a script can copy it, and a line about it on stderr. Success looks like exit 0 and one path. Exit 1 says on stderr why nothing was written; exit 3 means the backup was written but older ones couldn't be removed.

**When it fails:** the server logs `a backup failed; the next try is in an hour` at `error`, with the reason (a full disk, a missing or unwritable folder). Fix it, then take one now to check. At the server's start, a `backup_dir` that doesn't exist, isn't a folder, is a link, or holds the database or a key file stops it with exit 1 and the reason.

**Disk:** keep room for `backup_keep` + 1 copies of the database, plus the database's write-ahead log while a backup runs. A long backup also delays the retention's next run by as long as it takes.

**File system:** `backup_dir` must be on a file system with hard links, since a finished backup is published by one, which never replaces a file. Not FAT or exFAT, and not every network share: on one without them, every backup fails with `couldn't publish`.

## Restoring a backup

Restore by hand, with the commands that exist (ADR-0022, S8). The paths below are the config's.

1. **Stop the server.** Success: its last line is `the server stopped`.
2. **Move the live database aside,** with its `-wal` and `-shm` files if they're there: a leftover `-wal` would be replayed onto the backup. Keep them until the restore is checked.
3. **Put the backup in place** as `[database] path`, owned by the server's user and readable only by it (`chmod 600`). On a new host, first bring back the vault's key files and the CA's key and certificate from where you keep them apart, at the paths the config names. Without the CA, every agent has to enroll again.
4. **`afkfleet-server migrate --config server.toml`.** Success: exit 0. A backup from a newer server than this one is refused here: run the version it came from.
5. **`afkfleet-server vault status --config server.toml`.** Success: exit 0. Exit 3 means the backup's secrets need a vault key that isn't configured: a backup from before a key rotation. Put that old key back in `master_key_files`, then follow README's rotation steps.
6. **`afkfleet-server agent list --config server.toml`,** then `agent disable --config server.toml --id <id>` for every agent you disabled after the backup was taken: the backup still has it enabled. An agent enrolled after it isn't in the backup: it's refused at its next connect (exit 5) and enrolls again.
7. **Start the server.** Success: `afkfleet-server healthcheck --config server.toml` exits 0. Then redo in the app what you did after the backup was taken, since the backup brought the old state back: disable users again, end sessions and revoke grants again, and tell users who changed their password or 2FA since that the old ones work again until they change them once more.

After the restore, an agent may still run bots the restored database doesn't give it: they're fenced (`bot.stray`) until the agent removes them, and right after the start nothing new is placed until every enabled agent is back or 90 s have passed.

## Metrics

The server and each agent answer `GET /metrics` in Prometheus' text format once `[metrics] bind` is set ([P12.4](../Plan.md); ADR-0022, S9–S12). README's [Metrics](../README.md#metrics) lists every key metric and its labels.

**Turning it on:** set `[metrics] bind` in the config, or `AFKFLEET_SERVER__METRICS__BIND` (`AFKFLEET_AGENT__METRICS__BIND` for an agent) in the environment, and restart. On a host install, use loopback: `127.0.0.1:9480` for the server, `127.0.0.1:9481` for an agent. Success: the start logs `the metrics endpoint is listening` with the address. Without the key it logs `metrics are off: [metrics] bind isn't set`, and a port that's taken stops the start with exit 1.

**Never publish the port.** It asks for no login. Don't open it in the host's firewall or your provider's, don't publish it with Docker's `ports:`, and don't route it through Caddy or any other proxy. A scraper reads it from the host itself, or from the containers' own network; the compose form comes with the production stack.

**Reading it from the host:**
```sh
curl -s http://127.0.0.1:9480/metrics
```
Success looks like `# HELP afkfleet_…` and `# TYPE …` lines, then one line per series, such as `afkfleet_refresh_token_reuses_total 0`. Every series is there from the start, at 0, and starts again from 0 when the process restarts. Another path answers 404.

**What to watch,** as examples for alerts:
- **Any refresh-token reuse** (`afkfleet_refresh_token_reuses_total` above 0): a refresh token came back after its rotation, and its session was revoked. It may have been stolen: the audit log's `security.refresh_token_reuse` entry shows whose.
- **The last backup older than `backup_interval_hours` plus an hour** (`time() - afkfleet_backup_last_success_timestamp_seconds`): backups fail, and the logs say why (see [Backups](#backups)). It's 0 while no backup exists, and stays 0 with backups off.
- **The retention's last success older than 10 minutes** (`afkfleet_retention_last_success_timestamp_seconds`): it runs every minute, so it fails every run or has stopped; `afkfleet_retention_phase_failures_total` names the phase.
- **Agent connect refusals rising** (`afkfleet_agent_connects_refused_total`): an agent with a revoked, expired or unknown certificate keeps trying, or something probes the gRPC port; the `reason` label says which. A disabled agent's own retries show as `disabled` until it's stopped.
- **Logins refused rising** (`afkfleet_rate_limit_refusals_total{limit="login"}`, `afkfleet_login_lockout_refusals_total`): someone is guessing passwords.
- **On an agent,** `afkfleet_outbox_refused_total` rising: its stream to the server keeps ending on a full outbox, and each new one resyncs.
