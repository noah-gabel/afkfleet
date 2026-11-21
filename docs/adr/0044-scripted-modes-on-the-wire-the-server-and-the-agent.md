# 0044. Scripted modes on the wire, the server and the agent: who writes one, what's checked at save, never below an agent's level, and what an agent reports

- **Status:** Accepted. Decided under ADR-0025
- **Date:** 2026-11-21
- **Related:** Plan.md Phase 15, group E (P15.20 to P15.23, P15.34); [ADR-0036](0036-scripted-modes.md) (D13 to D15), [ADR-0037](0037-scripts-read-system-chat.md) (C2, C5, C7), [ADR-0040](0040-fleet-script-as-built.md), [ADR-0042](0042-scripted-inputs-in-fleet-mc.md) and [ADR-0043](0043-the-script-runner-and-hooks-on-the-async-workers.md), whose decisions this builds; ADR-0010 (unknown JSON), ADR-0019 (an agent is trusted in nothing), ADR-0029 (the authorization matrix); `crates/fleet-proto/src/convert/script.rs`, `crates/fleet-core/src/placement.rs`, `crates/fleet-core/src/script/report.rs`, `crates/fleet-server/src/app/modes.rs`, `crates/fleet-server/src/app/agents/script_gate.rs`, `crates/fleet-server/src/grpc/registry.rs`, `crates/fleet-agent/src/config/`

## Context
Groups A to D built a scripted mode everywhere but on its way: fleet-core reads one, fleet-script runs one, fleet-mc moves the bot, fleet-runtime runs the hooks. Nothing carried a script from the app to an agent, and group A's guard read a stored scripted mode as unreadable so none could leak out as a mode without steps. Group E carries it: the wire, the server's API and its placement, the agent's own check and its reports, and a server's chat formats beside a bot's spec. ADR-0036's D13 to D15 and ADR-0037's C2, C5 and C7 decided what; this records how, and what the user's reviewer asked to see.

## Decisions

### E1. The wire grows without a new protocol version
- `ModeDefinition.script` (with its settings and every declaration), `Hello.script_api`, `BotSpec.chat_formats`, and `BotEvent`'s `script_status` and `script_log` are new fields and messages; `PROTOCOL_VERSION` stays 1. Every addition is one an older reader skips. That would be the silent failure D13 rules out, except that the server sends a script only to an agent whose hello names a level that runs it (E3).
- **`Hello.script_api` is a plain number:** 0, or absent, is an agent from before scripts; a level this server doesn't know is kept as it is, since an agent newer than its server still runs the levels it knows.
- **A script crosses only in its exact form.** The server writes it from fleet-core's checked `Script`, and the agent reads it back through the same rules; anything else is refused as `NotExact`, which ends the stream as every unreadable message does: it can come only from a bug or a hostile server.
- **Two bodies in one mode** (steps and a script) are refused on the wire as at the API (`TwoBodies`).

### E2. A scripted spec's size is bounded
A script's text is at most 16 KiB (D14), but its declarations could each be as long as a message's own bound: sixteen messages and eight patterns at their caps would make a spec of about 0.66 MB, and a full reconcile of 1,000 bots far more than the stream's 128 MiB. So fleet-core bounds **a mode's declarations at 16 KiB of text together** (`Script::MAX_DECLARED_BYTES`, `ScriptError::DeclarationsTooLarge`). A scripted spec is then at most 48 KiB, and fleet-proto's limits test holds the largest scripted mode, the largest chat formats and a reconcile of the most bots under the stream's bounds. The reference's examples declare a few hundred bytes.

### E3. Never below the level the mode needs, on every path
fleet-core's `ScriptApiLevel::runs_on(agent)` is the one rule; each path asks it:

| Path | What happens | Held by |
|---|---|---|
| A user sets the mode on a bot whose agent's live stream runs a lower level | **409 `agent_too_old`**, nothing written | `app/bots/settings.rs`; `tests/auth/modes/scripted.rs` |
| An Admin edits a mode into a script while a bot that uses it is on such an agent | **409 `agent_too_old`**, the mode as it was | `app/modes.rs`; the same file |
| Placement | A waiting scripted bot goes only to an agent at its level; an agent before scripts still gets step modes' bots | fleet-core's placement (and its proptest `no_agent_is_ever_sent_a_bot_whose_mode_needs_a_level_it_lacks`); the reconciler's `a_scripted_bot_is_placed_only_on_an_agent_that_runs_its_level` |
| A reconnect whose hello lists the bot (a resync) | Left out of the welcome's full reconcile, so the agent removes it, and unassigned as `agent_too_old`: no spec of its mode, not even one that stops it | `an_agent_that_comes_back_older_gets_no_spec_of_its_scripted_bot_and_lets_it_go` |
| A reconnect whose hello doesn't list it | Freed at once and placed on an agent that runs it | `a_scripted_bot_whose_agent_reconnects_older_without_it_moves_to_one_that_runs_it` |
| An agent replaced by an older one while the bot runs | As a resync: no spec, unassigned | `a_scripted_bots_agent_downgraded_while_it_runs_never_gets_its_spec` |
| A mode that becomes a script's while its bot runs on such an agent | An unassign, which carries no spec, never an update that would carry the script | `a_mode_that_becomes_a_script_while_its_bot_runs_on_an_older_agent_unassigns_it` |
| An assignment its agent never got, sent again | Logged as `agent_too_old`, never sent | fleet-core's placement tests |
| **The registry, last** | It refuses to queue any message whose spec needs a level the stream's hello lacks (`SendError::AgentTooOld`), whatever its caller decided | `a_spec_whose_mode_needs_a_level_the_stream_lacks_is_never_queued` |

Each case runs with an agent whose hello has no `script_api`. Over the real gRPC port, `an_agent_whose_hello_has_no_script_api_is_never_given_a_scripted_bot` holds the session's reading of the hello. **An agent that's offline** when the mode is set or edited is no refusal: the reconciler holds it to its level at its next hello.

### E4. Who may write one (D15)
- `Permission::CreateMode` and `UpdateMode` carry the mode's kind (`ModeKind::Steps` or `Script`). A script is an Admin's or the Owner's to create or edit, private or shared; an edit is asked for the mode as it is and as it will be, so a Member can't turn their step mode into a script, nor a script into anything. `DeleteMode` and `SetBotMode` are unchanged: **a Member runs a shared scripted mode on a bot they control**, with Manage when a declared command is off the allowlist, which reads a script's declared messages as it reads a step's chat.
- **The handler asks with the kind the request names, before anything reads the script,** so a Member's request is a 403 that no rule, no validator and no compiler saw: a body that isn't a program, or breaks a rule, is the same 403.
- The authorization matrix (ADR-0029) has rows for both kinds of create, an edit into a script (one's own, and a Member's shared mode) and setting a shared scripted mode on a bot.

### E5. What's checked at save (D14)
1. **fleet-core's rules,** each a 422 at its own field under `script` (`script.leash`, `script.messages[1].key`, `script.patterns[0].text`), with a line and a column where the rule names a place in a text (`FieldError.line`/`column`). Steps beside a script are a 422 at `steps`.
2. **The compile,** fleet-script's own (the engine an agent runs, with its limits), in `spawn_blocking` and before the write transaction opens. It parses and checks the script; **it never runs a hook** (a hook that would never end is saved at once). A text that isn't a program is a 422 at `script.source` with Rhai's message, its line and its column: the caller's own script, which they wrote. A compiler that fails (a panic, a lost task) is an internal fault: the generic 500 with its request ID, nothing of what it said in the answer.
3. Then the route's allowlist check, the version, the name and the bots, as for steps.

The server sets the level, the newest it knows; a request has no `api`.

### E6. What a viewer sees, and the audit entry
- **The answer** carries `script` beside empty `steps`: the source, the settings, the declarations, the level, and `any_time_chat` when a pattern counts at any time (C5). Below the right to edit the mode, a declared command is its `/` and name alone, `arguments_hidden`, as a chat step's is (S15). The source is shown whole: it can send only what's declared.
- **The audit entry** (`mode.create`, `mode.update`) says `body`: `steps` or `script`, and for a script `any_time_patterns` (C5) and `script_api`; never its text.

### E7. Group A's guard goes
A stored scripted mode of a level this version knows reads as itself; one of a level it doesn't know (a newer server's, after a downgrade) is still unreadable, left alone and logged, never run in part (ADR-0010). The guard's tests were replaced by tests of the real path (`a_stored_scripted_mode_reads_back_as_itself`, `a_bot_on_a_scripted_mode_reads_with_its_script`).

### E8. A server's chat formats in a bot's spec (P15.34, C2)
The fleet's read carries each server's formats beside its conflict texts, and every spec carries them, as the chat rate is carried, so the agent can tell a player's line from the server's. A row whose formats don't read back leaves its bots alone, as one whose texts don't (logged by its row, with the column): a bot is never sent no formats, which would let a script read a player's line as the server's. The runner reads them through a watch, so a change applies within the session.

### E9. The agent checks again and reports
- **Its own compile refuses per bot** (`Refused`, with the error and its line), its stream staying up (D13).
- **What it reports** is fleet-core's `ScriptState` (`Running`, `Stopped` or `Refused` with a `ScriptProblem`: its kind, the hook, the place and the engine's message; `Crashed`) and `ScriptLogLine` (a kind and a text). Each text is a `ReportText`: cleaned as untrusted text (control characters, formatting codes and text-direction marks out) and cut at 200 characters. A sent message is named by its key alone, never with its text or values, which only Manage may read (S6, S9).
- **Its outbound** keeps each bot's latest script status, sent after the bot's own status and death and never dropped for chat; a log line shares chat's class, the oldest dropped first.

### E10. What an agent reports is untrusted on the server (P15.23)
- **Cleaned at the edge:** the conversion cleans every text again as it reads it, whatever the agent sent, and refuses a place that isn't a line and a column from 1 together.
- **Bounded per bot:** a gate in the ingest (`ScriptGate`, the generic cell rate on the server's own clock, shared by every stream, so a reconnect resets nothing) takes a bot's status at most 10 at once and one more every 10 s, and its log lines 30 at once and one more a second. What's over is dropped and counted (`afkfleet_script_reports_dropped_total{kind}`); a running script says its status again at its next join, so a dropped one heals. It keeps a bot's buckets only while they aren't full again and sweeps past 4,096 bots.
- **Never stored:** they're published on the bus, and **the live events send them only to a viewer who may see the bot** (`bot_script`, `bot_script_log`), as plain text, checked when the event is taken and again when its frame is built.

### E11. A standalone agent's script file (P15.22)
A `[[standalone.bots]]` entry takes `script = { file = "/abs/path.rhai", … }` in place of `mode`: the settings and declarations under the API's names (`leash`, `tool_guard`, `block_guard`, `messages`, `patterns`; the first two default to 8 and 2), at the newest level. The file is read once, at load, before the runtime starts, never more than one byte past 16 KiB; a Windows file's CR LF reads as line breaks. Its problems are named at their keys and never repeat the file's text: both `mode` and `script`, a path that isn't absolute, a file that's missing, too long or not UTF-8, and fleet-core's rules. The runtime compiles it at the bot's start and refuses it there, logged, if it isn't a program. A standalone agent has no server's chat formats.

### E12. The real stream beside real scripts (the user's check)
`load_scripted_bots_on_the_real_stream` runs 200 bots on one managed agent through the loopback harness, their modes set through the API, in four phases: every bot on `afk`, on the reference's four examples, a quarter of them on P15.19's hot script, and all of them on it. The agent sends a heartbeat when it has sent nothing else for the server's interval, so a silence on its stream longer than the interval is how late the heartbeat came; the harness's proxy times every write the agent makes (TLS hides what a write holds; HTTP/2's pings are moved out of the measured window). The interval is 1 s, the least the server takes, for twenty silences a phase. The server, the agent and the harness share one runtime of four workers.

**On the development PC** (Windows, a debug build, 2026-11-21):

| Phase | Hooks in 20 s | CPU | The longest silence | Scripts stopped |
|---|---|---|---|---|
| Every bot on `afk` | 0 | 5 % of one core | 1.017 s | 0 |
| The four examples | 3,526 | 21 % | 1.018 s | 0 |
| A quarter hot | 6,525 | 71 % | 1.012 s | 2 |
| All hot | 15,906 | 238 % | 1.016 s | 0 |

**The heartbeat isn't later beside scripts than without them:** at most 17 ms past the interval in every phase, which is this PC's own timer step (15.6 ms on Windows), already there with no script at all. No bot went offline and the stream never ended. The arm64 run's numbers are in [ADR-0043](0043-the-script-runner-and-hooks-on-the-async-workers.md#what-was-measured-on-linuxarm64).

## Deviations
- **The bot's page saying its agent must be updated (D13)** is left to the bot's page itself (P15.26, group F): a scripted bot that no agent can run waits as `awaiting_agent`, and the API's own reason for it comes with the text that shows it. Setting the mode on a bot whose agent is live and too old already answers 409 `agent_too_old`, with the message "This bot's agent runs a version that can't run this scripted mode. Update the agent first."

## Known limits
- **The script's source is shown to every viewer of a mode,** as written; only declared commands' arguments are hidden below the right to edit. A script can send only what it declares.
- **Reports over a bot's rate are lost,** not queued: the log is a view of what a script does, not its record.
- **The levels are compared by the hello alone:** an agent that claims a level it doesn't run refuses each script with its own compile (E9), as any agent refuses one its engine doesn't take.
