// Tests for the DoD checks' recipes in the justfile (ADR-0020, U1, U33, U50–U52, S72,
// R123–R126). Run with `just scripts-test`. They read the recipes only: the AI never runs a
// dod-* recipe, and nothing here touches target/dod-run. The Rust tests load the configs
// with each recipe's exports (fleet-server's and fleet-agent's config tests,
// fleet-integration's dod_kit).
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { recipeBody, recipeDependencies, recipeParameters } from "./justfile.mjs";

/** Every DoD recipe, the cleanup included. */
const RECIPES = [
  "dod-mc-up",
  "dod-mc-down",
  "dod-server",
  "dod-owner",
  "dod-enrollment-token",
  "dod-enroll",
  "dod-agent",
  "dod-app",
  "dod-clean",
];

/** The recipes that run a server or agent binary, and the log filter each exports. */
const BINARIES = {
  "dod-server": "$AFKFLEET_SERVER__LOG__FILTER",
  "dod-owner": "$AFKFLEET_SERVER__LOG__FILTER",
  "dod-enrollment-token": "$AFKFLEET_SERVER__LOG__FILTER",
  "dod-enroll": "$AFKFLEET_AGENT__LOG__FILTER",
  "dod-agent": "$AFKFLEET_AGENT__LOG__FILTER",
};

/**
 * A path default's segments from the justfile's directory: `(justfile_directory() / "a")`,
 * or `(name / "a")` after an earlier path parameter `name` of the same recipe.
 */
function segments(parameters, value) {
  const terms = value
    .slice(1, -1)
    .split("/")
    .map((term) => term.trim());
  const [first, ...rest] = terms;
  const base =
    first === "justfile_directory()" ? [] : segments(parameters, parameters[first] ?? "(?)");
  return [...base, ...rest.map((term) => term.replace(/^"|"$/g, ""))];
}

/** Every parameter of `recipe` whose default is a path, with its segments. */
function paths(recipe) {
  const parameters = recipeParameters(recipe);
  return Object.entries(parameters)
    .filter(([, value]) => value?.startsWith("("))
    .map(([name, value]) => [name, segments(parameters, value)]);
}

describe("the DoD recipes", () => {
  it("are all there, each with a body", () => {
    for (const recipe of RECIPES) {
      assert.ok(recipeBody(recipe).length > 0, recipe);
    }
  });

  it("keep every path under target/dod-run", () => {
    for (const recipe of RECIPES) {
      for (const [name, path] of paths(recipe)) {
        assert.deepEqual(path.slice(0, 2), ["target", "dod-run"], `${recipe} ${name}`);
      }
    }
  });

  it("run every binary in release, logging at info", () => {
    for (const [recipe, filter] of Object.entries(BINARIES)) {
      assert.equal(recipeParameters(recipe)[filter], '"info"', recipe);
      const runs = recipeBody(recipe).filter(
        (line) => line.startsWith("cargo ") || /^node scripts\/dev-(pki|enroll)\.mjs /.test(line),
      );
      assert.ok(runs.length > 0, recipe);
      for (const line of runs) {
        assert.match(line, / --release /, `${recipe}: ${line}`);
      }
    }
  });

  it("name only the DoD configs and the DoD compose file", () => {
    for (const recipe of RECIPES) {
      for (const line of recipeBody(recipe)) {
        assert.doesNotMatch(line, /deploy\/dev|compose\.dev/, `${recipe}: ${line}`);
        for (const [, file] of line.matchAll(/(deploy\/\S+)/g)) {
          assert.match(file, /^deploy\/(dod\/(server|agent)\.toml|compose\.dod\.yaml)$/, recipe);
        }
      }
    }
  });

  it("put no secret on a command line: the Owner's password is asked at the terminal", () => {
    const owner = recipeBody("dod-owner").find((line) => line.includes("create-owner"));

    assert.ok(owner);
    assert.doesNotMatch(owner, /password/);
    assert.match(owner, /--username "\{\{ username \}\}"$/);
  });
});

describe("just dod-enroll", () => {
  it("is one line, so no earlier line can take the piped bundle", () => {
    assert.deepEqual(recipeBody("dod-enroll"), [
      'node scripts/dev-enroll.mjs --release deploy/dod/agent.toml "{{ dir }}"',
    ]);
  });
});

describe("just dod-clean", () => {
  it("passes only its folder, target/dod-run, and never touches Docker", () => {
    const [[name, folder]] = paths("dod-clean");

    assert.equal(name, "dir");
    assert.deepEqual(folder, ["target", "dod-run"]);
    assert.deepEqual(recipeBody("dod-clean"), ['node scripts/dod-clean.mjs "{{ dir }}"']);
  });

  it("checks the files the server and the agent write: the database's -wal and the heartbeat", () => {
    const server = Object.fromEntries(paths("dod-server"));
    const agent = Object.fromEntries(paths("dod-agent"));

    // dod-clean refuses while target/dod-run/afkfleet.db-wal or agent/agent.alive is there (U52).
    assert.deepEqual(server.$AFKFLEET_SERVER__DATABASE__PATH, ["target", "dod-run", "afkfleet.db"]);
    assert.deepEqual(agent.$AFKFLEET_AGENT__RUNTIME__HEARTBEAT_FILE, [
      "target",
      "dod-run",
      "agent",
      "agent.alive",
    ]);
  });
});

describe("just dod-mc-down", () => {
  it("removes the container with its volumes, which hold the player's name", () => {
    assert.deepEqual(recipeBody("dod-mc-down"), [
      "docker compose --file deploy/compose.dod.yaml down --volumes",
    ]);
  });
});

describe("just dod-app", () => {
  it("builds the release app first, then says the address and starts it", () => {
    assert.deepEqual(recipeDependencies("dod-app"), ["app-build"]);
    assert.deepEqual(recipeBody("dod-app"), [
      `@echo "Enter http://127.0.0.1:8090 as the server's address in the app."`,
      "target/release/afkfleet-desktop.exe",
    ]);
  });
});
