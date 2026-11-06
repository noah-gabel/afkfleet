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
  - *The server setup's way back* *(the user's decision at PR #95's hands-on)*. Opened from the start screen, "Server settings" used to lead back to the login while this PC still held the session, and nothing led back to the start screen. Now, leaving the setup without saving, when the start screen opened it, goes back to the start screen. That runs the launch again: it waits again while the server doesn't answer, or opens Home once it does. The setup's link then reads "Back" (the user's text), not "Back to login". Saving a new server keeps its behaviour: the switch logs out first.
- **U8. A short first install** *(the user's addition at the plan review, for C and D)*:
  - At most about ten commands from a host with Docker to a running afkfleet.
  - One setup script does the one-time steps. It checks the prerequisites, creates the vault key and the PKI with the right owners and modes, refuses to overwrite anything that exists, and prints what to do next.
  - An update is documented as two commands, and every step says what success looks like.
  - The AI writes the script and its tests and never runs it on a real host. Group C decides its form.
- **U9. Every subagent is read-only, and checked** *(the user's rule at group B's plan review)*. During group B's planning, the repository's `.config/` folder (nextest's config) was moved into `.claude/` when the first review agent started. Neither the user nor the main session moved it; it was moved back unchanged before the branch was made. From then on:
  - every subagent's brief says it is read-only: it creates, edits, moves or deletes no file, and runs no command that writes or changes git state;
  - the session notes `git status --short` with the untracked files before each subagent starts, and compares them after it finishes;
  - any change the session didn't make stops the work and goes to the user.

  CLAUDE.md's step 10 and `.claude/agents/Explore.md` carry it.

  *Tightened at group C's planning (the user's rule, U11, 2026-11-06):* `git status --short` misses an edit to a file that's already modified, new files inside an untracked folder, and ignored paths. The comparison is now `node scripts/tree-fingerprint.mjs`, run before and after every subagent: git's HEAD, refs, stash, local config and index, tracked changes by content (`git diff HEAD --binary`), every untracked file by content hash, and the ignored paths outside `target/` and `node_modules/` by path, size and mtime from lstat, never opened, so nothing under `secrets/` is read and nothing under `target/` (`target/dod-run` included) is even listed. It can't see a change inside those two folders. The briefs' wording follows Explore.md's: no command that "writes, installs, builds or changes git state". CLAUDE.md's text is the user's, as chosen at the planning.
- **Approved at group B's plan review** *(2026-11-05)*: S9–S12, R5–R8, U9, and PR #95's follow-ups as the group's first commits.
- **Approved at the plan review:** the four groups, S1–S8, and group A's texts as drafted. The waiting screen's buttons are the user's own "Server settings" and "Log out on this PC". A logout the server didn't confirm shows the existing shared warning ("Logged out here. Server not told ({reason}): the session may stay valid until it expires."), and one it confirmed shows nothing. README's line on the app's start is corrected to match.

### Decided by the review agents *(approved by the user at the plan review)*
One read-only agent per question, each given the options neutrally.

- **S1 (security). "Log out on this PC" reuses `logout` and `useLogout`.** *("Server settings"' way back from the setup is U7's, the user's decision at PR #95's hands-on.)*
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

- **S9 (security). The endpoint's form** *(group B)*.
  - **The server:** a second axum listener of `serve`'s own, serving only `GET /metrics` through the route registry:
    - `public_route(RateClass::Standard)` on a `Routes` of its own, behind the API's middleware (the timeout, the per-IP limit, request IDs, the error envelope);
    - never on the API's router, and never in its OpenAPI document;
    - it renders the `PrometheusHandle` of the exporter's recorder, with no exporter features;
    - `serve` runs it beside the API's port, so it stops gracefully within ADR-0015's deadline.
  - **The agent:** a minimal HTTP/1.1 responder of its own over tokio's `TcpListener`, on the run's `JoinSet` with a child token. ADR-0015's health check is the precedent for minimal HTTP code of our own.
    - **Limits:** 4 connections at once (one more is closed at once), 5 s per connection, a request head of at most 2 KiB, and a 1 s pause after an accept error that isn't one connection's.
    - **Answers:** only `GET /metrics` gets 200 (`text/plain; version=0.0.4`). Another path is 404, another method 405, a malformed or oversized request 400, each with `Connection: close`.
    - Nothing from a request is logged.
  - **Rules 1 and 3** hold on the server as they do for `/health/*`: the metrics router's allowlist test, `/metrics` answering 404 on the API's port, and tests for 404, 405 and 429. With no input and no credentials, 401, 403 and 422 don't apply.
  - *Rejected:*
    - **The exporter's `http-listener`:** it spawns unowned tasks (its upkeep, every connection), has no header or idle timeout and no connection cap, can't stop gracefully, retries a failed accept in a hot loop, and answers on every path. It would need a new feature and an exception to the async rules.
    - **axum in the agent:** it adds axum, axum-core and matchit.
    - **The agent's metrics over gRPC:** standalone agents have no server.
- **S10 (security). `[metrics] bind`, off unless set.**
  - **The key:** both binaries gain an optional `[metrics]` section with one key, `bind`, and refuse unknown keys. The overrides are `AFKFLEET_SERVER__METRICS__BIND` and `AFKFLEET_AGENT__METRICS__BIND`, and the validated config holds `metrics: Option<SocketAddr>`.
  - **Binding:** `main.rs` binds the listener and hands it to the run, as for `[http]` and `[grpc]`.
  - **One `info` line at startup:** "the metrics endpoint is listening", with the address, or "metrics are off: [metrics] bind isn't set". The precedent is "the control plane is off"; it isn't a `warn`, since nothing is lost.
  - **Validation:** a `SocketAddr` whose port isn't 0. On the server, its port also differs from `[http] bind`'s and `[grpc] bind`'s resolved port, whatever the hosts (`SamePortAs`), which catches `0.0.0.0` against `127.0.0.1` and dual-stack `[::]`. Any IP address is allowed, since containers need `0.0.0.0`.
  - **Constants, not keys:** the path, the cap, the timeout and the head's size. No allowlist and no token.
  - **The dev configs** gain a commented `# [metrics]` block: `127.0.0.1:9480` for the server, `9481` for agents, clear of 9090, 9091, 9093 and 9100. Compose, the Dockerfile and `deploy/dod/` are unchanged.
  - *Rejected:*
    - **On by default at loopback:** a container's loopback can't be reached from outside, a shared host's port may be taken, and the binaries' parallel tests would race for it.
    - **The section's presence turning it on:** two ways to say one thing.
    - **`[http] metrics_bind`:** the agent has no `[http]`.
    - **Refusing `0.0.0.0`, or warning on it.**
    - **An allowlist or a token:** either can be added later without breaking a config.
- **S11 (security). Totals only, fixed labels, counted where each event happens.**
  - **No per-agent, per-bot or per-user label.** The ingest note's "per agent" stays in housekeeping's minute line, which names the agent, and each agent's own endpoint is its own scrape target.
    - *Rejected: an `agent_id` label.* It's bounded and not personal, but a disabled agent's series would stay until a restart, and every re-enrollment adds a set. ADR-0013's "no `bot_id`" is the precedent.
    - *Rejected: an agent-name label:* text an operator typed.
  - **Label values:** each a `&'static str` from an exhaustive match, through typed recording methods, never a `&str` argument. The sources are `ConnectRefusal::as_str`, `Unplaced::as_str` (through `reason()`), `Phase::name`, and new `Limit` and `NotTaken` names.
  - **Every series is registered at 0** at startup, and a test pins the label keys and every value to `^[a-z][a-z0-9_]*$`.
  - **Counting points:**
    - rate limits at the refusal, not from the minute's summary;
    - conflicts at every detection, before the once-an-hour filter;
    - connect refusals before the audit cap;
    - the fresh-session limit only as `limit="fresh_sessions"` (R43), and `undelivered` isn't a denial;
    - a successful retention run is one with no failed phase that wasn't stopped.
  - **Added beyond the notes:** the lockout's refusals, and S28's dropped reports.
  - **Left out, addable later without breaking anything:** failures before `Welcome`, sticky-state writes, the token service's error kinds.
- **S12 (security). The server injects its metrics.** `ServerMetrics` is cheap to clone, holds one handle per series, has typed methods with exhaustive matches, and registers every series at 0 in the recorder it's built over.
  - `ServerPorts` gains `metrics`, built over its own `PrometheusBuilder::new().build_recorder()`, whose handle the metrics port renders. Nothing is installed globally.
  - The services `serve` builds take it, and the widely tested ones gain `with_metrics`, like `with_live_hints`, defaulting to a no-op. The `backup` command gets none, so by type it records nothing.
  - **Why:** each in-process server has its own registry, and a restart starts at 0; counts on any thread reach the test; an unregistered series can't be counted.
  - **The agent keeps the global facade:** `main.rs` keeps the handle `install_recorder()` returns and hands it, with the bound listener, to the run. Without histograms no upkeep is needed, and `idle_timeout` is never set, since it would drop the series at 0.
  - *Rejected:*
    - **The global facade on the server:** counts on worker threads are invisible to a thread-local test recorder, the in-process servers would share one registry, and a missing label shows only at its first event.
    - **A port trait with a fake:** fleet-testkit can't see fleet-server's traits, and the render would still need Prometheus.

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
- **R5. Linked files** *(group B's first commit)*: a key file or the database that is itself a link is checked in both places, where it's named and where it really is, so a link inside `backup_dir` that points out of it stays refused. The review asked for the named place only when the file is missing; keeping both is strictly tighter.
- **R6. The backups' last success** is read from the newest finished name at `serve`'s start and at each check of whether one is due, which lists the folder anyway, so a backup the CLI took shows within a minute. R4 asked only for the read at start.
- **R7. The server's metrics port** is `axum::serve`, like the API's, with no timeout for a request head that never ends: axum 0.8 sets no timer on hyper's builder. The threat model's accepted risks record it for this port; the port is internal, never published or proxied (U1, U5).
- **R8. Gauges** carry durations (seconds), sizes (bytes) and times (Unix seconds), each converted without a lossy `as` cast.

### As built (group A)
On `p12/p12.3-start-screen-and-backups`, as planned (S1–S8, R1–R4).

**The start screen** (S1, U7): while it waits, "Server settings" and "Log out on this PC", both through what exists (`navigate("/setup")`, `useLogout`). While a logout runs, every other choice on the start screen waits, "Try again" in the failed state included, so neither a navigation nor a launch run again can race it. The e2e fake's `refuseAll` now answers `logout` as the real command does: the session ends and the answer is `unconfirmed`. New baselines: "the start screen, waiting", dark and light. Red first: six Vitest tests against the old screen; the e2e story against the old fake.

*PR #95's hands-on (U7's second point):* the start screen opens `/setup?from=start`. The setup route reads that search through `setupSearch`, which keeps nothing else, as the audit route's does. The form then offers "Back", a link to `/`, which runs the launch again; from anywhere else it still offers "Back to login". This holds in both start-screen states: waiting, and a session that can't be used. Red first: two Vitest tests found no "Back" link. The e2e step (waiting, Server settings, Back, waiting again with the session kept; then Back once the server answers, Home, and no logout) went red through a temporary mutation, the start screen opening `/setup` without `from=start`. No baseline changed.

**`journal_size_limit = 0`** (S7): both pragma tests read -1 before; the at-rest test found the WAL at 7,906,312 bytes with the deleted chat's text after the snapshot ended; after the change it's under the test's 256 KiB bound, without the text.

**The backup** (S2–S6):
- **fleet-core's `backup`** holds the rules without IO: `BackupPolicy` (the two ranges and the 720 h product), `BackupName` (formatted and parsed from its parts, as fleet-core's chrono has no formatter), the due rule with `FUTURE_SLACK` (1 h), pruning, and `STALE_PARTIAL` (24 h).
- **The config:** `DatabaseConfig` gains `backups: Option<BackupConfig>`, so every literal of it gained `backups: None` (28 in all, mechanical). Five problem kinds were added: `Needs`, `ParentDir`, `HoldsDatabase`, `HoldsKey` and `ProductOver`. A `..` is refused by its text too, since a Windows verbatim path keeps it as a plain name. The old test that used `backup_keep` as a later phase's key now names `[database]`'s keys.
- **`infra::sqlite::backup`:**
  - The copy runs on `PoolRole::Backup`, a read-only connection of its own.
  - **R3:** sqlx's macros describe both `VACUUM INTO ?` and `PRAGMA quick_check`, so no exception to rule 8 was needed.
  - **The stop and the bound** reach `VACUUM INTO` through sqlx's safe `set_progress_handler`, every 100 instructions. At 1,000, a small database's statements never reached the handler. A token already cancelled is seen before the copy starts.
  - **The new file** is checked on an `immutable` connection, so nothing is opened beside it.
  - **The sync** opens the file for writing, since Windows refuses to flush a read-only handle.
- **The schedule:** `Retention::back_up_when_due` returns what it did (`BackupStep`), so its rules are tested directly. One task-level test checks that the loop takes a backup and stops cleanly. `serve` checks the folder at its start (`StartError::Backups`, exit 1) and warns once outside dev mode when backups are off: "backups are off: set [database] backup_dir, and copy its backups off this host".
- **The command:** `cli::backup`. Its exit-3 test holds an older backup open so it can't be removed, which only Windows refuses, so that test runs there alone. `just dev-backup` tries it on the dev database.
- **Red first:** fleet-core's rules ran against stubs, and the config tests against a validator that ignored the keys. The file module, the step, `serve`'s wiring, the command and the restore were each written before their tests. For those, the tests went red through temporary mutations, each caught:
  - a rename for the hard link;
  - no progress handler;
  - no hour's wait after a failure;
  - the tick not taking the backup;
  - no `with_backups` in `serve`;
  - the path on stderr;
  - exit 0 for a failed prune;
  - the temporary file left beside the backup.

**The restore** (S8): `docs/runbook.md` starts, an operator's guide for any Linux host with Docker, with backups and the restore's seven steps, each saying what success looks like (U8's rule, early). `accounts::at_rest::a_restored_backup_opens_with_its_key_and_holds_only_ciphertext` covers the steps' premise. A backup of a database with a linked account holds only ciphertext, with nothing beside it. Copied alone into a fresh folder, it opens without migrating, and its token opens with the same key. Without that key, `vault status` exits 3.

**Records:** README covers the start's line, the `backup` command, the keys and what backups hold. The threat model gains a desktop row, a B6 row, two accepted risks, and its "What a delete leaves" corrected. `.claude/rules/commands.md` gains `dev-backup`. `server.md`'s app list gains `backup`. P12.3 is ticked.

### As built (group B)
On `p12/p12.4-metrics`, as planned (S9–S12, R5–R8), with U9 as its first commit.

**PR #95's follow-ups** (R5): a stop ends the backup's sync and check too, the check running on one `immutable` connection under the copy's progress handler; linked key files and a linked database are checked in both places; pruning removes a leftover `.afkfleet-backup-<stamp>.db.partial-journal` once a day old and never a backup newer than the one just written; the runbook names the hard links `backup_dir` needs; the `backup` command's tests cover pending migrations and a live `serve` beside it.
- **Two existing tests changed on purpose:** they pinned the old prune rule, and now expect the newer backup kept (fleet-core's `pruning_never_takes_the_backup_just_written_or_a_newer_one`, the service's `a_backup_dated_ahead_of_the_clock_doesnt_count`).
- **The two link tests are Unix-only,** like group A's folder-link test: this PC can't make links, so CI's Linux jobs run them.

**The config** (S10): both validators ran as stubs answering "off" first; 13 server and 8 agent config tests failed on their assertions.

**The server** (S9, S11, S12):
- `crate::metrics` holds every series: `ServerMetrics` over each server's own recorder, the label enums (`Limit`, `LockoutRefusal`, `ConnectRefused`, `GrantKind`, `SessionDenial`, `NotTaken`) and `RetentionRun`. The startup render is a sorted snapshot, since the exporter writes in hash order: 83 series at 0 with their help texts.
- The registry's `DenySession`, the plan's `registry_filter`, is counted as `reason="not_assigned"`: what it denies is a bot not assigned to the asking agent.
- Each counting point has a test through its own harness, built with a recorder of the test's own. The account's security counters are also tested through `serve`'s real wiring (`accounts::metrics_wiring`), and so was the registry's session denial (`serve::control_plane`). *(Corrected by PR #96's review, in group C:* `serve::control_plane` now also covers the connect refusals (no certificate, revoked), an online grant and the ingest's stored rows, each red when its `with_metrics` or `ControlPorts`' `metrics` is taken out of `cli/serve.rs`. The retention's isn't, since serve's first retention pass comes a minute after its start.)

**The agent** (S9, S12):
- `metrics_endpoint` is the responder, generic over an `Incoming` source and a `Render`. Its unit tests run over in-memory pipes on paused time, so the cap, the timeout and the pause are measured exactly. The real sockets are covered through the run: standalone (`run::endpoint`), managed (`control::endpoint`) and the binary (`cli`'s `metrics` module).
- The outbox lives in the managed watch's `ManagedTasks`, so a managed run's samples record its counts by type, and a standalone agent's three counters stay at 0.
- tokio's `net` and `io-util` join fleet-agent's own manifest; azalea-protocol builds both already, so Cargo.lock gains no crate.

**Red first.** New code ran against stubs where it was written test-first (the config, fleet-core's rules, the backup module's unit tests). Everything written before its tests went red through temporary mutations, each restored by checksum, and each caught:
- the backup command: migrating instead of refusing; refusing while a `-wal` exists;
- the server's module: no registration; the global recorder; a stopped run counted a success; a duplicate label;
- the server's endpoint: no render extension; the route on the API too; the scrape logged at `info`; `main.rs` ignoring `[metrics]`;
- the server's counts: a limit's refusal not counted; a reuse not counted; only the recorded conflicts counted; refusals counted only under the audit cap; an online grant not counted; stored rows not counted; a retention run not counted; `serve` forgetting the rate limits' metrics; `serve` forgetting the registry's;
- the agent: any path answered with the metrics; no connection cap; no per-connection timeout (it first survived, since the silent-connection test read without a bound; the test now bounds its read); an endless head read past the limit; no pause after an accept error; the stop leaving open connections; the watch not recording the outbox; the outbox's series not registered; a standalone run not serving; a managed run not serving; `main.rs` ignoring `[metrics]`; the run's end neither stopping nor shutting down the endpoint (forgetting its `JoinSet` alone, the first try, is still ended by the run's token, as designed).

**Records:** README gains `[metrics] bind` for both binaries and a Metrics section with every key metric and its labels. The runbook gains "Metrics": turning it on, never publishing the port, reading it with `curl` from the host, and what to watch. The threat model gains B8, a threats row whose tests its citations check, and an accepted risk (R7). `.claude/rules/server.md`'s layout gains the metrics module and router. P12.4 is ticked; P12.2, P12.6, P12.7, P13.1, P13.2 and P13.4 gain notes.

### Notes for later groups
- **C (P12.1, P12.2, P12.7):**
  - **Its questions:** U50 for production; the image scanner (a dependency); Caddy's image; resource limits from the measured numbers (P12.2's notes); the SFTP account's host side (U3); U8's setup script's form.
  - **U4:** its own project, network and named volumes, no host networking.
  - **U5:** both paths, each in the smoke test.
  - **The backups:** the backups' folder on a volume, `backup_dir` set in the production config, and the smoke test taking a backup across a restart.
  - **The metrics ports** (S10, from group B): production binds `0.0.0.0:9480` for the server and `0.0.0.0:9481` for agents, on the project's network only, with no `ports:` and no `EXPOSE`. Caddy and the host's proxy route only 8080. The smoke test scrapes both from inside and checks that `/metrics` through either proxy path reaches neither.
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
    - the metrics in compose form, with group B's alerts;
    - every P12.6 note.
  - **The first release tag** is the user's.
- **Phase 13** (from group B): the metrics router in the authorization matrix (P13.1); a fuzz target for the agent's responder (P13.2); a scrape during the load test (P13.4).

## Consequences
- **Easier:**
  - A server that moved or is gone no longer traps the app at its start.
  - The database can be copied off the host as plain SQLite files, bounded in number and in how long they keep deleted chat, without stopping the server.
  - A WAL that grew no longer keeps its size or its old pages until a restart.
  - Operators can alert on a refresh-token reuse, failing backups or a retention that died, without reading logs (P12.4).
- **Harder:**
  - The backup file's name is a contract: the copier, the runbook and the pruning depend on it.
  - A long backup delays the retention's next pass.
  - Restoring stays a manual procedure, with its security steps in the runbook.
  - Every new count is a typed method and a series in the startup snapshot, and its label values become a contract for alerts.
  - Two small HTTP surfaces more, each internal: the server's metrics router and the agent's own responder.

## Alternatives considered
- **The stack (P12.1, P12.2, P12.7) as group A:** it would have put the infra questions and U50 into this planning, before the features it packages exist.
- **Backups and metrics in one group:** about 6,000 lines, over the size target.
- The rejected options of S1–S12, each listed with its decision above.
