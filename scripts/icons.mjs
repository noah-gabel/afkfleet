// Renders the desktop app's Windows icon, icon.ico, from its SVG source.
//
//   node scripts/icons.mjs
//
// tauri-build embeds apps/desktop/src-tauri/icons/icon.ico in the exe, and a Windows build
// fails without it (ADR-0017). `tauri icon` renders every platform's sizes (macOS, iOS,
// Android, the Store logos), but only icon.ico is used, so the CLI writes into a fresh
// folder under target/, only icon.ico is copied next to icon.svg, and that folder is
// removed again. Run through `just icons` after changing icon.svg; it needs
// `pnpm install` first, for the CLI. Exits with 1 if the CLI fails or writes no icon.ico.
// Tests: scripts/icons.test.mjs.
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

/** The repository's root folder. */
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** The desktop app's package, which has the Tauri CLI as a dev-dependency. */
const APP_DIR = path.join(ROOT, "apps", "desktop");

/** The folder that holds icon.svg and the committed icon.ico. */
const ICONS_DIR = path.join(APP_DIR, "src-tauri", "icons");

/** The one rendered file that's kept. */
const ICON = "icon.ico";

/** The arguments for Node: the CLI's entry script, then its `icon` command. */
export function cliArgs(cliEntry, source, outDir) {
  return [cliEntry, "icon", source, "--output", outDir];
}

/**
 * Copies icon.ico from the CLI's output folder into the icons folder, replacing the old
 * one. Returns a message if the CLI wrote no icon.ico, and copies nothing then; otherwise
 * null.
 */
export function adoptIcon(outDir, iconsDir) {
  const rendered = path.join(outDir, ICON);
  if (!existsSync(rendered)) {
    return `tauri icon wrote no ${ICON} into ${outDir}`;
  }
  copyFileSync(rendered, path.join(iconsDir, ICON));
  return null;
}

function main() {
  const target = path.join(ROOT, "target");
  mkdirSync(target, { recursive: true });
  // A fresh folder for this run only, so only what this run created is ever removed.
  const outDir = mkdtempSync(path.join(target, "icons-"));
  try {
    const cli = createRequire(path.join(APP_DIR, "package.json")).resolve(
      "@tauri-apps/cli/tauri.js",
    );
    const source = path.join(ICONS_DIR, "icon.svg");
    const result = spawnSync(process.execPath, cliArgs(cli, source, outDir), {
      cwd: APP_DIR,
      stdio: "inherit",
    });
    if (result.error !== undefined || result.status !== 0) {
      console.error(`tauri icon failed: ${result.error?.message ?? `exit code ${result.status}`}`);
      return 1;
    }
    const problem = adoptIcon(outDir, ICONS_DIR);
    if (problem !== null) {
      console.error(problem);
      return 1;
    }
    console.log(`wrote ${path.relative(ROOT, path.join(ICONS_DIR, ICON))} from icon.svg`);
    return 0;
  } finally {
    rmSync(outDir, { recursive: true, force: true });
  }
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = main();
}
