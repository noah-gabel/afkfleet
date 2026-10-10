// Tests for testkit-check.mjs. Run with `just scripts-test`.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { checkTree, parseTree } from "./testkit-check.mjs";

/** Only fleet-testkit itself: every dependent reaches it through dev-dependencies. */
const CLEAN = "fleet-testkit v0.1.0 (C:\\path\\to\\afkfleet\\crates\\fleet-testkit)\n";

/** fleet-server depends on fleet-testkit directly, and fleet-agent through fleet-mc. */
const DEPENDED_ON = `fleet-testkit v0.1.0 (/path/to/afkfleet/crates/fleet-testkit)
fleet-server v0.1.0 (/path/to/afkfleet/crates/fleet-server)
fleet-mc v0.1.0 (/path/to/afkfleet/crates/fleet-mc)
fleet-agent v0.1.0 (/path/to/afkfleet/crates/fleet-agent)
fleet-agent v0.1.0 (/path/to/afkfleet/crates/fleet-agent) (*)
`;

/** What `--prefix indent` would print: edge-kind headers and tree drawing. */
const WITH_HEADER = `fleet-testkit v0.1.0 (/path/to/afkfleet/crates/fleet-testkit)
[build-dependencies]
fleet-proto v0.1.0 (/path/to/afkfleet/crates/fleet-proto)
`;

describe("parseTree", () => {
  it("reads the package name of every line, with the (*) marker and sources", () => {
    const result = parseTree(DEPENDED_ON);

    assert.deepEqual(result.packages, [
      "fleet-testkit",
      "fleet-server",
      "fleet-mc",
      "fleet-agent",
      "fleet-agent",
    ]);
    assert.deepEqual(result.problems, []);
  });

  it("reads registry packages, proc macros and paths with parentheses", () => {
    const result = parseTree(
      [
        "serde_derive v1.0.229 (proc-macro)",
        "azalea v0.16.0+mc26.1",
        "fleet-testkit v0.1.0 (C:\\Program Files (x86)\\afkfleet\\crates\\fleet-testkit)",
        "some-crate v1.0.0 (https://github.com/example/some-crate#0123abcd) (*)",
      ].join("\n"),
    );

    assert.deepEqual(result.packages, ["serde_derive", "azalea", "fleet-testkit", "some-crate"]);
    assert.deepEqual(result.problems, []);
  });

  it("skips edge-kind headers and empty lines", () => {
    const result = parseTree(`${WITH_HEADER}\n[dev-dependencies]\n[dependencies]\n`);

    assert.deepEqual(result.packages, ["fleet-testkit", "fleet-proto"]);
    assert.deepEqual(result.problems, []);
  });

  it("reads Windows line endings", () => {
    const result = parseTree(CLEAN.replace("\n", "\r\n"));

    assert.deepEqual(result.packages, ["fleet-testkit"]);
    assert.deepEqual(result.problems, []);
  });

  it("reports every line it can't read, with its number", () => {
    const result = parseTree(
      `${CLEAN}└── fleet-server v0.1.0 (/x)\nwarning: nothing to print\nfleet-x 1.0.0\n`,
    );

    assert.deepEqual(result.packages, ["fleet-testkit"]);
    assert.deepEqual(result.problems, [
      "line 2 of cargo tree's output can't be read: `└── fleet-server v0.1.0 (/x)`",
      "line 3 of cargo tree's output can't be read: `warning: nothing to print`",
      "line 4 of cargo tree's output can't be read: `fleet-x 1.0.0`",
    ]);
  });
});

describe("checkTree", () => {
  it("passes when only fleet-testkit itself is listed", () => {
    assert.deepEqual(checkTree(CLEAN), []);
  });

  it("reports each package that depends on fleet-testkit once", () => {
    assert.deepEqual(checkTree(DEPENDED_ON), [
      "fleet-server depends on fleet-testkit through a normal or build dependency",
      "fleet-mc depends on fleet-testkit through a normal or build dependency",
      "fleet-agent depends on fleet-testkit through a normal or build dependency",
    ]);
  });

  it("reports a build dependency after a header", () => {
    assert.deepEqual(checkTree(WITH_HEADER), [
      "fleet-proto depends on fleet-testkit through a normal or build dependency",
    ]);
  });

  it("fails on output it can't read", () => {
    assert.deepEqual(checkTree(`${CLEAN}garbage\n`), [
      "line 2 of cargo tree's output can't be read: `garbage`",
    ]);
  });

  it("fails when cargo didn't list fleet-testkit at all", () => {
    assert.deepEqual(checkTree(""), [
      "cargo tree didn't list fleet-testkit, so the check proves nothing",
    ]);
  });
});
