# 0009. Phase 0 dependency and tooling conventions

- **Status:** Accepted
- **Date:** 2026-10-05
- **Related:** Plan.md §5, §7.5, §8, tasks P0.3–P0.11; ADR-0003

## Context
Phase 0 sets up the workspace and every quality gate before any product code exists. Several choices weren't fixed by `Plan.md` or turned out to conflict with reality while setting things up:
- how to pin versions
- how to keep a single TLS provider
- which OpenAPI release line to use
- how CI and local checks stay identical

The user approved these defaults in the Phase 0 plan. Two of them were confirmed separately: declaring dependencies on first use, and the TLS spike.

## Decision
### Dependencies
- **Pinning.**
  - `Cargo.toml` uses caret requirements on the full version (e.g. `"1.53.2"`), and the committed `Cargo.lock` is the exact pin. Updates are manual pull requests.
  - `=` is used only where semver can't express the constraint: `azalea`/`azalea-auth`, whose `+mcX.Y` build metadata semver ignores (ADR-0003).
- **Declared on first use.**
  - The pinned nightly's cargo warns about unused workspace dependencies (`cargo::unused_workspace_dependencies`).
  - So `[workspace.dependencies]` holds only crates that are in use. Plan.md §5 is the single source for each crate's verified version and feature constraints.
  - The lint isn't relaxed.
- **Feature policy.**
  - Features that choose a TLS stack or crypto provider are fixed at workspace level. Every other feature is enabled by the member that uses the crate.
  - Crates used by `fleet-core` (`backon`, `uuid`, `chrono`, `rand`) have their default features off, so no IO or tokio leaks in.
- **OpenAPI:** `utoipa` 6.0.0 with `utoipa-axum` 0.3.0, the current line, released 2026-09-22. We avoid a 5 → 6 migration later and re-check it in P6.
- **WebSocket client:** `tokio-tungstenite` 0.29.0, the version axum 0.8.9's `ws` feature uses, so only one tungstenite gets built.

### One TLS provider: rustls with aws-lc-rs
- **Crate settings** (Plan.md §5):

  | Crate | Setting |
  |---|---|
  | `rustls` | default features (aws-lc-rs) |
  | `reqwest` | `rustls` (aws-lc-rs + platform verifier) |
  | `tonic` | `tls-aws-lc` |
  | `rcgen` | `aws_lc_rs` |
  | `tokio-tungstenite` | `rustls-tls-native-roots` |
  | `tauri-plugin-updater`, `testcontainers` | default features off |

- **Banned in `deny.toml`:** `ring`, `openssl-sys`, `native-tls`. A justified exception needs a `wrappers` entry and its own ADR.
- **No `webpki-roots`.** Its CDLA-Permissive-2.0 license isn't on the allowlist; the crates use the platform verifier, native roots or our own CA instead.
- **NASM is enforced on Windows.** rustls's `aws_lc_rs` feature turns on aws-lc's `prebuilt-nasm`, which silently links prebuilt object files when NASM isn't on `PATH`. `.cargo/config.toml` sets `AWS_LC_SYS_PREBUILT_NASM=0`, so such a build fails instead, and CI installs NASM on Windows. We build from source; no prebuilt binary objects.
- **Evidence:** `spikes/tls-provider/` builds on Windows and Linux with this configuration, and `cargo tree -i ring --target all` is empty (P0.10).

### cargo-deny
These go beyond the plan:
- `unsound = "all"` and `yanked = "deny"`.
- `[graph] targets` is limited to `x86_64-pc-windows-msvc` and `x86_64-unknown-linux-gnu`, so mobile-only dependencies don't count.
- Unused allowlist licenses aren't reported.

### Tooling
- **The `justfile` is the single entry point.**
  - `check` and `ci` are composed of building-block recipes (`fmt-check`, `clippy`, `docs`, `doctest`, `test-ci`, `stable-check`, `scripts-test`, `ui-check`, `ui-audit`), and **every CI job runs exactly one of them**.
  - `just` and the cargo tools are installed in CI at pinned versions.
- **Platforms.** CI runs `clippy` and `test` on both Ubuntu and Windows, matching development (Windows) and production (Linux). All other jobs run on Ubuntu.
- **Toolchains.** The nightly comes from `rust-toolchain.toml` (`rustup install`), with no toolchain action. `stable-check` uses the stable version named in the `justfile` (1.99.0).
- **Doctests** run in `check` and CI (`cargo test --doc`), because nextest skips them.
- **Slow tests** are matched as `test(/(^|::)slow_/)`, since nextest filters on the full test path.
- **Scripts** under `scripts/` are plain Node ES modules with no dependencies (Node 24 is required everywhere anyway), tested with `node --test`.
  - The first one is `coverage-gates.mjs`, which checks the §8 gates over the llvm-cov JSON report.
  - It skips gates whose files don't exist yet; the workspace gate never skips.
- **Recipes for later phases** (`gen`, `db-prepare`, `mc-up`/`mc-down`, `dev-*`, `ui-test`, `e2e`) exist as stubs that name their phase and fail. The `CLAUDE.md` command table stays in sync.

## Consequences
- **Local equals CI.** A green `just ci` locally means a green CI run, apart from the OS matrix.
- **Supply chain.** It stays narrow: one TLS implementation, built from source, with bans enforced on every push and weekly.
- **Adding a crate means two edits:** its §5 row first, then its `[workspace.dependencies]` entry in the phase that uses it.
- **Strictness has a cost.** Stricter deny rules (`unsound = "all"`, `ring` banned) will sometimes block a dependency. Each case then needs the user's decision and an ADR, which is intended.
- **Windows CI.** It doubles the clippy and test minutes. That's free on a public repository with standard runners.

## Alternatives considered
- **Exact `=` pins everywhere.** They fight cargo's resolver whenever a transitive dependency needs a newer patch. `Cargo.lock` already pins exactly.
- **Declare every crate up front and allow the cargo lint.** That relaxes a lint and hides really stale entries later.
- **`ring` as the provider.** No NASM on Windows, but azalea and reqwest 0.13 already default to aws-lc-rs, so `ring` would mean a second provider or fighting defaults everywhere.
- **Allowing prebuilt NASM objects.** Easier on Windows, but it puts binary objects we didn't build into every Windows build.
- **PowerShell or Rust (`xtask`) for scripts.** PowerShell adds a second scripting language to test. An `xtask` crate is heavier than a 100-line script.
