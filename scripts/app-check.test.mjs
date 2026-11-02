// Tests for app-check.mjs. Run with `just scripts-test`.
import assert from "node:assert/strict";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { describe, it } from "node:test";

import { adoptCheckExe, identifierProblem } from "./app-check.mjs";

const REAL = "io.example.app";
const CHECK = "io.example.app.check";

/** A fresh folder that the test removes again. */
function tempDir(t) {
  const dir = mkdtempSync(path.join(tmpdir(), "app-check-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

/** A source exe holding `body`, and where the copy goes. */
function setUp(t, body) {
  const dir = tempDir(t);
  const source = path.join(dir, "built.exe");
  if (body !== undefined) {
    writeFileSync(source, body);
  }
  return { dir, source, dest: path.join(dir, "out", "check.exe") };
}

describe("identifierProblem", () => {
  it("takes an exe that holds the check identifier, each time in full", () => {
    assert.equal(identifierProblem(Buffer.from(`aa${CHECK}bb${CHECK}`), CHECK, REAL), null);
  });

  it("refuses an exe with only the real identifier", () => {
    assert.match(identifierProblem(Buffer.from(`aa${REAL}bb`), CHECK, REAL) ?? "", /check/);
  });

  it("refuses an exe with neither", () => {
    assert.match(identifierProblem(Buffer.from("nothing here"), CHECK, REAL) ?? "", /check/);
  });

  it("refuses identifiers the count rule can't read: the check one must extend the real one", () => {
    const one = (check, real) => identifierProblem(Buffer.from(`${check} ${real}`), check, real);

    // One of each counts equal, so without the premise an exe holding both would pass.
    assert.match(one("io.other.check", REAL) ?? "", /must start with/);
    assert.match(one(`${REAL}check`, REAL) ?? "", /must start with/);
    assert.match(one(REAL, REAL) ?? "", /must start with/);
  });

  it("holds for the app's own two identifiers", () => {
    const identifierIn = (name) =>
      JSON.parse(
        readFileSync(new URL(`../apps/desktop/src-tauri/${name}`, import.meta.url), "utf8"),
      ).identifier;
    const check = identifierIn("tauri.check.conf.json");

    assert.equal(
      identifierProblem(Buffer.from(check), check, identifierIn("tauri.conf.json")),
      null,
    );
  });

  it("refuses an exe with both, as the real one could still be what runs", () => {
    assert.match(identifierProblem(Buffer.from(`${CHECK} ${REAL}main`), CHECK, REAL) ?? "", /real/);
  });
});

describe("adoptCheckExe", () => {
  it("copies a check exe to its own path, leaving the build and no part file", (t) => {
    const { source, dest } = setUp(t, `aa${CHECK}bb`);

    assert.equal(adoptCheckExe({ source, dest, check: CHECK, real: REAL }), null);

    assert.equal(readFileSync(dest, "utf8"), `aa${CHECK}bb`);
    assert.ok(existsSync(source));
    assert.deepEqual(readdirSync(path.dirname(dest)), ["check.exe"]);
  });

  it("leaves another run's part files alone: each run has its own", (t) => {
    const { source, dest } = setUp(t, `aa${CHECK}bb`);
    mkdirSync(path.dirname(dest), { recursive: true });
    const others = [`${dest}.part`, `${dest}.1.part`];
    for (const other of others) {
      writeFileSync(other, "another run's");
    }

    assert.equal(adoptCheckExe({ source, dest, check: CHECK, real: REAL }), null);

    assert.equal(readFileSync(dest, "utf8"), `aa${CHECK}bb`);
    for (const other of others) {
      assert.equal(readFileSync(other, "utf8"), "another run's");
    }
  });

  it("leaves no part file when the copy itself fails", (t) => {
    const { dir, dest } = setUp(t, undefined);
    // A folder exists but can't be copied as a file.
    const source = path.join(dir, "a-folder.exe");
    mkdirSync(source);

    assert.throws(() => adoptCheckExe({ source, dest, check: CHECK, real: REAL }));

    assert.deepEqual(readdirSync(path.dirname(dest)), []);
  });

  it("replaces an older copy", (t) => {
    const { source, dest } = setUp(t, `new${CHECK}`);
    adoptCheckExe({ source, dest, check: CHECK, real: REAL });
    writeFileSync(source, `newer${CHECK}`);

    assert.equal(adoptCheckExe({ source, dest, check: CHECK, real: REAL }), null);

    assert.equal(readFileSync(dest, "utf8"), `newer${CHECK}`);
  });

  for (const [name, body] of [
    ["the real identifier alone", `aa${REAL}bb`],
    ["both identifiers", `${CHECK} ${REAL}main`],
  ]) {
    it(`refuses an exe with ${name}, leaving nothing to launch`, (t) => {
      const { source, dest } = setUp(t, `first${CHECK}`);
      adoptCheckExe({ source, dest, check: CHECK, real: REAL });
      writeFileSync(source, body);

      const problem = adoptCheckExe({ source, dest, check: CHECK, real: REAL });

      assert.match(problem ?? "", /identifier/);
      assert.deepEqual(readdirSync(path.dirname(dest)), []);
    });
  }

  it("reports a missing build, leaving nothing to launch", (t) => {
    const { dir, dest } = setUp(t, undefined);
    const source = path.join(dir, "missing.exe");

    const problem = adoptCheckExe({ source, dest, check: CHECK, real: REAL });

    assert.match(problem ?? "", /missing\.exe/);
    assert.equal(existsSync(dest), false);
  });

  it("deletes the older copy even when the build is missing", (t) => {
    const { source, dest } = setUp(t, `first${CHECK}`);
    adoptCheckExe({ source, dest, check: CHECK, real: REAL });
    rmSync(source);

    assert.match(adoptCheckExe({ source, dest, check: CHECK, real: REAL }) ?? "", /built\.exe/);

    assert.deepEqual(readdirSync(path.dirname(dest)), []);
  });
});
