# 0011. fleet-mc and fleet-testkit conventions and Phase 3 refinements

- **Status:** Accepted
- **Date:** 2026-10-06
- **Related:** Plan.md Phase 3 (P3.1–P3.9), §5, §6, Appendix A; ADR-0003, ADR-0004, ADR-0008, ADR-0010

## Context
Phase 3 builds two crates:
- `fleet-mc`, the azalea adapter behind the `fleet_core::mc` ports
- `fleet-testkit`, the fakes that Phase 4 tests against

Before writing any code, the Phase 3 plan did two things.

**It re-checked azalea's advisories.** The threat model and ADR-0003 require them to be resolved or accepted before azalea enters the workspace.

**It listed where Plan.md, ADR-0008 and ADR-0010 were ambiguous or contradicted each other.** Examples:
- **The abandoned-thread limit.** Plan.md §6 row 8 says the agent process exits above it. ADR-0010 says `connect()` returns `HostUnavailable` then, which the bot treats as a transient retry.
- **The connect timeout** has no config key (flagged in ADR-0010).
- **A fail-fast `refresh()`** (ADR-0010) can only return a Microsoft-flow `AuthError`, which azalea logs at error level as if it were true.
- **Session-server errors.** ADR-0008 §9 turns every recorded join error into `AuthInvalid`, so a Mojang outage would put every online bot into `Failed(Auth)`.

The azalea facts below were checked in the azalea 0.16.0 sources.

The user answered every question on 2026-10-06, and this ADR records the answers. Phase 3 merges in five group PRs plus one task PR (below). If a later group's review changes a decision, that group's PR amends this ADR.

## Decision
### Grouping
One branch and one PR per group, one commit per task, in this order:

| Group | Branch | Tasks |
|---|---|---|
| A | `p3/p3.1-testkit-fakes` | P3.1 |
| B | `p3/p3.2-p3.5-host-pool-and-events` | P3.2, P3.5 |
| C | `p3/p3.3-account-adapter` | P3.3, alone, so the Minecraft token handling gets a focused review |
| D | `p3/p3.4-p3.6-connector-actions-slow-suite` | P3.4, P3.7, P3.6, in that commit order. P3.6 can only be proven against a live server, so the slow harness comes first |
| E | `p3/p3.8-teardown-and-wrap-up` | P3.8 and the phase wrap-up |
| — | `p3/p3.9-hold-use` | P3.9, its own task after group E |

### Supply chain
Re-checked on 2026-10-06 against the current rustsec.org entries, and with a `cargo deny` dry run of the root `deny.toml` against azalea 0.16.0's graph. The dry run found exactly the items below and nothing else. azalea 0.16.0 is still the latest release; azalea `main` (unreleased, Minecraft 26.2) already uses hickory 0.26.1 but still uses `rsa`.

| Item | Why it's acceptable | Decision |
|---|---|---|
| RUSTSEC-2023-0071, `rsa` 0.10.0-rc.18 (Marvin timing side channel). No fixed version exists | The attack targets RSA decryption, and azalea never decrypts. It only parses Mojang's chat-signing key and signs our own chat with `sign_with_rng`, which is blinded. The handshake encrypts with the server's public key through the separate `rsa_public_encrypt_pkcs1`. Only online accounts sign, only our own rate-limited chat, and the key is Mojang's short-lived chat-signing key, not an account credential | Accept. `ignore` with a reason, re-checked at every azalea bump |
| RUSTSEC-2026-0118, `hickory-proto` 0.25.2 (NSEC3 loop) | The affected code only compiles with hickory's DNSSEC features. azalea enables `system-config` and `tokio`, so hickory-proto gets only `std`, `tokio` and `futures-io` | `ignore` ("not compiled"), removed at the next azalea bump |
| RUSTSEC-2026-0119, `hickory-proto` 0.25.2 (quadratic name compression) | Only *encoding* is quadratic. We encode only our own one-question queries for a validated `ServerAddress`; responses are decoded | `ignore` ("unreachable"), removed at the next azalea bump. SRV records keep working |
| `minecraft_folder_path` 0.1.2 (Unlicense), `socks5-impl` 0.8.7 (GPL-3.0-or-later) | Both are unconditional azalea dependencies, and both licenses are GPL-compatible (the second is our own) | Per-crate `[[licenses.exceptions]]`; the global allowlist stays as it is |

The `deny.toml` entries land in group B, together with azalea, because cargo-deny warns about ignores that match nothing. ADR-0012 records them there.

*(group B)* With azalea in the workspace, `cargo deny check` reported exactly these five items. [ADR-0012](0012-azalea-advisory-and-license-exceptions.md) records the entries; the license exceptions are pinned to the crate versions.

### Dependencies
These crates are in the Plan.md §5 registry already; these are new uses, approved by the user:
- **fleet-mc:** `uuid` and `reqwest` (no features). They're needed only to name `Uuid` and `reqwest::Proxy` in azalea's `AccountTrait`.
- **fleet-mc, dev:** `tracing-subscriber` (the log-redaction tests) and `rstest`.
- **fleet-testkit:** `tokio` (`sync`, `time`), for its channels and its paused-time liveness stamps.
- **`tokio-util`** (Phase 3's "Introduces" line) is added only if a task really needs it.
- **fleet-mc** *(group B review)*: `azalea-chat` and `azalea-language`, `=0.16.0` like azalea, which already depends on both, so the graph doesn't change. They're used only by the bounded renderer (P3.5): azalea doesn't re-export azalea-chat's `PrimitiveOrComponent`, the type of a translation's arguments, and the renderer looks up translation templates itself.
- **fleet-mc, dev** *(group C, the user's approval)*: `log` 0.4.34, already in the graph through azalea and reqwest. The log capture's own test emits a `log` record to prove that records from `log`-based crates (reqwest, rustls) reach the capture.
- *(group C)* fleet-mc also uses `secrecy`, which §5 already lists for it. The workspace entry of `reqwest` fixes the `rustls` feature, per the root manifest's TLS rule; azalea-auth already enables it, so fleet-mc's build doesn't change, and fleet-mc enables no features of its own.
- *(group D, the user's decision)* **The log capture moves to fleet-testkit** (`fleet_testkit::log_capture`), so fleet-mc's slow tests can reach it and P9's server redaction test can reuse it. fleet-testkit gains `tracing` and `tracing-subscriber`, and `log` as a dev-dependency. fleet-mc drops `tracing-subscriber` and `log` and dev-depends on fleet-testkit. As testkit library code the capture doesn't panic: `install()` and `check_absent()` return errors, and a `SecretLeak` names only targets and counts.
- *(group D)* **`testcontainers` 0.28.0** (§5, no features) is a fleet-mc dev-dependency for the slow tests. `cargo deny` passes with it, and it adds no duplicate crates beyond those already reported.

### Lint guards
- **Bounded channels.** The root `clippy.toml` bans `tokio::sync::mpsc::unbounded_channel` through `disallowed-methods`.
  - **The two exceptions.** azalea's API forces two unbounded channels: the join callback and `LocalPlayerEvents` (ADR-0008 §1). Each gets `#[expect(clippy::disallowed_methods, reason = "…")]` pointing here; the user approved these two.
  - **The event channel** is drained at once into a bounded bridge (ADR-0008 §4).
- **Crate-local configs.** Clippy reads the nearest `clippy.toml` and doesn't merge it with the root one. So `scripts/clippy-config.test.mjs` fails `just scripts-test` when a crate-local config lacks any root setting: the test allowances or the `unbounded_channel` ban. fleet-core's config therefore carries the ban too.
- **Microsoft flows** (group B). fleet-mc's own `clippy.toml` bans azalea's Microsoft login functions:
  - `auth`, `get_ms_link_code`, `get_ms_auth_token`, `interactive_get_ms_auth_token`, `refresh_ms_auth_token`, `get_minecraft_token`
  - the account cache functions
  - azalea's Microsoft `Account` constructors

  So "agents never receive Microsoft tokens" (security rule 6) is checked by the lints, not only by review. The paths are verified by a probe, as in ADR-0010.

  *(group B)* The 0.16.0 list has 13 paths, all through azalea's re-exports and without `allow-invalid`, so a path that stops resolving after a bump is reported:
  - the six `azalea::auth` functions above
  - `azalea::auth::cache::{get_account_in_cache, set_account_in_cache}`
  - `azalea::account::Account::{microsoft, microsoft_with_opts, microsoft_with_custom_client_id_and_scope, with_microsoft_access_token, with_microsoft_access_token_and_custom_client_id_and_scope}`

  A temporary probe referenced all 13, and clippy flagged each one; `azalea::auth::sessionserver::join` and `Account::offline` stayed allowed. The probe wasn't committed. `check_ownership` and `get_profile` take a Minecraft token, not a Microsoft one, so they aren't banned.
- **The azalea-auth rule, clarified.** fleet-mc uses only `azalea::auth::sessionserver` and `azalea::auth::certs`, through azalea's re-export, with no direct dependency on `azalea-auth`. Microsoft flows are fleet-server's alone.

### Test kit (P3.1)
`fleet-testkit` has `FakeConnector`, `FakeSession`, `FakeEvents` and a test-side `SessionController` per session.
- **Connect results.** `FakeConnector` returns scripted results in order: a session, or a `ConnectError`. With nothing scripted it starts a session.
- **Liveness.** Both stamps read `tokio::time::Instant::now().into_std()` until a test freezes one. So paused-time `advance` moves them, and a frozen stamp stays where it was.
- **Hang mode** freezes both stamps, and every call fails with `SessionError::TimedOut`.
- **Fail-on-action** makes `perform` fail with a chosen `SessionError`.
- **The event contract** of ADR-0010 is enforced by the fake, so the actor's tests rely on the real contract:
  - at most one terminal event, then `None`
  - `Died` at most once until `respawn()`
  - only `Chat` is dropped, and counted, when the bounded event queue is full
- **The log** records performed actions, sent chat, respawns and disconnects, in order.
- **No panics.** The fakes are library code and follow every lint, so misuse returns an error or `false`.

### Host pool (P3.2)
- **One thread per session.** One `std::thread` per session, with a current-thread runtime and a `LocalSet` (ADR-0008 §2).
- **Thread names.** `mc-` plus the last 12 hex characters of the `BotId`, which is its random part. That's 15 bytes, Linux's limit; the leading v7 timestamp would make truncated names collide.
- **Jobs.** They go over a bounded queue. A full queue is `QueueFull`, and no answer within the job timeout is `TimedOut`.
- **Counting.** A drop guard counts live threads. Shutdown waits with a timeout, and a thread that doesn't end is abandoned (detached) and counted. `live_threads()` and `abandoned_threads()` are public diagnostics for P3.8 and P4.9.
- **The abandoned-thread limit** resolves the contradiction:
  - Above a configurable limit (default 3, agent key `[runtime] max_abandoned_threads`), the pool refuses new threads, so `connect()` returns `HostUnavailable` and no more threads pile up.
  - The library never ends the process: the agent watches the count and exits at the limit, and Docker restarts it (P5.3, Plan.md §6 row 8).
  - *(group B, the user's decision)* "At the limit" means `abandoned_threads() >= max_abandoned_threads`: the pool refuses new threads at the same count where the agent exits. So the limit is a `NonZeroUsize`, since 0 would refuse every thread.
  - *(group B, the user's decision)* `abandoned_threads()` only counts up, even when an abandoned thread ends after all. `live_threads()` goes down then. The agent's restart resets both.
- **Test time.** These tests wait for a real OS thread, and tokio's paused clock auto-advances while the test runtime is idle, which races the thread. So host-pool tests use **real time with generous upper-bound timeouts that only fire on failure, and never a sleep**. A hang is a job blocked on a std `sync_channel` that the test releases at the end. Paused time stays the rule wherever all waiting happens on the test's own runtime.
- **No `CancellationToken`** *(group B, the user's decision)*. This is a deliberate exception to the CLAUDE.md rule that every spawned task has a child `CancellationToken`:
  - A host thread's jobs are tasks of the thread's `JoinSet`, which owns them.
  - A oneshot stop signal ends the job loop, and dropping the `JoinSet` and the `LocalSet` cancels every task the session left, azalea's runner included.
  - So `tokio-util` stays out of fleet-mc until a task needs it.
- **Threads dropped while hung** *(group B review)*. A thread whose last handle drops before any shutdown outcome becomes an orphan of the pool: its exit signal and a deadline, the shutdown timeout after the drop. `spawn()` and `abandoned_threads()` settle the orphans first, without blocking and without spawning tasks: an ended one is forgotten, and one past its deadline is counted as abandoned and logged at `warn`. Otherwise a hang whose handles were dropped would leak uncounted, and the limit wouldn't see it.
- **Handles** *(group B, the user's requirement)*. `HostThread` is `Clone`. The thread also ends once every clone is dropped, because the job channel and the stop signal close with the last one. So an actor that panics or is aborted without calling `disconnect()` can't leave a bot running. A test covers it, and no task on the host thread may hold a clone (P3.4).
- **Never joined** *(group B)*. Joining a thread blocks, which the caller's runtime must not do, and a hung thread can't be joined at all. Each thread sends an exit signal as the last thing it does: its drop guard lowers the live count and then closes a `watch` channel. Shutdown waits for that signal, and `HostThread::ended()` exposes it.
- **Jobs** *(group B)*. `run` queues a job at once and returns a `Send` future for the answer. A job whose caller has given up by the time it starts is skipped. A panicking job answers `Closed`, is logged at `warn`, and the thread keeps serving.

### Account adapter (P3.3)
- **Token.** An immutable `SecretString`, so it needs no lock.
- **Certificates.** In a `std::sync::Mutex`, read poison-tolerantly, never held across an await.
- **`Debug`** is hand-written and redacted.
- **`join()`** calls `azalea::auth::sessionserver::join` inside `tokio::time::timeout`, since azalea's reqwest client has no timeout. azalea runs `join()` on Bevy's process-wide IO pool, not on the bot's host thread, so the account reports to the session's bridge over a bounded channel.
- **Session-server errors** are split:
  - `InvalidSession` and `ForbiddenOperation` → `AuthRejected`.
  - `Banned` and `MultiplayerDisabled` → a new permanent reason.
  - An outage, an HTTP error, rate limiting, an unknown or unexpected response, or our own timeout → a new transient reason.

  That's a small fleet-core change (`DisconnectReason`, the classifier and their tests) inside the P3.3 commit. The names are settled there, and ADR-0010 is amended then.
- **`refresh()`** is a no-op that returns `Ok(())`. azalea then retries `join()` once with the same token, which fails the same way, so its logs stay accurate, and fleet-mc ends the session on the first recorded rejection anyway. This keeps ADR-0010's intent (the adapter never asks for a token) and replaces its wording "fails fast".
- **Offline accounts** come from `Account::offline`.
- **The adapter doesn't check `expires_at`.** The credential provider (P4.3) and the session server decide.
- **Log redaction.** The tests install a process-wide subscriber that captures **every level (TRACE) from every target, azalea included**, because azalea logs from the host thread and from Bevy's IO pool. Each asserts that the token never appears:
  - a fast test covers our adapter's own paths
  - the online-mode slow scenario covers azalea's full login path with a marker token
  - the user's real-account check covers it with the real token

  When the assertion fails, its message **never prints the captured lines or the token**. It reports only where the token appeared, as a target and a count, so a failing run with a real token can't leak it to the terminal.
- **Known limit.** `AccountTrait::access_token()` hands azalea a plain `String` copy that we can't zeroize.
- **Built in group C** (the user's decisions, 2026-10-07):
  - **The new reasons.** `DisconnectReason::AccountRestricted{restriction}` covers `Banned` and `MultiplayerDisabled`. It's permanent, with the new kinds `PermanentKind::AccountBanned` and `PermanentKind::MultiplayerDisabled`; the existing `Banned` stays the Minecraft server's ban, so the two stay apart. `DisconnectReason::SessionServerFailed{failure}` is transient, with `Unreachable` (`AuthServersUnreachable`, an HTTP error), `RateLimited`, `TimedOut` (our timeout) and `Unexpected` (`Unknown`, `UnexpectedResponse`). It mirrors `ConnectFailed{failure}`, so the bot status can say why. ADR-0010 is amended.
  - **`FailReason::Kicked{kind}` is renamed `FailReason::Permanent{kind}`**, since a session-server refusal isn't a kick.
  - **On our timeout**, `join()` returns `ClientSessionServerError::Unknown("no answer within the session-join timeout")`, so azalea's error log says what happened. It's neither `InvalidSession` nor `ForbiddenOperation`, so azalea doesn't refresh and retry.
  - **The report channel** is a bounded `mpsc` channel of capacity 1, written with `try_send`. It carries the classified `DisconnectReason`, and P3.4's host-thread pump turns it into `EventSink::terminate`. A full channel already holds the report that ends the session, and a closed one means the session is gone, so a report that doesn't fit is dropped without waiting. The account holds no `EventSink` clone, so the bridge's "a dropped sink ends the session" rule doesn't depend on azalea's lifetimes. An offline account's channel is closed from the start.
  - **The log.** A failed join logs one `debug` line with the `bot_id` and a fixed label of the error's kind, never azalea's error text, which can carry the session server's response body. The session's terminal event carries the reason.
  - **Test seam.** The join's bookkeeping (timeout, classification, report, log) is a function that takes the session-server future, so fast tests drive it with scripted results and, under paused time, a future that never ends. The one call into azalea is covered by the online-mode slow scenario (P3.7).
  - **Certificates** live in a small generic slot, a poison-tolerant `Mutex<Option<T>>` whose `Debug` shows only whether it's set. That's unit-tested without building an `RsaPrivateKey`, which would need `rsa` as a new dev-dependency.
  - **`refresh()`** is overridden explicitly, not left to azalea's default, so a bump can't change it silently.
  - **`McConfig::session_join_timeout`** holds the 10 s default.
  - **Dead code until P3.4.** Nothing outside the tests builds an account until the connector does, so `mod account` carries a temporary `#[expect(dead_code, reason = …)]` in non-test builds, approved by the user. P3.4 removes it, as it does for `mod events`.
  - **The log capture** is installed with tracing-subscriber's `try_init()`, which also installs the `tracing-log` bridge, so `log` records from reqwest and rustls are captured under their own target (the user's addition). Its own tests prove the bridge, span fields and a failure message without the secret.
- **azalea logs secrets at TRACE** *(found in group C, the user's decision)*. In every online session azalea fetches the chat-signing certificates with the Minecraft token, and `azalea_auth::certs::fetch_certificates` logs the whole response at `trace`, the chat-signing private key PEM included. That's a known limit (threat model).
  - *(the user's decision after review)* **Both binaries cap `azalea_auth` at `info`**, whatever the configured filter says: the agent in P5.2, fleet-server in P9.3. On fleet-server, `trace` would also log the Microsoft access token, the whole token response with the refresh token, the Xbox Live, XSTS and Minecraft auth responses, and the account cache. azalea-auth's `debug` lines carry nothing secret, but one level for both is simpler to check. P9's redaction test checks the server's cap.

### Connector (P3.4)
- **Startup.** Variant C, with auto-reconnect and auto-respawn disabled and the single-threaded executor (ADR-0008 §1–3).
- **Connect timeout.** It runs from `connect()` until `Joined`: resolve, TCP connect, login, session join and configuration. Its default is 30 s, from the new agent key `[runtime] connect_timeout_secs` (Appendix A, parsed in P5.1). On expiry the session reports `ConnectionFailed(TimedOut)` and calls `exit()`.
- **Resolving** happens inside the session, under that timeout, so a resolve error arrives as `ConnectionFailed`. `ConnectError` stays `HostUnavailable` only.
- **Crashes.** An `AppExit` error becomes `Disconnected(SessionCrashed)`.
- **Diagnostics.** `live_worlds()` (a `Weak` of the ECS) and `dropped_chat()` are public diagnostics.
- **Fault injection** for the containment test is a small hook that adds a system to a session's App. It sits behind a fleet-mc cargo feature that's off by default; the exact API is shown in group D's PR.
- **Built in group D** (the user's decisions, 2026-10-07):
  - **Shape.** `AzaleaConnector::new(&McConfig)` owns its host pool. `connect()` spawns the host thread and queues the session's driver there with `HostThread::start`, a crate-private addition to the host pool. That's a `JoinSet` job without an answer or a job timeout, so the driver's owner is the one this ADR approved. Then `connect()` returns. `McSession` is the `SessionHandle`.
  - **The driver** holds the session's only counting `EventSink` and no `HostThread` handle.
    - Every wait before `Joined` (resolving, the join callback, the connect) also watches the stop signal, the connect deadline and azalea's runner. The deadline is taken in std time in `connect()`, so a paused clock on the caller's runtime can't move it.
    - Its loop checks, in order: stop, the account's reports, the connect deadline until joined, the runner's end, then events. *(The user's notice on the plan)* The deadline comes before events, so a server that keeps sending events before `Joined` can't starve it. A unit test floods the loop and was red against the events-first order.
  - **Endings.**
    - A resolve error is `ConnectionFailed(Unresolvable)`, tested through the pure waiting helper, never through DNS.
    - A runner end nobody asked for (`Ok(AppExit)` included), a closed join callback or a closed event channel is `Disconnected(SessionCrashed)`.
  - **Teardown** follows ADR-0008 §10.
    1. `disconnect()` closes the bridge first, so the driver's end isn't reported as a crash.
    2. It stops the driver. The driver writes `AppExit` (with or without a `Client`), waits up to `app_exit_timeout` (2 s) for the runner, then drops the `Client` and the event receiver. The oneshot of `AppExit` is never polled again after it finished.
    3. `disconnect()` waits up to that plus 1 s for the driver, then shuts the host thread down, which abandons a hung one.
  - **The owner's bridge handle** is a `BridgeControl` that doesn't count as a source, so "a dropped sink ends the session" still holds.
  - **The `Client`** lives in a slot that only host-thread jobs clone from, so no handle outside the thread keeps a World alive. A drop guard empties it.
  - **Calls** fail with `NotInWorld` before `Joined`, or when the host thread finds no `Client`, and with `Closed` once the session ended or was torn down.
  - **Fault injection.** `AzaleaConnector::with_app_hook(self, impl Fn(BotId, &mut azalea::app::App) + Send + Sync + 'static) -> Self`, behind the off-by-default `fault-injection` feature. The hook runs after azalea's plugins and before the single-threaded executor is set, so its systems run single-threaded too. *(The user's notice on the plan)* With the feature and without `debug_assertions`, fleet-mc hits a `compile_error!`, so no release build can include the hook. Clippy's `--all-features` run and the slow tests are debug builds.
  - **Stubs inside the PR** *(the user's decision)*. P3.4's `perform`, `send_chat` and `respawn` check the state and run an empty host job, so P3.7 and P3.6 get real red runs. `respawn` already tells the bridge, inside its job.
  - **The event channel is attached when azalea spawns the bot** *(found in CI on Linux, the user's decision)*.
    - **The race.** In one `Update` pass, azalea spawns the bot's entity, sends the join callback, polls the connect task and reports a failed connect through `LocalPlayerEvents`. It drops the event when the entity doesn't have that component yet.
    - **Why only on Linux.** Variant C, as in ADR-0008 §1, the spike and azalea's own `Client::join`, attached it after the callback. On a loaded Linux host, a refused loopback connect failed before that, so the event was lost and the session ended at the connect timeout as `TimedOut`. Locally it took one CPU and eight parallel test processes to reproduce: 19 of 24 runs failed.
    - **The fix.** `build_app` adds an observer on `Add` of `LocalEntity` that inserts `LocalPlayerEvents` at once, before the connect is polled. *(The user's addition)* It moves the one sender into the bot's component, taken from an `Option`, so no clone stays in the App. The channel closes when the bot's entity goes, which is still a crash, and a second local entity gets no channel, logged at `warn`.
    - **Tests.** Unit tests cover the component at spawn, the second entity, the channel closing with the entity, and a connect failure in the spawn frame reaching the session; they were red first. Under the same Linux stress, 24 of 24 runs pass.
    - **Upstream.** azalea's own `Client::join` has the same race, so it joins the list of upstream reports left to the user (ADR-0008 §11).

- **Chat signing** *(found in group E by the user's real-account check; the user's decisions, 2026-10-07)*.
  - **The bug.** `send_chat` returned `Ok` for chat that a server enforcing secure chat drops.
    - azalea-client 0.16.0 fetches the chat-signing certificates in the background once the bot is in the game and authenticated (`chat_signing.rs`). Only when they arrive does it send `ServerboundChatSessionUpdate` and insert `ChatSigningSession`.
    - Until then, `chat/handler.rs` sends chat with `signature: None`, and the server answers "chat disabled due to missing profile public key". P1.8 only passed because it slept 3 s first.
    - A failed fetch is retried an hour later (`OnlyRefreshCertsAfter`). A fetch that hangs never ends, because azalea's HTTP client has no timeout.
  - **The state.** A `SigningPlugin` in every session's App publishes the session's signing state over a `watch` channel, every frame:
    - `NotNeeded`: an offline account, a server that didn't authenticate the join (offline mode), or one whose login packet says it doesn't enforce secure chat. azalea parses `enforces_secure_chat` but doesn't keep it, so the plugin reads it from the received packets.
    - `Ready`: `ChatSigningSession` is there.
    - `Failed`: `OnlyRefreshCertsAfter` without a session.
    - `Pending`: anything else.

    It also publishes when the bot entered the game state.
  - **`send_chat`.**
    - Commands, `NotNeeded` and `Ready` go out at once, so offline accounts never wait.
    - `Pending` waits on the caller's side, never on the host thread, until the bot entered the game plus `McConfig::chat_signing_timeout` (10 s). Measuring from the game state means a hung fetch can't make every call wait.
    - `Failed`, or `Pending` at that deadline, returns the new `SessionError::ChatUnavailable` without sending (ADR-0010). It's logged at `warn` once per session, and the session stays up. Later calls check again, so chat works once azalea's hourly retry succeeds. A key fetch that hangs keeps chat unavailable until the next reconnect, since azalea never retries it.
    - The host job checks the state again right before it queues the message.
  - **Tests.** The pure parts (`needs_signing`, `Certs`, `signing`, `decide`, the deadline) and the wait, under paused time, were red against stubs first. So were the plugin, on an App with hand-made components and login packets, and the fake's new `fail_chat`.
    - The offline slow scenarios show that offline accounts are unaffected.
    - Only the user's real-account check can show the fix end to end, since only a real token gets `IsAuthenticated`.
  - **Commands: a known limit, not decided** *(found by the user's real-account check)*. azalea sends every command unsigned. On a server that enforces secure chat, commands with message arguments (`/me`, `/msg`, `/tell`, `/w`, `/say`, `/teammsg`) are rejected:
    - the bot gets one system message (`chat.disabled.invalid_command_signature`) and stays up
    - the server logs an ERROR each time
    - `send_chat` returns `Ok` anyway

    The options are to sign commands, which needs the server's command tree, or to refuse such commands on enforcing servers with an error. P11.3 decides it for user commands and P4.5 for mode chat; command signing isn't built in Phase 3 *(the user's decision)*. The real-account check expects the rejection, and fails if the emote's echo ever arrives.

### Events (P3.5)
- **Chat:**
  - The sender comes only from `Player` and `Disguised` packets (ADR-0008 §7).
  - An unknown registry `ChatKind` falls back to `chat`.
  - The text is what the vanilla client shows (`unsigned_content`, else the signed content), without the chat-type decoration.
  - **Action-bar messages** (`overlay`) are status displays, not chat. They're dropped and counted.
- **Lifecycle events:**
  - `Joined` is the first `Spawn` only.
  - A `Died` before `Joined` is held until after it, because the state machine ignores `Died` outside Online.
  - `Died` is de-duplicated until respawn.
- **Liveness.** Ticks and received packets (`ReceiveGamePacketEvent`) only stamp liveness. So the `packet-event` feature stays off.
- **The bridge:**
  - It only drops `Chat`, counted, with one `warn` per burst.
  - Other sources inject terminal signals into it: the account's auth result, the connect timeout and `AppExit`. The first terminal signal wins.
  - Chat is logged only at `debug`, as a `?` field.
- **Refined in group B** (the user's decisions):
  - **Chat kinds.** A chat type's registry id is assigned by the server. fleet-mc reads it in vanilla's order, which azalea's `ChatKindKey::ALL` follows: 1 is an emote, 2 an incoming whisper, 4 `/say`. Everything else is `chat`, including the echo of a whisper the bot sent, team chat, unknown ids and inline (`Direct`) chat types. Resolving the id against the registry the server actually sent would need the session's ECS in the mapping. So a server whose data packs reorder chat types can mislabel a kind. That's a known limit; the text and the sender don't depend on it.
  - **Chat senders are server-attributed, not verified** *(corrected in the group B review)*. azalea 0.16 never verifies chat signatures, and fleet-mc shows a `Player` packet's `unsigned_content` when there is one. So a sender's name, a `Player` packet's UUID and the text are all only what the server claims. No permission or trigger decision may rely on a chat sender. A feature that needs one must verify the message signature and use the signed body, in an ADR of its own.
  - **System messages** keep their whole sanitized text, and their sender is never guessed.
  - **Action-bar messages** have their own counter, `ignored_action_bar`, apart from `dropped_chat`, and they log nothing. Some servers send several a second, and `dropped_chat` should stay a signal of overload.
  - **Unknown events.** azalea's `Event` is `#[non_exhaustive]`. A variant the mapping doesn't name lands in its catch-all, which ignores it and logs it at `debug`. Every azalea bump re-checks the variants against the mapping (ADR-0003, step 5).
  - **The queue** follows the fake's design: a `Mutex<VecDeque>` plus `Notify`, so lifecycle events always go in and `next()` is cancel-safe. Chat is dropped once the queue holds `event_capacity` events. The contract bounds the lifecycle events: one `Joined`, one terminal event, and one `Died` per respawn.
  - **Liveness stamps** are atomic offsets from the session's start, so they never move backwards. A Bevy plugin stamps every `ReceiveGamePacketEvent` in `Update`, as a closure system that holds the session's stamps.
  - **Dead code until P3.4.** Outside the tests, nothing calls the producer side (the mapping, the sink and the liveness plugin) until the connector does. The user approved a temporary `#[expect(dead_code, reason = …)]` on `mod events`, in non-test builds only. P3.4 removes it, and the expectation fails as soon as everything is used.
  - **Test values.** Kick reasons are built from JSON with `serde_json`, a dev-dependency, since azalea doesn't re-export `TranslatableComponent`.
- **A dropped sink ends the session** *(group B review)*. When the last `EventSink` clone is dropped, a session that hasn't ended gets `Disconnected(SessionCrashed)`, the first terminal event if no other came, logged at `warn`. Its events can't come anymore, for example because the host thread is gone, and the actor must not wait for them. After a terminal event or `close()`, it's a no-op.
- **Bounded rendering of server text** *(group B review, the user's decisions)*:
  - **The problem.** azalea's `FormattedText::to_string()` clones and re-renders every argument of a translation. For an unknown key, the template is the key itself, or the JSON `fallback`, and each `%1$s` re-renders its argument. So nesting grows exponentially: a 507-byte system message renders to 16.7 million characters.
  - **The renderer.** fleet-mc renders server text itself, wherever the mapping used `to_string()` (six places).
    - It follows azalea-chat 0.16's `TranslatableComponent::read` rules: `%%` is `%`; `%s` is the next argument; `%N$s` is argument N (one digit) and leaves the `%s` count alone; a missing argument is empty; any other `%` is written as it is; a `%` and a digit without `$s` makes the template invalid, and the translation shows its key. The template is `azalea_language::get(key)`, else the fallback, else the key.
    - It walks the text by reference, with an explicit stack instead of recursion, so hostile nesting can't exhaust the host thread's stack, and it never clones a subtree.
    - It stops after 4,096 characters or 16,384 steps, and reports whether it stopped. A step is a component visited, an argument substituted (whatever it renders to) or a character written. Every template character is either written or part of a placeholder, so the budget bounds all the work.
    - Tests compare it with azalea-chat's own rendering on ordinary text.
  - **Cut-off chat** goes through `IncomingChat::from_parts` with `truncated = true`. Only a cut-off text counts; a cut-off sender name is only capped. A kick's class comes from its top-level key, so cutting off its text never changes it.
  - **azalea still renders in two places we don't control:** its disconnect plugin formats kick reasons at `info`, and its own `read` panics on `%0$s` with overflow checks on. So the agent's default log filter keeps azalea's targets at `warn` (P5.2).

### Actions (P3.6)
- **Pure helpers** are unit-tested: the pitch clamp after a `Turn`, the yaw wrap, skipping non-finite angles (logged), and the attack packet's bytes.
- **Live tests** check the effects through RCON.
- **HoldUse.** azalea 0.16.0 has only a one-shot use (`start_use_item`). Holding works by sending `UseItem` and later a raw `PlayerAction{ReleaseUseItem}`, for items with a use duration (shield, bow, food); block and entity clicks would need repeated packets. P3.6 verifies this on the test server and records the result here. **P3.9** then adds `HoldUse{on}` as its own task right after group E, or closes with a note if it isn't feasible.
- **Built in group D** (the user's decisions, 2026-10-07):
  - **One synchronous host job per action.** Several `Client` methods panic when a component is missing (`set_direction`, `set_crouching`, `hit_result`, `with_raw_connection_mut`), so the job first checks the component through `get_component` or `try_query_self`. A missing one gives `NotInWorld`. No read guard is held while a `Client` method runs.
  - **Mapping.**
    - `Look` and `Turn` use `set_direction`. `Turn` reads `LookDirection`, wraps the yaw and clamps the pitch.
    - `Jump` is `jump`, `UseItem` is `start_use_item` (a block click when the bot looks at a block), `Sneak` is `set_crouching`, `SwingArm` triggers `SwingArmEvent`, and `SelectHotbarSlot` is `set_selected_hotbar_slot`.
    - `AttackFacingEntity`:
      - It takes the `HitResultComponent`, which azalea limits to the reach. Nothing in reach gives `Ok`, logged at `debug`.
      - It writes the attack packet by hand: a `VarInt` packet ID and a `VarInt` entity ID, through `RawConnection::write_raw` (ADR-0008 §8).
      - It swings and resets `TicksSinceLastAttack`, as azalea's own attack would.
    - The respawn writes `PerformRespawnEvent` and calls `BridgeControl::respawned` in the same job.
  - **Non-finite angles** are skipped before the job, logged at `warn`, and the call returns `Ok`.
  - **Live checks** are in `slow_actions_scenario`. Swings are seen by a second bot through a test-only system added by the `fault-injection` hook, which counts `Animate` packets *(the user's decision)*. A failed `execute if` answers nothing over RCON, so "not sneaking" is checked with `execute unless`.
  - **HoldUse: feasible** *(checked by a temporary probe, never committed, the user's decision)*. Against the 26.1 test server, with a bow and arrows:
    1. One `start_use_item` started drawing.
    2. A release after about 1 tick shot nothing (the control).
    3. 25 ticks of holding shot nothing yet.
    4. A `ServerboundPlayerAction { action: ReleaseUseItem, pos: BlockPos::default(), direction: Down, seq: 0 }` then shot one arrow. The bow's `minecraft.used` statistic went to 1, the arrows from 16 to 15, and one arrow entity appeared.

    The packet encodes correctly through azalea's normal writer (`RawConnection::write`, or `Client::write_packet` on the host thread), so no hand-written bytes are needed. **P3.9** maps `HoldUse{on: true}` to `start_use_item` and `{on: false}` to that release. Items without a use duration (block and entity clicks) aren't covered. To repeat the probe, run those steps with the `fault-injection` hook adding a system that writes the release packet when a flag is set.

### Slow tests (P3.7, P3.8)
- **Local servers only.** They use testcontainers with `itzg/minecraft-server`, offline and online mode, bound to localhost. The image and `VERSION` are the ones pinned in `deploy/compose.dev.yaml`, and a fast test asserts that the two pins match.
- **The online-mode container** (`ONLINE_MODE` and `ENFORCE_SECURE_PROFILE` on) contacts Mojang's session server, as the spike did. The automated tests use a garbage token or an offline account, never real credentials.
- **Few scenario tests.** nextest runs every test in its own process, so tests can't share a container, and every container downloads the server jar. So there are a few scenario tests, each owning one container and running several steps, serialized by a nextest test group.
- **Fault containment.** A panic injected into one session's ECS ends that session as `Disconnected(SessionCrashed)`. A second session from the same pool keeps ticking, sends chat and performs an action.
- **P3.8 leak test:**
  - It takes its baseline after a warm-up join (P1.9).
  - `live_threads()` and `live_worlds()` must return to that baseline on every platform.
  - On Linux only, the OS thread count from `/proc/self/status` must as well.
- **The user's real-account check** (group E):
  - It's a `manual_` test in its own nextest profile, run by a `just` recipe.
  - It reads `secrets/p1.8-account.txt`, which the user creates with the archived spike's `fetch-token`. The AI never reads it.
  - It joins the online-mode container and sends signed chat.
  - No error message ever includes the file's contents, format errors included; they name only the line number and the expected key.
- **A `slow-tests` CI workflow** comes in group E: weekly and on demand, ubuntu, not a required check.
- **Built in group D** (P3.7, the user's decisions, 2026-10-07):
  - **One test binary,** `crates/fleet-mc/tests/minecraft/`, with a harness, the fast `pins` test and a module per scenario: offline, online and fault containment.
    - The nextest test group `minecraft` (`max-threads = 1`) serializes the `slow_` tests.
    - `just test-slow` turns on `fault-injection`. The containment module is `cfg`-gated on it, so `just test` still builds the binary and runs `pins`.
  - **Containers.** `GenericImage` with the compose `tag@digest`, the dev stack's environment and `MEMORY` 1G. It waits for the image's healthcheck within 5 min. A host-config modifier binds the one mapped game port (host port 0) to `127.0.0.1` and turns `publish_all_ports` off, so RCON is never published.
  - **RCON** runs through `exec(["rcon-cli", …])`.
  - **Waiting.** Waits are bounded (60 s), and a failed wait shows the events the session sent meanwhile.
  - **Found: a bot never sees its own join message.** The server broadcasts it before it adds the player. So the offline scenario's second bot sees the first one join and rejoin, as in the spike, and both bots see the first one's chat echo.
  - **Red, then green** *(the user's decision)*. The chat steps failed against P3.4's stub `send_chat`; P3.7 wired it to `client.chat`, which also sends `/commands` (ADR-0008 §7).
  - **Results.** All three scenarios pass in about 50 s.
    - **Online mode:** a garbage token gives `AuthRejected` in seconds, an offline account is kicked with `unverified_username`, and the marker token never appears at any level of any target.
    - **Containment:** a panic hooked into one session's `GameTick` ends it as `SessionCrashed`. The other session on the same pool keeps ticking, chats and acts, and both threads shut down without being abandoned.
- **Built in group E** (P3.8 and the wrap-up, the user's decisions, 2026-10-07):
  - **The leak test,** `slow_teardown_scenario`, runs one warm-up cycle and three measured ones. Each cycle puts four bots online at once and ends them each way a session can end:
    - two with the full teardown
    - one kicked through RCON before its `disconnect()`
    - one whose handles are all dropped without `disconnect()` (the actor-panic path, see "Host pool")

    The full teardown is `McSession::disconnect()`, which follows ADR-0008 §10. P3.8's "not just `disconnect()`" means azalea's `Client::disconnect()`.
  - **Checks.**
    - After the warm-up and after every cycle, `live_threads()` and `live_worlds()` are **0**: they count only fleet-mc's own threads and Worlds. No thread may be abandoned.
    - **On Linux,** the OS thread count from `/proc/self/status` must reach **at most** the baseline, within 30 s. The baseline is taken after the warm-up, once no host thread is left in the OS's list. That's longer than tokio's 10 s keep-alive, so an idle blocking-pool thread that existed at the baseline can come and go without failing the test.
    - Every check runs while the container is up, so testcontainers' cleanup thread never counts.
  - **Red runs.**
    - Against a temporary, uncommitted break (the driver kept a `Client` clone), the scenario failed with "every World being freed after the warm-up didn't happen within 60s".
    - In CI on Linux, a temporary commit parked one thread after the baseline.
      - **The first run passed when it had to fail.** fleet-mc counts a host thread as ended just before its OS thread exits, so a warm-up thread still exiting inflated the baseline and hid the parked one.
      - **The fix:** the baseline is taken only once no thread named `mc-…` is left in `/proc/self/task`. A failed check shows the threads by name, now and at the baseline.
      - **The next run failed as it should:** "8 OS threads … more than the 7 after the warm-up", the extra one under the test thread's name. The revert passed.
      - **The 7 baseline threads** are the main and test threads, Bevy's four pool threads and async-compat's runtime thread.
    - The Linux run before merge used a temporary `push` trigger on the branch, removed before the PR was opened, since the PR's description can't be edited once it's open.
  - **The real-account check,** `manual_real_account_scenario`, joins the online-mode container with the user's real token, sends signed chat and waits for its echo, then sends `/me` and waits for the emote's echo, and tears down.
    - The steps only record their results, as event kinds. The teardown and the log checks always run, and failed steps are reported after them *(the user's decision, after a failed chat skipped the log checks)*.
    - `/me` is expected to be rejected, the known limit under "Chat signing". If the emote's echo arrives instead, the step fails with "azalea now signs commands: update this step and the P11 note".
    - **The user's results** (2026-10-07, after the chat-signing fix):
      - passed: the join, the signed chat, the token absent from every log at every level, and `PRIVATE KEY` only under `azalea_auth::certs`
      - `/me` was rejected, with one system message and an ERROR in the server's log
    - Then:
      - the token must not appear in the capture, at any level of any target
      - `PRIVATE KEY` may appear only under `azalea_auth::certs`, whose `fetch_certificates` logs the certificate response at `trace` in azalea-auth 0.16.0 (the accepted risk P5.2 caps). Any other target fails
  - **Only the user runs it.** Only the nextest `manual` profile includes `manual_` tests, and `just test-real-account` runs it. CLAUDE.md says the AI never runs either.
    - The file is read straight into a `SecretString` and parsed strictly: `name=`, `uuid=` and `token=` in `fetch-token`'s order, with CRLF, trailing empty lines and spaces accepted. Errors name only a line number and the expected key.
    - A failed wait names only the step and the kinds of events.
    - `fetch-token` writes no expiry, so a rejected join says the token may have expired and to fetch it again. The README says to fetch it right before running.
  - **The `slow-tests` workflow** (`.github/workflows/slow-tests.yml`) runs `just test-slow` on ubuntu, Mondays at 07:17 UTC and on demand. It has a 60 min timeout and isn't a required check.
  - **The threat model's B4 section** adds two accepted risks: Minecraft servers aren't authenticated, and a server can grow a bot's memory.

### Closed or moved items
- **ADR-0008 §11, "CPU without the `packet-event` feature":** closed without a measurement. fleet-mc builds without the feature, so the cost can only drop below the P1.7 numbers, and no decision depends on it.
- **ADR-0008 §11, the abandoned-thread limit:** decided above (host pool).

### Defaults
They aren't config keys yet: a fleet-mc config struct holds them, and P5 adds keys if needed.
- The event bridge holds 64 events, and the job queue 32.
- The job timeout is 5 s, and the session-join timeout 10 s.
- Teardown waits 2 s for `AppExit` and 5 s for the host thread.

## Consequences
- **Easier:**
  - Every Minecraft fault the runtime must survive can be scripted with the fake.
  - azalea's three advisories are accepted with a recorded reason and a removal trigger.
  - Two lint guards make the security rules about unbounded channels and Microsoft tokens mechanical.
- **Harder:**
  - fleet-core grows two disconnect reasons in group C, and `FailReason::Kicked` becomes `FailReason::Permanent`.
  - The host-pool tests run in real time, bounded by timeouts.
  - The slow suite needs Docker and takes about ten minutes.
- **Later phases inherit requirements:**
  - **P5.1** parses `connect_timeout_secs` and `max_abandoned_threads`.
  - **P5.3** exits the agent when the pool's abandoned-thread count reaches the limit.
  - **P4.9** exports the pool's and connector's diagnostics as metrics.
  - **P5.2** and **P9.3** cap `azalea_auth` at `info` *(group C)*.
  - **P12.2's** memory limit on the agent container also bounds the World a hostile server can grow *(group E, threat model)*.
  - **Every azalea bump** re-checks the three ignored advisories and removes the hickory ones once azalea uses hickory ≥ 0.26.1.
- **Flagged, not decided** (group B, the user's request):
  - **A per-server chat format (P4.1, P11.9).** Some servers send player chat through a plugin, as system messages like `[CLAN] Name : message`. A per-server chat format, next to the per-server conflict texts, could extract the sender from such messages for display.
    - A parsed sender is marked as parsed from the text. Like every chat sender, it's never used for any permission or trigger decision.
    - *(corrected in the group B review)* No chat sender is verified, a `Player` packet's UUID included: azalea 0.16 doesn't verify chat signatures, so senders are only what the server claims (see "Events"). A feature that needs a trustworthy sender must verify the signature and use the signed body, in its own ADR.
    - P3.5 keeps the whole sanitized text of system messages, so this stays possible.

## Alternatives considered
- **Blocking Phase 3 until upstream fixes the advisories.** No `rsa` fix is in sight, so it would stall the project.
- **Resolving addresses with the OS resolver, so hickory never runs.** Servers that rely on SRV records would then need an explicit host and port.
- **Adding Unlicense and GPL-3.0-or-later to the global allowlist.** It's broader than these two crates need.
- **Re-implementing Mojang's join call with reqwest instead of `azalea::auth`.** It's more HTTP and TLS surface for no gain.
- **Mapping every session-server error to `AuthRejected`,** as ADR-0008 §9 first read. A Mojang outage would fail every online bot until Reset.
- **A `refresh()` that returns an error.** It saves one Mojang call, but azalea would log a misleading Microsoft-flow error.
- **Counting abandoned threads only, or letting fleet-mc end the process.** The first lets threads pile up until P4.7. The second makes a library end its caller, which is hard to test.
- **A connect timeout for the TCP connect only.** A server that stalls the login would be left to its own 30 s kick.
- **Sharing one container across tests, or caching the server jar in a shared volume.** nextest can't share a container between processes, and a shared volume is mutable state between tests.
- **Measuring CPU without `packet-event`.** It would need a load-test harness, and it can't change a decision.
