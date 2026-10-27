import { describe, expect, it } from "vitest";

import { bot } from "../../test/live";
import { botFacts, type Place, uptimeNextChange } from "./facts";

const NOW = Date.parse("2026-10-26T14:05:00.000Z");

/** A bot online since `ms` before `NOW` (after it, for a negative `ms`). */
function onlineFor(ms: number) {
  const since = new Date(NOW - ms).toISOString();
  return bot(1, {
    desired_state: "running",
    status: { kind: "reported", state: "online", since, online_since: since, attempt: 1 },
  });
}

describe("when a bot's uptime changes next (R100)", () => {
  it.each([
    [0, 1000],
    [58_250, 750],
    [59_999, 1],
    [60_000, 60_000],
    [7_530_000, 30_000],
    [7_559_999, 1],
  ])("online for %i ms: in %i ms", (online, next) => {
    expect(uptimeNextChange(onlineFor(online), NOW)).toBe(next);
  });

  it("waits until a clock ahead of the PC's reaches the first second, at most a minute", () => {
    expect(uptimeNextChange(onlineFor(-4000), NOW)).toBe(5000);
    expect(uptimeNextChange(onlineFor(-300_000), NOW)).toBe(60_000);
  });

  it("waits a minute for a bot that isn't online", () => {
    expect(uptimeNextChange(bot(1), NOW)).toBe(60_000);
  });
});

/** The facts `bot` shows at `place` with `modes`, as label and value pairs. */
function shown(
  shownBot: ReturnType<typeof bot>,
  place: Place,
  modes: Parameters<typeof botFacts>[1] = [],
): [string, string][] {
  return botFacts(shownBot, modes, NOW, place).map((one) => [one.term, one.value]);
}

describe("a bot's facts", () => {
  it.each(["card", "screen"] as const)(
    "on a %s, give both reasons of a bot not placed and resting, the not-running one first",
    (place) => {
      const paused = bot(1, {
        desired_state: "running",
        status: { kind: "unplaced", why: "no_server", state: "paused", reason: "banned" },
      });
      const failed = bot(1, {
        desired_state: "running",
        status: { kind: "unplaced", why: "pending", state: "failed", reason: "crash_loop" },
      });

      expect(shown(paused, place).filter(([term]) => term !== "Mode")).toEqual([
        ["Not running", "No server set"],
        ["Paused", "Banned from this server"],
      ]);
      expect(shown(failed, place).filter(([term]) => term !== "Mode")).toEqual([
        ["Not running", "Its account waits for approval"],
        ["Failed", "It kept crashing"],
      ]);
    },
  );

  it("give one reason under Reason", () => {
    const resting = bot(1, {
      desired_state: "stopped",
      status: { kind: "unplaced", why: "stopped", state: "paused", reason: "banned" },
    });

    expect(shown(resting, "card")).toContainEqual(["Reason", "Banned from this server"]);
  });

  it("say the mode couldn't be loaded when the modes can't be read", () => {
    expect(shown(bot(1), "card", "failed")).toContainEqual(["Mode", "Couldn't load the mode"]);
    expect(shown(bot(1), "card", "loading")).toContainEqual(["Mode", "…"]);
  });
});
