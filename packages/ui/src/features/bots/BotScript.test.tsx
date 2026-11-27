// A bot's script on its screen (Plan.md P15.26; ADR-0036, D13; ADR-0044, E9, E10; ADR-0045): its
// status and its error with the link to its line, its log as the live events bring it, and the
// waiting reason when no agent online runs its mode.
import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { fakeWatch } from "../../api/FakeApiClient";
import type { BotResponse } from "../../generated/BotResponse";
import { SCRIPT_API } from "../../generated/script-api";
import { bot, botId, event, mode, modeId } from "../../test/live";
import { atBot } from "../../test/signedIn";

const ID = botId(1);
const BALANCE = SCRIPT_API.examples[3]?.script;
/** Bot 1's mode: a script. */
const SCRIPTED = mode(1, {
  name: "Balance",
  ...(BALANCE === undefined ? {} : { script: BALANCE }),
});

/** Bot 1's screen, the bot `shown` on mode 1, with a live watch the test pushes to. */
async function scripted(shown: BotResponse = bot(1)) {
  const watch = fakeWatch();
  const app = await atBot(ID, {
    bot: async () => shown,
    modes: async () => ({ items: [SCRIPTED] }),
    watchEvents: watch.handler,
  });
  await screen.findByRole("heading", { name: shown.account.name });
  return { app, watch };
}

/** The screen's Script card. */
function card(): HTMLElement {
  return screen.getByRole("region", { name: "Script" });
}

describe("a bot's script on its screen (P15.26)", () => {
  it("says nothing came yet, then its status and its log as the live events bring them", async () => {
    const { watch } = await scripted();
    expect(
      await within(card()).findByText("No word from its script since the app connected."),
    ).toBeTruthy();

    act(() => {
      watch.latest().push(event({ type: "bot_script", bot_id: ID, script: { state: "running" } }));
      watch
        .latest()
        .push(
          event({ type: "bot_script_log", bot_id: ID, log: { kind: "sent", text: "balance" } }),
        );
      watch
        .latest()
        .push(
          event({ type: "bot_script_log", bot_id: ID, log: { kind: "line", text: "paid 120" } }),
        );
    });

    expect(await within(card()).findByText("Running")).toBeTruthy();
    const log = within(card()).getByRole("list", { name: "Log" });
    expect(
      within(log)
        .getAllByRole("listitem")
        .map((item) => item.textContent),
    ).toEqual(["Sentbalance", "Logpaid 120"]);
  });

  it("shows a stopped script's error, its hook and its place, linked to that line of its mode", async () => {
    const { app, watch } = await scripted();

    act(() => {
      watch.latest().push(
        event({
          type: "bot_script",
          bot_id: ID,
          script: {
            state: "stopped",
            problem: {
              kind: "error",
              hook: "on_join",
              line: 3,
              column: 5,
              message: "Function not found: hodl",
            },
          },
        }),
      );
    });

    const shown = card();
    expect(await within(shown).findByText("Stopped")).toBeTruthy();
    expect(within(shown).getByText("An error of the script's")).toBeTruthy();
    expect(within(shown).getByText("In on_join, line 3, column 5")).toBeTruthy();
    expect(within(shown).getByText("Function not found: hodl")).toBeTruthy();
    await app.user.click(within(shown).getByRole("link", { name: "Show in the script" }));
    await waitFor(() =>
      expect(app.runtime.router.state.location.href).toBe(`/modes/${modeId(1)}?line=3&column=5`),
    );
  });

  it("shows a refusal by its agent without a link when there's no place", async () => {
    const { watch } = await scripted();

    act(() => {
      watch.latest().push(
        event({
          type: "bot_script",
          bot_id: ID,
          script: { state: "refused", problem: { kind: "compile", message: "too new" } },
        }),
      );
    });

    expect(await within(card()).findByText("Refused by its agent")).toBeTruthy();
    expect(within(card()).queryByRole("link", { name: "Show in the script" })).toBeNull();
  });

  it("labels a click line as one, its fate in words with the name a script reads (P16.16)", async () => {
    const { watch } = await scripted();
    const text = "a click on slot 13 may have worked: the script checks the next screen (unclear)";

    act(() => {
      watch
        .latest()
        .push(event({ type: "bot_script_log", bot_id: ID, log: { kind: "click", text } }));
    });

    const log = await within(card()).findByRole("list", { name: "Log" });
    expect(
      within(log)
        .getAllByRole("listitem")
        .map((item) => item.textContent),
    ).toEqual([`Click${text}`]);
  });

  it("has no Script card for a bot on a step mode with nothing reported", async () => {
    await atBot(ID, { bot: async () => bot(1), modes: async () => ({ items: [mode(1)] }) });
    await screen.findByRole("heading", { name: bot(1).account.name });

    expect(screen.queryByRole("region", { name: "Script" })).toBeNull();
  });

  it("says an agent must be updated while every agent online is too old for its mode (D13)", async () => {
    await scripted(
      bot(1, {
        desired_state: "running",
        status: { kind: "unplaced", why: "agent_too_old" },
      }),
    );

    expect(screen.getByText("Waiting for an updated agent")).toBeTruthy();
    expect(
      screen.getByText(
        "Its mode is a script, and every agent online runs a version too old for it. Update an agent.",
      ),
    ).toBeTruthy();
  });
});

describe("a bot whose mode needs script API level 2 (P16.15)", () => {
  it("says its agent needs this release, and why the mode needs it", async () => {
    const lister = SCRIPT_API.examples.find((one) => one.id === "auction_lister")?.script;
    const watch = fakeWatch();
    const shown = bot(1, {
      desired_state: "running",
      status: { kind: "unplaced", why: "agent_too_old" },
    });
    await atBot(ID, {
      bot: async () => shown,
      modes: async () => ({
        items: [mode(1, { name: "Lister", ...(lister === undefined ? {} : { script: lister }) })],
      }),
      watchEvents: watch.handler,
    });
    await screen.findByRole("heading", { name: shown.account.name });

    expect(
      screen.getByText(
        "Its mode uses script API level 2 (screens, signs, the clock or the bot's name), and every agent online runs a version from before it. Update an agent to this release.",
      ),
    ).toBeTruthy();
  });
});
