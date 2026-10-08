# 0013. fleet-runtime conventions and Phase 4 refinements

- **Status:** Accepted
- **Date:** 2026-10-08
- **Related:** Plan.md Phase 4 (P4.1–P4.9), §4, §5, §6, §7.4, §8, Appendices A, C and D; ADR-0008, ADR-0010, ADR-0011

## Context
Phase 4 builds `fleet-runtime`: one supervised actor per bot that drives `fleet_core::bot::transition()`, with a mode runner, a chat queue, a watchdog, a supervisor, a chaos test and metrics. All of it is proven with fleet-testkit's fakes on paused time.

Before writing any code, the Phase 4 plan listed every open question, including the ones Phase 2 and Phase 3 had flagged for P4. It also found four places where the plan conflicted with the code as built:
- **The classifier can't see the conflict texts.** `DisconnectReason::classify(&self)` and `transition(.., &RetryPolicy)` take no extra input, and fleet-mc's event mapping, slow tests and manual test call `classify()`.
- **The credentials port.** If it lived in fleet-runtime, fleet-testkit (which may depend only on fleet-core) couldn't provide its fake.
- **fleet-mc's diagnostics.** fleet-runtime may depend only on fleet-core (§4), so it can't read them.
- **The snapshot's `uptime`.** A stored uptime goes stale in a `watch` value, and `attempt` already sits inside `BotState`.

The user answered every question on 2026-10-08, and this ADR records the answers. Phase 4 merges in five group PRs (below). If a later group's review changes a decision, that group's PR amends this ADR.

## Decision
### Grouping
One branch and one PR per group, one commit per task, in this order:

| Group | Branch | Tasks |
|---|---|---|
| A | `p4/p4.1-p4.3-spec-and-credentials` | P4.1, P4.3 |
| B | `p4/p4.4-p4.5-chat-queue-and-mode-runner` | P4.5, then P4.4. Mode chat goes through the queue. The runtime clock and the governor clock land in P4.5, their first user |
| C | `p4/p4.2-p4.6-bot-actor-and-watchdog` | P4.2, P4.6 |
| D | `p4/p4.7-supervisor-and-fleet` | P4.7 |
| E | `p4/p4.8-p4.9-metrics-chaos-and-wrap-up` | P4.9, P4.8 and the phase wrap-up |

The queue and the mode runner are built against the fake before the actor, so the actor PR wires real pieces instead of placeholders that span PRs.

### Spec, snapshot and conflict texts (P4.1)
- **Where they live.** `BotSpec` and `BotSnapshot` are pure data in `fleet_core::bot`, without serde. fleet-proto converts them, the server builds a validated spec from its row, and the runtime uses the same type.
- **`BotSpec`:** `{ id, account: BotAccount, server, mode: ModeDefinition, desired: DesiredRunState, conflict_texts: ConflictTexts }`.
  - `BotAccount` is `Offline(McUsername)` or `Online(AccountId)`. The offline credentials provider serves `Offline`; the managed one asks the server for `Online`.
  - **A bot's account never changes.** The runtime refuses a spec with a different account for a bot it already runs (`AccountChanged`); the server removes the bot and assigns a new one.
  - The mode is the resolved, validated `ModeDefinition`, so the agent never looks a mode up.
- **`BotSnapshot`:** `{ bot_id, state, since, last_disconnect }`. `attempt()` and `uptime(now)` are derived from the state when they're read, so nothing goes stale between state changes. `last_disconnect` also records failed connects and watchdog trips.
- **`BotRules`.** `transition(&BotState, BotEvent, now, &BotRules)`, with `BotRules { retry: RetryPolicy, conflict_texts: ConflictTexts }`. The retry policy comes from the runtime's config, the texts from the bot's spec.
- **`DisconnectReason::classify(&self)` stays unchanged.** fleet-mc's event mapping and tests call it. `ConflictTexts::classify(&self, &DisconnectReason)` applies the texts on top of it: a kick that `classify()` calls Transient, whatever its key (none, an unknown one, or a known transient one such as `multiplayer.disconnect.server_full`), whose sanitized message is on the list, becomes `Conflict{DuplicateLogin}`. A key that means Permanent, Conflict or AuthInvalid always wins. This applies in Connecting and Online alike, which covers a proxy's "already connected" kick.
- **`ConflictTexts`:**
  - At most 16 entries, each 1–1024 characters, which is the kick-message cap. Matching is exact against the whole message, so a shorter cap could make a long custom proxy message impossible to configure.
  - Matching is exact and case-sensitive. Entries are taken as written, without trimming. Duplicates count once.
  - An entry with a character the kick sanitizer strips (`§`, a control character other than `\n`, a bidi control or an invisible character) is rejected, since it could never match. The error names the entry's index and the character's position, never the text.
- **Config.** The standalone agent takes `conflict_texts = […]` per `[[standalone.bots]]` entry; a missing key means an empty list (P5.1). In managed mode they're stored per server, set once in the app for all bots on that server (P11).
- **The chat format** for plugin chat in system messages is display-only. It's deferred to the server side (P11.9) and stays out of `BotSpec`.

### Credentials (P4.3)
- **The port lives in `fleet_core::mc`**, next to `SessionCredentials`: `session(SessionRequest { bot_id, account, fresh })` returns `SessionCredentials` or `CredentialError { retryable }`. Its fake is in fleet-testkit, `OfflineCredentials` in fleet-runtime, and the managed implementation in fleet-agent (P10).
- **The session-request timeout** is 30 s, a `RuntimeConfig` default. The runtime doesn't check `expires_at`; the managed provider doesn't hand out a token that expires within the connect timeout (P10).

### Chat queue (P4.5)
- 16 messages, with one governor bucket per bot: one message every 3 s, burst 3 (§7.4's per-bot value). The bucket is checked when a message is queued. A full bucket returns `RateLimited`, a full queue `QueueFull`; it never waits.
- User chat while the bot isn't Online returns `NotOnline`; nothing is queued for a later session.
- `send_chat` returns a ticket once the message is queued. The outcome arrives later as `ChatSent` or `ChatFailed`, which matches P10's `SendChat` with a request id.
- `ChatUnavailable` drops the message. User chat gets `ChatFailed{ChatUnavailable}`; mode chat is logged at `debug` (fleet-mc already warns once per session) and skipped. No retry.
- On a disconnect or teardown, queued messages fail with `ChatFailed{Disconnected}`.
- A mode chat step that's rate-limited or finds the queue full is skipped and logged, and runs again at its next scheduled time.
- A token isn't refunded when a send fails.
- **Commands with message arguments** (`/me`, `/msg`, …) on servers that enforce secure chat get nothing special in Phase 4: they go out and the server rejects them. P11.3's rule for user commands applies to mode chat too, through `ChatUnavailable`, because only fleet-mc knows whether a session signs.

### Mode runner (P4.4)
- A failed action is skipped. The first failure of each `SessionError` kind per session logs at `warn`, later ones at `debug`.
- **A mode change** lets go first (`HoldUse{on:false}`, `Sneak{on:false}`), then starts the new plan with its at-start steps. The selected slot stays, and an update with an equal definition changes nothing.

### Actor (P4.2)
- **Events.** `FleetEvent { bot_id, at, kind }`, where `kind` is `StateChanged(BotSnapshot)`, `Died`, `ChatReceived(IncomingChat)`, `ChatSent{ticket}` or `ChatFailed{ticket, reason}`. There's one bounded `broadcast` per fleet, and a lagging subscriber resyncs (§6 row 12).
- **Respawn.** A failed `respawn()` call is retried every 5 s while the session lives; the first failure logs at `warn`, the retries at `debug`. After 12 failed calls in a row the session ends with the new `DisconnectReason::RespawnFailed` (Transient), so the bot reconnects and respawns on join. An `Ok` ends the retrying, even if the server ignored it, since the ports can't tell.
- **Holds after a respawn** aren't re-applied; that's a known limit. A repeating `HoldUse{on: true}` step does re-apply a hold after a death, an at-start one doesn't. After a respawn none of the at-start setup runs again (look, slot, sneak, hold); P11 decides whether modes get "on respawn" steps, which needs a `SessionEvent::Respawned` port event.

### Watchdog (P4.6)
It checks every 1 s while Online. A tick stall gives `WatchdogTimeout`, a packet stall `Disconnected(LivenessTimeout)`, and the tick stall wins when both are stale (ADR-0010).

### Supervisor and `Fleet` (P4.7)
- **Inside.** Actors run in tokio's `JoinSet`, which reports `JoinError::is_panic()`, each with a child `CancellationToken`. The `Fleet` handle talks to the supervisor task over a bounded queue with a reply timeout, and the supervisor forwards to the actor inboxes with `try_send`. That's how "the Fleet API always responds" holds.
- **API:** `apply(spec, restore: Option<StickyState>)`, `remove`, `reset`, `resume`, `restart`, `send_chat`, `snapshot`, `snapshot_all`, `subscribe` and `shutdown(timeout)`.
  - Bots start and stop only through the spec's desired state. `restart` is a no-op when the desired state is Stopped.
  - `apply` refuses a new bot above `[runtime] max_bots` with `AtCapacity`.
  - The errors are `UnknownBot`, `Busy`, `AtCapacity`, `AccountChanged`, `ShuttingDown` and `TimedOut`.
  - `remove` returns once the supervisor accepted it. The teardown runs in the background, then `StateChanged(Stopped)` and `Removed` go out. Until `Removed`, `apply` for that `BotId` returns `Busy`.
- **Restarts never skip the backoff.**
  - The supervisor keeps each bot's spec, snapshot `watch`, restart window and circuit breaker. The actor writes a copy of its breaker into a private `watch` after each breaker effect, so a panic doesn't reset it.
  - A pure `fleet_core::bot::restore(&BotState, now, &BotRules) -> Transition` decides the restored state and its effects:
    - AwaitingSession, Connecting, Online and Backoff come back as `Backoff{attempt}` with `ScheduleRetry`; an Online bot that had lasted the stable period comes back as `Backoff{1}`
    - Paused, Failed and Stopped stay
    - Stopping becomes Stopped

    Then the spec's desired state applies.
  - A panic isn't a breaker failure; only the restart window counts it.
  - The 6th panic within 10 min gives `CrashLoop`: the supervisor starts the actor once more with `CrashLoop` as its first event, so `Failed(CrashLoop)` goes through `transition()`. If that actor panics too, the supervisor publishes `Failed(CrashLoop)` itself and starts an actor only on Reset.
- **Across an agent restart** the server is the source of truth: it stores `bots.last_state` and sends the sticky state with `AssignBot`/`ReconcileFull` (P10). The standalone agent persists nothing, which is a documented limit for a dev-only mode with offline accounts.

### Chaos test (P4.8)
A fixed 500 cases (`with_cases(500)`), as an exception to `PROPTEST_CASES`, so the DoD's count holds locally and in CI within the 30 s budget. Actor panics come from a test-only connector wrapper in fleet-runtime's `tests/`; the testkit fakes stay panic-free. The "no reconnect storms" invariant holds across actor panics too, with no exception for connects after a restart.

### Metrics (P4.9)
- `afkfleet_bots{state}` (gauge), `afkfleet_bot_reconnects_total`, `afkfleet_watchdog_trips_total{kind="tick"|"packet"}` and `afkfleet_actor_restarts_total`. No `bot_id` label.
- Tests install a hand-written recorder per test through metrics' thread-local `set_default_local_recorder`, so they work under plain `cargo test` too. No new crate.
- fleet-mc's diagnostics (`live_threads`, `abandoned_threads`, `live_worlds`, `dropped_chat`, `ignored_action_bar`) are exported by the agent in P5.3, since fleet-runtime may depend only on fleet-core.

### Determinism and lint guard
- `Fleet::new` takes a wall-clock anchor (`DateTime<Utc>`) and a seed (`u64`) from its caller. The runtime clock derives `DateTime<Utc>` from tokio's `Instant` anchored there (ADR-0010), and each actor's `StdRng` is derived from the seed. The runtime never reads the wall clock or the OS's randomness, so chrono's `clock` and rand's `sys_rng` stay off. P5 supplies both values.
- `crates/fleet-runtime/clippy.toml` repeats the root settings and bans `std::time::Instant::now`, `SystemTime::now`, chrono's `Utc::now`/`Local::now` and rand's OS entry points. tokio's `Instant::now` stays allowed: it's the runtime's clock. This makes the lints stricter.
- *(PR #14 review, the user's request)* It also bans `std::time::Instant::elapsed` and `std::time::SystemTime::elapsed`. They read the real clock just like `now`. The watchdog (P4.6) compares the session's `Liveness` stamps, which are std `Instant`s, so `stamp.elapsed()` would bypass paused time; it compares against tokio's clock instead. A temporary probe, never committed, called each one, and clippy flagged both as `disallowed_methods` with their reasons.

### Settings without a config key
They're `RuntimeConfig` fields with defaults, like fleet-mc's `McConfig`, and P5.1 maps only Appendix A's keys:

| Setting | Default |
|---|---|
| Session-request timeout | 30 s |
| Chat bucket | 1 message per 3 s, burst 3 |
| Chat queue | 16 |
| Actor inbox | 32 |
| Supervisor command queue | 64 |
| Event broadcast | 1024 |
| Watchdog check period | 1 s |
| Respawn retry | every 5 s, 12 failed calls |
| Restart window | 6 panics in 10 min |

### Dependencies
Approved by the user; all are in Plan.md §5:
- `governor` 0.10.4 without default features, plus `std`. The defaults add `quanta`, `dashmap` and `jitter`, and `jitter` pulls rand 0.9 and getrandom 0.3 into normal dependencies. Without `quanta` there's no `DefaultClock`, so the queue uses `direct_with_clock` with a tokio-backed clock (group B).
- `tokio-util` 0.7.19 without features, for `CancellationToken` only (group B).
- `metrics` 0.24.6, which has no features (group E).
- `chrono` 0.4.45 without default features in fleet-runtime, for `DateTime` only; §5's "Used in" now lists the runtime.
- `tokio` (`rt`, `sync`, `time`, `macros`), `rand` (`std_rng`), `tracing`, `thiserror`; dev: fleet-testkit, tokio `test-util`, rstest, proptest (`std`).
- Not added: `metrics-util`, `futures-util`.

## Consequences
- **Easier:**
  - One validated spec type runs from the server's row to the actor.
  - Conflict texts are a pure, tested function in core, and fleet-mc is untouched.
  - A crashing actor can't skip a backoff or reset its breaker, so the chaos test's storm invariant holds without exceptions.
  - The runtime is deterministic: its clock and its randomness come from its caller.
- **Harder:**
  - `transition()` takes `&BotRules` instead of `&RetryPolicy` (amends ADR-0010).
  - fleet-core grows `restore()` (P4.7) and `DisconnectReason::RespawnFailed` (P4.2).
  - The supervisor holds more per bot: the spec, the snapshot `watch`, the restart window and the breaker copy.
- **Later phases inherit requirements** (each has a note in Plan.md):
  - **P5.1:** the per-bot `conflict_texts` key; P5 supplies the wall-clock anchor and the seed.
  - **P5.3:** export fleet-mc's diagnostics; standalone persists no Paused or Failed state.
  - **P10:**
    - the managed credentials provider never hands out a token that expires within the connect timeout
    - the server sends the sticky state in `AssignBot`/`ReconcileFull`
    - the server moves a bot to another agent only after the old agent's `Removed` event (or a timeout), so two agents never run the same account at once
  - **P11:**
    - conflict texts per server in managed mode
    - where the per-bot chat limit lives (server settings, sent to agents)
    - "on respawn" mode steps and `SessionEvent::Respawned`, with which the respawn retry can confirm that the bot is alive instead of trusting an `Ok`
  - **P11.3:** its rule for commands with message arguments covers mode chat too.
  - **P11.9:** the chat format is parsed on the server.

## Alternatives considered
- **`BotSpec` in fleet-runtime.** The server would build proto messages straight from its rows, and validation would be split over two paths.
- **`AccountId` only, or no account in the spec.** Standalone bots would need made-up account IDs, or the provider a separate map from bot to username.
- **A conflict-text parameter on `classify()`** (the first plan). It would change fleet-mc's event mapping and its slow and manual tests; a separate function in core leaves fleet-mc alone.
- **An actor-side pre-check with a new disconnect reason.** It puts a lifecycle decision in the actor.
- **Comparing only kicks with no key or an unknown key.** `classify_key` would have to list vanilla's known transient keys, and nobody configures vanilla's own texts anyway.
- **A shorter text cap (256).** A long custom proxy message would be impossible to configure, since matching is exact.
- **Sanitizing config texts on load.** A typo with a `§` would silently change the text.
- **Storing `uptime` in the snapshot,** refreshed every second. It costs a publish per bot per second.
- **Re-applying holds after a respawn,** or restarting the mode then. Both need a "respawned" port event the ports don't have; restarting the whole mode would also repeat at-start chat and attacks on every death.
- **Awaiting the real send in `send_chat`.** The caller would wait behind the queue and up to fleet-mc's 10 s signing wait.
- **Retrying or reconnecting on `ChatUnavailable`.** Retrying has unbounded latency; reconnecting lets chat failures drive reconnects.
- **fleet-mc refusing message-argument commands now.** A hard-coded vanilla list misses aliases and plugin overrides, and it belongs to P11.3's rule for user commands.
- **Restarting a crashed actor from Stopped.** Its next connect would skip the backoff, and a fresh breaker would forget an open circuit.
- **An agent state file for Paused and Failed.** File IO, a format and a writable volume on a read-only root, for a dev-only mode.
- **A shared `Mutex` registry or breaker.** CLAUDE.md prefers message passing, and a panic while the lock is held poisons it.
- **`TaskTracker`.** It doesn't report a panicked task by itself.
- **Following `PROPTEST_CASES` in the chaos test.** 256 locally is below the DoD's 500, and 1000 in CI may break the 30 s budget.
- **A core diagnostics port, or fleet-mc recording metrics itself.** More port surface, or a new fleet-mc dependency, for numbers the agent can read directly.
- **`metrics-util`'s `DebuggingRecorder`.** A new crate for what a small test recorder does.
- **The runtime reading the clock and the OS's randomness itself.** chrono's `clock` and rand's OS features would then unify into fleet-core's build.
