// Tests for scripts/app-installer.mjs (ADR-0022, S22, U31): the NSIS installer built without any
// signing key, then found and hashed. The build itself is a stand-in here: no test runs Tauri.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterEach, describe, it } from "node:test";

import {
  CLI_ARGS,
  installerName,
  main,
  SIGNING_VARIABLES,
  signingVariable,
  workspaceVersion,
} from "./app-installer.mjs";
import { recipeBody } from "./justfile.mjs";

/** The temporary folders made so far, removed after each test. */
const made = [];

afterEach(() => {
  for (const dir of made.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

/** A Cargo.toml whose workspace version is `version`. */
function cargoToml(version) {
  return `[workspace]\nmembers = ["crates/*"]\n\n[workspace.package]\nversion = "${version}"\nedition = "2024"\n`;
}

/** A stand-in repository: its Cargo.toml and the bundle folder the build writes into. */
function repo(version = "0.1.0") {
  const root = mkdtempSync(path.join(tmpdir(), "afkfleet-installer-"));
  made.push(root);
  writeFileSync(path.join(root, "Cargo.toml"), cargoToml(version));
  const bundle = path.join(root, "target", "release", "bundle", "nsis");
  mkdirSync(bundle, { recursive: true });
  return { root, bundle };
}

/** A sink for stdout or stderr. */
function sink() {
  const parts = [];
  return { write: (text) => parts.push(text), text: () => parts.join("") };
}

/** main() with a stand-in build that runs `build(bundle)` and answers `status`. */
function run({ root, bundle }, { env = {}, args = [], status = 0, build = () => {} } = {}) {
  const out = sink();
  const err = sink();
  const calls = [];
  const code = main(args, {
    root,
    env,
    out,
    err,
    runCli: (cliArgs) => {
      calls.push(cliArgs);
      build(bundle);
      return status;
    },
  });
  return { code, out: out.text(), err: err.text(), calls };
}

/** Writes the installer of `version` into `bundle`. */
function writeInstaller(bundle, version = "0.1.0", bytes = "installer bytes") {
  writeFileSync(path.join(bundle, installerName(version)), bytes);
}

describe("app-installer", () => {
  it("names the installer as Tauri's NSIS bundler does", () => {
    assert.equal(installerName("0.1.0"), "afkfleet_0.1.0_x64-setup.exe");
    assert.equal(installerName("12.3.45"), "afkfleet_12.3.45_x64-setup.exe");
  });

  it("builds the NSIS bundle alone, skipping every signature", () => {
    assert.deepEqual(CLI_ARGS, ["build", "--bundles", "nsis", "--no-sign"]);
  });

  it("is what its recipe runs", () => {
    assert.deepEqual(recipeBody("app-installer"), ["node scripts/app-installer.mjs"]);
  });

  it("reads the workspace's version, and nothing that isn't X.Y.Z", () => {
    assert.equal(workspaceVersion(cargoToml("0.1.0")), "0.1.0");
    assert.equal(
      workspaceVersion(`[package]\nversion = "9.9.9"\n${cargoToml("1.2.3")}`),
      "1.2.3",
      "the workspace's, not a package's",
    );
    assert.equal(workspaceVersion(cargoToml("0.1")), null);
    assert.equal(workspaceVersion(cargoToml("0.1.0-rc.1")), null);
    assert.equal(workspaceVersion("[workspace]\n"), null);
  });

  it("finds any signing key's variable, set even empty", () => {
    assert.equal(signingVariable({}), null);
    assert.equal(signingVariable({ PATH: "x", TAURI_CONFIG: "{}" }), null);
    for (const name of SIGNING_VARIABLES) {
      assert.equal(signingVariable({ [name]: "" }), name);
    }
    assert.ok(SIGNING_VARIABLES.includes("TAURI_SIGNING_PRIVATE_KEY"));
    assert.ok(SIGNING_VARIABLES.includes("TAURI_SIGNING_PRIVATE_KEY_PASSWORD"));
    assert.ok(SIGNING_VARIABLES.includes("TAURI_PRIVATE_KEY"));
  });

  it("refuses to build while a signing key's variable is set, naming it but never its value", () => {
    for (const name of SIGNING_VARIABLES) {
      const result = run(repo(), { env: { [name]: "a-secret-value" } });

      assert.equal(result.code, 1, name);
      assert.match(result.err, new RegExp(name));
      assert.ok(!result.err.includes("a-secret-value") && !result.out.includes("a-secret-value"));
      assert.deepEqual(result.calls, [], "nothing was built");
    }
  });

  it("refuses arguments with exit 2", () => {
    const result = run(repo(), { args: ["--sign"] });

    assert.equal(result.code, 2);
    assert.deepEqual(result.calls, []);
  });

  it("refuses a workspace without an X.Y.Z version before building", () => {
    const where = repo();
    writeFileSync(path.join(where.root, "Cargo.toml"), "[workspace]\n");

    const result = run(where);

    assert.equal(result.code, 1);
    assert.match(result.err, /version/);
    assert.deepEqual(result.calls, []);
  });

  it("stops when the build fails", () => {
    const result = run(repo(), { status: 3, build: (bundle) => writeInstaller(bundle) });

    assert.equal(result.code, 1);
    assert.match(result.err, /tauri build failed/);
    assert.equal(result.out, "");
  });

  it("refuses a build that left no installer, a stale one, or a signature beside it", () => {
    const missing = run(repo());
    const stale = run(repo(), {
      build: (bundle) => {
        writeInstaller(bundle);
        const old = new Date(Date.now() - 3600_000);
        utimesSync(path.join(bundle, installerName("0.1.0")), old, old);
      },
    });
    const signed = run(repo(), {
      build: (bundle) => {
        writeInstaller(bundle);
        writeFileSync(path.join(bundle, `${installerName("0.1.0")}.sig`), "sig");
      },
    });
    const otherVersion = run(repo(), { build: (bundle) => writeInstaller(bundle, "0.0.9") });

    assert.match(missing.err, /afkfleet_0\.1\.0_x64-setup\.exe/);
    assert.match(stale.err, /older than this build/);
    assert.match(signed.err, /\.sig/);
    assert.match(otherVersion.err, /afkfleet_0\.1\.0_x64-setup\.exe/);
    for (const result of [missing, stale, signed, otherVersion]) {
      assert.equal(result.code, 1);
      assert.equal(result.out, "");
    }
  });

  it("prints the installer's SHA-256 and name, as SHA256SUMS lists them, and adds it to CI's summary", () => {
    const where = repo("0.2.0");
    const summary = path.join(where.root, "summary.md");
    writeFileSync(summary, "");

    const result = run(where, {
      env: { GITHUB_STEP_SUMMARY: summary },
      build: (bundle) => writeInstaller(bundle, "0.2.0", "the setup exe"),
    });

    const sha = createHash("sha256").update("the setup exe").digest("hex");
    assert.equal(result.code, 0, result.err);
    assert.equal(result.out, `${sha}  afkfleet_0.2.0_x64-setup.exe\n`);
    assert.deepEqual(result.calls, [CLI_ARGS]);
    assert.match(
      readFileSync(summary, "utf8"),
      new RegExp(`${sha}  afkfleet_0\\.2\\.0_x64-setup\\.exe`),
    );
  });
});
