# 0017. fleet-client and desktop app conventions, Phase 8 refinements

- **Status:** Accepted
- **Date:** 2026-10-12
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

  Then all passed. The 31 that passed against the stubs check refusals, fixed texts and `Debug`, which a do-nothing stub satisfies too. No snapshot is new or changed.

### Notes for later groups
- **P8.4 (C):**
  - The keychain implements `SessionStore`, doing its blocking calls off the runtime.
  - The server profile reuses `ServerUrl` and `CustomCa::from_pem`.
  - `main` installs the aws-lc-rs provider before building the client.
  - `CommandError` maps `ClientError` with a default branch, since `ApiFailure::code` is `Open`.
  - The step tokens never reach the Rust side's own state either: the client holds them.
- **P8.6/P8.7 (D):**
  - The unreachable banner shows on `Unreachable` and `TimedOut`.
  - Server setup explains `Certificate`.
  - A 429 or 503 shows `retry_after`.
  - `expires_in` restarts a timed-out login or setup step.
  - Register-then-log-in never meets `AlreadyLoggedIn`, since registering holds no session.
- **P9.3:** fleet-server leaves `stable_crates` when azalea-auth arrives; fleet-client leaves with it, since fleet-server is its dev-dependency and `--all-targets` builds it.
- **P11:** the WebSocket client joins fleet-client, with tokio-tungstenite and futures-util (§5).

## Consequences
- **Easier:**
  - No token of any kind can leave the client by accident: the app's Rust side never handles one.
  - The refresh rules the server's tests pin are tested from the client's side too, against the real server and against forced interleavings.
  - A logout or any other end can't leave a token behind, whatever a refresh in flight does.
  - A custom CA is pinned: nothing else is trusted for that profile, and a key picked by mistake is refused.
- **Harder:**
  - fleet-client's tests build the whole server (and argon2 at the baseline), so they take a few seconds.
  - A server can't be configured with a request timeout above 19 s.
  - Requests serialize, so a field added to a request must serialize correctly too (the round-trip test covers it).

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
