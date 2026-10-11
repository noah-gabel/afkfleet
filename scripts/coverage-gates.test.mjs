// Tests for coverage-gates.mjs. Run with `just scripts-test`.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { evaluateGates, GATES as GATES_IN_USE } from "./coverage-gates.mjs";

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

/** A gate over path prefixes, like the auth and vault gates (ADR-0016). */
const PREFIX_GATES = [
  {
    name: "auth",
    min: 90,
    required: true,
    prefixes: ["crates/fleet-core/src/auth/", "crates/fleet-server/src/app/users.rs"],
  },
];

describe("evaluateGates with prefixes", () => {
  it("sums a folder's files and a single file", () => {
    const results = byName(
      evaluateGates(
        report([
          ["/repo/crates/fleet-core/src/auth/mod.rs", 10, 10],
          ["/repo/crates/fleet-core/src/auth/deep/password.rs", 80, 90],
          ["/repo/crates/fleet-server/src/app/users.rs", 95, 100],
        ]),
        ROOT,
        PREFIX_GATES,
      ),
    );

    assert.equal(results.auth.status, "pass");
    assert.equal(results.auth.covered, 185);
    assert.equal(results.auth.count, 200);
    assert.deepEqual(results.auth.missing, []);
  });

  it("matches a file entry exactly and a folder entry only below it", () => {
    const results = byName(
      evaluateGates(
        report([
          ["/repo/crates/fleet-core/src/auth.rs", 0, 100],
          ["/repo/crates/fleet-core/src/authz/mod.rs", 0, 100],
          ["/repo/crates/fleet-server/src/app/users.rs.orig", 0, 100],
          ["/repo/crates/fleet-server/src/app/users_old.rs", 0, 100],
          ["/repo/crates/fleet-core/src/auth/mod.rs", 9, 10],
          ["/repo/crates/fleet-server/src/app/users.rs", 10, 10],
        ]),
        ROOT,
        PREFIX_GATES,
      ),
    );

    assert.equal(results.auth.count, 20);
    assert.equal(results.auth.status, "pass");
  });

  it("fails when a listed prefix matches no file, naming it", () => {
    const results = byName(
      evaluateGates(
        report([["/repo/crates/fleet-core/src/auth/mod.rs", 10, 10]]),
        ROOT,
        PREFIX_GATES,
      ),
    );

    assert.equal(results.auth.status, "fail");
    assert.deepEqual(results.auth.missing, ["crates/fleet-server/src/app/users.rs"]);
  });

  it("fails a required prefix gate that matches no file at all", () => {
    const results = byName(
      evaluateGates(report([["/repo/crates/fleet-core/src/lib.rs", 10, 10]]), ROOT, PREFIX_GATES),
    );

    assert.equal(results.auth.status, "fail");
    assert.equal(results.auth.percent, null);
    assert.deepEqual(results.auth.missing, PREFIX_GATES[0].prefixes);
  });

  it("fails a prefix gate below its minimum even when every prefix matches", () => {
    const results = byName(
      evaluateGates(
        report([
          ["/repo/crates/fleet-core/src/auth/mod.rs", 50, 100],
          ["/repo/crates/fleet-server/src/app/users.rs", 50, 100],
        ]),
        ROOT,
        PREFIX_GATES,
      ),
    );

    assert.equal(results.auth.status, "fail");
    assert.deepEqual(results.auth.missing, []);
  });
});

describe("the project's gates", () => {
  it("make the auth and vault gates required lists of prefixes", () => {
    for (const name of ["auth", "vault"]) {
      const gate = GATES_IN_USE.find((g) => g.name === name);
      assert.equal(gate.required, true, name);
      assert.equal(gate.min, 90, name);
      assert.ok(Array.isArray(gate.prefixes) && gate.prefixes.length > 0, name);
    }
  });

  it("count fleet-core's auth module under the auth gate", () => {
    const files = GATES_IN_USE.flatMap((gate) => gate.prefixes ?? []).map((prefix) => [
      `/repo/${prefix.endsWith("/") ? `${prefix}mod.rs` : prefix}`,
      100,
      100,
    ]);
    files.push(["/repo/crates/fleet-core/src/auth/password.rs", 1, 100]);

    const results = byName(evaluateGates(report(files), ROOT, GATES_IN_USE));

    assert.ok(results.auth.count > results.auth.covered, "fleet-core's auth file counts");
    assert.deepEqual(results.vault.missing, []);
  });
});
