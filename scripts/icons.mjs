// Renders the desktop app's Windows icon, icon.ico, from its SVG sources.
//
//   node scripts/icons.mjs
//
// tauri-build embeds apps/desktop/src-tauri/icons/icon.ico in the exe, and a Windows build fails
// without it (ADR-0017). `tauri icon` writes only six sizes into an ICO, and takes one source per
// run, while Windows draws a desktop app's icon at 14 sizes across its DPI settings, and takes the
// next size up for any it lacks; and Orbit's ring needs a source of its own at 16 px (ADR-0020,
// U95, S118). So the CLI renders each size as a PNG into a fresh folder under target/, this script
// checks them and writes the ICO itself, and that folder is removed again. The old icon stays until
// everything checks out. Run through `just icons` after changing either source; it needs
// `pnpm install` first, for the CLI. Exits with 1, naming the problem.
// Tests: scripts/icons.test.mjs.
import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

/** The repository's root folder. */
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** The desktop app's package, which has the Tauri CLI as a dev-dependency. */
const APP_DIR = path.join(ROOT, "apps", "desktop");

/** The folder that holds the sources and the committed icon.ico. */
const ICONS_DIR = path.join(APP_DIR, "src-tauri", "icons");

/** The one file that's written. */
const ICON = "icon.ico";

/** Every size in the ICO, ascending, as Microsoft's icon construction guide lists them. */
export const ICON_SIZES = [16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256];

/** Which source each size is rendered from: at 16 px Orbit's ring needs its own (U95). */
export const RENDERS = [
  { source: "icon-16.svg", sizes: [16] },
  { source: "icon.svg", sizes: ICON_SIZES.filter((size) => size !== 16) },
];

/** The arguments for Node: the CLI's entry script, then its `icon` command at `sizes`. */
export function cliArgs(cliEntry, source, outDir, sizes) {
  return [cliEntry, "icon", source, "--output", outDir, "--png", sizes.join(",")];
}

/** A PNG's first eight bytes. */
const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

/** What's wrong with `bytes` as an 8-bit RGBA PNG of `size`, the form an ICO's 32-bit entry holds. */
function pngProblem(bytes, size) {
  if (bytes.length < 33 || !bytes.subarray(0, 8).equals(PNG_SIGNATURE)) {
    return "isn't a PNG";
  }
  const [width, height] = [bytes.readUInt32BE(16), bytes.readUInt32BE(20)];
  if (width !== size || height !== size) {
    return `is ${width}x${height}, not ${size}x${size}`;
  }
  if (bytes[24] !== 8 || bytes[25] !== 6) {
    return `isn't 8-bit RGBA (bit depth ${bytes[24]}, colour type ${bytes[25]})`;
  }
  return null;
}

/**
 * The PNG of each of `sizes` in `outDir`, as the CLI names them, as `{ images, problem }`: `problem`
 * names the first one that's missing or wrong.
 */
export function rendered(outDir, sizes) {
  const images = [];
  for (const size of sizes) {
    const name = `${size}x${size}.png`;
    const file = path.join(outDir, name);
    if (!existsSync(file)) {
      return { images: [], problem: `tauri icon wrote no ${name} into ${outDir}` };
    }
    const bytes = readFileSync(file);
    const problem = pngProblem(bytes, size);
    if (problem !== null) {
      return { images: [], problem: `${name} ${problem}` };
    }
    images.push({ size, bytes });
  }
  return { images, problem: null };
}

/** An ICONDIR's size, and each ICONDIRENTRY's. */
const HEADER = 6;
const ENTRY = 16;

/** An ICO holding `images`, each `{ size, bytes }` a PNG: 256 is stored as 0, as the format says. */
export function buildIco(images) {
  const header = Buffer.alloc(HEADER + images.length * ENTRY);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(images.length, 4);
  let offset = header.length;
  images.forEach(({ size, bytes }, index) => {
    const at = HEADER + index * ENTRY;
    header[at] = size === 256 ? 0 : size;
    header[at + 1] = size === 256 ? 0 : size;
    header.writeUInt16LE(1, at + 4);
    header.writeUInt16LE(32, at + 6);
    header.writeUInt32LE(bytes.length, at + 8);
    header.writeUInt32LE(offset, at + 12);
    offset += bytes.length;
  });
  return Buffer.concat([header, ...images.map((image) => image.bytes)]);
}

/** The images of the ICO `ico`, each `{ size, bytes }`. */
export function readIco(ico) {
  return Array.from({ length: ico.readUInt16LE(4) }, (_, index) => {
    const at = HEADER + index * ENTRY;
    const [length, offset] = [ico.readUInt32LE(at + 8), ico.readUInt32LE(at + 12)];
    return { size: ico[at] === 0 ? 256 : ico[at], bytes: ico.subarray(offset, offset + length) };
  });
}

/**
 * Writes icon.ico into `iconsDir` from the renders in `outDir`, replacing the old one only once all
 * of them check out. Returns a message if one doesn't; otherwise null.
 */
export function adoptIcon(outDir, iconsDir) {
  const { images, problem } = rendered(outDir, ICON_SIZES);
  if (problem !== null) {
    return problem;
  }
  const part = path.join(iconsDir, `${ICON}.part`);
  writeFileSync(part, buildIco(images));
  renameSync(part, path.join(iconsDir, ICON));
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
    for (const { source, sizes } of RENDERS) {
      const result = spawnSync(
        process.execPath,
        cliArgs(cli, path.join(ICONS_DIR, source), outDir, sizes),
        { cwd: APP_DIR, stdio: "inherit" },
      );
      if (result.error !== undefined || result.status !== 0) {
        console.error(
          `tauri icon failed on ${source}: ${result.error?.message ?? `exit code ${result.status}`}`,
        );
        return 1;
      }
    }
    const problem = adoptIcon(outDir, ICONS_DIR);
    if (problem !== null) {
      console.error(problem);
      return 1;
    }
    console.log(
      `wrote ${path.relative(ROOT, path.join(ICONS_DIR, ICON))} at ${ICON_SIZES.length} sizes`,
    );
    return 0;
  } finally {
    rmSync(outDir, { recursive: true, force: true });
  }
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = main();
}
