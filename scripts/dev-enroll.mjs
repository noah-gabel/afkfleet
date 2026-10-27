// Enrolls an agent with a server: creates its folder, then runs `afkfleet-agent enroll`.
//
//   node scripts/dev-enroll.mjs [--release] <config> <folder>
//
// `just dev-enroll` runs it as its only line, with deploy/dev/agent.managed.toml and the
// bundle piped in; `just dod-enroll` the same with --release and deploy/dod/agent.toml, so
// the DoD checks' binaries are release builds (ADR-0020, S72, R123). With just's
// windows-shell, every recipe line gets just's stdin, so an earlier line could take the
// bundle. This script never reads stdin: the agent gets this process's stdin, stdout and
// stderr, and the AFKFLEET_AGENT__CONTROL_PLANE__* paths the recipe sets (ADR-0019, S19).
// The folder (an absolute path; target/dev-agent or target/dod-run/agent by default) is
// created when it's missing. Exits with the agent's exit code, 1 when it can't be started,
// 2 on a usage error. Tests: scripts/dev-enroll.test.mjs.
import { spawnSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

/** `afkfleet-agent enroll`'s command with `config`, built if needed, in release when `release`. */
export function enrollCommand(release, config) {
  return [
    "cargo",
    [
      "run",
      "--quiet",
      ...(release ? ["--release"] : []),
      "-p",
      "fleet-agent",
      "--bin",
      "afkfleet-agent",
      "--",
      "enroll",
      "--config",
      config,
    ],
  ];
}

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

/**
 * The command line: an optional `--release`, a `.toml` config, then one absolute folder.
 * Returns `{ release, config, folder }`, or null for anything else.
 */
export function parseArgs(args) {
  const release = args[0] === "--release";
  const [config, folder, ...rest] = release ? args.slice(1) : args;
  if (config === undefined || !config.endsWith(".toml") || folder === undefined) {
    return null;
  }
  return rest.length === 0 && path.isAbsolute(folder) ? { release, config, folder } : null;
}

function main(args) {
  const parsed = parseArgs(args);
  if (parsed === null) {
    console.error(
      "usage: node scripts/dev-enroll.mjs [--release] <config> <folder> (an absolute path)",
    );
    return 2;
  }
  const [command, commandArgs] = enrollCommand(parsed.release, parsed.config);
  return enrollInto(parsed.folder, command, commandArgs);
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = main(process.argv.slice(2));
}
