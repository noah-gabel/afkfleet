// Prints a fingerprint of the working tree, taken before and after every
// subagent runs and compared (the user's rule, ADR-0022, U9 and U11;
// CLAUDE.md's step 10). Any change the session didn't make stops the work.
//
//   node scripts/tree-fingerprint.mjs            one SHA-256 per part
//   node scripts/tree-fingerprint.mjs --paths    the entries behind them
//
// The parts, each one line:
// - head: HEAD's commit and the branch it's on, as they are;
// - refs: every ref and the object it names (`git for-each-ref`);
// - stash: `git stash list`;
// - config: the repository's own git config (`git config --local --list`);
// - index: the staged changes (`git diff --cached --binary`);
// - tracked: every change to a tracked file against HEAD (`git diff HEAD
//   --binary`), so a second edit to a file that's already modified shows;
// - untracked: every untracked file that isn't ignored, those inside an
//   untracked folder too, by path and the SHA-256 of its content;
// - ignored: every ignored path outside target/ and node_modules/, at any
//   depth, by path, size and mtime from lstat. None is ever opened, so
//   nothing under secrets/ is read, and nothing under target/ is even
//   listed: target/dod-run stays unread (CLAUDE.md, rule 12).
//
// It can't see a change inside target/ or node_modules/. `--paths` prints
// paths, sizes, mtimes and hashes, never a file's content. Exits 0, 1 when git
// fails (no repository here, for one), 2 on a usage error.
// Tests: scripts/tree-fingerprint.test.mjs.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { lstatSync, readFileSync, readlinkSync } from "node:fs";
import { join } from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

/** The folders never listed: build output and packages, at any depth. */
export const SKIPPED = [":(exclude,glob)**/target/**", ":(exclude,glob)**/node_modules/**"];

/** The most git may print for one part: a whole binary diff fits. */
const MAX_OUTPUT = 1024 * 1024 * 1024;

/** A failed git command, with what it printed on stderr. */
export class GitError extends Error {}

/** git's stdout for `args` in `cwd`, as a Buffer. */
function git(cwd, args) {
  const result = spawnSync("git", args, { cwd, maxBuffer: MAX_OUTPUT });
  if (result.error !== undefined) {
    throw new GitError(`git ${args[0]} couldn't run: ${result.error.message}`);
  }
  if (result.status !== 0) {
    throw new GitError(`git ${args[0]} failed: ${result.stderr.toString().trim()}`);
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

/** An untracked file's content hash: a link's target, never what it points at. */
function contentHash(file) {
  const stat = lstatSync(file);
  return stat.isSymbolicLink() ? sha256(`link:${readlinkSync(file)}`) : sha256(readFileSync(file));
}

/**
 * The fingerprint of the repository at `cwd`: `lines`, one per part, and the
 * `untracked` and `ignored` entries behind two of them.
 */
export function fingerprint(cwd) {
  const head = git(cwd, ["rev-parse", "--verify", "--quiet", "HEAD"]).toString().trim();
  const branch = spawnSync("git", ["symbolic-ref", "--quiet", "HEAD"], { cwd })
    .stdout.toString()
    .trim();
  const untracked = paths(git(cwd, ["ls-files", "-z", "--others", "--exclude-standard"])).map(
    (path) => ({ path, sha256: contentHash(join(cwd, path)) }),
  );
  const ignored = paths(
    git(cwd, ["ls-files", "-z", "--others", "--ignored", "--exclude-standard", "--", ...SKIPPED]),
  ).map((path) => {
    const stat = lstatSync(join(cwd, path), { bigint: true });
    return { path, size: stat.size.toString(), mtime: stat.mtimeNs.toString() };
  });
  const lines = [
    `head ${head} ${branch === "" ? "(detached)" : branch}`,
    `refs ${sha256(git(cwd, ["for-each-ref", "--format=%(refname) %(objectname)"]))}`,
    `stash ${sha256(git(cwd, ["stash", "list"]))}`,
    `config ${sha256(git(cwd, ["config", "--local", "--list"]))}`,
    `index ${sha256(git(cwd, ["diff", "--cached", "--binary"]))}`,
    `tracked ${sha256(git(cwd, ["diff", "HEAD", "--binary"]))}`,
    `untracked ${untracked.length} ${sha256(untracked.map((e) => `${e.path}\0${e.sha256}\n`).join(""))}`,
    `ignored ${ignored.length} ${sha256(ignored.map((e) => `${e.path}\0${e.size}\0${e.mtime}\n`).join(""))}`,
  ];
  return { lines, untracked, ignored };
}

/** What `--paths` prints: each entry on a line of its own. */
export function entryLines({ untracked, ignored }) {
  return [
    ...untracked.map((e) => `untracked ${e.sha256} ${e.path}`),
    ...ignored.map((e) => `ignored ${e.size} ${e.mtime} ${e.path}`),
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
