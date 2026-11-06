// Tests for scripts/updater-sign.mjs (ADR-0022, S24, U1): the maintainer's signing step. No real
// key is ever used: the cases that sign make throwaway key pairs with the pinned Tauri CLI, in a
// temporary folder outside the repository, with an empty password. The refusals run against a
// stand-in that records whether the CLI would have run.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { EventEmitter } from "node:events";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { after, afterEach, before, describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { recipeBody } from "./justfile.mjs";
import {
  downloadUrl,
  keyId,
  main,
  manifest,
  readHidden,
  signatureFacts,
  signFile,
} from "./updater-sign.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
/** The pinned CLI, as the repository resolves it. */
const CLI = createRequire(path.join(ROOT, "apps", "desktop", "package.json")).resolve(
  "@tauri-apps/cli/tauri.js",
);

/** Folders removed after each test, and the key pairs' folder after all. */
const made = [];
let keys;

afterEach(() => {
  for (const dir of made.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

/** A throwaway key pair, empty password, made by the pinned CLI in `dir`. */
function generate(dir, name) {
  const key = path.join(dir, name);
  const result = spawnSync(process.execPath, [CLI, "signer", "generate", "--ci", "-w", key], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  assert.equal(result.status, 0, result.stderr);
  return { key, pubkey: readFileSync(`${key}.pub`, "utf8").trim() };
}

before(() => {
  const dir = mkdtempSync(path.join(tmpdir(), "afkfleet-updater-keys-"));
  keys = { dir, one: generate(dir, "one.key"), two: generate(dir, "two.key") };
});

after(() => {
  rmSync(keys.dir, { recursive: true, force: true });
});

/** A stand-in repository whose committed public key is `pubkey`. */
function repo(pubkey) {
  const root = mkdtempSync(path.join(tmpdir(), "afkfleet-updater-repo-"));
  made.push(root);
  writeFileSync(
    path.join(root, "Cargo.toml"),
    '[workspace.package]\nversion = "0.2.0"\nrepository = "https://github.com/example/afkfleet"\n',
  );
  const srcTauri = path.join(root, "apps", "desktop", "src-tauri");
  mkdirSync(srcTauri, { recursive: true });
  writeFileSync(
    path.join(srcTauri, "tauri.conf.json"),
    JSON.stringify({ plugins: { updater: { pubkey } } }),
  );
  return root;
}

/** An installer of `version` in a download folder of its own; its path and SHA-256. */
function download(version = "0.2.0", bytes = "the setup exe") {
  const dir = mkdtempSync(path.join(tmpdir(), "afkfleet-updater-download-"));
  made.push(dir);
  const file = path.join(dir, `afkfleet_${version}_x64-setup.exe`);
  writeFileSync(file, bytes);
  return { file, sha: createHash("sha256").update(bytes).digest("hex") };
}

/** A sink for stdout or stderr. */
function sink() {
  const parts = [];
  return { write: (text) => parts.push(text), text: () => parts.join("") };
}

/**
 * main() in `root`: `sign` stands in for the CLI unless `real` (then the pinned CLI signs, with
 * an empty password), a terminal unless `tty` is false.
 */
function run(root, args, { real = false, tty = true } = {}) {
  const out = sink();
  const err = sink();
  const signed = [];
  const code = main(args, {
    root,
    cli: CLI,
    out,
    err,
    stdinIsTTY: tty,
    now: () => new Date("2026-11-07T12:34:56.789Z"),
    readPassword: async () => (real ? "" : "a-password"),
    signFile: real
      ? (options) => signFile({ ...options, password: null })
      : (options) => {
          signed.push(options);
          return { status: 0, stderr: "" };
        },
  });
  return Promise.resolve(code).then((exit) => ({
    code: exit,
    out: out.text(),
    err: err.text(),
    signed,
  }));
}

describe("updater-sign", () => {
  it("is what its recipe runs, the four arguments passed through", () => {
    assert.deepEqual(recipeBody("updater-sign"), [
      'node scripts/updater-sign.mjs "{{ key }}" "{{ installer }}" "{{ sha256 }}" "{{ version }}"',
    ]);
  });

  it("reads a public key's ID as minisign prints it", () => {
    const id = keyId(keys.one.pubkey);

    assert.match(id, /^[0-9A-F]{16}$/);
    assert.notEqual(id, keyId(keys.two.pubkey));
    assert.match(Buffer.from(keys.one.pubkey, "base64").toString("utf8"), new RegExp(id));
  });

  it("builds the download address from the repository and the version's tag", () => {
    assert.equal(
      downloadUrl("https://github.com/example/afkfleet", "0.2.0"),
      "https://github.com/example/afkfleet/releases/download/v0.2.0/afkfleet_0.2.0_x64-setup.exe",
    );
  });

  it("writes the manifest's fields alone: version, date, and the Windows release", () => {
    assert.deepEqual(
      manifest({
        version: "0.2.0",
        signature: "c2ln",
        url: "https://example.com/a.exe",
        now: new Date("2026-11-07T12:34:56.789Z"),
      }),
      {
        version: "0.2.0",
        pub_date: "2026-11-07T12:34:56Z",
        platforms: {
          "windows-x86_64": { signature: "c2ln", url: "https://example.com/a.exe" },
        },
      },
    );
  });

  it("reads a typed password without echoing it, and only up to Enter", async () => {
    const input = new EventEmitter();
    const modes = [];
    input.isTTY = true;
    input.setRawMode = (on) => modes.push(on);
    input.setEncoding = () => {};
    input.resume = () => {};
    input.pause = () => {};
    const output = sink();

    const reading = readHidden("Password: ", input, output);
    input.emit("data", "se");
    input.emit("data", "cx\u007fret\r");
    const password = await reading;

    assert.equal(password, "secret");
    assert.equal(output.text(), "Password: \n");
    assert.deepEqual(modes, [true, false]);
  });

  it("stops reading a password at Ctrl+C", async () => {
    const input = new EventEmitter();
    input.isTTY = true;
    input.setRawMode = () => {};
    input.setEncoding = () => {};
    input.resume = () => {};
    input.pause = () => {};

    const reading = readHidden("Password: ", input, sink());
    input.emit("data", "abc\u0003");

    await assert.rejects(reading, /cancelled/);
  });

  it("refuses a usage error with exit 2, signing nothing", async () => {
    const root = repo(keys.one.pubkey);

    for (const args of [[], ["a", "b", "c"], ["a", "b", "c", "d", "e"]]) {
      const result = await run(root, args);
      assert.equal(result.code, 2, JSON.stringify(args));
      assert.deepEqual(result.signed, []);
    }
  });

  it("refuses before the CLI runs: a bad version, name, hash, path or key, or no terminal", async () => {
    const root = repo(keys.one.pubkey);
    const { file, sha } = download();
    const inside = path.join(root, "afkfleet-updater.key");
    writeFileSync(inside, "not a real key");
    const cases = [
      [[keys.one.key, file, sha, "0.2"], {}, /X\.Y\.Z/],
      [[keys.one.key, file, sha, "0.3.0"], {}, /afkfleet_0\.3\.0_x64-setup\.exe/],
      [[keys.one.key, file, "abc", "0.2.0"], {}, /SHA-256/],
      [["one.key", file, sha, "0.2.0"], {}, /absolute/],
      [[keys.one.key, "afkfleet_0.2.0_x64-setup.exe", sha, "0.2.0"], {}, /absolute/],
      [[inside, file, sha, "0.2.0"], {}, /inside the repository/],
      [[keys.dir, file, sha, "0.2.0"], {}, /isn't a file/],
      [[keys.one.key, file, sha, "0.2.0"], { tty: false }, /terminal/],
    ];

    for (const [args, options, reason] of cases) {
      const result = await run(root, args, options);
      assert.equal(result.code, 1, `${args}: ${result.err}`);
      assert.match(result.err, reason);
      assert.equal(result.out, "");
      assert.deepEqual(result.signed, [], "the CLI never ran");
    }
    assert.ok(!existsSync(path.join(root, "target", "updater-sign", "v0.2.0")), "nothing left");
  });

  it("refuses a download whose SHA-256 differs, and leaves nothing", async () => {
    const root = repo(keys.one.pubkey);
    const { file } = download();

    const result = await run(root, [keys.one.key, file, "0".repeat(64), "0.2.0"]);

    assert.equal(result.code, 1);
    assert.match(result.err, /SHA-256/);
    assert.deepEqual(result.signed, []);
    assert.ok(!existsSync(path.join(root, "target", "updater-sign", "v0.2.0")));
  });

  it("refuses to sign a version twice: the output folder must be new", async () => {
    const root = repo(keys.one.pubkey);
    const { file, sha } = download();
    mkdirSync(path.join(root, "target", "updater-sign", "v0.2.0"), { recursive: true });

    const result = await run(root, [keys.one.key, file, sha, "0.2.0"]);

    assert.equal(result.code, 1);
    assert.match(result.err, /exists/);
    assert.deepEqual(result.signed, []);
  });

  it("passes the key by path and the version, never a password argument, with no signing variable", async () => {
    const root = repo(keys.one.pubkey);
    const { file, sha } = download();

    const result = await run(root, [keys.one.key, file, sha, "0.2.0"]);

    // The stand-in signs nothing, so the run then refuses for the missing signature.
    assert.equal(result.code, 1);
    assert.match(result.err, /no signature/);
    assert.equal(result.signed.length, 1);
    const [options] = result.signed;
    assert.equal(options.key, keys.one.key);
    assert.equal(options.version, "0.2.0");
    assert.equal(options.password, "a-password");
    assert.equal(path.basename(options.file), "afkfleet_0.2.0_x64-setup.exe");
    assert.ok(!result.out.includes("a-password") && !result.err.includes("a-password"));
  });

  it("signs with the pinned CLI: only the signature's path on stdout, latest.json beside it", async () => {
    const root = repo(keys.one.pubkey);
    const { file, sha } = download();

    const result = await run(root, [keys.one.key, file, sha.toUpperCase(), "0.2.0"], {
      real: true,
    });

    assert.equal(result.code, 0, result.err);
    const folder = path.join(root, "target", "updater-sign", "v0.2.0");
    const sig = path.join(folder, "afkfleet_0.2.0_x64-setup.exe.sig");
    assert.equal(result.out, `${sig}\n`);
    const signature = readFileSync(sig, "utf8");
    const facts = signatureFacts(signature);
    assert.equal(facts.keyId, keyId(keys.one.pubkey));
    assert.equal(facts.file, "afkfleet_0.2.0_x64-setup.exe");
    assert.equal(facts.version, "0.2.0");
    assert.deepEqual(JSON.parse(readFileSync(path.join(folder, "latest.json"), "utf8")), {
      version: "0.2.0",
      pub_date: "2026-11-07T12:34:56Z",
      platforms: {
        "windows-x86_64": {
          signature,
          url: "https://github.com/example/afkfleet/releases/download/v0.2.0/afkfleet_0.2.0_x64-setup.exe",
        },
      },
    });
    assert.ok(!result.out.includes(signature.trim()), "the signature itself isn't printed");
  });

  it("refuses a signature by another key than the committed one, and leaves nothing", async () => {
    const root = repo(keys.one.pubkey);
    const { file, sha } = download();

    const result = await run(root, [keys.two.key, file, sha, "0.2.0"], { real: true });

    assert.equal(result.code, 1);
    assert.match(result.err, /key/);
    assert.equal(result.out, "");
    assert.ok(!existsSync(path.join(root, "target", "updater-sign", "v0.2.0")));
  });

  it("signs without a version when asked: the signature then carries none", () => {
    const dir = mkdtempSync(path.join(tmpdir(), "afkfleet-updater-plain-"));
    made.push(dir);
    const file = path.join(dir, "afkfleet_0.2.0_x64-setup.exe");
    writeFileSync(file, "bytes");

    const result = signFile({ cli: CLI, key: keys.one.key, file, version: null, password: null });

    assert.equal(result.status, 0, result.stderr);
    const facts = signatureFacts(readFileSync(`${file}.sig`, "utf8"));
    assert.equal(facts.version, null);
    assert.equal(facts.file, "afkfleet_0.2.0_x64-setup.exe");
  });
});
