// The update's proof's fixture (Plan.md P12.5; ADR-0022, S24, R19): a throwaway release, made at
// test time and never committed.
//
//   node scripts/updater-fixture.mjs <empty folder> <http://127.0.0.1:port> <X.Y.Z>
//
// The desktop crate's tests/updater.rs runs it before it serves the folder on loopback and drives
// the real updater plugin against it. It makes two key pairs with the pinned Tauri CLI (`--ci`, an
// empty password) in a temporary folder of its own, outside <folder>, which it removes again; it
// takes no key path, so it can never use a real key. Into <folder> it writes:
// - `afkfleet_<X.Y.Z>_x64-setup.exe`: random bytes standing in for the installer;
// - `pubkey`: the first pair's public key, as tauri.conf.json would hold it;
// - `latest.json`: the first key's signature, bound to the version, as `just updater-sign`
//   writes it (signed through the same functions);
// - `unversioned.json`: the first key's signature without a version;
// - `other.json`: the second key's signature, bound to the version.
// Each manifest's address is <http://127.0.0.1:port>/ and the installer's name. It prints
// nothing, and never a private key; exit 0 done, 1 refused or failed, 2 usage error.
// Tests: scripts/updater-fixture.test.mjs.
import { spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import {
  copyFileSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

import { installerName } from "./app-installer.mjs";
import { manifest, signFile } from "./updater-sign.mjs";

/** The files the fixture writes for `version`. */
export function files(version) {
  return [installerName(version), "pubkey", "latest.json", "unversioned.json", "other.json"];
}

/** A key pair made by the CLI at `cli` in `dir`, empty password; its key's path and public key. */
function generate(cli, dir, name) {
  const key = path.join(dir, name);
  const result = spawnSync(process.execPath, [cli, "signer", "generate", "--ci", "-w", key], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (result.status !== 0) {
    throw new Error(`the CLI couldn't make a key pair: ${result.stderr}`);
  }
  return { key, pubkey: readFileSync(`${key}.pub`, "utf8").trim() };
}

/** The signature of `file`, signed by `key` (bound to `version` unless null). */
function signature(cli, key, file, version) {
  const signed = signFile({ cli, key, file, version, password: null });
  if (signed.status !== 0) {
    throw new Error(`the CLI couldn't sign: ${signed.stderr}`);
  }
  return readFileSync(`${file}.sig`, "utf8");
}

/** Writes the fixture into `out`; returns the exit code. */
function main(args) {
  if (args.length !== 3) {
    process.stderr.write("usage: node scripts/updater-fixture.mjs <empty folder> <base> <X.Y.Z>\n");
    return 2;
  }
  const [out, base, version] = args;
  if (!/^http:\/\/127\.0\.0\.1:\d+$/.test(base)) {
    process.stderr.write(
      "the base must be http://127.0.0.1:<port>: the fixture is loopback only\n",
    );
    return 2;
  }
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version)) {
    process.stderr.write("the version must be X.Y.Z\n");
    return 2;
  }
  let empty;
  try {
    empty = statSync(out).isDirectory() && readdirSync(out).length === 0;
  } catch {
    empty = false;
  }
  if (!empty) {
    process.stderr.write(`${out} must be an empty folder\n`);
    return 1;
  }
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  const cli = createRequire(path.join(root, "apps", "desktop", "package.json")).resolve(
    "@tauri-apps/cli/tauri.js",
  );
  const keys = mkdtempSync(path.join(tmpdir(), "afkfleet-fixture-keys-"));
  try {
    const one = generate(cli, keys, "one.key");
    const two = generate(cli, keys, "two.key");
    const name = installerName(version);
    // Signed beside the keys, so no .sig lands in <out>.
    const signed = path.join(keys, name);
    writeFileSync(signed, randomBytes(64 * 1024));
    copyFileSync(signed, path.join(out, name));
    const url = `${base}/${name}`;
    const now = new Date();
    const write = (file, sig) =>
      writeFileSync(
        path.join(out, file),
        `${JSON.stringify(manifest({ version, signature: sig, url, now }), null, 2)}\n`,
      );
    write("latest.json", signature(cli, one.key, signed, version));
    write("unversioned.json", signature(cli, one.key, signed, null));
    write("other.json", signature(cli, two.key, signed, version));
    writeFileSync(path.join(out, "pubkey"), `${one.pubkey}\n`);
    return 0;
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    return 1;
  } finally {
    rmSync(keys, { recursive: true, force: true });
  }
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = main(process.argv.slice(2));
}
