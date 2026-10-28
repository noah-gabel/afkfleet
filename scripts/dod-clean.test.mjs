// Tests for dod-clean.mjs. Run with `just scripts-test`.
//
// Every test builds its own repository in a temporary folder and never touches this one's
// target/dod-run (ADR-0020, U1, U33): `clean` gets the temporary repository as its root,
// and the command-line tests run a copy of the script placed in that repository's
// scripts/, which finds its repository from its own location.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { once } from "node:events";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  utimesSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { afterEach, beforeEach, describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { clean } from "./dod-clean.mjs";

const SCRIPT = fileURLToPath(new URL("./dod-clean.mjs", import.meta.url));

const DONE = "Removed target/dod-run and everything in it.";
const ANOTHER = "dod-clean removes only this repository's target/dod-run.";
const MISSING = "Nothing to clean: target/dod-run doesn't exist.";
const WAL =
  "afkfleet.db-wal is there: dod-server is running, or a crash left it. Stop dod-server with Ctrl+C. After a crash, start just dod-server once, stop it with Ctrl+C, then run just dod-clean again.";
const HEARTBEAT =
  "The DoD agent is running (its heartbeat is under 30 s old). Stop dod-agent with Ctrl+C, then run just dod-clean again.";

/** The message for a link at `where`. */
function linked(where) {
  return `${where} is a link or junction: dod-clean doesn't follow it. Remove the link itself, then run just dod-clean again.`;
}

/** The time the tests clean at. */
const NOW = Date.parse("2026-10-27T12:00:00Z");

let tmp;
let repo;
let target;
let folder;
let outside;

/** A link at `where` to the folder `to`: a junction on Windows, a symlink elsewhere. */
function link(to, where) {
  symlinkSync(to, where, process.platform === "win32" ? "junction" : "dir");
}

/** Writes `text` to `file`, creating its folder. */
function put(file, text) {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, text);
}

/** Sets `file`'s modification time `seconds` before NOW. */
function aged(file, seconds) {
  const at = (NOW - seconds * 1000) / 1000;
  utimesSync(file, at, at);
}

beforeEach(() => {
  tmp = mkdtempSync(path.join(tmpdir(), "dod-clean-"));
  repo = path.join(tmp, "repo");
  target = path.join(repo, "target");
  folder = path.join(target, "dod-run");
  outside = path.join(tmp, "outside");
  // What the DoD run leaves: the database, the key, the PKI, the agent's identity.
  put(path.join(folder, "afkfleet.db"), "database");
  put(path.join(folder, "vault-key-1"), "key");
  put(path.join(folder, "pki", "ca.crt"), "ca");
  put(path.join(folder, "agent", "agent.key"), "agent key");
  put(path.join(folder, "agent", "agent.alive"), "");
  aged(path.join(folder, "agent", "agent.alive"), 60);
  // What must stay: a sibling folder in target/, a file next to dod-run, and a folder
  // outside the repository that a link inside dod-run points to.
  put(path.join(target, "dev-server", "afkfleet.db"), "dev database");
  put(path.join(target, "keep.txt"), "keep");
  put(path.join(outside, "precious.txt"), "precious");
  link(outside, path.join(folder, "link"));
});

afterEach(() => {
  rmSync(tmp, { recursive: true, force: true });
});

/** `promise`, or a rejection with `what` after 5 s. */
function within(promise, what) {
  let timer;
  const late = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(what)), 5000);
  });
  return Promise.race([promise, late]).finally(() => clearTimeout(timer));
}

/** Asserts that nothing outside target/dod-run was touched. */
function outsideIsUntouched() {
  assert.equal(
    readFileSync(path.join(target, "dev-server", "afkfleet.db"), "utf8"),
    "dev database",
  );
  assert.equal(readFileSync(path.join(target, "keep.txt"), "utf8"), "keep");
  assert.equal(readFileSync(path.join(outside, "precious.txt"), "utf8"), "precious");
}

describe("clean", () => {
  it("removes target/dod-run and everything in it, and nothing outside", () => {
    const result = clean(repo, folder, NOW);

    assert.deepEqual(result, { status: 0, message: DONE });
    assert.equal(existsSync(folder), false);
    outsideIsUntouched();
  });

  it("removes a link inside as an entry, never following it", () => {
    clean(repo, folder, NOW);

    assert.equal(existsSync(path.join(folder, "link")), false);
    assert.ok(existsSync(path.join(outside, "precious.txt")));
  });

  it("refuses any other path and removes nothing", () => {
    for (const other of [target, path.join(target, "dev-server"), path.join(folder, "pki"), tmp]) {
      const result = clean(repo, other, NOW);

      assert.deepEqual(result, { status: 1, message: ANOTHER }, other);
    }
    assert.ok(existsSync(path.join(folder, "afkfleet.db")));
    outsideIsUntouched();
  });

  it("takes the folder in another case only where paths ignore case", () => {
    const result = clean(repo, folder.toUpperCase(), NOW);

    if (process.platform === "win32") {
      assert.deepEqual(result, { status: 0, message: DONE });
    } else {
      assert.deepEqual(result, { status: 1, message: ANOTHER });
    }
  });

  it("refuses a target/ that is a link, and removes nothing behind it", () => {
    const real = path.join(tmp, "real-target");
    put(path.join(real, "dod-run", "afkfleet.db"), "database");
    const linkedRepo = path.join(tmp, "linked-repo");
    mkdirSync(linkedRepo);
    link(real, path.join(linkedRepo, "target"));

    const result = clean(linkedRepo, path.join(linkedRepo, "target", "dod-run"), NOW);

    assert.deepEqual(result, { status: 1, message: linked(path.join(linkedRepo, "target")) });
    assert.ok(existsSync(path.join(real, "dod-run", "afkfleet.db")));
  });

  it("refuses a dod-run that is a link, and removes nothing behind it", () => {
    rmSync(folder, { recursive: true, force: true });
    link(outside, folder);

    const result = clean(repo, folder, NOW);

    assert.deepEqual(result, { status: 1, message: linked(folder) });
    outsideIsUntouched();
  });

  it("refuses while afkfleet.db-wal is there", () => {
    put(path.join(folder, "afkfleet.db-wal"), "");

    const result = clean(repo, folder, NOW);

    assert.deepEqual(result, { status: 1, message: WAL });
    assert.ok(existsSync(path.join(folder, "afkfleet.db")));
  });

  it("refuses while the agent's heartbeat is under 30 s old, or in the future", () => {
    for (const seconds of [10, 29, -60]) {
      aged(path.join(folder, "agent", "agent.alive"), seconds);

      const result = clean(repo, folder, NOW);

      assert.deepEqual(result, { status: 1, message: HEARTBEAT }, String(seconds));
    }
    assert.ok(existsSync(path.join(folder, "agent", "agent.key")));
  });

  it("cleans with a heartbeat 30 s old or older", () => {
    aged(path.join(folder, "agent", "agent.alive"), 30);

    const result = clean(repo, folder, NOW);

    assert.deepEqual(result, { status: 0, message: DONE });
  });

  it("finds nothing to clean when the folder, or target/, doesn't exist", () => {
    rmSync(folder, { recursive: true, force: true });

    assert.deepEqual(clean(repo, folder, NOW), { status: 0, message: MISSING });

    rmSync(target, { recursive: true, force: true });

    assert.deepEqual(clean(repo, folder, NOW), { status: 0, message: MISSING });
  });

  it("takes the folder named through a linked path to the repository (PR #78's review)", () => {
    const linkedRepo = path.join(tmp, "repo-link");
    link(repo, linkedRepo);

    const result = clean(repo, path.join(linkedRepo, "target", "dod-run"), NOW);

    assert.deepEqual(result, { status: 0, message: DONE });
    assert.equal(existsSync(folder), false);
    outsideIsUntouched();
  });

  it("stops at an error while removing, naming its cause and what to do (PR #78's review)", () => {
    const busy = path.join(folder, "agent");
    const failing = () => {
      throw Object.assign(new Error("resource busy or locked"), { code: "EBUSY", path: busy });
    };

    const result = clean(repo, folder, NOW, failing);

    assert.deepEqual(result, {
      status: 1,
      message:
        "dod-clean couldn't remove target/dod-run/agent (EBUSY): something holds it open, such as a terminal or an editor in that folder. Close it, then run just dod-clean again.",
    });
  });

  it("stops at a folder a process sits in, and says so (Windows)", {
    skip: process.platform !== "win32",
  }, async () => {
    // The child holds its working folder once it has started, so it says so first;
    // every wait is bounded.
    const inside = spawn(
      process.execPath,
      ["-e", "process.stdout.write('ready'); setTimeout(() => {}, 30000)"],
      { cwd: path.join(folder, "agent"), stdio: ["ignore", "pipe", "ignore"] },
    );
    const exited = once(inside, "exit");
    try {
      await within(once(inside.stdout, "data"), "the process in the folder never started");

      const result = clean(repo, folder, NOW);

      assert.equal(result.status, 1);
      assert.match(
        result.message,
        /^dod-clean couldn't remove target\/dod-run\/agent \(E[A-Z]+\): something holds it open/,
      );
    } finally {
      inside.kill();
      await within(exited, "the process in the folder never ended");
    }
  });
});

describe("the command line", () => {
  /** Runs the script's copy in the temporary repository with `args`. */
  function run(args) {
    const copy = path.join(repo, "scripts", "dod-clean.mjs");
    mkdirSync(path.dirname(copy), { recursive: true });
    copyFileSync(SCRIPT, copy);
    return spawnSync(process.execPath, [copy, ...args], { encoding: "utf8" });
  }

  it("cleans its own repository's folder, exit 0 and the line on stdout", () => {
    const ran = run([folder]);

    assert.equal(ran.status, 0, ran.stderr);
    assert.equal(ran.stdout, `${DONE}\n`);
    assert.equal(existsSync(folder), false);
    outsideIsUntouched();
  });

  it("runs and cleans when started through a linked path to the repository (PR #78's review)", () => {
    run([]);
    const linkedRepo = path.join(tmp, "repo-link");
    link(repo, linkedRepo);

    const ran = spawnSync(
      process.execPath,
      [
        path.join(linkedRepo, "scripts", "dod-clean.mjs"),
        path.join(linkedRepo, "target", "dod-run"),
      ],
      { encoding: "utf8" },
    );

    assert.equal(ran.status, 0, ran.stderr);
    assert.equal(ran.stdout, `${DONE}\n`);
    assert.equal(existsSync(folder), false);
    outsideIsUntouched();
  });

  it("refuses another path with exit 1, its line on stderr", () => {
    const ran = run([path.join(target, "dev-server")]);

    assert.equal(ran.status, 1);
    assert.equal(ran.stderr, `${ANOTHER}\n`);
    assert.ok(existsSync(path.join(folder, "afkfleet.db")));
  });

  it("refuses anything but one folder with exit 2", () => {
    for (const args of [[], [folder, folder]]) {
      const ran = run(args);

      assert.equal(ran.status, 2, JSON.stringify(args));
      assert.equal(ran.stderr, "usage: node scripts/dod-clean.mjs <folder>\n");
    }
    assert.ok(existsSync(path.join(folder, "afkfleet.db")));
  });
});
