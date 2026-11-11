// Runs one fuzz target, or all of them, each for a number of seconds.
//
//   node scripts/fuzz.mjs <target|all> [seconds]
//
// The targets are fuzz/fuzz_targets/*.rs, run with cargo-fuzz 0.13.2 (Plan.md P13.2,
// ADR-0030): `cargo fuzz run <target> -- -max_total_time=<seconds>`, one after the other,
// stopping at the first that fails. A failing input is kept in fuzz/artifacts/<target>/:
// by libFuzzer as `crash-<hash>` where a panic reaches it, and always as `failing-input`,
// which the target writes itself at a panic, to the path this script hands it (on Windows
// a panic ends the target before libFuzzer saves anything). `just fuzz` writes the seeds
// first.
//
// On Windows the targets need MSVC's AddressSanitizer runtime at run time
// (clang_rt.asan_dynamic-x86_64.dll, Visual Studio's "C++ AddressSanitizer" component):
// without it on PATH a target exits with 0xc0000135 before it runs. The script asks vswhere
// where it is and puts that folder first on the target's PATH; nothing else changes.
// Tests: scripts/fuzz.test.mjs.
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync } from "node:fs";
import { dirname, join, win32 } from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

/** The folder of the cargo-fuzz crate's targets, from the repository's root. */
export const TARGETS_FOLDER = join("fuzz", "fuzz_targets");

/** How long a target runs when no time is given, in seconds. */
export const DEFAULT_SECONDS = 60;

/** The longest run of one target: a day. */
export const MAX_SECONDS = 86_400;

/** The variable that tells a target where to write the input it fails on (fleet-fuzz's `kept`). */
export const KEEP_AT = "AFKFLEET_FUZZ_KEEP_AT";

/** The file a target's failing input is kept in, from the repository's root. */
export function failingInput(target) {
  return join("fuzz", "artifacts", target, "failing-input");
}

/** MSVC's AddressSanitizer runtime, which a fuzz target loads on Windows. */
export const ASAN_DLL = "clang_rt.asan_dynamic-x86_64.dll";

/** What vswhere searches every Visual Studio installation for. */
export const ASAN_PATTERN = `VC\\Tools\\MSVC\\**\\bin\\HostX64\\x64\\${ASAN_DLL}`;

/** The targets' names: one per `.rs` file of `fileNames`, sorted. */
export function targetNames(fileNames) {
  return fileNames
    .filter((name) => name.endsWith(".rs"))
    .map((name) => name.slice(0, -".rs".length))
    .sort();
}

/**
 * Reads the arguments after the script's name against the `known` targets. Returns
 * `{ targets, seconds }`, or `{ error }` with a message.
 */
export function parseArgs(args, known) {
  const [target, seconds = String(DEFAULT_SECONDS), ...rest] = args;
  if (target === undefined || rest.length > 0) {
    return { error: "usage: node scripts/fuzz.mjs <target|all> [seconds]" };
  }
  if (target !== "all" && !known.includes(target)) {
    return { error: `no fuzz target is named \`${target}\`: ${known.join(", ")}, or all` };
  }
  if (!/^[1-9]\d{0,4}$/.test(seconds) || Number(seconds) > MAX_SECONDS) {
    return { error: `the time is a whole number of seconds from 1 to ${MAX_SECONDS}` };
  }
  return { targets: target === "all" ? known : [target], seconds: Number(seconds) };
}

/** cargo's arguments for running `target` for `seconds`. */
export function cargoArgs(target, seconds) {
  return ["fuzz", "run", target, "--", `-max_total_time=${seconds}`];
}

/**
 * The folder of the newest toolset's AddressSanitizer runtime in vswhere's `output`, a
 * path a line; `undefined` when it names none.
 */
export function asanFolder(output) {
  const found = output
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.toLowerCase().endsWith(ASAN_DLL.toLowerCase()))
    .sort();
  const newest = found.at(-1);
  // vswhere's paths are Windows paths, whatever platform reads them.
  return newest === undefined ? undefined : win32.dirname(newest);
}

/** `env` with `folder` first on its PATH, whatever case the variable's name has. */
export function withOnPath(env, folder, separator) {
  const name = Object.keys(env).find((key) => key.toUpperCase() === "PATH") ?? "PATH";
  const rest = env[name];
  return { ...env, [name]: rest ? `${folder}${separator}${rest}` : folder };
}

/** Where Visual Studio's installer puts vswhere. */
function vswherePath(env) {
  const programs = env["ProgramFiles(x86)"] ?? "C:\\Program Files (x86)";
  return join(programs, "Microsoft Visual Studio", "Installer", "vswhere.exe");
}

/** The environment a target runs in: on Windows, with the sanitizer's runtime on PATH. */
function targetEnv() {
  if (process.platform !== "win32") {
    return { env: process.env };
  }
  const vswhere = vswherePath(process.env);
  if (!existsSync(vswhere)) {
    return { error: "vswhere.exe isn't there: Visual Studio's installer puts it in place" };
  }
  const asked = spawnSync(vswhere, ["-products", "*", "-find", ASAN_PATTERN], {
    encoding: "utf8",
  });
  const folder = asked.status === 0 ? asanFolder(asked.stdout) : undefined;
  if (folder === undefined) {
    return {
      error: `${ASAN_DLL} isn't installed: add Visual Studio's "C++ AddressSanitizer" component`,
    };
  }
  return { env: withOnPath(process.env, folder, ";") };
}

function main() {
  const root = join(dirname(fileURLToPath(import.meta.url)), "..");
  const known = targetNames(readdirSync(join(root, TARGETS_FOLDER)));
  const parsed = parseArgs(process.argv.slice(2), known);
  const environment = parsed.error === undefined ? targetEnv() : {};
  const error = parsed.error ?? environment.error;
  if (error !== undefined) {
    console.error(error);
    process.exit(2);
  }
  for (const target of parsed.targets) {
    console.log(`fuzzing ${target} for ${parsed.seconds} s`);
    const kept = join(root, failingInput(target));
    mkdirSync(dirname(kept), { recursive: true });
    const run = spawnSync("cargo", cargoArgs(target, parsed.seconds), {
      cwd: root,
      env: { ...environment.env, [KEEP_AT]: kept },
      stdio: "inherit",
    });
    if (run.status !== 0) {
      console.error(`${target} failed: its input is ${failingInput(target)}`);
      process.exit(run.status ?? 1);
    }
  }
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main();
}
