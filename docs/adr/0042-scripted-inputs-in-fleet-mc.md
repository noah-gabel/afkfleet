# 0042. A scripted mode's inputs in fleet-mc, as built: one place for what's held, guards before azalea's own systems, and what the live scenarios found

- **Status:** Accepted. Decided under ADR-0025
- **Date:** 2026-11-18
- **Related:** Plan.md Phase 15 (P15.10 to P15.15, P15.32); [ADR-0036](0036-scripted-modes.md) (D6, D9 to D12, D18) and [ADR-0037](0037-scripts-read-system-chat.md) (C2, C3, C9), whose decisions this builds; [ADR-0040](0040-fleet-script-as-built.md) (E10); [ADR-0026](0026-a-bot-in-a-minecart.md) and [ADR-0034](0034-a-stopgap-action-into-the-nearest-minecart.md) (riding and the stopgap action); ADR-0020 (S22, S27: the bridge's death and respawn); `crates/fleet-mc/src/script/`, `crates/fleet-mc/src/riding/passengers.rs`, `crates/fleet-mc/tests/minecraft/{scripted,chat_lines}.rs`; `docs/releasing.md`

## Context
Group C makes fleet-mc translate a scripted mode's inputs: keys, mouse buttons, a look at a point, mounting. It applies fleet-core's leash and its two guards of mining on the session's host thread, reads the snapshot a hook sees, and reports a world change. ADR-0036 made the decisions; this records how they were built against azalea 0.16.0, what the user's reviewer asked for at the group's start, and what a real 26.1 server showed.

**Everything about azalea below was read in azalea 0.16.0's own source** (docs.rs, `azalea-client`'s `plugins/{mining,movement,interact,loading}.rs` and `plugins/packet/game/mod.rs`, `azalea`'s `bot.rs`). Each fact fleet-mc depends on is in `docs/releasing.md`'s list for the next bump.

## Decisions

### G1. The port: what the runtime calls, and what it hears
- **`GameAction` gains** `Press(Input)`, `Tap(Input)`, `HoldUntilUsed`, `Release(Input)`, `ReleaseKeys`, `ReleaseAll`, `LookAtPosition(Position)`, `Mount` and `Dismount`. Group A left them to this group so that `main` never held an action the adapter drops (Plan.md's note at P15.3). `Mount` names no kind: only a minecart has a seat here (ADR-0026), and fleet-script already refuses every other.
- **`SessionHandle` gains two calls:**
  - `observe()` answers with an `Observation`: the snapshot, and the leash's anchor. `NotInWorld` while the bot is between worlds.
  - `set_guards(Guards)` sets the leash's radius and the tool guard. **A new session starts with the defaults of both** (8 blocks, 2 uses), so the runner sets them at every join, before it presses anything. Forgetting it leaves the defaults, never no guard.
- **`SessionEvent` gains** `WorldChanged` and `LetGo(LetGo)`, with the reasons `Leash`, `ToolGuard` and `ItemUsed`; `LetGo::inputs()` says what went up, for `Instance::let_go`.
- **Both are pending at most once.** A second world change, or a second let-go for one reason, before the first was read adds nothing: a server that sends a bot back and forth can't fill the queue. The contract's bound on lifecycle events grows by four.
- **The actor ignores both events,** so a step mode is as it was. Group D's runner reads them.

### G2. `Held`: one place says what's down
A component on the bot holds the seven keys that can be held, the keys tapped, and both buttons. **A game action only changes it.** Each game tick, before azalea's physics, one system writes azalea's key state from it: `PhysicsState::move_direction`, `trying_to_sprint`, `trying_to_crouch` and `Jumping`, which is what azalea's `walk`, `sprint`, `set_crouching` and `set_jumping` set.

**Why not azalea's own methods.** `walk` and `sprint` send a message that azalea handles in a later frame, and `walk` ends a sprint by itself. The leash must let go of keys inside a game tick, before that tick's physics, which only a write to the state can do. With one writer, a press, the leash and a world change can never disagree about what's held.

- **A key is down from the tick after its press;** a job between two ticks can't be earlier.
- **A tap is down for exactly one game tick:** the tick's physics and its input packet see it, a system after the physics ends it, and the next tick writes the key up. Jump is tapped the same way, not through azalea's `jump()`, whose reset would also end a held jump.
- **Sprinting is a vanilla client's:** the key asks for it; azalea starts it only while the bot walks forward, and letting go of the key alone doesn't end a sprint that goes on.
- **`ReleaseKeys` and `ReleaseAll` always write the key state,** whatever was held, so a sneak a step mode set past this component ends too.
- **The three keys that only tap** (drop, drop a stack, swap) send the vanilla client's `PlayerAction` with the origin as its block, pressed or tapped.
- **A new session's bot holds nothing:** azalea spawns the bot's entity, and an observer gives it an empty `Held` at that moment. So does a login or a respawn packet (G6).

### G3. The leash's tick
`Placement` on the bot holds the anchor and where the bot stood at the last tick.

- **The anchor is where the server placed the bot:** its position at the first game tick after a login or a respawn at which the teleport has come *and* the client has loaded (`HasClientLoaded`). A join, a respawn and a world change are all that one rule.
- **A server teleport of more than 8 blocks** from where the bot stood moves the anchor (fleet-core's `Leash::teleported`); a shorter one doesn't, and neither counts as walking.
- **Each tick, before azalea's physics:** fleet-core's `Leash::tick` judges the move since the last tick. Beyond the radius and further, every movement key in `Held` goes up, held or tapped, the key state is written in the same tick, and `LetGo::Leash` is reported. A bot that holds no movement key and is carried further reports nothing.
- **A riding bot is judged the same way,** by its seat's position.

### G4. The left button: azalea's hold-to-mine, behind the two guards
azalea 0.16.0 mines through four components: `LeftClickMine` (hold-to-mine is on), `MiningQueued` (a block it starts at the next tick), `Mining` (the block it mines) and `MineProgress`. Each game tick, **before azalea's `MiningSystems`,** one system applies fleet-core's two decisions:

| | What it does |
|---|---|
| **The tool guard** | The held item wears and is down to the uses the mode keeps: the button goes up in `Held`, and `LetGo::ToolGuard` is reported |
| **What azalea queued or mines** | Judged by its own block and the bot's feet *now*. Below the feet, or with the button up: `MiningQueued` is removed; `Mining` is removed with an `AbortDestroyBlock` and its progress reset |
| **Whether it may start** | `LeftClickMine` is on exactly while the button is down and the block under the crosshair may be mined, or nothing is under it (azalea then stops by itself) |

- **The hold only pauses at the mining guard:** the button stays down, and mining goes on when the bot looks at a block it may mine.
- **Letting go is only the button's state.** The next tick stops whatever azalea mines, since the button is up. So `Release`, `ReleaseAll`, the tool guard and a world change need no code of their own for mining.
- **Why the block being mined is judged again each tick:** a block level with the feet when mining began reaches below them once the bot has risen (a jump, a slab).
- **An entity under the crosshair when the button goes down is attacked once** (fleet-mc's own attack packet, ADR-0008 §8), then the hold begins. A tap attacks an entity or swings.

### G5. The right button
- **A press uses once and notes the stack in hand.** From the next tick on, the hold uses again **every 4 game ticks**, the vanilla client's delay, unless the item is used over time.
- **"Used over time" is read off the item:** a `consumable` or a `blocks_attacks` component (azalea knows every item's default components), or one of Minecraft's own items with a use duration: bow, crossbow, trident, spyglass, goat horn, brush. Such an item is one long use, **started again only when the stack in hand changed,** which is how a second piece of food follows the first.
- **Held until used,** the button goes up at that change instead, with a `ReleaseUseItem` and `LetGo::ItemUsed`.
- **Why not the server's "using item" flag.** A vanilla client asks its own prediction; azalea has none, and the server's flag arrives a round trip later, so on a slow connection a second use would slip out in the fourth tick. The item says the same thing without waiting.

### G6. A world change (P15.14)
- **What counts:** a Login packet after the join's own, or a Respawn packet that answers no death. Both are read inside the frame, right after the frame's deaths (ADR-0020, S27), so a Respawn in its death's own frame is that death's.
- **Reported when the bot is placed** in the new world (G3), not at the packet: a hook then has a position and an anchor.
- **Both packets let go of everything at once:** `Held` is emptied, and azalea's `LeftClickMine`, `Mining`, `MiningQueued` and a queued use are removed without a packet, since the block was in the other world.
- **A Login while the bot is dead** is a world change too. Its death stays open in the bridge until a respawn is confirmed, as before; whether a proxy's server takes a dead player over at all isn't known.
- **The actor and a step mode treat it as before:** a test holds that both new events leave a step-mode bot online with nothing published.

### G7. The order, pinned
Five systems run before azalea's own in a game tick, in one fixed order among themselves: placing, the leash, the key state (all before `PhysicsSystems`), the mining guards (before `MiningSystems`) and the right button (before azalea's `handle_start_use_item_queued`).

**The test the reviewer asked for** (`connector::app`, on the session's real App): probes are added *inside* azalea's `PhysicsSystems` and `MiningSystems`, so they stand where azalea's own systems stand and see what those see. For a bot that walks on past its leash and is about to mine, and mines, the floor it stands on, one game tick must show them: no key down, nothing queued, nothing mined.

**Red, by moving the systems:**
- The mining guard after `MiningSystems`: the probe saw `(true, true)`, the floor still queued and being mined.
- The leash and the key state after `PhysicsSystems`: the probe saw `Forward`.

The guards change components through commands, which Bevy applies at the sync point before the next system that's ordered after them. The probe in `MiningSystems` holds that too: without the sync point it would see what was removed.

### G8. What a hook sees
`observe()` reads the snapshot on the host thread: the bot's health, food, position, look, dimension, vehicle, held item with the uses it has left, selected slot and what its crosshair is on; the entities within 16 blocks; the blocks within 4.

- **Every kind is azalea's own name of a kind it knows** (`minecraft:minecart`), so it's an identifier by construction.
- **A dimension's name is the server's.** One that is no identifier becomes `minecraft:unknown`, and so does the vehicle of a bot whose vehicle the server never showed it.
- **No name, lore or sign text is read** (ADR-0036, D12).
- **The uses a tool has left** are its `max_damage` minus its `damage`; none for an item that doesn't wear or can't break.

### G9. Who sits in which vehicle, and mounting
- **`Passengers`** on the bot holds the vehicles someone sits in, read from `SetPassengers`.
- **It stays bounded,** as the reviewer asked:
  - a list for a vehicle the server never showed the bot is ignored;
  - an entry goes when its vehicle's list is empty, and when the server removes the vehicle;
  - the whole index goes at a login and at a respawn, and with the bot's entity at a disconnect;
  - at 512 entries it's swept of vehicles the bot no longer knows, and if it's still full, a new one isn't taken. A test lists ten thousand vehicles and finds 512.
- **`Mount`** is ADR-0034's click by ID with its patience of 200 ticks, **passing over a minecart someone sits in,** and taking it once it's free. The stopgap `EnterMinecart` is unchanged: it takes the nearest, whoever sits in it.
- **`Dismount`** is a tap of the sneak key for a riding bot, which is how a player gets out (ADR-0026), and nothing for one on foot.

### G10. What a player's lines arrive as (P15.32)
`slow_chat_lines_scenario`: a second player puts a reply's exact text, `Balance: $500`, before the bot in every way a player has on a vanilla server. Each line the bot receives is held to fleet-core's own `candidate_line` and to the balance example's pattern.

| The line | Arrives as | A candidate | Fits the pattern |
|---|---|---|---|
| `AfkBot2 joined the game` | system | yes | no |
| The player's chat | a player's chat, from them | no | |
| The player's whisper (`/msg`) | a player's whisper, from them | no | |
| The player's `/me` | a player's emote, from them | no | |
| `AfkBot2 was slain by Armor Stand using [Balance: $500]` | system | **yes** | **no** |
| The same with line breaks around the item's name | system, three lines | no | |
| The server's own `tellraw` of the text | system | yes | yes, 500 |

**The plan's sentence was wrong, as the reviewer said.** It expected only the `tellraw` line to be a candidate. A death line is a system message, one line, whole: a candidate by C2. What stops it is C3: the renamed text is *inside* the line, and a pattern must fit a whole line. The candidate rule isn't bent to fit the plan; the plan's note is corrected.

**An item's name can hold a line break** (an operator's `/give` with a custom name; here an armour stand's item). The death line then arrives as one message of three lines, one of which is the reply's text exactly. It's no candidate, by C2's third rule.

### G11. The live scenarios (P15.15)
Three, each a server of its own, each passed five times in a row before it was trusted:

| Scenario | What it shows |
|---|---|
| `slow_walking_scenario` | The leash ends a walk between 8 and 9 blocks out, a sprint under 9.5, and at a radius of 3 between 3 and 4; walking back and out again; each release and a tap; a tapped and a held jump; a held sneak; **a sneaking bot stops at an edge** (it overhangs by under 0.3 blocks, as a vanilla client does) and walks over it without the key; a dimension change and back are two world changes, each with nothing held and a new anchor |
| `slow_mouse_scenario` | The mining guard on the block the bot stands on (60 ticks, nothing sent); a real cobblestone generator mined block after block until the tool guard ends it at 2 uses left, 4 blocks; a new hold ended at once; the right button's uses **exactly 4 game ticks apart**, counted in azalea's tick-end packets; held until used, one golden apple and one use; mounting past a minecart a pig sits in; a refused break |
| `slow_chat_lines_scenario` | G10 |

**Found by the first run of the generator.** A held left button goes on to whatever the crosshair lands on once a block is gone, as a vanilla client's does. Behind the generator's block stood the trench's stone wall, and the bot mined it: three cobblestone and one stone for the tool's four uses. The scene now has bedrock there, and the reference says it: back a generator with something the tool can't break, or let go and look.

**A break the server refuses** (D11's known limit, as the reviewer asked): the bot isn't an operator, and the block is inside the spawn protection. Over 100 game ticks of holding with an iron pickaxe, the bot sent **9 starts and 8 or 9 finished breaks** in each of six runs. Each time it mined the block to its end, the server put it back, and the bot began again. The block stayed, the tool wasn't worn, nothing was counted as mined, no let-go came, the session stayed up, and the bot's own picture of the block was the server's again afterwards. So a refused break costs time and packets, and nothing else; a script can't tell it from a block that keeps growing back.

### G12. A proxy in the slow suite: not now
D18 left it to this group. The second login is tested where it's decided: the packet handed to the session's own packet reader, and the bridge's rule, with the placement that follows. A proxy with a lobby is one more image to pin and one more server to start in every slow run, to show the same packet arrive. It's worth it once a user reports a proxy that behaves otherwise.

## Deviations from the plan
- **P15.11's text** said "never a block at or below the feet". Group A sharpened the rule; fleet-mc applies fleet-core's decision, and the task's text now says so.
- **P15.32's text** expected one candidate; G10 has what's true.
- **`hold_for`** isn't fleet-mc's: fleet-script's instance lets go when the time is up (ADR-0040, E10). The live scenario shows each release instead.

## Known limits
- **A press reaches azalea at the next game tick,** up to 50 ms after the action.
- **A held left button mines whatever the crosshair lands on** (G11), and tries a refused break again and again.
- **The index of passengers can call a vehicle occupied** whose passenger left without a new list from the server. A script then passes it over until one comes.
- **The right button's "used over time"** is a list for the six items that say so in no component; a new such item in a Minecraft version would be clicked every 4 ticks until it's added.
- **A step mode's `Sneak` and a script's keys don't mix:** the key state is written from `Held` whenever a script's key changes. A mode is steps or a script, never both.
- **fleet-mc depends on more of azalea's inner workings,** re-checked at every bump (`docs/releasing.md`): the four mining components and `MiningSystems`; `PhysicsState`'s four fields, `Jumping` and `PhysicsSystems`; `StartUseItemQueued` and its handler; `HasClientLoaded` not being removed at a Login; `ItemStack::get_component` answering with an item's default components.

## Alternatives considered
- **azalea's `walk`, `sprint` and `left_click_mine`.** See G2: a frame late, and the leash and the guards must act inside the tick.
- **Exclusive systems for the guards,** which change the World at once. Bevy counts an exclusive system as touching everything, so its place would have to be declared against every system of the tick, or it warns at each session's start (ADR-0026).
- **Judging only what the bot looks at.** A block that azalea already queued or mines would be mined to its end after the bot rose or the button went up.
- **A leash in the runtime.** ADR-0036, D10: a stalled runtime would let a bot walk on.
- **The world change at the packet.** A hook would run with no position and no anchor.

## Consequences
- **fleet-mc performs every input of a scripted mode,** and holds a bot to its leash and its guards whatever the runtime does.
- **Group D** maps fleet-script's `Action` onto `GameAction`, sets the guards at every join, observes before each hook, and hands `LetGo` to `Instance::let_go`.
- **Three more live scenarios,** about two and a half minutes of `just test-slow`.
- **The reference** says what a held left button does once its block is gone, and what a refused break looks like.
