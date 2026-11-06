// Tests for scripts/update-rehearsal.mjs (ADR-0022, R20): the user's rehearsal of an update on their
// own PC. No test builds or serves anything for real: the build is a stand-in, and the parts that
// decide what's built, signed and served are tested alone.
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterEach, describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { recipeBody } from "./justfile.mjs";
import {
  FIRST,
  installerFile,
  main,
  overlay,
  REHEARSAL_IDENTIFIER,
  route,
  SECOND,
  steps,
} from "./update-rehearsal.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

const made = [];

afterEach(() => {
  for (const dir of made.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

/** A sink for stdout or stderr. */
function sink() {
  const parts = [];
  return { write: (text) => parts.push(text), text: () => parts.join("") };
}

describe("update-rehearsal", () => {
  it("is what its recipe runs", () => {
    assert.deepEqual(recipeBody("update-rehearsal"), ["node scripts/update-rehearsal.mjs"]);
  });

  it("builds an app of its own: identifier, product and binary, the throwaway key, loopback", () => {
    const config = overlay({ version: FIRST, pubkey: "a2V5", port: 47123 });

    assert.deepEqual(config, {
      identifier: "io.github.noah-gabel.afkfleet.rehearsal",
      productName: "afkfleet rehearsal",
      mainBinaryName: "afkfleet-rehearsal",
      version: "0.0.1",
      plugins: {
        updater: { pubkey: "a2V5", endpoints: ["http://127.0.0.1:47123/latest.json"] },
      },
    });
    assert.equal(REHEARSAL_IDENTIFIER, config.identifier);
  });

  it("keeps an identifier of its own and points the updater at loopback alone", () => {
    const real = JSON.parse(
      readFileSync(path.join(ROOT, "apps", "desktop", "src-tauri", "tauri.conf.json"), "utf8"),
    ).identifier;
    const config = overlay({ version: SECOND, pubkey: "a2V5", port: 50000 });

    // Its own name keeps it off the installed app; plain http to 127.0.0.1 is what the app's
    // policy turns updates on for in a debug build (src-tauri/src/update.rs, `updates_on`).
    assert.notEqual(config.identifier, real);
    assert.deepEqual(config.plugins.updater.endpoints, ["http://127.0.0.1:50000/latest.json"]);
  });

  it("rehearses an update from 0.0.1 to 0.0.2, never the real app's versions", () => {
    assert.equal(FIRST, "0.0.1");
    assert.equal(SECOND, "0.0.2");
    assert.equal(installerFile(SECOND), "afkfleet-rehearsal_0.0.2_x64-setup.exe");
  });

  it("serves the manifest and the newer installer alone, nothing else", () => {
    const files = { "latest.json": "/r/latest.json", [installerFile(SECOND)]: "/r/second.exe" };

    assert.equal(route("/latest.json", files), "/r/latest.json");
    assert.equal(route(`/${installerFile(SECOND)}`, files), "/r/second.exe");
    for (const other of [
      "/",
      `/${installerFile(FIRST)}`,
      "/key",
      "/../latest.json",
      "/latest.json?x=1",
      "/LATEST.JSON",
    ]) {
      assert.equal(route(other, files), null, other);
    }
  });

  it("tells the user each step and what success looks like, and how to remove it all", () => {
    const text = steps({ folder: "C:\\r", first: "C:\\r\\first.exe", port: 47123 });

    for (const expected of [
      "C:\\r\\first.exe",
      'Start "afkfleet rehearsal"',
      "afkfleet 0.0.2 is available.",
      "Install and restart",
      "no row offers an update any more",
      '"afkfleet rehearsal" at version 0.0.2',
      "Settings › Apps",
      "Delete the application data",
      "Ctrl+C",
    ]) {
      assert.ok(text.includes(expected), expected);
    }
  });

  it("refuses arguments, and a folder an earlier rehearsal left, building nothing", async () => {
    const root = mkdtempSync(path.join(tmpdir(), "afkfleet-rehearsal-"));
    made.push(root);
    mkdirSync(path.join(root, "target", "update-rehearsal"), { recursive: true });
    writeFileSync(path.join(root, "target", "update-rehearsal", "x"), "x");
    const built = [];
    const io = () => ({
      root,
      out: sink(),
      err: sink(),
      runCli: (args) => {
        built.push(args);
        return 0;
      },
    });

    const usage = io();
    const left = io();

    assert.equal(await main(["--now"], usage), 2);
    assert.equal(await main([], left), 1);
    assert.match(left.err.text(), /update-rehearsal exists/);
    assert.deepEqual(built, []);
  });
});
