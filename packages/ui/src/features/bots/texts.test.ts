import { describe, expect, it } from "vitest";
import type { Entry, Notice } from "../../api/liveRecord";
import type { BotResponse } from "../../generated/BotResponse";
import type { BotStatus } from "../../generated/BotStatus";
import type { LastDisconnect } from "../../generated/LastDisconnect";
import { formatClock, formatClockSeconds, formatDate } from "../../lib/time";
import { bot, mode } from "../../test/live";
import {
  agentLastSeen,
  agentText,
  disconnectText,
  entryText,
  gapText,
  modeName,
  noticeLine,
  noticeToast,
  notRunningText,
  restingText,
  stateLine,
  stateReasonText,
  unplacedText,
} from "./texts";

/** A bot whose status is `status`, wanted `desired`. */
function withStatus(status: BotStatus, desired: BotResponse["desired_state"] = "running") {
  return bot(1, { status, desired_state: desired });
}

describe("a bot's state line", () => {
  it.each([
    [{ kind: "reported", state: "online" }, "Online", "success"],
    [{ kind: "reported", state: "connecting" }, "Connecting", "info"],
    [{ kind: "reported", state: "awaiting_session" }, "Signing in", "info"],
    [{ kind: "reported", state: "backoff" }, "Reconnecting", "info"],
    [{ kind: "reported", state: "stopping" }, "Stopping", "info"],
    [{ kind: "reported", state: "stopped" }, "Stopped", "neutral"],
    [{ kind: "reported", state: "paused", reason: "duplicate_login" }, "Paused", "warning"],
    [{ kind: "reported", state: "failed", reason: "banned" }, "Failed", "destructive"],
    [{ kind: "awaiting_report" }, "Starting", "info"],
    [{ kind: "unknown", state: "online" }, "Agent offline", "neutral"],
    [{ kind: "unplaced", why: "no_server", state: "stopped" }, "Not running", "neutral"],
    [{ kind: "unplaced", why: "pending", state: "paused", reason: "auth" }, "Paused", "warning"],
    [
      { kind: "unplaced", why: "no_server", state: "failed", reason: "crash_loop" },
      "Failed",
      "destructive",
    ],
    [{ kind: "reported", state: "hibernating" }, "hibernating", "neutral"],
    [{ kind: "teleported" }, "teleported", "neutral"],
  ] as [BotStatus, string, string][])("%o reads %s", (status, text, tone) => {
    expect(stateLine(withStatus(status))).toEqual({ text, tone });
  });

  it("reads Stopped for a bot that isn't placed because it's wanted stopped", () => {
    expect(stateLine(withStatus({ kind: "unplaced", why: "stopped" }, "stopped"))).toEqual({
      text: "Stopped",
      tone: "neutral",
    });
  });
});

describe("a bot's reason", () => {
  it.each([
    ["duplicate_login", "Someone logged in with its account"],
    ["banned", "Banned from this server"],
    ["not_whitelisted", "Not on this server's whitelist"],
    ["wrong_version", "Server runs another Minecraft version"],
    ["account_banned", "Minecraft account banned"],
    ["multiplayer_disabled", "Multiplayer off for its account"],
    ["auth", "Microsoft refused sign-in: link the account again"],
    ["session_denied", "Session refused: link the account again, then Reset"],
    ["crash_loop", "Crash loop"],
    ["a_newer_reason", "a_newer_reason"],
  ] as [BotStatus["reason"] & string, string][])(
    "paused or failed for %s reads %s",
    (reason, text) => {
      expect(stateReasonText(reason)).toBe(text);
    },
  );

  it.each([
    ["no_server", "No server set"],
    ["owner_disabled", "Owner's login disabled"],
    ["pending", "Account waits for approval"],
    ["reauth_required", "Account must be linked again"],
    ["refused", "Account link refused"],
    ["offline_not_allowed", "Offline accounts not allowed"],
    ["awaiting_agent", "No agent with room yet"],
    ["a_newer_why", "a_newer_why"],
  ] as [BotStatus["why"] & string, string][])("not running for %s reads %s", (why, text) => {
    expect(unplacedText(why)).toBe(text);
  });

  it("is a paused or failed state's reason, wherever the state rests", () => {
    expect(restingText({ kind: "reported", state: "paused", reason: "banned" })).toBe(
      "Banned from this server",
    );
    expect(
      restingText({ kind: "unplaced", why: "no_server", state: "failed", reason: "auth" }),
    ).toBe("Microsoft refused sign-in: link the account again");
    expect(restingText({ kind: "unknown", state: "paused", reason: "duplicate_login" })).toBe(
      "Someone logged in with its account",
    );
    expect(restingText({ kind: "reported", state: "online" })).toBeNull();
  });

  it("is why a bot isn't running, but never that it's wanted stopped", () => {
    expect(notRunningText({ kind: "unplaced", why: "no_server", state: "stopped" })).toBe(
      "No server set",
    );
    expect(
      notRunningText({ kind: "unplaced", why: "pending", state: "failed", reason: "auth" }),
    ).toBe("Account waits for approval");
    expect(notRunningText({ kind: "unplaced", why: "stopped" })).toBeNull();
    expect(notRunningText({ kind: "reported", state: "online" })).toBeNull();
  });
});

describe("a bot's last disconnect", () => {
  it.each([
    [
      { kind: "kicked", message: "Server closed | Come back later" },
      "Kicked: Server closed | Come back later",
    ],
    [
      { kind: "kicked", key: "multiplayer.disconnect.idling" },
      "Kicked (multiplayer.disconnect.idling)",
    ],
    [{ kind: "kicked" }, "Kicked"],
    [{ kind: "connection_closed" }, "Connection closed"],
    [
      { kind: "connect_failed", connect_failure: "refused" },
      "Couldn't connect: connection refused",
    ],
    [{ kind: "connect_failed", connect_failure: "timed_out" }, "Couldn't connect: timed out"],
    [
      { kind: "connect_failed", connect_failure: "unresolvable" },
      "Couldn't connect: address not found",
    ],
    [
      { kind: "connect_failed", connect_failure: "host_unavailable" },
      "Couldn't connect: host unreachable",
    ],
    [{ kind: "connect_failed", connect_failure: "other" }, "Couldn't connect: network error"],
    [
      { kind: "connect_failed", connect_failure: "blocked" },
      "Blocked by its agent. Ask whoever runs it to allow this address.",
    ],
    [
      { kind: "connect_failed", connect_failure: "a_newer_failure" },
      "Couldn't connect: a_newer_failure",
    ],
    [{ kind: "connect_failed" }, "Couldn't connect"],
    [{ kind: "auth_rejected" }, "Server refused sign-in"],
    [{ kind: "account_restricted", restriction: "banned" }, "Account restricted: banned"],
    [
      { kind: "account_restricted", restriction: "multiplayer_disabled" },
      "Account restricted: multiplayer turned off",
    ],
    [{ kind: "account_restricted" }, "Account restricted"],
    [
      { kind: "session_server_failed", session_server_failure: "unreachable" },
      "Minecraft session server unreachable",
    ],
    [
      { kind: "session_server_failed", session_server_failure: "rate_limited" },
      "Minecraft session server rate-limited",
    ],
    [
      { kind: "session_server_failed", session_server_failure: "timed_out" },
      "Minecraft session server timed out",
    ],
    [
      { kind: "session_server_failed", session_server_failure: "unexpected" },
      "Minecraft session server unexpected answer",
    ],
    [{ kind: "session_server_failed" }, "Minecraft session server failed"],
    [{ kind: "session_crashed" }, "Session crashed"],
    [{ kind: "watchdog_timeout" }, "Stopped responding"],
    [{ kind: "liveness_timeout" }, "Server stopped answering"],
    [{ kind: "respawn_failed" }, "Couldn't respawn"],
    [{ kind: "a_newer_kind" }, "a_newer_kind"],
  ] as [LastDisconnect, string][])("%o reads %s", (last, text) => {
    expect(disconnectText(last)).toBe(text);
  });
});

describe("the other texts", () => {
  it("say when an agent was last seen: at a time today, on a date before today", () => {
    const now = new Date(2026, 9, 26, 14, 5).getTime();
    const today = new Date(2026, 9, 26, 0, 30).toISOString();
    const yesterday = new Date(2026, 9, 25, 23, 30).toISOString();

    expect(agentLastSeen(today, now)).toBe(`Agent last seen at ${formatClock(today)}.`);
    expect(agentLastSeen(yesterday, now)).toBe(
      `Agent last seen on ${formatDate(yesterday)} at ${formatClock(yesterday)}.`,
    );
  });

  it("name a built-in mode as such", () => {
    expect(modeName(mode(1, { name: "afk", ownership: "built_in" }))).toBe("afk (built in)");
    expect(modeName(mode(2, { name: "My loop", ownership: "mine" }))).toBe("My loop");
  });

  it.each([
    ["online", "relay-1 · online"],
    ["offline", "relay-1 · offline"],
    ["expired", "relay-1 · certificate expired"],
    ["disabled", "relay-1 · disabled"],
    ["a_newer_status", "relay-1 · a_newer_status"],
  ] as const)("say an agent that's %s", (status, text) => {
    expect(
      agentText({
        id: "01900000-0000-7000-8000-000000000501",
        name: "relay-1",
        status: status as "online",
      }),
    ).toBe(text);
  });
});

describe("the timeline's texts (U46, R111)", () => {
  const AT = "2026-10-26T14:00:05.000Z";
  const named = (id: string) => `mode ${id}`;

  it.each([
    [
      {
        kind: "status",
        arrived: 0,
        desired: "running",
        newDisconnect: false,
        status: { kind: "reported", state: "online" },
      },
      "Online",
    ],
    [
      {
        kind: "status",
        arrived: 0,
        desired: "running",
        newDisconnect: true,
        status: {
          kind: "reported",
          state: "backoff",
          last_disconnect: { kind: "kicked", message: "Testing" },
        },
      },
      "Reconnecting · Kicked: Testing",
    ],
    [
      {
        kind: "status",
        arrived: 0,
        desired: "running",
        newDisconnect: false,
        status: { kind: "unplaced", why: "no_server", state: "paused", reason: "banned" },
      },
      "Paused · No server set · Banned from this server",
    ],
    [{ kind: "died", arrived: 0, at: AT, count: 1 }, "Died"],
    [{ kind: "died", arrived: 0, at: AT, count: 3 }, `Died 3×, last at ${formatClockSeconds(AT)}`],
    [
      { kind: "alert", arrived: 0, at: AT, state: "paused", reason: "banned" },
      "Paused: Banned from this server",
    ],
    [
      { kind: "alert", arrived: 0, at: AT, state: "failed", reason: "crash_loop" },
      "Failed: Crash loop",
    ],
    [{ kind: "mode", arrived: 0, from: "a", to: "b" }, "Mode: mode a to mode b"],
    [{ kind: "desired", arrived: 0, desired: "running" }, "Start requested"],
    [{ kind: "desired", arrived: 0, desired: "stopped" }, "Stop requested"],
  ] as [Entry, string][])("read %o as %s", (entry, text) => {
    expect(entryText(entry, named)).toBe(text);
  });

  it("say how long live updates were down", () => {
    expect(gapText(754_000)).toBe("Live updates down for 12 min. Events may be missing.");
  });
});

describe("the notices' texts (U42)", () => {
  const AT = "2026-10-26T14:00:05.000Z";
  const base = { id: 1, botId: "b", name: "AfkBot1", count: 1, at: AT };

  it.each([
    [{ ...base, kind: "died" }, "AfkBot1 died.", `AfkBot1 died at ${formatClock(AT)}.`],
    [
      { ...base, kind: "died", count: 3 },
      "AfkBot1 died.",
      `AfkBot1 died 3 times, last at ${formatClock(AT)}.`,
    ],
    [
      { ...base, kind: "paused", reason: "banned" },
      "AfkBot1 paused: banned from this server.",
      "AfkBot1 paused: banned from this server.",
    ],
    [
      { ...base, kind: "failed", reason: "auth" },
      "AfkBot1 failed: Microsoft refused sign-in: link the account again.",
      "AfkBot1 failed: Microsoft refused sign-in: link the account again.",
    ],
    [
      { ...base, kind: "blocked" },
      "AfkBot1 blocked: its agent refuses this address.",
      "AfkBot1 blocked: its agent refuses this address.",
    ],
    [
      { ...base, kind: "respawn" },
      "AfkBot1 couldn't respawn and left. Reconnects by itself.",
      "AfkBot1 couldn't respawn and left. Reconnects by itself.",
    ],
    [{ ...base, kind: "died", name: null }, "A bot died.", `A bot died at ${formatClock(AT)}.`],
  ] as [Notice, string, string][])("read %o as its toast and its line", (notice, toast, line) => {
    expect([noticeToast(notice), noticeLine(notice)]).toEqual([toast, line]);
  });
});

describe("the timeline stays short for a bot its agent can't reach (S125)", () => {
  it("names the state, not the wait", () => {
    const text = entryText(
      {
        kind: "status",
        arrived: 0,
        status: { kind: "unknown", state: "online" },
        desired: "running",
        newDisconnect: false,
      } as Parameters<typeof entryText>[0],
      (id) => id,
    );

    expect(text).toBe("Agent offline");
    expect(text).not.toContain("Tied");
  });
});

describe("a reason inside a notice's sentence (R213)", () => {
  const failed = (reason: Parameters<typeof stateReasonText>[0]) =>
    noticeLine({ id: 1, botId: "b", name: "AfkBot5", kind: "failed", reason } as Notice);

  it("starts in lowercase, as the rest of the sentence reads", () => {
    expect(failed("crash_loop")).toBe("AfkBot5 failed: crash loop.");
  });

  it("keeps a name's capital", () => {
    expect(failed("auth")).toBe(
      "AfkBot5 failed: Microsoft refused sign-in: link the account again.",
    );
  });

  it("keeps the capital of a reason shown alone", () => {
    expect(stateReasonText("crash_loop")).toBe("Crash loop");
  });
});
