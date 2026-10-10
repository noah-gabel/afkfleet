# afkfleet development commands. Keep the command table in CLAUDE.md in sync.
#
# Recipes run in PowerShell 7 on Windows and in sh on Linux CI, so every line
# must work in both: one command per line, no `&&` or `||`, environment
# variables only through just's `export` or `$`-parameters (just exports both,
# so they work in pwsh and sh alike). Anything more complex goes into scripts/.

set windows-shell := ["pwsh", "-NoLogo", "-NoProfile", "-Command"]

export RUSTDOCFLAGS := "-D warnings"

# The stable toolchain for `stable-check`, and the crates it checks: every crate
# that doesn't depend on azalea or azalea-auth.
stable := "1.99.0"
stable_crates := "-p fleet-core -p fleet-testkit -p fleet-runtime -p fleet-startup -p fleet-server"

# List all recipes.
default:
    @just --list

# Formatting, lints, docs, tests and frontend checks: run before a task is done.
check: fmt-check clippy docs test doctest scripts-test ui-check

# Everything CI runs.
ci: fmt-check clippy docs test-ci scripts-test cov deny stable-check ui-check ui-audit

# Run nextest for the workspace, or for one crate: `just test fleet-core`.
test crate="":
    cargo nextest run {{ if crate == "" { "--workspace" } else { "-p " + crate } }}

# They need fleet-mc's test-only `fault-injection` feature (ADR-0011). The server
# image is pulled and the agent's image built first, so neither eats into a timed
# test; AFKFLEET_E2E_IMAGE_BUILT tells the compose e2e test that the image is
# this source's (ADR-0014).
# Slow tests: containers and a real Minecraft server (needs Docker), one at a time.
test-slow $AFKFLEET_E2E_IMAGE_BUILT="1":
    docker compose --file deploy/compose.dev.yaml pull minecraft
    docker compose --file deploy/compose.dev.yaml build agent
    cargo nextest run --workspace --profile slow --no-tests=warn --features fleet-mc/fault-injection

# The user's real-account check (ADR-0011): a real token from
# secrets/p1.8-account.txt joins a local online-mode container (needs Docker).
# It needs the user's real credentials, so only the user runs it; the AI never
# does. Create the file with the archived spike's fetch-token right before.
test-real-account:
    cargo nextest run -p fleet-mc --profile manual

# Coverage, checked against the gates from Plan.md §8.
cov:
    cargo llvm-cov nextest --workspace --no-report
    cargo llvm-cov report --summary-only
    cargo llvm-cov report --json --output-path target/coverage.json
    node scripts/coverage-gates.mjs target/coverage.json

# cargo-deny: advisories, licenses, bans and sources.
deny:
    cargo deny check

# Format Rust and frontend code.
fmt:
    cargo fmt --all
    pnpm exec biome format --write .

# Export ts-rs types to packages/ui/src/generated/ and check proto codegen.
gen: (_unavailable "gen" "P6")

# Prepare sqlx offline query data in .sqlx/.
db-prepare: (_unavailable "db-prepare" "P6")

# Start the local offline-mode Minecraft server and wait until it's healthy (needs Docker).
mc-up:
    docker compose --file deploy/compose.dev.yaml up --detach --wait --wait-timeout 300 minecraft

# Build the agent's image and start the local server and the agent (deploy/dev/agent.compose.toml) in Docker, then wait until both are healthy.
stack-up:
    docker compose --file deploy/compose.dev.yaml up --build --detach --wait --wait-timeout 300

# Stop the local stack (the server and the agent) and delete the server's world.
mc-down:
    docker compose --file deploy/compose.dev.yaml down --volumes

# Run the server with deploy/dev/ configs.
dev-server: (_unavailable "dev-server" "P6")

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

# Run the desktop app in development mode.
dev-app: (_unavailable "dev-app" "P8")

# Frontend unit and component tests (Vitest).
ui-test: (_unavailable "ui-test" "P8")

# End-to-end tests (Playwright).
e2e: (_unavailable "e2e" "P8")

# --- Building blocks of `check` and `ci` (CI runs them as separate jobs) ---

# Check Rust formatting.
fmt-check:
    cargo fmt --all --check

# Clippy with all workspace lints, warnings denied.
clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Build the docs with warnings denied (RUSTDOCFLAGS above).
docs:
    cargo doc --no-deps --workspace

# Doctests; nextest doesn't run them.
doctest:
    cargo test --workspace --doc

# Tests with the CI profile (JUnit, no retries), then doctests.
test-ci:
    cargo nextest run --workspace --profile ci
    cargo test --workspace --doc

# The workspace crates that must also build on stable Rust.
stable-check:
    cargo +{{ stable }} check {{ stable_crates }} --all-targets

# Tests for the scripts in scripts/.
scripts-test:
    node --test "scripts/*.test.mjs"

# Biome, then each package's typecheck and tests (the frontend packages arrive in P8).
ui-check:
    pnpm exec biome ci .
    pnpm -r --if-present run typecheck
    pnpm -r --if-present run test

# Known vulnerabilities in npm dependencies.
ui-audit:
    pnpm audit

_unavailable recipe phase:
    @echo "just {{ recipe }} is available from {{ phase }}."
    @exit 1
