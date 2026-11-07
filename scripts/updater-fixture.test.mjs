// Tests for scripts/updater-fixture.mjs (ADR-0022, S24, R19): the update's proof's fixture, made
// at test time with throwaway keys from the pinned CLI, signed through updater-sign's functions.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { afterEach, describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { files } from "./updater-fixture.mjs";
import { keyId, signatureFacts } from "./updater-sign.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const FIXTURE = path.join(ROOT, "scripts", "updater-fixture.mjs");

const made = [];

afterEach(() => {
  for (const dir of made.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

/** An empty folder of the test's own. */
function folder() {
  const dir = mkdtempSync(path.join(tmpdir(), "afkfleet-fixture-"));
  made.push(dir);
  return dir;
}

/** The fixture run with `args`; its exit code and output. */
function fixture(args) {
  const result = spawnSync(process.execPath, [FIXTURE, ...args], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  return { code: result.status, out: result.stdout, err: result.stderr };
}

describe("updater-fixture", () => {
  it("writes the installer, the public key and three manifests, and prints nothing", () => {
    const out = folder();

    const result = fixture([out, "http://127.0.0.1:4321", "0.9.0"]);

    assert.equal(result.code, 0, result.err);
    assert.equal(result.out, "");
    assert.deepEqual(readdirSync(out).sort(), files("0.9.0").sort());
    const pubkey = readFileSync(path.join(out, "pubkey"), "utf8").trim();
    const read = (name) => JSON.parse(readFileSync(path.join(out, name), "utf8"));
    const signed = read("latest.json");
    const unversioned = read("unversioned.json");
    const other = read("other.json");
    const url = "http://127.0.0.1:4321/afkfleet_0.9.0_x64-setup.exe";
    for (const manifest of [signed, unversioned, other]) {
      assert.equal(manifest.version, "0.9.0");
      assert.equal(manifest.platforms["windows-x86_64"].url, url);
    }
    const facts = (manifest) => signatureFacts(manifest.platforms["windows-x86_64"].signature);
    assert.equal(facts(signed).keyId, keyId(pubkey));
    assert.equal(facts(signed).version, "0.9.0");
    assert.equal(facts(unversioned).keyId, keyId(pubkey));
    assert.equal(facts(unversioned).version, null);
    assert.notEqual(facts(other).keyId, keyId(pubkey));
    assert.equal(facts(other).version, "0.9.0");
    assert.ok(readFileSync(path.join(out, "afkfleet_0.9.0_x64-setup.exe")).length >= 1024);
  });

  it("keeps no private key, and never writes outside its folder", () => {
    const out = folder();

    fixture([out, "http://127.0.0.1:4321", "0.9.0"]);

    for (const name of readdirSync(out)) {
      assert.ok(!name.endsWith(".key") && !name.endsWith(".sig"), name);
      assert.doesNotMatch(readFileSync(path.join(out, name), "latin1"), /secret key/);
    }
  });

  it("refuses a folder that isn't empty, a bad address or version, and other arguments", () => {
    const full = folder();
    writeFileSync(path.join(full, "x"), "x");
    const missing = path.join(folder(), "missing");

    for (const args of [
      [full, "http://127.0.0.1:4321", "0.9.0"],
      [missing, "http://127.0.0.1:4321", "0.9.0"],
      [folder(), "https://example.com", "0.9.0"],
      [folder(), "http://127.0.0.1:4321", "0.9"],
      [folder(), "http://127.0.0.1:4321"],
    ]) {
      const result = fixture(args);
      assert.notEqual(result.code, 0, JSON.stringify(args));
    }
    mkdirSync(missing, { recursive: true });
  });
});
