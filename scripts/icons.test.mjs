// Tests for icons.mjs. Run with `just scripts-test`.
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { adoptIcon, buildIco, cliArgs, ICON_SIZES, RENDERS, readIco, rendered } from "./icons.mjs";

const ICONS = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "apps",
  "desktop",
  "src-tauri",
  "icons",
);

/** A fresh folder that the test removes again. */
function tempDir(t) {
  const dir = mkdtempSync(path.join(tmpdir(), "icons-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

/** The first bytes of a PNG of `size`: its signature and IHDR, 8-bit RGBA unless told otherwise. */
function png(size, { bits = 8, colour = 6, height = size } = {}) {
  const bytes = Buffer.alloc(33);
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]).copy(bytes, 0);
  bytes.writeUInt32BE(13, 8);
  bytes.write("IHDR", 12, "latin1");
  bytes.writeUInt32BE(size, 16);
  bytes.writeUInt32BE(height, 20);
  bytes[24] = bits;
  bytes[25] = colour;
  return bytes;
}

/** A folder holding a PNG for each size, as the CLI names them. */
function rendersIn(t, sizes = ICON_SIZES) {
  const dir = tempDir(t);
  for (const size of sizes) {
    writeFileSync(path.join(dir, `${size}x${size}.png`), png(size));
  }
  return dir;
}

describe("the sizes", () => {
  it("are every size Windows draws a desktop app's icon at, in order", () => {
    assert.deepEqual(ICON_SIZES, [16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256]);
  });

  it("each come from one render: 16 from its tuned source, the rest from the master", () => {
    assert.deepEqual(RENDERS, [
      { source: "icon-16.svg", sizes: [16] },
      { source: "icon.svg", sizes: [20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256] },
    ]);
    assert.deepEqual(
      RENDERS.flatMap((render) => render.sizes).sort((a, b) => a - b),
      ICON_SIZES,
    );
  });
});

describe("cliArgs", () => {
  it("runs the CLI's icon command on the source, into the folder, at the sizes", () => {
    assert.deepEqual(cliArgs("/cli/tauri.js", "/icons/icon.svg", "/target/icons-1", [20, 24]), [
      "/cli/tauri.js",
      "icon",
      "/icons/icon.svg",
      "--output",
      "/target/icons-1",
      "--png",
      "20,24",
    ]);
  });
});

describe("rendered", () => {
  it("reads each size's PNG", (t) => {
    const dir = rendersIn(t, [16, 20]);

    const { images, problem } = rendered(dir, [16, 20]);

    assert.equal(problem, null);
    assert.deepEqual(
      images.map((image) => image.size),
      [16, 20],
    );
  });

  it("names a size the CLI didn't write", (t) => {
    const dir = rendersIn(t, [16]);

    assert.match(rendered(dir, [16, 20]).problem ?? "", /20x20\.png/);
  });

  it("names a size whose file isn't a PNG", (t) => {
    const dir = rendersIn(t, [16]);
    writeFileSync(path.join(dir, "20x20.png"), "not a png at all, though it's long enough to read");

    assert.match(rendered(dir, [16, 20]).problem ?? "", /20x20\.png.*PNG/);
  });

  for (const [name, bytes] of [
    ["the wrong width", png(32)],
    ["the wrong height", png(20, { height: 21 })],
    ["16-bit channels", png(20, { bits: 16 })],
    ["no alpha", png(20, { colour: 2 })],
  ]) {
    it(`names a size with ${name}`, (t) => {
      const dir = rendersIn(t, [16]);
      writeFileSync(path.join(dir, "20x20.png"), bytes);

      assert.match(rendered(dir, [16, 20]).problem ?? "", /20x20\.png/);
    });
  }
});

describe("buildIco", () => {
  const images = [
    { size: 16, bytes: png(16) },
    { size: 256, bytes: png(256) },
  ];

  it("writes a header, a directory entry per image and the payloads, contiguous", () => {
    const ico = buildIco(images);

    assert.equal(ico.readUInt16LE(0), 0);
    assert.equal(ico.readUInt16LE(2), 1);
    assert.equal(ico.readUInt16LE(4), 2);
    const firstOffset = ico.readUInt32LE(6 + 12);
    assert.equal(firstOffset, 6 + 2 * 16);
    const firstLength = ico.readUInt32LE(6 + 8);
    assert.equal(ico.readUInt32LE(6 + 16 + 12), firstOffset + firstLength);
    assert.equal(ico.length, firstOffset + firstLength + ico.readUInt32LE(6 + 16 + 8));
  });

  it("stores each size, with 256 as 0, as 32 bits on one plane", () => {
    const ico = buildIco(images);

    assert.equal(ico[6], 16);
    assert.equal(ico[7], 16);
    assert.equal(ico[6 + 16], 0);
    assert.equal(ico[7 + 16], 0);
    assert.equal(ico.readUInt16LE(6 + 4), 1);
    assert.equal(ico.readUInt16LE(6 + 6), 32);
  });

  it("reads back as what went in", () => {
    const back = readIco(buildIco(images));

    assert.deepEqual(
      back.map((image) => image.size),
      [16, 256],
    );
    assert.deepEqual(back[1]?.bytes, images[1]?.bytes);
  });
});

describe("adoptIcon", () => {
  it("writes the icon from every render, leaving the rest of the folder alone", (t) => {
    const out = rendersIn(t);
    const icons = tempDir(t);
    writeFileSync(path.join(icons, "icon.svg"), "<svg/>");

    assert.equal(adoptIcon(out, icons), null);

    assert.deepEqual(
      readIco(readFileSync(path.join(icons, "icon.ico"))).map((image) => image.size),
      ICON_SIZES,
    );
    assert.equal(readFileSync(path.join(icons, "icon.svg"), "utf8"), "<svg/>");
  });

  for (const [name, spoil] of [
    ["a missing render", (out) => rmSync(path.join(out, "32x32.png"))],
    [
      "a render that isn't a PNG",
      (out) => writeFileSync(path.join(out, "32x32.png"), "x".repeat(40)),
    ],
    ["a render of the wrong size", (out) => writeFileSync(path.join(out, "32x32.png"), png(48))],
  ]) {
    it(`names ${name} and leaves the old icon byte-equal`, (t) => {
      const out = rendersIn(t);
      const icons = tempDir(t);
      writeFileSync(path.join(icons, "icon.ico"), "old");
      spoil(out);

      assert.match(adoptIcon(out, icons) ?? "", /32x32\.png/);

      assert.equal(readFileSync(path.join(icons, "icon.ico"), "utf8"), "old");
    });
  }
});

describe("the committed icon", () => {
  it("holds every size, each a PNG of that size", () => {
    const images = readIco(readFileSync(path.join(ICONS, "icon.ico")));

    assert.deepEqual(
      images.map((image) => image.size),
      ICON_SIZES,
    );
    for (const { size, bytes } of images) {
      assert.equal(bytes.readUInt32BE(16), size);
      assert.equal(bytes.readUInt32BE(20), size);
    }
  });

  it("has a tuned 16 px source in the master's colours", () => {
    const colours = (file) =>
      [...readFileSync(path.join(ICONS, file), "utf8").matchAll(/#[0-9a-f]{3,8}/g)]
        .map((match) => match[0])
        .sort();

    assert.deepEqual(colours("icon-16.svg"), colours("icon.svg"));
  });

  it("rebuilds the exe when it changes, which tauri-build alone doesn't", () => {
    const build = readFileSync(path.resolve(ICONS, "..", "build.rs"), "utf8");

    assert.ok(build.includes('println!("cargo:rerun-if-changed=icons/icon.ico");'));
  });

  it("is the logo the app shows, byte for byte", () => {
    const logo = path.resolve(
      ICONS,
      "..",
      "..",
      "..",
      "..",
      "packages",
      "ui",
      "src",
      "assets",
      "orbit.svg",
    );

    assert.deepEqual(readFileSync(logo), readFileSync(path.join(ICONS, "icon.svg")));
  });
});
