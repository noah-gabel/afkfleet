// Tests for coverage-gates.mjs. Run with `just scripts-test`.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { evaluateGates } from "./coverage-gates.mjs";

const ROOT = "/repo";

/** Builds a minimal `cargo llvm-cov --json` export from [path, covered, count] triples. */
function report(files) {
  return {
    data: [
      {
        files: files.map(([filename, covered, count]) => ({
          filename,
          summary: { lines: { covered, count } },
        })),
      },
    ],
  };
}

const GATES = [
  { name: "workspace", min: 80, pattern: /^/, required: true },
  { name: "fleet-core", min: 90, pattern: /^crates\/fleet-core\// },
  { name: "fleet-runtime", min: 85, pattern: /^crates\/fleet-runtime\// },
];

function byName(results) {
  return Object.fromEntries(results.map((r) => [r.name, r]));
}

describe("evaluateGates", () => {
  it("passes every gate when coverage is above each minimum", () => {
    const results = evaluateGates(
      report([
        ["/repo/crates/fleet-core/src/lib.rs", 95, 100],
        ["/repo/crates/fleet-runtime/src/lib.rs", 90, 100],
      ]),
      ROOT,
      GATES,
    );

    assert.deepEqual(
      results.map((r) => [r.name, r.status]),
      [
        ["workspace", "pass"],
        ["fleet-core", "pass"],
        ["fleet-runtime", "pass"],
      ],
    );
  });

  it("fails a crate gate that is below its minimum", () => {
    const results = byName(
      evaluateGates(report([["/repo/crates/fleet-core/src/lib.rs", 89, 100]]), ROOT, GATES),
    );

    assert.equal(results["fleet-core"].status, "fail");
    assert.equal(results["fleet-core"].percent, 89);
  });

  it("weights the workspace gate by line count, not by file", () => {
    const results = byName(
      evaluateGates(
        report([
          ["/repo/crates/fleet-core/src/a.rs", 1, 1],
          ["/repo/crates/fleet-core/src/b.rs", 70, 99],
        ]),
        ROOT,
        GATES,
      ),
    );

    assert.equal(results.workspace.percent, 71);
    assert.equal(results.workspace.status, "fail");
  });

  it("skips a gate whose files don't exist yet", () => {
    const results = byName(
      evaluateGates(report([["/repo/crates/fleet-core/src/lib.rs", 10, 10]]), ROOT, GATES),
    );

    assert.equal(results["fleet-runtime"].status, "skip");
  });

  it("matches Windows paths with backslashes and drive letters", () => {
    const results = byName(
      evaluateGates(
        report([["C:\\work\\repo\\crates\\fleet-core\\src\\lib.rs", 50, 100]]),
        "C:\\work\\repo",
        GATES,
      ),
    );

    assert.equal(results["fleet-core"].status, "fail");
  });

  it("ignores files outside the repository", () => {
    const results = byName(
      evaluateGates(
        report([
          ["/repo/crates/fleet-core/src/lib.rs", 100, 100],
          ["/home/user/.cargo/registry/src/dep/lib.rs", 0, 100],
        ]),
        ROOT,
        GATES,
      ),
    );

    assert.equal(results.workspace.percent, 100);
  });

  it("fails the workspace gate when the report has no files at all", () => {
    const results = byName(evaluateGates(report([]), ROOT, GATES));

    assert.equal(results.workspace.status, "fail");
  });

  it("rejects a report that isn't an llvm-cov JSON export", () => {
    assert.throws(() => evaluateGates({ unexpected: true }, ROOT, GATES), /llvm-cov JSON export/);
  });
});
