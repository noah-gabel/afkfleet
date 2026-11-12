// Runs cargo-mutants over fleet-core, over the auth and vault modules, or both.
//
//   node scripts/mutants.mjs <core|auth-vault|all> [shards]
//
// cargo-mutants 27.1.0 changes the code one place at a time (a mutant) and runs the tests
// on each: a mutant no test fails on is missed (Plan.md P13.3, ADR-0031). The scopes are
// `core`, all of fleet-core, and `auth-vault`, the files the coverage gates of those two
// names list outside fleet-core (coverage-gates.mjs), so a module that joins a gate joins
// here. `.cargo/mutants.toml` names the test tool, the profile and the mutants left out.
//
// The run never touches the working tree, and never copies it: cargo-mutants' own copy
// would take every file git ignores along, and stops at pnpm's linked folders on Windows.
// Instead each shard gets a git worktree of the commit under test, `target/mutants/tree-<k>`,
// which holds tracked files only, and cargo-mutants changes that tree in place
// (`--in-place --shard k/n`), all shards at once. So the run tests HEAD: with uncommitted
// changes it refuses to start. A tree is reset before and after its run, since a mutated
// run leaves files behind (proptest's failure seeds, and a mutant in the source when the
// run is cut off).
//
// Reports: target/mutants/<scope>/shard-<k>/mutants.out/, and beside it shard-<k>.log,
// cargo-mutants' own output. The script prints each shard's counts, every missed mutant
// and every one whose tests ran out of time, then PASS or FAIL. Exits with 1 when a mutant
// is missed or a shard fails, 2 on wrong arguments or a tree it can't use. A whole run
// takes hours. Tests: scripts/mutants.test.mjs.
import { spawn, spawnSync } from "node:child_process";
import { closeSync, existsSync, mkdirSync, openSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

import { GATES } from "./coverage-gates.mjs";

/** What a run can cover. */
export const SCOPES = ["core", "auth-vault"];

/** The shards of a run when none are given: as many builds run at once. */
export const DEFAULT_SHARDS = 4;

/** The most shards a run takes. */
export const MAX_SHARDS = 8;

/** The coverage gates whose modules the `auth-vault` scope mutates. */
const GATE_NAMES = ["auth", "vault"];

/** fleet-core's folder: the `core` scope covers it whole, so `auth-vault` leaves it out. */
const CORE_FOLDER = "crates/fleet-core/";

/** The runs' folder, from the repository's root. */
const OUT_FOLDER = join("target", "mutants");

/** The lists of a report this script reads, by outcome. */
const LISTS = ["caught", "missed", "timeout", "unviable"];

/**
 * Reads the arguments after the script's name. Returns `{ scopes, shards }`, or
 * `{ error }` with a message.
 */
export function parseArgs(args) {
  const [scope, shards = String(DEFAULT_SHARDS), ...rest] = args;
  if (scope === undefined || rest.length > 0) {
    return { error: "usage: node scripts/mutants.mjs <core|auth-vault|all> [shards]" };
  }
  if (scope !== "all" && !SCOPES.includes(scope)) {
    return { error: `no scope is named \`${scope}\`: ${SCOPES.join(", ")}, or all` };
  }
  if (!/^[1-9]$/.test(shards) || Number(shards) > MAX_SHARDS) {
    return { error: `the shards are a whole number from 1 to ${MAX_SHARDS}` };
  }
  return { scopes: scope === "all" ? SCOPES : [scope], shards: Number(shards) };
}

/**
 * What the `auth-vault` scope mutates, from the coverage `gates`: `packages`, sorted, and
 * `files` as cargo-mutants' globs, a folder with everything below it. Throws when a gate
 * is missing or an entry isn't a file of a crate.
 */
export function gateFilters(gates) {
  const packages = new Set();
  const files = [];
  for (const name of GATE_NAMES) {
    const gate = gates.find((candidate) => candidate.name === name);
    if (gate?.prefixes === undefined) {
      throw new Error(`no coverage gate is named ${name}`);
    }
    for (const entry of gate.prefixes) {
      if (entry.startsWith(CORE_FOLDER)) {
        continue;
      }
      const crate = /^crates\/([^/]+)\//.exec(entry)?.[1];
      if (crate === undefined) {
        throw new Error(`${entry} isn't under crates/`);
      }
      packages.add(crate);
      files.push(entry.endsWith("/") ? `${entry}**` : entry);
    }
  }
  return { packages: [...packages].sort(), files };
}

/** cargo-mutants' arguments that pick `scope`'s mutants. */
export function selection(scope, gates) {
  if (scope === "core") {
    return ["-p", "fleet-core"];
  }
  const { packages, files } = gateFilters(gates);
  return [...packages.flatMap((name) => ["-p", name]), ...files.flatMap((file) => ["-f", file])];
}

/**
 * cargo's arguments for shard `shard` of `shards`, counted from 1, its report going into
 * `out`. cargo-mutants counts its shards from 0.
 */
export function mutantsArgs(picked, shard, shards, out) {
  return ["mutants", "--in-place", "--shard", `${shard - 1}/${shards}`, "-o", out, ...picked];
}

/** A shard's worktree, from the repository's root. */
export function treePath(shard) {
  return join(OUT_FOLDER, `tree-${shard}`);
}

/** The worktrees' paths in `git worktree list --porcelain`'s output, with `/`. */
export function registeredTrees(porcelain) {
  return porcelain
    .split("\n")
    .map((line) => line.trimEnd())
    .filter((line) => line.startsWith("worktree "))
    .map((line) => line.slice("worktree ".length).replaceAll("\\", "/"));
}

/** A report list's mutants, one per line of `text`. */
export function outcomeLines(text) {
  return text
    .split("\n")
    .map((line) => line.trimEnd())
    .filter((line) => line !== "");
}

/** cargo-mutants' exit codes a shard with these lists may end with. */
function expectedStatuses(result) {
  if (result.missed.length > 0) {
    // 2 says missed; with a timeout beside it, either code tells the truth.
    return result.timeout.length > 0 ? [2, 3] : [2];
  }
  return result.timeout.length > 0 ? [3] : [0];
}

/**
 * The verdict over a scope's shards, each `{ shard, status, caught, unviable, missed,
 * timeout }` with cargo-mutants' exit code, two counts and two lists of mutants. Returns
 * the `lines` to print and the exit `code`: 1 when a mutant is missed, or a shard's exit
 * code isn't the one its lists stand for. A mutant whose tests ran out of time counts as
 * found: its test never ends.
 */
export function verdict(scope, results) {
  const lines = [];
  for (const result of results) {
    lines.push(
      `${scope}, shard ${result.shard}: ${result.caught} caught, ${result.missed.length} missed, ` +
        `${result.timeout.length} timed out, ${result.unviable} unviable`,
    );
    lines.push(...result.missed.map((mutant) => `missed: ${mutant}`));
    lines.push(...result.timeout.map((mutant) => `timed out: ${mutant}`));
  }
  const failed = results.find((result) => !expectedStatuses(result).includes(result.status));
  if (failed !== undefined) {
    lines.push(
      `${scope}: FAIL, shard ${failed.shard} ended with ${failed.status}: its log says why`,
    );
    return { code: 1, lines };
  }
  const count = (list) => results.reduce((sum, result) => sum + list(result), 0);
  const missed = count((result) => result.missed.length);
  if (missed > 0) {
    lines.push(
      `${scope}: FAIL, ${missed} missed: a test for each, or its line in .cargo/mutants.toml with why`,
    );
    return { code: 1, lines };
  }
  const timeouts = count((result) => result.timeout.length);
  const caught = count((result) => result.caught);
  lines.push(
    `${scope}: PASS, ${caught} caught and none missed` +
      (timeouts > 0 ? `; ${timeouts} timed out, each one a test that never ends` : ""),
  );
  return { code: 0, lines };
}

/** Stops the script: a tree or a commit it can't use. */
function refuse(message) {
  console.error(message);
  process.exit(2);
}

/** Runs git in `cwd` and gives its output; stops the script when it fails. */
function git(cwd, args) {
  const run = spawnSync("git", args, { cwd, encoding: "utf8" });
  if (run.status !== 0) {
    refuse(`git ${args.join(" ")} failed in ${cwd}: ${run.stderr.trim()}`);
  }
  return run.stdout;
}

/** A path as git writes it, for comparing: with `/`, and on Windows without regard to case. */
function comparable(path) {
  const slashed = path.replaceAll("\\", "/");
  return process.platform === "win32" ? slashed.toLowerCase() : slashed;
}

/**
 * Shard `shard`'s worktree at `commit`, with nothing a run left behind: created when it's
 * missing, reset when it's this repository's worktree, refused when it's anything else.
 */
function prepareTree(root, shard, commit) {
  const tree = join(root, treePath(shard));
  if (!existsSync(tree)) {
    git(root, ["worktree", "add", "--quiet", "--detach", tree, commit]);
    return tree;
  }
  const registered = registeredTrees(git(root, ["worktree", "list", "--porcelain"]));
  const top = existsSync(join(tree, ".git"))
    ? git(tree, ["rev-parse", "--show-toplevel"]).trim()
    : "";
  if (
    !registered.map(comparable).includes(comparable(tree)) ||
    comparable(top) !== comparable(tree)
  ) {
    refuse(`${treePath(shard)} is there, but it isn't this repository's worktree: move it away`);
  }
  // Its own tree alone: tracked files back to their commit, then the files a mutated run
  // wrote. What git ignores (the tree's `target/`) stays.
  git(tree, ["checkout", "--quiet", "--", "."]);
  git(tree, ["clean", "--quiet", "-fd"]);
  git(tree, ["checkout", "--quiet", "--detach", commit]);
  return tree;
}

/** Runs one shard and resolves with cargo-mutants' exit code. */
function runShard(tree, args, log) {
  return new Promise((resolve) => {
    const output = openSync(log, "w");
    const child = spawn("cargo", args, { cwd: tree, stdio: ["ignore", output, output] });
    const ended = (status) => {
      closeSync(output);
      resolve(status);
    };
    child.on("error", () => ended(null));
    child.on("close", (status) => ended(status));
  });
}

/** A report list's mutants, or none when the run wrote no such list. */
function readList(out, name) {
  const file = join(out, "mutants.out", `${name}.txt`);
  return existsSync(file) ? outcomeLines(readFileSync(file, "utf8")) : [];
}

/** Runs `scope` in `shards` worktrees at once and gives its verdict. */
async function runScope(root, scope, shards, commit) {
  const picked = selection(scope, GATES);
  const folder = join(root, OUT_FOLDER, scope);
  mkdirSync(folder, { recursive: true });
  const numbers = Array.from({ length: shards }, (_, index) => index + 1);
  const trees = numbers.map((shard) => prepareTree(root, shard, commit));
  console.log(`${scope}: ${shards} shards, their logs in ${join(OUT_FOLDER, scope)}`);
  const statuses = await Promise.all(
    numbers.map((shard, index) => {
      const out = join(folder, `shard-${shard}`);
      const args = mutantsArgs(picked, shard, shards, out);
      return runShard(trees[index], args, join(folder, `shard-${shard}.log`));
    }),
  );
  for (const shard of numbers) {
    prepareTree(root, shard, commit);
  }
  const results = numbers.map((shard, index) => {
    const [caught, missed, timeout, unviable] = LISTS.map((name) =>
      readList(join(folder, `shard-${shard}`), name),
    );
    return {
      shard,
      status: statuses[index],
      caught: caught.length,
      unviable: unviable.length,
      missed,
      timeout,
    };
  });
  return verdict(scope, results);
}

async function main() {
  const root = join(dirname(fileURLToPath(import.meta.url)), "..");
  const parsed = parseArgs(process.argv.slice(2));
  if (parsed.error !== undefined) {
    refuse(parsed.error);
  }
  if (git(root, ["status", "--porcelain"]).trim() !== "") {
    refuse("there are uncommitted changes: the run tests the last commit, so commit them first");
  }
  const commit = git(root, ["rev-parse", "HEAD"]).trim();
  console.log(`mutants of commit ${commit.slice(0, 8)}`);
  let code = 0;
  for (const scope of parsed.scopes) {
    const result = await runScope(root, scope, parsed.shards, commit);
    for (const line of result.lines) {
      console.log(line);
    }
    code = Math.max(code, result.code);
  }
  process.exit(code);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  await main();
}
