// The user's rehearsal of an update on their own PC (Plan.md P12.5; ADR-0022, R20): the user only;
// the AI never runs it.
//
//   just update-rehearsal
//
// It proves the whole update with a throwaway key before any real release: a key pair made by the
// pinned CLI (an empty password) into target/update-rehearsal/, which must not exist yet; two
// debug NSIS installers, 0.0.1 and 0.0.2, of an app of their own ("afkfleet rehearsal", identifier
// io.github.noah-gabel.afkfleet.rehearsal, binary afkfleet-rehearsal), each with the throwaway
// public key and a manifest on this PC's loopback, so they never touch the installed afkfleet: its
// folder, its uninstall entry, its running-app check, its single-instance lock, its profile and its
// Credential Manager entries all go by another name. The app turns updates on in a debug build
// whose updater points only at plain http on loopback, as this one's does (src-tauri/src/update.rs,
// `updates_on`), and the plugin lets an http endpoint through in a debug build. 0.0.2 is signed through `just updater-sign`'s own functions, bound to its
// version, and latest.json names it. Then it serves the manifest and that installer alone on
// 127.0.0.1 until Ctrl+C, and prints the steps.
//
// Exit codes: 0 stopped with Ctrl+C, 1 refused or failed (it says why), 2 usage error. It needs
// `pnpm install` and builds for a few minutes. Tests: scripts/update-rehearsal.test.mjs.
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { createRequire } from "node:module";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

import { manifest, signFile } from "./updater-sign.mjs";

/** The rehearsal's identifier: its own, so it never touches the installed afkfleet. */
export const REHEARSAL_IDENTIFIER = "io.github.noah-gabel.afkfleet.rehearsal";

/** The installed version, and the one it updates to: never the real app's. */
export const FIRST = "0.0.1";
export const SECOND = "0.0.2";

/** The rehearsal's product, as Windows shows it, and its binary. */
const PRODUCT = "afkfleet rehearsal";
const BINARY = "afkfleet-rehearsal";

/** The folder it works in, under the repository's target/. */
const FOLDER = path.join("target", "update-rehearsal");

/** An installer's name in the rehearsal's folder: without the product's space. */
export function installerFile(version) {
  return `${BINARY}_${version}_x64-setup.exe`;
}

/** The configuration laid over tauri.conf.json for `version`, its key and its loopback port. */
export function overlay({ version, pubkey, port }) {
  return {
    identifier: REHEARSAL_IDENTIFIER,
    productName: PRODUCT,
    mainBinaryName: BINARY,
    version,
    plugins: {
      updater: { pubkey, endpoints: [`http://127.0.0.1:${port}/latest.json`] },
    },
  };
}

/** The file served at `requested`, from `files` by name, or null: nothing else is served. */
export function route(requested, files) {
  const name = requested.startsWith("/") ? requested.slice(1) : null;
  return name !== null && Object.hasOwn(files, name) ? files[name] : null;
}

/** What the user does, and what success looks like. */
export function steps({ folder, first, port }) {
  return [
    "",
    `The rehearsal's release is served on http://127.0.0.1:${port}/ (its files: ${folder}).`,
    `1. Run ${first}: it installs for your Windows user, without an admin prompt.`,
    `2. Start "afkfleet rehearsal" from the Start menu. Success: within seconds a row at the top says "afkfleet ${SECOND} is available."`,
    `3. Click "Install and restart". Success: a small installer window shows its progress, the app starts again by itself, and no row offers an update any more; Windows Settings › Apps lists "afkfleet rehearsal" at version ${SECOND}.`,
    '4. Close it and remove it: Windows Settings › Apps › Installed apps › afkfleet rehearsal › Uninstall, with "Delete the application data" ticked.',
    `5. Press Ctrl+C here, then delete ${folder}.`,
    "",
  ].join("\n");
}

/** Runs the pinned Tauri CLI with `args` in the desktop app's folder; its exit status. */
function runPinnedCli(root, args) {
  const appDir = path.join(root, "apps", "desktop");
  const cli = createRequire(path.join(appDir, "package.json")).resolve("@tauri-apps/cli/tauri.js");
  const result = spawnSync(process.execPath, [cli, ...args], { cwd: appDir, stdio: "inherit" });
  return result.error === undefined ? result.status : 1;
}

/** A loopback server for `files`; resolves with it once it listens. */
function listen() {
  const files = {};
  const server = createServer((request, response) => {
    const file = request.method === "GET" ? route(request.url ?? "", files) : null;
    if (file === null) {
      response.writeHead(404, { "content-length": "0" }).end();
      return;
    }
    const body = readFileSync(file);
    response.writeHead(200, { "content-length": String(body.length) }).end(body);
  });
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      resolve({ files, port: typeof address === "object" && address !== null ? address.port : 0 });
    });
  });
}

/**
 * Builds, signs and serves the rehearsal. `io` holds the repository's `root`, `out` and `err`, and
 * may hold `runCli(args)` for the pinned CLI.
 */
export async function main(args, io) {
  const { root, out, err } = io;
  const runCli = io.runCli ?? ((cliArgs) => runPinnedCli(root, cliArgs));
  if (args.length > 0) {
    err.write("usage: just update-rehearsal\n");
    return 2;
  }
  const folder = path.join(root, FOLDER);
  if (existsSync(folder)) {
    err.write(
      `${FOLDER} exists: uninstall an earlier rehearsal's app, delete the folder, and run again\n`,
    );
    return 1;
  }
  mkdirSync(folder, { recursive: true });
  const key = path.join(folder, "throwaway.key");
  if (runCli(["signer", "generate", "--ci", "-w", key]) !== 0) {
    err.write("the CLI couldn't make the throwaway key pair\n");
    return 1;
  }
  const pubkey = readFileSync(`${key}.pub`, "utf8").trim();
  const server = await listen();
  const bundles = path.join(root, "target", "debug", "bundle", "nsis");
  for (const version of [FIRST, SECOND]) {
    const config = path.join(folder, `tauri.rehearsal.${version}.conf.json`);
    writeFileSync(config, JSON.stringify(overlay({ version, pubkey, port: server.port }), null, 2));
    const built = runCli([
      "build",
      "--debug",
      "--bundles",
      "nsis",
      "--no-sign",
      "--config",
      config,
    ]);
    if (built !== 0) {
      err.write(`the rehearsal's ${version} didn't build\n`);
      return 1;
    }
    copyFileSync(
      path.join(bundles, `${PRODUCT}_${version}_x64-setup.exe`),
      path.join(folder, installerFile(version)),
    );
  }
  const second = path.join(folder, installerFile(SECOND));
  const cli = createRequire(path.join(root, "apps", "desktop", "package.json")).resolve(
    "@tauri-apps/cli/tauri.js",
  );
  const signed = signFile({ cli, key, file: second, version: SECOND, password: null });
  if (signed.status !== 0) {
    err.write(`the CLI couldn't sign the rehearsal's ${SECOND}: ${signed.stderr.trim()}\n`);
    return 1;
  }
  const latest = path.join(folder, "latest.json");
  const url = `http://127.0.0.1:${server.port}/${installerFile(SECOND)}`;
  const signature = readFileSync(`${second}.sig`, "utf8");
  writeFileSync(
    latest,
    `${JSON.stringify(manifest({ version: SECOND, signature, url, now: new Date() }), null, 2)}\n`,
  );
  server.files["latest.json"] = latest;
  server.files[installerFile(SECOND)] = second;
  out.write(
    steps({ folder: FOLDER, first: path.join(folder, installerFile(FIRST)), port: server.port }),
  );
  return new Promise((resolve) => {
    process.once("SIGINT", () => resolve(0));
  });
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  main(process.argv.slice(2), { root, out: process.stdout, err: process.stderr }).then(
    (code) => {
      process.exit(code);
    },
    (error) => {
      process.stderr.write(`${error.message}\n`);
      process.exit(1);
    },
  );
}
