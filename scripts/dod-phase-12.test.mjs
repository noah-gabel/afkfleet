// Tests for docs/dod/phase-12.md, the user's checklist for Phase 12's definition of done
// (Plan.md, Phase 12's DoD; ADR-0022, U6, S49): a clean install by the runbook alone, then
// a friend's login by the friends' guide alone. The user alone runs it, on their own host
// and PCs; these tests only read it. Run with `just scripts-test`.
//
// Its shape: part (a) links only to the runbook's sections and part (b) only to the
// friends' guide's, each to a section that exists (a runbook section that says what
// success is); every step has one Do, one Success and one empty Outcome; nothing is
// ticked; no fenced block, no command in inline code, no address, domain, email or home
// folder; and the privacy line is there.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, it } from "node:test";

import { fences, headings, inlineCode, links, prose, sections } from "./markdown.mjs";
import { ROOT } from "./threat-model-citations.mjs";

/** The kit, from the repository's root. */
const KIT = "docs/dod/phase-12.md";

/** The line that says what never goes into the kit or a report. */
export const PRIVACY =
  "**Never write here, or report, the host's name, address, domain or provider, `.env`'s values, a fingerprint, an invite, a username or a password (CLAUDE.md rule 11).**";

/** A step's empty outcome. */
const OUTCOME = "- **Outcome:** ☐ pass ☐ fail — notes: ____";

/** The kit's two parts: their headings' anchors and the guide each follows alone. */
const PARTS = [
  { anchor: "a-a-clean-install-by-the-runbook-alone", guide: "../runbook.md" },
  { anchor: "b-a-friends-login-by-the-friends-guide-alone", guide: "../using-the-app.md" },
];

/** Inline code that is a command. */
const COMMAND = /^(?:docker|sudo|sh|curl|ssh|scp) /;

/** What private data looks like: an address, a domain, an email or a home folder. */
const PRIVATE = [
  /\b\d{1,3}(?:\.\d{1,3}){3}\b/g,
  /\b[a-z0-9-]+(?:\.[a-z0-9-]+)*\.(?:com|net|org|io|dev|app|de|uk|eu|info|me|xyz|cloud|co|us)\b/gi,
  /\S+@\S+/g,
  /[A-Za-z]:\\Users\\\S*|\/home\/\S+/g,
];

/**
 * The numbered steps in a part's `text` (its section), each as the lines from its number
 * to the next: `{ number, lines }`.
 */
function steps(text) {
  const found = [];
  for (const line of text.split("\n")) {
    const start = /^(\d+)\. /.exec(line);
    if (start !== null) {
      found.push({ number: Number(start[1]), lines: [line] });
    } else if (found.length > 0) {
      found.at(-1).lines.push(line);
    }
  }
  return found;
}

/**
 * What's wrong with the kit `markdown`, given the `guides` it links to (their texts, by
 * the kit's relative link: `../runbook.md`, `../using-the-app.md`).
 */
export function kitProblems(markdown, guides) {
  const problems = [];
  if (!markdown.split("\n").some((line) => line.trim() === PRIVACY)) {
    problems.push("the privacy line is missing");
  }

  const found = sections(markdown);
  const headingLines = new Map(headings(markdown).map((heading) => [heading.slug, heading]));
  const allowed = new Map();
  for (const { anchor, guide } of PARTS) {
    const part = found.get(anchor);
    if (part === undefined) {
      problems.push(`there's no part ${anchor}`);
      continue;
    }
    const partSteps = steps(part.text);
    if (partSteps.length === 0) {
      problems.push(`${anchor}: no step`);
    }
    for (const step of partSteps) {
      const at = `${anchor}, step ${step.number}`;
      const trimmed = step.lines.map((line) => line.trim());
      const count = (pattern) => trimmed.filter((line) => pattern.test(line)).length;
      const dos = count(/^- \*\*Do:\*\* \S/);
      const successes = count(/^- \*\*Success:\*\* \S/);
      const outcomes = trimmed.filter((line) => line === OUTCOME).length;
      if (dos !== 1) {
        problems.push(`${at}: ${dos} Do lines, not one`);
      }
      if (successes !== 1) {
        problems.push(`${at}: ${successes} Success lines, not one`);
      }
      if (outcomes !== 1) {
        problems.push(`${at}: ${outcomes} empty Outcome lines, not one`);
      }
      const doLine = trimmed.find((line) => line.startsWith("- **Do:** ")) ?? "";
      if (dos === 1 && links(doLine).length === 0) {
        problems.push(`${at}: its Do links no section`);
      }
    }
    const first = headingLines.get(anchor).line;
    const last = first + part.text.split("\n").length;
    for (let line = first + 1; line <= last; line += 1) {
      allowed.set(line, guide);
    }
  }

  for (const { target, line } of links(markdown)) {
    const guide = allowed.get(line);
    if (guide === undefined) {
      problems.push(`line ${line}: a link outside the parts: ${target}`);
      continue;
    }
    const [path, anchor] = target.split("#");
    if (path !== guide || anchor === undefined) {
      problems.push(`line ${line}: ${target} isn't a section of ${guide}`);
      continue;
    }
    const linked = sections(guides[guide] ?? "").get(anchor);
    if (linked === undefined) {
      problems.push(`line ${line}: ${guide} has no section #${anchor}`);
    } else if (guide === "../runbook.md" && !linked.text.includes("Success")) {
      problems.push(`line ${line}: ${target} doesn't say what success is`);
    }
  }

  for (const { line, text } of prose(markdown)) {
    if (/☑|☒|\[[xX]\]/.test(text)) {
      problems.push(`line ${line}: a ticked box`);
    }
    for (const pattern of PRIVATE) {
      for (const [token] of text.matchAll(pattern)) {
        problems.push(`line ${line}: an address, a domain, an email or a home folder: ${token}`);
      }
    }
  }
  for (const block of fences(markdown)) {
    problems.push(`line ${block.line}: a fenced block`);
  }
  for (const { code, line } of inlineCode(markdown)) {
    if (COMMAND.test(code)) {
      problems.push(`line ${line}: a command in inline code: ${code}`);
    }
  }
  return problems;
}

/** A guide's text, as the kit links it. */
function guide(name) {
  return readFileSync(join(ROOT, "docs", name.replace("../", "")), "utf8");
}

/** The fixtures' guides: a runbook section with a Success, one without, and an app's. */
const GUIDES = {
  "../runbook.md": "## Installing\nSuccess: it runs.\n## Notes\nNothing to check.\n",
  "../using-the-app.md": "## Logging in\nHome opens.\n",
};

/** A step of `guide`'s section `anchor`. */
const step = (number, target) =>
  [
    `${number}. **A step.**`,
    `   - **Do:** [It](${target}).`,
    "   - **Success:** it works.",
    `   ${OUTCOME}`,
  ].join("\n");

/** A kit with `a` and `b` as its parts' steps, and `extra` lines after the header. */
const kit = ({
  a = step(1, "../runbook.md#installing"),
  b = step(1, "../using-the-app.md#logging-in"),
  extra = "",
} = {}) =>
  [
    "# Phase 12's definition of done",
    "",
    PRIVACY,
    extra,
    "## (a) A clean install by the runbook alone",
    "",
    a,
    "",
    "## (b) A friend's login by the friends' guide alone",
    "",
    b,
    "",
  ].join("\n");

describe("kitProblems", () => {
  it("passes a kit of the right shape", () => {
    assert.deepEqual(kitProblems(kit(), GUIDES), []);
  });

  it("names a missing privacy line, and a missing part", () => {
    const markdown = kit().replace(PRIVACY, "").replace("## (b)", "## (c)");

    assert.deepEqual(kitProblems(markdown, GUIDES), [
      "the privacy line is missing",
      "there's no part b-a-friends-login-by-the-friends-guide-alone",
      "line 15: a link outside the parts: ../using-the-app.md#logging-in",
    ]);
  });

  it("names a step without a Do, with two Successes, or with its Outcome filled in", () => {
    const a = [
      "1. **A step.**",
      "   - **Success:** it works.",
      "   - **Success:** it works again.",
      "   - **Outcome:** ☐ pass ☐ fail — notes: it worked",
    ].join("\n");

    assert.deepEqual(kitProblems(kit({ a }), GUIDES), [
      "a-a-clean-install-by-the-runbook-alone, step 1: 0 Do lines, not one",
      "a-a-clean-install-by-the-runbook-alone, step 1: 2 Success lines, not one",
      "a-a-clean-install-by-the-runbook-alone, step 1: 0 empty Outcome lines, not one",
    ]);
  });

  it("names a link to the other guide, a missing section, or a runbook section without a Success", () => {
    const a = [
      step(1, "../using-the-app.md#logging-in"),
      step(2, "../runbook.md#gone"),
      step(3, "../runbook.md#notes"),
    ].join("\n");

    assert.deepEqual(kitProblems(kit({ a }), GUIDES), [
      "line 8: ../using-the-app.md#logging-in isn't a section of ../runbook.md",
      "line 12: ../runbook.md has no section #gone",
      "line 16: ../runbook.md#notes doesn't say what success is",
    ]);
  });

  it("names a ticked box, a fenced block, a command and private data", () => {
    const extra = [
      "- ☑ done, [x] done",
      "Run `sh setup.sh check` on 203.0.113.9, fleet.example.com or for ops@example.com.",
      "```text",
      "ok",
      "```",
    ].join("\n");

    assert.deepEqual(kitProblems(kit({ extra }), GUIDES), [
      "line 4: a ticked box",
      "line 5: an address, a domain, an email or a home folder: 203.0.113.9",
      "line 5: an address, a domain, an email or a home folder: fleet.example.com",
      "line 5: an address, a domain, an email or a home folder: example.com",
      "line 5: an address, a domain, an email or a home folder: ops@example.com.",
      "line 6: a fenced block",
      "line 5: a command in inline code: sh setup.sh check",
    ]);
  });
});

describe("docs/dod/phase-12.md", () => {
  it("has the kit's shape, linking the two guides alone", () => {
    const markdown = readFileSync(join(ROOT, KIT), "utf8");
    const guides = {
      "../runbook.md": guide("../runbook.md"),
      "../using-the-app.md": guide("../using-the-app.md"),
    };

    assert.deepEqual(kitProblems(markdown, guides), []);
  });
});
