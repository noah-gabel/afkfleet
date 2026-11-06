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
  utimesSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import process from "node:process";
import { afterEach, describe, it } from "node:test";

import { entryLines, fingerprint, main } from "./tree-fingerprint.mjs";

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

/**
 * A repository with one commit: a tracked `a.txt`, and a `.gitignore` that
 * ignores `target/`, `node_modules/`, `secrets/` and `*.log`, as the real one
 * does.
 */
function repo() {
  const dir = mkdtempSync(join(tmpdir(), "afkfleet-fingerprint-"));
  made.push(dir);
  git(dir, "init", "--quiet", "--initial-branch=main");
  git(dir, "config", "user.name", "Test");
  git(dir, "config", "user.email", "test@example.com");
  git(dir, "config", "core.autocrlf", "false");
  writeFileSync(join(dir, ".gitignore"), "target/\nnode_modules/\nsecrets/\n*.log\n");
  writeFileSync(join(dir, "a.txt"), "one\n");
  git(dir, "add", ".");
  git(dir, "commit", "--quiet", "-m", "first");
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

describe("fingerprint", () => {
  it("names every part once, in order", () => {
    const dir = repo();

    const names = fingerprint(dir).lines.map((l) => l.split(" ")[0]);

    assert.deepEqual(names, [
      "head",
      "refs",
      "stash",
      "config",
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

    assert.equal(first.length, 8);
    assert.deepEqual(fingerprint(dir).lines, first);
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
  it("lists the untracked by hash and the ignored by size and mtime, never content", () => {
    const dir = repo();
    writeFileSync(join(dir, "new.txt"), "visible\n");
    mkdirSync(join(dir, "secrets"));
    writeFileSync(join(dir, "secrets", "key"), "not-printed\n");
    utimesSync(join(dir, "secrets", "key"), new Date(2_000_000), new Date(2_000_000));

    const lines = entryLines(fingerprint(dir));

    assert.equal(lines.length, 2);
    assert.match(lines[0], /^untracked [0-9a-f]{64} new\.txt$/);
    assert.equal(lines[1], "ignored 12 2000000000000 secrets/key");
    assert.ok(!lines.join("\n").includes("visible"));
    assert.ok(!lines.join("\n").includes("not-printed"));
  });
});

describe("main", () => {
  /** A stream that keeps what's written. */
  function sink() {
    const chunks = [];
    return { write: (text) => chunks.push(text), text: () => chunks.join("") };
  }

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
    assert.match(out.text(), /^untracked [0-9a-f]{64} new\.txt\n$/);
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
