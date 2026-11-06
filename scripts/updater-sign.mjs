// The maintainer's signing step for a release's installer (Plan.md P12.5; ADR-0022, S24, U1):
// on their own PC only, never in CI, never by the AI.
//
//   just updater-sign <key> <installer> <sha256> <version>
//
// - <key>: the updater's private key file, an absolute path outside this repository. This
//   script never opens it: the pinned Tauri CLI reads it.
// - <installer>: the absolute path of the `afkfleet_<version>_x64-setup.exe` downloaded from the
//   draft release.
// - <sha256>: the installer's SHA-256 as the release workflow printed it.
// - <version>: the release's X.Y.Z, the tag without its `v`.
//
// It refuses before anything runs when an argument doesn't fit, the key lies inside the
// repository, stdin isn't a terminal, or `target/updater-sign/v<version>/` exists. It copies the
// installer there and checks the copy's SHA-256, then asks for the key's password, hidden, and
// hands it to the CLI in that one child process's environment, never on a command line
// (`signer sign --private-key-path <key> --app-version <version>`, so the signature carries the
// version the app's `requireSignedVersion` checks). It drops the CLI's own output, checks that
// the signature is by the committed public key and names the file and the version, and writes
// latest.json beside it. stdout holds the signature file's path alone; stderr what to do next.
// On any refusal after the folder was made, it removes the folder again.
//
// Exit codes: 0 signed, 1 refused or failed (it says why), 2 usage error. Needs `pnpm install`
// first, for the CLI. Tests: scripts/updater-sign.test.mjs, with throwaway keys only; the
// update's proof (apps/desktop/src-tauri/tests/updater.rs) signs through these same functions,
// by way of scripts/updater-fixture.mjs.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFileSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

import { installerName, SIGNING_VARIABLES } from "./app-installer.mjs";

/** How long the CLI may take to sign, the password already given. */
const SIGN_TIMEOUT_MS = 120_000;

/** The manifest's key for the one platform afkfleet ships. */
const PLATFORM = "windows-x86_64";

/** An X.Y.Z version. */
const VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

/** The 8 bytes of a minisign key ID, as minisign prints them: little-endian, upper-case hex. */
function idHex(bytes) {
  return Buffer.from(bytes).reverse().toString("hex").toUpperCase();
}

/** The key ID of a Tauri public key: base64 of minisign's public key file. */
export function keyId(pubkey) {
  const lines = Buffer.from(pubkey.trim(), "base64").toString("utf8").split("\n");
  const raw = Buffer.from(lines[1] ?? "", "base64");
  return raw.length === 42 ? idHex(raw.subarray(2, 10)) : "";
}

/**
 * What a `.sig` file says: its key ID, and the trusted comment's file and version (null when it
 * carries none). The file is base64 of minisign's signature box.
 */
export function signatureFacts(text) {
  const lines = Buffer.from(text.trim(), "base64").toString("utf8").split("\n");
  const raw = Buffer.from(lines[1] ?? "", "base64");
  const trusted = (lines[2] ?? "").replace(/^trusted comment: /, "");
  const fields = Object.fromEntries(
    trusted.split("\t").map((part) => {
      const at = part.indexOf(":");
      return at < 0 ? [part, ""] : [part.slice(0, at), part.slice(at + 1)];
    }),
  );
  return {
    keyId: raw.length === 74 ? idHex(raw.subarray(2, 10)) : "",
    file: fields.file ?? null,
    version: fields.version ?? null,
  };
}

/** The release asset's address: that one version's installer, by its tag. */
export function downloadUrl(repository, version) {
  return `${repository}/releases/download/v${version}/${installerName(version)}`;
}

/** latest.json's content: the version, the date in whole seconds, the Windows release. */
export function manifest({ version, signature, url, now }) {
  return {
    version,
    pub_date: `${now.toISOString().slice(0, 19)}Z`,
    platforms: { [PLATFORM]: { signature, url } },
  };
}

/**
 * Reads a line from the terminal `input` without echoing it: `prompt` to `output`, then every
 * key up to Enter, Backspace taking one back. Ctrl+C rejects.
 */
export function readHidden(prompt, input, output) {
  output.write(prompt);
  input.setRawMode(true);
  input.setEncoding("utf8");
  input.resume();
  return new Promise((resolve, reject) => {
    let text = "";
    const finish = () => {
      input.removeListener("data", onData);
      input.setRawMode(false);
      input.pause();
      output.write("\n");
    };
    const onData = (chunk) => {
      for (const key of chunk) {
        if (key === "\r" || key === "\n") {
          finish();
          resolve(text);
          return;
        }
        if (key === "\u0003") {
          finish();
          reject(new Error("cancelled"));
          return;
        }
        if (key === "\u007f" || key === "\b") {
          text = text.slice(0, -1);
        } else {
          text += key;
        }
      }
    };
    input.on("data", onData);
  });
}

/**
 * Signs `file` with the pinned CLI at `cli`: the key by its path, `version` into the trusted
 * comment unless null, `password` in the child's environment alone (null: none, and stdin isn't a
 * terminal, so the CLI takes an empty one, as throwaway test keys have). Every signing variable
 * of the parent is left out. The CLI's stdout, the signature in base64, is dropped.
 */
export function signFile({ cli, key, file, version, password }) {
  const env = { ...process.env };
  for (const name of SIGNING_VARIABLES) {
    delete env[name];
  }
  if (password !== null) {
    env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD = password;
  }
  const args = [cli, "signer", "sign", "--private-key-path", key];
  if (version !== null) {
    args.push("--app-version", version);
  }
  args.push(file);
  const result = spawnSync(process.execPath, args, {
    env,
    stdio: ["ignore", "pipe", "pipe"],
    encoding: "utf8",
    timeout: SIGN_TIMEOUT_MS,
  });
  return { status: result.status ?? 1, stderr: result.stderr ?? "" };
}

/** `[workspace.package] repository` of Cargo.toml's text. */
function repository(cargoToml) {
  const section = cargoToml.split(/^\[/m).find((part) => part.startsWith("workspace.package]"));
  return section?.match(/^repository = "([^"]*)"$/m)?.[1] ?? null;
}

/** Whether `inner` lies in `outer` (both real paths) or is it. */
function within(outer, inner) {
  const relative = path.relative(outer, inner);
  return relative === "" || (!relative.startsWith("..") && !path.isAbsolute(relative));
}

/** The refusal for `args`, before anything runs, or null. */
function argumentProblem([key, installer, sha256, version], root) {
  if (!VERSION.test(version)) {
    return `the version must be X.Y.Z, the tag without its v: ${version}`;
  }
  if (path.basename(installer) !== installerName(version)) {
    return `the installer must be ${installerName(version)}, as the release workflow named it`;
  }
  if (!/^[0-9a-fA-F]{64}$/.test(sha256)) {
    return "the SHA-256 must be the 64 hex digits the release workflow printed";
  }
  if (!path.isAbsolute(key) || !path.isAbsolute(installer)) {
    return "the key's and the installer's paths must be absolute";
  }
  let realKey;
  try {
    realKey = realpathSync(key);
  } catch {
    return `there's no key at ${key}`;
  }
  if (within(realpathSync(root), realKey)) {
    return "the key lies inside the repository: keep it outside, offline (docs/releasing.md)";
  }
  if (!statSync(realKey).isFile()) {
    return `the key ${key} isn't a file`;
  }
  return null;
}

/**
 * Signs the release's installer. `io` holds the repository's `root`, the CLI's entry `cli`, `out`
 * and `err` with a `write`, `stdinIsTTY`, `now()`, `readPassword(prompt)` and `signFile(options)`.
 */
export async function main(args, io) {
  const { root, out, err } = io;
  if (args.length !== 4) {
    err.write("usage: just updater-sign <key> <installer> <sha256> <version>\n");
    return 2;
  }
  const [key, installer, sha256, version] = args;
  const problem = argumentProblem(args, root);
  if (problem !== null) {
    err.write(`${problem}\n`);
    return 1;
  }
  if (!io.stdinIsTTY) {
    err.write("stdin isn't a terminal: the key's password is typed into one, hidden\n");
    return 1;
  }
  const name = installerName(version);
  const parent = path.join(root, "target", "updater-sign");
  const folder = path.join(parent, `v${version}`);
  mkdirSync(parent, { recursive: true });
  try {
    mkdirSync(folder);
  } catch {
    err.write(`${folder} exists: a version is signed once; remove it only if that run failed\n`);
    return 1;
  }
  const refuse = (text) => {
    rmSync(folder, { recursive: true, force: true });
    err.write(`${text}\n`);
    return 1;
  };
  const copy = path.join(folder, name);
  copyFileSync(installer, copy);
  const sha = createHash("sha256").update(readFileSync(copy)).digest("hex");
  if (sha !== sha256.toLowerCase()) {
    return refuse(`the installer's SHA-256 is ${sha}, not the one given: download it again`);
  }
  let password;
  try {
    password = await io.readPassword(`Password for ${path.basename(key)}: `);
  } catch {
    return refuse("cancelled: nothing was signed");
  }
  const signed = io.signFile({ cli: io.cli, key, file: copy, version, password });
  if (signed.status !== 0) {
    return refuse(`the CLI couldn't sign it: ${signed.stderr.trim()}`);
  }
  const sigPath = `${copy}.sig`;
  let signature;
  try {
    signature = readFileSync(sigPath, "utf8");
  } catch {
    return refuse("the CLI wrote no signature");
  }
  const facts = signatureFacts(signature);
  const config = JSON.parse(
    readFileSync(path.join(root, "apps", "desktop", "src-tauri", "tauri.conf.json"), "utf8"),
  );
  if (facts.keyId === "" || facts.keyId !== keyId(config?.plugins?.updater?.pubkey ?? "")) {
    return refuse(
      "the signature's key isn't the committed public key: installed apps would refuse it",
    );
  }
  if (facts.file !== name || facts.version !== version) {
    return refuse("the signature doesn't name this installer and version");
  }
  const repo = repository(readFileSync(path.join(root, "Cargo.toml"), "utf8"));
  if (repo === null) {
    return refuse("Cargo.toml's [workspace.package] repository is missing");
  }
  const latest = manifest({ version, signature, url: downloadUrl(repo, version), now: io.now() });
  writeFileSync(path.join(folder, "latest.json"), `${JSON.stringify(latest, null, 2)}\n`, {
    flag: "wx",
  });
  out.write(`${sigPath}\n`);
  err.write(
    "signed: upload this .sig and the latest.json beside it to the draft release (docs/releasing.md)\n",
  );
  return 0;
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  const cli = createRequire(path.join(root, "apps", "desktop", "package.json")).resolve(
    "@tauri-apps/cli/tauri.js",
  );
  main(process.argv.slice(2), {
    root,
    cli,
    out: process.stdout,
    err: process.stderr,
    stdinIsTTY: process.stdin.isTTY === true,
    now: () => new Date(),
    readPassword: (prompt) => readHidden(prompt, process.stdin, process.stderr),
    signFile,
  }).then(
    (code) => {
      process.exitCode = code;
    },
    (error) => {
      process.stderr.write(`${error.message}\n`);
      process.exitCode = 1;
    },
  );
}
