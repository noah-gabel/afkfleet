// Checks the DoD's 24-hour run by its logs (ADR-0020, S135, U129, U133): the user runs it
// after the run, as `just dod-log-check`, and the AI never does.
//
//   node scripts/dod-log-check.mjs <logs folder>
//
// It reads the files scripts/dod-log.mjs left in target/dod-run/logs, one start of dod-server
// and one of dod-agent, and prints only what's safe to share: counts per level and target (a
// target whole when it's a module path of afkfleet's sources, else its crate alone), the
// warnings' and errors' messages when they are fixed texts of afkfleet's own sources,
// each bot's sessions by number, and times from the first session; never a field's value, a
// message with values, a third-party message, a file name or a path. It ends in PASS (exit 0)
// or FAIL with its reasons (exit 1); with nothing to check, a log it can't read (named by its
// error's code alone) or a wrong command line, exit 2.
//
// PASS needs: exactly one start of each that ended with code 0 after its stop line; every
// log line one of the binaries' JSON lines and every stderr line cargo's; no ERROR but
// azalea's one ConnectionReset read error in a restart's window (U133); no panic; and every
// bot's session renewed after its lifetime while it reconnected by itself (U129), Online for
// at least the run's 25 h. Tests: scripts/dod-log-check.test.mjs.
import { existsSync, readdirSync, readFileSync, realpathSync, statSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

import { LOG_FILE } from "./dod-log.mjs";

/** The repository, found from this script's place. */
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
/** fleet-server's `REUSE_MARGIN`: a session is reused until this long before it expires. */
export const REUSE_MARGIN_MS = 10 * MINUTE;
/** How long before the bot leaves Online azalea's reset may be logged (U133). */
const RESTART_LEAD_MS = 10_000;
/** The run's length, which every bot must be Online for (U129). */
const RUN_MS = 25 * HOUR;
/** `fleet_core::bot::BotState`'s variants, in their order there. */
export const BOT_STATES = [
  "Stopped",
  "AwaitingSession",
  "Connecting",
  "Online",
  "Backoff",
  "Paused",
  "Failed",
  "Stopping",
];
/** The states a reconnect goes through by itself, between two Onlines. */
const RECONNECTING = new Set(["Backoff", "AwaitingSession", "Connecting"]);
/** The messages and the target it reads, each pinned against its Rust file by a test. */
export const TEXTS = {
  granted: "a session was granted",
  stateChanged: "the bot's state changed",
  desiredSet: "a bot's desired state was set",
  commandSent: "a command was sent to a bot's agent",
  stickyCleared: "a resting bot's sticky state was cleared",
  requestFinished: "request finished",
  serverStopped: "the server stopped",
  agentStopped: "the agent stopped",
  panicked: "a thread panicked",
  panicTarget: "afkfleet::panic",
};
/** A lifecycle route's template, as the request span's `route` holds it. */
const LIFECYCLE_ROUTE = /\/bots\/\{id\}\/(start|stop|restart|reset|resume)$/;
/** The levels a JSON line may have. */
const LEVELS = ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"];
/** cargo's own stderr lines. */
const CARGO = /^\s*(Compiling|Finished|Running|Blocking)\b/;
/** U133's one allowed error: azalea's read of a socket the closing server reset. */
const RESET = {
  target: "azalea_client::plugins::connection",
  prefix: "Error reading packet from Client: IoError",
  kind: "kind: ConnectionReset",
};
const ANOTHER_TARGET = "(another target)";
const NOT_OURS = "(message not shown: not afkfleet's own)";
const NOT_FIXED = "(message not shown: not a fixed text of afkfleet's sources)";
const ALLOWED = "(allowed: the container's restart)";
const USAGE = "usage: node scripts/dod-log-check.mjs <logs folder>";
const PASS =
  "PASS: no error, no panic, nothing unhandled, and every bot's session was renewed after its lifetime while it reconnected by itself.";

/** Every `.rs` file under `dir`, at any depth. */
function rustFiles(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const at = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      return rustFiles(at);
    }
    return entry.isFile() && entry.name.endsWith(".rs") ? [at] : [];
  });
}

/** A normal Rust string literal's text, its escapes read and its continued lines joined. */
function literalText(body) {
  return body
    .replace(/\\\r?\n\s*/g, "")
    .replace(/\\(u\{([0-9a-fA-F]+)\}|x([0-9a-fA-F]{2})|.)/g, (_, code, unicode, hex) => {
      if (unicode !== undefined) {
        return String.fromCodePoint(Number.parseInt(unicode, 16));
      }
      if (hex !== undefined) {
        return String.fromCharCode(Number.parseInt(hex, 16));
      }
      return { n: "\n", r: "\r", t: "\t", 0: "\0" }[code] ?? code;
    });
}

/**
 * Every string literal in `root`'s `crates/*\/src/**\/*.rs`, as its text reads: the only
 * messages the check prints are among them, so its output holds nothing but public text.
 */
export function sourceLiterals(root) {
  const literals = new Set();
  const crates = path.join(root, "crates");
  for (const crate of readdirSync(crates, { withFileTypes: true })) {
    const src = path.join(crates, crate.name, "src");
    if (!crate.isDirectory() || !existsSync(src)) {
      continue;
    }
    for (const file of rustFiles(src)) {
      const text = readFileSync(file, "utf8");
      for (const match of text.matchAll(/r(#*)"([\s\S]*?)"\1|"((?:[^"\\]|\\[\s\S])*)"/g)) {
        literals.add(match[2] ?? literalText(match[3]));
      }
    }
  }
  return literals;
}

/** The names of `toml`'s `[package]` and `[[bin]]`s (a Cargo.toml), as a target writes them. */
function crateNames(toml) {
  const names = { lib: null, bins: [] };
  let section = "";
  for (const raw of toml.split(/\r?\n/)) {
    const line = raw.trim();
    if (line.startsWith("[")) {
      section = line;
      continue;
    }
    const name = /^name\s*=\s*"([^"]+)"$/.exec(line)?.[1]?.replaceAll("-", "_");
    if (name !== undefined && section === "[package]") {
      names.lib = name;
    } else if (name !== undefined && section === "[[bin]]") {
      names.bins.push(name);
    }
  }
  return names;
}

/**
 * Every module path of the workspace's crates, as a target writes it: each library's modules,
 * from its files under `src` and the inline modules they declare; each binary by its name alone,
 * as its `main.rs` declares none; and the panic hook's `afkfleet::panic`. A target is printed
 * whole only when it's one of them, so no segment past a crate brings a value along.
 */
export function modulePaths(root) {
  const paths = new Set([TEXTS.panicTarget]);
  const crates = path.join(root, "crates");
  for (const crate of readdirSync(crates, { withFileTypes: true })) {
    const manifest = path.join(crates, crate.name, "Cargo.toml");
    const src = path.join(crates, crate.name, "src");
    if (!crate.isDirectory() || !existsSync(manifest) || !existsSync(src)) {
      continue;
    }
    const { lib, bins } = crateNames(readFileSync(manifest, "utf8"));
    for (const bin of bins) {
      paths.add(bin);
    }
    if (lib === null) {
      continue;
    }
    paths.add(lib);
    for (const file of rustFiles(src)) {
      const parts = path.relative(src, file).replace(/\.rs$/, "").split(path.sep);
      if (parts.join("/") === "main") {
        continue;
      }
      if (parts.at(-1) === "mod" || parts.join("/") === "lib") {
        parts.pop();
      }
      for (let end = 1; end <= parts.length; end += 1) {
        paths.add([lib, ...parts.slice(0, end)].join("::"));
      }
      const text = readFileSync(file, "utf8");
      for (const [, name] of text.matchAll(
        /^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{/gm,
      )) {
        paths.add([lib, ...parts, name].join("::"));
      }
    }
  }
  return paths;
}

/**
 * The crates in `lockText` (Cargo.lock) as a target's first segment writes them, with the
 * binaries' own crates and the panic hook's `afkfleet`.
 */
export function lockedCrates(lockText) {
  const crates = new Set(["afkfleet", "afkfleet_server", "afkfleet_agent"]);
  for (const [, name] of lockText.matchAll(/^name = "([^"]+)"$/gm)) {
    crates.add(name.replaceAll("-", "_"));
  }
  return crates;
}

/** The milliseconds of chrono's `DateTime<Utc>` Display (`2026-11-04 19:30:00.123 UTC`), or null. */
export function chronoTime(text) {
  const match = /^(\d{4}-\d{2}-\d{2}) (\d{2}:\d{2}:\d{2})(\.\d+)? UTC$/.exec(String(text));
  if (match === null) {
    return null;
  }
  const ms = Date.parse(`${match[1]}T${match[2]}${(match[3] ?? "").slice(0, 4)}Z`);
  return Number.isNaN(ms) ? null : ms;
}

/** A tracing timestamp's milliseconds, or null. */
function lineTime(text) {
  if (typeof text !== "string" || !/^\d{4}-\d{2}-\d{2}T/.test(text)) {
    return null;
  }
  const ms = Date.parse(text.replace(/(\.\d{3})\d+/, "$1"));
  return Number.isNaN(ms) ? null : ms;
}

const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);

/** One of the binaries' JSON lines (ADR-0014's shape), or null. */
function readLine(raw) {
  let fields;
  try {
    fields = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!isObject(fields)) {
    return null;
  }
  const at = lineTime(fields.timestamp);
  const shaped =
    at !== null &&
    LEVELS.includes(fields.level) &&
    typeof fields.target === "string" &&
    (fields.message === undefined || typeof fields.message === "string") &&
    (fields.span === undefined || isObject(fields.span)) &&
    (fields.spans === undefined || Array.isArray(fields.spans));
  return shaped ? { at, level: fields.level, target: fields.target, fields } : null;
}

/** The bot a line is about: its own `bot_id`, or its span's. */
function botOf(fields) {
  const span =
    fields.span?.bot_id ??
    fields.spans?.findLast?.((each) => isObject(each) && "bot_id" in each)?.bot_id;
  return fields.bot_id ?? span ?? null;
}

/**
 * A target as printed: whole when it's a module path of afkfleet's sources; else, when it's a
 * Rust path whose first segment is a known crate, that crate alone, as a segment past it may
 * carry anything (PR #93's review); else the placeholder.
 */
function shownTarget(target, context) {
  const pathLike = /^[A-Za-z_][A-Za-z0-9_]*(::[A-Za-z_][A-Za-z0-9_]*)*$/.test(target);
  const crate = target.split("::")[0];
  if (!pathLike || !context.crates.has(crate)) {
    return ANOTHER_TARGET;
  }
  return context.paths.has(target) ? target : crate;
}

/** A message as printed: quoted when it's afkfleet's own and a fixed text, else why not. */
function shownMessage(line, context) {
  const own =
    /^(fleet_|afkfleet)/.test(line.target) && shownTarget(line.target, context) !== ANOTHER_TARGET;
  if (!own) {
    return NOT_OURS;
  }
  const message = line.fields.message;
  return typeof message === "string" && context.literals.has(message) ? `"${message}"` : NOT_FIXED;
}

/** `ms` after `t0` as `+h:mm`. */
function since(ms, t0) {
  const minutes = Math.floor(Math.max(0, ms - t0) / MINUTE);
  return `+${Math.floor(minutes / 60)}:${String(minutes % 60).padStart(2, "0")}`;
}

/** A duration as `h:mm:ss`. */
function duration(ms) {
  const seconds = Math.floor(ms / 1000);
  const parts = [Math.floor(seconds / 3600), Math.floor(seconds / 60) % 60, seconds % 60];
  return `${parts[0]}:${String(parts[1]).padStart(2, "0")}:${String(parts[2]).padStart(2, "0")}`;
}

/** The starts in `folder`, by kind, each with its three files' text (null when missing). */
function readStarts(folder) {
  const starts = { server: new Map(), agent: new Map() };
  let others = 0;
  for (const name of readdirSync(folder)) {
    const match = LOG_FILE.exec(name);
    if (match === null || !statSync(path.join(folder, name)).isFile()) {
      others += 1;
      continue;
    }
    const [, kind, stamp, part] = match;
    const start = starts[kind].get(stamp) ?? { log: null, stderr: null, exit: null };
    start[{ jsonl: "log", "stderr.txt": "stderr", "exit.json": "exit" }[part]] = readFileSync(
      path.join(folder, name),
      "utf8",
    );
    starts[kind].set(stamp, start);
  }
  return { starts, others };
}

/** The non-empty lines of `text`, without their line breaks. */
const linesOf = (text) => text.split(/\r?\n/).filter((line) => line.trim() !== "");

/** Reads one start's log: its JSON lines and how many lines aren't. */
function readLog(text) {
  const lines = [];
  let invalid = 0;
  for (const raw of linesOf(text ?? "")) {
    const line = readLine(raw);
    if (line === null) {
      invalid += 1;
    } else {
      lines.push(line);
    }
  }
  return { lines, invalid };
}

/** Why a start's exit record or its last line fails, if they do. */
function endReasons(kind, start, lines) {
  const reasons = [];
  if (start.exit === null) {
    reasons.push(
      `the ${kind} hasn't ended, or ended without its exit record: stop it with Ctrl+C, then check again`,
    );
  } else {
    let record = null;
    try {
      record = JSON.parse(start.exit);
    } catch {
      // Unreadable, below.
    }
    if (isObject(record) && Number.isInteger(record.code) && record.signal === null) {
      if (record.code !== 0) {
        reasons.push(`the ${kind} exited with code ${record.code}`);
      }
    } else if (isObject(record) && record.code === null && typeof record.signal === "string") {
      const name = /^SIG[A-Z0-9]+$/.test(record.signal) ? record.signal : "a signal";
      reasons.push(`the ${kind} was ended by ${name}`);
    } else {
      reasons.push(`the ${kind}'s exit record isn't readable`);
    }
  }
  const stop = kind === "server" ? TEXTS.serverStopped : TEXTS.agentStopped;
  const last = lines.at(-1)?.fields;
  if (last?.message !== stop || last.exit_code !== 0) {
    reasons.push(`the ${kind}'s last line isn't "${stop}" with exit code 0`);
  } else if (kind === "agent" && ((last.aborted ?? 0) !== 0 || (last.crashed ?? 0) !== 0)) {
    reasons.push("the agent's stop aborted or crashed a bot");
  }
  return reasons;
}

/** Each bot's states from the agent's lines, in order: `{ at, name }`. */
function statesByBot(agentLines) {
  const states = new Map();
  for (const line of agentLines) {
    const bot = botOf(line.fields);
    if (line.fields.message !== TEXTS.stateChanged || bot === null) {
      continue;
    }
    const name = String(line.fields.state ?? "").split(/[ {]/)[0];
    states.set(bot, [...(states.get(bot) ?? []), { at: line.at, name }]);
  }
  return states;
}

/** Each bot's online grants, in the order of their first one: `[bot, [{ at, expires }]]`. */
function grantsByBot(serverLines, reasons) {
  const grants = new Map();
  for (const line of serverLines) {
    const { fields } = line;
    if (
      fields.message !== TEXTS.granted ||
      fields.kind !== "online" ||
      fields.bot_id === undefined
    ) {
      continue;
    }
    const expires = chronoTime(fields.expires_at);
    if (expires === null) {
      reasons.add("a session grant's expiry isn't readable");
      continue;
    }
    grants.set(fields.bot_id, [...(grants.get(fields.bot_id) ?? []), { at: line.at, expires }]);
  }
  return [...grants];
}

/** Whether a server line is a lifecycle call: a lifecycle route's request, or one for `bot`. */
function isLifecycle(line, bot) {
  const { fields } = line;
  if (fields.message === TEXTS.requestFinished) {
    return LIFECYCLE_ROUTE.test(String(fields.span?.route ?? ""));
  }
  const forBot = [TEXTS.desiredSet, TEXTS.commandSent, TEXTS.stickyCleared].includes(
    fields.message,
  );
  return forBot && fields.bot_id === bot;
}

/**
 * Why a renewal at `at` wasn't a reconnect by itself, or null when it was: the bot's last
 * Online before it ended in a Backoff, only a reconnect's states followed until it was Online
 * again after the grant, and no lifecycle call came in between. Also gives the window.
 */
function reconnect(states, at, serverLines, bot) {
  const lastOnline = states.findLastIndex((each) => each.name === "Online" && each.at <= at);
  const left = states[lastOnline + 1];
  if (lastOnline === -1 || left === undefined || left.name !== "Backoff" || left.at > at) {
    return { why: "it didn't follow a session that ended by itself" };
  }
  const back = states.findIndex((each, index) => index > lastOnline && each.name === "Online");
  const between = states.slice(lastOnline + 1, back === -1 ? undefined : back);
  if (between.some((each) => !RECONNECTING.has(each.name))) {
    return { why: "it stopped, paused or failed in between" };
  }
  if (back === -1) {
    return { why: "it wasn't Online again after it" };
  }
  const window = { from: left.at, to: states[back].at };
  if (
    serverLines.some(
      (line) => line.at >= window.from && line.at <= window.to && isLifecycle(line, bot),
    )
  ) {
    return { why: "a Start, Stop, Restart, Reset or Resume came in between" };
  }
  return { why: null, window };
}

/** When `states` were first Online and last left it, and the time not Online between. */
function onlineSpan(states, lastLine) {
  const first = states.findIndex((each) => each.name === "Online");
  if (first === -1) {
    return null;
  }
  let to = lastLine;
  let off = 0;
  let gaps = 0;
  let leftAt = null;
  for (const each of states.slice(first + 1)) {
    if (each.name === "Online" && leftAt !== null) {
      off += each.at - leftAt;
      gaps += 1;
      leftAt = null;
    } else if (each.name !== "Online" && leftAt === null) {
      leftAt = each.at;
    }
  }
  if (leftAt !== null) {
    to = leftAt;
  }
  return { from: states[first].at, to, off, gaps };
}

/** The bots' part of the report, and its reasons, from both logs. */
function judgeBots(serverLines, agentLines, reasons) {
  const grants = grantsByBot(serverLines, reasons);
  const states = statesByBot(agentLines);
  const lastLine = agentLines.at(-1)?.at ?? 0;
  const windows = [];
  const report = [];
  if (grants.length === 0) {
    reasons.add("no bot got a session");
    return { report, windows };
  }
  const t0 = grants[0][1][0].at;
  report.push("Bots (times from the first session):");
  grants.forEach(([bot, granted], index) => {
    const k = index + 1;
    const own = states.get(bot) ?? [];
    let latest = granted[0].expires;
    let sessions = 1;
    let early = 0;
    const renewed = [];
    const failures = [];
    for (const grant of granted.slice(1)) {
      if (grant.expires <= latest) {
        continue;
      }
      sessions += 1;
      if (grant.at < latest - REUSE_MARGIN_MS) {
        early += 1;
      } else {
        const { why, window } = reconnect(own, grant.at, serverLines, bot);
        if (why === null) {
          renewed.push(grant.at);
          windows.push(window);
        } else {
          failures.push(
            `bot ${k}'s renewal at ${since(grant.at, t0)} wasn't a reconnect by itself: ${why}`,
          );
        }
      }
      latest = grant.expires;
    }
    const at = renewed.length === 0 ? "" : ` at ${renewed.map((ms) => since(ms, t0)).join(", ")}`;
    report.push(
      `Bot ${k}: ${sessions} sessions; ${renewed.length} renewed after the lifetime${at}; ${early} renewed early`,
    );
    const span = onlineSpan(own, lastLine);
    if (span !== null) {
      report.push(
        `Bot ${k}: Online from ${since(span.from, t0)} to ${since(span.to, t0)}; not Online for ${duration(span.off)} in ${span.gaps} gaps`,
      );
    }
    if (renewed.length === 0) {
      if (failures.length === 0) {
        reasons.add(
          `bot ${k}'s session was never renewed after its lifetime, which ended at ${since(granted[0].expires, t0)}`,
        );
      }
      for (const failure of failures) {
        reasons.add(failure);
      }
    }
    if (span === null || span.to - span.from < RUN_MS) {
      const from = span === null ? "+0:00" : since(span.from, t0);
      const to = span === null ? "+0:00" : since(span.to, t0);
      reasons.add(`bot ${k} was Online from ${from} to ${to}, under the run's 25 h`);
    }
  });
  return { report, windows };
}

/** Whether `line` is U133's reset, inside one of `windows` that hasn't used its one yet. */
function allowedReset(line, windows, used) {
  const message = String(line.fields.message ?? "");
  if (
    line.level !== "ERROR" ||
    line.target !== RESET.target ||
    !message.startsWith(RESET.prefix) ||
    !message.includes(RESET.kind)
  ) {
    return false;
  }
  const window = windows.find(
    (each) => !used.has(each) && line.at >= each.from - RESTART_LEAD_MS && line.at <= each.to,
  );
  if (window === undefined) {
    return false;
  }
  used.add(window);
  return true;
}

/** One process's part of the report, and its reasons. */
function judgeProcess(kind, start, log, context, windows, reasons) {
  const label = kind === "server" ? "The server's log" : "The agent's log";
  const levels = new Map(LEVELS.map((level) => [level, 0]));
  const targets = new Map();
  const rows = new Map();
  const used = new Set();
  let errors = 0;
  let panics = 0;
  for (const line of log.lines) {
    levels.set(line.level, levels.get(line.level) + 1);
    const target = shownTarget(line.target, context);
    const counts = targets.get(target) ?? { INFO: 0, WARN: 0, ERROR: 0 };
    if (line.level in counts) {
      counts[line.level] += 1;
    }
    targets.set(target, counts);
    if (line.level !== "WARN" && line.level !== "ERROR") {
      continue;
    }
    const allowed = kind === "agent" && allowedReset(line, windows, used);
    if (line.level === "ERROR" && !allowed) {
      if (line.target === TEXTS.panicTarget) {
        panics += 1;
      } else {
        errors += 1;
      }
    }
    const shown = `${shownMessage(line, context)}${allowed ? ` ${ALLOWED}` : ""}`;
    const key = `${line.level}\u0000${target}\u0000${shown}`;
    const row = rows.get(key) ?? { level: line.level, target, shown, count: 0 };
    row.count += 1;
    rows.set(key, row);
  }
  const report = [
    `${label}: ${log.lines.length + log.invalid} lines`,
    `By level: ${LEVELS.filter(
      (level) => ["ERROR", "WARN", "INFO"].includes(level) || levels.get(level) > 0,
    )
      .map((level) => `${level} ${levels.get(level)}`)
      .join(", ")}`,
    "By target:",
    ...[...targets]
      .sort(
        ([a], [b]) =>
          (a === ANOTHER_TARGET) - (b === ANOTHER_TARGET) || (a < b ? -1 : a > b ? 1 : 0),
      )
      .map(
        ([target, counts]) =>
          `  ${target}  INFO ${counts.INFO}  WARN ${counts.WARN}  ERROR ${counts.ERROR}`,
      ),
    "Warnings and errors:",
  ];
  const sorted = [...rows.values()].sort(
    (a, b) =>
      (a.level === "ERROR" ? 0 : 1) - (b.level === "ERROR" ? 0 : 1) ||
      b.count - a.count ||
      (a.target < b.target ? -1 : a.target > b.target ? 1 : 0),
  );
  report.push(
    ...(sorted.length === 0
      ? ["  none"]
      : sorted.map((row) => `  ${row.level}  ${row.target}  ×${row.count}  ${row.shown}`)),
  );
  if (log.invalid > 0) {
    reasons.add(`${log.invalid} lines of the ${kind}'s log aren't its JSON lines`);
  }
  const stray = linesOf(start.stderr ?? "").filter((line) => !CARGO.test(line)).length;
  if (stray > 0) {
    reasons.add(`${stray} lines of the ${kind}'s stderr aren't cargo's`);
  }
  if (errors > 0) {
    reasons.add(`${errors} ERROR lines in the ${kind}'s log (listed above)`);
  }
  if (panics > 0) {
    reasons.add(`${panics} panics in the ${kind}'s log`);
  }
  for (const reason of endReasons(kind, start, log.lines)) {
    reasons.add(reason);
  }
  return report;
}

/**
 * Checks the logs in `folder`: `{ code, text }`, the text being everything to print.
 * `context` holds the sources' literals, Cargo.lock's crates and the module paths.
 */
export function checkLogs(folder, context) {
  if (!existsSync(folder) || !statSync(folder).isDirectory()) {
    return {
      code: 2,
      text: "No logs to check: the logs folder doesn't exist. just dod-server and just dod-agent write it.\n",
    };
  }
  const { starts, others } = readStarts(folder);
  const servers = starts.server.size;
  const agents = starts.agent.size;
  if (servers === 0 && agents === 0) {
    return { code: 2, text: "No logs to check: the logs folder holds no server or agent start.\n" };
  }
  const count = (n, kind) => `${n} ${kind} start${n === 1 ? "" : "s"}`;
  const ignored = others === 0 ? "" : `, ${others} other file${others === 1 ? "" : "s"} ignored`;
  const out = [
    `afkfleet's DoD log check: ${count(servers, "server")}, ${count(agents, "agent")}${ignored}`,
  ];
  const reasons = new Set();
  for (const [kind, n] of [
    ["server", servers],
    ["agent", agents],
  ]) {
    if (n === 0) {
      reasons.add(`no ${kind} start in the logs folder`);
    } else if (n > 1) {
      reasons.add(`${n} ${kind} starts in the logs folder; the run is one start of each`);
    }
  }
  if (reasons.size === 0) {
    const [server] = starts.server.values();
    const [agent] = starts.agent.values();
    for (const [kind, start] of [
      ["server", server],
      ["agent", agent],
    ]) {
      for (const [part, word] of [
        ["log", "log"],
        ["stderr", "stderr"],
      ]) {
        if (start[part] === null) {
          reasons.add(`the ${kind}'s start has no ${word} file`);
        }
      }
    }
    const serverLog = readLog(server.log);
    const agentLog = readLog(agent.log);
    const bots = judgeBots(serverLog.lines, agentLog.lines, reasons);
    out.push(
      "",
      ...judgeProcess("server", server, serverLog, context, bots.windows, reasons),
      "",
      ...judgeProcess("agent", agent, agentLog, context, bots.windows, reasons),
      "",
      ...bots.report,
    );
  }
  out.push("");
  if (reasons.size === 0) {
    out.push(PASS);
  } else {
    out.push("FAIL:", ...[...reasons].map((reason) => `- ${reason}`));
  }
  return { code: reasons.size === 0 ? 0 : 1, text: `${out.join("\n")}\n` };
}

/**
 * What an error is called in the check's output: its system code (`ENOENT`, `EBUSY`, …), or
 * "unknown". Never its message, which names the path.
 */
export function errorCode(error) {
  const code = error?.code;
  return typeof code === "string" && /^[A-Z][A-Z0-9_]*$/.test(code) ? code : "unknown";
}

/** The script: checks the one folder it's given, against this repository's sources. */
export async function main(args, io = {}) {
  const out = io.stdout ?? ((text) => process.stdout.write(text));
  const err = io.stderr ?? ((text) => process.stderr.write(text));
  if (args.length !== 1) {
    err(`${USAGE}\n`);
    return 2;
  }
  const context = {
    literals: sourceLiterals(ROOT),
    crates: lockedCrates(readFileSync(path.join(ROOT, "Cargo.lock"), "utf8")),
    paths: modulePaths(ROOT),
  };
  let checked;
  try {
    checked = checkLogs(args[0], context);
  } catch (error) {
    // A dangling link, a file a scanner holds (EBUSY, EPERM) or one gone while it reads: Node's
    // own report would name the path (PR #93's review).
    err(`The check couldn't read the logs (${errorCode(error)}); nothing was judged.\n`);
    return 2;
  }
  const { code, text } = checked;
  (code === 2 ? err : out)(text);
  return code;
}

// Node runs a script by its real path, so the entry check compares with process.argv[1]'s
// (PR #78's review).
if (
  process.argv[1] !== undefined &&
  import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href
) {
  process.exitCode = await main(process.argv.slice(2));
}
