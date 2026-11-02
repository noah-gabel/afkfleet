// Tests for e2e-bundle-guard.mjs. Run with `just scripts-test`; `just e2e` runs the check on
// the real builds.
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { describe, it } from "node:test";

import { check, MARKER, NEEDLES, PUSH_HOOK, ROOT } from "./e2e-bundle-guard.mjs";

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

/** The fonts every bundle ships, as Vite names them, and the CSS that points at them. */
const FONTS = {
  "assets/geist-latin-wght-normal-abc123.woff2": "font",
  "assets/ibm-plex-mono-latin-400-normal-abc123.woff2": "font",
  "assets/ibm-plex-mono-latin-600-normal-abc123.woff2": "font",
  "assets/app.css":
    "@font-face{src:url(/assets/geist-latin-wght-normal-abc123.woff2)}@font-face{src:url(/assets/ibm-plex-mono-latin-400-normal-abc123.woff2)}@font-face{src:url(/assets/ibm-plex-mono-latin-600-normal-abc123.woff2)}",
};
const CLEAN = { "index.html": "<div id=root>", "assets/app.js": "invoke('me')", ...FONTS };
const E2E = {
  "index.html": "<div>",
  "assets/app.js": `dataset.build="${MARKER}";fetch("/e2e/ipc/"+c);window["${PUSH_HOOK}"]=p`,
  ...FONTS,
};

/** The e2e bundle without `needle`. */
function without(needle) {
  return { ...E2E, "assets/app.js": E2E["assets/app.js"].replace(needle, "x") };
}

/** The value `name` an e2e source file exports, as its text spells it. */
function exported(path, name) {
  const text = readFileSync(join(ROOT, path), "utf8");
  return new RegExp(`export const ${name} = "([^"]*)";`).exec(text)?.[1];
}

describe("check's fonts (ADR-0020, S111)", () => {
  const problemsOf = (changes) => {
    let found = [];
    withBundles({ desktop: { ...CLEAN, ...changes }, ui: CLEAN, e2e: E2E }, (at) => {
      found = check({ shipped: [at("desktop"), at("ui")], e2e: at("e2e") });
    });
    return found;
  };

  for (const [name, css] of [
    ["data:font", "@font-face{src:url(data:font/woff2;base64,AAAA)}"],
    ["data:application/font", "@font-face{src:url(data:application/font-woff2;base64,AAAA)}"],
    ["data:application/x-font", "@font-face{src:url(data:application/x-font-woff;base64,AAAA)}"],
  ]) {
    it(`refuses an inlined font, ${name}, which the CSP would block`, () => {
      const problems = problemsOf({ "assets/app.css": `${FONTS["assets/app.css"]}${css}` });
      assert.equal(problems.length, 1, problems.join());
      assert.match(problems[0] ?? "", /inlined font/);
    });
  }

  for (const face of ["geist-latin-wght", "ibm-plex-mono-latin-400", "ibm-plex-mono-latin-600"]) {
    it(`refuses a bundle without ${face}`, () => {
      let found = [];
      const desktop = Object.fromEntries(
        Object.entries(CLEAN).filter(([path]) => !path.includes(face)),
      );
      withBundles({ desktop, ui: CLEAN, e2e: E2E }, (at) => {
        found = check({ shipped: [at("desktop"), at("ui")], e2e: at("e2e") });
      });
      assert.ok(
        found.some((problem) => problem.includes(`no ${face} font`)),
        found.join(),
      );
    });
  }

  it("refuses a font the CSS names and the bundle doesn't hold", () => {
    const problems = problemsOf({
      "assets/app.css": `${FONTS["assets/app.css"]}@font-face{src:url(/assets/gone.woff2)}`,
    });
    assert.deepEqual(
      problems.map((problem) => problem.includes("gone.woff2")),
      [true],
    );
  });

  it("checks the e2e bundle too, which must render what ships", () => {
    withBundles(
      {
        desktop: CLEAN,
        ui: CLEAN,
        e2e: { ...E2E, "assets/geist-latin-wght-normal-abc123.woff2": "" },
      },
      (at) => {
        assert.deepEqual(check({ shipped: [at("desktop"), at("ui")], e2e: at("e2e") }), []);
      },
    );
    withBundles(
      { desktop: CLEAN, ui: CLEAN, e2e: { "assets/app.js": E2E["assets/app.js"] } },
      (at) => {
        assert.ok(check({ shipped: [at("desktop"), at("ui")], e2e: at("e2e") }).length > 0);
      },
    );
  });
});

describe("the fonts' notices (ADR-0020, S110)", () => {
  const notices = readFileSync(join(ROOT, "THIRD-PARTY-NOTICES.md"), "utf8");

  for (const licence of [
    "packages/ui/node_modules/@fontsource-variable/geist/LICENSE",
    "packages/ui/node_modules/@fontsource/ibm-plex-mono/LICENSE",
  ]) {
    it(`hold ${licence} verbatim`, () => {
      assert.ok(notices.includes(readFileSync(join(ROOT, licence), "utf8").trim()));
    });
  }
});

describe("check", () => {
  it("passes clean shipped bundles and an e2e bundle with its marker", () => {
    withBundles({ desktop: CLEAN, ui: CLEAN, e2e: E2E }, (at) => {
      assert.deepEqual(check({ shipped: [at("desktop"), at("ui")], e2e: at("e2e") }), []);
    });
  });

  it("names a shipped file that holds the marker or fetchInvoke's path", () => {
    withBundles(
      {
        desktop: { ...FONTS, "assets/app.js": `x="${MARKER}"` },
        ui: { ...FONTS, "assets/web.js": 'fetch("/e2e/ipc/me")' },
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
        desktop: { ...FONTS, "assets/app.js": 'window["__afkfleetE2ePush"]=p' },
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
    withBundles({ desktop: CLEAN, e2e: without(MARKER) }, (at) => {
      assert.deepEqual(check({ shipped: [at("desktop")], e2e: at("e2e") }), [
        `${at("e2e")} doesn't hold "${MARKER}", so the check proves nothing`,
      ]);
    });
  });

  it("fails an e2e bundle without fetchInvoke's path: the search would prove nothing", () => {
    withBundles({ desktop: CLEAN, e2e: without("/e2e/ipc/") }, (at) => {
      assert.deepEqual(check({ shipped: [at("desktop")], e2e: at("e2e") }), [
        `${at("e2e")} doesn't hold "/e2e/ipc/", so the check proves nothing`,
      ]);
    });
  });

  it("fails an e2e bundle without the live events hook: the search would prove nothing", () => {
    withBundles({ desktop: CLEAN, e2e: without(PUSH_HOOK) }, (at) => {
      assert.deepEqual(check({ shipped: [at("desktop")], e2e: at("e2e") }), [
        `${at("e2e")} doesn't hold "${PUSH_HOOK}", so the check proves nothing`,
      ]);
    });
  });

  it("looks for exactly the marker and the hook the e2e build defines", () => {
    assert.equal(exported("apps/desktop/e2e/app/marker.ts", "E2E_MARKER"), MARKER);
    assert.equal(exported("apps/desktop/e2e/app/push.ts", "PUSH_HOOK"), PUSH_HOOK);
    assert.deepEqual(NEEDLES, [MARKER, "/e2e/ipc/", PUSH_HOOK]);
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
