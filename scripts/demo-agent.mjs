// Runs Phase 5's demo (Plan.md, Phase 5's DoD; ADR-0014): the compose agent's
// bots for an hour, with one Minecraft server restart at half time, then a
// summary and a verdict.
//
//   node scripts/demo-agent.mjs [--minutes <n>]      (`just demo-agent [minutes]`)
//
// It builds the agent's image and runs deploy/compose.dev.yaml as the project
// `afkfleet-demo`, with deploy/compose.isolated.yaml on top, so it can run
// beside `just mc-up`. The stack is removed at the end, also after Ctrl+C or an
// error. It judges the agent by deploy/dev/stack-checks.json, like the compose
// e2e test (crates/fleet-agent/tests/slow_compose), and records the agent's
// memory and CPU with `docker stats` for P12.2. agent.log, minecraft.log,
// stats.jsonl and summary.txt are saved under target/demo-agent/<UTC time>/.
// Only summary.txt belongs in a pull request: minecraft.log holds the bots'
// container IPs.
//
// Exits with 0 (PASS), 1 (FAIL), or 2: a usage error, an environment it can't
// run in, a run too short to judge, or an interrupted run.
// Tests: scripts/demo-agent.test.mjs.
import { execFile, spawn } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath, pathToFileURL } from "node:url";

/** The oldest Compose that knows `!reset`, which compose.isolated.yaml uses. */
export const MIN_COMPOSE = [2, 24];
/** The DoD's run: an hour. */
const DOD_MINUTES = 60;
/** The attempt the upfront minimum assumes a bot reaches during a ~30 s restart. */
const ASSUMED_ATTEMPT = 3;
/** The demo's compose project; never the dev stack's or the e2e test's. */
const COMPOSE = [
  "compose",
  "-p",
  "afkfleet-demo",
  "--file",
  "deploy/compose.dev.yaml",
  "--file",
  "deploy/compose.isolated.yaml",
];
/** How often the server's health is read after the restart. */
const POLL_MS = 250;
/** The actor's line for every state change. */
const STATE_CHANGED = "the bot's state changed";
const USAGE = "usage: node scripts/demo-agent.mjs [--minutes <whole minutes>]";

/** Reads the command line: `--minutes <n>`, 60 when left out. Returns `{ minutes }` or `{ error }`. */
export function parseArgs(args) {
  if (args.length === 0) {
    return { minutes: DOD_MINUTES };
  }
  if (args.length === 2 && args[0] === "--minutes" && /^[1-9][0-9]*$/.test(args[1])) {
    return { minutes: Number(args[1]) };
  }
  return { error: USAGE };
}

/** The `[major, minor]` in `docker compose version --short`'s output, or null. */
export function composeVersion(short) {
  const match = /^v?(\d+)\.(\d+)/.exec(short.trim());
  return match === null ? null : [Number(match[1]), Number(match[2])];
}

/** Whether `version` is at least MIN_COMPOSE. */
export function isNewEnough(version) {
  if (version === null) {
    return false;
  }
  const [major, minor] = version;
  return major > MIN_COMPOSE[0] || (major === MIN_COMPOSE[0] && minor >= MIN_COMPOSE[1]);
}

/** Reads stack-checks.json's text. */
export function readStackChecks(json) {
  const checks = JSON.parse(json);
  return {
    expectedWarnings: checks.expected_warnings.map((warning) => ({
      target: warning.target,
      messagePrefix: warning.message_prefix,
      why: warning.why,
    })),
    expectedRestartErrors: checks.expected_restart_errors.map((error) => ({
      target: error.target,
      messagePrefix: error.message_prefix,
      why: error.why,
    })),
    retryWindows: checks.retry_windows.map((window) => ({
      attempt: window.attempt,
      minMs: window.min_ms,
      maxMs: window.max_ms,
    })),
    connectTimeoutMs: checks.connect_timeout_ms,
    stopGracePeriodMs: checks.stop_grace_period_ms,
  };
}

/** The retry policy's window after `attempt` failures; the last one applies to every later attempt. */
export function retryWindow(checks, attempt) {
  const windows = checks.retryWindows;
  const window = windows[Math.min(attempt, windows.length) - 1];
  return { minMs: window.minMs, maxMs: window.maxMs };
}

/**
 * How long a bot whose last backoff was `attempt` may take to come Online again
 * from the server's healthy moment: the rest of that backoff, one more failed
 * attempt with its backoff, and the attempt that works, plus a second. The same
 * formula as the compose e2e test's.
 */
export function reconnectDeadlineMs(checks, attempt) {
  const connect = checks.connectTimeoutMs;
  return (
    retryWindow(checks, attempt).maxMs +
    connect +
    retryWindow(checks, attempt + 1).maxMs +
    connect +
    1000
  );
}

/** The shortest run that can judge the restart: half of it covers attempt 3's deadline and the stop. */
export function minimumMinutes(checks) {
  const half = reconnectDeadlineMs(checks, ASSUMED_ATTEMPT) + checks.stopGracePeriodMs;
  return Math.ceil((2 * half) / 60000);
}

/** Milliseconds since the epoch, from RFC 3339 with any number of fractional digits; null if it isn't one. */
function parseTime(text) {
  const ms = Date.parse(String(text).replace(/(\.\d{3})\d+/, "$1"));
  return Number.isNaN(ms) ? null : ms;
}

function iso(ms) {
  return new Date(ms).toISOString();
}

function parseLine(raw) {
  let fields;
  try {
    fields = JSON.parse(raw);
  } catch {
    return null;
  }
  const at = parseTime(fields?.timestamp);
  const texts = [fields?.level, fields?.target, fields?.message];
  if (at === null || texts.some((text) => typeof text !== "string")) {
    return null;
  }
  return {
    raw,
    at,
    level: fields.level,
    target: fields.target,
    message: fields.message,
    botId: fields.span?.bot_id ?? null,
    fields,
  };
}

/** Parses the agent's log: `{ lines, invalid }`, where `invalid` holds every line that isn't one of its JSON lines. */
export function parseLog(text) {
  const lines = [];
  const invalid = [];
  for (const raw of text.split(/\r?\n/)) {
    const trimmed = raw.trim();
    if (trimmed === "") {
      continue;
    }
    const line = parseLine(trimmed);
    if (line === null) {
      invalid.push(trimmed);
    } else {
      lines.push(line);
    }
  }
  return { lines, invalid };
}

/** A state's name and attempt, from its Debug text (`Backoff { attempt: 2 }`). */
export function parseState(debug) {
  const match = /attempt: (\d+)/.exec(debug);
  return { name: debug.split(/[ {]/)[0], attempt: match === null ? null : Number(match[1]) };
}

/** Each bot's username by its ID, from "starting a standalone bot". */
export function botNames(lines) {
  return new Map(
    lines
      .filter((line) => line.message === "starting a standalone bot")
      .map((line) => [line.fields.bot_id, line.fields.username]),
  );
}

function stateChanges(lines, names) {
  return lines
    .filter((line) => line.message === STATE_CHANGED && names.has(line.botId))
    .map((line) => ({
      at: line.at,
      username: names.get(line.botId),
      debug: String(line.fields.state),
      state: parseState(String(line.fields.state)),
    }));
}

/** Every state change, in order, without Online's `since`. */
export function timeline(lines, names) {
  return stateChanges(lines, names).map((change) => ({
    at: change.at,
    username: change.username,
    state: change.debug.replace(/since: [^,}]*, /, ""),
  }));
}

/** WARN and ERROR lines by level and target: errors first, then the most frequent. */
export function warnErrorCounts(lines) {
  const counts = new Map();
  for (const line of lines.filter((line) => line.level === "WARN" || line.level === "ERROR")) {
    const key = `${line.level} ${line.target}`;
    const count = counts.get(key) ?? { level: line.level, target: line.target, count: 0 };
    count.count += 1;
    counts.set(key, count);
  }
  const rank = (level) => (level === "ERROR" ? 0 : 1);
  return [...counts.values()].sort(
    (a, b) =>
      rank(a.level) - rank(b.level) ||
      b.count - a.count ||
      (a.target < b.target ? -1 : a.target > b.target ? 1 : 0),
  );
}

/** Whether an entry of `expected` matches `line` by target and message prefix. */
function matches(line, expected) {
  return expected.some(
    (entry) => entry.target === line.target && line.message.startsWith(entry.messagePrefix),
  );
}

/**
 * The ERRORs a server restart may cause: lines an entry of `expected` matches,
 * logged from `from` to `to`, and no more of them than bot sessions that ended
 * there with `ConnectionClosed`. A server that closes a socket with unread
 * client data sends a TCP reset, which can arrive before its kick, and each
 * reset ends one live session. Any further ones are left out, so they stay
 * unexpected. The same rule as the compose e2e test's.
 */
export function restartErrors(lines, expected, from, to) {
  const window = lines.filter((line) => from <= line.at && line.at <= to);
  const closed = window.filter(
    (line) =>
      line.message === "the session ended; the bot connects again" &&
      line.botId !== null &&
      line.fields.reason === "ConnectionClosed",
  ).length;
  return window
    .filter((line) => line.level === "ERROR" && matches(line, expected))
    .slice(0, closed);
}

/** Every ERROR not in `allowed`, and every WARN no expected warning matches. */
export function unexpectedLines(lines, expected, allowed = []) {
  return lines.filter(
    (line) =>
      (line.level === "ERROR" && !allowed.includes(line)) ||
      (line.level === "WARN" && !matches(line, expected)),
  );
}

/**
 * Each bot's reconnect after the restart: `lines` from index `from` on came
 * after it, and the server was healthy again at `healthyAt`. A bot must be
 * Online again by its deadline, from the attempt of its last backoff before then.
 */
export function reconnects({ lines, names, from, healthyAt, checks }) {
  const after = stateChanges(lines.slice(from), names);
  const usernames = [...new Set(names.values())].sort();
  return usernames.map((username) => {
    const states = after.filter((change) => change.username === username);
    const onlineIndex = states.findIndex((change) => change.state.name === "Online");
    const online = onlineIndex === -1 ? null : states[onlineIndex];
    const attempts = states
      .slice(0, onlineIndex === -1 ? states.length : onlineIndex)
      .filter((change) => change.state.name === "AwaitingSession").length;
    const backoff = states
      .filter((change) => change.at < healthyAt && change.state.name === "Backoff")
      .at(-1);
    const result = {
      username,
      leftAt: states[0]?.at ?? null,
      onlineAt: online?.at ?? null,
      attempts,
      deadlineAt: null,
      problem: null,
    };
    if (backoff === undefined) {
      return {
        ...result,
        problem: `${username} didn't back off before the server was healthy again at ${iso(healthyAt)}`,
      };
    }
    result.deadlineAt = healthyAt + reconnectDeadlineMs(checks, backoff.state.attempt);
    if (online === null) {
      result.problem = `${username} isn't Online again after the restart`;
    } else if (online.at > result.deadlineAt) {
      result.problem = `${username} came Online again at ${iso(online.at)}, after its deadline ${iso(result.deadlineAt)}`;
    }
    return result;
  });
}

/** The fields of the log's last line, which should be "the agent stopped" with the report; null without lines. */
export function shutdownReport(lines) {
  const last = lines.at(-1);
  if (last === undefined) {
    return null;
  }
  return {
    message: last.message,
    exitCode: last.fields.exit_code ?? null,
    stopped: last.fields.stopped ?? null,
    aborted: last.fields.aborted ?? null,
    crashed: last.fields.crashed ?? null,
  };
}

/** What `docker inspect <container>` says: whether it runs, its start, exit code, restarts and probes. */
export function parseInspect(json) {
  const [container] = JSON.parse(json);
  const state = container.State;
  return {
    running: state.Running,
    startedAt: parseTime(state.StartedAt),
    exitCode: state.ExitCode,
    restartCount: container.RestartCount,
    probes: (state.Health?.Log ?? []).map((probe) => ({
      start: parseTime(probe.Start),
      end: parseTime(probe.End),
      exitCode: probe.ExitCode,
    })),
  };
}

/** The end of the first successful probe that started at or after `startedAt`, or null. */
export function healthySince(probes, startedAt) {
  const good = probes
    .filter((probe) => probe.exitCode === 0 && probe.start >= startedAt)
    .sort((a, b) => a.start - b.start);
  return good.length === 0 ? null : good[0].end;
}

const UNITS = {
  B: 1,
  kB: 1e3,
  KB: 1e3,
  MB: 1e6,
  GB: 1e9,
  TB: 1e12,
  KiB: 1024,
  MiB: 1024 ** 2,
  GiB: 1024 ** 3,
  TiB: 1024 ** 4,
};

/** A size such as `40.5MiB` or `1000kB`, in MiB; null if it isn't one. */
export function parseMemoryMiB(text) {
  const match = /^\s*([\d.]+)\s*([A-Za-z]+)\s*$/.exec(text);
  const unit = match === null ? undefined : UNITS[match[2]];
  if (unit === undefined) {
    return null;
  }
  return (Number(match[1]) * unit) / 1024 ** 2;
}

/**
 * The frames in `docker stats --format "{{json .}}"`'s output. It redraws the
 * terminal between frames even into a pipe, so each JSON object is cut out of
 * its line. Frames without numbers (a container not yet or no longer running)
 * are left out.
 */
export function parseStatsOutput(text) {
  const samples = [];
  for (const line of text.split("\n")) {
    const start = line.indexOf("{");
    const end = line.lastIndexOf("}");
    if (start === -1 || end < start) {
      continue;
    }
    let frame;
    try {
      frame = JSON.parse(line.slice(start, end + 1));
    } catch {
      continue;
    }
    const cpuPercent = Number.parseFloat(String(frame.CPUPerc ?? "").replace("%", ""));
    const memoryMiB = parseMemoryMiB(String(frame.MemUsage ?? "").split("/")[0]);
    if (Number.isFinite(cpuPercent) && memoryMiB !== null && memoryMiB > 0) {
      samples.push({ cpuPercent, memoryMiB });
    }
  }
  return samples;
}

function summarize(samples, key) {
  const peak = samples.reduce((best, sample) => (sample[key] > best[key] ? sample : best));
  const values = samples.map((sample) => sample[key]).sort((a, b) => a - b);
  return {
    peak: peak[key],
    peakAt: peak.at,
    mean: values.reduce((sum, value) => sum + value, 0) / values.length,
    p95: values[Math.ceil(0.95 * values.length) - 1],
  };
}

/** The peak (with its time), mean and 95th percentile of memory and CPU; null without samples. */
export function statsSummary(samples) {
  if (samples.length === 0) {
    return null;
  }
  return {
    frames: samples.length,
    memory: summarize(samples, "memoryMiB"),
    cpu: summarize(samples, "cpuPercent"),
  };
}

/** The CPU's model from `lscpu`, else from /proc/cpuinfo (which has none on ARM), else "unknown". */
export function cpuModel(lscpu, cpuinfo) {
  const fromLscpu = /^Model name:\s*(.+)$/m.exec(lscpu);
  if (fromLscpu !== null) {
    return fromLscpu[1].trim();
  }
  const fromCpuinfo = /^model name\s*:\s*(.+)$/m.exec(cpuinfo);
  return fromCpuinfo === null ? "unknown" : fromCpuinfo[1].trim();
}

/**
 * Judges the run. Fails on an ERROR (but those `restartErrors` allowed, in
 * `allowedErrors`), a warning nobody expects (or a line that isn't the agent's
 * JSON), a bot not Online again by its deadline, an agent
 * that restarted, or a shutdown that isn't exit 0 with nothing aborted or
 * crashed. The stats are never judged.
 */
export function verdict(facts) {
  const failures = [];
  const fail = (criterion, lines) => {
    if (lines.length > 0) {
      failures.push({ criterion, lines });
    }
  };
  fail(
    "an ERROR line",
    facts.lines
      .filter((line) => line.level === "ERROR" && !facts.allowedErrors.includes(line))
      .map((line) => line.raw),
  );
  fail("a warning nobody expects", [
    ...unexpectedLines(facts.lines, facts.expectedWarnings)
      .filter((line) => line.level === "WARN")
      .map((line) => line.raw),
    ...facts.invalid.map((raw) => `not one of the agent's JSON lines: ${raw}`),
  ]);
  fail(
    "a bot not Online again by its deadline",
    facts.reconnects
      .filter((reconnect) => reconnect.problem !== null)
      .map((reconnect) => reconnect.problem),
  );
  const restarted = [];
  if (facts.agentRuns !== 1) {
    restarted.push(`"the agent is running" appears ${facts.agentRuns} times`);
  }
  if (facts.agentAfter.startedAt !== facts.agentBefore.startedAt) {
    restarted.push(`the agent's container started again at ${iso(facts.agentAfter.startedAt)}`);
  }
  if (facts.agentAfter.restartCount !== facts.agentBefore.restartCount) {
    restarted.push(`Docker restarted the agent's container ${facts.agentAfter.restartCount} times`);
  }
  fail("the agent restarted", restarted);
  const shutdown = [];
  if (facts.exitCode !== 0) {
    shutdown.push(`the agent's container exited with ${facts.exitCode}`);
  }
  const report = facts.report;
  if (report === null || report.message !== "the agent stopped") {
    shutdown.push(`the last line isn't "the agent stopped": ${report?.message ?? "no lines"}`);
  } else if (report.exitCode !== 0 || report.aborted !== 0 || report.crashed !== 0) {
    shutdown.push(
      `the report: exit_code ${report.exitCode}, stopped ${report.stopped}, aborted ${report.aborted}, crashed ${report.crashed}`,
    );
  }
  fail("the shutdown wasn't clean", shutdown);
  return { pass: failures.length === 0, failures };
}

/** `+mm:ss` (or `-mm:ss`) for a span of milliseconds. */
function clock(ms) {
  const seconds = Math.round(Math.abs(ms) / 1000);
  const sign = ms < 0 && seconds > 0 ? "-" : "+";
  return `${sign}${String(Math.floor(seconds / 60)).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`;
}

function relativeToRestart(at, restartAt) {
  const offset = at - restartAt;
  const span = clock(Math.abs(offset)).slice(1);
  return offset < 0 ? `${span} before the restart` : `${span} after the restart`;
}

/** The summary the demo prints and saves as summary.txt. It holds no host path, host name or IP. */
export function formatSummary(report) {
  const out = [];
  const since = (at) => clock(at - report.startAt);
  const dod =
    report.minutes < DOD_MINUTES ? ` (not the DoD run: the DoD runs ${DOD_MINUTES} min)` : "";
  out.push(
    `afkfleet demo: ${report.bots} bots for ${report.minutes} min, one server restart at half time${dod}`,
  );
  const machine = report.machine;
  out.push(
    `Measured on: ${machine.architecture}, ${machine.cpus} CPUs (${machine.cpuModel}), ${machine.os}, ${machine.memoryGiB.toFixed(1)} GiB of memory`,
  );
  out.push("");
  if (report.stats === null) {
    out.push("The agent's container: no docker stats frames");
  } else {
    const { memory, cpu, frames } = report.stats;
    out.push(`The agent's container (docker stats, ${frames} frames):`);
    out.push(
      `  memory: peak ${memory.peak.toFixed(1)} MiB (${relativeToRestart(memory.peakAt, report.restartAt)}), mean ${memory.mean.toFixed(1)} MiB, p95 ${memory.p95.toFixed(1)} MiB`,
    );
    out.push(
      `  CPU:    peak ${cpu.peak.toFixed(2)} % of one core (${relativeToRestart(cpu.peakAt, report.restartAt)}), mean ${cpu.mean.toFixed(2)} %, p95 ${cpu.p95.toFixed(2)} %`,
    );
  }
  out.push("");
  out.push("Warnings and errors by target:");
  if (report.counts.length === 0) {
    out.push("  none");
  }
  const width = Math.max(0, ...report.counts.map((count) => count.target.length));
  for (const count of report.counts) {
    out.push(`  ${count.level.padEnd(5)}  ${count.target.padEnd(width)}  ${count.count}`);
  }
  if (report.allowedErrors > 0) {
    const resets = `${report.allowedErrors} connection reset${report.allowedErrors === 1 ? "" : "s"}`;
    out.push(
      `  Of the errors, ${resets} while the server restarted, at most one per session closed there, as stack-checks.json allows.`,
    );
  }
  out.push("");
  out.push(`State changes (times from "the agent is running"):`);
  const nameWidth = Math.max(0, ...report.changes.map((change) => change.username.length));
  for (const change of report.changes) {
    out.push(`  ${since(change.at)}  ${change.username.padEnd(nameWidth)}  ${change.state}`);
  }
  out.push("");
  out.push(
    `Reconnects after the restart (the server healthy again at ${since(report.healthyAt)}):`,
  );
  for (const reconnect of report.reconnects) {
    const name = reconnect.username.padEnd(nameWidth);
    if (reconnect.onlineAt === null || reconnect.leftAt === null) {
      out.push(`  ${name}  ${reconnect.problem}`);
      continue;
    }
    const took = ((reconnect.onlineAt - reconnect.leftAt) / 1000).toFixed(1);
    const deadline = reconnect.deadlineAt === null ? "none" : since(reconnect.deadlineAt);
    const status = reconnect.problem === null ? "in time" : reconnect.problem;
    out.push(
      `  ${name}  left at ${since(reconnect.leftAt)}, Online again at ${since(reconnect.onlineAt)} (${took} s, ${reconnect.attempts} attempts); deadline ${deadline}: ${status}`,
    );
  }
  out.push("");
  const shutdown = report.report;
  out.push(
    shutdown === null
      ? `Shutdown: container exit code ${report.exitCode}; no log lines`
      : `Shutdown: container exit code ${report.exitCode}; last line "${shutdown.message}" with exit_code ${shutdown.exitCode}, stopped ${shutdown.stopped}, aborted ${shutdown.aborted}, crashed ${shutdown.crashed}`,
  );
  out.push("");
  out.push(`Verdict: ${report.verdict.pass ? "PASS" : "FAIL"}${dod}`);
  for (const failure of report.verdict.failures) {
    out.push(`  - ${failure.criterion}:`);
    for (const line of failure.lines) {
      out.push(`      ${line}`);
    }
  }
  return `${out.join("\n")}\n`;
}

// --- Running the demo (not unit-tested; `just demo-agent 8` exercises it) ---

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** `docker <args>`, its output captured; rejects on a non-zero exit. */
function docker(args, timeoutMs = 120000) {
  return new Promise((resolve, reject) => {
    execFile(
      "docker",
      args,
      { cwd: ROOT, windowsHide: true, timeout: timeoutMs, maxBuffer: 256 * 1024 ** 2 },
      (error, stdout, stderr) => {
        if (error) {
          reject(new Error(`docker ${args.join(" ")} failed: ${error.message}\n${stderr}`));
        } else {
          resolve(stdout);
        }
      },
    );
  });
}

/** `docker <args>` with its output on the terminal; rejects on a non-zero exit. */
function dockerLoud(args) {
  return new Promise((resolve, reject) => {
    const child = spawn("docker", args, { cwd: ROOT, windowsHide: true, stdio: "inherit" });
    child.on("error", reject);
    child.on("exit", (code) =>
      code === 0 ? resolve() : reject(new Error(`docker ${args.join(" ")} exited with ${code}`)),
    );
  });
}

/** Follows the agent's `docker stats` from its own process group, so Ctrl+C reaches only this script. */
function followStats(container, startHost) {
  const samples = [];
  const child = spawn("docker", ["stats", "--format", "{{json .}}", container], {
    cwd: ROOT,
    windowsHide: true,
    detached: true,
    stdio: ["ignore", "pipe", "ignore"],
  });
  let pending = "";
  child.stdout.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    pending += chunk;
    const lines = pending.split("\n");
    pending = lines.pop();
    for (const line of lines) {
      for (const sample of parseStatsOutput(line)) {
        samples.push({ at: Date.now() - startHost, ...sample });
      }
    }
  });
  return {
    samples,
    stop: () => {
      if (child.exitCode === null) {
        child.kill();
      }
    },
  };
}

async function machine() {
  const info = JSON.parse(await docker(["info", "--format", "{{json .}}"]));
  const quietly = (args) => docker(args).catch(() => "");
  const lscpu = await quietly([...COMPOSE, "exec", "-T", "minecraft", "lscpu"]);
  const cpuinfo = await quietly([...COMPOSE, "exec", "-T", "minecraft", "cat", "/proc/cpuinfo"]);
  return {
    architecture: info.Architecture,
    cpus: info.NCPU,
    cpuModel: cpuModel(lscpu, cpuinfo),
    os: info.OperatingSystem,
    memoryGiB: info.MemTotal / 1024 ** 3,
  };
}

async function inspect(container) {
  return parseInspect(await docker(["inspect", container]));
}

function logs(service) {
  return docker([...COMPOSE, "logs", "--no-color", "--no-log-prefix", service]);
}

/** Polls the restarted server until a probe after its new start succeeds; returns that probe's end. */
async function waitHealthy(container, startedBefore, untilHost, signal) {
  while (Date.now() < untilHost) {
    const state = await inspect(container);
    if (state.startedAt > startedBefore) {
      const healthy = healthySince(state.probes, state.startedAt);
      if (healthy !== null) {
        return healthy;
      }
    }
    await sleep(POLL_MS, undefined, { signal });
  }
  throw new Error("the server wasn't healthy again before the run's end");
}

function stamp(date) {
  return date
    .toISOString()
    .replace(/[-:]/g, "")
    .replace(/\.\d{3}Z$/, "Z");
}

async function main(args) {
  const parsed = parseArgs(args);
  if (parsed.error !== undefined) {
    console.error(parsed.error);
    return 2;
  }
  const { minutes } = parsed;
  const checks = readStackChecks(
    readFileSync(path.join(ROOT, "deploy/dev/stack-checks.json"), "utf8"),
  );
  const minimum = minimumMinutes(checks);
  if (minutes < minimum) {
    console.error(
      `${minutes} minutes are too short to judge the restart: half the run must hold a reconnect's deadline and the agent's stop, so it needs at least ${minimum} minutes.`,
    );
    return 2;
  }
  const version = composeVersion(await docker(["compose", "version", "--short"]).catch(() => ""));
  if (!isNewEnough(version)) {
    console.error(
      `compose.isolated.yaml's \`!reset\` needs Docker Compose ${MIN_COMPOSE.join(".")} or newer.`,
    );
    return 2;
  }

  const abort = new AbortController();
  const interrupt = () => abort.abort();
  process.on("SIGINT", interrupt);
  process.on("SIGBREAK", interrupt);
  const outDir = path.join(ROOT, "target", "demo-agent", stamp(new Date()));
  let stats = null;
  let saved = false;
  const save = async () => {
    mkdirSync(outDir, { recursive: true });
    writeFileSync(path.join(outDir, "agent.log"), await logs("agent").catch(() => ""));
    writeFileSync(path.join(outDir, "minecraft.log"), await logs("minecraft").catch(() => ""));
    const samples = stats?.samples ?? [];
    writeFileSync(
      path.join(outDir, "stats.jsonl"),
      samples.map((sample) => JSON.stringify(sample)).join("\n"),
    );
    saved = true;
  };
  try {
    await dockerLoud(["compose", "--file", "deploy/compose.dev.yaml", "build", "agent"]);
    await dockerLoud([...COMPOSE, "down", "--volumes", "--timeout", "10"]);
    await dockerLoud([
      ...COMPOSE,
      "up",
      "--detach",
      "--wait",
      "--wait-timeout",
      "300",
      "--no-build",
    ]);
    const startHost = Date.now();
    const endHost = startHost + minutes * 60000;
    const agent = (await docker([...COMPOSE, "ps", "--all", "--quiet", "agent"])).trim();
    const server = (await docker([...COMPOSE, "ps", "--all", "--quiet", "minecraft"])).trim();
    stats = followStats(agent, startHost);
    const measuredOn = await machine();
    const agentBefore = await inspect(agent);
    const serverBefore = await inspect(server);
    console.log(
      `The stack is up; the server restarts in ${minutes / 2} min, and the run ends in ${minutes} min.`,
    );

    await sleep(startHost + (minutes * 60000) / 2 - Date.now(), undefined, {
      signal: abort.signal,
    });
    // The restart's window runs, by the Docker VM's clock, from the agent's
    // last line before the restart to the server's healthy moment.
    const before = parseLog(await logs("agent")).lines;
    const beforeRestart = before.length;
    const windowFrom = before.at(-1)?.at ?? 0;
    const restartAt = Date.now() - startHost;
    await dockerLoud([...COMPOSE, "restart", "--no-deps", "minecraft"]);
    const healthyAt = await waitHealthy(server, serverBefore.startedAt, endHost, abort.signal);
    const early = parseLog(await logs("agent")).lines;
    const deadlines = reconnects({
      lines: early,
      names: botNames(early),
      from: beforeRestart,
      healthyAt,
      checks,
    }).map((reconnect) => (reconnect.deadlineAt === null ? 0 : reconnect.deadlineAt - healthyAt));
    const needed = Math.max(0, ...deadlines) + checks.stopGracePeriodMs;
    if (needed > endHost - Date.now()) {
      await save();
      console.error(
        `Too short to judge: the slowest bot's reconnect deadline and the agent's stop need ${Math.ceil(needed / 1000)} s, but the run has ${Math.floor((endHost - Date.now()) / 1000)} s left. Logs: ${outDir}`,
      );
      return 2;
    }
    console.log("The server is healthy again; the run goes on to its end.");

    await sleep(endHost - Date.now(), undefined, { signal: abort.signal });
    stats.stop();
    await docker([...COMPOSE, "stop", "agent"], checks.stopGracePeriodMs + 60000);
    const agentAfter = await inspect(agent);
    await save();
    const { lines, invalid } = parseLog(readFileSync(path.join(outDir, "agent.log"), "utf8"));
    const names = botNames(lines);
    const running = lines.filter((line) => line.message === "the agent is running");
    const results = reconnects({ lines, names, from: beforeRestart, healthyAt, checks });
    const shutdown = shutdownReport(lines);
    const allowedErrors = restartErrors(lines, checks.expectedRestartErrors, windowFrom, healthyAt);
    const judged = verdict({
      lines,
      invalid,
      expectedWarnings: checks.expectedWarnings,
      allowedErrors,
      reconnects: results,
      agentRuns: running.length,
      agentBefore,
      agentAfter,
      exitCode: agentAfter.exitCode,
      report: shutdown,
    });
    const summary = formatSummary({
      minutes,
      machine: measuredOn,
      bots: names.size,
      stats: statsSummary(stats.samples),
      restartAt,
      counts: warnErrorCounts(lines),
      allowedErrors: allowedErrors.length,
      changes: timeline(lines, names),
      reconnects: results,
      startAt: running[0]?.at ?? lines[0]?.at ?? 0,
      healthyAt,
      exitCode: agentAfter.exitCode,
      report: shutdown,
      verdict: judged,
    });
    writeFileSync(path.join(outDir, "summary.txt"), summary);
    console.log(`\n${summary}`);
    console.log(`Saved under ${outDir}`);
    return judged.pass ? 0 : 1;
  } catch (error) {
    if (abort.signal.aborted) {
      console.error("Interrupted; the stack is removed.");
    } else {
      console.error(error instanceof Error ? error.message : error);
    }
    return 2;
  } finally {
    stats?.stop();
    if (!saved) {
      await save().catch(() => {});
    }
    await dockerLoud([...COMPOSE, "down", "--volumes", "--timeout", "10"]).catch((error) =>
      console.error(error.message),
    );
    process.off("SIGINT", interrupt);
    process.off("SIGBREAK", interrupt);
  }
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main(process.argv.slice(2)).then((code) => {
    process.exitCode = code;
  });
}
