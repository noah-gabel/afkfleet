// The dev database's copy for `just dev-rotate-key`, and the check `just dev-server-rotated`
// runs first (ADR-0018).
//
//   node scripts/dev-rotation.mjs copy <dev database> <copy>
//   node scripts/dev-rotation.mjs check <copy>
//
// `copy` refuses, before it changes anything: when the copy's folder exists (it never deletes
// or overwrites anything; delete the folder to start over), when a `-wal` file sits next to the
// dev database (a running `just dev-server` keeps its latest writes there, and a clean stop
// removes it), or when the dev database is missing. Then it creates the folder, copies the file
// without ever overwriting one, and makes it owner-only (0600 on Unix, which the server
// requires). `check` refuses a missing copy, since `serve` would create an empty database in
// its place. Both take absolute paths. Exits with 0 when done, 1 when refused or failed, 2 on a
// usage error. Tests: scripts/dev-rotation.test.mjs.
import { chmodSync, constants, copyFileSync, existsSync, mkdirSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

/** A refusal: nothing was changed, and the message says why. */
export class Refused extends Error {}

/** Copies the closed dev database `source` to `copy`, in a folder of its own. */
export function copyDatabase(source, copy) {
  const folder = path.dirname(copy);
  if (existsSync(folder)) {
    throw new Refused(`${folder} exists; delete it to start over`);
  }
  if (existsSync(`${source}-wal`)) {
    throw new Refused(`${source}-wal exists: stop \`just dev-server\` first`);
  }
  if (!existsSync(source)) {
    throw new Refused(`there's no dev database at ${source}; run \`just dev-server\` first`);
  }
  // Not recursive: a folder that appeared since the check fails here instead of being reused.
  mkdirSync(folder);
  copyFileSync(source, copy, constants.COPYFILE_EXCL);
  chmodSync(copy, 0o600);
}

/** Refuses when there's no copy at `copy`. */
export function checkCopy(copy) {
  if (!existsSync(copy)) {
    throw new Refused(`there's no copy at ${copy}; run \`just dev-rotate-key\` first`);
  }
}

/** The command's arguments, or `undefined` for a usage error. */
function parse(args) {
  const [command, ...paths] = args;
  if (!paths.every((file) => path.isAbsolute(file))) {
    return undefined;
  }
  if (command === "copy" && paths.length === 2) {
    return { command, paths };
  }
  if (command === "check" && paths.length === 1) {
    return { command, paths };
  }
  return undefined;
}

function main(args) {
  const parsed = parse(args);
  if (parsed === undefined) {
    console.error(
      "usage: node scripts/dev-rotation.mjs copy <absolute dev database> <absolute copy>\n" +
        "       node scripts/dev-rotation.mjs check <absolute copy>",
    );
    return 2;
  }
  const [first, second] = parsed.paths;
  try {
    if (parsed.command === "copy") {
      copyDatabase(first, second);
      console.log(`copied ${first} to ${second}`);
    } else {
      checkCopy(first);
    }
    return 0;
  } catch (error) {
    console.error(`dev-rotation: ${error.message}`);
    return 1;
  }
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = main(process.argv.slice(2));
}
