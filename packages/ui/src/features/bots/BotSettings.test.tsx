import { screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/ApiClient";
import type { BotResponse } from "../../generated/BotResponse";
import type { GrantLevel } from "../../generated/GrantLevel";
import { account } from "../../test/accounts";
import { deferred } from "../../test/fixtures";
import { bot, botId, mode, modeId } from "../../test/live";
import { fakeTime } from "../../test/renderApp";
import { expectWait, limited } from "../../test/retryWait";
import { atBot } from "../../test/signedIn";
import { refusal } from "../../test/signIn";
import { HIDDEN_MODE } from "./texts";

const ID = botId(1);
const NOW = new Date("2026-10-26T14:05:00.000Z");

afterEach(() => {
  vi.useRealTimers();
});

const AFK = mode(1, { name: "afk", ownership: "built_in" });
const FARM = mode(2, { name: "farm", ownership: "built_in" });
const WARP = mode(3, { name: "warp", ownership: "someone_else", unlisted_command: true });

/** Bot 1 as a viewer at `level` sees it: running afk, its server set unless `address` is null. */
function seen(level: GrantLevel, address: string | null = "play.example.com") {
  return bot(1, {
    account: account(1, { level }),
    mode_id: AFK.id,
    ...(address === null || level === "view" ? {} : { server_address: address }),
  });
}

/** Bot 1's screen; the bot answers what `current.bot` holds when it's read. */
async function settings(shown: BotResponse, handlers: Partial<ApiClient> = {}) {
  const time = fakeTime();
  vi.setSystemTime(NOW);
  const current = { bot: shown };
  const app = await atBot(
    ID,
    {
      bot: async () => current.bot,
      modes: async () => ({ items: [WARP, FARM, AFK] }),
      stepUp: async () => {},
      ...handlers,
    },
    { advanceTimers: time.advanceTimers },
  );
  await screen.findByRole("heading", { name: shown.account.name });
  return { app, time, current };
}

/** The mode selector. */
async function selector(): Promise<HTMLSelectElement> {
  return (await screen.findByRole("combobox", { name: "Mode" })) as HTMLSelectElement;
}

/** The selector's options: each one's text, and whether it's a choice. */
function options(select: HTMLSelectElement): [string, boolean][] {
  return [...select.options].map((option) => [option.text, !option.disabled]);
}

/** How many times `method` was called. */
function calls(app: { fake: { calls: { method: string }[] } }, method: keyof ApiClient): number {
  return app.fake.calls.filter((call) => call.method === method).length;
}

describe("a bot's Settings: the mode", () => {
  it("offer a Control holder the modes they may choose, an unlisted one hidden", async () => {
    await settings(seen("control"));

    const select = await selector();

    expect(screen.getByRole("heading", { name: "Settings" })).toBeDefined();
    expect(options(select)).toEqual([
      ["farm (built in)", true],
      ["afk (built in)", true],
    ]);
    expect(select.value).toBe(AFK.id);
  });

  it("offer a Manage holder every mode, an unlisted one too", async () => {
    await settings(seen("manage"));

    expect(options(await selector())).toEqual([
      ["warp", true],
      ["farm (built in)", true],
      ["afk (built in)", true],
    ]);
  });

  it("show a mode the viewer can't see as the current value, never as a choice", async () => {
    await settings(bot(1, { account: account(1, { level: "control" }), mode_id: modeId(9) }));

    const select = await selector();

    expect(options(select)[0]).toEqual([HIDDEN_MODE, false]);
    expect(select.selectedOptions[0]?.text).toBe(HIDDEN_MODE);
    expect(screen.queryByText(modeId(9))).toBeNull();
  });

  it("switch at once, saying Switching… meanwhile, then read the bot again", async () => {
    const answer = deferred<BotResponse>();
    const { app, current } = await settings(seen("control"), {
      setBotMode: () => answer.promise,
    });

    await app.user.selectOptions(await selector(), FARM.id);

    expect(await screen.findByText("Switching…")).toBeDefined();
    current.bot = { ...seen("control"), mode_id: FARM.id };
    answer.resolve(current.bot);
    expect(
      await screen.findByRole("option", { name: "farm (built in)", selected: true }),
    ).toBeDefined();
    expect(screen.queryByText("Switching…")).toBeNull();
    expect(app.fake.calls.filter((call) => call.method === "setBotMode")).toEqual([
      { method: "setBotMode", args: [ID, { mode_id: FARM.id }] },
    ]);
  });

  it("say only a manager may choose a mode on a 403, and read the bot and the modes again", async () => {
    const { app } = await settings(seen("control"), {
      setBotMode: async () => {
        throw refusal(403, "forbidden");
      },
    });
    const select = await selector();
    const before = [calls(app, "bot"), calls(app, "modes")];

    await app.user.selectOptions(select, FARM.id);

    expect(
      await screen.findByText("Only someone who manages this bot can choose that mode."),
    ).toBeDefined();
    expect([calls(app, "bot"), calls(app, "modes")]).toEqual([
      (before[0] ?? 0) + 1,
      (before[1] ?? 0) + 1,
    ]);
  });

  it("show a 422's field text and read the modes again", async () => {
    const { app } = await settings(seen("control"), {
      setBotMode: async () => {
        throw refusal(422, "validation_failed", {
          fields: [{ path: "mode_id", message: "isn't a mode you can use" }],
        });
      },
    });
    const select = await selector();
    const before = calls(app, "modes");

    await app.user.selectOptions(select, FARM.id);

    expect(await screen.findByText("isn't a mode you can use")).toBeDefined();
    expect(calls(app, "modes")).toBe(before + 1);
  });
});

describe("a bot's Settings: the server", () => {
  it("change the server after the password, in place", async () => {
    let asked = 0;
    const { app, current } = await settings(seen("manage"), {
      patchBot: async (_id, request) => {
        asked += 1;
        if (asked === 1) {
          throw refusal(403, "step_up_required");
        }
        current.bot = { ...seen("manage", request.server_address) };
        return current.bot;
      },
    });
    const section = within(screen.getByRole("region", { name: "Settings" }));

    await app.user.click(section.getByRole("button", { name: "Change" }));
    const field = section.getByLabelText("Server address");
    expect(section.getByText("Host or host:port, e.g. play.example.com")).toBeDefined();
    await app.user.clear(field);
    await app.user.type(field, "127.0.0.1:25565");
    await app.user.click(section.getByRole("button", { name: "Save" }));
    await app.user.type(await screen.findByLabelText("Current password"), "a password");
    await app.user.click(screen.getByRole("button", { name: "Continue" }));

    expect(await section.findByText("127.0.0.1:25565")).toBeDefined();
    expect(section.queryByLabelText("Server address")).toBeNull();
    expect(
      app.fake.calls.filter((call) => call.method === "patchBot").map((call) => call.args),
    ).toEqual([
      [ID, { server_address: "127.0.0.1:25565" }],
      [ID, { server_address: "127.0.0.1:25565" }],
    ]);
  });

  it("set a server where none is, and cancel without a call", async () => {
    const { app } = await settings(seen("manage", null));
    const section = within(screen.getByRole("region", { name: "Settings" }));

    expect(section.getByText("Not set")).toBeDefined();
    await app.user.click(section.getByRole("button", { name: "Set" }));
    await app.user.click(section.getByRole("button", { name: "Cancel" }));

    expect(section.queryByLabelText("Server address")).toBeNull();
    expect(calls(app, "patchBot")).toBe(0);
  });

  it("show a 422 at the field", async () => {
    const { app } = await settings(seen("manage"), {
      patchBot: async () => {
        throw refusal(422, "validation_failed", {
          fields: [{ path: "server_address", message: "must have a port from 1 to 65535" }],
        });
      },
    });
    const section = within(screen.getByRole("region", { name: "Settings" }));

    await app.user.click(section.getByRole("button", { name: "Change" }));
    await app.user.click(section.getByRole("button", { name: "Save" }));

    expect(await section.findByText("must have a port from 1 to 65535")).toBeDefined();
  });

  it("wait out a 429 on Save", async () => {
    const { app, time } = await settings(seen("manage"), {
      patchBot: async () => {
        throw limited(3);
      },
    });
    const section = within(screen.getByRole("region", { name: "Settings" }));

    await app.user.click(section.getByRole("button", { name: "Change" }));
    await app.user.click(section.getByRole("button", { name: "Save" }));

    await expectWait(app, time, { button: "Save", method: "patchBot", seconds: 3 });
  });
});

describe("what Settings leave to the facts (R105)", () => {
  it("leave a Control holder the address, read-only among the facts", async () => {
    await settings(seen("control"));

    const facts = screen.getAllByRole("term").map((term) => term.textContent);

    expect(facts).toContain("Server");
    expect(facts).not.toContain("Mode");
    expect(screen.queryByRole("button", { name: "Change" })).toBeNull();
  });

  it("give a View holder no Settings, the mode among the facts", async () => {
    await settings(seen("view"));

    expect(screen.queryByRole("heading", { name: "Settings" })).toBeNull();
    expect(screen.getAllByRole("term").map((term) => term.textContent)).toContain("Mode");
  });

  it("move a manager's mode and server into Settings", async () => {
    await settings(seen("manage"));

    const facts = screen.queryAllByRole("term").map((term) => term.textContent);

    expect(facts).not.toContain("Mode");
    expect(facts).not.toContain("Server");
  });
});
