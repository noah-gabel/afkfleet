// Gives `just app-build-check`'s debug exe a path of its own (ADR-0020, U87, S99).
//
//   node scripts/app-check.mjs
//
// Every debug build writes the same target/debug/afkfleet-desktop.exe: `just app-build-check`,
// `just app-build --debug` and nextest's build of the desktop package, and only the first
// carries the check identifier. So the recipe's second line copies that build here, to the one
// file the AI launches, and nothing else writes it. A real identifier in that file would reach
// the user's running app, its single-instance lock, its profile and its Credential Manager
// entries, so the bytes are checked before the copy gets its name. The older copy goes first:
// any failure leaves nothing to launch. Exits with 1, saying why.
// Tests: scripts/app-check.test.mjs.
import { copyFileSync, existsSync, mkdirSync, readFileSync, renameSync, rmSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

/** The one file the AI launches, relative to the repository, with `/`. */
export const CHECK_EXE = "target/app-check/afkfleet-check.exe";

/** What every debug build writes, relative to the repository, with `/`. */
const DEBUG_EXE = "target/debug/afkfleet-desktop.exe";

/** How many times `needle` is in `bytes`. */
function count(bytes, needle) {
  let found = 0;
  for (let at = bytes.indexOf(needle); at !== -1; at = bytes.indexOf(needle, at + needle.length)) {
    found += 1;
  }
  return found;
}

/**
 * What's wrong with an exe holding `bytes`, or null. The real identifier is a prefix of the check
 * one, so each of its appearances must be the start of a check one: anything more means the build
 * also holds the real one, and a plain search couldn't tell. That rests on the prefix, so it's
 * required: between unrelated identifiers, one of each would count equal and pass.
 */
export function identifierProblem(bytes, check, real) {
  if (!check.startsWith(`${real}.`)) {
    return `the check identifier ${check} must start with the real one, ${real}, and a dot`;
  }
  const checks = count(bytes, check);
  if (checks === 0) {
    return `it doesn't hold the check identifier ${check}`;
  }
  if (count(bytes, real) !== checks) {
    return `it also holds the real identifier ${real}`;
  }
  return null;
}

/**
 * Copies the check build at `source` to `dest`; a message if it can't be, and nothing at `dest`
 * then. A part file of this run's own holds the copy while it's checked, so `dest` only ever holds
 * a checked exe and two runs can't take each other's.
 */
export function adoptCheckExe({ source, dest, check, real }) {
  const part = `${dest}.${process.pid}.part`;
  mkdirSync(path.dirname(dest), { recursive: true });
  rmSync(dest, { force: true });
  if (!existsSync(source)) {
    return `there's no build at ${source}`;
  }
  try {
    copyFileSync(source, part);
    const problem = identifierProblem(readFileSync(part), check, real);
    if (problem !== null) {
      return `refusing ${source}, wrong identifier: ${problem}`;
    }
    renameSync(part, dest);
    return null;
  } finally {
    rmSync(part, { force: true });
  }
}

function main() {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  const srcTauri = path.join(root, "apps", "desktop", "src-tauri");
  const identifierIn = (name) =>
    JSON.parse(readFileSync(path.join(srcTauri, name), "utf8")).identifier;
  const problem = adoptCheckExe({
    source: path.join(root, DEBUG_EXE),
    dest: path.join(root, CHECK_EXE),
    check: identifierIn("tauri.check.conf.json"),
    real: identifierIn("tauri.conf.json"),
  });
  if (problem !== null) {
    console.error(problem);
    return 1;
  }
  console.log(`wrote ${CHECK_EXE}`);
  return 0;
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = main();
}
