// The release's push and merge, rehearsed (ADR-0047): scripts/release.mjs's own `push` and
// `merge` steps run unchanged against a registry of this run's own on 127.0.0.1, with tiny
// stand-in images for the server and the agent on both architectures, built as `just images`
// builds the real ones (`docker build`, no default attestations, as the release's build jobs set).
// Every docker command the two steps run goes to that registry instead of GHCR, and its answers
// come back naming GHCR, so each step does exactly what it does at a tag: the per-architecture
// tags pushed with `docker push`, then each index made with `docker buildx imagetools create` from
// their digests and checked before and after. A second merge must then refuse, as the index
// exists. No tag, no token, nothing pushed anywhere else. `save` and `load`, which only hand the
// images from the build runners to the push runners, are left out: this writes the IDs file
// `save` would.
//
// CI's release-images job runs it on the release's merge runner, so a runner whose Docker or
// Buildx would break the release turns CI red, and the release's guard refuses a tag on a commit
// CI didn't pass. It prints the Docker and Buildx versions it ran with.
//
// Linux only: Docker's daemon must reach the registry on 127.0.0.1, which Docker Desktop's can't.
// Every name is this run's own (`afkfleet-release-images-<pid>-<random>`): the registry container,
// removed at the end with its volume, and the stand-ins' tags, removed with their images.
//
//   node scripts/release-images-check.mjs
//
// Exit codes: 0 both steps worked, 1 they didn't (it says where), 2 arguments given or not on
// Linux. Tests: scripts/release-images-check.test.mjs.
import { spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath, pathToFileURL } from "node:url";

import { ARCHITECTURES, IMAGES, REGISTRY, main as release, workspaceVersion } from "./release.mjs";

/** The registry: distribution 3.1.2, pinned by its multi-arch index digest (Plan.md §5). */
export const REGISTRY_IMAGE =
  "registry:3.1.2@sha256:82f6ff4a7f982960c8aa51617fea3aabc485df6e55cce80d22deb1de36f1bc3c";
/** The owner the stand-ins are pushed under, in place of the repository's. */
export const OWNER = "afkfleet-check";
/** A stand-in: one small layer and the labels `save` checks, nothing run, so no emulator. */
export const DOCKERFILE = [
  "FROM scratch",
  "ARG IMAGE",
  "ARG VERSION",
  "ARG REVISION",
  "ARG CREATED",
  "COPY stand-in /stand-in",
  'LABEL org.opencontainers.image.title="$IMAGE" org.opencontainers.image.version="$VERSION" org.opencontainers.image.revision="$REVISION" org.opencontainers.image.created="$CREATED"',
  "",
].join("\n");
/** The bound on each docker command. */
const COMMAND_MS = 5 * 60 * 1000;
/** How long the registry may take to answer once started. */
const READY_MS = 30 * 1000;
/** How often its readiness is asked. */
const POLL_MS = 250;
/** 127.0.0.1 and a port, as `docker port` prints a port published there. */
const PUBLISHED = /^127\.0\.0\.1:([1-9]\d{0,4})$/;
/** What docker says when asked to remove a container or image that isn't there. */
const ABSENT = /no such (container|image)|not found/i;

/** A step that didn't work: exit 1. */
class Failed extends Error {}

/** Sends every GHCR reference in `text` to `local` instead. */
export function toLocal(text, local) {
  return text.replaceAll(`${REGISTRY}/`, `${local}/`);
}

/** Names GHCR again wherever `text` names `local`. */
export function fromLocal(text, local) {
  return text.replaceAll(`${local}/`, `${REGISTRY}/`);
}

/** `run`, with every argument sent to `local` and every answer naming GHCR again. */
export function redirected(run, local) {
  return (command, args, options = {}) => {
    const result = run(
      command,
      args.map((arg) => toLocal(arg, local)),
      options,
    );
    return {
      ...result,
      stdout: fromLocal(result.stdout, local),
      stderr: fromLocal(result.stderr, local),
    };
  };
}

/** The registry's address from `docker port`'s answer: exactly 127.0.0.1 and a port, or null. */
export function publishedAddress(text) {
  const address = text.trim();
  const port = Number(PUBLISHED.exec(address)?.[1] ?? 0);
  return port >= 1 && port <= 65535 ? address : null;
}

/** Runs a command, its output captured and its time bounded; a start failure counts as failed. */
function runCommand(command, args, options = {}) {
  const result = spawnSync(command, args, {
    encoding: "utf8",
    env: options.env,
    maxBuffer: 64 * 1024 * 1024,
    stdio: ["ignore", "pipe", "pipe"],
    timeout: COMMAND_MS,
  });
  return {
    status: result.status ?? 1,
    stdout: result.stdout ?? "",
    stderr: result.stderr || (result.error ? String(result.error.message) : ""),
  };
}

/** A sink that keeps what's written, and passes it on to `to`. */
function tee(to) {
  const parts = [];
  return {
    write: (text) => {
      parts.push(text);
      to.write(text);
    },
    text: () => parts.join(""),
  };
}

/** Waits until the registry at `local` answers its API's root, within READY_MS. */
async function ready(local, fetchOnce) {
  const deadline = Date.now() + READY_MS;
  for (;;) {
    try {
      const response = await fetchOnce(`http://${local}/v2/`, {
        signal: AbortSignal.timeout(2000),
      });
      if (response.ok) {
        return;
      }
    } catch {
      // Not listening yet.
    }
    if (Date.now() >= deadline) {
      throw new Failed(`the registry at ${local} didn't answer within ${READY_MS / 1000} s`);
    }
    await delay(POLL_MS);
  }
}

/**
 * Runs the rehearsal; its exit code. `io.run` stands in for docker, `io.fetch` for the registry's
 * readiness check, `io.platform` for the OS.
 */
export async function main(args, io) {
  if (args.length !== 0) {
    io.err.write("usage: release-images-check.mjs (it takes no arguments)\n");
    return 2;
  }
  if (io.platform !== "linux") {
    io.err.write(
      "release-images-check runs on Linux alone: Docker's daemon must reach a registry on 127.0.0.1, which Docker Desktop's can't. CI's release-images job runs it.\n",
    );
    return 2;
  }
  const run = io.run ?? runCommand;
  const name = `afkfleet-release-images-${process.pid}-${randomBytes(4).toString("hex")}`;
  const made = [];
  const work = mkdtempSync(path.join(tmpdir(), "afkfleet-release-images-"));
  /** Runs docker; its stdout, or a failure naming `what`. */
  const docker = (dockerArgs, what, options = {}) => {
    const result = run("docker", dockerArgs, options);
    if (result.status !== 0) {
      throw new Failed(`${what} failed: ${result.stderr.trim() || `exit ${result.status}`}`);
    }
    return result.stdout;
  };

  try {
    const engine = docker(
      ["version", "--format", "{{.Server.Version}}"],
      "asking Docker's version",
    );
    const buildx = docker(["buildx", "version"], "asking Buildx's version");
    io.out.write(`Docker ${engine.trim()}, Buildx: ${buildx.trim()}\n`);

    docker(
      ["run", "--detach", "--rm", "--name", name, "--publish", "127.0.0.1::5000", REGISTRY_IMAGE],
      "starting the registry",
    );
    const answer = docker(["port", name, "5000/tcp"], "reading the registry's port");
    const local = publishedAddress(answer);
    if (local === null) {
      throw new Failed(`the registry isn't published on 127.0.0.1 alone: ${answer.trim()}`);
    }
    await ready(local, io.fetch ?? fetch);
    io.out.write(`the registry: ${local}, standing in for ${REGISTRY} below\n`);

    const version = workspaceVersion(readFileSync(path.join(io.root, "Cargo.toml"), "utf8"));
    const revision = "0".repeat(40);
    const created = new Date().toISOString().replace(/\.\d{3}Z$/, "Z");
    const context = path.join(work, "context");
    const images = path.join(work, "images");
    const digests = path.join(work, "digests");
    for (const folder of [context, images, digests]) {
      mkdirSync(folder);
    }
    writeFileSync(path.join(context, "Dockerfile"), DOCKERFILE);
    writeFileSync(
      path.join(context, "stand-in"),
      "a stand-in of scripts/release-images-check.mjs\n",
    );
    for (const arch of ARCHITECTURES) {
      const lines = [];
      for (const image of IMAGES) {
        const tag = `${name}/${image}:${arch}`;
        made.push(tag);
        docker(
          [
            "build",
            "--platform",
            `linux/${arch}`,
            ...[
              `IMAGE=${image}`,
              `VERSION=${version}`,
              `REVISION=${revision}`,
              `CREATED=${created}`,
            ].flatMap((arg) => ["--build-arg", arg]),
            "--tag",
            tag,
            context,
          ],
          `building ${tag}`,
          { env: { ...io.env, BUILDX_NO_DEFAULT_ATTESTATIONS: "1" } },
        );
        const id = docker(["image", "inspect", "--format", "{{.Id}}", tag], `inspecting ${tag}`);
        lines.push(`${image}=${id.trim()}\n`);
      }
      writeFileSync(path.join(images, `images-${arch}.txt`), lines.join(""));
    }

    const outputs = path.join(work, "outputs");
    writeFileSync(outputs, "");
    const steps = {
      root: io.root,
      env: {
        ...io.env,
        GITHUB_OUTPUT: outputs,
        GITHUB_REPOSITORY_OWNER: OWNER,
        AFKFLEET_IMAGE_VERSION: version,
        AFKFLEET_IMAGE_REVISION: revision,
        AFKFLEET_IMAGE_CREATED: created,
      },
      out: io.out,
      err: io.err,
      run: redirected(run, local),
    };
    for (const arch of ARCHITECTURES) {
      made.push(...IMAGES.map((image) => `${local}/${OWNER}/${image}:${version}-${arch}`));
      if (release(["push", arch, images, digests], steps) !== 0) {
        throw new Failed(`release.mjs push ${arch} didn't work (above)`);
      }
    }
    if (release(["merge", digests], steps) !== 0) {
      throw new Failed("release.mjs merge didn't work (above)");
    }
    io.out.write(readFileSync(outputs, "utf8"));
    for (const image of IMAGES) {
      const index = `${local}/${OWNER}/${image}:${version}`;
      let raw;
      try {
        raw = JSON.parse(
          docker(["buildx", "imagetools", "inspect", "--raw", index], `reading ${index}`),
        );
      } catch (error) {
        throw error instanceof Failed ? error : new Failed(`${index} isn't JSON`);
      }
      const kinds = (Array.isArray(raw?.manifests) ? raw.manifests : []).map(
        (one) => one?.mediaType,
      );
      io.out.write(`${fromLocal(index, local)}: ${raw?.mediaType}, of ${kinds.join(" and ")}\n`);
    }

    io.out.write("a second merge, which must refuse as the indexes exist:\n");
    const again = tee(io.err);
    if (
      release(["merge", digests], { ...steps, err: again }) !== 1 ||
      !/exists already/.test(again.text())
    ) {
      throw new Failed("a second merge didn't refuse the indexes that exist");
    }
    io.out.write("release-images-check: the release's push and merge work on this runner\n");
    return 0;
  } catch (error) {
    if (error instanceof Failed) {
      io.err.write(`release-images-check: ${error.message}\n`);
      return 1;
    }
    throw error;
  } finally {
    for (const [what, removal] of [
      [name, ["rm", "--force", "--volumes", name]],
      ...made.reverse().map((tag) => [tag, ["image", "rm", tag]]),
    ]) {
      const result = run("docker", removal);
      if (result.status !== 0 && !ABSENT.test(result.stderr)) {
        io.err.write(`release-images-check: ${what} wasn't removed: ${result.stderr.trim()}\n`);
      }
    }
    rmSync(work, { recursive: true, force: true });
  }
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = await main(process.argv.slice(2), {
    root: path.resolve(path.dirname(fileURLToPath(import.meta.url)), ".."),
    env: process.env,
    out: process.stdout,
    err: process.stderr,
    platform: process.platform,
  });
}
