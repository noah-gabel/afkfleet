// Tests for mutants.mjs. Run with `just scripts-test`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { GATES } from "./coverage-gates.mjs";
import { recipeBody } from "./justfile.mjs";
import {
  DEFAULT_SHARDS,
  gateFilters,
  MAX_SHARDS,
  mutantsArgs,
  outcomeLines,
  parseArgs,
  registeredTrees,
  SCOPES,
  selection,
  treePath,
  verdict,
} from "./mutants.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

const SOME_GATES = [
  { name: "workspace", min: 80, pattern: /^/ },
  {
    name: "auth",
    prefixes: [
      "crates/fleet-core/src/auth/",
      "crates/fleet-server/src/app/auth.rs",
      "crates/fleet-server/src/http/extractors/",
      "crates/fleet-api-types/src/secret.rs",
    ],
  },
  {
    name: "vault",
    prefixes: ["crates/fleet-startup/src/secret_file.rs", "crates/fleet-server/src/app/vault.rs"],
  },
  { name: "msauth", prefixes: ["crates/fleet-server/src/infra/msauth/"] },
];

describe("parseArgs", () => {
  it("runs one scope in the default number of shards", () => {
    assert.deepEqual(parseArgs(["core"]), { scopes: ["core"], shards: DEFAULT_SHARDS });
  });

  it("runs every scope for `all`, in the shards given", () => {
    assert.deepEqual(parseArgs(["all", "2"]), { scopes: SCOPES, shards: 2 });
    assert.deepEqual(parseArgs(["auth-vault", String(MAX_SHARDS)]), {
      scopes: ["auth-vault"],
      shards: MAX_SHARDS,
    });
  });

  it("refuses no scope, an unknown one and anything after the shards", () => {
    assert.match(parseArgs([]).error, /^usage: /);
    assert.match(parseArgs(["core", "2", "more"]).error, /^usage: /);
    assert.match(
      parseArgs(["server"]).error,
      /no scope is named `server`: core, auth-vault, or all/,
    );
  });

  it("refuses a number of shards that isn't a whole number from 1 to the most", () => {
    for (const shards of ["0", "-1", "1.5", "four", "", String(MAX_SHARDS + 1), "04"]) {
      assert.match(parseArgs(["core", shards]).error, /whole number from 1 to 8/, shards);
    }
  });
});

describe("gateFilters", () => {
  it("takes the auth and vault gates' entries outside fleet-core, a folder with everything below it", () => {
    assert.deepEqual(gateFilters(SOME_GATES), {
      packages: ["fleet-api-types", "fleet-server", "fleet-startup"],
      files: [
        "crates/fleet-server/src/app/auth.rs",
        "crates/fleet-server/src/http/extractors/**",
        "crates/fleet-api-types/src/secret.rs",
        "crates/fleet-startup/src/secret_file.rs",
        "crates/fleet-server/src/app/vault.rs",
      ],
    });
  });

  it("throws when a gate is missing or an entry isn't a crate's file", () => {
    assert.throws(() => gateFilters([SOME_GATES[1]]), /no coverage gate is named vault/);
    assert.throws(
      () => gateFilters([{ name: "auth", prefixes: ["apps/desktop/src/x.rs"] }, SOME_GATES[2]]),
      /apps\/desktop\/src\/x\.rs isn't under crates\//,
    );
  });

  it("finds entries in the real gates, with fleet-server's auth service among them", () => {
    const { packages, files } = gateFilters(GATES);

    assert.deepEqual(packages, ["fleet-api-types", "fleet-server", "fleet-startup"]);
    assert.ok(files.includes("crates/fleet-server/src/app/auth.rs"));
    assert.ok(files.includes("crates/fleet-server/src/infra/crypto/vault.rs"));
    assert.ok(files.every((file) => !file.startsWith("crates/fleet-core/")));
  });
});

describe("selection", () => {
  it("is all of fleet-core for `core`", () => {
    assert.deepEqual(selection("core", SOME_GATES), ["-p", "fleet-core"]);
  });

  it("is the gates' packages and files for `auth-vault`", () => {
    assert.deepEqual(selection("auth-vault", SOME_GATES), [
      "-p",
      "fleet-api-types",
      "-p",
      "fleet-server",
      "-p",
      "fleet-startup",
      "-f",
      "crates/fleet-server/src/app/auth.rs",
      "-f",
      "crates/fleet-server/src/http/extractors/**",
      "-f",
      "crates/fleet-api-types/src/secret.rs",
      "-f",
      "crates/fleet-startup/src/secret_file.rs",
      "-f",
      "crates/fleet-server/src/app/vault.rs",
    ]);
  });
});

describe("mutantsArgs", () => {
  it("runs one shard in place, with its report in the folder given", () => {
    assert.deepEqual(mutantsArgs(["-p", "fleet-core"], 2, 4, "C:\\path\\to\\out"), [
      "mutants",
      "--in-place",
      "--shard",
      "1/4",
      "-o",
      "C:\\path\\to\\out",
      "-p",
      "fleet-core",
    ]);
  });

  // cargo-mutants counts its shards from 0 and refuses `4/4`: "shard k must be less than n".
  it("names the shards from 0, as cargo-mutants counts them", () => {
    const shardOf = (shard) => mutantsArgs([], shard, 4, "out")[3];

    assert.deepEqual([1, 2, 3, 4].map(shardOf), ["0/4", "1/4", "2/4", "3/4"]);
  });
});

describe("treePath", () => {
  it("is a folder per shard under target/mutants", () => {
    assert.equal(treePath(3).replaceAll("\\", "/"), "target/mutants/tree-3");
  });
});

describe("registeredTrees", () => {
  it("gives every worktree's path, with `/`, whatever git wrote", () => {
    const porcelain = [
      "worktree C:/path/to/afkfleet",
      "HEAD 1111111111111111111111111111111111111111",
      "branch refs/heads/main",
      "",
      "worktree C:/path/to/afkfleet/target/mutants/tree-1",
      "HEAD 2222222222222222222222222222222222222222",
      "detached",
      "",
    ].join("\n");

    assert.deepEqual(registeredTrees(porcelain), [
      "C:/path/to/afkfleet",
      "C:/path/to/afkfleet/target/mutants/tree-1",
    ]);
    assert.deepEqual(registeredTrees(""), []);
  });
});

describe("outcomeLines", () => {
  it("gives a list's lines, without empty ones and line ends", () => {
    assert.deepEqual(outcomeLines("a.rs:1:2: delete -\r\nb.rs:3:4: replace + with -\n\n"), [
      "a.rs:1:2: delete -",
      "b.rs:3:4: replace + with -",
    ]);
    assert.deepEqual(outcomeLines(""), []);
  });
});

describe("verdict", () => {
  const shard = (over) => ({
    shard: 1,
    status: 0,
    caught: 10,
    unviable: 2,
    missed: [],
    timeout: [],
    ...over,
  });

  it("passes when every shard caught every mutant", () => {
    const { code, lines } = verdict("core", [shard({}), shard({ shard: 2, caught: 5 })]);

    assert.equal(code, 0);
    assert.deepEqual(lines, [
      "core, shard 1: 10 caught, 0 missed, 0 timed out, 2 unviable",
      "core, shard 2: 5 caught, 0 missed, 0 timed out, 2 unviable",
      "core: PASS, 15 caught and none missed",
    ]);
  });

  it("passes with a mutant whose tests ran out of time, and lists it", () => {
    const { code, lines } = verdict("core", [
      shard({ status: 3, timeout: ["a.rs:1:2: replace > with < in f"] }),
    ]);

    assert.equal(code, 0);
    assert.deepEqual(lines, [
      "core, shard 1: 10 caught, 0 missed, 1 timed out, 2 unviable",
      "timed out: a.rs:1:2: replace > with < in f",
      "core: PASS, 10 caught and none missed; 1 timed out, each one a test that never ends",
    ]);
  });

  it("fails on a missed mutant, and lists every one", () => {
    const { code, lines } = verdict("auth-vault", [
      shard({ status: 2, missed: ["a.rs:1:2: delete !", "a.rs:3:4: delete -"] }),
      shard({ shard: 2 }),
    ]);

    assert.equal(code, 1);
    assert.deepEqual(lines, [
      "auth-vault, shard 1: 10 caught, 2 missed, 0 timed out, 2 unviable",
      "missed: a.rs:1:2: delete !",
      "missed: a.rs:3:4: delete -",
      "auth-vault, shard 2: 10 caught, 0 missed, 0 timed out, 2 unviable",
      "auth-vault: FAIL, 2 missed: a test for each, or its line in .cargo/mutants.toml with why",
    ]);
  });

  it("fails when a shard ended any other way, whatever its lists say", () => {
    for (const status of [1, 4, 70, null]) {
      const { code, lines } = verdict("core", [shard({ status })]);

      assert.equal(code, 1, String(status));
      assert.equal(
        lines.at(-1),
        `core: FAIL, shard 1 ended with ${status}: its log says why`,
        String(status),
      );
    }
  });

  it("fails when a shard's exit code and its lists disagree", () => {
    assert.equal(verdict("core", [shard({ status: 0, missed: ["a.rs:1:2: delete !"] })]).code, 1);
    assert.equal(verdict("core", [shard({ status: 2 })]).code, 1);
  });
});

describe("the mutants recipe", () => {
  it("hands its scope and shards to the script, and nothing else", () => {
    const justfile = readFileSync(join(root, "justfile"), "utf8");

    assert.deepEqual(recipeBody("mutants", justfile), [
      "node scripts/mutants.mjs {{ scope }} {{ shards }}",
    ]);
  });

  it("uses the profile and the test tool the config names", () => {
    const config = readFileSync(join(root, ".cargo", "mutants.toml"), "utf8");
    const cargo = readFileSync(join(root, "Cargo.toml"), "utf8");

    assert.match(config, /^test_tool = "nextest"$/m);
    assert.match(config, /^profile = "mutants"$/m);
    assert.match(cargo, /^\[profile\.mutants\]$/m);
  });
});
