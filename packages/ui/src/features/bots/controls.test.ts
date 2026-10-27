import { describe, expect, it } from "vitest";

import type { BotResponse } from "../../generated/BotResponse";
import type { BotStatus } from "../../generated/BotStatus";
import { bot } from "../../test/live";
import { controlsFor } from "./controls";

/** A bot whose status is `status`, wanted `desired`. */
function withStatus(status: BotStatus, desired: BotResponse["desired_state"]) {
  return bot(1, { status, desired_state: desired });
}

describe("which controls a bot's state offers (U38, R94, R103)", () => {
  it.each([
    [{ kind: "reported", state: "online" }, "running", ["stop", "restart"]],
    [{ kind: "reported", state: "connecting" }, "running", ["stop", "restart"]],
    [{ kind: "reported", state: "awaiting_session" }, "running", ["stop", "restart"]],
    [{ kind: "reported", state: "backoff" }, "running", ["stop", "restart"]],
    [{ kind: "reported", state: "stopping" }, "running", ["stop"]],
    [{ kind: "reported", state: "stopped" }, "running", ["stop"]],
    [{ kind: "reported", state: "stopping" }, "stopped", ["start"]],
    [{ kind: "reported", state: "stopped" }, "stopped", ["start"]],
    [{ kind: "reported", state: "paused", reason: "banned" }, "running", ["resume", "stop"]],
    [{ kind: "reported", state: "failed", reason: "auth" }, "running", ["reset", "stop"]],
    [
      { kind: "unplaced", why: "stopped", state: "paused", reason: "banned" },
      "stopped",
      ["resume"],
    ],
    [{ kind: "unplaced", why: "stopped", state: "failed", reason: "auth" }, "stopped", ["reset"]],
    [
      { kind: "unplaced", why: "no_server", state: "paused", reason: "banned" },
      "running",
      ["resume", "stop"],
    ],
    [{ kind: "unplaced", why: "no_server" }, "running", ["stop"]],
    [{ kind: "unplaced", why: "stopped" }, "stopped", ["start"]],
    [{ kind: "awaiting_report" }, "running", ["stop"]],
    [{ kind: "unknown", state: "online" }, "running", ["stop"]],
    [{ kind: "unknown", state: "paused", reason: "banned" }, "running", ["stop"]],
    [{ kind: "unknown", state: "online" }, "stopped", ["start"]],
    [{ kind: "teleported" }, "running", ["stop"]],
    [{ kind: "reported", state: "hibernating" }, "running", ["stop"]],
    [{ kind: "reported", state: "online" }, "dormant", ["start"]],
  ] as [BotStatus, BotResponse["desired_state"], string[]][])(
    "%o wanted %s offers %o",
    (status, desired, controls) => {
      expect(controlsFor(withStatus(status, desired))).toEqual(controls);
    },
  );
});
