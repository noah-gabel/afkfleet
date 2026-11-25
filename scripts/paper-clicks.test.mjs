// The summary of a person's clicks on the Paper harness (scripts/paper-clicks.mjs;
// Plan.md P16.2, ADR-0049).
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { click, summarize } from "./paper-clicks.mjs";

/** A click line as the harness logs it. */
function line(tick, nanos, kind = "LEFT") {
  return `[12:00:00 INFO]: [afkfleet-harness] harness-click player=Someone screen="Auction House" raw=53 slot=53 click=${kind} action=PICKUP_ALL hotbar=-1 cancelled=true tick=${tick} nanos=${nanos}`;
}

describe("click()", () => {
  it("reads a click's tick, time and kind", () => {
    assert.deepEqual(click(line(7, 1500)), { tick: 7, nanos: 1500, kind: "LEFT" });
  });

  it("skips every other line", () => {
    assert.equal(click("[12:00:00 INFO]: Done (5.4s)!"), undefined);
    assert.equal(
      click("[12:00:00 INFO]: [afkfleet-harness] harness-open player=x tick=1 nanos=2"),
      undefined,
    );
    assert.equal(click("harness-click tick=x nanos=2 click=LEFT"), undefined);
  });
});

describe("summarize()", () => {
  it("counts the clicks, the shortest gap, the most in a tick and in a second, and each kind", () => {
    const summary = summarize([
      line(10, 0),
      "[12:00:00 INFO]: something else",
      line(10, 30e6, "SHIFT_LEFT"),
      line(11, 80e6),
      line(30, 1_050e6),
    ]);

    assert.deepEqual(summary, {
      clicks: 4,
      shortestGapMs: 30,
      mostInATick: 2,
      mostInASecond: 3,
      kinds: { LEFT: 3, SHIFT_LEFT: 1 },
    });
  });

  it("names no player", () => {
    assert.ok(!JSON.stringify(summarize([line(1, 0), line(2, 1)])).includes("Someone"));
  });

  it("has nothing to say without clicks, and no gap for one", () => {
    assert.equal(summarize(["[12:00:00 INFO]: Done"]), undefined);
    assert.equal(summarize([line(1, 0)]).shortestGapMs, undefined);
  });
});
