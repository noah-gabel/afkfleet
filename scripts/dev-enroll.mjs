// Enrolls a dev agent with dev-server: creates its folder, then runs `afkfleet-agent enroll`.
//
//   node scripts/dev-enroll.mjs <folder>
//
// `just dev-enroll` runs it as its only line, with the bundle piped in: with just's
// windows-shell, every recipe line gets just's stdin, so an earlier line could take the
// bundle. This script never reads stdin: the agent gets this process's stdin, stdout and
// stderr, and the AFKFLEET_AGENT__CONTROL_PLANE__* paths the recipe sets (ADR-0019, S19).
// The folder (an absolute path; target/dev-agent by default) is created when it's missing.
// Exits with the agent's exit code, 1 when it can't be started, 2 on a usage error.
// Tests: scripts/dev-enroll.test.mjs.
import { spawnSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

/** The agent's command: `afkfleet-agent enroll` with the managed dev config, built if needed. */
export const ENROLL = [
  "cargo",
  [
    "run",
    "--quiet",
    "-p",
    "fleet-agent",
    "--bin",
    "afkfleet-agent",
    "--",
    "enroll",
    "--config",
    "deploy/dev/agent.managed.toml",
  ],
];

/**
 * Creates `dir`, then runs `command` with `args` on this process's stdin, stdout and stderr.
 * Returns its exit code, or 1 when it can't be started or was killed.
 */
export function enrollInto(dir, command, args) {
  mkdirSync(dir, { recursive: true });
  const ran = spawnSync(command, args, { stdio: "inherit" });
  if (ran.error !== undefined) {
    console.error(`dev-enroll: ${command} can't be started: ${ran.error.message}`);
    return 1;
  }
  return ran.status ?? 1;
}

function main(args) {
  if (args.length !== 1 || !path.isAbsolute(args[0])) {
    console.error("usage: node scripts/dev-enroll.mjs <folder> (an absolute path)");
    return 2;
  }
  const [command, commandArgs] = ENROLL;
  return enrollInto(args[0], command, commandArgs);
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = main(process.argv.slice(2));
}
