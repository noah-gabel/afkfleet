// A bot's script as the live events report it (Plan.md P15.26; ADR-0044, E10; ADR-0045): its
// status, the newest alone, and its log, the newest lines last. Pure.
//
// The server stores none of it: it reaches the app only on the live events (`bot_script`,
// `bot_script_log`), to a viewer who may see the bot. So the app keeps each bot's record in the
// query cache, under its own key, for as long as the session: whether its screen is open or not,
// since a status is said only at a change and at each join. A `bot_removed` resets it, and the
// session's end clears the cache with it.
import type { ScriptLogLine } from "../generated/ScriptLogLine";
import type { ScriptStatus } from "../generated/ScriptStatus";

/** The most log lines kept for a bot: the newest. */
export const SCRIPT_LOG_MAX = 200;

/** A log line as the app keeps it: numbered in the order it came, for its place in a list. */
export interface LoggedLine extends ScriptLogLine {
  readonly id: number;
}

/** A bot's script, as the live events reported it. */
export interface ScriptRecord {
  /** The newest status, or none yet. */
  readonly status: ScriptStatus | null;
  /** The log, oldest first. */
  readonly log: readonly LoggedLine[];
  /** The next line's number. */
  readonly next: number;
}

/** Nothing reported yet. */
export const EMPTY_SCRIPT_RECORD: ScriptRecord = { status: null, log: [], next: 0 };

/** Whether `data` is a script record: what the cache holds under a script's key. */
export function isScriptRecord(data: unknown): data is ScriptRecord {
  return (
    typeof data === "object" &&
    data !== null &&
    "status" in data &&
    "log" in data &&
    "next" in data &&
    Array.isArray(data.log) &&
    typeof data.next === "number"
  );
}

/** `record` with `status` as its newest. */
export function withStatus(record: ScriptRecord, status: ScriptStatus): ScriptRecord {
  return { ...record, status };
}

/** `record` with `line` last, the oldest dropped past the bound. */
export function withLine(record: ScriptRecord, line: ScriptLogLine): ScriptRecord {
  const logged: LoggedLine = { id: record.next, kind: line.kind, text: line.text };
  return { ...record, log: [...record.log, logged].slice(-SCRIPT_LOG_MAX), next: record.next + 1 };
}
