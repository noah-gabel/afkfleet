// Tests for fuzz.mjs. Run with `just scripts-test`.
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import {
  ASAN_DLL,
  asanFolder,
  cargoArgs,
  DEFAULT_SECONDS,
  failingInput,
  KEEP_AT,
  MAX_SECONDS,
  parseArgs,
  TARGETS_FOLDER,
  targetNames,
  withOnPath,
} from "./fuzz.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

const KNOWN = ["agent_message", "chat_sender"];

describe("targetNames", () => {
  it("names every .rs file, sorted, and nothing else", () => {
    assert.deepEqual(targetNames(["b.rs", "notes.md", "a.rs", "c.rs.bak"]), ["a", "b"]);
  });
});

describe("parseArgs", () => {
  it("runs one target for the default time", () => {
    assert.deepEqual(parseArgs(["chat_sender"], KNOWN), {
      targets: ["chat_sender"],
      seconds: DEFAULT_SECONDS,
    });
  });

  it("runs every target for `all`, for the time given", () => {
    assert.deepEqual(parseArgs(["all", "300"], KNOWN), { targets: KNOWN, seconds: 300 });
  });

  it("takes the longest run and refuses one second more", () => {
    assert.equal(parseArgs(["all", String(MAX_SECONDS)], KNOWN).seconds, MAX_SECONDS);
    assert.match(parseArgs(["all", String(MAX_SECONDS + 1)], KNOWN).error, /whole number/);
  });

  for (const seconds of ["0", "-5", "1.5", "60s", "", "0x10", "999999"]) {
    it(`refuses the time \`${seconds}\``, () => {
      assert.match(parseArgs(["all", seconds], KNOWN).error, /whole number of seconds/);
    });
  }

  it("refuses a name that isn't a target's, and lists the targets", () => {
    const { error } = parseArgs(["chat"], KNOWN);

    assert.match(error, /no fuzz target is named `chat`/);
    assert.match(error, /agent_message, chat_sender, or all/);
  });

  it("refuses no argument and a third one", () => {
    assert.match(parseArgs([], KNOWN).error, /usage/);
    assert.match(parseArgs(["all", "60", "more"], KNOWN).error, /usage/);
  });
});

describe("cargoArgs", () => {
  it("runs the target with libFuzzer's time limit", () => {
    assert.deepEqual(cargoArgs("chat_sender", 300), [
      "fuzz",
      "run",
      "chat_sender",
      "--",
      "-max_total_time=300",
    ]);
  });
});

describe("failingInput", () => {
  it("is a file in the target's own artifacts folder", () => {
    assert.equal(
      failingInput("chat_sender"),
      join("fuzz", "artifacts", "chat_sender", "failing-input"),
    );
  });

  it("is handed over under the name fleet-fuzz reads", () => {
    const kept = readFileSync(join(root, "crates", "fleet-fuzz", "src", "kept.rs"), "utf8");

    assert.ok(kept.includes(`pub const KEEP_AT: &str = "${KEEP_AT}";`));
  });
});

describe("asanFolder", () => {
  it("is the folder of the newest toolset's runtime", () => {
    const output = [
      `C:\\VS\\VC\\Tools\\MSVC\\14.44.1\\bin\\HostX64\\x64\\${ASAN_DLL}`,
      `C:\\VS\\VC\\Tools\\MSVC\\14.50.2\\bin\\HostX64\\x64\\${ASAN_DLL}`,
      "",
    ].join("\r\n");

    assert.equal(asanFolder(output), "C:\\VS\\VC\\Tools\\MSVC\\14.50.2\\bin\\HostX64\\x64");
  });

  it("is nothing when vswhere found no runtime", () => {
    assert.equal(asanFolder(""), undefined);
    assert.equal(asanFolder("C:\\VS\\something-else.dll\r\n"), undefined);
  });
});

describe("withOnPath", () => {
  it("puts the folder first, under the variable's own name", () => {
    assert.deepEqual(withOnPath({ Path: "C:\\a;C:\\b", OTHER: "x" }, "C:\\asan", ";"), {
      Path: "C:\\asan;C:\\a;C:\\b",
      OTHER: "x",
    });
  });

  it("sets PATH when there is none", () => {
    assert.deepEqual(withOnPath({}, "/asan", ":"), { PATH: "/asan" });
  });
});

describe("the repository's targets", () => {
  const files = targetNames(readdirSync(join(root, TARGETS_FOLDER)));

  it("are the binaries fuzz/Cargo.toml declares, each at its own file", () => {
    const manifest = readFileSync(join(root, "fuzz", "Cargo.toml"), "utf8");
    const bins = [...manifest.matchAll(/\[\[bin\]\]\nname = "([a-z_]+)"\npath = "([^"]+)"/g)];

    assert.deepEqual(bins.map((bin) => bin[1]).sort(), files);
    for (const [, name, path] of bins) {
      assert.equal(path, `fuzz_targets/${name}.rs`);
    }
  });

  it("are the targets fleet-fuzz lists, each calling the body of its own name", () => {
    const lib = readFileSync(join(root, "crates", "fleet-fuzz", "src", "lib.rs"), "utf8");
    const listed = [...lib.matchAll(/^ {4}\("([a-z_]+)", ([a-z_]+)::([a-z_]+)\),$/gm)];

    assert.deepEqual(listed.map((entry) => entry[1]).sort(), files);
    for (const [, name, module, body] of listed) {
      assert.equal(body, name);
      const target = readFileSync(join(root, TARGETS_FOLDER, `${name}.rs`), "utf8");
      assert.ok(
        target.includes(`run(\n    fleet_fuzz::${module}::${name},\n    data\n)`),
        `${name}.rs runs fleet_fuzz::${module}::${name}`,
      );
      assert.ok(
        target.includes("init: keep_failing_input()"),
        `${name}.rs keeps its failing input`,
      );
    }
  });
});
