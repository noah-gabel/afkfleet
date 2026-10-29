# afkfleet development commands. Keep the command table in CLAUDE.md in sync.
#
# Recipes run in PowerShell 7 on Windows and in sh on Linux CI, so every line
# must work in both: one command per line, no `&&` or `||`, environment
# variables only through just's `export` or `$`-parameters (just exports both,
# so they work in pwsh and sh alike). Anything more complex goes into scripts/.

set windows-shell := ["pwsh", "-NoLogo", "-NoProfile", "-Command"]

export RUSTDOCFLAGS := "-D warnings"

# The desktop app's crate. It ships only for Windows, and on Linux it would need
# WebKitGTK and GTK3, so every workspace-wide Rust recipe leaves it out there. Windows
# covers it: clippy and test on windows-latest, and CI's tauri-build job (ADR-0017).
desktop := "afkfleet-desktop"
linux_exclude := if os() == "linux" { "--exclude " + desktop } else { "" }

# The stable toolchain for `stable-check`, and the crates it checks: every crate
# that doesn't depend on azalea or azalea-auth (the desktop crate only off Linux).
stable := "1.99.0"
stable_crates := "-p fleet-core -p fleet-testkit -p fleet-runtime -p fleet-startup -p fleet-proto -p fleet-server -p fleet-api-types -p fleet-client" +(if os() == "linux" { "" } else { " -p " + desktop })

# The throwaway database `cargo sqlx prepare` checks queries against. Absolute,
# because rustc runs the query macros in the workspace root, not in the crate.
sqlx_db := "sqlite:" + justfile_directory() / "target" / "sqlx-prepare.db"

# List all recipes.
default:
    @just --list

# Formatting, lints, docs, tests, frontend checks and the e2e suite: run before a task is done.
check: fmt-check clippy docs test doctest db-check gen-check testkit-check tauri-check scripts-test ui-check e2e

# Everything CI runs.
ci: fmt-check clippy docs test-ci db-check migrations-check gen-check testkit-check tauri-check scripts-test cov cov-desktop deny stable-check ui-check ui-audit e2e

# Run nextest for the workspace, or for one crate: `just test fleet-core`.
test crate="":
    cargo nextest run {{ if crate == "" { "--workspace " + linux_exclude } else { "-p " + crate } }}

# They need fleet-mc's test-only `fault-injection` feature (ADR-0011). The server
# image is pulled and the agent's image built first, so neither eats into a timed
# test; AFKFLEET_E2E_IMAGE_BUILT tells the compose e2e test that the image is
# this source's (ADR-0014).
# Slow tests: containers and a real Minecraft server (needs Docker), one at a time.
test-slow $AFKFLEET_E2E_IMAGE_BUILT="1":
    docker compose --file deploy/compose.dev.yaml pull minecraft
    docker compose --file deploy/compose.dev.yaml build agent
    cargo nextest run --workspace {{ linux_exclude }} --profile slow --no-tests=warn --features fleet-mc/fault-injection

# The user's real-account check (ADR-0011): a real token from
# secrets/p1.8-account.txt joins a local online-mode container (needs Docker).
# It needs the user's real credentials, so only the user runs it; the AI never
# does. Create the file with the archived spike's fetch-token right before.
test-real-account:
    cargo nextest run -p fleet-mc --profile manual

# The user's Microsoft check (ADR-0018): the Microsoft adapter, and the link flow
# through serve's own routes, against the user's own account. The adapter's checks
# stay in memory; the link flow's database is a temporary folder, deleted at every
# end and sealed with a vault key drawn for the run and kept in memory. It prints a link and a code to enter, the deadline,
# then pass or FAIL per step, never a token, a name or a UUID. It needs the user's
# real account, so only the user runs it; the AI never does. `filter` picks one of
# its three tests (`account_check`, `declined_link` or `link_flow`); without it, all
# run in turn.
test-ms-account *filter:
    cargo nextest run -p fleet-server --profile manual --no-capture {{ filter }}

# The user's Phase 10 check (ADR-0019, S73): in managed mode, a bot with the user's
# own Microsoft account goes Online on a local online-mode Minecraft container
# through the API, and offline through stop (needs Docker). The server and the agent
# run in-process; the database, the PKI and the agent's identity live in a temporary
# folder, deleted at every end, sealed with a vault key drawn for the run and kept in
# memory. It prints a link and a code to enter, then pass or FAIL per step, never a
# token, a name or a UUID. It needs the user's real account, so only the user runs
# it; the AI never does. The image is pulled first, so the pull never eats into the
# timed run.
test-managed-account:
    docker compose --file deploy/compose.dev.yaml pull minecraft
    cargo nextest run -p fleet-integration --profile manual --no-capture manual_managed_bot_goes_online_and_offline

# Coverage, checked against the gates from Plan.md §8.
cov:
    cargo llvm-cov nextest --workspace {{ linux_exclude }} --no-report
    cargo llvm-cov report --summary-only
    cargo llvm-cov report --json --output-path target/coverage.json
    node scripts/coverage-gates.mjs target/coverage.json

# The desktop crate is measured on Windows only, since Linux never builds it (ADR-0017).
# `--no-rustc-wrapper` instruments through RUSTFLAGS: cargo-llvm-cov 0.9.1's rustc wrapper
# leaves the lib's unit-test binary uninstrumented on Windows (no profiler runtime, no
# .profraw), so the in-crate tests wouldn't count (ADR-0017).
# Coverage of the desktop crate (Windows), checked against its gate.
[windows]
cov-desktop:
    cargo llvm-cov nextest -p afkfleet-desktop --no-rustc-wrapper --no-report
    cargo llvm-cov report -p afkfleet-desktop --summary-only
    cargo llvm-cov report -p afkfleet-desktop --json --output-path target/coverage-desktop.json
    node scripts/coverage-gates.mjs --report desktop target/coverage-desktop.json

# Coverage of the desktop crate: measured on Windows only (ADR-0017).
[linux]
cov-desktop:
    @echo "The desktop crate is measured on Windows only: CI runs just cov-desktop in its tauri-build job (ADR-0017)."

# Two passes with the same deny.toml (ADR-0017): the workspace without the desktop
# crate on every target in deny.toml, then the desktop crate alone on Windows, the only
# platform it ships for. Its Linux GTK3 stack is never built or shipped. A new desktop
# target joins the second pass.
# cargo-deny: advisories, licenses, bans and sources.
deny:
    cargo deny --exclude {{ desktop }} check
    cargo deny --manifest-path apps/desktop/src-tauri/Cargo.toml --target x86_64-pc-windows-msvc check

# Format Rust and frontend code.
fmt:
    cargo fmt --all
    pnpm exec biome format --write .

# Each run exports into a fresh folder in target/gen/, and only files ts-rs generated
# are ever deleted from packages/ui/src/generated/ (ADR-0015). The gRPC code isn't
# exported or committed: fleet-proto's build script generates it at every build, and a
# snapshot test pins the wire schema it comes from (ADR-0019).
# Export the API's TypeScript types (fleet-api-types) to packages/ui/src/generated/.
gen:
    cargo run -p fleet-api-types --example typescript -- write packages/ui/src/generated

# Prepare sqlx's offline query data in crates/fleet-server/.sqlx/ (needs sqlx-cli 0.9.0).
# Run it after changing a query or adding a migration; every build reads that data
# (SQLX_OFFLINE in .cargo/config.toml), and `-- --all-targets` covers the tests' queries.
[working-directory('crates/fleet-server')]
db-prepare $DATABASE_URL=sqlx_db: _sqlx-db
    cargo sqlx prepare -- --all-targets

# Fail if crates/fleet-server/.sqlx/ is missing or differs from a query (the sqlx-offline job).
[working-directory('crates/fleet-server')]
db-check $DATABASE_URL=sqlx_db: _sqlx-db
    cargo sqlx prepare --check -- --all-targets

# Rebuild the throwaway database prepare checks queries against from the migrations.
# Node creates target/ first, which a cold CI cache doesn't have yet.
[working-directory('crates/fleet-server')]
_sqlx-db $DATABASE_URL=sqlx_db:
    node -e "require('node:fs').mkdirSync(process.argv[1], { recursive: true })" "{{ justfile_directory() / 'target' }}"
    cargo sqlx database reset -y

# Fail if packages/ui/src/generated/ isn't exactly a fresh export (the ts-types-fresh job).
gen-check:
    cargo run -p fleet-api-types --example typescript -- check packages/ui/src/generated

# Fail if a migration that exists on main was modified, deleted or renamed: merged
# migrations are never edited (ADR-0015). Needs an up-to-date origin/main; CI fetches it.
migrations-check:
    git diff --exit-code --diff-filter=a --name-only origin/main HEAD -- crates/fleet-server/migrations/

# Start the local offline-mode Minecraft server and wait until it's healthy (needs Docker).
mc-up:
    docker compose --file deploy/compose.dev.yaml up --detach --wait --wait-timeout 300 minecraft

# Build the agent's image and start the local server and the agent (deploy/dev/agent.compose.toml) in Docker, then wait until both are healthy.
stack-up:
    docker compose --file deploy/compose.dev.yaml up --build --detach --wait --wait-timeout 300

# Stop the local stack (the server and the agent) and delete the server's world.
mc-down:
    docker compose --file deploy/compose.dev.yaml down --volumes

# The database path, the vault key's path and the PKI's four paths must be absolute, which
# a committed config can't hold for every machine, so the recipe passes them:
# target/dev-server/afkfleet.db, target/dev-server/vault-key-1 and target/dev-server/pki/
# by default, or the absolute paths given. The server refuses a missing folder, so Node
# creates it first, and the vault key when it's missing (never overwritten, never
# printed; ADR-0016), then the PKI with `pki init` when its files are missing, for
# localhost, 127.0.0.1 and ::1 (never overwritten; ADR-0019). Never read the key files.
# With the PKI, [grpc] is on: the agents' port listens on 127.0.0.1:7443.
# No exit message: on Windows, Ctrl+C reaches just and PowerShell too, so just would
# call a clean stop a failure. The server logs its own errors and exit code.
# Run the server with deploy/dev/server.toml (dev mode), a database, a vault key and a PKI in target/dev-server/.
[no-exit-message]
dev-server $AFKFLEET_SERVER__DATABASE__PATH=(justfile_directory() / "target" / "dev-server" / "afkfleet.db") $AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1=(justfile_directory() / "target" / "dev-server" / "vault-key-1") $AFKFLEET_SERVER__GRPC__CA_CERT_FILE=(justfile_directory() / "target" / "dev-server" / "pki" / "ca.crt") $AFKFLEET_SERVER__GRPC__CA_KEY_FILE=(justfile_directory() / "target" / "dev-server" / "pki" / "ca.key") $AFKFLEET_SERVER__GRPC__SERVER_CERT_FILE=(justfile_directory() / "target" / "dev-server" / "pki" / "server.crt") $AFKFLEET_SERVER__GRPC__SERVER_KEY_FILE=(justfile_directory() / "target" / "dev-server" / "pki" / "server.key"):
    node -e "require('node:fs').mkdirSync(require('node:path').dirname(process.argv[1]), { recursive: true })" "{{ AFKFLEET_SERVER__DATABASE__PATH }}"
    node scripts/dev-vault-key.mjs "{{ AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1 }}"
    node scripts/dev-pki.mjs deploy/dev/server.toml "{{ AFKFLEET_SERVER__GRPC__CA_CERT_FILE }}" "{{ AFKFLEET_SERVER__GRPC__CA_KEY_FILE }}" "{{ AFKFLEET_SERVER__GRPC__SERVER_CERT_FILE }}" "{{ AFKFLEET_SERVER__GRPC__SERVER_KEY_FILE }}"
    cargo run -p fleet-server --bin afkfleet-server -- serve --config deploy/dev/server.toml

# An enrollment token from the server's host (ADR-0019, U3), for dev-server's database and
# PKI (the same paths): the bundle alone on stdout, so `$bundle = just dev-enrollment-token`
# takes it whole; its expiry and the CA's fingerprint on stderr. It never creates or
# migrates the database, and works while dev-server runs. Never print or paste the bundle
# anywhere else: it's a secret for 15 minutes.
# No exit message: the command prints its own error line and exit code.
# Create an enrollment token in dev-server's database and print its bundle.
[no-exit-message]
dev-enrollment-token $AFKFLEET_SERVER__DATABASE__PATH=(justfile_directory() / "target" / "dev-server" / "afkfleet.db") $AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1=(justfile_directory() / "target" / "dev-server" / "vault-key-1") $AFKFLEET_SERVER__GRPC__CA_CERT_FILE=(justfile_directory() / "target" / "dev-server" / "pki" / "ca.crt") $AFKFLEET_SERVER__GRPC__CA_KEY_FILE=(justfile_directory() / "target" / "dev-server" / "pki" / "ca.key") $AFKFLEET_SERVER__GRPC__SERVER_CERT_FILE=(justfile_directory() / "target" / "dev-server" / "pki" / "server.crt") $AFKFLEET_SERVER__GRPC__SERVER_KEY_FILE=(justfile_directory() / "target" / "dev-server" / "pki" / "server.key"):
    cargo run --quiet -p fleet-server --bin afkfleet-server -- agent create-enrollment-token --config deploy/dev/server.toml

# Enrolls a dev agent with dev-server, its bundle piped in: `$bundle | just dev-enroll`.
# deploy/dev/agent.managed.toml dials 127.0.0.1:7443 and pins dev-server's CA; the agent's
# key and certificate go into `dir` (target/dev-agent by default, created when missing),
# owner-only, and are never replaced: enrolling again needs another folder, or the old
# files moved away (ADR-0019, S18). Never open agent.key.
# One line on purpose: every recipe line gets just's stdin, so an earlier line could take
# the bundle. scripts/dev-enroll.mjs creates the folder and runs the agent's enroll on its
# own stdin, stdout and stderr.
# No exit message: the command prints its own lines and exit code.
# Enroll a dev agent with dev-server, its bundle on stdin.
[no-exit-message]
dev-enroll dir=(justfile_directory() / "target" / "dev-agent") $AFKFLEET_AGENT__CONTROL_PLANE__CA_CERT_FILE=(justfile_directory() / "target" / "dev-server" / "pki" / "ca.crt") $AFKFLEET_AGENT__CONTROL_PLANE__CERT_FILE=(dir / "agent.crt") $AFKFLEET_AGENT__CONTROL_PLANE__KEY_FILE=(dir / "agent.key"):
    node scripts/dev-enroll.mjs deploy/dev/agent.managed.toml "{{ dir }}"

# No exit message: on Windows, Ctrl+C reaches just and PowerShell too, so just
# would call a clean stop a failure. The agent logs its own errors and exit code.
# The identity is the one `dev-enroll` left in `dir` (target/dev-agent by default), with
# dev-server's CA; its heartbeat file goes into `dir` too (ADR-0019, S71). Its bots dial
# only public addresses and 127.0.0.1.
# Run a managed agent with deploy/dev/agent.managed.toml against dev-server's control plane.
[no-exit-message]
dev-managed-agent dir=(justfile_directory() / "target" / "dev-agent") $AFKFLEET_AGENT__CONTROL_PLANE__CA_CERT_FILE=(justfile_directory() / "target" / "dev-server" / "pki" / "ca.crt") $AFKFLEET_AGENT__CONTROL_PLANE__CERT_FILE=(dir / "agent.crt") $AFKFLEET_AGENT__CONTROL_PLANE__KEY_FILE=(dir / "agent.key") $AFKFLEET_AGENT__RUNTIME__HEARTBEAT_FILE=(dir / "agent.alive"):
    cargo run -p fleet-agent --bin afkfleet-agent -- run --config deploy/dev/agent.managed.toml

# No exit message: the console prints its own errors, and on Windows Ctrl+C reaches just
# and PowerShell too. It needs a terminal, so the AI never runs it.
# The password, the 2FA code and a step-up are typed hidden, never an argument (ADR-0019,
# S71); a user without 2FA sets it up in `just dev-app` first. `server` is dev-server's API.
# A console for dev-server's bot, mode and admin endpoints: bots, bot, offline, server, mode,
# start, stop, chat, command, history, modes, new-mode, edit-mode, delete-mode, events, users,
# audit, agents, release, held, shared, leave, servers, new-server, edit-server, delete-server,
# preview-formats, quit. `events` runs over fleet-client's own
# socket, so it works over https too and never sees a ticket or a token (ADR-0020, R80).
[no-exit-message]
dev-bots username server="http://127.0.0.1:8080":
    cargo run --quiet -p fleet-client --example dev-bots -- "{{ username }}" "{{ server }}"

# The dev database's Owner, through `user create-owner` with the password typed twice
# at the terminal. It passes the same paths as dev-server: the dev config's validation
# needs the vault key's path too, though create-owner never reads the key (ADR-0016).
# No exit message: the command prints its own error line and exit code.
# Create the Owner `username` in dev-server's database.
[no-exit-message]
dev-owner username $AFKFLEET_SERVER__DATABASE__PATH=(justfile_directory() / "target" / "dev-server" / "afkfleet.db") $AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1=(justfile_directory() / "target" / "dev-server" / "vault-key-1"):
    node -e "require('node:fs').mkdirSync(require('node:path').dirname(process.argv[1]), { recursive: true })" "{{ AFKFLEET_SERVER__DATABASE__PATH }}"
    cargo run -p fleet-server --bin afkfleet-server -- user create-owner --config deploy/dev/server.toml --username "{{ username }}"

# `agent list` on dev-server's database: every agent with its ID, name, state (enabled,
# expired or disabled), capacity, version, last sign of life, certificate expiry and
# enrollment, tab-separated after a header. It passes the same paths as dev-server (the dev
# config's validation needs the vault key's path; nothing reads the key), never creates or
# migrates the database, and works while dev-server runs (ADR-0019). No exit message: the
# command prints its own error line and exit code.
# List dev-server's agents.
[no-exit-message]
dev-agents $AFKFLEET_SERVER__DATABASE__PATH=(justfile_directory() / "target" / "dev-server" / "afkfleet.db") $AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1=(justfile_directory() / "target" / "dev-server" / "vault-key-1"):
    cargo run --quiet -p fleet-server --bin afkfleet-server -- agent list --config deploy/dev/server.toml

# `agent disable --id id` on dev-server's database: the agent is disabled for good, every
# certificate it holds revoked and its name free for a new enrollment; a running dev-server
# ends its stream within 10 s (ADR-0019, U8). The same paths as dev-agents. No exit message:
# the command prints its own result, error line and exit code.
# Disable dev-server's agent `id` for good.
[no-exit-message]
dev-disable-agent id $AFKFLEET_SERVER__DATABASE__PATH=(justfile_directory() / "target" / "dev-server" / "afkfleet.db") $AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1=(justfile_directory() / "target" / "dev-server" / "vault-key-1"):
    cargo run --quiet -p fleet-server --bin afkfleet-server -- agent disable --config deploy/dev/server.toml --id "{{ id }}"

# `agent release --id id` on dev-server's database: every bot of the disabled agent `id`, and
# every record of a bot it may still run, is cleared, so a running dev-server places the bots
# on other agents within 30 s. Refused while the agent is enabled. Only once its host and its
# bots are stopped (ADR-0019, S32). The same paths as dev-agents. No exit message: the command
# prints its own result, error line and exit code.
# Release dev-server's disabled agent `id`'s bots.
[no-exit-message]
dev-release-agent id $AFKFLEET_SERVER__DATABASE__PATH=(justfile_directory() / "target" / "dev-server" / "afkfleet.db") $AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1=(justfile_directory() / "target" / "dev-server" / "vault-key-1"):
    cargo run --quiet -p fleet-server --bin afkfleet-server -- agent release --config deploy/dev/server.toml --id "{{ id }}"

# `vault status` on dev-server's database with its key 1: what each key ID seals, read from
# the envelopes' headers. It reads no key file and never creates or migrates the database
# (ADR-0018). No exit message: the command prints its own report, error line and exit code
# (3 when a secret names a key that isn't configured).
# Show what dev-server's vault keys seal.
[no-exit-message]
dev-vault-status $AFKFLEET_SERVER__DATABASE__PATH=(justfile_directory() / "target" / "dev-server" / "afkfleet.db") $AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1=(justfile_directory() / "target" / "dev-server" / "vault-key-1"):
    cargo run -p fleet-server --bin afkfleet-server -- vault status --config deploy/dev/server.toml

# The rotation check on a copy (Phase 9's DoD; ADR-0018). Stop dev-server first: the copy
# is refused while dev-server's database has a -wal file, and when target/dev-server/rotation/
# exists (nothing is ever deleted or overwritten). Then a second dev key, vault-key-2, is
# created when it's missing (never overwritten, never printed; never read it), and
# `vault rotate-key` re-encrypts the copy's secrets under it, with keys 1 and 2 configured.
# The paths are dev-server's database, the copy, and the two key files.
# Copy dev-server's database and rotate the copy's vault key to a new key 2.
[no-exit-message]
dev-rotate-key source=(justfile_directory() / "target" / "dev-server" / "afkfleet.db") $AFKFLEET_SERVER__DATABASE__PATH=(justfile_directory() / "target" / "dev-server" / "rotation" / "afkfleet.db") $AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1=(justfile_directory() / "target" / "dev-server" / "vault-key-1") $AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__2=(justfile_directory() / "target" / "dev-server" / "vault-key-2") $AFKFLEET_SERVER__VAULT__ACTIVE_KEY_ID="2":
    node scripts/dev-rotation.mjs copy "{{ source }}" "{{ AFKFLEET_SERVER__DATABASE__PATH }}"
    node scripts/dev-vault-key.mjs "{{ AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__2 }}"
    cargo run -p fleet-server --bin afkfleet-server -- vault rotate-key --config deploy/dev/server.toml

# The rotated copy, served with key 2 only, so logging in with 2FA proves the secrets open
# without key 1. It refuses to start without the copy, since serve would create an empty
# database. No exit message, as for dev-server. Don't run it beside dev-server: both use
# 127.0.0.1:8080.
# Run the server on dev-rotate-key's copy with key 2 only.
[no-exit-message]
dev-server-rotated $AFKFLEET_SERVER__DATABASE__PATH=(justfile_directory() / "target" / "dev-server" / "rotation" / "afkfleet.db") $AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__2=(justfile_directory() / "target" / "dev-server" / "vault-key-2") $AFKFLEET_SERVER__VAULT__ACTIVE_KEY_ID="2":
    node scripts/dev-rotation.mjs check "{{ AFKFLEET_SERVER__DATABASE__PATH }}"
    cargo run -p fleet-server --bin afkfleet-server -- serve --config deploy/dev/server.toml

# No exit message: on Windows, Ctrl+C reaches just and PowerShell too, so just
# would call a clean stop a failure. The agent logs its own errors and exit code.
# Run the agent with deploy/dev/agent.toml: three bots against `just mc-up`'s server.
[no-exit-message]
dev-agent:
    cargo run -p fleet-agent --bin afkfleet-agent -- run --config deploy/dev/agent.toml

# No exit message: the script prints its own verdict and errors, and on Windows
# Ctrl+C reaches just and PowerShell too. Its exit code: 0 PASS, 1 FAIL, 2 not judged.
# The DoD demo: the compose agent's bots for `minutes` with one server restart, then a summary and a verdict.
[no-exit-message]
demo-agent minutes="60":
    node scripts/demo-agent.mjs --minutes {{ minutes }}

# No exit message: on Windows, Ctrl+C reaches just and PowerShell too, so just would
# call a clean stop a failure.
# Run the desktop app in development mode: Vite's dev server and the app, with hot reload.
[no-exit-message]
dev-app:
    pnpm --filter @afkfleet/desktop tauri dev

# The release exe lands in target/release/; `just app-build --debug` builds a debug exe
# with the embedded frontend and the release CSP, plus devtools, in target/debug/.
# Build the desktop app's exe (no installer): the frontend, then the Rust side.
app-build *args:
    pnpm --filter @afkfleet/desktop tauri build {{ args }}

# Render the desktop app's icon.ico from apps/desktop/src-tauri/icons/icon.svg (needs pnpm install).
icons:
    node scripts/icons.mjs

# --- The definition of done's checks with a real account (ADR-0020, U1, U33, U50-U52, S72) ---
# Phase 11's first check and M's 24-hour run. THE USER ONLY: the AI never runs a dod-*
# recipe, and never reads, lists or prints anything in target/dod-run/, which holds the
# user's real account, sealed, next to the vault key that opens it, the PKI and the agent's
# identity. Every path is a parameter under target/dod-run/; every binary is a release build
# (`--release`, so a long run's exes never block debug builds) logging at info, so no chat
# text is printed (chat is logged only at debug). Their own ports, so they run beside the
# dev stack: HTTP 127.0.0.1:8090, gRPC 127.0.0.1:7453, Minecraft 127.0.0.1:25566. Nothing
# secret is ever an argument: dod-owner asks for the password hidden, and dod-enroll reads
# the bundle from stdin. The steps are README's "The DoD's first check".

# The user only: start the DoD checks' online-mode Minecraft server and wait until it's healthy (needs Docker).
dod-mc-up:
    docker compose --file deploy/compose.dod.yaml up --detach --wait --wait-timeout 300 minecraft

# The user only: remove the DoD checks' Minecraft server with its world, which holds the player's name.
dod-mc-down:
    docker compose --file deploy/compose.dod.yaml down --volumes

# Exactly what dod-server, dod-owner, dod-enrollment-token, dod-enroll and dod-agent run, so
# none of them builds while an enrollment token's 15 minutes run (PR #78's review). It touches
# nothing in target/dod-run/; the first builds take minutes, and cargo shows its progress.
# The user only: build the DoD checks' release server and agent before the stack starts.
dod-build:
    cargo build --release -p fleet-server --bin afkfleet-server
    cargo build --release -p fleet-agent --bin afkfleet-agent

# As dev-server, with deploy/dod/server.toml: Node creates the folder, the vault key when
# it's missing (never overwritten, never printed) and the PKI with `pki init` when its files
# are missing, for localhost, 127.0.0.1 and ::1 (never overwritten). [grpc] is on, on
# 127.0.0.1:7453. No exit message: on Windows, Ctrl+C reaches just and PowerShell too.
# The user only: run the DoD checks' server with deploy/dod/server.toml, its database, vault key and PKI in target/dod-run/.
[no-exit-message]
dod-server $AFKFLEET_SERVER__DATABASE__PATH=(justfile_directory() / "target" / "dod-run" / "afkfleet.db") $AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1=(justfile_directory() / "target" / "dod-run" / "vault-key-1") $AFKFLEET_SERVER__GRPC__CA_CERT_FILE=(justfile_directory() / "target" / "dod-run" / "pki" / "ca.crt") $AFKFLEET_SERVER__GRPC__CA_KEY_FILE=(justfile_directory() / "target" / "dod-run" / "pki" / "ca.key") $AFKFLEET_SERVER__GRPC__SERVER_CERT_FILE=(justfile_directory() / "target" / "dod-run" / "pki" / "server.crt") $AFKFLEET_SERVER__GRPC__SERVER_KEY_FILE=(justfile_directory() / "target" / "dod-run" / "pki" / "server.key") $AFKFLEET_SERVER__LOG__FILTER="info":
    node -e "require('node:fs').mkdirSync(require('node:path').dirname(process.argv[1]), { recursive: true })" "{{ AFKFLEET_SERVER__DATABASE__PATH }}"
    node scripts/dev-vault-key.mjs "{{ AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1 }}"
    node scripts/dev-pki.mjs --release deploy/dod/server.toml "{{ AFKFLEET_SERVER__GRPC__CA_CERT_FILE }}" "{{ AFKFLEET_SERVER__GRPC__CA_KEY_FILE }}" "{{ AFKFLEET_SERVER__GRPC__SERVER_CERT_FILE }}" "{{ AFKFLEET_SERVER__GRPC__SERVER_KEY_FILE }}"
    cargo run --release -p fleet-server --bin afkfleet-server -- serve --config deploy/dod/server.toml

# `user create-owner` with the password typed twice at the terminal, hidden; never an
# argument. The same six paths as dod-server: deploy/dod/server.toml sets [grpc] bind, so
# its validation needs them all, though create-owner reads no key (R126). No exit message:
# the command prints its own error line and exit code.
# The user only: create the Owner `username` in the DoD checks' database.
[no-exit-message]
dod-owner username $AFKFLEET_SERVER__DATABASE__PATH=(justfile_directory() / "target" / "dod-run" / "afkfleet.db") $AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1=(justfile_directory() / "target" / "dod-run" / "vault-key-1") $AFKFLEET_SERVER__GRPC__CA_CERT_FILE=(justfile_directory() / "target" / "dod-run" / "pki" / "ca.crt") $AFKFLEET_SERVER__GRPC__CA_KEY_FILE=(justfile_directory() / "target" / "dod-run" / "pki" / "ca.key") $AFKFLEET_SERVER__GRPC__SERVER_CERT_FILE=(justfile_directory() / "target" / "dod-run" / "pki" / "server.crt") $AFKFLEET_SERVER__GRPC__SERVER_KEY_FILE=(justfile_directory() / "target" / "dod-run" / "pki" / "server.key") $AFKFLEET_SERVER__LOG__FILTER="info":
    node -e "require('node:fs').mkdirSync(require('node:path').dirname(process.argv[1]), { recursive: true })" "{{ AFKFLEET_SERVER__DATABASE__PATH }}"
    cargo run --release -p fleet-server --bin afkfleet-server -- user create-owner --config deploy/dod/server.toml --username "{{ username }}"

# The bundle alone on stdout, so `$bundle = just dod-enrollment-token` takes it whole; its
# expiry and the CA's fingerprint on stderr, with cargo's progress (not quiet: run dod-build
# first, so no build waits while the token's 15 minutes run). Never print or paste the bundle anywhere else:
# it's a secret for 15 minutes. The same six paths as dod-server (R126). No exit message:
# the command prints its own error line and exit code.
# The user only: create an enrollment token in the DoD checks' database and print its bundle.
[no-exit-message]
dod-enrollment-token $AFKFLEET_SERVER__DATABASE__PATH=(justfile_directory() / "target" / "dod-run" / "afkfleet.db") $AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1=(justfile_directory() / "target" / "dod-run" / "vault-key-1") $AFKFLEET_SERVER__GRPC__CA_CERT_FILE=(justfile_directory() / "target" / "dod-run" / "pki" / "ca.crt") $AFKFLEET_SERVER__GRPC__CA_KEY_FILE=(justfile_directory() / "target" / "dod-run" / "pki" / "ca.key") $AFKFLEET_SERVER__GRPC__SERVER_CERT_FILE=(justfile_directory() / "target" / "dod-run" / "pki" / "server.crt") $AFKFLEET_SERVER__GRPC__SERVER_KEY_FILE=(justfile_directory() / "target" / "dod-run" / "pki" / "server.key") $AFKFLEET_SERVER__LOG__FILTER="info":
    cargo run --release -p fleet-server --bin afkfleet-server -- agent create-enrollment-token --config deploy/dod/server.toml

# `$bundle | just dod-enroll`: deploy/dod/agent.toml dials 127.0.0.1:7453 and pins
# dod-server's CA; the key and certificate go into `dir`, owner-only, and are never
# replaced. One line on purpose: every recipe line gets just's stdin, so an earlier line
# could take the bundle. No exit message: the command prints its own lines and exit code.
# The user only: enroll the DoD checks' agent with dod-server, its bundle on stdin.
[no-exit-message]
dod-enroll dir=(justfile_directory() / "target" / "dod-run" / "agent") $AFKFLEET_AGENT__CONTROL_PLANE__CA_CERT_FILE=(justfile_directory() / "target" / "dod-run" / "pki" / "ca.crt") $AFKFLEET_AGENT__CONTROL_PLANE__CERT_FILE=(dir / "agent.crt") $AFKFLEET_AGENT__CONTROL_PLANE__KEY_FILE=(dir / "agent.key") $AFKFLEET_AGENT__LOG__FILTER="info":
    node scripts/dev-enroll.mjs --release deploy/dod/agent.toml "{{ dir }}"

# The identity dod-enroll left, with dod-server's CA; its heartbeat file, which dod-clean
# checks, goes into `dir` too. Its bot dials only 127.0.0.1 (dod-mc-up's server), never a
# public address (U50): the recipe exports public_targets false itself, as the log filter,
# so a variable in the shell can't flip it (PR #78's review). No exit message: on Windows,
# Ctrl+C reaches just and PowerShell too.
# The user only: run the DoD checks' agent with deploy/dod/agent.toml against dod-server.
[no-exit-message]
dod-agent dir=(justfile_directory() / "target" / "dod-run" / "agent") $AFKFLEET_AGENT__CONTROL_PLANE__CA_CERT_FILE=(justfile_directory() / "target" / "dod-run" / "pki" / "ca.crt") $AFKFLEET_AGENT__CONTROL_PLANE__CERT_FILE=(dir / "agent.crt") $AFKFLEET_AGENT__CONTROL_PLANE__KEY_FILE=(dir / "agent.key") $AFKFLEET_AGENT__RUNTIME__HEARTBEAT_FILE=(dir / "agent.alive") $AFKFLEET_AGENT__CONTROL_PLANE__PUBLIC_TARGETS="false" $AFKFLEET_AGENT__LOG__FILTER="info":
    cargo run --release -p fleet-agent --bin afkfleet-agent -- run --config deploy/dod/agent.toml

# app-build first (the release exe), then the exe itself: a GUI app, so the recipe returns
# once it starts (R125). Close every other afkfleet window first: a second start only
# brings the first forward.
# The user only: build and start the release app for the DoD checks' server.
dod-app: app-build
    @echo "Enter http://127.0.0.1:8090 as the server's address in the app."
    target/release/afkfleet-desktop.exe

# Only this repository's target/dod-run; it refuses while dod-server or dod-agent runs, or
# a link in the way, each saying what to do, and never touches Docker (U52). No exit
# message: the script prints its own lines.
# The user only: delete target/dod-run/ once the DoD is finished (after unlinking the account in the app).
[no-exit-message]
dod-clean dir=(justfile_directory() / "target" / "dod-run"):
    node scripts/dod-clean.mjs "{{ dir }}"

# Frontend unit and component tests (Vitest), in every package that has them.
ui-test:
    pnpm -r --if-present run test

# The shipped bundles are built as they ship (the desktop app's frontend, packages/ui's web
# entry) into target/e2e/, then the e2e bundle; the guard checks the e2e build reached
# neither (scripts/e2e-bundle-guard.mjs), then Playwright runs the specs in the installed
# Edge against `vite preview` of the e2e bundle, under the release CSP (Plan.md P8.9,
# ADR-0017). Edge must be installed; nothing is downloaded.
# End-to-end tests (Playwright, Edge) against the e2e build.
e2e:
    pnpm --filter @afkfleet/desktop exec vite build --outDir ../../target/e2e/shipped-desktop --emptyOutDir
    pnpm --filter @afkfleet/ui exec vite build --outDir ../../target/e2e/shipped-ui --emptyOutDir
    pnpm --filter @afkfleet/desktop run e2e:build
    node scripts/e2e-bundle-guard.mjs
    pnpm --filter @afkfleet/desktop exec playwright test

# --- Building blocks of `check` and `ci` (CI runs them as separate jobs) ---

# Check Rust formatting.
fmt-check:
    cargo fmt --all --check

# Clippy with all workspace lints, warnings denied.
clippy:
    cargo clippy --workspace {{ linux_exclude }} --all-targets --all-features -- -D warnings

# Build the docs with warnings denied (RUSTDOCFLAGS above).
docs:
    cargo doc --no-deps --workspace {{ linux_exclude }}

# The desktop crate stays out of every doctest run, on Windows too: tauri-build links the
# VC++ runtime statically by putting a stub msvcrt.lib in its build output, and cargo
# hands every build script's link paths to every doctest, which then can't link. It has
# no doctests (ADR-0017).
# Doctests; nextest doesn't run them.
doctest:
    cargo test --workspace --exclude {{ desktop }} --doc

# Doctests leave the desktop crate out everywhere (see `doctest`).
# Tests with the CI profile (JUnit, no retries), then doctests.
test-ci:
    cargo nextest run --workspace {{ linux_exclude }} --profile ci
    cargo test --workspace --exclude {{ desktop }} --doc

# The workspace crates that must also build on stable Rust.
stable-check:
    cargo +{{ stable }} check {{ stable_crates }} --all-targets

# No workspace member may depend on fleet-testkit outside its dev-dependencies,
# because its SeededRandom isn't secure (ADR-0015). Cargo resolves the dependency
# graph; the script fails on any other dependent and on output it can't read.
testkit-check:
    node scripts/testkit-check.mjs

# The desktop app's hardening (Plan.md P8.3, ADR-0017): the CSP, withGlobalTauri, the
# capabilities, the command manifest and tauri's features (through `cargo tree`, which
# needs only the dependency graph, so it runs on Linux too). Changing any of them means
# changing scripts/tauri-hardening.mjs too, deliberately.
# Fail if the desktop app's hardening changed.
tauri-check:
    node scripts/tauri-hardening.mjs

# Tests for the scripts in scripts/.
scripts-test:
    node --test "scripts/*.test.mjs"

# Biome, then each package's typecheck and tests.
ui-check:
    pnpm exec biome ci .
    pnpm -r --if-present run typecheck
    pnpm -r --if-present run test

# Known vulnerabilities in npm dependencies.
ui-audit:
    pnpm audit

