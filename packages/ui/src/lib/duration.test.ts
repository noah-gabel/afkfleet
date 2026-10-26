import { describe, expect, it } from "vitest";

import { formatDuration } from "./duration";

const SECOND = 1000;
const MINUTE = 60 * SECOND;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

describe("a duration", () => {
  it.each([
    [0, "0 s"],
    [45 * SECOND + 900, "45 s"],
    [MINUTE, "1 min"],
    [12 * MINUTE + 59 * SECOND, "12 min"],
    [HOUR, "1 h"],
    [3 * HOUR + 12 * MINUTE, "3 h 12 min"],
    [DAY, "1 d"],
    [2 * DAY + 4 * HOUR + 59 * MINUTE, "2 d 4 h"],
    [-5 * MINUTE, "0 s"],
  ])("of %i ms reads %s", (ms, text) => {
    expect(formatDuration(ms)).toBe(text);
  });
});
