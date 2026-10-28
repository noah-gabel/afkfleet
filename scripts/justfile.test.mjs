// Tests for justfile.mjs. Run with `just scripts-test`.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { readJustfile, recipeBody, recipeDependencies, recipeParameters } from "./justfile.mjs";

const TEXT = `set windows-shell := ["pwsh", "-NoLogo", "-NoProfile", "-Command"]

# A recipe with no parameters.
plain:
    echo one
    echo two

plain-two:
    echo three

[no-exit-message]
paths dir=(justfile_directory() / "target" / "dod-run" / "agent") $AFKFLEET_A__FILE=(dir / "agent.crt") $AFKFLEET_A__LOG__FILTER="info" username:
    node scripts/x.mjs "{{ dir }}"

after: plain _helper-two
    echo four
`;

describe("recipeBody", () => {
  it("gives a recipe's lines without their indentation, up to the first that isn't indented", () => {
    assert.deepEqual(recipeBody("plain", TEXT), ["echo one", "echo two"]);
  });

  it("finds a recipe by its whole name, with or without parameters or dependencies", () => {
    assert.deepEqual(recipeBody("plain-two", TEXT), ["echo three"]);
    assert.deepEqual(recipeBody("paths", TEXT), ['node scripts/x.mjs "{{ dir }}"']);
    assert.deepEqual(recipeBody("after", TEXT), ["echo four"]);
  });

  it("throws for a recipe the justfile doesn't have", () => {
    assert.throws(() => recipeBody("plai", TEXT), /the justfile has no recipe plai/);
  });
});

describe("recipeParameters", () => {
  it("gives every parameter with its default as written, and null for none", () => {
    assert.deepEqual(recipeParameters("paths", TEXT), {
      dir: '(justfile_directory() / "target" / "dod-run" / "agent")',
      $AFKFLEET_A__FILE: '(dir / "agent.crt")',
      $AFKFLEET_A__LOG__FILTER: '"info"',
      username: null,
    });
  });

  it("gives no parameters for a recipe without any", () => {
    assert.deepEqual(recipeParameters("plain", TEXT), {});
    assert.deepEqual(recipeParameters("after", TEXT), {});
  });
});

describe("recipeDependencies", () => {
  it("gives the recipes after the header's colon, in order", () => {
    assert.deepEqual(recipeDependencies("after", TEXT), ["plain", "_helper-two"]);
    assert.deepEqual(recipeDependencies("paths", TEXT), []);
  });
});

describe("readJustfile", () => {
  it("reads the repository's justfile", () => {
    assert.deepEqual(recipeBody("scripts-test", readJustfile()), [
      'node --test "scripts/*.test.mjs"',
    ]);
  });
});
