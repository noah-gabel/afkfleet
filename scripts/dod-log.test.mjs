// Tests for dod-log.mjs (ADR-0020, S134, U132). Run with `just scripts-test`.
//
// Every run logs into its own temporary folder and never touches target/dod-run (U33). The
// commands are this Node with `-e`, so nothing here starts cargo or a binary of afkfleet.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { EventEmitter } from "node:events";
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { afterEach, beforeEach, describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import {
  isControl,
  LOG_FILE,
  lineReader,
  logFiles,
  main,
  parseArgs,
  runLogged,
  shownFolder,
  terminalLine,
} from "./dod-log.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const START = new Date("2026-11-03T19:00:05.123Z");
const NAME = "server-20261103T190005Z";

/** HH:MM:SS in UTC, so the tests don't depend on this PC's time zone. */
const utcClock = (ms) => new Date(ms).toISOString().slice(11, 19);

/** One of the binaries' JSON lines (ADR-0014's shape), with `fields` beside its keys. */
function jsonLine(fields = {}) {
  return JSON.stringify({
    timestamp: "2026-11-03T19:00:05.123456Z",
    level: "INFO",
    target: "fleet_server::app::agents::session_grants",
    message: "a session was granted",
    ...fields,
  });
}

/** The terminal's two streams, captured: each write in order, as text. */
function terminal(onWrite = () => {}) {
  const writes = { out: [], err: [] };
  return {
    writes,
    stdout: (data) => {
      writes.out.push(String(data));
      onWrite(String(data));
    },
    stderr: (data) => writes.err.push(String(data)),
  };
}

/** `promise`, or a failure naming `what` once `ms` pass first: no wait here is unbounded. */
async function within(promise, what, ms = 10000) {
  let timer;
  const late = new Promise((_, reject) => {
    timer = setTimeout(
      () => reject(new assert.AssertionError({ message: `${what} took over ${ms} ms` })),
      ms,
    );
  });
  try {
    return await Promise.race([promise, late]);
  } finally {
    clearTimeout(timer);
  }
}

/** A command that runs `script` in this Node. */
function node(script) {
  return [process.execPath, ["-e", script]];
}

let folder;

beforeEach(() => {
  folder = mkdtempSync(path.join(tmpdir(), "dod-log-"));
});

afterEach(() => {
  rmSync(folder, { recursive: true, force: true });
});

describe("parseArgs", () => {
  it("reads an absolute folder, a kind, and the command with its arguments", () => {
    assert.deepEqual(parseArgs([folder, "server", "cargo", "run", "--release"]), {
      folder,
      kind: "server",
      command: "cargo",
      args: ["run", "--release"],
    });
    assert.deepEqual(parseArgs([folder, "agent", "cargo"]), {
      folder,
      kind: "agent",
      command: "cargo",
      args: [],
    });
  });

  it("refuses a relative folder, another kind, or no command", () => {
    assert.equal(parseArgs(["target/dod-run/logs", "server", "cargo"]), null);
    assert.equal(parseArgs([folder, "app", "cargo"]), null);
    assert.equal(parseArgs([folder, "server"]), null);
    assert.equal(parseArgs([]), null);
  });
});

describe("logFiles", () => {
  it("names one start's three files by its kind and UTC second, as LOG_FILE reads them", () => {
    const files = logFiles(folder, "server", START);

    assert.deepEqual(files, {
      name: NAME,
      log: path.join(folder, `${NAME}.jsonl`),
      stderr: path.join(folder, `${NAME}.stderr.txt`),
      exit: path.join(folder, `${NAME}.exit.json`),
    });
    const parts = [files.log, files.stderr, files.exit].map((file) =>
      LOG_FILE.exec(path.basename(file))?.slice(1),
    );
    assert.deepEqual(parts, [
      ["server", "20261103T190005Z", "jsonl"],
      ["server", "20261103T190005Z", "stderr.txt"],
      ["server", "20261103T190005Z", "exit.json"],
    ]);
    assert.equal(LOG_FILE.exec("app-20261103T190005Z.jsonl"), null);
    assert.equal(LOG_FILE.exec(`${NAME}.jsonl.bak`), null);
  });
});

describe("shownFolder", () => {
  it("shows the folder by its path in the repository, with slashes", () => {
    assert.equal(shownFolder(path.join(ROOT, "target", "dod-run", "logs")), "target/dod-run/logs");
  });

  it("never shows an absolute path: a folder outside the repository shows by its name", () => {
    assert.equal(shownFolder(folder), path.basename(folder));
  });
});

describe("terminalLine (U132)", () => {
  it("reads a JSON line as the time, the level, the target, the message and the fields", () => {
    const line = jsonLine({
      kind: "online",
      fresh: false,
      session_request: 3,
      span: { bot_id: "0192abcd", name: "bot" },
      spans: [{ bot_id: "0192abcd", name: "bot" }],
    });

    assert.equal(
      terminalLine(line, utcClock),
      "19:00:05  INFO fleet_server::app::agents::session_grants: a session was granted kind=online fresh=false session_request=3 bot_id=0192abcd",
    );
  });

  it("quotes a value with a space, a quote or an equals sign, and shows an object as JSON", () => {
    const line = jsonLine({ level: "WARN", error: "connection refused", state: { a: 1 } });

    assert.equal(
      terminalLine(line, utcClock),
      '19:00:05  WARN fleet_server::app::agents::session_grants: a session was granted error="connection refused" state={"a":1}',
    );
  });

  it("escapes control characters JSON decoding brought back, so no escape reaches the terminal", () => {
    const line = jsonLine({ message: "kicked \u001b[31mred\u001b[0m", reason: "a\nb" });

    const shown = terminalLine(line, utcClock);

    assert.ok(![...shown].some(isControl), shown);
    assert.match(shown, /kicked \\u\{1b\}\[31mred\\u\{1b\}\[0m/);
    // A quoted value is JSON's, which writes the line break out itself.
    assert.match(shown, /reason="a\\nb"/);
  });

  it("shows a line raw that isn't JSON or isn't one of the binaries' lines", () => {
    for (const raw of [
      "not json",
      '{"timestamp":',
      "null",
      "[1,2]",
      '{"level":"INFO","target":"x"}',
      '{"timestamp":"never","level":"INFO","target":"x"}',
      '{"timestamp":"2026-11-03T19:00:05Z","level":5,"target":"x"}',
    ]) {
      assert.equal(terminalLine(raw, utcClock), raw);
    }
  });

  it("shows a line raw when formatting it fails", () => {
    const failing = () => {
      throw new Error("no clock");
    };

    assert.equal(terminalLine(jsonLine(), failing), jsonLine());
  });
});

describe("lineReader", () => {
  it("hands on whole lines however the chunks split them, then the partial last line", () => {
    const lines = [];
    const reader = lineReader((line) => lines.push(line));
    const accent = Buffer.from("é\n", "utf8");

    reader.push(Buffer.from("a\nb"));
    reader.push(Buffer.from("c\r\n"));
    reader.push(accent.subarray(0, 1));
    reader.push(accent.subarray(1));
    reader.push(Buffer.from("last"));
    assert.deepEqual(lines, ["a", "bc", "é"]);
    reader.end();

    assert.deepEqual(lines, ["a", "bc", "é", "last"]);
  });

  it("hands on nothing at the end when the last line was whole", () => {
    const lines = [];
    const reader = lineReader((line) => lines.push(line));

    reader.push(Buffer.from("a\n"));
    reader.end();

    assert.deepEqual(lines, ["a"]);
  });
});

// Real processes: each test fails after 20 s instead of waiting for ever.
describe("runLogged", { timeout: 20000 }, () => {
  it("writes every byte of stdout to the log first, as received, then the readable line to the terminal", async () => {
    const files = logFiles(folder, "server", START);
    const valid = jsonLine({ kind: "online" });
    const sent = `${valid}\n{"broken": \n${valid.slice(0, 20)}`;
    const held = [];
    const io = terminal((text) => held.push([text, readFileSync(files.log, "utf8")]));
    const [command, args] = node(
      `const s = ${JSON.stringify(sent)}; process.stdout.write(s.slice(0, 7)); process.stdout.write(s.slice(7));`,
    );

    const code = await runLogged(files, command, args, { ...io, clock: utcClock });

    assert.equal(code, 0);
    assert.deepEqual(readFileSync(files.log), Buffer.from(sent, "utf8"));
    assert.deepEqual(io.writes.out, [
      "19:00:05  INFO fleet_server::app::agents::session_grants: a session was granted kind=online\n",
      '{"broken": \n',
      `${valid.slice(0, 20)}\n`,
    ]);
    // File first: when each line reached the terminal, the log already held it.
    assert.ok(held[0][1].startsWith(`${valid}\n`));
    assert.ok(held[1][1].startsWith(`${valid}\n{"broken": \n`));
    assert.equal(held[2][1], sent);
  });

  it("still writes a line whole that it can't format, and shows it raw", async () => {
    const files = logFiles(folder, "server", START);
    const io = terminal();
    const failing = () => {
      throw new Error("no clock");
    };
    const [command, args] = node(`console.log(${JSON.stringify(jsonLine())})`);

    await runLogged(files, command, args, { ...io, clock: failing });

    assert.equal(readFileSync(files.log, "utf8").replace(/\r?\n$/, ""), jsonLine());
    assert.deepEqual(
      io.writes.out.map((text) => text.trimEnd()),
      [jsonLine()],
    );
  });

  it("copies stderr to its file and to the terminal unchanged", async () => {
    const files = logFiles(folder, "agent", START);
    const io = terminal();
    const [command, args] = node(`process.stderr.write("   Compiling x\\n\\u001b[1mtail")`);

    await runLogged(files, command, args, { ...io, clock: utcClock });

    assert.equal(readFileSync(files.stderr, "utf8"), "   Compiling x\n\u001b[1mtail");
    assert.equal(io.writes.err.join(""), "   Compiling x\n\u001b[1mtail");
    assert.equal(readFileSync(files.log, "utf8"), "");
  });

  it("records the command's exit code and returns it", async () => {
    const files = logFiles(folder, "server", START);
    const [command, args] = node("process.exit(3)");

    const code = await runLogged(files, command, args, { ...terminal(), clock: utcClock });

    assert.equal(code, 3);
    assert.deepEqual(JSON.parse(readFileSync(files.exit, "utf8")), { code: 3, signal: null });
  });

  it("returns 1 with an exit record when the command can't start", async () => {
    const files = logFiles(folder, "server", START);
    const io = terminal();

    const code = await runLogged(files, "afkfleet-no-such-command", [], { ...io, clock: utcClock });

    assert.equal(code, 1);
    assert.deepEqual(JSON.parse(readFileSync(files.exit, "utf8")), { code: null, signal: null });
    assert.match(io.writes.err.join(""), /^afkfleet-no-such-command can't be started: /);
    assert.match(
      readFileSync(files.stderr, "utf8"),
      /^afkfleet-no-such-command can't be started: /,
    );
  });

  it("keeps running through SIGINT and SIGBREAK, forwards SIGTERM, and lets go of them all at the end", async () => {
    const files = logFiles(folder, "server", START);
    const signals = new EventEmitter();
    let ready;
    const started = new Promise((resolve) => {
      ready = resolve;
    });
    const io = terminal((text) => {
      if (text.startsWith("ready")) {
        ready();
      }
    });
    const [command, args] = node('console.log("ready"); setInterval(() => {}, 1000);');

    const run = runLogged(files, command, args, { ...io, clock: utcClock, signals });
    try {
      await within(Promise.race([started, run]), "the command's first line");
      assert.deepEqual(
        ["SIGINT", "SIGBREAK", "SIGTERM"].map((name) => signals.listenerCount(name)),
        [1, 1, 1],
      );
      signals.emit("SIGINT");
      signals.emit("SIGBREAK");
      assert.equal(existsSync(files.exit), false);
    } finally {
      // Ends the command however the checks above went, so no process outlives the test.
      signals.emit("SIGTERM");
    }
    const code = await within(run, "the run's end");

    assert.equal(code, 1);
    assert.deepEqual(JSON.parse(readFileSync(files.exit, "utf8")), {
      code: null,
      signal: "SIGTERM",
    });
    assert.deepEqual(
      ["SIGINT", "SIGBREAK", "SIGTERM"].map((name) => signals.listenerCount(name)),
      [0, 0, 0],
    );
  });
});

describe("main", { timeout: 20000 }, () => {
  it("says where it logs, by the folder's shown path, then runs the command", async () => {
    const io = terminal();
    const [command, args] = node('console.log("hi")');

    const code = await main([folder, "server", command, ...args], {
      ...io,
      clock: utcClock,
      now: START,
    });

    assert.equal(code, 0);
    assert.equal(
      io.writes.out[0],
      `Logging to ${path.basename(folder)}/${NAME}.jsonl (and .stderr.txt, .exit.json).\n`,
    );
    assert.equal(readFileSync(path.join(folder, `${NAME}.jsonl`), "utf8").trim(), "hi");
  });

  it("creates a missing folder", async () => {
    const logs = path.join(folder, "logs");
    const [command, args] = node("");

    await main([logs, "agent", command, ...args], { ...terminal(), clock: utcClock, now: START });

    assert.ok(existsSync(path.join(logs, "agent-20261103T190005Z.exit.json")));
  });

  it("refuses a taken name before the command starts, and changes nothing", async () => {
    for (const part of ["jsonl", "stderr.txt", "exit.json"]) {
      rmSync(folder, { recursive: true, force: true });
      folder = mkdtempSync(path.join(tmpdir(), "dod-log-"));
      const taken = path.join(folder, `${NAME}.${part}`);
      const marker = path.join(folder, "ran");
      writeFileSync(taken, "kept");
      const io = terminal();
      const [command, args] = node(
        `require("node:fs").writeFileSync(${JSON.stringify(marker)}, "")`,
      );

      const code = await main([folder, "server", command, ...args], {
        ...io,
        clock: utcClock,
        now: START,
      });

      assert.equal(code, 1, part);
      assert.equal(readFileSync(taken, "utf8"), "kept", part);
      assert.equal(existsSync(marker), false, part);
      assert.deepEqual(
        io.writes.err,
        [`${NAME} already exists in ${path.basename(folder)}; wait a second and start again.\n`],
        part,
      );
    }
  });

  it("exits 2 with its usage on a wrong command line, starting nothing", async () => {
    const io = terminal();

    const code = await main(["relative", "server", "cargo"], {
      ...io,
      clock: utcClock,
      now: START,
    });

    assert.equal(code, 2);
    assert.deepEqual(io.writes.err, [
      "usage: node scripts/dod-log.mjs <absolute folder> <server|agent> <command> [arguments…]\n",
    ]);
  });
});

describe("the script, its terminal gone (PR #93's review)", { timeout: 20000 }, () => {
  it("keeps every line in the file and writes the exit record when the terminal's reader closes early", async () => {
    // One line, then 200 more once the test says so on stdin, which the command shares.
    const script = [
      'console.log("first");',
      'process.stdin.once("data", () => {',
      '  for (let n = 0; n < 200; n += 1) console.log("line " + n);',
      "});",
    ].join("\n");
    const wrapper = spawn(
      process.execPath,
      [path.join(ROOT, "scripts", "dod-log.mjs"), folder, "server", ...node(script).flat()],
      { stdio: ["pipe", "pipe", "pipe"] },
    );
    let said = "";
    wrapper.stderr.on("data", (chunk) => {
      said += chunk;
    });
    const ended = new Promise((resolve) => wrapper.on("close", (code) => resolve(code)));
    await within(
      new Promise((resolve) => {
        let shown = "";
        wrapper.stdout.on("data", (chunk) => {
          shown += chunk;
          if (shown.includes("first\n")) {
            resolve();
          }
        });
      }),
      "the first line on the terminal",
    );

    wrapper.stdout.destroy();
    wrapper.stdin.end("go\n");
    const code = await within(ended, "the wrapper's end");

    const names = readdirSync(folder);
    const file = (part) => {
      const name = names.find((each) => each.endsWith(part));
      assert.ok(name !== undefined, `no ${part} in ${names.join(", ")}; stderr: ${said}`);
      return readFileSync(path.join(folder, name), "utf8");
    };
    assert.equal(code, 0, said);
    assert.equal(
      file(".jsonl"),
      ["first", ...Array.from({ length: 200 }, (_, n) => `line ${n}`), ""].join("\n"),
    );
    assert.deepEqual(JSON.parse(file(".exit.json")), { code: 0, signal: null });
  });
});
