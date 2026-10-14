// Tests for dev-rotation.mjs. Run with `just scripts-test`.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { afterEach, beforeEach, describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { checkCopy, copyDatabase, Refused } from "./dev-rotation.mjs";

const SCRIPT = path.join(path.dirname(fileURLToPath(import.meta.url)), "dev-rotation.mjs");

/** Bytes that stand for a database file; the script never reads them as one. */
const DATABASE = Buffer.from([0x53, 0x51, 0x4c, 0x69, 0x74, 0x65, 0x00, 0xff, 0x01]);

let dir;
let source;
let folder;
let copy;

beforeEach(() => {
  dir = mkdtempSync(path.join(tmpdir(), "dev-rotation-"));
  source = path.join(dir, "afkfleet.db");
  folder = path.join(dir, "rotation");
  copy = path.join(folder, "afkfleet.db");
});

afterEach(() => {
  rmSync(dir, { recursive: true, force: true });
});

function run(args) {
  return spawnSync(process.execPath, [SCRIPT, ...args], { encoding: "utf8" });
}

describe("copyDatabase", () => {
  it("copies the closed dev database's bytes into a folder of its own", () => {
    writeFileSync(source, DATABASE);

    copyDatabase(source, copy);

    assert.deepEqual(readFileSync(copy), DATABASE);
    assert.deepEqual(readFileSync(source), DATABASE);
  });

  it("makes the copy owner-only on Unix", { skip: process.platform === "win32" }, () => {
    writeFileSync(source, DATABASE, { mode: 0o644 });

    copyDatabase(source, copy);

    assert.equal(statSync(copy).mode & 0o777, 0o600);
  });

  it("refuses an existing folder, empty or not, and changes nothing", () => {
    writeFileSync(source, DATABASE);
    mkdirSync(folder);
    writeFileSync(copy, "an earlier copy");

    assert.throws(() => copyDatabase(source, copy), {
      name: "Error",
      message: `${folder} exists; delete it to start over`,
    });
    assert.equal(readFileSync(copy, "utf8"), "an earlier copy");
    rmSync(copy);
    assert.throws(() => copyDatabase(source, copy), Refused);
    assert.ok(!existsSync(copy));
  });

  it("refuses while a -wal file sits next to the dev database, before creating anything", () => {
    writeFileSync(source, DATABASE);
    writeFileSync(`${source}-wal`, "");

    assert.throws(() => copyDatabase(source, copy), {
      message: `${source}-wal exists: stop \`just dev-server\` first`,
    });
    assert.ok(!existsSync(folder));
  });

  it("refuses a missing dev database, before creating anything", () => {
    assert.throws(() => copyDatabase(source, copy), {
      message: `there's no dev database at ${source}; run \`just dev-server\` first`,
    });
    assert.ok(!existsSync(folder));
  });

  it("checks the folder before the -wal file", () => {
    writeFileSync(source, DATABASE);
    writeFileSync(`${source}-wal`, "");
    mkdirSync(folder);

    assert.throws(() => copyDatabase(source, copy), {
      message: `${folder} exists; delete it to start over`,
    });
  });
});

describe("checkCopy", () => {
  it("accepts an existing copy", () => {
    mkdirSync(folder);
    writeFileSync(copy, DATABASE);

    assert.doesNotThrow(() => checkCopy(copy));
  });

  it("refuses a missing copy", () => {
    assert.throws(() => checkCopy(copy), {
      message: `there's no copy at ${copy}; run \`just dev-rotate-key\` first`,
    });
    assert.throws(() => checkCopy(copy), Refused);
  });
});

describe("the command", () => {
  it("copies and says where", () => {
    writeFileSync(source, DATABASE);

    const result = run(["copy", source, copy]);

    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, `copied ${source} to ${copy}\n`);
    assert.deepEqual(readFileSync(copy), DATABASE);
  });

  it("reports a refusal on stderr and exits 1", () => {
    writeFileSync(source, DATABASE);
    writeFileSync(`${source}-wal`, "");

    const result = run(["copy", source, copy]);

    assert.equal(result.status, 1);
    assert.equal(result.stdout, "");
    assert.equal(
      result.stderr,
      `dev-rotation: ${source}-wal exists: stop \`just dev-server\` first\n`,
    );
  });

  it("checks a copy: 0 when it exists, 1 when it doesn't", () => {
    const missing = run(["check", copy]);
    mkdirSync(folder);
    writeFileSync(copy, DATABASE);
    const present = run(["check", copy]);

    assert.equal(missing.status, 1);
    assert.equal(
      missing.stderr,
      `dev-rotation: there's no copy at ${copy}; run \`just dev-rotate-key\` first\n`,
    );
    assert.equal(present.status, 0, present.stderr);
    assert.equal(present.stdout, "");
  });

  it("refuses anything else as a usage error", () => {
    for (const args of [
      [],
      ["copy", source],
      ["copy", "relative.db", copy],
      ["copy", source, "relative/copy.db"],
      ["check"],
      ["check", "relative.db"],
      ["check", copy, copy],
      ["move", source, copy],
    ]) {
      const result = run(args);

      assert.equal(result.status, 2, `${args}: ${result.stderr}`);
    }
    assert.ok(!existsSync(folder));
  });
});
