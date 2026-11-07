// The host's release files (Plan.md P12.6; ADR-0022, S23, S31, S33, S34): the release workflow's
// `bundle` job runs it, with `contents: read` alone, and its file reaches the draft release as an
// artifact. It imports only Node's own modules: the job checks out this file, deploy/, LICENSE and
// Cargo.toml, and nothing else.
//
//   node scripts/deploy-bundle.mjs <X.Y.Z> <created> <server digest> <agent digest> <out dir>
//
// It writes `<out dir>/afkfleet-deploy-X.Y.Z.tar.gz`, never over a file, and prints its SHA-256
// and name on stdout, as SHA256SUMS lists them. The archive holds the production files alone, at
// its root (an operator unpacks it into the folder that holds compose.yaml), and RELEASE:
// - `version=`, `updates_from=` (Cargo.toml's `[workspace.metadata.afkfleet]`), the two images
//   by their index digests, and `file=<sha256>  <name>` for every other file, which
//   `sh setup.sh fetch` checks before it renames anything into place;
// - every `image: ${AFKFLEET_SERVER_IMAGE:?…}` line becomes `${AFKFLEET_SERVER_IMAGE:-<the
//   release's image>}` (the agent's alike), so `.env` needs no image line; the repository's files
//   keep `:?`, so the smoke tests still name `:dev` in their `.env`.
//
// Reproducible: the tar is a ustar of our own, its entries in byte order, 0:0 with no names,
// files 0644 and folders 0755 (the runbook runs `sh setup.sh`), every entry dated `created`, so
// the same inputs give the same tar on every platform; gzip's header carries no name or time and
// says Unix. It refuses a link, anything but a regular file, a CR, a name over 99 bytes, and a
// file in deploy/host/ or deploy/prod/ that isn't on its list.
//
// Exit codes: 0 done, 1 refused (it says why on stderr), 2 usage error.
// Tests: scripts/deploy-bundle.test.mjs.
import { createHash } from "node:crypto";
import { lstatSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";
import { gzipSync } from "node:zlib";

/**
 * Every file the bundle ships, as (its name in the bundle, its path in the repository), in the
 * archive's order. A new production file joins this list or DEVELOPMENT_ONLY.
 */
export const BUNDLE_FILES = [
  ["Caddyfile", "deploy/Caddyfile"],
  ["LICENSE", "LICENSE"],
  ["compose.agent.yaml", "deploy/compose.agent.yaml"],
  ["compose.caddy.yaml", "deploy/compose.caddy.yaml"],
  ["compose.edge.yaml", "deploy/compose.edge.yaml"],
  ["compose.grpc.yaml", "deploy/compose.grpc.yaml"],
  ["compose.loopback.yaml", "deploy/compose.loopback.yaml"],
  ["compose.yaml", "deploy/compose.yaml"],
  ["env.example", "deploy/env.example"],
  ["files.sh", "deploy/files.sh"],
  ["host/sftp-backups.sh", "deploy/host/sftp-backups.sh"],
  ["prod/agent.remote.toml", "deploy/prod/agent.remote.toml"],
  ["prod/agent.toml", "deploy/prod/agent.toml"],
  ["prod/server.toml", "deploy/prod/server.toml"],
  ["setup.sh", "deploy/setup.sh"],
];

/** The entries of deploy/ that are for development alone, never shipped. */
export const DEVELOPMENT_ONLY = [
  "compose.dev.yaml",
  "compose.dod.yaml",
  "compose.isolated.yaml",
  "dev",
  "dod",
  "docker",
];

/** The folders of deploy/ whose every file must be on the list. */
const LISTED_FOLDERS = ["host", "prod"];

const VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const DIGEST = /^sha256:[0-9a-f]{64}$/;
const OWNER = /^[a-z0-9](?:[a-z0-9-]{0,37}[a-z0-9])?$/;
const CREATED = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/;
/** The one form of an image line the bundle rewrites (S34). */
const IMAGE_LINE =
  /^( +)image: \$\{AFKFLEET_(SERVER|AGENT)_IMAGE:\?set AFKFLEET_\2_IMAGE in \.env\}$/;
/** The image variables, which nothing else may mention outside a comment. */
const IMAGE_VARIABLES = /AFKFLEET_(SERVER|AGENT)_IMAGE/;

/** A refusal: the bundle can't be built (exit 1). */
class Refusal extends Error {}
/** A usage error: the workflow called the script wrongly (exit 2). */
class Usage extends Error {}

/** The bundle's file name for `version`: release.mjs's, which a test holds equal. */
export function bundleName(version) {
  return `afkfleet-deploy-${version}.tar.gz`;
}

/** A TOML `[section]`'s text, up to the next section, or undefined. */
function section(toml, name) {
  return toml.split(/^\[/m).find((part) => part.startsWith(`${name}]`));
}

/**
 * Cargo.toml's `[workspace.package] version` when it's X.Y.Z, else null: release.mjs's, which a
 * test holds equal. A copy, so this file imports nothing of the repository's.
 */
export function workspaceVersion(cargoToml) {
  const version = section(cargoToml, "workspace.package")?.match(/^version = "([^"]*)"$/m)?.[1];
  return version !== undefined && VERSION.test(version) ? version : null;
}

/** X.Y.Z's three numbers, compared part by part: below 0, equal 0, above a positive number. */
function compare(a, b) {
  const [left, right] = [a, b].map((version) => version.split(".").map(Number));
  for (let at = 0; at < 3; at += 1) {
    if (left[at] !== right[at]) {
      return left[at] - right[at];
    }
  }
  return 0;
}

/**
 * Cargo.toml's `[workspace.metadata.afkfleet] updates_from`: the oldest release a host can update
 * from with `sh setup.sh fetch` (S33). It must be X.Y.Z and not above the workspace's version.
 */
export function updatesFrom(cargoToml) {
  const found = section(cargoToml, "workspace.metadata.afkfleet")?.match(
    /^updates_from = "([^"]*)"$/m,
  )?.[1];
  if (found === undefined || !VERSION.test(found)) {
    throw new Refusal(
      "Cargo.toml's [workspace.metadata.afkfleet] updates_from is missing or isn't X.Y.Z",
    );
  }
  const version = workspaceVersion(cargoToml);
  if (version === null) {
    throw new Refusal("Cargo.toml's [workspace.package] version isn't X.Y.Z");
  }
  if (compare(found, version) > 0) {
    throw new Refusal(`updates_from ${found} is above the workspace's version ${version}`);
  }
  return found;
}

/**
 * `text` of the compose file `name` with each image line's default set to the release's image
 * (`refs.server`, `refs.agent`). Any other mention of the image variables outside a comment is
 * refused, so no line is left that a host's `.env` would have to fill.
 */
export function rewriteImages(text, name, refs) {
  return text
    .split("\n")
    .map((line) => {
      const found = IMAGE_LINE.exec(line);
      if (found !== null) {
        const [, indent, which] = found;
        const image = which === "SERVER" ? refs.server : refs.agent;
        return `${indent}image: \${AFKFLEET_${which}_IMAGE:-${image}}`;
      }
      if (!line.trimStart().startsWith("#") && IMAGE_VARIABLES.test(line)) {
        throw new Refusal(
          `${name} names an image variable in a form the bundle can't set: ${line}`,
        );
      }
      return line;
    })
    .join("\n");
}

/** Writes `text` into `block` at `offset`, at most `length` bytes, NUL-padded. */
function field(block, offset, length, text) {
  const bytes = Buffer.from(text, "utf8");
  if (bytes.length > length) {
    throw new Refusal(`a tar field is too long: ${text}`);
  }
  bytes.copy(block, offset);
}

/** Writes `value` as octal, zero-padded to `length - 1` digits and a NUL. */
function octal(block, offset, length, value) {
  field(block, offset, length, `${value.toString(8).padStart(length - 1, "0")}\0`);
}

/** One ustar header: a folder when `name` ends in `/`, else a file of `size` bytes. */
function header(name, size, mtime) {
  if (Buffer.byteLength(name) > 99) {
    throw new Refusal(`${name}: a name over 99 bytes`);
  }
  const folder = name.endsWith("/");
  const block = Buffer.alloc(512);
  field(block, 0, 100, name);
  octal(block, 100, 8, folder ? 0o755 : 0o644);
  octal(block, 108, 8, 0);
  octal(block, 116, 8, 0);
  octal(block, 124, 12, size);
  octal(block, 136, 12, mtime);
  block.fill(0x20, 148, 156);
  field(block, 156, 1, folder ? "5" : "0");
  field(block, 257, 6, "ustar\0");
  field(block, 263, 2, "00");
  octal(block, 329, 8, 0);
  octal(block, 337, 8, 0);
  const sum = block.reduce((total, byte) => total + byte, 0);
  field(block, 148, 8, `${sum.toString(8).padStart(6, "0")}\0 `);
  return block;
}

/**
 * A ustar archive of `entries` ({ name, data } for a file, { name } ending in `/` for a folder),
 * in the order given, each dated `mtime` (Unix seconds), ending in two empty blocks.
 */
export function tarOf(entries, mtime) {
  const blocks = [];
  for (const { name, data } of entries) {
    const bytes = data ?? Buffer.alloc(0);
    blocks.push(header(name, bytes.length, mtime), bytes);
    const rest = bytes.length % 512;
    if (rest !== 0) {
      blocks.push(Buffer.alloc(512 - rest));
    }
  }
  blocks.push(Buffer.alloc(1024));
  return Buffer.concat(blocks);
}

/** `tar` gzipped, its header with no name, mtime 0 and the OS byte set to Unix. */
function gzip(tar) {
  const bytes = gzipSync(tar, { level: 9 });
  if (bytes[0] !== 0x1f || bytes[1] !== 0x8b || bytes[3] !== 0 || bytes.readUInt32LE(4) !== 0) {
    throw new Refusal("gzip wrote a header with a name, flags or a time");
  }
  // Node's zlib writes the OS it was built for (Windows differs); the archive says Unix.
  bytes[9] = 3;
  return bytes;
}

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

/** A shipped file's bytes from `root`, refused unless it's a regular file without a CR. */
function readShipped(root, source) {
  const file = path.join(root, source);
  const stat = lstatSync(file);
  if (stat.isSymbolicLink()) {
    throw new Refusal(`${source} is a link`);
  }
  if (!stat.isFile()) {
    throw new Refusal(`${source} isn't a regular file`);
  }
  const bytes = readFileSync(file);
  if (bytes.includes(0x0d)) {
    throw new Refusal(`${source} holds a CR: the repository's files are LF only`);
  }
  return bytes;
}

/** Refuses a file in deploy/host/ or deploy/prod/ that isn't on the list. */
function checkListedFolders(root) {
  for (const folder of LISTED_FOLDERS) {
    for (const file of readdirSync(path.join(root, "deploy", folder))) {
      const name = `${folder}/${file}`;
      if (!BUNDLE_FILES.some(([listed]) => listed === name)) {
        throw new Refusal(`deploy/${name} isn't on the bundle's list: add it, or move it out`);
      }
    }
  }
}

/**
 * The bundle for `version` from `root`'s files: { name, bytes, release }, `release` being
 * RELEASE's text. Refuses a version other than Cargo.toml's, and anything the module docs say.
 */
export function buildBundle({ root, version, created, serverDigest, agentDigest, owner }) {
  const cargoToml = readFileSync(path.join(root, "Cargo.toml"), "utf8");
  if (workspaceVersion(cargoToml) !== version) {
    throw new Refusal(`${version} isn't the workspace's version in Cargo.toml`);
  }
  const from = updatesFrom(cargoToml);
  checkListedFolders(root);
  const refs = {
    server: `ghcr.io/${owner}/afkfleet-server:${version}@${serverDigest}`,
    agent: `ghcr.io/${owner}/afkfleet-agent:${version}@${agentDigest}`,
  };
  const files = BUNDLE_FILES.map(([name, source]) => {
    let bytes = readShipped(root, source);
    if (name.endsWith(".yaml")) {
      bytes = Buffer.from(rewriteImages(bytes.toString("utf8"), name, refs), "utf8");
    }
    if (name === "env.example") {
      const set = bytes
        .toString("utf8")
        .split("\n")
        .find((line) => /^AFKFLEET_(SERVER|AGENT)_IMAGE=/.test(line));
      if (set !== undefined) {
        throw new Refusal(`env.example sets ${set.split("=")[0]}: the bundle's images are its own`);
      }
    }
    return { name, data: bytes };
  });
  const release = [
    "# afkfleet's release files (ADR-0022, S23), written by scripts/deploy-bundle.mjs. Never edit it.",
    `version=${version}`,
    `updates_from=${from}`,
    `server_image=${refs.server}`,
    `agent_image=${refs.agent}`,
    ...files.map(({ name, data }) => `file=${sha256(data)}  ${name}`),
    "",
  ].join("\n");
  const folders = [
    ...new Set(
      files.filter(({ name }) => name.includes("/")).map(({ name }) => `${name.split("/")[0]}/`),
    ),
  ].map((name) => ({ name }));
  const entries = [
    ...folders,
    ...files,
    { name: "RELEASE", data: Buffer.from(release, "utf8") },
  ].sort((a, b) => Buffer.compare(Buffer.from(a.name), Buffer.from(b.name)));
  const mtime = Date.parse(created) / 1000;
  return { name: bundleName(version), bytes: gzip(tarOf(entries, mtime)), release };
}

/** The command; answers its exit code. `io`: { env, root, out, err }. */
export function main(args, io) {
  try {
    if (args.length !== 5) {
      throw new Usage(
        "usage: node scripts/deploy-bundle.mjs <X.Y.Z> <created> <server digest> <agent digest> <out dir>",
      );
    }
    const [version, created, serverDigest, agentDigest, out] = args;
    const owner = (io.env.GITHUB_REPOSITORY_OWNER ?? "").toLowerCase();
    if (!VERSION.test(version)) {
      throw new Usage(`${version} isn't X.Y.Z`);
    }
    if (!CREATED.test(created) || Number.isNaN(Date.parse(created))) {
      throw new Usage(`${created} isn't the guard's created time (YYYY-MM-DDTHH:MM:SSZ)`);
    }
    if (!DIGEST.test(serverDigest) || !DIGEST.test(agentDigest)) {
      throw new Usage("each digest must be sha256:<64 hex>");
    }
    if (!OWNER.test(owner)) {
      throw new Usage("GITHUB_REPOSITORY_OWNER isn't a GitHub owner's name");
    }
    const bundle = buildBundle({
      root: io.root,
      version,
      created,
      serverDigest,
      agentDigest,
      owner,
    });
    try {
      writeFileSync(path.join(out, bundle.name), bundle.bytes, { flag: "wx" });
    } catch (error) {
      throw new Refusal(
        error.code === "EEXIST"
          ? `${bundle.name} exists already in ${out}: nothing is written over`
          : `${bundle.name} can't be written: ${error.code ?? error.message}`,
      );
    }
    io.out.write(`${sha256(bundle.bytes)}  ${bundle.name}\n`);
    return 0;
  } catch (error) {
    if (error instanceof Usage) {
      io.err.write(`deploy-bundle: ${error.message}\n`);
      return 2;
    }
    if (error instanceof Refusal) {
      io.err.write(`deploy-bundle: ${error.message}\n`);
      return 1;
    }
    throw error;
  }
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = main(process.argv.slice(2), {
    env: process.env,
    root: fileURLToPath(new URL("..", import.meta.url)),
    out: process.stdout,
    err: process.stderr,
  });
}
