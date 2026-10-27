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
  stateBadge,
  stateReasonText,
  unplacedText,
} from "./texts";

/** A bot whose status is `status`, wanted `desired`. */
function withStatus(status: BotStatus, desired: BotResponse["desired_state"] = "running") {
  return bot(1, { status, desired_state: desired });
}

describe("a bot's state badge", () => {
  it.each([
    [{ kind: "reported", state: "online" }, "Online", "default"],
    [{ kind: "reported", state: "connecting" }, "Connecting", "secondary"],
    [{ kind: "reported", state: "awaiting_session" }, "Signing in", "secondary"],
    [{ kind: "reported", state: "backoff" }, "Reconnecting", "secondary"],
    [{ kind: "reported", state: "stopping" }, "Stopping", "secondary"],
    [{ kind: "reported", state: "stopped" }, "Stopped", "outline"],
    [{ kind: "reported", state: "paused", reason: "duplicate_login" }, "Paused", "secondary"],
    [{ kind: "reported", state: "failed", reason: "banned" }, "Failed", "destructive"],
    [{ kind: "awaiting_report" }, "Starting", "secondary"],
    [{ kind: "unknown", state: "online" }, "Agent offline", "outline"],
    [{ kind: "unplaced", why: "no_server", state: "stopped" }, "Not running", "outline"],
    [{ kind: "unplaced", why: "pending", state: "paused", reason: "auth" }, "Paused", "secondary"],
    [
      { kind: "unplaced", why: "no_server", state: "failed", reason: "crash_loop" },
      "Failed",
      "destructive",
    ],
    [{ kind: "reported", state: "hibernating" }, "hibernating", "outline"],
    [{ kind: "teleported" }, "teleported", "outline"],
  ] as [BotStatus, string, string][])("%o reads %s", (status, text, variant) => {
    expect(stateBadge(withStatus(status))).toEqual({ text, variant });
  });

  it("reads Stopped for a bot that isn't placed because it's wanted stopped", () => {
    expect(stateBadge(withStatus({ kind: "unplaced", why: "stopped" }, "stopped"))).toEqual({
      text: "Stopped",
      variant: "outline",
    });
  });
});

describe("a bot's reason", () => {
  it.each([
    ["duplicate_login", "Someone logged in with its account"],
    ["banned", "Banned from this server"],
    ["not_whitelisted", "Not on this server's whitelist"],
    ["wrong_version", "This server runs another Minecraft version"],
    ["account_banned", "Its Minecraft account is banned"],
    ["multiplayer_disabled", "Multiplayer is turned off for its account"],
    ["auth", "Microsoft refused its sign-in: link the account again"],
    ["session_denied", "Its session was refused: link the account again, then Reset"],
    ["crash_loop", "It kept crashing"],
    ["a_newer_reason", "a_newer_reason"],
  ] as [BotStatus["reason"] & string, string][])(
    "paused or failed for %s reads %s",
    (reason, text) => {
      expect(stateReasonText(reason)).toBe(text);
    },
  );

  it.each([
    ["no_server", "No server set"],
    ["owner_disabled", "Its owner's login is disabled"],
    ["pending", "Its account waits for approval"],
    ["reauth_required", "Its account must be linked again"],
    ["refused", "Its account's link was refused"],
    ["offline_not_allowed", "This server doesn't allow offline accounts"],
    ["awaiting_agent", "Waiting for an agent with room"],
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
    ).toBe("Microsoft refused its sign-in: link the account again");
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
    ).toBe("Its account waits for approval");
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
      "Blocked: its agent refuses this address. Ask whoever runs the agent to allow it.",
    ],
    [
      { kind: "connect_failed", connect_failure: "a_newer_failure" },
      "Couldn't connect: a_newer_failure",
    ],
    [{ kind: "connect_failed" }, "Couldn't connect"],
    [{ kind: "auth_rejected" }, "The server refused its sign-in"],
    [{ kind: "account_restricted", restriction: "banned" }, "Account restricted: banned"],
    [
      { kind: "account_restricted", restriction: "multiplayer_disabled" },
      "Account restricted: multiplayer turned off",
    ],
    [{ kind: "account_restricted" }, "Account restricted"],
    [
      { kind: "session_server_failed", session_server_failure: "unreachable" },
      "Minecraft's session server was unreachable",
    ],
    [
      { kind: "session_server_failed", session_server_failure: "rate_limited" },
      "Minecraft's session server rate-limited it",
    ],
    [
      { kind: "session_server_failed", session_server_failure: "timed_out" },
      "Minecraft's session server timed out",
    ],
    [
      { kind: "session_server_failed", session_server_failure: "unexpected" },
      "Minecraft's session server answered unexpectedly",
    ],
    [{ kind: "session_server_failed" }, "Minecraft's session server failed"],
    [{ kind: "session_crashed" }, "Its session crashed"],
    [{ kind: "watchdog_timeout" }, "It stopped responding"],
    [{ kind: "liveness_timeout" }, "The server stopped answering"],
    [{ kind: "respawn_failed" }, "It couldn't respawn after dying"],
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

    expect(agentLastSeen(today, now)).toBe(`Its agent was last seen at ${formatClock(today)}.`);
    expect(agentLastSeen(yesterday, now)).toBe(
      `Its agent was last seen on ${formatDate(yesterday)} at ${formatClock(yesterday)}.`,
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
      "Failed: It kept crashing",
    ],
    [{ kind: "mode", arrived: 0, from: "a", to: "b" }, "Mode: mode a → mode b"],
    [{ kind: "desired", arrived: 0, desired: "running" }, "Start requested"],
    [{ kind: "desired", arrived: 0, desired: "stopped" }, "Stop requested"],
  ] as [Entry, string][])("read %o as %s", (entry, text) => {
    expect(entryText(entry, named)).toBe(text);
  });

  it("say how long live updates were down", () => {
    expect(gapText(754_000)).toBe("Live updates were down for 12 min; some events may be missing.");
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
      "AfkBot1 was paused: Banned from this server.",
      "AfkBot1 was paused: Banned from this server.",
    ],
    [
      { ...base, kind: "failed", reason: "auth" },
      "AfkBot1 failed: Microsoft refused its sign-in: link the account again.",
      "AfkBot1 failed: Microsoft refused its sign-in: link the account again.",
    ],
    [
      { ...base, kind: "blocked" },
      "AfkBot1 can't connect: its agent refuses this server's address.",
      "AfkBot1 can't connect: its agent refuses this server's address.",
    ],
    [
      { ...base, kind: "respawn" },
      "AfkBot1 couldn't respawn and left the server. It reconnects by itself.",
      "AfkBot1 couldn't respawn and left the server. It reconnects by itself.",
    ],
    [{ ...base, kind: "died", name: null }, "A bot died.", `A bot died at ${formatClock(AT)}.`],
  ] as [Notice, string, string][])("read %o as its toast and its line", (notice, toast, line) => {
    expect([noticeToast(notice), noticeLine(notice)]).toEqual([toast, line]);
  });
});
