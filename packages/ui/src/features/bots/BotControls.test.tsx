import { screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import type { BotResponse } from "../../generated/BotResponse";
import { account } from "../../test/accounts";
import { bot, botId, mode } from "../../test/live";
import { fakeTime } from "../../test/renderApp";
import { expectWait, limited } from "../../test/retryWait";
import { atBot } from "../../test/signedIn";
import { refusal } from "../../test/signIn";
import { SENT_NOTE, VIEW_ONLY } from "./texts";

const ID = botId(1);
const NOW = new Date("2026-10-26T14:05:00.000Z");
const SINCE = "2026-10-26T12:00:00.000Z";

afterEach(() => {
  vi.useRealTimers();
});

const ONLINE = bot(1, {
  server_address: "127.0.0.1:25565",
  desired_state: "running",
  status: { kind: "reported", state: "online", since: SINCE, online_since: SINCE, attempt: 1 },
});

/** Bot 1's screen, its bot `shown` at first, its later reads `then` (else `shown`). */
async function controls(
  shown: BotResponse,
  handlers: Partial<ApiClient> = {},
  then: BotResponse = shown,
) {
  const time = fakeTime();
  vi.setSystemTime(NOW);
  let reads = 0;
  const app = await atBot(
    ID,
    {
      bot: async () => {
        reads += 1;
        return reads === 1 ? shown : then;
      },
      modes: async () => ({ items: [mode(1, { name: "afk", ownership: "built_in" })] }),
      ...handlers,
    },
    { advanceTimers: time.advanceTimers },
  );
  await screen.findByRole("heading", { name: shown.account.name });
  return { app, time, reads: () => reads };
}

/** The controls' labels. */
const LABELS = ["Start", "Stop", "Restart", "Resume", "Reset"];

/** The names of the controls the screen offers, in their order. */
function offered(): string[] {
  return screen
    .queryAllByRole("button")
    .map((button) => button.textContent ?? "")
    .filter((name) => LABELS.includes(name));
}

describe("a bot's controls", () => {
  it("offer a running bot's Stop and Restart to a Control holder", async () => {
    await controls(bot(1, { ...ONLINE, account: account(1, { level: "control" }) }));

    expect(offered()).toEqual(["Stop", "Restart"]);
    expect(screen.queryByText(VIEW_ONLY)).toBeNull();
  });

  it("offer a paused bot's Resume and Stop, and a failed one's Reset and Stop", async () => {
    await controls(
      bot(1, {
        desired_state: "running",
        status: { kind: "reported", state: "paused", reason: "banned", since: SINCE },
      }),
    );

    expect(offered()).toEqual(["Resume", "Stop"]);
  });

  it("offer nothing to a View holder, who reads one muted line instead", async () => {
    await controls(bot(1, { ...ONLINE, account: account(1, { level: "view" }) }));

    expect(offered()).toEqual([]);
    expect(screen.getByText(VIEW_ONLY)).toBeDefined();
  });

  it("start a stopped bot with no note, and read it again", async () => {
    const started = bot(1, { desired_state: "running", status: { kind: "awaiting_report" } });
    const { app, reads } = await controls(bot(1), { startBot: async () => started }, started);

    await app.user.click(screen.getByRole("button", { name: "Start" }));

    expect(await screen.findByText("Starting")).toBeDefined();
    expect(app.fake.calls.filter((call) => call.method === "startBot")).toHaveLength(1);
    expect(reads()).toBe(2);
    expect(screen.queryByText(SENT_NOTE)).toBeNull();
  });

  it("say a restart was sent until the bot changes", async () => {
    const { app } = await controls(
      ONLINE,
      { restartBot: async () => ({ bot: ONLINE, effect: "sent" }) },
      ONLINE,
    );

    await app.user.click(screen.getByRole("button", { name: "Restart" }));

    expect(await screen.findByText(SENT_NOTE)).toBeDefined();
  });

  it("say nothing was sent when a resume cleared a stored state", async () => {
    const paused = bot(1, {
      desired_state: "stopped",
      status: { kind: "unplaced", why: "stopped", state: "paused", reason: "banned" },
    });
    const cleared = bot(1, { status: { kind: "unplaced", why: "stopped" } });
    const { app } = await controls(
      paused,
      { resumeBot: async () => ({ bot: cleared, effect: "cleared" }) },
      cleared,
    );

    await app.user.click(screen.getByRole("button", { name: "Resume" }));

    expect(await screen.findByRole("button", { name: "Start" })).toBeDefined();
    expect(screen.queryByText(SENT_NOTE)).toBeNull();
  });

  it.each([
    [409, "bot_not_running", "Only a running bot can be restarted."],
    [403, "forbidden", "You don't have permission to do this."],
  ])("show a %i's message by the controls and read the bot again", async (status, code, text) => {
    const { app, reads } = await controls(ONLINE, {
      restartBot: async () => {
        throw refusal(status, code, { message: text });
      },
    });

    await app.user.click(screen.getByRole("button", { name: "Restart" }));

    expect(await screen.findByText(text)).toBeDefined();
    expect(reads()).toBe(2);
  });

  it("wait out a 429 on the controls that spend the bot's limit, never on Stop", async () => {
    const { app, time } = await controls(ONLINE, {
      restartBot: async () => {
        throw limited(3);
      },
    });

    await app.user.click(screen.getByRole("button", { name: "Restart" }));

    expect(await screen.findByText("Try again in 3 s.")).toBeDefined();
    expect((screen.getByRole("button", { name: "Stop" }) as HTMLButtonElement).disabled).toBe(
      false,
    );
    await expectWait(app, time, { button: "Restart", method: "restartBot", seconds: 3 });
  });
});
