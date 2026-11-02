// The Start or Stop button on Home's cards (Plan.md P11.11; ADR-0020, U88, S124): where the viewer's
// level allows, one call per card, its refusal and its wait by the button, as on the bot's screen.
import { act, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { BOTS_LIST_KEY } from "../../api/queries";
import type { BotResponse } from "../../generated/BotResponse";
import { account } from "../../test/accounts";
import { deferred } from "../../test/fixtures";
import { bot, mode } from "../../test/live";
import type { RenderedApp } from "../../test/renderApp";
import { fakeTime } from "../../test/renderApp";
import { limited } from "../../test/retryWait";
import { signedIn } from "../../test/signedIn";
import { refusal } from "../../test/signIn";

const ONLINE: BotResponse = bot(1, {
  desired_state: "running",
  status: { kind: "reported", state: "online" },
});
const STOPPED: BotResponse = bot(1);
/** Resting paused and wanted stopped: it offers Resume alone, so no button on its card. */
const RESTING: BotResponse = bot(1, {
  desired_state: "stopped",
  status: { kind: "unplaced", state: "paused" },
});

afterEach(() => {
  vi.useRealTimers();
});

/** Home over `handlers`, showing `shown`. */
async function home(
  shown: readonly BotResponse[],
  handlers: Partial<ApiClient> = {},
  advanceTimers?: (ms: number) => unknown,
) {
  const app = await signedIn(
    {
      bots: async () => ({ items: [...shown] }),
      modes: async () => ({ items: [mode(1, { name: "afk", ownership: "built_in" })] }),
      ...handlers,
    },
    advanceTimers === undefined ? {} : { advanceTimers },
  );
  await screen.findByRole("link", { name: `Open ${shown[0]?.account.name}` });
  return app;
}

/** How many times the list was read. */
const listReads = (app: RenderedApp) =>
  app.fake.calls.filter((call) => call.method === "bots").length;

/** Says the live watch is up or down, as the live layer does. */
const liveWatch = (app: RenderedApp, connected: boolean) =>
  act(() => app.runtime.stores.live.getState().setConnected(connected));

/** The list shows `shown` now, as a live frame leaves it. */
const showing = (app: RenderedApp, shown: readonly BotResponse[]) =>
  act(() => {
    app.runtime.queryClient.setQueryData(BOTS_LIST_KEY, {
      pages: [{ items: [...shown] }],
      pageParams: [undefined],
    });
  });

const disabled = (element: HTMLElement) => element.hasAttribute("disabled");
const button = (name: string) => screen.getByRole("button", { name });

describe("a card's button", () => {
  it.each(["control", "manage"] as const)(
    "is there for a %s holder, named for its bot",
    async (level) => {
      await home([bot(1, { account: account(1, { level }) })]);

      expect(button("Start AfkBot1").textContent).toBe("Start");
    },
  );

  it("isn't there for a View holder, and nothing says so on the card", async () => {
    await home([bot(1, { account: account(1, { level: "view" }) })]);

    expect(screen.queryByRole("button", { name: /AfkBot1/ })).toBeNull();
    expect(screen.queryByText(/View only|can view/)).toBeNull();
  });

  it.each([
    ["a stopped bot", STOPPED, "Start AfkBot1"],
    ["an online bot, Stop and not Restart", ONLINE, "Stop AfkBot1"],
    [
      "a bot paused while it should run, Stop and not Resume",
      bot(1, { desired_state: "running", status: { kind: "reported", state: "paused" } }),
      "Stop AfkBot1",
    ],
  ])("offers one button on %s", async (_name, shown, offered) => {
    await home([shown]);

    const buttons = within(screen.getByRole("main"))
      .getAllByRole("button")
      .map((one) => one.getAttribute("aria-label"));
    expect(buttons).toEqual([offered]);
  });

  it("offers none on a bot it can only resume, and the card still opens it", async () => {
    const app = await home([RESTING]);

    expect(screen.queryByRole("button", { name: /AfkBot1/ })).toBeNull();
    await app.user.click(screen.getByRole("link", { name: "Open AfkBot1" }));
    await waitFor(() => expect(app.path()).toMatch(/^\/bots\//));
  });

  it("calls start once and stays on Home", async () => {
    const start = vi.fn(async () => ({ ...STOPPED, desired_state: "running" as const }));
    const app = await home([STOPPED], { startBot: start });

    await app.user.click(button("Start AfkBot1"));

    await waitFor(() => expect(start).toHaveBeenCalledTimes(1));
    expect(start).toHaveBeenCalledWith(STOPPED.id);
    expect(app.path()).toBe("/home");
  });

  it("calls stop on a running bot", async () => {
    const stop = vi.fn(async () => ({ ...ONLINE, desired_state: "stopped" as const }));
    const app = await home([ONLINE], { stopBot: stop });

    await app.user.click(button("Stop AfkBot1"));

    await waitFor(() => expect(stop).toHaveBeenCalledTimes(1));
  });

  it("asks for no password, as the bot's screen doesn't", async () => {
    const stepUp = vi.fn(async () => {});
    const start = vi.fn(async () => STOPPED);
    const app = await home([STOPPED], { stepUp, startBot: start });

    await app.user.click(button("Start AfkBot1"));

    await waitFor(() => expect(start).toHaveBeenCalledTimes(1));
    expect(stepUp).not.toHaveBeenCalled();
    expect(screen.queryByLabelText("Current password")).toBeNull();
  });

  it("runs one call at a time on a card, and cards side by side", async () => {
    const first = deferred<BotResponse>();
    const started: string[] = [];
    const app = await home([bot(1), bot(2)], {
      startBot: (id) => {
        started.push(id);
        return id === bot(1).id ? first.promise : Promise.resolve(bot(2));
      },
    });

    const one = button("Start AfkBot1");
    await app.user.click(one);
    expect(disabled(one)).toBe(true);
    await app.user.click(button("Start AfkBot2"));

    await waitFor(() => expect(started).toEqual([bot(1).id, bot(2).id]));
    await act(async () => {
      first.resolve(bot(1));
      await first.promise;
    });
    await waitFor(() => expect(disabled(one)).toBe(false));
  });

  it("is the same button when Start becomes Stop, so the keyboard's place survives", async () => {
    const app = await home([STOPPED]);
    const before = button("Start AfkBot1");
    before.focus();

    showing(app, [ONLINE]);

    const after = await screen.findByRole("button", { name: "Stop AfkBot1" });
    expect(after).toBe(before);
    expect(document.activeElement).toBe(after);
  });
});

describe("what's read after a card's call", () => {
  it("leaves the list to the live frame while the watch is up", async () => {
    const start = vi.fn(async () => STOPPED);
    const app = await home([STOPPED], { startBot: start });
    liveWatch(app, true);
    const before = listReads(app);

    await app.user.click(button("Start AfkBot1"));

    await waitFor(() => expect(start).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(disabled(button("Start AfkBot1"))).toBe(false));
    expect(listReads(app)).toBe(before);
  });

  it("reads the list when the watch is down", async () => {
    const app = await home([STOPPED], { startBot: async () => STOPPED });
    liveWatch(app, false);
    const before = listReads(app);

    await app.user.click(button("Start AfkBot1"));

    await waitFor(() => expect(listReads(app)).toBe(before + 1));
  });

  it.each([
    [403, "forbidden"],
    [404, "not_found"],
    [409, "conflict"],
  ] as const)("reads the list after a %i, even with the watch up", async (status, code) => {
    const app = await home([STOPPED], {
      startBot: async () => {
        throw refusal(status, code);
      },
    });
    liveWatch(app, true);
    const before = listReads(app);

    await app.user.click(button("Start AfkBot1"));

    await waitFor(() => expect(listReads(app)).toBe(before + 1));
  });

  it("reads nothing after a 429", async () => {
    const time = fakeTime();
    const app = await home(
      [STOPPED],
      {
        startBot: async () => {
          throw limited(30);
        },
      },
      time.advanceTimers,
    );
    const before = listReads(app);

    await app.user.click(button("Start AfkBot1"));

    await screen.findByText("Try again in 30 seconds.");
    expect(listReads(app)).toBe(before);
  });
});

describe("a card's refusal", () => {
  const SERVER_NOT_SET = "Its server isn't set.";
  const notSet = () => refusal(409, "conflict", { message: SERVER_NOT_SET });

  it("shows a 409's own message by the button, and stays on Home", async () => {
    const app = await home([STOPPED], {
      startBot: async () => {
        throw notSet();
      },
    });

    await app.user.click(button("Start AfkBot1"));

    expect((await screen.findByRole("alert")).textContent).toBe(SERVER_NOT_SET);
    expect(app.path()).toBe("/home");
  });

  it("goes at the next call", async () => {
    let calls = 0;
    const app = await home([STOPPED], {
      startBot: async () => {
        calls += 1;
        if (calls === 1) {
          throw notSet();
        }
        return STOPPED;
      },
    });
    await app.user.click(button("Start AfkBot1"));
    await screen.findByRole("alert");

    await app.user.click(button("Start AfkBot1"));

    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
  });

  it("goes when the bot's state changes, as a card may have no next call", async () => {
    const app = await home([STOPPED], {
      startBot: async () => {
        throw refusal(500, "internal");
      },
    });
    await app.user.click(button("Start AfkBot1"));
    await screen.findByRole("alert");

    showing(app, [RESTING]);

    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
  });

  it("stays after a 403 that takes the button away, saying why it went", async () => {
    const GONE = "You can't control this bot any more.";
    let shown = STOPPED;
    const app = await home([STOPPED], {
      bots: async () => ({ items: [shown] }),
      startBot: async () => {
        shown = bot(1, { account: account(1, { level: "view" }) });
        throw refusal(403, "forbidden", { message: GONE });
      },
    });

    await app.user.click(button("Start AfkBot1"));

    await waitFor(() => expect(screen.queryByRole("button", { name: /AfkBot1/ })).toBeNull());
    expect(screen.getByRole("alert").textContent).toBe(GONE);
  });

  it("takes the card away after a 404", async () => {
    let items = [STOPPED];
    const app = await home([STOPPED], {
      bots: async () => ({ items }),
      startBot: async () => {
        items = [];
        throw refusal(404, "not_found");
      },
    });

    await app.user.click(button("Start AfkBot1"));

    await waitFor(() => expect(screen.queryByRole("link", { name: "Open AfkBot1" })).toBeNull());
  });
});

describe("a card's wait", () => {
  /** Home, a Start limited for 30 s, its countdown showing. */
  async function limitedStart() {
    const time = fakeTime();
    const app = await home(
      [STOPPED],
      {
        startBot: async () => {
          throw limited(30);
        },
      },
      time.advanceTimers,
    );
    await app.user.click(button("Start AfkBot1"));
    await screen.findByText("Try again in 30 seconds.");
    return app;
  }

  it("counts down on Start", async () => {
    await limitedStart();

    expect(disabled(button("Start AfkBot1"))).toBe(true);
  });

  it("spares Stop, which the limit exempts", async () => {
    const app = await limitedStart();

    showing(app, [ONLINE]);

    const stop = await screen.findByRole("button", { name: "Stop AfkBot1" });
    expect(disabled(stop)).toBe(false);
  });

  it("hides its countdown once no button is offered", async () => {
    const app = await limitedStart();

    showing(app, [RESTING]);

    await waitFor(() => expect(screen.queryByText(/Try again in/)).toBeNull());
  });

  it("keeps its state when Show more adds a page", async () => {
    const time = fakeTime();
    const app = await signedIn(
      {
        bots: async (before) =>
          before === undefined ? { items: [bot(2)], next: bot(2).id } : { items: [bot(1)] },
        modes: async () => ({ items: [] }),
        startBot: async () => {
          throw limited(30);
        },
      },
      { advanceTimers: time.advanceTimers },
    );
    const start = await screen.findByRole("button", { name: "Start AfkBot2" });
    await app.user.click(start);
    await screen.findByText("Try again in 30 seconds.");

    await app.user.click(button("Show more"));
    await screen.findByRole("button", { name: "Start AfkBot1" });

    expect(button("Start AfkBot2")).toBe(start);
    expect(disabled(start)).toBe(true);
    expect(screen.getAllByText(/Try again in/).length).toBeGreaterThan(0);
  });
});

describe("Show more", () => {
  it("waits while the list is being read again", async () => {
    const reading = deferred<{ items: BotResponse[]; next: string }>();
    let reads = 0;
    const app = await signedIn({
      bots: async () => {
        reads += 1;
        return reads === 1 ? { items: [STOPPED], next: bot(2).id } : reading.promise;
      },
      modes: async () => ({ items: [] }),
    });
    const more = await screen.findByRole("button", { name: "Show more" });
    expect(disabled(more)).toBe(false);

    act(() => {
      void app.runtime.queryClient.refetchQueries({ queryKey: BOTS_LIST_KEY });
    });

    await waitFor(() => expect(disabled(more)).toBe(true));
    await act(async () => {
      reading.resolve({ items: [STOPPED], next: bot(2).id });
      await reading.promise;
    });
    await waitFor(() => expect(disabled(more)).toBe(false));
  });
});
