// Checks the coverage gates from Plan.md §8 against a `cargo llvm-cov --json` export.
//
//   node scripts/coverage-gates.mjs <coverage.json>
//
// Every gate is measured in lines, summed over all files its pattern matches.
// A gate whose files don't exist yet is skipped, unless it's `required`.
// Exits with 1 if any gate fails. Tests: scripts/coverage-gates.test.mjs.
import { readFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

/**
 * The gates from Plan.md §8. Patterns match repository-relative paths with `/`.
 * The auth and vault paths follow the server layout in CLAUDE.md; P7 confirms them.
 */
export const GATES = [
  { name: "workspace", min: 80, pattern: /^/, required: true },
  { name: "fleet-core", min: 90, pattern: /^crates\/fleet-core\// },
  { name: "fleet-runtime", min: 85, pattern: /^crates\/fleet-runtime\// },
  {
    name: "auth",
    min: 90,
    pattern: /^crates\/fleet-server\/src\/(app\/auth|infra\/crypto\/(passwords|tokens))(\.rs|\/)/,
  },
  { name: "vault", min: 90, pattern: /^crates\/fleet-server\/src\/infra\/crypto\/vault(\.rs|\/)/ },
];

/**
 * Evaluates `gates` against an llvm-cov JSON export.
 * Returns one `{ name, min, status: "pass" | "fail" | "skip", percent, covered, count }` per gate.
 * Throws if `report` isn't an llvm-cov JSON export.
 */
export function evaluateGates(report, repoRoot, gates) {
  const files = report?.data?.[0]?.files;
  if (!Array.isArray(files)) {
    throw new Error("not an llvm-cov JSON export: `data[0].files` is missing");
  }

  const lines = files.flatMap((file) => {
    const relative = relativeToRoot(file.filename, repoRoot);
    return relative === null
      ? []
      : [{ path: relative, covered: file.summary.lines.covered, count: file.summary.lines.count }];
  });

  return gates.map(({ name, min, pattern, required = false }) => {
    const matched = lines.filter((line) => pattern.test(line.path));
    if (matched.length === 0) {
      return { name, min, status: required ? "fail" : "skip", percent: null, covered: 0, count: 0 };
    }
    const covered = matched.reduce((sum, line) => sum + line.covered, 0);
    const count = matched.reduce((sum, line) => sum + line.count, 0);
    const percent = count === 0 ? 100 : (covered * 100) / count;
    return { name, min, status: percent >= min ? "pass" : "fail", percent, covered, count };
  });
}

/** Returns `filename` relative to `repoRoot` with `/` separators, or null if it's outside. */
function relativeToRoot(filename, repoRoot) {
  const file = filename.replaceAll("\\", "/");
  const root = `${repoRoot.replaceAll("\\", "/").replace(/\/+$/, "")}/`;
  // Windows paths are case-insensitive, and drive letters come in either case.
  const inside = /^[A-Za-z]:\//.test(root)
    ? file.toLowerCase().startsWith(root.toLowerCase())
    : file.startsWith(root);
  return inside ? file.slice(root.length) : null;
}

function describe(result) {
  const label = `${result.status.toUpperCase().padEnd(4)}  ${result.name.padEnd(14)}`;
  if (result.percent === null) {
    return `${label}no files yet (min ${result.min} %)`;
  }
  return `${label}${result.percent.toFixed(2).padStart(6)} %  (min ${result.min} %, ${result.covered}/${result.count} lines)`;
}

function main(args) {
  const [reportPath] = args;
  if (reportPath === undefined) {
    console.error("usage: node scripts/coverage-gates.mjs <coverage.json>");
    return 2;
  }
  const report = JSON.parse(readFileSync(reportPath, "utf8"));
  const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  const results = evaluateGates(report, repoRoot, GATES);
  for (const result of results) {
    console.log(describe(result));
  }
  const failed = results.filter((result) => result.status === "fail");
  if (failed.length > 0) {
    console.error(`coverage gates failed: ${failed.map((result) => result.name).join(", ")}`);
    return 1;
  }
  return 0;
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = main(process.argv.slice(2));
}
