// Runs the screenshot baselines in Playwright's pinned Linux image (Plan.md P11.11; ADR-0020, U97,
// U118, U125, S130, R226).
//
//   node scripts/shots.mjs compare               (just shots)
//   node scripts/shots.mjs update                (just shots-update)
//   node scripts/shots.mjs design target/design-<name>[/...]
//
// It builds deploy/docker/shots.Dockerfile, whose context is an allowlist, prunes only this image's
// earlier untagged builds, then runs it with no network, no capabilities and two mounts: the
// baselines (read-only to compare, read-write to update), or a design folder for a before or an
// after, and target/shots/results/ for what a failed shot left. A mount's folder that is a link or
// a junction, or sits under one, is refused, so the container writes nowhere but these. The moment
// the run ends, anything it left there but shots, or Playwright's own results, stops it with exit 1,
// listed and kept, before anything on this PC reads them (strays). On CI a failure lists every diff
// in the job's summary. Exits with the run's status, or 1 or 2 saying why.
// Tests: scripts/shots.test.mjs.
import { spawnSync } from "node:child_process";
import { appendFileSync, existsSync, lstatSync, mkdirSync, readdirSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

/** The image's local tag, and its title label, the one thing a prune removes by. */
export const IMAGE = "afkfleet-shots:local";
const TITLE = "org.opencontainers.image.title=afkfleet-shots";

/** The Dockerfile, the baselines and the results, relative to the repository, with `/`. */
export const DOCKERFILE = "deploy/docker/shots.Dockerfile";
export const BASELINES = "apps/desktop/e2e/baselines";
export const RESULTS = "target/shots/results";

/** Where the image keeps the repository's copy. */
const APP = "/app";

/** What each mode asks of Playwright's snapshots. */
const UPDATE = { compare: "none", update: "changed", design: "all" };

const USAGE =
  "usage: node scripts/shots.mjs compare | update | design target/design-<name>[/<folder>]";
const NO_DOCKER = "Docker doesn't answer. Start Docker Desktop, then run just shots again.";

/** `docker build`'s arguments. */
export function buildArgs() {
  return ["build", "--platform", "linux/amd64", "--file", DOCKERFILE, "--tag", IMAGE, "."];
}

/** `docker image prune`'s: only this image's earlier builds, which lost their tag. */
export function pruneArgs() {
  return ["image", "prune", "--force", "--filter", `label=${TITLE}`];
}

/**
 * `docker run`'s arguments for `mode`, on a host `platform` with the repository at `root`: as the
 * host's own user on Linux, so the runner owns what it writes, and the image's `pwuser` elsewhere.
 * Only `HOME` and `CI` reach the container. `design` is the design mode's folder.
 */
export function runArgs({ mode, root, platform, uid, gid, design }) {
  const join = platform === "win32" ? path.win32.join : path.posix.join;
  const mount = (source, target, readonly) => [
    "--mount",
    `type=bind,source=${source},target=${target}${readonly ? ",readonly" : ""}`,
  ];
  const args = [
    "run",
    "--rm",
    "--init",
    "--platform",
    "linux/amd64",
    "--network",
    "none",
    "--user",
    platform === "linux" ? `${uid}:${gid}` : "pwuser",
    "--cap-drop",
    "ALL",
    "--security-opt",
    "no-new-privileges",
    "--shm-size",
    "1g",
    "--env",
    "HOME=/tmp",
    "--env",
    "CI",
  ];
  if (mode === "design") {
    args.push("--env", "AFKFLEET_SHOTS_DESIGN=1", ...mount(design, `${APP}/target/design`, false));
  } else {
    args.push(...mount(join(root, BASELINES), `${APP}/${BASELINES}`, mode === "compare"));
  }
  args.push(
    ...mount(join(root, RESULTS), `${APP}/${RESULTS}`, false),
    IMAGE,
    // Not `pnpm exec`, which would check the image's install again and can't write it.
    "node_modules/.bin/playwright",
    "test",
    "--config",
    "playwright.shots.config.ts",
    `--update-snapshots=${UPDATE[mode]}`,
  );
  return args;
}

/**
 * The design mode's folder for `given`, absolute: only under the repository's `target/design-*`,
 * so a before or an after can't land anywhere else, `target/dod-run/` and `target/dev-server/`
 * above all. Throws saying why.
 */
export function designFolder(root, given) {
  const folder = path.resolve(root, given);
  const parts = path.relative(root, folder).split(path.sep);
  if (parts[0] !== "target" || !(parts[1] ?? "").startsWith("design-") || parts.includes("..")) {
    throw new Error(`a design folder must be under target/design-<name>, not ${given}`);
  }
  return folder;
}

/** The problem with mounting `folder`, under `root`, or null: no link or junction on the way. */
export function mountProblem(root, folder) {
  const relative = path.relative(root, folder);
  if (relative === "" || relative.startsWith("..") || path.isAbsolute(relative)) {
    return `${folder} isn't inside the repository`;
  }
  let at = root;
  for (const part of relative.split(path.sep)) {
    at = path.join(at, part);
    const stat = lstatSync(at, { throwIfNoEntry: false });
    if (stat === undefined) {
      return `${at} doesn't exist`;
    }
    if (stat.isSymbolicLink()) {
      return `${at} is a link or junction: the shots don't mount through one`;
    }
    if (!stat.isDirectory()) {
      return `${at} isn't a folder`;
    }
  }
  return null;
}

/** Every `*-diff.png` under `folder`, relative to `root`, with `/`, in order. */
export function diffs(root, folder) {
  if (!existsSync(folder)) {
    return [];
  }
  const found = [];
  for (const entry of readdirSync(folder, { recursive: true, withFileTypes: true })) {
    if (entry.isFile() && entry.name.endsWith("-diff.png")) {
      found.push(
        path.relative(root, path.join(entry.parentPath, entry.name)).split(path.sep).join("/"),
      );
    }
  }
  return found.sort();
}

/** The one folder a baselines' or a design folder holds, and its shots' names (`slug` in e2e/shots.ts). */
const SHOTS_FOLDER = "theme-sweep.spec.ts";
const SHOT = /^[a-z0-9]+(?:-[a-z0-9]+)*-(?:dark|light)\.png$/;

/** What Playwright leaves in the results: a folder per test, of PNGs and an error context. */
const RESULT = /^(?:[^/\\]+\.png|error-context\.md)$/;

/** Whether `kind`'s folder may hold a folder `name` at `depth`, or a file. */
const ALLOWED = {
  shots: {
    folder: (name, depth) => depth === 0 && name === SHOTS_FOLDER,
    file: (name, depth) => depth === 1 && SHOT.test(name),
  },
  results: {
    folder: (_name, depth) => depth === 0,
    file: (name, depth) => (depth === 0 ? name === ".last-run.json" : RESULT.test(name)),
  },
};

/**
 * Everything under `folder` the container wasn't meant to leave there, relative with `/` (a
 * folder's with a trailing `/`), in order: a link or junction, a hard-linked or other file that
 * isn't a plain one, and any folder or name `kind` doesn't hold ("shots": the baselines' or a
 * design's one folder of shots; "results": Playwright's). Reads no file and follows no link, and
 * doesn't look inside a folder it lists. Empty when `folder` doesn't exist.
 */
export function strays(folder, kind) {
  if (lstatSync(folder, { throwIfNoEntry: false }) === undefined) {
    return [];
  }
  const found = [];
  const visit = (relative, depth) => {
    for (const name of readdirSync(path.join(folder, relative)).sort()) {
      const inner = relative === "" ? name : `${relative}/${name}`;
      const stat = lstatSync(path.join(folder, inner));
      if (stat.isDirectory()) {
        if (ALLOWED[kind].folder(name, depth)) {
          visit(inner, depth + 1);
        } else {
          found.push(`${inner}/`);
        }
      } else if (!stat.isFile() || stat.nlink !== 1 || !ALLOWED[kind].file(name, depth)) {
        found.push(inner);
      }
    }
  };
  visit("", 0);
  return found.sort();
}

/** What the run left that it shouldn't have, under the folders it could write, from `root`. */
function leftBehind(root, results, shotsFolder) {
  const from = (folder, kind) =>
    strays(folder, kind).map((inner) =>
      path.relative(root, path.join(folder, inner)).split(path.sep).join("/"),
    );
  return [...from(results, "results"), ...from(shotsFolder, "shots")];
}

/**
 * The run for `args` on `host`: its root, `docker` (the arguments, and whether its output shows,
 * answering the status), platform, ids, environment and error output. Answers the exit status.
 */
export function shots(args, host) {
  const { root, docker, error } = host;
  const [mode, given] = args;
  if (!(mode in UPDATE) || (mode === "design") !== (given !== undefined) || args.length > 2) {
    error(USAGE);
    return 2;
  }
  if (docker(["version", "--format", "{{.Server.Version}}"], false) !== 0) {
    error(NO_DOCKER);
    return 1;
  }
  let design;
  try {
    design = mode === "design" ? designFolder(root, given) : undefined;
  } catch (refused) {
    error(refused.message);
    return 2;
  }
  const results = path.join(root, RESULTS);
  const shotsFolder = design ?? path.join(root, BASELINES);
  for (const folder of [results, shotsFolder]) {
    mkdirSync(folder, { recursive: true });
    const problem = mountProblem(root, folder);
    if (problem !== null) {
      error(problem);
      return 1;
    }
  }
  for (const step of [buildArgs(), pruneArgs()]) {
    const status = docker(step, true);
    if (status !== 0) {
      return status;
    }
  }
  const status = docker(
    runArgs({ mode, root, platform: host.platform, uid: host.uid, gid: host.gid, design }),
    true,
  );
  // Before anything reads what the container wrote: nothing but shots may be there, or the next
  // `just e2e` on this PC could run it (S130).
  const left = leftBehind(root, results, shotsFolder);
  if (left.length > 0) {
    error(
      `The container left what only shots may be. Nothing was deleted; look at each, remove what isn't yours, and run again:\n${left.join("\n")}`,
    );
    return 1;
  }
  const failed = status === 0 ? [] : diffs(root, results);
  if (failed.length > 0) {
    error(
      `The shots that differ, each beside its expected and actual image:\n${failed.join("\n")}`,
    );
    if (host.env.GITHUB_STEP_SUMMARY !== undefined) {
      appendFileSync(
        host.env.GITHUB_STEP_SUMMARY,
        `### Screenshots that differ\n\nRun \`just shots\` to see them in ${RESULTS}/.\n\n${failed.map((file) => `- \`${file}\``).join("\n")}\n`,
      );
    }
  }
  return status;
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  process.exitCode = shots(process.argv.slice(2), {
    root,
    docker: (args, shown) =>
      spawnSync("docker", args, { cwd: root, stdio: shown ? "inherit" : "pipe" }).status ?? 1,
    platform: process.platform,
    uid: process.getuid?.(),
    gid: process.getgid?.(),
    env: process.env,
    error: (message) => console.error(message),
  });
}
