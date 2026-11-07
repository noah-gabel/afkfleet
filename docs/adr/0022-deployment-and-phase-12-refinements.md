# 0022. Deployment and operations: Phase 12 refinements

- **Status:** Accepted
- **Date:** 2026-11-05
- **Related:** Plan.md Phase 12 (P12.1–P12.8), §5, §6, §7, Appendix A; ADR-0014, ADR-0015, ADR-0016, ADR-0017, ADR-0018, ADR-0019, ADR-0020

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
| D1 *(D's plan review, R27)* | `p12/p12.5-release` | PR #97's follow-ups, then P12.5: the installer, the updater, the maintainer's signing step and the rehearsal, the release workflow; the friends' and the maintainer's guides; README's entry-point top (ticks P12.5) |
| D2 *(R27)* | `p12/p12.6-runbook` | P12.6 and the DoD's kit: S23's bundle and `setup.sh fetch`, S25's operations on the volumes, S26, the runbook finished as the operators' guide, the Minecraft version bump in `docs/releasing.md` (ticks P12.6) |
| E *(U35)* | `p12/tidy-up` | P12.8: the repository tidied for readability, no behaviour change; its plan lists first what it would move, merge, shorten or delete (ticks P12.8) |

- **New dependencies** arrive with their group. Each is shown to the user with its version, licence and release date, checked again at that group's planning. Today's figures:

  | Dependency | Group | Version, licence, date |
  |---|---|---|
  | `tauri-plugin-updater` | D | 2.14.1, Apache-2.0 OR MIT, 2026-10-28; *chosen at D's planning, U25* |
  | `@tauri-apps/cli` | D | 2.12.2 → 2.12.4 (2026-10-21); *chosen at D's planning, U26* |
  | `actions/upload-artifact`, `actions/download-artifact` | D | v7.0.2 and v8.0.2, MIT, 2026-10-07, for `release.yml` alone; *U31* |
  | Caddy's official image | C | 2.11.8 (`2.11.8-alpine`), Apache-2.0, rebuilt 2026-11-05; *chosen at C's planning: `2.11.7-alpine`, U13* |
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
  - **No public servers yet:** no production configuration lets an agent dial public Minecraft servers until the user decides it (ADR-0020's U50, now for production). *Decided at group C's planning (U10): production agents may dial public servers, with a switch in the host's `.env`.*
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

  *Tightened at group C's planning (the user's rule, U11, 2026-11-06):* `git status --short` misses an edit to a file that's already modified, new files inside an untracked folder, and ignored paths. The comparison is now `node scripts/tree-fingerprint.mjs`, run before and after every subagent: git's HEAD, refs, stash, local config and index, tracked changes by content (`git diff HEAD --binary`), every untracked file by content hash, and the ignored paths outside `target/` and `node_modules/` by path, size and mtime from lstat, never opened, so nothing under `secrets/` is read and nothing under `target/` (`target/dod-run` included) is even listed. It can't see a change inside those two folders. *Corrected at PR #97's review:* the first version's exclude pathspecs kept `target/` out of the output, but git still opened it (strace showed `target/dod-run/`); now git lists the ignored folders without entering them (`--directory`), the script walks each one itself, skipping every entry named `target` or `node_modules` and never following a link, and a warning from git's listings stops the fingerprint. It also hashes the index's entries with their flags (`git ls-files --stage -v`, so skip-worktree shows) and every hook by mode and content, takes a repository or worktree inside the tree by its HEAD, and runs from the repository's top whatever the folder; CLAUDE.md's sentence names the hooks and the index's flags (the user's approval at the review). The briefs' wording follows Explore.md's: no command that "writes, installs, builds or changes git state". CLAUDE.md's text is the user's, as chosen at the planning.
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

### Group C's planning *(2026-11-06)*
Group C (P12.1, P12.2, P12.7) was planned after group B's merge. Eight read-only review agents decided its open questions and one gathered the dependency facts; before and after each, the session compared the tree's fingerprint (U11), and nothing changed. The user answered the questions that were theirs, then approved the plan with the changes U17–U24.

**The user's answers:**
- **U10. U50 for production** *(the user's decision, replacing U2's "no public servers yet")*: production agents' bots may dial public Minecraft servers from the first start. `deploy/prod/agent.toml` writes `public_targets = true` with a comment citing it; `env.example` sets `AFKFLEET_AGENT__CONTROL_PLANE__PUBLIC_TARGETS=true`, and compose passes `${AFKFLEET_AGENT__CONTROL_PLANE__PUBLIC_TARGETS:-true}`. To stop all public dialling (an incident, say), the operator sets it to `false` in the host's `.env`, which updates never replace, and runs `docker compose up -d agent`; D's runbook lists it under incident response. `private_targets` stays empty, so bots never dial the host's own networks or the services beside afkfleet (U4). A test pins both.
- **U11. U9's check, tightened** *(see U9)*: `scripts/tree-fingerprint.mjs`, and CLAUDE.md's full text as the user chose it.
- **U12. The image scanner is Grype, pinned to its newest release at least 7 days old** *(the user's rule for this pin)*: v0.121.0 (2026-10-19, Apache-2.0), its linux_amd64 and linux_arm64 archives checked against their SHA-256 (GitHub's immutable-release digests, cross-checked with the project's checksums file at implementation). Its successors up to v0.121.2 fix db diff, version ranges and AlmaLinux reporting, bump dependencies, and, in v0.121.1, go-jose for GHSA-mgcc-cww5-fp2q: a DoS when parsing JWKs from untrusted sources, which a scan of our own `docker save` files and Anchore's database never does, so it doesn't matter here. The scan fails on a HIGH or CRITICAL finding a newer package fixes (`--only-fixed --fail-on high`) and lists every finding in the job's summary; an ignore needs the user's approval and a note here. Trivy was the alternative, whose release binaries, images and action tags were compromised in March 2026 (CVE-2026-33634); OSV-Scanner the other.
- **U13. Caddy `2.11.7-alpine`** (2026-10-03, Apache-2.0), the newest release at least 7 days old, pinned by its multi-arch index digest. 2.11.8's and 2.11.9's fixes concern file_server, the file matcher, rewrite, php_fastcgi, `{host}` roots, go-jose and client_auth, none of which our Caddyfile uses; it has 2.11.5's reverse_proxy fix for hop-by-hop headers on a 101. Grype scans it weekly beside our images.
- **U14. The SFTP account's test image** reuses the pinned `rust:1.99.0-slim-trixie` with Debian 13's `openssh-server` from apt, so no new base image; a test holds its `FROM` equal to the Dockerfiles' builder.
- **U15. ShellCheck 0.11.0** (2025-08-04, GPL-3.0) checks `deploy/`'s POSIX sh scripts in CI's frontend job (`just sh-check`, in `just ci`, not `just check`), installed by the already-pinned `taiki-e/install-action`, which checks its SHA-256.
- **U16. CI:** every workflow keeps `contents: read`, no secrets and `persist-credentials: false`, with no new action and no new required check. The Stack workflow runs nightly, on demand and on the pull requests that touch `deploy/**`, `.dockerignore`, `Cargo.lock` or `rust-toolchain.toml` (and its own file, as the Screenshots workflow lists its own), on `ubuntu-24.04-arm` and `ubuntu-24.04`.
- No text friends read changes in group C: every new text is the operator's or a log's.

**The user's changes at the plan review:**
- **U17. One pull request,** though about 7,600 lines, over the 5,300 the groups aim at: P12.2 and P12.7 had to stay together for P12.7's test-first rule, and the user prefers fewer rounds.
- **U18. Operator settings survive updates.** An operator never edits a tracked file: every setting the runbook names (key rotation's `master_key_files` and `active_key_id`, `backup_keep` and `backup_interval_hours`, chat retention, the limits, the subnets, `public_targets`, later `private_targets`) is a line in the host's `.env`, which updates never replace, or in a host-only `compose.override.yaml` added to `COMPOSE_FILE`. Compose hands `.env` to the server, the agent and `setup` (`env_file`), whose unknown keys stop the start; compose's own settings are `${VAR:-default}`; what compose sets in `environment:` (each path's `trusted_proxies`, the metrics address) follows the path and can't be overridden by mistake. Static tests pin it.
- **U19. The server's metrics port is on `fleet` only:** the server has a fixed address there (`AFKFLEET_SERVER_FLEET_ADDRESS`, default `172.30.0.10`, outside the automatic range) and `[metrics] bind` is set to it, so nothing on `edge` reaches 9480.
- **U20. Offline accounts never in `deploy/`:** the smoke test's overlay that allows them lives with the tests, and a static test pins that nothing under `deploy/` allows them, `deploy/dev/` (the development configs) excepted.
- **U21. The smoke test's limits check** measures memory and pids, never CPU, which 8 concurrent argon2 logins are meant to hit.
- **U22. D's notes gain the keys' backup** (below).
- **U23. `HEAD /metrics`** answers 200 with the head alone on the agent too, as the server's axum `get` does.
- **U24. `fleet_testkit::docker`:** the compose e2e test's docker helpers move there, shared with the smoke and SFTP tests.

**Decided by the review agents** *(approved by the user at the plan review)*:
- **S13 (security). The images.** The server's image is its own `deploy/docker/server.Dockerfile`, following the agent's: the same bases by index digest and the same chef and planner stages word for word, so a build of both shares their layers; cook and build select `fleet-server` alone. distroless/cc's `/etc/ssl/certs/ca-certificates.crt` is what reqwest's platform verifier reads (openssl-probe), so ADR-0018's CA note holds. It makes `/data`, `/backups`, `/keys`, `/ca` and `/tls` owner-only and 65532's (a `mkdir -m 0700` in the builder, `COPY --chown`), has no `VOLUME` (an anonymous volume would hide a forgotten mount), runs as `USER 65532:65532`, exposes 8080 and 7443, never 9480 or 9481, and checks its health through `healthcheck` on its `CMD`'s config path. The agent's image gains `/data` and the same `USER`. The four static OCI labels in both; `created`, `revision` and `version` are D's. Tests pin both files through `fleet_testkit::dockerfile`. *Rejected:* one file with two targets (it breaks the instruction helper and the paths); one builder for both packages (Cargo merges their features); caching through `type=gha` (a new action).
- **S14 (security). Keys and data on named volumes, made by the server's image as 65532.** `data` (the database), `keys` (`vault-key-N`, `ca.key`), `ca` (`ca.crt`, also mounted read-only into the local agent), `tls` (the server's pair), `agent-data` (the agent's identity); the server mounts `keys`, `ca` and `tls` read-only. `fleet_startup::secret_file` checks the mode alone; the files come out 0600 and 65532's because the server's own commands write them. The new `afkfleet-server vault create-key [--id N]` writes the file `master_key_files` names (32 bytes from getrandom as padded base64, owner-only, never over a file, no database, the key never printed), and a `setup` service under its own profile (the server's image, `network_mode: none`, `keys`/`ca`/`tls` writable, never `data`) runs it and `pki init --dns server …`; compose never starts it by itself. A forgotten volume fails loudly (no `VOLUME`, `read_only`); a static test pins every config path to a mount, and the smoke test runs the image alone, read-only. *Rejected:* compose's file-backed secrets (a root `chown` at setup, every rotation and every restore; refused from Windows folders; a compose entry per rotated key; no more security than read-only volumes): Plan.md's "Docker secrets" bullet gets a deviation note; the keys on `data` (a copy of the database's volume would hold the key that opens it); host folders (root, and they fail on Windows).
- **S15 (security). The backups' folder and the SFTP account.** The backups live in a host folder bind-mounted at `backup_dir = "/backups"` (long syntax, `create_host_path: false`): `AFKFLEET_BACKUPS_DIR`, default `/srv/afkfleet-backups/backups`, 65532:65532, 0700, in a root-owned chroot folder (0755). The account `afkfleet-backup` has uid and gid 65532, the only way to read 0600 files the server owns (an ACL's mask follows the create mode), shell `nologin`, home `/nonexistent`, password field `*`, and one `restrict` key line in `/etc/ssh/afkfleet-backup.authorized_keys` (root, 0644). `deploy/host/sftp-backups.sh install --key FILE | uninstall` (POSIX sh, root) writes the drop-in `/etc/ssh/sshd_config.d/10-afkfleet-backup.conf` (`Match User`: publickey only, `ChrootDirectory`, `ForceCommand internal-sftp -R -d /backups`, no TTY, no user rc, `DisableForwarding` and each forwarding keyword explicitly, `PermitListen none`, `PermitOpen none`); it refuses a missing `Include` above every Match block, a chroot path part not root's or writable, a taken name, uid or gid, or an existing drop-in; it reloads only after `sshd -t`, `sshd -T -C user=…` showing every value and plain `sshd -T` unchanged, else it removes the drop-in and exits 1. `uninstall` removes the drop-in, the key file and the account, and leaves the folders. A throwaway-container test proves the account can list and read and can't write, delete, rename, link, get a shell or a TTY, forward, or log in by password. *Rejected:* a named volume reached under Docker's own folder; a volume with `driver_opts o=bind`; an SFTP sidecar; a second sshd; runbook steps alone.
- **S16 (security). The proxy paths, picked by `COMPOSE_FILE` in the host's `.env`.** `compose.yaml` alone publishes nothing; `compose.edge.yaml` adds the `edge` network (an existing proxy in a container joins it at `AFKFLEET_PROXY_ADDRESS`), `compose.caddy.yaml` afkfleet's own Caddy on 80 and 443, `compose.loopback.yaml` 8080 on `127.0.0.1` for a proxy installed on the host, `compose.grpc.yaml` 7443 for remote agents (unpublished otherwise). Each path's file sets `[http] trusted_proxies` from the same variables as its network: the proxy's `/32`, or for the host form the `fleet` gateway's. Networks: `fleet` (server, agent, probe; subnet pinned through `.env`, default `172.30.0.0/24`) and `edge` (server, the proxy; default `172.30.1.0/24`, Caddy at `.2`, automatic addresses only in `.128/25`); Caddy is never on `fleet`; neither is internal; no host networking; IPv4 only. The Caddyfile: `admin off`, `persist_config off`, HTTP/1.1 and 2, `grace_period 20s`; a global log to stderr whose filter deletes `ticket` and `q` from every logged URI, since Caddy's error logger logs a 5xx's URI even without an access log; `import tls_{$AFKFLEET_TLS}` (Let's Encrypt's directory alone, no other CA as a fallback; `internal` for tests); HSTS; only `/api/v1/*`, `/health/live` and `/health/ready` go to `server:8080`, everything else is 404. *Rejected:* compose profiles (they can't add a port or a variable to the always-on server); the server on the proxy's external network (8080, 9480 and 7443 open to it).
- **S17 (security). Services, limits, restarts.** `compose.yaml` runs the server and a managed agent (`https://server:7443`, the CA from the `ca` volume, `agent-data` at `/data`); remote hosts get `deploy/compose.agent.yaml`, whose CA certificate is a compose secret from a host file (root once on that host). Configs in `deploy/prod/`, mounted read-only at `/etc/afkfleet/`. `restart: unless-stopped` everywhere: an agent's exit 5 loops at Docker's backoff (at most one start a minute, under the per-certificate limit), and the runbook says to stop it, read `refusal` and fix it. Limits as `.env` knobs with defaults, no swap (`memswap_limit` = `mem_limit`): agent 512m, 1.0 CPU, 512 pids (50 bots: ADR-0008's 222 MiB × 1.21, about 1.9× headroom); server 512m, 1.0, 256; Caddy 256m, 0.5, 128. Grace: server 30 s (over 19 + 5 + 1 s), agent 20 s, Caddy 25 s. Every service: user 65532, `read_only`, `cap_drop: [ALL]`, `no-new-privileges`, json-file logs at 10m × 5, and tmpfs (`noexec,nosuid,nodev`): the server's `/tmp` 64m (SQLite's temporary files stay off the disk), the agent's 1m, Caddy's `/tmp` and `/config`. *Rejected:* `on-failure:N` (its count never resets in moby, and it isn't restarted after a daemon restart).
- **S18 (security). The setup script, `deploy/setup.sh check|init|update`** (POSIX sh: no arrays, `[[`, `local`, `pipefail`, `echo -e`, `read -s`, `mktemp`, `readlink -f`, `sed -i`). It reads `.env` line by line, never sources it, and refuses placeholders. `init`: Docker with Compose v2.24 or newer (rootless and userns-remap refused), `.env`, the images, the ports for the chosen path, a clean state, the backups' folder (made as root only, and it says why), `vault create-key`, `pki init` (the CA's fingerprint printed), the server healthy, the local agent enrolled through a pipe from `exec … create-enrollment-token` into `run … enroll` (the bundle exists only there), everything healthy, the next steps. On a failure it names what it created and deletes nothing. `update`: a backup with the running server, `pull`, the server first, then the rest. `check`: the read-only steps and which path fits. Ports are checked with Docker only (`docker run --rm -p 80:8080 <server image> --version`), and it says that free here isn't reachable from outside. Node tests run it against a stub `docker`.
- **S19 (security). The smoke tests:** fleet-integration's `slow_smoke_*` for Caddy, a container proxy and a host proxy. In CI (`AFKFLEET_SMOKE_SETUP=host`, throwaway runners) they run `deploy/setup.sh init` in a temporary copy, with `sudo` only for the backups' temporary folder; elsewhere the test runs the same steps itself, the backups on a named volume. Login through fleet-client with the full 2FA flow (TOTP from the Docker VM's clock); Caddy's internal root read into memory alone; a second Caddy as the stand-in proxy. They check the client's real address in the audit log (and a forged `X-Forwarded-For` ignored), no `ticket`, `q`, password, TOTP secret or recovery code in any log after a forced 502, both metrics ports scraped from inside and `/metrics` through every path reaching neither, survival of the database and a backup across a recreate with the mounts inspected, clean exits, and memory and pids under 75 % of the limits (U21). Every wait is bounded and wakes on an event, as the compose e2e test's (ADR-0014): no new real-time exception.
- **S20 (security). `/metrics?…`** is answered by both endpoints as `/metrics`: a query isn't another path, and Prometheus' `params` may add one; only the exact path is the route.

**The plan's reconciliations:**
- **R9.** The secrets live on S14's volumes, over S13's assumed `/run/secrets` and S17's compose secret for the local agent's `ca.crt`: the local agent mounts only `ca`, so it never sees `ca.key`.
- **R10.** The server's image keeps `/backups`, as R4 records and as a named backups volume on a Windows PC needs, over S15's "leave it out": a forgotten bind mount then makes every backup fail with an `error` line and the last-success metric, and the static test and the smoke test's mount check catch it first.
- **R11.** Caddy runs as 65532 from the official image with no layer of ours, over S17's two-line layer: its binary's capability is `+p` alone and `/data/caddy` is mode 1777 in the image, which a new named volume inherits.
- **R12.** The nightly runs both architectures (S13), over S19's arm64 alone.
- **R13.** `init` needs root only for the backups' folder, since S14 leaves no host-file secret; the SFTP account is its own command.
- **R14.** The host proxy form is tested with the test itself as the host's proxy: no host networking, so it runs on a Windows PC too.
- **R15.** One workflow, Stack, for the images and the smoke tests.
- **R16.** `deploy/env.example`, not `.env.example`: CLAUDE.md's rule 12 keeps the AI from reading `.env*`, and `.gitignore` ignores them.
- **R17.** The `probe` service stays in `compose.yaml` under its profile: the runbook's way to read the metrics on the project's network (P12.6's note).
- **R18.** No `pipefail` (dash lacks it): the enrollment pipeline's status is `enroll`'s, and a failed token command leaves it without a bundle.

### As built (group C)
On `p12/p12-stack`, one pull request (U17), in nine commits, the plan's ten with the images and their CI in one: PR #96's follow-ups, U9's fingerprint (U11), `vault create-key`, the images and their CI, the smoke tests red against a skeleton, the compose project, the setup script, the SFTP account, then CI and the records.

**The compose project (P12.2)** was built feature by feature against the smoke tests, each feature added after its red run:
- each path's `trusted_proxies` (red: the audit recorded the proxy's own address, or the `fleet` gateway's in the host form);
- the server's metrics bound to its `fleet` address (U19; red: Caddy's container reached `server:9480`);
- `env_file: .env` (U18; red: the test's `.env` setting wasn't in the server's environment);
- the Caddyfile's log filter (red: a forced 502's ticket was in Caddy's log).

U18's pins on `env.example` (every compose variable listed, the runbook's settings, placeholders only, no tracked `compose.override.yaml`, and every setting it names loading in both production configs) are Rust tests in `tests/compose.rs` and both crates' `tests/deploy.rs`, not in the setup script's node test S18 named: they're about compose and the configs, and that test reads the scripts.

**Decided during the build** *(for the user's review)*:
- **S21 (security, low). Caddy keeps `cap_drop: [ALL]` and adds back `NET_BIND_SERVICE` alone** *(decided by a read-only review agent, the tree's fingerprint unchanged before and after; it corrects R11's premise)*. The pinned 2.11.7 image's binary carries `cap_net_bind_service=+ep`; the `+p` R11 read arrived only with 2.11.8. With every capability dropped, Docker can't execute it at all ("exec /usr/bin/caddy: operation not permitted", exit 255), which the smoke test found. So `deploy/compose.caddy.yaml` and the smoke test's stand-in add `cap_add: [NET_BIND_SERVICE]` beside `cap_drop: [ALL]`, user 65532, `read_only` and `no-new-privileges`, and a static test keeps `cap_add` off every other service. Caddy then holds that one capability, permitted and effective (the smoke test reads `/proc/1/status`: inherited and ambient empty, permitted, effective and bounding `0x400`); it allows binding ports below 1024, which Docker lets any process in a container do anyway. From Caddy 2.11.8 on the binary runs with nothing added, so the line goes when the pin moves. *The review agent expected `no-new-privileges` to keep the permitted and effective sets empty; the smoke run showed them holding the capability, so the check and the comments say what Caddy really holds; the decision stands.*
  - *Rejected:* an image of our own whose binary has no file capability (a third image to build on every host or to publish, for one bit); unprivileged ports inside the container (the binary fails before it reads any config); no `cap_drop` (Docker's 14 default capabilities); moving the pin to 2.11.8 (U13's, the user's: 2.11.8 is 7 days old from 2026-11-11).
  - *For the user, found by the same agent:* U13's comparison missed two 2.11.8 fixes that touch our Caddyfile: the `query` log filter (caddy#8255, parameters after a `#`) and hop-by-hop headers on 1xx responses. Neither changes what our app sends, but both argue for the pin's next move.

**Deviations from the plan:**
- **`init` runs no `migrate`:** `serve` runs every pending migration at its start, `setup` never mounts `data` (S14), and a `run` of the server would clash with its fixed `fleet` address (U19).
- **No restore in compose form yet.** Restoring into the `data` volume means copying into a stopped container (`docker compose cp`, with `-a` for the owner) and removing a leftover `-wal` without a shell in the image, which no test proves yet; the runbook says it follows, and D writes and proves it (below).
- **`just test-slow` builds both images** and so runs the smoke tests too: the weekly Slow tests job's timeout goes from 60 to 120 minutes. The Stack workflow's goes from 90 to 120, since it now installs the toolchain and builds the tests, with the already-pinned rust-cache and nextest.
- **The audit's image scan covers Caddy's pinned image** beside ours (U13's "scanned weekly"); a test keeps the justfile's pin equal to the compose files'.
- **The SFTP test's container has no network:** sshd and the client meet on its loopback.
- **ShellCheck found one thing,** SC1003 on the key line's backslash pattern in `sftp-backups.sh`, rewritten as `*\\*` and checked under dash and busybox.
- **The threat model's metrics risk** had split azalea's accepted risk from its two sub-bullets since group B; it moved below them.

**Red first.** New code ran against stubs with its real signature; what existed before its tests went red through temporary mutations, each restored by checksum:
- the follow-ups: each wiring count red with its `with_metrics`, or `ControlPorts`' `metrics`, taken out of `cli/serve.rs`; the responder's head refused a split of its end (1/3, 2/2, 3/1);
- the fingerprint: 13 of 14 tests against a stub with the real exports (the 14th is Unix-only);
- `vault create-key`: six unit tests against a stub that did nothing, the binary's with `main.rs` answering 0;
- the images: the nine server deploy tests against stub files; the agent's through mutations (`public_targets = false`, no `USER`);
- the smoke tests: against the skeleton, then once more before each feature above;
- `env.example`'s pins: a misspelt server key and agent key, a removed variable and a non-placeholder domain;
- the setup script: 15 of 16 against a stub exiting 0 (the 16th, a self-test of the syntax rules, was red on its own regex, then fixed);
- the SFTP account: against a stub exiting 0 (red at the first refusal); its image's `FROM` pin through a changed digest; the justfile's Caddy pin the same way.

**Where it ran:**
- **On the PC:** the three smoke tests (the test's own steps, the backups on a named volume), the SFTP test, the setup script's stub runs under dash in a throwaway container (the screenshot image, no network, `scripts/` and `deploy/` read-only), ShellCheck 0.11.0 (its release binary, checked against install-action's manifest's SHA-256, in a throwaway container), and the static tests.
- **Only in CI:** the smoke tests running `deploy/setup.sh init` itself (`AFKFLEET_SMOKE_SETUP=host`, `sudo` for the backups' folder alone), and everything on arm64.

**PR #97's review** *(the same branch, new commits, each red first)*:
- **`setup.sh` never changes an existing backups' parent:** `install -d` had made a 1000:1000 750 folder root's 755. An existing parent must be root's and writable by root alone, or `init` refuses and leaves it; a missing one is made only as root and named among what the run created. `AFKFLEET_BACKUPS_DIR` follows `sftp-backups.sh`'s path rules (no `//`, `.` or `..` parts, no trailing `/`, a parent other than `/`).
- **`sftp-backups.sh`'s checks:** the same path rules, so the chroot is never `/`; the key file's one line must start with the key's type, so no option (`cert-authority`) comes through behind `restrict`; `root_only` pads stat's `%a`, so a mode printed as `2` counts.
- **`sftp-backups.sh` leaves the host as it found it:** every tool and the reload command are checked before the first write; one cleanup removes exactly what the run added (the key file, the drop-in, the group, the account), for every later failure and, through an EXIT trap, for an unexpected one; `taken()` asks `getent`; `uninstall` removes the account first and stops, removing nothing, when `userdel` fails.
- **The tree fingerprint** never lets git into `target/` or `node_modules/` (see U9's note), stops on a warning from git's listings, takes nested repositories by their HEAD, hashes the hooks and the index's flags, and runs from the top.
- **Small ones:** registry paths in image references (`ghcr.io/owner/…`), with no whitespace, glob character or leading `-`; `AFKFLEET_GRPC_NAMES`' placeholder refused like the domain's; `AFKFLEET_ACME_EMAIL` required (`:?`), since an empty one makes Caddy restart in a loop.
- **How each went red:** the setup cases and the fingerprint's under dash and git 2.43 in a throwaway container (Windows skips them); the SFTP refusals as a table whose cases report what went wrong instead of stopping the test, compared before a reset, so one run showed every case the old script failed (it installed the account for mode `2`, `cert-authority`, `//backups`, `/./` and `/../`; exited 127 without `groupadd`; left its files after a failed `useradd` or reload; half-removed on a failing `userdel`); the unreadable-`target/` case in two steps (git's warnings made fatal, then git's own "could not open directory 'target/'"); three fingerprint pins through mutations.

**Records:** the runbook gains the backups in the compose project, the SFTP pull (host and PC), and the metrics through `probe`. The threat model gains a row for the deployment (B1, B6, B8), Caddy's image in the supply chain, and four accepted risks: Caddy's capability (S21), the host proxy path's trust, the SFTP account's reach, the shared uid. Plan.md ticks P12.2 and P12.7 with their notes and corrects P12.2's routes, Appendix A's `trusted_proxies` and P12.2's Caddy-log note.

### Group D's planning *(2026-11-06)*
Group D (P12.5, P12.6, the DoD's kit) was planned after group C's merge. Six read-only review agents decided its open questions (S22–S27); before and after each, the session compared the tree's fingerprint (U11), and nothing changed. External facts were checked on the web: tauri-plugin-updater 2.14.1 (its default `rustls-tls` pulls `ring`), the Tauri CLI's signer options (`--app-version` since 2.12.0), Tauri's NSIS template and language strings, tauri-build's version info, GHCR's visibility rules and Caddy's release dates. The user answered the questions that were theirs, then approved the plan with two rounds of changes: docs for three readers and the tidy-up group (U34, U35) at the first review, and two CI changes at the second (in U31).

**The user's answers:**
- **U25. tauri-plugin-updater 2.14.1** (Apache-2.0 OR MIT, 2026-10-28), `default-features = false`: no `rustls-tls` (ring, banned), no `system-proxy`, no `zip`. It uses the workspace's reqwest 0.13 (rustls on aws-lc-rs, the platform verifier). Cargo.lock gains the plugin and minisign-verify 0.3.0 (MIT, 2026-09-25), and also the plugin's macOS-only `tar`, `filetime`, `osakit` and `objc2-osa-kit`, never built for Windows (found at the build; the plan said two entries).
- **U26. @tauri-apps/cli 2.12.2 → 2.12.4** (2026-10-21): NSIS 3.13, and an uninstall that no longer leaves `uninstall.exe` and the folder behind.
- **U27. The update's UX.** One background check at each start, and "Check for updates" in an "Updates" section on Profile, next to Theme. A newer version shows a row under the server banner on every screen, with "Install and restart" and "Later". Nothing installs without the click.
- **U28. The install's UI:** passive (a progress bar only; the app restarts by itself).
- **U29. The installer:** per user (`currentUser`, no administrator prompt); WebView2's `downloadBootstrapper` (Tauri's default).
- **U30. Capabilities:** `main.json` grants `allow-check-update` and `allow-install-update`, no plugin or core permission; the CSP is unchanged.
- **U31. CI.**
  - **The draft release:** a separate Linux job that runs no build code holds `contents: write`. `actions/upload-artifact` v7.0.2 and `actions/download-artifact` v8.0.2 (both 2026-10-07, MIT) are approved for `release.yml` alone, pinned by SHA.
  - **The gate, both kinds:** the guard gets `actions: read` and requires **CI** green on the tagged commit, *not Stack (the user's change at approval: Stack runs on main only at night, so a tag made the same day would be refused)*; and each image job runs `just test-stack` before it pushes, exactly as the Stack workflow does (`AFKFLEET_SMOKE_SETUP=host`, the same filter), pinned in `scripts/release-workflow.test.mjs`.
  - **The credential** *(the user's change at approval)*: in each image job, `docker login` runs only after `test-stack`, right before the push, and `docker logout` right after it, so no build or test code runs with the registry credential. The merge job builds the index from the per-architecture digests the image jobs output, never from the `X.Y.Z-arm64` / `X.Y.Z-amd64` tags, and checks them. Both are pinned in `scripts/release-workflow.test.mjs`.
  - **On every pull request:** CI's `tauri-build` job builds the NSIS installer (`just app-installer`).
  - **Secrets and checks:** no secret (the job token alone), and no required check added or removed.
- **U32. The GitHub settings the maintainer sets:** a tag ruleset on `v*`, and immutable releases (no release environment). The two packages are made Public once by hand, which can't be undone.
- **U33. Texts friends read** (approved as printed in the chat, with the user's changes; verbatim):
  - **The notice row:** "afkfleet {version} is available." [Install and restart] [Later] · "Downloading the update… afkfleet restarts by itself once it's installed." · "The update wasn't installed: {message}" [Try again] [Later].
  - **Profile › Updates:** "Updates" · "Installed: afkfleet {current}." · "Check for updates" / "Checking…" · "afkfleet is up to date." · "afkfleet {version} is available." [Install and restart] · "Couldn't check for updates: {message}" · "Updates are off in this build."
  - **The error codes' messages:** `update_unreachable` "GitHub can't be reached to get the update." · `update_manifest` "The update information from GitHub couldn't be read." *(the user's wording)* · `update_invalid` "The download couldn't be verified." *(the user's wording: the failed row already says it wasn't installed)* · `update_install` "The installer couldn't be started." · `no_update` "There's no update to install. Check again." · `update_busy` "An update check or install is already running."
  - **The ellipsis** is the single character "…" everywhere *(the user's change)*.
  - **The installer and Windows:** publisher and company "afkfleet" (earlier builds showed "github"); copyright "The afkfleet contributors. GPL-3.0-or-later."; no licence page and no homepage; Tauri's standard English NSIS pages and shortcuts.
  - **The friends' guide,** `docs/using-the-app.md`, word for word as committed in D1, with the user's two changes to it: step 2 of "Installing the app" is optional and works from any folder ("Optional: to check the download is complete, run `Get-FileHash "$HOME\Downloads\afkfleet_X.Y.Z_x64-setup.exe"` in PowerShell. The SHA-256 it prints must equal that file's line in the release's `SHA256SUMS` (capital or small letters don't matter)."), and "Connecting to a server" reads "Leave **CA certificates (PEM)** empty: the server's certificate comes from a public certificate authority such as Let's Encrypt, which Windows already trusts."
  - **The draft release's body** (on the release page, which friends may read), written by `scripts/release.mjs notes`:
    ```
    afkfleet X.Y.Z

    Desktop app for Windows: afkfleet_X.Y.Z_x64-setup.exe
    How to install and use it: https://github.com/noah-gabel/afkfleet/blob/main/docs/using-the-app.md
    Server images:
      ghcr.io/noah-gabel/afkfleet-server:X.Y.Z@sha256:…
      ghcr.io/noah-gabel/afkfleet-agent:X.Y.Z@sha256:…
    SHA-256 of every file: SHA256SUMS
    ```
- **U34. Docs for three readers** *(the user's change at the first review)*:
  - **`docs/runbook.md`, the operators' guide:** the install, settings, updates, backups and the restore, the keys, metrics, agents, incident response and the uninstall. Nothing about GitHub settings, tags, signing or the updater's key: an operator only uses published releases.
  - **`docs/releasing.md`, the maintainer's guide:** the one-time GitHub setup, the updater's key pair and its offline backup, each release, and the Minecraft version bump (ADR-0003's procedure, moved there from P12.6's runbook list because it's code work). Generic, with no provider names.
  - **The friends' guide,** in plain language: installing the app, connecting, registering with an invite, 2FA, updates.
  - **README becomes the entry point:** what afkfleet is, links to the three guides, then the developer setup.
  - **The DoD's kit** points only at the runbook and the friends' guide.
- **U35. Group E after D** *(the user's change at the first review)*: `p12/tidy-up`, Phase 12's last group, with no behaviour change, tidies the repository for readability. Its plan first lists what it would move, merge, shorten or delete, each with why, for the user's approval. Candidates: README's length and order; an index for `docs/`; stale notes, dead scripts or recipes and outdated comments; Plan.md's finished phases condensed, as long as every note an ADR or a comment points to stays findable. D records it as P12.8 and doesn't start it.
- **The user's approvals at the plan review:** the split into D1 and D2, then E (R27); `just updater-sign` and `just update-rehearsal` joining CLAUDE.md's "the user only" recipes; the image names `ghcr.io/noah-gabel/afkfleet-{server,agent}` as covered by rule 11's repository-URL exception; R22; the friends' guide and the release body as written.
- **The user's rules for the release, from the brief:** the key pair is generated on the user's PC; the AI never sees, handles or stores the private key or its password, and neither goes into the repository, CI, a command line or any output (the public key may be committed). Signing happens only on the user's PC: CI builds the installer without the private key and prints its SHA-256; the user downloads it, checks the hash, signs locally, and uploads the signature and `latest.json` to the draft before publishing. No paid service; the installer stays unsigned. The first release tag is the user's.

**Decided by the review agents** *(for the user's review)*:
- **S22 (security). The release workflow,** `.github/workflows/release.yml`, run by a pushed tag `v*` alone:
  - **`guard`** (`contents: read`, `actions: read`): the tag is `vX.Y.Z`, X.Y.Z equals `[workspace.package] version`, the commit is on `origin/main`, and CI is green on that exact commit (U31). It outputs the version, the revision and one UTC `created`.
  - **`image` ×2** (native `ubuntu-24.04-arm` and `ubuntu-24.04`, `packages: write`): both images built with the OCI labels `version`, `revision` and `created` as build args (`ARG`s in the last stage only), `just test-stack` against them, then (U31) the login, a refusal of any tag that exists, the push of `X.Y.Z-arm64` / `X.Y.Z-amd64`, the digests and the logout (also an `if: always()` step).
  - **`merge`** (`packages: write`, no build code): each `X.Y.Z` index made with `docker buildx imagetools create` from the digests, with index annotations; each digest's platform checked, each per-architecture tag still pointing at it, and the index exactly linux/amd64 and linux/arm64; both index digests output; a warning while an anonymous pull still fails.
  - **Tags:** never `latest` or `X.Y` (a moving tag could update an agent before its server), and never overwritten.
  - **`installer`** (`windows-latest`, `contents: read`): `just app-installer`, which refuses any signing key's variable and builds with `--no-sign`; exactly one `afkfleet_X.Y.Z_x64-setup.exe` and no `.sig`; its SHA-256 in the log and the job's summary.
  - **`release`** (Linux, `contents: write`, no checkout of build code): the files downloaded, `SHA256SUMS` written, then `gh release create vX.Y.Z --draft --verify-tag` with the body above.
  - **Bounds:** a `release` concurrency group, never cancelled; timeouts of 10, 150, 15, 60 and 10 minutes; `persist-credentials: false` everywhere; no `secrets.`, no `environment:`, actions only from the approved SHAs.
- **S23 (security). The host's release files** (built in D2):
  - **The bundle:** `afkfleet-deploy-X.Y.Z.tar.gz`, an allowlist of the production files alone (the six compose files, the Caddyfile, `env.example`, `setup.sh`, `host/`, `prod/`, `LICENSE`, and a generated `RELEASE` file with `version=`, `updates_from=` and the file list), built reproducibly by `scripts/deploy-bundle.mjs` in a `bundle` job (`contents: read`) and listed in `SHA256SUMS`.
  - **Image references:** the bundle's four `image:` lines default to `${AFKFLEET_SERVER_IMAGE:-ghcr.io/<owner>/afkfleet-server:X.Y.Z@sha256:<index digest>}`; the tracked files keep `:?`, so the smoke tests still run `:dev`, and `env.example`'s image lines become comments.
  - **`sh setup.sh fetch X.Y.Z`:** refuses a lower or equal version, or one below `updates_from`; downloads over HTTPS into `.afkfleet-fetch/`, checks the sum, and renames each release file into place; never touches `.env`, `compose.override.yaml` or `ca.crt`. Then `sh setup.sh update` (the server first), which also refuses a running server newer than `RELEASE`.
  - **The install** in about 11 commands, each with its success line; **the uninstall** by label, never a prune.
- **S24 (security). The signing step and the update's proof:**
  - **`just updater-sign key installer sha256 version`** (`scripts/updater-sign.mjs`, the user only) refuses a key inside the repository, a wrong file name or version, an existing output folder, a hash mismatch, and stdin that isn't a terminal. It copies the installer into `target/updater-sign/vX.Y.Z/`, runs the pinned CLI's `signer sign --private-key-path <key> --app-version X.Y.Z` with every `TAURI_SIGNING_*` variable stripped and never `-p` *(as built, and accepted by the user at PR #98's review: the password reaches the CLI in that one child's `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` alone, never as an argument)*, drops the CLI's output, checks that the `.sig`'s key ID is the committed key's and its trusted comment names the file and the version, writes `latest.json` with `wx` (version, `pub_date`, `platforms.windows-x86_64` {signature, url}; no notes), and prints only the `.sig`'s path.
  - **The proof:** `scripts/updater-sign.test.mjs` with two throwaway key pairs made by the pinned CLI outside the repository; `apps/desktop/src-tauri/tests/updater.rs` drives the real plugin with the real config (only the public key and the endpoint overridden) against a loopback server, its release made by `scripts/updater-fixture.mjs` with a throwaway pair of its own. A newer release is found and downloaded byte for byte; tampered bytes, another key, a version raised after signing and an unversioned signature are refused; the same version is no update. Real time, 5 s bounds, no sleep.
- **S25 (security). Operations on the volumes** (built in D2), with no new image and no new server code: a `files` profile service on Caddy's pinned image (nonroot, `network_mode: none`, `data` and `tls` read-write, the backups read-only, never `keys` or `ca`); a `maintenance` service on the server's image (`network_mode: none`, `data` read-write, the key volumes read-only) for `migrate`, `vault status`, `vault rotate-key` and `agent list/disable` while `serve` is stopped; `setup.sh keys-backup [DEST]`, `keys-restore DIR`, `restore BACKUP` (S8's steps, the database, `-wal` and `-shm` moved aside, never deleted), `renew-cert` and `forget-aside`; key rotation as runbook commands; two slow tests (a restore after a crash; a new host from the keys' copy).
- **S26 (security). Dynamic DNS: runbook steps only** (D2): a root-only `/etc/afkfleet-dns/update.curl` filled with `sudoedit`, an `afkfleet-dns.service` (oneshot, `DynamicUser`, `LoadCredential`, curl `--config %d/update --ipv4 -sS --fail --max-time 30`) with a 5-minute timer, a cron form, success and failure lines, the uninstall, and a threat-model row (whoever holds the token can get a certificate for the domain).
- **S27 (security). The updater's integration:**
  - **Commands:** `check_update()` → `UpdateCheck { current, state: off | up_to_date | available { version } }`, the version at most 64 characters, never notes, dates or URLs; `install_update()` installs only the update Rust kept, taken out first. The plugin is registered, but none of its permissions is granted.
  - **Bounds:** the check 20 s (connect 5 s) and the download 5 min, each with an outer timeout; a 64 MiB cap; one `try_lock` for both commands → `update_busy`.
  - **Before installing:** the live watch stopped, the write guard taken, and fleet-client's refresh in flight finished (`Client::settle()`), so a rotation is saved; a failure releases everything.
  - **When updates are on:** only in a release build under the real identifier (R20 adds the rehearsal); otherwise `off`, with no network call. The check config gets `plugins.updater.endpoints: []` and `app-build-check` `--no-bundle`.
  - **The UI:** one check at the start, never retried, kept out of the server banner; "Later" held in memory; the sweep's new rows and baselines.
  - **Pins:** `tauri-hardening.mjs` pins `plugins` exactly (the key, the endpoint, `requireSignedVersion`, `installMode: "passive"`), no `dangerous*`, `allowDowngrades` or `installerArgs`, the check config's keys, no `createUpdaterArtifacts`, the plugin's features exactly none, no `updater:` grant; `tests/acl.rs` refuses the plugin's commands.

**The plan's reconciliations:**
- **R19.** S24's fixtures are made inside the test (the user's rule), over S27's committed signatures.
- **R20. A rehearsal on the user's PC:** `just update-rehearsal` (the user only) makes a throwaway pair in `target/update-rehearsal/`, builds two debug installers, 0.0.1 and 0.0.2, of "afkfleet rehearsal" (identifier `io.github.noah-gabel.afkfleet.rehearsal`, binary `afkfleet-rehearsal`, the throwaway key, a manifest on `127.0.0.1`), signs 0.0.2 through `updater-sign`'s functions and serves the folder until Ctrl+C. Its own product name keeps it off the real install. S27's policy is widened by exactly that case, pinned by a table test.
- **R21.** `check_update` also returns `current`. A check whose manifest or version is bad is `update_manifest`; `update_invalid` is the download's verification alone.
- **R22. The keys' copy is a root-only folder of root-only files** (S25), not one root-only file as U22 words it; each file's owner and mode is checked, and a restore needs no archiver. *Approved by the user.*
- **R23.** `.env` can't remove `master_key_files.1` from `prod/server.toml`, so after a rotation key 1 stays configured and on the volume: harmless, since everything is sealed again with key 2 and the same volume holds both. A note for P13.7.
- **R24.** Each image job exports the label build args for the whole job, so `test-stack`'s own `just images` comes from the cache and the pushed image is the tested one.
- **R25.** The bundle (D2) is built in a `bundle` job (`contents: read`) and reaches `release` as an artifact, so the write job still runs no build code.
- **R26.** testing.md's "nothing is uploaded as a CI artifact (`actions/upload-artifact` isn't approved)" becomes "approved for the release workflow alone; screenshots are never uploaded".
- **R27. The split:** D1 (`p12/p12.5-release`: the follow-ups, P12.5, the friends' and maintainer's guides, README's top, E's record) and D2 (`p12/p12.6-runbook`: S23, S25, S26, the runbook finished, the Minecraft bump, the DoD's kit), each after the other's merge, then E. D2's install and update steps need D1's published images, and the other order would leave the runbook pointing at commands that don't exist.
- **R28.** The friends' guide is its own file, `docs/using-the-app.md`, linked from README's top and from each release's body, so it can be handed to a friend alone.
- **R29. Which group writes which guide:** D1 the friends' guide, `docs/releasing.md` (all but the Minecraft bump) and README's entry-point top; D2 the runbook as the operators' guide alone, the bump in `releasing.md`, and README's operator material moved into the runbook; E README's deeper shortening (U35).

### As built (group D1)
On `p12/p12.5-release`, in eight commits: PR #97's follow-ups; the installer; the maintainer's signing step and the update's fixture (before the Rust side, so `tests/updater.rs` could be red first); the updater's Rust side and its IPC plumbing; the notice and Profile's Updates section; the rehearsal; the release workflow; the records. Every test was red first, against a stub with the real signature or, for a test that pins behaviour already there, through a recorded temporary mutation restored by checksum:
- **The follow-ups:** each new pin in `tests/compose.rs` caught its own mutation (a `user:` on `setup`, `nodev` dropped, a published metrics port, single-quoted `ALLOW_OFFLINE`, `-Server` removed); the SFTP test's twelve new cases each caught one of eight mutations of `sftp-backups.sh`, and none of the fifteen old cases did; the setup tests' three, the smoke test's exact address and its image-alone guard likewise.
- **The installer:** 9 of 10 red against a stub (the tenth passed vacuously over the stub's empty variable list, which another test pins). `just app-installer` built a 4.25 MiB installer; its exe's CompanyName reads "afkfleet".
- **The signing step:** 14 of 14 red against a stub, with throwaway key pairs made by the pinned CLI.
- **The Rust side:** 15 of 21 unit tests red against a stub, 8 of 9 real-plugin tests (`tests/updater.rs`), the four ACL cases under a scratch grant of `updater:default`, fleet-client's `settle` against one that returned at once, and the five TypeScript and five hardening tests against theirs. Two real findings: a loopback download arrives in one poll, before the cap's signal, so the whole download's size is checked too; and a manifest that isn't JSON came back from reqwest as a decode error, now `update_manifest`.
- **The UI:** 10 of 13 red against empty components. The check is kept in a store, not TanStack Query's cache: a logout clears the cache and a resync invalidates it, either of which would ask GitHub again. 20 baselines: 18 new and Profile's two, every one read; two fixes after the first read (the notice row's fixed height, the status line's minimum height), then read again.
- **The rehearsal:** 7 of 7 red against a stub; the script runs on the user's PC alone.
- **The release workflow:** the label pins red before the `ARG`s; `scripts/release.test.mjs` 18 of 18 red against a stub, then one more case red before its code; `scripts/release-workflow.test.mjs` red against a stub workflow. Eight mutations, each caught: the login before `just images`, `test-stack` out of host mode, the push step's logout dropped, a per-architecture tag named in the merge job, a `secrets.` reference, a checkout in the release job, the guard asking Stack's workflow, and the merge's sources taken from the tags.

**Decided during the build** *(for the user's review)*:
- **The signing step prompts for the password itself** (raw mode, hidden) and hands it to the CLI in its one child's `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, instead of S24's "the CLI's own console prompt": the script then controls everything printed, and the password still never reaches a command line or any output. Throwaway keys use `signer generate --ci` (an empty password).
- **The pushed images are checked by their platform and their three release labels**, instead of R24's image-ID comparison: `test-stack`'s own `just images` leaves `:dev` as the tested image by construction, and the labels prove it was built with this release's args. `BUILDX_NO_DEFAULT_ATTESTATIONS=1` keeps each push one platform's image.
- **A tag the registry denies reading counts as absent.** On the first release the packages don't exist yet, and GHCR may answer "denied" rather than "not found"; a token that can't read a tag can't overwrite it either, so nothing is overwritten. A network error still refuses.
- **The guard also requires the tag's commit to be the run's own `GITHUB_SHA`**, so every later job, which checks out that commit, builds what the guard checked; and it reads the run's own `headSha` and `conclusion` instead of trusting `gh run list`'s filters alone.
- **`gh release list` instead of `gh release view`** to find an existing release: it lists drafts too, so a second run never adds a second draft for the same tag.
- **The image matrix doesn't fail fast:** each architecture finishes, and re-running the failed one alone completes the release (a finished job re-run would be refused, its tag existing).
- **CI's Windows test job installs the frontend's packages,** since `tests/updater.rs` makes its release with the pinned CLI; the `tauri-build` job already did.
- **(security) The updater's on/off policy reads the build's own endpoints, not its identifier** *(decided by a read-only review agent; the tree's fingerprint was identical before and after)*. S27 and R20 compared the app's identifier with two constants, but both contain the real identifier, so every build held it and `scripts/app-check.mjs` (ADR-0020, U87, S99) refused the check exe: "it also holds the real identifier". `update::updates_on` now takes the merged config's `plugins.updater`: on in a release build whose endpoints are exactly `[RELEASE_ENDPOINT]` (the committed `latest.json` URL), or in a debug build whose endpoints are all plain http to an IPv4 loopback address (the rehearsal's); everything else is off, the check build's empty list, a missing or unreadable list and a mixed one among them. Every build this repository makes keeps its answer: `just dev-app`, `just app-build --debug` and nextest's builds are off (debug, GitHub's endpoint), `just app-build-check` is off (an empty list), a release build is on, the rehearsal is on. The only builds that differ are ones the repository never makes: a release under another identifier with the committed endpoint (it reaches only GitHub and installs only what the committed key signed), and a debug build under the real identifier with a hand-made loopback overlay (it reaches only this PC). The guard and its rule are unchanged; no dependency is added. Rejected: deciding in build.rs from `TAURI_CONFIG` (a new build-dependency, and a copy of tauri-utils' merge that could drift and fail open); widening the guard (the user's rule, U87); hashing the identifier (a new dependency, and a disguised literal). The table test keeps its name, the threat model's citation; a second test pins the committed `tauri.conf.json` on in a release alone and off once `tauri.check.conf.json` is laid over it, so the constant can't drift from the config. Red: the three "on" cases against a stub answering off, then six mutations (any http host, https in a debug build, `any` for `all`, the release among others, an empty list passing, entries that aren't text skipped), each caught, the last after two cases were added.
- **The desktop crate's `tokio` gains `macros`,** which `update.rs`'s `select!` needs: the workspace's other members enable it, so only a build of the app alone (`tauri build`: the check build, the installer) failed. Found by `just app-build-check`; CI's `tauri-build` job builds the app alone too.
- **P12.5's "README and the runbook explain SmartScreen"** became the friends' guide's "Installing the app" (U34).

**The user's answer during the build:**
- **U36. The friends' guide's "Connecting to a server" matches the app as it is** *(asked when the check build showed it didn't)*. Server setup shows a **Use a custom CA** checkbox, and "CA certificates (PEM)" and **Choose file…** appear only once it's ticked ("CA file" is only a hidden input's label); the fingerprints ("Current custom CA: …") show only for a saved server, so U33's "compare … If they differ, don't save" couldn't be followed. No app change: the guide leaves **Use a custom CA** unticked for a server with a public certificate; for a custom CA it ticks it, chooses the file with **Choose file…**, saves, then compares **Current custom CA** under **Server settings** on the login screen before logging in, with the safety step explicit: "If they differ, don't log in. Choose the right file or untick **Use a custom CA**, and ask the admin." The app's exact labels throughout, so the guide's other steps also name **Register with an invite**, **Repeat password**, **Use a recovery code** and **Check for updates** as the app shows them.

**Records:** the friends' guide (`docs/using-the-app.md`) and the maintainer's (`docs/releasing.md`, without the Minecraft bump, which D2 adds); README's Guides at its top, Phase 12's progress in its Status, the new recipes and the updater; the runbook's intro links the friends' guide. The threat model gains B7's updates, the threats rows "A forged or tampered app update" and "A release built from other code, or published by someone else", and five accepted risks: the unsigned installer, NSIS fetched under SHA-1, the unsigned `latest.json`, a lost updater key, and an image job's token during its build. Plan.md ticks P12.5 with its note, splits D in the group table, adds P12.6's note on the three readers and an unticked P12.8 (E), and updates §5 (the plugin, the CLI, the artifact actions). `.claude/rules/commands.md` gains `app-installer`, `updater-sign`, `update-rehearsal` and `images`' build args; `testing.md` takes R26; CLAUDE.md's "the user only" line gains `just updater-sign` and `just update-rehearsal`.

### PR #98's review (group D1) *(2026-11-07)*
The user's review of PR #98 and their hands-on test, fixed on `p12/p12.5-release` as new commits, each red first.

**The user's finding:**
- **U37. The app always shows its installed version** *(the user's hands-on finding: "Installed: afkfleet {current}." appeared only after a check that reached GitHub, and today none can)*. It's read from the app itself, with no network call:
  - Profile › Updates shows "Installed: afkfleet {current}." in every state (off, checking, up to date, available, failed) and before any check;
  - the login screen and the start screen show a small muted line "afkfleet {version}" at the bottom, so a friend who can't log in can tell their admin which version they have.

  *As built:* a new command, `app_version` (no argument; `AppVersion { version }`), answered from the version the managed `Updater` already holds, with no lock, so it answers at once even while a check or an install runs. The user approved its grant (`allow-app-version`), pinned like `check_update`'s: build.rs's list, the capability, `tauri-hardening.mjs`, `ipc-contract.json` and `tests/acl.rs` (now 71 commands). The user preferred an existing command's answer: `server_profile` answers a tagged enum about the server, so carrying the app's version there would have changed that type's shape and every test's fake answer for it, and `check_update` asks GitHub when updates are on, so neither fits. The UI reads it through one query (`["appVersion"]`, never stale, never retried), unwatched like the updater's calls; the update store's `current` went. The line sits as far from the window's bottom on both screens (the start screen's bottom padding became the login screen's 24 px), so it doesn't move between them. Red: two export tests, the unit test and the contract against a stub answering an empty version, and ten Vitest tests. Ten baselines changed and were read: `login`, `login-refused`, `the-start-screen-waiting`, `the-start-screen-failed` and `the-update-notice-at-the-login`, each in dark and light; Profile's didn't change, since the sweep's check had already filled the line.

**Must fix (security):**
- **The image jobs split in two** *(the user's finding: `just test-stack` runs every dependency's build.rs before the push step, and build code can write `$GITHUB_PATH`, `$GITHUB_ENV`, `~/.docker/config.json` or `scripts/release.mjs` in the workspace, so the push step's `docker login` could hand the token to a planted `docker` or credential helper)*. Each architecture now has:
  - **`build`** (`contents: read` alone, no package permission): `just images` and `just test-stack` exactly as before, then `release.mjs save`: the `:dev` images checked (platform, labels), each saved by its image ID with the IDs recorded, and uploaded with the approved upload-artifact;
  - **`push`** (`packages: write`, 30 minutes) on a fresh runner: a sparse checkout of `scripts/release.mjs` alone (`persist-credentials: false`), setup-node, the download, `release.mjs load` (each tarball must load the recorded ID, of linux/<arch>, with this release's labels) before any login, then the login, `release.mjs push` (tags by ID, never the `:dev` tags), the digests and the logout. No cargo, just, npm or pnpm, and no repository code but release.mjs, which now imports only Node's own modules (its two small helpers copied from app-installer.mjs, a test holding them equal).

  The threat model's "an image job holds the job token while it builds and tests" went away. Red: 12 of 27 release.mjs tests against a re-export stub; the workflow pins red against the old workflow; seven workflow mutations, each caught.
- **Every job that can write is pinned whole** (push, merge, release): its exact keys (so no `container:`, `services:` or anything else), every step word for word, and no `run:` calling npm, npx, pnpm, cargo, just or rustup. The token regex allows spaces (`\$\{\{\s*github\.token\s*\}\}`). The concurrency adds `queue: max` (GitHub's larger queues, 2026-05-07), so a release waiting behind another is never dropped.
- **`scripts/tauri-hardening.mjs` refuses any other Tauri configuration in src-tauri** *(the user's finding: Tauri merges `tauri.windows.conf.json` into the Windows build, which could replace the updater's key unseen)*: any name starting with "tauri" and ending in `.conf.json`, `.conf.json5` or `.toml`, in any case, but `tauri.conf.json` and `tauri.check.conf.json` (`checkConfigFiles`, called by `checkRepoFiles` on the folder's real listing). Red: two tests against a stub; two mutations (the check overlay not allowed, any `.toml` refused), each caught; and the real check refused a real `tauri.windows.conf.json`.
- **`scripts/updater-sign.mjs`** *(the user accepted the password in the child's `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` alone, recorded in S24)*: `signFile` takes a stand-in spawn and parent environment, and tests pin its real arguments (no `-p` or `--password`, never the password) and environment (every `TAURI_SIGNING_*` and `TAURI_PRIVATE_KEY*` variable of the parent left out, in any case, now by prefix instead of a fixed list; only the password's set, and only when one is given). A real signature whose trusted comment names another version, or another file, is refused. `within()` compares the first path part with ".." itself, so a key in `<repo>/..keys/` counts as inside the repository. Red: the `..keys` case and both spawn tests; two mutations (the trusted comment unchecked, a case-sensitive strip), each caught.
- **The streaming cap, proven while it streams:** `tests/updater.rs`'s `the_cap_stops_a_download_while_it_streams_past_64_mib` serves the installer as a body with no length that never ends (at most 512 MiB, so a broken cap can't fill memory): with the real 64 MiB cap the download is refused as `update_invalid` well within its bound, and the server sees the client stop reading after more than 64 MiB and less than four times that. It pins behaviour that was there, so its red is a mutation: the chunk counter never raising the signal left the final size check alone, which the old cap test still passed, and the new test failed, the client having read the server's whole 512 MiB first.

### Group D2's planning *(2026-11-07)*
Group D2 (P12.6, the runbook finished, and the DoD's kit) was planned after D1's merge, on `p12/p12.6-runbook`, as one pull request (the user's choice, as for C). S23, S25 and S26 were approved at D's plan review and weren't reopened; review agents decided how to build them (S28–S51), each read-only, with the tree's fingerprint identical before and after each (U9).

**The user's answers:**
- **U38. The release body** gains two lines, the bundle and the runbook at the release's tag: `Files for hosting the server: afkfleet-deploy-X.Y.Z.tar.gz` and `How to host it: https://github.com/noah-gabel/afkfleet/blob/vX.Y.Z/docs/runbook.md`.
- **U39. The friends' guide's "Removing the app"** in the user's words: log out first (it removes the session the PC keeps in Windows' Credential Manager), then Windows 11's Apps › Installed apps or Windows 10's Apps › Apps & features, and "Delete the application data" ticked for the saved server address.
- **U40. Docker Engine 28.0 or newer is checked at the install** *(the user's addition at the plan review)*: `check_docker`, so `check`, `init` and every subcommand that calls it, refuses an older or unreadable Engine and names the version it found, because `keys-restore`'s `cp -a` and the `uid:gid` user need it: an old Engine is found at the install, never in the middle of a restore. The runbook's "Before you start" lists it.
- **U41. How the keys' copy leaves the host and comes back** *(the user's addition and answer)*: through the operator's own SSH login, never the backups' SFTP account, via a short-lived file (sudo can't prompt without a terminal). A folder made `0700` and the file `0600` before anything is written; `sudo tar` into that file, never to a terminal or a command line; its SHA-256 compared on the host and the PC (`sha256sum`, `Get-FileHash`, capital or small letters alike) before the host's file and folder are deleted; kept offline apart from the backups, as the updater's key is. The way back stages the same way, compares before `keys-restore`, and deletes the staged copy right after it. Every step has its success line, and the transfer's blocks are marked as run by no test.
- **No other text friends read changes;** no new dependency, image or action (the bundle is written with Node's own modules; the services use the pinned server and Caddy images; the `bundle` job uses actions `release.yml` already pins); no new CI permission, secret or required check (`bundle` is `contents: read`). **Caddy's pin (U13):** on 2026-11-07 neither 2.11.8 (from 2026-11-11) nor 2.11.9 (from 2026-11-12) was 7 days old, at the first commit and again before the PR, so it stays.

**Decided by the review agents** *(for the user's review)*:
- **PR #98's review items:**
  - **S28.** A failed install clears an `available` check (`installFailed`; `up_to_date` and `off` stay), so Profile stops offering a release Rust already dropped; the notice keeps "The update wasn't installed" with Try again and Later.
  - **S29 (security).** `update_install` means exactly "Rust stopped the watch": every failure after `begin_switch()` answers it, the install's second signature check included, and the UI restarts the live watch on that code alone. No IPC type changes.
  - **S30 (security).** The install's start is claimed once: the release and its bytes sit in a shared `Mutex<Option<…>>`, the main thread's closure takes them before installing, the 30 s bound takes them to drop them, and the first wins. A closure that won is waited for without a further bound (the installer may still exit the app). The desktop crate's dev `tokio` gains `test-util`.
- **S23's mechanics, the host's release files:**
  - **S31 (security).** `scripts/deploy-bundle.mjs` writes the bundle with its own ustar writer: names at the archive's root in byte order, 0:0, the guard's `created` date, files 0644 and folders 0755, gzip level 9 with the OS byte 3 and mtime 0; it refuses links, other entry types, a `\r`, a name over 99 bytes and any file of `deploy/host/` or `deploy/prod/` off its allowlist. `fetch` sets the modes itself, and `check`/`init` refuse a release file the containers can't read.
  - **S32 (security).** The `bundle` job: `needs: [guard, merge]`, 10 minutes, `contents: read`, a sparse checkout of the script, `deploy/`, `LICENSE` and `Cargo.toml`, the bundle uploaded as an artifact; `release` downloads it, checks exactly the two files are there, lists both in `SHA256SUMS` and attaches them. Pinned whole in `release-workflow.test.mjs`.
  - **S33 (security).** `RELEASE`: a comment, `version=`, `updates_from=`, `server_image=` and `agent_image=` (digest references), then `file=<sha256>  <name>` per file. `updates_from` lives in `Cargo.toml`'s `[workspace.metadata.afkfleet]`.
  - **S34 (security).** Only the exact image lines are rewritten to a default of the digest reference; `env.example`'s image lines become comments; with a `RELEASE` the images come from it (a `.env` line overrides with a warning), without one both `.env` lines are needed; either variable in setup.sh's own environment is refused.
  - **S35 (security).** `sh setup.sh fetch X.Y.Z`: curl over https alone with bounded size, time and redirects, from the repository's release URL; a well-formed newer version only, `updates_from` respected; the bundle's sum checked, its entries' types and names, every file against the new `RELEASE`, every installed file against the old or new list (an operator's edit refuses); `.env`, `compose.override.yaml` and `ca.crt` never touched; `RELEASE` moved in last; `update` refuses while `.afkfleet-fetch` exists.
  - **S36 (security).** `update` reads the running server's `org.opencontainers.image.version` label and refuses one newer than `RELEASE`; an empty label (a local build) isn't compared.
  - **S37.** The install in eleven commands, the update in two, each with its success line.
- **S25's mechanics, operations on the volumes:**
  - **S38 (security).** `files` (Caddy's pinned image, profile `files`, no network, `deploy/files.sh` mounted read-only with fixed verbs, `data` and `tls` with `nocopy`, the backups read-only, 64m, 0.5 CPU, 16 pids) and `maintenance` (the server's image, no network, `keys` read-only, the server's limits). Neither creates a volume; `files.sh` checks each volume root is `65532:65532 700`.
  - **S39 (security).** `keys-backup [DEST]` copies `keys` and `ca` from the server's container into a root-only folder of root-only files and prints names alone; `keys-restore DIR` restores them into new volumes on a clean project and issues a server certificate from the restored CA. `tls` and `agent-data` stay out.
  - **S40 (security).** `restore NAME` moves the database, its `-wal` and `-shm` aside under a stamp (never replaying a `-wal`), copies the backup in, migrates it, checks the vault, lists the agents, disables again the agents disabled since the backup when the server was healthy, and starts; after a crash it ends stopped and prints the commands. `forget-aside [STAMP]` lists, or deletes exactly one stamp's names, with the server healthy.
  - **S41 (security).** `renew-cert`: the old pair aside, a new one from the same CA for `server` and `AFKFLEET_GRPC_NAMES`, the old one back on a failure.
  - **S42 (security).** The vault key's rotation and incident response stay runbook commands, no subcommand (`.env` edits between steps).
- **Tests that run the runbook's commands:**
  - **S43.** Two slow tests in `smoke::ops` (`slow_ops_restore_after_a_crash`, `slow_ops_new_host_from_the_keys_copy`), in `test-stack`'s filter; Script mode (CI) runs `setup.sh`, Steps mode (the PC) the same compose calls.
  - **S44.** The install and `update` in tests: the runbook's install lines on a bundle `deploy-bundle.mjs` builds, `fetch` through a stub curl, `update` for real in (a).
  - **S45.** `scripts/runbook-commands.mjs` ties each command line of the runbook to a test, or its block carries the marker `> **Not run by afkfleet's tests:** <why>.`.
  - **S46.** Which command lands in which test, and which blocks are marked (the downloads, U41's transfer, `docker image rm`, the firewall, DNS, the provider's console).
- **The runbook, README, the kit, the bump, the threat model:**
  - **S47.** The runbook's 22 sections for operators alone, every procedure step with "Success:", placeholders alone; `compose.rs` pins its settings to `env.example`, and `docs-placeholders.test.mjs` the guides to placeholders.
  - **S48.** README's operator material moves into the runbook, each heading kept with a pointer; the config reference, the API routes, the commands and exit codes stay for developers; pointers fixed in `vault rotate-key`'s help, `at_rest.rs` and CLAUDE.md's bump line (a pointer, no rule change).
  - **S49.** `docs/dod/phase-12.md`, the user's checklist: a clean install by the runbook alone, then a friend's login by the friends' guide alone; each step a Do, a Success and an empty Outcome; `scripts/dod-phase-12.test.mjs` pins its shape.
  - **S50.** `docs/releasing.md` becomes the Minecraft bump's only source, ADR-0003's steps and numbers kept, step 5's checks grouped, a step 7 for the release; ADR-0003's procedure a pointer.
  - **S51 (security).** The threat model's new rows (the keys copied, restored or read by the volume tools; an update that installs other files or images), accepted risks (the dynamic-DNS token, the bundle's integrity without authenticity, `files` beside the database, `keys-restore` on Docker's `cp -a`), assets (row 2's home, the domain's record), and the citations checker requires a cited `scripts/*.test.mjs` to exist.

**The plan's reconciliations:**
- **R30 (security).** `keys-restore` stages a copy owned by 65532 in a root-only folder of its own before `cp -a`, so either reading of Docker's ownership rule gives the server's user, and the SFTP account (also 65532) can't reach the staged files.
- **R31.** The threat model cites S43's test names.

### As built (group D2)
On `p12/p12.6-runbook`, in eleven commits: PR #98's review items; the bundle; the release; setup.sh's release side; the `files` and `maintenance` services; the ops slow tests; `restore`, `forget-aside` and `renew-cert`; `keys-backup`, `keys-restore`, init's restored state and U40; the runbook finished with its checker; releasing.md's bump and the DoD's kit; the records. Each red first; every mutation restored by checksum.
- **PR #98's items:** 3 of 10 new Rust tests red against the seam carrying today's logic (the second check answered `update_invalid`; a late start installed; the bound gave up on a start the main thread runs), 7 Vitest cases red before the fix; the pins through mutations. The old test that expected a restart after a download's `update_invalid` asserted the defect and now expects none.
- **The bundle and the release:** `deploy-bundle.test.mjs` 22 of 22 red against a stub, the fixture's SHA-256 pinned from the first green build and its bytes read by the test's own ustar parser and the system's tar; `release-workflow.test.mjs` red against the old workflow and S32's six mutations each caught.
- **setup.sh:** 26 new stub cases red for its release side, 12 for `files.sh` (under dash, in the throwaway container), 11 for `restore`/`forget-aside`/`renew-cert`, 10 for the keys and U40; ShellCheck 0.11.0 clean after splitting one `&& ||` (SC2015).
- **The ops tests,** in Steps mode on the PC: first red on two real gaps (`docker compose create` has no `--no-deps`; a second TOTP login inside the first's step), then green: the crash test 92 s, the new host 34 s, the SFTP test 16 s. Script mode runs on CI.
- **The runbook's checker:** 20 of 22 red against stubs, then two findings in the real runbook (commands in inline code). `docs-placeholders` caught a private subnet given as an example; `dod-phase-12.test.mjs` red against a one-line kit and its fixtures against a stubbed checker.

**Decided during the build** *(for the user's review)*:
- **The review's updater note was half right.** tauri-plugin-updater 2.14.1 compares versions first: a release that isn't newer is "no update" even without `windows-x86_64`, but a newer one without it is `TargetsNotFound`, so `update_manifest`. The docs say both, and two tests on the real plugin pin them.
- **`put-back` and `tls-back` move what's there to `<name>.undone-STAMP`,** never deleting it, and `forget` deletes those names too: S40's exact list gains them.
- **`files.sh` takes `FILES_ROOT`** in place of `/vol`, for its tests alone; the service never sets it.
- **fleet-testkit's `finish_bytes` and `Compose::run_bytes`** keep stdout's bytes for the keys' copy as a tar stream; `finish` converts lossily instead of dropping bytes that aren't UTF-8.
- **`Owner::again` waits for the authenticator's next step,** woken by the server's log lines or every 2 s within `SLOW` (S19's form, no new real-time exception): a second login inside the first's step is refused, as it should be.
- **The checker's rows name a literal and its file,** not the line itself: tests add `-T` and their own names. A `powershell` block is always marked (the operator's PC). **The Owner's line is marked:** it asks for the password at a terminal, and the tests run the same command with `--password-stdin`.
- **More runbook commands run by tests than S46 listed:** the folder lines and the fetch folder's removal in the stub tests; `files put-back` (then a second restore), `ps` and `logs --tail 50 agent` in the crash test; the compromised host's `docker compose stop` by the new-host test. **`keys-restore` makes the backups' folder** as `init` would (it runs `check_backups` as root), so the new-host test leaves it to it.
- **The guides' links are checked** (`markdown.test.mjs`: README, the runbook, the friends' and the maintainer's guides, the kit), and README stays out of the placeholder check: it's the developers', naming their tools' sites.
- **This PC's working `LICENSE` had CRLF** from an old checkout; the bundle's `\r` refusal caught it, and it was rewritten from the index (git's bytes are LF).

**Records:** the runbook, README's pointers, Guides and Status; the maintainer's guide's bump and ADR-0003's pointer; CLAUDE.md's bump line (a pointer, no rule change); the DoD's kit; the threat model's two rows, four accepted risks, its assets and the citations checker's new rule (S51); Plan.md's P12.6 ticked with its note, the lost agent's note moved under it from P13.4, and the DoD's note naming the kit.

### Notes for later groups
- **C (P12.1, P12.2, P12.7):**
  - **Its questions:** U50 for production; the image scanner (a dependency); Caddy's image; resource limits from the measured numbers (P12.2's notes); the SFTP account's host side (U3); U8's setup script's form.
  - **U4:** its own project, network and named volumes, no host networking.
  - **U5:** both paths, each in the smoke test.
  - **The backups:** the backups' folder on a volume, `backup_dir` set in the production config, and the smoke test taking a backup across a restart.
  - **The metrics ports** (S10, from group B): production binds `0.0.0.0:9480` for the server and `0.0.0.0:9481` for agents, on the project's network only, with no `ports:` and no `EXPOSE`. Caddy and the host's proxy route only 8080. The smoke test scrapes both from inside and checks that `/metrics` through either proxy path reaches neither.
  - Group C may be split at its plan review.
- **D (P12.5, P12.6)** *(P12.5 and the follow-ups built in D1; what follows is D2's)*:
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
  - **From group C** *(2026-11-06)*:
    - **The settings (U18):** every setting the runbook tells an operator to change is a line of the host's `.env` (`deploy/env.example` lists them, with their defaults), or a host-only `compose.override.yaml` the operator adds to `COMPOSE_FILE`; never an edit of a tracked file, which an update replaces.
    - **The keys' backup (U22):** how the operator copies the vault key and the CA's key from the `keys` volume into a root-only file on the host, never printed to a terminal or put on a command line (check first that `docker compose cp` reads a volume's path, and keeps the owner with `-a`). The SFTP pull deliberately doesn't carry them, and losing the volume without a copy loses every linked account.
    - **The restore in compose form,** proven by a test before the runbook gives it: the backup copied into the stopped server's `data` volume as 65532, 0600, and a leftover `-wal` removed without a shell in the image.
    - **The install** in about ten commands: copy `deploy/` to the host, `cp env.example .env` and fill it in, `sudo sh setup.sh init`, the Owner (`docker compose exec server … user create-owner`), the SFTP account, the app. **The update** in two: `sh setup.sh update` after the new release's files.
    - **`Restarting (5)`:** an agent refused for good loops at Docker's backoff (S17): stop it, read `refusal` in its log, fix it, start it.
    - **A remote agent** (`deploy/compose.agent.yaml`): the server's `ca.crt` copied to that host, then `chown 65532:65532` and `chmod 600` as root once, and `compose.grpc.yaml` on the server's host; the server certificate's names from `AFKFLEET_GRPC_NAMES` at `init`.
    - **The host:** Compose v2.24 or newer (`setup.sh` checks it); IPv4 only, so the domain's A record; rootless Docker refused.
    - **An existing proxy's duties:** TLS, WebSocket upgrades, `X-Forwarded-For` set (never passed on from the client), only `/api/v1/*`, `/health/live` and `/health/ready` routed, never a metrics port, and no `ticket` or `q` in its logs; from a container, it joins `afkfleet_edge` at `AFKFLEET_PROXY_ADDRESS`.
    - **Caddy's pin moves** when the user decides (U13): 2.11.8 is 7 days old from 2026-11-11, drops S21's `cap_add`, and fixes the `query` log filter and 1xx hop-by-hop headers.
  - **From PR #97's review, for D's first commit** *(the user's list, 2026-11-06)*:
    - **`tests/compose.rs`:** no service sets `user`, `read_only`, `cap_drop`, `security_opt` or `logging` itself (an override would silently replace the anchor); every service's exact ports across all six files, instead of the text match on `"9480:`; the U20 check also strips single quotes; `noexec,nosuid,nodev` on every tmpfs and `-Server` in the Caddyfile pinned.
    - **Limits for `setup` and `probe`:** fixed small values.
    - **Test gaps:** in the SFTP test, the `sshd -t` after writing, "unchanged for everyone else", the `AllowUsers` check, the 65532:65532 700 check, `root_only`'s owner half, `taken`'s name and gid halves, the `ssh-keygen` and quote checks; in the setup tests, `check_clean`'s containers half, the userns refusal, a failing token command reaching `enroll`.
    - **The smoke test:** the image-alone container gets a name and a drop guard that removes it; the `X-Forwarded-For` check asserts the exact client address.
    - **The threat model's accepted risks:** a peer on `edge` with `CAP_NET_ADMIN` could route to the server's `fleet` address; the SFTP account's check runs `sshd -T -C` with one test address, so an earlier drop-in with a `Match Address` could override it for real logins from a LAN.
  - **The first release tag** is the user's.
  - **From group D1, for D2** *(2026-11-06)*:
    - **Its scope (R27, R29):** S23's bundle, `release.yml`'s `bundle` job (`contents: read`, its artifact to `release`, R25), `setup.sh fetch` and `update`'s version check; S25's `files` and `maintenance` services, `setup.sh`'s five subcommands and the two slow tests; S26; the runbook finished as the operators' guide alone (U34), with README's operator material moved into it; the Minecraft version bump in `docs/releasing.md`, replacing its placeholder heading; the DoD's kit (`docs/dod/phase-12.md`, pointing only at the runbook and the friends' guide, a node test pinning its shape).
    - **The release body** gains the bundle's line, and `SHA256SUMS` lists it; `scripts/release-workflow.test.mjs` pins the new job like the others.
    - **R22** (the keys' copy as a root-only folder of root-only files) and **R23** (key 1 stays configured after a rotation; a note for P13.7) are D2's.
    - **Caddy's pin:** 2.11.8 qualifies under the 7-day rule from 2026-11-11 and 2.11.9 from 2026-11-12; ask the user with its date, its fixes and S21's `cap_add` (which 2.11.8 drops).
  - **From PR #98's review, for D2's first commit** *(the user's list, 2026-11-07)*:
    - **A failed install** leaves Profile offering "Install and restart" for a release Rust already dropped: clear the check on `installFailed` (or route the button through check-then-install), with a test.
    - **`useUpdates` restarts the live watch after any install failure,** though Rust stopped it only after a successful download: restart it only then.
    - **The install's timeout doesn't cancel the queued install,** which can still exit the app later, without `settle`.
    - **update.rs's and app.rs's docs:** since tauri-plugin-updater 2.14.0, a manifest without `windows-x86_64` is "no update", not `update_manifest`.
    - **Tests:** "Later" lasting only until the next start (a real restart); the notice not coming back after a login or a logout; `settle()` and `begin_switch()` on a live session.
    - **`tests/compose.rs`:** refuse `exec`, `suid` and `dev` in any tmpfs; check long-syntax tmpfs volumes; strip tabs in the U20 check; move the stale doc comment back to its test.
    - **The friends' guide:** Windows 10's path is "Apps & features".
- **E (P12.8, after D2's merge; U35):** its plan first lists every move, merge, shortening or deletion with why, for the user's approval, and changes no behaviour. Every note an ADR or a comment points to stays findable; CLAUDE.md's pointers to `.claude/rules/` and the threat model's citations keep working.
  - **From D2** *(2026-11-07)*: README keeps the config reference (`scripts/rate-limit-docs.test.mjs` reads its `[rate_limits]` keys), the API routes, the commands and exit-code tables, Status's list and Phase 11's DoD steps; whether they move (a `docs/configuration.md`, a `docs/` index) is E's (S48). Plan.md's and ADR-0020's historical "README" pointers stay.
- **Phase 13** (from group B): the metrics router in the authorization matrix (P13.1); a fuzz target for the agent's responder (P13.2); a scrape during the load test (P13.4).
- **Phase 13** (from D2): R23, key 1 staying configured after a rotation (`prod/server.toml` names it, and `.env` can't remove it), for P13.7; the four accepted risks S51 added, reviewed with the rest in P13.7.

## Consequences
- **Easier:**
  - A server that moved or is gone no longer traps the app at its start.
  - The database can be copied off the host as plain SQLite files, bounded in number and in how long they keep deleted chat, without stopping the server.
  - A WAL that grew no longer keeps its size or its old pages until a restart.
  - Operators can alert on a refresh-token reuse, failing backups or a retention that died, without reading logs (P12.4).
  - A host with only Docker runs afkfleet beside its other services, behind its own Caddy or an existing proxy, with its settings in a `.env` no update replaces, and its backups pulled off by an account that can only read them (group C).
  - Friends install the app from a release page with one guide, and get updates in the app, checked against the maintainer's key; a release is a tag, a workflow that holds no key, and the maintainer's signing on their own PC (group D1).
- **Harder:**
  - The deploy files' shape is pinned by static tests and the smoke tests, so a compose change updates both; the smoke tests need both images built and Docker (group C).
  - Caddy's and Grype's pins move by hand, each to a release at least 7 days old (U12, U13).
  - The backup file's name is a contract: the copier, the runbook and the pruning depend on it.
  - A long backup delays the retention's next pass.
  - Restoring stays a manual procedure, with its security steps in the runbook.
  - Every new count is a typed method and a series in the startup snapshot, and its label values become a contract for alerts.
  - Two small HTTP surfaces more, each internal: the server's metrics router and the agent's own responder.
  - Every release needs the maintainer, their offline key and its password: losing either strands installed apps until friends install by hand (group D1).
  - A tag pushed to the registry is never pushed again, so a release whose push failed halfway costs a version number.
  - The release workflow's shape is pinned by its tests, so a change to it changes them too.

## Alternatives considered
- **The stack (P12.1, P12.2, P12.7) as group A:** it would have put the infra questions and U50 into this planning, before the features it packages exist.
- **Backups and metrics in one group:** about 6,000 lines, over the size target.
- The rejected options of S1–S12, each listed with its decision above.
- **Signing the update in CI** with the key as a repository secret: the user's rule keeps the private key on their PC alone, so CI builds without it and the maintainer signs (U1, S24).
- **One release job that builds and writes:** it would run build code with `contents: write`; the writing job runs no repository code instead (U31).
- **Moving tags such as `latest` or `X.Y`:** a moving tag could update an agent before its server (S22).
