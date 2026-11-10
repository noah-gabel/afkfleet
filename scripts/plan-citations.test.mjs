// Every citation of Plan.md in a tracked file names something Plan.md still has (P12.8; ADR-0022,
// S54): a task ID after "Plan.md" (`Plan.md P6.2`, `Plan.md's P11.4 note`, a list such as
// `Plan.md P12.6, P12.7`) has its task line, `**P6.2**`; a section (`Plan.md §7.5`) its heading,
// `## 7.` or `### 7.5 …`; an appendix (`Plan.md Appendix A`) its heading, `### A.`. About 870
// code comments cite Plan.md by task ID, so a summarised phase must keep every ID findable.
// Run with `just scripts-test`.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, it } from "node:test";

import { ROOT } from "./threat-model-citations.mjs";

/** The file this test reads citations against, and its own name, whose cases cite nothing. */
const PLAN = "Plan.md";
const SELF = "scripts/plan-citations.test.mjs";

/** One cited thing: a task ID, a section or an appendix. */
const ID = String.raw`P\d+\.\d+|§\d+(?:\.\d+)?|Appendix [A-Z]\b`;

/** "Plan.md", then the run of IDs right after it, joined by commas, slashes or "and". */
const CITATION = new RegExp(
  String.raw`Plan\.md(?:['’]s)?[,:]?[ (]*((?:${ID})(?:\s*(?:,|/|and|or)\s*(?:${ID}))*)`,
  "g",
);

/** The IDs `text` cites after "Plan.md", each with its line. */
export function citations(text) {
  const found = [];
  text.split("\n").forEach((line, index) => {
    for (const [, run] of line.matchAll(CITATION)) {
      for (const [id] of run.matchAll(new RegExp(ID, "g"))) {
        found.push({ id, line: index + 1 });
      }
    }
  });
  return found;
}

/**
 * What `plan` holds: its task IDs, sections and appendices, in the forms `citations` gives. A
 * section's numbered item counts as its subsection (`§3.6` is §3's item 6, "fail small").
 */
export function targets(plan) {
  const held = new Set();
  let top = null;
  for (const line of plan.split("\n")) {
    const task = /^\s*- \[[ x]\] \*\*(P\d+\.\d+)\*\*/.exec(line);
    if (task) {
      held.add(task[1]);
    }
    const section = /^## (\d+)\. /.exec(line) ?? /^### (\d+\.\d+) /.exec(line);
    if (section) {
      held.add(`§${section[1]}`);
    }
    if (/^#{1,2} /.test(line)) {
      top = /^## (\d+)\. /.exec(line)?.[1] ?? null;
    }
    const item = /^(\d+)\. /.exec(line);
    if (item && top !== null) {
      held.add(`§${top}.${item[1]}`);
    }
    const appendix = /^### ([A-Z])\. /.exec(line);
    if (appendix) {
      held.add(`Appendix ${appendix[1]}`);
    }
  }
  return held;
}

/** Every tracked text file but Plan.md and this test, from the repository's root. */
function trackedFiles() {
  return execFileSync("git", ["ls-files", "-z"], { cwd: ROOT, encoding: "utf8" })
    .split("\0")
    .filter((file) => file !== "" && file !== PLAN && file !== SELF);
}

/** The citations in tracked files that name nothing `plan` holds, as `file:line: id`. */
function dangling(plan) {
  const held = targets(plan);
  const problems = [];
  let count = 0;
  for (const file of trackedFiles()) {
    const text = readFileSync(join(ROOT, file), "utf8");
    if (text.includes("\0")) {
      continue;
    }
    for (const { id, line } of citations(text)) {
      count += 1;
      if (!held.has(id)) {
        problems.push(`${file}:${line}: ${id}`);
      }
    }
  }
  return { count, problems };
}

/** "Plan.md", spelled so this file's own scan finds no citation in its cases. */
const P = ["Plan", "md"].join(".");

describe("citations", () => {
  it("reads task IDs, sections and appendices right after Plan.md, lists included", () => {
    const text = [
      `see ${P} P6.2; and ${P}'s P11.4 note`,
      `${P} §7.5, ${P} §5 and ${P}, Appendix A`,
      `(${P} P12.6, P12.7 and P12.8) ${P} P3.1/P3.2`,
      `P9.9 alone, or ${P} without an ID, cites nothing`,
    ].join("\n");

    assert.deepEqual(
      citations(text).map(({ id, line }) => `${line} ${id}`),
      [
        "1 P6.2",
        "1 P11.4",
        "2 §7.5",
        "2 §5",
        "2 Appendix A",
        "3 P12.6",
        "3 P12.7",
        "3 P12.8",
        "3 P3.1",
        "3 P3.2",
      ],
    );
  });
});

describe("targets", () => {
  it("are the task lines' IDs, the numbered headings, a section's items and the appendices", () => {
    const plan = [
      "## 3. Architectural principles",
      "1. **Hexagonal architecture.**",
      "   2. an indented number is no item",
      "## 7. Security model",
      "### 7.5 Secrets & cryptography",
      "- [x] **P6.2** Config",
      "  - [ ] **P13.1** 🔴 Hardening",
      "**P9.9** in prose is no task line",
      "# A heading of its own",
      "4. after it, no section's item",
      "### A. Example configuration",
    ].join("\n");

    assert.deepEqual(
      [...targets(plan)].sort(),
      ["Appendix A", "P13.1", "P6.2", "§3", "§3.1", "§7", "§7.5"].sort(),
    );
  });
});

describe("Plan.md's citations", () => {
  it("each name a task, a section or an appendix Plan.md still has", () => {
    const { count, problems } = dangling(readFileSync(join(ROOT, PLAN), "utf8"));

    assert.ok(count > 700, `only ${count} citations read`);
    assert.deepEqual(problems, []);
  });
});
