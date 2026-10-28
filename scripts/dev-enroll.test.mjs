// Tests for dev-enroll.mjs. Run with `just scripts-test`.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { afterEach, beforeEach, describe, it } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

import { enrollCommand, parseArgs } from "./dev-enroll.mjs";
import { recipeBody, recipeParameters } from "./justfile.mjs";

const SCRIPTS = path.dirname(fileURLToPath(import.meta.url));
const SCRIPT = path.join(SCRIPTS, "dev-enroll.mjs");

/** A stand-in for the agent: counts the bytes on its stdin, prints the count, exits 3. */
const COUNTING = [
  "-e",
  "let n = 0; process.stdin.on('data', (d) => { n += d.length; }).on('end', () => { process.stdout.write(String(n)); process.exit(3); });",
];

let dir;

beforeEach(() => {
  dir = mkdtempSync(path.join(tmpdir(), "dev-enroll-"));
});

afterEach(() => {
  rmSync(dir, { recursive: true, force: true });
});

describe("enrollInto", () => {
  it("creates the folder and runs the command on this process's stdin and stdout, with its exit code", () => {
    const folder = path.join(dir, "target", "dev-agent");
    const runner = [
      "--input-type=module",
      "-e",
      `import { enrollInto } from ${JSON.stringify(pathToFileURL(SCRIPT).href)};
       process.exitCode = enrollInto(${JSON.stringify(folder)}, process.execPath, ${JSON.stringify(COUNTING)});`,
    ];

    const ran = spawnSync(process.execPath, runner, {
      input: `${"x".repeat(146)}\n`,
      encoding: "utf8",
    });

    assert.equal(ran.status, 3, ran.stderr);
    assert.equal(ran.stdout, "147");
    assert.ok(existsSync(folder));
  });

  it("reports a command that can't start with exit 1, in words that suit any recipe", () => {
    const command = path.join(dir, "no-such-command");
    const runner = [
      "--input-type=module",
      "-e",
      `import { enrollInto } from ${JSON.stringify(pathToFileURL(SCRIPT).href)};
       process.exitCode = enrollInto(${JSON.stringify(dir)}, ${JSON.stringify(command)}, []);`,
    ];

    const ran = spawnSync(process.execPath, runner, { encoding: "utf8" });

    assert.equal(ran.status, 1);
    assert.ok(ran.stderr.startsWith(`${command} can't be started: `), ran.stderr);
  });
});

describe("enrollCommand", () => {
  it("runs the agent's enroll with the config it's given", () => {
    assert.deepEqual(enrollCommand(false, "deploy/dev/agent.managed.toml"), [
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
    ]);
  });

  it("builds it in release only when asked, its progress shown, since a first build takes minutes", () => {
    assert.deepEqual(enrollCommand(true, "deploy/dod/agent.toml"), [
      "cargo",
      [
        "run",
        "--release",
        "-p",
        "fleet-agent",
        "--bin",
        "afkfleet-agent",
        "--",
        "enroll",
        "--config",
        "deploy/dod/agent.toml",
      ],
    ]);
  });
});

describe("the command line", () => {
  it("reads an optional --release, the config and one absolute folder", () => {
    assert.deepEqual(parseArgs(["deploy/dev/agent.managed.toml", dir]), {
      release: false,
      config: "deploy/dev/agent.managed.toml",
      folder: dir,
    });
    assert.deepEqual(parseArgs(["--release", "deploy/dod/agent.toml", dir]), {
      release: true,
      config: "deploy/dod/agent.toml",
      folder: dir,
    });
  });

  it("refuses anything else with exit 2", () => {
    for (const args of [
      [],
      [dir],
      ["--release", dir],
      ["deploy/dev/agent.managed.toml"],
      ["deploy/dev/agent.managed.toml", "target/dev-agent"],
      ["deploy/dev/agent.managed.toml", dir, dir],
      ["--debug", "deploy/dev/agent.managed.toml", dir],
    ]) {
      const ran = spawnSync(process.execPath, [SCRIPT, ...args], { encoding: "utf8" });

      assert.equal(ran.status, 2, JSON.stringify(args));
      assert.match(ran.stderr, /^usage: /);
    }
  });
});

describe("just dev-enroll", () => {
  it("is one line, so no earlier line can take the piped bundle", () => {
    assert.deepEqual(recipeBody("dev-enroll"), [
      'node scripts/dev-enroll.mjs deploy/dev/agent.managed.toml "{{ dir }}"',
    ]);
  });
});

describe("just dev-managed-agent", () => {
  it("runs the identity dev-enroll leaves: the same folder, CA, key and certificate", () => {
    const enroll = recipeParameters("dev-enroll");
    const managed = recipeParameters("dev-managed-agent");
    for (const name of [
      "dir",
      "$AFKFLEET_AGENT__CONTROL_PLANE__CA_CERT_FILE",
      "$AFKFLEET_AGENT__CONTROL_PLANE__CERT_FILE",
      "$AFKFLEET_AGENT__CONTROL_PLANE__KEY_FILE",
    ]) {
      assert.ok(enroll[name], `dev-enroll has ${name}`);
      assert.equal(managed[name], enroll[name], name);
    }
  });
});
