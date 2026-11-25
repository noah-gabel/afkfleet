// The Paper harness's Dockerfile pins every input and fetches nothing after its
// checked downloads (scripts/harness-pins.mjs; Plan.md P16.1, ADR-0049).
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

import { DOCKERFILE, instructions, problems } from "./harness-pins.mjs";

const DIGEST = "a".repeat(64);
const SUM = "b".repeat(64);

/** A Dockerfile that holds every rule, with `extra` instructions at its end. */
function pinned(...extra) {
  return [
    `FROM image@sha256:${DIGEST} AS jdk`,
    "FROM jdk AS server",
    `ADD --checksum=sha256:${SUM} https://example.com/a.jar a.jar`,
    "RUN --network=none java -jar a.jar",
    "FROM jdk",
    "COPY --from=server /a /a",
    ...extra,
  ].join("\n");
}

describe("the harness's Dockerfile", () => {
  it("pins every input and fetches nothing after its downloads", () => {
    assert.deepEqual(problems(readFileSync(DOCKERFILE, "utf8")), []);
  });

  it("downloads Paper's jar and Mojang's by their SHA-256, and builds the plugin offline", () => {
    const text = readFileSync(DOCKERFILE, "utf8");
    const adds = instructions(text).filter((line) => line.startsWith("ADD "));
    assert.equal(adds.length, 2);
    assert.match(adds[0], /fill-data\.papermc\.io\/.*paper-26\.1\.2-74\.jar/);
    assert.match(adds[1], /piston-data\.mojang\.com\/.*server\.jar/);
    const javac = instructions(text).find((line) => line.includes("javac"));
    assert.match(javac, /^RUN --network=none /);
  });
});

describe("problems()", () => {
  it("finds none in a Dockerfile that holds every rule", () => {
    assert.deepEqual(problems(pinned()), []);
  });

  it("reads continued lines as one instruction and skips comments", () => {
    assert.deepEqual(instructions("# a comment\nRUN a \\\n    b\n\nADD c d"), [
      "RUN a b",
      "ADD c d",
    ]);
  });

  for (const [what, extra, expected] of [
    [
      "an image by tag",
      ["FROM eclipse-temurin:25-jdk"],
      "FROM eclipse-temurin:25-jdk isn't pinned by digest",
    ],
    [
      "a download without a checksum",
      ["ADD https://example.com/b.jar b.jar"],
      "ADD https://example.com/b.jar has no --checksum=sha256:<64 hex digits>",
    ],
    [
      "a checksum too short",
      [`ADD --checksum=sha256:${"c".repeat(63)} https://example.com/b.jar b.jar`],
      "ADD https://example.com/b.jar has no --checksum=sha256:<64 hex digits>",
    ],
    [
      "plain http",
      [`ADD --checksum=sha256:${SUM} http://example.com/b.jar b.jar`],
      "ADD http://example.com/b.jar isn't https",
    ],
    [
      "a RUN with network after the downloads",
      ["RUN java -version"],
      "RUN after a download without --network=none: RUN java -version",
    ],
    [
      "curl",
      ["RUN --network=none curl -o x https://example.com/x"],
      "RUN fetches with a tool of its own: RUN --network=none curl -o x https://example.com/x",
    ],
    [
      "a build tool",
      ["RUN --network=none gradle build"],
      "RUN fetches with a tool of its own: RUN --network=none gradle build",
    ],
    [
      "a copy from an image",
      ["COPY --from=busybox:latest /bin/sh /sh"],
      "COPY from an image instead of a stage: COPY --from=busybox:latest /bin/sh /sh",
    ],
  ]) {
    it(`refuses ${what}`, () => {
      assert.deepEqual(problems(pinned(...extra)), [expected]);
    });
  }

  it("refuses a Dockerfile without a checked download", () => {
    assert.deepEqual(problems(`FROM image@sha256:${DIGEST}\nRUN true`), [
      "no checked download: Paper's jar comes in by ADD --checksum",
    ]);
  });
});
