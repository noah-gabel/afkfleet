// Holds the per-test time limit in .config/nextest.toml. Run with `just scripts-test`.
//
// A test that never ends must fail by its name, not hold a run until a CI job's own timeout
// (ADR-0031): every profile ends a test after ten 60 s periods. The default profile's
// setting is inherited by the others, and each says it again, so dropping one line can't
// lift the limit for the profile CI runs.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

/** The limit every profile sets, as written. */
const LIMIT = 'slow-timeout = { period = "60s", terminate-after = 10 }';

/** Each `[profile.<name>]` table's own lines in `text`, by name: overrides and comments left out. */
function profiles(text) {
  const tables = new Map();
  let current = null;
  for (const line of text.split("\n").map((raw) => raw.trim())) {
    const header = /^\[+([^\]]+)\]+$/.exec(line)?.[1];
    if (header !== undefined) {
      const name = /^profile\.([a-z]+)$/.exec(header)?.[1];
      current = name === undefined ? null : name;
      if (current !== null && !tables.has(current)) {
        tables.set(current, []);
      }
    } else if (current !== null && line !== "" && !line.startsWith("#")) {
      tables.get(current).push(line);
    }
  }
  return tables;
}

describe("profiles", () => {
  it("reads each profile's own settings, apart from its overrides and other tables", () => {
    const tables = profiles(
      [
        "[profile.default]",
        "# a comment",
        'default-filter = "x"',
        "",
        "[[profile.default.overrides]]",
        'filter = "y"',
        "[test-groups]",
        "minecraft = { max-threads = 1 }",
        "[profile.ci]",
        "retries = 0",
        "[profile.ci.junit]",
        'path = "junit.xml"',
      ].join("\n"),
    );

    assert.deepEqual(
      [...tables],
      [
        ["default", ['default-filter = "x"']],
        ["ci", ["retries = 0"]],
      ],
    );
  });
});

describe(".config/nextest.toml", () => {
  const tables = profiles(readFileSync(join(root, ".config", "nextest.toml"), "utf8"));

  it("has the four profiles the recipes and CI run", () => {
    assert.deepEqual([...tables.keys()].sort(), ["ci", "default", "manual", "slow"]);
  });

  it("ends a test that never ends after ten minutes, in every profile", () => {
    for (const [name, lines] of tables) {
      const limits = lines.filter((line) => line.startsWith("slow-timeout"));

      assert.deepEqual(limits, [LIMIT], name);
    }
  });

  it("never lets a test that ran out of time pass", () => {
    for (const [name, lines] of tables) {
      assert.ok(!lines.some((line) => line.includes("on-timeout")), name);
    }
  });
});
