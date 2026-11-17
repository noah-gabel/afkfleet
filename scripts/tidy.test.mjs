// Tests for tidy.mjs. Run with `just scripts-test`.
//
// Every test builds its own repository in a temporary folder and never touches this one's
// target/: `tidy` gets the temporary folder as its root, and the command-line test runs
// copies of the scripts placed in that folder's scripts/, which find their repository
// from their own location.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  copyFileSync,
  existsSync,
  lstatSync as fsLstat,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  symlinkSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { afterEach, beforeEach, describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { recipeBody } from "./justfile.mjs";
import { tidy, workspaceMembers } from "./tidy.mjs";

const SCRIPTS = path.dirname(fileURLToPath(import.meta.url));

/** What cargo writes into every build folder it creates. */
const CACHE_TAG =
  "Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by cargo.\n";

const MEMBERS = ["fleet-core", "fleet-server", "afkfleet-fuzz"];

/** A mutation clean-up that found nothing, as `clean` answers. */
const NO_MUTANTS = () => ({
  status: 0,
  message: "Nothing to clean: target/mutants doesn't exist.",
});

let tmp;
let repo;
let target;
let outside;

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

/** A cargo build folder at `where`, with the profile folders `profiles`. */
function cargoFolder(where, profiles = ["debug"]) {
  put(path.join(where, "CACHEDIR.TAG"), CACHE_TAG);
  for (const profile of profiles) {
    put(path.join(where, profile, ".cargo-lock"), "");
  }
}

/** A build unit of `crate` in the profile folder `profile`, as cargo lays one out. */
function unit(profile, crate, hash, bytes = 10) {
  const folder = path.join(profile, "build", crate, hash);
  put(path.join(folder, "fingerprint", "invoked.timestamp"), "");
  put(path.join(folder, "out", `lib-${hash}.rlib`), "r".repeat(bytes));
  return folder;
}

/** Runs tidy on the temporary repository with the tests' members and no mutation runs. */
function run(options = {}) {
  return tidy(repo, { members: MEMBERS, mutants: NO_MUTANTS, ...options });
}

beforeEach(() => {
  tmp = mkdtempSync(path.join(tmpdir(), "afkfleet-tidy-"));
  repo = path.join(tmp, "repo");
  target = path.join(repo, "target");
  outside = path.join(tmp, "outside");
  mkdirSync(repo);
  put(path.join(outside, "kept.txt"));
});

afterEach(() => {
  rmSync(tmp, { recursive: true, force: true });
});

describe("tidy", () => {
  it("removes every profile's incremental data and nothing beside it", () => {
    cargoFolder(target, ["debug", "release"]);
    put(path.join(target, "debug", "incremental", "fleet_core-1abc", "s-1", "query-cache.bin"));
    put(path.join(target, "release", "incremental", "fleet_core-2abc", "dep-graph.bin"));
    put(path.join(target, "debug", "afkfleet-server.exe"));
    put(path.join(target, "debug", "examples", "seeds.exe"));
    put(path.join(target, "doc", "index.html"));
    put(path.join(target, "tmp", "incremental", "kept.bin"));

    const { status } = run();

    assert.equal(status, 0);
    assert.ok(!existsSync(path.join(target, "debug", "incremental")));
    assert.ok(!existsSync(path.join(target, "release", "incremental")));
    assert.ok(existsSync(path.join(target, "debug", ".cargo-lock")));
    assert.ok(existsSync(path.join(target, "debug", "afkfleet-server.exe")));
    assert.ok(existsSync(path.join(target, "debug", "examples", "seeds.exe")));
    assert.ok(existsSync(path.join(target, "doc", "index.html")));
    // A folder without cargo's lock file is no profile folder, whatever it holds.
    assert.ok(existsSync(path.join(target, "tmp", "incremental", "kept.bin")));
  });

  it("removes the build units of this workspace's crates and keeps every dependency's", () => {
    cargoFolder(target);
    const debug = path.join(target, "debug");
    const core = unit(debug, "fleet-core", "0123456789abcdef");
    const server = unit(debug, "fleet-server", "fedcba9876543210");
    const serde = unit(debug, "serde", "00112233445566ff");
    // A dependency whose name only starts like a member's.
    const lookalike = unit(debug, "fleet-core-macros", "aaaaaaaaaaaaaaaa");

    const { status } = run();

    assert.equal(status, 0);
    assert.ok(!existsSync(core));
    assert.ok(!existsSync(server));
    assert.ok(!existsSync(path.join(debug, "build", "fleet-core")));
    assert.ok(existsSync(path.join(serde, "out", "lib-00112233445566ff.rlib")));
    assert.ok(existsSync(path.join(serde, "fingerprint", "invoked.timestamp")));
    assert.ok(existsSync(path.join(lookalike, "out", "lib-aaaaaaaaaaaaaaaa.rlib")));
  });

  it("never enters the DoD run's folder or the dev server's, whatever they look like", () => {
    cargoFolder(target);
    for (const kept of ["dod-run", "dev-server"]) {
      const folder = path.join(target, kept);
      // Each dressed up as a build folder and as a profile folder at once.
      cargoFolder(folder);
      put(path.join(folder, ".cargo-lock"), "");
      put(path.join(folder, "incremental", "kept.bin"));
      put(path.join(folder, "afkfleet-1-2_0.profraw"));
      unit(folder, "fleet-core", "0123456789abcdef");
      put(path.join(folder, "debug", "incremental", "kept.bin"));
      unit(path.join(folder, "debug"), "fleet-server", "fedcba9876543210");
    }
    put(path.join(target, "dev-server", "vault-key-1"), "key");
    put(path.join(target, "dev-server", "afkfleet.db"), "db");
    put(path.join(target, "dod-run", "afkfleet.db"), "db");
    const entered = [];

    const { status } = run({
      io: {
        readdirSync(where) {
          entered.push(where);
          return readdirSync(where);
        },
      },
    });

    assert.equal(status, 0);
    for (const kept of ["dod-run", "dev-server"]) {
      const folder = path.join(target, kept);
      assert.ok(existsSync(path.join(folder, "incremental", "kept.bin")));
      assert.ok(existsSync(path.join(folder, "afkfleet-1-2_0.profraw")));
      assert.ok(existsSync(path.join(folder, "afkfleet.db")));
      assert.ok(existsSync(path.join(folder, "debug", "incremental", "kept.bin")));
      assert.ok(
        existsSync(path.join(folder, "build", "fleet-core", "0123456789abcdef", "fingerprint")),
      );
      assert.ok(
        existsSync(path.join(folder, "debug", "build", "fleet-server", "fedcba9876543210")),
      );
      // Not even listed.
      assert.deepEqual(
        entered.filter((where) => where.startsWith(folder)),
        [],
      );
    }
    assert.equal(readFileSync(path.join(target, "dev-server", "vault-key-1"), "utf8"), "key");
  });

  it("tidies a build folder inside a build folder: coverage's, and a target triple's", () => {
    cargoFolder(target);
    const cov = path.join(target, "llvm-cov-target");
    cargoFolder(cov);
    put(path.join(cov, "afkfleet-100-200_0.profraw"));
    put(path.join(cov, "afkfleet.profdata"));
    put(path.join(cov, "afkfleet-profraw-list"));
    put(path.join(cov, ".rustc_info.json"));
    put(path.join(cov, "debug", "incremental", "x.bin"));
    const covCore = unit(path.join(cov, "debug"), "fleet-core", "0123456789abcdef");
    const covSerde = unit(path.join(cov, "debug"), "serde", "00112233445566ff");
    const fuzz = path.join(repo, "fuzz", "target");
    cargoFolder(fuzz, ["release"]);
    const triple = path.join(fuzz, "x86_64-pc-windows-msvc");
    cargoFolder(triple, ["release"]);
    const fuzzOwn = unit(path.join(triple, "release"), "afkfleet-fuzz", "1111111111111111");
    const fuzzDep = unit(path.join(triple, "release"), "libfuzzer-sys", "2222222222222222");
    put(path.join(triple, "release", "chat_sender.exe"));
    put(path.join(triple, "release", "incremental", "y.bin"));

    const { status } = run();

    assert.equal(status, 0);
    assert.deepEqual(readdirSync(cov).sort(), [".rustc_info.json", "CACHEDIR.TAG", "debug"]);
    assert.ok(!existsSync(path.join(cov, "debug", "incremental")));
    assert.ok(!existsSync(covCore));
    assert.ok(existsSync(covSerde));
    assert.ok(!existsSync(fuzzOwn));
    assert.ok(existsSync(fuzzDep));
    assert.ok(!existsSync(path.join(triple, "release", "incremental")));
    assert.ok(existsSync(path.join(triple, "release", "chat_sender.exe")));
  });

  it("removes the coverage reports in target by their names, and no other file", () => {
    cargoFolder(target);
    for (const name of ["coverage.json", "coverage-desktop.json", "cov-fleet-core.json"]) {
      put(path.join(target, name), "{}");
    }
    for (const name of ["coverage.txt", "recovery.json", "cover.json", "sqlx-prepare.db"]) {
      put(path.join(target, name));
    }
    put(path.join(target, "cov-folder.json", "kept.txt"));
    // A profraw file outside a coverage build folder's top is nobody's to remove.
    put(path.join(target, "tmp", "afkfleet-1-2_0.profraw"));

    const { status } = run();

    assert.equal(status, 0);
    assert.deepEqual(readdirSync(target).sort(), [
      "CACHEDIR.TAG",
      "cov-folder.json",
      "cover.json",
      "coverage.txt",
      "debug",
      "recovery.json",
      "sqlx-prepare.db",
      "tmp",
    ]);
    assert.ok(existsSync(path.join(target, "tmp", "afkfleet-1-2_0.profraw")));
  });

  it("says what it freed, by kind, and what it left alone", () => {
    cargoFolder(target);
    const debug = path.join(target, "debug");
    put(path.join(debug, "incremental", "a", "big.bin"), "i".repeat(3 * 2 ** 20));
    unit(debug, "fleet-core", "0123456789abcdef", 2 ** 20);
    unit(debug, "fleet-server", "fedcba9876543210", 2 ** 20);
    put(path.join(target, "coverage.json"), "c".repeat(2 ** 19));

    const { status, freed, lines } = run();

    assert.equal(status, 0);
    assert.deepEqual(freed, {
      incremental: 3 * 2 ** 20,
      units: 2 * 2 ** 20,
      coverage: 2 ** 19,
      mutants: 0,
    });
    assert.deepEqual(lines, [
      "just tidy freed 5.5 MiB:",
      "  3.0 MiB  incremental compilation data",
      "  2.0 MiB  build output of this workspace's own crates (2 units)",
      "  0.5 MiB  coverage data and reports",
      "  0.0 MiB  mutation runs: Nothing to clean: target/mutants doesn't exist.",
      "It kept every dependency's build, the finished binaries, and everything that is no build output.",
    ]);
  });

  it("counts in GiB from one GiB on", () => {
    cargoFolder(target);
    // A sparse size would need a real 1 GiB file, so the sizes come from the test's own lstat.
    put(path.join(target, "debug", "incremental", "big.bin"));
    const big = path.join(target, "debug", "incremental", "big.bin");

    const { lines } = run({
      io: {
        lstatSync(where) {
          const stat = fsLstat(where);
          if (where === big) {
            return Object.assign(Object.create(Object.getPrototypeOf(stat)), stat, {
              size: 3 * 2 ** 29,
            });
          }
          return stat;
        },
      },
    });

    assert.equal(lines[0], "just tidy freed 1.5 GiB:");
    assert.equal(lines[1], "  1.5 GiB  incremental compilation data");
  });

  it("only measures with dryRun, and removes nothing", () => {
    cargoFolder(target);
    const debug = path.join(target, "debug");
    put(path.join(debug, "incremental", "a.bin"), "i".repeat(2 ** 20));
    const core = unit(debug, "fleet-core", "0123456789abcdef", 2 ** 20);
    put(path.join(target, "coverage.json"), "{}");
    let asked = 0;

    const { status, freed, lines } = run({
      dryRun: true,
      mutants: () => {
        asked += 1;
        return NO_MUTANTS();
      },
    });

    assert.equal(status, 0);
    assert.equal(asked, 0);
    assert.equal(freed.incremental, 2 ** 20);
    assert.equal(freed.units, 2 ** 20);
    assert.equal(lines[0], "just tidy would free 2.0 MiB:");
    assert.equal(lines[4], "  mutation runs aren't measured: just mutants-clean removes them.");
    assert.ok(existsSync(path.join(debug, "incremental", "a.bin")));
    assert.ok(existsSync(path.join(core, "fingerprint", "invoked.timestamp")));
    assert.ok(existsSync(path.join(target, "coverage.json")));
  });

  it("has nothing to do without a target folder, or in one cargo didn't make", () => {
    const missing = run();

    assert.equal(missing.status, 0);
    assert.equal(missing.lines[0], "just tidy freed 0.0 MiB:");

    put(path.join(target, "debug", ".cargo-lock"), "");
    put(path.join(target, "debug", "incremental", "kept.bin"));
    put(path.join(target, "coverage.json"), "{}");
    // A tag another tool wrote isn't cargo's.
    put(path.join(target, "CACHEDIR.TAG"), "Signature: something else\n");

    const foreign = run();

    assert.equal(foreign.status, 0);
    assert.equal(foreign.lines[0], "just tidy freed 0.0 MiB:");
    assert.ok(existsSync(path.join(target, "debug", "incremental", "kept.bin")));
    assert.ok(existsSync(path.join(target, "coverage.json")));
  });

  it("refuses a target that is a link, without following it", () => {
    cargoFolder(outside);
    put(path.join(outside, "debug", "incremental", "kept.bin"));
    link(outside, target);

    const { status, lines } = run();

    assert.equal(status, 1);
    assert.deepEqual(lines, [
      "target is a link or junction: tidy doesn't follow it. Remove the link itself, then run just tidy again.",
    ]);
    assert.ok(existsSync(path.join(outside, "debug", "incremental", "kept.bin")));
  });

  it("leaves a linked folder where it is, says so, and tidies the rest", () => {
    cargoFolder(target, ["debug", "release"]);
    const debug = path.join(target, "debug");
    put(path.join(outside, "inc", "kept.bin"));
    link(path.join(outside, "inc"), path.join(debug, "incremental"));
    put(path.join(outside, "core", "0123456789abcdef", "out", "kept.rlib"));
    mkdirSync(path.join(debug, "build"), { recursive: true });
    link(path.join(outside, "core"), path.join(debug, "build", "fleet-core"));
    const server = unit(debug, "fleet-server", "fedcba9876543210");
    put(path.join(target, "release", "incremental", "x.bin"));

    const { status, lines } = run();

    assert.equal(status, 1);
    assert.ok(existsSync(path.join(outside, "inc", "kept.bin")));
    assert.ok(existsSync(path.join(outside, "core", "0123456789abcdef", "out", "kept.rlib")));
    assert.ok(existsSync(path.join(debug, "incremental")));
    assert.ok(!existsSync(server));
    assert.ok(!existsSync(path.join(target, "release", "incremental")));
    assert.deepEqual(lines.slice(-3), [
      "Left where they are, 2 links or junctions tidy doesn't follow:",
      "  target/debug/incremental",
      "  target/debug/build/fleet-core",
    ]);
  });

  it("removes a link inside a folder it removes as an entry, never what it points to", () => {
    cargoFolder(target);
    put(path.join(target, "debug", "incremental", "a", "x.bin"));
    link(outside, path.join(target, "debug", "incremental", "a", "linked"));

    const { status } = run();

    assert.equal(status, 0);
    assert.ok(!existsSync(path.join(target, "debug", "incremental")));
    assert.ok(existsSync(path.join(outside, "kept.txt")));
  });

  it("leaves a unit whose file is held open with its fingerprint, and goes on", () => {
    cargoFolder(target);
    const debug = path.join(target, "debug");
    const held = unit(debug, "fleet-server", "fedcba9876543210");
    const heldFile = path.join(held, "out", "lib-fedcba9876543210.rlib");
    const other = unit(debug, "fleet-server", "0000000000000000");
    put(path.join(debug, "incremental", "a.bin"));

    const { status, lines } = run({
      io: {
        unlinkSync(where) {
          if (where === heldFile) {
            throw Object.assign(new Error("resource busy or locked"), {
              code: "EBUSY",
              path: where,
            });
          }
          unlinkSync(where);
        },
      },
    });

    assert.equal(status, 1);
    assert.ok(existsSync(heldFile));
    // The fingerprint stays, so cargo still takes the unit for built.
    assert.ok(existsSync(path.join(held, "fingerprint", "invoked.timestamp")));
    assert.ok(!existsSync(other));
    assert.ok(!existsSync(path.join(debug, "incremental")));
    assert.deepEqual(lines.slice(-2), [
      "Left 1 entry something holds open (a build, a running server, agent or app, an editor); close it and run just tidy again:",
      "  target/debug/build/fleet-server/fedcba9876543210/out/lib-fedcba9876543210.rlib (EBUSY)",
    ]);
  });

  it("passes on what the mutation runs' clean-up says, and its refusal", () => {
    cargoFolder(target);
    put(path.join(target, "debug", "incremental", "a.bin"));
    put(path.join(target, "mutants", "core", "shard-1.log"), "m".repeat(2 ** 20));
    let root;

    const removed = run({
      mutants: (where) => {
        root = where;
        rmSync(path.join(target, "mutants"), { recursive: true });
        return { status: 0, message: "Removed target/mutants; it held no worktree." };
      },
    });

    assert.equal(root, repo);
    assert.equal(removed.status, 0);
    assert.equal(removed.freed.mutants, 2 ** 20);
    assert.equal(
      removed.lines[4],
      "  1.0 MiB  mutation runs: Removed target/mutants; it held no worktree.",
    );

    put(path.join(target, "mutants", "core", "shard-1.log"), "m".repeat(2 ** 20));
    put(path.join(target, "debug", "incremental", "b.bin"));

    const refused = run({
      mutants: () => ({ status: 1, message: "target/mutants/mine is a worktree no run made." }),
    });

    assert.equal(refused.status, 1);
    assert.equal(refused.freed.mutants, 0);
    assert.equal(
      refused.lines[4],
      "  0.0 MiB  mutation runs: target/mutants/mine is a worktree no run made.",
    );
    assert.ok(!existsSync(path.join(target, "debug", "incremental")));
  });
});

describe("workspaceMembers", () => {
  it("reads the members' package names from the manifests, and the fuzz crate's", () => {
    put(
      path.join(repo, "Cargo.toml"),
      '[workspace]\nresolver = "3"\nmembers = ["crates/*", "apps/desktop/src-tauri"]\nexclude = ["spikes"]\n\n[workspace.package]\nversion = "0.1.0"\n',
    );
    put(
      path.join(repo, "crates", "fleet-core", "Cargo.toml"),
      '[package]\nname = "fleet-core"\nversion.workspace = true\n\n[dependencies]\nname = "not-this"\n',
    );
    put(
      path.join(repo, "crates", "fleet-mc", "Cargo.toml"),
      '# a comment\n[package]\ndescription = "x"\nname="fleet-mc"\n',
    );
    put(path.join(repo, "crates", "notes.md"));
    put(path.join(repo, "crates", "empty", "README.md"));
    put(
      path.join(repo, "apps", "desktop", "src-tauri", "Cargo.toml"),
      '[package]\nname = "afkfleet-desktop"\n',
    );
    put(
      path.join(repo, "fuzz", "Cargo.toml"),
      '[package]\nname = "afkfleet-fuzz"\n\n[workspace]\n',
    );

    assert.deepEqual(workspaceMembers(repo), [
      "afkfleet-desktop",
      "afkfleet-fuzz",
      "fleet-core",
      "fleet-mc",
    ]);
  });

  it("throws when a manifest names no package, or the workspace no members", () => {
    put(path.join(repo, "Cargo.toml"), '[workspace]\nmembers = ["crates/*"]\n');
    put(path.join(repo, "crates", "broken", "Cargo.toml"), '[lib]\nname = "x"\n');

    assert.throws(() => workspaceMembers(repo), /crates\/broken\/Cargo.toml names no package/);

    put(path.join(repo, "Cargo.toml"), "[workspace]\n");

    assert.throws(() => workspaceMembers(repo), /Cargo.toml lists no workspace members/);
  });

  it("finds this repository's crates", () => {
    const members = workspaceMembers(path.join(SCRIPTS, ".."));
    const crates = readdirSync(path.join(SCRIPTS, "..", "crates"));

    for (const crate of crates) {
      assert.ok(members.includes(crate), `${crate} is missing`);
    }
    assert.ok(members.includes("afkfleet-desktop"));
    assert.ok(members.includes("afkfleet-fuzz"));
    assert.equal(members.length, crates.length + 2);
  });
});

describe("the command line", () => {
  function install() {
    for (const name of ["tidy.mjs", "mutants-clean.mjs", "mutants.mjs", "coverage-gates.mjs"]) {
      put(path.join(repo, "scripts", name));
      copyFileSync(path.join(SCRIPTS, name), path.join(repo, "scripts", name));
    }
    put(path.join(repo, "Cargo.toml"), '[workspace]\nmembers = ["crates/*"]\n');
    put(path.join(repo, "crates", "fleet-core", "Cargo.toml"), '[package]\nname = "fleet-core"\n');
    const git = spawnSync("git", ["init", "--quiet"], { cwd: repo, encoding: "utf8" });
    assert.equal(git.status, 0, git.stderr);
  }

  it("tidies the repository the script sits in, and says so", () => {
    install();
    cargoFolder(target);
    const core = unit(path.join(target, "debug"), "fleet-core", "0123456789abcdef");
    const serde = unit(path.join(target, "debug"), "serde", "00112233445566ff");

    const run = spawnSync(process.execPath, [path.join(repo, "scripts", "tidy.mjs")], {
      cwd: tmp,
      encoding: "utf8",
    });

    assert.match(run.stdout, /^just tidy freed 0\.0 MiB:\n/);
    assert.match(run.stdout, /\(1 unit\)\n/);
    assert.equal(run.status, 0, run.stderr);
    assert.ok(!existsSync(core));
    assert.ok(existsSync(serde));
  });

  it("measures alone with --dry-run", () => {
    install();
    cargoFolder(target);
    const core = unit(path.join(target, "debug"), "fleet-core", "0123456789abcdef");

    const run = spawnSync(process.execPath, [path.join(repo, "scripts", "tidy.mjs"), "--dry-run"], {
      cwd: tmp,
      encoding: "utf8",
    });

    assert.match(run.stdout, /^just tidy would free 0\.0 MiB:\n/);
    assert.equal(run.status, 0, run.stderr);
    assert.ok(existsSync(core));
  });

  it("takes no other argument", () => {
    const run = spawnSync(process.execPath, [path.join(SCRIPTS, "tidy.mjs"), "target"], {
      encoding: "utf8",
    });

    assert.equal(run.stderr, "usage: node scripts/tidy.mjs [--dry-run]\n");
    assert.equal(run.status, 2);
  });
});

describe("the tidy recipe", () => {
  it("runs the script alone, with its arguments", () => {
    const justfile = readFileSync(path.join(SCRIPTS, "..", "justfile"), "utf8");

    assert.deepEqual(recipeBody("tidy", justfile), ["node scripts/tidy.mjs {{ args }}"]);
  });
});
