// Tests for icons.mjs. Run with `just scripts-test`.
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { describe, it } from "node:test";

import { adoptIcon, cliArgs } from "./icons.mjs";

/** A fresh folder that the test removes again. */
function tempDir(t, prefix) {
  const dir = mkdtempSync(path.join(tmpdir(), prefix));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

describe("cliArgs", () => {
  it("runs the CLI's icon command on the source, into the output folder", () => {
    assert.deepEqual(cliArgs("/cli/tauri.js", "/icons/icon.svg", "/target/icons-1"), [
      "/cli/tauri.js",
      "icon",
      "/icons/icon.svg",
      "--output",
      "/target/icons-1",
    ]);
  });
});

describe("adoptIcon", () => {
  it("copies only icon.ico from what the CLI wrote", (t) => {
    const out = tempDir(t, "icons-out-");
    const icons = tempDir(t, "icons-dest-");
    writeFileSync(path.join(out, "icon.ico"), "ico bytes");
    writeFileSync(path.join(out, "icon.png"), "png");
    writeFileSync(path.join(out, "32x32.png"), "png");
    writeFileSync(path.join(out, "icon.icns"), "icns");
    mkdirSync(path.join(out, "ios"));
    writeFileSync(path.join(out, "ios", "AppIcon-20x20@1x.png"), "png");
    writeFileSync(path.join(icons, "icon.svg"), "<svg/>");

    const problem = adoptIcon(out, icons);

    assert.equal(problem, null);
    assert.deepEqual(readdirSync(icons).sort(), ["icon.ico", "icon.svg"]);
    assert.equal(readFileSync(path.join(icons, "icon.ico"), "utf8"), "ico bytes");
  });

  it("replaces an existing icon.ico", (t) => {
    const out = tempDir(t, "icons-out-");
    const icons = tempDir(t, "icons-dest-");
    writeFileSync(path.join(out, "icon.ico"), "new");
    writeFileSync(path.join(icons, "icon.ico"), "old");

    assert.equal(adoptIcon(out, icons), null);
    assert.equal(readFileSync(path.join(icons, "icon.ico"), "utf8"), "new");
  });

  it("reports a missing icon.ico and leaves the icons folder alone", (t) => {
    const out = tempDir(t, "icons-out-");
    const icons = tempDir(t, "icons-dest-");
    writeFileSync(path.join(out, "icon.png"), "png");
    writeFileSync(path.join(icons, "icon.ico"), "old");

    const problem = adoptIcon(out, icons);

    assert.match(problem ?? "", /wrote no icon\.ico/);
    assert.equal(readFileSync(path.join(icons, "icon.ico"), "utf8"), "old");
  });
});
