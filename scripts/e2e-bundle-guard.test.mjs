// Tests for e2e-bundle-guard.mjs. Run with `just scripts-test`; `just e2e` runs the check on
// the real builds.
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { describe, it } from "node:test";

import { check, MARKER } from "./e2e-bundle-guard.mjs";

/** Bundles in a temporary folder, each `name: { path: text }`, removed after `use`. */
function withBundles(bundles, use) {
  const root = mkdtempSync(join(tmpdir(), "e2e-guard-"));
  try {
    for (const [name, files] of Object.entries(bundles)) {
      mkdirSync(join(root, name), { recursive: true });
      for (const [path, text] of Object.entries(files)) {
        mkdirSync(dirname(join(root, name, path)), { recursive: true });
        writeFileSync(join(root, name, path), text);
      }
    }
    use((name) => join(root, name));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

const CLEAN = { "index.html": "<div id=root>", "assets/app.js": "invoke('me')" };
const E2E = {
  "index.html": "<div>",
  "assets/app.js": `dataset.build="${MARKER}";fetch("/e2e/ipc/"+c)`,
};

describe("check", () => {
  it("passes clean shipped bundles and an e2e bundle with its marker", () => {
    withBundles({ desktop: CLEAN, ui: CLEAN, e2e: E2E }, (at) => {
      assert.deepEqual(check({ shipped: [at("desktop"), at("ui")], e2e: at("e2e") }), []);
    });
  });

  it("names a shipped file that holds the marker or fetchInvoke's path", () => {
    withBundles(
      {
        desktop: { "assets/app.js": `x="${MARKER}"` },
        ui: { "assets/web.js": 'fetch("/e2e/ipc/me")' },
        e2e: E2E,
      },
      (at) => {
        assert.deepEqual(check({ shipped: [at("desktop"), at("ui")], e2e: at("e2e") }), [
          `${at("desktop")}/assets/app.js holds "${MARKER}": the e2e build reached a shipped bundle`,
          `${at("ui")}/assets/web.js holds "/e2e/ipc/": the e2e build reached a shipped bundle`,
        ]);
      },
    );
  });

  it("names a shipped file that holds the e2e build's live events hook", () => {
    withBundles(
      {
        desktop: { "assets/app.js": 'window["__afkfleetE2ePush"]=p' },
        e2e: E2E,
      },
      (at) => {
        assert.deepEqual(check({ shipped: [at("desktop")], e2e: at("e2e") }), [
          `${at("desktop")}/assets/app.js holds "__afkfleetE2ePush": the e2e build reached a shipped bundle`,
        ]);
      },
    );
  });

  it("fails an e2e bundle without the marker: the search would prove nothing", () => {
    withBundles({ desktop: CLEAN, e2e: CLEAN }, (at) => {
      assert.deepEqual(check({ shipped: [at("desktop")], e2e: at("e2e") }), [
        `${at("e2e")} doesn't hold the marker, so the check proves nothing`,
      ]);
    });
  });

  it("fails a bundle that wasn't built", () => {
    withBundles({ empty: {}, e2e: E2E }, (at) => {
      assert.deepEqual(check({ shipped: [at("missing"), at("empty")], e2e: at("e2e") }), [
        `${at("missing")} doesn't exist: build it first (just e2e)`,
        `${at("empty")} holds no files: build it first (just e2e)`,
      ]);
    });
  });
});
