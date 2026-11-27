import { describe, expect, it } from "vitest";

import { ranges } from "./sight";

describe("the empty slots' ranges (P16.20)", () => {
  it.each([
    [[], ""],
    [[4], "4"],
    [[0, 1, 2, 3, 5, 6, 7, 8], "0–3, 5–8"],
    [[5, 10, 11, 44], "5, 10–11, 44"],
    [[1, 3, 5], "1, 3, 5"],
  ])("writes %j as %s", (numbers, text) => {
    expect(ranges(numbers)).toBe(text);
  });
});
