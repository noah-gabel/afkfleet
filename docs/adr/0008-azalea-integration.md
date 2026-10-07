# 0008. azalea integration

- **Status:** Accepted
- **Date:** 2026-10-05
- **Related:** Plan.md §6, Phase 1 (P1.1–P1.10), P2.3, P2.4, P2.7, P2.10, Phase 3, P4.3, P4.6; ADR-0003, ADR-0004. Evidence: `spikes/azalea/FINDINGS.md`; code: `spikes/azalea/src/`

## Context
`fleet-mc` will wrap azalea `0.16.0+mc26.1` (ADR-0003) behind the Minecraft ports, and `fleet-runtime` builds self-healing bots on top. Before writing either, the Phase 1 spike checked what azalea really does. Every result below was observed against a vanilla 26.1 server (`deploy/compose.dev.yaml`), most of them in a Linux container like production. The spike code is throwaway, but the snippets in this ADR are meant to be reused.

## Decision
### 1. How a bot is started: a hand-rolled `Client::join` ("Variant C")
There are three ways to start a client, and we use the third:

| | `Client::join` | `ClientBuilder` / `SwarmBuilder` | **Variant C** |
|---|---|---|---|
| Disable auto-reconnect and auto-respawn | ✗ (fixed plugin set) | ✓ | ✓ |
| Returns `(Client, events)` directly | ✓ | ✗ (only inside a handler) | ✓ |
| Learns how the ECS runner ended | ✗ | panic in `start()` | ✓ (`AppExit` receiver) |
| Teardown | `exit()` | **can self-deadlock** (azalea bug, below) | `exit()` |

Variant C rebuilds the ~20 lines of `Client::join` from public pieces. It relies on `#[doc(hidden)] start_ecs_runner`, which is acceptable because azalea is pinned exactly and the ADR-0003 bump procedure re-checks it:

```rust
// Runs on an MC host thread, inside its LocalSet (start_ecs_runner uses spawn_local).
let address = address.resolve().await?;                     // azalea::protocol::address::ResolvableAddr
let mut app = App::new();
app.add_plugins((
    DefaultPlugins,
    DefaultBotPlugins.build()
        .disable::<AutoReconnectPlugin>()
        .disable::<AutoRespawnPlugin>(),
));
single_threaded(&mut app);                                   // see §3
let (ecs, start_running_systems, appexit_rx) = start_ecs_runner(app.main_mut());
start_running_systems();
let (cb_tx, mut cb_rx) = mpsc::unbounded_channel();
ecs.write().write_message(StartJoinServerEvent {
    account,
    connect_opts: ConnectOpts { address, server_proxy: None, sessionserver_proxy: None },
    start_join_callback_tx: Some(cb_tx),
});
let entity = cb_rx.recv().await.ok_or(…)?;
let (event_tx, event_rx) = mpsc::unbounded_channel();
ecs.write().entity_mut(entity).insert(LocalPlayerEvents(event_tx));
let client = Client::new(entity, ecs);
// appexit_rx: Ok(AppExit) after exit(); Err(_) the moment the runner task dies (a panic).
```

> Refined by [ADR-0011](0011-fleet-mc-and-fleet-testkit-conventions-and-phase-3-refinements.md) (group D, found in CI on Linux): inserting `LocalPlayerEvents` after the join callback, as above, can lose the session's first events. azalea spawns the bot, polls the connect and reports a failed connect in the same frame, so a refused connect on a loaded Linux host was dropped. fleet-mc attaches the channel with an observer on `Add` of `LocalEntity` instead, the moment the bot is spawned. azalea's own `Client::join` has the same race.

### 2. Hosting model: one App and one MC host thread per bot
Every bot gets its own Bevy App/World (Variant C) **and its own host thread**.
- **Blast radius:** a panic or a hang affects exactly one bot.
- **Cost:** measured in Linux (P1.7, `--cpus 4`, steady state, single-threaded executor):

  | Bots | RSS | CPU | Threads |
  |---|---|---|---|
  | 10 | 57 MiB | 0.09 cores | 18 |
  | 25 | 116 MiB | 0.22 cores | 33 |
  | 50 | 222 MiB | 0.43 cores | 58 |

  Sharing 4 host threads would save only ~27 MiB and ~0.09 cores at 50 bots, so it isn't worth the larger blast radius. The rule written down before measuring was "the smallest blast radius that fits ≤ ~1.5 GiB and ≤ ~1 core at 50 bots".
- **Pool:** `McHostPool` becomes a *spawner*. A connect starts a dedicated host thread, and the thread ends when its session ends. A hung thread is abandoned, never joined.

- **Host thread:** a `std::thread` with a current-thread tokio runtime and a `LocalSet`. Work arrives as `Send` closures over a **bounded** queue, each runs in `spawn_local`, and results come back over a oneshot. Every azalea call runs on the bot's host thread. `Client` is `Send + Sync`, but each call takes the ECS lock, which is held for a whole Update or GameTick.
- **No Swarms.** One panic kills the whole shard, and a Swarm's teardown can deadlock.

### 3. Every App uses Bevy's single-threaded executor
azalea enables Bevy's multi-threaded executor, which runs all systems of all Apps on a **process-wide compute pool** (2 threads with 4 CPUs). Hanging 4 Apps on 4 separate host threads froze **every** bot in the process, including a healthy one on a fifth thread. With the single-threaded executor, systems run on the App's own host thread, so a hang stays there. It's also far cheaper: 50 bots on 4 host threads used **2.34 cores** with the multi-threaded executor and **0.34 cores** with the single-threaded one. Many small Apps dispatching tiny systems to a shared pool at 60 Hz is mostly overhead.

```rust
pub fn single_threaded(app: &mut App) {
    let mut schedules = app.world_mut().resource_mut::<Schedules>();
    for (_, schedule) in schedules.iter_mut() {
        schedule.set_executor_kind(ExecutorKind::SingleThreaded);
    }
}
```

### 4. Events: bounded bridge, ticks as a timestamp
- azalea's event channel is **unbounded**. A host-side pump forwards events into a bounded channel with `try_send` and counts what it drops.
  > Refined by [ADR-0010](0010-fleet-core-conventions-and-phase-2-refinements.md) (group E): only chat events may be dropped. `Joined`, `Died`, `Disconnected` and `ConnectionFailed` are always delivered.
- **`Tick`** (20 Hz) is never queued: it only updates an atomic "last tick" timestamp, which is all the watchdog needs.
- **`Packet` events** flood the channel (most of the drops). `fleet-mc` builds azalea with `default-features = false, features = ["online-mode"]`:
  - no `packet-event`
  - no `log`, because `log` adds Bevy's `LogPlugin` to every App and fights our tracing subscriber
- **`Init`** arrives before the TCP connect, so `ConnectionFailed` always follows it.
- **`Death`** can arrive twice (the combat-kill packet, then health reaching 0); de-duplicate it.
- After a disconnect or failed connect, the runner keeps running: every session ends with `exit()`.

### 5. Watchdog and timeouts (refines Plan.md §6 rows 1 and 6, P3.4, P4.6)
The ECS runner's `AppExit` receiver, a tick timestamp and a packet timestamp cover different faults:

| Fault | Seen as | Recovery |
|---|---|---|
| Panic in azalea or our systems | `appexit_rx` returns `Err` **at once** | Drop every handle of the bot: the World drops, the socket closes, and the server removes the player within ~1 s. Then reconnect via backoff. |
| Hang in a system | no Tick for `watchdog_timeout`, **and** the host thread's job queue stops answering | The host thread is lost (it can't be killed). Abandon it and reconnect the bot on a fresh thread. Count abandoned threads; above a small limit, exit the process and let Docker restart it. The server drops the zombie socket after its keepalive timeout (~30 s). |
| Frozen server or a silently dropped link | **no Tick stall**: ticks are client-side and kept going for 75 s | A **packet-liveness timeout**: no packet / `KeepAlive` (sent by the server every ~15 s) for 30 s while Online → tear down, transient disconnect |
| Unreachable address | `ConnectionFailed(TimedOut)` only after the OS's TCP timeout (21 s measured on Windows; Linux defaults are longer, not measured); azalea has **no connect timeout** | `fleet-mc` enforces its own connect timeout, then `exit()`. `exit()` cleanly cancels a connect in progress. |
| Unresolvable host | `ResolveError` from `resolve()` in milliseconds, no event | Map to `ConnectionFailed` |

> Refined by [ADR-0011](0011-fleet-mc-and-fleet-testkit-conventions-and-phase-3-refinements.md) (Phase 3 plan):
> - **The abandoned-thread limit** defaults to 3. Above it, fleet-mc refuses new host threads (`connect()` returns `HostUnavailable`), and the agent exits (P5.3).
> - **The connect timeout** runs from `connect()` until `Joined`, resolving included.

### 6. Disconnect reasons (input for P2.4)
Kick reasons arrive as `Event::Disconnect(Option<FormattedText>)`.
- **Classify by translation key, never by text.** The key is in `FormattedText::Translatable(t).key` and the args are in `t.args`.
- A `Text` reason (a custom kick message, or plugin-generated text) has no key and defaults to **transient**.

| Key | Args | Class |
|---|---|---|
| `multiplayer.disconnect.banned` | — | Permanent(Banned) |
| `multiplayer.disconnect.banned.reason` | `[reason]` | Permanent(Banned) |
| `multiplayer.disconnect.ip_banned` | — | Permanent(Banned) |
| `multiplayer.disconnect.banned_ip.reason` | `[reason]` | Permanent(Banned) |
| `multiplayer.disconnect.not_whitelisted` | — | Permanent(NotWhitelisted) |
| `multiplayer.disconnect.incompatible` | `[server version]` (both directions: newer and older server) | Permanent(WrongVersion) |
| `multiplayer.disconnect.duplicate_login` | — | **Conflict(DuplicateLogin)**: sent to the session that was already online, so a human logging in kicks the bot, and a bot reconnect would kick the human |
| `multiplayer.disconnect.unverified_username` | — | AuthInvalid (the server couldn't verify the session; e.g. an offline account on an online-mode server) |
| `multiplayer.disconnect.slow_login` | — | Transient, unless the account hook recorded an auth error first (§9) |
| `multiplayer.disconnect.server_full` | — | Transient |
| `multiplayer.disconnect.server_shutdown` | — | Transient |
| `multiplayer.disconnect.kicked` | — | Transient |
| `multiplayer.disconnect.idling` | — | Transient (the mode should prevent it, see §8) |
| `Disconnect(None)` (TCP closed) | — | Transient |
| `ConnectionFailed(Io)` | `ConnectionRefused`, `TimedOut`, … | Transient |

### 7. Chat (input for P2.3)
- **Kinds.** `Player`/`Disguised` packets carry a registry `ChatKind` (`chat_type.chat_type`): chat (0), emote (1), whisper incoming (2), say/announcement (4). That's the typed way to tell them apart. `System` packets (join/leave, command output, `tellraw`) have no sender.
- **Sender spoofing.** For `System` messages, azalea's `sender()` and `split_sender_and_content()` **guess the sender by regex on the plain text**: a `tellraw "<AfkBot7> …"` came out with sender `AfkBot7`. Take the sender only from `Player`/`Disguised` packets; `sender_uuid` exists only for `Player`.
- **Formatting.** Legacy `§` codes arrive parsed into styled siblings, and `to_string()` gives plain text without them. The core sanitizer still strips `§` and control characters.
- **Sending.** `chat("text")` sends a message, and `chat("/cmd")` or `write_command_packet("cmd")` sends a command. azalea **silently drops control characters and `§` and truncates to 256 characters**; the core `ChatMessage` limits match, so nothing gets changed silently. Offline accounts send unsigned chat.

### 8. Actions (input for P2.7, P3.6)
| `Action` | Call (on the host thread) |
|---|---|
| `Look{yaw,pitch}` | `set_direction(yaw, pitch)`, or `look_at(Vec3)` |
| `Jump` | `jump()` |
| `Sneak{on}` | `set_crouching(on)` |
| `SwingArm` | `ecs.write().trigger(SwingArmEvent { entity })` (no `Client` method) |
| `UseItem` | `start_use_item()` |
| `SelectHotbarSlot(n)` | `set_selected_hotbar_slot(n)`. **It panics for n ≥ 9**; the core type guarantees `0..=8` |
| `AttackFacingEntity` | `hit_result().as_entity_hit_result()` gives the target (the picker already respects reach, `entity_interaction_range` = 3.0), then **`attack_raw`** (below) |
| respawn | `ecs.write().write_message(PerformRespawnEvent { entity })` |

- **`Client::attack` is broken in azalea 0.16.0 against 26.1.** `ServerboundAttack.entity_id` lacks `#[var]`, so the server kicks the bot (`… ServerboundAttackPacket was larger than I expected, found 3 bytes extra`). The workaround writes the packet by hand:

  ```rust
  let entity_id = ecs.get::<EntityIdIndex>(client.entity)?.get_by_ecs_entity(target)?;
  let packet_id = ServerboundGamePacket::Attack(ServerboundAttack { entity_id }).id();
  let mut raw = Vec::new();
  write_var_int(&mut raw, packet_id);
  write_var_int(&mut raw, u32::from_ne_bytes(entity_id.0.to_ne_bytes()));  // VarInt of the i32 bits
  client.with_raw_connection_mut(|mut c| c.net_conn().map(|n| n.write_raw(&raw)));
  client.ecs.write().trigger(SwingArmEvent { entity: client.entity });
  // Also reset TicksSinceLastAttack, which attack() would have done.
  ```

  Report it upstream, and re-check it at every azalea bump.
- **Idle timer.** Rotating alone does **not** reset the server's idle timer, so a bot that only rotates gets kicked with `multiplayer.disconnect.idling`. Swing, jump, sneak and hotbar changes do reset it. The `afk` preset needs one of those more often than the server's idle timeout.

### 9. Accounts (input for P3.3, P4.3)
Agents get a Minecraft access token, UUID and name from the server, never a Microsoft token (security rule 6). So `fleet-mc` implements azalea's `AccountTrait`; `Account::from(impl AccountTrait)` wraps it.
- **Why not azalea's built-in account?** `Account::with_microsoft_access_token` needs a Microsoft token, which agents must never hold.
- **Trap: `join()` does nothing by default.** The default implementation returns `Ok` without doing anything. Ours calls `azalea_auth::sessionserver::join` with the token.
- **Trap: certificates.** `certs()`/`set_certs()` must store them; chat signing `expect`s them.
- **`Debug` is hand-written** and redacts the token. azalea's `MicrosoftAccount` *derives* `Debug` over its token, so never Debug-print an azalea `Account`.
- **Auth failures produce no event.** When the session server rejects the token, azalea calls `refresh()` **once**, retries `join()`, then only *logs* the error. The connection then hangs in the login phase until the server gives up ~30 s later, with `multiplayer.disconnect.slow_login` or a bare `Disconnect(None)`. So the account itself records the `ClientSessionServerError` kind from `join()`, and `fleet-mc` turns it into `AuthInvalid` and calls `exit()` straight away.
  > Refined by [ADR-0011](0011-fleet-mc-and-fleet-testkit-conventions-and-phase-3-refinements.md): only `InvalidSession` and `ForbiddenOperation` mean a rejected token. `Banned` and `MultiplayerDisabled` get a permanent reason; an outage, rate limit or timeout gets a transient one. `join()` runs on Bevy's IO pool, not on the host thread.
- **`refresh()`** is where P4.3's "refresh once" goes: ask the `SessionCredentialProvider` for one fresh token and swap it in. If none comes, return an error.
  > Superseded by [ADR-0010](0010-fleet-core-conventions-and-phase-2-refinements.md): the "request one fresh token" now lives in the core state machine, so `refresh()` fails fast.
  > Refined by [ADR-0011](0011-fleet-mc-and-fleet-testkit-conventions-and-phase-3-refinements.md): `refresh()` is a no-op that returns `Ok(())`. Every error it could return is a Microsoft-flow `AuthError`, which azalea would log at error level as if it were true.

```rust
impl AccountTrait for ExternalTokenAccount {
    fn username(&self) -> &str { &self.name }
    fn uuid(&self) -> Uuid { self.uuid }
    fn access_token(&self) -> Option<String> { Some(self.token.expose_secret().to_owned()) }
    fn refresh(&self) -> BoxFuture<'_, Result<(), AuthError>> { /* ask the provider once */ }
    fn certs(&self) -> Option<Certificates> { self.certs.lock().clone() }
    fn set_certs(&self, certs: Certificates) { *self.certs.lock() = Some(certs); }
    fn join<'a>(&'a self, public_key: &'a [u8], private_key: &'a [u8; 16], server_id: &'a str,
                proxy: Option<reqwest::Proxy>) -> BoxFuture<'a, Result<(), ClientSessionServerError>> {
        Box::pin(async move {
            let r = sessionserver::join(SessionServerJoinOpts {
                access_token: self.token.expose_secret(), public_key, private_key,
                uuid: &self.uuid, server_id, proxy,
            }).await;
            if let Err(e) = &r { self.record_auth_error(e); }   // the only signal we get
            r
        })
    }
}
```

The garbage-token and offline cases were verified against a local online-mode server (`spikes/azalea/compose.online.yaml`). The user also ran the optional real-account join against that server on 2026-10-06 (spike README). The bot spawned, its chat message was accepted while the server enforced secure profiles, and `exit()` disconnected it cleanly. So `join()`, certificate storage and chat signing work with a real token.

### 10. Teardown and leaks
**A session ends in this order:**
1. `client.exit()`
2. wait for `appexit_rx`, with a short timeout
3. drop every `Client` clone and the event receiver
4. close the session's host thread

Closing the thread drops its `LocalSet`, which drops azalea's runner task and with it the World. So even a bot whose `exit()` got lost, or one that only ever saw `disconnect()`, is freed.

- **`disconnect()` alone isn't a teardown.** It closes the connection, but the ECS runner keeps going and the World stays alive.
- **Verified in Linux over 20 cycles of 25 bots (P1.9):**
  - threads (8) and fds (16) return exactly to the post-warm-up baseline in every cycle
  - every bot's World is freed (checked through a `Weak` reference)
  - all host threads end, and the server lists no leftover players
- **Memory.** RSS after teardown plateaus at ~110 MiB from cycle ~9 on: allocator fragmentation, not a leak. glibc doesn't hand freed memory back to the OS. `MALLOC_ARENA_MAX=2` made it worse, so we don't set it.
- **One-time cost.** The first join creates Bevy's process-wide task pools (compute, IO, async compute) and `async-compat`'s runtime thread. They stay for the life of the process.

### 11. Open questions: answered or deferred
| Question (Plan.md Phase 1) | Status |
|---|---|
| Is `Client` `Send`, usable from other threads? (P1.2) | **Answered**: `Send + Sync + Clone`; usable, but calls stay on the host thread |
| Build a client without auto-reconnect and auto-respawn? (P1.3) | **Answered**: Variant C (§1) |
| What do failures look like, and are translation keys available? (P1.3) | **Answered**: §5, §6 |
| Chat kinds, sender, plain text, sending (P1.4) | **Answered**: §7 |
| Actions, swing API, reach check (P1.5) | **Answered**: §8; `attack()` needs the workaround |
| What does a panic do to the client, its events, and other clients? (P1.6) | **Answered**: §3, §5 (hangs added) |
| Per-bot App vs Swarm shards (P1.7) | **Answered**: §2 |
| Custom `AccountTrait` with external token and cert storage (P1.8) | **Answered**: compile-checked, verified with a garbage token, and verified by the user with a real account and signed chat on 2026-10-06 (§9) |
| Clean disconnect, no leaks (P1.9) | **Answered**: §10 |
| CPU without the `packet-event` feature | **Deferred to P3**: measured with it, so the numbers are an upper bound |
| How many abandoned (hung) host threads before restarting the process | **Deferred to P4.6/P4.7**: a policy value, not an azalea question |
| Report the attack-packet and Swarm-deadlock bugs upstream | **Deferred to the user**: outside this repository |

> Updated by [ADR-0011](0011-fleet-mc-and-fleet-testkit-conventions-and-phase-3-refinements.md) (Phase 3 plan):
> - **CPU without `packet-event`** is closed without a measurement: fleet-mc builds without the feature, so the cost can only drop below these numbers.
> - **The abandoned-thread limit** defaults to 3 (Plan.md Appendix A, `max_abandoned_threads`).

## Consequences
- **Gotchas `fleet-mc` must handle:**
  - `Client` getters (`username()`, `position()`, …) **panic** when a component is missing: before login, and after `exit()`. Keep the name ourselves, and only call getters while the bot is known to be in a world.
  - Never hold an ECS guard (`component()` returns one) while calling another `Client` method on the same thread: `parking_lot` locks aren't reentrant. azalea itself does exactly this in `SwarmBuilder::start`: its client-handler loop calls `username()` while holding `ecs.write()`, which deadlocks when an event arrives after `exit()` cleared the World. That's the reason we avoid `ClientBuilder` and `SwarmBuilder`; report it upstream.
  - azalea relies on `#[doc(hidden)] start_ecs_runner`, `RawConnection::write_raw` and the plugin list. All are re-checked at every azalea bump (ADR-0003 step 5).
- **Hangs can't be contained inside the process,** only bounded (a single-threaded executor, per-host-thread blast radius, abandoned-thread limit, process restart). If production shows frequent hangs, the process-per-bot connector stays the escape hatch (ADR-0004).
- **Plan changes this implies** (accepted in the Phase 1 review and applied to Plan.md in the same PR):
  - P2.4: the key table.
  - P2.7: the `afk` preset and the idle timer.
  - P2.10 / P3.5: `SessionEvent` needs a packet-liveness signal.
  - P3.2: `McHostPool` becomes a spawner of one host thread per session (abandon on hang) instead of N threads with least-loaded placement.
  - P3.4: Variant C, the single-threaded executor, connect timeout, `attack_raw`.
  - P4.6: the watchdog refinements in §5.

## Alternatives considered
- **`Client::join`.** Auto-reconnect and auto-respawn can't be turned off (respawn not at all), and it hides how the runner ended.
- **`ClientBuilder` (one per bot).** It can turn off the plugins, but `start()` blocks until `exit()`, the `Client` only exists inside a per-event handler task, and teardown can self-deadlock.
- **Swarm shards.** The cheapest in memory: 59 MiB for one Swarm of 50, against 222 MiB. But one panic stops the whole shard, and teardown can deadlock (P1.2, P1.6). Shards of 10 with the default executor also cost 0.96 cores.
- **Bevy's default multi-threaded executor.** Hangs starve the process-wide pool and freeze every bot, and it costs ~7× the CPU.
- **A small pool of shared host threads** (ADR-0004's original design). It costs about the same, but a hang freezes every bot on the thread.
