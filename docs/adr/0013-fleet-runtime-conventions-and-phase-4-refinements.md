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
- *(group B, the user's decisions)* What the queue looks like as built:
  - **Events.** `FleetEvent` is defined in P4.5 with all its kinds (see Actor), so the queue's `ChatDelivery` publishes `ChatSent` and `ChatFailed` on the fleet's `broadcast` itself. The `RuntimeClock` stamps them, which makes the queue the clock's first user.
  - **Tickets** come from one fleet-wide counter, `ChatTickets`. A ticket never repeats while the agent runs, even across actor restarts, so the agent can map it to the control plane's request id.
  - **`ChatFailure`** is `Disconnected`, `ChatUnavailable`, `TimedOut`, `SessionBusy` or `NotInWorld`. A message still queued at a close, or a send that finds the session `Closed`, is `Disconnected`; the other `SessionError`s map one to one (`QueueFull` is `SessionBusy`).
  - **Mode chat** that's sent is published as `ModeChatSent{message}`, so the server learns what a mode said (P11). Mode chat that isn't sent is only logged.
  - **Admission** reserves a queue slot before it takes a token, so `QueueFull` uses up no token and `RateLimited` holds no slot.
  - **Logging.** Mode chat that's refused (`RateLimited`, `QueueFull`) or fails in the session logs at `warn` for the first time per kind and session, then at `debug`, like a failed action. `ChatUnavailable` stays at `debug`, and *(PR #15 review)* so does `Closed`: it only means the session has ended, so a disconnect that catches mode chat in flight or still queued doesn't warn. User chat that fails logs at `debug`. No log carries the text.
  - **The bucket** (`ChatBucket`) is shared: user chat and mode chat draw from it, it outlives the sessions, and the supervisor keeps it across actor restarts (P4.7). It runs on governor's `direct_with_clock` with a clock that reads tokio's `Instant` as a std `Instant`.
  - **Shape.** `ChatQueue::open(session, events, clock, cancel)` returns the session's `ChatDelivery`, which the actor spawns, and a `ModeChat` handle for the mode runner. `close()` cancels the delivery, which fails what's still queued. A send that's already running finishes, since every `SessionHandle` call has its own timeout, so its outcome is the real one. The task is named `ChatDelivery` because fleet-core's incoming chat already has a `ChatSender`.

### Mode runner (P4.4)
- A failed action is skipped. The first failure of each `SessionError` kind per session logs at `warn`, later ones at `debug`.
- **A mode change** lets go first (`HoldUse{on:false}`, `Sneak{on:false}`), then starts the new plan with its at-start steps. The selected slot stays, and an update with an equal definition changes nothing.
- *(group B, the user's decisions)* What the runner looks like as built:
  - **One task per Online session**, started at `StartMode` with a `CancellationToken` and a `watch::Receiver<ModeDefinition>`. A changed definition lets go and restarts the plan inside the same task, so the warn-once memory lasts the whole session.
  - **It ends** at cancellation, when a call fails with `Closed` (the session has ended), or when the owner drops the `watch` sender. A cancellation interrupts a tick's actions; a mode change waits until they're done.
  - **Logging.** Failed actions and refused mode chat (`RateLimited`, `QueueFull`) share the runner's warn-once log. Mode chat that finds the queue closed (`NotOnline`), and *(PR #15 review)* a call that finds the session `Closed`, always log at `debug` (see Actor).

### Actor (P4.2)
- **Events.** `FleetEvent { bot_id, at, kind }`, where `kind` is `StateChanged(BotSnapshot)`, `Died`, `ChatReceived(IncomingChat)`, `ChatSent{ticket}`, `ChatFailed{ticket, reason}`, *(group B)* `ModeChatSent{message}` or *(group C)* `Alert(BotNotification)`. There's one bounded `broadcast` per fleet, and a lagging subscriber resyncs (§6 row 12). *(group B)* The type is defined in P4.5, the queue being its first publisher.
- **Chat queue and mode runner** *(group B, the user's decision)*. At `StartMode` the actor opens the chat queue before it starts the mode runner. At `StopMode` or a disconnect it cancels the runner first and closes the queue after that, so mode chat never meets a closed queue; each gets its own child token of the session's token. A mode-chat send that still finds the queue closed (`NotOnline`) logs at `debug`, never `warn`, so a disconnect can't produce a spurious warning. *(PR #15 review)* The same holds for mode chat that the ended session refuses with `Closed`, in flight or still queued. The actor instruments both tasks with the bot's span.
- **Respawn.** A failed `respawn()` call is retried every 5 s while the session lives; the first failure logs at `warn`, the retries at `debug`. After 12 failed calls in a row the session ends with the new `DisconnectReason::RespawnFailed` (Transient), so the bot reconnects and respawns on join. An `Ok` ends the retrying, even if the server ignored it, since the ports can't tell.
- **Holds after a respawn** aren't re-applied; that's a known limit. A repeating `HoldUse{on: true}` step does re-apply a hold after a death, an at-start one doesn't. After a respawn none of the at-start setup runs again (look, slot, sneak, hold); P11 decides whether modes get "on respawn" steps, which needs a `SessionEvent::Respawned` port event.
- *(group C, the user's decisions)* What the actor looks like as built:
  - **Commands.** One `BotCommand` per `Fleet` call: `UpdateSpec`, `Restart`, `Reset`, `Resume` and `SendChat` (with a `oneshot` reply). There's no Start or Stop: the actor starts and stops the bot only as the spec's desired state says, when it starts and on every `UpdateSpec`, so the two can never disagree. `Restart` runs Stop then Start inside the actor, so a full inbox can't leave a restart half done, and it does nothing while the desired state is Stopped. This deviates from Plan.md's inbox list.
  - **API.** `BotActor::new(BotActorParts { spec, connector, credentials, retry, config, clock, rng, events, bucket, tickets, snapshot, breaker })` returns the actor and its `BotInbox` (`try_send`; `InboxError::Full` or `Closed`), and `run(cancel)` returns an `ActorExit`. The caller owns the snapshot and breaker `watch`es, so the supervisor keeps them across restarts. The breaker watch's current value is where the actor starts, and the actor writes a copy after each breaker effect. The `RetryPolicy` is passed in and joins the spec's conflict texts in `BotRules`. `ResetBreaker` is `record_success()`, which leaves a breaker exactly as a new one. *(group E)* `snapshot` is now a `SnapshotPublisher` from the supervisor's `SnapshotOwner`, so every write moves the bot in the bots gauge (see Metrics).
  - **In-flight work.** The session request, the retry timer and the respawn retry are futures the actor owns and polls in its `select!` only while their state lasts; leaving the state drops them, which is the cancellation ADR-0010 asks for. `connect()` is bounded by the connect timeout (Appendix A's `connect_timeout_secs`, now a `RuntimeConfig` field); running out is `ConnectFailed(TimedOut)`, so a connector that hangs can't freeze the actor.
  - **Teardown** runs off the loop, as a task in the actor's `JoinSet`, so the actor keeps serving its inbox and timers while fleet-mc tears a session down. Each session has a generation number, and a finished teardown is fed as `SessionClosed` only while no newer session has connected. The actor puts no timeout of its own on `disconnect()`, and the teardown task takes no `CancellationToken`: the port guarantees `disconnect()` finishes, since fleet-mc abandons a hung host thread after its own timeouts, and the actor waits for it even when it's cancelled itself. Its `JoinSet` owns it, so an aborted actor aborts it too, which drops the handle and ends the host thread (ADR-0011). CLAUDE.md lists this as the second exception to "every task has a child token".
  - **A server change or `Restart`** goes through `Stopping{restart}` while Connecting or Online. With no session yet (AwaitingSession or Backoff) it starts a new run at once: Stopped, then `AwaitingSession{1}` with `ResetBreaker`, dropping the rest of the backoff, a pending request and the breaker's cool-down, since it's a deliberate act. A mode change only restarts the mode, and new conflict texts apply at once. An `UpdateSpec` for another bot or account is ignored and logged at `error`, with IDs only.
  - **Order.** The `select!` is biased, so paused-time tests are reproducible: cancellation, finished tasks, the inbox, the session-request answer, the retry timer, the respawn retry, and the session's events last. The server controls how fast session events arrive, so a chat flood can't hold up the rest.
  - **Ready session events go first.** Before every input that ends a session, the actor applies the session events that are already ready, with one non-waiting poll each. Those inputs are: Restart, an `UpdateSpec` that changes the server or sets desired = Stopped, cancellation, a closed inbox, the respawn retry's last failure and a crash exit. So a duplicate-login kick that's already queued always pauses the bot first, and the command then finds it Paused (§6 row 3). The drain stops at `RuntimeConfig::session_drain` = 67 events. fleet-mc's queue drops chat at 64 but always admits lifecycle events, and the port contract allows at most 3 of those at once (`Joined`, one `Died` until the respawn, one terminal event); the queue is first in, first out, so 67 reads reach everything that was queued when the input came. After the drain, the actor's own session-ending inputs apply only if the same session is still current.
  - **Ending.** Cancellation, or every inbox sender dropped, stops the bot through `transition()` (Stopping, then Stopped), waits for every teardown and returns `ActorExit::Stopped`. Paused and Failed ignore the Stop.
  - **A crashed task.** If the mode runner, the chat delivery or a teardown panics, the actor logs the task and the bot's ID at `error` (never the panic payload), applies the ready session events, tears the session down without `transition()` and returns `ActorExit::TaskCrashed{task}`. The last published state stays, so P4.7's `restore()` treats it as after a real panic, and the supervisor counts it in the restart window exactly like one. Nothing re-raises the panic, so library code gets no panic path.
  - **Respawn.** A failed call is retried 5 s after it failed; the session ends after the 12th failed call in a row, about 55 s after the death. `Closed` ends the retry at `debug` and never counts, since the session's own end follows. Each death warns on its first failure.
  - **Events and logs.** `Notify` is published as `Alert(BotNotification)`, right after the `StateChanged` it belongs to. The starting state (Stopped) goes into the snapshot `watch` without an event. `last_disconnect` changes only when a session ends on its own: a kick or other reason, a failed connect (including no host thread and the actor's connect timeout), events that end without a reason (`SessionCrashed`), a watchdog or liveness trip, or a respawn failure; a deliberate end leaves it as it was. State changes log at `info`. A session that ends on its own and is retried, a session-request timeout and a retryable credential error log at `warn`, with the reason's kind and translation key; a kick's text goes only to `debug`. Paused logs at `warn` (a human playing is expected), Failed at `error`.
  - **Tests.** fleet-testkit's fake session gained `fail_respawn(error)`, `succeed_respawn()` and `delay_disconnect(duration)`; a delayed `disconnect()` closes the session at once and returns after the delay on tokio's clock, the way fleet-mc closes first and then waits for its thread. Crashes come from a test-only panicking wrapper in fleet-runtime's `tests/`, as P4.8 decided; the fakes stay panic-free.
- *(group D, found in the plan review and fixed in P4.7)* **Reset and Resume apply the desired state.** Group C's actor fed only `Reset` or `Resume`, so a Paused bot whose spec had since said desired = Stopped connected again on Resume (and a Failed one on Reset), and resending the spec couldn't stop it once P4.7 answers an equal spec without forwarding it. Both now feed their event, then apply the desired state in the same turn, as `UpdateSpec` does: with desired = Stopped the bot passes `AwaitingSession{1}` and ends Stopped. The session request is dropped before it's ever polled, so the provider is never asked and nothing connects. No core change was needed, and a reset of a bot without an actor does the same. A regression test in `tests/bot_actor.rs` covers both paths.
- *(group E, found in the chaos-test plan and fixed in P4.8)* **A crashing actor closes its inbox first.**
  - **The gap.** After a task crashed, `crash()` waited for every teardown before it returned `TaskCrashed`, and it read no command meanwhile. fleet-mc's teardown can take seconds, and the chaos test's slow teardowns up to 20 s. In that wait, a `send_chat` forwarded to the actor waited for an answer that never came and ended `TimedOut`, which breaks "the Fleet API always responds". A Reset, Resume or Restart was answered `Ok` and then dropped with the inbox, so a resumed Paused bot came back Paused after the restart.
  - **The fix.** `crash()` now closes its inbox before anything else. The supervisor's next `try_send` fails as `Closed`, which it already answers `Busy`. The commands already queued are dropped, so a waiting `send_chat` gets `Busy` at once from its dropped reply. A caller can retry once the restarted actor runs.
  - **Known limit.** A command forwarded in the same instant as the crash, before the actor has seen it, is still lost, as with a panic of the actor itself. The chaos test's model treats a call made in the same instant as a crash of that bot as possibly lost.
  - **Test.** A regression test in `tests/fleet/supervisor.rs` failed first, with `TimedOut` for `send_chat` and `Ok` for `resume`. It now checks that `send_chat`, `resume` and `restart` answer `Busy` without the clock moving.

### Watchdog (P4.6)
It checks every 1 s while Online. A tick stall gives `WatchdogTimeout`, a packet stall `Disconnected(LivenessTimeout)`, and the tick stall wins when both are stale (ADR-0010).

*(group C, the user's decisions)* As built:
- **It's a timer in the actor's `select!`,** armed when the bot goes Online and dropped when it leaves. The first check comes 1 s after the join, and each check arms the next one a period later. It reads the session's `Liveness` and feeds `transition()` directly, with no task or channel of its own.
- **A stamp is stale when it's at least its timeout old,** so with 30 s and a 1 s period a stall is caught 30–31 s after the last tick or packet. The check compares the stamps with tokio's clock as a std `Instant` through `saturating_duration_since`, never the banned `elapsed()`, so paused time drives it, and a stamp later than now counts as fresh.
- **Ready session events go first.** Before a trip ends the session, the actor applies the session events that are already ready (see Actor). A trip applies only if the same session is still online afterwards, so a queued duplicate-login kick pauses the bot instead.
- **Settings.** `watchdog_timeout` and `packet_liveness_timeout` (30 s each) are `RuntimeConfig` fields for Appendix A's keys. `watchdog_period` (1 s) is a default without a key; a zero period counts as 1 ms, so the biased loop can't spin. A trip is recorded in `last_disconnect` and logs at `warn` like any session that ends on its own; P4.9 counts it.

### Supervisor and `Fleet` (P4.7)
- **Inside.** Actors run in tokio's `JoinSet`, which reports `JoinError::is_panic()`, each with a child `CancellationToken`. The `Fleet` handle talks to the supervisor task over a bounded queue with a reply timeout, and the supervisor forwards to the actor inboxes with `try_send`. That's how "the Fleet API always responds" holds.
- **API:** `apply(spec, restore: Option<StickyState>)`, `remove`, `reset`, `resume`, `restart`, `send_chat`, `snapshot`, `snapshot_all`, `subscribe` and `shutdown(timeout)`.
  - Bots start and stop only through the spec's desired state. `restart` is a no-op when the desired state is Stopped.
  - `apply` refuses a new bot above `[runtime] max_bots` with `AtCapacity`.
  - The errors are `UnknownBot`, `Busy`, `AtCapacity`, `AccountChanged`, *(group D)* `AccountInUse`, `ShuttingDown` and `TimedOut`.
  - `remove` returns once the supervisor accepted it. The teardown runs in the background, then `StateChanged(Stopped)` and `Removed` go out. Until `Removed`, `apply` for that `BotId` returns `Busy`.
  - *(group D, the user's decision; refines the line above)* `StateChanged(Stopped)` goes out only when the actor stops a running bot through `transition()`. A Paused, Failed or crash-looped bot gets only `Removed`, and its last published state stays. The reason is P10.5: it moves a bot by waiting for the old agent's `Removed`, then sends the server's stored `last_state` with `AssignBot` as the sticky restore. A `Stopped` right before `Removed` would overwrite a stored Paused or Failed, so the new agent would connect and kick the human who's playing (§6 row 3), or retry a failed account.
- **Restarts never skip the backoff.**
  - The supervisor keeps each bot's spec, snapshot `watch`, restart window and circuit breaker. The actor writes a copy of its breaker into a private `watch` after each breaker effect, so a panic doesn't reset it.
  - A pure `fleet_core::bot::restore(&BotState, now, &BotRules) -> Transition` decides the restored state and its effects:
    - AwaitingSession, Connecting, Online and Backoff come back as `Backoff{attempt}` with `ScheduleRetry`; an Online bot that had lasted the stable period comes back as `Backoff{1}`
    - Paused, Failed and Stopped stay
    - Stopping becomes Stopped

    Then the spec's desired state applies.
  - A panic isn't a breaker failure; only the restart window counts it.
  - *(group B, the user's decision)* The supervisor also keeps each bot's `ChatBucket` and hands the same one to a restarted actor, so a crash doesn't refill it. It owns the fleet's `ChatTickets` and gives every actor a clone.
  - The 6th panic within 10 min gives `CrashLoop`: the supervisor starts the actor once more with `CrashLoop` as its first event, so `Failed(CrashLoop)` goes through `transition()`. If that actor panics too, the supervisor publishes `Failed(CrashLoop)` itself and starts an actor only on Reset.
- *(group D, the user's decisions)* What the supervisor looks like as built:
  - **Shape.** `Fleet::new(FleetParts { connector, credentials, retry, circuit, config, anchor, seed })` returns `Result<(Fleet, Supervisor), FleetSetupError>`. The caller runs `supervisor.run(cancel)` in a task it owns (P5.3), like the actor's `run`. `Fleet` isn't generic and is `Clone`.
  - **Setup checks.** `Fleet::new` checks the settings before it builds anything:
    - The chat rate limit becomes a `ChatQuota` (`try_new`), checked once. Every bot's bucket is `ChatBucket::with_quota(quota)`, which can't fail; `ChatBucket::new` stays and is built on the two.
    - Every capacity handed to tokio is checked against tokio's documented limit, since tokio panics above it: `event_buffer` against `usize::MAX / 2` (`broadcast`), and `supervisor_queue`, `actor_inbox` and `chat_queue` against `Semaphore::MAX_PERMITS` (`mpsc`). `broadcast` allocates its whole buffer up front, so a huge value that passes can still run out of memory; `mpsc` allocates as it fills.
    - Errors: `FleetSetupError::ChatQuota(ChatBucketError)` and `CapacityTooLarge { setting: CapacitySetting }`, which names the field.
  - **Calls.** A `Fleet` call `try_send`s to the supervisor's queue: full is `Busy` at once. It then waits for the answer up to `reply_timeout` (5 s, no config key): `TimedOut`. A closed queue or a dropped reply means the supervisor has ended: `ShuttingDown`. `TimedOut` is only for a live supervisor that doesn't answer in time.
  - **Forwarding.** The supervisor forwards one `BotCommand` per call with `try_send` and never waits for an actor. When that fails (`Full`, or `Closed` in the short race before an actor's exit is handled), the call is `Busy`, and `apply` doesn't store the spec, so a restart never starts from a spec the caller was told failed. For `send_chat` the supervisor answers with the receiver the actor answers on; an actor that ends before it answers also gives `Busy`. `send_chat` returns `Result<ChatTicket, SendChatError>`, with `Fleet(FleetError)` and `Chat(ChatError)`.
  - **`apply` for a new bot.** It's refused with `AccountInUse` when a live bot's account clashes, by the new `BotAccount::clashes_with`: offline names compare ignoring ASCII case, since Minecraft names are unique whatever their case, and online accounts by `AccountId`. If only a bot that's being removed holds the account, the answer is `Busy`, since the account is free once its `Removed` goes out. `max_bots` (50) counts every bot the fleet knows, in any state, until its `Removed`.
  - **`apply` for a known bot.** `AccountChanged` compares exactly, like the actor, so a change only in case is a changed account (offline-mode servers derive the player's UUID from the exact name). A spec equal to the stored one answers `Ok` without being forwarded, so a full reconcile (P10) doesn't fill the inboxes. A restore for a bot the fleet already runs is ignored and logged at `debug`: the actor's own state is newer than the server's stored one.
  - **`StickyState`** is `fleet_core::bot::StickyState { Paused(PauseReason), Failed(FailReason) }`, pure data next to `BotSpec`, with `From<StickyState> for BotState`.
  - **Starting points.** `BotActorParts` gained `start: Transition`, which the supervisor computes:

    | Start | Value |
    |---|---|
    | New bot | `Stopped`, no effects |
    | Sticky | `Paused` or `Failed`, no effects |
    | Restart | `restore()` |
    | Crash loop | `transition(&restore(..).state, CrashLoop, ..)` |

    The actor takes on the state, executes the effects (e.g. `ScheduleRetry`), then applies the desired state: Running feeds `Start`, Stopped feeds `Stop`.
  - **What a start publishes.** A `StateChanged`, with `since` = now, goes out only if the start state differs from what the snapshot `watch` holds. A restart that keeps Paused publishes nothing and keeps its `since`. A new bot's `watch` starts as Stopped, so a sticky start publishes `StateChanged(Paused)` without an `Alert`, since the server already knows. A crash loop's `Notify` still publishes its `Alert`. `last_disconnect` carries over from the `watch`.
  - **`restore()` of a stable Online** returns `[RecordSuccess, ScheduleRetry{1}]`, as `leave()` does when a stable session ends any other way.
  - **Crashes.** A panic, a `TaskCrashed`, and an exit the supervisor didn't ask for all count in the bot's restart window (`restart_limit` = 6 within `restart_window` = 10 min). An unexpected `Stopped` or abort is a bug: it logs at `error` and is handled like a crash. A restart logs at `warn` in the bot's span, with the kind of crash and the count in the window, since it's a recovered fault and the panic itself is logged by the actor or P5.2's panic hook. The crash loop, and a crash of the crash-loop actor, log at `error`. `FailureWindow` gained `len()` and `is_empty()` for the count.
  - **After the crash-loop actor crashes too,** the bot has no actor and shows `Failed(CrashLoop)`, published only if it changed. Calls act as on a live Failed actor: `apply` stores the spec for the next actor, `resume` and `restart` answer `Ok` and do nothing, `send_chat` is `Chat(NotOnline)`, `remove` publishes `Removed` at once, and `reset` starts an actor from `transition(Failed(CrashLoop), Reset)`.
  - **The restart window is cleared** only when the supervisor forwards a `reset` while the bot is crash-looped: its actor was started for a crash loop, or it has none. So a human's retry after a crash loop gets the whole window. A Reset out of any other Failed reason leaves it alone. *(PR #17 review)* The supervisor goes by its own record, not only by the published `Failed(CrashLoop)`: a Reset can arrive before the crash-loop actor has published that state, and it would otherwise leave the flag set, so a single crash much later would fail the bot at once.
  - **While a bot is being removed,** `apply` is `Busy`; `reset`, `resume`, `restart` and `send_chat` are `UnknownBot`; a second `remove` is `Ok`; `snapshot` and `snapshot_all` still show the bot until `Removed`. A crash during removal logs at `warn` and isn't restarted; `Removed` still goes out.
  - **Shutdown.** `shutdown(timeout)` cancels every actor, waits up to `timeout`, then aborts the rest, which drops their sessions so fleet-mc ends the host threads. It returns `ShutdownReport { stopped, aborted, crashed }` and logs aborts at `warn`; a crash during the shutdown counts under `crashed` and logs at `warn`. Calls that arrive meanwhile, and every later call, answer `ShuttingDown`. A removal accepted before still publishes `Removed`. The call waits `timeout` plus the reply timeout. A cancelled run token, or every `Fleet` dropped, shuts down the same way within `RuntimeConfig::shutdown_timeout` (10 s, Appendix A's `shutdown_timeout_secs`).
  - **Order.** The `select!` is biased: cancellation, actor exits (`join_next_with_id`, mapped back to the bot by task ID), then calls. A crash is handled and its actor restarted before the next call.
  - **Randomness.** The supervisor keeps one `StdRng` seeded from the seed, and every actor start draws `StdRng::seed_from_u64(next_u64())`, so a run replays for the same order of calls.
  - **`snapshot_all`** is sorted by `BotId`: the supervisor keeps its bots in a `BTreeMap`.
  - **Tests** are one folder crate, `tests/fleet/`, as fleet-mc's `tests/minecraft/`: `main.rs` reaches the shared `Levels` with `#[path = "../common/mod.rs"]`, `panicky.rs` has the connector whose connects or sessions panic on cue, and `supervisor.rs` the scenarios. P4.8's chaos test becomes `tests/fleet/chaos.rs` and reuses `panicky.rs`, so every helper is used somewhere in the crate.
- **Across an agent restart** the server is the source of truth: it stores `bots.last_state` and sends the sticky state with `AssignBot`/`ReconcileFull` (P10). The standalone agent persists nothing, which is a documented limit for a dev-only mode with offline accounts.

### Chaos test (P4.8)
A fixed 500 cases (`with_cases(500)`), as an exception to `PROPTEST_CASES`, so the DoD's count holds locally and in CI within the 30 s budget. Actor panics come from a test-only connector wrapper in fleet-runtime's `tests/`; the testkit fakes stay panic-free. The "no reconnect storms" invariant holds across actor panics too, with no exception for connects after a restart.

*(group E, the user's decisions)* What the chaos test looks like as built (`tests/fleet/chaos.rs`):
- **Cases.**
  - **Size.** 1–3 bots, 1–30 steps, and 0–90 s of paused time after each step, with Appendix A's settings.
  - **Faults,** each for one bot:
    - transient, permanent and duplicate-login kicks
    - failed connects, a connect that never joins (failed by the test after the connect timeout, as fleet-mc would), auth rejections, and retryable, refused and stuck credentials
    - a hung session, a dead link, a tick stall, and teardowns of 0–20 s
    - panics in connect, perform or teardown, and crash bursts of 1–8 panics on the next connects, at most 8 per bot, so some cases reach the crash loop
  - **Calls:** spec updates (`afk` and a cheap custom mode, two placeholder servers, the desired state), Restart, Reset, Resume, `send_chat`, the reads, and remove plus re-add. The re-add works as P10.5's move does: `apply` is retried on `Busy` until `Removed`, with the last published Paused or Failed as the restore.
- **Faults are scripted per bot in the test:** `panicky.rs` for connects and panics, `ScriptedCredentials` for session requests. A server task joins each new session unless the bot's script says otherwise. fleet-testkit doesn't change. A fault counts only once it has landed, and the end-state rule reads what the test injected, never the bot's own events, so a bug that fails a bot on a transient fault can't excuse itself.
- **Invariants.**
  - **No panic escapes:** the supervisor's task ends cleanly.
  - **Healing:** a bot that should run, saw only transient faults and at most 5 fired crashes, ends Online; with 6 or more it may end `Failed(CrashLoop)`. Reset and Resume bring a Failed or Paused bot back into that class, unless the call came in the same instant as a crash of its bot (see Actor).
  - **The storm bound,** attempt-aware:
    - `AwaitingSession{1}` only follows a deliberate call.
    - `AwaitingSession{m≥2}` only follows `Backoff{m−1}`, at least `bounds(m−1).0` after it.
    - `Backoff{n}` keeps the attempt of the state it left, or is `Backoff{1}` after a stable Online.
    - Each connect follows its own published `Connecting`, read by the connector from its own event receiver, so a bot never connects while Paused or Failed. The fresh-token retry is allowed by its own rule.
    - A `Lagged` on either receiver fails the case.
  - **The API:** every call answers before the clock moves, and never `TimedOut`.
  - **Metrics:** the bots gauge equals `snapshot_all` and is 0 after the shutdown; the reconnect counter equals the published `Connecting`s with attempt > 1 or `auth_retried`.
- **Probes** *(added during the build)*. Every bot the test holds and isn't removing gets a `send_chat` every 5 s. Without them, the test as planned passed all 500 cases with the crash-gap fix reverted, since random steps rarely call a bot whose crashed actor is still tearing down. With them it failed at once.
- **Settling** ends once every bot is in a state it stays in, with no stalled session left and no perform panic waiting on an Online bot. The cap is each scripted fault's worst wait times their number plus 3.
- **Coverage.**
  - The test counts the cases that reached each of 8 targets and fails if one is 0. The targets are a crash loop, a duplicate-login pause, the fresh-token retry, a sticky re-add, a teardown running at a connect, a trip of each kind, and a probe while a crashed session is still inside `disconnect()`.
  - The weights keep each target in about 7 % of cases or more.
  - The test also asserts that it ran exactly 500 new cases, so `PROPTEST_CASES` can't change the count.
- **Red runs** on temporary breaks: the reverted crash-gap fix, `restore()` resetting the attempt, and `restore()` without `ScheduleRetry`. Each failed with a shrunk one-bot case (Plan.md P4.8).

### Metrics (P4.9)
- `afkfleet_bots{state}` (gauge), `afkfleet_bot_reconnects_total`, `afkfleet_watchdog_trips_total{kind="tick"|"packet"}` and `afkfleet_actor_restarts_total`. No `bot_id` label.
- Tests install a hand-written recorder per test through metrics' thread-local `set_default_local_recorder`, so they work under plain `cargo test` too. No new crate.
- fleet-mc's diagnostics (`live_threads`, `abandoned_threads`, `live_worlds`, `dropped_chat`, `ignored_action_bar`) are exported by the agent in P5.3, since fleet-runtime may depend only on fleet-core.
- *(group E, the user's decisions)* What the metrics look like as built:
  - **The bots gauge changes with every write of a bot's snapshot.**
    - The supervisor's `Entry` holds a public `SnapshotOwner`, which isn't `Clone`. `new` counts the bot in its first state, and its `Drop` takes the bot out of its current state. So removing a bot (the entry is dropped) and the supervisor's end (after `run`) take it out on every path.
    - The owner hands out a clonable `SnapshotPublisher` for each actor. `BotActorParts.snapshot` takes that instead of the raw `watch::Sender`, which stays private. Each publish replaces the snapshot and moves the bot from the old state's label to the new one, from `send_replace`'s old value; a change within one label does nothing. A publisher that's dropped, as when an actor ends, changes nothing.
    - So no write can skip the gauge. P4.8's chaos test checks the gauge against `snapshot_all` after every case, and checks that it's 0 once the fleet has shut down.
  - **A reconnect** is counted at the actor's `connect()` when `attempt > 1 || auth_retried`: every connect that isn't the first of a deliberate run (Start, Reset, Resume, Restart, a server change). That includes the fresh-token retry, a retry after a crash restart, and connects that then fail.
  - **A watchdog trip** is counted only once it applies, after the drained session events and the `is_current_online` guard, so each count is a session the watchdog ended. It's labelled `kind="tick"` for `WatchdogTimeout` and `kind="packet"` for `LivenessTimeout`.
  - **An actor restart** is counted where the supervisor starts an actor after a crash, the crash-loop start included. Not counted: a new bot, a Reset that starts an actor for a bot without one, a crash during removal or a shutdown, and a crash of the crash-loop actor.
  - **Labels.** A private, exhaustive match in fleet-runtime, with no catch-all arm, maps each `BotState` variant to one of 8 `state` labels: `stopped`, `awaiting_session`, `connecting`, `online`, `backoff`, `paused`, `failed`, `stopping`. A new variant doesn't compile until it has one. A unit test checks that they're unique snake_case names. fleet-core is unchanged.
  - **Registration.** `Fleet::new` describes the four metrics, which gives the exporter its `# HELP` lines, and registers every series at 0 (`increment(0)` never overwrites): 8 states, 2 trip kinds and the two plain counters. The names are private constants. Both go to the recorder installed when `Fleet::new` runs, so the agent installs its exporter first (P5.3).
  - **Tests.**
    - `tests/fleet/metrics.rs` drives every case the `Fleet` can reach. `tests/fleet/recorder.rs` holds the recorder, and the helpers shared with `supervisor.rs` moved to `tests/fleet/harness.rs`.
    - Three cases are unit tests with a second recorder in `src/metrics.rs` (`cfg(test)`, which integration tests can't see), because the `Fleet` can't reach them: a crash of the crash-loop actor, which has no session, so neither its crash nor the supervisor's own `Failed(CrashLoop)` can be caused from outside; and a trip the drained events overtake, which only a hand-driven actor can line up. `SnapshotOwner`'s rules are unit-tested there too.
    - Two temporary mutations, never committed, showed that the tests for what isn't counted catch the bug: counting a trip before the drain failed one test, and counting every crash as a restart failed three.

### Determinism and lint guard
- `Fleet::new` takes a wall-clock anchor (`DateTime<Utc>`) and a seed (`u64`) from its caller. The runtime clock derives `DateTime<Utc>` from tokio's `Instant` anchored there (ADR-0010), and each actor's `StdRng` is derived from the seed. The runtime never reads the wall clock or the OS's randomness, so chrono's `clock` and rand's `sys_rng` stay off. P5 supplies both values.
- `crates/fleet-runtime/clippy.toml` repeats the root settings and bans `std::time::Instant::now`, `SystemTime::now`, chrono's `Utc::now`/`Local::now` and rand's OS entry points. tokio's `Instant::now` stays allowed: it's the runtime's clock. This makes the lints stricter.
- *(PR #14 review, the user's request)* It also bans `std::time::Instant::elapsed` and `std::time::SystemTime::elapsed`. They read the real clock just like `now`. The watchdog (P4.6) compares the session's `Liveness` stamps, which are std `Instant`s, so `stamp.elapsed()` would bypass paused time; it compares against tokio's clock instead. A temporary probe, never committed, called each one, and clippy flagged both as `disallowed_methods` with their reasons.
- *(group B, the PR #14 review's request)* The chrono and rand bans are proven too. chrono arrived in P4.5 and rand in P4.4. A temporary probe, never committed, enabled chrono's `clock` and rand's `thread_rng` for fleet-runtime only and used each banned path. Clippy flagged all 12 with their reasons: `chrono::Utc::now`, `chrono::Local::now`, `rand::rng`, `random`, `random_iter`, `random_range`, `random_bool`, `random_ratio`, `fill` and `make_rng` as `disallowed_methods`, and `rand::rngs::ThreadRng` and `SysRng` as `disallowed_types`. So `allow-invalid` hides none of them.

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
| Session events applied before a session-ending input *(group C)* | 67 (64 + 3) |
| Restart window | 6 panics in 10 min |
| `Fleet` reply timeout *(group D)* | 5 s |

*(group C)* The connect timeout, the watchdog timeout and the packet-liveness timeout are `RuntimeConfig` fields too, but they're Appendix A's `[runtime]` keys (`connect_timeout_secs`, `watchdog_timeout_secs`, `packet_liveness_timeout_secs`, 30 s each), which P5.1 maps.

*(group D)* So are `max_bots` (50) and the shutdown timeout (10 s), for Appendix A's `max_bots` and `shutdown_timeout_secs`. The new `RuntimeConfig` fields are `max_bots`, `supervisor_queue` (64), `event_buffer` (1024), `reply_timeout` (5 s), `shutdown_timeout` (10 s), `restart_limit` (6) and `restart_window` (10 min).

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
  - **P10.7** *(group B)*: the agent maps `FleetEvent`s to `BotEvent`, a chat ticket back to `SendChat`'s request id, and `ModeChatSent` to outgoing chat without one. *(PR #15 review)* A ticket's event can arrive before the `send_chat` reply, so the agent handles a ticket it hasn't mapped yet. *(group C)* It maps `Alert` too.
  - **P4.7** *(group C)*: the supervisor counts `ActorExit::TaskCrashed` like a panic, owns each bot's snapshot and breaker `watch`es, sends one `BotCommand` per `Fleet` call, and gives `BotActorParts` a restored starting point for a restarted actor (the `Transition` from `restore()`, or a sticky Paused or Failed from `apply`'s restore), deciding what the actor publishes for it.
  - **P4.8** *(group C)*: a `Restart` or server change starts a deliberate new run, in AwaitingSession and Backoff too, so the storm invariant leaves those connects out of its bound. *(group D)* The chaos test becomes `tests/fleet/chaos.rs` and reuses `tests/fleet/panicky.rs`.
  - **P5.1** *(group D)*: map `max_bots` and `shutdown_timeout_secs` to `RuntimeConfig`, and refuse two `[[standalone.bots]]` entries whose accounts clash, with `BotAccount::clashes_with`.
  - **P5.3** *(group D)*: the agent runs `supervisor.run(cancel)` in a task it owns, and reports a `FleetSetupError` from `Fleet::new` at startup.
  - **P5.4** *(group D)*: a signal calls `Fleet::shutdown(shutdown_timeout)`, or cancels the supervisor's token, and logs the `ShutdownReport`.
  - **P5.3** *(group E)*:
    - the agent passes `Fleet::new` a random seed from `getrandom`, never a fixed one, so two agents never jitter their reconnects in lockstep
    - if the supervisor's task ends without a requested shutdown (a `JoinError` or an early return), the agent logs it at `error` and exits with an error code, so Docker restarts it, as at the abandoned-thread limit
    - the agent installs the Prometheus recorder before it calls `Fleet::new`, since the metrics' descriptions and 0-series go to the recorder installed at that moment
  - **P5.6** *(group E)*: the compose file gives the agent a `stop_grace_period` above `shutdown_timeout` (e.g. 15 s for the 10 s default), since `docker stop` kills after 10 s by default.
  - **P10.5** *(group D)*: a removed Paused, Failed or crash-looped bot publishes no `StateChanged(Stopped)` before `Removed`, so the server keeps its stored state and restores it on the next agent.
  - **P10.7** *(group D)*: the agent maps `FleetEventKind::Removed` too, since P10.5's move waits for it.
  - **P11.1** *(group B)*: `chat_messages` can record outgoing mode chat from `ModeChatSent`.
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
- *(group B)* **The queue reporting outcomes to the actor over its own channel,** with `FleetEvent` left to P4.2. The actor would have to drain that channel at all times, or the delivery would stall behind it.
- *(group B)* **Per-bot tickets** start over after an actor restart, so a ticket the agent still maps could repeat. **Caller-supplied ids** would contradict "`send_chat` returns a ticket".
- *(group B)* **Taking the token before the queue slot.** A message refused with `QueueFull` would still use up a token.
- *(group B)* **No event for mode chat,** or ticket-less `ChatSent`. Without an event the server never learns what a mode said; an optional ticket would blur user chat and mode chat in one kind.
- *(group B)* **A fresh chat bucket after an actor restart.** Each crash would allow another burst.
- *(group C)* **Start and Stop commands in the inbox,** as Plan.md listed them. The desired state and the commands could then disagree, and a restart as two messages could stop halfway when the inbox fills up.
- *(group C)* **Awaiting the teardown inside the effect.** Simpler, but the inbox and the timers would wait for as long as fleet-mc's teardown takes. **An actor-side teardown timeout** would add a setting for a bound the port already gives.
- *(group C)* **Spawned tasks for the session request, the timers and the respawn retry,** reporting back over a channel. More tasks and stale answers to filter, where a dropped future cancels for free.
- *(group C)* **The actor creating its `watch`es** and returning receivers. The supervisor would rewire them after every restart and pass the breaker in separately.
- *(group C)* **Keeping the backoff on a server change or Restart without a session.** A new server would wait out the old one's backoff and breaker cool-down.
- *(group C)* **Re-raising a task's panic in the actor** (`resume_unwind`): a panic path in library code. **Ending the session as `SessionCrashed`:** a mode that always panics would reconnect forever, slowed only by the backoff. **Only logging it:** the session would run on without its mode or chat.
- *(group C)* **Silent teardown on cancellation,** without `transition()`: the last published state would never say the bot stopped.
- *(group C)* **Session events first in the `select!`.** A server flooding chat could hold up the inbox and the timers. **No drain before a session-ending input:** a queued duplicate-login kick would be lost to a Restart, and the bot would reconnect and kick the human. **An unbounded drain:** a chat flood could hold the input back for as long as it lasts.
- *(group C)* **A respawn retry on a fixed 5 s cadence,** or one that counts `Closed`: a call that's slow to fail would bunch the retries, and an ended session would warn for nothing.
- *(group C)* **`Notify` as a log line only:** the app would derive alerts from state changes itself.
- *(group C)* **Clearing `last_disconnect` on a deliberate end:** the last fault would vanish with a Stop.
- *(group C)* **A watchdog task per session,** reporting stalls over a channel: one more task and channel for a check the actor's loop does in place. **Stale only beyond the timeout (`>`):** a stall would be caught a second later, at 31–32 s.
- *(group D)* **`Fleet::spawn` into a given `JoinSet`, or a `Fleet` that holds the supervisor's `JoinHandle`.** The first hides the task from its owner's own patterns; the second needs a lock around the handle for every clone.
- *(group D)* **Cancelling without a bound, or aborting at once,** when the supervisor's token is cancelled or every handle dropped. The first relies only on the port's guarantee; the second publishes no Stopped at all.
- *(group D)* **`shutdown` returning `()` or `Err(TimedOut)` on aborts.** The caller couldn't tell how many bots stopped cleanly. **Waiting for room in a full supervisor queue:** overload is an error, not a wait.
- *(group D)* **A `Chat` variant in `FleetError`, or a nested `Result`.** Every other call's signature would admit an error it can't produce, or callers would unwrap twice.
- *(group D)* **`StickyState` in fleet-runtime, or a plain `BotState`.** fleet-proto and fleet-server couldn't build the first, and the second allows restoring an Online bot.
- *(group D)* **An `ActorStart` enum the actor resolves itself** (reading its last state from the `watch` and calling `restore()`). The actor would know about restarts; a plain `Transition` keeps all of that in the supervisor.
- *(group D)* **Publishing every start state, or none.** Always publishing repeats an unchanged Paused after each restart; never publishing hides Online becoming Backoff after a crash.
- *(group D)* **`restore()` without `RecordSuccess` for a stable Online:** a breaker that was half-open when the bot joined would stay half-open after the crash.
- *(group D)* **Forcing or refusing a restore for a known bot.** Forcing needs a path into Paused or Failed outside `transition()`; refusing makes the agent resend without it.
- *(group D)* **Every removal ending with `StateChanged(Stopped)`:** see the refined `remove` line above; P10.5 would lose a stored Paused.
- *(group D)* **Clearing the restart window on every reset, or never.** Any reset would hide a crash loop that's still going; never clearing would send a reset soon after a crash loop straight back into it.
- *(group D)* **`Busy` for every call to a bot without an actor:** `apply` couldn't update its spec until a reset.
- *(group D)* **`apply` keeping the spec when the inbox is closed:** the caller would be told `Ok` for a spec the next actor might start from only by luck of timing.
- *(group D)* **Seeds mixed from the bot ID:** order-independent, but needs a hand-written stable mixing function.
- *(group D)* **Restart logs at `error`:** a recovered fault at the level that means "a human must act", next to the panic's own error log.
- *(group D)* **Calls before actor exits in the `select!`:** more calls would meet a crashed actor's closed inbox.
- *(group D)* **`max_bots` counting only desired-Running bots:** an `apply` that flips a known bot to Running could then fail with `AtCapacity`.
- *(group D)* **No account-clash check in the runtime:** a server or config bug would run two bots on one account, which kick each other into Paused.
- *(group D)* **A private comparison in the supervisor** instead of `BotAccount::clashes_with`: P5.1 would write its own. **Naming it `is_same_account`:** it would contradict `AccountChanged`, which treats a change only in case as a different account.
- *(group D)* **Forgetting a bot that stopped unexpectedly, or keeping it without an actor:** a runtime bug would silently stop a bot instead of ending in `CrashLoop`.
- *(group D)* **A validated `RuntimeConfig`, or an `apply` error for a zero chat interval:** the supervisor would still carry an error path that can't be reached, or report a config problem per bot.
- *(group D)* **Leaving the capacities unchecked** (the group B/C precedent): a caller's `RuntimeConfig` could make the library panic inside tokio.
- *(group D)* **Supervisor tests in `bot_actor.rs`, or a shared helper included by `#[path]` in several crates:** one oversized file, or a helper that's dead code wherever it isn't used.
- *(group D, Reset and Resume)* **A core event from Paused or Failed straight to Stopped:** no `AwaitingSession` detour, but `transition()`, its proptests and ADR-0010's event list would change.
- **Retrying or reconnecting on `ChatUnavailable`.** Retrying has unbounded latency; reconnecting lets chat failures drive reconnects.
- **fleet-mc refusing message-argument commands now.** A hard-coded vanilla list misses aliases and plugin overrides, and it belongs to P11.3's rule for user commands.
- **Restarting a crashed actor from Stopped.** Its next connect would skip the backoff, and a fresh breaker would forget an open circuit.
- **An agent state file for Paused and Failed.** File IO, a format and a writable volume on a read-only root, for a dev-only mode.
- **A shared `Mutex` registry or breaker.** CLAUDE.md prefers message passing, and a panic while the lock is held poisons it.
- **`TaskTracker`.** It doesn't report a panicked task by itself.
- **Following `PROPTEST_CASES` in the chaos test.** 256 locally is below the DoD's 500, and 1000 in CI may break the 30 s budget.
- **A core diagnostics port, or fleet-mc recording metrics itself.** More port surface, or a new fleet-mc dependency, for numbers the agent can read directly.
- **`metrics-util`'s `DebuggingRecorder`.** A new crate for what a small test recorder does.
- *(group E)* **A gauge that the supervisor recounts on a timer.** It can't drift, but it lags, and every paused-time test would see one more timer in the supervisor's `select!`. **Subtracting only on removal:** a fleet that has shut down would keep its last counts.
- *(group E)* **A crate-private publish function** next to the raw `watch::Sender`: a raw `send_replace` would still compile and skip the gauge. **One clonable type with an explicit `retire()`:** a path that forgets it would show only in the chaos test, and a `Drop` on the clones would count each ended actor out again.
- *(group E)* **Counting reconnects where the actor logs a session's end:** that counts disconnects, not connects, and misses crash restarts. **Only `attempt > 1`:** the fresh-token retry is the bot connecting on its own too.
- *(group E)* **Counting every stall the watchdog finds:** stalls that the drained events overtook ended no session.
- *(group E)* **Counting every crash, or only the restarts after the crash-loop start:** the first counts crashes rather than restarts; the second hides the last start.
- *(group E)* **`BotState::as_str()` in fleet-core:** the labels belong to the metrics. **A public `describe_metrics()` for the agent,** or no descriptions until P12.4: series would appear only once something happens, and the agent would carry one more step.
- *(group E, chaos test)* **Panics capped at 5 per bot:** the crash loop would never be reached. **A model of the restart window:** more test code to get right than a rule that allows `CrashLoop` from 6 crashes on. **Leaving out duplicate logins:** the main safety rule (§6 row 3) would go unchecked.
- *(group E, chaos test)* **Per-bot scripts in fleet-testkit:** its API would grow for one test. **Judging a bot by its own events:** a bug that fails a bot on a transient fault would excuse itself.
- *(group E, chaos test)* **A minimum gap between connects, or a count bound:** a restart that reset the attempt counter would pass. **Scaled-down settings:** the test would prove a policy nobody runs.
- *(group E, chaos test)* **The `farm` preset:** an attack every 650–800 ms is about 30 times the wakeups. **Probes only after each step:** the crash-teardown target was reached in about 2.5 % of cases. **No probes:** the test missed the crash gap entirely.
- *(group E)* **The test recorder in fleet-testkit:** a new dependency there. **A separate test crate for the metrics:** a second copy of the fleet setup. **A test hook to reach the crash-loop actor's crash:** a code path in the runtime only for tests.
- **The runtime reading the clock and the OS's randomness itself.** chrono's `clock` and rand's OS features would then unify into fleet-core's build.
