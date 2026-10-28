// Deletes the DoD checks' folder, target/dod-run, when the user is done with it.
//
//   node scripts/dod-clean.mjs <folder>
//
// `just dod-clean` runs it with this repository's target/dod-run (ADR-0020, U1, U33, U52,
// S72, R130). The user only: the AI never runs it. The folder holds the user's real
// account, sealed, next to the vault key that opens it, so the script is strict:
// - It accepts exactly the target/dod-run of the repository it sits in, found from its own
//   location, and refuses any other path (on Windows ignoring case, as paths do there).
// - It refuses a target/ or a dod-run that is a link or junction, without following it.
// - It refuses while afkfleet.db-wal is there: SQLite removes it when the last connection
//   closes, and a server stopped by a signal or closed cleanly leaves none (ADR-0018's
//   `a_server_stopped_by_a_signal_leaves_no_wal_or_shm` and
//   `a_closed_open_migrated_database_leaves_no_wal_or_shm`), so one means dod-server runs,
//   or crashed.
// - It refuses while the agent's heartbeat, agent/agent.alive, is under 30 s old (or in the
//   future): the agent touches it while it runs.
// - Otherwise it walks the folder with lstat: a link or junction is removed as an entry,
//   never followed, a folder emptied and removed, a file removed. An error on the way (a
//   folder a terminal sits in: EBUSY on Windows) stops it with one line naming the cause and
//   what to do; what it removed by then stays removed, and running it again removes the rest.
// - The folder is compared by real paths, so a linked path to the repository names it too.
// A missing folder is nothing to clean. It never touches Docker: `just dod-mc-down` removes
// the container. Each refusal says what to do (U52). Exits with 0 when the folder was
// removed or didn't exist, 1 when it refused or stopped at an error, 2 on a usage error.
// Tests: scripts/dod-clean.test.mjs.
import { lstatSync, readdirSync, realpathSync, rmdirSync, unlinkSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

/** The heartbeat's age below which the DoD agent counts as running. */
const HEARTBEAT_MS = 30_000;

const DONE = "Removed target/dod-run and everything in it.";
const ANOTHER = "dod-clean removes only this repository's target/dod-run.";
const MISSING = "Nothing to clean: target/dod-run doesn't exist.";
const WAL =
  "afkfleet.db-wal is there: dod-server is running, or a crash left it. Stop dod-server with Ctrl+C. After a crash, start just dod-server once, stop it with Ctrl+C, then run just dod-clean again.";
const HEARTBEAT =
  "The DoD agent is running (its heartbeat is under 30 s old). Stop dod-agent with Ctrl+C, then run just dod-clean again.";

/** The refusal for a link at `where`. */
function linked(where) {
  return `${where} is a link or junction: dod-clean doesn't follow it. Remove the link itself, then run just dod-clean again.`;
}

/** `where`'s lstat, or null when nothing is there. */
function entry(where) {
  try {
    return lstatSync(where);
  } catch (error) {
    if (error.code === "ENOENT" || error.code === "ENOTDIR") {
      return null;
    }
    throw error;
  }
}

/** `where`'s real path, links resolved, or null when it doesn't exist. */
function real(where) {
  try {
    return realpathSync(where);
  } catch {
    return null;
  }
}

/** The line for an error while removing: its cause, and what to do. */
function stopped(root, dodRun, error) {
  const where = path
    .relative(root, error.path ?? dodRun)
    .split(path.sep)
    .join("/");
  return `dod-clean couldn't remove ${where} (${error.code ?? error.message}): something holds it open, such as a terminal or an editor in that folder. Close it, then run just dod-clean again.`;
}

/** Whether two absolute paths name the same place: ignoring case on Windows. */
function samePath(a, b) {
  return process.platform === "win32" ? a.toLowerCase() === b.toLowerCase() : a === b;
}

/** Removes a link or junction itself, never what it points to. */
function removeLink(where) {
  try {
    unlinkSync(where);
  } catch (error) {
    // A directory link Windows won't unlink: rmdir removes the link, not its target.
    if (error.code !== "EPERM" && error.code !== "EISDIR") {
      throw error;
    }
    rmdirSync(where);
  }
}

/** Removes `where` and everything under it, links as entries, never followed. */
function remove(where) {
  const stat = lstatSync(where);
  if (stat.isSymbolicLink()) {
    removeLink(where);
  } else if (stat.isDirectory()) {
    for (const name of readdirSync(where)) {
      remove(path.join(where, name));
    }
    rmdirSync(where);
  } else {
    unlinkSync(where);
  }
}

/**
 * Cleans `folder` if it's `repo`'s target/dod-run, at the time `now` (ms), removing with
 * `removeTree`: returns `{ status, message }`, the exit code and the line to print. Both
 * sides are compared by their real paths (PR #78's review): `repo`, and the folder's
 * target/.., so a linked path to the repository names it too, while target/ and dod-run
 * themselves are never resolved: a link there is refused below.
 */
export function clean(repo, folder, now = Date.now(), removeTree = remove) {
  const root = realpathSync(repo);
  const target = path.join(root, "target");
  const dodRun = path.join(target, "dod-run");
  const given = path.resolve(folder);
  const givenRoot = real(path.dirname(path.dirname(given)));
  const named = path.join(path.basename(path.dirname(given)), path.basename(given));
  if (
    givenRoot === null ||
    !samePath(givenRoot, root) ||
    !samePath(named, path.join("target", "dod-run"))
  ) {
    return { status: 1, message: ANOTHER };
  }
  for (const where of [target, dodRun]) {
    const stat = entry(where);
    if (stat === null) {
      return { status: 0, message: MISSING };
    }
    if (stat.isSymbolicLink()) {
      return { status: 1, message: linked(where) };
    }
  }
  if (entry(path.join(dodRun, "afkfleet.db-wal")) !== null) {
    return { status: 1, message: WAL };
  }
  const heartbeat = entry(path.join(dodRun, "agent", "agent.alive"));
  if (heartbeat !== null && now - heartbeat.mtimeMs < HEARTBEAT_MS) {
    return { status: 1, message: HEARTBEAT };
  }
  try {
    removeTree(dodRun);
  } catch (error) {
    return { status: 1, message: stopped(root, dodRun, error) };
  }
  return { status: 0, message: DONE };
}

function main(args) {
  if (args.length !== 1) {
    console.error("usage: node scripts/dod-clean.mjs <folder>");
    return 2;
  }
  const repo = path.join(path.dirname(fileURLToPath(import.meta.url)), "..");
  const { status, message } = clean(repo, args[0]);
  if (status === 0) {
    console.log(message);
  } else {
    console.error(message);
  }
  return status;
}

// Node runs a script by its real path, so the entry check compares with process.argv[1]'s:
// run through a linked path, a plain comparison would silently skip main (PR #78's review).
if (
  process.argv[1] !== undefined &&
  import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href
) {
  process.exitCode = main(process.argv.slice(2));
}
