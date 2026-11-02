// The three Vite configs never inline an asset (Plan.md P11.11; ADR-0020, S111). The smallest font
// face is 5,620 B, over Vite's default limit, so nothing else fails if a config loses this until a
// smaller face goes in as `data:`, which `default-src 'self'` refuses. It's read through the guard's
// lexer, so a comment can't satisfy it.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { lex } from "./theme-guard.mjs";

const CONFIGS = [
  "apps/desktop/vite.config.ts",
  "apps/desktop/vite.e2e.config.ts",
  "packages/ui/vite.config.ts",
];

/** Whether the config `source` sets `assetsInlineLimit: 0` inside its `build`. */
function pinsNoInlining(source) {
  const { code } = lex(source);
  const build = /\bbuild\s*:\s*\{/.exec(code);
  if (build === null) {
    return false;
  }
  const from = build.index + build[0].length;
  let depth = 1;
  let at = from;
  while (at < code.length && depth > 0) {
    depth += code[at] === "{" ? 1 : code[at] === "}" ? -1 : 0;
    at += 1;
  }
  return /\bassetsInlineLimit\s*:\s*0(?![\d.])/.test(code.slice(from, at));
}

describe("the Vite configs", () => {
  for (const config of CONFIGS) {
    it(`${config} inlines nothing`, () => {
      const path = fileURLToPath(new URL(`../${config}`, import.meta.url));
      assert.equal(pinsNoInlining(readFileSync(path, "utf8")), true);
    });
  }
});

describe("the check", () => {
  for (const [name, source] of [
    ["no build", "export default { plugins: [] };"],
    ["no limit", 'export default { build: { outDir: "x" } };'],
    ["Vite's default back", "export default { build: { assetsInlineLimit: 4096 } };"],
    ["only a comment", 'export default { build: { /* assetsInlineLimit: 0, */ outDir: "x" } };'],
    ["the limit outside build", 'export default { assetsInlineLimit: 0, build: { outDir: "x" } };'],
  ]) {
    it(`refuses ${name}`, () => {
      assert.equal(pinsNoInlining(source), false);
    });
  }

  it("takes the limit in build", () => {
    assert.equal(pinsNoInlining("export default { build: { assetsInlineLimit: 0 } };"), true);
  });
});
