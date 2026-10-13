# 0017. fleet-client and desktop app conventions, Phase 8 refinements

- **Status:** Accepted
- **Date:** 2026-10-12 (amended in the review of PR #34; groups B and C added the same day; groups D and E on 2026-10-13)
- **Related:** Plan.md Phase 8 (P8.1–P8.9), §4, §5, §7.5, §8, Appendix A, P9.3; ADR-0006, ADR-0007, ADR-0015, ADR-0016

## Context
Phase 8 builds the desktop app: the Tauri shell, the React UI, and the Rust side that talks to the server (ADR-0007). The Rust side talks to the server only through `fleet-client` (P8.1), a typed client of the API. The server's tests already pin its contract (ADR-0016):
- a single-flight refresh
- only a 401 `unauthorized` from an authenticated route refreshes
- no grace window
- a maximum response body size (ADR-0015)

Before any code, the user split the phase into groups and answered group A's open questions on 2026-10-12, one at a time, and amended the plan in review. This ADR records the answers. Each later group asks its own implementation questions and adds its decisions here.

## Decision
### Grouping *(the user's choice: five groups, P8.7 split)*
One branch and one PR per group, one commit per task, in this order, each after the previous PR is merged:

| Group | Branch | Tasks |
|---|---|---|
| A | `p8/p8.1-fleet-client` | P8.1 |
| B | `p8/p8.2-p8.3-scaffold-and-hardening` | P8.2, P8.3 |
| C | `p8/p8.4-p8.5-bridge` | P8.4, P8.5 |
| D | `p8/p8.6-p8.7-shell-and-auth-screens` | P8.6; P8.7's server setup, login with 2FA, register and enrollment |
| E | `p8/p8.7-p8.9-profile-admin-wrap-up` | P8.7's profile; P8.8; P8.9; the DoD, threat model B2/B7 and README |

P8.7 is ticked in E. Where only the user can check something by hand on Windows (the window, the keychain, the phone's authenticator, an installed dev build), the group's plan names the exact steps.

### Group A: the API types
- **Requests serialize too** *(the user's choice)*. Every request DTO derives `Serialize` next to `Deserialize`, so fleet-client sends exactly the shared types; nothing mirrors them. An `Option` field is left out when it holds nothing (`skip_serializing_if`), never sent as `null`. **This amends ADR-0015's request rule**, which named only `Deserialize`, garde and `deny_unknown_fields`. A test round-trips each request through the client's JSON and the server's reading and validation. The TypeScript export and the `OpenAPI` document don't change.
- **Core types through fleet-api-types** *(the user's choice)*. fleet-api-types re-exports the fleet-core IDs its DTOs carry (`UserId`, `SessionId`, `InviteId`), so fleet-client and the app name them without depending on fleet-core, and §4's table stays as it is. A later DTO with another core type re-exports it too.

### Group A: the test server
- **`serve::app_with`** *(the user's choice)*. `serve::app` keeps its signature and calls the new `app_with(…, ports: ServerPorts)` with `ServerPorts::system()`: `SystemClock`, a `MonotonicClock` for the limiters, and `OsRandom`. Only `app` builds those, so `serve` and `user create-owner` stay the only places the real ones are built. fleet-client's tests pass `ManualClock` (as both clocks) and `SeededRandom`, then run `serve::serve` on 127.0.0.1:0. So they exercise the production wiring, housekeeping included, with the production rate limits.
- **fleet-startup** is a dev-dependency of fleet-client *(the user's approval)*, for the `ShutdownSignals` that stops the server.

### Group A: the tokens
- **The refresh token lives only in a `SessionStore`** *(the user's choice)*. The port has `load`, `save` and `clear`, each `-> impl Future + Send`, and the client is generic over it. The client never keeps a copy: a refresh loads it, sends it and saves the new one, and a login saves it. If a save fails, the client clears the store and logs a `warn`, so a restart can't send a replaced token; the session ends at the next refresh. `MemoryStore` serves tests and callers without a keychain. P8.4 implements the keychain.
- **No access token means refresh first** *(the user's choice)*. An authenticated request without an access token, as after a start, runs the single-flight refresh before sending, and counts as already retried. With an empty store it answers `NoSession` and sends nothing.
- **Step tokens stay inside the client** *(the user's choice)*. The password step's mfa or setup token and a 2FA replacement's setup token are the one pending step, held in memory. The methods take only codes, so no token of any kind leaves fleet-client:
  - `login` answers `MfaRequired { expires_in }` or `SetupRequired { expires_in }`
  - `login_mfa(code)`, `mfa_enroll()`, `mfa_confirm(code)` and `mfa_reset() -> expires_in` use the pending step

  A 401 on a setup route drops the step and never refreshes; a 401 `invalid_credentials` at `login_mfa` keeps it (3 tries); a successful login or confirm ends it.
- **`login` with a session held is refused** *(the user's choice)*. With a refresh token in the store or an access token in memory, it answers `AlreadyLoggedIn` before any request. The caller logs out first, so a session is never left live on the server with its token forgotten.

### Group A: the session's end
- **When** *(the user's choice)*. The session ends on a refused refresh, on a 401 `unauthorized` for a request already retried with the newest token (a revoked session), on an empty store when a refresh is needed, and at every logout, even when the server can't be reached. Transient failures (unreachable, a timeout, 429, 503) never end it.
- **An end never races a save** *(the user's change after the plan review)*. The session carries an epoch. A second tokio mutex, the store gate, guards the epoch and every store write. The lock order is always the refresh lock, then the gate, and an end takes only the gate, so nothing deadlocks.
  - **Every end** takes the gate, bumps the epoch, clears the store and drops the access token and the pending step, without waiting for a refresh's network call.
  - **Every save** (a refresh's and `login_mfa`'s) records the epoch when it starts. After its POST, it takes the gate and saves and adopts only if the epoch is unchanged.
  - **When the epoch changed,** it saves and holds nothing, its caller gets `NoSession`, and it revokes the session its POST just created on the server, best effort, with that session's own access token.

  A save can never land after an end, and the server keeps no orphaned session when it can be reached. The gate is held only around store calls, each bounded.
- **Logout ends locally first** *(the user's change)*, then revokes on the server, best effort, with the tokens it took:
  1. Under the gate, it takes the access token and the stored refresh token, then ends the session. With neither it sends nothing and answers `Ok`.
  2. With an access token, it sends `POST /auth/logout`.
  3. Without one, or when that answers 401, it falls back to the refresh token it took: it sends `POST /auth/refresh` (saving nothing), then logs out with the answer's access token. A refresh that holds the stored token at that moment (a flag it sets under the gate when it loads the token, cleared when it ends) has already sent it, so sending it again would be reuse; the logout leaves the server's side to that refresh's epoch rule.
  4. An error there is returned, but the local end stands.

### Group A: a dropped caller never stops a save *(the user's review of PR #34)*
- **The problem.** A refresh ran inside the future of the request that needed it. A caller dropped after `POST /auth/refresh` reached the server but before the new pair was saved (a `select!`, a timeout) lost the rotated refresh token. The store kept the replaced one, so the next refresh was reuse, which revoked the session and recorded a false `security.refresh_token_reuse`. A code step dropped the same way left an orphaned session on the server. The user reproduced it with the scripted transport.
- **The rule.** Once the request may reach the server, the request, the save and the adoption run in a task of their own (`tokio::spawn`) over an `Arc` of the core, and the caller awaits its answer. A dropped caller drops only its wait. The task saves and adopts the new pair, or, after an end, revokes it (the epoch rule).
  - **For a refresh**, the task holds the refresh lock (taken with `lock_owned`) and the refresh-out mark until it ends. Refreshes stay one at a time, and a caller that arrives meanwhile finds its result.
  - **The store gate, the epoch rule, `refresh_out` and the single flight are unchanged.**
- **Owner.** The task is bounded by its request's and its store calls' timeouts. Its only owner is its caller's wait, so it's the third exception CLAUDE.md lists to "every spawned task has an owner". fleet-client's tokio gains `rt`.
- **A task that ends without an answer** (a panic, which non-test code can't cause, or a runtime shutting down) is the new `ClientError::Interrupted` *(the user's choice over reusing `Unreachable`)*, so a bug or a shutdown never reads as a server that's down.
- **Tests.** The caller is dropped while the answer is held, then the answer is released. The store holds the new refresh token, the access token is adopted, and the next request sends it without refreshing again, both after a refresh and after a code step. Both were red against the old code (the store held `afk_rt_0`, or nothing), as the user had reproduced. A third test, red against a mapping to `NoSession`, checks `Interrupted`.

### Group A: the server, the trust and the bounds
- **`ServerUrl`** *(the user's choice)*: an origin only, meaning a scheme, a host and an optional port, with no user info, path, query or fragment. `https` works for any host; plain `http` only for a literal loopback address (127.0.0.0/8 or `[::1]`), for `just dev-server` and the tests. `http://localhost` is refused, since name resolution could point it elsewhere. P8.4's server profile reuses the type.
- **No redirects, no proxies** *(the user's choice)*: `redirect::Policy::none()` (a 3xx is an unexpected answer), and `no_proxy()`, so the environment's proxy variables can't route the traffic elsewhere.
- **Trust** *(the user's choices)*. `Trust::System` checks against the system's roots through the platform verifier. `Trust::CustomCa` uses `tls_certs_only`, so a profile with its own CA trusts nothing else, which matches §7.5's "system roots or a configured custom CA". `CustomCa::from_pem` is strict, since the PEM comes from a file the user picks:
  - at most 64 KiB
  - every PEM section a `CERTIFICATE` (a private key or anything else is refused)
  - 1 to 16 certificates, each loading as a rustls trust anchor
  - errors that name the problem and the position, never any content
- **Timeouts** *(the user's choice)*: 5 s to connect, 20 s for a whole request, 5 s for each store call, as constants.
- **The server's request timeout is capped at 19 s** *(the user's addition)*. `[http] request_timeout_secs` goes from 1–300 to 1–19, below the client's 20 s. A slower server answer could let a refresh commit after the client gave up, and the client's next refresh would then send the replaced token: reuse, which ends the session. The key's docs and Plan.md's Appendix A say why. **This narrows ADR-0015's range**, a merged rule.
- **Body cap** *(the user's choice)*: 1 MiB (`MAX_RESPONSE_BYTES`). A larger `Content-Length` is refused before reading, and a body that grows past it is cut off and its connection dropped.
- **No automatic retries** *(the user's choice)*. A 429 or 503 comes back with its `retry_after`, and P8.6/P8.7 decide what to show. reqwest's default retry stays: it only re-sends requests known to be safe, low-level NACKs from requests the server never processed.

### Group A: errors, endpoints and tests
- **Fine-grained errors** *(the user's choice)*. `ClientError` has these variants:
  - `Unreachable`: a connection failure, or a 502, 503 or 504 without the envelope, which is a proxy whose server is down
  - `Certificate`: rustls's `InvalidCertificate` anywhere in the chain
  - `TimedOut`
  - `TooLarge`
  - `UnexpectedResponse { status, reason }`, where `reason` is one of `NotJson`, `NoEnvelope`, `Redirect`, `Status`, `Body` or `UnknownLoginStatus`
  - `Api(ApiFailure)`, holding `status`, `code` (an `Open`), `message`, `request_id`, `fields` and `retry_after`
  - `NoSession`
  - `NoPendingStep`
  - `AlreadyLoggedIn`
  - `Store`
  - `Interrupted`: a refresh's or a code step's task ended without an answer (added in the review of PR #34)

  No `Display` holds a token, a body or a URL; reqwest's errors are kept without their URL.
- **Endpoints** *(the user's choice)*: every `/api/v1` route plus `health_live()` and `health_ready()`; not the dev-only `OpenAPI` document. `/auth/refresh` is called only by the client itself.
- **Crate-private test seams** *(the user's choice)*. A `pub(crate)` `Transport` trait and `pub(crate)` `Timeouts`. The logic lives in a `pub(crate) Core<S, T>`, and `Client<S>` holds an `Arc<Core<S, ReqwestTransport>>`, so the public type has one parameter (a private trait bound on it would trip `private_bounds`). There's no cargo feature.
  - Unit tests script answers held behind a `Notify` to force each interleaving.
  - They run 100 ms timeouts against a local socket that never answers and a store that hangs.
  - The integration tests run the real server with the real constants.
- **A coverage gate** *(the user's choice)*: `fleet-client` at 90 %, required, over `crates/fleet-client/src/`.

### Dependencies *(the user's approval)*
- **fleet-client:**
  - fleet-api-types
  - reqwest 0.13.5 (the workspace entry: no default features, `rustls`)
  - rustls 0.23.45, only to recognize a refused certificate and to check a custom CA's trust anchors
  - serde 1.0.229
  - serde_json 1.0.151
  - secrecy 0.10.3
  - thiserror 2.0.21
  - tokio 1.53.2 (`sync`, `time`)
  - tracing 0.1.44
- **Dev:**
  - fleet-server (a new workspace path entry), fleet-startup, fleet-core and fleet-testkit
  - tokio (`macros`, `net`, `rt`)
  - tempfile 3.27.0
  - totp-rs 6.0.0 (`otpauth`)
  - chrono 0.4.45
  - rstest 0.27.0
  - rcgen 0.14.10: new in `[workspace.dependencies]` with no default features (they select ring); fleet-client enables `aws_lc_rs` and `pem`
- **stable-check:** fleet-client joins `stable_crates`, under P0.11's rule.

### As built (group A)
- **Layout:** `fleet_client::{client, error, store, trust, url}` are public. `session` (`Core`, the decision rule `after_unauthorized`, the routes) and `transport` are crate-private.
- **The loopback check reads `host_str()`.** reqwest re-exports `Url` but not the url crate's `Host`, so the host's text is parsed as an IP address. The parser writes an IP host in its canonical form (`http://127.1` reads as 127.0.0.1), and a name never parses as one.
- **The PEM labels are checked line by line.** rustls-pki-types 1.15.1 skips sections whose label it doesn't know (checked in its `pem.rs`), and a type's iterator skips other known kinds. So every `-----BEGIN ` line must read `CERTIFICATE`, and only then does pki-types decode them.
- **A refused certificate is found inside an `io::Error`.** hyper wraps the TLS failure in an `io::Error`, whose own `source()` skips the error it wraps, so the search also looks through `io::Error::get_ref`.
- **A logout without a session still bumps the epoch**, so a code step in flight at that moment saves nothing (the user's second test).
- **A Member listing users or invites gets 403 `forbidden`** from the server; the client's docs say so.
- **Lockfile.** The build gains rcgen, pem and yasna, and nothing else compiles (`cargo tree -i` for each). Cargo.lock also lists x509-parser 0.18.1, asn1-rs, asn1-rs-derive, asn1-rs-impl, der-parser, oid-registry, nom, minimal-lexical, rusticata-macros, synstructure 0.13.2, bit-vec 0.9.1 and untrusted 0.7.1. Cargo records the optional dependency behind rcgen's weak feature `x509-parser?/verify-aws` in the lockfile even though no feature enables it, so none of them is ever built. `cargo deny check` passes.
- **Tests,** red against compiling stubs first (request `Serialize` writing a unit, the old range, a client whose rule always refreshed and whose methods answered `NoSession`, a URL and PEM check that accepted anything, no body cap):
  - fleet-api-types: 2 of 3
  - the config's range case
  - fleet-client: 101 of 132
  - the gates script's new check

  Then all passed. The 31 that passed against the stubs check refusals, fixed texts and `Debug`, which a do-nothing stub satisfies too. No snapshot is new or changed. The review of PR #34 added three tests, each red first (see above).

### Group B: the desktop crate, Windows only *(the user's choices, 2026-10-12)*
- **The workspace member.** `apps/desktop/src-tauri` is the crate `afkfleet-desktop`. Its library holds the logic; the binary only calls `run()` and has `doc = false`, since the two share a name.
- **Linux leaves it out.** The app ships only for Windows, and on Linux Tauri needs WebKitGTK and GTK3. So the justfile adds `--exclude afkfleet-desktop` to every workspace-wide Rust recipe when `os()` is linux:
  - clippy, docs, doctest, test, test-ci, test-slow and cov
  - CI's Linux jobs keep their YAML unchanged

  Windows covers it in three places:
  - clippy and test on windows-latest
  - stable-check, which includes it off Linux
  - a new `tauri-build` job on windows-latest: `just docs`, `just stable-check` and `just app-build`

  Coverage never measures it, since `cov` runs on Linux only. Its logic is tested on Windows but not counted toward the gates.
- **Doctests leave it out everywhere** (found when `just check` ran). tauri-build links the VC++ runtime statically, so friends need no redistributable: it writes a stub `msvcrt.lib` into its build output and adds that folder to the link path. Cargo hands every build script's link paths to every doctest in a run, so fleet-core's doctests linked against the stub and failed. `doctest` and `test-ci` therefore run `cargo test --workspace --exclude afkfleet-desktop --doc` on every platform. The desktop crate has no doctests; its own tests link through its own build script as usual.
- **cargo deny in two passes** *(the user's choice over ignores)*. With tauri 2.12.3, tauri-build 2.7.2 and single-instance 2.5.3, the repo's `deny.toml` on all three targets reported three errors, all from the Linux GTK3 stack:
  - `unsound` RUSTSEC-2024-0429: glib 0.18.5, fixed only in ≥ 0.20, which the GTK3 bindings don't use
  - `unmaintained` RUSTSEC-2024-0370: proc-macro-error 1.0.4, through glib-macros and gtk3-macros
  - license: target-lexicon 0.12.16, `Apache-2.0 WITH LLVM-exception`, through system-deps, a build dependency of the GTK sys crates

  With `--target x86_64-pc-windows-msvc` alone, every check passed. So `just deny` runs two passes with the same `deny.toml`:
  1. the workspace with `--exclude afkfleet-desktop`, on every target, rules unchanged
  2. the desktop crate alone (`--manifest-path apps/desktop/src-tauri/Cargo.toml`) with `--target x86_64-pc-windows-msvc`

  Both were tested in a throwaway workspace first. There are no ignores and no exceptions, and the ring, openssl-sys and native-tls bans stay.

  **This narrows deny's targets for one crate, and holds only while the desktop app ships only for Windows: any new desktop target joins the second pass.** Gating Tauri behind `cfg(windows)` wouldn't help: cargo-deny filters each dependency edge against all its targets at once, so tauri's Linux-only edges stay in its combined graph (tested).
- **Docker.** The agent's image copies `apps/desktop/src-tauri` in both stages, and `.dockerignore`'s allowlist admits it (minus its `target` and `gen`), because cargo can't load the workspace without a member. Nothing in the image builds it.
- **Identity** *(the user's choice)*:
  - identifier `io.github.noah-gabel.afkfleet`, derived from the repository's own URL
  - productName and window title `afkfleet`
  - npm packages `@afkfleet/ui` and `@afkfleet/desktop`

  The identifier names the app's data folder, its WebView2 profile and the single-instance mutex, so it never changes.
- **The icon** *(the user's choice)*. An original placeholder, `icons/icon.svg`: a rounded square with three white dots, no text, no third-party logo. `just icons` (`scripts/icons.mjs`) renders `icon.ico` from it with `tauri icon` into a fresh folder under `target/`, keeps only `icon.ico` and removes that folder. Only the SVG and the ICO are committed; tauri-build embeds the ICO in the exe, and a Windows build fails without it.

### Group B: the frontend *(the user's choices and approvals)*
- **Layout.**
  - `packages/ui` exports its sources (`.` → `src/index.ts`, `./styles.css`); there is no build step in between.
  - Its own `index.html` and Vite config are the web entry for development. *(Group E: the Playwright build became apps/desktop's e2e entry, see group E's e2e suite.)*
  - `apps/desktop` is a thin entry (`index.html`, `src/main.tsx`) that renders `<App />` from `@afkfleet/ui`. Its Vite dev server runs on `localhost:1420` with `strictPort`, matching `devUrl`.
  - Tailwind scans `packages/ui/src` (`@import "tailwindcss" source(".")`), so the desktop bundle finds the UI's classes.
- **TypeScript 7.0.2** *(the user's choice over 6.0.3 or both side by side)*. This is the Go port, with a native `tsc` about 10× faster. It has no JavaScript compiler API until 7.1, so no tool that needs one (typescript-eslint, ts-node, vite-plugin-checker) can join until then; Biome, Vite 8, Vitest and `tsc --noEmit` need none. Since `types` now defaults to `[]`, each tsconfig lists `vite/client`. Editor diagnostics come from VS Code's TypeScript or the "TypeScript Native Preview" extension, and can differ slightly from CI's `tsc`.
- **shadcn/ui: configuration and theme only** *(the user's choice)*. `packages/ui/components.json` is set up with:
  - style `radix-nova`, Radix per §5
  - base color neutral
  - aliases through package.json `#` subpath imports (`#components/*`, `#lib/*`, `#hooks/*`), since TS 7 has no `baseUrl`

  Its theme tokens are in `src/styles.css`. No shadcn package is installed. `cn` (shadcn's own replacement for clsx + tailwind-merge), class-variance-authority, radix-ui, tw-animate-css, lucide-react and the `shadcn` package's tailwind.css arrive with the first component, with the user's approval there. The CLI only ever runs as `pnpm dlx shadcn@4.21.4`, never as a dependency, since that package pulls in ts-morph, an MCP SDK and dotenvx.
- **Vitest now** *(the user's choice)*, so the frontend job's `pnpm -r run test` covers `packages/ui` from the start with a smoke test. Its setup file unmounts after each test: Vitest runs without globals, and Testing Library's automatic cleanup needs a global `afterEach`. jsdom stays at 29.1.1 because jsdom 30 needs Node ^24.15.
- **The Tauri CLI** is the npm `@tauri-apps/cli` *(the user's choice over `cargo install`)*. It ships prebuilt binaries through per-platform packages with integrity hashes in `pnpm-lock.yaml`, and pnpm audit covers it.
- **Biome** sets `security.noDangerouslySetInnerHtml` and `noDangerouslySetInnerHtmlWithChildren` to `error` explicitly, so the preset can't lower them. `css.parser.tailwindDirectives` lets it parse Tailwind 4's at-rules.
- **Dependencies** *(the user's approval)*, all pinned exactly (no `^`), with `pnpm-lock.yaml` as the lock:
  - Rust: tauri 2.12.3, with no default features at workspace level; the desktop crate enables `wry` and `compression` (P8.3 dropped `common-controls-v6`, see below). tauri-build 2.7.2. tauri-plugin-single-instance 2.5.3 (P8.3).
  - npm build: typescript 7.0.2, vite 8.3.4, react and react-dom 19.3.0, tailwindcss 4.3.3.
  - Off the Introduces list, approved: `@vitejs/plugin-react` 6.1.2 (oxc on Vite 8, no Babel), `@tailwindcss/vite` 4.3.3, `@types/react` and `@types/react-dom` 19.3.0, `@tauri-apps/cli` 2.12.2.
  - Tests: vitest 5.0.3 (5.0.4 was a day old), jsdom 29.1.1, `@testing-library/react` 16.3.3 and its peer `@testing-library/dom` 10.4.2.

  No dependency runs an install script.

### Group B: the hardening (P8.3) *(the user's choices)*
- **The CSP is P8.3's, exactly:** `default-src 'self'; img-src 'self' data:; style-src 'self'; connect-src ipc: http://ipc.localhost`. Relaxing it needs an ADR.
  - **No `devCsp`.** `tauri dev` on desktop loads Vite's dev server directly (`PROXY_DEV_SERVER` is mobile-only in tauri 2.12), and Tauri sends a CSP only with the assets its own protocol serves. So neither `csp` nor `devCsp` reaches a dev page, and a `devCsp` would be dead config. The real CSP is checked in a `tauri build --debug` exe, which embeds the frontend, applies `csp` and still has devtools.
  - **`dangerousDisableAssetCspModification: true`.** By default Tauri hashes the bundled scripts and nonces inline styles into the header it serves, adding `script-src 'self' 'sha256-…'`. With the flag, the served header is P8.3's directives, and an inline `<script>` or `<style>` that ever lands in index.html is blocked instead of being let in. **It makes the served CSP stricter, not looser.** Tauri's warning is about CSPs with `'unsafe-inline'`, which the added nonces would silently disable, so the flag must not be removed because of its name.
  - **The header's order varies.** Tauri keeps the policy in a hash map and writes it out per response, so the served header holds exactly these four directives and sources, but in an order that changes between runs (seen in three runs).
- **`withGlobalTauri: false`**, written out although it's the default, so the check pins it.
- **`freezePrototype: true`** *(the user's addition)*. Tauri runs `Object.freeze(Object.prototype)` in every webview, dev and release, which blocks prototype-pollution gadgets. **Caveat:** besides writes to `Object.prototype`, assigning a shadowing property such as `toString` or `constructor` on a plain object throws in strict code and silently fails in sloppy code. A library that does either breaks. Removing the flag needs an ADR.
- **The ACL is on from the start.** tauri-build switches the ACL on for the app's own commands only when the app has a permission manifest. An empty `AppManifest::commands` list generates none, and without one every app command runs from the app's own pages: the ACL test's probe command ran.
  - So `src-tauri/permissions/default.toml` holds an empty `[default]` permission that grants nothing and that no capability lists. With it, every command needs an `allow-<command>` in a capability (the probe is refused: "not allowed. Command not found").
  - build.rs lists the commands in `const COMMANDS` and passes them to `AppManifest::commands`, which generates each one's `allow-`/`deny-` permission. It's empty until group C.
  - The one capability, `main` for window `main` (`app.security.capabilities: ["main"]`), grants nothing: no `core:default` or other core permission, since the UI calls no Tauri API, and nothing for single-instance, which has no JavaScript API. A core command is refused too.
- **Devtools only in debug builds.** No `devtools` feature and no window `devtools` setting. A release build turns WebView2's devtools off (`AreDevToolsEnabled`), so F12, Ctrl+Shift+I and "Inspect" do nothing. WebView2's own context menu stays, minus "Inspect" *(the user's choice not to suppress it)*.
- **tauri's features:** `wry` and `compression` only.
  - No `dynamic-acl`: no runtime `add_capability`, so the ACL is what the build resolved.
  - No `common-controls-v6` *(the user's choice when the ACL test's exe couldn't start)*. It exists for the predefined About dialog, and makes Tauri import `TaskDialogIndirect`, which exists only in Common Controls v6. Windows loads v6 only for an exe whose manifest asks for it, and tauri-build embeds that manifest into the app's binary, never into test executables, so the test exe failed with 0xc0000139. We have no menu and no native dialogs. A group that adds them revisits this with an ADR line.
- **One instance.** `tauri_plugin_single_instance` is registered first, as the plugin requires. A second launch hands over and exits at once; the first instance restores, shows and focuses its main window. Each step is tried even if an earlier one failed, and failures are dropped, since the launch that asked has already ended.
- **The navigation guard** *(the user's addition)*. An internal plugin's `on_navigation` hook allows only the app's own origin (`http://tauri.localhost`, any path or query), plus `build.devUrl`'s origin under `cfg(dev)`, and refuses everything else, so neither a link nor a script can take the window to another site. There's no new-window handler: wry refuses `window.open` and `target=_blank` when none is set.
- **Pinned** *(the user's choice: a script plus a Rust test)*.
  - `scripts/tauri-hardening.mjs` (`just tauri-check`, in `check`, `ci` and CI's deny job) checks:
    - every `app.security` key exactly, and `withGlobalTauri`
    - that no window sets `devtools`, `url` or `useHttpsScheme`, and that `devUrl` is `http://localhost:1420`
    - the one capability's identifier, window, keys and grants, which must equal `allow-<command>` for build.rs's commands
    - build.rs's `COMMANDS` and the empty default permission
    - tauri's resolved features from `cargo tree -e no-dev`, exactly; `devtools`, `dynamic-acl` and `custom-protocol` are each named as never allowed

    Its expected values change only together with an ADR.
  - The ACL test (`src-tauri/tests/acl.rs`) builds the real context on Tauri's mock runtime and shows that an app command no capability grants, and a core command, are refused.

### As built (group B)
- **Layout:**
  - `afkfleet_desktop::run` builds the app.
  - `focus` (the `MainWindow` trait, `focus_main_window`) and `navigation` (`guard`, `is_allowed`) are crate-private.
  - The guard plugin uses `try_build`, mapping its error to `tauri::Error::PluginInitialization`, so our code never calls a panicking `build()`.
- **Tests,** red first against compiling stubs:
  - `scripts/icons.test.mjs`: 4 of 4
  - the UI's smoke test
  - `navigation`: 14 of 19. The 5 that passed allow the app's origin, which an allow-everything stub does too.
  - `focus`: 2 of 2
  - the ACL test's app-command case, red until `default.toml` existed. The core-command case passed from the start, since no capability granted it anything.
  - `scripts/tauri-hardening.test.mjs`: 18 of 25. The 7 that passed accept the right input, which an empty stub does too.

  Then all passed. No snapshot is new or changed.
- **Checked live** in a `tauri build --debug` exe over WebView2's DevTools protocol on 127.0.0.1:
  - the page loads at `http://tauri.localhost/` with the heading and Tailwind's styles
  - no console errors and no CSP violations
  - the served CSP has the four directives (in varying order)
  - `window.__TAURI__` is undefined and `Object.prototype` is frozen
  - `location.href = "https://example.com"` stays on the app; `window.open` returns `null` and opens nothing
  - a second launch exits at once, leaving one page
- **The release exe** builds through `just app-build` in about a minute on the dev machine; the agent's image and the slow suite (`just stack-up`, `just test-slow`) still work.

### Group C: the bridge's Rust side (P8.4) *(the user's choices, 2026-10-12)*
- **Every endpoint now** *(the user's choice)*. 20 commands, so groups D and E only build screens and never touch the ACL or the hardening pin: `server_profile`, `set_server_profile`, `check_server`, `login`, `login_mfa`, `logout`, `register`, `step_up`, `mfa_enroll`, `mfa_confirm`, `mfa_reset`, `me`, `change_password`, `sessions`, `revoke_session`, `users`, `patch_user`, `invites`, `create_invite`, `revoke_invite`. They're grouped per feature in `commands/{server,auth,mfa,me,users,invites}.rs`.
  - Each command is a thin wrapper that calls the `AppState` method of the same name (`endpoints.rs`, `state.rs`).
  - None takes or returns a token.
- **The IPC types live in fleet-api-types' `app` module** *(the user's choice)*, so they're exported with the DTOs and never hand-written in TypeScript. The types are `CommandError`, `CommandErrorCode`, `ApiErrorDetails`, `LoginStepKind`, `LoginStepResponse`, `SetupStepResponse`, `LogoutResponse`, `ServerProfileRequest`, `ServerProfileView`, `CustomCaView`, `ServerProfileResponse`, `ServerProfileState` and `ProfileUnreadable`.
  - None crosses the HTTP API. `typescript::export` is split into `export_api` and `export_app`; the `OpenAPI` comparison runs over `export_api` only.
  - A test pins the 13 files `export_app` adds.
- **`CommandError { code, message, api?, fields? }`** *(the user's choice, with their addition of `api.message`)*:
  - **`code`** is a closed `CommandErrorCode`, since the Rust side and the UI ship in one exe:
    - mapped from `ClientError`: `unreachable`, `certificate`, `timed_out`, `too_large`, `unexpected_response`, `api`, `no_session`, `no_pending_step`, `already_logged_in`, `store`, `interrupted`
    - the app's own: `no_server`, `invalid_input`, `invalid_server_url`, `invalid_ca`, `client_build`, `profile_save`
    - `ipc`, produced only by the UI for a rejection from Tauri itself (P8.5)
  - **`message`** is the code's fixed sentence.
  - **`api`** is there only for `api`: the status, the server's `Open<ErrorCode>`, the server's bounded message, the request ID, the fields and `retry_after_secs`. The UI's default branch for an unknown server code shows that message and the request ID (ADR-0015). `api` is boxed, so every `Result<_, CommandError>` stays small (clippy's `result_large_err`); the wire format doesn't change.
  - **`fields`** lists the rules a local check broke (garde's paths).
  - The mapping from `ClientError` is an exhaustive match, so a new variant doesn't compile until it has a code. No field ever holds a token, a URL or a raw body.
- **Logout always answers Ok** *(the user's choice)*: `{ server: "ended" }`, or `{ server: "unconfirmed", error }` when the server couldn't be told. The session has ended in the app either way, so D can say the server's session may live until it expires.
- **Validation at the boundary:**
  - Each request DTO is garde-validated before its call (`invalid_input`, fields).
  - A code must have 1 to 64 characters; the server checks its shape.
  - A list's `before`/`limit` become a validated `PageQuery` *(the user's choice: plain optional parameters, so no TypeScript type mirrors `PageQuery`)*.

### Group C: the server profile *(the user's choices)*
- **Validated by fleet-client's own rules:** `ServerUrl::parse` and `CustomCa::from_pem`.
  - `ServerProfileRequest` caps `url` at 2048 bytes and `ca_pem` at 65536 bytes (`CustomCa::MAX_BYTES`; a desktop test asserts the two are equal).
  - A refused address is `invalid_server_url`, a refused CA `invalid_ca`, each with one fixed message and the rule's own text under `fields` (`url`, `ca_pem`).
- **Stored** in `server-profile.json` in `app_local_data_dir()` (`%LOCALAPPDATA%\io.github.noah-gabel.afkfleet\`):
  - The file is `{ version: 1, url, ca_pem? }`. The CA is re-encoded as canonical PEM from the DER `from_pem` parsed, never the pasted text, and read back through `from_pem`.
  - It's written to `server-profile.json.tmp`, synced, and renamed over the old file.
  - Startup reads at most 128 KiB *(the user's review addition)*.
- **A file that can't be used** *(the user's choice)*: the app starts without a profile, and `server_profile` answers `{ state: "unreadable", reason }` with `io` (including a file over 128 KiB), `json`, `version`, `url`, `ca` or `client` (no client can be built over it). The reason never repeats the file's content.
- **The view** *(the user's choice)*: `{ url, custom_ca?: { certificates, fingerprints } }`. Each fingerprint is the certificate's SHA-256 over its DER, written as `openssl x509 -noout -fingerprint -sha256` prints it: uppercase hex pairs joined by colons.
- **Server checks** *(the user's choice: both)*. `set_server_profile` calls the new client's `health_live()`, and an error changes nothing. `check_server` calls `health_ready()` on the current profile, so P8.6's banner tells a busy server (503 `busy` with `retry_after_secs`) from one that's down.

### Group C: switching servers *(the user's rules)*
- **A stored session resumes only under the identical profile it was created with.**
  - The keychain entry is keyed by origin: service = the app identifier, account = `ServerUrl::as_str()`. So a client loads only a token from its own server, and spellings of one origin (`https://EXAMPLE.com:443`) share one entry.
  - Setting the identical profile (same origin, same certificates by canonical PEM) is a no-op that keeps the session: `{ change: "none", profile }`.
  - Any other profile logs out of the old server (if one was set), then clears the new origin's entry without any request: `Client::discard_session()`. A token stored there came from a profile whose certificates can't be compared any more: from the `none` or `unreadable` state, or an older profile for that origin. The answer is then `{ change: "switched", profile, logout }`.
  - The `change` tag *(the user's review change)* keeps D from reading a kept session as logged out.
- **One `RwLock`** *(the user's choice)*. Every command holds a shared guard of the connection for its whole client call, and takes it exactly once. `set_server_profile` takes it exclusively, so nothing runs on the old client after the swap.
- **The order** *(the user's review change)*:
  1. Validation, building the new client and `health_live()` run before the exclusive guard. They touch no shared state, and an unreachable server would otherwise block every command for up to 20 s.
  2. Under the guard: the identical check, the old client's `logout()`, `discard_session()`, the file write and the swap.

  For the same origin with another CA, the logout runs before the discard, so it can still revoke with the old token.
- **Aborts** *(accepted in review)*. A `store` error from the discard, or `profile_save` from the write, comes after the old logout. The old profile stays current, with its session already logged out, and the file is unchanged. Tests show both. **Amended by group D:** the discard's failure is now `session_discard`, since `store` also comes from building the new origin's store before the guard, when nothing has changed.

### Group C: the keychain *(the user's choices)*
- **keyring-core 1.0.0 over windows-native-keyring-store 1.1.0** (without its `search` feature), building entries from an explicit store, with no global default store.
  - In Credential Manager the entry is a generic credential whose target is `<origin>.<identifier>`, e.g. `http://127.0.0.1:8080/.io.github.noah-gabel.afkfleet`: the store's `"{prefix}{user}{divider}{service}{suffix}"` with its defaults `""`, `"."` and `""`.
  - Its persistence is `Local` (this machine only, never roaming), set with the `persistence` modifier.
  - keyring-core's mock store refuses every modifier, so `KeychainFactory::windows` sets it and `KeychainFactory::new` (tests) doesn't.
- **In order, off the runtime** *(the user's design)*:
  - Each call takes its entry's turn, a tokio mutex taken with `lock_owned()` in async code, whose guard moves into its `spawn_blocking` closure. A call that times out while it waits never runs, and one that has started finishes before the next starts, so calls on one entry run strictly in issue order. The Windows store doesn't order calls on one entry from different threads.
  - **The turn is per entry, not per store** *(the user's review of PR #36)*. `KeychainFactory` keeps one turn per origin (`ServerUrl::as_str()`), shares it with every store it builds for that origin, and its clones share the map. With a turn per store, the old and the new client of one origin (a CA-only change) didn't wait for each other, so a late save on the old store could land after the new store's discard, leaving an old session's token under the new profile. Tests, red first against a stub that handed out a new turn per store: two stores of one origin share the turn (`Arc::ptr_eq`, also through a clone and another spelling), and a held call on one makes the other's call wait. Different origins don't share.
  - Against a std mutex taken inside the closure, the test fails as the user predicted: the timed-out save ran after the clear.
- **A new task exception** *(the user's addition to CLAUDE.md's list)*. A keychain call that outlives fleet-client's 5 s store timeout runs on in its blocking thread, holding its entry's turn, until the OS returns. Its owner is the turn; the next call waits for it.
- **No content in errors.** keyring-core's errors can hold the stored bytes (`BadEncoding`, `BadDataFormat`), so they become a `KeychainError` naming only the kind (`NoAccess`, `Platform`, `BadData`, `Invalid`, `Other`, `Interrupted`). That error is what fleet-client may log.
- **Startup.** `main` installs the aws-lc-rs provider (`startup::install_crypto_provider`). `run()` builds the app, then manages the `AppState`: the profile file plus the Windows store under `app.config().identifier`. That happens before the event loop starts, so no command runs without it, and the mock-runtime tests manage theirs the same way.

### Group C: fleet-client additions *(the user's choices)*
- **`CustomCa`:**
  - It keeps its parsed `CertificateDer`s.
  - `to_pem()` writes canonical PEM (base64 in lines of 64, `\n`).
  - `fingerprints()` are SHA-256 per DER, in file order.
  - **`from_pem` also answers `TooLarge` when the canonical PEM would exceed 64 KiB** *(the user's choice)*: a pasted CA just under the cap with unbroken base64 lines grows about 1.6 % when re-encoded. So `from_pem(&ca.to_pem())` always succeeds.
  - Its `Debug` is now `CustomCa { certificates: n, .. }`, since clippy's `missing_fields_in_debug` asks to account for the DER, which it never prints.
  - base64 0.23.1 (`alloc`) and sha2 0.11.0 join fleet-client.
- **`Client::discard_session()`** *(the user's choice over tokio in the desktop crate)*. It ends the session locally the way an end does: under the store gate it bumps the epoch, drops the access token and the pending step, and clears the store within `STORE_TIMEOUT`, sending nothing. Unlike every other end, a failed clear is returned (`ClientError::Store`), since its caller must not go on as if no token were stored. A refresh in flight saves nothing afterwards (the epoch rule). `end_under_gate` now reuses its two halves.

### Group C: the ACL, the contract and the coverage *(the user's choices)*
- **Each command is listed** in build.rs's `COMMANDS`, granted as `allow-<command>` in `capabilities/main.json`, and pinned in `scripts/tauri-hardening.mjs`.
- **`permissions/autogenerated/` is committed** *(the user's choice)*. tauri-build writes one `<command>.toml` there per command and never deletes one. The hardening script requires exactly those 20 files, each byte-equal to tauri-utils 2.10.1's template, so a stale or edited grant fails. Its `readFolder` now returns folders as folders (the EISDIR fix). `.gitattributes` marks the folder `linguist-generated`.
- **One contract fixture,** `src-tauri/tests/ipc-contract.json` *(the user's design)*, lists every command with the `ApiClient` method, its inputs, the exact args `TauriApiClient` sends, and the answer of an app without a profile.
  - The hardening script requires its commands to equal `COMMANDS`, in order (Tauri can't list the registered handlers).
  - The ACL test sends each entry from the main window on the mock runtime with the real handlers and capability. It requires each answer to parse positively *(the user's tweak)*: a rejection as a `CommandError` with the expected code, an Ok as the command's own response type with the expected value. Tauri's not-found, not-allowed and invalid-args strings only label a failure.
  - The probe and core-command refusals stay.
- **Coverage on Windows** *(the user's choice: 90 %)*:
  - `just cov-desktop` (in `ci` and CI's `tauri-build` job) measures the desktop crate alone. It feeds `scripts/coverage-gates.mjs --report desktop`, whose new `desktop` gate is required over `src/` minus `lib.rs` and `main.rs`, which start the real event loop. Each gate now belongs to the `workspace` or the `desktop` report, and only the selected report's gates run. A Windows path's `\\?\` prefix is stripped.
  - **`--no-rustc-wrapper`** *(the user's choice after the spike)*. The spike found that cargo-llvm-cov 0.9.1's rustc wrapper (the default) builds the lib's unit-test binary on Windows without instrumentation: no profiler runtime, no `.profraw`, so the in-crate tests counted 0 %. Integration-test binaries were instrumented, and renaming the lib didn't help. With RUSTFLAGS the in-crate tests count. This probably explains the old Windows gap in fleet-server's local coverage too. The CI Linux job stays the authority for the workspace gates.
  - **A prefix gate also fails on a listed file with executable lines and none covered** *(the user's review of PR #36)*, naming the file with "uninstrumented test binary?", so a test binary the coverage build skipped can't hide behind a gate's total. Tests, red first. CI's Linux report had no such file in any gate.
  - Measured locally: 94.95 %; CI measured 95.17 %.
- **The keychain's ordering tests run on real time** *(the user's choice, like the sqlx rule)*. Its blocking calls run on Tauri's runtime, outside the test's, so paused time would fire the bounds early. The call that must time out gets 100 ms, every other wait 5 s, and the fake slot is always released by a guard.
- **One test writes to the real Credential Manager** *(the user's choice)*. It uses the service `io.github.noah-gabel.afkfleet.test-<process id>`, the account `https://example.com/` and a placeholder token, deletes the entry in a drop guard, and checks `Local` and the target name. CLAUDE.md's rule 12 now forbids reading the app's own entries.
- **Dependencies** *(the user's approval)*:
  - **Desktop:** fleet-client, fleet-api-types, keyring-core 1.0.0, windows-native-keyring-store 1.1.0 (no default features), rustls, secrecy, serde, serde_json, thiserror, garde.
  - **Desktop dev:** fleet-server, fleet-testkit, fleet-startup, fleet-core, tokio (`macros`, `net`, `rt`, `time`), tempfile, totp-rs, chrono, rcgen.

  Cargo.lock gains only keyring-core and windows-native-keyring-store; byteorder, zeroize, windows-sys 0.61 and log were already in it.

### Group C: the bridge's TypeScript side (P8.5) *(the user's choices)*
- **`ApiClient`** (`packages/ui/src/api/ApiClient.ts`) has 20 camelCase methods, typed only by the generated types, one import per file. `API_METHODS` lists them in build.rs's order. The list methods take plain optional parameters (`sessions(before?, limit?)`).
- **Errors.** Every method rejects with `ApiError extends Error { error: CommandError }` *(the user's choice)*.
  - `toApiError` keeps a rejection that parses as a `CommandError`, checked by a zod schema whose app codes come from a `Record<CommandErrorCode, true>`, so the compiler fails when the Rust enum changes.
  - It turns anything else into `ipc` with its fixed sentence, Tauri's own text dropped. Tauri rejects with plain strings when the ACL refuses or an argument doesn't deserialize.
  - `describeError` switches exhaustively on `code` (a `never` default) and keeps a default branch on `api.code` that shows the server's message and the request ID (ADR-0015). Group D builds its texts on it.
- **A validating wrapper** *(the user's choice)*. `validated(client)` parses every input with its zod schema (`schemas.ts`) before the inner client is called.
  - A failure rejects with `invalid_input`, fields named the way garde names them (`username`, `limit`, `""` for the whole request), the shape the Rust side answers.
  - `App` wraps whatever client it's given, so fakes go through the same checks.
  - The schemas mirror the server's rules: lengths in code points, the profile's caps in UTF-8 bytes, a username's characters, a version 7 ID, a page size, a patch with exactly one change, invites and patches without `owner`, and no unknown fields.
- **The limits can't drift** *(the user's choice)*. `packages/ui/src/api/limits.ts` holds only `export const NAME = <number>;` lines, named like the Rust constants. fleet-api-types' `tests/limits.rs` parses it and requires exactly the expected names and values.
- **`FakeApiClient`** *(the user's choice)* takes per-method handlers and records every call with its arguments. An unscripted method rejects with an `ApiError` naming it, and `fakeApiError(code, { api, fields })` builds errors for handlers.
- **`UnavailableApiClient`** *(the user's choice)* is the web entry's client until P8.9: every method rejects with `ipc` and "The web build has no server connection.", the one text for `ipc` that isn't the fixed sentence, since nothing in that build is the app's.
- **`TauriApiClient(invoke)`** *(the user's choice)* lives in `apps/desktop/src`. `invoke` is injected, so `main.tsx` passes `@tauri-apps/api/core`'s and tests a recording fake. Every method sends an args object, `{}` without arguments.
  - Vitest runs in apps/desktop (Node environment) over `ipc-contract.json`: each `method(...inputs)` must send exactly the entry's `command` and `args`, and the contract's methods must equal `API_METHODS`.
  - With the Rust test of the same fixture, a renamed command or argument key can't pass both suites.
- **Import rules, enforced** *(the user's choice)*. biome.json's `style/noRestrictedImports` refuses every `@tauri-apps/*` import in packages/ui. In apps/desktop it refuses everything but `@tauri-apps/api/core`: the bare `@tauri-apps/api`, `/event` and `/mocks` were refused when probed. So no API that needs a grant can slip in, and `withGlobalTauri` stays false with no core permission.
- **The frozen prototype.** One Vitest file freezes `Object.prototype`, as the webview does (`freezePrototype`), then runs the validating wrapper with zod. zod 4.6.5 works there.
- **Dependencies** *(the user's approval)*: `@tauri-apps/api` 2.12.1 in apps/desktop, zod 4.6.5 in packages/ui, vitest 5.0.3 as an apps/desktop dev-dependency, all exact. None has an install script; `pnpm audit` is clean. packages/ui exports `./generated/*`, so apps/desktop imports each type from its own file.

### As built (group C)
- **Layout:**
  - fleet-api-types: `app`; `typescript::{export_api, export_app}`.
  - fleet-client: `CustomCa::{to_pem, fingerprints}`, `Client::discard_session`.
  - The desktop crate:
    - `configure`, `run`
    - `keychain::{KeychainFactory, KeychainStore, KeychainError, StoreFactory}`
    - `profile::{ServerProfile, Loaded, load, save, fingerprint}`
    - `state::AppState`
    - `startup::{install_crypto_provider, StartError}`
    - crate-private: `commands`, `endpoints` (the methods of `AppState`), `error`
  - packages/ui: `api/{ApiClient, ApiError, schemas, limits, validated, context, FakeApiClient, UnavailableApiClient, describeError}`.
  - apps/desktop: `TauriApiClient`.
- **The spike** linked the desktop crate's tests with aws-lc-sys and the bundled SQLite next to tauri-build's msvcrt stub without trouble. It found the coverage wrapper gap above.
- **Tests,** red first against compiling stubs that returned wrong values:
  - fleet-api-types' `app`: 1 of 14, on the stub's empty messages; a second failure was the test's own field-order mistake, fixed.
  - `tests/limits.rs`: red on a missing constant.
  - fleet-client: 7 of 7 `CustomCa` tests and 4 of 4 `discard` tests.
  - The desktop crate: 94 of 129. The 35 that passed are the existing `navigation`, `focus` and probe tests, plus checks a stub satisfies too: both sides of an equality returning the stub's `store`, the fake slot over the real ordering code, the fixed texts. The ordering test also failed against a std mutex taken inside the blocking call.
  - `tauri-hardening`: 10 of 34.
  - `coverage-gates`: 7 of 23.
  - The UI: 25 of 31.
  - `TauriApiClient`: 4 of 24, against a renamed command, a dropped argument and no rejection mapping.

  Then all passed. No snapshot is new or changed.
- **Coverage:** the desktop gate measured 94.95 % on the dev machine; CI's `tauri-build` job measures it on every push.
- **Lockfile:**
  - Cargo.lock gains only keyring-core 1.0.0 and windows-native-keyring-store 1.1.0.
  - pnpm-lock.yaml gains `@tauri-apps/api` 2.12.1 and zod 4.6.5.
- **Not yet checked live:** the user's hands-on steps in the PR (the Credential Manager entry appearing and going away, `just dev-server` over loopback HTTP).

### Group D: dependencies and the release age *(the user's approvals, 2026-10-13)*
- **Pinned exactly, each the newest release at least 3 days old:** `@tanstack/react-router` 1.170.42, `@tanstack/react-query` 5.104.1, `sonner` 2.0.8, `zustand` 5.0.15, `lucide-react` 1.53.0 (ISC), `cn` 0.4.0, `class-variance-authority` 0.7.1 (Apache-2.0), `@radix-ui/react-slot` 1.4.0, `@radix-ui/react-label` 2.1.16, and `@testing-library/user-event` 14.6.7 (dev); P8.7 adds `react-hook-form` 7.89.0 and `@hookform/resolvers` 5.9.1. All MIT unless named. None runs an install script.
  - The router brings `@tanstack/router-core`, `history`, `react-store` and `store`, `seroval` and `seroval-plugins`, `cookie-es` and `isbot` (Unlicense); class-variance-authority brings `clsx`; the Radix packages `react-primitive` and `react-compose-refs`.
  - **`cn`'s npm name** held an unrelated package (0.1.x, 2013) until shadcn took it over (0.2.0, 2026-09-01); 0.4.0 is published from `shadcn-ui/cn` by GitHub Actions with npm's trusted publishing, and its tarball has no `eval`, `new Function` or `<style>` creation.
- **Single Radix packages** *(the user's condition)*: `@radix-ui/react-slot` and `@radix-ui/react-label`, never the `radix-ui` umbrella, which brings 55 primitives for the two used. Each copied component's `import { X } from "radix-ui"` becomes `import * as X from "@radix-ui/react-x"` (both export `Root`), and every later copy gets the same edit.
- **`minimumReleaseAge: 4320`** *(the user's rule)* in `pnpm-workspace.yaml`: pnpm 12 refuses a release younger than 3 days when it resolves, transitive ones included, and verifies the whole lockfile against it at every install, `--frozen-lockfile` included. A fix that can't wait goes into `minimumReleaseAgeExclude` with a comment and the user's approval. CLAUDE.md states the rule.
  - **Found when it was switched on:** the lockfile already held 13 approved entries younger than 3 days (`@tauri-apps/cli` 2.12.2 and its 11 platform packages, `tinybench` 6.2.2, all published 2026-10-10), so every install failed until 2026-10-13 20:08 UTC. They were excluded locally while developing *(the user's choice)*. To commit without waiting *(the user's decision)*, both of group D's commits carry a `minimumReleaseAgeExclude` of exactly those 13 versions, commented "approved on 2026-10-13, already on main since group C, remove after 2026-10-13 20:08 UTC"; a last commit before the merge removes it, with a clean install under the full rule shown in the PR, so `main` never carries it.

### Group D: no runtime styles *(the user's decisions)*
- **The problem.** The release CSP's `style-src 'self'` blocks a `<style>` element or a constructed stylesheet added at runtime, while `just dev-app` (no CSP) shows nothing wrong. sonner 2.0.8 runs `__insertCSS(…)` at import time, appending one to `<head>`. React's `style={…}` props go through the CSS object model, which the CSP allows.
- **sonner is patched** *(the user's choice over an unpatched sonner, an own toaster or Radix Toast)*: `patches/sonner@2.0.8.patch` (`pnpm patch`, `patchedDependencies` in `pnpm-workspace.yaml`) deletes only the top-level `__insertCSS(…)` call in `dist/index.mjs` and `dist/index.js`. `styles.css` imports `sonner/dist/styles.css` into Tailwind's `components` layer, so the bundle serves it from 'self' and utilities still win. **Every sonner bump redoes the patch and runs the guard below.**
- **Two guards, red first** *(the user's design)*:
  - `Toaster.test.tsx` imports sonner afresh, shows a toast and requires no `<style>` in the document. It was red against the unpatched 2.0.8.
  - **The absolute guard** (`src/test/styleGuard.ts`, run from the setup file's afterEach before Testing Library's cleanup): after every test the document holds no `<style>`, no stylesheet and no adopted stylesheet, whoever added it and when. So a library that injects at import time fails the first test of every file that imports it, and a component that renders one fails too. Its unit test imports a test-only module that injects at import time (red against a no-op guard); a throwaway test showed the global hook failing on it. Vitest processes no CSS here, so nothing had to be turned off.
- **Checked in the bundle:** `vite build` holds no `createElement("style")` or `insertRule`, and the unused `__insertCSS` function is tree-shaken out.
- **zod is jitless** *(the user's choice, after the live check)*. The debug exe's first live check found one violation: `script-src` blocked an `eval`. zod 4.6.5 (group C's) probes for a JIT with `new Function("")` at its first object parse; the probe is caught and zod parses without JIT, but WebView2 reports the violation at every start. `src/api/zod.ts` runs `z.config({ jitless: true })` and re-exports `z`; every UI module imports zod from there, and Biome's `noRestrictedImports` refuses `zod` and `zod/**` anywhere else in packages/ui (probed: both refused, the module itself allowed), so no schema exists before the config. `src/api/zod.test.ts` parses through `validated()`, the forms' schemas and `isCommandError` with `Function` spied and requires no call: red against the module without the config (5 calls: the probe and the compiled parsers), then green. The rebuilt exe showed no violation. **A zod bump checks that `jitless` still skips the probe.**

### Group D: shadcn's components *(the user's choices)*
- **Six, copied from the registry:** Button, Input, Label, Textarea, Card and Alert, from `https://ui.shadcn.com/r/styles/radix-nova/<name>.json` (what the CLI 4.21.4 reads), never through the CLI, which installs packages of its own choosing (the umbrella, at versions the release age may refuse).
  - The CLI's own edits are repeated: aliases (none in these six) and `"use client"` dropped, since rsc is false.
  - Ours: the Radix import above, and Biome's `useImportType` (`import type * as React`), formatting and import order. A lint finding gets a minimal fix, never an ignore comment *(the user's rule)*.
  - Each file's header names its source, the MIT license and every edit.
- **Our own:** `FieldMessage` and `PasswordInput` (P8.7), and a `Toaster` over sonner with `theme="light"` and lucide's icons (shadcn's own needs next-themes, whose inline script the CSP would block).
- **Native checkboxes** *(the user's choice)* instead of shadcn's Checkbox and another Radix package. No shadcn tailwind.css and no tw-animate-css: none of the six uses their variants or animations.

### Group D: the shell (P8.6) *(the user's choices)*
- **Routes in code** over **a memory history** *(the user's choices over the router plugin's generated file, and over hash or browser history)*: the URL never changes, so a reload or a restart starts at `/`, and nothing depends on how Tauri serves an unknown path. `/` is the launch; `/setup`, `/login` and `/register` are signed out; `/home`, `/server` and the `/admin` screens sit in a signed-in layout.
- **One runtime per App** (`app/runtime.ts`): the UI stores, the QueryClient, the API client and the router, built once and without side effects, since StrictMode may build it twice. The stores are zustand stores created per App, so tests never share state *(the user's choice of zustand)*.
- **The QueryClient** *(the user's choice)*: no retries, no refetch on focus or reconnect. `me` is fresh for 60 s *(the user's choice)*, so a role changed by an Admin shows within about a minute of moving between screens; the server profile never goes stale, and the app invalidates it after its own changes.
- **The launch** *(the user's choice)*:
  - `serverProfile()`: `none` or `unreadable` → `/setup`.
  - Then `me()`: Ok → signed in, then Home; `no_session` → login, with no toast.
  - `unreachable` or `timed_out` → the start screen, waiting while the banner retries.
  - Anything else → the start screen with describeError's text and **Try again, Server settings and Log out** *(the user's addition: a session the app can't use, like a keychain entry it can't read, must not leave the user stuck)*.
- **The watched client** *(the user's choice over the caches' `onError` and `meta`, which is kept per query key and never sees direct calls)*: `watched(validated(client))` sees every call's outcome. Each call records the session's generation and the banner's epoch when it starts.
  - `unreachable`/`timed_out` raise the banner; `certificate` raises its certificate state.
  - **`no_session`** ends the session only while it's signed in and only for a call of the current generation; the flip happens once. So, as the user required: one navigation, one cache clear and one toast (a fixed id) however many calls fail; nothing at launch, inside the sign-in flow or during and after a deliberate logout; and an answer from before a re-login can't end the new session.
  - **The end goes to /login first, then empties the cache** *(the user's change)*, so no mounted screen refetches after its data is gone. A logout and a server switch do the same.
  - **A success takes the banner down** *(the user's choice)*, but only for a call that started after the banner went up *(the user's rule, like the generation)*, and does what a successful check does: it refetches the active queries and re-runs the current route's loader, so no screen stays in its error state.
- **The banner** *(the user's choices)*: one `checkServer()` every 10 s while down, never two at once, plus "Retry now". A busy (503) or rate-limited (429) check waits its `retry_after_secs`, at least 10 s; any other failure keeps the banner up with its text. A refused certificate gets its own state: no timer, the shared certificate text, "Server settings".
- **Role-aware navigation** *(the user's choice)*: one table with each item's privilege. Home and Server for everyone; Users and Invites for Admins and the Owner, behind the admin layout's guard, which answers "not found" to anyone else, like the server's 404. A role this app doesn't know gets a Member's privilege. The two admin screens are placeholders until group E.
- **The error boundary** *(the user's choice)*: a class component around everything, and the router's `defaultErrorComponent`. "Something went wrong in the app." with Try again and Restart (a reload, which starts at the launch). An API error shows describeError's text; any other error only its name, never its message or stack. The error goes to `console.error`.
- **Logout** *(the user's choices)*: the header's "Log out" sets `logging_out`, then `/login` and the cache. `unconfirmed` adds a warning that the server's session may stay valid until it expires. A rejection (`ipc`, `interrupted`) stays signed in with "Couldn't log out: … Try again."
- **Home** shows who is signed in, with which role, on which server.
- **Light theme only** *(the user's choice)*; Plan.md's P11.10 follows Windows' setting later.

### As built (group D, P8.6)
- **Layout (packages/ui/src):**
  - `app/`: `runtime`, `stores`, `watched`, `queryClient`, `leave`, `context` (router context, stores provider), `rootRoute`, `router`
  - `api/queries`: `meQuery`, `serverProfileQuery`, `useMe`, `useServerProfile`
  - `features/shell/`: `launch`, `guards`, `nav`, `routes`, the hooks `useLogout` and `useServerRetry`, and the components; `features/home`, `features/admin`
  - `components/ui/` (the copies) and `components/Toaster`; `lib/origin`
  - `App` builds the runtime once; `AppRoot` renders one (tests pass their own and an `onRestart`).
- **Tests,** red first against compiling stubs:
  - the style guard: 3 of 4; sonner's guard against the unpatched package
  - the stores and the watched client: 23 of 32; the QueryClient's defaults: 1 of 2 (shown against a stub after the module came first)
  - the shell (launch, session end, header, router, nav, banner, error screen, App): 64 of 67, the 3 that passed being what a stub also does
  - **the desktop crate:** `a_logout_removes_a_keychain_entry_that_cant_be_read` runs the real `KeychainStore` over keyring-core's mock with non-UTF-8 bytes in the entry: `me()` answers `store`, `logout()` removes the entry, the next login gives a step. It pins existing behaviour (fleet-client's logout clears the store even when its load failed), so it was shown red through a temporary change that skipped the clear after a failed load *(the user's choice)*, then reverted.

  Then all passed. No snapshot is new or changed.
- **Checked live** in a `just app-build --debug` exe over WebView2's DevTools protocol on 127.0.0.1, without a server (so nothing could be saved): the login, register and server setup screens, a refused address, an unreachable one (the banner came up), the custom CA box. No CSP violation and no console error after the zod fix, no `<style>` element or adopted sheet in the document, sonner's 24 rules in the bundled stylesheet, `Object.prototype` frozen, no `window.__TAURI__`.
- **Found while building:** a check that settled before React rendered set `checking` to true and back in one batch, so the effect that waited on it never ran again and the 10 s rhythm stopped. The wait now chains from the end of each check.
- **Test isolation:** sonner replays every undismissed toast to a newly mounted Toaster, so the setup file dismisses them all after each test.

### Group D: `session_discard` *(the user's choice: a new code, not moving the store's build)*
- **The problem, found in planning.** `set_server_profile` answered `store` in two places: from `connect()`, which builds the new origin's keychain store before the exclusive guard (nothing changed), and from `discard_session()` after the old server's logout (logged out). The UI couldn't tell whether the user was still signed in.
- **The fix.** The new store is still built before the guard, so a broken Credential Manager is found before anyone is logged out, and `store` from there keeps meaning "nothing changed". A failed discard after the logout answers the new `CommandErrorCode::SessionDiscard` ("The new server's old session can't be removed from the Windows Credential Manager."). So `session_discard` and `profile_save` only ever mean "logged out, the old profile stays" *(my choice of two codes over one, which the user left to me: each keeps its own fixed sentence)*.
- **What changed with it:** the generated `CommandErrorCode.ts`, `ApiError`'s code record and `describeError` (both failed to compile until they had it, by design). `ipc-contract.json` doesn't change: its `set_server_profile` entry answers `invalid_server_url`.
- **Tests:** `a_switch_whose_new_entry_cant_be_cleared_…` now expects `session_discard` (red against `store`); the error mapping's test against a stub that answered `store`; and `a_refused_store_for_the_new_origin_leaves_a_live_session_and_the_profile_untouched`, which pins existing behaviour and was shown red through a temporary change that logged out of the old server before building the store.

### Group D: the screens (P8.7's part) *(the user's choices)*
- **One flow component** per `/login` and `/register` *(the user's choice over a store shared by one route per step)*: a reducer (`authFlowReducer.ts`) inside `AuthFlow`, so the password, the TOTP secret, the QR code and the recovery codes live only in its state. The password is kept only while a later step needs it: from a `setup_required` login (or a registration) until the login after the recovery codes answers.
- **Secrets never enter TanStack's caches** *(the user's rule, widened to answers)*: every call whose request or answer carries a secret is a direct call through `useApiClient()`, never a query or mutation, whose caches keep variables and data after a screen is gone: `login`, `register`, `loginMfa`, `mfaEnroll`, `mfaConfirm`, and in E `stepUp`, `changePassword` and `createInvite`. A test runs a finished and a cancelled flow and finds no password, TOTP secret, QR code or recovery code in the QueryClient. CLAUDE.md states the rule.
- **Every way into the app** sets the session signed in with a new generation *(the user's change)*: the launch's `me()`, the code step's success, a login that finds the same user's session still held, and "Continue as". A test shows a `no_session` after "Continue as" still ends the session.
- **The code step** *(the user's choices)*: every whitespace removed before sending, so `123 456` works. The flow counts wrong codes, since a used-up or expired step answers like a wrong one: "Wrong code. 2 tries left.", "Wrong code. 1 try left.", and the third goes back to the password step with "That sign-in step ended. Enter your password again." (username kept, password dropped). The step's deadline (from `expires_in`) does the same, by a timer and before each submit, which then sends nothing. "This step ends in 5 minutes.", plus one polite announcement in the last minute. A malformed code (422) shows at the field and doesn't count; `no_pending_step` ends the step.
- **The setup step** *(the user's choices)*: `mfaEnroll()` at the start and on "New QR code" (only the newest answer shows; StrictMode's double start included). The QR code only through `<img src>`, and only when it starts with `data:image/svg+xml;base64,`, since the CSP allows any `data:` image; otherwise a message points to the key. The key in groups of 4, selectable. A hint that only the newest QR code works and an entry scanned from an earlier one should be deleted *(the user's addition)*. The confirming code is 6 digits; a 422 keeps the step; a 401 or `no_pending_step` ends it; 10 minutes with the same warning.
- **Recovery codes** *(the user's choices)*: shown once as selectable text, "Copy all" (`navigator.clipboard.writeText`, with a toast that the clipboard and Windows' clipboard history may keep a copy, or one to copy by hand), a required "I've saved these codes". Continue re-runs `login()` with the kept password, which goes as soon as it answers, then "Wait until your authenticator app shows a new code, then enter it. The code you just used won't work again." If that login fails: the password step with "2FA is set up. Log in with your password and a code from your app." *(the user's change)*.
- **Registering** *(the user's choices)*: the invite trimmed of surrounding spaces and line breaks (ADR-0016), the password typed twice, both with a "show" toggle (one `PasswordInput`, also used by the login form and later by E's forms); the repeat is checked by the form's schema and never sent. Then a login with the same credentials straight into the setup; if it fails, the login form with the username, "Your account was created. Log in to set up 2FA." and the reason, the password dropped.
- **A session still held at login** (`already_logged_in`) *(the user's choice)*: the flow calls `me()` once. The same user (compared case-insensitively) opens Home; another user shows "You're still signed in as <u>." with "Continue as <u>" and "Log out" *(the user's addition)*; a failure shows its text with "Log out". The password is dropped either way.
- **Server setup** *(the user's choices)*: one form for `/setup` (signed out: the first run, an unreadable profile with its reason, from the login or start screen) and `/server` (signed in).
  - The address, and a native "Use a custom CA" checkbox that shows a textarea and "Choose file…" (`.pem`, `.crt`, `.cer`; refused unread above 64 KiB; the file only fills the textarea).
  - A current custom CA starts checked, with its certificate count and fingerprints to compare, and must be given again to keep it ("Paste or choose the CA again to keep it"), since its PEM never reaches the UI and an empty field would silently fall back to the system's roots *(the user's addition)*. Unchecking it is the way to the system's roots.
  - Signed in, the first Save shows "Saving a different server or CA logs you out of <origin>." with "Save and log out" and "Cancel". No Radix Dialog, whose scroll lock injects a `<style>`.
  - `none` keeps the session ("Nothing changed; you're still logged in."); `switched` ends it quietly (to /login first, then the cache), with the warning when the old server couldn't be told; `profile_save` and `session_discard` end it with "You were logged out, but the new server couldn't be saved: … The previous server is still set."; anything else changed nothing and shows on the form (a refused certificate with the shared text, `unreachable` with "Nothing changed.").
- **Texts.** The toasts, the banner's lines, the certificate explanation, the unreadable profile's reasons (each followed by "Enter the server again.") and the flow's step messages are the user's approved texts, with their changes. The headings, field labels, hints and the forms' own rule messages ("Enter the server's address.", "The passwords don't match.", "Paste or choose the CA's certificates.", a file too large for a CA, a QR code that can't be shown) were written in the build and are listed in the PR for review.

### As built (group D, P8.7's part)
- **Layout:** `features/auth/` (`authFlowReducer`, `schemas`, `hooks/useDeadline`, the components `AuthFlow`, `AuthCard`, `LoginForm` with `StillSignedIn`, `RegisterForm`, `CodeStep`, `SetupStep`, `RecoveryCodesStep`); `features/server/` (`schemas`, `unreadable`, `certificateHelp`, `hooks/useSaveServer`, `ServerForm`, `CaDetails`); `components/{FormField,FieldMessage,PasswordInput}`; `lib/formErrors` (a failed call's `fields` onto its form).
- **Tests,** red first: the flow's reducer 9 of 11 against an identity stub; the field components 3 of 4; the login, register and server screens 57 of 57 against group D's placeholder routes; the desktop crate 2 of 134 against a stub; the UI didn't compile until it knew the new code. The frozen-prototype flow test (launch, the first login with its 2FA setup, Home, a toast, a logout, with `Object.prototype` frozen before any import) passed at once, so it was shown red through a temporary shadowing `toString` assignment in the flow, then reverted. Then all passed: 210 UI tests.

### Group D: the review of PR #37 *(the user's changes)*
- **A server switch while signed in is a deliberate logout.** `useSaveServer` sets `logging_out` before `setServerProfile`, so a call that answers `no_session` during the switch or right after its answer (the old server's session is gone) ends nothing and shows no "session ended"; `none` and an error that changed nothing call `cancelLogout` (signed in, same generation); `switched`, `profile_save` and `session_discard` end it quietly as before. Two tests, red against the old code: the `no_session` arriving before and after the switch's answer.
- **Late answers in the sign-in flow.**
  - Every step has an id (`stepId`, bumped at every change of step, replacing the form's `round`), and every action names the step it was issued in (`from`); the reducer drops an action whose step is no longer current. So a code checked after its step expired can't sign in, a confirm after the setup step ended shows no recovery codes, and a wrong code from an older code step doesn't count in a newer one.
  - Signing in and navigating check that the flow is still on screen and its step current (`alive`, the current step's id), so a late answer or a late `me()` check never signs in or navigates after the user left the route.
  - Each call runs through `guarded` for the step it started in: while it runs, that step's Cancel, links ("Register with an invite", "Server settings", "Back to login"), "Log out" and "Continue as" are disabled, and a second submit does nothing. A new step isn't blocked by an older step's call still in flight.
- **The setup secret** is the flow's: requested once when the setup step starts (a ref keeps StrictMode's second effect run from asking again) and on "New QR code", which waits while one loads and hides the old secret and QR code meanwhile (`loading`). The secret stays in the step's state and goes with it.
- **Edge's own reveal button** *(the user's addition)*. WebView2 is Edge, which adds its own password-reveal (and clear) button to inputs, so `PasswordInput` showed two eye icons. `styles.css`'s base layer hides `input::-ms-reveal` and `input::-ms-clear`. Checked in the debug exe over the DevTools protocol: a screenshot after typing showed both icons before the rule and only ours after it, and WebView2 keeps both rules in the CSSOM, so it understands them.
- **A step's deadline counts from when its request was sent**, not from its answer, so the client never waits longer than the server.
- **A failed login after registering lands on `/login`** with its notice and the username, handed over in the history entry's state (read back through a zod check; memory history, so it never leaves the app), so that form's "Register with an invite" link works.
- **Tests,** red first: 13 flow tests and the 2 switch tests against the old code; the reducer's 11 of 13 against the old reducer (the 2 that passed are what an identity reducer also does); the register suite's two handoff cases now expect `/login`. Eight tests pinned behaviour the old code already had and were shown red through temporary mutations, each restored: a double click on Verify and on the recovery codes' Continue (the guard and the button's `disabled` removed), "Continue as" (made to wait on a held `invalidateQueries`, after its first version turned out not to reach the guard), a call from before "Continue as" (`endIfSignedIn` without the generation), `me`'s invalidation at the code step, the same user's held session and "Continue as" (the invalidation removed), and the setup step's deadline check before submit (the check removed).

### Group E: the release age and the order *(the user's choices, 2026-10-13)*
- **Group D's exclude is gone.** `main` still carried its 13 entries when group E started, at 14:44 UTC. They reached 3 days only at 20:08 UTC, so the branch's first commit removed them and no `pnpm install` ran before then. The first install after it, which added P8.9's dev dependencies, is the clean install under the full rule shown in the PR. Until then pnpm's pre-run dependency check printed "installing dependencies" failures, which didn't stop any script.

### Group E: step-up for the admin writes *(the user's decision, from the threat model's stolen-PC case)*
- **The three admin writes need a step-up.** `PATCH /users/{id}`, `POST /invites` and `DELETE /invites/{id}` register with `step_up_route`, checked by the `SteppedUpUser` extractor only, as `POST /me/mfa/reset` is *(the user's choice over a re-check in the transaction)*. Listing stays authed. [ADR-0016](0016-fleet-server-auth-conventions-and-phase-7-refinements.md)'s amendment holds the server's details.
- **The app asks only when the server wants it** *(the user's choice over asking before every write or a client-side timer)*. `useStepUpAction` sends the write. On 403 `step_up_required` its row or form shows "Confirm it's you" inline (`StepUpPrompt` around `StepUpForm`), steps up with a direct call, and sends the same write once more. Several writes within 5 minutes ask once. A retry the server refuses again shows its message, never a second prompt.
- **Later phases decide the same per action** *(the user's request)*: Plan.md's notes on Phases 9 and 11 ask, for each destructive or privilege-changing action, whether it needs a step-up.

### Group E: the wrong-code signal *(the user's decision, before the app shows it)*
- **The count leaves out the typos of the login that succeeded.** A warning that fires on every typo teaches everyone to ignore it, including the time it's real.
  - The password step mints a `LoginStepId` (fleet-core), which the mfa challenge carries.
  - The code step records it as `login_step_id` on its `auth.login` Failure entries and on the Success.
  - `failed_code_attempts` leaves out the failures of the newest Success's own step.
- So a count above 0 means a login step that never succeeded. [ADR-0016](0016-fleet-server-auth-conventions-and-phase-7-refinements.md)'s amendment gives the server's side; the warning's text no longer mentions the user's own typos.

### Group E: the profile (P8.7's part) *(the user's choices)*
- **Routes:** `/profile` is the overview: the account, the warnings, the two flows' buttons and the sessions. `/profile/password` and `/profile/2fa` sit beside it, each a flow of its own *(the user's choice over flows inline or separate pages)*.
- **Step-up first, then the action** *(the user's choice over one combined form)*. "Confirm it's you" takes the password (`stepUp`, a direct call; it lives only in `StepUpForm`, which empties its field after every answer).
  - **The password:** the new password typed twice, before the step-up's 5 minutes end, counted from when it was sent. A weak password's 422 costs one request per retry instead of two Login-bucket requests.
  - **The authenticator:** `mfaReset` right away, then the setup step and the new recovery codes, with their own texts, and "Finish" without a new login.
  - `step_up_required` or a deadline goes back to the password with "That step ended. Enter your password again."
  - The reducer (`profileFlowReducer`) and the guards are group D's patterns: every step has an id, late answers count only for their step, and a call's step waits while it runs.
- **The warnings** *(the user's choice: the profile and Home)*. They show wrong codes since the previous login (naming the last) and 3 or fewer recovery codes left. On Home they link to the profile and can be dismissed for the current sign-in (the per-App `notices` store, keyed by the session's generation).
- **Sessions** *(the user's choices)*. "Show more". Another session's "Log out" runs at once; this session's is the header's logout.
  - **"Log out all other sessions"** asks first, then fetches **every** page the server lists before ending each other session, so an older one (a stolen PC's) on a later page goes too *(the user's change)*.
  - It **runs to the end even when the screen is left**, since stopping halfway could leave a stolen PC's session alive with nobody seeing the report *(the user's change at the plan's approval)*. The report shows on the screen, or as a toast.
- **Shared with group D's flow:** `hooks/useStepGuard`, `useEnrollment`, `nextSetup`, `isStepGone`, and `SetupStep`'s and `RecoveryCodesStep`'s texts as props.

### Group E: the admin screens (P8.8) *(the user's choices)*
- **Only what the server would allow** *(the user's choice)*. `permissions.ts` mirrors `authorize()`. Its test reads fleet-core's authorization-matrix snapshot itself (a `?raw` import) and checks every cell of the Users, CreateInvite and RevokeInvite tables, with their headers and row counts, so a rule change on the server fails the UI's test, the way `ipc-contract.json` keeps the commands in step *(the user's change)*.
- **Inline confirmations, no dialog** *(the user's choice)*. A role change, disabling and revoking an invite each ask first, naming what they end: all of a user's sessions, and with a demotion or a disabling their pending invites. Enabling runs at once.
- **The invite's code** is shown once in the screen's state. `createInvite` is a direct call; Done or leaving drops the code.
  - **"Copy invitation"** *(the user's choice)* copies the server, the code, its end with its time zone, and a custom CA's fingerprints with "Ask me for the CA file".
  - **"Copy code"** copies the code alone.
  - Both copies toast the clipboard note.
- **Paging:** "Show more" on the users, invites and sessions *(the user's choice)*.

### Group E: the e2e suite (P8.9) *(the user's choices)*
- **The real client, over HTTP** *(the user's choice over a separate fetch client, MSW's service worker, or a REST client with tokens in JavaScript)*. The e2e entry (`apps/desktop/e2e`) renders the desktop app's UI with its real `TauriApiClient`, over `fetchInvoke`. `fetchInvoke` sends each command's args as JSON to `/e2e/ipc/<command>` and settles like Tauri's `invoke`.
  - Every answer is a 200 with an envelope, so the browser logs no failed request.
  - The e2e build holds no token: the fake stands in for the Rust side as well.
- **MSW in the Playwright process** through `@msw/playwright` (Playwright's routing): no service worker and nothing of MSW in any bundle. The fake backend (`e2e/fake/backend.ts`) answers exactly `ipc-contract.json`'s commands with the generated types and the Rust side's fixed messages, fresh per test.
- **The installed Edge** *(the user's choice)*, `channel: "msedge"`: the engine of WebView2, and nothing downloaded outside the lockfile.
- **The release CSP in e2e** *(the user's choice)*. `vite preview` serves the release CSP of `tauri.conf.json` with only `connect-src` changed to `'self'` (a test compares them, and checks the served header).
  - Every browser spec freezes `Object.prototype` as Tauri does.
  - It fails on any CSP violation, console error, uncaught error, `<style>`, adopted stylesheet or stylesheet from another origin.
- **Never shipped** *(the user's rule)*:
  - The e2e entry builds only through `vite.e2e.config.ts`, into `dist-e2e/`.
  - Biome refuses any import of `e2e` in `apps/desktop/src`.
  - `just e2e` builds both shipped bundles (the desktop frontend and packages/ui's web entry) into `target/e2e/` and runs `scripts/e2e-bundle-guard.mjs`. It refuses the e2e marker or `/e2e/ipc/` in them, and requires the marker in the e2e bundle.
- **The specs** *(the user's choice: the named flows plus the DoD's story)*:
  - logging in with the code step
  - the first login's 2FA setup with recovery codes
  - registering with an invite into the setup
  - the story: the Owner invites (the step-up prompt, the code once, copied), the invitee registers, sets up 2FA and logs in, and the Owner finds them in the user list
- **Where it runs** *(the user's choice)*: `just e2e` is part of `just check` and `just ci`, and a new `e2e` CI job runs on windows-latest. `packages/ui`'s own web entry keeps `UnavailableApiClient` for `pnpm dev`.
- **Dependencies** *(the user's approval, all exact, devDependencies of `@afkfleet/desktop`)*:
  - `@playwright/test` 1.64.0 (Apache-2.0, 2026-10-07)
  - `msw` 3.0.3 (MIT, 2026-10-10)
  - `@msw/playwright` 0.7.0 (MIT, 2026-09-28)
  - `@types/node` 24.19.2 (MIT, 2026-10-09)

  None runs an install script.

### Group E: the DoD and the threat model *(the user's choices)*
- **The friend's PC moves to Phase 12** *(the user's choice over a LAN proxy with its own CA now)*. Phase 8 proves the rest on the user's PC (the release exe over loopback, by the PR's hands-on steps) and in Playwright. Phase 12's DoD gains the line.
- **The threat model's "Desktop app (B2, B7)"** covers:
  - what crosses the webview boundary (never a token) and what sits on the PC
  - a STRIDE table citing the desktop and fleet-client tests
  - a stolen or shared PC with a live session in the Credential Manager: what it gives, what step-up now keeps from it, malware reading the credential, and the remedies

  `scripts/threat-model-citations.mjs` now also walks `apps/desktop/src-tauri`.
- **P11.11's screenshots** *(the user's addition)*: the installed Edge moves with Windows, so P11.11 decides how to keep screenshots stable.

### As built (group E)
- **Layout (packages/ui/src):**
  - `hooks/useStepGuard`
  - `components/StepUpForm`
  - `lib/{stepUp, time, clipboard}`
  - `features/profile/` (`profileFlowReducer`, `schemas`, `routes`, `hooks/useLogOutOthers`, the components `ProfileOverview`, `SecurityWarnings`, `SessionList`, `ProfileFlow`, `ConfirmStep`, `NewPasswordStep`)
  - `features/admin/` (`permissions`, `invitation`, `hooks/useStepUpAction`, the components `UsersScreen`, `InvitesScreen`, `StepUpPrompt`)
  - `features/auth/{hooks/useEnrollment, stepErrors}`
  - the `notices` store
  - `api/queries`' paged queries
- **Layout (apps/desktop):**
  - `e2e/` (`app/{main, fetchInvoke, marker}`, `base`, `csp`, `fake/{backend, handlers}`, `fixtures`, `specs/`, `unit/`)
  - `vite.e2e.config.ts`, `playwright.config.ts`, `tsconfig.e2e.json`
- **Server:** fleet-core `LoginStepId`; `LoginService::new` takes the random source; `Challenges::issue_mfa` takes the step's ID; `LoginError::StepId`.
- **Found while building:** `showFieldErrors` kept only a field's last message, so a weak password's 422 showed only zxcvbn's last hint, not "is too weak". Every message now shows, one per line (`FieldMessage` keeps line breaks), red first.
- **Tests,** red first:
  - **The server:**
    - the step-up registration 5 of 5
    - fleet-client 4 and the desktop 3 until they stepped up
    - 38 of the 49 admin tests without their stamps
    - the wrong-code count 3 of 3
    - the login step's metadata and error path through mutations
  - **The UI:**
    - the profile 52 against compiling stubs
    - the admin screens 33 of 38
    - `formErrors` 1 of 3, and `tests/limits.rs`
    - the session list's "every page" and "runs to the end" through mutations of `useLogOutOthers`
  - **The scripts:** the citation walk 3 of 3, the bundle guard 3 of 4.
  - **The e2e specs,** written with the code, through mutations: an injected `<style>`, a `console.error` and an `eval` (each caught), the fake skipping the step-up, `e2eCsp` returning its input, `fetchInvoke` resolving everything, the fake missing a command, and the marker in the shipped desktop bundle.

  Then all passed: 1199 server tests, fleet-client's 147, the desktop crate's 134, the UI's 329 Vitest tests and the 17 Playwright specs. The OpenAPI snapshot changed twice (the step-up's `x-step-up` and 403 texts; `FailedCodeAttempts`' description), read before accepting.
- **Lockfile:** pnpm-lock.yaml gains the four packages with the transitive set of the scratch resolution, except `undici-types` 7.24.6 (from `@types/node` 24) and jsdom's existing `tough-cookie` and `tldts`.

### Group E: the review of PR #38 *(the user's change)*
- **A refusal's wait counts down.** A 429 (a rate limit, or a locked username) or a 503 (busy) carries the server's `Retry-After`. The forms showed it as static text ("Try again in N s.") with the submit enabled, so every click during the wait was one more refused request.
- **`useRetryWait`** (`hooks/`) starts the wait a refusal asks for, ticks once a second from its start (the wait is whole seconds), and ends it on the tick that reaches the deadline, keeping the end's notice until the next submit. **`RetryWaitMessage`** (`components/`) shows it.
  - Every form that sends a request uses them: login, the code step, the 2FA setup, registering, `StepUpForm` (the profile's step-up and the admin prompt), the new password and the server form. Its submit stays disabled while the wait runs, and a submit then does nothing.
  - A login that ran by itself (after registering, or after the recovery codes) hands its wait to the login form in the notice (`Notice.wait`, through the handoff's zod check too), which starts the countdown.
- **Screen readers hear the start and the end, never each second** *(the user's rule)*. The alert's spoken text names the whole wait ("Try again in 30 seconds.") and never changes. The ticking "Try again in 29 s." is `aria-hidden`. The end, "You can try again now.", is a status that appears once.
- **After a reload** the countdown is gone; the next try shows the server's remaining wait again, which the server enforces anyway *(accepted by the user)*.
- **Tests,** red first with fake timers against a stub hook: the hook 4 of 6, the message 2 of 3 (it checks the spoken text without the hidden parts), and every form 10 of 10 (`test/retryWait.ts`: the countdown, the disabled submit, a click that sends nothing, the end). The two that passed against the stub check refusals a stub also gives. A first version chained a timeout from each render, which fake timers advanced only one tick per step; a single interval replaced it.

### Notes for later groups
- **P8.6/P8.7 (D), from group C:**
  - Components take the client with `useApiClient()`, and TanStack Query's hooks wrap its methods. A failure is an `ApiError`: build texts on `describeError`, show `invalid_input`'s, `invalid_server_url`'s and `invalid_ca`'s `fields` next to their inputs, and keep the default branch on `api.code`.
  - Forms reuse `schemas.ts` (`@hookform/resolvers` arrives with the first form, with the user's approval).
  - **Server setup:**
    - `server_profile`'s `unreadable` asks for the server again.
    - Warn before `set_server_profile` while logged in, since any change but an identical profile logs out.
    - `change: "none"` kept the session.
    - `change: "switched"` carries the old session's logout.
    - A custom CA's fingerprints are for the user to compare with what the server's admin gave them.
  - **A logout's `unconfirmed`** says the server's session may live until it expires.
  - **The banner** retries with `check_server` (`health_ready`), so a 503 `busy` with `retry_after_secs` reads differently from `unreachable`.
- **P8.6/P8.7 (D):**
  - The unreachable banner shows on `Unreachable` and `TimedOut`.
  - Server setup explains `Certificate`.
  - A 429 or 503 shows `retry_after`.
  - `expires_in` restarts a timed-out login or setup step.
  - Register-then-log-in never meets `AlreadyLoggedIn`, since registering holds no session.
  - From group B: shadcn's packages (`cn`, class-variance-authority, radix-ui, tw-animate-css, lucide-react, and the `shadcn` package's tailwind.css or a copy of it) arrive with the first component, with the user's approval. Radix's dialogs insert `<style>` elements, which `style-src 'self'` blocks: check them in a debug build.
- **E, from group D:**
  - The profile's step-up and password change, and the invite's creation, are direct calls (the secret rule above); reuse `PasswordInput`, `FormField`, `showFieldErrors` and `AuthCard`'s pattern.
  - Replace the admin placeholders (`features/admin`) and add Profile to the navigation table (`features/shell/nav.ts`).
  - The 2FA replacement (`mfaReset`, then enroll and confirm) can reuse `SetupStep` and `RecoveryCodesStep`.
  - P8.9's web build replaces `UnavailableApiClient` with MSW; the watched client and the stores work the same there. *(Group E chose otherwise: the e2e entry in apps/desktop reuses `TauriApiClient` over a fetch-based `invoke`, and packages/ui's web entry keeps `UnavailableApiClient`.)*
- **E, from group B:** `scripts/threat-model-citations.mjs` walks only `crates/`, so it must also walk `apps/desktop/src-tauri` (skipping `node_modules`, `gen` and `target`) before B2 cites the desktop tests (group C's are in `tests/bridge/`, `tests/acl.rs` and the in-crate `keychain`, `profile` and `error` tests). A friend's dev build needs `bundle.active` and the installer's icons (P12.5 at the latest).
- **From group E:**
  - **P9, P11:** decide for each destructive or privilege-changing action whether it needs a step-up (Plan.md's notes); the app reuses `useStepUpAction`.
  - **P11.11:** the screenshot tests need a stable rendering, but the installed Edge moves with Windows and the runner image.
  - **P12:** Phase 12's DoD holds "a friend on another PC installs the release build, registers with an invite and logs in with 2FA", moved from Phase 8.
- **P9.3:** fleet-server leaves `stable_crates` when azalea-auth arrives; fleet-client leaves with it, since fleet-server is its dev-dependency and `--all-targets` builds it.
- **P11:** the WebSocket client joins fleet-client, with tokio-tungstenite and futures-util (§5).

## Consequences
- **Easier:**
  - No token of any kind can leave the client by accident: the app's Rust side never handles one.
  - The refresh rules the server's tests pin are tested from the client's side too, against the real server and against forced interleavings.
  - A logout or any other end can't leave a token behind, whatever a refresh in flight does.
  - A custom CA is pinned: nothing else is trusted for that profile, and a key picked by mistake is refused.
- **Easier (group B):**
  - The desktop shell is hardened before any command exists: a new command is refused until it's listed and granted, and `just tauri-check` fails on any change to the CSP, the capabilities, the global API or tauri's features.
  - The webview can't be steered to another site, and a second launch can't open a second window.
- **Easier (group C):**
  - The UI talks to the server through one typed client whose every command, argument and error is generated or pinned. A renamed command or argument fails the contract fixture on both sides, a new error code fails to compile, and a new command fails `just tauri-check` until it's listed, granted and in the fixture.
  - A refresh token can only ever be sent to the server it came from, and a session resumes only under the exact profile it was created with.
  - A broken profile file or keychain never stops the app from starting; the UI is told why.
- **Easier (group D):**
  - Every call's outcome is handled in one place, so a screen never re-implements the banner or the session's end, and a session ends exactly once.
  - A library that injects styles at runtime fails CI before the release build is ever tried.
  - No npm release younger than 3 days can enter the lockfile, transitive ones included.
- **Easier (group E):**
  - A stolen or shared PC with a live session can't change a password, an authenticator, a role or a user, or mint an invite, without the password.
  - The wrong-code warning fires only for a login step that never succeeded, not for a user's own typo.
  - A rule change in `authorize()` fails the admin screens' test until the UI follows.
  - Every push runs the UI in Edge under the release CSP, so a style injection or an `eval` fails CI before any exe is built.
- **Harder:**
  - The refresh and the code step outlive their caller in a task the client spawns, an exception to the task-ownership rule.
  - fleet-client's tests build the whole server (and argon2 at the baseline), so they take a few seconds.
  - A server can't be configured with a request timeout above 19 s.
  - Requests serialize, so a field added to a request must serialize correctly too (the round-trip test covers it).
  - (Group B) The desktop crate builds only on Windows. Linux CI never compiles it, Linux coverage never measures it (group C adds a Windows gate), and its cargo deny pass covers Windows only.
  - (Group B) Doctests can't include the desktop crate.
  - (Group B) Every hardening change also changes `scripts/tauri-hardening.mjs`.
  - (Group B) No tool that needs TypeScript's compiler API can join before TypeScript 7.1.
  - (Group C) A keychain call that outlives the client's timeout keeps running in its thread, a fourth exception to the task-ownership rule.
  - (Group C) Switching servers always logs out, even when only the CA changed, and a reinstall loses the session.
  - (Group C) Every new command touches build.rs, the capability, the hardening script, the contract fixture, `ApiClient`, `TauriApiClient` and `FakeApiClient`.
  - (Group C) The desktop coverage build instruments every crate through RUSTFLAGS (`--no-rustc-wrapper`), so it's slower than the wrapper's.
  - (Group D) Every sonner bump redoes its patch. shadcn's components are copied by hand, with their edits repeated.
  - (Group D) A package younger than 3 days can't be installed without an approved exclude, and the first install after the rule failed on group B's and C's own entries for a few hours.
  - (Group D) The sign-in flow can't use TanStack's mutations for its calls, so it tracks pending and errors itself. The UI must keep two codes (`profile_save`, `session_discard`) that mean a switch logged out.
  - (Group E) An Admin's write may ask for the password first, once every 5 minutes. The e2e fake must follow every new command (its test against `ipc-contract.json` fails until it does). `just check` builds three bundles and starts Edge, which takes about a minute more. The installed Edge's version isn't pinned.

## Alternatives considered
- **Client-side request structs:** a second definition of every request.
- **fleet-core in §4's table for fleet-client:** a wider dependency for three ID types.
- **Copying the server's wiring into fleet-client's tests,** which could drift, **or the real clock,** with a 60 s access lifetime and waits for the next TOTP step.
- **A copy of the refresh token in memory:** the token in two places for a little resilience against a failing keychain.
- **Sending without an access token to get the 401:** one extra round trip per start.
- **Step tokens handed to the caller as opaque values:** the app's Rust side would hold tokens.
- **Logout only after the server confirmed:** an unreachable server would keep the session on the machine.
- **`http://localhost`, or any URL:** a password could cross a network in clear.
- **Environment proxies or reqwest's redirects:** traffic steered elsewhere.
- **The custom CA merged with the system's roots:** a mis-issued public certificate would pass.
- **A lenient or minimal PEM check:** a private key picked by mistake would pass.
- **Configurable timeouts, or 10/30 s:** knobs nobody needs, or a slower verdict when the server is gone.
- **Only a note on the server's timeout:** a config above 20 s would quietly break refreshes.
- **A 256 KiB or 4 MiB body cap:** too tight for P11's pages, or hardly a bound.
- **Retrying a 503 once:** a hidden wait the UI can't show.
- **Coarse errors:** the app would have to dig into sources to tell a refused certificate from a server that's down.
- **Only the public API in tests:** unforced interleavings and real 20 s waits.
- **The session code under the auth gate, or no gate:** the client's other security code (the certificate checks, the body cap) would stay at 80 %.
- **Waiting for an in-flight refresh at logout:** a logout could take a refresh's full timeout, and that refresh would save after the logout began.
- **The refresh inside its caller's future** (the first version): a dropped caller lost the rotated token. **A shared future instead of a task:** a future nobody polls any more stops too. **Reporting a lost task as `Unreachable`:** a bug or a shutdown would read as a server that's down.
- **(Group B) deny ignores** for the GTK3 stack's advisories and license: global, so they would hide the same crates in a shipped Linux binary. **WebKitGTK in Linux CI:** a never-shipped GTK build compiled and tested on every push. **`cfg(windows)`-gated Tauri dependencies:** cfg noise, and no help for deny, whose graph combines its targets.
- **(Group B) A relaxed CSP on Vite's dev server, or dev without one:** a second policy to keep, or no hot reload. **Tauri's default CSP modification:** the served header would differ from the config and let a future inline script in automatically.
- **(Group B) tauri's default features** (`dynamic-acl`, `x11`, `dbus`), **or keeping `common-controls-v6` with a manifest linked into the test executables:** runtime capability adding, or build plumbing for an About dialog we don't have.
- **(Group B) TypeScript 6.0.3, or 7 and 6 side by side:** slower checks, or two compilers to keep in step. **A full `shadcn init`, or clsx + tailwind-merge:** packages before anything uses them, or components that need editing. **Vitest in group C, jsdom 30, the cargo-installed Tauri CLI:** uncovered packages, a Node upgrade, or a from-source build in CI.
- **(Group B) A generated icon or one the user provides:** more code for a placeholder, or a wait. **`afkfleet.desktop` as the identifier:** no domain behind it. **Only the script as the pin:** the ACL's behaviour untested until group C.
- **(Group B) A dynamic VC++ runtime instead of excluding the desktop crate from doctests:** friends would need the redistributable.
- **(Group C) Only group D's commands now:** ACL and hardening changes in every later group. **IPC types in the desktop crate with a second ts-rs export, or a UI-side error model:** two writers to the generated folder, or a hand-written mirror. **A flat error code namespace, or only `{ code, message }`:** a closed and an open set mixed, or no `retry_after` and fields for D. **Logout as an error when the server can't confirm:** every caller would have to know that error means success.
- **(Group C) Refusing a profile change while logged in:** a check-then-swap race and a new fleet-client API. **Resuming a stored token after a reinstall or a broken file:** a session under certificates that can't be compared. **A shared keychain entry for every server:** a token one race away from another server. **Roaming or Enterprise persistence:** the token on other machines. **The raw pasted PEM, or base64 DER in the file:** the user's text stored as is, or a second parser.
- **(Group C) Plain `spawn_blocking` for the keychain, or a std mutex inside it:** a timed-out save could land after the clear that followed it (the test proved it). **A dedicated keychain thread:** one more long-lived thread with its own shutdown.
- **(Group C) Unit tests only, or a stub HTTP server, for the commands:** the switch rules untested, or a hand-written server that drifts. **Gitignoring the generated permissions:** a stale grant would stay local and unseen. **The default rustc wrapper for coverage:** the in-crate tests would count 0 % on Windows.
- **(Group D) sonner unpatched:** a CSP violation at every start, and its styles applied only through our import. **An own toaster or Radix Toast:** code we'd maintain instead of a two-line patch.
- **(Group D) The router plugin's generated route tree:** a dev dependency and a generated file for a dozen routes. **Hash or browser history:** a reload would resume a screen whose state lives only in memory, and browser history depends on Tauri's fallback for unknown paths.
- **(Group D) The caches' `onError` with `meta` for the global handling:** `meta` stays with a query's key, and direct calls never reach the caches. **A style guard relative to a baseline:** an import-time injection would be the baseline.
- **(Group D) A zustand store shared by one route per sign-in step:** the password in module state until something clears it. **Mutations with `gcTime: 0`:** an observer still holds the variables while mounted, and removal runs on a timer. **Re-running the password step itself after a timed-out code:** the password kept for as long as the user keeps failing.
- **(Group D) Checking `me()` after an ambiguous `store` from a server switch, or always treating it as logged out:** a guess where the Rust side can say it. **Moving the new store's build after the logout:** a broken Credential Manager would be found only after logging out.
- **(Group D) The `radix-ui` umbrella or the shadcn CLI:** 55 primitives for two, and packages the CLI picks itself. **TanStack Query's default retries:** an unreachable server would take over a minute to show.
- **(Group E) Asking for the password before every admin write, or a client-side window:** two Login-bucket requests per write, or a timer that guesses the server's stamp. **A re-check of the stamp in each admin write's transaction:** more code in three services for a gap of milliseconds. **No step-up for admin writes:** an unlocked PC could change roles and mint invites.
- **(Group E) One form with the current and the new password:** each retry of a weak password costs two Login-bucket requests. **The flows inline on the profile, or separate pages:** a flow's secrets would outlive it on screen, or the overview would split up. **A warning on the profile only:** a sign of a guessed password could go unseen.
- **(Group E) Showing every action and letting the server refuse, or disabling with a reason:** buttons that can only fail. **A copy of the matrix's cases in the UI's test:** a rule change on the server would pass unnoticed.
- **(Group E) MSW's service worker in the e2e bundle, a separate fetch client, or a REST client with tokens in JavaScript:** MSW in a bundle, a second command mapping, or fleet-client's session logic rewritten in TypeScript. **Playwright's downloaded Chromium:** a browser from outside the lockfile, and not WebView2's engine. **No CSP in e2e:** the style guard alone, without the browser's real blocking.
- **(Group E) A LAN proxy with its own CA for the friend's PC now:** a dev-only Caddy setup Phase 12 replaces anyway.
- **(Group C) A stateful fake server for the UI's tests:** the server's rules re-implemented. **zod inside TauriApiClient only:** fakes would skip the checks. **Tauri's `mockIPC`:** an import the rule would need to allow. **Review only for the import rules:** an event or window API could slip in unnoticed.
