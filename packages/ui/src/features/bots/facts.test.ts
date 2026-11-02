import { describe, expect, it } from "vitest";

import { bot } from "../../test/live";
import type { Place } from "../../components/Facts";
import { botFacts, uptimeNextChange } from "./facts";

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
        ["Not running", "Account waits for approval"],
        ["Failed", "Crash loop"],
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
    expect(shown(bot(1), "card", "failed")).toContainEqual(["Mode", "Couldn't load mode"]);
    expect(shown(bot(1), "card", "loading")).toContainEqual(["Mode", "…"]);
  });
});

describe("a bot tied to an agent that can't be reached (S125)", () => {
  const AGENT = { id: "01900000-0000-7000-8000-000000000301", name: "agent-main" };
  const TIED = ["Waiting for agent", "Tied until it returns or is released."];
  const ASK_ADMIN = ["Waiting for agent", "Tied until it returns or an admin releases it."];

  /** The fact it shows at `place`, among the rest, wanted `desired`. */
  function waitingFact(
    agent: ReturnType<typeof bot>["agent"],
    place: Place,
    desired: "running" | "stopped",
  ) {
    const shownBot = bot(1, {
      desired_state: desired,
      status: { kind: "unknown", state: "online", agent_last_seen_at: "2026-10-26T12:00:00.000Z" },
      ...(agent === undefined ? {} : { agent }),
    });
    return shown(shownBot, place).filter(([term]) => term.startsWith("Waiting"));
  }

  describe.each(["card", "screen"] as const)("on a %s", (place) => {
    describe.each(["running", "stopped"] as const)("wanted %s", (desired) => {
      it("tells a Member, who isn't told the agent, that an admin releases it", () => {
        expect(waitingFact(undefined, place, desired)).toEqual([ASK_ADMIN]);
      });

      it("tells whoever sees an offline agent that it may return", () => {
        expect(waitingFact({ ...AGENT, status: "offline" }, place, desired)).toEqual([TIED]);
      });

      it("never says an expired agent returns", () => {
        expect(waitingFact({ ...AGENT, status: "expired" }, place, desired)).toEqual([
          ["Waiting for release", "Agent certificate expired."],
        ]);
      });

      it("never says a disabled agent returns", () => {
        expect(waitingFact({ ...AGENT, status: "disabled" }, place, desired)).toEqual([
          ["Waiting for release", "Agent disabled."],
        ]);
      });

      it("reads a status this version doesn't know as one that may return", () => {
        const agent = { ...AGENT, status: "resting" } as unknown as ReturnType<typeof bot>["agent"];
        expect(waitingFact(agent, place, desired)).toEqual([TIED]);
      });
    });

    it("comes first, with a paused bot's reason under its own label", () => {
      const facts = shown(
        bot(1, {
          desired_state: "running",
          status: { kind: "unknown", state: "paused", reason: "banned" },
        }),
        place,
      );

      expect(facts.filter(([term]) => term === "Reason")).toEqual([]);
      const terms = facts.map(([term]) => term);
      expect(terms.indexOf("Waiting for agent")).toBeLessThan(terms.indexOf("Paused"));
      expect(terms).toContain("Paused");
    });

    it("comes first, with a failed bot's reason under its own label", () => {
      const facts = shown(
        bot(1, {
          desired_state: "running",
          status: { kind: "unknown", state: "failed", reason: "crash_loop" },
        }),
        place,
      );

      const terms = facts.map(([term]) => term);
      expect(terms.indexOf("Waiting for agent")).toBeLessThan(terms.indexOf("Failed"));
    });

    it("never says it isn't running: an unreachable agent keeps its bots running", () => {
      const facts = shown(bot(1, { status: { kind: "unknown", state: "online" } }), place);

      expect(facts.map(([term]) => term)).not.toContain("Not running");
    });

    it.each([
      [{ kind: "reported", state: "online" }],
      [{ kind: "awaiting_report" }],
      [{ kind: "unplaced", why: "awaiting_agent" }],
    ] as const)("isn't there for %j", (status) => {
      const facts = shown(bot(1, { status: status as ReturnType<typeof bot>["status"] }), place);

      expect(facts.filter(([term]) => term.startsWith("Waiting for a"))).toEqual([]);
    });
  });
});
