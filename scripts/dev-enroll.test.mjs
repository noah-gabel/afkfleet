// Tests for dev-enroll.mjs. Run with `just scripts-test`.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { afterEach, beforeEach, describe, it } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

import { ENROLL, enrollInto } from "./dev-enroll.mjs";

const SCRIPTS = path.dirname(fileURLToPath(import.meta.url));
const SCRIPT = path.join(SCRIPTS, "dev-enroll.mjs");
const JUSTFILE = path.join(SCRIPTS, "..", "justfile");

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

/** The lines of `recipe`'s body in the justfile, without their indentation. */
function recipeBody(recipe) {
  const lines = readFileSync(JUSTFILE, "utf8").split("\n");
  const start = lines.findIndex((line) => line.startsWith(`${recipe} `) || line === `${recipe}:`);
  assert.notEqual(start, -1, `the justfile has no recipe ${recipe}`);
  const body = [];
  for (const line of lines.slice(start + 1)) {
    if (!line.startsWith("    ")) {
      break;
    }
    body.push(line.trim());
  }
  return body;
}

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

  it("reports a command that can't start with exit 1", () => {
    const status = enrollInto(dir, path.join(dir, "no-such-command"), []);

    assert.equal(status, 1);
  });

  it("runs the agent's enroll with the managed dev config", () => {
    assert.deepEqual(ENROLL, [
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
});

describe("the command line", () => {
  it("refuses anything but one absolute folder with exit 2", () => {
    for (const args of [[], ["target/dev-agent"], [dir, dir]]) {
      const ran = spawnSync(process.execPath, [SCRIPT, ...args], { encoding: "utf8" });

      assert.equal(ran.status, 2, JSON.stringify(args));
      assert.match(ran.stderr, /^usage: /);
    }
  });
});

describe("just dev-enroll", () => {
  it("is one line, so no earlier line can take the piped bundle", () => {
    assert.deepEqual(recipeBody("dev-enroll"), ['node scripts/dev-enroll.mjs "{{ dir }}"']);
  });
});

/** The parameters of `recipe`'s header, by name, each with its default. */
function recipeParameters(recipe) {
  const header = readFileSync(JUSTFILE, "utf8")
    .split("\n")
    .find((line) => line.startsWith(`${recipe} `));
  assert.ok(header, `the justfile has no recipe ${recipe}`);
  const parameters = {};
  for (const [, name, value] of header.matchAll(
    /(\$?[A-Za-z_][A-Za-z0-9_]*)=(\((?:[^()]|\([^()]*\))*\)|"[^"]*")/g,
  )) {
    parameters[name] = value;
  }
  return parameters;
}

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
