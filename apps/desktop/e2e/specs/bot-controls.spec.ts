// A bot driven from its screen (Plan.md P11.9; ADR-0020, U38, U40, U41, S47): the Owner sets
// its server after the password, switches its mode and starts it; a pushed frame then shows
// it Online with its running controls.
import type { LiveUpdateMessage } from "@afkfleet/ui/generated/LiveUpdateMessage";

import { type FakeBackend, OWNER, PASSWORD } from "../fake/backend";
import { expect, fakeBackend, test } from "../fixtures";
import { enterCode, expectHome, logIn, push } from "./steps";

const SINCE = "2026-10-26T12:00:00.000Z";

/** The built-in `farm` mode, as the server seeds it. */
const FARM = "01a14202-2800-7000-8000-000000000002";

/** The `bot` frame of the bot of `name`, as the fake shows it now. */
function frame(backend: FakeBackend, name: string): LiveUpdateMessage {
  const bot = backend.bot(name);
  return { update: "event", event: { type: "bot", bot_id: bot.id, bot } };
}

test("the Owner sets a bot's server after the password, switches its mode and starts it", async ({
  page,
  context,
}) => {
  const backend = await fakeBackend(context);
  backend.addAccount(OWNER);
  await page.goto("/");
  await logIn(page, OWNER);
  await enterCode(page, backend.code(OWNER));
  await expectHome(page, OWNER);
  await page.getByRole("link", { name: "Open AfkBot1" }).click();
  const main = page.getByRole("main");
  const badge = main.locator("[data-slot=badge]");
  const settings = main.getByRole("region", { name: "Settings" });

  await settings.getByRole("button", { name: "Set" }).click();
  await settings.getByLabel("Server address").fill("127.0.0.1:25565");
  await settings.getByRole("button", { name: "Save" }).click();
  await settings.getByLabel("Current password").fill(PASSWORD);
  await settings.getByRole("button", { name: "Continue" }).click();
  await expect(settings.getByText("127.0.0.1:25565")).toBeVisible();

  await settings.getByLabel("Mode").selectOption({ label: "farm (built in)" });
  await expect(settings.getByLabel("Mode")).toHaveValue(FARM);

  await main.getByRole("button", { name: "Start" }).click();
  await expect(badge).toHaveText("Starting");
  backend.setBot("AfkBot1", {
    status: { kind: "reported", state: "online", since: SINCE, online_since: SINCE, attempt: 1 },
  });
  await push(page, frame(backend, "AfkBot1"));
  await expect(badge).toHaveText("Online");
  await expect(main.getByRole("button", { name: "Stop" })).toBeVisible();
  await expect(main.getByRole("button", { name: "Restart" })).toBeVisible();
});
