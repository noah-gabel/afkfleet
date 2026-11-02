// Keeps the e2e build out of everything shipped (Plan.md P8.9, ADR-0017).
//
//   node scripts/e2e-bundle-guard.mjs
//
// The e2e build swaps Tauri's IPC for fetchInvoke, which sends every command over HTTP to the
// page's own origin. That must never reach a build anyone runs: in the app it would send the
// user's password to whatever answers there. So `just e2e` builds the desktop app's frontend
// and packages/ui's web entry the way they ship (into target/e2e/), builds the e2e bundle,
// and runs this check:
// - no shipped bundle holds the e2e build's marker (e2e/app/marker.ts, repeated here on
//   purpose), fetchInvoke's path `/e2e/ipc/`, or the live events hook's name
//   (e2e/app/push.ts's `PUSH_HOOK`, repeated too; a test pins both copies to their sources);
// - the e2e bundle does hold every one of them, so the search finds what it looks for;
// - every bundle exists and holds files, so a skipped build can't pass;
// - every bundle holds the Latin fonts as files of their own, none inlined as `data:`, which the
//   CSP refuses (Vite inlines a small asset unless told not to; ADR-0020, S111).
// Exits with 1, naming each problem. Tests: scripts/e2e-bundle-guard.test.mjs.
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join, relative, sep } from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

/** The e2e build's marker: e2e/app/marker.ts's `E2E_MARKER`. */
export const MARKER = "afkfleet-e2e-build-only";

/** The live events hook's name: e2e/app/push.ts's `PUSH_HOOK`. */
export const PUSH_HOOK = "__afkfleetE2ePush";

/** What no shipped bundle may hold. */
export const NEEDLES = [MARKER, "/e2e/ipc/", PUSH_HOOK];

/** Every file under `folder`, as `{ path, text }`, `path` relative to `folder`. */
export function files(folder) {
  const found = [];
  const walk = (current) => {
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const full = join(current, entry.name);
      if (entry.isDirectory()) {
        walk(full);
      } else if (entry.isFile()) {
        found.push({
          path: relative(folder, full).split(sep).join("/"),
          text: readFileSync(full, "latin1"),
        });
      }
    }
  };
  walk(folder);
  return found;
}

/** A folder's problems as a bundle: missing, or without any file. */
function empty(folder) {
  if (!existsSync(folder)) {
    return [`${folder} doesn't exist: build it first (just e2e)`];
  }
  return files(folder).length === 0 ? [`${folder} holds no files: build it first (just e2e)`] : [];
}

/** The Latin faces a bundle must hold as files of their own, as Vite names them. */
export const FACES = ["geist-latin-wght", "ibm-plex-mono-latin-400", "ibm-plex-mono-latin-600"];

/**
 * A bundle's problems with its fonts: one inlined, which the CSP refuses, a face it doesn't hold,
 * or a font its CSS names and it doesn't hold.
 */
function fontProblems(folder) {
  const problems = [];
  const held = files(folder);
  const names = held.map((file) => file.path.split("/").at(-1) ?? "");
  for (const { path, text } of held) {
    if (/\.(?:css|js|html)$/.test(path)) {
      if (/data:(?:font\/|application\/(?:x-)?font)/.test(text)) {
        problems.push(`${folder}/${path} holds an inlined font: the CSP would block it`);
      }
      for (const match of text.matchAll(/url\(["']?([^"')]+\.woff2)/g)) {
        const name = (match[1] ?? "").split("/").at(-1) ?? "";
        if (!names.includes(name)) {
          problems.push(`${folder}/${path} names ${name}, which the bundle doesn't hold`);
        }
      }
    }
  }
  for (const face of FACES) {
    if (!names.some((name) => name.startsWith(`${face}-`) && name.endsWith(".woff2"))) {
      problems.push(`${folder} holds no ${face} font`);
    }
  }
  return problems;
}

/** The problems of the shipped bundles `shipped` and the e2e bundle `e2e`; see the module comment. */
export function check({ shipped, e2e }) {
  const problems = [];
  for (const folder of shipped) {
    const missing = empty(folder);
    problems.push(...missing);
    if (missing.length > 0) {
      continue;
    }
    problems.push(...fontProblems(folder));
    for (const file of files(folder)) {
      for (const needle of NEEDLES) {
        if (file.text.includes(needle)) {
          problems.push(
            `${folder}/${file.path} holds "${needle}": the e2e build reached a shipped bundle`,
          );
        }
      }
    }
  }
  const missing = empty(e2e);
  problems.push(...missing);
  if (missing.length === 0) {
    problems.push(...fontProblems(e2e));
    const built = files(e2e);
    for (const needle of NEEDLES) {
      if (!built.some((file) => file.text.includes(needle))) {
        problems.push(`${e2e} doesn't hold "${needle}", so the check proves nothing`);
      }
    }
  }
  return problems;
}

/** The repository's root: this script's folder's parent. */
export const ROOT = join(fileURLToPath(new URL(".", import.meta.url)), "..");

/** The folders `just e2e` builds. */
export function repositoryBundles(root = ROOT) {
  return {
    shipped: [
      join(root, "target", "e2e", "shipped-desktop"),
      join(root, "target", "e2e", "shipped-ui"),
    ],
    e2e: join(root, "apps", "desktop", "dist-e2e"),
  };
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  const problems = check(repositoryBundles());
  for (const problem of problems) {
    console.error(`e2e-bundle-guard: ${problem}`);
  }
  process.exit(problems.length === 0 ? 0 : 1);
}
