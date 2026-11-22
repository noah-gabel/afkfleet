// Tests for scripts/release-images-check.mjs (ADR-0047): how release.mjs's push and merge are sent
// to a registry of the run's own, the stand-ins it builds, and that it removes only what it made.
// No test runs docker: a stand-in answers each call as scripted and records it. The rehearsal
// itself runs in CI's release-images job, against a real registry.
import assert from "node:assert/strict";
import path from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import {
  DOCKERFILE,
  fromLocal,
  main,
  OWNER,
  publishedAddress,
  REGISTRY_IMAGE,
  redirected,
  toLocal,
} from "./release-images-check.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const LOCAL = "127.0.0.1:49153";
const DIGEST = `sha256:${"a".repeat(64)}`;

/** A sink for stdout or stderr. */
function sink() {
  const parts = [];
  return { write: (text) => parts.push(text), text: () => parts.join("") };
}

/**
 * main() with docker answered by the first of `answers` whose `when` is a prefix of the command
 * line ending at a space, or the whole of it; by default it succeeds with no output.
 */
async function run(args, answers = [], platform = "linux") {
  const calls = [];
  const out = sink();
  const err = sink();
  const code = await main(args, {
    root: ROOT,
    env: { PATH: "unused" },
    out,
    err,
    platform,
    fetch: async () => ({ ok: true }),
    run: (command, commandArgs) => {
      const line = [command, ...commandArgs].join(" ");
      calls.push(line);
      const found =
        answers.find((answer) => line === answer.when || line.startsWith(`${answer.when} `)) ?? {};
      return { status: found.status ?? 0, stdout: found.stdout ?? "", stderr: found.stderr ?? "" };
    },
  });
  return { code, out: out.text(), err: err.text(), calls };
}

/** Docker's answers up to the stand-ins' build. */
const STARTED = [
  { when: "docker version", stdout: "29.1.0\n" },
  { when: "docker buildx version", stdout: "github.com/docker/buildx v0.39.0 0123abc\n" },
  { when: "docker port", stdout: `${LOCAL}\n` },
];

describe("release-images-check: the registry in GHCR's place", () => {
  it("sends every GHCR reference to the local registry, and its answers back as GHCR's", () => {
    const seen = [];
    const run = redirected((command, args, options) => {
      seen.push({ command, args, options });
      return {
        status: 0,
        stdout: `${LOCAL}/${OWNER}/afkfleet-server@${DIGEST}\n`,
        stderr: `ERROR: ${LOCAL}/${OWNER}/afkfleet-agent:0.2.1: not found\n`,
      };
    }, LOCAL);

    const result = run(
      "docker",
      [
        "buildx",
        "imagetools",
        "create",
        "--tag",
        `ghcr.io/${OWNER}/afkfleet-server:0.2.1`,
        `ghcr.io/${OWNER}/afkfleet-server@${DIGEST}`,
      ],
      { env: { DOCKER_CONFIG: "anonymous" } },
    );

    assert.deepEqual(seen, [
      {
        command: "docker",
        args: [
          "buildx",
          "imagetools",
          "create",
          "--tag",
          `${LOCAL}/${OWNER}/afkfleet-server:0.2.1`,
          `${LOCAL}/${OWNER}/afkfleet-server@${DIGEST}`,
        ],
        options: { env: { DOCKER_CONFIG: "anonymous" } },
      },
    ]);
    assert.equal(result.stdout, `ghcr.io/${OWNER}/afkfleet-server@${DIGEST}\n`);
    assert.equal(result.stderr, `ERROR: ghcr.io/${OWNER}/afkfleet-agent:0.2.1: not found\n`);
  });

  it("leaves what names neither registry as it is", () => {
    assert.equal(toLocal("ghcr.io", LOCAL), "ghcr.io");
    assert.equal(toLocal("--format {{json .Manifest}}", LOCAL), "--format {{json .Manifest}}");
    assert.equal(fromLocal("127.0.0.1:49153", LOCAL), "127.0.0.1:49153");
  });

  it("takes the registry's port published on 127.0.0.1, and nothing else", () => {
    assert.equal(publishedAddress("127.0.0.1:49153\n"), "127.0.0.1:49153");
    assert.equal(publishedAddress("127.0.0.1:65535"), "127.0.0.1:65535");
    for (const answer of [
      "",
      "0.0.0.0:49153",
      "[::]:49153",
      "127.0.0.1:0",
      "127.0.0.1:65536",
      "127.0.0.1:049153",
      "127.0.0.1:49153\n[::1]:49153",
      "localhost:49153",
    ]) {
      assert.equal(publishedAddress(answer), null, JSON.stringify(answer));
    }
  });

  it("pins the registry's image by its digest", () => {
    assert.match(REGISTRY_IMAGE, /^registry:\d+\.\d+\.\d+@sha256:[0-9a-f]{64}$/);
  });
});

describe("release-images-check: the stand-ins", () => {
  it("are one small layer with the release's labels, built from nothing and running nothing", () => {
    const lines = DOCKERFILE.split("\n");

    assert.equal(lines[0], "FROM scratch");
    assert.ok(lines.includes("COPY stand-in /stand-in"));
    assert.equal(
      lines.some((line) => /^RUN\b/.test(line)),
      false,
    );
    for (const label of ["version", "revision", "created"]) {
      assert.match(DOCKERFILE, new RegExp(`org\\.opencontainers\\.image\\.${label}="\\$`));
    }
  });
});

describe("release-images-check: refusals and cleanup", () => {
  it("refuses arguments, and runs on Linux alone, running nothing", async () => {
    const given = await run(["extra"]);
    const windows = await run([], [], "win32");

    assert.equal(given.code, 2);
    assert.match(given.err, /no arguments/);
    assert.deepEqual(given.calls, []);
    assert.equal(windows.code, 2);
    assert.match(windows.err, /Linux alone.*release-images job/);
    assert.deepEqual(windows.calls, []);
  });

  it("prints the Docker and Buildx it runs with, first", async () => {
    const result = await run([], [...STARTED, { when: "docker build", status: 1 }]);

    assert.deepEqual(result.calls.slice(0, 2), [
      "docker version --format {{.Server.Version}}",
      "docker buildx version",
    ]);
    assert.match(result.out, /^Docker 29\.1\.0, Buildx: github\.com\/docker\/buildx v0\.39\.0/);
  });

  it("refuses a registry published anywhere but 127.0.0.1, before building anything", async () => {
    const result = await run([], [{ when: "docker port", stdout: "0.0.0.0:49153\n" }, ...STARTED]);

    assert.equal(result.code, 1);
    assert.match(result.err, /127\.0\.0\.1 alone: 0\.0\.0\.0:49153/);
    assert.equal(
      result.calls.some((call) => call.startsWith("docker build ")),
      false,
    );
  });

  it("stops at a step that fails, says which, and removes only what this run made", async () => {
    const result = await run(
      [],
      [...STARTED, { when: "docker build", status: 1, stderr: "ERROR: no space left\n" }],
    );

    assert.equal(result.code, 1);
    assert.match(
      result.err,
      /building afkfleet-release-images-\d+-[0-9a-f]{8}\/afkfleet-server:arm64 failed: ERROR: no space left/,
    );
    const started = result.calls.find((call) => call.startsWith("docker run "));
    const name = /--name (afkfleet-release-images-\d+-[0-9a-f]{8}) /.exec(started)?.[1];
    assert.ok(name, started);
    assert.match(
      started,
      new RegExp(`--publish 127\\.0\\.0\\.1::5000 ${REGISTRY_IMAGE.replace(/[.]/g, "\\.")}$`),
    );
    const at = result.calls.findIndex((call) => call.startsWith("docker build "));
    assert.deepEqual(result.calls.slice(at + 1), [
      `docker rm --force --volumes ${name}`,
      `docker image rm ${name}/afkfleet-server:arm64`,
    ]);
  });

  it("says what it couldn't remove, but not what was never there", async () => {
    const result = await run(
      [],
      [
        ...STARTED,
        { when: "docker build", status: 1 },
        {
          when: "docker rm",
          status: 1,
          stderr: "Error response from daemon: No such container: x\n",
        },
        { when: "docker image rm", status: 1, stderr: "Error response from daemon: conflict\n" },
      ],
    );

    assert.equal(result.code, 1);
    assert.doesNotMatch(result.err, /No such container/);
    assert.match(
      result.err,
      /afkfleet-server:arm64 wasn't removed: Error response from daemon: conflict/,
    );
  });
});
