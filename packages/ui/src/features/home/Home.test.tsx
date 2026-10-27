import { screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import { fakeWatch } from "../../api/FakeApiClient";
import type { BotPage } from "../../generated/BotPage";
import { account } from "../../test/accounts";
import { me } from "../../test/fixtures";
import { bot, botFrame, botId, event, mode } from "../../test/live";
import { fakeTime } from "../../test/renderApp";
import { signedIn } from "../../test/signedIn";
import { refusal } from "../../test/signIn";
import { HIDDEN_MODE, LINK_AN_ACCOUNT, NO_BOTS } from "../bots/texts";

const NOW = new Date("2026-10-26T14:05:00.000Z");
const SINCE = "2026-10-26T12:00:00.000Z";

afterEach(() => {
  vi.useRealTimers();
});

/** The built-in `afk` mode, which every test bot runs. */
const AFK = mode(1, { name: "afk", ownership: "built_in" });

const ONLINE = bot(2, {
  server_address: "127.0.0.1:25565",
  desired_state: "running",
  status: { kind: "reported", state: "online", since: SINCE, online_since: SINCE },
});

const STOPPED = bot(1, {
  server_address: "play.example.com",
  status: {
    kind: "reported",
    state: "stopped",
    since: SINCE,
    last_disconnect: { kind: "kicked", message: "Server closed" },
  },
});

/** Home over `handlers`, at `NOW`, the bots `page` unless they say otherwise. */
async function home(page: BotPage, handlers: Partial<ApiClient> = {}) {
  const time = fakeTime();
  vi.setSystemTime(NOW);
  const app = await signedIn(
    { bots: async () => page, modes: async () => ({ items: [AFK] }), ...handlers },
    { advanceTimers: time.advanceTimers },
  );
  return { app, time };
}

/** The card of the bot whose account is `name`. */
async function card(name: string) {
  const link = await screen.findByRole("link", { name: `Open ${name}` });
  const shown = link.closest<HTMLElement>("[data-slot=card]");
  if (shown === null) {
    throw new Error(`no card around ${name}'s link`);
  }
  return within(shown);
}

/** The fact a card shows under `term`, or null when it shows none. */
function fact(inCard: ReturnType<typeof within>, term: string): string | null {
  const shown = inCard.queryByText(term, { selector: "dt" });
  return shown?.nextElementSibling?.textContent ?? null;
}

describe("Home", () => {
  it("opens on the bots, right under the Header, with no sign-in line", async () => {
    await home({ items: [ONLINE] });

    expect(await screen.findByRole("heading", { name: "Bots" })).toBeDefined();
    expect(screen.queryByText(/Signed in as/)).toBeNull();
    expect(screen.queryByText("Bots arrive in a later version.")).toBeNull();
  });

  it("shows each bot's card: its name and badge, then its labelled facts, none it lacks", async () => {
    await home({ items: [ONLINE, STOPPED] });

    const online = await card("AfkBot2");
    const stopped = await card("AfkBot1");

    expect(online.getByText("Online")).toBeDefined();
    expect(fact(online, "Server")).toBe("127.0.0.1:25565");
    expect(fact(online, "Mode")).toBe("afk (built in)");
    expect(fact(online, "Uptime")).toBe("2 h 5 min");
    expect(fact(online, "Last disconnect")).toBeNull();
    expect(stopped.getByText("Stopped")).toBeDefined();
    expect(fact(stopped, "Uptime")).toBeNull();
    expect(fact(stopped, "Last disconnect")).toBe("Kicked: Server closed");
    expect(fact(stopped, "Reason")).toBeNull();
  });

  it("says a card's mode couldn't be loaded when the modes can't be read", async () => {
    await home(
      { items: [ONLINE] },
      {
        modes: async () => {
          throw refusal(500, "internal");
        },
      },
    );

    const online = await card("AfkBot2");

    expect(await online.findByText("Couldn't load the mode")).toBeDefined();
  });

  it("shows why a bot isn't running, and no address where the server sent none", async () => {
    await home({
      items: [bot(1, { desired_state: "running", status: { kind: "unplaced", why: "no_server" } })],
    });

    const shown = await card("AfkBot1");

    expect(shown.getByText("Not running")).toBeDefined();
    expect(fact(shown, "Reason")).toBe("No server set");
    expect(fact(shown, "Server")).toBeNull();
  });

  it("names a mode the viewer can't see as such", async () => {
    await home({ items: [ONLINE] }, { modes: async () => ({ items: [] }) });

    expect(fact(await card("AfkBot2"), "Mode")).toBe(HIDDEN_MODE);
  });

  it("opens a bot's screen from its card", async () => {
    const { app } = await home({ items: [ONLINE] }, { bot: async () => ONLINE });

    await app.user.click(await screen.findByRole("link", { name: "Open AfkBot2" }));

    expect(app.path()).toBe(`/bots/${botId(2)}`);
  });

  it("follows the bots live: a card's change, a new bot, and one that goes", async () => {
    const watch = fakeWatch();
    await home({ items: [ONLINE, STOPPED] }, { watchEvents: watch.handler });
    await card("AfkBot2");

    watch.latest().push(botFrame(bot(2, { status: { kind: "reported", state: "stopping" } })));
    watch.latest().push(botFrame(bot(3, { account: account(3) })));
    watch.latest().push(event({ type: "bot_removed", bot_id: botId(1) }));

    expect((await card("AfkBot2")).getByText("Stopping")).toBeDefined();
    expect(await card("AfkBot3")).toBeDefined();
    expect(screen.queryByRole("link", { name: "Open AfkBot1" })).toBeNull();
  });

  it("shows more bots a page at a time", async () => {
    const { app } = await home(
      { items: [] },
      {
        bots: async (before) =>
          before === undefined ? { items: [ONLINE], next: botId(2) } : { items: [STOPPED] },
      },
    );
    await card("AfkBot2");

    await app.user.click(screen.getByRole("button", { name: "Show more" }));

    expect(await card("AfkBot1")).toBeDefined();
    expect(screen.queryByRole("button", { name: "Show more" })).toBeNull();
  });

  it("offers one clear next step when there are no bots", async () => {
    const { app } = await home({ items: [] });

    expect(await screen.findByText(NO_BOTS)).toBeDefined();
    await app.user.click(screen.getByRole("link", { name: LINK_AN_ACCOUNT }));
    expect(app.path()).toBe("/accounts/link");
  });

  it("says so when the bots can't be read", async () => {
    await home(
      { items: [] },
      {
        bots: async () => {
          throw refusal(500, "internal");
        },
      },
    );

    expect(await screen.findByText(/^Couldn't load the bots: /)).toBeDefined();
  });

  it("shows a warning as one compact line above the cards, never a box", async () => {
    await home({ items: [ONLINE] }, { me: async () => ({ ...me(), recovery_codes_left: 2 }) });

    const warning = await screen.findByText(/recovery codes? left/);
    const line = warning.closest<HTMLElement>("[role=alert]");

    expect(line?.dataset.slot).toBe("notice-line");
    expect(
      within(line as HTMLElement).getByRole("link", { name: "Open your profile" }),
    ).toBeDefined();
    expect(within(line as HTMLElement).getByRole("button", { name: "Dismiss" })).toBeDefined();
    const heading = screen.getByRole("heading", { name: "Bots" });
    expect(
      (line as HTMLElement).compareDocumentPosition(heading) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });
});
