// Tests for dev-vault-key.mjs. Run with `just scripts-test`.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { afterEach, beforeEach, describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { ensureKey, KEY_BYTES } from "./dev-vault-key.mjs";

const SCRIPT = path.join(path.dirname(fileURLToPath(import.meta.url)), "dev-vault-key.mjs");

let dir;

beforeEach(() => {
  dir = mkdtempSync(path.join(tmpdir(), "dev-vault-key-"));
});

afterEach(() => {
  rmSync(dir, { recursive: true, force: true });
});

describe("ensureKey", () => {
  it("writes 32 random bytes as standard padded base64 and a line break", () => {
    const file = path.join(dir, "vault-key-1");

    const result = ensureKey(file);

    assert.equal(result, "created");
    const text = readFileSync(file, "utf8");
    assert.match(text, /^[A-Za-z0-9+/]{43}=\n$/);
    assert.equal(Buffer.from(text.trimEnd(), "base64").length, KEY_BYTES);
  });

  it("never overwrites an existing file, whatever it holds", () => {
    const file = path.join(dir, "vault-key-1");
    writeFileSync(file, "the operator's own key\n");

    const result = ensureKey(file);

    assert.equal(result, "kept");
    assert.equal(readFileSync(file, "utf8"), "the operator's own key\n");
  });

  it("keeps a key it created on the next run", () => {
    const file = path.join(dir, "vault-key-1");
    ensureKey(file);
    const first = readFileSync(file, "utf8");

    const result = ensureKey(file);

    assert.equal(result, "kept");
    assert.equal(readFileSync(file, "utf8"), first);
  });

  it("creates a missing folder", () => {
    const file = path.join(dir, "target", "dev-server", "vault-key-1");

    assert.equal(ensureKey(file), "created");
    assert.ok(statSync(file).isFile());
  });

  it("makes two different keys for two files", () => {
    ensureKey(path.join(dir, "a"));
    ensureKey(path.join(dir, "b"));

    assert.notEqual(
      readFileSync(path.join(dir, "a"), "utf8"),
      readFileSync(path.join(dir, "b"), "utf8"),
    );
  });

  it("writes the file owner-only on Unix", { skip: process.platform === "win32" }, () => {
    const file = path.join(dir, "vault-key-1");

    ensureKey(file);

    assert.equal(statSync(file).mode & 0o777, 0o600);
  });
});

describe("the command", () => {
  it("never prints the key", () => {
    const file = path.join(dir, "vault-key-1");

    const run = spawnSync(process.execPath, [SCRIPT, file], { encoding: "utf8" });

    assert.equal(run.status, 0, run.stderr);
    const key = readFileSync(file, "utf8").trimEnd();
    assert.ok(!run.stdout.includes(key) && !run.stderr.includes(key));
    assert.equal(run.stdout, `created the dev vault key at ${file}\n`);
  });

  it("prints nothing when the key exists", () => {
    const file = path.join(dir, "vault-key-1");
    ensureKey(file);

    const run = spawnSync(process.execPath, [SCRIPT, file], { encoding: "utf8" });

    assert.equal(run.status, 0, run.stderr);
    assert.equal(run.stdout, "");
  });

  it("refuses a relative path or a missing one as a usage error", () => {
    for (const args of [[], ["relative/vault-key-1"], ["/a", "/b"]]) {
      const run = spawnSync(process.execPath, [SCRIPT, ...args], { encoding: "utf8" });

      assert.equal(run.status, 2, `${args}: ${run.stderr}`);
    }
  });
});
