// The release workflow's own steps (Plan.md P12.5; ADR-0022, S22, U31), each a subcommand that
// .github/workflows/release.yml runs:
//
//   node scripts/release.mjs guard <GITHUB_REF>          (the guard job)
//   node scripts/release.mjs push <arm64|amd64> <dir>    (each image job, logged in)
//   node scripts/release.mjs merge <dir>                 (the merge job, logged in)
//   node scripts/release.mjs notes <X.Y.Z> <server digest> <agent digest>
//
// guard: the tag is `vX.Y.Z`, X.Y.Z is the workspace's version, the tag's commit is the run's
// own (GITHUB_SHA) and on main, and CI's own workflow succeeded on a push of exactly that commit
// (never Stack's: each image job runs `just test-stack` itself, U31). It writes `version`,
// `revision` and one UTC `created` to GITHUB_OUTPUT, and nothing when it refuses.
//
// push: the two images `just images` and `just test-stack` left as `afkfleet-*:dev`, which are
// the tested ones, must be linux/<arch> and carry this release's labels (AFKFLEET_IMAGE_*). Each
// tag `ghcr.io/<owner>/afkfleet-*:X.Y.Z-<arch>` must not exist yet: nothing is ever overwritten.
// Then it tags, pushes and records each image's digest in `<dir>/digests-<arch>.txt`.
//
// merge: from both digests files, each index `X.Y.Z` is made with `docker buildx imagetools
// create` from the digests themselves (`name@sha256:…`), never from the per-architecture tags.
// First each digest must be one linux image of its architecture, each per-architecture tag must
// still point at it, and the index's tag must not exist; afterwards the index must hold exactly
// those two digests, linux/arm64 and linux/amd64. It writes both index digests to GITHUB_OUTPUT,
// then tries an anonymous pull of each and warns while the packages still need a login.
//
// notes: the draft release's body, as approved (ADR-0022, U33), on stdout.
//
// Exit codes: 0 done, 1 refused (it says why on stderr), 2 usage error. Tests:
// scripts/release.test.mjs; the workflow's shape: scripts/release-workflow.test.mjs.
import { spawnSync } from "node:child_process";
import { appendFileSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

import { installerName, workspaceVersion } from "./app-installer.mjs";

/** The repository's own URL, as the Dockerfiles' source label has it. */
export const SOURCE = "https://github.com/noah-gabel/afkfleet";
/** The images a release pushes, in order. */
export const IMAGES = ["afkfleet-server", "afkfleet-agent"];
/** The architectures, in the order each index lists them. */
export const ARCHITECTURES = ["arm64", "amd64"];
/** The registry the images go to. */
export const REGISTRY = "ghcr.io";

const VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const COMMIT = /^[0-9a-f]{40}$/;
const DIGEST = /^sha256:[0-9a-f]{64}$/;
const OWNER = /^[a-z0-9](?:[a-z0-9-]{0,37}[a-z0-9])?$/;
const CREATED = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/;
const LABEL = "org.opencontainers.image";

/** A refusal: the release must not go on (exit 1). */
class Refusal extends Error {}
/** A usage error: the workflow called the script wrongly (exit 2). */
class Usage extends Error {}

/** The version in a `refs/tags/vX.Y.Z` ref; null for anything else. */
export function parseTag(ref) {
  const found = /^refs\/tags\/v([0-9.]+)$/.exec(ref);
  return found !== null && VERSION.test(found[1]) ? found[1] : null;
}

/** The draft release's body (ADR-0022, U33). */
export function notes({ version, owner, server, agent }) {
  return [
    `afkfleet ${version}`,
    "",
    `Desktop app for Windows: ${installerName(version)}`,
    `How to install and use it: ${SOURCE}/blob/main/docs/using-the-app.md`,
    "Server images:",
    `  ${REGISTRY}/${owner}/afkfleet-server:${version}@${server}`,
    `  ${REGISTRY}/${owner}/afkfleet-agent:${version}@${agent}`,
    "SHA-256 of every file: SHA256SUMS",
    "",
  ].join("\n");
}

/** Runs a command, its output captured; a start failure counts as a failed run. */
function runCommand(command, args, options = {}) {
  const result = spawnSync(command, args, {
    encoding: "utf8",
    env: options.env,
    maxBuffer: 64 * 1024 * 1024,
    stdio: ["ignore", "pipe", "pipe"],
  });
  return {
    status: result.status ?? 1,
    stdout: result.stdout ?? "",
    stderr: result.stderr ?? (result.error ? String(result.error.message) : ""),
  };
}

/** The steps' shared context. */
function context(io) {
  const { env, out } = io;
  const run = io.run ?? runCommand;
  /** Runs a command that must succeed; its stdout. */
  const must = (command, args, what) => {
    const result = run(command, args);
    if (result.status !== 0) {
      throw new Refusal(`${what} failed: ${result.stderr.trim() || `exit ${result.status}`}`);
    }
    return result.stdout;
  };
  /** Appends `name=value` lines to GITHUB_OUTPUT. */
  const output = (pairs) => {
    const file = env.GITHUB_OUTPUT;
    if (file === undefined || file === "") {
      throw new Usage("GITHUB_OUTPUT isn't set");
    }
    appendFileSync(file, pairs.map(([name, value]) => `${name}=${value}\n`).join(""));
  };
  return { env, out, run, must, output, root: io.root, now: io.now ?? (() => new Date()) };
}

/** The repository owner, lowercased as GHCR names it. */
function owner(env) {
  const name = (env.GITHUB_REPOSITORY_OWNER ?? "").toLowerCase();
  if (!OWNER.test(name)) {
    throw new Usage("GITHUB_REPOSITORY_OWNER isn't a GitHub owner's name");
  }
  return name;
}

/** The release's version, revision and created time, from the AFKFLEET_IMAGE_* variables. */
function release(env) {
  const version = env.AFKFLEET_IMAGE_VERSION ?? "";
  const revision = env.AFKFLEET_IMAGE_REVISION ?? "";
  const created = env.AFKFLEET_IMAGE_CREATED ?? "";
  if (!VERSION.test(version) || !COMMIT.test(revision) || !CREATED.test(created)) {
    throw new Usage(
      "AFKFLEET_IMAGE_VERSION, AFKFLEET_IMAGE_REVISION and AFKFLEET_IMAGE_CREATED must be the guard's outputs",
    );
  }
  return { version, revision, created };
}

/** Parses JSON a command printed; a refusal naming `what` when it isn't. */
function json(text, what) {
  try {
    return JSON.parse(text);
  } catch {
    throw new Refusal(`${what} didn't print JSON`);
  }
}

/**
 * Whether a reference exists in the registry: true, false, or a refusal when that's unknown.
 * "Denied" counts as absent: the package may not exist yet (the first release), and a token that
 * can't read a tag can't overwrite it either, since pushing needs more than reading.
 */
function exists(ctx, reference, options = {}) {
  const result = ctx.run("docker", ["buildx", "imagetools", "inspect", reference], options);
  if (result.status === 0) {
    return true;
  }
  if (/not found|manifest unknown|name unknown|denied/i.test(result.stderr)) {
    return false;
  }
  throw new Refusal(`whether ${reference} exists can't be checked: ${result.stderr.trim()}`);
}

/** The digest the registry serves for `reference`. */
function served(ctx, reference) {
  const manifest = json(
    ctx.must(
      "docker",
      ["buildx", "imagetools", "inspect", "--format", "{{json .Manifest}}", reference],
      `inspecting ${reference}`,
    ),
    `inspecting ${reference}`,
  );
  const digest = manifest?.digest;
  if (typeof digest !== "string" || !DIGEST.test(digest)) {
    throw new Refusal(`the registry gave no digest for ${reference}`);
  }
  return digest;
}

function guard(ctx, args) {
  if (args.length !== 1) {
    throw new Usage("guard takes the run's GITHUB_REF");
  }
  const tag = args[0];
  const version = parseTag(tag);
  if (version === null) {
    throw new Refusal(`${tag} isn't a release tag: releases are tagged vX.Y.Z`);
  }
  const workspace = workspaceVersion(readFileSync(path.join(ctx.root, "Cargo.toml"), "utf8"));
  if (workspace !== version) {
    throw new Refusal(
      `the tag says ${version}, but Cargo.toml's workspace version is ${workspace ?? "unreadable"}`,
    );
  }
  if (!(ctx.env.GITHUB_OUTPUT ?? "")) {
    throw new Usage("GITHUB_OUTPUT isn't set");
  }
  const revision = ctx
    .must("git", ["rev-parse", "--verify", `${tag}^{commit}`], `reading ${tag}`)
    .trim();
  if (!COMMIT.test(revision)) {
    throw new Refusal(`${tag} doesn't name a commit`);
  }
  if (revision !== ctx.env.GITHUB_SHA) {
    throw new Refusal(`${tag} is on ${revision}, not on this run's GITHUB_SHA: the tag moved`);
  }
  const onMain = ctx.run("git", [
    "merge-base",
    "--is-ancestor",
    revision,
    "refs/remotes/origin/main",
  ]);
  if (onMain.status !== 0) {
    throw new Refusal(`${revision} isn't on main: a release is tagged on a commit of main`);
  }
  const runs = ctx.run("gh", [
    "run",
    "list",
    "--workflow",
    "ci.yml",
    "--branch",
    "main",
    "--commit",
    revision,
    "--event",
    "push",
    "--status",
    "success",
    "--json",
    "databaseId,headSha,conclusion",
    "--limit",
    "1",
  ]);
  const found = runs.status === 0 ? json(runs.stdout, "gh run list") : null;
  const green =
    Array.isArray(found) &&
    found.some((one) => one?.headSha === revision && one?.conclusion === "success");
  if (!green) {
    throw new Refusal(
      `CI hasn't succeeded on ${revision} on main yet${runs.status === 0 ? "" : ` (gh: ${runs.stderr.trim()})`}: wait for its run, then re-run this workflow`,
    );
  }
  const created = ctx
    .now()
    .toISOString()
    .replace(/\.\d{3}Z$/, "Z");
  ctx.output([
    ["version", version],
    ["revision", revision],
    ["created", created],
  ]);
  ctx.out.write(`v${version} on ${revision}: on main, CI green; created ${created}\n`);
}

function push(ctx, args) {
  const [arch, dir] = args;
  if (args.length !== 2 || !ARCHITECTURES.includes(arch)) {
    throw new Usage(`push takes an architecture (${ARCHITECTURES.join(" or ")}) and a folder`);
  }
  const { version, revision, created } = release(ctx.env);
  const name = owner(ctx.env);
  const local = (image) => `${image}:dev`;
  const remote = (image) => `${REGISTRY}/${name}/${image}:${version}-${arch}`;

  for (const image of IMAGES) {
    const platform = ctx
      .must(
        "docker",
        ["image", "inspect", "--format", "{{.Os}}/{{.Architecture}}", local(image)],
        `inspecting ${local(image)}`,
      )
      .trim();
    if (platform !== `linux/${arch}`) {
      throw new Refusal(`${local(image)} is ${platform || "of no platform"}, not linux/${arch}`);
    }
    const labels =
      json(
        ctx.must(
          "docker",
          ["image", "inspect", "--format", "{{json .Config.Labels}}", local(image)],
          `inspecting ${local(image)}`,
        ),
        `inspecting ${local(image)}`,
      ) ?? {};
    for (const [label, value] of [
      ["version", version],
      ["revision", revision],
      ["created", created],
    ]) {
      if (labels[`${LABEL}.${label}`] !== value) {
        throw new Refusal(
          `${local(image)}'s ${LABEL}.${label} isn't ${value}: it wasn't built for this release`,
        );
      }
    }
  }
  for (const image of IMAGES) {
    if (exists(ctx, remote(image))) {
      throw new Refusal(`${remote(image)} exists already: a release never overwrites a tag`);
    }
  }
  const lines = [];
  for (const image of IMAGES) {
    ctx.must("docker", ["tag", local(image), remote(image)], `tagging ${remote(image)}`);
    ctx.must("docker", ["push", remote(image)], `pushing ${remote(image)}`);
    const repoDigests =
      json(
        ctx.must(
          "docker",
          ["image", "inspect", "--format", "{{json .RepoDigests}}", remote(image)],
          `inspecting ${remote(image)}`,
        ),
        `inspecting ${remote(image)}`,
      ) ?? [];
    const prefix = `${REGISTRY}/${name}/${image}@`;
    const own = (Array.isArray(repoDigests) ? repoDigests : [])
      .filter((entry) => typeof entry === "string" && entry.startsWith(prefix))
      .map((entry) => entry.slice(prefix.length));
    if (own.length !== 1 || !DIGEST.test(own[0])) {
      throw new Refusal(`${remote(image)} has no single digest in ${REGISTRY}/${name}/${image}`);
    }
    lines.push(`${image}=${own[0]}\n`);
    ctx.out.write(`pushed ${remote(image)}@${own[0]}\n`);
  }
  writeFileSync(path.join(dir, `digests-${arch}.txt`), lines.join(""), { flag: "wx" });
}

/** One architecture's digests file: exactly one well-formed digest per image. */
function readDigests(dir, arch) {
  const file = path.join(dir, `digests-${arch}.txt`);
  let text;
  try {
    text = readFileSync(file, "utf8");
  } catch {
    throw new Refusal(`${file} is missing: the ${arch} image job left no digests`);
  }
  const entries = text
    .split("\n")
    .filter((line) => line !== "")
    .map((line) => line.split("="));
  const digests = new Map(entries.map(([image, digest]) => [image, digest]));
  const wellFormed =
    entries.length === IMAGES.length &&
    digests.size === IMAGES.length &&
    IMAGES.every((image) => DIGEST.test(digests.get(image) ?? ""));
  if (!wellFormed) {
    throw new Refusal(`${file} isn't one digest for each of ${IMAGES.join(" and ")}`);
  }
  return digests;
}

function merge(ctx, args) {
  if (args.length !== 1) {
    throw new Usage("merge takes the folder the digests were downloaded to");
  }
  const { version, revision, created } = release(ctx.env);
  const name = owner(ctx.env);
  const byArch = new Map(ARCHITECTURES.map((arch) => [arch, readDigests(args[0], arch)]));
  if (!(ctx.env.GITHUB_OUTPUT ?? "")) {
    throw new Usage("GITHUB_OUTPUT isn't set");
  }
  const repository = (image) => `${REGISTRY}/${name}/${image}`;
  const index = (image) => `${repository(image)}:${version}`;

  for (const image of IMAGES) {
    for (const arch of ARCHITECTURES) {
      const digest = byArch.get(arch)?.get(image);
      const pinned = `${repository(image)}@${digest}`;
      const config = json(
        ctx.must(
          "docker",
          ["buildx", "imagetools", "inspect", "--format", "{{json .Image}}", pinned],
          `inspecting ${pinned}`,
        ),
        `inspecting ${pinned}`,
      );
      if (typeof config?.architecture !== "string" || typeof config?.os !== "string") {
        throw new Refusal(`${pinned} isn't one platform's image`);
      }
      if (`${config.os}/${config.architecture}` !== `linux/${arch}`) {
        throw new Refusal(`${pinned} is ${config.os}/${config.architecture}, not linux/${arch}`);
      }
      const tagged = `${repository(image)}:${version}-${arch}`;
      if (served(ctx, tagged) !== digest) {
        throw new Refusal(`${tagged} no longer points at ${digest}`);
      }
    }
    if (exists(ctx, index(image), {})) {
      throw new Refusal(`${index(image)} exists already: a release never overwrites a tag`);
    }
  }
  const digests = [];
  for (const image of IMAGES) {
    const sources = ARCHITECTURES.map(
      (arch) => `${repository(image)}@${byArch.get(arch)?.get(image)}`,
    );
    ctx.must(
      "docker",
      [
        "buildx",
        "imagetools",
        "create",
        "--tag",
        index(image),
        ...[
          `version=${version}`,
          `revision=${revision}`,
          `created=${created}`,
          `source=${SOURCE}`,
          "licenses=GPL-3.0-or-later",
        ].flatMap((annotation) => ["--annotation", `index:${LABEL}.${annotation}`]),
        ...sources,
      ],
      `creating ${index(image)}`,
    );
    const raw = json(
      ctx.must(
        "docker",
        ["buildx", "imagetools", "inspect", "--raw", index(image)],
        `inspecting ${index(image)}`,
      ),
      `inspecting ${index(image)}`,
    );
    const listed = (Array.isArray(raw?.manifests) ? raw.manifests : []).map(
      (one) => `${one?.platform?.os}/${one?.platform?.architecture}=${one?.digest}`,
    );
    const expected = ARCHITECTURES.map((arch) => `linux/${arch}=${byArch.get(arch)?.get(image)}`);
    if (listed.length !== expected.length || listed.some((one, at) => one !== expected[at])) {
      throw new Refusal(
        `${index(image)} holds ${listed.join(", ") || "nothing"}, not exactly ${expected.join(", ")}`,
      );
    }
    digests.push(served(ctx, index(image)));
    ctx.out.write(`created ${index(image)}@${digests.at(-1)}\n`);
  }
  ctx.output(IMAGES.map((image, at) => [`${image.replace("afkfleet-", "")}_digest`, digests[at]]));

  const anonymous = mkdtempSync(path.join(tmpdir(), "afkfleet-anonymous-"));
  try {
    for (const image of IMAGES) {
      const pulled = ctx.run("docker", ["buildx", "imagetools", "inspect", index(image)], {
        env: { ...ctx.env, DOCKER_CONFIG: anonymous },
      });
      if (pulled.status !== 0) {
        ctx.out.write(
          `::warning::${index(image)} can't be pulled without a login yet: make the ${image} package Public (docs/releasing.md) before publishing the release\n`,
        );
      }
    }
  } finally {
    rmSync(anonymous, { recursive: true, force: true });
  }
}

function printNotes(ctx, args) {
  const [version, server, agent] = args;
  if (args.length !== 3 || !VERSION.test(version) || !DIGEST.test(server) || !DIGEST.test(agent)) {
    throw new Usage("notes takes X.Y.Z and the server's and the agent's index digests");
  }
  ctx.out.write(notes({ version, owner: owner(ctx.env), server, agent }));
}

const COMMANDS = { guard, push, merge, notes: printNotes };

/** Runs one subcommand; its exit code. `io.run` and `io.now` stand in for commands and the clock. */
export function main(args, io) {
  const [command, ...rest] = args;
  const step = Object.hasOwn(COMMANDS, command ?? "") ? COMMANDS[command] : undefined;
  if (step === undefined) {
    io.err.write(`usage: release.mjs <${Object.keys(COMMANDS).join("|")}> …\n`);
    return 2;
  }
  try {
    step(context(io), rest);
    return 0;
  } catch (error) {
    if (error instanceof Refusal) {
      io.err.write(`release ${command}: refused: ${error.message}\n`);
      return 1;
    }
    if (error instanceof Usage) {
      io.err.write(`release ${command}: ${error.message}\n`);
      return 2;
    }
    throw error;
  }
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = main(process.argv.slice(2), {
    root: path.resolve(path.dirname(fileURLToPath(import.meta.url)), ".."),
    env: process.env,
    out: process.stdout,
    err: process.stderr,
  });
}
