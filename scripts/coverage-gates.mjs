// Checks the coverage gates from Plan.md §8 against a `cargo llvm-cov --json` export.
//
//   node scripts/coverage-gates.mjs <coverage.json>
//
// Every gate is measured in lines, summed over all files its pattern or its prefixes
// match. A gate whose files don't exist yet is skipped, unless it's `required`; a gate
// with prefixes fails when any one of them matches no file. Exits with 1 if any gate
// fails. Tests: scripts/coverage-gates.test.mjs.
import { readFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

/**
 * The gates from Plan.md §8, over repository-relative paths with `/`.
 *
 * - A `pattern` gate sums every file its regex matches.
 * - A `prefixes` gate lists its modules (ADR-0016): an entry ending in `/` is a folder,
 *   any other entry exactly one file. A listed entry that matches no file in the report
 *   fails the gate, so a typo or a rename can't drop a module to the workspace gate. Each
 *   Phase 7 group adds the modules it creates; a file without executable lines (a plain
 *   `mod` list) never appears in the report, so it isn't listed. ADR-0016 confirms these
 *   paths for the auth and vault gates, required from Phase 7's group A on.
 */
export const GATES = [
  { name: "workspace", min: 80, pattern: /^/, required: true },
  { name: "fleet-core", min: 90, pattern: /^crates\/fleet-core\// },
  { name: "fleet-runtime", min: 85, pattern: /^crates\/fleet-runtime\// },
  {
    name: "auth",
    min: 90,
    required: true,
    prefixes: [
      "crates/fleet-core/src/auth/",
      "crates/fleet-server/src/app/auth.rs",
      "crates/fleet-server/src/app/users.rs",
      "crates/fleet-server/src/infra/crypto/passwords.rs",
      "crates/fleet-server/src/http/extractors/",
      "crates/fleet-server/src/infra/crypto/tokens.rs",
      "crates/fleet-server/src/ports/tokens.rs",
      "crates/fleet-server/src/http/routes.rs",
      "crates/fleet-server/src/http/rate_limit/",
      "crates/fleet-server/src/app/lockout.rs",
      "crates/fleet-server/src/infra/crypto/attempt_keys.rs",
      "crates/fleet-server/src/ports/login_attempts.rs",
      "crates/fleet-server/src/app/challenges.rs",
      "crates/fleet-server/src/app/mfa.rs",
      "crates/fleet-server/src/app/login.rs",
      "crates/fleet-server/src/app/invites.rs",
      "crates/fleet-server/src/infra/crypto/totp.rs",
      "crates/fleet-server/src/infra/crypto/recovery_codes.rs",
      "crates/fleet-server/src/ports/totp.rs",
      "crates/fleet-server/src/ports/recovery_codes.rs",
      "crates/fleet-api-types/src/secret.rs",
    ],
  },
  {
    name: "vault",
    min: 90,
    required: true,
    prefixes: [
      "crates/fleet-server/src/infra/crypto/vault.rs",
      "crates/fleet-server/src/ports/vault.rs",
      "crates/fleet-startup/src/secret_file.rs",
    ],
  },
];

/** Whether `path` falls under the prefix `entry`: below it if it's a folder, equal otherwise. */
function underPrefix(path, entry) {
  return entry.endsWith("/") ? path.startsWith(entry) : path === entry;
}

/**
 * Evaluates `gates` against an llvm-cov JSON export.
 * Returns one `{ name, min, status: "pass" | "fail" | "skip", percent, covered, count }` per
 * gate, plus `missing`, the prefixes that matched no file, for a gate with prefixes.
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

  return gates.map(({ name, min, pattern, prefixes, required = false }) => {
    const matches = (path) =>
      prefixes === undefined
        ? pattern.test(path)
        : prefixes.some((entry) => underPrefix(path, entry));
    const missing =
      prefixes === undefined
        ? undefined
        : prefixes.filter((entry) => !lines.some((line) => underPrefix(line.path, entry)));
    const extra = missing === undefined ? {} : { missing };
    const matched = lines.filter((line) => matches(line.path));
    if (matched.length === 0) {
      const status = required ? "fail" : "skip";
      return { name, min, status, percent: null, covered: 0, count: 0, ...extra };
    }
    const covered = matched.reduce((sum, line) => sum + line.covered, 0);
    const count = matched.reduce((sum, line) => sum + line.count, 0);
    const percent = count === 0 ? 100 : (covered * 100) / count;
    const complete = missing === undefined || missing.length === 0;
    const status = complete && percent >= min ? "pass" : "fail";
    return { name, min, status, percent, covered, count, ...extra };
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
  const missing =
    result.missing === undefined || result.missing.length === 0
      ? ""
      : `; no file under ${result.missing.join(", ")}`;
  if (result.percent === null) {
    return `${label}no files yet (min ${result.min} %)${missing}`;
  }
  return `${label}${result.percent.toFixed(2).padStart(6)} %  (min ${result.min} %, ${result.covered}/${result.count} lines)${missing}`;
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
