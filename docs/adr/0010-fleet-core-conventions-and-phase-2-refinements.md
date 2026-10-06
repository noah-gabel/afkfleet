# 0010. fleet-core conventions and Phase 2 refinements

- **Status:** Accepted
- **Date:** 2026-10-06
- **Related:** Plan.md Phase 2 (P2.1–P2.11), §5, §6, §7.3, Appendices A, D, E; ADR-0002, ADR-0004, ADR-0008, ADR-0009

## Context
Phase 2 builds `fleet-core`: the pure domain core with no IO, no tokio and no azalea (ADR-0002). Before writing any code, the Phase 2 plan had to settle questions that run through every task:
- how time and randomness get in
- where error types come from
- which representation stored data uses, so it stays compatible

Plan.md, Appendix E and ADR-0008 also turned out to be ambiguous, or to contradict each other, in about two dozen places. Examples:
- the state machine can't reset its attempt counter, because `Online{since}` drops the attempt and `transition` gets no policy
- "auth invalid twice" sits both in the state machine (Appendix E, §6 row 5) and in azalea's `refresh()` (ADR-0008 §9)
- an Admin's implicit Manage on every account conflicts with "Admins can't touch the Owner or other Admins"

The crate facts below were checked on docs.rs for the pinned versions.

The user answered every open question in the Phase 2 plan on 2026-10-06. This ADR records the answers. Phase 2 merges in five group PRs (Plan.md, Phase 2 note). If a later group's review changes one of these decisions, that group's PR amends this ADR.

## Decision
### Conventions
**Module layout.**
- `fleet-core` has the modules `id`, `value`, `chat`, `disconnect`, `time`, `resilience`, `bot`, `mode`, `authz` and `mc`, plus a crate-private `text` module with the shared character rules.
- Submodule files are private. Each module root re-exports its public items, so every item has exactly one path, e.g. `fleet_core::bot::transition` and `fleet_core::authz::authorize`.

**Time.**
- **`now`:** every pure function takes `now: chrono::DateTime<Utc>`.
  - chrono is built without `clock`/`now`, so core never reads the clock.
  - Durations are `std::time::Duration`, and config values in `*_secs` are converted at the edge.
  - `time::add` and `time::elapsed` do all `DateTime ± Duration` arithmetic. They saturate instead of panicking, as chrono's `+` would. `elapsed` returns zero if the clock went backwards.
- **Liveness timestamps** (last tick, last packet) are `std::time::Instant`, which is monotonic.
  - They're written about 20 times a second, never stored or sent, and only compared against a timeout. A wall-clock step would trip every bot's watchdog at once.
  - Both stamps are set when the session is created, so they're never empty.
- **P4 requirement:** the runtime's clock derives `DateTime<Utc>` from tokio's `Instant`, anchored at startup.
  - Then paused-time tests drive `transition`, the circuit breaker and `ModePlan`.
  - NTP steps don't affect them.

**Randomness.**
- `rand` is built without default features, so there's no `sys_rng`, no `thread_rng` and no OS randomness in core.
- Functions take `rng: &mut R` with `R: rand::Rng + ?Sized`. In rand 0.10, `Rng` is the core trait and `RngExt` has `random_range`.
- Core types never store an RNG.
- IDs take 10 caller-supplied random bytes instead of an RNG. The server fills them from `getrandom`.
- **Tests:** a seeded `StdRng`. proptest (on rand 0.9) draws a `u64` seed.
- **Panic guard:** only non-empty ranges built from validated values are sampled, and probability is an integer percent. `random_range` panics on an empty range, and `random_bool` panics outside [0, 1].

**Errors.**
- **Per task:** each task builds its own error enum, because each task tests its error paths first. P2.11 audits them and adds the crate docs.
- **One `thiserror` enum per module.**
- **Variants carry context**, such as a step index, a limit, a length or the *position* of an invalid character. They never carry the untrusted input. For example, `IdError::Malformed` doesn't wrap `uuid::Error`, whose message echoes the input.
- **No `#[non_exhaustive]`:** the workspace's exhaustive `match`es should break when a variant is added.
- **Total functions:** `transition`, `classify` and the chat sanitizer return no errors.

**Persistence formats.**
- **JSON** is used only for `modes.definition_json` and the agent config.
  - IDs serialize through `try_from`/`into` `Uuid`. That's the wire format of `transparent`, but validated.
  - Value objects use `try_from = "String"`, `into = "String"`, so every deserialize re-runs the constructor.
  - Mode types are internally tagged (`"type"`), snake_case, with **struct variants only**: serde can't serialize `Variant(u8)` or `Variant(UnitEnum)` inside an internally tagged enum.
  - Durations are `*_ms` integers.
- **Compatibility:**
  - Unknown fields are ignored when reading stored data, so a rolled-back server can still read newer rows. API input stays strict through the DTOs' `deny_unknown_fields` (P6.9).
  - Changes are additive only, with `#[serde(default)]`. A rename or removal needs a DB migration that rewrites the stored JSON.
  - insta snapshots of the presets are the tripwire.
- **SQL columns** (`chat_messages.kind`, `users.role`, `account_grants.level`) use `as_str()` and `FromStr`. A round-trip test pins the strings.
- **No serde in Phase 2** for `IncomingChat`, `DisconnectReason`, `BotState`, the policies, `ModePlan`, `Liveness` or `SessionCredentials`. Their wire formats come with the proto and DTO conversions (P6, P10).

**Lint guard.** `crates/fleet-core/clippy.toml` repeats the root test allowances and adds `disallowed-methods` and `disallowed-types` for the clock and OS-randomness entry points.
- Missing features alone don't keep them out: once P4 and P6 turn on chrono `clock` and rand's std features, a workspace build unifies those features into `fleet-core` too.
- The lists cover every entry point of the pinned versions: `Instant::now`, `SystemTime::now`, chrono's `Utc::now` and `Local::now`, rand's free functions (`rng`, `random*`, `fill`, `make_rng`) and the `ThreadRng`/`SysRng` types, and uuid's `now_v1`/`now_v6`/`now_v7`, `new_v4`, `new_v7` and `Timestamp::now`. A probe built with rand's `thread_rng` and uuid's `v7`/`std` features confirmed that clippy flags them. (Amended after the group A review: the first list had only `rand::rng` from rand, and only `now_v7`/`new_v4` from uuid.)
- This makes the lints stricter, not weaker.

**Dependencies** (versions from Plan.md §5; ADR-0009 policy):
- **Normal:** `serde` (`derive`), `thiserror`, `uuid` (dfo, `serde`; never `v4`, `v7` or `rng`, which pull in getrandom), `chrono` (dfo, `serde`), `rand` (dfo), `backon` (dfo: only fastrand is left), `secrecy`.
- **Dev:** `rstest` (dfo), `proptest` (dfo + `std`, which keeps `PROPTEST_CASES`), `insta` (`json`), `serde_json`, `rand` (`std_rng`).
- **Dev-only features:** `rstest` and `proptest` are declared without default features at workspace level. That drops futures-timer, proc-macro-crate and rusty-fork.
- **DoD check:** "`cargo tree -p fleet-core` shows no tokio or IO crates" means normal edges: `cargo tree -p fleet-core -e normal`. Dev-only crates (insta's tempfile, proptest's rand 0.9 with getrandom) don't ship.
- proptest's `proptest-regressions/` files are committed as permanent regression seeds.

### Refinements of Plan.md, Appendix E and ADR-0008
**IDs (P2.1).**
- The constructor is `new_v7(created_at, random: [u8; 10])`. It goes through `uuid::Builder::from_unix_timestamp_millis` and rejects timestamps outside 0 ≤ ms < 2⁴⁸, which uuid would otherwise truncate silently.
- Parsing and deserializing accept only v7 UUIDs. Built-in mode rows get fixed v7 constants.

**Value objects (P2.2).**
- **`ChatMessage`:**
  - The 256 limit counts **UTF-16 code units**, because vanilla checks the Java string length; an emoji counts as 2.
  - Rejects control characters, `§` and bidi controls (U+202A–202E, U+2066–2069, U+200E/F, U+061C).
- **`Username`:** ASCII `[a-z0-9_.-]` after lowercasing, starting with a letter or digit.
- **`ServerAddress`:**
  - Remembers whether a port was given. `port()` still returns 25565 by default.
  - Rejects non-ASCII/IDN hosts, `_`, a trailing dot and userinfo.
  - The last label of a domain must start with a letter. No top-level domain starts with a digit, and resolvers that accept the old `inet_aton` forms would read `2130706433` or `0x7f000001` as an IP address. (Amended after the group A review.)
  - IPv6 needs brackets when a port follows.
  - Loopback and private addresses are allowed (dev uses localhost). An SSRF policy would be a server rule (P11), and it must check the resolved IP addresses, not the host string.

**Incoming chat (P2.3).**
- **Kinds:** `chat`, `emote`, `whisper`, `announcement`, `system`. Team chat and outgoing whispers map to `chat`.
- **Sender:** only player-type packets have one (ADR-0008 §7). It's `{ name: sanitized display text, ≤ 64 chars; uuid: optional }`, not a `McUsername`, because display names can carry prefixes and nicknames.
- **Text:**
  - Capped at 1024 chars, cut on a char boundary, with a `truncated` flag.
  - `\n` is kept. Other control characters, `§` together with the following char, bidi controls and invisible characters are stripped, from the text and the sender name alike.
  - "Invisible" means the format characters (Cf) that show no glyph, U+2028/U+2029, the Hangul fillers, the tags U+E0000–E007F and every variation selector except U+FE0F, which picks the emoji form of a symbol. The ranges are in `text.rs`. (Amended after the group A review: the first list had only U+200B–U+200D, U+2060, U+FEFF and U+00AD, so line separators, blank fillers and hidden tag text got through.)
  - The lists are hand-written; no new crate.

**Disconnect reasons (P2.4).** Beyond ADR-0008 §6, the classifier input also has:
- `AuthRejected` (the account hook, §9), which is classified as `AuthInvalid`
- `SessionCrashed` (`AppExit` error, §5), transient
- `WatchdogTimeout` (tick stall), transient
- `LivenessTimeout` (no packets), transient
- `ConnectFailed{HostUnavailable}` (the port's connect error), transient

**Resilience (P2.5).**
- **`RetryPolicy`:**
  - The factor is fixed at 2 (Appendix A has no factor key).
  - backon computes base, cap and jitter, seeded from the injected RNG through `with_jitter_seed`. backon adds its jitter (`d + d·U[0,1)`) *after* the cap, so it's given `max / 2`. The jittered delay then stays ≤ `max`, which requires `max ≥ 2 × base`.
  - `attempt` is clamped to 64 before iterating, because the plateau is always reached by then.
- **`FailureWindow`** ("N failures within a window") is a pure counter, shared by the circuit breaker and P4.7's crash-loop limit.
- **`CircuitBreaker`:**
  - One per bot, owned by the actor; bots share no state. It only decides how long to wait.
  - A failure is a connect failure, or a transient disconnect before the stable period. A success is a stable online period.
  - Appendix E's "circuit open too long → Failed" arrow is dropped. A flapping server stays transient.

**Bot state machine (P2.6), changes to Appendix E.**
- **Signature:** `transition(&BotState, BotEvent, now, &RetryPolicy) -> Transition`. Attempts are 1-based.
- **States:** `Online{since, attempt}`, `AwaitingSession{attempt, fresh}`, `Connecting{attempt, auth_retried}`, `Stopping{restart}`.
  - Without the attempt in `Online`, the stable-period reset of P2.6 can't be computed.
- **"Request one fresh token, then fail" lives in the state machine, not in azalea's `refresh()`:**
  - The first `AuthInvalid` before reaching Online goes to `AwaitingSession{fresh: true}` and emits `RequestSession{fresh: true}`, so the server bypasses its token cache. There's no backoff.
  - The second goes to `Failed(Auth)`.
  - fleet-mc's `refresh()` fails fast (ADR-0008 §9 points here).
  - A non-retryable `SessionUnavailable` goes to `Failed(SessionDenied)`.
- **New event `CrashLoop`:** from any state to `Failed(CrashLoop)`, so the supervisor's restart limit (§6 row 7) also goes through `transition()`. Packet liveness arrives as `Disconnected(LivenessTimeout)`.
- **Stop and restart:**
  - `Stop` goes straight to `Stopped` when there's no session.
  - From Connecting or Online, `Stop` goes to `Stopping{restart: false}` and emits `Disconnect`. `SessionClosed` then gives `Stopped`.
  - `Start` during `Stopping` sets `restart: true`, and `SessionClosed` then gives `AwaitingSession`. So restart (Appendix B) and a server change (P4.2) also go through `transition()`.
- **Other exits:**
  - Leaving Connecting or Online for any reason emits `Disconnect`, so the full teardown from ADR-0008 §10 always runs.
  - `SessionClosed` while Connecting or Online counts as a transient disconnect (`SessionCrashed`).
  - Events that don't fit the state are no-ops: no effects, no error.
- **Actor contracts:**
  - The actor holds the session credentials; events carry none.
  - Leaving a state cancels that state's timer or session request, so a stale `RetryDue` can't fire early.
  - The actor sends `SessionClosed` once teardown has finished or timed out. fleet-mc abandons a hung host thread, so `Stopping` always ends.

**Modes (P2.7, P2.8).**
- **`afk` preset:** `RotateRandom` ±30° yaw and ±10° pitch every 45–120 s, plus `SwingArm` every 20–40 s. 40 s is below 60 s, the shortest non-zero `player-idle-timeout`; rotation alone doesn't reset the idle timer (ADR-0008 §8).
- **`farm` preset:** `SelectHotbarSlot(0)` at start, plus `AttackFacingEntity` every 650–800 ms, with no Look: the server restores the account's saved rotation at join. Any other direction or slot is a custom mode.
- **Schedules:**
  - Each gap is uniform in [interval, interval + jitter].
  - The first run comes one gap after the start; there's no catch-up after a long gap.
  - `probability` is an integer percent from 1 to 100.
  - Interval and jitter are each ≤ 24 h.
  - An empty mode is valid (idle).
  - At most one `AtStart` chat step, because it runs on every join.
- **`ModePlan` returns resolved `PlannedAction`s:** `RotateRandom` becomes a relative `Turn`, and chat is kept apart so it goes through the P4.5 queue. `next_due` is `None` when only `AtStart` steps exist.
- **Mode JSON** uses struct variants (`{"type":"select_hotbar_slot","slot":3}`) instead of Plan.md's tuple notation.

**Authorization (P2.9).**
- **Admins:** an Admin's implicit Manage covers accounts owned by Members, plus the Admin's own. Accounts of the Owner and of other Admins need an explicit grant (§7.1 over §7.3).
- **Commands in modes:**
  - `CommandAllowlist` is a core type, because `authorize` is pure.
  - Changing a bot's mode needs Manage when the mode contains a command outside the allowlist; otherwise a mode could get around "any `/command` needs Manage".
  - P11 must re-run that check for every bot when a mode is edited.

**Minecraft ports (P2.10).**
- `connect` returns a `Result`, for immediate failures such as the abandoned-thread limit. Network failures still arrive as `ConnectionFailed` events.
- `SessionHandle` gains `respawn()`, because `Effect::Respawn` needs a port method.
- `perform` takes a `GameAction`; chat goes through `send_chat`.
- `disconnect()` means the full teardown from ADR-0008 §10, including abandoning a hung thread.
- `SessionCredentials` is `Offline{username}` or `Online{username, uuid, access_token, expires_at}`.

## Consequences
- **Easier:** every rule in `fleet-core` is deterministic and testable with a seeded RNG and fixed times. Stored data can't skip validation, and untrusted text can't reach logs through error messages.
- **The state machine has more fields than Appendix E** (`attempt` in `Online`, `fresh`, `auth_retried`, `restart`). In exchange, the runtime never decides lifecycle state itself.
- **Later phases inherit requirements:**
  - **P3.1 and P4.6** stamp liveness with `tokio::time::Instant::now().into_std()`.
  - **P4** maps tokio's `Instant` to `DateTime` for `now`.
  - **P11** re-checks mode commands on edit.
- **Flagged, not decided:**
  - **P4.1:** `BotSpec` sits in fleet-runtime, but fleet-proto and fleet-server need it too.
  - **P3.4/P5:** the connect timeout has no config key.
  - **P4.7/P10:** `Paused` and `Failed` must survive an actor or agent restart, so a fresh `Start` doesn't kick a human (§6 row 3).
- **Costs:**
  - The test build compiles rand 0.9 (proptest) next to 0.10; cargo-deny only warns.
  - A clippy config lives in `crates/fleet-core/` and repeats the root file's test allowances.

## Alternatives considered
- **`DateTime<Utc>` for liveness.** It would be one time type, but wall-clock steps would trip every bot's watchdog at once, and the adapter would need the runtime's clock.
- **Error types only in P2.11.** Tasks A–D couldn't test their error paths first.
- **serde on every domain type.** It adds compatibility surface that nothing stores yet. The proto and DTO conversions own those wire formats.
- **Our own jitter on top of an unjittered backon, or no backon in core.** Both are possible. Halving backon's cap keeps Plan.md's "wraps backon" without delays above the configured maximum.
- **The adapter refreshes the token** (ADR-0008 §9 as written). That keeps the retry policy out of the pure core, where it can't be tested exhaustively.
- **Admins with Manage on literally every account.** This contradicts §7.1 and lets an Admin take over the Owner's accounts.
