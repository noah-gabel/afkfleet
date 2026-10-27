import { screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { fakeWatch } from "../../api/FakeApiClient";
import type { BotResponse } from "../../generated/BotResponse";
import { formatClock, formatDate, formatTime } from "../../lib/time";
import { bot, botFrame, botId, event, mode, modeId } from "../../test/live";
import { fakeTime } from "../../test/renderApp";
import { atBot } from "../../test/signedIn";
import { refusal } from "../../test/signIn";
import { BOT_NOT_FOUND, HIDDEN_MODE } from "./texts";

const ID = botId(1);
const NOW = new Date("2026-10-26T14:05:00.000Z");
const SINCE = "2026-10-26T12:00:00.000Z";

afterEach(() => {
  vi.useRealTimers();
});

/** The built-in `afk` mode, which bot 1 runs. */
const AFK = mode(1, { name: "afk", ownership: "built_in" });

/** Bot 1's screen, its bot `shown`, at `NOW`. */
async function botScreen(shown: BotResponse, handlers: Partial<ApiClient> = {}) {
  const time = fakeTime();
  vi.setSystemTime(NOW);
  const app = await atBot(
    ID,
    { bot: async () => shown, modes: async () => ({ items: [AFK] }), ...handlers },
    { advanceTimers: time.advanceTimers },
  );
  await screen.findByRole("heading", { name: shown.account.name });
  return { app, time };
}

/** The fact the screen shows under `term`, or null when it shows none. */
function fact(term: string): string | null {
  const shown = screen.queryByText(term, { selector: "dt" });
  return shown?.nextElementSibling?.textContent ?? null;
}

const ONLINE = bot(1, {
  server_address: "127.0.0.1:25565",
  desired_state: "running",
  status: { kind: "reported", state: "online", since: SINCE, online_since: SINCE, attempt: 1 },
});

describe("a bot's screen", () => {
  it("shows an online bot: its state, server, mode, since when and for how long", async () => {
    await botScreen(ONLINE);

    expect(screen.getByText("Online")).toBeDefined();
    expect(fact("Server")).toBe("127.0.0.1:25565");
    expect(fact("Mode")).toBe("afk (built in)");
    expect(fact("State since")).toBe(formatTime(SINCE));
    expect(fact("Uptime")).toBe("2 h 5 min");
    expect(fact("Last disconnect")).toBeNull();
    expect(fact("Reason")).toBeNull();
  });

  it("counts the uptime on, once a minute", async () => {
    const { time } = await botScreen(ONLINE);

    await time.advance(60_000);

    expect(fact("Uptime")).toBe("2 h 6 min");
  });

  it("counts the uptime each second in its first minute, then each minute", async () => {
    const since = new Date(NOW.getTime() - 58_000).toISOString();
    const { time } = await botScreen(
      bot(1, {
        desired_state: "running",
        status: { kind: "reported", state: "online", since, online_since: since, attempt: 1 },
      }),
    );
    const shown = [fact("Uptime")];

    for (const step of [1000, 1000, 59_000, 1000]) {
      await time.advance(step);
      shown.push(fact("Uptime"));
    }

    expect(shown).toEqual(["58 s", "59 s", "1 min", "1 min", "2 min"]);
  });

  it("changes the uptime on its whole minute, however long after it the screen opened", async () => {
    const since = new Date(NOW.getTime() - (2 * 3600 + 5 * 60 + 30) * 1000).toISOString();
    const { time } = await botScreen(
      bot(1, {
        desired_state: "running",
        status: { kind: "reported", state: "online", since, online_since: since, attempt: 1 },
      }),
    );
    const before = fact("Uptime");

    await time.advance(30_000);

    expect([before, fact("Uptime")]).toEqual(["2 h 5 min", "2 h 6 min"]);
  });

  it("shows a reconnecting bot's attempt and how its last session ended", async () => {
    await botScreen(
      bot(1, {
        desired_state: "running",
        status: {
          kind: "reported",
          state: "backoff",
          since: SINCE,
          attempt: 3,
          last_disconnect: { kind: "kicked", message: "Server closed" },
        },
      }),
    );

    expect(screen.getByText("Reconnecting")).toBeDefined();
    expect(fact("Reconnect attempt")).toBe("3");
    expect(fact("Last disconnect")).toBe("Kicked: Server closed");
    expect(fact("Uptime")).toBeNull();
  });

  it("shows why a paused bot rests", async () => {
    await botScreen(
      bot(1, {
        desired_state: "running",
        status: { kind: "reported", state: "paused", reason: "duplicate_login", since: SINCE },
      }),
    );

    expect(screen.getByText("Paused")).toBeDefined();
    expect(fact("Reason")).toBe("Someone logged in with its account");
  });

  it("shows why a bot isn't running, and leaves out the address the server didn't send", async () => {
    await botScreen(
      bot(1, { desired_state: "running", status: { kind: "unplaced", why: "no_server" } }),
    );

    expect(screen.getByText("Not running")).toBeDefined();
    expect(fact("Reason")).toBe("No server set");
    expect(fact("Server")).toBeNull();
    expect(fact("State since")).toBeNull();
  });

  it("says when its agent was last seen while the agent is offline", async () => {
    await botScreen(
      bot(1, {
        desired_state: "running",
        status: { kind: "unknown", state: "online", since: SINCE, agent_last_seen_at: SINCE },
      }),
    );

    expect(screen.getByText("Agent offline")).toBeDefined();
    expect(screen.getByText(`Its agent was last seen at ${formatClock(SINCE)}.`)).toBeDefined();
  });

  it("says on which day its agent was last seen before today", async () => {
    const seen = new Date(NOW.getTime() - 3 * 24 * 3600 * 1000).toISOString();
    await botScreen(
      bot(1, {
        desired_state: "running",
        status: { kind: "unknown", state: "online", since: seen, agent_last_seen_at: seen },
      }),
    );

    expect(
      screen.getByText(`Its agent was last seen on ${formatDate(seen)} at ${formatClock(seen)}.`),
    ).toBeDefined();
  });

  it("gives both reasons of a bot that isn't placed and rests paused", async () => {
    await botScreen(
      bot(1, {
        desired_state: "running",
        status: { kind: "unplaced", why: "no_server", state: "paused", reason: "banned" },
      }),
    );

    expect(fact("Not running")).toBe("No server set");
    expect(fact("Paused")).toBe("Banned from this server");
    expect(fact("Reason")).toBeNull();
  });

  it("says the mode couldn't be loaded when the modes can't be read", async () => {
    await botScreen(ONLINE, {
      modes: async () => {
        throw refusal(500, "internal");
      },
    });

    expect(await screen.findByText("Couldn't load the mode")).toBeDefined();
  });

  it("shows its agent when the server sends it, as it does to Admins and the Owner", async () => {
    await botScreen(
      bot(1, {
        agent: { id: "01900000-0000-7000-8000-000000000501", name: "relay-1", status: "online" },
      }),
    );

    expect(fact("Agent")).toBe("relay-1 · online");
  });

  it("names a mode the viewer can't see as such, never by its ID", async () => {
    await botScreen(bot(1, { mode_id: modeId(9) }));

    expect(fact("Mode")).toBe(HIDDEN_MODE);
    expect(screen.queryByText(modeId(9))).toBeNull();
  });

  it("follows the bot's frames", async () => {
    const watch = fakeWatch();
    await botScreen(ONLINE, { watchEvents: watch.handler });

    watch
      .latest()
      .push(
        botFrame(
          bot(1, { desired_state: "running", status: { kind: "reported", state: "stopping" } }),
        ),
      );

    expect(await screen.findByText("Stopping")).toBeDefined();
  });

  it("says a bot it can't find doesn't exist or can't be seen", async () => {
    vi.setSystemTime(NOW);
    const app = await atBot(ID, {
      bot: async () => {
        throw refusal(404, "not_found");
      },
    });

    expect(await screen.findByText(BOT_NOT_FOUND)).toBeDefined();
    await app.user.click(within(screen.getByRole("main")).getByRole("link", { name: "Home" }));
    expect(app.path()).toBe("/home");
  });

  it("says so when the bot goes while its screen is open", async () => {
    const watch = fakeWatch();
    let gone = false;
    await botScreen(ONLINE, {
      watchEvents: watch.handler,
      bot: async () => {
        if (gone) {
          throw refusal(404, "not_found");
        }
        return ONLINE;
      },
    });

    gone = true;
    watch.latest().push(event({ type: "bot_removed", bot_id: ID }));

    expect(await screen.findByText(BOT_NOT_FOUND)).toBeDefined();
  });
});
