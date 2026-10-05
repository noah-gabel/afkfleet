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
