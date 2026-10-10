// Tests for demo-agent.mjs. Run with `just scripts-test`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

import {
  botNames,
  composeVersion,
  cpuModel,
  formatSummary,
  healthySince,
  isNewEnough,
  minimumMinutes,
  parseArgs,
  parseInspect,
  parseLog,
  parseMemoryMiB,
  parseState,
  parseStatsOutput,
  readStackChecks,
  reconnectDeadlineMs,
  reconnects,
  restartErrors,
  retryWindow,
  shutdownReport,
  statsSummary,
  timeline,
  unexpectedLines,
  verdict,
  warnErrorCounts,
} from "./demo-agent.mjs";

const STACK_CHECKS = readStackChecks(
  readFileSync(new URL("../deploy/dev/stack-checks.json", import.meta.url), "utf8"),
);

/** A small stack-checks.json: windows 5–10 s, 10–20 s, then capped at 15–30 s. */
const CHECKS = readStackChecks(
  JSON.stringify({
    about: "test",
    expected_warnings: [
      {
        target: "azalea_client::plugins::join",
        message_prefix: "failed to create connection",
        why: "the server is down",
      },
    ],
    expected_restart_errors: [],
    retry_windows: [
      { attempt: 1, min_ms: 5000, max_ms: 10000 },
      { attempt: 2, min_ms: 10000, max_ms: 20000 },
      { attempt: 3, min_ms: 15000, max_ms: 30000 },
    ],
    connect_timeout_ms: 30000,
    stop_grace_period_ms: 20000,
  }),
);

const T0 = "2026-10-10T08:00:00";

/** One of the agent's JSON lines, at `T0` plus `secs`. */
function line(secs, level, target, message, fields = {}, botId = undefined) {
  const at = new Date(Date.parse(`${T0}Z`) + secs * 1000).toISOString().replace("Z", "123Z");
  const span = botId === undefined ? {} : { span: { bot_id: botId, name: "bot" } };
  return JSON.stringify({ timestamp: at, level, message, ...fields, target, ...span });
}

function starting(secs, botId, username) {
  return line(secs, "INFO", "fleet_agent::run", "starting a standalone bot", {
    bot_id: botId,
    username,
    server: "minecraft:25565",
    mode: "afk",
  });
}

function state(secs, botId, debug) {
  return line(
    secs,
    "INFO",
    "fleet_runtime::actor::effects",
    "the bot's state changed",
    { state: debug },
    botId,
  );
}

function at(secs) {
  return Date.parse(`${T0}Z`) + secs * 1000;
}

describe("parseArgs", () => {
  it("runs 60 minutes by default", () => {
    assert.deepEqual(parseArgs([]), { minutes: 60 });
  });

  it("takes the minutes from --minutes", () => {
    assert.deepEqual(parseArgs(["--minutes", "8"]), { minutes: 8 });
  });

  it("rejects anything else with a usage error", () => {
    for (const args of [
      ["--minutes"],
      ["--minutes", "0"],
      ["--minutes", "1.5"],
      ["8"],
      ["--hours", "1"],
    ]) {
      assert.ok(parseArgs(args).error, JSON.stringify(args));
    }
  });
});

describe("composeVersion", () => {
  it("reads major and minor from `docker compose version --short`", () => {
    assert.deepEqual(composeVersion("5.5.1\n"), [5, 5]);
    assert.deepEqual(composeVersion("v2.24.0"), [2, 24]);
    assert.deepEqual(composeVersion("2.24.0-desktop.1"), [2, 24]);
    assert.equal(composeVersion("compose"), null);
  });

  it("needs 2.24 or newer, comparing major before minor", () => {
    assert.equal(isNewEnough([5, 5]), true);
    assert.equal(isNewEnough([2, 24]), true);
    assert.equal(isNewEnough([2, 23]), false);
    assert.equal(isNewEnough(null), false);
  });
});

describe("the retry windows", () => {
  it("are read from stack-checks.json", () => {
    assert.deepEqual(retryWindow(STACK_CHECKS, 1), { minMs: 5000, maxMs: 10000 });
    assert.equal(STACK_CHECKS.connectTimeoutMs, 30000);
    assert.equal(STACK_CHECKS.stopGracePeriodMs, 20000);
    assert.ok(STACK_CHECKS.expectedWarnings.length > 0);
    assert.ok(STACK_CHECKS.expectedRestartErrors.length > 0);
  });

  it("use the last window for every later attempt", () => {
    assert.deepEqual(retryWindow(CHECKS, 2), { minMs: 10000, maxMs: 20000 });
    assert.deepEqual(retryWindow(CHECKS, 9), { minMs: 15000, maxMs: 30000 });
  });

  it("give a reconnect the backoff, one more failed attempt and a join, plus a second", () => {
    // 10 s + 30 s + 20 s + 30 s + 1 s.
    assert.equal(reconnectDeadlineMs(CHECKS, 1), 91000);
    // The real windows: 40 s + 30 s + 80 s + 30 s + 1 s.
    assert.equal(reconnectDeadlineMs(STACK_CHECKS, 3), 181000);
  });

  it("need half the run to cover the deadline for attempt 3 plus the agent's stop", () => {
    // (181 s + 20 s) × 2 = 402 s, rounded up to whole minutes.
    assert.equal(minimumMinutes(STACK_CHECKS), 7);
  });
});

describe("parseLog", () => {
  it("reads the agent's JSON lines and keeps any other line apart", () => {
    const text = `${starting(0, "b-4", "AfkBot4")}\n\nWARN[0000] not JSON\n`;

    const { lines, invalid } = parseLog(text);

    assert.equal(lines.length, 1);
    assert.equal(lines[0].message, "starting a standalone bot");
    assert.equal(lines[0].level, "INFO");
    assert.equal(lines[0].target, "fleet_agent::run");
    assert.equal(lines[0].at, at(0));
    assert.equal(lines[0].fields.username, "AfkBot4");
    assert.deepEqual(invalid, ["WARN[0000] not JSON"]);
  });

  it("takes a state line's bot from its span", () => {
    const { lines } = parseLog(state(1, "b-4", "Backoff { attempt: 2 }"));

    assert.equal(lines[0].botId, "b-4");
  });
});

describe("parseState", () => {
  it("reads the state's name and attempt from its Debug text", () => {
    assert.deepEqual(parseState("Backoff { attempt: 12 }"), { name: "Backoff", attempt: 12 });
    assert.deepEqual(parseState("Online { since: 2026-10-10T08:00:05.12Z, attempt: 2 }"), {
      name: "Online",
      attempt: 2,
    });
    assert.deepEqual(parseState("Stopped"), { name: "Stopped", attempt: null });
  });
});

describe("the log's summary", () => {
  const text = [
    starting(0, "b-4", "AfkBot4"),
    line(0.5, "INFO", "fleet_agent::run", "the agent is running", { bots: 1 }),
    state(1, "b-4", "Connecting { attempt: 1, auth_retried: false }"),
    state(2, "b-4", "Online { since: 2026-10-10T08:00:02.123456789Z, attempt: 1 }"),
    state(100, "b-4", "Backoff { attempt: 1 }"),
    line(
      100,
      "WARN",
      "fleet_runtime::actor::effects",
      "the session ended; the bot connects again",
      {},
      "b-4",
    ),
    state(107, "b-4", "AwaitingSession { attempt: 2, fresh: false }"),
    line(107.1, "WARN", "azalea_client::plugins::join", "failed to create connection: refused"),
    state(107.2, "b-4", "Backoff { attempt: 2 }"),
    state(121, "b-4", "AwaitingSession { attempt: 3, fresh: false }"),
    state(121.5, "b-4", "Online { since: 2026-10-10T08:02:01.5Z, attempt: 3 }"),
    line(130, "ERROR", "fleet_runtime::actor", "something broke", {}, "b-4"),
    line(200, "INFO", "fleet_agent::run::finish", "shutting down", { signal: "SIGTERM" }),
    line(201, "INFO", "afkfleet_agent", "the agent stopped", {
      exit_code: 0,
      stopped: 1,
      aborted: 0,
      crashed: 0,
    }),
  ].join("\n");
  const { lines } = parseLog(text);
  const names = botNames(lines);

  it("knows each bot's username by its ID", () => {
    assert.deepEqual(names, new Map([["b-4", "AfkBot4"]]));
  });

  it("lists every state change in order, without Online's since", () => {
    const changes = timeline(lines, names);

    assert.equal(changes.length, 7);
    assert.deepEqual(changes[1], {
      at: at(2),
      username: "AfkBot4",
      state: "Online { attempt: 1 }",
    });
  });

  it("counts warnings and errors by level and target", () => {
    assert.deepEqual(warnErrorCounts(lines), [
      { level: "ERROR", target: "fleet_runtime::actor", count: 1 },
      { level: "WARN", target: "azalea_client::plugins::join", count: 1 },
      { level: "WARN", target: "fleet_runtime::actor::effects", count: 1 },
    ]);
  });

  it("finds every error, and every warning stack-checks.json doesn't expect", () => {
    const unexpected = unexpectedLines(lines, CHECKS.expectedWarnings).map((line) => line.message);

    assert.deepEqual(unexpected, ["the session ended; the bot connects again", "something broke"]);
  });

  it("measures each bot's reconnect after the restart against its deadline", () => {
    // The restart split the log before line 4; the server was healthy at +110 s,
    // and the bot's last backoff before then was its second: 20 s + 30 s + 30 s
    // + 30 s + 1 s from +110 s.
    const result = reconnects({ lines, names, from: 4, healthyAt: at(110), checks: CHECKS });

    assert.deepEqual(result, [
      {
        username: "AfkBot4",
        leftAt: at(100),
        onlineAt: at(121.5),
        attempts: 2,
        deadlineAt: at(110) + 111000,
        problem: null,
      },
    ]);
  });

  it("fails a reconnect after its deadline, or one that never happened", () => {
    const tight = {
      ...CHECKS,
      retryWindows: [{ attempt: 1, minMs: 1, maxMs: 1 }],
      connectTimeoutMs: 1,
    };
    const late = reconnects({ lines, names, from: 4, healthyAt: at(110), checks: tight });
    const never = reconnects({
      lines: lines.slice(0, 9),
      names,
      from: 4,
      healthyAt: at(110),
      checks: CHECKS,
    });

    assert.match(late[0].problem, /after its deadline/);
    assert.match(never[0].problem, /isn't Online again/);
  });

  it("reads the shutdown report from the last line", () => {
    assert.deepEqual(shutdownReport(lines), {
      message: "the agent stopped",
      exitCode: 0,
      stopped: 1,
      aborted: 0,
      crashed: 0,
    });
  });
});

describe("restartErrors", () => {
  const RESET = {
    target: "azalea_client::plugins::connection",
    messagePrefix:
      "Error reading packet from Client: IoError { source: Os { code: 104, kind: ConnectionReset",
    why: "a reset during the server's shutdown",
  };
  const reset = (secs) =>
    line(
      secs,
      "ERROR",
      "azalea_client::plugins::connection",
      'Error reading packet from Client: IoError { source: Os { code: 104, kind: ConnectionReset, message: "Connection reset by peer" } }',
    );
  const closed = (secs, botId) =>
    line(
      secs,
      "WARN",
      "fleet_runtime::actor::effects",
      "the session ended; the bot connects again",
      { reason: "ConnectionClosed" },
      botId,
    );

  it("allows a reset while the server restarts, once per session closed there", () => {
    const { lines } = parseLog(
      [closed(1, "b-4"), reset(1.5), closed(2, "b-5"), reset(2.5), reset(3)].join("\n"),
    );

    const allowed = restartErrors(lines, [RESET], at(0), at(10));

    assert.deepEqual(
      allowed.map((line) => line.at),
      [at(1.5), at(2.5)],
    );
  });

  it("allows no reset outside the restart, and no other error", () => {
    const { lines } = parseLog(
      [
        closed(1, "b-4"),
        reset(20),
        line(
          2,
          "ERROR",
          "azalea_client::plugins::connection",
          "Error reading packet from Client: Parse",
        ),
      ].join("\n"),
    );

    assert.deepEqual(restartErrors(lines, [RESET], at(0), at(10)), []);
  });

  it("leaves an allowed error out of the unexpected lines", () => {
    const { lines } = parseLog(
      [reset(1), line(2, "ERROR", "fleet_runtime::actor", "something broke")].join("\n"),
    );

    const unexpected = unexpectedLines(lines, [], [lines[0]]).map((line) => line.message);

    assert.deepEqual(unexpected, ["something broke"]);
  });
});

describe("docker inspect", () => {
  const json = JSON.stringify([
    {
      RestartCount: 1,
      State: {
        Running: true,
        ExitCode: 0,
        StartedAt: "2026-10-10T08:00:15.123456789Z",
        Health: {
          Log: [
            { Start: "2026-10-10T08:00:00Z", End: "2026-10-10T08:00:00.2Z", ExitCode: 0 },
            {
              Start: "2026-10-10T08:00:20.1+00:00",
              End: "2026-10-10T08:00:20.4+00:00",
              ExitCode: 1,
            },
            { Start: "2026-10-10T08:00:25.1Z", End: "2026-10-10T08:00:25.35Z", ExitCode: 0 },
          ],
        },
      },
    },
  ]);

  it("gives the state and the probes", () => {
    const state = parseInspect(json);

    assert.equal(state.running, true);
    assert.equal(state.exitCode, 0);
    assert.equal(state.restartCount, 1);
    assert.equal(state.startedAt, Date.parse("2026-10-10T08:00:15.123Z"));
    assert.equal(state.probes.length, 3);
  });

  it("is healthy from the end of the first good probe after the start", () => {
    const state = parseInspect(json);

    assert.equal(
      healthySince(state.probes, state.startedAt),
      Date.parse("2026-10-10T08:00:25.35Z"),
    );
    assert.equal(healthySince(state.probes.slice(0, 2), state.startedAt), null);
  });
});

describe("docker stats", () => {
  it("reads frames between the terminal's control codes", () => {
    const frame = (cpu, memory) =>
      `\u001b[H\u001b[2J{"CPUPerc":"${cpu}","MemUsage":"${memory} / 512MiB","Name":"agent"}\u001b[K\n`;
    const output = `${frame("1.50%", "40.5MiB")}${frame("--", "-- / --")}${frame("0.00%", "0B")}\u001b[J${frame("12.25%", "1.5GiB")}`;

    assert.deepEqual(parseStatsOutput(output), [
      { cpuPercent: 1.5, memoryMiB: 40.5 },
      { cpuPercent: 12.25, memoryMiB: 1536 },
    ]);
  });

  it("reads binary and decimal memory units", () => {
    assert.equal(parseMemoryMiB("512KiB"), 0.5);
    assert.equal(parseMemoryMiB("2GiB"), 2048);
    assert.equal(parseMemoryMiB("1048576B"), 1);
    assert.equal(parseMemoryMiB("1000kB"), 1000000 / 1048576);
    assert.equal(parseMemoryMiB("--"), null);
  });

  it("sums up the peak, the mean and the 95th percentile, with the peak's time", () => {
    const samples = Array.from({ length: 20 }, (_, index) => ({
      at: index * 1000,
      cpuPercent: index === 7 ? 50 : 1,
      memoryMiB: 30 + index,
    }));

    const summary = statsSummary(samples);

    assert.equal(summary.frames, 20);
    assert.deepEqual(summary.memory, { peak: 49, peakAt: 19000, mean: 39.5, p95: 48 });
    assert.deepEqual(summary.cpu, { peak: 50, peakAt: 7000, mean: 3.45, p95: 1 });
  });

  it("has nothing to sum up without samples", () => {
    assert.equal(statsSummary([]), null);
  });
});

describe("cpuModel", () => {
  it("prefers lscpu's model name, which also names ARM cores", () => {
    assert.equal(cpuModel("Architecture: aarch64\nModel name:   Neoverse-N1\n", ""), "Neoverse-N1");
  });

  it("falls back to /proc/cpuinfo, then to unknown", () => {
    assert.equal(cpuModel("", "processor\t: 0\nmodel name\t: AMD Ryzen 7\n"), "AMD Ryzen 7");
    assert.equal(cpuModel("", "processor\t: 0\nBogoMIPS\t: 50.00\n"), "unknown");
  });
});

describe("verdict", () => {
  const good = {
    lines: [],
    invalid: [],
    expectedWarnings: [],
    allowedErrors: [],
    reconnects: [{ username: "AfkBot4", problem: null }],
    agentRuns: 1,
    agentBefore: { startedAt: 1, restartCount: 0 },
    agentAfter: { startedAt: 1, restartCount: 0 },
    exitCode: 0,
    report: { message: "the agent stopped", exitCode: 0, stopped: 1, aborted: 0, crashed: 0 },
  };

  it("passes a clean run", () => {
    assert.deepEqual(verdict(good), { pass: true, failures: [] });
  });

  it("doesn't count an ERROR the restart allows", () => {
    const { lines } = parseLog(
      line(
        1,
        "ERROR",
        "azalea_client::plugins::connection",
        "Error reading packet from Client: IoError",
      ),
    );

    assert.deepEqual(verdict({ ...good, lines, allowedErrors: [lines[0]] }), {
      pass: true,
      failures: [],
    });
  });

  it("names every failed criterion with its lines", () => {
    const { lines } = parseLog(
      [
        line(1, "ERROR", "fleet_runtime::actor", "something broke"),
        line(2, "WARN", "fleet_mc::bridge", "a queue is full"),
      ].join("\n"),
    );

    const result = verdict({
      ...good,
      lines,
      invalid: ["not JSON"],
      reconnects: [{ username: "AfkBot4", problem: "AfkBot4 isn't Online again" }],
      agentRuns: 2,
      agentAfter: { startedAt: 2, restartCount: 1 },
      exitCode: 137,
      report: null,
    });

    assert.equal(result.pass, false);
    const byCriterion = Object.fromEntries(
      result.failures.map((failure) => [failure.criterion, failure.lines]),
    );
    assert.deepEqual(Object.keys(byCriterion), [
      "an ERROR line",
      "a warning nobody expects",
      "a bot not Online again by its deadline",
      "the agent restarted",
      "the shutdown wasn't clean",
    ]);
    assert.match(byCriterion["an ERROR line"][0], /something broke/);
    assert.match(byCriterion["a warning nobody expects"].join("\n"), /a queue is full/);
    assert.match(byCriterion["a warning nobody expects"].join("\n"), /not JSON/);
  });

  it("fails a shutdown that aborted a bot", () => {
    const result = verdict({ ...good, report: { ...good.report, stopped: 0, aborted: 1 } });

    assert.deepEqual(
      result.failures.map((failure) => failure.criterion),
      ["the shutdown wasn't clean"],
    );
  });
});

describe("formatSummary", () => {
  const report = {
    minutes: 8,
    machine: {
      architecture: "x86_64",
      cpus: 8,
      cpuModel: "AMD Ryzen 7",
      os: "Docker Desktop",
      memoryGiB: 15.5,
    },
    bots: 5,
    stats: statsSummary([{ at: 0, cpuPercent: 1, memoryMiB: 40 }]),
    restartAt: 240000,
    counts: [{ level: "WARN", target: "azalea_client::plugins::join", count: 3 }],
    changes: [{ at: 1000, username: "AfkBot4", state: "Online { attempt: 1 }" }],
    reconnects: [
      {
        username: "AfkBot4",
        leftAt: 2000,
        onlineAt: 12000,
        attempts: 2,
        deadlineAt: 99000,
        problem: null,
      },
    ],
    startAt: 0,
    allowedErrors: 1,
    healthyAt: 9000,
    exitCode: 0,
    report: { message: "the agent stopped", exitCode: 0, stopped: 5, aborted: 0, crashed: 0 },
    verdict: { pass: true, failures: [] },
  };

  it("shows the machine, the stats, the counts, the timeline, the reconnects and the report", () => {
    const text = formatSummary(report);

    for (const part of [
      "x86_64",
      "AMD Ryzen 7",
      "40.0 MiB",
      "azalea_client::plugins::join",
      "AfkBot4",
      "Online { attempt: 1 }",
      "1 connection reset",
      "stopped 5",
      "PASS",
    ]) {
      assert.ok(text.includes(part), `${part} in:\n${text}`);
    }
  });

  it("counts a single attempt in the singular", () => {
    const text = formatSummary({
      ...report,
      reconnects: [{ ...report.reconnects[0], attempts: 1 }],
    });

    assert.match(text, /\(10\.0 s, 1 attempt\)/);
  });

  it("marks a run under an hour as not the DoD run", () => {
    assert.match(formatSummary(report), /not the DoD run/);
    assert.doesNotMatch(formatSummary({ ...report, minutes: 60 }), /not the DoD run/);
  });

  it("lists each failed criterion with its lines", () => {
    const text = formatSummary({
      ...report,
      minutes: 60,
      verdict: {
        pass: false,
        failures: [{ criterion: "an ERROR line", lines: ['{"level":"ERROR"}'] }],
      },
    });

    assert.match(text, /FAIL/);
    assert.match(text, /an ERROR line/);
    assert.match(text, /\{"level":"ERROR"\}/);
  });
});
