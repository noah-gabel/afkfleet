import { describe, expect, it } from "vitest";

import type { BotResponse } from "../generated/BotResponse";
import type { BotStatus } from "../generated/BotStatus";
import type { LiveUpdateMessage } from "../generated/LiveUpdateMessage";
import { bot, botFrame, botId, disconnected, event, modeId, refetch } from "../test/live";
import {
  acknowledge,
  dismiss,
  down,
  drop,
  ENTRIES_MAX,
  emptyRecord,
  GAPS_MAX,
  known,
  type LiveRecord,
  observe,
  timeline,
} from "./liveRecord";

const T0 = Date.parse("2026-10-26T14:00:00.000Z");
const SINCE = "2026-10-26T13:59:58.000Z";

/** `updates` observed one second apart from `T0`, after a first `ready`, nothing open. */
function observed(updates: LiveUpdateMessage[], from: LiveRecord = readied()): LiveRecord {
  return updates.reduce(
    (record, update, at) => observe(record, update, T0 + at * 1000, null),
    from,
  );
}

/** A record past its generation's first `ready`. */
function readied(): LiveRecord {
  return observe(emptyRecord(), refetch("ready"), T0 - 60_000, null);
}

/** Bot 1 with `status`, wanted running. */
function withStatus(status: BotStatus, changes: Partial<BotResponse> = {}): BotResponse {
  return bot(1, { desired_state: "running", status, ...changes });
}

const ONLINE: BotStatus = { kind: "reported", state: "online", since: SINCE, attempt: 1 };
const BACKOFF: BotStatus = { kind: "reported", state: "backoff", since: SINCE, attempt: 2 };

/** Bot 1's entries, oldest first, by kind. */
function kinds(record: LiveRecord): string[] {
  return (record.bots[botId(1)]?.entries ?? []).map((entry) => entry.kind);
}

/** A died event for bot 1 at `at`. */
function died(at: string): LiveUpdateMessage {
  return event({ type: "bot_died", bot_id: botId(1), at });
}

/** A frame of bot 1 backing off after its agent refused the address. */
function blocked(attempt: number): LiveUpdateMessage {
  return botFrame(
    withStatus({
      kind: "reported",
      state: "backoff",
      since: SINCE,
      attempt,
      last_disconnect: { kind: "connect_failed", connect_failure: "blocked" },
    }),
  );
}

describe("the live record's entries (S71)", () => {
  it("records a bot's first frame as its status, and a fill-in from a read never (R108)", () => {
    const filled = known(readied(), [withStatus(ONLINE)]);
    const seen = observed([botFrame(bot(2))]);

    expect(kinds(filled)).toEqual([]);
    expect(seen.bots[botId(2)]?.entries.map((entry) => entry.kind)).toEqual(["status"]);
  });

  it("records a status change against the last known one, at its own time, and a repeat never", () => {
    const record = observed(
      [botFrame(withStatus(BACKOFF)), botFrame(withStatus(BACKOFF))],
      known(readied(), [withStatus(ONLINE)]),
    );

    const entries = record.bots[botId(1)]?.entries ?? [];

    expect(entries).toHaveLength(1);
    expect(entries[0]).toMatchObject({ kind: "status", at: SINCE, arrived: T0 });
  });

  it("never lets a fill-in overwrite a known status", () => {
    const record = known(observed([botFrame(withStatus(BACKOFF))]), [withStatus(ONLINE)]);

    expect(record.bots[botId(1)]?.known?.status).toEqual(BACKOFF);
  });

  it("marks a status entry whose last disconnect is new", () => {
    const kicked = withStatus({
      ...BACKOFF,
      last_disconnect: { kind: "kicked", message: "Testing" },
    });

    const record = observed([botFrame(kicked)], known(readied(), [withStatus(ONLINE)]));

    expect(record.bots[botId(1)]?.entries[0]).toMatchObject({
      kind: "status",
      newDisconnect: true,
    });
  });

  it("records a mode and a desired state that changed, at their arrival", () => {
    const record = observed(
      [
        botFrame(withStatus(ONLINE, { mode_id: modeId(2) })),
        botFrame(withStatus(ONLINE, { mode_id: modeId(2), desired_state: "stopped" })),
      ],
      known(readied(), [withStatus(ONLINE)]),
    );

    expect(record.bots[botId(1)]?.entries).toEqual([
      { kind: "mode", arrived: T0, from: modeId(1), to: modeId(2) },
      { kind: "desired", arrived: T0 + 1000, desired: "stopped" },
    ]);
  });

  it("merges consecutive deaths, with their count and the last one's time", () => {
    const record = observed([died("2026-10-26T14:00:01.000Z"), died("2026-10-26T14:00:05.000Z")]);

    expect(record.bots[botId(1)]?.entries).toEqual([
      { kind: "died", arrived: T0 + 1000, at: "2026-10-26T14:00:05.000Z", count: 2 },
    ]);
  });

  it("merges an alert with its neighbouring status, whichever comes first", () => {
    const paused = withStatus({
      kind: "reported",
      state: "paused",
      reason: "banned",
      since: SINCE,
    });
    const alert = event({
      type: "bot_alert",
      bot_id: botId(1),
      at: SINCE,
      state: "paused",
      reason: "banned",
    });
    const start = known(readied(), [withStatus(ONLINE)]);

    expect(kinds(observed([botFrame(paused), alert], start))).toEqual(["alert"]);
    expect(kinds(observed([alert, botFrame(paused)], start))).toEqual(["alert"]);
  });

  it("keeps at most 100 entries per bot, dropping the oldest", () => {
    const frames = Array.from({ length: ENTRIES_MAX + 1 }, (_, n) =>
      botFrame(withStatus({ ...BACKOFF, attempt: n + 2 })),
    );

    const entries = observed(frames).bots[botId(1)]?.entries ?? [];

    expect(entries).toHaveLength(ENTRIES_MAX);
    expect(entries[0]).toMatchObject({ status: { attempt: 3 } });
  });
});

describe("the live record's gaps (S71, R107)", () => {
  it("marks no gap at a generation's first ready, then one from the disconnect to the refetch", () => {
    const record = [refetch("ready"), disconnected(5), refetch("ready")].reduce(
      (now, update, at) => observe(now, update, T0 + at * 6000, null),
      emptyRecord(),
    );

    expect(record.gaps).toEqual([{ arrived: T0 + 12_000, downMs: 6000 }]);
  });

  it("keeps no gap under 5 s, nor a resync's, which had no down time", () => {
    const record = observed([disconnected(0), refetch("ready"), refetch("resync")]);

    expect(record.gaps).toEqual([]);
  });

  it("counts a gap from the watch's end when a new watch starts", () => {
    const ended = down(readied(), T0);

    const record = observe(ended, refetch("ready"), T0 + 10_000, null);

    expect(record.gaps).toEqual([{ arrived: T0 + 10_000, downMs: 10_000 }]);
  });

  it("keeps at most 100 gaps", () => {
    let record = readied();
    for (let n = 0; n <= GAPS_MAX; n += 1) {
      record = observe(down(record, n * 10_000), refetch("ready"), n * 10_000 + 6000, null);
    }

    expect(record.gaps).toHaveLength(GAPS_MAX);
  });

  it("shows the bot's entries and the gaps in one timeline, newest first", () => {
    const record = observe(
      observe(down(observed([died(SINCE)]), T0 + 2000), refetch("ready"), T0 + 9000, null),
      died("2026-10-26T14:00:10.000Z"),
      T0 + 10_000,
      null,
    );

    expect(timeline(record, botId(1)).map((item) => item.kind)).toEqual(["died", "gap", "died"]);
  });
});

describe("the live record's notices (S71, U42)", () => {
  it("gives a death a notice and a toast, merging later ones into the notice", () => {
    const record = observed([botFrame(withStatus(ONLINE)), died(SINCE), died(SINCE)]);

    expect(record.notices).toMatchObject([{ kind: "died", count: 2, name: "AfkBot1" }]);
    expect(record.toasts.map((toast) => toast.kind)).toEqual(["died", "died"]);
  });

  it("names a bot it never saw as unknown", () => {
    expect(observed([died(SINCE)]).notices[0]?.name).toBeNull();
  });

  it("gives a paused or failed alert its notice", () => {
    const alert = (state: "paused" | "failed") =>
      event({ type: "bot_alert", bot_id: botId(1), at: SINCE, state, reason: "banned" });

    const record = observed([alert("paused"), alert("failed")]);

    expect(
      record.notices.map((notice) => [notice.kind, "reason" in notice ? notice.reason : null]),
    ).toEqual([
      ["paused", "banned"],
      ["failed", "banned"],
    ]);
  });

  it("gives a blocked address one notice per streak", () => {
    const record = observed([blocked(2), blocked(3), botFrame(withStatus(ONLINE)), blocked(2)]);

    expect(record.toasts.filter((toast) => toast.kind === "blocked")).toHaveLength(2);
  });

  it("gives a failed respawn a notice only when an Online session ended with it", () => {
    const respawnFailed = (state: "backoff" | "connecting") =>
      botFrame(
        withStatus({
          kind: "reported",
          state,
          since: SINCE,
          attempt: 1,
          last_disconnect: { kind: "respawn_failed" },
        }),
      );

    const fromOnline = observed([botFrame(withStatus(ONLINE)), respawnFailed("backoff")]);
    const carriedOn = observed([respawnFailed("backoff"), respawnFailed("connecting")]);

    expect(fromOnline.notices.map((notice) => notice.kind)).toEqual(["respawn"]);
    expect(carriedOn.notices).toEqual([]);
  });

  it("toasts a notice for the bot whose screen is open but keeps no line of it (R109)", () => {
    const record = observe(readied(), died(SINCE), T0, botId(1));

    expect(record.toasts).toHaveLength(1);
    expect(record.notices).toEqual([]);
  });

  it("drops a notice dismissed, a bot's acknowledged ones, and a removed bot's everything", () => {
    const record = observed([
      botFrame(withStatus(ONLINE)),
      botFrame(bot(2)),
      died(SINCE),
      event({ type: "bot_died", bot_id: botId(2), at: SINCE }),
    ]);
    const first = record.notices[0]?.id ?? -1;

    expect(dismiss(record, first).notices.map((notice) => notice.botId)).toEqual([botId(2)]);
    expect(acknowledge(record, botId(2)).notices.map((notice) => notice.botId)).toEqual([botId(1)]);
    const dropped = drop(record, botId(1));
    expect(dropped.bots[botId(1)]).toBeUndefined();
    expect(dropped.notices.map((notice) => notice.botId)).toEqual([botId(2)]);
  });

  it("drops a bot's ring and notices at bot_removed", () => {
    const record = observed([
      botFrame(withStatus(ONLINE)),
      died(SINCE),
      event({ type: "bot_removed", bot_id: botId(1) }),
    ]);

    expect(record.bots[botId(1)]).toBeUndefined();
    expect(record.notices).toEqual([]);
  });
});
