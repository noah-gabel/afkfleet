// Tests for tree-fingerprint.mjs, over a temporary git repository each.
// Run with `just scripts-test`.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  appendFileSync,
  chmodSync,
  mkdirSync,
  mkdtempSync,
  rmSync,
  symlinkSync,
  utimesSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import process from "node:process";
import { afterEach, describe, it } from "node:test";

import { entryLines, fingerprint, main } from "./tree-fingerprint.mjs";

/** The cases that take a folder's permissions away: not on Windows. */
const UNIX =
  process.platform === "win32" ? "Windows can't take a folder's permissions away" : false;

/**
 * The cases that need a folder nobody may read: not on Windows, and not as root, who reads a
 * folder whatever its mode (PR #97's review: the throwaway container ran them as root).
 */
const UNREADABLE =
  UNIX || (process.getuid?.() === 0 ? "root reads a folder whatever its mode" : false);

/** The temporary repositories made so far, removed after each test. */
const made = [];

afterEach(() => {
  for (const dir of made.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

/** Runs git in `dir`; fails the test if git fails. */
function git(dir, ...args) {
  const result = spawnSync("git", args, { cwd: dir, encoding: "utf8" });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout;
}

/** A fresh repository in `dir` with one commit of `file`. */
function init(dir, file = "a.txt") {
  git(dir, "init", "--quiet", "--initial-branch=main");
  git(dir, "config", "user.name", "Test");
  git(dir, "config", "user.email", "test@example.com");
  git(dir, "config", "core.autocrlf", "false");
  writeFileSync(join(dir, file), "one\n");
  git(dir, "add", ".");
  git(dir, "commit", "--quiet", "-m", "first");
}

/**
 * A repository with one commit: a tracked `a.txt`, and a `.gitignore` that
 * ignores `target/`, `node_modules/`, `secrets/` and `*.log`, as the real one
 * does.
 */
function repo() {
  const dir = mkdtempSync(join(tmpdir(), "afkfleet-fingerprint-"));
  made.push(dir);
  writeFileSync(join(dir, ".gitignore"), "target/\nnode_modules/\nsecrets/\n*.log\n");
  init(dir);
  return dir;
}

/** The fingerprint's line for `part`. */
function line(dir, part) {
  const found = fingerprint(dir).lines.find((l) => l.startsWith(`${part} `));
  assert.ok(found !== undefined, `no ${part} line`);
  return found;
}

/** Every line but `head`, which a commit changes on purpose. */
function parts(dir) {
  return fingerprint(dir).lines.filter((l) => !l.startsWith("head "));
}

/** A stream that keeps what's written. */
function sink() {
  const chunks = [];
  return { write: (text) => chunks.push(text), text: () => chunks.join("") };
}

/** Runs `body` with `folders` unreadable to everyone, then gives them back. */
function unreadable(folders, body) {
  for (const folder of folders) {
    chmodSync(folder, 0o000);
  }
  try {
    return body();
  } finally {
    for (const folder of folders) {
      chmodSync(folder, 0o755);
    }
  }
}

describe("fingerprint", () => {
  it("names every part once, in order", () => {
    const dir = repo();

    const names = fingerprint(dir).lines.map((l) => l.split(" ")[0]);

    assert.deepEqual(names, [
      "head",
      "refs",
      "stash",
      "config",
      "hooks",
      "stage",
      "index",
      "tracked",
      "untracked",
      "ignored",
    ]);
  });

  it("is the same twice for an unchanged tree", () => {
    const dir = repo();
    writeFileSync(join(dir, "new.txt"), "x\n");
    mkdirSync(join(dir, "secrets"));
    writeFileSync(join(dir, "secrets", "key"), "s\n");

    const first = fingerprint(dir).lines;

    assert.equal(first.length, 10);
    assert.deepEqual(fingerprint(dir).lines, first);
  });

  it("is the same from any folder of the repository", () => {
    const dir = repo();
    mkdirSync(join(dir, "sub", "deeper"), { recursive: true });
    writeFileSync(join(dir, "sub", "deeper", "x.txt"), "x\n");
    writeFileSync(join(dir, "top.txt"), "t\n");
    mkdirSync(join(dir, "secrets"));
    writeFileSync(join(dir, "secrets", "key"), "s\n");
    const out = sink();

    const code = main([], join(dir, "sub", "deeper"), out, sink());

    assert.deepEqual(fingerprint(join(dir, "sub")).lines, fingerprint(dir).lines);
    assert.equal(code, 0);
    assert.equal(out.text(), `${fingerprint(dir).lines.join("\n")}\n`);
  });

  it("sees a second edit to a file that's already modified", () => {
    const dir = repo();
    writeFileSync(join(dir, "a.txt"), "two\n");
    const before = line(dir, "tracked");
    const statusBefore = git(dir, "status", "--short");

    writeFileSync(join(dir, "a.txt"), "three\n");

    assert.notEqual(line(dir, "tracked"), before);
    assert.equal(git(dir, "status", "--short"), statusBefore, "git status alone misses it");
  });

  it("sees a new file inside an untracked folder, and an edit to one", () => {
    const dir = repo();
    mkdirSync(join(dir, "new"));
    writeFileSync(join(dir, "new", "one.txt"), "1\n");
    const before = line(dir, "untracked");
    const statusBefore = git(dir, "status", "--short");

    writeFileSync(join(dir, "new", "two.txt"), "2\n");
    const added = line(dir, "untracked");
    writeFileSync(join(dir, "new", "two.txt"), "22\n");

    assert.notEqual(added, before);
    assert.notEqual(line(dir, "untracked"), added);
    assert.match(added, /^untracked 2 [0-9a-f]{64}$/);
    assert.equal(git(dir, "status", "--short"), statusBefore, "git status alone misses it");
  });

  it("takes a repository of its own inside the tree by its HEAD, never as a file", () => {
    const dir = repo();
    const inner = join(dir, "inner");
    mkdirSync(inner);
    init(inner, "i.txt");
    const empty = join(dir, "empty");
    mkdirSync(empty);
    git(empty, "init", "--quiet");

    const before = fingerprint(dir);
    writeFileSync(join(inner, "i.txt"), "two\n");
    git(inner, "commit", "--quiet", "-am", "second");

    assert.deepEqual(
      before.untracked.map((e) => e.path),
      ["empty/", "inner/"],
    );
    assert.notEqual(
      line(dir, "untracked"),
      before.lines.find((l) => l.startsWith("untracked ")),
    );
  });

  it("takes a worktree inside the tree by its HEAD", () => {
    const dir = repo();
    git(dir, "worktree", "add", "--quiet", "wt");

    const found = fingerprint(dir);

    assert.ok(
      found.untracked.some((e) => e.path === "wt/"),
      JSON.stringify(found.untracked),
    );
  });

  it("sees an index flag: skip-worktree, then assume-unchanged", () => {
    const dir = repo();
    const before = line(dir, "stage");

    git(dir, "update-index", "--skip-worktree", "a.txt");
    const skipped = line(dir, "stage");
    git(dir, "update-index", "--no-skip-worktree", "--assume-unchanged", "a.txt");

    assert.notEqual(skipped, before);
    assert.notEqual(line(dir, "stage"), skipped);
    assert.notEqual(line(dir, "stage"), before);
  });

  it("sees a new hook and an edit to one, which runs at the next commit", () => {
    const dir = repo();
    const hooks = join(dir, ".git", "hooks");
    mkdirSync(hooks, { recursive: true });
    const before = line(dir, "hooks");

    writeFileSync(join(hooks, "pre-commit"), "#!/bin/sh\nexit 0\n");
    const added = line(dir, "hooks");
    writeFileSync(join(hooks, "pre-commit"), "#!/bin/sh\nexit 1\n");

    assert.notEqual(added, before);
    assert.notEqual(line(dir, "hooks"), added, "the same size, other content");
  });

  it("sees a hook made executable", { skip: UNIX }, () => {
    const dir = repo();
    const hook = join(dir, ".git", "hooks", "pre-commit");
    writeFileSync(hook, "#!/bin/sh\nexit 0\n");
    chmodSync(hook, 0o644);
    const before = line(dir, "hooks");

    chmodSync(hook, 0o755);

    assert.notEqual(line(dir, "hooks"), before);
  });

  it("sees an ignored file outside target/ appear, grow and be touched", () => {
    const dir = repo();
    mkdirSync(join(dir, "secrets"));
    const empty = line(dir, "ignored");

    writeFileSync(join(dir, "secrets", "key"), "s\n");
    const added = line(dir, "ignored");
    appendFileSync(join(dir, "secrets", "key"), "more\n");
    const grown = line(dir, "ignored");
    utimesSync(join(dir, "secrets", "key"), new Date(1_000_000), new Date(1_000_000));

    assert.notEqual(added, empty);
    assert.notEqual(grown, added);
    assert.notEqual(line(dir, "ignored"), grown);
  });

  it("never lists anything under target/ or node_modules/, at any depth", () => {
    const dir = repo();
    const before = parts(dir);

    for (const folder of [
      "target",
      "node_modules",
      join("sub", "target"),
      join("sub", "node_modules"),
    ]) {
      mkdirSync(join(dir, folder), { recursive: true });
      writeFileSync(join(dir, folder, "x"), "built\n");
    }
    git(dir, "check-ignore", "--quiet", "sub/target/x");

    assert.match(line(dir, "ignored"), /^ignored 0 [0-9a-f]{64}$/);
    assert.deepEqual(parts(dir), before);
    assert.deepEqual(
      fingerprint(dir).ignored.map((e) => e.path),
      [],
      "nothing under target/ or node_modules/ is even listed",
    );
  });

  it("walks an ignored folder itself, skipping target/ and node_modules/ inside it", () => {
    const dir = repo();
    for (const folder of [
      ["secrets", "deep", "target"],
      ["secrets", "node_modules"],
    ]) {
      mkdirSync(join(dir, ...folder), { recursive: true });
      writeFileSync(join(dir, ...folder, "built"), "x\n");
    }
    writeFileSync(join(dir, "secrets", "deep", "key"), "k\n");
    writeFileSync(join(dir, "secrets", "target"), "a file named target\n");

    assert.deepEqual(
      fingerprint(dir).ignored.map((e) => e.path),
      ["secrets/deep/key"],
    );
  });

  it("never follows a link inside an ignored folder", { skip: UNIX }, () => {
    const dir = repo();
    mkdirSync(join(dir, "elsewhere"));
    writeFileSync(join(dir, "elsewhere", "inside"), "x\n");
    mkdirSync(join(dir, "secrets"));
    symlinkSync(join(dir, "elsewhere"), join(dir, "secrets", "link"));

    assert.deepEqual(
      fingerprint(dir).ignored.map((e) => e.path),
      ["secrets/link"],
    );
  });

  it("counts a file once when git lists its folder as well", () => {
    const dir = repo();
    mkdirSync(join(dir, "logs"));
    writeFileSync(join(dir, "logs", "a.log"), "l\n");

    assert.deepEqual(
      fingerprint(dir).ignored.map((e) => e.path),
      ["logs/a.log"],
    );
  });

  it("never lets git into target/: an unreadable one changes nothing", { skip: UNIX }, () => {
    const dir = repo();
    mkdirSync(join(dir, "target", "dod-run"), { recursive: true });
    writeFileSync(join(dir, "target", "dod-run", "key"), "s\n");
    mkdirSync(join(dir, "sub", "node_modules"), { recursive: true });
    writeFileSync(join(dir, "sub", "keep.txt"), "k\n");
    const before = fingerprint(dir).lines;
    const out = sink();
    const err = sink();

    const code = unreadable([join(dir, "target"), join(dir, "sub", "node_modules")], () =>
      main([], dir, out, err),
    );

    assert.equal(err.text(), "", "git wasn't let into either");
    assert.equal(code, 0);
    assert.equal(out.text(), `${before.join("\n")}\n`);
  });

  it("stops when a folder it must list can't be read, rather than missing it", {
    skip: UNREADABLE,
  }, () => {
    const dir = repo();
    mkdirSync(join(dir, "new"));
    writeFileSync(join(dir, "new", "x.txt"), "x\n");
    mkdirSync(join(dir, "secrets", "inner"), { recursive: true });
    writeFileSync(join(dir, "secrets", "inner", "key"), "s\n");

    for (const folder of [join(dir, "new"), join(dir, "secrets", "inner")]) {
      const err = sink();

      const code = unreadable([folder], () => main([], dir, sink(), err));

      assert.equal(code, 1, folder);
      assert.match(err.text(), /^the tree's fingerprint couldn't be taken: /, folder);
    }
  });

  it("sees a new branch, a stash, a staged change and a config change", () => {
    const dir = repo();
    const refs = line(dir, "refs");
    const config = line(dir, "config");

    git(dir, "branch", "other");
    const branched = line(dir, "refs");
    writeFileSync(join(dir, "a.txt"), "staged\n");
    const index = line(dir, "index");
    git(dir, "add", "a.txt");
    const staged = line(dir, "index");
    const stash = line(dir, "stash");
    git(dir, "stash", "--quiet");
    git(dir, "config", "core.autocrlf", "true");

    assert.notEqual(branched, refs);
    assert.notEqual(staged, index);
    assert.notEqual(line(dir, "stash"), stash);
    assert.notEqual(line(dir, "config"), config);
  });

  it("sees a commit in head", () => {
    const dir = repo();
    const before = line(dir, "head");

    writeFileSync(join(dir, "a.txt"), "two\n");
    git(dir, "commit", "--quiet", "-am", "second");

    assert.notEqual(line(dir, "head"), before);
    assert.match(line(dir, "head"), /^head [0-9a-f]{40} refs\/heads\/main$/);
  });

  it("never opens an ignored file: one nobody may read still counts", {
    skip: process.platform === "win32" ? "Windows can't take a file's read bit away" : false,
  }, () => {
    const dir = repo();
    mkdirSync(join(dir, "secrets"));
    writeFileSync(join(dir, "secrets", "key"), "s\n");
    chmodSync(join(dir, "secrets", "key"), 0o000);

    const found = fingerprint(dir);

    assert.deepEqual(
      found.ignored.map((e) => [e.path, e.size]),
      [["secrets/key", "2"]],
    );
  });
});

describe("entryLines", () => {
  it("lists the untracked by hash, the ignored by size and mtime, never content", () => {
    const dir = repo();
    writeFileSync(join(dir, "new.txt"), "visible\n");
    mkdirSync(join(dir, "secrets"));
    writeFileSync(join(dir, "secrets", "key"), "not-printed\n");
    utimesSync(join(dir, "secrets", "key"), new Date(2_000_000), new Date(2_000_000));

    const lines = entryLines(fingerprint(dir)).filter((l) => !l.startsWith("hook "));

    assert.equal(lines.length, 2);
    assert.match(lines[0], /^untracked [0-9a-f]{64} new\.txt$/);
    assert.equal(lines[1], "ignored 12 2000000000000 secrets/key");
    assert.ok(!lines.join("\n").includes("visible"));
    assert.ok(!lines.join("\n").includes("not-printed"));
  });

  it("lists every hook by hash and mode", () => {
    const dir = repo();
    writeFileSync(join(dir, ".git", "hooks", "pre-commit"), "#!/bin/sh\nexit 0\n");

    const hooks = entryLines(fingerprint(dir)).filter((l) => l.startsWith("hook "));

    assert.ok(
      hooks.some((l) => /^hook [0-9a-f]{64} [0-7]+ pre-commit$/.test(l)),
      hooks.join("\n"),
    );
  });
});

describe("main", () => {
  it("prints the fingerprint's lines and exits 0", () => {
    const dir = repo();
    const out = sink();
    const err = sink();

    const code = main([], dir, out, err);

    assert.equal(code, 0);
    assert.equal(out.text(), `${fingerprint(dir).lines.join("\n")}\n`);
    assert.equal(err.text(), "");
  });

  it("prints the entries with --paths", () => {
    const dir = repo();
    writeFileSync(join(dir, "new.txt"), "x\n");
    const out = sink();

    const code = main(["--paths"], dir, out, sink());

    assert.equal(code, 0);
    assert.match(out.text(), /^untracked [0-9a-f]{64} new\.txt$/m);
  });

  it("exits 1 outside a git repository", () => {
    const dir = mkdtempSync(join(tmpdir(), "afkfleet-fingerprint-"));
    made.push(dir);
    const err = sink();

    const code = main([], dir, sink(), err);

    assert.equal(code, 1);
    assert.match(err.text(), /^the tree's fingerprint couldn't be taken: /);
  });

  it("exits 2 on an unknown argument", () => {
    const dir = repo();
    const err = sink();

    const code = main(["--all"], dir, sink(), err);

    assert.equal(code, 2);
    assert.match(err.text(), /^usage: /);
  });
});
