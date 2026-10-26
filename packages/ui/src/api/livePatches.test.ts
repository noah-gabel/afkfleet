import type { QueryKey } from "@tanstack/react-query";
import { describe, expect, it } from "vitest";

import type { AccountResponse } from "../generated/AccountResponse";
import type { EventMessage } from "../generated/EventMessage";
import { account, accountId } from "../test/accounts";
import { bot, botId, line, mode, modeId, pages } from "../test/live";
import { type LivePatch, patchesFor, READ_AGAIN } from "./livePatches";
import {
  ACCOUNT_LIST_KEY,
  ACCOUNTS_KEY,
  AWAITING_KEY,
  accountKey,
  BOTS_LIST_KEY,
  botKey,
  chatKey,
  grantsKey,
  MODES_LIST_KEY,
} from "./queries";

/** What `event`'s patch for `queryKey` makes of `data`. */
function patched(event: EventMessage, queryKey: QueryKey, data: unknown): unknown {
  const patch = patchesFor(event).find(
    (one): one is Extract<LivePatch, { kind: "set" }> =>
      one.kind === "set" && JSON.stringify(one.queryKey) === JSON.stringify(queryKey),
  );
  if (patch === undefined) {
    throw new Error(`no patch for ${JSON.stringify(queryKey)}`);
  }
  return patch.update(data);
}

/** The items of every page of `data`, in order. */
function itemsOf(data: unknown): unknown[] {
  return (data as ReturnType<typeof pages>).pages.flatMap((page) => page.items);
}

/** A `bot` frame of bot `n`, with `changes`. */
function frame(n: number, changes: Parameters<typeof bot>[1] = {}): EventMessage {
  const shown = bot(n, changes);
  return { type: "bot", bot_id: shown.id, bot: shown };
}

describe("a bot frame", () => {
  it("replaces the cached bot, and the same frame again changes nothing more", () => {
    const cached = pages({ items: [bot(5), bot(3)], next: botId(3) });
    const online = frame(3, { status: { kind: "reported", state: "online" } });

    const once = patched(online, BOTS_LIST_KEY, cached);
    const twice = patched(online, BOTS_LIST_KEY, once);

    expect(itemsOf(once)).toEqual([bot(5), online.bot]);
    expect(twice).toEqual(once);
    expect(patched(online, botKey(botId(3)), bot(3))).toEqual(online.bot);
  });

  it.each([
    ["at the top", 6, [6, 5, 3]],
    ["between two", 4, [5, 4, 3]],
  ])("places a new bot %s of the first page", (_, n, order) => {
    const cached = pages({ items: [bot(5), bot(3)], next: botId(3) });

    const next = patched(frame(n), BOTS_LIST_KEY, cached);

    expect(itemsOf(next)).toEqual(order.map((one) => bot(one)));
  });

  it("leaves a bot older than every loaded page with more after it to its own page", () => {
    const cached = pages({ items: [bot(5), bot(3)], next: botId(3) });

    expect(patched(frame(2), BOTS_LIST_KEY, cached)).toBe(cached);
  });

  it("places an older bot on the last page when nothing follows it", () => {
    const cached = pages({ items: [bot(5), bot(3)], next: botId(3) }, { items: [bot(2)] });
    const empty = pages<ReturnType<typeof bot>>({ items: [] });

    expect(itemsOf(patched(frame(1), BOTS_LIST_KEY, cached))).toEqual([
      bot(5),
      bot(3),
      bot(2),
      bot(1),
    ]);
    expect(itemsOf(patched(frame(1), BOTS_LIST_KEY, empty))).toEqual([bot(1)]);
  });

  it("replaces or places its account in the list and the account, and reads its grants again", () => {
    const renamed = frame(3, { account: account(3, { name: "AfkBot33" }) });
    const cached = pages({ items: [account(5), account(3)] });

    expect(itemsOf(patched(renamed, ACCOUNT_LIST_KEY, cached))).toEqual([
      account(5),
      account(3, { name: "AfkBot33" }),
    ]);
    expect(itemsOf(patched(frame(4), ACCOUNT_LIST_KEY, cached))).toEqual([
      account(5),
      account(4),
      account(3),
    ]);
    expect(patched(renamed, accountKey(accountId(3)), account(3))).toEqual(
      account(3, { name: "AfkBot33" }),
    );
    expect(patchesFor(renamed)).toContainEqual({
      kind: "invalidate",
      queryKey: grantsKey(accountId(3)),
    });
  });
});

describe("the waiting list from a bot frame (S65)", () => {
  const pending = (n: number) => account(n, { status: "pending" });
  const statusFrame = (n: number, status: AccountResponse["status"]) =>
    frame(n, { account: account(n, { status, name: "AfkBot99" }) });

  it("replaces a pending account it lists", () => {
    const listed = [pending(4), pending(2)];

    expect(patched(statusFrame(2, "pending"), AWAITING_KEY, listed)).toEqual([
      pending(4),
      account(2, { status: "pending", name: "AfkBot99" }),
    ]);
  });

  it("reads the list again for a pending account it doesn't list: only the server knows", () => {
    expect(patched(statusFrame(3, "pending"), AWAITING_KEY, [pending(4)])).toBe(READ_AGAIN);
  });

  it.each(["active", "reauth_required", "refused"] as const)(
    "takes an account that's %s off the list, and changes nothing when it isn't on it",
    (status) => {
      const listed = [pending(4), pending(2)];

      expect(patched(statusFrame(2, status), AWAITING_KEY, listed)).toEqual([pending(4)]);
      expect(patched(statusFrame(3, status), AWAITING_KEY, listed)).toBe(listed);
    },
  );

  it("reads the list again for a status this version doesn't know", () => {
    const newer = statusFrame(2, "archived" as AccountResponse["status"]);

    expect(patched(newer, AWAITING_KEY, [pending(2)])).toBe(READ_AGAIN);
  });
});

describe("the other events", () => {
  it("bot_removed takes the bot off the list, drops it, resets its chat and reads every account again", () => {
    const removed: EventMessage = { type: "bot_removed", bot_id: botId(3) };
    const cached = pages({ items: [bot(5), bot(3)] });

    expect(itemsOf(patched(removed, BOTS_LIST_KEY, cached))).toEqual([bot(5)]);
    expect(patchesFor(removed)).toEqual(
      expect.arrayContaining([
        { kind: "remove", queryKey: botKey(botId(3)) },
        { kind: "reset", queryKey: chatKey(botId(3)) },
        { kind: "invalidate", queryKey: ACCOUNTS_KEY },
      ]),
    );
  });

  it("chat merges its line into the bot's chat by the row's ID", () => {
    const chat: EventMessage = { type: "chat", bot_id: botId(1), line: line(8) };
    const again: EventMessage = { type: "chat", bot_id: botId(1), line: line(9, "edited") };
    const cached = pages({ items: [line(9), line(7)] });

    expect(itemsOf(patched(chat, chatKey(botId(1)), cached))).toEqual([line(9), line(8), line(7)]);
    expect(itemsOf(patched(again, chatKey(botId(1)), cached))).toEqual([
      line(9, "edited"),
      line(7),
    ]);
  });

  it("mode replaces or places the mode, and mode_removed takes it off", () => {
    const edited: EventMessage = { type: "mode", mode: mode(2, { version: 2 }) };
    const removed: EventMessage = { type: "mode_removed", mode_id: modeId(2) };
    const cached = pages({ items: [mode(3), mode(2)] });

    expect(itemsOf(patched(edited, MODES_LIST_KEY, cached))).toEqual([
      mode(3),
      mode(2, { version: 2 }),
    ]);
    expect(itemsOf(patched(removed, MODES_LIST_KEY, cached))).toEqual([mode(3)]);
    expect(patched({ type: "mode_removed", mode_id: modeId(1) }, MODES_LIST_KEY, cached)).toBe(
      cached,
    );
  });

  it.each([
    "bot_died",
    "bot_alert",
    "chat_sent",
    "chat_failed",
    "ready",
    "resync",
    "bot_teleported",
  ])("%s patches nothing", (type) => {
    const notice = { type, bot_id: botId(1) } as EventMessage;

    expect(patchesFor(notice)).toEqual([]);
  });

  it("a frame missing what its kind carries patches nothing", () => {
    expect(patchesFor({ type: "bot" })).toEqual([]);
    expect(patchesFor({ type: "chat", bot_id: botId(1) })).toEqual([]);
    expect(patchesFor({ type: "mode" })).toEqual([]);
  });
});
