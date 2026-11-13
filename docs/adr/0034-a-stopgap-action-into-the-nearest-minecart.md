# 0034. A stopgap mode action into the nearest minecart, and a right-click every 200 ms

- **Status:** Accepted
- **Date:** 2026-11-13
- **Related:** ADR-0010 (the mode model and its limits), ADR-0011 (`UseItem`, `HoldUse`), ADR-0019 (a mode on the wire), ADR-0020 (S20, S25: on-respawn steps, and a reader that doesn't know a case; R160: a step the app doesn't know), ADR-0026 (riding); `crates/fleet-mc/src/riding/enter.rs`, `crates/fleet-mc/tests/minecraft/riding.rs`; the runbook's [Updating](../runbook.md#updating); `docs/releasing.md`

## Context
The user's report: on some servers, after a long night-time restart, both of their bots were back without their minecarts. They stood beside them, still running the farm mode. A short reconnect puts a bot back into its minecart by itself; a long one doesn't, and where each minecart sits isn't known exactly.

**What was asked,** for release 0.1.3:
1. **A mode action that gets a bot into the nearest minecart:** the nearest one within the player's entity interaction range, which is as far as a server accepts an entity click. The bot turns to it and right-clicks it. A bot that rides already (ADR-0026's `Riding`) does nothing, and so does one with no minecart in reach. It's meant as a step at start and on respawn, before the farm's own steps, so the mode's next look step must still apply after it.
2. **A repeating `UseItem` every 200 ms:** a click every 4 game ticks, the vanilla client's own speed for a player who holds right-click. Only for `UseItem`: the general 250 ms and the 500 ms for attacking and `HoldUse` stay.

**The action is temporary.** Scripted modes are planned, with a `mount()` of their own, and this action goes once they exist. So it's as small as it can be: one action, minecarts only, no general layer for vehicles or for finding entities, and nothing built ahead for scripts or for its own removal.

**What the test server showed** (26.1, `slow_enter_minecart_scenario` and its first version):

| | Measured |
|---|---|
| A click on a minecart by its entity ID, from a bot on foot beside it | The server seats the bot: a teleport to the seat, then `SetPassengers` with the bot, as ADR-0026's table has a mount |
| That teleport's look | Both rotation flags relative, with 0 and 0: the bot keeps its look, whatever it was |
| A first version that clicked at once, inside the action | Beside a minecart: in. Seated: nothing. Out of reach: nothing. **Right after a join, beside the minecart: nothing, and the bot stayed on foot.** It found no minecart within reach and sent no click |
| The action as built, run at a join in the same place | In. Over eleven runs the click came 26 to 96 ms after the action |

**Why a join is too early.** fleet-mc reports a join at azalea's `Spawn` event, which azalea sends once the bot's own chunk is loaded (azalea 0.16.0's `events.rs`: `Added<InLoadedChunk>`). A mode's steps at start run at once then (ADR-0013). At that moment the bot has no minecart within its reach, and a tick or two later it has. The measurement doesn't tell whether the server hadn't sent the minecart yet or the bot's own position wasn't final; waiting covers both.

## Decision
Mine, under ADR-0025, within what was asked.

### The action
- **Its names:** `Action::EnterMinecart` and `GameAction::EnterMinecart` in fleet-core, `{"type":"enter_minecart"}` in a mode's JSON, case 11 of the wire's `Action`, `ModeActionKind::EnterMinecart` in the API, and "Get into the nearest minecart" in the editor's list, last. It takes no field.
- **It's an ordinary action for the mode rules:** any schedule, the general 250 ms minimum when it repeats, and not a limited kind. A step that does nothing for a seated bot needs no limit of its own, and a repeating one (once a minute, say) also puts a bot back that lost its seat in the middle of a session.

**What fleet-mc does** (`riding/enter.rs`, in the riding plugin):

1. **The action only marks the bot** as `Entering`, with 200 game ticks of patience. A bot that has `Riding`, whatever it rides, isn't marked. The action again starts the patience again.
2. **A system runs each game tick for a marked bot** that the server takes clicks from (`HasClientLoaded`; a bot that isn't loaded waits and loses no patience):
   - A bot that rides by now loses the mark.
   - Otherwise it looks for **the nearest minecart within reach.** A minecart is within reach when the line from the bot's eyes to the middle of the minecart's box enters the box within the bot's own `entity_interaction_range`: what a player aiming at its middle could click, measured as azalea's own pick measures an entity. Only `EntityKind::Minecart` counts (the one a player rides, 0.98 wide and 0.7 high), and only an entity the bot has the server's ID of. Of two equally near, the lower ID.
   - With none, a tick of patience goes. At 0 the mark goes, and nothing happened.
   - With one, the mark goes and the bot clicks, once.
3. **The click is a turn, a click and a turn back, on the wire alone.**
   - The look at the click's place goes out as a `ServerboundMovePlayerRot` of its own, in azalea's 0.15° steps from the bot's look as every look of the bot's is, unless it's the look the server already has.
   - Then one `ServerboundInteract` with the minecart's ID, the main hand and whether the bot sneaks.
   - **The bot's own `LookDirection` never changes.** `LastSentLookDirection` is set to the look that went out, so azalea's position packet, later in the same tick, finds the bot's look differs and sends it again.
4. **The system's place** is after azalea's physics and its sprinting packet and before its position packet, chained after ADR-0026's seat keeping, so Bevy warns about no undeclared order (`connector::app`'s schedule test).

**Why the turn never touches the bot's own look.** A mode's steps at start run back to back, and the click comes a tick later at the earliest, and later still after a join. A look step right after the action has run by then. Had the click turned the bot itself, the mode's look would be gone, with nothing after it to bring it back. As built, the order of the two steps doesn't matter, and a mode without a look step keeps the direction the server restored at the join, which the built-in `farm` relies on (ADR-0010).

**Why the click names the minecart.** A click through what the bot looks at (`UseItem`'s way: azalea's queued use reads the bot's target at the next tick) clicks whatever the look step after it faces, and a block in the line of sight takes it.

**Why one click.** azalea's own entity click sends the packet twice (ADR-0026's known limits: a villager counted two talks for one `UseItem`). Each packet is a click of its own for the server, and one seats the bot.

### A right-click every 200 ms
- **`ModeDefinition::MIN_USE_ITEM_INTERVAL` is 200 ms,** for a repeating `Action::UseItem` alone. `min_interval` lists it; every other action keeps its minimum: 250 ms in general, 500 ms for `AttackFacingEntity` and `HoldUse`, 30 s for `SendChat`.
- **The API's refusal** below it reads "must be at least 200 ms for using an item", at the step's `interval_ms`, as the other minimums' texts do. The editor has no rule of its own: the server's text shows at the field, and "0.2 seconds" is what a friend types.
- `UseItem` was never a limited kind (ADR-0010), so a mode may still hold several such steps. Nothing changes there.

### An agent older than the server
The server is updated first (the runbook), so for a while agents are older than it.
- **A mode with `enter_minecart` reaches a 0.1.2 agent as an action it can't read:** an unknown case of a `oneof` is a field prost skips, the action reads as unset, and the agent refuses the whole message (`Action.action` missing). It never runs the step as another action.
- **A `use_item` step below 250 ms is refused too:** an agent checks every mode it's sent with its own version's rules (ADR-0019).
- **Either way the agent's stream ends as `unreadable` and reconnects in a loop,** as ADR-0020's S25 has it for `on_respawn`: its bots keep the modes they had and take no command until the agent is updated or its bots are switched off that mode.
- **So: the server, then every agent, then modes that use what's new.** The runbook's Updating says so, and so do the release's notes.
- **An app older than the server** shows a mode with the new action read-only, with "Step unknown to this app." (R160).
- **A server rolled back to 0.1.2** reads a stored mode with the action as corrupt: the runbook's "Going back" already asks to switch bots off such modes before a restore.

### What removing the action will need
None of it is built now.
- **Stored modes that use it** must be converted (to a script with its `mount()`), or the release that removes it must refuse to start, or to migrate, while one is stored, and name each. A fleet-core without the variant can't read such a mode at all: it's the "corrupt" above.
- **The wire's case 11 is retired, not reused:** `reserved 11;` and `reserved "enter_minecart";` in `mode.proto`. An agent still on a version with it never gets it from a server without it.
- **The API's kind goes;** apps older than that server keep working, since no mode holds it any more.
- **In fleet-mc:** `riding/enter.rs`, the system's line in the riding plugin, the action's arm in `actions.rs`, the live scenario's last part, and this ADR's line in `docs/releasing.md`.
- **In the UI:** the list's entry and the schema's key; both stop compiling with the generated type.

### Tests
- **fleet-core:** the action's JSON and tag; it's valid at start, on respawn and repeating from 250 ms, refused at 249 ms; a use at 200 ms is valid and at 199 ms refused with its own minimum; both in the property tests' valid drafts.
- **fleet-proto:** the step crosses as itself; a use at 200 ms crosses and one at 199 ms is refused; an action case this version doesn't know is refused as missing; a reader declared as before the case sees the action unset.
- **fleet-api-types:** the kind's name and fields; every kind's minimum and its text, the use's own among them.
- **fleet-mc, on an App with the riding plugin:** the action alone sends nothing; at the next tick, a look at the minecart and then one click on its ID; the bot's own look is unchanged and azalea has it to send again; a look between the action and the click stays the bot's; no look packet for a bot that faces the minecart already; a sneaking bot's click says so; the nearest of three; a minecart that appears 30 ticks later is clicked then; the patience ends after 200 ticks and starts again with the action; one click and never another; a bot that rides, at the action or by the time of the click, does nothing; out of reach, other kinds and an entity without an ID don't count; the reach is the bot's own attribute; a bot that isn't loaded waits; a bot that lacks a component sends nothing, and one whose entity is gone is `NotInWorld`.
- **Live,** `slow_enter_minecart_scenario`, with the bot's packets recorded as in ADR-0026's scenario:
  1. Far from the minecart: no click and no look packet while it looks for one; after its patience, moved beside the minecart, it stays on foot.
  2. Beside the minecart, the action and a look step back to back: the bot is seated, the packet before the one click is a look at the minecart, the server has the step's look, still a second later, and the mount was the one teleport.
  3. Seated, the action again: no click, no look, no teleport, still seated.
  4. Taken out by the server and disconnected, it joins again and runs the action at its join: it's seated, with one click, looking where the server had it.
  5. Seated, it leaves and joins again, which the server answers with the bot in its minecart, as after a short reconnect. The action at that join: no click in the eleven seconds after it, and still seated.
- **Red first:** every unit test failed against a stub first (the action a no-op, the conversions a jump, the minimums another action's). The live scenario's fourth part failed against the first version, which clicked inside the action: "`data get entity AfkBot1 RootVehicle` still answered "Found no elements matching RootVehicle" after 60s", with no click among the bot's packets.
- **Repeated:** the scenario passed 5 of 5 runs before its fifth part was added and 6 of 6 with it, about 43 s each.

## Consequences
- A bot a server put back beside its minecart gets in again, at its join or its respawn, and a farm's look holds.
- A repeating right-click may run at the vanilla client's own rate.
- **Known limits of the action:**
  - **An occupied minecart counts.** azalea doesn't know other entities' passengers (ADR-0026), so when the nearest minecart already holds someone, the bot clicks it, the server does nothing, and the bot doesn't try the next one. Two bots whose minecarts stand within reach of each other can miss this way; a repeating step tries again, but at the same minecart.
  - **The nearest minecart is whichever stands there.** Someone else's minecart beside the bot is taken as readily as its own, and the bot then goes where that one goes. The threat model's row about a server making a bot act says so.
  - **It clicks once.** A click the server refuses (the bot sneaks, the minecart moved away) isn't repeated until the action runs again.
  - **Nothing checks the line of sight:** a block between the eyes and the minecart doesn't stop the click, and whether a server accepts it is the server's to say; vanilla 26.1 did in the scenario's open scene, and no case with a block was tried.
  - **Ten seconds of patience** are a guess for a server slower than the test server. One that shows the minecart later leaves the bot on foot; a repeating step covers it.
  - **One look packet outside azalea's own, at most, per click:** a vanilla client sends its look with the tick's position packet.
  - Other vehicles are as before (ADR-0026).
- **fleet-mc depends on a little more of azalea,** re-checked at every bump (`docs/releasing.md`): the two packets' shapes, `LastSentLookDirection` and how `send_position` reads it, `HasClientLoaded`, `direction_looking_at`.
- The live scenario adds about 40 s to `just test-slow`.
- **Agents must be updated before modes use either addition** (above).

## Alternatives considered
- **Clicking inside the action, at once.** The first version. It fails right after a join, the action's main use (measured above). Rejected.
- **Turning the bot itself and leaving it turned,** for the mode's next look step to correct. With the click a tick or seconds after the action, that look step has already run. Rejected for a look that only changes on the wire.
- **A look and azalea's queued use,** as `UseItem` clicks. The use reads the bot's target at the next tick: a look step after the action redirects it, and so does anything in the line of sight. Rejected for a click by ID.
- **azalea's `EntityInteractEvent`.** It sends the click twice, needs the bot's `HitResultComponent` and warns about a bot without one. Rejected for one packet written here.
- **Only at start and on respawn,** as a new mode rule. A rule, its error, its API text and its editor state, for an action that goes again; and a repeating step is useful. Rejected.
- **Skipping occupied minecarts.** It needs every vehicle's passengers tracked from `SetPassengers`: the layer this action isn't to have. Left as a known limit.
- **Holding a mode back from an agent that's too old,** by the version in its hello. A gate in the control plane for a stopgap; `on_respawn` shipped without one (S25), and the order of an update covers it. Rejected.
- **Waiting in the runtime instead** (the steps at start a moment after the join). It would delay every mode's steps on a guess, and the on-respawn case would need the same. Rejected for a wait where the minecart is seen.
