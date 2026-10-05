# Findings (Phase 1 azalea spike)

Short results per task. ADR-0008 is the curated record; this file keeps the evidence behind it. All runs used azalea `0.16.0+mc26.1`, `nightly-2026-08-21`, and the dev server from `deploy/compose.dev.yaml` (`itzg/minecraft-server:2026.9.2-java25`, vanilla 26.1, offline mode).

## P1.1 Local test server
- `just mc-up` pulls the pinned image, starts vanilla 26.1 and is healthy after about 40 s on the first run (the server jar is downloaded once per world volume).
- Only `127.0.0.1:25565` is published. RCON listens inside the container; `docker compose … exec minecraft rcon-cli <cmd>` works without knowing the random password.
- `LEVEL_TYPE=minecraft:flat` works. The log shows `ERROR: No key layers in MapLike[{}]` because itzg writes `generator-settings={}`, but the default flat preset is used anyway: grass at y −61 and bedrock at y −64 (checked with `execute if block`).
- **In Git Bash, set `MSYS_NO_PATHCONV=1`** before `docker compose exec … <path>`. Otherwise `/data/…` gets rewritten to a Windows path.
- `cargo run -- join 15`: `AfkBot1` logs in and spawns at `(2.5, -60.0, 5.5)`, and `rcon-cli list` shows `AfkBot1`. After `exit()` the process ends.

## P1.2 A client on a dedicated MC host thread
Setup (`src/host.rs`, `src/session.rs`): one OS thread with a current-thread runtime and a `LocalSet`; jobs arrive as `Send` closures over a bounded queue, run as `spawn_local`, and reply over a oneshot. Three ways to start a bot on it:

| Variant | How | Plugins | Gives us |
|---|---|---|---|
| A `join` | `Client::join` | fixed set: auto-reconnect and auto-respawn **on** | `(Client, UnboundedReceiver<Event>)` |
| B `builder` | `ClientBuilder` + handler that hands out the `Client` on `Init` | our choice | `Client` via handler; `start()` future |
| C `custom` | `Client::join`'s 20 lines rebuilt from public pieces: `App` + our plugins → `start_ecs_runner` (`#[doc(hidden)]`) → `StartJoinServerEvent` → `LocalPlayerEvents(tx)` → `Client::new` | our choice | `(Client, UnboundedReceiver<Event>, oneshot::Receiver<AppExit>)` |

Results (`cargo run -- host <variant> 3`, then `exit-race`, `host-builder-exit`):
- **`Client` is `Send + Sync + Clone`** (compile-time assert). `set_direction` and `position()` called from a tokio *worker* thread work, and the host thread sees the new direction. We still keep azalea calls on host threads (CLAUDE.md): every call takes the ECS `RwLock`, and that lock is held for a whole Update/GameTick.
- **The handoff works** for all three: three bots on one host thread, ~20 ticks/s each; the oneshot delivers the client to the multi-threaded runtime.
- **Nested `LocalSet` works.** B's `start()` creates its own `LocalSet` inside our host task, and three builders on one host thread run fine.
- **`exit()` tears down A and C cleanly.** The runner returns `AppExit::Success`, the World is cleared (0 entities), and the event channel closes. With C, `exit()` from another thread worked on the first call in 12 of 12 trials.
- **B (and therefore every `Swarm`) can deadlock on teardown. This is an azalea bug.**
  - Where: in `SwarmBuilder::start`, the client-handler loop holds `ecs.write()` and, when an event arrives for a bot whose state component is gone (the World was just cleared by `exit()`), logs `first_bot.username()`. `username()` reads the same ECS on the same thread, and `parking_lot` locks aren't reentrant: **self-deadlock** (`azalea/src/swarm/builder.rs` ~555–580).
  - Effect: the host thread hangs forever while holding the write lock, so any later `exit()` or getter from another thread hangs too.
  - Frequency: a race. In `host-builder-exit`, `start()` didn't return in 2 of 4 runs where `exit()` came from another thread, and 1 of 2 even without nesting; `exit-race builder` hung on its first trial.
- **Getters panic outside the game.** `client.username()` panics (`Our client is missing a required component: GameProfileComponent`) before login and after `exit()`. Keep the name ourselves; call getters only while the bot is known to be in a world.
- **`packet-event` floods the channel.** With nobody reading for 3 s, 61 events were dropped from a 256-slot channel; nearly all were `Event::Packet`. The bridge now counts packets and only forwards them on request. `fleet-mc` should build azalea without `packet-event`.
- `Init` is sent as soon as `LocalPlayerEvents` is inserted, **before** the TCP connect, so `ConnectionFailed` always comes after `Init`.
- tokio's `num_alive_tasks` doesn't count `LocalSet` tasks; our own counter only sees our jobs, not azalea's internal `spawn_local`s.

**Conclusion:** Variant C. It disables plugins like B, avoids B's teardown deadlock, and is the only variant that reports how the runner ended (`AppExit`, or the sender dropped on a panic). The cost is relying on `#[doc(hidden)] start_ecs_runner`, acceptable because azalea is pinned exactly and the ADR-0003 bump procedure re-checks it.

## P1.3 Without auto-reconnect/respawn, and what failures look like
**Plugins off.** Variant C (`DefaultBotPlugins` with `AutoReconnectPlugin` and `AutoRespawnPlugin` disabled) works:

| Check | C (plugins off) | A `Client::join` (plugins on) |
|---|---|---|
| 10 s after a kick | nothing happens | `Login` + `Spawn` again after 5.3 s; **no new `Init`** |
| 10 s after `/kill` | still dead (health 0) | respawned (health 20) |

`Client::join` can't do it: a per-entity `AutoReconnectDelay(Duration::MAX)` could stop reconnecting, but nothing turns off auto-respawn.

**After a disconnect or failed connect, the ECS runner keeps running** (`runner_ended=false` 8–10 s later), so every session must end with `exit()`. `exit()` also cleanly cancels a connect that's still pending (blackhole case): the runner returns `Success` and the event channel closes.

**Failure catalogue** (`cargo run -- fail <scenario>`). All kick reasons arrive as `Event::Disconnect(Some(FormattedText))`. A translatable reason carries a stable key, and some carry args:

| Scenario | Event | Variant | Key | Args | Plain text |
|---|---|---|---|---|---|
| Closed port (`127.0.0.1:25599`) | `ConnectionFailed`, after 2.1 s on Windows | — | — | `io::ErrorKind::ConnectionRefused` (os error 10061) | |
| Blackhole (`192.0.2.10`) | `ConnectionFailed`, after **21 s** (Windows TCP default; azalea has **no connect timeout**) | — | — | `TimedOut` (10060) | |
| Unresolvable (`nonexistent.invalid`) | no event: `resolve()` returns `ResolveError` in ~5 ms | — | — | | |
| `kick AfkBot1 <reason>` | `Disconnect` | **Text** | none | — | the operator's reason |
| `kick AfkBot1` | `Disconnect` | Translatable | `multiplayer.disconnect.kicked` | — | Kicked by an operator |
| `ban` (while online) | `Disconnect` | Translatable | `multiplayer.disconnect.banned` | — | You are banned from this server |
| Joining while banned | `Disconnect` (login phase) | Translatable | `multiplayer.disconnect.banned.reason` | `[reason]` | You are banned from this server.\nReason: … |
| `ban-ip` (while online) | `Disconnect` | Translatable | `multiplayer.disconnect.ip_banned` | — | You have been IP banned from this server |
| Joining while IP-banned | `Disconnect` (login) | Translatable | `multiplayer.disconnect.banned_ip.reason` | `[reason]` | Your IP address is banned … |
| Not whitelisted | `Disconnect` (login) | Translatable | `multiplayer.disconnect.not_whitelisted` | — | You are not white-listed on this server! |
| Duplicate login | `Disconnect` to the **already-online** session; the newcomer gets in | Translatable | `multiplayer.disconnect.duplicate_login` | — | You logged in from another location |
| Server full | `Disconnect` (login) | Translatable | `multiplayer.disconnect.server_full` | — | The server is full! |
| Server newer (26.3) | `Disconnect` (login) | Translatable | `multiplayer.disconnect.incompatible` | `["26.3"]` | Incompatible client! Please use 26.3 |
| Server older (1.21.11) | `Disconnect` (login) | Translatable | `multiplayer.disconnect.incompatible` | `["1.21.11"]` | Incompatible client! Please use 1.21.11 |
| Idle timeout (`setidletimeout 1`) | `Disconnect` after 60 s of standing still | Translatable | `multiplayer.disconnect.idling` | — | You have been idle for too long! |
| `stop` | `Disconnect` | Translatable | `multiplayer.disconnect.server_shutdown` | — | Server closed |
| `docker kill` (TCP gone) | `Disconnect(None)` immediately | — | — | — | |
| `docker pause` for 75 s (server frozen, TCP open) | **nothing**: no event, and **ticks continued** (1,501 = 20/s); `Disconnect(None)` only once the server was unpaused (its own watchdog had crashed it) | — | — | — | |
| `/kill` (death) | `Chat` (`death.attack.genericKill`), then **`Death` twice** (packet, then health 0) | | | | |

Consequences:
- **A Tick watchdog can't see a dead server.** Ticks are client-side, so a frozen server or a silently dropped link (no RST) leaves the bot "Online" forever. azalea has no read timeout. fleet-mc also needs a **packet-liveness timeout**: vanilla servers send a KeepAlive every ~15 s (`Event::KeepAlive`), and vanilla clients give up after 30 s.
- **fleet-mc must enforce its own connect timeout** (P3.4) and call `exit()` when it fires; the OS timeout is 21 s on Windows and much longer on Linux.
- **Classifier input (P2.4):** classify by translation key, never by text. A `Text` reason (custom kick message, or plugin-generated text) has no key and should default to transient. `banned*`, `ip_banned`, `banned_ip.*`, `not_whitelisted` and `incompatible` are permanent; `duplicate_login` is the conflict case; `server_full`, `server_shutdown`, `kicked`, `idling` and `Disconnect(None)` look transient.
- `Death` must be de-duplicated when mapping to a `Died` session event.

## P1.4 Chat
`cargo run -- chat`: AfkBot1 listens, AfkBot2 talks, RCON plays the console (offline server, `enforce-secure-profile=false`).

| Case | `ChatPacket` | Registry `ChatKind` | azalea `sender` | `sender_uuid` | `is_whisper` | Key of `message()` | Plain text |
|---|---|---|---|---|---|---|---|
| Player chat | `Player` (unsigned) | id 0 (`chat`) | `AfkBot2` | the player's UUID | false | `chat.type.text` | `<AfkBot2> hello …` |
| `/msg AfkBot1 …` | `Disguised` | id 2 (`msg_command_incoming`) | `AfkBot2` | **None** | true | `commands.message.display.incoming` | `AfkBot2 whispers to you: …` |
| `/me waves` | `Disguised` | id 1 (`emote_command`) | `AfkBot2` | None | false | `chat.type.emote` | `* AfkBot2 waves` |
| RCON `say` | `Disguised` | id 4 (`say_command`) | `Rcon` | None | false | `chat.type.announcement` | `[Rcon] …` |
| RCON `tell AfkBot1 …` | `Disguised` | id 2 | `Rcon` | None | true | `commands.message.display.incoming` | `Rcon whispers to you: …` |
| Join / leave | `System` | — | None | None | false | `multiplayer.player.joined` / `.left`, args `[name]` | `AfkBot2 joined the game` |
| `/list` reply | `System` | — | None | None | false | `commands.list.players` | `There are 2 of a max of 60 …` |
| `tellraw` text | `System` | — | None | None | false | none (`Text`) | as sent |
| `tellraw "<Notch> I am not really Notch"` | `System` | — | **`Notch`** (spoofed) | None | false | none | `<Notch> I am not …` |

- **Kinds:** `Player`/`Disguised` carry a registry `ChatKind` (`chat_type.chat_type`), a typed, reliable way to tell chat, emote, whisper and announcement apart. `System` carries no sender at all.
- **Sender spoofing.** For `System` messages azalea's `sender()`/`split_sender_and_content()` guess the sender **by regex on the plain text**. Anything that can produce a system message (a `tellraw`, a plugin, a bridge bot) can fake `<Notch> …`. P2.3 must take the sender only from `Player`/`Disguised` packets (`chat_type.name`) and treat `System` text as sender-less.
- **Formatting.** Legacy `§` codes inside a component arrive already parsed into styled siblings, so `to_string()` has no `§`. The core sanitizer must still strip `§` and control characters (defense in depth, P2.3).
- **Sending:**
  - `chat("text")` sends a chat message. `chat("/list")` sends a command, and `write_command_packet("list")` does the same without the slash; both got the `/list` reply.
  - azalea **filters control characters and `§` and silently truncates to 256 characters** before sending: `a§cb\x07c\td` arrived as `acbcd`, and 300 × `x` arrived as 256 characters. No kick. The core `ChatMessage` limits (≤ 256, no control characters, no `§`, P2.2) match, so nothing is silently changed.
  - Offline accounts send unsigned chat (`signed=false`), which a server with `enforce-secure-profile=false` accepts.

## P1.5 Actions
`cargo run -- actions` and `cargo run -- idle-actions`. Every call runs on the host thread through the job channel and is checked against the server via RCON where possible.

| Action | azalea 0.16.0 call | Check | Result |
|---|---|---|---|
| Look | `set_direction(yaw, pitch)`; `look_at(Vec3)` | `direction()`; RCON `data get entity AfkBot1 Rotation` | `[45.0f, -10.05f]` on the server (pitch is quantized) |
| Jump | `jump()` (one jump; `set_jumping(bool)` = hold) | y offset per tick | rises to +1.25 and lands after 10 ticks |
| Sneak | `set_crouching(bool)` | `crouching()` | true |
| Swing | no `Client` method: `ecs.write().trigger(SwingArmEvent { entity })` | other bot receives `ClientboundAnimate` | `SwingMainHand` |
| Hotbar | `set_selected_hotbar_slot(u8)`; **panics if ≥ 9** (`assert!`) | RCON `execute if items entity AfkBot1 weapon.mainhand minecraft:stick` | Test passed |
| Use item | `start_use_item()` (main hand) | snowball count via RCON `clear AfkBot1 minecraft:snowball 0` | 16 → 15 |
| Target in view | `hit_result()` → `HitResult::Entity(EntityHitResult { entity, .. })` (the bot's ECS `Entity`); updates one tick after a look | pig 2 blocks ahead / 6 blocks ahead | entity at 2 blocks; **not** at 6 blocks: the picker respects reach |
| Reach | `attributes().entity_interaction_range.calculate()` | | 3.0 |
| Attack | `attack(entity)`: **broken, gets the bot kicked** (see below). Workaround `attack_raw` | pig Health via RCON | 10.0 → 9.0 with the workaround |
| Respawn | no `Client` method: `ecs.write().write_message(PerformRespawnEvent { entity })` | `health()` | 0.0 → 20.0 |

- **`attack()` is broken in azalea 0.16.0 against vanilla 26.1.** `ServerboundAttack { entity_id: MinecraftEntityId }` lacks `#[var]`, so the id goes out as a 4-byte int where the server expects a VarInt. The server disconnects the bot with `Internal Exception: io.netty.handler.codec.DecoderException: … ServerboundAttackPacket was larger than I expected, found 3 bytes extra`. Every attack kicks the bot.
  - **Workaround (`attack_raw` in `src/exp/actions.rs`):** look up the target's `MinecraftEntityId` in the bot's `EntityIdIndex`, write `VarInt(packet id) + VarInt(entity id)` with `RawConnection::net_conn().write_raw`, then trigger `SwingArmEvent`. It works (damage dealt, no kick). Unlike `attack()`, it doesn't reset azalea's `TicksSinceLastAttack`, so `has_attack_cooldown()` stays false; fleet-mc should reset that component too, or rely on the core's attack interval (≥ 500 ms).
  - Worth reporting upstream; the next azalea bump should re-check it.
- **Idle timer** (`setidletimeout 1`, one action every 20 s per bot, 100 s):

  | Action | Kicked for idling? |
  |---|---|
  | none | yes |
  | rotate (`set_direction`) | **yes**: rotating alone doesn't count as activity |
  | swing | no |
  | jump | no |
  | sneak toggle | no |
  | hotbar change | no |

  So an anti-AFK mode needs a swing, jump, sneak or hotbar change more often than the server's idle timeout. The `afk` preset (P2.7) as written ("a random small rotation every 45–120 s, an occasional jump, and a swing every few minutes") only survives if the jump or swing comes often enough.
- `set_selected_hotbar_slot` and the attack workaround take the ECS write lock briefly; all actions are cheap and belong on the host thread.

## P1.6 Panics and hangs inside the ECS (Linux container)
`linux.sh fault <mode> [st]`: a custom `GameTick` system panics or loops forever once a shared flag is set. Bots: AfkBot1–3 on host thread 0, plus a **witness** (AfkBot9) on host thread 1. The witness also tells us when the server drops AfkBot1 (it sees AfkBot1 leave the tab list). Container: `--cpus 4`, so Bevy's process-wide pools are IO 1, async compute 1, **compute 2** threads. `st` switches every App to Bevy's single-threaded executor.

| Run | Faulty bot | Other bots, same host thread | Witness (other host thread) | Host thread | How we learn about it | Server drops the faulty bot |
|---|---|---|---|---|---|---|
| (a) App per bot, **panic** | ticks stop; event channel **stays open** | keep ticking | keeps ticking | survives, responds | `appexit_rx` fails **at once** ("runner task died") | not within 20 s; **~1 s after we drop every handle** (World dropped → socket closed) |
| (b) **Swarm** of 3, panic | all 3 stop | — (same shard) | keeps ticking | survives | `start()` panics (azalea's `expect` at `swarm/builder.rs:597`) | ~28 s (server keepalive timeout) |
| (c) App per bot, **hang** | ticks stop | **freeze too** (host thread blocked) | keeps ticking | **blocked forever** (jobs time out) | only the Tick watchdog | ~29 s (keepalive timeout); dropping handles doesn't help, the stuck runner holds the World |
| (d) **starve**: one hang per compute-pool thread, each on its own host thread | all stop | — | **freezes too** | witness's host thread blocked | only the Tick watchdog | — |
| (a) + `st`, panic | ticks stop | keep ticking | keeps ticking | survives (panic is caught by tokio on the host thread) | `appexit_rx` fails at once | ~1 s after dropping every handle |
| (c) + `st`, hang | ticks stop | freeze (host thread blocked) | keeps ticking | blocked forever | Tick watchdog | ~29 s |
| (d) + `st`, starve | all stop | — | **keeps ticking** | witness fine | Tick watchdog | ~29 s |

Conclusions:
- **Panics are contained to one App** (per-bot Apps, with either executor) and are detected immediately through the runner's `AppExit` receiver, so the Tick watchdog isn't needed for them. Recovery: drop every handle of that bot (World → socket closes, the server removes the player within a second), then reconnect through the normal backoff path. Without the drop, the bot stays a zombie on the server until its keepalive timeout.
- **A Swarm shard dies as a whole** on one panic, and its teardown can deadlock (P1.2).
- **Hangs are the dangerous case.**
  - They block the whole host thread, and nothing in-process can free it.
  - With Bevy's default multi-threaded executor, systems run on a **process-wide compute pool** (2 threads here), so a couple of hangs anywhere freeze **every bot in the process**.
  - The **single-threaded executor** fixes the second part: systems run on the App's own host thread, and a hang stays on that thread.
- **What the watchdog has to do:**
  - Tick stalled while Online, on a host thread whose job queue no longer responds: the host thread is hung. Abandon it (it can't be joined or killed), move its other bots to a fresh host thread (they reconnect), and count abandoned threads.
  - Each abandoned thread keeps its bots' sockets until the server's keepalive timeout (~30 s), and the thread itself leaks. Above a small limit, restart the agent process (Docker `restart: unless-stopped`).
- The `par_iter_mut` in azalea-entity still uses the compute pool under `st`, but Bevy's scope lets the calling thread run those tasks, and the starve run showed no stall.

## P1.7 Resources: hosting models (Linux container)
**Decision rule (written before measuring):**
1. Shards (Swarms) are only acceptable if per-bot isolation fails the budget. P1.6 showed one panic kills a whole shard, and P1.2 showed Swarm teardown can deadlock.
2. Among per-bot models, prefer the smallest blast radius (`an-st` > `a4-st` > `a1-st`) whose **50-bot** steady state fits a small VPS: **RSS ≤ ~1.5 GiB** and **CPU ≤ ~1 core**, with ticks at ~20/s per bot.
3. If two models are within ~20 % of each other, the smaller blast radius wins.

Method: `linux.sh scale <model> <bots>`, one fresh process per data point, `--cpus 4`.
- Joins are staggered 250 ms apart. After every bot has spawned, a 30 s warm-up, then a 60 s window: RSS sampled every 5 s; CPU from utime+stime over the window (`getconf CLK_TCK`); threads and fds at the end.
- Built with `packet-event` (the bridge drops packet events), so CPU is an upper bound for a fleet-mc built without it.
- `docker stats` sampled the server and spike containers every 15 s alongside.

Results (steady state, Linux container `--cpus 4`, 20.0 ticks/s per bot in every run, no dropped events; baseline process before joining: 9 MiB, 5 threads):

| Model | Bots | Join (s) | RSS avg (MiB) | RSS per bot (MiB) | CPU (cores) | CPU per bot | Threads | fds |
|---|---|---|---|---|---|---|---|---|
| `a1-st` | 10 | 2.6 | 54 | 4.5 | 0.070 | 0.70 % | 9 | 28 |
| `a4-st` | 10 | 2.6 | 55 | 4.7 | 0.081 | 0.81 % | 12 | 40 |
| `an-st` | 10 | 2.6 | 57 | 4.8 | 0.089 | 0.89 % | 18 | 64 |
| `a4` (multi-threaded executor) | 10 | 2.6 | 57 | 4.8 | **1.601** | 16.0 % | 12 | 40 |
| `s10` | 10 | 2.6 | 30 | 2.2 | 0.214 | 2.14 % | 9 | 28 |
| `s` | 10 | 2.5 | 30 | 2.2 | 0.214 | 2.14 % | 9 | 28 |
| `a1-st` | 25 | 6.4 | 104 | 3.8 | 0.153 | 0.61 % | 9 | 43 |
| `a4-st` | 25 | 6.4 | 105 | 3.9 | 0.176 | 0.70 % | 12 | 55 |
| `an-st` | 25 | 6.4 | 116 | 4.3 | 0.215 | 0.86 % | 33 | 139 |
| `a4` | 25 | 6.7 | 109 | 4.0 | **2.152** | 8.6 % | 12 | 55 |
| `s10` | 25 | 6.1 | 49 | 1.6 | 0.580 | 2.32 % | 11 | 51 |
| `s` | 25 | 6.6 | 39 | 1.2 | 0.237 | 0.95 % | 9 | 43 |
| `a1-st` | 50 | 12.8 | 193 | 3.7 | 0.292 | 0.58 % | 9 | 68 |
| `a4-st` | 50 | 12.8 | 195 | 3.7 | 0.336 | 0.67 % | 12 | 80 |
| **`an-st`** | 50 | 12.8 | **222** | 4.3 | **0.425** | 0.85 % | 58 | 264 |
| `a4` | 50 | 16.1 | 204 | 3.9 | **2.338** | 4.7 % | 12 | 80 |
| `s10` | 50 | 12.2 | 82 | 1.5 | 0.956 | 1.91 % | 13 | 84 |
| `s` | 50 | 13.2 | 59 | 1.0 | 0.266 | 0.53 % | 9 | 68 |

The server (vanilla 26.1, view distance 4) used ~18 % of one core on average during the 50-bot runs (peak 34 %), with ~1.9 GiB of memory.

- **The multi-threaded executor is the expensive part:** 1.6–2.3 cores for 10–50 bots, against 0.07–0.34 cores for the same bots with the single-threaded executor (≈ 7× at 50 bots). Many small Apps each dispatching their systems to the shared compute pool at 60 Hz costs far more than running them inline. `s10` (5 multi-threaded Apps) shows the same effect; one big Swarm (`s`) amortizes it.
- **Memory:** about 3.7–4.3 MiB per bot for an App per bot, 1–2 MiB per bot in a Swarm. Host threads add little: `an-st` is +27 MiB and +46 threads over `a4-st` at 50 bots.
- **Threads:** process-wide Bevy pools (compute 2, IO 1, async compute 1, plus `async-compat`) and 2 tokio workers, plus one per host thread. No per-bot threads appear unless we create them.

**Choice (by the rule above): `an-st`, one App and one host thread per bot, with the single-threaded executor.** It has the smallest blast radius: a panic or a hang affects exactly one bot. At 50 bots it uses 222 MiB and 0.43 cores, far inside the budget (≤ 1.5 GiB, ≤ 1 core). Swarms would save ~160 MiB at 50 bots, but they fail the isolation requirement (P1.6) and can deadlock on teardown (P1.2).

## P1.8 Custom `AccountTrait` with an externally supplied token
`ExternalTokenAccount` (`src/exp/account.rs`) holds name, UUID and a Minecraft access token, the shape an agent gets from the server. It compiles against azalea 0.16.0. The trait signatures need `uuid::Uuid` and `reqwest::Proxy`, which resolve to the same crates azalea uses.
- **The trait has two traps.** The default `join()` does **nothing** (it returns `Ok`), so a custom account must call `azalea_auth::sessionserver::join` itself, or online-mode joins fail. And `certs`/`set_certs` must actually store the certificates: chat signing does `account.certs().expect(…)` once a `ChatSigningSession` exists.
- **`Debug` must be hand-written.** azalea's own `MicrosoftAccount` *derives* `Debug` over its access token, and `Account` is `Debug`, so Debug-printing an azalea `Account` can leak a token. Ours prints `token: "<redacted>"`.
- **`refresh()`** is called by azalea **once** after `InvalidSession`/`ForbiddenOperation`, then `join` is retried. That's the natural hook for P4.3's "ask the control plane for one fresh token".

`cargo run -- account-check` against the local online-mode server (`compose.online.yaml`, no real credentials):

| Case | What happens |
|---|---|
| `ExternalTokenAccount` with a garbage token | Session server: `ForbiddenOperation` → our `refresh()` (1×) → retry fails → azalea only **logs** `Error during authentication: SessionServer(ForbiddenOperation)`, **no event**. The connection then hangs in the login phase until the server gives up after **~30 s**: `Disconnect(multiplayer.disconnect.slow_login)` in one run, `Disconnect(None)` in another. Only our account's recorded `last_join_error = ForbiddenOperation` says "the token is bad". |
| `Account::offline` against online mode | `Disconnect(multiplayer.disconnect.unverified_username)` ("Failed to verify username!") after ~0.25 s |

So fleet-mc must take auth failures from the account hook (`join()` errors), immediately call `exit()` and report `AuthInvalid`, instead of waiting ~30 s for an unhelpful disconnect.

**Real-account test: the user's optional step.** The steps are in README.md. Its result goes into the PR conversation, not into this file.
