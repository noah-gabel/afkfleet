# 0024. SQLite's advisory via sqlx, zlib in the runtime image, and Caddy 2.11.9

- **Status:** Accepted
- **Date:** 2026-11-09
- **Related:** ADR-0015 (sqlx); ADR-0014 and ADR-0022 (the images, their pins, and the user's 7-day rule, U12, U13); ADR-0021 (the last advisory ignore); `deny.toml`, the justfile's `deny`, `deploy/docker/server.Dockerfile`, `deploy/docker/agent.Dockerfile`; Plan.md §5

## Context
The weekly Audit of 2026-11-09 failed on two advisories published since the week before:
- **`cargo deny`: RUSTSEC-2026-0459 (CVE-2026-119375).** SQLite before 3.54.0 can return uninitialized page-cache memory when a process opens a database file together with a sibling write-ahead log (`-wal`) whose page size differs from the database's: a crafted pair of files makes ordinary queries return stale bytes from the process's memory. The attack vector is local. `libsqlite3-sys` 0.6.0 to 0.38.2 bundle affected SQLite versions; 0.39.0 (2026-10-24) fixes it. fleet-server builds `libsqlite3-sys` 0.37.0 from source through sqlx's `sqlite-bundled`. **No update reaches the fix:** sqlx-sqlite 0.9.0, the newest sqlx release (2026-05-21), requires `libsqlite3-sys` `>=0.30.1, <0.38.0`.
- **The image scan: zlib in the runtime base.** Grype found `zlib1g 1:1.3.dfsg+really1.3.1-1+b1` in the server's image, from `gcr.io/distroless/cc-debian13:nonroot` at `sha256:e792ab3d…`. Two High CVEs (CVE-2026-76844, a heap overflow in `gz_write` after a failed write; CVE-2026-85091) and one Medium (CVE-2026-27171) are fixed in `1:1.3.dfsg+really1.3.1-1+deb13u1` (DSA-6613-1). The agent's image has the same base. The scan fails on any High finding a newer package fixes.

Every pull request's CI runs `just deny` too, so the first one blocks every merge, group E's included.

## Decision
The user's, 2026-11-09: a `deps/` pull request of its own, before group E's.

**1. SQLite: RUSTSEC-2026-0459 ignored, alone, until a sqlx release accepts the fix.**
- **The removal trigger,** in the ignore's own reason too: a sqlx release whose sqlx-sqlite accepts `libsqlite3-sys >= 0.39.0`. That release's bump of sqlx removes the ignore in the same pull request.
- **Why the exposure is negligible:** the attack needs a crafted database and its `-wal` opened together, and nothing hands the server either.
  - The server opens only its own database, in an owner-only volume: a new file is created `0600`, and on Unix the server refuses a database, `-wal` or `-shm` that other users can read (P6.3).
  - Backups are single files made by `VACUUM INTO`, never with a `-wal`.
  - `restore` moves the database's `-wal` and `-shm` aside before it puts a backup in place, so no leftover `-wal` is ever opened with it.
  - Whoever could write files into the data volume could replace the database itself, which is more than this advisory gives.
  - The dev and DoD databases are the developer's own, in their own folders.
- **Upstream:** launchbadge/sqlx#4502, "Bump the upper bound on libsqlite3-sys to allow 0.39.x", merged on 2026-11-08, makes sqlx-sqlite's requirement `>=0.30.1, <0.40.0` on sqlx's `main`; on 2026-11-09 no release carries it yet, so the trigger will most likely come with sqlx's next release.
- **A stale ignore fails:** `[advisories] unused-ignored-advisory = "deny"`, so the workspace's `cargo deny` pass fails with `advisory-not-detected` as soon as an ignored advisory matches nothing, this one once sqlx's fix is in the lockfile. The desktop crate's pass gets `-A advisory-not-detected`: its graph holds none of the ignored crates (azalea's, testcontainers' or sqlx's), so every ignore would read as stale there.
- **Red first:** with `main`'s `deny.toml`, the workspace pass fails on RUSTSEC-2026-0459; with a stale ignore added (RUSTSEC-2026-0346, a rustls advisory no longer in the graph), it fails with `advisory-not-detected`, while the desktop pass with `-A advisory-not-detected` passes; `deny.toml` was restored by checksum after each run.

**2. zlib: both Dockerfiles pin distroless's current index digest, a security exception to the 7-day rule.**
- **The pin:** `gcr.io/distroless/cc-debian13:nonroot@sha256:cc66d222f0a7ec996b78a99fe76a969bc80b539a5016ad866c14d1949bc6d0ab`, in both Dockerfiles (a test holds them equal).
- **Why the exception:** the user's rule takes the newest release at least 7 days old (ADR-0022, U12, U13), but this is a High finding with a fix, in the base of both shipped images. A distroless image's creation date is fixed at 1970 for reproducible builds, so its digest's age can't be read anyway.
- **How it was checked:** `docker buildx imagetools inspect gcr.io/distroless/cc-debian13:nonroot` showed exactly that index digest, with `linux/amd64` and `linux/arm64/v8` images.

**3. Caddy: 2.11.9 now, a second security exception to the 7-day rule, since a Critical finding is remotely reachable.** This branch's Audit run (2026-11-09) passed the server's and the agent's images on the new base, then failed on Caddy's pinned `caddy:2.11.7-alpine@sha256:e70b164d…`, which the scheduled run hadn't reached. The 2.11.7 tag still pointed at that digest: no rebuild fixes it, only a newer Caddy. The user's rule, at the review: move Caddy in a `deps/` pull request of its own on 2026-11-12, when 2.11.9 is 7 days old, unless a finding is remotely reachable in our setup, and if one is, take 2.11.9 now with the checks below.
- **What our Caddy uses** (`deploy/Caddyfile`): HTTP/1.1 and HTTP/2 (`protocols h1 h2`, no HTTP/3), TLS from Let's Encrypt over ACME, `reverse_proxy` to the server's port 8080 over HTTP/1.1, header changes, `respond 404`, a filtered log; no `encode`, `file_server`, `php_fastcgi`, templates or admin API. No service runs `curl`: Caddy's binary is static Go, `probe` runs busybox's `wget`, `files` busybox's `sh`, and nothing has a healthcheck that calls `curl`.
- **Reachable, remotely, through the HTTP/2 server and the reverse proxy** (Go's `net/http`, its internal `http2` and `golang.org/x/net/http2` v0.59.0 in Caddy 2.11.7's go1.26.8 build):
  - **GO-2026-6612, Critical** (CVE-2026-78663): a client that resets streams after sending data gets connection-level flow-control credit back twice, so it can buffer past `MaxReceiveBufferPerConnection`, bounded by the concurrent-stream and stream-level limits. Fixed in go1.26.9, go1.27.2 and x/net v0.60.0.
  - **High:** GO-2026-6603 (memory from a Trailer declaring many fields), GO-2026-6610 (an HTTP/2 reverse proxy forwarding malformed framing headers to HTTP/1 clients), GO-2026-6611 (CPU from many small SETTINGS frames), GO-2026-6617 (an HTTP/2 server crash from concurrent HPACK table-size changes).
- **Not reachable here:** GO-2026-6604 (Critical, Windows `os.Root`), GO-2026-6609 (Range in `FileServer`), GO-2026-6608 (multipart parsing), GO-2026-6860 (x/net's HTML parser), GO-2026-6607 (ECH, which we don't configure), the CONNECT cases GO-2026-6605 and GO-2026-6613 (the server answers no CONNECT with 2xx), klauspost/compress's GO-2026-6865 (`encode` isn't used), and the Alpine packages: brotli-libs (CVE-2026-111497, Critical, known exploited), curl and libcurl (nineteen CVEs, seven Critical) and zlib (CVE-2026-4738, Critical), none of which any process of ours loads.
- **The pin:** `caddy:2.11.9-alpine@sha256:0bbb055bda6793c999bde78b1b5ce3e89f829fb031af2cf50056ea1246559fee` (2026-11-05; `linux/amd64` and `linux/arm64/v8`), the same everywhere Caddy's image is named: `compose.caddy.yaml`, `compose.yaml`'s `probe` and `files`, the smoke test's stand-in and the `image-scan` recipe, which `compose.rs` holds equal (red first on the tag). 2.11.8 brought Go 1.27 release builds; 2.11.9 adds Caddy's own fixes (GHSA-q34h-28rj-5hjm, GHSA-9cr8-wmhh-39pm, go-jose v4.1.6 for GHSA-mgcc-cww5-fp2q).
- **S21's `cap_add: [NET_BIND_SERVICE]` stays,** as the user chose. 2.11.8's binary carries the file capability as `+p` rather than 2.11.7's `+ep`, so Caddy now holds it permitted but not effective: the smoke test, red first on 2.11.9 (`CapPrm` and `CapBnd` 0x400, `CapEff` 0), now expects exactly that, and Caddy binds 80 and 443 as Docker lets any user in a container. The threat model's "Caddy holds one capability" says so; the line can go at a later Caddy move.

- **Three ignores for what 2.11.9 still carries, unreachable here** (`.grype.yaml`). Approved by the user on 2026-11-09 (ADR-0022, U12): none of the three is reachable in our setup (the Caddyfile has no encode, no templates and parses no HTML; Caddy's binary is static Go and loads no zlib), and no Caddy release fixes them yet. This branch's second Audit run passed the server's and the agent's images and failed Caddy 2.11.9 on three findings no Caddy release fixes yet: GO-2026-6860 (High, `golang.org/x/net` v0.60.0's HTML parser; we parse no HTML), GO-2026-6865 (High, `klauspost/compress` v1.20.1, which Caddy uses for `encode`; we don't) and CVE-2026-4738 (Critical, Alpine's `zlib` 1.3.2-r1, which no process of ours loads). Each ignore names its vulnerability, its package's exact version and type, and its reason, so it stops matching once a Caddy image updates that package, and every other finding still fails the scan. Ignored findings leave the job's table; the reasons stay in `.grype.yaml`. Remove an entry when the pinned Caddy image no longer holds that version.
**4. No separate release now.** The fixes reach hosts with the next release, v0.1.1, which the user can now release promptly.

## Consequences
- **Easier:** `cargo deny` and the image scan pass again, and the ignore can't outlive its reason: sqlx's fixed release turns it into a failure.
- **Harder:** every future ignore must match the workspace's graph, or the check fails; an ignore that matters only for the desktop crate would need its own handling.
- **Accepted:** until sqlx's release, fleet-server bundles an SQLite with RUSTSEC-2026-0459, behind the exposure argued above. The runtime base moves to a digest that may be younger than 7 days.
- **Unchanged:** the builder image (`rust:1.99.0-slim-trixie`) isn't shipped or scanned, so it keeps its pin.

## Alternatives considered
- **sqlx-sqlite from sqlx's `main` (a git source):** `deny.toml`'s sources allow crates.io alone.
- **sqlx's `sqlite-unbundled`, linking the image's own SQLite:** Debian 13's SQLite predates 3.54.0 too, and it changes what the image ships.
- **Waiting for sqlx's release:** the weekly Audit and every pull request's CI stay red until then.
- **Leaving `unused-ignored-advisory` at its default, `warn`:** a warning in a passing run is easy to miss.
- **Waiting 7 days for the base image's digest:** a High finding with a fix would ship meanwhile.
