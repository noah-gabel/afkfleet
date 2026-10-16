// Tests for dev-pki.mjs. Run with `just scripts-test`.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { afterEach, beforeEach, describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { ensurePki, NAMES } from "./dev-pki.mjs";

const SCRIPT = path.join(path.dirname(fileURLToPath(import.meta.url)), "dev-pki.mjs");

let dir;

beforeEach(() => {
  dir = mkdtempSync(path.join(tmpdir(), "dev-pki-"));
});

afterEach(() => {
  rmSync(dir, { recursive: true, force: true });
});

/** The four files in `folder`. */
function files(folder) {
  return ["ca.crt", "ca.key", "server.crt", "server.key"].map((name) => path.join(folder, name));
}

/** A stand-in for pki init that counts its runs and ends with `status`. */
function counting(status) {
  const init = () => {
    init.runs += 1;
    return status;
  };
  init.runs = 0;
  return init;
}

describe("ensurePki", () => {
  it("keeps a PKI whose four files exist, without running pki init or reading them", () => {
    const paths = files(dir);
    for (const file of paths) {
      writeFileSync(file, "the dev PKI");
    }
    const init = counting(0);

    const result = ensurePki(paths, init);

    assert.equal(result, "kept");
    assert.equal(init.runs, 0);
    for (const file of paths) {
      assert.equal(readFileSync(file, "utf8"), "the dev PKI");
    }
  });

  it("runs pki init once, in a folder it creates, when no file exists", () => {
    const paths = files(path.join(dir, "target", "dev-server", "pki"));
    const init = counting(0);

    const result = ensurePki(paths, init);

    assert.equal(result, "created");
    assert.equal(init.runs, 1);
    assert.ok(existsSync(path.dirname(paths[0])));
  });

  it("leaves a partial PKI to pki init, which refuses it, and reports the failure", () => {
    const paths = files(dir);
    writeFileSync(paths[0], "the old CA");
    const init = counting(1);

    const result = ensurePki(paths, init);

    assert.equal(result, "failed");
    assert.equal(init.runs, 1);
    assert.equal(readFileSync(paths[0], "utf8"), "the old CA");
  });

  it("names the dev server certificate for every address just dev-enroll may dial", () => {
    assert.deepEqual(NAMES, ["--dns", "localhost", "--ip", "127.0.0.1", "--ip", "::1"]);
  });
});

describe("the command line", () => {
  it("refuses anything but four absolute paths with exit 2", () => {
    for (const args of [
      [],
      files(dir).slice(0, 3),
      ["ca.crt", "ca.key", "server.crt", "server.key"],
    ]) {
      const ran = spawnSync(process.execPath, [SCRIPT, ...args], { encoding: "utf8" });

      assert.equal(ran.status, 2, JSON.stringify(args));
      assert.match(ran.stderr, /^usage: /);
    }
  });

  it("does nothing and exits 0 when the PKI exists", () => {
    const paths = files(dir);
    for (const file of paths) {
      writeFileSync(file, "the dev PKI");
    }

    const ran = spawnSync(process.execPath, [SCRIPT, ...paths], { encoding: "utf8" });

    assert.equal(ran.status, 0, ran.stderr);
    assert.equal(ran.stdout, "");
  });
});
