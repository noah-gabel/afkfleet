# 0022. Deployment and operations: Phase 12 refinements

- **Status:** Accepted
- **Date:** 2026-11-05
- **Related:** Plan.md Phase 12 (P12.1–P12.7), §6, §7, Appendix A; ADR-0014, ADR-0015, ADR-0016, ADR-0017, ADR-0018, ADR-0019, ADR-0020

## Context
Phase 12 makes afkfleet deployable and distributable:
- the server's and the agent's images (P12.1);
- a compose setup (P12.2);
- backups (P12.3);
- metrics on an internal port (P12.4);
- the Windows installer with its updater (P12.5);
- the runbook (P12.6);
- a nightly compose smoke test (P12.7).

It also carries the user's finding from Phase 11's 24-hour run. The start screen's "Waiting for the server…" offers no way out, so a server that moved or is gone for good leaves the app stuck (ADR-0020, "The DoD's 24-hour run").

The plan read the code first:
- No server image, production compose file, backup or `docs/runbook.md` exists yet.
- The agent's image exists (P5.6). The server has no metrics.
- The start screen's other errors already offer "Try again", "Server settings" and "Log out". `logout` already ends the session in the app before it tries the server.

Every open question went to its own read-only review agent, given the options neutrally (CLAUDE.md's step 10). The user decided the phase's frame, their privacy, the backups' destination, the ports, the DoD's install and one UX point. They approved the review agents' decisions (S1–S8) at the plan review. Each later group asks its own questions in its session and adds its decisions here.

## Decision
### Grouping *(proposed in the plan, approved by the user)*
Features come first, then what packages them, then the release. There is one branch and one PR per group, each after the previous PR is merged. Each group aims at about 5,300 lines; one that grows past that is split at its plan review.

| Group | Branch | Tasks |
|---|---|---|
| A | `p12/p12.3-start-screen-and-backups` | The start screen's way out (the user's finding), then P12.3: the backup file, its schedule beside the retention, `backup`, `journal_size_limit`, the restore steps, `docs/runbook.md` started (ticks P12.3) |
| B | `p12/p12.4-metrics` | P12.4: the endpoint on an internal port in the server and the agent, and the key metrics of every P12.4 note, the backups' among them (ticks P12.4) |
| C | `p12/p12-stack` | P12.1: the server's image, the agent's labels, arm64 (amd64 if it's cheap), the image scan. P12.2: the compose project beside other services (U4), optional Caddy and the existing-proxy path (U5), the backups' host side for the SFTP pull (U3), U8's setup script. P12.7: the nightly smoke test of both paths (ticks P12.1, P12.2, P12.7) |
| D | `p12/p12.5-release` | P12.5: the NSIS installer, the updater, the release workflow on GitHub Releases, where the images are published. P12.6: the runbook finished (U4, U8). The DoD's kit (ticks P12.5, P12.6) |

- **New dependencies** arrive with their group. Each is shown to the user with its version, licence and release date, checked again at that group's planning. Today's figures:

  | Dependency | Group | Version, licence, date |
  |---|---|---|
  | `tauri-plugin-updater` | D | 2.14.1, Apache-2.0 OR MIT, 2026-10-28 |
  | Caddy's official image | C | 2.11.8 (`2.11.8-alpine`), Apache-2.0, rebuilt 2026-11-05 |
  | cargo-chef | C | already pinned at 0.1.78 (Apache-2.0 OR MIT, 2026-08-12) since P5.6 |
  | a metrics crate | B | none new: `metrics` and `metrics-exporter-prometheus` are in §5 for the server already |

  Group A adds none.
- **Who decides what.** The review agents decide each group's open questions. The user decides:
  - U50 for production, asked at C's planning, where the production agent config is written;
  - every dependency, the image scanner included (C);
  - CI permissions for publishing releases and images, and any CI secret (D);
  - the texts friends read (one table per group plan), looks and UX.

  The first release tag is the user's, and it freezes the protocol (ADR-0019, S9).
- **The DoD's steps** are the user's, on their own host:
  - the clean install by the runbook (U6), once D is merged and the first release exists;
  - then a friend's check against it.

  The AI never touches the host.

### The user's choices
- **U1. The phase's frame** *(the user's decisions, as amended during the planning)*:
  - **No paid services** are required.
  - **Images** are built for linux/arm64 in CI on GitHub's arm runners, and for amd64 too if it's cheap.
  - **HTTPS** comes from Let's Encrypt over the HTTP-01 challenge on ports 80 and 443, with no Caddy plugin, for a domain name that points at the host; a free dynamic-DNS subdomain works. The runbook keeps such a record in step with the host's public IP: an update after a change, its token kept on the host only, never in the repository, in CI or on a command line. The update reads its URL from a root-only file.
    - *Fallback, recorded and not built:* Let's Encrypt's IP-address certificates (the shortlived profile, available since January 2026), if the DNS name goes away.
    - A DNS-01 setup or a custom Caddy build would be a new dependency, and the user's question.
  - **The installer is unsigned:** there is no paid Authenticode certificate. README and the runbook tell friends what Windows' "Windows protected your PC" means and how to continue ("More info", "Run anyway").
  - **Updates:** the Tauri updater still signs its updates with its own key pair, whose private key the user keeps offline. The update manifest and files live on GitHub Releases.
  - **Metrics** stay on an internal port, with no paid monitoring.
- **U2. Privacy and hard limits** *(the user's rules)*:
  - **The user's hosting stays private.** Nothing in the repository, the ADRs, the docs, PRs, commits or CI names the user's provider, host, address or DNS name, or says they use any particular cloud. Provider-specific steps appear only generically ("open 80 and 443 in your provider's firewall and the host's"). CLAUDE.md's security rule 11 carries this.
  - **The AI never connects to the host.** It never sees or handles its login, SSH keys, DNS token, vault key, CA key or the updater's private key, and none of them goes on a command line, into the repository or into CI. Every step on the real host and with real keys is the user's, from the runbook, as with the DoD runs.
  - **CI secrets** are used only where a step truly needs one, and each is the user's question first.
  - **No public servers yet:** no production configuration lets an agent dial public Minecraft servers until the user decides it (ADR-0020's U50, now for production).
- **U3. Backups leave the host** *(the user's answer to the plan's question)*: they are pulled to an operator's PC over SFTP. The PC copies from an account on the host that can only read the backups. There is no third party and no credential on the host. Group C builds the host side, and D documents it.
- **U4. afkfleet installs beside other services** *(the user's rule)*:
  - The runbook is an operator's guide for any Linux host with Docker, not a guide to one server.
  - afkfleet has its own compose project, network and named volumes, and no host networking.
  - It changes nothing on the host beyond what the runbook lists. An uninstall section removes everything it added and nothing more.
- **U5. Ports 80 and 443: both paths** *(the user's answer)*.
  - Caddy is an optional part of afkfleet's compose project, for hosts where 80 and 443 are free.
  - Otherwise afkfleet runs behind an existing reverse proxy. The runbook says what that proxy must do: TLS, WebSocket upgrades for the live events, X-Forwarded-For with `[http] trusted_proxies` set to its address, HSTS, and no logging of the events' `ticket` or the grant search's `q` (ADR-0020, S28, S127).
  - The smoke test runs both paths.
- **U6. The DoD's install** *(the user's change to Phase 12's DoD)*. "A fresh VPS gets to a running system by following only the runbook" becomes: **a clean afkfleet install (no earlier afkfleet state) on an existing Linux host that keeps its other services, following only the runbook; then the friend's check against it.**
- **U7. "Log out on this PC" asks nothing first** *(the user's answer)*, like every other Log out in the app.
- **U8. A short first install** *(the user's addition at the plan review, for C and D)*:
  - At most about ten commands from a host with Docker to a running afkfleet.
  - One setup script does the one-time steps. It checks the prerequisites, creates the vault key and the PKI with the right owners and modes, refuses to overwrite anything that exists, and prints what to do next.
  - An update is documented as two commands, and every step says what success looks like.
  - The AI writes the script and its tests and never runs it on a real host. Group C decides its form.
- **Approved at the plan review:** the four groups, S1–S8, and group A's texts as drafted. The waiting screen's buttons are the user's own "Server settings" and "Log out on this PC". A logout the server didn't confirm shows the existing shared warning ("Logged out here. Server not told ({reason}): the session may stay valid until it expires."), and one it confirmed shows nothing. README's line on the app's start is corrected to match.

### Decided by the review agents *(approved by the user at the plan review)*
One read-only agent per question, each given the options neutrally.

- **S1 (security). "Log out on this PC" reuses `logout` and `useLogout`.**
  - **How it works:** fleet-client's `logout` clears the stored refresh token under its gate before any request, then tries the server once. An unreachable server answers `unconfirmed`, whose warning says the server wasn't told; a server that answers meanwhile is told. This is ADR-0017's "ends locally first, then revokes, best effort".
  - **The wait:** on the start screen the app holds no access token, so the logout sends one refresh. The button stays busy about 5 s for a host that drops packets (`CONNECT_TIMEOUT`) and about 20 s for a silent server (`REQUEST_TIMEOUT`); about 40 s is the worst case.
  - **What the build needs:**
    - While a logout runs, "Server settings" is disabled too, on the waiting screen and in the failed state. Otherwise the logout's `leaveSession` would move a user who went to `/setup` on to `/login`.
    - The e2e fake's `refuseAll` answers `logout` with `{ server: "unconfirmed", error }`, as the real command does, instead of rejecting it.
  - *Rejected:*
    - A new command over `discard()`: it needs a capability grant, the user's approval under rule 10, and it never revokes even when the server answers.
    - A new argument on `logout`: it changes the IPC contract, and a shorter bound breaks `REQUEST_TIMEOUT`'s documented reason (ADR-0017 rejected configurable timeouts).
- **S2 (security). One backup file.**
  - **The statement:** `sqlx::query!("VACUUM INTO ?", path)` with the path bound, so security rule 8 holds. sqlite.org allows any expression after `INTO`, and sqlx 0.9 describes a statement without columns by counting its parameters.
  - **The connection:** a read-only connection opened just for the backup, through `connect_options` with the shared pragmas.
    - The read pool's `query_only` refuses the new file's write transaction.
    - The write connection would hold the one writer for minutes.
    - SQLite's `VACUUM INTO` takes only a read lock on the source and runs on a read-only connection.
  - **The file:**
    1. Created empty with `create_new` and mode 0600 (SQLite would create it 0644 minus umask).
    2. Filled by `VACUUM INTO` and synced.
    3. Checked on a read-only connection: the schema check `open_migrated` uses, and `quick_check` answering exactly "ok".
    4. Published with a hard link, which never overwrites. The temporary file is then removed and, on Unix, the folder synced.

    A failure or a full disk removes the temporary file. `backup_dir` is absolute, since sqlx accepts URI file names.
  - *Rejected:*
    - a fixed literal: the dated name needs formatting, which rule 8 forbids;
    - SQLite's backup API: `unsafe` or a new crate;
    - the read pool with `query_only` switched off: SQL outside the macros on a shared connection;
    - no check: a bad copy could push a good one out.
- **S3 (security, low). The schedule runs inside the retention task's loop.**
  - **The order:** each tick runs the retention's pass, then its summary if due, then a backup if due. One loop orders them with no lock, so a backup's read snapshot never overlaps a retention batch (P12.3's note, S44, S45), and each backup follows a finished pass.
  - **Due:** a backup is due when the newest finished backup's name stamp is older than `backup_interval_hours`, so a restart neither skips nor doubles one. The first comes about a minute after a start. A stamp more than the retention's clock slack in the future doesn't count, with one `warn`.
  - **The stop** cancels a running backup through sqlx's progress handler (also bounded by time) and removes its temporary file. Waiting could overrun ADR-0015's shutdown deadline.
  - **A failed backup** is one `error` line, tried again after an hour, not every minute.
  - **The cost:** a long backup delays the next retention pass, so chat overshoots its bounds by the ingest's rate × the backup's time. The runbook's disk sizing says so.
  - *Rejected:*
    - a lock shared by two tasks: two waits that must each race the stop, for the same order;
    - independent schedules: the WAL grows under the snapshot;
    - backups every interval from the start: a server that restarts more often never backs up;
    - a fixed hour of the day: a server down at that hour skips the day.
- **S4 (security). The config, the names and the pruning.**
  - **Off unless configured:** backups are off unless `[database] backup_dir` is set. Outside dev mode, `serve` warns once at startup that backups are off.
  - **The keys:**
    - `backup_interval_hours`: 1–168, default 24.
    - `backup_keep`: 1–60, default 14.
    - Their product is at most 720 h, chat's 30-day ceiling.
    - Either key without `backup_dir` is a problem at that key.
  - **`backup_dir`** is absolute, with no `..`. It is neither the database's folder nor one above it, and no vault key file, CA key or server key lies inside it. That enforces "back up the vault key separately".
  - **At start,** `serve` and `backup` check the folder on disk: it exists, is a folder and isn't a link. It's never created, for ADR-0015's reason (an unmounted volume).
  - **Keeping:** by count, pruned only after a success. Deleted chat then lives in the host's backups for at most keep × interval: 14 days by default, 30 at most. Off-host copies are the operator's.
  - **Names:** `afkfleet-backup-YYYYMMDDTHHMMSSZ.db` from the Clock's UTC second, parsed exactly, so name order is time order.
  - **Pruning:**
    - It lists only the folder itself, with `symlink_metadata`.
    - Only regular files whose whole name matches count or are deleted. Links, folders and other names are never touched (ADR-0020, R130).
    - It never deletes the file just written.
    - CLI backups count toward the kept set.
  - *Rejected:*
    - on by default, which would need a container path as default;
    - required, which would force backups in dev and tests;
    - keeping by age: failures would age out good backups;
    - ordering by modification time: a copy changes it.
- **S5 (security). `afkfleet-server backup --config …`** takes one backup now into `backup_dir`, through the same service, named and pruned the same way. It works beside a running `serve`.
  - **It refuses** (exit 1) a missing `backup_dir`, a missing database or pending migrations, and creates or migrates nothing: a forgotten volume must not become a "successful" empty backup.
  - **It never reads the vault keys.**
  - **Output:** stdout holds the new file's absolute path alone, which a test pins. stderr holds one line with its size, the count pruned, and that the keys aren't in it.
  - **Exit codes:** 0 written; 1 nothing written; 2 usage error; 3 written, but pruning failed.
  - *Rejected:* `--output` for a path elsewhere. A copy outside the kept set has no bound for deleted chat, and a whole database could land anywhere. It can be added later without breaking a script.
- **S6. A backup writes no audit entry.**
  - **Why:** `VACUUM INTO` writes nothing to the database, ADR-0016 ties audit actions to writes, and the commands that don't write (`pki init`) aren't audited. A merged action name could never be dropped.
  - **The logs:**
    - one `info` line per finished backup, with `file` (the name), `bytes`, `duration_ms` and, for a scheduled one, `pruned`;
    - an `error` line for a failed one, since a human must act (a full disk, a missing volume);
    - a `warn` line for a failed prune;
    - an `info` line for one the stop cut short.
  - **Accepted risk** (threat model): a backup taken on the host leaves no audit entry, since whoever runs the binary there holds the file anyway.
- **S7 (security, low). `PRAGMA journal_size_limit = 0` on every connection,** beside `secure_delete`.
  - **What it does:** after each reset, SQLite cuts the `-wal` file back. A WAL that grew while a backup held its snapshot no longer keeps its size until the next restart, together with the old page images of deleted chat beyond the part that gets reused.
  - **Its cost:** one truncation per cycle.
  - The threat model's "What a delete leaves" and S44's "What lingers" are corrected.
  - *Rejected:*
    - leaving it unset, today's leftover;
    - a `TRUNCATE` checkpoint after each run: it blocks writers, against S45's sharing of the write connection, and needs a new path onto it.
- **S8 (security). Restoring is done by the runbook's steps, with the commands that exist.**
  - **The steps:**
    1. Stop the server.
    2. Move the database aside with its `-wal` and `-shm` (a leftover `-wal` would be replayed onto the backup).
    3. Put the backup in place, owned by the server's user, mode 600, after bringing back the vault key files and the CA pair.
    4. Run `migrate`.
    5. Run `vault status`: exit 3 means a backup from before a rotation.
    6. Run `agent list`, then `agent disable` for each agent disabled since the backup (ADR-0019, S27).
    7. Start the server, then redo the security actions taken since the backup: users disabled, sessions and grants revoked, passwords changed, 2FA replaced.
  - **The docs:** group A starts `docs/runbook.md` with "Backups and restore", at the binary level. C adds the compose form and the SFTP pull, and D finishes the runbook. README carries `backup`'s reference and links there.
  - *Rejected:*
    - a `restore` command: destructive, and it can't prove the server is stopped;
    - a check command: its one new check was integrity, which the backup's own check covers now.

### The plan's reconciliations of the agents' answers
- **R1. The temporary file** is `.afkfleet-backup-YYYYMMDDTHHMMSSZ.db.partial`: a dot file, so a copier taking `afkfleet-backup-*.db` never takes it. An attempt removes only its own temporary file. Others go through pruning once their stamp is 24 h old, so `serve` never deletes a temporary file the CLI is writing at that moment. (S3 had proposed removing leftovers before each attempt.)
- **R2. A same-second clash** is refused: the hard link fails on an existing name, and nothing is overwritten.
- **R3. Rule 8 for the two PRAGMA reads.** The backup's `quick_check` and the test of `journal_size_limit` are read through sqlx's macros. If sqlx can't describe either, the work stops and the user decides. ADR-0015's test exception names `journal_mode` alone.
- **R4. For later groups:**
  - **P12.4:** the backups' metrics: last success, failures, duration, bytes. The last success is read at start from the newest name's stamp, since CLI backups run in another process.
  - **P12.6:**
    - Disk sizing is (keep + 1) × the database's size, plus the WAL's peak during a backup.
    - Take a backup with the old server before an upgrade, since a new image refuses pending migrations.
    - The key rotation's "copy the database" becomes `backup`.
  - **Group C:** the image creates the backups' folder owned by 65532. The SFTP account reads files that are 0600 and owned by 65532.

### Notes for later groups
- **B (P12.4):** the endpoint (the exporter's `http-listener` feature or a route of the server's own) and its config keys are B's questions, with the metrics every P12.4 note lists and R4's backup metrics.
- **C (P12.1, P12.2, P12.7):**
  - **Its questions:** U50 for production; the image scanner (a dependency); Caddy's image; resource limits from the measured numbers (P12.2's notes); the SFTP account's host side (U3); U8's setup script's form.
  - **U4:** its own project, network and named volumes, no host networking.
  - **U5:** both paths, each in the smoke test.
  - **The backups:** the backups' folder on a volume, `backup_dir` set in the production config, and the smoke test taking a backup across a restart.
  - Group C may be split at its plan review.
- **D (P12.5, P12.6):**
  - **Publishing:** the release workflow and where images are published, whose CI permissions are the user's question.
  - **The installer:** unsigned, with the SmartScreen text (U1).
  - **The updater:** its public key in the config, its private key never in CI.
  - **The runbook,** finished for any Linux host:
    - the install in about ten commands and updates in two (U8);
    - generic dynamic-DNS and firewall steps (U1, U2);
    - the uninstall (U4);
    - the restore steps (S8);
    - every P12.6 note.
  - **The first release tag** is the user's.

## Consequences
- **Easier:**
  - A server that moved or is gone no longer traps the app at its start.
  - The database can be copied off the host as plain SQLite files, bounded in number and in how long they keep deleted chat, without stopping the server.
  - A WAL that grew no longer keeps its size or its old pages until a restart.
- **Harder:**
  - The backup file's name is a contract: the copier, the runbook and the pruning depend on it.
  - A long backup delays the retention's next pass.
  - Restoring stays a manual procedure, with its security steps in the runbook.

## Alternatives considered
- **The stack (P12.1, P12.2, P12.7) as group A:** it would have put the infra questions and U50 into this planning, before the features it packages exist.
- **Backups and metrics in one group:** about 6,000 lines, over the size target.
- The rejected options of S1–S8, each listed with its decision above.
