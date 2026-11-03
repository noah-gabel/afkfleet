// Runs one of the DoD checks' binaries and keeps its log (ADR-0020, S134, U132).
//
//   node scripts/dod-log.mjs <absolute folder> <server|agent> <command> [arguments…]
//
// `just dod-server` and `just dod-agent` run their binary through it, with JSON at info, so
// the 24-hour run's logs are kept in target/dod-run/logs for `just dod-log-check`. Each start
// gets three files of its own, named by its kind and its UTC second and created exclusively,
// so nothing is ever appended to or overwritten: `<kind>-<stamp>.jsonl` holds stdout, every
// byte as received; `.stderr.txt` stderr; `.exit.json` the command's exit code or signal,
// once it ends. The terminal gets each stdout line short and readable, written only after
// the line is in the file, and stderr unchanged.
//
// The command shares this console, so Ctrl+C reaches it as it would without the wrapper; the
// wrapper itself keeps running through Ctrl+C and Ctrl+Break until the command has ended and
// its last lines are written, forwards SIGTERM, and exits with the command's code: 1 when it
// was ended by a signal or couldn't start, 2 on a usage error. dod-owner, dod-enrollment-token
// and dod-enroll never run through it: their streams carry the password prompt and the bundle.
// Tests: scripts/dod-log.test.mjs.
import { spawn } from "node:child_process";
import {
  closeSync,
  existsSync,
  mkdirSync,
  openSync,
  realpathSync,
  writeFileSync,
  writeSync,
} from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

/** The repository, found from this script's place. */
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
/** The kinds of binary it runs, each its file names' first word. */
const KINDS = ["server", "agent"];
/** A start's file: its kind, its UTC second and which of the three it is. */
export const LOG_FILE = /^(server|agent)-(\d{8}T\d{6}Z)\.(jsonl|stderr\.txt|exit\.json)$/;
/** The keys of a JSON line that its readable form shows in their own places, or not at all. */
const SHAPE = new Set(["timestamp", "level", "target", "message", "span", "spans"]);
/** The signals the wrapper outlives while the command runs; SIGTERM is passed on to it. */
const OUTLIVED = ["SIGINT", "SIGBREAK"];
const USAGE =
  "usage: node scripts/dod-log.mjs <absolute folder> <server|agent> <command> [arguments…]";

/**
 * The command line: an absolute folder, a kind, then the command and its arguments. Returns
 * `{ folder, kind, command, args }`, or null for anything else.
 */
export function parseArgs(args) {
  const [folder, kind, command, ...rest] = args;
  if (folder === undefined || !path.isAbsolute(folder) || !KINDS.includes(kind)) {
    return null;
  }
  return command === undefined || command === "" ? null : { folder, kind, command, args: rest };
}

/** `date`'s UTC second, as in `20261103T190005Z`. */
function stamp(date) {
  return date
    .toISOString()
    .replace(/[-:]/g, "")
    .replace(/\.\d{3}Z$/, "Z");
}

/** The three files of a `kind` start at `date` in `folder`, and their common name. */
export function logFiles(folder, kind, date) {
  const name = `${kind}-${stamp(date)}`;
  return {
    name,
    log: path.join(folder, `${name}.jsonl`),
    stderr: path.join(folder, `${name}.stderr.txt`),
    exit: path.join(folder, `${name}.exit.json`),
  };
}

/**
 * How the terminal names `folder`: its path in the repository, with slashes, or only its
 * name when it lies outside, so no absolute path (a home folder's name) is ever printed.
 */
export function shownFolder(folder, root = ROOT) {
  const relative = path.relative(root, folder);
  if (relative === "" || relative.startsWith("..") || path.isAbsolute(relative)) {
    return path.basename(folder);
  }
  return relative.split(path.sep).join("/");
}

/** Whether `c` is a C0 or C1 control character, an escape among them. */
export function isControl(c) {
  const point = c.codePointAt(0);
  return point < 0x20 || (point >= 0x7f && point < 0xa0);
}

/** `text` with every control character written out (`\u{1b}`), so none reaches the terminal. */
function visible(text) {
  return [...text]
    .map((c) => (isControl(c) ? `\\u{${c.codePointAt(0).toString(16)}}` : c))
    .join("");
}

/** A field's value as the readable line shows it: bare when it can be, else quoted or JSON. */
function shownValue(value) {
  if (typeof value === "string") {
    return /^[^\s"=]+$/.test(value) ? visible(value) : visible(JSON.stringify(value));
  }
  return visible(JSON.stringify(value));
}

/** HH:MM:SS of `ms` on this PC's clock. */
function localClock(ms) {
  const date = new Date(ms);
  return [date.getHours(), date.getMinutes(), date.getSeconds()]
    .map((part) => String(part).padStart(2, "0"))
    .join(":");
}

/**
 * One stdout line as the terminal shows it (U132): "HH:MM:SS LEVEL target: message key=value
 * …", the event's fields first, then its span's. A line that isn't one of the binaries' JSON
 * lines, or that can't be formatted, comes back as it is.
 */
export function terminalLine(text, clock = localClock) {
  try {
    const fields = JSON.parse(text);
    if (fields === null || typeof fields !== "object" || Array.isArray(fields)) {
      return text;
    }
    const { timestamp, level, target, message, span } = fields;
    if ([timestamp, level, target].some((part) => typeof part !== "string")) {
      return text;
    }
    const at = Date.parse(timestamp.replace(/(\.\d{3})\d+/, "$1"));
    if (Number.isNaN(at)) {
      return text;
    }
    const spanFields =
      span !== null && typeof span === "object" && !Array.isArray(span)
        ? Object.entries(span).filter(([key]) => key !== "name")
        : [];
    const pairs = [...Object.entries(fields).filter(([key]) => !SHAPE.has(key)), ...spanFields]
      .map(([key, value]) => ` ${visible(key)}=${shownValue(value)}`)
      .join("");
    const said = message === undefined ? "" : visible(String(message));
    return `${clock(at)} ${visible(level).padStart(5)} ${visible(target)}: ${said}${pairs}`;
  } catch {
    return text;
  }
}

/**
 * Cuts the bytes it's given into lines and hands each to `onLine` without its line break,
 * however the chunks split them; `end()` hands on a last line that had no break.
 */
export function lineReader(onLine) {
  let pending = Buffer.alloc(0);
  return {
    push(chunk) {
      pending = Buffer.concat([pending, chunk]);
      let at = pending.indexOf(0x0a);
      while (at !== -1) {
        onLine(pending.subarray(0, at).toString("utf8").replace(/\r$/, ""));
        pending = pending.subarray(at + 1);
        at = pending.indexOf(0x0a);
      }
    },
    end() {
      if (pending.length > 0) {
        onLine(pending.toString("utf8"));
        pending = Buffer.alloc(0);
      }
    },
  };
}

/** Calls `write`, so the terminal's trouble (a closed console) never stops the log. */
function tryWrite(write, data) {
  try {
    write(data);
  } catch {
    // The file already has it; the terminal is only a copy.
  }
}

/**
 * Runs `command` with `args`, its stdout and stderr into `files` and the terminal, and
 * resolves to the code to exit with, once its exit record is written. `io` replaces the
 * terminal (`stdout`, `stderr`), the clock and the process's signals, for the tests.
 */
export function runLogged(files, command, args, io = {}) {
  const out = io.stdout ?? ((data) => process.stdout.write(data));
  const err = io.stderr ?? ((data) => process.stderr.write(data));
  const clock = io.clock ?? localClock;
  const signals = io.signals ?? process;
  const log = openSync(files.log, "wx", 0o600);
  const stderr = openSync(files.stderr, "wx", 0o600);
  const lines = lineReader((line) => tryWrite(out, `${terminalLine(line, clock)}\n`));
  return new Promise((resolve) => {
    const child = spawn(command, args, { stdio: ["inherit", "pipe", "pipe"] });
    const outlive = () => {};
    const forward = () => child.kill("SIGTERM");
    for (const name of OUTLIVED) {
      signals.on(name, outlive);
    }
    signals.on("SIGTERM", forward);
    let ended = false;
    const end = (code, signal, failure) => {
      if (ended) {
        return;
      }
      ended = true;
      for (const name of OUTLIVED) {
        signals.off(name, outlive);
      }
      signals.off("SIGTERM", forward);
      if (failure !== undefined) {
        const said = `${command} can't be started: ${failure.message}\n`;
        writeSync(stderr, said);
        tryWrite(err, said);
      }
      closeSync(log);
      closeSync(stderr);
      writeFileSync(files.exit, `${JSON.stringify({ code, signal })}\n`, {
        flag: "wx",
        mode: 0o600,
      });
      resolve(code === null ? 1 : code);
    };
    child.stdout.on("data", (chunk) => {
      // The file first, every byte as it came; only then the terminal's copy (U132).
      writeSync(log, chunk);
      lines.push(chunk);
    });
    child.stdout.on("end", () => lines.end());
    child.stderr.on("data", (chunk) => {
      writeSync(stderr, chunk);
      tryWrite(err, chunk);
    });
    child.on("error", (failure) => end(null, null, failure));
    // `close`, not `exit`: it comes once both pipes are drained, so every line is in.
    child.on("close", (code, signal) => end(code, signal));
  });
}

/**
 * The script: refuses a wrong command line (2) and a start whose files already exist (1),
 * then says where it logs and runs the command. `io` is `runLogged`'s, with `now`.
 */
export async function main(args, io = {}) {
  const out = io.stdout ?? ((data) => process.stdout.write(data));
  const err = io.stderr ?? ((data) => process.stderr.write(data));
  const parsed = parseArgs(args);
  if (parsed === null) {
    err(`${USAGE}\n`);
    return 2;
  }
  mkdirSync(parsed.folder, { recursive: true, mode: 0o700 });
  const files = logFiles(parsed.folder, parsed.kind, io.now ?? new Date());
  const shown = shownFolder(parsed.folder);
  if ([files.log, files.stderr, files.exit].some((file) => existsSync(file))) {
    err(`${files.name} already exists in ${shown}; wait a second and start again.\n`);
    return 1;
  }
  out(`Logging to ${shown}/${files.name}.jsonl (and .stderr.txt, .exit.json).\n`);
  return runLogged(files, parsed.command, parsed.args, io);
}

// Node runs a script by its real path, so the entry check compares with process.argv[1]'s
// (PR #78's review).
if (
  process.argv[1] !== undefined &&
  import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href
) {
  process.exitCode = await main(process.argv.slice(2));
}
