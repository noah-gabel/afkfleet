---
paths:
  - "**/Cargo.toml"
  - "Cargo.lock"
  - "deny.toml"
  - "**/package.json"
  - "pnpm-workspace.yaml"
  - "pnpm-lock.yaml"
  - "patches/**"
---

# Crate registry: one crate per concern
Moved from `CLAUDE.md` word for word; it points here. CLAUDE.md keeps the rule: never add a dependency or swap a crate without asking the user first.

- Add dependencies only in `[workspace.dependencies]`; members use `dep.workspace = true`. Declare a crate there when the first member uses it, with the version and feature constraints from Plan.md §5. Cargo warns about unused workspace dependencies.
- **Default features in `fleet-core`:** disable them and enable only what you need. Some crates pull in IO or tokio by default (for example `backon` with its tokio sleeper). `cargo tree -p fleet-core` must show no tokio and no IO crates.
- Every crate inherits `license` (`GPL-3.0-or-later`) and `publish = false` from `[workspace.package]` (`license.workspace = true`, `publish.workspace = true`).
- **npm packages are at least 3 days old.** `pnpm-workspace.yaml` sets `minimumReleaseAge: 4320` (minutes), so pnpm refuses any younger release when it resolves, transitive ones included, and also checks the whole lockfile at every install. A fix that can't wait goes into `minimumReleaseAgeExclude`, with a comment saying why and the user's approval; nothing else goes there (ADR-0017).

**Rust crates**

| Area | Concern | Crate |
|---|---|---|
| Domain & runtime | Minecraft | `azalea` (fleet-mc only) |
| | Chat components, translations | `azalea-chat`, `azalea-language` (fleet-mc only, for its bounded renderer) |
| | MS auth | fleet-server's own adapter over `reqwest`; never `azalea-auth` directly (ADR-0018) |
| | Async runtime | `tokio` |
| | Cancellation | `tokio-util` (`CancellationToken`, `TaskTracker`) |
| | Streams | `tokio-stream` (fleet-server's and fleet-agent's gRPC streams) |
| | Sink/Stream traits | `futures-util` |
| | dyn async ports | `async-trait` |
| | Retry | `backon` |
| | Rate limits | `governor` (fleet-server's HTTP limits too, behind its own middleware; no `tower_governor`, ADR-0016) |
| | Cache / single-flight | `moka` |
| Errors, config, observability | Errors | `thiserror` (libraries), `anyhow` (`main.rs` only) |
| | Serialization | `serde`, `serde_json` |
| | Config | `figment` |
| | Validation | `garde` (DTOs/config); value objects use constructors in core |
| | Logging | `tracing`, `tracing-subscriber` |
| | Metrics | `metrics`, `metrics-exporter-prometheus` |
| | IDs | `uuid` v7 |
| | Time | `chrono` (UTC) |
| | CLI | `clap` |
| | Password prompt | `rpassword` |
| API, transport, persistence | HTTP | `axum` (+ws), `axum-extra`, `tower`, `tower-http` |
| | OpenAPI | `utoipa`, `utoipa-axum` |
| | DB | `sqlx` (sqlite) |
| | gRPC | `tonic`, `tonic-prost`, `prost`, `tonic-prost-build`, `protox` |
| | TLS | `rustls` (**aws-lc-rs provider only**) |
| | PKI | `rcgen` (Ed25519 keys from getrandom bytes only, never `KeyPair::generate`; ADR-0019) |
| | Certificate parsing | `x509-parser` (fleet-server; rcgen 0.14 no longer reads certificates; ADR-0019) |
| | HTTP client | `reqwest` (rustls) |
| | WS client | `tokio-tungstenite` |
| | TS types | `ts-rs` |
| Security | Password hashing | `argon2` |
| | Encryption | `chacha20poly1305` |
| | Hashing | `sha2` |
| | Keyed hashing (HMAC) | `hmac` |
| | Constant-time | `subtle` |
| | Encoding | `base64` |
| | Secrets | `secrecy`, `zeroize` |
| | Secure randomness | `getrandom` |
| | Non-security randomness | `rand` |
| | 2FA | `totp-rs` |
| | QR code | `qrcodegen` (fleet-server writes the SVG itself) |
| | Password strength | `zxcvbn` |
| Desktop | Shell | `tauri` ≥ 2.11.1, `tauri-build` |
| | Plugins | `tauri-plugin-opener`, `tauri-plugin-single-instance`, `tauri-plugin-updater` |
| | Keychain | `keyring-core` + `windows-native-keyring-store` |
| Tests | Testing crates | `rstest`, `proptest`, `insta`, `mockall`, `testcontainers`, tokio `test-util`, `log` (dev only: fleet-testkit's log capture and fleet-agent's telemetry, each to prove the `log` bridge; fleet-server's one normal use is sqlx's `LevelFilter`, and its logging macros are banned there), `tempfile` (fleet-server's database tests, fleet-integration's cases), `ipnet` (dev only: fleet-server's property test of fleet-core's `IpNet`), `hyper-util` (dev only, `tokio`: fleet-server's in-memory control-plane test hands tonic's client a pipe through `rt::TokioIo`) |
| | Fuzzing | `libfuzzer-sys`, `arbitrary` |

**Frontend packages**

| Concern | Package |
|---|---|
| Package manager | pnpm |
| Build | `vite`, `@vitejs/plugin-react` |
| UI framework | `react` 19 (+ `@types/react`, `@types/react-dom`) |
| Language | `typescript` 7 (strict) |
| Server state | `@tanstack/react-query` |
| Routing | `@tanstack/react-router` |
| UI state | `zustand` |
| Validation | `zod` |
| Forms | `react-hook-form` + `@hookform/resolvers` |
| Styling | `tailwindcss`, `@tailwindcss/vite` |
| Components | shadcn/ui (radix-nova), copied from its registry with `cn`, `class-variance-authority` and single Radix packages (`@radix-ui/react-slot`, `@radix-ui/react-label`; never the `radix-ui` umbrella) |
| Icons | `lucide-react` |
| Toasts | `sonner` |
| Tests | `vitest`, `jsdom`, `@testing-library/react` (+ its peer `@testing-library/dom`), `@testing-library/user-event`, `msw` with `@msw/playwright` (in the Playwright process only), `@playwright/test` (the installed Edge), `@types/node` (`apps/desktop`'s e2e config and specs) |
| Lint & format | `@biomejs/biome` |
| Tauri bridge | `@tauri-apps/api` (`apps/desktop` only) |
| Tauri CLI | `@tauri-apps/cli` (`apps/desktop` dev only) |
