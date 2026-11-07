// Tests for scripts/deploy-bundle.mjs, the host's release files (ADR-0022, S23, S31, S33, S34).
// Run with `just scripts-test`. Every bundle here is built from this repository's files or a
// copy of them in a temporary folder, with placeholder digests: nothing is published.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFileSync,
  cpSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import process from "node:process";
import { afterEach, describe, it } from "node:test";
import { fileURLToPath } from "node:url";
import { gunzipSync } from "node:zlib";

import {
  BUNDLE_FILES,
  buildBundle,
  bundleName,
  DEVELOPMENT_ONLY,
  main,
  rewriteImages,
  tarOf,
  updatesFrom,
} from "./deploy-bundle.mjs";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const VERSION = readFileSync(join(ROOT, "Cargo.toml"), "utf8").match(
  /^\[workspace\.package\][^[]*?^version = "([^"]+)"$/ms,
)[1];
const CREATED = "2026-11-07T12:00:00Z";
const SERVER = `sha256:${"a".repeat(64)}`;
const AGENT = `sha256:${"b".repeat(64)}`;
const OWNER = "noah-gabel";

/** The temporary folders made so far, removed after each test. */
const made = [];

afterEach(() => {
  for (const dir of made.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

function temporary() {
  const dir = mkdtempSync(join(tmpdir(), "afkfleet-bundle-"));
  made.push(dir);
  return dir;
}

/** A copy of what the bundle reads, in a folder of its own, for a case to change. */
function copyRoot() {
  const root = temporary();
  for (const [, source] of BUNDLE_FILES) {
    mkdirSync(dirname(join(root, source)), { recursive: true });
    copyFileSync(join(ROOT, source), join(root, source));
  }
  copyFileSync(join(ROOT, "Cargo.toml"), join(root, "Cargo.toml"));
  return root;
}

/** The release's inputs, `root`'s files by default. */
function inputs(root = ROOT, version = VERSION) {
  return {
    root,
    version,
    created: CREATED,
    serverDigest: SERVER,
    agentDigest: AGENT,
    owner: OWNER,
  };
}

/** A tar's entries: each header's fields and its data; checks every header's checksum. */
function entries(tar) {
  const found = [];
  let at = 0;
  while (at + 512 <= tar.length) {
    const head = tar.subarray(at, at + 512);
    if (head.every((byte) => byte === 0)) {
      break;
    }
    const field = (offset, length) =>
      head
        .subarray(offset, offset + length)
        .toString("latin1")
        .replace(/\0.*$/s, "");
    const octal = (offset, length) => Number.parseInt(field(offset, length).trim(), 8);
    const blank = Buffer.from(head);
    blank.fill(0x20, 148, 156);
    assert.equal(
      blank.reduce((sum, byte) => sum + byte, 0),
      octal(148, 8),
      "checksum",
    );
    const size = octal(124, 12);
    found.push({
      name: field(0, 100),
      mode: octal(100, 8),
      uid: octal(108, 8),
      gid: octal(116, 8),
      size,
      mtime: octal(136, 12),
      type: field(156, 1),
      magic: field(257, 6),
      version: head.subarray(263, 265).toString("latin1"),
      uname: field(265, 32),
      gname: field(297, 32),
      prefix: field(345, 155),
      data: tar.subarray(at + 512, at + 512 + size),
    });
    at += 512 + Math.ceil(size / 512) * 512;
  }
  return found;
}

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

describe("the archive", () => {
  it("a fixture's tar has one pinned SHA-256 on every platform", () => {
    const tar = tarOf(
      [
        { name: "a.txt", data: Buffer.from("afkfleet\n") },
        { name: "host/" },
        { name: "host/b.sh", data: Buffer.from("#!/bin/sh\n") },
      ],
      1_793_793_600,
    );

    assert.equal(
      sha256(tar),
      "3368b1de26de6a84c803f3a32852e5697dca7fcf767092fe578dc213a6df6e91",
      "the tar's bytes changed: every published bundle's tar would differ",
    );
  });

  it("two builds with the same inputs are byte-identical", () => {
    const first = buildBundle(inputs());
    const second = buildBundle(inputs());

    assert.equal(first.name, bundleName(VERSION));
    assert.ok(first.bytes.length > 1000);
    assert.deepEqual(first.bytes, second.bytes);
  });

  it("gzip's header has no name, mtime 0 and OS byte 3", () => {
    const { bytes } = buildBundle(inputs());

    assert.deepEqual([...bytes.subarray(0, 4)], [0x1f, 0x8b, 8, 0], "magic, deflate, no flags");
    assert.deepEqual([...bytes.subarray(4, 8)], [0, 0, 0, 0], "mtime");
    assert.equal(bytes[9], 3, "Unix");
  });

  it("every entry is 0:0, 0644 or 0755, dated created, in byte order, as ustar", () => {
    const listed = entries(gunzipSync(buildBundle(inputs()).bytes));
    const names = listed.map((entry) => entry.name);

    assert.deepEqual(
      names,
      [...names].sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b))),
    );
    for (const entry of listed) {
      const folder = entry.name.endsWith("/");
      assert.equal(entry.type, folder ? "5" : "0", entry.name);
      assert.equal(entry.mode, folder ? 0o755 : 0o644, entry.name);
      assert.equal(entry.uid, 0, entry.name);
      assert.equal(entry.gid, 0, entry.name);
      assert.equal(entry.uname, "", entry.name);
      assert.equal(entry.gname, "", entry.name);
      assert.equal(entry.prefix, "", entry.name);
      assert.equal(entry.magic, "ustar", entry.name);
      assert.equal(entry.version, "00", entry.name);
      assert.equal(entry.mtime, Date.parse(CREATED) / 1000, entry.name);
    }
  });

  it("the system's tar lists exactly its entries", (context) => {
    const dir = temporary();
    const bundle = buildBundle(inputs());
    writeFileSync(join(dir, bundle.name), bundle.bytes);
    const listed = spawnSync("tar", ["-tzf", bundle.name], { cwd: dir, encoding: "utf8" });
    if (listed.error !== undefined) {
      context.skip("no tar on PATH");
      return;
    }

    assert.equal(listed.status, 0, listed.stderr);
    assert.deepEqual(
      listed.stdout.split(/\r?\n/).filter((line) => line !== ""),
      entries(gunzipSync(bundle.bytes)).map((entry) => entry.name),
    );
  });
});

describe("what it ships", () => {
  it("ships exactly the allowlist and RELEASE, each file as the repository holds it", () => {
    const listed = entries(gunzipSync(buildBundle(inputs()).bytes));
    const files = listed.filter((entry) => !entry.name.endsWith("/"));

    assert.deepEqual(
      files.map((entry) => entry.name).sort(),
      [...BUNDLE_FILES.map(([name]) => name), "RELEASE"].sort(),
    );
    for (const entry of files) {
      const source = BUNDLE_FILES.find(([name]) => name === entry.name)?.[1];
      if (source !== undefined && !source.endsWith(".yaml")) {
        assert.deepEqual(entry.data, readFileSync(join(ROOT, source)), entry.name);
      }
    }
    assert.deepEqual(
      listed.filter((entry) => entry.name.endsWith("/")).map((entry) => entry.name),
      ["host/", "prod/"],
    );
  });

  it("names every other entry of deploy/ as for development only", () => {
    const shipped = new Set(
      BUNDLE_FILES.map(([, source]) => source)
        .filter((source) => source.startsWith("deploy/"))
        .map((source) => source.slice("deploy/".length).split("/")[0]),
    );
    for (const entry of readdirSync(join(ROOT, "deploy"))) {
      assert.ok(
        shipped.has(entry) !== DEVELOPMENT_ONLY.includes(entry),
        `deploy/${entry}: shipped or development-only, not both or neither`,
      );
    }
    for (const folder of ["host", "prod"]) {
      for (const file of readdirSync(join(ROOT, "deploy", folder))) {
        assert.ok(
          BUNDLE_FILES.some(([name]) => name === `${folder}/${file}`),
          `deploy/${folder}/${file}`,
        );
      }
    }
  });

  it("ships every file the smoke tests copy", () => {
    const stack = readFileSync(
      join(ROOT, "crates/fleet-integration/tests/integration/smoke/stack.rs"),
      "utf8",
    );
    const copied = [
      ...stack.match(/const DEPLOY_FILES[^=]*= \[([^\]]*)\]/s)[1].matchAll(/"([^"]+)"/g),
    ].map((found) => found[1]);

    assert.ok(copied.length > 5, copied.join(", "));
    for (const file of copied) {
      assert.ok(
        BUNDLE_FILES.some(([name]) => name === file),
        file,
      );
    }
  });

  it("RELEASE lists version, updates_from, both images and each file's SHA-256", () => {
    const bundle = buildBundle(inputs());
    const listed = entries(gunzipSync(bundle.bytes));
    const release = listed.find((entry) => entry.name === "RELEASE").data.toString("utf8");
    const lines = release.split("\n");

    assert.match(lines[0], /^# afkfleet's release files/);
    assert.deepEqual(lines.slice(1, 5), [
      `version=${VERSION}`,
      `updates_from=${updatesFrom(readFileSync(join(ROOT, "Cargo.toml"), "utf8"))}`,
      `server_image=ghcr.io/${OWNER}/afkfleet-server:${VERSION}@${SERVER}`,
      `agent_image=ghcr.io/${OWNER}/afkfleet-agent:${VERSION}@${AGENT}`,
    ]);
    const files = listed.filter((entry) => !entry.name.endsWith("/") && entry.name !== "RELEASE");
    assert.deepEqual(
      lines.slice(5).filter((line) => line !== ""),
      files.map((entry) => `file=${sha256(entry.data)}  ${entry.name}`),
    );
    assert.ok(release.endsWith("\n"));
    assert.equal(bundle.release, release);
  });
});

describe("the image lines", () => {
  const LINE = `    image: \${AFKFLEET_SERVER_IMAGE:?set AFKFLEET_SERVER_IMAGE in .env}`;
  const refs = {
    server: "ghcr.io/o/afkfleet-server:1.2.3@sha256:1",
    agent: "ghcr.io/o/afkfleet-agent:1.2.3@sha256:2",
  };

  it("every server and agent image line gets the release's digest as its default", () => {
    const bundle = buildBundle(inputs());
    const listed = entries(gunzipSync(bundle.bytes));
    const text = (name) => listed.find((entry) => entry.name === name).data.toString("utf8");
    const server = `\${AFKFLEET_SERVER_IMAGE:-ghcr.io/${OWNER}/afkfleet-server:${VERSION}@${SERVER}}`;
    const agent = `\${AFKFLEET_AGENT_IMAGE:-ghcr.io/${OWNER}/afkfleet-agent:${VERSION}@${AGENT}}`;

    const images = (name) =>
      text(name)
        .split("\n")
        .filter((line) => line.trim().startsWith("image:"))
        .map((line) => line.trim());
    assert.deepEqual(
      images("compose.yaml").filter((line) => !line.includes("caddy:")),
      [`image: ${server}`, `image: ${agent}`, `image: ${server}`, `image: ${server}`],
    );
    assert.deepEqual(images("compose.agent.yaml"), [`image: ${agent}`]);
    for (const name of ["compose.yaml", "compose.agent.yaml"]) {
      assert.ok(!text(name).includes(":?set AFKFLEET_"), name);
    }
  });

  it("rewrites only the exact form, keeping its indent", () => {
    assert.equal(
      rewriteImages(`a:\n${LINE}\n`, "compose.yaml", refs),
      `a:\n    image: \${AFKFLEET_SERVER_IMAGE:-ghcr.io/o/afkfleet-server:1.2.3@sha256:1}\n`,
    );
  });

  it("refuses any other mention of the image variables, outside comments", () => {
    for (const line of [
      `    image: \${AFKFLEET_SERVER_IMAGE}`,
      `    image: \${AFKFLEET_SERVER_IMAGE:-afkfleet-server:dev}`,
      `    image: \${AFKFLEET_AGENT_IMAGE:?set AFKFLEET_SERVER_IMAGE in .env}`,
      "      AFKFLEET_AGENT_IMAGE: x",
    ]) {
      assert.throws(() => rewriteImages(`${line}\n`, "compose.yaml", refs), /AFKFLEET_/, line);
    }
    assert.equal(
      rewriteImages("# AFKFLEET_AGENT_IMAGE the agent's image\n", "compose.yaml", refs),
      "# AFKFLEET_AGENT_IMAGE the agent's image\n",
    );
  });

  it("refuses an env.example that sets an image", () => {
    const root = copyRoot();
    const file = join(root, "deploy", "env.example");
    writeFileSync(file, `${readFileSync(file, "utf8")}AFKFLEET_AGENT_IMAGE=afkfleet-agent:dev\n`);

    assert.throws(() => buildBundle(inputs(root)), /env\.example sets AFKFLEET_AGENT_IMAGE/);
  });
});

describe("its refusals", () => {
  it("refuses a link, a folder, a CR and an extra file in host/ or prod/", (context) => {
    const cases = [
      [
        "a CR",
        (root) => {
          const file = join(root, "deploy", "setup.sh");
          writeFileSync(file, readFileSync(file, "utf8").replace("\n", "\r\n"));
        },
        /setup\.sh holds a CR/,
      ],
      [
        "a folder where a file is listed",
        (root) => {
          rmSync(join(root, "deploy", "Caddyfile"));
          mkdirSync(join(root, "deploy", "Caddyfile"));
        },
        /Caddyfile isn't a regular file/,
      ],
      [
        "an extra file in prod/",
        (root) => writeFileSync(join(root, "deploy", "prod", "extra.toml"), "x = 1\n"),
        /deploy\/prod\/extra\.toml isn't on the bundle's list/,
      ],
      [
        "an extra file in host/",
        (root) => writeFileSync(join(root, "deploy", "host", "extra.sh"), "exit 0\n"),
        /deploy\/host\/extra\.sh isn't on the bundle's list/,
      ],
    ];
    for (const [label, change, refusal] of cases) {
      const root = copyRoot();
      change(root);
      assert.throws(() => buildBundle(inputs(root)), refusal, label);
    }
    const root = copyRoot();
    const target = join(root, "deploy", "Caddyfile");
    rmSync(target);
    try {
      symlinkSync(join(root, "deploy", "env.example"), target);
    } catch {
      context.diagnostic("no symbolic links here: the link case is CI's");
      return;
    }
    assert.throws(() => buildBundle(inputs(root)), /Caddyfile is a link/);
  });

  it("refuses a link anywhere on a shipped file's path: deploy/prod or deploy as a link", () => {
    for (const folder of ["deploy/prod", "deploy/host", "deploy"]) {
      const root = copyRoot();
      const elsewhere = join(temporary(), "moved");
      cpSync(join(root, folder), elsewhere, { recursive: true });
      rmSync(join(root, folder), { recursive: true });
      symlinkSync(elsewhere, join(root, folder), "junction");

      assert.throws(() => buildBundle(inputs(root)), /is a link/, folder);
    }
  });

  it("refuses a name over 99 bytes", () => {
    assert.throws(
      () => tarOf([{ name: `${"a".repeat(100)}`, data: Buffer.from("") }], 0),
      /99 bytes/,
    );
  });

  it("refuses a version other than the workspace's", () => {
    assert.throws(() => buildBundle(inputs(ROOT, "9.9.9")), /isn't the workspace's version/);
  });

  it("reads updates_from, and refuses one that's malformed or above the version", () => {
    const toml = (value) =>
      `[workspace.package]\nversion = "0.3.0"\n\n[workspace.metadata.afkfleet]\nupdates_from = "${value}"\n`;

    assert.equal(updatesFrom(toml("0.2.0")), "0.2.0");
    for (const value of ["0.2", "v0.2.0", "0.02.0", ""]) {
      assert.throws(() => updatesFrom(toml(value)), /updates_from/, value);
    }
    assert.throws(() => updatesFrom(toml("0.3.1")), /above the workspace's version/);
    assert.throws(() => updatesFrom('[workspace.package]\nversion = "0.3.0"\n'), /updates_from/);
  });

  it("updates_from is X.Y.Z and not above the workspace version in this repository", () => {
    const found = updatesFrom(readFileSync(join(ROOT, "Cargo.toml"), "utf8"));

    assert.match(found, /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/);
  });
});

describe("the command", () => {
  /** Runs main with `args` and GITHUB_REPOSITORY_OWNER, capturing its output. */
  function run(args, owner = "Noah-Gabel") {
    let out = "";
    let err = "";
    const code = main(args, {
      env: { GITHUB_REPOSITORY_OWNER: owner },
      root: ROOT,
      out: { write: (text) => (out += text) },
      err: { write: (text) => (err += text) },
    });
    return { code, out, err };
  }

  it("writes the bundle into the folder and prints its SHA-256 and name alone", () => {
    const dir = temporary();

    const { code, out, err } = run([VERSION, CREATED, SERVER, AGENT, dir]);

    assert.equal(code, 0, err);
    const bytes = readFileSync(join(dir, bundleName(VERSION)));
    assert.equal(out, `${sha256(bytes)}  ${bundleName(VERSION)}\n`);
    const release = entries(gunzipSync(bytes)).find((entry) => entry.name === "RELEASE");
    assert.match(release.data.toString(), /^server_image=ghcr\.io\/noah-gabel\//m);
  });

  it("never writes over a bundle that exists", () => {
    const dir = temporary();
    writeFileSync(join(dir, bundleName(VERSION)), "old");

    const { code, err } = run([VERSION, CREATED, SERVER, AGENT, dir]);

    assert.equal(code, 1);
    assert.match(err, /exists already/);
    assert.equal(readFileSync(join(dir, bundleName(VERSION)), "utf8"), "old");
  });

  it("refuses wrong arguments as a usage error", () => {
    const dir = temporary();
    for (const [label, args, owner] of [
      ["too few", [VERSION, CREATED, SERVER, AGENT], undefined],
      ["a version", ["1.0", CREATED, SERVER, AGENT, dir], undefined],
      ["a created time", [VERSION, "2026-11-07", SERVER, AGENT, dir], undefined],
      ["a digest", [VERSION, CREATED, "sha256:abc", AGENT, dir], undefined],
      ["an owner", [VERSION, CREATED, SERVER, AGENT, dir], "-bad"],
    ]) {
      const { code, err } = run(args, owner);
      assert.equal(code, 2, `${label}: ${err}`);
    }
  });

  it("runs as a script with its exit code", () => {
    const result = spawnSync(process.execPath, [join(ROOT, "scripts", "deploy-bundle.mjs")], {
      encoding: "utf8",
    });

    assert.equal(result.status, 2);
    assert.match(result.stderr, /usage/);
  });
});
