// The warning before an agent's certificate ends (the user's decision at PR #92's review): within
// 14 calendar days, in the user's words, and none once it has ended, which the server calls
// Expired.
import { describe, expect, it } from "vitest";

import { certificateWarning } from "./agentTexts";

/** Local noon on 2026-11-03, and a time `days` calendar days on at `hour` local. */
const NOW = new Date(2026, 10, 3, 12, 0).getTime();
const on = (days: number, hour: number) => new Date(2026, 10, 3 + days, hour, 0).toISOString();

describe("certificateWarning", () => {
  it.each([
    [15, 8, undefined],
    [14, 8, "Expires in 14 days. Enroll a new agent before then."],
    [2, 23, "Expires in 2 days. Enroll a new agent before then."],
    [1, 0, "Expires tomorrow. Enroll a new agent before then."],
    [0, 20, "Expires today. Enroll a new agent before then."],
    [0, 8, undefined],
    [-3, 12, undefined],
  ])("%i days on at %i h: %s", (days, hour, said) => {
    expect(certificateWarning(on(days, hour), NOW)).toBe(said);
  });
});
