// Checks that no workspace member depends on fleet-testkit outside its dev-dependencies.
//
//   node scripts/testkit-check.mjs
//
// fleet-testkit's SeededRandom isn't secure at all, so the crate must never be linked
// into a binary: every member may list it only as a dev-dependency (ADR-0015). Cargo
// answers that itself: `cargo tree -i fleet-testkit -e normal,build --target all` lists
// every package that reaches fleet-testkit through a normal or build dependency, on any
// target, so `workspace = true`, renames, target-specific tables and members anywhere in
// the workspace are all covered without parsing a manifest here. Exits with 1 if any
// package besides fleet-testkit itself shows up, or if a line of cargo's output can't be
// read. Run through `just testkit-check`, which needs Rust; the frontend CI job that runs
// `just scripts-test` has none, so the tests cover only the parsing.
// Tests: scripts/testkit-check.test.mjs.
import { spawnSync } from "node:child_process";
import process from "node:process";
import { pathToFileURL } from "node:url";

/** The crate that must stay a dev-dependency. */
export const TESTKIT = "fleet-testkit";

/** The `cargo tree` arguments: fleet-testkit's dependents through normal and build edges. */
export const CARGO_TREE_ARGS = [
  "tree",
  "--workspace",
  "--edges",
  "normal,build",
  "--target",
  "all",
  "--invert",
  TESTKIT,
  "--prefix",
  "none",
  "--format",
  "{p}",
];

/** A package line: name, version and optional parenthesized suffixes (source, proc-macro, (*)). */
const PACKAGE_LINE = /^([A-Za-z0-9_-]+) v\d\S*(?: \(.+\))?$/;

/** An edge-kind header, which `--prefix indent` prints above build and dev dependents. */
const HEADER_LINE = /^\[(?:build-|dev-)?dependencies\]$/;

/**
 * Reads `cargo tree` output. Returns `{ packages, problems }`: `packages` holds the
 * package name of every line, in order, and `problems` one message per line that is
 * neither a package nor an edge-kind header like `[build-dependencies]`.
 */
export function parseTree(output) {
  const packages = [];
  const problems = [];
  output.split("\n").forEach((rawLine, index) => {
    const line = rawLine.endsWith("\r") ? rawLine.slice(0, -1) : rawLine;
    if (line.trim() === "" || HEADER_LINE.test(line)) {
      return;
    }
    const match = PACKAGE_LINE.exec(line);
    if (match === null) {
      problems.push(`line ${index + 1} of cargo tree's output can't be read: \`${line}\``);
    } else {
      packages.push(match[1]);
    }
  });
  return { packages, problems };
}

/**
 * Checks `cargo tree` output. Returns one message per problem: an unreadable line, a
 * package that depends on fleet-testkit, or output that doesn't name fleet-testkit at
 * all (cargo matched nothing, so the check would prove nothing).
 */
export function checkTree(output) {
  const { packages, problems } = parseTree(output);
  if (!packages.includes(TESTKIT)) {
    problems.push(`cargo tree didn't list ${TESTKIT}, so the check proves nothing`);
  }
  const dependents = [...new Set(packages.filter((name) => name !== TESTKIT))];
  return [
    ...problems,
    ...dependents.map(
      (name) => `${name} depends on ${TESTKIT} through a normal or build dependency`,
    ),
  ];
}

function main() {
  const result = spawnSync("cargo", CARGO_TREE_ARGS, { encoding: "utf8" });
  if (result.error !== undefined || result.status !== 0) {
    console.error(result.stderr ?? "");
    console.error(`cargo tree failed: ${result.error?.message ?? `exit code ${result.status}`}`);
    return 1;
  }
  const problems = checkTree(result.stdout);
  for (const problem of problems) {
    console.error(problem);
  }
  if (problems.length > 0) {
    console.error(`${TESTKIT} may only be a dev-dependency (ADR-0015)`);
    return 1;
  }
  console.log(`no workspace member depends on ${TESTKIT} outside its dev-dependencies`);
  return 0;
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = main();
}
