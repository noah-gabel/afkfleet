import { screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { fakeWatch } from "../../api/FakeApiClient";
import type { LiveUpdateMessage } from "../../generated/LiveUpdateMessage";
import { formatClock, formatClockSeconds } from "../../lib/time";
import { bot, botFrame, botId, disconnected, event, mode, modeId, refetch } from "../../test/live";
import { fakeTime } from "../../test/renderApp";
import { atBot, signedIn } from "../../test/signedIn";

const ID = botId(1);
const NOW = new Date("2026-10-26T14:05:00.000Z");
const AT = "2026-10-26T14:04:30.000Z";

afterEach(() => {
  vi.useRealTimers();
});

const ONLINE = bot(1, {
  desired_state: "running",
  status: { kind: "reported", state: "online", since: AT, online_since: AT, attempt: 1 },
});

/** The handlers every test here needs, with a watch the test pushes into. */
function live() {
  const watch = fakeWatch();
  return {
    watch,
    handlers: {
      watchEvents: watch.handler,
      bots: async () => ({ items: [ONLINE] }),
      bot: async () => ONLINE,
      modes: async () => ({
        items: [
          mode(2, { name: "farm" }),
          mode(1, { name: "afk", ownership: "built_in" as const }),
        ],
      }),
    },
  };
}

/** Pushes `updates` into the current watch, letting React settle after each. */
async function push(watch: ReturnType<typeof fakeWatch>, ...updates: LiveUpdateMessage[]) {
  for (const update of updates) {
    watch.latest().push(update);
    await Promise.resolve();
  }
}

/** A death of bot 1 at `at`. */
function died(at = AT): LiveUpdateMessage {
  return event({ type: "bot_died", bot_id: ID, at });
}

/** Home's compact lines about the bot. */
function lines(): string[] {
  return [...document.querySelectorAll("[data-slot=notice-line] > span")]
    .map((line) => line.textContent ?? "")
    .filter((line) => line.startsWith("AfkBot1"));
}

describe("the notices (U42, R109, R110)", () => {
  it("toast a death on any screen, and keep it as Home's line, merged, until dismissed", async () => {
    const time = fakeTime();
    vi.setSystemTime(NOW);
    const { watch, handlers } = live();
    const app = await signedIn(handlers, { advanceTimers: time.advanceTimers });
    await screen.findByRole("link", { name: "Open AfkBot1" });

    await push(watch, refetch("ready"), botFrame(ONLINE), died());

    expect(await screen.findByText("AfkBot1 died.")).toBeDefined();
    expect(lines()).toEqual([`AfkBot1 died at ${formatClock(AT)}.`]);
    await push(watch, died("2026-10-26T14:04:50.000Z"));
    expect(lines()).toEqual([
      `AfkBot1 died 2 times, last at ${formatClock("2026-10-26T14:04:50.000Z")}.`,
    ]);
    const line = document.querySelector<HTMLElement>("[data-slot=notice-line]");
    await app.user.click(within(line ?? document.body).getByRole("button", { name: "Dismiss" }));
    expect(lines()).toEqual([]);
  });

  it("open the bot's screen from Home's line, which then goes", async () => {
    const time = fakeTime();
    vi.setSystemTime(NOW);
    const { watch, handlers } = live();
    const app = await signedIn(handlers, { advanceTimers: time.advanceTimers });
    await screen.findByRole("link", { name: "Open AfkBot1" });
    await push(watch, refetch("ready"), botFrame(ONLINE), died());

    const line = await screen.findByText(`AfkBot1 died at ${formatClock(AT)}.`);
    await app.user.click(
      within(line.closest<HTMLElement>("[data-slot=notice-line]") ?? document.body).getByRole(
        "link",
        { name: "Open" },
      ),
    );

    expect(app.path()).toBe(`/bots/${ID}`);
    await app.user.click(within(screen.getByRole("main")).getByRole("link", { name: "Home" }));
    await screen.findByRole("link", { name: "Open AfkBot1" });
    expect(lines()).toEqual([]);
  });

  it("toast a death on its bot's open screen, but leave no line on Home", async () => {
    const time = fakeTime();
    vi.setSystemTime(NOW);
    const { watch, handlers } = live();
    const app = await atBot(ID, handlers, { advanceTimers: time.advanceTimers });
    await screen.findByRole("heading", { name: "AfkBot1" });

    await push(watch, refetch("ready"), botFrame(ONLINE), died());

    expect(await screen.findByText("AfkBot1 died.")).toBeDefined();
    await app.user.click(within(screen.getByRole("main")).getByRole("link", { name: "Home" }));
    await screen.findByRole("link", { name: "Open AfkBot1" });
    expect(lines()).toEqual([]);
  });

  it("open the bot's screen from the toast", async () => {
    const time = fakeTime();
    vi.setSystemTime(NOW);
    const { watch, handlers } = live();
    const app = await signedIn(handlers, { advanceTimers: time.advanceTimers });
    await screen.findByRole("link", { name: "Open AfkBot1" });
    await push(watch, refetch("ready"), botFrame(ONLINE), died());

    const toast = (await screen.findByText("AfkBot1 died.")).closest<HTMLElement>("li");
    await app.user.click(within(toast ?? document.body).getByRole("button", { name: "Open" }));

    expect(app.path()).toBe(`/bots/${ID}`);
  });
});

describe("the timeline (U46)", () => {
  it("say nothing came yet, since sign-in, while the app is open", async () => {
    const time = fakeTime();
    vi.setSystemTime(NOW);
    const { handlers } = live();
    await atBot(ID, handlers, { advanceTimers: time.advanceTimers });

    const section = await screen.findByRole("region", { name: "Timeline" });

    expect(within(section).getByText("Since you signed in, while the app is open.")).toBeDefined();
    expect(within(section).getByText("Nothing yet.")).toBeDefined();
  });

  it("list what came, newest first, each at its time to the second, modes by name", async () => {
    const time = fakeTime();
    vi.setSystemTime(NOW);
    const { watch, handlers } = live();
    await atBot(ID, handlers, { advanceTimers: time.advanceTimers });
    const section = await screen.findByRole("region", { name: "Timeline" });
    // Fake time moves with real time: set it right before the updates arrive.
    vi.setSystemTime(NOW);

    await push(watch, refetch("ready"), died(), botFrame({ ...ONLINE, mode_id: modeId(2) }));

    const items = await within(section).findAllByRole("listitem");
    expect(items.map((item) => item.textContent)).toEqual([
      `${formatClockSeconds(NOW.toISOString())}Mode: afk (built in) → farm`,
      `${formatClockSeconds(AT)}Died`,
    ]);
  });

  it("show a gap of 5 s or more, never a shorter one", async () => {
    const time = fakeTime();
    vi.setSystemTime(NOW);
    const { watch, handlers } = live();
    await atBot(ID, handlers, { advanceTimers: time.advanceTimers });
    const section = await screen.findByRole("region", { name: "Timeline" });
    // Fake time moves with real time: set it right before the updates arrive.
    vi.setSystemTime(NOW);
    await push(watch, refetch("ready"), disconnected(0));
    await time.advance(4000);
    await push(watch, refetch("ready"), disconnected(5));

    await time.advance(6000);
    await push(watch, refetch("ready"));

    const items = await within(section).findAllByRole("listitem");
    expect(items.map((item) => item.textContent)).toEqual([
      `${formatClockSeconds(new Date(NOW.getTime() + 10_000).toISOString())}Live updates were down for 6 s; some events may be missing.`,
    ]);
  });
});
