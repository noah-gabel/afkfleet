// Tests for scripts/markdown.mjs, the docs' checks' Markdown reader (ADR-0022, S45, S49).
// Run with `just scripts-test`.
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, normalize } from "node:path";
import { describe, it } from "node:test";

import { fences, headings, inlineCode, links, sections, slug } from "./markdown.mjs";
import { ROOT } from "./threat-model-citations.mjs";

/** The guides whose links must resolve, from the repository's root. */
const GUIDES = [
  "README.md",
  "docs/runbook.md",
  "docs/using-the-app.md",
  "docs/releasing.md",
  "docs/dod/phase-12.md",
];

/**
 * The links in `file` (from the repository's root) that don't resolve: a relative link
 * to a missing file, or to a Markdown file's anchor it doesn't have.
 */
function brokenLinks(file) {
  const broken = [];
  for (const { target, line } of links(readFileSync(join(ROOT, file), "utf8"))) {
    if (/^[a-z]+:/i.test(target)) {
      continue;
    }
    const [path, anchor] = target.split("#");
    const linked = path === "" ? file : normalize(join(dirname(file), path));
    if (!existsSync(join(ROOT, linked))) {
      broken.push(`${file}:${line}: ${target}`);
    } else if (anchor !== undefined && linked.endsWith(".md")) {
      const found = headings(readFileSync(join(ROOT, linked), "utf8")).map((h) => h.slug);
      if (!found.includes(anchor)) {
        broken.push(`${file}:${line}: ${target}`);
      }
    }
  }
  return broken;
}

describe("slug", () => {
  it("makes GitHub's anchors: lower case, punctuation dropped, each space a hyphen", () => {
    assert.equal(slug("The keys' copy"), "the-keys-copy");
    assert.equal(
      slug("When a service won't start or keeps restarting"),
      "when-a-service-wont-start-or-keeps-restarting",
    );
    assert.equal(slug("Running `setup.sh` (as root)"), "running-setupsh-as-root");
    assert.equal(slug("A [linked](other.md#x) heading"), "a-linked-heading");
    assert.equal(slug("snake_case and - hyphens"), "snake_case-and---hyphens");
  });
});

describe("headings", () => {
  it("are read outside fences, a repeated anchor numbered as GitHub numbers it", () => {
    const markdown = [
      "# Title",
      "",
      "## Steps",
      "```sh",
      "# not a heading",
      "```",
      "### Steps ##",
      "####### seven is text",
      "#also text",
    ].join("\n");

    assert.deepEqual(headings(markdown), [
      { level: 1, text: "Title", slug: "title", line: 1 },
      { level: 2, text: "Steps", slug: "steps", line: 3 },
      { level: 3, text: "Steps", slug: "steps-1", line: 7 },
    ]);
  });
});

describe("fences", () => {
  it("reads each block's language, its lines without the fence's indent, and the line before it", () => {
    const markdown = [
      "Intro.",
      "",
      "> a marker",
      "```sh",
      "one",
      "```",
      "1. A step:",
      "   ```ini",
      "   KEY=value",
      "     indented",
      "   ```",
      "",
      "````text",
      "```",
      "````",
    ].join("\n");

    assert.deepEqual(fences(markdown), [
      {
        info: "sh",
        line: 4,
        closed: true,
        body: [{ line: 5, text: "one" }],
        before: { line: 3, text: "> a marker" },
      },
      {
        info: "ini",
        line: 8,
        closed: true,
        body: [
          { line: 9, text: "KEY=value" },
          { line: 10, text: "  indented" },
        ],
        before: { line: 7, text: "1. A step:" },
      },
      {
        info: "text",
        line: 13,
        closed: true,
        body: [{ line: 14, text: "```" }],
        before: { line: 11, text: "```" },
      },
    ]);
  });

  it("says when a block is never closed, and gives no line before the first", () => {
    const [block] = fences("```\nopen");

    assert.equal(block.info, "");
    assert.equal(block.closed, false);
    assert.equal(block.before, null);
    assert.deepEqual(block.body, [{ line: 2, text: "open" }]);
  });
});

describe("links and inline code", () => {
  it("are read outside fences, a link inside inline code not counting", () => {
    const markdown = [
      "See [the guide](using-the-app.md#start) and `[not](a-link.md)`, then `sh setup.sh check`.",
      "```sh",
      "[no](link.md) `no`",
      "```",
      "![an image](shot.png)",
    ].join("\n");

    assert.deepEqual(links(markdown), [
      { text: "the guide", target: "using-the-app.md#start", line: 1 },
      { text: "an image", target: "shot.png", line: 5 },
    ]);
    assert.deepEqual(inlineCode(markdown), [
      { code: "[not](a-link.md)", line: 1 },
      { code: "sh setup.sh check", line: 1 },
    ]);
  });
});

describe("sections", () => {
  it("hold each heading's lines until the next heading of its level or a higher one", () => {
    const markdown = [
      "## One",
      "a",
      "### Inner",
      "b",
      "```sh",
      "## not a heading",
      "```",
      "## Two",
      "c",
    ].join("\n");
    const found = sections(markdown);

    assert.deepEqual(found.get("one"), {
      level: 2,
      line: 1,
      text: ["a", "### Inner", "b", "```sh", "## not a heading", "```"].join("\n"),
    });
    assert.equal(found.get("inner").text, ["b", "```sh", "## not a heading", "```"].join("\n"));
    assert.equal(found.get("two").text, "c");
  });
});

describe("the guides' links", () => {
  for (const guide of GUIDES) {
    it(`${guide} links only to files and sections that exist`, () => {
      assert.deepEqual(brokenLinks(guide), []);
    });
  }
});
