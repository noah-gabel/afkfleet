import { describe, expect, it } from "vitest";

import { CHAT_RECALL_MAX, NOT_BROWSING, newer, older, remembered } from "./recall";

describe("the lines a bot's chat remembers (the user's addition to P15.26)", () => {
  it("keeps a sent line last, the same line twice in a row once", () => {
    const once = remembered(remembered([], "/spawn"), "hi");

    expect(remembered(once, "hi")).toEqual(["/spawn", "hi"]);
    expect(remembered(remembered(once, "/spawn"), "hi")).toEqual(["/spawn", "hi", "/spawn", "hi"]);
  });

  it(`keeps the newest ${CHAT_RECALL_MAX} lines`, () => {
    let lines: readonly string[] = [];
    for (let n = 1; n <= CHAT_RECALL_MAX + 3; n += 1) {
      lines = remembered(lines, `line ${n}`);
    }

    expect(lines).toHaveLength(CHAT_RECALL_MAX);
    expect(lines[0]).toBe("line 4");
    expect(lines.at(-1)).toBe(`line ${CHAT_RECALL_MAX + 3}`);
  });
});

describe("browsing them", () => {
  const lines = ["first", "second", "third"];

  it("goes back from the newest, keeping what was typed, and stops at the oldest", () => {
    const one = older(lines, NOT_BROWSING, "typing");
    const two = one === null ? null : older(lines, one.browse, "third");
    const three = two === null ? null : older(lines, two.browse, "second");
    const past = three === null ? null : older(lines, three.browse, "first");

    expect([one?.text, two?.text, three?.text]).toEqual(["third", "second", "first"]);
    expect(one?.browse).toEqual({ at: 0, draft: "typing" });
    expect(past).toBeNull();
  });

  it("goes forward again, and past the newest shows what was typed", () => {
    const back = older(lines, { at: 1, draft: "typing" }, "second");

    expect(back?.text).toBe("first");
    expect(newer(lines, { at: 2, draft: "typing" })).toEqual({
      browse: { at: 1, draft: "typing" },
      text: "second",
    });
    expect(newer(lines, { at: 0, draft: "typing" })).toEqual({
      browse: NOT_BROWSING,
      text: "typing",
    });
  });

  it("does nothing without lines, or going forward when it isn't browsing", () => {
    expect(older([], NOT_BROWSING, "typing")).toBeNull();
    expect(newer(lines, NOT_BROWSING)).toBeNull();
  });
});
