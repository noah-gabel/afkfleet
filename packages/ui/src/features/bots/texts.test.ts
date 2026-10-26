import { describe, expect, it } from "vitest";

import type { BotResponse } from "../../generated/BotResponse";
import type { BotStatus } from "../../generated/BotStatus";
import type { LastDisconnect } from "../../generated/LastDisconnect";
import { bot, mode } from "../../test/live";
import {
  agentText,
  disconnectText,
  modeName,
  reasonText,
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
    expect(reasonText(withStatus({ kind: "reported", state: "paused", reason: "banned" }))).toBe(
      "Banned from this server",
    );
    expect(
      reasonText(
        withStatus({ kind: "unplaced", why: "no_server", state: "failed", reason: "auth" }),
      ),
    ).toBe("Microsoft refused its sign-in: link the account again");
    expect(
      reasonText(withStatus({ kind: "unknown", state: "paused", reason: "duplicate_login" })),
    ).toBe("Someone logged in with its account");
  });

  it("is why a bot isn't running, but never that it's wanted stopped", () => {
    expect(reasonText(withStatus({ kind: "unplaced", why: "no_server", state: "stopped" }))).toBe(
      "No server set",
    );
    expect(reasonText(withStatus({ kind: "unplaced", why: "stopped" }, "stopped"))).toBeNull();
    expect(reasonText(withStatus({ kind: "reported", state: "online" }))).toBeNull();
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
