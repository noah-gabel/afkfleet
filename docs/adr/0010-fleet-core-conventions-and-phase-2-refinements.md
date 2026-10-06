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
  - Mode types are internally tagged (`"type"`), snake_case, with **unit or struct variants, never tuple or newtype variants**: serde can't serialize `Variant(u8)` or `Variant(UnitEnum)` inside an internally tagged enum. A unit variant reads and writes as `{"type":"jump"}`. (Clarified in group C: the first wording said "struct variants only".)
  - Durations are `*_ms` integers.
- **Compatibility:**
  - Unknown fields are ignored when reading stored data, so a rolled-back server can still read newer rows. API input stays strict through the DTOs' `deny_unknown_fields` (P6.9).
  - Changes are additive only, with `#[serde(default)]`. A rename or removal needs a DB migration that rewrites the stored JSON.
  - **New tags fail closed** *(group C)*. A new action or schedule type is a tag an older server doesn't know, so a stored mode that uses one fails to load there instead of running in part. The rollback promise above covers new fields, not new tags.
  - insta snapshots of the presets, and of a draft with every tag and field name, are the tripwire.
- **SQL columns** (`chat_messages.kind`, `users.role`, `account_grants.level`) use `as_str()` and `FromStr`. A round-trip test pins the strings.
- **No serde in Phase 2** for `IncomingChat`, `DisconnectReason`, `BotState`, the policies, `ModePlan` (with `PlanTick`, `PlannedAction` and `GameAction`, *group C*), `Liveness` or `SessionCredentials`. Their wire formats come with the proto and DTO conversions (P6, P10).

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
- Parsing and deserializing accept only v7 UUIDs. Built-in mode rows get fixed v7 constants, which P11.1's migration seeds *(group C)*.

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

**Bot state machine (P2.6), changes to Appendix E.** Points marked *(group B)* were settled while building P2.6. The user decided sticky Paused and Failed, the breaker effects and the plain-text duplicate login on 2026-10-06.
- **Signature:** `transition(&BotState, BotEvent, now, &RetryPolicy) -> Transition`. Attempts are 1-based.
- **States:** `Online{since, attempt}`, `AwaitingSession{attempt, fresh}`, `Connecting{attempt, auth_retried}`, `Stopping{restart}`.
  - Without the attempt in `Online`, the stable-period reset of P2.6 can't be computed.
- **Attempt counting** *(group B)*:
  - `AwaitingSession`, `Connecting` and `Online` carry the number of the current attempt.
  - `Backoff{n}` and `ScheduleRetry{n}` name the attempt that failed. The actor waits `RetryPolicy::delay(n)`, so the first retry waits 5–10 s (§6). `RetryDue` asks for attempt n + 1.
  - A session that ends after the stable period starts the counter over: `Backoff{1}`.
- **"Request one fresh token, then fail" lives in the state machine, not in azalea's `refresh()`:**
  - The first `AuthInvalid` before reaching Online goes to `AwaitingSession{fresh: true}` and emits `RequestSession{fresh: true}`, so the server bypasses its token cache. There's no backoff.
  - The second goes to `Failed(Auth)`.
  - "Twice" means twice in a row *(group B)*. `fresh` doesn't survive a Backoff, and `auth_retried` starts over after Online or a Backoff; the backoff bounds the rest.
  - `AuthInvalid` while Online counts as transient *(group B)*. The session was accepted at join, and a retry without a backoff would let a server drive reconnects.
  - `Reset` from `Failed(Auth)` asks for a normal session; the account has been signed in again by then *(group B)*.
  - fleet-mc's `refresh()` fails fast (ADR-0008 §9 points here).
  - A non-retryable `SessionUnavailable` goes to `Failed(SessionDenied)`.
- **New event `CrashLoop`:** from any state to `Failed(CrashLoop)`, so the supervisor's restart limit (§6 row 7) also goes through `transition()`. Packet liveness arrives as `Disconnected(LivenessTimeout)`. `CrashLoop` overwrites an earlier failure reason and drops a pending restart *(group B)*.
- **Session ends** *(group B)*: `ConnectFailed(f)`, `Disconnected(reason)`, `WatchdogTimeout` (the same as `Disconnected(WatchdogTimeout)`) and `SessionClosed` are handled alike in Connecting and Online, and are no-ops in every other state.
- **Stop and restart:**
  - `Stop` goes straight to `Stopped` when there's no session, except in Paused and Failed (below).
  - From Connecting or Online, `Stop` goes to `Stopping{restart: false}` and emits `Disconnect`. `SessionClosed` then gives `Stopped`.
  - `Start` during `Stopping` sets `restart: true`, and `SessionClosed` then gives `AwaitingSession`. So restart (Appendix B) and a server change (P4.2) also go through `transition()`.
- **Paused and Failed are sticky** *(group B)*:
  - `Start` and `Stop` are no-ops there. Only `Resume` (Paused), `Reset` (Failed) and `CrashLoop` leave them.
  - So Stop-then-Start can't kick a human (§6 row 3) or retry a failed bot without Reset (§6 row 2). Restart and server changes wait for Resume or Reset.
  - The first P2.6 invariant therefore excludes Paused and Failed.
- **Circuit-breaker effects** *(group B)*. `ScheduleRetry{attempt}` stays as in Appendix E. Three new effects tell the actor's breaker what happened, so the rule is tested in core:
  - `RecordFailure`: a session failed before the stable period (connecting failed, or a transient end while Online). It always comes right before `ScheduleRetry`.
  - `RecordSuccess`: the bot leaves Online after the stable period, whatever the exit.
  - `ResetBreaker`: `Start`, `Reset` or `Resume` (re)starts the bot, so a run started on purpose doesn't inherit an old cool-down. Automatic retries still go through the breaker.
  - A retry after `SessionUnavailable` doesn't involve the breaker.
- **`Notify`** is only for alerts that need a human: entering Paused or Failed *(group B)*.
- **Effect order** *(group B)*: `StopMode`, `Disconnect`, then `RecordSuccess` or `RecordFailure`, then `ScheduleRetry`, `RequestSession` or `Notify`. A deliberate start emits `ResetBreaker` before `RequestSession`.
- **Other exits:**
  - Leaving Connecting or Online for any reason emits `Disconnect`, so the full teardown from ADR-0008 §10 always runs.
  - `SessionClosed` while Connecting or Online counts as a transient disconnect (`SessionCrashed`).
  - Events that don't fit the state are no-ops: no effects, no error.
- **Actor contracts:**
  - The actor holds the session credentials; events carry none.
  - Leaving a state cancels that state's timer or session request, so a stale `RetryDue` can't fire early.
  - The actor sends `SessionClosed` once teardown has finished or timed out. fleet-mc abandons a hung host thread, so `Stopping` always ends.
  - Only the current session's events reach `transition()`, at most one terminal event per session *(group B)*. Once `Disconnect` has run, the actor drops that session's events, and it reports `SessionClosed` for it only if no newer session has connected since.
  - Every `RequestSession` gets exactly one answer; a timeout becomes `SessionUnavailable{retryable: true}`. Connecting ends through fleet-mc's connect timeout *(group B)*.
  - The actor publishes every state change, plus `Died`, as events for the app (P4.7, P11). `Notify` is only for the alerts above *(group B)*.

**Modes (P2.7, P2.8).** Points marked *(group C)* were settled while building P2.7 and P2.8. The user decided the limited steps, the angle ranges, the first error, new tags failing closed and the preset scope on 2026-10-06.
- **`afk` preset:** `RotateRandom` ±30° yaw and ±10° pitch every 45–120 s, plus `SwingArm` every 20–40 s. 40 s is below 60 s, the shortest non-zero `player-idle-timeout`; rotation alone doesn't reset the idle timer (ADR-0008 §8).
- **`farm` preset:** `SelectHotbarSlot(0)` at start, plus `AttackFacingEntity` every 650–800 ms, with no Look: the server restores the account's saved rotation at join. Any other direction or slot is a custom mode.
- **Presets are definitions only** *(group C)*: `ModeDefinition::afk()` and `farm()`. The agent config's names (`mode = "afk"`) come with P5.1, the fixed IDs with P11.1.
- **Schedules:**
  - Each gap is uniform in [interval, interval + jitter].
  - The first run comes one gap after the start; there's no catch-up after a long gap.
  - `probability` is an integer percent from 1 to 100.
  - Interval and jitter are each ≤ 24 h.
  - An empty mode is valid (idle).
  - At most one `AtStart` chat step, because it runs on every join.
- **Limited steps** *(group C)*: at most one `AtStart` and one `Every` step each for `AttackFacingEntity` and `SendChat`. The intervals are per step, so two repeating attacks at 500 ms would attack every 250 ms, and 32 chat steps at 30 s would send about one message a second; several at-start attacks would fire together on every join.
- **Angles** *(group C)* are degrees. `Look`: yaw −180 to 180 (as the debug screen shows it), pitch −90 to 90. `RotateRandom`: `max_yaw` 0 to 180, `max_pitch` 0 to 90, and not both 0, since that never turns. NaN, ±∞ and negative maxima are rejected; a negative maximum would also make the sampled range empty.
- **Validation** *(group C)*:
  - `ModeDraft::validate` returns the first `ModeError` in step order. `TooManySteps` is checked before any step; within a step the action comes first, then the probability, the schedule and the limited kind.
  - `step` is the index into `steps`, and `AngleOutOfRange` names its field, so P11.4 can show field errors.
  - The minimum interval and the limited kinds come from exhaustive `match`es over `Action`, and so does `commands()`: a new action needs a decision about each.
  - Durations count in whole milliseconds, as they're stored: `validate` drops anything finer before checking, so a definition round-trips through its JSON exactly.
- **`HotbarSlot`** *(group C)* is a newtype for 0..=8, because azalea panics above 8 (ADR-0008 §8), and the port's actions carry it too. An invalid slot fails while deserializing, like an invalid `ChatMessage`.
- **`ModePlan` returns resolved `PlannedAction`s:** `RotateRandom` becomes a relative `Turn`, and chat is kept apart so it goes through the P4.5 queue. `next_due` is `None` when only `AtStart` steps exist. After applying a `Turn`, the adapter (P3.6) clamps the pitch to [-90, 90].
  - **API** *(group C)*: `ModePlan::start(&definition, now, rng) -> (ModePlan, PlanTick)` runs the at-start steps and schedules the rest; `tick(now, rng) -> PlanTick` runs every due step in step order. `PlannedAction` is `Game(GameAction)` or `Chat(ChatMessage)`, and `GameAction` lives in `mode`, as the user decided.
  - **Rescheduling from `now`** *(group C)*: a step that ran is due again one gap after `now`, not after its old due time. That is what "no catch-up" means, and it keeps two runs of a step at least an interval apart even when the runtime wakes late, so the attack and chat limits hold. A skipped roll reschedules too.
  - A tick before the next due time, including one with the clock gone backwards, runs nothing *(group C)*.
- **Mode JSON** uses struct variants (`{"type":"select_hotbar_slot","slot":3}`) instead of Plan.md's tuple notation. A mode is `{"steps":[{"action":{…},"schedule":{"type":"every","interval_ms":…,"jitter_ms":…},"probability":…}]}`; every field is required *(group C)*.

**Authorization (P2.9).** Points marked *(group D)* were settled while building P2.9. On 2026-10-06 the user decided the mode rules, grants, roles, the allowlist format, invites, self-service routes and agent enrollment.
- **Admins:** an Admin's implicit Manage covers accounts owned by Members, plus the Admin's own. Accounts of the Owner and of other Admins need an explicit grant (§7.1 over §7.3).
- **Commands in modes:**
  - `CommandAllowlist` is a core type, because `authorize` is pure.
  - Changing a bot's mode needs Manage when the mode contains a command outside the allowlist; otherwise a mode could get around "any `/command` needs Manage".
  - P11 must re-run that check for every bot when a mode is edited.
- **API** *(group D)*: `authorize(&Actor, Permission, &ResourceContext) -> Result<(), AuthzError>`.
  - **`ResourceContext`** is one of:
    - `Global`
    - `Account{owner, grant}`: `grant` is the actor's own grant, and a bot is authorized through its account
    - `Mode{owner, visibility}`
    - `User(UserRef)`
    - `Invite{role}`
    - `Personal{owner}`

    The caller loads these facts, including the owner's current role.
  - **`AuthzError`** is one of:
    - `NotFound`: the actor can't even see the resource, so the API answers 404
    - `Forbidden`: 403
    - `WrongResource`: a permission checked against the wrong kind of resource, which is a caller bug
  - **Structure.** Each kind of context has its own function, and it matches every `Permission` without a wildcard, so a new permission needs a decision for each. The order is always: does the permission apply, may the actor see the resource, may they do this.
  - **`Permission`** is `Copy`. `SendChat` and `SetBotMode` carry a `CommandCheck`. Only `CommandAllowlist::check` and `check_mode` create one, so a caller can't claim "allowlisted" without an allowlist, and no chat text reaches `Debug` output.
  - **Names.** `Role`, `GrantLevel` and `ModeVisibility` map to `users.role`, `account_grants.level` and `modes.visibility` through `as_str` and `FromStr`.
- **Accounts** *(group D)*:
  - The effective level is the higher of the explicit grant and the implicit Manage: the owner (decided by ID), the Owner, or an Admin on a Member's account.
  - Only Manage edits grants, so "nobody grants above their own level" always holds; a test pins it.
  - Nobody grants to themselves. Otherwise an Admin could turn implicit Manage into a grant that outlives the Member's promotion to Admin.
  - The server address and auto-start need Manage, because Appendix B's `PATCH /bots/{id}` needs Manage except for the mode.
- **Allowlist** *(group D)*:
  - Entries are written the way they're typed in chat (`"/spawn"`). Each must be a valid `ChatMessage`: `/` plus a non-empty command name without arguments. At most 64 entries; a missing key means an empty list.
  - Errors carry the entry's index, never its text.
  - An allowlisted command passes with any arguments.
- **Modes** *(group D)*:
  - Built-in modes are read-only for everyone, the Owner included.
  - Private modes follow the account rule: their owner, the Owner, and Admins for Members' modes. Everyone else gets 404.
  - Shared modes are visible to everyone, and edited by their creator and the Owner. Shared modes need Admin+ (Appendix B), so a creator demoted to Member can no longer edit theirs.
  - Changing a mode needs the rights both before and after a visibility change, so an Admin can't share a Member's private mode.
- **Users and invites** *(group D)*:
  - Only the Owner changes roles, between Member and Admin. Admins disable and enable Members. Nobody acts on themselves or on an Owner, and nobody becomes the Owner this way.
  - Ownership transfer waits for a route.
  - Admins list all invites. Creating and revoking an invite follow the same rule: Member invites need Admin+, Admin invites the Owner, and Owner invites are never allowed.
  - Members get 404 on other users and on invites.
- **Agents** *(group D)*: only the Owner enrolls agents; viewing and disabling them stays Admin+. This deviates from §7.3. An agent receives session tokens for the bots assigned to it, and P10.5 assigns bots to any agent with free capacity, so an Admin's own agent could otherwise get the Owner's tokens.
- **Personal resources** *(group D)*: profile, sessions, 2FA, link flows and event tickets belong to one user. Everyone else gets 404, the Owner included. So every authed handler calls `authorize()`, the self-service routes too.
- **The Owner's limits** *(group D)*: §7.3's "can do everything" leaves out:
  - editing built-in modes
  - changing their own role or disabling themselves
  - acting on another Owner
  - Owner invites
  - granting to themselves
  - other users' personal resources
- **Not in `authorize`** *(group D)*: step-up (§7.2) is an authentication check in P7, and the account quota belongs to P9.4.
- **Matrix** *(group D)*:
  - The snapshot `authorization_matrix` has one table per kind of resource. Accounts are shown by actor, owner and grant; everything else by actor and target. The cells are `ok`, `403` or `404`.
  - An exhaustive match in the test makes a new permission fail until the matrix lists it.
  - A separate test checks every permission against every other kind of resource.

**Minecraft ports (P2.10).**
- `connect` returns a `Result`, for immediate failures such as the abandoned-thread limit. Network failures still arrive as `ConnectionFailed` events.
- `SessionHandle` gains `respawn()`, because `Effect::Respawn` needs a port method.
- `perform` takes a `GameAction`; chat goes through `send_chat`. `GameAction` is `fleet_core::mode::GameAction`, defined in P2.8, and its hotbar slot is a `HotbarSlot` *(group C)*.
- `disconnect()` means the full teardown from ADR-0008 §10, including abandoning a hung thread.
- `SessionCredentials` is `Offline{username}` or `Online{username, uuid, access_token, expires_at}`.

Points marked *(group E)* were settled while building P2.10. On 2026-10-06 the user decided the errors, the event delivery, liveness as data and the `BotId` in `ConnectParams`.
- **Traits** *(group E)*:
  - `MinecraftConnector` has the associated types `Session: SessionHandle` and `Events: SessionEvents`.
  - The connector and the handle are `Send + Sync + 'static`, the events `Send + 'static`.
  - Every method returns `impl Future<…> + Send` (RPITIT). A doctest in `mc` drives the ports from generic code and checks that the resulting future is `Send`.
- **`ConnectParams`** *(group E)* is `{bot_id, server, credentials, connect_timeout}`. The `BotId` lets fleet-mc name host threads after their bot and tag its logs.
- **`connect`** *(group E)* resolves once the session has started, before the bot reaches the server. The outcome arrives as `Joined`, `ConnectionFailed` or `Disconnected`, and a connect timeout as `ConnectionFailed(TimedOut)`.
- **Errors** *(group E)*:
  - `ConnectError` is `HostUnavailable`. The actor reports it as `ConnectFailure::HostUnavailable`.
  - `SessionError` is `Closed` (the session ended or is being torn down), `QueueFull` (the host thread's bounded queue is full: overload is an error, not a wait), `TimedOut` (the host thread didn't answer; the watchdog decides whether it hangs) or `NotInWorld` (azalea's getters panic before login and after `exit()`, so the adapter checks first).
- **Event delivery** *(group E)*: the bridge is bounded (ADR-0008 §4), but only `Chat` may be dropped when the consumer lags, and the adapter counts those drops. `Joined`, `Died`, `Disconnected` and `ConnectionFailed` are always delivered. A dropped `Died` would leave the bot dead, and a dropped `Disconnected` would leave the actor waiting.
- **`SessionEvents::next`** *(group E)* is cancel-safe, because the actor calls it in `select!`. `Died` comes once per death. A session ends with at most one terminal event, `Disconnected` or `ConnectionFailed`, after which `next` returns `None`.
- **`disconnect()`** *(group E)* returns `()`. The teardown always finishes, and calling it again, from any clone or after the session ended on its own, is harmless.
- **Liveness** *(group E)* is data only: `Liveness { last_tick, last_packet }`, with public fields and no constructor, read synchronously through `liveness()`.
  - The comparison against the timeouts is P4.6's. When both stamps are stale, the tick stall wins (`WatchdogTimeout`), because a hung host thread stops both.
  - std can't make an `Instant` without `Instant::now()`, which `fleet-core`'s clippy config bans, so core doesn't test with one. The fakes build them (P3.1).
- **Non-finite angles** *(group E, the user's decision)*: the adapter skips any `GameAction` with a non-finite angle and logs it, as a safety net behind mode validation (P3.6).

## Consequences
- **Easier:** every rule in `fleet-core` is deterministic and testable with a seeded RNG and fixed times. Stored data can't skip validation, and untrusted text can't reach logs through error messages.
- **The state machine has more fields and effects than Appendix E**: `attempt` in `Online`, `fresh`, `auth_retried` and `restart`, plus the three breaker effects. In exchange, the runtime never decides lifecycle state or breaker outcomes itself.
- **Later phases inherit requirements:**
  - **P3.1 and P4.6** stamp liveness with `tokio::time::Instant::now().into_std()`.
  - **P4** maps tokio's `Instant` to `DateTime` for `now`.
  - **P4.1** adds per-server conflict texts (below).
  - **P4.2** executes the breaker effects and follows the group B actor contracts. A spec change to a Paused or Failed bot takes effect at Resume or Reset.
  - **P11** re-checks mode commands on edit. For Paused or Failed bots the app shows Resume or Reset instead of Start and Stop, because the state machine ignores Start and Stop there.
  - **P2.10** takes `mode::GameAction` in `perform`. **P5.1** maps the config's mode names to the presets, **P11.1** seeds the built-in mode rows with fixed IDs, and **P11.4** turns each `ModeError` into a field error at `steps[step]` *(group C)*.
  - **From group D** (each has a note in Plan.md):
    - **P7.1** adds a single-Owner index.
    - **P7.8** maps `AuthzError` to 404, 403 or 500, and checks the self-service routes through `Personal`.
    - **P7.9** has the Owner change roles and Admins disable Members.
    - **P9.4 and P9.6** check link flows through `Personal` and grants with their grantee.
    - **P10.3** has only the Owner enroll agents.
    - **P11.2** checks `ViewMode` and `SetBotMode` when a bot switches modes.
    - **P11.4** follows the mode rules.
    - **P11.9** shows the grants on a promoted Member's accounts.
  - **From group E** (each has a note in Plan.md):
    - **P3.4** names host threads after the `BotId` in `ConnectParams`.
    - **P3.5** drops only `Chat` events, and delivers every lifecycle event.
    - **P3.6** skips and logs actions with non-finite angles.
    - **P4.6** compares the `Liveness` stamps, and the tick stall wins when both are stale.
- **Decided in group B: plain-text duplicate login** (flagged in the group A review).
  - **The problem:** a duplicate login reported as plain text, with no translation key, classifies as transient (ADR-0008 §6). A proxy or plugin may kick the bot that way when a human logs in; BungeeCord/Waterfall in online mode probably does. The bot then reconnects and kicks the human, and the circuit breaker only limits how often.
  - **The decision:** no state-machine change. A bot's spec gets an optional list of kick texts that count as a duplicate login, empty by default (P4.1). The classifier compares the sanitized kick message exactly against that list, so vanilla servers still go by key.
- **Flagged, not decided:**
  - **P4.1:** `BotSpec` sits in fleet-runtime, but fleet-proto and fleet-server need it too.
  - **P3.4/P5:** the connect timeout has no config key.
  - **P4.7/P10:** `Paused` and `Failed` must survive an actor or agent restart, so a fresh `Start` doesn't kick a human (§6 row 3).
  - **P9.6/P9.8** *(group D)*: how a Member picks a grantee. Members can't list users, and looking users up by name must not let them enumerate usernames.
  - **P7.13** *(group D)*: the audit log is Admin+, so Admins see the Owner's and other Admins' activity, including their IP addresses and the chat their bots sent.
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
- **Stop and Start leaving Paused and Failed** (group B). It was the first reading of "Stop from any state ends in Stopped". But then Stop-then-Start, or the restart endpoint, kicks a human or retries a failed bot without Reset.
- **The breaker outcome as a cause on `ScheduleRetry`** (group B). A stable session that ends without a retry (Stop, a duplicate login, a permanent kick, `CrashLoop`) would never report its success. A stale cool-down would then survive into the next run, and one failed connect after Resume would wait 15 min instead of 5–10 s.
- **Pausing on every keyless kick while Online** (group B). Paper and Spigot send their configurable shutdown and restart kicks as plain text, so every server restart would pause every bot until someone resumed it.
- **Documenting proxies as unsupported for conflict detection** (group B). It's cheaper, but it leaves the bot fighting a human behind a BungeeCord-style proxy.
- **Per-step limits only, or a combined average rate per kind** (group C). Per step leaves attacks without any cap. An average rate (Σ 1/interval ≤ 1/min) is more flexible, but two steps can still fire at the same moment.
- **An `Unknown` catch-all action** (group C). An older server could then run the rest of a newer mode, but a rolled-back bot would silently run only part of it.
- **All errors at once** (group C). P11.4 could show every field error together, but it's more API and test surface, and the app's schema catches most mistakes before they reach the server.
- **Rescheduling from the old due time** (group C). It keeps a steadier rhythm, but after a late tick the next run could come sooner than the interval, so the chat and attack limits wouldn't hold.
- **A preset enum with names and fixed IDs** (group C). The names would become a config format, and the IDs would need an unchecked constructor in `id`, before anything uses them.
- **`Permission<'a>` with references to the message and the allowlist** (group D). `authorize` would run the check itself. But `Permission` would carry a lifetime, and its `Debug` output would contain chat text. Either way, the caller has to send the message it checked.
- **Any Admin edits any shared mode** (group D). That's simpler, but an Admin could change another Admin's or the Owner's modes. The user chose creator and Owner.
- **Re-sharing below Manage** (group D). "Never above your own level" would then matter: Control holders could share up to Control. That's more surface, and it contradicts §7.3's "edit grants: Manage".
- **Admins enroll agents, as §7.3 says** (group D). An Admin's agent could receive the Owner's session tokens unless P10 restricted the scheduler. Owner-only enrollment closes that with one rule.
- **Self-service routes exempt from `authorize()`** (group D). One permission less, but it breaks "every authed handler calls `authorize()`", and each handler would scope its own queries.
- **A `TransferOwnership` permission now** (group D). It has no route or task yet. It comes with one.
- **A pure stall check in core** (group E): `Liveness::stall(now, timeouts)`. It would pin the precedence rule in core, but core can't make an `Instant` in tests without an exception to its clippy guard, and "traits only" is P2.10's scope.
- **Dropping any event under load** (group E), as ADR-0008 §4 first read. The actor would then depend on timeouts to notice a lost `Died` or `Disconnected`.
- **One opaque session error** (group E). It's simpler, but P4.4's logs couldn't tell overload from a dead session.
- **`ConnectParams` without the `BotId`** (group E). Spans can carry the bot's ID, but a hung host thread that's abandoned is easier to find by name.
