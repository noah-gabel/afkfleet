# 0043. The script runner in fleet-runtime, and a script's hooks on the async workers

- **Status:** Accepted. Decided under ADR-0025. One point is put to the user at its end: hooks take turns on the async workers instead of moving to a thread pool of their own
- **Date:** 2026-11-21
- **Related:** Plan.md Phase 15 (P15.16 to P15.19, P15.33); [ADR-0036](0036-scripted-modes.md) (D1, D5, D6, D9, D18), [ADR-0037](0037-scripts-read-system-chat.md) (C4, C5, C7), [ADR-0040](0040-fleet-script-as-built.md) with its amendment (E5, E6, E10, A1, A2) and [ADR-0042](0042-scripted-inputs-in-fleet-mc.md), whose decisions this builds; ADR-0013 (the runtime's clock and tasks), ADR-0032 (the load test); `crates/fleet-runtime/src/script/`, `crates/fleet-runtime/tests/fleet/scripted.rs`, `crates/fleet-integration/tests/integration/load/scripted.rs`

## Context
Group D makes fleet-runtime run a scripted mode: it gives a script its events and its time, takes a snapshot before each hook, performs what the hook asks for, and releases everything at every way out. The decisions were made in ADR-0036 and ADR-0037; this records how they were built, and answers the question the user's reviewer put first: **a hook may take 5 ms and a script 20 ms a second (ADR-0040's amendment), and hooks run on the async runtime's workers, beside the agent's stream, its heartbeat and every step-mode bot. Can the workers take it?**

In short: with the hooks run as they come due, no. Up to the workers' capacity nothing changes; past it, everything on the runtime is as late as the hooks queued ahead of it, without a bound (R10). So hooks take turns now, and what isn't a hook waits for one hook at most, whatever the scripts do. The numbers are under "What was measured".

## Decisions

### R1. Where a script runs: in its session's mode runner task
`ModeRunner` is still the one task a session's mode runs in. It runs a step mode's plan itself, as before, and a scripted mode through a `ScriptRunner` it lends its session, its chat handle and its clock. A change from a step mode to a script, and back, happens inside that task:

- **Steps to a script:** the steps let go as at every mode change (`HoldUse` off, `Sneak` off), then the script starts.
- **A script to anything else:** the script ends in an orderly way first (R6).
- **A runner without a script's parts** (a test of the step runner alone) does nothing with a scripted mode, as with a mode without steps.

`fleet-runtime` depends on `fleet-script` for it (Plan.md §4). It names no Rhai type: `Program`, `Instance`, `Action` and the two error types are fleet-script's own.

**Alternative: a task of its own for a bot's script, across sessions.** It would hold the instance without a lock. But it would need the session's handle, chat queue and events handed over at every join and taken back at every end, by an actor that already has one task a session for exactly that.

### R2. What outlives a session: the bot's `ScriptSlot`
ADR-0036's D5 says the state "belongs to the bot's actor". It belongs to something that outlives the actor too:

| In the slot | Kept across | Gone at |
|---|---|---|
| **The instance,** with the script's state, and whether the bot has joined since its start | Reconnects, world changes, deaths, respawns, an actor that crashed and was started again | A Stop; a mode that was changed or saved |
| **The limiter** | Everything | Never: no sequence of events gets around a minimum (D7) |
| **The chat watch** | When each any-time pattern last matched: always | Its windows: every disconnect, stop and new start |

- **The supervisor keeps a bot's slot,** as it keeps its chat bucket, and hands a clone to each of the bot's actors. A crash of the actor therefore resets no limit and doesn't run a stopped script again.
- **Who ends a run.** At a Stop, the actor, whether a runner lives or not (a bot can be stopped between two sessions). At a mode change, the runner, after the old script's orderly end. A mode saved as it was changes nothing.
- **A script that stopped stays stopped** across sessions (D3): the instance stays in the slot with its error, and the status is said again at every join.
- **A hook runs under the slot's lock,** a std mutex held for the hook's 5 ms at most and never across an await. The instance gets a copy of the limiter and the slot takes the copy back, so a hook that panics costs the bot no limit.
- **A hook that panics is a script that crashed.** The lock is poisoned by it; the slot then says `Crashed` and runs nothing until the next Stop or mode change. Without this, the bot's actor would crash at every join until the supervisor's crash-loop limit took the bot offline.

**Compiling.** A script is compiled when its run starts, in `spawn_blocking` (D1), once for all the bots of a fleet that run it: `ScriptPrograms` is a list of weak references, so a program lives exactly as long as an instance holds it. A script the engine doesn't take stands as `Refused` with its line; the bot stays online and idle.

### R3. How a session's events reach the script
The bot's actor stays the one reader of its session's events (ADR-0013). What a script must hear, it puts into the session's `ScriptFeed`; the runner reads its inbox between two hooks.

- **A death, a respawn, a world change and each reason of a let-go wait at most once.** One that comes again moves to the back, so the order of the last of each kind holds: after "died, respawned, died" the script hears "respawned, died" and knows the bot is dead. Seven entries at most, whatever a server does.
- **Chat is kept only while a script that declares patterns runs,** up to 64 lines (`script_chat_lines`). A line beyond that is dropped and counted (`afkfleet_script_chat_dropped_total{reason="queue"}`).
- **A step mode's runner never reads the feed,** and a script that starts forgets what waited there.

**Alternative: a bounded channel.** A full one would have to drop a death or block the actor.

### R4. A hook's turn
1. **The session says what the bot sees** (`observe()`), right before the hook.
   - **A join, a respawn and a world change wait for the bot's placement:** the session is asked again every 100 ms until it answers with a world. `on_join` in the reference's examples selects a slot and looks somewhere, which a bot between worlds is refused.
   - **A death, a chat match and `on_stop` don't wait.** Without a snapshot they're shown what the bot saw last, or `Snapshot::nowhere()`, and told the bot isn't in a world.
   - **A timer's wake that finds the session too busy to answer is tried again 250 ms later.** The instance isn't told the bot left its world then: it would stop counting its timers, and on a quiet bot nothing would start them again.
2. **The hook takes its turn** (R10), then runs to its end on the instance, with the time from the runtime's clock and its own time read from the fleet's stopwatch (R5).
3. **What it asked for is performed, in order.** Every action has passed the bot's limiter inside the instance. A failed one is skipped and logged once a kind, as a step mode's; a hold the session didn't take is told to the instance (`let_go`), so `held` says what's true.

**The mode's guards** (leash, tool guard, block guard) are set at every join and every world change, before anything is pressed, and again every 100 ms until the session took them: a new session has the defaults (ADR-0042, G1), and a script must never press under another mode's leash.

### R5. Time
- **A script's timers, its `hold_for`s and its replies' windows run on tokio's clock,** through the runtime's `RuntimeClock`: the runner sleeps until the slot's next wake. Paused time moves them to the millisecond.
- **A hook's own time is read from a `Stopwatch`** (fleet-core's port; ADR-0040, A1) that the fleet is built with: `FleetParts::stopwatch`, without a default, since a fleet without one would bound no hook's time. The agent hands in `fleet_agent::stopwatch::OsStopwatch`, the OS's monotonic clock; it lives in fleet-agent because fleet-runtime's own lints ban that clock (ADR-0013). Tests hand in fleet-testkit's `ManualStopwatch`.
- **Tests let paused time pass by sleeping, not by `advance`.** tokio doesn't move paused time while a `spawn_blocking` task runs (its `time::pause` documents it; read in tokio 1.53.3), so a sleep waits for the compiler and then for exactly its length.

### R6. Every exit releases everything (P15.17)

| The exit | Who releases | `on_stop` |
|---|---|---|
| **A Stop;** a bot's removal; the fleet's shutdown | The instance's last action after `on_stop` is the `ReleaseAll`; the actor waits for the runner's end before it disconnects | Yes |
| **A mode that was changed or saved** | The same, in the runner's own task; the session lives on | Yes |
| **An error, a limit** | The instance: its one action left is the `ReleaseAll` | No |
| **A death, a world change** | The instance, before the hook (D6) | No |
| **A disconnect** | The session's end; the instance is told (`disconnected()`), so nothing is held and no timer is set at the next join | No |
| **The runner's task ends any other way:** a panic, a cancel, an orderly end that ran out of time | **The bot's actor** | No |

- **A Stop is orderly, and the actor waits for it.** The runner is asked to stop while the session and the chat queue still live. The actor goes on to the disconnect when the runner's task has ended, or after 2 s (`script_stop_timeout`): a hook of 5 ms and a few session calls, each bounded by the session's own timeout. A session that hangs can't hold a Stop longer.
- **A removal and a shutdown are a Stop.** The runner's token is no child of the actor's own, so an actor that is cancelled still ends its mode in order; the actor cancels the runner wherever a session ends, and its `JoinSet` aborts the task if the actor itself is dropped.
- **`on_stop`'s one message goes to the session at once,** past the queue, by the bot's chat rate. The queue's turn would come after the session it's meant for.
- **The mark.** A session carries a flag that says a script may still hold something in it. The runner sets it when a script runs there and clears it once the script's last `ReleaseAll` went through. The actor reads it wherever a runner's task ends (its end, a panic, a Stop) and sends a `ReleaseAll` of its own if it's still set. A script that never ran in the session leaves it alone.
- **The fake session knows what a scripted mode holds** (`SessionController::held`: what its presses put down and no release, world change, let-go or end of the session has let go of). Each exit's test finds the `ReleaseAll` in the session's log and nothing held.

### R7. Status, error and log (P15.18)
**`FleetEventKind::Script(ScriptEvent)`:**

| | |
|---|---|
| `Status(Running)` | A script's run started |
| `Status(Stopped(error))` | It failed or hit a limit: the kind, the hook and, where Rhai has one, the line and column |
| `Status(Refused(error))` | This agent's engine doesn't take it: the compile error with its place |
| `Status(Crashed)` | A hook panicked (R2) |
| `Log(…)` | A line the script wrote, or one the runtime wrote about its chat (R8) |

- **The status is said again at every join,** so a subscriber that missed it hears it; `ScriptSlot::status()` says it at any time.
- **Bounded:** a script's own lines are 200 characters and 20 a minute (fleet-script); an error's message is 200 characters; what a script is refused of its chat is logged at most 20 times a minute, further ones counted (`afkfleet_script_log_dropped_total`). A send, a window and a match need no limit of their own: the bot's chat rate and a pattern's one match a second bound them (ADR-0037, C7).
- **A script's text is its own, and is logged through `tracing` only at `debug`,** like chat: a log line, Rhai's message about the script, a declared message as it was sent. At `info`, a stop names the hook, the line, the column and a label for the kind of reason. A test holds each text at `debug` alone.
- **fleet-script says what it refused** (`Ran::refused`): each kind of thing a hook asked for and each reason once, with how often. The script got each as a result; the runtime counts them, and logs those of `chat.send` and `chat.expect`.
- **The agent keeps these events in its `debug` log** until the wire carries them (P15.20, P15.23).

**Six metrics,** all counters: the agent's recorder keeps no histograms (P5.3).

| | Labels |
|---|---|
| `afkfleet_script_hooks_total`, `afkfleet_script_hook_microseconds_total` | `hook`: the seven hooks. Their quotient is a hook's mean time |
| `afkfleet_script_refused_actions_total` | `reason`: the ten a script reads |
| `afkfleet_scripts_stopped_total` | `reason`: `limit`, `error`, `misuse`, `state`, `refused`, `crashed` |
| `afkfleet_script_chat_dropped_total` | `reason`: `queue`, `gap` |
| `afkfleet_script_log_dropped_total` | |

### R8. Chat in the runner (P15.33)
- **A chat line is put to fleet-core's candidate rule** with the server's chat formats, then to the bot's `ChatWatch`. The formats are empty until P15.34 sends them with a bot's spec, as on a standalone agent; no scripted mode reaches an agent before then.
- **A reply's window opens when the session took the command.** A declared message goes into the bot's chat queue like every other, at the bot's own rate. The delivery tells the runner what became of it, with the moment the session answered; the windows that waited for that command open then, from that moment.

  | What happens to the command | The window |
  |---|---|
  | The queue refuses it: it's full, or the bot's chat rate is used up | None. The log says why, and that the window was dropped |
  | The limiter refuses plain chat (30 s) | Nothing was queued, and `chat.expect` answers `no_command` |
  | The session doesn't take it (it can't sign it, it didn't answer) | None, the same lines |
  | The queue closes with the command still in it | None |
  | The session took it | Opens, at that moment, for its length |

- **A window is open for all of its last millisecond** (fleet-core's rule), so the runner's wake for it is the millisecond after. A wake at the window's end found it still open and asked for the same wake again without end: found by a test that never finished.
- **An any-time pattern matches once a second;** a line inside that second is dropped and counted (`reason="gap"`).
- **Every send, window, match and refusal is a log line** (C7), typed (`ScriptLog`): `Sent`, `NotSent`, `WindowOpened`, `WindowExpired`, `WindowDropped`, `Matched`, `Refused`.

### R9. One hook a turn *(found while the load test was designed)*
fleet-script's `Instance::wake` ran every due timer in one call, each as a hook of its own. A script with several timers due together therefore held its thread for up to the second's whole 20 ms, four times what the reviewer's question assumes.

- **`wake` runs one due timer now,** the one that was due first, and asks to be woken again at once while another is due. The order is the same (ADR-0040, E10's note).
- **The runner gives its worker back after every hook** (`yield_now`), before it comes back for the next one.

So no call on an instance, and no stretch of a runner's task without an await, is longer than one hook: 5 ms.

### R10. Hooks take turns
**What was measured first** (the table below, "as they come due"): a hook's 5 ms is no problem by itself. The problem is how many of them are runnable at once. Each script's runner is a task; when two hundred of them are due, a task that isn't a hook waits behind one hook of each. On one worker with every bot on the hot script, the heartbeat came 0.78 s late and the step-mode bots lost two thirds of their steps. Nothing failed, but nothing bounds it either: it grows with the fleet.

**`ScriptTurns`, one for a fleet:** a hook takes a turn before it runs.
- **At most one fewer hooks run at once than the runtime has async workers, and at least one** (`script_hooks_at_once`, `None` for that rule). One worker is always free for what isn't a hook.
- **A script keeps its turn until it has given its worker back once.** That is what bounds the wait on a single worker, where hooks never overlap anyway: while one script holds the turn, the others that are due wait at the turnstile, outside the worker's queue. A task that becomes ready has one hook ahead of it, not two hundred.
- **The turns come in the order they were asked for** (tokio's semaphore is fair).
- **The turn is never held across a session call:** the snapshot is taken before it, the actions are performed after it.

**What it costs.**
- **Scripts get the workers' time less one worker.** On two workers, hot scripts that could have used both cores get one; their own hooks begin later (the table has it). A fleet of the examples doesn't notice: its hooks are a millisecond of work a second, all two hundred together.
- **A hook's snapshot is as old as its wait for a turn.** Without load that's a fraction of a millisecond. Under a load that makes hooks wait, a hook acts on what was true when it asked. The session's own guards judge the game as it is, every tick (the leash, the three guards of mining), so this costs a script accuracy, not the fleet safety.

**Why not a thread pool for hooks** (ADR-0040, E6, names one, with 2 MiB of stack a thread):
- **A pool would keep hooks off the async workers altogether.** But what protects the other tasks is the bound on how many hooks run and wait, and the turnstile gives that bound on the workers themselves, which have the 2 MiB a hook needs.
- **A pool moves every hook to another thread and back:** the instance, the snapshot and the limiter cross twice a hook, and the slot's lock is taken on a thread tokio doesn't know. And it's a set of threads to own and to end, where the runtime today has none of its own but the blocking pool.
- **What a pool would add** is that a hook never delays an async task at all, where a turn delays it by one hook at most on a single worker. The measurement shows no such delay beyond what the machine's timers have anyway: 9 ms for a heartbeat on one worker with two hundred hot scripts, against 8 to 10 ms with none.

If the user prefers the pool, `ScriptTurns` is the place: a turn would become a job handed to a pool's thread.

## What was measured
`just load` (`load_200_scripted_bots`), a release build, on the development PC (Windows, 8 threads), 2026-11-21. fleet-runtime's own `Fleet` with 200 scripted bots, fifty on each of the reference's four examples, and ten step-mode bots that jump every 250 ms; the sessions are the fake; the server "talks" three lines a second to every bot. A share of the scripted bots is put on **the hot script:** four hooks a second, each a loop of an array's method that calls a function of the script's, the costliest kind of operation the engine has (ADR-0040, A1), calibrated at the test's start to take **2.8 ms** alone (135 rounds, 1,659 operations a hook): 11 ms of a second's 20 ms alone, and more beside others. Each phase is measured for six seconds after two to settle.

**The stream's heartbeat isn't in it, and why.** A scripted mode can't reach a managed agent before the wire carries one (P15.20), so the real stream can't run beside real scripts yet. What the stream's heartbeat is to the runtime is a task with a timer; the test has such a task, beating every 250 ms and asking the supervisor what the agent's own heartbeat asks it (`Fleet::snapshot_all`). The stream's interval is 10 s and its timeout 30 s.

**A machine's own timers.** With nothing to do, a timer on this PC fires 4 to 16 ms late. Every "late" below includes that.

**The examples alone** (200 bots, no hot script), the same on every runtime:

| | |
|---|---|
| Hooks | 175 a second, all 200 bots together |
| A hook took | 5 to 7 µs at the median, 10 to 33 µs at the 95th percentile, **0.13 ms at most** |
| The most operations in a hook | **43** |
| All hooks together | 1 to 2 ms of work a second: 0 to 1 % of one core, with the supervisor, the actors and the fake sessions |
| A hook waited for its turn | 0.13 to 0.28 ms at the median, 0.8 ms at most (every bot joined in the same instant, so their timers come due together) |
| The supervisor's answer to the heartbeat | 13 to 19 µs at the median, 0.23 ms at most, at every load below too |
| Memory, the whole process with 210 bots | 24 to 30 MiB |
| Stopped | None, in any phase of any run |

**With a share of the bots on the hot script.** "As they come due" is the runner as first built (one hook a turn, and the worker given back after each, R9; no bound on how many run at once); "in turns" is as it is now. Late is the longest of the phase; a timer's hook is given as median and longest. The steps are those of the ten step-mode bots, 220 to 230 due in a phase.

*One worker* (one hook at a time either way; the turn is what keeps the others out of the queue):

| Hot | Hooks' work | Heartbeat late | A step late | A timer's hook late |
|---|---|---|---|---|
| | | as they come due / in turns | as they come due / in turns | as they come due / in turns |
| 0 % | 1 ms/s | 10 ms / 8 ms | 11 ms / 12 ms | 6, 7 ms / 6, 14 ms |
| 10 % | 0.24 s/s | 12 ms / 11 ms | 6 ms / 11 ms | 6, 14 ms / 6, 16 ms |
| 25 % | 0.58 s/s | 14 ms / 11 ms | 13 ms / 6 ms | 5, 21 ms / 5, 14 ms |
| 50 % | 1.0 s/s, all of the worker | **56 ms** / 7 ms | **56 ms** / 9 ms | 49, 136 ms / 40, 63 ms |
| 100 % | 1.0 s/s, all of the worker | **780 ms, and 11 of 24 beats came** / **9 ms, all 24** | **575 ms, and 80 steps came** / **10 ms, all 230** | 344, 851 ms / 329, 373 ms |

*Two workers* (in turns: one hook at a time):

| Hot | Hooks' work, as they come due / in turns | Heartbeat late | A step late | A timer's hook late |
|---|---|---|---|---|
| 0 % | 1 ms/s | 11 ms / 9 ms | 11 ms / 11 ms | 6, 13 ms / 6, 14 ms |
| 10 % | 0.28 / 0.23 s/s | 8 ms / 13 ms | 17 ms / 12 ms | 6, 19 ms / 6, 20 ms |
| 25 % | 0.62 / 0.58 s/s | 11 ms / 7 ms | 11 ms / 3 ms | 5, 18 ms / 5, 12 ms |
| 50 % | 1.2 / 1.0 s/s | 18 ms / 5 ms | 18 ms / 6 ms | 5, 19 ms / **38, 65 ms** |
| 100 % | 2.0 / 1.0 s/s | **88 ms** / **3 ms** | **92 ms, and 187 steps came** / **6 ms, all 230** | 57, 99 ms / **322, 344 ms** |

*Four workers* (in turns: three hooks at a time; the hot scripts never ask for more than two and a half workers):

| Hot | Hooks' work | Heartbeat late | A step late | A timer's hook late |
|---|---|---|---|---|
| 0 % | 1 to 2 ms/s | 7 ms / 9 ms | 5 ms / 11 ms | 6, 8 ms / 6, 13 ms |
| 10 % | 0.29 / 0.27 s/s | 12 ms / 13 ms | 15 ms / 11 ms | 6, 18 ms / 5, 12 ms |
| 25 % | 0.69 / 0.61 s/s | 10 ms / 7 ms | 11 ms / 17 ms | 6, 26 ms / 5, 31 ms |
| 50 % | 1.25 / 1.22 s/s | 10 ms / 12 ms | 20 ms / 10 ms | 5, 23 ms / 5, 17 ms |
| 100 % | 2.5 / 2.5 s/s | 20 ms / 3 ms | 20 ms / 3 ms | 5, 25 ms / 5, 24 ms |

**What the tables say:**
- **The reference's examples are free:** a millisecond or two of work a second for two hundred bots, and nothing else on the runtime notices them, with or without turns.
- **As they come due, hooks are harmless until their work reaches what the workers have, and unbounded after.** At that point every task of the runtime waits behind one hook of every script that's due: 0.78 s for a heartbeat on one worker, and the step-mode bots, whose late steps aren't made up for, lost two thirds of theirs. No bot went offline and no question to the supervisor went unanswered; the stream's heartbeat, at 10 s with a timeout of 30 s, would have survived this fleet. But the wait grows with the number of scripts, and nothing in the runner bounded it.
- **In turns, what isn't a hook is on time at every share on every runtime:** a heartbeat 13 ms late at most and a step 17 ms, which is what this machine's timers do with nothing running (the 0 % rows).
- **The scripts pay for it, and only the ones that ask for more than the turns give.** On two workers, the hot scripts get one worker's time instead of two: at 50 % their hooks begin 38 ms late where they were on time, at 100 % a third of a second late where it was 57 ms. That's a hundred to two hundred scripts, each at the edge of its own budget.
- **A hook's time depends on what runs beside it.** The hot hook takes 2.8 ms alone, 3.0 to 3.4 ms at the median beside others on four workers, and up to 4.8 ms. In an earlier run it was set to 3.4 ms alone: with three at a time, every one of two hundred crossed a hook's 5 ms and was stopped, as a hook past its time is. In the runs above a handful of the two hundred hot scripts (2 to 7) stopped for the second's 20 ms, and one for a hook's 5 ms. A script that plans to the edge of its budget stops on a busy agent; the reference's examples are four hundred times under it.
- **A hook's own longest times** (15 to 21 ms, a few in thousands) are the OS setting the thread aside: the instance counts such a gap as 1 ms (ADR-0040, A1), and those hooks ran on.

## The budgets (P15.19)
ADR-0036's D3 and ADR-0040's A1 left four numbers to this test.

| Budget | | The examples | The hot script | |
|---|---|---|---|---|
| Operations a hook | 10,000 | 43 at most | 1,659 | **Stays** |
| Operations a second | 30,000 | A few hundred | 6,636 | **Stays** |
| A hook's time | 5 ms | 0.13 ms at most | 2.8 ms alone, up to 4.8 ms beside others | **Stays** |
| A second's time | 20 ms | Under 0.1 ms | 11 to 14 ms; a few of 200 crossed it | **Stays** |

- **The operation budgets are far from what a real script needs** and don't bound a hook's time (A1); they stay as the bound on a loop that never ends.
- **The time budgets bound one script, and they held:** no hook ran on past 5 ms, no script past 20 ms in a second; the ones that crossed were stopped.
- **They don't bound a fleet,** and they aren't meant to: 200 scripts at 20 ms are four workers' whole time. What bounds a fleet's hooks now is the turnstile, at the workers less one. So the two time budgets are a script's own allowance and no longer what the runtime's other tasks depend on, which is why they can stay where A1 put them.

## The snapshot's cost on the host thread
`load_a_snapshots_cost_on_the_host_thread`, in fleet-mc's own scene (the session's real World, the facts reader as a hook's snapshot calls it): the cube of 729 block places around the bot, a quarter of it in a loaded chunk of stone and cobblestone, and 200 entities the bot knows, 64 of them within range.

| | A snapshot |
|---|---|
| A release build | **30 µs** |
| A debug build | 236 µs |

A game tick is 50 ms. The examples' 175 hooks a second over 200 bots are one snapshot a bot every 1.1 s: 30 µs of each bot's own host thread in a second. A script with a timer every 250 ms, the shortest there is, takes four snapshots a second: 0.12 ms of its host thread's second. The cost is the entities': the reader looks at every entity the bot knows to find those in range.

## The tests, and what showed them red
The runner's architecture had to stand before its component tests could be written, so they weren't written first. Each property the reviewer named was then shown red by a temporary change to the code, restored afterwards:

| The change | Red |
|---|---|
| The actor doesn't release after a runner that panicked | `a_runner_that_panics_leaves_nothing_held` |
| A Stop cancels the runner at once | `a_stop_runs_on_stop_and_releases_everything_before_the_session_goes`, `on_stops_one_message_goes_out_before_the_session_ends` |
| A window opens when the hook asks | `a_command_the_session_doesnt_take_opens_no_window` |
| A command the queue refuses is treated as sent | `a_command_the_rate_limit_refuses_opens_no_window`, `a_command_the_full_queue_refuses_opens_no_window` |
| A script's log line is logged at `info`; so is why it stopped | `a_scripts_text_is_logged_only_at_debug`, each |
| The instance gets a stopwatch of its own, not the fleet's | `a_script_that_hits_a_limit_stops_with_everything_released` |
| The mode's guards aren't set at a join | `a_join_sets_the_modes_guards_takes_a_snapshot_and_runs_on_join` |
| The runner never wakes by itself | `timers_and_hold_for_run_on_tokios_time`, `a_reply_after_its_window_is_nothing_and_the_log_says_it_ran_out` |
| The actor doesn't end the run at a Stop | `a_stop_while_the_bot_is_offline_ends_the_scripts_run_too` |
| A session's end isn't told to the instance | `after_a_disconnect_nothing_is_held_in_the_next_session_either` |
| The runner doesn't give its worker back after a hook | `the_tasks_that_wait_for_a_worker_get_it_between_two_hooks_that_are_due_at_once` |
| A turn is given back before the yield, or never taken | `what_else_is_due_waits_for_one_hook_not_for_one_of_every_script` |

One change turned nothing red and was removed instead: a second `ReleaseAll` at an orderly end, which the actor's own already covers. fleet-script's two changes (`Ran::refused`, one timer a wake) and the turnstile's own tests were written first and seen red.

## Put to the user
**Hooks take turns on the async workers; they don't move to a thread pool** (R10). The reviewer's remedy, should the workers not take it, was a pool. I measured that they don't take it as first built, and that with turns they do, without a pool's threads and handoffs. If the pool is wanted all the same, it replaces what a turn does and nothing around it.

## Known limits
- **The agent's stream itself isn't measured** beside scripts (above). Group E can run this test's fleet beside a real stream once a scripted mode travels.
- **A hook's snapshot is as old as its wait for a turn** (R10).
- **What a hook that fails wrote into its log is lost** with everything else it asked for (ADR-0040): the app shows the error and its line.
- **`on_stop` is cut after 2 s** on a session that doesn't answer; the release then comes from the session's end.
- **A script's status isn't in the bot's snapshot.** It's an event, said again at every join, and `ScriptSlot::status()`; how it reaches the app is group E's (P15.23).

## What group E inherits
- **The server's chat formats** belong in a bot's spec (P15.34); the actor passes an empty list until then (`effects.rs`, `start_mode`).
- **`FleetEventKind::Script`** is what the wire's status and log messages carry (P15.20, P15.23); the conductor drops it today.
- **The agent's own check of a script** (P15.22) can share `ScriptPrograms`' compile, or compile itself; either way a script the runtime is handed is compiled once a fleet.
- **A standalone agent's script file** (P15.22) needs nothing new here: a scripted `ModeDefinition` in a bot's spec runs.
