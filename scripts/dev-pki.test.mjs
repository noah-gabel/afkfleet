// Tests for dev-pki.mjs. Run with `just scripts-test`.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { afterEach, beforeEach, describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { ensurePki, NAMES, parseArgs, pkiInitCommand } from "./dev-pki.mjs";
import { recipeBody } from "./justfile.mjs";

const SCRIPTS = path.dirname(fileURLToPath(import.meta.url));
const SCRIPT = path.join(SCRIPTS, "dev-pki.mjs");

/** The config dev-server passes. */
const DEV = "deploy/dev/server.toml";

let dir;

beforeEach(() => {
  dir = mkdtempSync(path.join(tmpdir(), "dev-pki-"));
});

afterEach(() => {
  rmSync(dir, { recursive: true, force: true });
});

/** The four files in `folder`. */
function files(folder) {
  return ["ca.crt", "ca.key", "server.crt", "server.key"].map((name) => path.join(folder, name));
}

/** A stand-in for pki init that counts its runs and ends with `status`. */
function counting(status) {
  const init = () => {
    init.runs += 1;
    return status;
  };
  init.runs = 0;
  return init;
}

describe("ensurePki", () => {
  it("keeps a PKI whose four files exist, without running pki init or reading them", () => {
    const paths = files(dir);
    for (const file of paths) {
      writeFileSync(file, "the dev PKI");
    }
    const init = counting(0);

    const result = ensurePki(paths, init);

    assert.equal(result, "kept");
    assert.equal(init.runs, 0);
    for (const file of paths) {
      assert.equal(readFileSync(file, "utf8"), "the dev PKI");
    }
  });

  it("runs pki init once, in a folder it creates, when no file exists", () => {
    const paths = files(path.join(dir, "target", "dev-server", "pki"));
    const init = counting(0);

    const result = ensurePki(paths, init);

    assert.equal(result, "created");
    assert.equal(init.runs, 1);
    assert.ok(existsSync(path.dirname(paths[0])));
  });

  it("leaves a partial PKI to pki init, which refuses it, and reports the failure", () => {
    const paths = files(dir);
    writeFileSync(paths[0], "the old CA");
    const init = counting(1);

    const result = ensurePki(paths, init);

    assert.equal(result, "failed");
    assert.equal(init.runs, 1);
    assert.equal(readFileSync(paths[0], "utf8"), "the old CA");
  });

  it("names the dev server certificate for every address just dev-enroll may dial", () => {
    assert.deepEqual(NAMES, ["--dns", "localhost", "--ip", "127.0.0.1", "--ip", "::1"]);
  });
});

describe("pkiInitCommand", () => {
  it("runs pki init with the config it's given and the names", () => {
    assert.deepEqual(pkiInitCommand(false, DEV), [
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
        DEV,
        ...NAMES,
      ],
    ]);
  });

  it("builds it in release only when asked", () => {
    assert.deepEqual(pkiInitCommand(true, "deploy/dod/server.toml"), [
      "cargo",
      [
        "run",
        "--release",
        "-p",
        "fleet-server",
        "--bin",
        "afkfleet-server",
        "--",
        "pki",
        "init",
        "--config",
        "deploy/dod/server.toml",
        ...NAMES,
      ],
    ]);
  });

  it("names the DoD agent's address too, which dials the DoD server by its IP", () => {
    const agent = readFileSync(path.join(SCRIPTS, "..", "deploy", "dod", "agent.toml"), "utf8");
    const host = agent.match(/^url = "https:\/\/([^:/"]+):\d+"$/m)?.[1];

    assert.equal(host, "127.0.0.1");
    assert.ok(NAMES.some((name, at) => name === host && NAMES[at - 1] === "--ip"));
  });
});

describe("the command line", () => {
  it("reads an optional --release, the config and four absolute paths", () => {
    const paths = files(dir);

    assert.deepEqual(parseArgs([DEV, ...paths]), { release: false, config: DEV, files: paths });
    assert.deepEqual(parseArgs(["--release", "deploy/dod/server.toml", ...paths]), {
      release: true,
      config: "deploy/dod/server.toml",
      files: paths,
    });
  });

  it("refuses anything else with exit 2", () => {
    for (const args of [
      [],
      files(dir),
      ["--release", ...files(dir)],
      [DEV, ...files(dir).slice(0, 3)],
      [DEV, "ca.crt", "ca.key", "server.crt", "server.key"],
      [DEV, ...files(dir), "extra"],
    ]) {
      const ran = spawnSync(process.execPath, [SCRIPT, ...args], { encoding: "utf8" });

      assert.equal(ran.status, 2, JSON.stringify(args));
      assert.match(ran.stderr, /^usage: /);
    }
  });

  it("does nothing and exits 0 when the PKI exists", () => {
    const paths = files(dir);
    for (const file of paths) {
      writeFileSync(file, "the dev PKI");
    }

    const ran = spawnSync(process.execPath, [SCRIPT, DEV, ...paths], { encoding: "utf8" });

    assert.equal(ran.status, 0, ran.stderr);
    assert.equal(ran.stdout, "");
  });
});

describe("just dev-server", () => {
  it("creates the PKI with the dev config, not in release", () => {
    assert.ok(
      recipeBody("dev-server").includes(
        'node scripts/dev-pki.mjs deploy/dev/server.toml "{{ AFKFLEET_SERVER__GRPC__CA_CERT_FILE }}" "{{ AFKFLEET_SERVER__GRPC__CA_KEY_FILE }}" "{{ AFKFLEET_SERVER__GRPC__SERVER_CERT_FILE }}" "{{ AFKFLEET_SERVER__GRPC__SERVER_KEY_FILE }}"',
      ),
    );
  });
});
