// Prints a fingerprint of the working tree, taken before and after every
// subagent runs and compared (the user's rule, ADR-0022, U9 and U11;
// CLAUDE.md's step 10). Any change the session didn't make stops the work.
//
//   node scripts/tree-fingerprint.mjs            one SHA-256 per part
//   node scripts/tree-fingerprint.mjs --paths    the entries behind them
//
// It runs from the repository's top (`git rev-parse --show-toplevel`),
// whichever folder it's started in. The parts, each one line:
// - head: HEAD's commit and the branch it's on, as they are;
// - refs: every ref and the object it names (`git for-each-ref`);
// - stash: `git stash list`;
// - config: the repository's own git config (`git config --local --list`);
// - hooks: every file under the hooks folder (`git rev-parse --git-path
//   hooks`, so core.hooksPath's too) by path, mode and content: a new or
//   changed hook runs at the next commit;
// - stage: every index entry with its mode, object and flags (`git ls-files
//   --stage -v`), so skip-worktree and assume-unchanged show;
// - index: the staged changes (`git diff --cached --binary`);
// - tracked: every change to a tracked file against HEAD (`git diff HEAD
//   --binary`), so a second edit to a file that's already modified shows;
// - untracked: every untracked file that isn't ignored, those inside an
//   untracked folder too, by path and the SHA-256 of its content; a
//   repository or worktree of its own inside the tree by its HEAD (or its
//   folder's lstat when it has none), never read as a file;
// - ignored: every ignored path outside target/ and node_modules/, by path,
//   size and mtime from lstat. git lists the ignored folders without
//   entering them (`--directory`), and this script walks each one itself,
//   skipping every entry named target or node_modules at any depth and never
//   following a link. No ignored file is ever opened, so nothing under
//   secrets/ is read, and git never enters target/ or node_modules/: target/
//   dod-run stays unread and unlisted (CLAUDE.md, rule 12).
//
// git's listings must come back without a warning: a folder git couldn't
// read would hide its changes, so it stops the fingerprint instead. It can't
// see a change inside target/ or node_modules/. `--paths` prints paths,
// sizes, mtimes, modes and hashes, never a file's content. Exits 0, 1 when
// git or a folder fails (no repository here, an unreadable folder), 2 on a
// usage error. Tests: scripts/tree-fingerprint.test.mjs.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, lstatSync, readdirSync, readFileSync, readlinkSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

/** The folders never entered or listed, by name, at any depth. */
export const SKIPPED = ["target", "node_modules"];

/** The most git may print for one part: a whole binary diff fits. */
const MAX_OUTPUT = 1024 * 1024 * 1024;

/** A failed git command, with what it printed on stderr. */
export class GitError extends Error {}

/** git's stdout for `args` in `cwd`, as a Buffer; `strict` refuses a warning too. */
function git(cwd, args, strict = false) {
  const result = spawnSync("git", args, { cwd, maxBuffer: MAX_OUTPUT });
  if (result.error !== undefined) {
    throw new GitError(`git ${args[0]} couldn't run: ${result.error.message}`);
  }
  if (result.status !== 0) {
    throw new GitError(`git ${args[0]} failed: ${result.stderr.toString().trim()}`);
  }
  if (strict && result.stderr.length > 0) {
    throw new GitError(`git ${args[0]} warned: ${result.stderr.toString().trim()}`);
  }
  return result.stdout;
}

/** The paths of a `-z` listing, in git's order. */
function paths(output) {
  return output
    .toString("utf8")
    .split("\0")
    .filter((path) => path !== "");
}

/** The SHA-256 of `data`, in hex. */
function sha256(data) {
  return createHash("sha256").update(data).digest("hex");
}

/** By path, as the parts list their entries. */
function byPath(a, b) {
  return a.path < b.path ? -1 : a.path > b.path ? 1 : 0;
}

/** A file's content hash: a link's target, never what it points at. */
function contentHash(file) {
  const stat = lstatSync(file);
  return stat.isSymbolicLink() ? sha256(`link:${readlinkSync(file)}`) : sha256(readFileSync(file));
}

/**
 * A repository or worktree of its own inside the tree (`dir/` in git's
 * listing): its HEAD, or its folder's lstat when it has none yet.
 */
function nestedHash(root, path) {
  const head = spawnSync("git", ["rev-parse", "--verify", "--quiet", "HEAD"], {
    cwd: join(root, path),
  });
  if (head.status === 0) {
    return sha256(`dir:${head.stdout.toString().trim()}`);
  }
  const stat = lstatSync(join(root, path), { bigint: true });
  return sha256(`dir:${stat.size}:${stat.mtimeNs}`);
}

/** Every untracked entry that isn't ignored: a file by content, a repository by its HEAD. */
function untrackedEntries(root) {
  return paths(git(root, ["ls-files", "-z", "--others", "--exclude-standard"], true)).map(
    (path) => ({
      path,
      sha256: path.endsWith("/") ? nestedHash(root, path) : contentHash(join(root, path)),
    }),
  );
}

/** Records the entry at `path` in `found`, by lstat. */
function record(root, path, found) {
  const stat = lstatSync(join(root, path), { bigint: true });
  found.set(path, { path, size: stat.size.toString(), mtime: stat.mtimeNs.toString() });
}

/** Records every entry under the folder `dir`: target/ and node_modules/ skipped, no link followed. */
function walk(root, dir, found) {
  for (const entry of readdirSync(join(root, dir), { withFileTypes: true })) {
    if (SKIPPED.includes(entry.name)) {
      continue;
    }
    const path = `${dir}/${entry.name}`;
    if (entry.isDirectory()) {
      walk(root, path, found);
    } else {
      record(root, path, found);
    }
  }
}

/**
 * Every ignored file outside target/ and node_modules/, by path, size and
 * mtime. git lists the ignored folders without entering them; a folder it
 * lists with its files too counts each file once.
 */
function ignoredEntries(root) {
  const found = new Map();
  const listed = paths(
    git(
      root,
      ["ls-files", "-z", "--others", "--ignored", "--exclude-standard", "--directory"],
      true,
    ),
  );
  for (const listing of listed) {
    const path = listing.endsWith("/") ? listing.slice(0, -1) : listing;
    if (path.split("/").some((part) => SKIPPED.includes(part))) {
      continue;
    }
    if (lstatSync(join(root, path)).isDirectory()) {
      walk(root, path, found);
    } else {
      record(root, path, found);
    }
  }
  return [...found.values()].sort(byPath);
}

/** Every file under the hooks folder, by path, mode and content. */
function hookEntries(root) {
  const named = git(root, ["rev-parse", "--git-path", "hooks"]).toString().trim();
  const folder = isAbsolute(named) ? named : join(root, named);
  const found = [];
  const visit = (relative) => {
    for (const entry of readdirSync(join(folder, relative), { withFileTypes: true })) {
      const path = relative === "" ? entry.name : `${relative}/${entry.name}`;
      if (entry.isDirectory()) {
        visit(path);
      } else {
        const file = join(folder, path);
        found.push({ path, mode: lstatSync(file).mode.toString(8), sha256: contentHash(file) });
      }
    }
  };
  if (existsSync(folder)) {
    visit("");
  }
  return found.sort(byPath);
}

/**
 * The fingerprint of the repository `cwd` is in, taken from its top:
 * `lines`, one per part, and the `hooks`, `untracked` and `ignored` entries
 * behind three of them.
 */
export function fingerprint(cwd) {
  const root = git(cwd, ["rev-parse", "--show-toplevel"]).toString().trim();
  const head = git(root, ["rev-parse", "--verify", "--quiet", "HEAD"]).toString().trim();
  const branch = spawnSync("git", ["symbolic-ref", "--quiet", "HEAD"], { cwd: root })
    .stdout.toString()
    .trim();
  const hooks = hookEntries(root);
  const untracked = untrackedEntries(root);
  const ignored = ignoredEntries(root);
  const lines = [
    `head ${head} ${branch === "" ? "(detached)" : branch}`,
    `refs ${sha256(git(root, ["for-each-ref", "--format=%(refname) %(objectname)"]))}`,
    `stash ${sha256(git(root, ["stash", "list"]))}`,
    `config ${sha256(git(root, ["config", "--local", "--list"]))}`,
    `hooks ${hooks.length} ${sha256(hooks.map((e) => `${e.path}\0${e.mode}\0${e.sha256}\n`).join(""))}`,
    `stage ${sha256(git(root, ["ls-files", "--stage", "-v", "-z"]))}`,
    `index ${sha256(git(root, ["diff", "--cached", "--binary"]))}`,
    `tracked ${sha256(git(root, ["diff", "HEAD", "--binary"]))}`,
    `untracked ${untracked.length} ${sha256(untracked.map((e) => `${e.path}\0${e.sha256}\n`).join(""))}`,
    `ignored ${ignored.length} ${sha256(ignored.map((e) => `${e.path}\0${e.size}\0${e.mtime}\n`).join(""))}`,
  ];
  return { lines, hooks, untracked, ignored };
}

/** What `--paths` prints: each entry on a line of its own. */
export function entryLines({ hooks, untracked, ignored }) {
  return [
    ...untracked.map((e) => `untracked ${e.sha256} ${e.path}`),
    ...ignored.map((e) => `ignored ${e.size} ${e.mtime} ${e.path}`),
    ...hooks.map((e) => `hook ${e.sha256} ${e.mode} ${e.path}`),
  ];
}

/** Runs the command with `args` in `cwd`; returns its exit code. */
export function main(args, cwd, out = process.stdout, err = process.stderr) {
  const known = ["--paths"];
  const unknown = args.filter((arg) => !known.includes(arg));
  if (unknown.length > 0) {
    err.write(
      `usage: node scripts/tree-fingerprint.mjs [--paths] (unknown: ${unknown.join(" ")})\n`,
    );
    return 2;
  }
  try {
    const found = fingerprint(cwd);
    const lines = args.includes("--paths") ? entryLines(found) : found.lines;
    out.write(`${lines.join("\n")}\n`);
    return 0;
  } catch (error) {
    err.write(`the tree's fingerprint couldn't be taken: ${error.message}\n`);
    return 1;
  }
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  process.exitCode = main(process.argv.slice(2), process.cwd());
}
