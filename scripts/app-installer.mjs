// Builds the desktop app's NSIS installer, unsigned, with no updater signature (Plan.md P12.5;
// ADR-0022, S22, U1, U31).
//
//   node scripts/app-installer.mjs        (just app-installer)
//
// The installer is built without any key: this script refuses to start while a variable that
// would hand the Tauri CLI a signing key is set (it names the variable, never its value), and the
// CLI runs with `--no-sign`, which skips the updater's signing before any key is read. Signing the
// installer for the updater is the maintainer's step alone, on their own PC (docs/releasing.md).
// After the build it finds exactly this version's `afkfleet_X.Y.Z_x64-setup.exe`, written by
// this build and with no `.sig` beside it, and prints its SHA-256 and name on stdout as
// `sha256sum` and SHA256SUMS list them; in CI it also adds that line to the job's summary.
//
// Exit codes: 0 built, 1 refused or failed (it says why), 2 usage error. It needs `pnpm install`
// first, for the CLI. CI's tauri-build job runs it on every pull request, and the release
// workflow's installer job at a tag. Tests: scripts/app-installer.test.mjs.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { appendFileSync, readdirSync, readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

/** Every variable through which the Tauri CLI would read a signing key or its password. */
export const SIGNING_VARIABLES = [
  "TAURI_SIGNING_PRIVATE_KEY",
  "TAURI_SIGNING_PRIVATE_KEY_PATH",
  "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
  // The CLI still maps v1's names onto these.
  "TAURI_PRIVATE_KEY",
  "TAURI_PRIVATE_KEY_PATH",
  "TAURI_PRIVATE_KEY_PASSWORD",
  "TAURI_KEY_PASSWORD",
];

/** The CLI's arguments: the NSIS bundle alone, every signature skipped. */
export const CLI_ARGS = ["build", "--bundles", "nsis", "--no-sign"];

/** How long before the build's start an installer may be stamped (file systems round). */
const CLOCK_SLACK_MS = 2000;

/** The installer's file name for `version`, as Tauri's NSIS bundler writes it. */
export function installerName(version) {
  return `afkfleet_${version}_x64-setup.exe`;
}

/** `[workspace.package] version` of Cargo.toml's text, when it's X.Y.Z; else null. */
export function workspaceVersion(cargoToml) {
  const section = cargoToml.split(/^\[/m).find((part) => part.startsWith("workspace.package]"));
  const version = section?.match(/^version = "([^"]*)"$/m)?.[1];
  return version !== undefined && /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version)
    ? version
    : null;
}

/** The first signing variable set in `env`, even to an empty value; else null. */
export function signingVariable(env) {
  return SIGNING_VARIABLES.find((name) => Object.hasOwn(env, name)) ?? null;
}

/** What's wrong with the build's output in `bundle` for `version`, built from `startedMs` on. */
function installerProblem(bundle, version, startedMs) {
  const name = installerName(version);
  let names;
  try {
    names = readdirSync(bundle);
  } catch {
    return `the build left no ${bundle}, so no ${name}`;
  }
  if (!names.includes(name)) {
    return `the build left no ${name} in ${bundle}`;
  }
  const stats = statSync(path.join(bundle, name));
  if (!stats.isFile()) {
    return `${name} isn't a file`;
  }
  if (stats.mtimeMs < startedMs - CLOCK_SLACK_MS) {
    return `${name} is older than this build: the build didn't write it`;
  }
  const signatures = names.filter((other) => other.endsWith(".sig"));
  if (signatures.length > 0) {
    return `${signatures.join(", ")} lies beside the installer: nothing here may sign it`;
  }
  return null;
}

/** Runs the pinned Tauri CLI with `args` in the desktop app's folder; its exit status. */
function runPinnedCli(root, args) {
  const appDir = path.join(root, "apps", "desktop");
  const cli = createRequire(path.join(appDir, "package.json")).resolve("@tauri-apps/cli/tauri.js");
  const result = spawnSync(process.execPath, [cli, ...args], { cwd: appDir, stdio: "inherit" });
  return result.error === undefined ? result.status : 1;
}

/**
 * Builds and checks the installer. `io` holds the repository's `root`, the environment `env`,
 * `out` and `err` with a `write`, and `runCli(args)`, which runs the CLI and returns its status.
 */
export function main(args, io) {
  const { root, env, out, err } = io;
  const runCli = io.runCli ?? ((cliArgs) => runPinnedCli(root, cliArgs));
  if (args.length > 0) {
    err.write("usage: node scripts/app-installer.mjs\n");
    return 2;
  }
  const variable = signingVariable(env);
  if (variable !== null) {
    err.write(
      `${variable} is set: the installer is built without any signing key (ADR-0022, U1); unset it and run again\n`,
    );
    return 1;
  }
  const version = workspaceVersion(readFileSync(path.join(root, "Cargo.toml"), "utf8"));
  if (version === null) {
    err.write("Cargo.toml's [workspace.package] version isn't X.Y.Z\n");
    return 1;
  }
  const startedMs = Date.now();
  const status = runCli(CLI_ARGS);
  if (status !== 0) {
    err.write(`tauri build failed: exit code ${status}\n`);
    return 1;
  }
  const bundle = path.join(root, "target", "release", "bundle", "nsis");
  const problem = installerProblem(bundle, version, startedMs);
  if (problem !== null) {
    err.write(`${problem}\n`);
    return 1;
  }
  const name = installerName(version);
  const sha = createHash("sha256")
    .update(readFileSync(path.join(bundle, name)))
    .digest("hex");
  out.write(`${sha}  ${name}\n`);
  if (env.GITHUB_STEP_SUMMARY) {
    appendFileSync(env.GITHUB_STEP_SUMMARY, `### The installer\n\n\`${sha}  ${name}\`\n`);
  }
  err.write(`the installer is ${path.relative(root, path.join(bundle, name))}, unsigned\n`);
  return 0;
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  process.exitCode = main(process.argv.slice(2), {
    root,
    env: process.env,
    out: process.stdout,
    err: process.stderr,
  });
}
