import { describe, expect, it } from "vitest";

import type { ScriptLogLine } from "../generated/ScriptLogLine";
import {
  EMPTY_SCRIPT_RECORD,
  isScriptRecord,
  SCRIPT_LOG_MAX,
  withLine,
  withStatus,
} from "./scriptReports";

const line = (n: number): ScriptLogLine => ({ kind: "line", text: `line ${n}` });

describe("a bot's script record (P15.26)", () => {
  it("keeps the newest status alone", () => {
    const running = withStatus(EMPTY_SCRIPT_RECORD, { state: "running" });
    const stopped = withStatus(running, {
      state: "stopped",
      problem: { kind: "error", hook: "on_join", line: 3, column: 5, message: "boom" },
    });

    expect(running.status).toEqual({ state: "running" });
    expect(stopped.status?.state).toBe("stopped");
    expect(stopped.log).toEqual([]);
  });

  it("keeps the log's lines in their order, the newest last, at most its bound", () => {
    let record = EMPTY_SCRIPT_RECORD;
    for (let n = 1; n <= SCRIPT_LOG_MAX + 5; n += 1) {
      record = withLine(record, line(n));
    }

    expect(record.log).toHaveLength(SCRIPT_LOG_MAX);
    expect(record.log[0]).toEqual({ id: 5, ...line(6) });
    expect(record.log.at(-1)).toEqual({ id: SCRIPT_LOG_MAX + 4, ...line(SCRIPT_LOG_MAX + 5) });
  });

  it("knows a record from anything else in the cache", () => {
    expect(isScriptRecord(EMPTY_SCRIPT_RECORD)).toBe(true);
    expect(isScriptRecord(withLine(EMPTY_SCRIPT_RECORD, line(1)))).toBe(true);
    expect(isScriptRecord(undefined)).toBe(false);
    expect(isScriptRecord({ log: "x", status: null, next: 0 })).toBe(false);
    expect(isScriptRecord({ log: [], status: null })).toBe(false);
  });
});
