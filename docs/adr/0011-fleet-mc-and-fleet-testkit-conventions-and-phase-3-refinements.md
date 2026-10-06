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

### Dependencies
These crates are in the Plan.md §5 registry already; these are new uses, approved by the user:
- **fleet-mc:** `uuid` and `reqwest` (no features). They're needed only to name `Uuid` and `reqwest::Proxy` in azalea's `AccountTrait`.
- **fleet-mc, dev:** `tracing-subscriber` (the log-redaction tests) and `rstest`.
- **fleet-testkit:** `tokio` (`sync`, `time`), for its channels and its paused-time liveness stamps.
- **`tokio-util`** (Phase 3's "Introduces" line) is added only if a task really needs it.

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
- **Test time.** These tests wait for a real OS thread, and tokio's paused clock auto-advances while the test runtime is idle, which races the thread. So host-pool tests use **real time with generous upper-bound timeouts that only fire on failure, and never a sleep**. A hang is a job blocked on a std `sync_channel` that the test releases at the end. Paused time stays the rule wherever all waiting happens on the test's own runtime.

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

### Connector (P3.4)
- **Startup.** Variant C, with auto-reconnect and auto-respawn disabled and the single-threaded executor (ADR-0008 §1–3).
- **Connect timeout.** It runs from `connect()` until `Joined`: resolve, TCP connect, login, session join and configuration. Its default is 30 s, from the new agent key `[runtime] connect_timeout_secs` (Appendix A, parsed in P5.1). On expiry the session reports `ConnectionFailed(TimedOut)` and calls `exit()`.
- **Resolving** happens inside the session, under that timeout, so a resolve error arrives as `ConnectionFailed`. `ConnectError` stays `HostUnavailable` only.
- **Crashes.** An `AppExit` error becomes `Disconnected(SessionCrashed)`.
- **Diagnostics.** `live_worlds()` (a `Weak` of the ECS) and `dropped_chat()` are public diagnostics.
- **Fault injection** for the containment test is a small hook that adds a system to a session's App. It sits behind a fleet-mc cargo feature that's off by default; the exact API is shown in group D's PR.

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

### Actions (P3.6)
- **Pure helpers** are unit-tested: the pitch clamp after a `Turn`, the yaw wrap, skipping non-finite angles (logged), and the attack packet's bytes.
- **Live tests** check the effects through RCON.
- **HoldUse.** azalea 0.16.0 has only a one-shot use (`start_use_item`). Holding works by sending `UseItem` and later a raw `PlayerAction{ReleaseUseItem}`, for items with a use duration (shield, bow, food); block and entity clicks would need repeated packets. P3.6 verifies this on the test server and records the result here. **P3.9** then adds `HoldUse{on}` as its own task right after group E, or closes with a note if it isn't feasible.

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
  - fleet-core grows two disconnect reasons in group C.
  - The host-pool tests run in real time, bounded by timeouts.
  - The slow suite needs Docker and takes about ten minutes.
- **Later phases inherit requirements:**
  - **P5.1** parses `connect_timeout_secs` and `max_abandoned_threads`.
  - **P5.3** exits the agent when the pool's abandoned-thread count reaches the limit.
  - **P4.9** exports the pool's and connector's diagnostics as metrics.
  - **Every azalea bump** re-checks the three ignored advisories and removes the hickory ones once azalea uses hickory ≥ 0.26.1.

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
