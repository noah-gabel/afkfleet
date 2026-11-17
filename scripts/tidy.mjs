// Gives back the disk space that builds take and never return: `just tidy`.
//
//   node scripts/tidy.mjs [--dry-run]
//
// Cargo never removes what it built. Measured on the development PC after one group of
// work (2026-11-17, ADR-0039): target/debug held 121 GiB, of which 48 GiB were incremental
// compilation data and 53 GiB the build units of this workspace's own crates, six variants
// of every test binary, one for each way a recipe builds them. Coverage took 6.5 GiB, the
// fuzz targets' build 2.8 GiB, and every dependency's build together 16 GiB.
//
// It removes only what the next build writes again, in the repository it sits in, found
// from its own location:
// - `incremental/` in every profile folder (target/debug, target/release, …);
// - `build/<crate>/` there for every crate of this workspace: its build units. A
//   dependency's units stay, since they cost the most to build again and grow the least;
// - the same two in a build folder inside a build folder: cargo-llvm-cov's
//   (target/llvm-cov-target) and a target triple's (fuzz/target/<triple>), and there the
//   coverage data at its top (`*.profraw`, `*.profdata`, the profraw list);
// - the coverage reports at target's top, by name: `coverage.json`, `coverage-<name>.json`
//   and `cov-<name>.json`;
// - what the mutation runs keep, through mutants-clean.mjs and under its rules.
// The build folders are target/ and fuzz/target/. It never removes one as a whole, nor a
// finished binary, the docs, a log or anything else that is no build output.
//
// What it's strict about:
// - **target/dod-run and target/dev-server are never entered,** not even listed: the first
//   holds the DoD checks' real account, the second the dev vault key beside the dev
//   database. Nothing in them is build output.
// - A folder counts as cargo's only by cargo's own marks: its `CACHEDIR.TAG` with the tag's
//   signature for a build folder, its `.cargo-lock` for a profile folder. Anything else is
//   left alone, whatever it's called.
// - A target/ or fuzz/target/ that is a link or junction is refused. A linked folder where
//   it would remove one is left and named; a link inside a folder it removes is removed as
//   an entry, never followed.
// - A file something holds open (a build, a running server, agent or app) is left and
//   named, its build unit keeps its fingerprint, and the rest is tidied.
// `--dry-run` only measures. Exits with 0 when everything went or nothing was there, 1 when
// it refused or left something, 2 on a usage error. Tests: scripts/tidy.test.mjs.
import * as fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

import { clean } from "./mutants-clean.mjs";

/** The build folders, from the repository's root. */
const BUILD_FOLDERS = ["target", path.join("fuzz", "target")];

/** Folders in target/ that are never entered: the DoD run's and the dev server's. */
const NEVER_ENTERED = new Set(["dod-run", "dev-server"]);

/** How a cache directory tag starts; cargo writes one into every build folder. */
const CACHE_TAG = "Signature: 8a477f597d28d172789f06886806bc55";

/** How deep a build folder may sit inside another: llvm-cov's, a target triple's. */
const NESTED = 2;

/** Coverage data at a build folder's top, as cargo-llvm-cov leaves it. */
const COVERAGE_DATA = /(\.profraw|\.profdata|-profraw-list)$/;

/** A coverage report at target's top: coverage.json, coverage-<name>.json, cov-<name>.json. */
const COVERAGE_REPORT = /^(coverage|coverage-[\w.-]+|cov-[\w.-]+)\.json$/;

/** How many left entries are named before the rest is counted. */
const NAMED_AT_MOST = 10;

/** `bytes` as text: MiB below one GiB, GiB from there. */
function size(bytes) {
  return bytes < 2 ** 30
    ? `${(bytes / 2 ** 20).toFixed(1)} MiB`
    : `${(bytes / 2 ** 30).toFixed(1)} GiB`;
}

/** `count` of a thing, in words. */
function counted(count, one, many) {
  return count === 1 ? `1 ${one}` : `${count} ${many}`;
}

/** The lines of a manifest's section `[name]`, without comments' lines. */
function section(text, name) {
  const lines = [];
  let inside = false;
  for (const line of text.split(/\r?\n/)) {
    const header = /^\s*\[([^\]]+)\]/.exec(line);
    if (header) {
      inside = header[1].trim() === name;
    } else if (inside && !/^\s*#/.test(line)) {
      lines.push(line);
    }
  }
  return lines;
}

/** The package's name in the manifest at `file`, named `shown` in an error. */
function packageName(file, shown) {
  for (const line of section(fs.readFileSync(file, "utf8"), "package")) {
    const name = /^\s*name\s*=\s*"([^"]+)"/.exec(line);
    if (name) {
      return name[1];
    }
  }
  throw new Error(`${shown} names no package`);
}

/**
 * The names of this workspace's packages, sorted: every member the root manifest lists
 * (a folder, or `folder/*` for each folder in it with a manifest), and the fuzz crate,
 * a workspace of its own that builds them too. Read from the manifests, so it needs no
 * cargo.
 */
export function workspaceMembers(repo) {
  const workspace = section(fs.readFileSync(path.join(repo, "Cargo.toml"), "utf8"), "workspace");
  const listed = /(?:^|\n)\s*members\s*=\s*\[([^\]]*)\]/.exec(workspace.join("\n"));
  const entries = listed ? [...listed[1].matchAll(/"([^"]+)"/g)].map((match) => match[1]) : [];
  if (entries.length === 0) {
    throw new Error("Cargo.toml lists no workspace members");
  }
  const folders = [];
  for (const entry of entries) {
    if (entry.endsWith("/*")) {
      const parent = entry.slice(0, -2);
      for (const name of fs.readdirSync(path.join(repo, parent))) {
        if (fs.existsSync(path.join(repo, parent, name, "Cargo.toml"))) {
          folders.push(`${parent}/${name}`);
        }
      }
    } else {
      folders.push(entry);
    }
  }
  if (fs.existsSync(path.join(repo, "fuzz", "Cargo.toml"))) {
    folders.push("fuzz");
  }
  const names = folders.map((folder) =>
    packageName(path.join(repo, folder, "Cargo.toml"), `${folder}/Cargo.toml`),
  );
  return [...new Set(names)].sort();
}

/**
 * Tidies the repository `repo`. `members` are its packages' names, `dryRun` only measures,
 * `mutants` is the mutation runs' clean-up, and `io` replaces file-system calls in tests.
 * Returns `{ status, freed, lines }`: the exit code, the bytes freed by kind, and the
 * lines to print.
 */
export function tidy(repo, { members, dryRun = false, mutants = clean, io = {} }) {
  const { lstatSync, readdirSync, unlinkSync, rmdirSync } = { ...fs, ...io };
  const root = path.resolve(repo);
  const freed = { incremental: 0, units: 0, coverage: 0, mutants: 0 };
  /** Links where a folder to remove was expected, as messages name them. */
  const links = [];
  /** Entries that couldn't be removed, each `{ named, code }`. */
  const held = [];
  let units = 0;

  /** `where` from the repository's root, with `/`. */
  const named = (where) => path.relative(root, where).split(path.sep).join("/");

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

  /** Runs one removal; an entry it can't remove is noted and left. */
  function attempt(where, removal) {
    if (dryRun) {
      return true;
    }
    try {
      removal();
      return true;
    } catch (error) {
      held.push({ named: named(where), code: error.code ?? "error" });
      return false;
    }
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

  /**
   * Removes `where` and everything under it, links as entries, never followed, counting
   * its files' bytes as `kind`. A folder's `last` entry goes after the others, and only
   * when they all went. Returns whether everything went.
   */
  function remove(where, kind, last = null) {
    const stat = lstatSync(where);
    if (stat.isSymbolicLink()) {
      return attempt(where, () => removeLink(where));
    }
    if (!stat.isDirectory()) {
      if (!attempt(where, () => unlinkSync(where))) {
        return false;
      }
      freed[kind] += stat.size;
      return true;
    }
    let all = true;
    const names = readdirSync(where).sort();
    for (const name of names.filter((name) => name !== last)) {
      all = remove(path.join(where, name), kind) && all;
    }
    if (all && last !== null && names.includes(last)) {
      all = remove(path.join(where, last), kind);
    }
    return all && attempt(where, () => rmdirSync(where));
  }

  /** The folder at `where` to remove or look into: null for none, and for a link, noted. */
  function folder(where) {
    const stat = entry(where);
    if (stat?.isSymbolicLink()) {
      links.push(named(where));
      return null;
    }
    return stat?.isDirectory() ? where : null;
  }

  /** A profile folder: its incremental data, and this workspace's crates' build units. */
  function profile(where) {
    const incremental = folder(path.join(where, "incremental"));
    if (incremental !== null) {
      remove(incremental, "incremental");
    }
    const build = folder(path.join(where, "build"));
    if (build === null) {
      return;
    }
    for (const member of members) {
      const crate = folder(path.join(build, member));
      if (crate === null) {
        continue;
      }
      let all = true;
      for (const unit of readdirSync(crate).sort()) {
        // The fingerprint goes last, so a unit that keeps a held file still counts as built.
        const gone = remove(path.join(crate, unit), "units", "fingerprint");
        units += gone ? 1 : 0;
        all = gone && all;
      }
      if (all) {
        attempt(crate, () => rmdirSync(crate));
      }
    }
  }

  /** Whether `where` holds cargo's own cache directory tag. */
  function cargos(where) {
    try {
      const tag = path.join(where, "CACHEDIR.TAG");
      return lstatSync(tag).isFile() && fs.readFileSync(tag, "utf8").startsWith(CACHE_TAG);
    } catch {
      return false;
    }
  }

  /** A build folder: its profile folders, the build folders in it, the coverage files. */
  function buildFolder(where, depth, reports) {
    for (const name of readdirSync(where).sort()) {
      if (NEVER_ENTERED.has(name)) {
        continue;
      }
      const inside = path.join(where, name);
      const stat = lstatSync(inside);
      if (stat.isFile()) {
        if (COVERAGE_DATA.test(name) || (reports && COVERAGE_REPORT.test(name))) {
          remove(inside, "coverage");
        }
      } else if (stat.isDirectory()) {
        if (entry(path.join(inside, ".cargo-lock"))?.isFile()) {
          profile(inside);
        } else if (depth < NESTED && cargos(inside)) {
          buildFolder(inside, depth + 1, false);
        }
      }
    }
  }

  /** The bytes of every file under `where`, links not followed; 0 when nothing is there. */
  function measure(where) {
    const stat = entry(where);
    if (stat === null || stat.isSymbolicLink()) {
      return 0;
    }
    if (!stat.isDirectory()) {
      return stat.size;
    }
    return readdirSync(where).reduce((sum, name) => sum + measure(path.join(where, name)), 0);
  }

  for (const relative of BUILD_FOLDERS) {
    if (entry(path.join(root, relative))?.isSymbolicLink()) {
      return {
        status: 1,
        freed,
        lines: [
          `${relative.split(path.sep).join("/")} is a link or junction: tidy doesn't follow it. Remove the link itself, then run just tidy again.`,
        ],
      };
    }
  }
  for (const [index, relative] of BUILD_FOLDERS.entries()) {
    const where = path.join(root, relative);
    if (entry(where)?.isDirectory() && cargos(where)) {
      buildFolder(where, 0, index === 0);
    }
  }

  let status = 0;
  let mutantsMessage = "";
  if (!dryRun) {
    const kept = path.join(root, "target", "mutants");
    const before = measure(kept);
    const cleaned = mutants(repo);
    status = cleaned.status === 0 ? 0 : 1;
    freed.mutants = Math.max(0, before - measure(kept));
    mutantsMessage = cleaned.message;
  }

  const total = freed.incremental + freed.units + freed.coverage + freed.mutants;
  const sizes = [freed.incremental, freed.units, freed.coverage, freed.mutants].map(size);
  const width = Math.max(...sizes.map((text) => text.length));
  const [incremental, own, coverage, runs] = sizes.map((text) => text.padStart(width));
  const lines = [
    `just tidy ${dryRun ? "would free" : "freed"} ${size(total)}:`,
    `  ${incremental}  incremental compilation data`,
    `  ${own}  build output of this workspace's own crates (${counted(units, "unit", "units")})`,
    `  ${coverage}  coverage data and reports`,
    dryRun
      ? "  mutation runs aren't measured: just mutants-clean removes them."
      : `  ${runs}  mutation runs: ${mutantsMessage}`,
    "It kept every dependency's build, the finished binaries, and everything that is no build output.",
  ];
  if (links.length > 0) {
    lines.push(
      `Left where they are, ${counted(links.length, "link or junction", "links or junctions")} tidy doesn't follow:`,
      ...links.map((link) => `  ${link}`),
    );
  }
  if (held.length > 0) {
    lines.push(
      `Left ${counted(held.length, "entry", "entries")} something holds open (a build, a running server, agent or app, an editor); close it and run just tidy again:`,
      ...held.slice(0, NAMED_AT_MOST).map(({ named: name, code }) => `  ${name} (${code})`),
    );
    if (held.length > NAMED_AT_MOST) {
      lines.push(`  and ${held.length - NAMED_AT_MOST} more`);
    }
  }
  return { status: links.length > 0 || held.length > 0 ? 1 : status, freed, lines };
}

function main(args) {
  const dryRun = args.length === 1 && args[0] === "--dry-run";
  if (args.length !== 0 && !dryRun) {
    console.error("usage: node scripts/tidy.mjs [--dry-run]");
    return 2;
  }
  const repo = path.join(path.dirname(fileURLToPath(import.meta.url)), "..");
  const { status, lines } = tidy(repo, { members: workspaceMembers(repo), dryRun });
  for (const line of lines) {
    if (status === 0) {
      console.log(line);
    } else {
      console.error(line);
    }
  }
  return status;
}

// Node runs a script by its real path, so the entry check compares with process.argv[1]'s.
if (
  process.argv[1] !== undefined &&
  import.meta.url === pathToFileURL(fs.realpathSync(process.argv[1])).href
) {
  process.exitCode = main(process.argv.slice(2));
}
