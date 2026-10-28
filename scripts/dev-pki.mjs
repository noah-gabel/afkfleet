// Creates a server's PKI when it's missing: the dev server's, or the DoD checks'.
//
//   node scripts/dev-pki.mjs [--release] <config> <ca cert> <ca key> <server cert> <server key>
//
// `just dev-server` runs it before the server starts, with deploy/dev/server.toml and the
// absolute paths it passes the server as AFKFLEET_SERVER__GRPC__*: target/dev-server/pki/ by
// default, next to the dev database and vault key, so wiping target/dev-server/ removes them
// together (ADR-0019). `just dod-server` runs it with --release and deploy/dod/server.toml,
// its paths in target/dod-run/pki/, so the DoD checks' binaries are release builds
// (ADR-0020, S72, R123).
// When all four files exist, it does nothing: they're never read, checked or overwritten
// here; the server checks them when it starts. Otherwise it creates their folder and runs
// `afkfleet-server pki init` with the config, for localhost, 127.0.0.1 and ::1, the names
// the agents dial, with the same environment, so the command writes exactly those paths.
// pki init itself refuses when some of the four exist: it never overwrites, and a new CA
// would lock out every enrolled agent. Exits with 0 when the PKI exists or was created, 1
// when pki init failed or refused, 2 on a usage error. Tests: scripts/dev-pki.test.mjs.
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

/** The arguments `pki init` gets after `--config`: the server certificate's names. */
export const NAMES = ["--dns", "localhost", "--ip", "127.0.0.1", "--ip", "::1"];

/**
 * Creates the PKI at `files` with `init` unless all four exist, creating their folders.
 * `init` runs `pki init` and returns its exit status. Returns "kept", "created" or "failed".
 */
export function ensurePki(files, init) {
  if (files.every((file) => existsSync(file))) {
    return "kept";
  }
  for (const file of files) {
    mkdirSync(path.dirname(file), { recursive: true });
  }
  return init() === 0 ? "created" : "failed";
}

/** `afkfleet-server pki init`'s command with `config`, a release build when `release`. */
export function pkiInitCommand(release, config) {
  return [
    "cargo",
    [
      "run",
      ...(release ? ["--release"] : []),
      "-p",
      "fleet-server",
      "--bin",
      "afkfleet-server",
      "--",
      "pki",
      "init",
      "--config",
      config,
      ...NAMES,
    ],
  ];
}

/**
 * The command line: an optional `--release`, a `.toml` config, then four absolute paths.
 * Returns `{ release, config, files }`, or null for anything else.
 */
export function parseArgs(args) {
  const release = args[0] === "--release";
  const [config, ...files] = release ? args.slice(1) : args;
  if (config === undefined || !config.endsWith(".toml") || files.length !== 4) {
    return null;
  }
  return files.every((file) => path.isAbsolute(file)) ? { release, config, files } : null;
}

/** Runs `command`, inheriting this process's environment and streams; its exit status. */
function run([command, args]) {
  const ran = spawnSync(command, args, { stdio: "inherit" });
  return ran.status ?? 1;
}

function main(args) {
  const parsed = parseArgs(args);
  if (parsed === null) {
    console.error(
      "usage: node scripts/dev-pki.mjs [--release] <config> <ca cert> <ca key> <server cert> <server key> (absolute paths)",
    );
    return 2;
  }
  const result = ensurePki(parsed.files, () => run(pkiInitCommand(parsed.release, parsed.config)));
  if (result === "failed") {
    console.error("the PKI wasn't created: see pki init's lines above");
    return 1;
  }
  return 0;
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = main(process.argv.slice(2));
}
