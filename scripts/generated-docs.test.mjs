// The generated TypeScript holds no rustdoc link (ADR-0020, S128, R254). ts-rs and utoipa copy
// a DTO's doc text as it is, so a `[`Name`]` link, which rustdoc turns into a link, reaches
// packages/ui/src/generated/ and the OpenAPI document as brackets. A DTO's docs name other
// items in backticks alone. Run with `just scripts-test`.
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

const GENERATED = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "packages",
  "ui",
  "src",
  "generated",
);

describe("the generated TypeScript", () => {
  it("holds no rustdoc link: a DTO's docs name items in backticks alone", () => {
    const files = readdirSync(GENERATED).filter((name) => name.endsWith(".ts"));
    assert.ok(files.length > 100, `${files.length} files`);

    const linked = files.flatMap((name) =>
      readFileSync(path.join(GENERATED, name), "utf8")
        .split("\n")
        .filter((line) => line.includes("[`"))
        .map((line) => `${name}: ${line.trim()}`),
    );

    assert.deepEqual(linked, []);
  });
});
