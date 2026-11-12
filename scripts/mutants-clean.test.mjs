// Tests for mutants-clean.mjs. Run with `just scripts-test`.
//
// Every test builds its own git repository in a temporary folder and never touches this
// one's target/: `clean` gets the temporary repository as its root, and the command-line
// test runs copies of the scripts placed in that repository's scripts/, which find their
// repository from their own location.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { afterEach, beforeEach, describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { recipeBody } from "./justfile.mjs";
import { registeredTrees } from "./mutants.mjs";
import { clean } from "./mutants-clean.mjs";

const SCRIPTS = path.dirname(fileURLToPath(import.meta.url));

const MISSING = "Nothing to clean: target/mutants doesn't exist.";

let tmp;
let repo;
let target;
let folder;
let outside;

/** Runs git in the temporary repository and gives its output; throws when it fails. */
function git(...args) {
  const run = spawnSync("git", args, { cwd: repo, encoding: "utf8" });
  assert.equal(run.status, 0, `git ${args.join(" ")}: ${run.stderr}`);
  return run.stdout;
}

/** Writes `text` to `file`, creating its folder. */
function put(file, text = "x") {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, text);
}

/** A link at `where` to the folder `to`: a junction on Windows, a symlink elsewhere. */
function link(to, where) {
  mkdirSync(path.dirname(where), { recursive: true });
  symlinkSync(to, where, process.platform === "win32" ? "junction" : "dir");
}

/** A worktree of the temporary repository at `where`, from its root, as a run makes one. */
function worktree(where) {
  git("worktree", "add", "--quiet", "--detach", where, "HEAD");
}

/** The registered worktrees' paths from the repository's root, with `/`, but the root's own. */
function worktrees() {
  const [, ...others] = registeredTrees(git("worktree", "list", "--porcelain"));
  return others.map((tree) => tree.slice(tree.lastIndexOf("/target/") + 1));
}

beforeEach(() => {
  tmp = mkdtempSync(path.join(tmpdir(), "afkfleet-mutants-clean-"));
  repo = path.join(tmp, "repo");
  target = path.join(repo, "target");
  folder = path.join(target, "mutants");
  outside = path.join(tmp, "outside");
  mkdirSync(repo);
  put(path.join(outside, "kept.txt"));
  git("init", "--quiet");
  put(path.join(repo, ".gitignore"), "target/\n");
  put(path.join(repo, "src", "lib.rs"), "fn f() {}\n");
  git("add", ".");
  git("-c", "user.name=test", "-c", "user.email=test@example.com", "commit", "--quiet", "-m", "x");
});

afterEach(() => {
  rmSync(tmp, { recursive: true, force: true });
});

describe("clean", () => {
  it("removes a run's worktrees and the folder, and nothing else in target", () => {
    worktree("target/mutants/tree-1");
    worktree("target/mutants/tree-2");
    worktree("target/worktrees/other");
    // What a run leaves: a build, a mutant in the source, a seed file, and its reports.
    put(path.join(folder, "tree-1", "target", "mutants", "build.bin"));
    put(path.join(folder, "tree-1", "src", "lib.rs"), "fn f() { /* mutated */ }\n");
    put(path.join(folder, "tree-2", "src", "lib.proptest-regressions"));
    put(path.join(folder, "core", "shard-1.log"));
    put(path.join(target, "debug", "kept.bin"));
    put(path.join(target, "dod-run", "kept.db"));

    const { status, message } = clean(repo);

    assert.equal(message, "Removed target/mutants and its 2 worktrees.");
    assert.equal(status, 0);
    assert.ok(!existsSync(folder));
    assert.deepEqual(worktrees(), ["target/worktrees/other"]);
    assert.ok(existsSync(path.join(target, "worktrees", "other", "src", "lib.rs")));
    assert.ok(existsSync(path.join(target, "debug", "kept.bin")));
    assert.ok(existsSync(path.join(target, "dod-run", "kept.db")));
  });

  it("removes the folder when it holds reports and no worktree", () => {
    put(path.join(folder, "auth-vault", "shard-1.log"));

    const { status, message } = clean(repo);

    assert.equal(message, "Removed target/mutants; it held no worktree.");
    assert.equal(status, 0);
    assert.ok(!existsSync(folder));
    assert.ok(existsSync(target));
  });

  it("has nothing to clean without the folder, or without target", () => {
    assert.deepEqual(clean(repo), { status: 0, message: MISSING });
    mkdirSync(target);
    assert.deepEqual(clean(repo), { status: 0, message: MISSING });
  });

  it("forgets a worktree whose folder was deleted by hand", () => {
    worktree("target/mutants/tree-1");
    rmSync(folder, { recursive: true, force: true });
    assert.deepEqual(worktrees(), ["target/mutants/tree-1"]);

    const { status, message } = clean(repo);

    assert.equal(message, "Nothing left in target/mutants; git forgot its 1 worktree.");
    assert.equal(status, 0);
    assert.deepEqual(worktrees(), []);
  });

  it("removes a tree folder git doesn't know with the rest", () => {
    put(path.join(folder, "tree-3", "src", "lib.rs"));

    const { status } = clean(repo);

    assert.equal(status, 0);
    assert.ok(!existsSync(folder));
  });

  it("refuses when the folder holds a worktree no run made, and removes nothing", () => {
    worktree("target/mutants/tree-1");
    worktree("target/mutants/mine");
    put(path.join(folder, "core", "shard-1.log"));

    const { status, message } = clean(repo);

    assert.equal(
      message,
      "target/mutants/mine is a worktree no mutation run made: mutants-clean leaves it and everything else. Move it or remove it with `git worktree remove`, then run just mutants-clean again.",
    );
    assert.equal(status, 1);
    assert.deepEqual(worktrees(), ["target/mutants/mine", "target/mutants/tree-1"]);
    assert.ok(existsSync(path.join(folder, "core", "shard-1.log")));
  });

  it("refuses a worktree nested deeper in the folder too", () => {
    worktree("target/mutants/core/tree-1");

    const { status, message } = clean(repo);

    assert.match(message, /^target\/mutants\/core\/tree-1 is a worktree no mutation run made: /);
    assert.equal(status, 1);
    assert.ok(existsSync(path.join(folder, "core", "tree-1", "src", "lib.rs")));
  });

  it("refuses a target or a folder that is a link, without following it", () => {
    link(outside, folder);

    const linkedFolder = clean(repo);

    assert.equal(
      linkedFolder.message,
      "target/mutants is a link or junction: mutants-clean doesn't follow it. Remove the link itself, then run just mutants-clean again.",
    );
    assert.equal(linkedFolder.status, 1);
    assert.ok(existsSync(path.join(outside, "kept.txt")));

    rmSync(target, { recursive: true, force: true });
    put(path.join(outside, "mutants", "core", "kept.log"));
    link(outside, target);

    const linkedTarget = clean(repo);

    assert.equal(
      linkedTarget.message,
      "target is a link or junction: mutants-clean doesn't follow it. Remove the link itself, then run just mutants-clean again.",
    );
    assert.equal(linkedTarget.status, 1);
    assert.ok(existsSync(path.join(outside, "mutants", "core", "kept.log")));
  });

  it("removes a link inside the folder as an entry, never what it points to", () => {
    put(path.join(folder, "core", "shard-1.log"));
    link(outside, path.join(folder, "core", "linked"));

    const { status } = clean(repo);

    assert.equal(status, 0);
    assert.ok(!existsSync(folder));
    assert.ok(existsSync(path.join(outside, "kept.txt")));
  });

  it("stops at a worktree git can't remove, with git's reason, and keeps the folder", () => {
    worktree("target/mutants/tree-1");
    worktree("target/mutants/tree-2");
    git("worktree", "lock", "target/mutants/tree-1");

    const { status, message } = clean(repo);

    assert.match(
      message,
      /^mutants-clean couldn't remove the worktree target\/mutants\/tree-1: .*lock.*\. Nothing else was removed after it; run just mutants-clean again once that's settled\.$/s,
    );
    assert.equal(status, 1);
    assert.ok(existsSync(path.join(folder, "tree-1", "src", "lib.rs")));
    assert.ok(existsSync(path.join(folder, "tree-2", "src", "lib.rs")));
  });
});

describe("the command line", () => {
  it("cleans the repository the script sits in, and says so", () => {
    for (const name of ["mutants-clean.mjs", "mutants.mjs", "coverage-gates.mjs"]) {
      put(path.join(repo, "scripts", name));
      copyFileSync(path.join(SCRIPTS, name), path.join(repo, "scripts", name));
    }
    worktree("target/mutants/tree-1");
    put(path.join(target, "debug", "kept.bin"));

    const run = spawnSync(process.execPath, [path.join(repo, "scripts", "mutants-clean.mjs")], {
      cwd: tmp,
      encoding: "utf8",
    });

    assert.equal(run.stdout, "Removed target/mutants and its 1 worktree.\n");
    assert.equal(run.status, 0, run.stderr);
    assert.ok(!existsSync(folder));
    assert.ok(existsSync(path.join(target, "debug", "kept.bin")));
  });

  it("takes no arguments", () => {
    const run = spawnSync(process.execPath, [path.join(SCRIPTS, "mutants-clean.mjs"), "core"], {
      encoding: "utf8",
    });

    assert.equal(run.stderr, "usage: node scripts/mutants-clean.mjs\n");
    assert.equal(run.status, 2);
  });
});

describe("the mutants-clean recipe", () => {
  it("runs the script alone", () => {
    const justfile = readFileSync(path.join(SCRIPTS, "..", "justfile"), "utf8");

    assert.deepEqual(recipeBody("mutants-clean", justfile), ["node scripts/mutants-clean.mjs"]);
  });
});
