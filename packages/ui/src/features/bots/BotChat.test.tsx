import { act, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { fakeWatch } from "../../api/FakeApiClient";
import type { BotResponse } from "../../generated/BotResponse";
import type { ChatLine } from "../../generated/ChatLine";
import type { ChatPage } from "../../generated/ChatPage";
import type { GrantLevel } from "../../generated/GrantLevel";
import type { LiveUpdateMessage } from "../../generated/LiveUpdateMessage";
import { formatClock, formatClockSeconds, formatDate } from "../../lib/time";
import { account } from "../../test/accounts";
import { deferred } from "../../test/fixtures";
import { bot, botId, event, line, refetch } from "../../test/live";
import { atBot } from "../../test/signedIn";
import { refusal } from "../../test/signIn";

const ID = botId(1);
const NOW = new Date("2026-10-27T15:00:00.000Z");
const TODAY = "2026-10-27T14:59:30.000Z";
const REQUEST = "01900000-0000-7000-8000-0000000004a1";

afterEach(() => {
  vi.useRealTimers();
});

/** Bot 1 as a viewer at `level` sees it, online. */
function seen(level: GrantLevel = "manage"): BotResponse {
  return bot(1, {
    account: account(1, { level }),
    desired_state: "running",
    status: { kind: "reported", state: "online", since: TODAY, online_since: TODAY },
  });
}

/** Line `id` with `changes`, said today. */
function said(id: number, changes: Partial<ChatLine> = {}): ChatLine {
  return { ...line(id), at: TODAY, ...changes };
}

/** Bot 1's screen, its chat answering `chat`, with a watch the test pushes into. */
async function console_(chat: ApiClient["botChat"], handlers: Partial<ApiClient> = {}) {
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(NOW);
  const watch = fakeWatch();
  const app = await atBot(ID, {
    bot: async () => seen(),
    botChat: chat,
    watchEvents: watch.handler,
    ...handlers,
  });
  await screen.findByRole("heading", { name: "Chat" });
  return { app, watch };
}

/** Pushes `updates` into the current watch. */
async function push(watch: ReturnType<typeof fakeWatch>, ...updates: LiveUpdateMessage[]) {
  for (const update of updates) {
    await act(async () => {
      watch.latest().push(update);
      await Promise.resolve();
    });
  }
}

/** The console's lines, in their order on the screen, as their text reads. */
function shown(): string[] {
  return within(screen.getByRole("log"))
    .getAllByRole("listitem")
    .map((item) => item.textContent ?? "");
}

/** A page of `lines`, newest first, with `next` when older ones follow. */
function page(lines: ChatLine[], next?: number): ChatPage {
  return next === undefined ? { items: lines } : { items: lines, next };
}

describe("the chat console (U43, U44)", () => {
  it("shows the history oldest at the top, each line's time, sender and text", async () => {
    await console_(async () =>
      page([
        said(3, { kind: "system", text: "AfkBot2 joined the game" }),
        said(2, { text: "hi", sender: { name: "Steve" } }),
        said(1, { kind: "emote", text: "waves", sender: { name: "Steve" } }),
      ]),
    );

    expect(await screen.findByText("waves")).toBeDefined();
    expect(shown()).toEqual([
      `${formatClock(TODAY)}* Stevewaves`,
      `${formatClock(TODAY)}Stevehi`,
      `${formatClock(TODAY)}ServerAfkBot2 joined the game`,
    ]);
    expect(screen.getAllByText(formatClock(TODAY))[0]?.getAttribute("title")).toBe(
      `${formatDate(TODAY)}, ${formatClockSeconds(TODAY)}`,
    );
  });

  it("dates a line from an earlier day", async () => {
    const before = "2026-10-24T09:00:00.000Z";
    await console_(async () => page([said(1, { at: before, text: "old" })]));

    expect(await screen.findByText(`${formatDate(before)}, ${formatClock(before)}`)).toBeDefined();
  });

  it("shows a user's line as asked of the bot, by You or by name, and a mode's with its tag", async () => {
    await console_(async () =>
      page([
        said(4, { direction: "out", kind: "mode", text: "/spawn" }),
        said(3, { direction: "out", kind: "user", text: "hello", mine: true }),
        said(2, {
          direction: "out",
          kind: "user",
          text: "hi all",
          sent_by: { id: "u", username: "afkadmin" },
        }),
        said(1, { direction: "out", kind: "user", text: "psst" }),
      ]),
    );

    expect(await screen.findByText("You asked AfkBot1 to send:")).toBeDefined();
    expect(screen.getByText("afkadmin asked AfkBot1 to send:")).toBeDefined();
    expect(screen.getByText("a user asked AfkBot1 to send:")).toBeDefined();
    expect(screen.getByText("mode")).toBeDefined();
    const mine = screen.getByText("hello").closest("li");
    expect(mine?.className).toContain("border-l");
  });

  it("marks outside the text what was left out of it", async () => {
    await console_(async () =>
      page([
        said(2, { direction: "out", kind: "user", text: "/login", arguments_hidden: true }),
        said(1, { text: "a long one", truncated: true }),
      ]),
    );

    expect((await screen.findByText("/login")).textContent).toBe("/login");
    expect(screen.getByText("(arguments hidden)")).toBeDefined();
    expect(screen.getByText("(cut short)")).toBeDefined();
  });

  it("shows a line's text as it came, a line break as ` | `, never pre-formatted, clipped", async () => {
    await console_(async () =>
      page([said(1, { text: "There are 2 players: | AfkBot1, AfkBot2" })]),
    );

    const text = await screen.findByText("There are 2 players: | AfkBot1, AfkBot2");

    expect(text.className).toContain("overflow-hidden");
    expect(document.querySelector('[class*="whitespace-pre"]')).toBeNull();
  });

  it("says when there's no chat yet, and while it loads", async () => {
    const answer = deferred<ChatPage>();
    const { app } = await console_(() => answer.promise);

    expect(screen.getByText("Loading chat…")).toBeDefined();
    answer.resolve(page([]));
    expect(await screen.findByText("No chat yet.")).toBeDefined();
    expect(app.fake.calls.filter((call) => call.method === "botChat")).toHaveLength(1);
  });

  it("says when the chat couldn't be read", async () => {
    await console_(async () => {
      throw refusal(404, "not_found");
    });

    expect(await screen.findByText(/^Couldn't load the chat: /)).toBeDefined();
  });

  it("loads older lines above, then says there are none", async () => {
    const { app } = await console_(async (_id, before) =>
      before === undefined ? page([said(3), said(2)], 2) : page([said(1)]),
    );
    await screen.findByText("line 3");

    await app.user.click(screen.getByRole("button", { name: "Show older" }));

    expect(await screen.findByText("No older lines.")).toBeDefined();
    expect(shown().map((text) => text.slice(-6))).toEqual(["line 1", "line 2", "line 3"]);
    expect(screen.queryByRole("button", { name: "Show older" })).toBeNull();
  });

  it("says when older lines couldn't be read, and tries again on Show older", async () => {
    let tries = 0;
    const { app } = await console_(async (_id, before) => {
      if (before === undefined) {
        return page([said(3), said(2)], 2);
      }
      tries += 1;
      if (tries === 1) {
        throw refusal(404, "not_found");
      }
      return page([said(1)]);
    });
    await screen.findByText("line 3");

    await app.user.click(screen.getByRole("button", { name: "Show older" }));
    expect(await screen.findByText(/^Couldn't load older lines: /)).toBeDefined();
    await app.user.click(screen.getByRole("button", { name: "Show older" }));

    expect(await screen.findByText("line 1")).toBeDefined();
    expect(screen.queryByText(/^Couldn't load older lines: /)).toBeNull();
  });
});

describe("the chat console, live (S70)", () => {
  it("adds a pushed line at the bottom", async () => {
    const { watch } = await console_(async () => page([said(1)]));
    await screen.findByText("line 1");

    await push(watch, refetch("ready"), event({ type: "chat", bot_id: ID, line: said(2) }));

    expect(await screen.findByText("line 2")).toBeDefined();
    expect(shown().map((text) => text.slice(-6))).toEqual(["line 1", "line 2"]);
  });

  it("marks another user's line from its frames: sent, or not sent with why", async () => {
    const { watch } = await console_(async () => page([]));
    const asked = (id: number, request: string) =>
      event({
        type: "chat",
        bot_id: ID,
        line: said(id, { direction: "out", kind: "user", text: `ask ${id}` }),
        request,
      });

    await push(
      watch,
      refetch("ready"),
      asked(1, REQUEST),
      event({ type: "chat_sent", bot_id: ID, request: REQUEST }),
      asked(2, "other"),
      event({ type: "chat_failed", bot_id: ID, request: "other", failure: "chat_unavailable" }),
    );

    expect(await screen.findByText("sent")).toBeDefined();
    const failed = screen.getByText("not sent: needs signed chat");
    expect(failed.className).toContain("text-destructive");
  });

  it("follows new lines while scrolled to the bottom, and stays put while scrolled up", async () => {
    const { watch } = await console_(async () => page([said(1)]));
    await screen.findByText("line 1");
    const log = screen.getByRole("log");
    const box = { height: 1000, top: 800 };
    Object.defineProperty(log, "scrollHeight", { configurable: true, get: () => box.height });
    Object.defineProperty(log, "clientHeight", { configurable: true, get: () => 200 });
    Object.defineProperty(log, "scrollTop", {
      configurable: true,
      get: () => box.top,
      set: (top: number) => {
        box.top = top;
      },
    });
    log.dispatchEvent(new Event("scroll"));

    box.height = 1100;
    await push(watch, refetch("ready"), event({ type: "chat", bot_id: ID, line: said(2) }));
    await screen.findByText("line 2");
    const followed = box.top;
    box.top = 100;
    log.dispatchEvent(new Event("scroll"));
    box.height = 1200;
    await push(watch, event({ type: "chat", bot_id: ID, line: said(3) }));
    await screen.findByText("line 3");

    expect(followed).toBe(1100);
    expect(box.top).toBe(100);
  });
});

describe("the chat console's look (PR #77's review)", () => {
  it("mutes a server's, an announcement's and a join or leave line whole, in italics (U44)", async () => {
    await console_(async () =>
      page([
        said(2, { kind: "announcement", text: "Restart in 5 minutes" }),
        said(1, { kind: "system", text: "AfkBot2 joined the game" }),
      ]),
    );

    for (const text of ["AfkBot2 joined the game", "Restart in 5 minutes"]) {
      const item = (await screen.findByText(text)).closest("li");
      expect(item?.className).toContain("italic");
      expect(item?.className).toContain("text-muted-foreground");
    }
  });

  it("marks a name a chat format read with its icon and label, outside the capped name, never in the players' color (U55)", async () => {
    const mark = "Read from the server's text by a chat format. Minecraft doesn't check this name.";
    await console_(async () =>
      page([
        said(3, { kind: "chat", text: "hello there", sender: { name: "Steve" } }),
        said(2, { kind: "system", text: "[Clan] Notch : hello", parsed_sender: "Notch" }),
        said(1, { kind: "system", text: "Server restarting soon" }),
      ]),
    );

    const matched = (await screen.findByText("[Clan] Notch : hello")).closest("li");
    const name = within(matched as HTMLElement).getByText("Notch");
    const label = within(matched as HTMLElement).getByText(mark);
    const marked = label.closest("[title]");

    expect(matched?.className).not.toContain("italic");
    expect(matched?.className).not.toContain("text-muted-foreground");
    expect(marked?.getAttribute("title")).toBe(mark);
    expect(marked?.querySelector("svg")?.getAttribute("aria-hidden")).toBe("true");
    expect(label.className).toContain("sr-only");
    expect(name.contains(label)).toBe(false);
    expect(name.className).toContain("truncate");
    expect(name.className).toContain("font-medium");
    expect(name.className).not.toContain("text-primary");
    expect(name.getAttribute("title")).toBe("Notch");
    expect(within(screen.getByRole("log")).getByText("Steve").className).toContain("text-primary");
    const unmatched = screen.getByText("Server restarting soon").closest("li");
    expect(unmatched?.className).toContain("italic");
    expect(within(unmatched as HTMLElement).getByText("Server")).toBeDefined();
    expect(within(unmatched as HTMLElement).queryByText(mark)).toBeNull();
  });

  it("lines the columns up across lines: each line a row of the list's own columns", async () => {
    await console_(async () => page([said(2), said(1)]));

    const item = (await screen.findByText("line 1")).closest("li");

    expect(item?.parentElement?.className).toContain("grid-cols-[auto_auto_minmax(0,1fr)]");
    expect(item?.className).toContain("grid-cols-subgrid");
    expect(item?.className).toContain("col-span-3");
  });

  it("caps a long sender's name, the whole name its tooltip", async () => {
    const name = "AVeryLongPlayerNameThatGoesOnAndOnAndOn";
    await console_(async () => page([said(1, { sender: { name } })]));

    const sender = await screen.findByText(name);

    expect(sender.className).toContain("truncate");
    expect(sender.getAttribute("title")).toBe(name);
  });
});

describe("the chat console's place when its height changes", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("stays at the bottom while following, when something under it takes room", async () => {
    let resized: (() => void) | null = null;
    vi.stubGlobal(
      "ResizeObserver",
      class {
        constructor(callback: () => void) {
          resized = callback;
        }
        observe() {}
        disconnect() {}
      },
    );
    await console_(async () => page([said(1)]));
    await screen.findByText("line 1");
    const log = screen.getByRole("log");
    const box = { height: 1000, client: 200, top: 800 };
    Object.defineProperty(log, "scrollHeight", { configurable: true, get: () => box.height });
    Object.defineProperty(log, "clientHeight", { configurable: true, get: () => box.client });
    Object.defineProperty(log, "scrollTop", {
      configurable: true,
      get: () => box.top,
      set: (top: number) => {
        box.top = top;
      },
    });
    log.dispatchEvent(new Event("scroll"));

    box.client = 120;
    act(() => resized?.());

    expect(box.top).toBe(1000);
  });
});

describe("the chat console's place", () => {
  it("keeps the reader's place when older lines load above", async () => {
    const olderPage = deferred<ChatPage>();
    const { app } = await console_(async (_id, before) =>
      before === undefined ? page([said(3), said(2)], 2) : olderPage.promise,
    );
    await screen.findByText("line 3");
    const log = screen.getByRole("log");
    const box = { height: 1000, top: 0 };
    Object.defineProperty(log, "scrollHeight", { configurable: true, get: () => box.height });
    Object.defineProperty(log, "clientHeight", { configurable: true, get: () => 200 });
    Object.defineProperty(log, "scrollTop", {
      configurable: true,
      get: () => box.top,
      set: (top: number) => {
        box.top = top;
      },
    });
    log.dispatchEvent(new Event("scroll"));

    await app.user.click(screen.getByRole("button", { name: "Show older" }));
    box.height = 1500;
    olderPage.resolve(page([said(1)]));
    await screen.findByText("line 1");

    expect(box.top).toBe(500);
  });
});

describe("who reads the chat", () => {
  it("shows a View holder the chat", async () => {
    await console_(async () => page([said(1)]), { bot: async () => seen("view") });

    expect(await screen.findByText("line 1")).toBeDefined();
  });
});
