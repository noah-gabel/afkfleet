// Tests for clippy-config.mjs. Run with `just scripts-test`.
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import {
  checkRepo,
  findCrateConfigs,
  missingRootSettings,
  parseClippyConfig,
} from "./clippy-config.mjs";

const ROOT_CONFIG = `# Root settings.
allow-unwrap-in-tests = true
allow-panic-in-tests = true

disallowed-methods = [
    { path = "tokio::sync::mpsc::unbounded_channel", reason = "bounded # always", allow-invalid = true },
]
`;

/** A crate config that repeats the root settings and adds its own. */
const FULL_CRATE_CONFIG = `# A crate's own config.
allow-unwrap-in-tests = true
allow-panic-in-tests = true  # repeated from the root

disallowed-methods = [
    { path = "std::time::Instant::now", reason = "time is a parameter" },
    { path = "tokio::sync::mpsc::unbounded_channel", reason = "bounded", allow-invalid = true },
]
`;

describe("parseClippyConfig", () => {
  it("reads settings and ignores comments", () => {
    const config = parseClippyConfig("# comment\nallow-unwrap-in-tests = true # trailing\n");

    assert.deepEqual([...config.scalars], [["allow-unwrap-in-tests", "true"]]);
  });

  it("reads the paths of a multi-line list, with `#` and `]` inside strings", () => {
    const config = parseClippyConfig(
      'disallowed-methods = [\n  { path = "a::b", reason = "x # ] y" },\n  { path = "c::d" },\n]\n',
    );

    assert.deepEqual([...(config.lists.get("disallowed-methods") ?? [])], ["a::b", "c::d"]);
  });

  it("reads a list of plain strings", () => {
    const config = parseClippyConfig('doc-valid-idents = ["GitHub", "azalea"]\n');

    assert.deepEqual([...(config.lists.get("doc-valid-idents") ?? [])], ["GitHub", "azalea"]);
  });

  it("rejects a table header, which clippy.toml doesn't use", () => {
    assert.throws(() => parseClippyConfig("[section]\nkey = 1\n"), /unsupported/);
  });

  it("rejects a list that is never closed", () => {
    assert.throws(
      () => parseClippyConfig('disallowed-methods = [\n  { path = "a" },\n'),
      /unclosed/,
    );
  });
});

describe("missingRootSettings", () => {
  it("returns nothing when the crate config repeats every root setting", () => {
    assert.deepEqual(missingRootSettings(ROOT_CONFIG, FULL_CRATE_CONFIG), []);
  });

  it("reports a missing setting", () => {
    const crate = FULL_CRATE_CONFIG.replace(
      "allow-panic-in-tests = true  # repeated from the root",
      "",
    );

    assert.deepEqual(missingRootSettings(ROOT_CONFIG, crate), [
      "`allow-panic-in-tests = true` is missing",
    ]);
  });

  it("reports a setting with a different value", () => {
    const crate = FULL_CRATE_CONFIG.replace(
      "allow-unwrap-in-tests = true",
      "allow-unwrap-in-tests = false",
    );

    assert.deepEqual(missingRootSettings(ROOT_CONFIG, crate), [
      "`allow-unwrap-in-tests` is `false`, but the root clippy.toml sets `true`",
    ]);
  });

  it("reports a missing list", () => {
    const crate = "allow-unwrap-in-tests = true\nallow-panic-in-tests = true\n";

    assert.deepEqual(missingRootSettings(ROOT_CONFIG, crate), [
      "`disallowed-methods` is missing `tokio::sync::mpsc::unbounded_channel`",
    ]);
  });

  it("reports a list that lacks a root entry", () => {
    const crate = FULL_CRATE_CONFIG.replace(
      '    { path = "tokio::sync::mpsc::unbounded_channel", reason = "bounded", allow-invalid = true },\n',
      "",
    );

    assert.deepEqual(missingRootSettings(ROOT_CONFIG, crate), [
      "`disallowed-methods` is missing `tokio::sync::mpsc::unbounded_channel`",
    ]);
  });
});

describe("findCrateConfigs", () => {
  it("finds every crate-local config, but not the root one or ignored folders", (t) => {
    const repo = mkdtempSync(path.join(tmpdir(), "clippy-config-"));
    t.after(() => rmSync(repo, { recursive: true, force: true }));
    const files = [
      "clippy.toml",
      "crates/a/clippy.toml",
      "crates/b/.clippy.toml",
      "apps/desktop/src-tauri/clippy.toml",
      "target/debug/clippy.toml",
      "node_modules/pkg/clippy.toml",
      "spikes/old/clippy.toml",
      ".git/clippy.toml",
      "crates/c/Cargo.toml",
    ];
    for (const file of files) {
      mkdirSync(path.join(repo, path.dirname(file)), { recursive: true });
      writeFileSync(path.join(repo, file), "");
    }

    assert.deepEqual(findCrateConfigs(repo), [
      "apps/desktop/src-tauri/clippy.toml",
      "crates/a/clippy.toml",
      "crates/b/.clippy.toml",
    ]);
  });
});

describe("the repository", () => {
  it("has every root clippy setting in every crate-local clippy.toml", () => {
    const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

    assert.deepEqual(checkRepo(repoRoot), []);
  });
});
