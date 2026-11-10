# 0026. A bot in a minecart: fleet-mc tracks the vehicle, keeps the seat and picks from it

- **Status:** Accepted
- **Date:** 2026-11-10
- **Related:** ADR-0003 (the azalea pin and its bump procedure); ADR-0008 §8 and ADR-0011 (the actions, `UseItem` and `HoldUse`); `crates/fleet-mc/src/riding/`, `crates/fleet-mc/tests/minecraft/riding.rs`; `docs/releasing.md`

## Context
The user's report: a farm that is used by sitting in a minecart and right-clicking works for a player, but a bot sitting in the same minecart, running a mode with `UseItem` or `HoldUse`, does nothing visible.

**azalea 0.16.0+mc26.1 has no riding on the client side** (azalea-client's source):
- `plugins/packet/game/mod.rs`: the handlers `set_passengers` and `move_vehicle` are empty, so azalea never learns that the bot is a passenger. Its `player_position` handler applies every teleport, riding or not.
- `plugins/movement.rs`: `is_passenger` is hard-coded to `false` ("TODO: implement passengers"), so azalea keeps running its own physics on the bot, and `send_position` reports wherever they left it ("TODO: posrot packet for being a passenger").
- `plugins/interact/pick.rs`: the bot's target is picked from azalea's own position plus its eye height, and the port of vanilla's `getEntityHitResult` has the own vehicle's branch commented out ("TODO: don't pick the entity we're riding on").
- `plugins/interact/mod.rs`: `handle_start_use_item_queued` sends `UseItemOn` for a block, `UseItem` for a miss, and an `Interact` with the entity for an entity.

**What actually goes wrong, measured on the 26.1 test server** (`slow_riding_scenario`'s first version, with a test-only probe of the bot's own picture):

| | The server | The bot's own picture |
|---|---|---|
| The minecart, on a rail on a floor at y = −60 | y = −59.9375 | the same |
| The bot's feet after `/ride … mount` | y = −60.35: the seat, 0.4125 below the minecart's position and so inside the floor block | y = −61.0: its physics dropped it through the block it was seated into, onto the one below |
| Its eyes | 1.27 above the floor | y = −59.38: inside the minecart's box (y −59.94 to −59.24) |
| What it looks at, whatever its look | | the minecart, picked at its own eyes |
| A use | nothing happens | two `Interact` packets on the minecart, no `UseItemOn` |

Meanwhile the bot sent one position packet a second with y = −61.0, and the server kept it at its seat: a server ignores a passenger's position.

**How a ride begins and ends**, in the order the server sent it:

| Moment | Packets to the bot |
|---|---|
| Mount | a teleport to the seat, then `SetPassengers` with the bot |
| `/ride … dismount`, or the bot's own `PlayerInput{shift}` (a `Sneak{on: true}`) | `SetPassengers` without the bot, then a teleport to where it gets out |
| The minecart is killed | `SetPassengers` without the bot, a teleport, then `RemoveEntities` |
| `/tp` of the riding bot | `SetPassengers` without the bot, then the teleport |
| `/tp` of the minecart with the bot in it | a teleport of the bot to the new seat; the ride goes on |

After every one of these endings, azalea's position was the server's again, without anything from fleet-mc: the teleport comes last and azalea applies it.

## Decision
Mine, under ADR-0025; the user left the approach open. fleet-mc gets a `riding` module with one plugin, `RidingPlugin`, in every session's App. It does three things, all only for a bot that rides:

**1. It tracks the vehicle.** `Riding { vehicle }` (the server's entity ID) is a component on the bot.
- A `SetPassengers` that lists the bot puts it on; a `SetPassengers` for the bot's vehicle that doesn't list it takes it off. `RemoveEntities` with the vehicle, and a `Respawn` or `Login` (a new world), take it off too: the server sent a `SetPassengers` first in every ending seen, so these are a safety net against a ride that would otherwise never end.
- The system reads azalea's `ReceiveGamePacketEvent` in `Update`, after azalea handled the frame's packets in `PreUpdate`, and applies a frame's changes in order.

**2. It keeps the bot at its seat.** In `GameTick`, after azalea's `PhysicsSystems` and before its `send_position`:
- The bot's position is set to the seat (the vehicle's position plus the seat's offset) and its velocity to zero, so azalea's physics start from the seat each tick and never carry the bot anywhere.
- `LastSentPosition` is set to the seat and `PhysicsState::position_remainder` to 0, the two things `send_position` decides by. So the bot sends no position while it sits; a changed look still goes out as `MovePlayerRot`, and azalea's answer to a teleport (`AcceptTeleportation` and its position) is untouched.
- **The seat's offset is −0.4125 for a minecart**, the value the server showed. The slow scenario compares the server's position of the bot with the minecart's at every run, so a Minecraft version that seats a player elsewhere fails it.

**3. It picks from the seat.** In `Update`, after azalea's `update_hit_result_component`, a riding bot's `HitResultComponent` is replaced:
- The eyes are at the seat plus a standing player's eye height (azalea's `calculate_dimensions(Player, Standing)`), whatever pose azalea gives a body that reaches into the floor.
- The pick is azalea's own `pick` again (`riding/pick.rs`), built from its public pieces (`pick_block`, `get_entities`, its pickable-entity query), because azalea's can't be called with other eyes: its options' fields are private. It adds the branch azalea's port leaves commented out: the own vehicle is only picked when its box holds the eyes, or when something was already picked at the eyes; otherwise the bot looks through it. From a minecart's seat the eyes are above its box, so a passenger clicks through its minecart onto what's behind it, as a player does.
- `UseItem`, `HoldUse` and `AttackFacingEntity` read `HitResultComponent` as before (ADR-0008 §8, ADR-0011), so nothing in `actions.rs` changes.

**Scope: only a minecart has a seat.** For any other vehicle the bot gets `Riding` but neither the seat keeping nor the pick, and behaves as before. A bot on foot matches none of the three systems' queries.

**Leaving** needs nothing new: the ride's `Riding` goes with the server's `SetPassengers`, and azalea applies the teleport that follows.

**Robustness.** The pick's system takes azalea's `Worlds` resource as optional and does nothing without it: a system whose resource is missing panics, and a panic in a session's App ends the session (found by this module's own tests).

### Tests
- **Unit** (`riding`, `riding::pick`): what each packet changes; several changes in one frame; the seat's offset; the seat keeping and that a bot on foot, one that left, one in a boat and one whose vehicle is unknown are left alone; the pick's entity rule case by case, vanilla's "picked at the eyes" case included; block against entity and both ranges. Two App tests run azalea's own `update_hit_result_component` together with the plugin on the measured scene: without `Riding` the target is the minecart (azalea's result, kept), with it the pig behind the minecart.
- **Live:** `slow_riding_scenario` in fleet-mc's `minecraft` test binary. On foot the bot opens a chest (the check itself works). In the minecart: it's where the server has it; it opens the same chest; it opens a chest that is behind its own minecart's box; it right-clicks a villager; `HoldUse` draws and shoots a bow; turn, swing, another slot's snowball, an attack on a pig and a jump work from the seat, with no position packet sent; it follows a minecart that is teleported, and answers each of those teleports. Then it leaves by sneaking, and by the minecart's kill: each time it ends where the server has it, stays there for 40 ticks and got exactly the one teleport. Last, on foot, it opens the chest again.
- **Red first:**
  - Before the fix, the scenario failed at the click from the minecart, with the picture in Context.
  - R1, the own vehicle's branch switched off: the click through the minecart failed; the target was the minecart, entered at its top.
  - R2, the seat keeping switched off: the mount's check failed; the bot never was at its seat in its own picture.
- **The scene is a platform 20 blocks above the ground.** On the ground, 6 of 8 runs of the finished scenario failed, none of them on the fix:
  - Four on two waits of the scenario's own: one read the server's position once while the bot was still settling after a dismount, the other went on before a teleport to where the bot already stood had arrived, and the teleport then undid the look that followed.
  - One because the minecart had been pushed off its rail: the server had it 0.84 blocks away, on the floor. The world's own animals wander on the ground (the bot's entity ID was 29 in that run, 1 in the others).
  - One on the attack, whose check named "a pig" without a tag: most likely a wild one.

  On the platform, with tagged entities and waits that read again, 5 of 5 passed, about 28 s each.

## Consequences
- A bot in a minecart uses, holds and attacks as a player sitting there would, and its own position is the server's during the ride.
- **fleet-mc now depends on more of azalea's inner workings**, re-checked at every bump (`docs/releasing.md`): the two empty handlers and the `is_passenger` stubs (once azalea rides by itself, this module goes); `send_position`'s two inputs and its place after `PhysicsSystems`; `update_hit_result_component`, `pick_block`, `get_entities`, `MaybePickableEntityQuery` and `HitResultComponent`'s `DerefMut`; and the `Component` derive, which names its crate as `bevy_ecs` and gets it as `azalea::ecs`.
- **Known limits:**
  - Other vehicles (boats, horses, pigs, striders, camels) are as before. Each has its own seat; a boat also carries two passengers and is steered by one, which needs `MoveVehicle` and more than a seat.
  - "The own vehicle" is the entity the bot sits in. Vanilla compares root vehicles, which is the same for a minecart: it carries one passenger and rides nothing.
  - While seated, a look goes out when it changes, not every tick.
  - azalea sends an entity click as two identical `Interact` packets, on foot too, so a villager counts two talks for one `UseItem`. Not changed here.
  - azalea's pickable entities leave some out (item frames, for one), on foot too. Not changed here.
- The live scenario adds about 30 s to `just test-slow`.

## Alternatives considered
- **Only the pick, no seat keeping.** It would have fixed the click: the server checks a click against its own position of the bot. But the bot's own position would stay wherever azalea's physics take a body the server holds elsewhere (under the floor here; anywhere, once the minecart drives on), and it would keep reporting that position. Everything azalea derives from the position would be wrong for the whole ride. Rejected for a position that is the server's.
- **`StartUseItemQueued::force_block`.** It clicks a named block without looking at it. A mode names a look, not a block, it does nothing for entities, and azalea's own doc warns that it stands out to anticheats. Rejected.
- **Stopping `send_position` by taking one of its components off the bot.** It would stop the looks too, and other azalea systems read those components. Rejected for the two values it decides by.
- **Taking the seat from the mount's teleport**, for any vehicle: the teleport comes before the `SetPassengers`, so the module would have to remember teleports it can't yet attribute, and the offset would only be right for seats that never move. Rejected for one measured value and a test that holds it.
- **Patching or forking azalea.** A fork is ours to maintain against an exact pin (ADR-0003). Rejected; the gaps are worth reporting upstream, which is the user's to do or to ask for.
