// Tests for coverage-gates.mjs. Run with `just scripts-test`.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { evaluateGates, GATES as GATES_IN_USE, gatesFor, parseArgs } from "./coverage-gates.mjs";

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

  it("matches a Windows path with the verbatim \\\\?\\ prefix", () => {
    const results = byName(
      evaluateGates(
        report([["\\\\?\\C:\\path\\to\\afkfleet\\crates\\fleet-core\\src\\lib.rs", 50, 100]]),
        "C:\\path\\to\\afkfleet",
        GATES,
      ),
    );

    assert.equal(results["fleet-core"].status, "fail");
    assert.equal(results["fleet-core"].count, 100);
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
  it("make the auth, vault and fleet-client gates required lists of prefixes", () => {
    for (const name of ["auth", "vault", "fleet-client"]) {
      const gate = GATES_IN_USE.find((g) => g.name === name);
      assert.ok(gate, name);
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

/** A desktop crate file, as llvm-cov on Windows names it (placeholder repository path). */
function desktopFile(relative) {
  return `C:\\path\\to\\afkfleet\\apps\\desktop\\src-tauri\\src\\${relative.replaceAll("/", "\\")}`;
}

describe("reports", () => {
  it("selects only the named report's gates", () => {
    const desktop = gatesFor("desktop").map((gate) => gate.name);
    const workspace = gatesFor("workspace").map((gate) => gate.name);

    assert.deepEqual(desktop, ["desktop"]);
    assert.ok(!workspace.includes("desktop"));
    assert.ok(["workspace", "auth", "vault", "fleet-client"].every((n) => workspace.includes(n)));
  });

  it("passes the desktop gate on a Windows report of the desktop crate alone", () => {
    const desktopGate = GATES_IN_USE.find((gate) => gate.name === "desktop");
    const files = desktopGate.prefixes.map((prefix) => [
      desktopFile(prefix.replace("apps/desktop/src-tauri/src/", "").replace(/\/$/, "/mod.rs")),
      95,
      100,
    ]);
    files.push([desktopFile("lib.rs"), 0, 50], [desktopFile("main.rs"), 0, 5]);

    const results = evaluateGates(report(files), "C:\\path\\to\\afkfleet", gatesFor("desktop"));

    assert.deepEqual(
      results.map((r) => [r.name, r.status, r.missing]),
      [["desktop", "pass", []]],
    );
  });

  it("fails the desktop gate below 90 % or with a module missing", () => {
    const low = report([[desktopFile("state.rs"), 80, 100]]);

    const results = evaluateGates(low, "C:\\path\\to\\afkfleet", gatesFor("desktop"));

    assert.equal(results[0].status, "fail");
    assert.ok(results[0].missing.length > 0);
  });
});

describe("the desktop gate", () => {
  it("is required at 90 % over the desktop crate's modules, never lib.rs or main.rs", () => {
    const gate = GATES_IN_USE.find((g) => g.name === "desktop");

    assert.ok(gate);
    assert.equal(gate.report, "desktop");
    assert.equal(gate.required, true);
    assert.equal(gate.min, 90);
    assert.ok(gate.prefixes.every((prefix) => prefix.startsWith("apps/desktop/src-tauri/src/")));
    for (const module of [
      "commands/",
      "endpoints.rs",
      "error.rs",
      "keychain.rs",
      "profile.rs",
      "state.rs",
    ]) {
      assert.ok(gate.prefixes.includes(`apps/desktop/src-tauri/src/${module}`), module);
    }
    for (const entry of ["lib.rs", "main.rs"]) {
      assert.ok(!gate.prefixes.includes(`apps/desktop/src-tauri/src/${entry}`), entry);
    }
  });
});

describe("parseArgs", () => {
  it("reads the report's path, the workspace report by default", () => {
    assert.deepEqual(parseArgs(["target/coverage.json"]), {
      report: "workspace",
      reportPath: "target/coverage.json",
    });
  });

  it("reads --report desktop", () => {
    assert.deepEqual(parseArgs(["--report", "desktop", "target/coverage-desktop.json"]), {
      report: "desktop",
      reportPath: "target/coverage-desktop.json",
    });
  });

  it("refuses an unknown report, a missing path or extra arguments", () => {
    assert.throws(() => parseArgs(["--report", "linux", "x.json"]), /usage/);
    assert.throws(() => parseArgs([]), /usage/);
    assert.throws(() => parseArgs(["--report"]), /usage/);
    assert.throws(() => parseArgs(["a.json", "b.json"]), /usage/);
  });
});
