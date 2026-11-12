// Removes what the mutation runs keep: their worktrees and target/mutants.
//
//   node scripts/mutants-clean.mjs
//
// `just mutants` leaves a git worktree a shard, target/mutants/tree-<k>, each with its own
// build (about 10 GB), and its reports beside them, so the next run builds little (ADR-0031).
// `just mutants-clean` gives the space back. It removes exactly this, in the repository it
// sits in, found from its own location:
// - every registered worktree named target/mutants/tree-<k>, with `git worktree remove
//   --force`, which also forgets one whose folder was deleted by hand. `--force` because a
//   tree holds its build, and a run that was cut off leaves a mutant in its source;
// - then target/mutants itself, walked with lstat: a link or junction is removed as an
//   entry, never followed, a folder emptied and removed, a file removed.
// It never touches anything else in target/, and it is strict about what it finds:
// - a target/ or a target/mutants that is a link or junction is refused, without
//   following it;
// - any other registered worktree inside target/mutants is refused, and nothing is
//   removed: only git should remove a worktree, and only its owner should say so;
// - when git can't remove a tree (it's locked, or a run still holds its files), it stops
//   there with git's reason. What it removed by then stays removed; running it again
//   removes the rest.
// Not while `just mutants` runs: the run would lose its trees. Exits with 0 when everything
// was removed or nothing was there, 1 when it refused or stopped at an error, 2 on a usage
// error. Tests: scripts/mutants-clean.test.mjs.
import { spawnSync } from "node:child_process";
import { lstatSync, readdirSync, realpathSync, rmdirSync, unlinkSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

import { OUT_FOLDER, registeredTrees } from "./mutants.mjs";

/** The folder as messages name it. */
const NAMED = OUT_FOLDER.split(path.sep).join("/");

const MISSING = `Nothing to clean: ${NAMED} doesn't exist.`;

/** A run's worktree, by its name inside the folder. */
const TREE = /^tree-[1-9]\d*$/;

/** `count` worktrees, in words. */
function worktrees(count) {
  return count === 1 ? "1 worktree" : `${count} worktrees`;
}

/** The refusal for a link at `named`. */
function linked(named) {
  return `${named} is a link or junction: mutants-clean doesn't follow it. Remove the link itself, then run just mutants-clean again.`;
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

/**
 * `where` as the file system names it, as far as it exists: its nearest existing folder's
 * real path, with the rest as given. git and Node can spell one path two ways (a short
 * name on Windows, a linked temporary folder on macOS).
 */
function canonical(where) {
  const rest = [];
  let current = path.resolve(where);
  for (;;) {
    try {
      return path.join(realpathSync.native(current), ...rest);
    } catch {
      const parent = path.dirname(current);
      if (parent === current) {
        return path.resolve(where);
      }
      rest.unshift(path.basename(current));
      current = parent;
    }
  }
}

/** A path for comparing: with `/`, and on Windows without regard to case. */
function comparable(where) {
  const slashed = where.split(path.sep).join("/");
  return process.platform === "win32" ? slashed.toLowerCase() : slashed;
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

/** Runs git in `root`: `{ ok, out, reason }`, the reason one line without git's "fatal: ". */
function git(root, args) {
  const run = spawnSync("git", args, { cwd: root, encoding: "utf8" });
  const said = (run.stderr ?? run.error?.message ?? "").trim().replace(/\s+/g, " ");
  return {
    ok: run.status === 0,
    out: run.stdout ?? "",
    reason: said.replace(/^fatal: /, "").replace(/\.$/, ""),
  };
}

/**
 * The registered worktrees inside `folder`, each `{ listed, inside }`: the path as git
 * lists it, and its path inside the folder with `/`. `null` when git can't list them.
 */
function treesInside(root, folder) {
  const listed = git(root, ["worktree", "list", "--porcelain"]);
  if (!listed.ok) {
    return null;
  }
  const prefix = `${comparable(folder)}/`;
  return registeredTrees(listed.out)
    .map((tree) => ({ listed: tree, found: canonical(tree) }))
    .filter(({ found }) => comparable(found).startsWith(prefix))
    .map(({ listed: tree, found }) => ({
      listed: tree,
      inside: found
        .slice(folder.length + 1)
        .split(path.sep)
        .join("/"),
    }))
    .sort((a, b) => a.inside.localeCompare(b.inside, "en", { numeric: true }));
}

/**
 * Removes the mutation runs' worktrees and target/mutants in the repository `repo`:
 * returns `{ status, message }`, the exit code and the line to print.
 */
export function clean(repo) {
  const root = realpathSync.native(repo);
  const target = path.join(root, "target");
  const folder = path.join(root, OUT_FOLDER);
  for (const [where, named] of [
    [target, "target"],
    [folder, NAMED],
  ]) {
    if (entry(where)?.isSymbolicLink()) {
      return { status: 1, message: linked(named) };
    }
  }
  const trees = treesInside(root, folder);
  if (trees === null) {
    return {
      status: 1,
      message: "mutants-clean couldn't ask git for this repository's worktrees.",
    };
  }
  const stranger = trees.find(({ inside }) => !TREE.test(inside));
  if (stranger !== undefined) {
    return {
      status: 1,
      message: `${NAMED}/${stranger.inside} is a worktree no mutation run made: mutants-clean leaves it and everything else. Move it or remove it with \`git worktree remove\`, then run just mutants-clean again.`,
    };
  }
  for (const tree of trees) {
    const removed = git(root, ["worktree", "remove", "--force", tree.listed]);
    if (!removed.ok) {
      return {
        status: 1,
        message: `mutants-clean couldn't remove the worktree ${NAMED}/${tree.inside}: ${removed.reason}. Nothing else was removed after it; run just mutants-clean again once that's settled.`,
      };
    }
  }
  if (entry(folder) === null) {
    return {
      status: 0,
      message:
        trees.length === 0
          ? MISSING
          : `Nothing left in ${NAMED}; git forgot its ${worktrees(trees.length)}.`,
    };
  }
  try {
    remove(folder);
  } catch (error) {
    const where = path
      .relative(root, error.path ?? folder)
      .split(path.sep)
      .join("/");
    return {
      status: 1,
      message: `mutants-clean couldn't remove ${where} (${error.code ?? error.message}): something holds it open, such as a run still going, a terminal or an editor in that folder. Close it, then run just mutants-clean again.`,
    };
  }
  return {
    status: 0,
    message:
      trees.length === 0
        ? `Removed ${NAMED}; it held no worktree.`
        : `Removed ${NAMED} and its ${worktrees(trees.length)}.`,
  };
}

function main(args) {
  if (args.length !== 0) {
    console.error("usage: node scripts/mutants-clean.mjs");
    return 2;
  }
  const repo = path.join(path.dirname(fileURLToPath(import.meta.url)), "..");
  const { status, message } = clean(repo);
  if (status === 0) {
    console.log(message);
  } else {
    console.error(message);
  }
  return status;
}

// Node runs a script by its real path, so the entry check compares with process.argv[1]'s.
if (
  process.argv[1] !== undefined &&
  import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href
) {
  process.exitCode = main(process.argv.slice(2));
}
