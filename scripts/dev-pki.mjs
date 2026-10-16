// Creates the dev server's PKI when it's missing.
//
//   node scripts/dev-pki.mjs <ca cert> <ca key> <server cert> <server key>
//
// `just dev-server` runs it before the server starts, with the absolute paths it passes the
// server as AFKFLEET_SERVER__GRPC__*: target/dev-server/pki/ by default, next to the dev
// database and vault key, so wiping target/dev-server/ removes them together (ADR-0019).
// When all four files exist, it does nothing: they're never read, checked or overwritten
// here; the server checks them when it starts. Otherwise it creates their folder and runs
// `afkfleet-server pki init` for localhost, 127.0.0.1 and ::1, the names `just dev-enroll`
// dials, with the same environment, so the command writes exactly those paths. pki init
// itself refuses when some of the four exist: it never overwrites, and a new CA would lock
// out every enrolled dev agent. Exits with 0 when the PKI exists or was created, 1 when pki
// init failed or refused, 2 on a usage error. Tests: scripts/dev-pki.test.mjs.
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

/** The arguments `pki init` gets after `--config`: the dev server certificate's names. */
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

/** Runs `afkfleet-server pki init` with the dev config, inheriting this process's environment. */
function pkiInit() {
  const ran = spawnSync(
    "cargo",
    [
      "run",
      "-p",
      "fleet-server",
      "--bin",
      "afkfleet-server",
      "--",
      "pki",
      "init",
      "--config",
      "deploy/dev/server.toml",
      ...NAMES,
    ],
    { stdio: "inherit" },
  );
  return ran.status ?? 1;
}

function main(args) {
  if (args.length !== 4 || !args.every((file) => path.isAbsolute(file))) {
    console.error(
      "usage: node scripts/dev-pki.mjs <ca cert> <ca key> <server cert> <server key> (absolute paths)",
    );
    return 2;
  }
  const result = ensurePki(args, pkiInit);
  if (result === "failed") {
    console.error("the dev PKI wasn't created: see pki init's lines above");
    return 1;
  }
  return 0;
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = main(process.argv.slice(2));
}
